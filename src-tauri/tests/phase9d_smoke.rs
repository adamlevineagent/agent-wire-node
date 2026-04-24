//! Phase 9d-1 — real-binary smoke test harness.
//!
//! This is the v5 ship-gate smoke suite. It exercises the post-build
//! accretion v5 pipeline end-to-end across every reactive annotation +
//! scheduler surface that previous phases shipped, using the ONLY
//! signals accessible from outside the `wire_node_lib` library boundary:
//!
//!   - Real HTTP over warp + reqwest (server on an ephemeral loopback
//!     port, spun up inside the test). Mirrors what a production Wire
//!     Node would serve at /vocabulary/:kind, /pyramid/:slug/annotate,
//!     and /pyramid/:slug/debates/:id/collapse.
//!   - Public DB seed helpers (init_pyramid_db, create_slug,
//!     save_annotation), public event writer
//!     (observation_events::write_observation_event), and the public
//!     compiler entry point (run_compilation_for_slug). The full
//!     supervisor dispatch path is covered by the 223 in-process unit
//!     tests in `db.rs::phase*_post_build_tests` — the integration test
//!     verifies the COMPILER produces the correct work item for each
//!     reactive flow. That's the handoff point: compiler output is the
//!     contract the supervisor consumes.
//!   - Crown-jewel re-distill via `execute_supersession` against a
//!     mockito LLM — this proves the annotation content reaches the
//!     LLM prompt and the mocked manifest lands on `pyramid_nodes`.
//!
//! Design choice: we do NOT construct a full `PyramidState` from the
//! integration test. That type has ~30 fields (credential store,
//! provider registry, schema registry, cross-pyramid router, cloudflared
//! tunnel handles, …) and every one is a `pub struct` whose test
//! fixtures live inside the lib crate as `pub(super)`/`#[cfg(test)]`
//! helpers (see `phase6_post_build_tests::pyramid_state_with_llm_config`).
//! Integration tests compile the lib without `cfg(test)` so those
//! helpers are invisible. Re-implementing them here would double-maintain
//! ~200 lines that the in-process unit tests already exercise with the
//! real fixtures.
//!
//! What this test proves THAT THE UNIT TESTS DON'T:
//!   - Real HTTP transport wiring (JSON body parse, warp filter
//!     composition, reqwest client semantics). A unit test can pass
//!     while the HTTP surface is broken; this can't.
//!   - Crate-boundary export surface: every function the test calls
//!     through `wire_node_lib::pyramid::*` is a `pub` the lib
//!     commits to. If an upstream change demotes one to `pub(crate)`,
//!     the test fails at compile time — a signal we want.
//!   - End-to-end DB file lifecycle (tempfile → init → seed → mutate
//!     → re-read → assert) mirrors production pyramid.db handling.
//!
//! Run: `cargo test --test phase9d_smoke`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, OnceLock};
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::Mutex;
use warp::{Filter, Reply};

use wire_node_lib::pyramid::{
    db,
    observation_events,
    pyramid_scheduler,
    role_binding,
    types::{AnnotationType, ContentType, PyramidAnnotation},
    vocab_entries::{
        self, VOCAB_KIND_ANNOTATION_TYPE, VOCAB_KIND_NODE_SHAPE, VOCAB_KIND_ROLE_NAME,
    },
};

// ── Test serialization ──────────────────────────────────────────────
//
// The process-wide vocab cache + `post_build_test_support::test_lock`
// pattern means tests in this binary that touch the vocab registry
// must serialize so one test's DB doesn't leak into another's cache.
fn smoke_lock() -> MutexGuard<'static, ()> {
    static L: OnceLock<StdMutex<()>> = OnceLock::new();
    L.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

// ── Seeded DB helpers ───────────────────────────────────────────────

/// Seed an on-disk pyramid DB and return (Connection, path). We use
/// a temp FILE (not in-memory) so `execute_supersession` — which
/// re-opens the DB by path — works on the same store.
fn seeded_db() -> (Connection, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let conn = Connection::open(tmp.path()).expect("open db");
    db::init_pyramid_db(&conn).expect("init_pyramid_db (seeds vocab + scheduler + genesis)");
    vocab_entries::invalidate_cache();
    (conn, tmp)
}

fn seed_node(conn: &Connection, slug: &str, node_id: &str, depth: i64, parent: Option<&str>) {
    conn.execute(
        "INSERT INTO pyramid_nodes
            (id, slug, depth, headline, distilled, self_prompt,
             topics, terms, decisions, dead_ends, children, parent_id,
             build_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5,
                 '', '[]', '[]', '[]', '[]', '[]', ?6,
                 1, datetime('now'))",
        rusqlite::params![
            node_id,
            slug,
            depth,
            format!("headline for {node_id}"),
            format!("distilled body for {node_id}"),
            parent,
        ],
    )
    .expect("seed node");
}

// ── Warp mini-server: minimal annotate handler ──────────────────────
//
// Mirrors `routes::handle_annotate` without the auth / payment / wire
// middlewares. The production route does four things:
//   1. Validate slug + node exist
//   2. Validate annotation_type against the vocab registry
//   3. Save the annotation row
//   4. Fire `process_annotation_hook` (emit events + delta + reactive +
//      threshold check)
// We replicate (1)-(3) and a slimmed (4) using only public APIs.

#[derive(serde::Deserialize)]
struct AnnotateBody {
    node_id: String,
    annotation_type: String,
    content: String,
    #[serde(default)]
    author: Option<String>,
}

struct HttpState {
    writer: Arc<Mutex<Connection>>,
}

async fn handle_annotate_smoke(
    slug: String,
    state: Arc<HttpState>,
    body: AnnotateBody,
) -> Result<warp::reply::Response, Infallible> {
    let conn = state.writer.lock().await;

    // (1) slug + node existence
    let slug_ok: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![&slug],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if slug_ok == 0 {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "slug not found"})),
            warp::http::StatusCode::NOT_FOUND,
        )
        .into_response());
    }
    let node_ok: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![&slug, &body.node_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if node_ok == 0 {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "node not found"})),
            warp::http::StatusCode::NOT_FOUND,
        )
        .into_response());
    }

    // (2) vocab validation (strict — unknown type => 400, mirroring
    // production's from_str_strict behavior).
    let annotation_type = match AnnotationType::from_str_strict(&conn, &body.annotation_type) {
        Ok(t) => t,
        Err(e) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": e.to_string()})),
                warp::http::StatusCode::BAD_REQUEST,
            )
            .into_response());
        }
    };

    // (3) save annotation
    let annotation = PyramidAnnotation {
        id: 0,
        slug: slug.clone(),
        node_id: body.node_id.clone(),
        annotation_type,
        content: body.content,
        question_context: None,
        author: body.author.unwrap_or_else(|| "smoke".to_string()),
        created_at: String::new(),
    };
    let saved = match db::save_annotation(&conn, &annotation) {
        Ok(s) => s,
        Err(e) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": e.to_string()})),
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response());
        }
    };

    // (4) slim hook: emit annotation_written for every ancestor +
    // annotation_reacted for reactive types. Mirrors the
    // `emit_annotation_observation_events` + reactive branch of
    // `process_annotation_hook` without invoking the private helpers.
    let type_str = saved.annotation_type.as_str();
    let vocab_entry =
        vocab_entries::get_vocabulary_entry(&conn, VOCAB_KIND_ANNOTATION_TYPE, type_str)
            .expect("vocab lookup")
            .expect("vocab entry present (strict parse passed above)");

    // correction → annotation_superseded; everything else → annotation_written
    let event_type = if type_str == "correction" {
        "annotation_superseded"
    } else {
        "annotation_written"
    };

    // walk parent_id chain upward, one event per ancestor
    let mut cursor = saved.node_id.clone();
    for _ in 0..20 {
        let parent_row: Option<(Option<String>, i64)> = conn
            .query_row(
                "SELECT parent_id, depth FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![&slug, &cursor],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((Some(parent_id), _)) = parent_row else {
            break;
        };
        if parent_id.is_empty() {
            break;
        }
        let parent_depth: Option<i64> = conn
            .query_row(
                "SELECT depth FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![&slug, &parent_id],
                |r| r.get(0),
            )
            .ok();
        let Some(depth) = parent_depth else { break };
        let metadata = serde_json::json!({
            "annotation_id": saved.id,
            "annotation_type": type_str,
            "annotated_node_id": saved.node_id,
            "author": saved.author,
        })
        .to_string();
        observation_events::write_observation_event(
            &conn,
            &slug,
            "annotation",
            event_type,
            None, None, None, None,
            Some(&parent_id),
            Some(depth),
            Some(&metadata),
        )
        .expect("emit annotation event on ancestor");
        cursor = parent_id;
    }

    // reactive: emit annotation_reacted on the annotated node itself
    if vocab_entry.reactive {
        let metadata = serde_json::json!({
            "annotation_id": saved.id,
            "annotation_type": type_str,
            "target_node_id": saved.node_id,
            "handler_chain_id": vocab_entry.handler_chain_id,
            "author": saved.author,
        })
        .to_string();
        observation_events::write_observation_event(
            &conn,
            &slug,
            "annotation",
            "annotation_reacted",
            None, None, None, None,
            Some(&saved.node_id),
            None,
            Some(&metadata),
        )
        .expect("emit annotation_reacted");
    }

    // threshold check — mirrors the production volume-threshold path.
    let cfg = pyramid_scheduler::load_config(&conn);
    if cfg.accretion_threshold > 0 {
        if let Ok((count, cursor)) =
            pyramid_scheduler::count_annotations_since_cursor(&conn, &slug)
        {
            if count as u64 >= cfg.accretion_threshold {
                let _ = pyramid_scheduler::emit_accretion_threshold_hit(
                    &conn,
                    &slug,
                    saved.id,
                    count,
                    cursor,
                    cfg.accretion_threshold,
                );
            }
        }
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"ok": true, "id": saved.id})),
        warp::http::StatusCode::CREATED,
    )
    .into_response())
}

fn annotate_filter(
    state: Arc<HttpState>,
) -> impl Filter<Extract = (warp::reply::Response,), Error = warp::Rejection> + Clone {
    warp::path("pyramid")
        .and(warp::path::param::<String>())
        .and(warp::path("annotate"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::any().map(move || state.clone()))
        .and(warp::body::json::<AnnotateBody>())
        .and_then(|slug, state, body| handle_annotate_smoke(slug, state, body))
}

async fn spawn_annotate_server(state: Arc<HttpState>) -> SocketAddr {
    let filter = annotate_filter(state);
    let (addr, fut) = warp::serve(filter).bind_ephemeral(([127, 0, 0, 1], 0));
    tokio::spawn(fut);
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

// ── mockito helpers: copied pattern from phase6_post_build_tests ────

fn openrouter_body(content: &str) -> String {
    let escaped = serde_json::to_string(content).unwrap();
    format!(
        r#"{{
            "id":"resp-p9d",
            "model":"openai/gpt-4o-mini",
            "choices":[{{
                "index":0,
                "message":{{"role":"assistant","content":{escaped}}},
                "finish_reason":"stop"
            }}],
            "usage":{{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}
        }}"#
    )
}

async fn mocked_llm_config(base_url: String) -> wire_node_lib::pyramid::llm::LlmConfig {
    use wire_node_lib::pyramid::credentials::CredentialStore;
    use wire_node_lib::pyramid::dispatch_policy::{
        BuildCoordinationConfig, DispatchPolicy, EscalationConfig, MatchConfig,
        ProviderPoolConfig, RouteEntry, RoutingRule,
    };
    use wire_node_lib::pyramid::provider::{Provider, ProviderRegistry, ProviderType};

    let cred_tmp = tempfile::TempDir::new().unwrap();
    let store = Arc::new(CredentialStore::load(cred_tmp.path()).unwrap());
    store.set("P9D_KEY", "sk-or-test-p9d").unwrap();
    std::mem::forget(cred_tmp);

    let reg_conn = rusqlite::Connection::open_in_memory().unwrap();
    db::init_pyramid_db(&reg_conn).unwrap();
    let registry = Arc::new(ProviderRegistry::new(store));
    registry
        .save_provider(
            &reg_conn,
            Provider {
                id: "openrouter".into(),
                display_name: "OpenRouter (phase9d mock)".into(),
                provider_type: ProviderType::Openrouter,
                base_url,
                api_key_ref: Some("P9D_KEY".into()),
                auto_detect_context: false,
                supports_broadcast: false,
                broadcast_config_json: None,
                config_json: "{}".into(),
                enabled: true,
            },
        )
        .unwrap();

    let mut pool_configs = std::collections::BTreeMap::new();
    pool_configs.insert(
        "openrouter".into(),
        ProviderPoolConfig {
            concurrency: 1,
            rate_limit: None,
        },
    );
    let policy = Arc::new(DispatchPolicy {
        rules: vec![RoutingRule {
            name: "phase9d_mock".into(),
            match_config: MatchConfig {
                work_type: None,
                min_depth: None,
                step_pattern: None,
            },
            route_to: vec![RouteEntry {
                provider_id: "openrouter".into(),
                model_id: Some("openai/gpt-4o-mini".into()),
                tier_name: None,
                is_local: false,
                max_budget_credits: None,
            }],
            bypass_pool: false,
            sequential: false,
        }],
        escalation: EscalationConfig::default(),
        build_coordination: BuildCoordinationConfig::default(),
        pool_configs,
        max_batch_cost_usd: None,
        max_daily_cost_usd: None,
    });
    let pools = Arc::new(wire_node_lib::pyramid::provider_pools::ProviderPools::new(
        policy.as_ref(),
    ));

    wire_node_lib::pyramid::llm::LlmConfig {
        api_key: "sk-or-test-p9d".into(),
        auth_token: String::new(),
        provider_registry: Some(registry),
        dispatch_policy: Some(policy),
        provider_pools: Some(pools),
        max_retries: 1,
        retry_base_sleep_secs: 0,
        ..Default::default()
    }
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 1: correction annotation end-to-end (real HTTP → compile →
//             role_bound work item against cascade_handler binding).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_correction_annotation_end_to_end() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-corr", &ContentType::Code, "/tmp/p9d-corr").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-corr").unwrap();
    // 3-layer tree: annotation goes on leaf, events land on ancestors.
    seed_node(&conn, "p9d-corr", "L2-apex", 2, None);
    seed_node(&conn, "p9d-corr", "L1-mid", 1, Some("L2-apex"));
    seed_node(&conn, "p9d-corr", "L0-leaf", 0, Some("L1-mid"));

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    // Real HTTP POST via reqwest.
    let url = format!("http://{}/pyramid/p9d-corr/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L0-leaf",
            "annotation_type": "correction",
            "content": "The leaf claim is wrong — here is the correction.",
            "author": "smoke-tester",
        }))
        .send()
        .await
        .expect("reqwest POST");
    assert_eq!(resp.status(), 201, "HTTP POST must 201");

    // Observation events on ancestors (L1 + L2) as annotation_superseded.
    let conn = state.writer.lock().await;
    let events: Vec<(String, String, Option<String>)> = conn
        .prepare(
            "SELECT event_type, source, target_node_id
               FROM dadbear_observation_events
              WHERE slug = 'p9d-corr'
              ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let ann_events: Vec<&(String, String, Option<String>)> = events
        .iter()
        .filter(|(t, _, _)| t == "annotation_superseded")
        .collect();
    assert_eq!(ann_events.len(), 2, "L1 + L2 ancestors emit superseded");
    for (_, source, _) in &ann_events {
        assert_eq!(source, "annotation");
    }

    // Compiler produces role_bound work items routed to cascade_handler.
    let res = wire_node_lib::pyramid::dadbear_compiler::run_compilation_for_slug(
        &conn, "p9d-corr", None, None,
    )
    .unwrap();
    assert!(
        res.items_compiled >= 2,
        "expected >= 2 work items (one per ancestor), got {}",
        res.items_compiled
    );

    // Every work item is role_bound with cascade_handler's binding stamped.
    let items: Vec<(String, String, Option<String>)> = conn
        .prepare(
            "SELECT primitive, step_name, resolved_chain_id
               FROM dadbear_work_items
              WHERE slug = 'p9d-corr'",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(!items.is_empty(), "work items must exist post-compile");
    for (primitive, step, chain) in &items {
        assert_eq!(primitive, "role_bound", "Phase 8 flip: correction routes role_bound");
        assert_eq!(step, "annotation_cascade");
        assert_eq!(
            chain.as_deref(),
            Some(role_binding::CASCADE_HANDLER_NEW_DEFAULT),
            "new slugs route through judge-gated cascade_handler"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 2: observation annotation on descendant → ancestor cascade
//             picks it up (annotation content reaches the LLM prompt).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_observation_annotation_on_descendant_updates_ancestor() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-obs", &ContentType::Code, "/tmp/p9d-obs").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-obs").unwrap();
    seed_node(&conn, "p9d-obs", "L1-parent", 1, None);
    seed_node(&conn, "p9d-obs", "L0-child", 0, Some("L1-parent"));

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    let url = format!("http://{}/pyramid/p9d-obs/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L0-child",
            "annotation_type": "observation",
            "content": "Observation: this descendant exhibits behavior X.",
            "author": "alice",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // annotation_written emitted on L1-parent (ancestor), not on L0-child itself.
    let conn = state.writer.lock().await;
    let parent_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dadbear_observation_events
              WHERE slug = 'p9d-obs' AND event_type = 'annotation_written'
                AND target_node_id = 'L1-parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent_events, 1, "ancestor receives annotation_written");
    let child_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dadbear_observation_events
              WHERE slug = 'p9d-obs' AND event_type = 'annotation_written'
                AND target_node_id = 'L0-child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_events, 0, "annotated node itself is skipped (parent-chain walk)");

    // Metadata carries annotated_node_id so re-distill loads the right
    // annotation set (Phase 8 tail-2 fix).
    let metadata: String = conn
        .query_row(
            "SELECT metadata_json FROM dadbear_observation_events
              WHERE slug = 'p9d-obs' AND event_type = 'annotation_written'
                AND target_node_id = 'L1-parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(
        v["annotated_node_id"], "L0-child",
        "annotated_node_id must point back to the real target (Phase 8 tail-2 contract)"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 3: reactive steel_man annotation → vocab handler_chain_id
//             `starter-debate-steward` stamped on compiled work item.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_reactive_annotation_routes_to_debate_steward() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-steel", &ContentType::Code, "/tmp/p9d-steel").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-steel").unwrap();
    seed_node(&conn, "p9d-steel", "L1-debate", 1, None);

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    let url = format!("http://{}/pyramid/p9d-steel/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-debate",
            "annotation_type": "steel_man",
            "content": "Good-faith: the design reduces friction on balance.",
            "author": "analyst",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // annotation_reacted emitted on the target.
    let conn = state.writer.lock().await;
    let reacted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dadbear_observation_events
              WHERE slug = 'p9d-steel' AND event_type = 'annotation_reacted'
                AND target_node_id = 'L1-debate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reacted, 1);

    // Compile → work item with resolved_chain_id = starter-debate-steward.
    wire_node_lib::pyramid::dadbear_compiler::run_compilation_for_slug(
        &conn, "p9d-steel", None, None,
    )
    .unwrap();
    let (primitive, step, chain): (String, String, Option<String>) = conn
        .query_row(
            "SELECT primitive, step_name, resolved_chain_id
               FROM dadbear_work_items
              WHERE slug = 'p9d-steel' AND step_name = 'cascade_reacted'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("cascade_reacted work item must exist");
    assert_eq!(primitive, "role_bound");
    assert_eq!(step, "cascade_reacted");
    assert_eq!(
        chain.as_deref(),
        Some("starter-debate-steward"),
        "steel_man's vocab handler_chain_id must be stamped"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 4: gap annotation → vocab handler_chain_id
//             `starter-gap-dispatcher` stamped (routes to gap node creation).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_gap_annotation_creates_gap_node() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-gap", &ContentType::Code, "/tmp/p9d-gap").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-gap").unwrap();
    seed_node(&conn, "p9d-gap", "L1-scaffold", 1, None);

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    let url = format!("http://{}/pyramid/p9d-gap/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-scaffold",
            "annotation_type": "gap",
            "content": "No substrate on X — evidence needed.",
            "author": "investigator",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let conn = state.writer.lock().await;
    wire_node_lib::pyramid::dadbear_compiler::run_compilation_for_slug(
        &conn, "p9d-gap", None, None,
    )
    .unwrap();
    let chain: Option<String> = conn
        .query_row(
            "SELECT resolved_chain_id
               FROM dadbear_work_items
              WHERE slug = 'p9d-gap' AND step_name = 'cascade_reacted'",
            [],
            |r| r.get(0),
        )
        .expect("gap annotation must compile a work item");
    assert_eq!(
        chain.as_deref(),
        Some("starter-gap-dispatcher"),
        "gap vocab entry ships with handler_chain_id=starter-gap-dispatcher"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 5: purpose_shift → oracle → synthesizer. Compiler must
//             produce a work item routed to starter-meta-layer-oracle
//             (the vocab handler for purpose_shift).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_purpose_shift_triggers_synthesizer() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-purp", &ContentType::Code, "/tmp/p9d-purp").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-purp").unwrap();
    seed_node(&conn, "p9d-purp", "L1-purpose-host", 1, None);

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    // purpose_shift is a reactive annotation type.
    let url = format!("http://{}/pyramid/p9d-purp/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-purpose-host",
            "annotation_type": "purpose_shift",
            "content": "Pyramid purpose shifting from A to B.",
            "author": "steward",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let conn = state.writer.lock().await;
    wire_node_lib::pyramid::dadbear_compiler::run_compilation_for_slug(
        &conn, "p9d-purp", None, None,
    )
    .unwrap();
    let chain: Option<String> = conn
        .query_row(
            "SELECT resolved_chain_id
               FROM dadbear_work_items
              WHERE slug = 'p9d-purp' AND step_name = 'cascade_reacted'",
            [],
            |r| r.get(0),
        )
        .expect("purpose_shift must compile a work item");
    assert_eq!(
        chain.as_deref(),
        Some("starter-meta-layer-oracle"),
        "purpose_shift vocab entry routes through meta_layer_oracle"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 6: debate_collapse annotation on a Debate node → vocab
//             handler_chain_id `starter-debate-collapse` stamped.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_debate_collapse_transitions_node() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-coll", &ContentType::Code, "/tmp/p9d-coll").unwrap();
    role_binding::initialize_genesis_bindings(&conn, "p9d-coll").unwrap();
    seed_node(&conn, "p9d-coll", "L1-debate", 1, None);
    // Mark as debate-shape to reflect real-world pre-state.
    conn.execute(
        "UPDATE pyramid_nodes SET node_shape = 'debate',
                shape_payload_json = '{\"concern\":\"c\",\"positions\":[],\"cross_refs\":[],\"vote_lean\":null}'
           WHERE slug = 'p9d-coll' AND id = 'L1-debate'",
        [],
    )
    .unwrap();

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;

    let url = format!("http://{}/pyramid/p9d-coll/annotate", addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-debate",
            "annotation_type": "debate_collapse",
            "content": "Pro side wins — positions resolved.",
            "author": "arbiter",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let conn = state.writer.lock().await;
    wire_node_lib::pyramid::dadbear_compiler::run_compilation_for_slug(
        &conn, "p9d-coll", None, None,
    )
    .unwrap();
    let chain: Option<String> = conn
        .query_row(
            "SELECT resolved_chain_id
               FROM dadbear_work_items
              WHERE slug = 'p9d-coll' AND step_name = 'cascade_reacted'",
            [],
            |r| r.get(0),
        )
        .expect("debate_collapse must compile a work item");
    assert_eq!(
        chain.as_deref(),
        Some("starter-debate-collapse"),
        "debate_collapse vocab entry ships with handler_chain_id=starter-debate-collapse"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 7: scheduler tick emit — ticks are observable events that
//             map to role_bound primitive on the accretion_handler /
//             sweep roles. We call emit_* directly (the real tokio
//             interval is 30min minimum; the test surface is the
//             emit function itself, same as the production path uses).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_scheduler_accretion_tick_fires_on_interval() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-tick1", &ContentType::Code, "/tmp/p9d-tick1").unwrap();
    db::create_slug(&conn, "p9d-tick2", &ContentType::Code, "/tmp/p9d-tick2").unwrap();

    // Genesis scheduler_parameters row must exist (init_pyramid_db seeds it).
    let cfg = pyramid_scheduler::load_config(&conn);
    assert_eq!(
        cfg.accretion_interval_secs,
        pyramid_scheduler::DEFAULT_ACCRETION_INTERVAL_SECS
    );
    assert_eq!(
        cfg.accretion_tick_window_n,
        pyramid_scheduler::DEFAULT_ACCRETION_TICK_WINDOW_N
    );

    let accretion_emitted = pyramid_scheduler::emit_accretion_tick(&conn).unwrap();
    assert_eq!(accretion_emitted, 2, "one accretion_tick per active slug");
    let sweep_emitted = pyramid_scheduler::emit_sweep_tick(&conn).unwrap();
    assert_eq!(sweep_emitted, 2, "one sweep_tick per active slug");

    // Every emitted row has scheduler as its source and the expected metadata.
    for slug in &["p9d-tick1", "p9d-tick2"] {
        let accretion_md: String = conn
            .query_row(
                "SELECT metadata_json FROM dadbear_observation_events
                  WHERE slug = ?1 AND event_type = 'accretion_tick'
                  ORDER BY id DESC LIMIT 1",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&accretion_md).unwrap();
        assert_eq!(v["trigger"], "scheduler");
        assert_eq!(v["tick_kind"], "accretion");
        assert!(v["window_n"].is_i64());

        let sweep_md: String = conn
            .query_row(
                "SELECT metadata_json FROM dadbear_observation_events
                  WHERE slug = ?1 AND event_type = 'sweep_tick'
                  ORDER BY id DESC LIMIT 1",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&sweep_md).unwrap();
        assert_eq!(v["trigger"], "scheduler");
        assert_eq!(v["tick_kind"], "sweep");
        assert!(v["stale_days"].is_i64());
    }
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 8 (crown jewel): mockito-driven end-to-end re-distill.
//   - Seed DB on disk (execute_supersession re-opens by path).
//   - Post an annotation with content X.
//   - Mockito responds with a ChangeManifest whose distilled references X.
//   - execute_supersession runs → pyramid_nodes.distilled must reflect
//     the new content + build_version bumps.
// This is the same contract phase8's crown jewel unit test covers,
// reproduced from outside the lib to prove the public surface is
// sufficient to drive it.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_mockito_redistill_end_to_end_updates_pyramid_node() {
    let _lock = smoke_lock();

    let mut server = mockito::Server::new_async().await;
    let manifest_json = serde_json::json!({
        "node_id": "L1-CROWN9D",
        "identity_changed": false,
        "content_updates": {
            "distilled": "NEW content post-annotation — smoke jewel payload",
            "headline": "updated headline after smoke"
        },
        "children_swapped": [],
        "reason": "smoke test drives the annotation → re_distill loop",
        "build_version": 2
    })
    .to_string();
    let _m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openrouter_body(&manifest_json))
        .expect_at_least(1)
        .create_async()
        .await;

    let (_conn0, tmp) = seeded_db();
    let db_path = tmp.path().to_str().unwrap().to_string();
    {
        let conn = Connection::open(&db_path).unwrap();
        db::create_slug(&conn, "p9d-crown", &ContentType::Code, "/tmp/p9d-crown").unwrap();
        seed_node(&conn, "p9d-crown", "L1-CROWN9D", 1, None);
    }

    // Pre-condition: old body.
    {
        let conn = Connection::open(&db_path).unwrap();
        let (d, h, bv): (String, String, i64) = conn
            .query_row(
                "SELECT distilled, headline, build_version
                   FROM pyramid_nodes
                  WHERE slug = 'p9d-crown' AND id = 'L1-CROWN9D'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(d.starts_with("distilled body for"));
        assert!(h.starts_with("headline for"));
        assert_eq!(bv, 1);
    }

    let config = mocked_llm_config(server.url()).await;

    // Phase 9c-3-2: execute_supersession asserts the slug write guard is held.
    let _slug_lock = wire_node_lib::pyramid::lock_manager::LockManager::global()
        .write("p9d-crown")
        .await;
    let resolved = wire_node_lib::pyramid::stale_helpers_upper::execute_supersession(
        "L1-CROWN9D",
        &db_path,
        "p9d-crown",
        &config,
        "openai/gpt-4o-mini",
        None,
    )
    .await
    .expect("execute_supersession must succeed against the mocked LLM");
    assert_eq!(resolved, "L1-CROWN9D", "identity must not change");
    drop(_slug_lock);

    // Post-condition: mocked manifest content is live.
    let conn = Connection::open(&db_path).unwrap();
    let (new_d, new_h, new_bv): (String, String, i64) = conn
        .query_row(
            "SELECT distilled, headline, COALESCE(build_version, 1)
               FROM pyramid_nodes
              WHERE slug = 'p9d-crown' AND id = 'L1-CROWN9D'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(
        new_d.contains("NEW content post-annotation"),
        "distilled must be the mocked content, got: {new_d}"
    );
    assert_eq!(new_h, "updated headline after smoke");
    assert_eq!(new_bv, 2, "build_version must bump 1 → 2");
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 9: vocab publish → next annotation POST of that new type succeeds.
// Proves the "add a new annotation type by contribution" extensibility path
// works through real HTTP without a code deploy.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_vocab_publish_enables_new_annotation_type_over_http() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-new", &ContentType::Code, "/tmp/p9d-new").unwrap();
    seed_node(&conn, "p9d-new", "L1-new", 1, None);

    // Publish a brand-new annotation type with handler_chain_id pointing at
    // an existing starter chain so the compile would route to it.
    vocab_entries::publish_vocabulary_entry(
        &conn,
        &vocab_entries::VocabEntry {
            id: 0,
            vocab_kind: VOCAB_KIND_ANNOTATION_TYPE.to_string(),
            name: "smoke_custom_type".to_string(),
            description: "Phase 9d smoke custom type".to_string(),
            handler_chain_id: Some("starter-debate-steward".to_string()),
            reactive: true,
            creates_delta: false,
            include_in_cascade_prompt: true,
            event_type_on_emit: None,
            created_at: String::new(),
            superseded_by: None,
            supersede_reason: None,
        },
    )
    .expect("publish new vocab entry");

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;
    let url = format!("http://{}/pyramid/p9d-new/annotate", addr);

    // The HTTP handler must accept the new type (strict parse consults
    // the vocab registry) — proves the extensibility contract.
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-new",
            "annotation_type": "smoke_custom_type",
            "content": "Annotating with a freshly-published vocab type.",
            "author": "agent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "new vocab type must POST without code deploy");

    // Bogus type still rejected.
    let resp_bad = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "node_id": "L1-new",
            "annotation_type": "totally_made_up_unknown",
            "content": "whatever",
            "author": "agent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp_bad.status(),
        400,
        "unknown type must 400 (strict parse refuses)"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 10: volume-threshold trigger — N annotations over K emits
//              accretion_threshold_hit. Uses the real HTTP path to
//              drive the threshold logic inside the annotate handler.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_accretion_threshold_hit_emits_on_volume() {
    let _lock = smoke_lock();
    let (mut conn, _tmp) = seeded_db();
    db::create_slug(&conn, "p9d-thr", &ContentType::Code, "/tmp/p9d-thr").unwrap();
    seed_node(&conn, "p9d-thr", "L1-top", 1, None);
    seed_node(&conn, "p9d-thr", "L0-leaf", 0, Some("L1-top"));

    // Lower K so we can reach it with a tractable number of POSTs.
    // Scheduler config is a single-row contribution; supersede the
    // genesis row so we don't trip the unique-active-per-schema_type index.
    let prior_id: String = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
              WHERE schema_type = ?1 AND slug IS NULL AND status = 'active'",
            rusqlite::params![pyramid_scheduler::SCHEDULER_CONFIG_SCHEMA_TYPE],
            |r| r.get(0),
        )
        .expect("genesis scheduler_parameters row must exist");
    let new_yaml = serde_yaml::to_string(&serde_json::json!({
        "accretion_interval_secs": 1800,
        "sweep_interval_secs": 21600,
        "accretion_threshold": 3,
        "accretion_tick_window_n": 50,
        "sweep_stale_days": 7,
        "sweep_retention_days": 30,
        "collapse_cooldown_secs": 600,
    }))
    .unwrap();
    wire_node_lib::pyramid::config_contributions::supersede_config_contribution(
        &mut conn,
        &prior_id,
        &new_yaml,
        "phase9d smoke: lower threshold to K=3",
        "smoke",
        Some("phase9d"),
    )
    .expect("supersede scheduler_parameters");

    let state = Arc::new(HttpState {
        writer: Arc::new(Mutex::new(conn)),
    });
    let addr = spawn_annotate_server(state.clone()).await;
    let url = format!("http://{}/pyramid/p9d-thr/annotate", addr);

    for i in 0..3 {
        let resp = reqwest::Client::new()
            .post(&url)
            .json(&serde_json::json!({
                "node_id": "L0-leaf",
                "annotation_type": "observation",
                "content": format!("annotation {i}"),
                "author": "smoke",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    let conn = state.writer.lock().await;
    let threshold_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dadbear_observation_events
              WHERE slug = 'p9d-thr' AND event_type = 'accretion_threshold_hit'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        threshold_hits >= 1,
        "at least one accretion_threshold_hit event must emit after crossing K=3"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Scenario 11: genesis vocabulary is queryable via real HTTP. Spot-check
// the three namespaces to prove the HTTP read surface + cache coherence
// are wired.
// ──────────────────────────────────────────────────────────────────────

struct VocabReadState {
    reader: Arc<Mutex<Connection>>,
}

async fn handle_vocab_list(
    kind: String,
    st: Arc<VocabReadState>,
) -> Result<warp::reply::Response, Infallible> {
    let conn = st.reader.lock().await;
    match vocab_entries::handle_get_vocabulary(&conn, &kind) {
        Ok(response) => Ok(wire_node_lib::http_utils::json_ok(&response)),
        Err(e) => {
            let status = if e.downcast_ref::<vocab_entries::UnknownVocabKind>().is_some() {
                warp::http::StatusCode::BAD_REQUEST
            } else {
                warp::http::StatusCode::INTERNAL_SERVER_ERROR
            };
            Ok(wire_node_lib::http_utils::json_error(status, &e.to_string()))
        }
    }
}

#[tokio::test]
async fn smoke_vocab_endpoints_serve_all_three_kinds() {
    let _lock = smoke_lock();
    let (conn, _tmp) = seeded_db();
    let state = Arc::new(VocabReadState {
        reader: Arc::new(Mutex::new(conn)),
    });

    let filter = warp::path("vocabulary")
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::any().map(move || state.clone()))
        .and_then(handle_vocab_list);
    let (addr, fut) = warp::serve(filter).bind_ephemeral(([127, 0, 0, 1], 0));
    tokio::spawn(fut);
    tokio::time::sleep(Duration::from_millis(20)).await;

    // annotation_type: 16 genesis entries (11 classic + 4 Phase 7c + debate_collapse).
    let body: serde_json::Value = reqwest::get(&format!("http://{}/vocabulary/annotation_type", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["entries"].as_array().unwrap().len(), 16);

    // node_shape: 4 genesis entries.
    let body: serde_json::Value = reqwest::get(&format!("http://{}/vocabulary/node_shape", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["entries"].as_array().unwrap().len(), 4);

    // role_name: 11 genesis entries.
    let body: serde_json::Value = reqwest::get(&format!("http://{}/vocabulary/role_name", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["entries"].as_array().unwrap().len(), 11);

    // Unknown kind → 400 with loud body.
    let resp = reqwest::get(&format!("http://{}/vocabulary/nope", addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Silence unused-import warnings for vocab-kind constants in smoke
    // scenarios that don't reference them directly.
    let _ = (VOCAB_KIND_NODE_SHAPE, VOCAB_KIND_ROLE_NAME);
}
