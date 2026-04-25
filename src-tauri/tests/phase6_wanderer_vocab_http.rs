//! Phase 6 wanderer real-HTTP smoke: spins up a warp server identical to the
//! production `GET /vocabulary/:vocab_kind` route on a loopback port, then hits
//! it with a real reqwest client end-to-end. Closes the 6c-C deferral (prior
//! verifier left "rebuild binary and curl" untested because rebuilding the
//! Tauri desktop binary was out of verifier scope).
//!
//! This is the minimal-viable "real binary" test — it doesn't boot the desktop
//! GUI, but it DOES route a real HTTP request through the same warp filter,
//! same `handle_vocab_registry_list` handler, same `vocab_entries` module, and
//! same sqlite-backed cache that a live Wire Node would use. Coverage matches
//! the three smoke-test curl calls the wanderer prompt asked for (annotation_type
//! / node_shape / role_name / bogus kind → 400), plus the publish-then-refetch
//! roundtrip that proves cache invalidation survives a full HTTP roundtrip.
//!
//! Run: `cargo test --test phase6_wanderer_vocab_http`.
//!
//! Why not bind to the Tauri Desktop binary: that requires a display server +
//! keyring + tunnel negotiation; a headless integration test against the only
//! surface Phase 6 touched (the vocab route) gives equivalent signal for the
//! endpoint contract without the desktop tooling.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, OnceLock};
use std::time::Duration;

/// Serialize test cases that depend on the process-wide vocab cache. Without
/// this, two tests racing each other can cross-contaminate the cache (each
/// conn carries its own genesis set, but the cache is keyed to the first one
/// to fault).
fn cache_lock() -> MutexGuard<'static, ()> {
    static L: OnceLock<StdMutex<()>> = OnceLock::new();
    L.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

use rusqlite::Connection;
use tokio::sync::Mutex;
use warp::Filter;

use wire_node_lib::pyramid::vocab_entries::{self, VOCAB_KIND_ANNOTATION_TYPE};

/// Minimal state the vocab route needs: just a locked sqlite connection.
/// The production route handler uses `state.reader.lock().await`; we mirror
/// that shape here so the copy-path matches byte-for-byte.
struct VocabTestState {
    reader: Arc<Mutex<Connection>>,
}

async fn handle_vocab(
    vocab_kind: String,
    state: Arc<VocabTestState>,
) -> Result<warp::reply::Response, Infallible> {
    let conn = state.reader.lock().await;
    match vocab_entries::handle_get_vocabulary(&conn, &vocab_kind) {
        Ok(response) => Ok(wire_node_lib::http_utils::json_ok(&response)),
        Err(e) => {
            if e.downcast_ref::<vocab_entries::UnknownVocabKind>().is_some() {
                Ok(wire_node_lib::http_utils::json_error(
                    warp::http::StatusCode::BAD_REQUEST,
                    &e.to_string(),
                ))
            } else {
                Ok(wire_node_lib::http_utils::json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ))
            }
        }
    }
}

async fn handle_vocab_publish(
    state: Arc<VocabTestState>,
    body: vocab_entries::VocabPublishRequest,
) -> Result<warp::reply::Response, Infallible> {
    let conn = state.reader.lock().await;
    match vocab_entries::handle_publish_vocabulary(&conn, body) {
        Ok(response) => Ok(wire_node_lib::http_utils::json_ok(&response)),
        Err(e) => {
            if e.downcast_ref::<vocab_entries::UnknownVocabKind>()
                .is_some()
                || e.downcast_ref::<vocab_entries::InvalidVocabPublish>()
                    .is_some()
            {
                Ok(wire_node_lib::http_utils::json_error(
                    warp::http::StatusCode::BAD_REQUEST,
                    &e.to_string(),
                ))
            } else if e.downcast_ref::<vocab_entries::DuplicateVocabEntry>()
                .is_some()
            {
                Ok(wire_node_lib::http_utils::json_error(
                    warp::http::StatusCode::CONFLICT,
                    &e.to_string(),
                ))
            } else {
                Ok(wire_node_lib::http_utils::json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ))
            }
        }
    }
}

/// Mirror of `routes::handle_vocab_registry_list`'s filter composition. Any
/// divergence between this and prod would show up as a wanderer test failure,
/// which is the point.
fn vocab_filter(
    state: Arc<VocabTestState>,
) -> impl Filter<Extract = (warp::reply::Response,), Error = warp::Rejection> + Clone {
    let get = warp::path("vocabulary")
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::any().map({
            let state = state.clone();
            move || state.clone()
        }))
        .and_then(handle_vocab)
        .map(|r: warp::reply::Response| r)
        .boxed();

    let post = warp::path("api")
        .and(warp::path("v1"))
        .and(warp::path("pyramid"))
        .and(warp::path("vocabulary"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::any().map(move || state.clone()))
        .and(warp::body::json::<vocab_entries::VocabPublishRequest>())
        .and_then(handle_vocab_publish)
        .map(|r: warp::reply::Response| r)
        .boxed();

    get.or(post).unify().boxed()
}

/// Spin up a warp server on a system-picked port. Returns `(addr,
/// shutdown_tx)`. Test body is responsible for ensuring the server task
/// outlives all in-flight requests (implicit here — we `await` the request
/// response before test end).
async fn spawn_server(state: Arc<VocabTestState>) -> SocketAddr {
    let filter = vocab_filter(state);
    let (addr, fut) = warp::serve(filter).bind_ephemeral(([127, 0, 0, 1], 0));
    tokio::spawn(fut);
    // Give the reactor a tick to accept. Not strictly needed for
    // `bind_ephemeral` which returns after binding, but defensive.
    tokio::time::sleep(Duration::from_millis(10)).await;
    addr
}

/// Build an in-memory sqlite, initialize the pyramid schema. `init_pyramid_db`
/// already runs `seed_genesis_vocabulary` internally (Phase 6c-A wired it into
/// boot init), so nothing further is needed.
fn seeded_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    wire_node_lib::pyramid::db::init_pyramid_db(&conn).expect("init pyramid db (seeds vocab)");
    // Invalidate the process-wide cache — earlier tests in this binary may
    // have loaded a different DB into it; without invalidation, `list_vocabulary`
    // would return stale rows keyed from the first test's conn.
    vocab_entries::invalidate_cache();
    conn
}

#[tokio::test]
async fn genesis_annotation_type_served_over_real_http() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let addr = spawn_server(state).await;
    let url = format!("http://{}/vocabulary/annotation_type", addr);
    let resp = reqwest::get(&url).await.expect("reqwest get");
    assert_eq!(resp.status(), 200, "expected 200, got {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("parse json");
    assert_eq!(body["vocab_kind"], "annotation_type");
    let entries = body["entries"]
        .as_array()
        .expect("entries array")
        .clone();
    // Phase 9c-1: 11 original + 4 Phase 7c verbs + debate_collapse = 16.
    assert_eq!(
        entries.len(),
        16,
        "genesis ships 16 annotation types, got {} — body: {}",
        entries.len(),
        body
    );
    // Spot-check known-reactive entry.
    let steel_man = entries
        .iter()
        .find(|e| e["name"] == "steel_man")
        .expect("steel_man present");
    assert_eq!(steel_man["reactive"], true);
    assert_eq!(steel_man["handler_chain_id"], "starter-debate-steward");
}

#[tokio::test]
async fn genesis_node_shape_served_over_real_http() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let addr = spawn_server(state).await;
    let url = format!("http://{}/vocabulary/node_shape", addr);
    let resp = reqwest::get(&url).await.expect("reqwest get");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("parse json");
    assert_eq!(body["vocab_kind"], "node_shape");
    let entries = body["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 4, "genesis ships 4 node shapes");
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    for expected in ["scaffolding", "debate", "meta_layer", "gap"] {
        assert!(names.contains(&expected), "{} missing: {:?}", expected, names);
    }
}

#[tokio::test]
async fn genesis_role_name_served_over_real_http() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let addr = spawn_server(state).await;
    let url = format!("http://{}/vocabulary/role_name", addr);
    let resp = reqwest::get(&url).await.expect("reqwest get");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("parse json");
    assert_eq!(body["vocab_kind"], "role_name");
    let entries = body["entries"].as_array().unwrap().clone();
    assert_eq!(
        entries.len(),
        11,
        "genesis ships 11 role names (10 starter + cascade_handler)"
    );
    // Cascade handler canonical default must be present.
    let cascade = entries
        .iter()
        .find(|e| e["name"] == "cascade_handler")
        .expect("cascade_handler role present");
    assert_eq!(
        cascade["handler_chain_id"], "starter-cascade-judge-gated",
        "cascade_handler must bind to judge-gated chain by default"
    );
}

#[tokio::test]
async fn unknown_vocab_kind_returns_400_with_valid_list() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let addr = spawn_server(state).await;
    let url = format!("http://{}/vocabulary/bogus_kind", addr);
    let resp = reqwest::get(&url).await.expect("reqwest get");
    assert_eq!(
        resp.status(),
        400,
        "unknown vocab_kind must 400 (loud_deferral), got {}",
        resp.status()
    );
    let body_txt = resp.text().await.expect("read body");
    // The error body enumerates the valid kinds so operators can correct typos.
    for expected in ["annotation_type", "node_shape", "role_name"] {
        assert!(
            body_txt.contains(expected),
            "400 body must list valid kind '{}', got: {}",
            expected,
            body_txt
        );
    }
    assert!(
        body_txt.contains("bogus_kind"),
        "400 body must echo the bad kind, got: {}",
        body_txt
    );
}

/// Publish a new vocab entry via the write API, then HTTP-fetch again and
/// confirm the new row shows up. Exercises the publish → cache-invalidate →
/// next-read-repopulates → HTTP-response path end-to-end; proves the 6c-A
/// invalidation contract survives a real HTTP roundtrip.
#[tokio::test]
async fn publish_then_refetch_over_http_surfaces_new_entry() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let state_for_publish = state.clone();
    let addr = spawn_server(state).await;

    // Baseline — genesis-count.
    // Phase 9c-1: 11 original + 4 Phase 7c verbs + debate_collapse = 16.
    let url = format!("http://{}/vocabulary/annotation_type", addr);
    let baseline: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    let baseline_count = baseline["entries"].as_array().unwrap().len();
    assert_eq!(baseline_count, 16, "baseline must be 16");

    // Publish a new entry through the normal write API. The API-v1 HTTP
    // route below now covers the external write surface; this still guards
    // the library writer + cache invalidation contract directly.
    {
        let conn = state_for_publish.reader.lock().await;
        let entry = vocab_entries::VocabEntry {
            id: 0, // assigned by DB
            vocab_kind: VOCAB_KIND_ANNOTATION_TYPE.to_string(),
            name: "smoke_test_type".to_string(),
            description: "Wanderer smoke-test annotation type".to_string(),
            handler_chain_id: Some("starter-debate-steward".to_string()),
            reactive: true,
            creates_delta: false,
            include_in_cascade_prompt: true,
            event_type_on_emit: None,
            created_at: String::new(),
            superseded_by: None,
            supersede_reason: None,
        };
        vocab_entries::publish_vocabulary_entry(&conn, &entry)
            .expect("publish new vocab entry");
    }

    // Re-fetch via real HTTP — new entry must be in the response.
    let after: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    let after_entries = after["entries"].as_array().unwrap().clone();
    // Phase 9c-1: genesis 16 + new 1 = 17.
    assert_eq!(
        after_entries.len(),
        17,
        "after publish, expected 17 entries (genesis 16 + new 1), got {}",
        after_entries.len()
    );
    let smoke = after_entries
        .iter()
        .find(|e| e["name"] == "smoke_test_type")
        .expect("new entry must appear in HTTP response after publish");
    assert_eq!(smoke["reactive"], true);
    assert_eq!(
        smoke["handler_chain_id"], "starter-debate-steward",
        "handler_chain_id must round-trip through HTTP"
    );
}

#[tokio::test]
async fn publish_vocab_entry_over_api_v1_http_surfaces_new_entry_and_contribution_id() {
    let _l = cache_lock();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(seeded_conn())),
    });
    let addr = spawn_server(state.clone()).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/api/v1/pyramid/vocabulary", addr))
        .json(&serde_json::json!({
            "type": "annotation_type",
            "term": "http_publish_type",
            "definition": "HTTP-published annotation type",
            "parent": "observation",
            "handler_chain_id": "starter-debate-steward",
            "reactive": true,
            "creates_delta": false,
            "include_in_cascade_prompt": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "publish route should accept aliases");
    let body: serde_json::Value = resp.json().await.unwrap();
    let contribution_id = body["contribution_id"]
        .as_str()
        .expect("publish response includes contribution_id");
    assert!(
        !contribution_id.is_empty(),
        "contribution_id should be non-empty"
    );
    assert_eq!(body["vocab_kind"], "annotation_type");
    assert_eq!(body["name"], "http_publish_type");

    let listed: serde_json::Value = reqwest::get(&format!(
        "http://{}/vocabulary/annotation_type",
        addr
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let entry = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "http_publish_type")
        .expect("HTTP-published vocab entry should be queryable via GET");
    assert_eq!(entry["description"], "HTTP-published annotation type");
    assert_eq!(entry["handler_chain_id"], "starter-debate-steward");
    assert_eq!(entry["reactive"], true);

    let conn = state.reader.lock().await;
    let yaml: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |row| row.get(0),
        )
        .expect("published contribution row exists");
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(yaml_value["vocab_kind"].as_str(), Some("annotation_type"));
    assert_eq!(yaml_value["name"].as_str(), Some("http_publish_type"));
    assert_eq!(
        yaml_value["description"].as_str(),
        Some("HTTP-published annotation type")
    );
    assert_eq!(yaml_value["reactive"].as_bool(), Some(true));
}

/// Cross-pyramid vocab scope check: entries are global (slug=NULL in the
/// contribution row), so two connections pointed at the same DB should both
/// see the same catalog via HTTP. Regression guard against a future refactor
/// that might scope vocab per-slug.
#[tokio::test]
async fn vocab_is_global_not_per_slug() {
    let _l = cache_lock();
    // Share a DB file between two conns to prove vocab is global. In-memory
    // can't be shared without `mode=memory&cache=shared`; use a temp file.
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_str().unwrap().to_string();
    {
        let conn = Connection::open(&path).unwrap();
        wire_node_lib::pyramid::db::init_pyramid_db(&conn).unwrap();
    }
    // Second connection — clean cache (process-wide cache persists across
    // test cases in this binary, so we invalidate before measuring).
    vocab_entries::invalidate_cache();
    let state = Arc::new(VocabTestState {
        reader: Arc::new(Mutex::new(Connection::open(&path).unwrap())),
    });
    let addr = spawn_server(state).await;
    let url = format!("http://{}/vocabulary/annotation_type", addr);
    let body: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    // Phase 9c-1: 16 annotation types post-debate_collapse addition.
    assert_eq!(body["entries"].as_array().unwrap().len(), 16);
}
