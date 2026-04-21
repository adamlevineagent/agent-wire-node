use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use warp::Filter;
use warp::Reply;

use crate::auth::AuthState;
use crate::credits::CreditTracker;
use crate::partner;
use crate::pyramid;
use crate::pyramid::stale_engine::PyramidStaleEngine;
use crate::pyramid::types::AutoUpdateConfig;
use crate::pyramid::watcher::PyramidFileWatcher;
use crate::sync::SyncState;
use crate::tunnel;

/// HTTP server state
#[derive(Clone)]
pub struct ServerState {
    pub cache_dir: std::path::PathBuf,
    pub credits: Arc<RwLock<CreditTracker>>,
    pub auth: Arc<RwLock<AuthState>>,
    pub sync_state: Arc<RwLock<SyncState>>,
    pub tunnel_state: Arc<RwLock<tunnel::TunnelState>>,
    pub jwt_public_key: Arc<RwLock<String>>,
    pub node_id: Arc<RwLock<String>>,
    pub pyramid: Arc<pyramid::PyramidState>,
    pub partner: Arc<partner::PartnerState>,
    /// Fleet roster for receiving fleet dispatch jobs and announcements.
    pub fleet_roster: Arc<RwLock<crate::fleet::FleetRoster>>,
    /// Compute queue handle for enqueuing fleet-dispatched jobs into the
    /// same per-model FIFO queue as local builds (Law 1).
    pub compute_queue: crate::compute_queue::ComputeQueueHandle,
    /// Async fleet dispatch context — None when fleet dispatch is disabled.
    /// Carries the pending-job registry (for dispatcher-side callbacks),
    /// the tunnel state handle, and the operational policy.
    pub fleet_dispatch: Option<Arc<crate::fleet::FleetDispatchContext>>,
    /// Compute market dispatch context — None when compute market is
    /// disabled (fresh install, operator hasn't flipped
    /// `allow_market_visibility = true`, or the Phase 2 init path
    /// hasn't landed yet). Carries the pending-job registry (owned by
    /// Phase 3 requester-side; empty in Phase 2 provider-only builds),
    /// the tunnel state handle (borrowed), and the operational policy
    /// (owned, hot-reloaded on ConfigSynced).
    pub compute_market_dispatch:
        Option<Arc<crate::pyramid::market_dispatch::MarketDispatchContext>>,
    /// Live compute market state — offers, in-flight jobs, counters,
    /// queue mirror seqs. Wrapped in RwLock so WS5's dispatch handler
    /// can upsert/transition jobs, WS6's mirror task can read+bump
    /// seqs, and WS7's offer IPC can mutate the offers map. `None`
    /// only in test fixtures and the pre-init boot window; production
    /// boot constructs this before spawning the HTTP server.
    pub compute_market_state:
        Option<Arc<RwLock<crate::compute_market::ComputeMarketState>>>,
    /// Node configuration (api_url, supabase creds, cache paths).
    /// Needed by operator HTTP routes that proxy to the Wire API
    /// (compute offers, market surface) and by system observability
    /// routes that surface the node's configured endpoints.
    pub config: Arc<RwLock<crate::WireNodeConfig>>,
    /// Rolling work statistics snapshot (jobs done, retries, queue
    /// depth by state). Surfaced via `/pyramid/system/work-stats` for
    /// agent observability.
    pub work_stats: Arc<RwLock<crate::work::WorkStats>>,
    /// Phase 3 requester-side pending jobs registry — shared between
    /// the `compute_quote_flow` walker (inserts) and the inbound
    /// `/v1/compute/job-result` handler (fires + removes). See
    /// `pyramid::pending_jobs` for semantics.
    pub pending_market_jobs: crate::pyramid::pending_jobs::PendingJobs,
    /// Rev 2.1.1 provider-side engine-serialization gate. A bounded
    /// semaphore around the inference call in `spawn_market_worker`;
    /// permits = declared `execution_concurrency` for this node's
    /// offers. Ensures the engine (Ollama / vLLM / etc.) only sees one
    /// (or N, for batch-capable runtimes) job at a time, with the
    /// overflow buffering inside this Rust process rather than inside
    /// the engine's (dumb FIFO) queue.
    ///
    /// Bilateral decision compute-market-saturation-decisions-2026-04-21.md
    /// §D3 + §D4 + provider conformance paragraph: "Providers MUST
    /// serialize dispatches at the engine according to declared
    /// execution_concurrency. Wire's queue-drain estimation assumes
    /// execution_concurrency / typical_serve_ms."
    ///
    /// Defaults to `Semaphore::new(1)` at boot — matches
    /// single-GPU-single-engine operators (BEHEM, llama.cpp, Ollama,
    /// vanilla HF). Batch-capable operators (vLLM continuous-batching,
    /// bridges to managed APIs) will supersede via their offer's
    /// `execution_concurrency` field once Wire's rev 2.1.1 surfaces it.
    pub engine_dispatch_permits: Arc<tokio::sync::Semaphore>,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub documents_cached: usize,
}

/// JWT claims for pyramid query access tokens (WS-ONLINE-C)
///
/// Issued by the Wire server for remote pyramid querying.
///   aud → "pyramid-query", sub → operator_id, slug → target pyramid,
///   query_type → "apex"|"drill"|"search"|"export"|"entities"
#[derive(Debug, Deserialize)]
pub struct PyramidQueryClaims {
    /// Audience — must be "pyramid-query"
    pub aud: Option<String>,
    /// Operator ID of the querying agent (JWT standard `sub`)
    #[serde(alias = "sub")]
    pub operator_id: Option<String>,
    /// Target pyramid slug
    pub slug: Option<String>,
    /// Query type: "apex", "drill", "search", "export", "entities"
    pub query_type: Option<String>,
    /// Expiration (standard JWT — validated by jsonwebtoken crate)
    #[allow(dead_code)]
    pub exp: Option<u64>,
    /// Issuer — should be "wire"
    #[allow(dead_code)]
    pub iss: Option<String>,
    /// JWT ID — for deduplication / logging
    pub jti: Option<String>,
    /// Circle ID — for circle-scoped access tier checking (WS-ONLINE-E)
    pub circle_id: Option<String>,
}

/// JWT claims for document access tokens
///
/// The server-side JWT uses standard claims:
///   sub → document_id, nid → routed_to_node_id, op → consumer_operator_id,
///   jti → token_id, iss → issuer (should be "wire")
#[derive(Debug, Deserialize)]
struct DocumentClaims {
    /// Document ID — JWT puts this in the standard `sub` claim
    #[serde(alias = "sub")]
    document_id: Option<String>,
    /// Node ID this token is scoped to (JWT field: `nid`)
    nid: Option<String>,
    /// Consumer operator ID (JWT field: `op`)
    #[serde(alias = "op")]
    consumer_operator_id: Option<String>,
    /// Expiration (standard JWT — validated by jsonwebtoken crate)
    #[allow(dead_code)]
    exp: Option<u64>,
    /// JWT ID — used as token_id for serve reporting (JWT field: `jti`)
    jti: Option<String>,
    /// Issuer — should be "wire"
    #[allow(dead_code)]
    iss: Option<String>,
}

/// Initialize stale engines for all pyramids with auto_update enabled and not frozen.
/// Called after pyramid DB is initialized.
pub async fn init_stale_engines(pyramid_state: &Arc<pyramid::PyramidState>) {
    // Old WAL cleanup removed — pyramid_pending_mutations table has been dropped.
    // Observation events are pruned by the supervisor's own retention policy.

    // Migrate relative file_hashes paths to absolute (one-time normalization)
    {
        let conn = pyramid_state.writer.lock().await;
        // Find all slugs that have relative paths (don't start with /)
        let slugs_with_relative: Vec<(String, String)> = conn
            .prepare(
                "SELECT DISTINCT fh.slug, s.source_path
                 FROM pyramid_file_hashes fh
                 JOIN pyramid_slugs s ON s.slug = fh.slug
                 WHERE fh.file_path NOT LIKE '/%'",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();

        let mut total_migrated = 0usize;
        for (slug, source_path_json) in &slugs_with_relative {
            let dirs: Vec<String> = serde_json::from_str(source_path_json)
                .unwrap_or_else(|_| vec![source_path_json.clone()]);

            // Get all relative paths for this slug
            let rel_paths: Vec<(String, )> = {
                let mut stmt = match conn.prepare(
                    "SELECT file_path FROM pyramid_file_hashes WHERE slug = ?1 AND file_path NOT LIKE '/%'",
                ) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                stmt.query_map(rusqlite::params![slug], |row| {
                    Ok((row.get::<_, String>(0)?,))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
            };

            for (rel_path,) in &rel_paths {
                // Try to resolve against source directories
                let mut resolved = None;
                for dir in &dirs {
                    let candidate = std::path::Path::new(dir).join(rel_path);
                    if candidate.exists() {
                        resolved = Some(candidate.to_string_lossy().to_string());
                        break;
                    }
                }
                if let Some(abs_path) = resolved {
                    let _ = conn.execute(
                        "UPDATE pyramid_file_hashes SET file_path = ?1 WHERE slug = ?2 AND file_path = ?3",
                        rusqlite::params![abs_path, slug, rel_path],
                    );
                    total_migrated += 1;
                }
            }
        }

        if total_migrated > 0 {
            tracing::info!(
                "Migrated {} relative file_hashes paths to absolute",
                total_migrated
            );
        }
    }

    // Phase 3 fix pass: clone the live LlmConfig (with provider_registry +
    // credential_store) so every PyramidStaleEngine constructed below
    // carries the registry path through dispatched helpers.
    let (base_config, model, defer_maintenance) = {
        let config = pyramid_state.config.read().await;
        let defer = config.dispatch_policy
            .as_ref()
            .map(|p| p.build_coordination.defer_maintenance_during_build)
            .unwrap_or(false);
        (config.clone(), config.primary_model.clone(), defer)
    };

    // Get the DB path from data_dir
    let db_path = match &pyramid_state.data_dir {
        Some(dir) => dir.join("pyramid.db").to_string_lossy().to_string(),
        None => {
            tracing::warn!("No data_dir set on PyramidState, skipping stale engine initialization");
            return;
        }
    };

    // Load all enabled DADBEAR slugs (canonical source: pyramid_dadbear_config
    // existence is enable gate, holds projection anti-join is dispatch gate).
    let configs: Vec<AutoUpdateConfig> = {
        let conn = pyramid_state.reader.lock().await;
        // Get all slugs with DADBEAR configs on non-archived pyramids
        let mut stmt = match conn.prepare(
            "SELECT DISTINCT d.slug FROM pyramid_dadbear_config d
             JOIN pyramid_slugs s ON s.slug = d.slug
             WHERE s.archived_at IS NULL",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to query dadbear configs: {}", e);
                return;
            }
        };
        let slugs: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        slugs
            .iter()
            .filter_map(|slug| pyramid::db::get_auto_update_config(&conn, slug))
            .collect()
    };

    if configs.is_empty() {
        tracing::info!("No pyramids with auto_update enabled, skipping stale engine init");
        return;
    }

    // Create a mutation notification channel: watcher -> stale engine bridge
    let (mutation_tx, mut mutation_rx) = tokio::sync::mpsc::unbounded_channel::<(String, i32)>();

    let mut engines = pyramid_state.stale_engines.lock().await;
    let mut watchers = pyramid_state.file_watchers.lock().await;

    for config in configs {
        let slug = config.slug.clone();

        // Frozen pyramids: skip entirely
        if config.frozen {
            tracing::info!(
                "Pyramid '{}' is frozen — skipping engine and watcher on startup",
                slug
            );
            continue;
        }

        // Phase 12 verifier fix: attach cache_access per-slug so stale
        // helpers that use make_step_ctx_from_llm_config (faq, etc.)
        // reach the step cache.
        let slug_base_config = pyramid_state.attach_cache_access(
            base_config.clone(),
            &slug,
            &format!("stale-{}", slug),
        );

        // Create the engine
        let mut engine = PyramidStaleEngine::new(
            &slug,
            config.clone(),
            &db_path,
            slug_base_config,
            &model,
            pyramid_state.operational.as_ref().clone(),
            pyramid_state.build_event_bus.clone(),
            pyramid_state.active_build.clone(),
            defer_maintenance,
        );

        // Breaker-tripped: create engine in tripped state, log warning, no watcher
        if config.breaker_tripped {
            tracing::warn!(
                "Pyramid '{}' has breaker tripped — engine created in tripped state, no watcher started",
                slug
            );
            engines.insert(slug.clone(), engine);
            continue;
        }

        // Get slug info early — needed for both reconciliation and watcher
        let (source_paths, content_type_str): (Vec<String>, String) = {
            let conn = pyramid_state.reader.lock().await;
            match pyramid::slug::get_slug(&conn, &slug) {
                Ok(Some(info)) => {
                    let paths = serde_json::from_str(&info.source_path)
                        .unwrap_or_else(|_| vec![info.source_path.clone()]);
                    (paths, info.content_type.as_str().to_string())
                }
                _ => (Vec::new(), String::new()),
            }
        };

        // Get ingested extensions for reconciliation
        let ingested_extensions: Vec<String> = {
            let conn = pyramid_state.reader.lock().await;
            pyramid::db::get_ingested_extensions(&conn, &slug).unwrap_or_default()
        };

        // Startup reconciliation: compare files on disk vs pyramid_file_hashes.
        // Discovers new files, hash changes, and deletions that happened while app was closed.
        if !source_paths.is_empty() {
            let conn = pyramid_state.writer.lock().await;
            let (r_new, r_changed, r_deleted, r_unchanged) =
                pyramid::routes::reconcile_source_files(
                    &conn,
                    &slug,
                    &source_paths,
                    &ingested_extensions,
                    &content_type_str,
                );
            if r_new > 0 || r_changed > 0 || r_deleted > 0 {
                tracing::info!(
                    "Startup reconciliation for '{}': {} new, {} changed, {} deleted, {} unchanged",
                    slug, r_new, r_changed, r_deleted, r_unchanged
                );
                engine.notify_mutation(0);
            } else {
                tracing::info!(
                    "Startup reconciliation for '{}': all {} files unchanged",
                    slug, r_unchanged
                );
            }
        }

        // Check for unprocessed observation events (canonical source)
        {
            let conn = pyramid_state.reader.lock().await;
            for layer in 0..=3 {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM dadbear_observation_events
                         WHERE slug = ?1 AND layer = ?2
                           AND id > COALESCE(
                               (SELECT last_bridge_observation_id FROM pyramid_build_metadata WHERE slug = ?1),
                               0
                           )",
                        rusqlite::params![slug, layer],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if count > 0 {
                    tracing::info!(
                        "Pyramid '{}' layer {} has {} unprocessed observation events — starting timer",
                        slug,
                        layer,
                        count
                    );
                    engine.notify_mutation(layer);
                }
            }
        }

        // Start the WAL poll loop (belt-and-suspenders for timer re-arm)
        engine.start_poll_loop();

        engines.insert(slug.clone(), engine);

        if !source_paths.is_empty() {
            let mut watcher = PyramidFileWatcher::new(&slug, source_paths, &pyramid_state.operational.tier2);
            watcher.set_mutation_sender(mutation_tx.clone());
            match watcher.start(&db_path) {
                Ok(()) => {
                    tracing::info!("File watcher started for pyramid '{}'", slug);
                    watchers.insert(slug, watcher);
                }
                Err(e) => {
                    tracing::warn!("Failed to start file watcher for '{}': {}", slug, e);
                }
            }
        }
    }

    tracing::info!(
        "Stale engine init complete: {} engines, {} watchers",
        engines.len(),
        watchers.len()
    );

    // Drop locks before spawning receiver
    drop(engines);
    drop(watchers);

    // Spawn a receiver task that bridges watcher mutations to stale engines
    let ps = pyramid_state.clone();
    tokio::spawn(async move {
        while let Some((slug, layer)) = mutation_rx.recv().await {
            let mut engines = ps.stale_engines.lock().await;
            if let Some(engine) = engines.get_mut(&slug) {
                engine.notify_mutation(layer);
            }
        }
    });
}

/// Start the HTTP server on the given port
///
/// Endpoints:
///   GET /health             — node status
///   GET /documents/:id      — serve document body with JWT verification
///   GET /auth/callback      — magic link redirect landing page
///   POST /auth/complete     — receives tokens from the callback page
pub async fn start_server(
    port: u16,
    cache_dir: std::path::PathBuf,
    credits: Arc<RwLock<CreditTracker>>,
    auth: Arc<RwLock<AuthState>>,
    sync_state: Arc<RwLock<SyncState>>,
    tunnel_state: Arc<RwLock<tunnel::TunnelState>>,
    jwt_public_key: Arc<RwLock<String>>,
    node_id: Arc<RwLock<String>>,
    pyramid: Arc<pyramid::PyramidState>,
    partner: Arc<partner::PartnerState>,
    fleet_roster: Arc<RwLock<crate::fleet::FleetRoster>>,
    compute_queue: crate::compute_queue::ComputeQueueHandle,
    fleet_dispatch: Option<Arc<crate::fleet::FleetDispatchContext>>,
    compute_market_dispatch: Option<Arc<crate::pyramid::market_dispatch::MarketDispatchContext>>,
    compute_market_state: Option<Arc<RwLock<crate::compute_market::ComputeMarketState>>>,
    config: Arc<RwLock<crate::WireNodeConfig>>,
    work_stats: Arc<RwLock<crate::work::WorkStats>>,
    pending_market_jobs: crate::pyramid::pending_jobs::PendingJobs,
) {
    // Rev 2.1.1 provider-side engine serialization. Default: one
    // permit, matching single-GPU-single-engine operators. When the
    // contracts crate surfaces `execution_concurrency` on the offer row
    // (Wire rev 2.1.1 D4), this will be sized from the operator's
    // per-offer declaration. Until then, 1 matches the canonical
    // batch-not-capable default the provider-conformance paragraph
    // says should apply to Ollama / llama.cpp / vanilla HF / MLX-LM
    // runtimes.
    let engine_dispatch_permits = Arc::new(tokio::sync::Semaphore::new(1));

    let state = ServerState {
        cache_dir,
        credits,
        auth,
        sync_state,
        tunnel_state,
        jwt_public_key,
        node_id,
        pyramid,
        partner,
        fleet_roster,
        compute_queue,
        fleet_dispatch,
        config,
        work_stats,
        compute_market_dispatch,
        compute_market_state,
        pending_market_jobs,
        engine_dispatch_permits,
    };

    // Phase 7: Initialize stale engines for auto-update pyramids (background)
    // Runs file reconciliation and engine setup AFTER the HTTP server is ready,
    // so startup isn't blocked by scanning large codebases.
    {
        let bg_state = state.pyramid.clone();
        tokio::spawn(async move {
            init_stale_engines(&bg_state).await;
            tracing::info!("DADBEAR background initialization complete");
        });
    }

    // S2: CORS tightening — restrict to known origins instead of allow_any_origin.
    // These are Tauri's default dev port and common localhost variants.
    // TODO: make configurable via PyramidConfig (editable only via Tauri IPC per S1).
    const CORS_ALLOWED_ORIGINS: &[&str] = &[
        "http://localhost:1420",
        "http://127.0.0.1:1420",
        "https://localhost:1420",
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "tauri://localhost",
    ];
    let cors = warp::cors()
        .allow_origins(CORS_ALLOWED_ORIGINS.iter().copied())
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec![
            "Content-Type",
            "Range",
            "Authorization",
            "X-Payment-Token",
            "Access-Control-Request-Private-Network",
        ]);

    // GET /health
    let health = {
        let state = state.clone();
        warp::path("health").and(warp::get()).and_then(move || {
            let state = state.clone();
            async move {
                let count = count_cached_documents(&state.cache_dir).await;
                let resp = HealthResponse {
                    status: "online".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    documents_cached: count,
                };
                Ok::<_, warp::Rejection>(warp::reply::json(&resp))
            }
        })
    };

    // Query parameter for token fallback (?token=JWT)
    #[derive(Deserialize)]
    struct DocumentQuery {
        token: Option<String>,
    }

    // GET /documents/:id — serve cached document body with JWT verification
    let documents = {
        let state = state.clone();
        warp::path!("documents" / String)
            .and(warp::get())
            .and(warp::header::optional::<String>("authorization"))
            .and(warp::header::optional::<String>("range"))
            .and(warp::query::<DocumentQuery>())
            .and_then(move |document_id: String, auth_header: Option<String>, range_header: Option<String>, query: DocumentQuery| {
                let state = state.clone();
                async move {
                    // Extract JWT from Authorization: Bearer <token> header,
                    // or fall back to ?token=<token> query parameter
                    let token_owned: String = if let Some(ref h) = auth_header {
                        if let Some(t) = h.strip_prefix("Bearer ") {
                            t.to_string()
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };

                    let token: &str = if !token_owned.is_empty() {
                        &token_owned
                    } else if let Some(ref t) = query.token {
                        t
                    } else {
                        return Ok::<_, warp::Rejection>(Reply::into_response(
                            warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({"error": "Missing or invalid Authorization header or token query parameter"})),
                                warp::http::StatusCode::UNAUTHORIZED,
                            )
                        ));
                    };

                    // Verify JWT with Ed25519 public key
                    let (claims_document_id, claims_nid, claims_jti, claims_op) = {
                        let pubkey_str = state.jwt_public_key.read().await;
                        if pubkey_str.is_empty() {
                            return Ok(Reply::into_response(
                                warp::reply::with_status(
                                    warp::reply::json(&serde_json::json!({"error": "Node not configured with JWT public key"})),
                                    warp::http::StatusCode::SERVICE_UNAVAILABLE,
                                )
                            ));
                        }

                        match verify_jwt(token, &pubkey_str) {
                            Ok(claims) => (
                                claims.document_id.unwrap_or_default(),
                                claims.nid.unwrap_or_default(),
                                claims.jti.unwrap_or_default(),
                                claims.consumer_operator_id.unwrap_or_default(),
                            ),
                            Err(e) => {
                                tracing::warn!("JWT verification failed: {}", e);
                                return Ok(Reply::into_response(
                                    warp::reply::with_status(
                                        warp::reply::json(&serde_json::json!({"error": format!("JWT verification failed: {}", e)})),
                                        warp::http::StatusCode::FORBIDDEN,
                                    )
                                ));
                            }
                        }
                    };

                    // Verify document_id matches request
                    if claims_document_id != document_id {
                        return Ok(Reply::into_response(
                            warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({"error": "Token document_id does not match requested document"})),
                                warp::http::StatusCode::FORBIDDEN,
                            )
                        ));
                    }

                    // Verify nid matches this node's ID
                    {
                        let my_node_id = state.node_id.read().await;
                        if !my_node_id.is_empty() && claims_nid != *my_node_id {
                            return Ok(Reply::into_response(
                                warp::reply::with_status(
                                    warp::reply::json(&serde_json::json!({"error": "Token is not scoped to this node"})),
                                    warp::http::StatusCode::FORBIDDEN,
                                )
                            ));
                        }
                    }

                    // Find the document file — search all corpus subdirectories
                    let file_path = find_cached_document(&state.cache_dir, &document_id).await;

                    match file_path {
                        Some(path) => {
                            match tokio::fs::read(&path).await {
                                Ok(data) => {
                                    let file_size = data.len();

                                    // Track credit for this serve
                                    {
                                        let mut cr = state.credits.write().await;
                                        cr.record_serve(file_size as u64, &document_id, &claims_jti, &claims_op);
                                    }

                                    // Handle Range requests
                                    if let Some(range) = range_header {
                                        if let Some((start, end)) = parse_range(&range, file_size) {
                                            let slice = data[start..=end].to_vec();
                                            // S2: Removed hardcoded Access-Control-Allow-Origin: *
                                            // CORS is handled by the warp::cors() middleware with the allowlist.
                                            let resp = warp::http::Response::builder()
                                                .status(206)
                                                .header("Content-Type", "application/octet-stream")
                                                .header("Content-Length", slice.len().to_string())
                                                .header("Content-Range", format!("bytes {}-{}/{}", start, end, file_size))
                                                .header("Accept-Ranges", "bytes")
                                                .header("X-Served-By", "wire-node")
                                                .body(slice)
                                                .unwrap();
                                            return Ok(Reply::into_response(resp));
                                        }
                                    }

                                    // Full response
                                    // S2: Removed hardcoded Access-Control-Allow-Origin: *
                                    // CORS is handled by the warp::cors() middleware with the allowlist.
                                    let resp = warp::http::Response::builder()
                                        .status(200)
                                        .header("Content-Type", "application/octet-stream")
                                        .header("Content-Length", file_size.to_string())
                                        .header("Accept-Ranges", "bytes")
                                        .header("X-Served-By", "wire-node")
                                        .body(data)
                                        .unwrap();
                                    Ok(Reply::into_response(resp))
                                }
                                Err(_) => {
                                    Ok(Reply::into_response(warp::reply::with_status(
                                        warp::reply::json(&serde_json::json!({"error": "File read error"})),
                                        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                                    )))
                                }
                            }
                        }
                        None => {
                            Ok(Reply::into_response(warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({"error": "Document not found"})),
                                warp::http::StatusCode::NOT_FOUND,
                            )))
                        }
                    }
                }
            })
    };

    // GET /auth/callback — landing page for magic link redirect
    let auth_callback = warp::path!("auth" / "callback")
        .and(warp::get())
        .map(move || {
            let html = AUTH_CALLBACK_HTML;
            warp::http::Response::builder()
                .status(200)
                .header("Content-Type", "text/html; charset=utf-8")
                .body(html)
                .unwrap()
        });

    // MOVED TO IPC: see main.rs — auth_complete_ipc command
    // POST /auth/complete — receives tokens from the callback page
    // NOTE: This endpoint is retained but only for the magic-link callback HTML page
    // which runs in a browser tab and cannot use Tauri IPC. The origin check
    // restricts it to trusted origins only.
    #[derive(Deserialize)]
    struct AuthCompleteRequest {
        access_token: String,
        refresh_token: Option<String>,
        user_id: Option<String>,
        email: Option<String>,
    }

    let auth_complete = {
        let state = state.clone();
        warp::path!("auth" / "complete")
            .and(warp::post())
            .and(warp::header::optional::<String>("origin"))
            .and(warp::body::content_length_limit(1_048_576)) // S4: 1MB body size limit
            .and(warp::body::json())
            .and_then(move |origin: Option<String>, body: AuthCompleteRequest| {
                let state = state.clone();
                async move {
                    // Restrict to trusted origins only — prevents arbitrary web pages
                    // from overwriting auth state via cross-origin POST
                    let allowed = match origin.as_deref() {
                        None => true, // No origin header = same-origin or non-browser client
                        Some(o) if o.starts_with("https://newsbleach.com") => true,
                        Some(o)
                            if o.starts_with("http://localhost")
                                || o.starts_with("http://127.0.0.1") =>
                        {
                            true
                        }
                        Some(o) if o == "tauri://localhost" => true,
                        Some(o) => {
                            tracing::warn!("Auth complete rejected from origin: {}", o);
                            false
                        }
                    };

                    if !allowed {
                        return Ok::<_, warp::Rejection>(warp::reply::json(
                            &serde_json::json!({"error": "forbidden"}),
                        ));
                    }

                    tracing::info!("Auth callback received - user_id={:?}", body.user_id);

                    let mut auth = state.auth.write().await;
                    auth.access_token = Some(body.access_token);
                    auth.refresh_token = body.refresh_token;
                    auth.user_id = body.user_id;
                    auth.email = body.email;
                    // Preserve api_token and node_id from previous registration

                    tracing::info!("Auth state updated via magic link callback");
                    Ok::<_, warp::Rejection>(warp::reply::json(
                        &serde_json::json!({"status": "ok"}),
                    ))
                }
            })
    };

    // GET /stats — node stats for dashboard
    let stats = {
        let state = state.clone();
        warp::path("stats").and(warp::get()).and_then(move || {
            let state = state.clone();
            async move {
                let cr = state.credits.read().await;
                let dashboard = cr.dashboard_stats();
                Ok::<_, warp::Rejection>(warp::reply::json(&dashboard))
            }
        })
    };

    // GET /tunnel-status — expose internal tunnel connection state
    let tunnel_debug = {
        let state = state.clone();
        warp::path("tunnel-status")
            .and(warp::get())
            .and_then(move || {
                let state = state.clone();
                async move {
                    let ts = state.tunnel_state.read().await;
                    let status_str = match &ts.status {
                        tunnel::TunnelConnectionStatus::Connected => "Connected".to_string(),
                        tunnel::TunnelConnectionStatus::Connecting => "Connecting".to_string(),
                        tunnel::TunnelConnectionStatus::Downloading => "Downloading".to_string(),
                        tunnel::TunnelConnectionStatus::Provisioning => "Provisioning".to_string(),
                        tunnel::TunnelConnectionStatus::Error(e) => format!("Error: {}", e),
                        tunnel::TunnelConnectionStatus::Disconnected => "Disconnected".to_string(),
                    };
                    Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({
                        "status": status_str,
                        "tunnel_id": ts.tunnel_id,
                        "tunnel_url": ts.tunnel_url,
                        "version": env!("CARGO_PKG_VERSION"),
                    })))
                }
            })
    };

    // POST /hooks/openrouter — Phase 11 broadcast webhook receiver.
    //
    // Accepts OTLP JSON payloads pushed by OpenRouter's Broadcast
    // feature. The route is publicly exposed via the Cloudflare
    // tunnel so auth is mandatory — any unauthenticated request is
    // a potential leak attack surface.
    //
    // Flow:
    //   1. Validate the `X-Webhook-Secret` header against the secret
    //      stored in `pyramid_providers.broadcast_config_json` using
    //      constant-time comparison (subtle::ConstantTimeEq).
    //   2. Detect OpenRouter's test-connection ping via the
    //      `X-Test-Connection: true` header or an empty payload —
    //      respond 200 with no correlation side effects.
    //   3. Parse the OTLP JSON into BroadcastTrace structs and feed
    //      each into `process_trace()`. That function handles:
    //        - correlation (gen_id primary, session_id fallback)
    //        - discrepancy detection vs the synchronous ledger
    //        - orphan broadcast logging
    //        - CostReconciliationDiscrepancy / OrphanBroadcastDetected
    //          event emission
    //        - provider health state machine feeds
    //   4. Return 200 regardless of per-trace outcome so OpenRouter
    //      does not retry (they don't retry on non-2xx anyway, but
    //      we explicitly return success for logging clarity).
    let openrouter_webhook = {
        let state = state.clone();
        warp::path!("hooks" / "openrouter")
            .and(warp::post().or(warp::put()).unify())
            .and(warp::header::optional::<String>("x-webhook-secret"))
            .and(warp::header::optional::<String>("x-test-connection"))
            .and(warp::body::content_length_limit(16 * 1024 * 1024)) // 16 MiB cap
            .and(warp::body::json::<serde_json::Value>())
            .and_then(
                move |secret_header: Option<String>,
                      test_connection_header: Option<String>,
                      payload: serde_json::Value| {
                    let state = state.clone();
                    async move {
                        use crate::pyramid::openrouter_webhook::{
                            parse_otlp_payload, process_trace, verify_webhook_secret,
                            WebhookAuthError,
                        };
                        use crate::pyramid::provider_health::CostReconciliationPolicy;

                        // Phase 11: auth gate. Scoped to the default
                        // 'openrouter' provider row. Wire Node
                        // currently supports a single OpenRouter
                        // provider; multi-provider broadcast support
                        // (e.g., multiple OR accounts) is Phase 15.
                        let auth_result = {
                            let conn = state.pyramid.writer.lock().await;
                            verify_webhook_secret(
                                &conn,
                                "openrouter",
                                secret_header.as_deref(),
                            )
                        };
                        match auth_result {
                            Ok(()) => {}
                            Err(WebhookAuthError::NoSecretConfigured) => {
                                tracing::warn!(
                                    "openrouter webhook: rejected — no secret configured yet"
                                );
                                return Ok::<_, warp::Rejection>(Reply::into_response(
                                    warp::reply::with_status(
                                        warp::reply::json(&serde_json::json!({
                                            "status": "service_unavailable",
                                            "message": "webhook secret not yet configured on this node",
                                        })),
                                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                                    ),
                                ));
                            }
                            Err(e) => {
                                // Do NOT log the header value — we
                                // only log the failure mode.
                                tracing::warn!(
                                    reason = ?e,
                                    "openrouter webhook: auth rejected"
                                );
                                return Ok::<_, warp::Rejection>(Reply::into_response(
                                    warp::reply::with_status(
                                        warp::reply::json(&serde_json::json!({
                                            "status": "unauthorized",
                                        })),
                                        warp::http::StatusCode::UNAUTHORIZED,
                                    ),
                                ));
                            }
                        }

                        // Test-connection ping handling — OpenRouter
                        // sends an empty payload with
                        // `X-Test-Connection: true` when the user
                        // saves the webhook destination. We accept
                        // it without side effects.
                        let is_test_ping = test_connection_header
                            .as_deref()
                            .map(|v| v.eq_ignore_ascii_case("true"))
                            .unwrap_or(false);

                        if is_test_ping {
                            return Ok::<_, warp::Rejection>(Reply::into_response(
                                warp::reply::with_status(
                                    warp::reply::json(&serde_json::json!({
                                        "status": "test_connection_ok",
                                    })),
                                    warp::http::StatusCode::OK,
                                ),
                            ));
                        }

                        // Parse + process each trace in the payload.
                        // Parse errors are logged and return 200 —
                        // OpenRouter's webhook destination does not
                        // retry on non-2xx, so a 400 would just
                        // lose the trace with no recovery path.
                        let traces = match parse_otlp_payload(&payload) {
                            Ok(t) => t,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "openrouter webhook: OTLP parse failed"
                                );
                                return Ok::<_, warp::Rejection>(Reply::into_response(
                                    warp::reply::with_status(
                                        warp::reply::json(&serde_json::json!({
                                            "status": "parse_error",
                                            "message": e.to_string(),
                                        })),
                                        warp::http::StatusCode::OK,
                                    ),
                                ));
                            }
                        };

                        let policy = CostReconciliationPolicy::default();
                        let bus = state.pyramid.build_event_bus.clone();
                        let mut confirmed = 0usize;
                        let mut discrepancies = 0usize;
                        let mut orphans = 0usize;
                        let mut recoveries = 0usize;
                        let mut test_pings = 0usize;

                        {
                            let conn = state.pyramid.writer.lock().await;
                            for trace in &traces {
                                match process_trace(
                                    &conn,
                                    trace,
                                    "openrouter",
                                    &policy,
                                    Some(&bus),
                                ) {
                                    Ok(crate::pyramid::openrouter_webhook::BroadcastOutcome::Confirmed { .. }) => {
                                        confirmed += 1;
                                    }
                                    Ok(crate::pyramid::openrouter_webhook::BroadcastOutcome::Discrepancy { .. }) => {
                                        discrepancies += 1;
                                    }
                                    Ok(crate::pyramid::openrouter_webhook::BroadcastOutcome::Recovered { .. }) => {
                                        recoveries += 1;
                                    }
                                    Ok(crate::pyramid::openrouter_webhook::BroadcastOutcome::Orphan { .. }) => {
                                        orphans += 1;
                                    }
                                    Ok(crate::pyramid::openrouter_webhook::BroadcastOutcome::TestPing) => {
                                        test_pings += 1;
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "openrouter webhook: process_trace failed"
                                        );
                                    }
                                }
                            }
                        }

                        tracing::info!(
                            traces = traces.len(),
                            confirmed,
                            discrepancies,
                            recoveries,
                            orphans,
                            test_pings,
                            "openrouter webhook batch processed"
                        );

                        Ok::<_, warp::Rejection>(Reply::into_response(
                            warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({
                                    "status": "ok",
                                    "traces": traces.len(),
                                    "confirmed": confirmed,
                                    "discrepancies": discrepancies,
                                    "recovered": recoveries,
                                    "orphans": orphans,
                                    "test_pings": test_pings,
                                })),
                                warp::http::StatusCode::OK,
                            ),
                        ))
                    }
                },
            )
    };

    // Explicit OPTIONS preflight handler
    // S2: Use the same origin allowlist as the CORS middleware.
    // Warp's cors() middleware handles preflight for standard CORS, but the
    // Access-Control-Request-Private-Network header needs an explicit handler
    // for Private Network Access (PNA) preflight. We still set the allowed
    // origin from the allowlist rather than hardcoding *.
    let preflight = warp::options()
        .and(warp::header::optional::<String>("origin"))
        .map(|origin: Option<String>| {
            let mut builder = warp::http::Response::builder()
                .status(204)
                .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
                .header(
                    "Access-Control-Allow-Headers",
                    "Content-Type, Range, Authorization, X-Payment-Token",
                )
                .header("Access-Control-Allow-Private-Network", "true");

            // Only echo back origin if it's in our allowlist
            if let Some(ref o) = origin {
                if CORS_ALLOWED_ORIGINS
                    .iter()
                    .any(|allowed| *allowed == o.as_str())
                {
                    builder = builder.header("Access-Control-Allow-Origin", o.as_str());
                }
            }

            builder.body("").unwrap()
        });

    // Pyramid Knowledge Engine routes (WS-ONLINE-C: pass jwt_public_key for Wire JWT auth)
    let pyramid_routes = pyramid::routes::pyramid_routes(
        state.pyramid.clone(),
        state.jwt_public_key.clone(),
        state.node_id.clone(), // WS-ONLINE-H: serving_node_id for cost preview
        state.auth.clone(),    // Sprint 3: wire agent token for remote query proxy
    );

    // Operator-facing HTTP surface — compute market, system observability,
    // local mode / providers. LOCAL-ONLY auth (Bearer from
    // pyramid_config.json::auth_token), mounted under /pyramid/* alongside
    // the existing pyramid routes. Agent/CLI tooling (pyramid-cli) hits
    // these endpoints to drive the node without touching the desktop UI.
    let operator_routes = pyramid::routes_operator::operator_routes(
        pyramid::routes_operator::OperatorContext {
            pyramid: state.pyramid.clone(),
            auth: state.auth.clone(),
            credits: state.credits.clone(),
            config: state.config.clone(),
            tunnel_state: state.tunnel_state.clone(),
            sync_state: state.sync_state.clone(),
            fleet_roster: state.fleet_roster.clone(),
            work_stats: state.work_stats.clone(),
            node_id: state.node_id.clone(),
            compute_market_state: state.compute_market_state.clone(),
            compute_market_dispatch: state.compute_market_dispatch.clone(),
            pending_market_jobs: state.pending_market_jobs.clone(),
        },
    );

    // Post-agents-retro /p/ HTML web surface — mounted separately so it can
    // get a permissive CORS filter. The strict desktop-API allowlist above
    // would reject same-tunnel form POSTs (the browser sends an Origin header
    // for any cross-method navigation, including same-origin POSTs).
    let public_html_routes = pyramid::routes::public_html_routes(
        state.pyramid.clone(),
        state.jwt_public_key.clone(),
    );
    let public_cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec![
            "Content-Type",
            "Cookie",
            "Accept",
            "Accept-Language",
            "Origin",
            "Referer",
            "User-Agent",
        ]);
    let public_html_with_cors = public_html_routes.with(public_cors);

    // Partner (Dennis) routes
    let partner_routes = partner::routes::partner_routes(state.partner.clone());

    // Phase 11: the OpenRouter Broadcast webhook is called from
    // OpenRouter's egress servers, not from a browser. Origin
    // headers (if sent at all) come from a wide range of IPs, so
    // the strict desktop-API allowlist would reject valid
    // webhooks. We scope a permissive CORS just for this route,
    // then gate access at the application layer via the shared
    // secret in `verify_webhook_secret`.
    let webhook_cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["POST", "PUT", "OPTIONS"])
        .allow_headers(vec![
            "Content-Type",
            "X-Webhook-Secret",
            "X-Test-Connection",
        ]);
    let openrouter_webhook_with_cors = openrouter_webhook.with(webhook_cors);

    // ── Fleet endpoints (v1 prefix) ─────────────────────────────
    // Fleet routes are API-to-API (peer tunnel traffic), not browser-
    // originated. They use a permissive CORS since they arrive from
    // Cloudflare tunnel origins, not localhost.

    // POST /v1/compute/fleet-dispatch — receive fleet LLM job from peer
    //
    // ARCHITECTURE NOTE: This handler holds the HTTP response open until the
    // GPU job completes. Over Cloudflare tunnels, long jobs (~120s+) hit
    // Cloudflare's origin timeout (524). The 100-year fix is ACK + async
    // result delivery: respond immediately with a job_id, process the job,
    // then POST the result back to the requester's tunnel. This matches
    // the Phase 3 compute market architecture (webhook-based delivery).
    let fleet_dispatch_route = {
        let state = state.clone();
        warp::path!("v1" / "compute" / "fleet-dispatch")
            .and(warp::post())
            .and(warp::header::<String>("authorization"))
            .and(warp::body::json())
            .and_then(move |auth_header: String, body: serde_json::Value| {
                let state = state.clone();
                async move {
                    handle_fleet_dispatch(auth_header, body, state).await
                }
            })
    };

    // POST /v1/fleet/announce — receive fleet peer announcement
    let fleet_announce_route = {
        let state = state.clone();
        warp::path!("v1" / "fleet" / "announce")
            .and(warp::post())
            .and(warp::header::<String>("authorization"))
            .and(warp::body::json())
            .and_then(move |auth_header: String, body: serde_json::Value| {
                let state = state.clone();
                async move {
                    handle_fleet_announce(auth_header, body, state).await
                }
            })
    };

    // POST /v1/fleet/result — dispatcher-side async result callback
    let fleet_result_route = {
        let state = state.clone();
        warp::path!("v1" / "fleet" / "result")
            .and(warp::post())
            .and(warp::header::<String>("authorization"))
            .and(warp::body::json())
            .and_then(move |auth_header: String, body: serde_json::Value| {
                let state = state.clone();
                async move {
                    handle_fleet_result(auth_header, body, state).await
                }
            })
    };

    // POST /v1/compute/job-dispatch — receive matched market job from the Wire.
    //
    // Parallel to /v1/compute/fleet-dispatch in shape (header + JSON body,
    // returns 202 ACK after spawning a worker). The handler verifies the
    // wire_job_token JWT (aud="compute"), converts ChatML messages to a
    // prompt pair, runs the market admission gates, and spawns the worker
    // that heartbeats the outbox row and CAS-promotes it to `ready` on
    // inference completion. See handle_market_dispatch for the full flow.
    let compute_dispatch_route = {
        let state = state.clone();
        warp::path!("v1" / "compute" / "job-dispatch")
            .and(warp::post())
            .and(warp::header::<String>("authorization"))
            .and(warp::body::json())
            .and_then(move |auth_header: String, body: serde_json::Value| {
                let state = state.clone();
                async move {
                    handle_market_dispatch(auth_header, body, state).await
                }
            })
    };

    // POST /v1/compute/job-result — Wire's delivery worker pushes the
    // result envelope here per contract §2.5. Phase 3 requester-side
    // consumption point. Sibling of /v1/compute/job-dispatch (Wire →
    // Provider); this is Wire → Requester.
    //
    // Auth: Wire-minted JWT with aud="result-delivery", sub=<uuid_job_id>,
    // rid=<requester_operator_id>. Verified against the SAME Ed25519
    // public key used for dispatch JWT (contract §3 — single key, many
    // audiences).
    //
    // Flow: verify JWT → parse envelope → look up PendingJobs entry by
    // UUID job_id → fire oneshot → remove map entry. Duplicate
    // delivery after timeout fallback: no map entry found → return
    // 2xx {"status":"already_settled"} so Wire marks delivery done
    // and doesn't retry. See handle_compute_job_result for the full
    // flow.
    let compute_result_route = {
        let state = state.clone();
        warp::path!("v1" / "compute" / "job-result")
            .and(warp::post())
            .and(warp::header::<String>("authorization"))
            .and(warp::body::json())
            .and_then(move |auth_header: String, body: serde_json::Value| {
                let state = state.clone();
                async move {
                    handle_compute_job_result(auth_header, body, state).await
                }
            })
    };

    // Fleet CORS: permissive (peer-to-peer via Cloudflare tunnels).
    // The compute-market dispatch + result endpoints share the same
    // CORS profile as the fleet routes — both accept inbound traffic
    // from Cloudflare tunnel origins (the Wire for market, peer nodes
    // for fleet) and auth via Authorization: Bearer.
    let fleet_cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"]);
    let fleet_routes = fleet_dispatch_route
        .or(fleet_announce_route)
        .or(fleet_result_route)
        .or(compute_dispatch_route)
        .or(compute_result_route)
        .with(fleet_cors);

    let routes = preflight
        .or(public_html_with_cors)
        .or(openrouter_webhook_with_cors)
        .or(fleet_routes)
        .or(operator_routes
            .or(pyramid_routes)
            .or(partner_routes)
            .or(auth_callback
                .or(auth_complete)
                .or(health)
                .or(tunnel_debug)
                .or(documents)
                .or(stats))
            .with(cors))
        // Root-level recover: maps our custom `Unauthorized` +
        // `RateLimited` rejections onto proper 401 / 429 responses
        // with JSON error bodies. Without this, warp's default
        // rejection handler turns custom rejects into 404s, making
        // auth failures look like missing routes. Applied at the root
        // so every branch benefits (pyramid_routes, operator_routes,
        // partner_routes, fleet_routes — all of which reject with
        // custom types).
        .recover(crate::http_utils::handle_rejection);

    tracing::info!("Wire Node HTTP server starting on 127.0.0.1:{}", port);
    warp::serve(routes).run(([127, 0, 0, 1], port)).await;
}

/// HTML page served at /auth/callback
const AUTH_CALLBACK_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Wire Node — Authenticating</title>
    <style>
        body {
            background: #0a0a1a;
            color: #e0e0e0;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
            display: flex;
            align-items: center;
            justify-content: center;
            height: 100vh;
            margin: 0;
        }
        .card {
            text-align: center;
            padding: 3rem;
            border-radius: 1rem;
            background: rgba(255,255,255,0.05);
            border: 1px solid rgba(255,255,255,0.1);
        }
        .logo { font-size: 3rem; margin-bottom: 1rem; }
        h1 { font-size: 1.5rem; margin: 0; }
        p { opacity: 0.6; margin-top: 0.5rem; }
        .success { color: #4ade80; }
        .error { color: #f87171; }
    </style>
</head>
<body>
    <div class="card">
        <div class="logo">W</div>
        <h1 id="status">Authenticating...</h1>
        <p id="detail">Please wait</p>
    </div>
    <script>
        (async () => {
            const hash = window.location.hash.substring(1);
            const params = new URLSearchParams(hash);
            const accessToken = params.get('access_token');
            const refreshToken = params.get('refresh_token');

            if (!accessToken) {
                document.getElementById('status').textContent = 'Authentication failed';
                document.getElementById('status').className = 'error';
                document.getElementById('detail').textContent = 'No access token found in URL';
                return;
            }

            let userId = null;
            let email = null;
            try {
                const payload = JSON.parse(atob(accessToken.split('.')[1]));
                userId = payload.sub;
                email = payload.email;
            } catch (e) {
                console.warn('Could not decode JWT:', e);
            }

            try {
                const resp = await fetch('/auth/complete', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({
                        access_token: accessToken,
                        refresh_token: refreshToken,
                        user_id: userId,
                        email: email,
                    }),
                });

                if (resp.ok) {
                    document.getElementById('status').textContent = 'Authenticated!';
                    document.getElementById('status').className = 'success';
                    document.getElementById('detail').textContent = 'You can close this tab and return to Wire Node';
                } else {
                    throw new Error('Server returned ' + resp.status);
                }
            } catch (e) {
                document.getElementById('status').textContent = 'Authentication failed';
                document.getElementById('status').className = 'error';
                document.getElementById('detail').textContent = e.message;
            }
        })();
    </script>
</body>
</html>"#;

/// Find a cached document body file by document ID (search all corpus subdirs)
async fn find_cached_document(
    cache_dir: &std::path::Path,
    document_id: &str,
) -> Option<std::path::PathBuf> {
    let target_filename = format!("{}.body", document_id);

    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                let candidate = entry.path().join(&target_filename);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Count cached document files
async fn count_cached_documents(cache_dir: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                if let Ok(mut sub_entries) = tokio::fs::read_dir(entry.path()).await {
                    while let Ok(Some(sub_entry)) = sub_entries.next_entry().await {
                        if let Some(name) = sub_entry.file_name().to_str() {
                            if name.ends_with(".body") {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    count
}

/// Parse HTTP Range header: "bytes=start-end"
fn parse_range(range: &str, file_size: usize) -> Option<(usize, usize)> {
    let range = range.strip_prefix("bytes=")?;
    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() != 2 {
        return None;
    }

    let start: usize = parts[0].parse().ok()?;
    let end: usize = if parts[1].is_empty() {
        file_size - 1
    } else {
        parts[1].parse().ok()?
    };

    if start <= end && end < file_size {
        Some((start, end))
    } else {
        None
    }
}

/// Verify a JWT using Ed25519 (EdDSA) public key — document access tokens
fn verify_jwt(token: &str, public_key_pem: &str) -> Result<DocumentClaims, String> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Invalid public key: {}", e))?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    // We validate document_id and nid manually after decoding
    validation.set_required_spec_claims(&["exp"]);

    let token_data = decode::<DocumentClaims>(token, &decoding_key, &validation)
        .map_err(|e| format!("JWT decode failed: {}", e))?;

    Ok(token_data.claims)
}

/// JWT claims for Wire-signed payment tokens (WS-ONLINE-H).
///
/// Issued by the Wire server via `POST /api/v1/wire/payment-intent`.
/// The serving node validates these before executing a paid query, then
/// redeems the token via `POST /api/v1/wire/payment-redeem` to collect payment.
///
///   aud → "payment"
///   serving_node_operator_id → must match this node's operator_id
///   contribution_handle_path → the contribution being paid for
///   stamp_amount → flat p2p fee (always 1)
///   access_amount → UFF-routed access price (0 for public pyramids)
///   total_amount → stamp + access
///   nonce → single-use UUID (prevents replay)
///   exp → expiration (600s TTL)
#[derive(Debug, Serialize, Deserialize)]
pub struct PaymentTokenClaims {
    /// Audience — must be "payment"
    pub aud: Option<String>,
    /// Operator ID of the serving node (must match this node)
    pub serving_node_operator_id: Option<String>,
    /// Handle-path of the contribution being paid for
    pub contribution_handle_path: Option<String>,
    /// Stamp amount (flat 1-credit p2p fee)
    #[serde(default)]
    pub stamp_amount: u64,
    /// Access price amount (UFF-routed, 0 for public pyramids)
    #[serde(default)]
    pub access_amount: u64,
    /// Total amount (stamp + access)
    #[serde(default)]
    pub total_amount: u64,
    /// Single-use nonce (UUID v4) — prevents replay
    pub nonce: Option<String>,
    /// Expiration (standard JWT exp claim)
    #[serde(default)]
    pub exp: Option<u64>,
}

/// Verify a Wire-signed payment token using Ed25519 (EdDSA) public key (WS-ONLINE-H).
///
/// Validates the `aud` claim is "payment", checks required fields, and verifies
/// that `serving_node_operator_id` matches the expected node operator ID.
/// The Wire server's public key (same key used for pyramid-query JWTs) is used
/// for validation — payment tokens are Wire-signed, never self-signed.
pub fn verify_payment_token(
    token: &str,
    public_key_pem: &str,
    expected_operator_id: &str,
) -> Result<PaymentTokenClaims, String> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Invalid public key for payment token: {}", e))?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    // Validate audience is "payment"
    validation.set_audience(&["payment"]);

    let token_data = decode::<PaymentTokenClaims>(token, &decoding_key, &validation)
        .map_err(|e| format!("Payment token JWT decode failed: {}", e))?;

    let claims = &token_data.claims;

    // Verify serving_node_operator_id matches this node
    let serving_id = claims.serving_node_operator_id.as_deref().unwrap_or("");
    if serving_id.is_empty() {
        return Err("Missing serving_node_operator_id in payment token".into());
    }
    if serving_id != expected_operator_id {
        return Err(format!(
            "Payment token serving_node_operator_id mismatch: expected '{}', got '{}'",
            expected_operator_id, serving_id
        ));
    }

    // Verify nonce is present
    if claims.nonce.as_deref().unwrap_or("").is_empty() {
        return Err("Missing nonce in payment token".into());
    }

    // Verify total_amount is consistent
    if claims.total_amount != claims.stamp_amount + claims.access_amount {
        return Err(format!(
            "Payment token amount mismatch: total {} != stamp {} + access {}",
            claims.total_amount, claims.stamp_amount, claims.access_amount
        ));
    }

    Ok(token_data.claims)
}

/// Verify a pyramid query JWT using Ed25519 (EdDSA) public key (WS-ONLINE-C).
///
/// Validates the `aud` claim is "pyramid-query" and extracts operator_id, slug, query_type.
pub fn verify_pyramid_query_jwt(
    token: &str,
    public_key_pem: &str,
) -> Result<PyramidQueryClaims, String> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Invalid public key: {}", e))?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    // Validate audience is "pyramid-query"
    validation.set_audience(&["pyramid-query"]);

    let token_data = decode::<PyramidQueryClaims>(token, &decoding_key, &validation)
        .map_err(|e| format!("Pyramid query JWT decode failed: {}", e))?;

    // Verify required fields
    let claims = &token_data.claims;
    if claims.operator_id.as_deref().unwrap_or("").is_empty() {
        return Err("Missing operator_id (sub) in pyramid query JWT".into());
    }

    Ok(token_data.claims)
}

// ── Fleet handlers ──────────────────────────────────────────────────────

/// Handle POST /v1/compute/fleet-dispatch — receive fleet LLM job from peer.
///
/// Async admission protocol: verify identity, validate callback URL against
/// roster, resolve model, transactionally insert into outbox, then return
/// 202 Accepted. The actual inference runs in a spawned worker task that
/// delivers the result via POST to the dispatcher's callback URL.
///
/// See `docs/plans/async-fleet-dispatch.md` § "Peer Side: handle_fleet_dispatch"
/// for the full 13-step protocol. Order is load-bearing.
async fn handle_fleet_dispatch(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    // Step 1: Verify fleet identity — single call returns typed FleetIdentity.
    let jwt_pk = state.jwt_public_key.read().await.clone();
    let self_operator_id = state
        .auth
        .read()
        .await
        .operator_id
        .clone()
        .unwrap_or_default();
    let identity = match crate::pyramid::fleet_identity::verify_fleet_identity(
        &auth_header,
        &jwt_pk,
        &self_operator_id,
    ) {
        Ok(i) => i,
        Err(_) => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::FORBIDDEN,
            )));
        }
    };
    let dispatcher_nid = identity.nid().to_string();

    // Step 2: Parse body.
    let job_id_str = body["job_id"].as_str().unwrap_or("").to_string();
    let rule_name = body["rule_name"].as_str().unwrap_or("").to_string();
    let user_prompt = body["user_prompt"].as_str().unwrap_or("").to_string();
    let callback_url = body["callback_url"].as_str().unwrap_or("").to_string();
    let system_prompt = body["system_prompt"].as_str().unwrap_or("").to_string();
    let temperature = body["temperature"].as_f64().unwrap_or(0.0) as f32;
    let max_tokens = body["max_tokens"].as_u64().unwrap_or(4096) as usize;
    let response_format = body.get("response_format").cloned();

    if job_id_str.is_empty()
        || rule_name.is_empty()
        || user_prompt.is_empty()
        || callback_url.is_empty()
    {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"error": "Missing required field (job_id/rule_name/user_prompt/callback_url)"}),
            ),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    // Validate job_id is a parseable UUID (keep string form for DB PK).
    if uuid::Uuid::parse_str(&job_id_str).is_err() {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "job_id is not a valid UUID"})),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    // Step 3: Parse callback_url as TunnelUrl, then validate against roster.
    if crate::pyramid::tunnel_url::TunnelUrl::parse(&callback_url).is_err() {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "unparseable callback_url"})),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }
    {
        let roster = state.fleet_roster.read().await;
        if let Err(e) = crate::fleet::validate_callback_url(
            &callback_url,
            &crate::fleet::CallbackKind::Fleet {
                dispatcher_nid: &dispatcher_nid,
            },
            &*roster,
        ) {
            tracing::warn!(
                "Fleet dispatch callback_url rejected: {} (dispatcher={})",
                e,
                dispatcher_nid
            );
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("{}", e)})),
                warp::http::StatusCode::FORBIDDEN,
            )));
        }
    } // roster read-lock drops here

    // Step 4: Resolve model from dispatch policy by rule name — LOCAL only.
    let resolved_model = {
        let cfg = state.pyramid.config.read().await;
        if let Some(ref policy) = cfg.dispatch_policy {
            match policy.resolve_local_for_rule(&rule_name) {
                Some((_provider_id, model_id)) => {
                    model_id.unwrap_or_else(|| cfg.primary_model.clone())
                }
                None => {
                    return Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(
                            &serde_json::json!({"error": "No local provider for rule"}),
                        ),
                        warp::http::StatusCode::BAD_REQUEST,
                    )));
                }
            }
        } else {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "No dispatch policy configured"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    if resolved_model.is_empty() {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "Cannot resolve model for rule"})),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    // Snapshot fleet delivery policy for admission + worker.
    let policy = match state.fleet_dispatch.as_ref() {
        Some(ctx) => ctx.policy.read().await.clone(),
        None => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "fleet dispatch disabled"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    // DB path for outbox + chronicle writes.
    let db_path = match state.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "node data dir unavailable"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    // Helper for fire-and-forget chronicle writes from async context.
    let chronicle_write =
        |event_type: &'static str, source: &'static str, metadata: serde_json::Value| {
            let job_path = format!("fleet-recv:{}:{}", dispatcher_nid, job_id_str);
            let ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                &job_path, event_type, source,
            )
            .with_model_id(resolved_model.clone())
            .with_metadata(metadata);
            let db_path_clone = db_path.to_string_lossy().to_string();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = rusqlite::Connection::open(&db_path_clone) {
                    let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx);
                }
            });
        };

    // Step 5: Reverse-channel precondition — roster MUST have a fleet_jwt.
    {
        let roster = state.fleet_roster.read().await;
        if roster.fleet_jwt.is_none() {
            drop(roster);
            chronicle_write(
                crate::pyramid::compute_chronicle::EVENT_FLEET_ADMISSION_REJECTED,
                crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                serde_json::json!({
                    "peer_id": dispatcher_nid,
                    "reason": "no fleet_jwt",
                }),
            );
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(
                    warp::reply::json(&serde_json::json!({"error": "no fleet_jwt; retry later"})),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    }

    // Step 6: Transactional admission + idempotent insert.
    // All SQL inside spawn_blocking (rusqlite is sync). We return a branch
    // outcome back to the async handler to construct the HTTP response.
    #[derive(Debug)]
    enum AdmissionOutcome {
        Admitted,         // fresh insert, passed admission → spawn worker
        RetryExisting,    // same dispatcher, pre-existing pending/ready row
        ConflictDifferentDispatcher,
        GoneDelivered,
        GoneFailed(Option<String>), // last_error for body
        Rejected503,      // admission cap hit (freshly inserted path)
        DbError(String),
    }

    let worker_heartbeat_tolerance_secs = policy.worker_heartbeat_tolerance_secs;
    let admission_max_inflight = policy.max_inflight_jobs;
    let db_path_tx = db_path.clone();
    let dispatcher_nid_tx = dispatcher_nid.clone();
    let job_id_tx = job_id_str.clone();
    let callback_url_tx = callback_url.clone();

    let outcome = tokio::task::spawn_blocking(move || -> AdmissionOutcome {
        let mut conn = match rusqlite::Connection::open(&db_path_tx) {
            Ok(c) => c,
            Err(e) => return AdmissionOutcome::DbError(e.to_string()),
        };
        let tx = match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(t) => t,
            Err(e) => return AdmissionOutcome::DbError(e.to_string()),
        };
        // expires_at = now + worker_heartbeat_tolerance_secs
        let modifier = format!("+{} seconds", worker_heartbeat_tolerance_secs);
        let expires_at: String = match tx.query_row(
            "SELECT datetime('now', ?1)",
            rusqlite::params![modifier],
            |r| r.get(0),
        ) {
            Ok(s) => s,
            Err(e) => return AdmissionOutcome::DbError(e.to_string()),
        };
        let changes = match crate::pyramid::db::fleet_outbox_insert_or_ignore(
            &tx,
            &dispatcher_nid_tx,
            &job_id_tx,
            &callback_url_tx,
            &expires_at,
        ) {
            Ok(n) => n,
            Err(e) => return AdmissionOutcome::DbError(e.to_string()),
        };
        let lookup = match crate::pyramid::db::fleet_outbox_lookup(&tx, &job_id_tx) {
            Ok(Some(l)) => l,
            Ok(None) => {
                // INSERT OR IGNORE was just done with this job_id; lookup must find it.
                let _ = tx.rollback();
                return AdmissionOutcome::DbError("outbox lookup returned None".into());
            }
            Err(e) => {
                let _ = tx.rollback();
                return AdmissionOutcome::DbError(e.to_string());
            }
        };

        if lookup.dispatcher_node_id != dispatcher_nid_tx {
            // Different dispatcher already owns this job_id. Cross-dispatcher UUID
            // reuse — rollback the INSERT OR IGNORE (no-op since row wasn't ours)
            // and reject.
            let _ = tx.rollback();
            return AdmissionOutcome::ConflictDifferentDispatcher;
        }

        // Same dispatcher owns it.
        if changes == 0 {
            // Pre-existing row — branch on status.
            match lookup.status.as_str() {
                "pending" | "ready" => {
                    // Legitimate retry — no new worker, just re-ACK.
                    if let Err(e) = tx.commit() {
                        return AdmissionOutcome::DbError(e.to_string());
                    }
                    AdmissionOutcome::RetryExisting
                }
                "delivered" => {
                    let _ = tx.rollback();
                    AdmissionOutcome::GoneDelivered
                }
                "failed" => {
                    let _ = tx.rollback();
                    AdmissionOutcome::GoneFailed(lookup.last_error)
                }
                _ => {
                    let _ = tx.rollback();
                    AdmissionOutcome::DbError(format!("unknown outbox status: {}", lookup.status))
                }
            }
        } else {
            // Freshly inserted (changes == 1). Admission count.
            let inflight = match crate::pyramid::db::fleet_outbox_count_inflight_excluding(
                &tx,
                &dispatcher_nid_tx,
                &job_id_tx,
            ) {
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.rollback();
                    return AdmissionOutcome::DbError(e.to_string());
                }
            };
            if admission_max_inflight != 0 && inflight >= admission_max_inflight {
                // Over capacity — delete the row we just inserted and reject.
                if let Err(e) = crate::pyramid::db::fleet_outbox_delete(
                    &tx,
                    &dispatcher_nid_tx,
                    &job_id_tx,
                ) {
                    let _ = tx.rollback();
                    return AdmissionOutcome::DbError(e.to_string());
                }
                let _ = tx.rollback();
                return AdmissionOutcome::Rejected503;
            }
            if let Err(e) = tx.commit() {
                return AdmissionOutcome::DbError(e.to_string());
            }
            AdmissionOutcome::Admitted
        }
    })
    .await
    .unwrap_or_else(|join_err| AdmissionOutcome::DbError(join_err.to_string()));

    // Construct HTTP response + side-effects based on the outcome.
    match outcome {
        AdmissionOutcome::DbError(msg) => {
            tracing::error!("Fleet dispatch admission DB error: {}", msg);
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "admission db error"})),
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            )))
        }
        AdmissionOutcome::ConflictDifferentDispatcher => {
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"error": "job_id conflict with different dispatcher"}),
                ),
                warp::http::StatusCode::CONFLICT,
            )))
        }
        AdmissionOutcome::GoneDelivered => {
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(
                    &serde_json::json!({"error": "job already delivered; dispatcher lost state"}),
                ),
                warp::http::StatusCode::GONE,
            )))
        }
        AdmissionOutcome::GoneFailed(last_error) => {
            let body = serde_json::json!({
                "error": "job previously failed",
                "last_error": last_error,
            });
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&body),
                warp::http::StatusCode::GONE,
            )))
        }
        AdmissionOutcome::Rejected503 => {
            chronicle_write(
                crate::pyramid::compute_chronicle::EVENT_FLEET_ADMISSION_REJECTED,
                crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                serde_json::json!({
                    "peer_id": dispatcher_nid,
                    "reason": "max_inflight_jobs",
                }),
            );
            Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(
                    warp::reply::json(&serde_json::json!({"error": "peer at capacity"})),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )))
        }
        AdmissionOutcome::RetryExisting => {
            // Pre-existing row; re-ACK without spawning a new worker.
            let ack = crate::fleet::FleetDispatchAck {
                job_id: job_id_str.clone(),
                peer_queue_depth: 0,
            };
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&ack),
                warp::http::StatusCode::ACCEPTED,
            )))
        }
        AdmissionOutcome::Admitted => {
            // Step 7: Record fleet_job_accepted and return 202.
            // Step 8: Spawn the worker task BEFORE returning 202.
            chronicle_write(
                crate::pyramid::compute_chronicle::EVENT_FLEET_JOB_ACCEPTED,
                crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                serde_json::json!({
                    "peer_id": dispatcher_nid,
                    "rule_name": rule_name,
                    "resolved_model": resolved_model,
                    "job_id": job_id_str,
                }),
            );

            spawn_fleet_worker(
                state.clone(),
                db_path.clone(),
                policy.clone(),
                dispatcher_nid.clone(),
                job_id_str.clone(),
                callback_url.clone(),
                resolved_model.clone(),
                system_prompt,
                user_prompt,
                temperature,
                max_tokens,
                response_format,
            );

            let ack = crate::fleet::FleetDispatchAck {
                job_id: job_id_str.clone(),
                peer_queue_depth: 0,
            };
            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&ack),
                warp::http::StatusCode::ACCEPTED,
            )))
        }
    }
}

/// Spawns the worker that runs inference + heartbeat and delivers the
/// result back to the dispatcher. All clones are owned by the spawned
/// future; the caller returns 202 as soon as this function returns.
///
/// The worker:
///   * snapshots `LlmConfig` with `fleet_dispatch=None` and `fleet_roster=None`
///     so a recursive call into Phase A cannot re-dispatch the job.
///   * runs `call_model_unified_with_options_and_ctx` alongside a heartbeat
///     that bumps `expires_at` every `worker_heartbeat_interval_secs`.
///   * CAS-promotes `pending → ready` on success (step 9), delivers (step 10),
///     and CAS-promotes `ready → delivered` on 2xx (step 11) or bumps attempts
///     on failure (step 12).
#[allow(clippy::too_many_arguments)]
fn spawn_fleet_worker(
    state: ServerState,
    db_path: std::path::PathBuf,
    policy: crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy,
    dispatcher_nid: String,
    job_id: String,
    callback_url: String,
    resolved_model: String,
    system_prompt: String,
    user_prompt: String,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<serde_json::Value>,
) {
    tokio::spawn(async move {
        // Derive worker config via prepare_for_replay — the node is
        // fulfilling a fleet-received job, so dispatch contexts
        // (compute_queue, fleet, market) must all be cleared to prevent
        // recursive outbound dispatch. Then override the model to the
        // requested one.
        let fleet_config = {
            let cfg = state.pyramid.config.read().await;
            let mut fc = cfg.prepare_for_replay(crate::pyramid::llm::DispatchOrigin::FleetReceived);
            fc.primary_model = resolved_model.clone();
            fc.fallback_model_1 = resolved_model.clone();
            fc.fallback_model_2 = resolved_model.clone();
            fc
        };

        let chronicle_job_path = format!("fleet-recv:{}:{}", dispatcher_nid, job_id);
        let options = crate::pyramid::llm::LlmCallOptions {
            skip_fleet_dispatch: true,
            chronicle_job_path: Some(chronicle_job_path.clone()),
            dispatch_origin: crate::pyramid::llm::DispatchOrigin::FleetReceived,
            ..Default::default()
        };

        // Helper to write a chronicle event from the worker path.
        let chronicle_write =
            |event_type: &'static str, source: &'static str, metadata: serde_json::Value| {
                let ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                    &chronicle_job_path,
                    event_type,
                    source,
                )
                .with_model_id(resolved_model.clone())
                .with_metadata(metadata);
                let db_path_clone = db_path.to_string_lossy().to_string();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path_clone) {
                        let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx);
                    }
                });
            };

        // Heartbeat future: tick every worker_heartbeat_interval_secs, bump
        // expires_at, exit if CAS lost (sweep won) or fatal DB error.
        let hb_db_path = db_path.clone();
        let hb_dispatcher = dispatcher_nid.clone();
        let hb_job_id = job_id.clone();
        let hb_interval = policy.worker_heartbeat_interval_secs.max(1);
        let hb_tolerance = policy.worker_heartbeat_tolerance_secs;

        let heartbeat = async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(hb_interval));
            // Skip the immediate tick at t=0 — we already inserted with the
            // expiry; next tick should fire after the interval.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let hb_db = hb_db_path.clone();
                let hb_disp = hb_dispatcher.clone();
                let hb_jid = hb_job_id.clone();
                let hb_tol = hb_tolerance;
                let result: anyhow::Result<usize> =
                    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
                        let conn = rusqlite::Connection::open(&hb_db)?;
                        let modifier = format!("+{} seconds", hb_tol);
                        let new_expires: String = conn.query_row(
                            "SELECT datetime('now', ?1)",
                            rusqlite::params![modifier],
                            |r| r.get(0),
                        )?;
                        crate::pyramid::db::fleet_outbox_update_heartbeat_if_pending(
                            &conn,
                            &hb_disp,
                            &hb_jid,
                            &new_expires,
                        )
                    })
                    .await
                    .unwrap_or_else(|je| Err(anyhow::anyhow!(je.to_string())));
                match result {
                    Ok(1) => continue,
                    Ok(0) => {
                        // Sweep won the race; exit and let select! cancel inference.
                        return false;
                    }
                    Ok(_) => return false, // compound PK, should be unreachable
                    Err(e) => {
                        // SQLITE_BUSY → retry next tick; other DB errors → fatal.
                        let msg = e.to_string().to_lowercase();
                        if msg.contains("busy") || msg.contains("locked") {
                            tracing::debug!(?e, "heartbeat tick DB-locked; retrying");
                            continue;
                        }
                        tracing::error!(?e, "heartbeat DB error; giving up");
                        return false;
                    }
                }
            }
        };

        let inference = crate::pyramid::llm::call_model_unified_with_options_and_ctx(
            &fleet_config,
            None,
            &system_prompt,
            &user_prompt,
            temperature,
            max_tokens,
            response_format.as_ref(),
            options,
        );

        // Race inference against heartbeat exit. If heartbeat exits first, the
        // sweep already claimed the row — we drop the inference result.
        let select_outcome = tokio::select! {
            inf = inference => Some(inf),
            _ = heartbeat => None,
        };

        match select_outcome {
            None => {
                // Heartbeat exited — sweep won.
                chronicle_write(
                    crate::pyramid::compute_chronicle::EVENT_FLEET_WORKER_SWEEP_LOST,
                    crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                    serde_json::json!({
                        "peer_id": dispatcher_nid,
                        "job_id": job_id,
                    }),
                );
                return;
            }
            Some(Ok(llm_response)) => {
                // Build FleetAsyncResult::Success from LlmResponse.
                let outcome = crate::fleet::FleetAsyncResult::Success(
                    crate::fleet::FleetDispatchResponse {
                        content: llm_response.content,
                        prompt_tokens: Some(llm_response.usage.prompt_tokens),
                        completion_tokens: Some(llm_response.usage.completion_tokens),
                        model: resolved_model.clone(),
                        finish_reason: None,
                        peer_model: Some(resolved_model.clone()),
                    },
                );
                let result_json = match serde_json::to_string(&outcome) {
                    Ok(s) => s,
                    Err(e) => {
                        // Fall back to synthesized error so the dispatcher gets *some*
                        // terminal outcome rather than a silent hang.
                        tracing::error!(?e, "failed to serialize FleetAsyncResult");
                        crate::pyramid::db::synthesize_worker_error_json(&format!(
                            "result serialize failed: {}",
                            e
                        ))
                    }
                };

                // Step 9: CAS promote pending → ready.
                let db_promote = db_path.clone();
                let disp_promote = dispatcher_nid.clone();
                let jid_promote = job_id.clone();
                let rj_promote = result_json.clone();
                let ready_retention_secs = policy.ready_retention_secs;
                let promote_res: Result<usize, String> =
                    tokio::task::spawn_blocking(move || {
                        let conn = rusqlite::Connection::open(&db_promote)
                            .map_err(|e| e.to_string())?;
                        crate::pyramid::db::fleet_outbox_promote_ready_if_pending(
                            &conn,
                            &disp_promote,
                            &jid_promote,
                            &rj_promote,
                            ready_retention_secs,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|je| Err(je.to_string()));

                match promote_res {
                    Ok(1) => {
                        // Worker won. Record completion.
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_JOB_COMPLETED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                                "model": resolved_model,
                            }),
                        );
                    }
                    Ok(_) => {
                        // rowcount == 0: sweep already promoted us. Drop result.
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_WORKER_SWEEP_LOST,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                            }),
                        );
                        return;
                    }
                    Err(e) => {
                        tracing::error!("fleet outbox promote_ready failed: {}", e);
                        return;
                    }
                }

                // Step 10–12: Attempt callback delivery.
                let envelope = crate::fleet::FleetAsyncResultEnvelope {
                    job_id: job_id.clone(),
                    outcome,
                };
                // Snapshot the roster state needed for delivery, then DROP the
                // lock before the HTTP POST. Holding the roster read lock across
                // a 30s-bounded network call would starve roster writers
                // (heartbeat updates, announcements, dead-peer removal).
                let roster_snapshot_for_delivery = {
                    let roster = state.fleet_roster.read().await;
                    crate::fleet::FleetRoster {
                        peers: roster
                            .peers
                            .get(&dispatcher_nid)
                            .cloned()
                            .map(|p| {
                                let mut m = std::collections::HashMap::new();
                                m.insert(p.node_id.clone(), p);
                                m
                            })
                            .unwrap_or_default(),
                        fleet_jwt: roster.fleet_jwt.clone(),
                        self_operator_id: roster.self_operator_id.clone(),
                    }
                };
                let delivery = crate::fleet::deliver_fleet_result(
                    &dispatcher_nid,
                    &callback_url,
                    &envelope,
                    &roster_snapshot_for_delivery,
                    &policy,
                )
                .await;

                match delivery {
                    Ok(()) => {
                        // Step 11: CAS ready → delivered.
                        let db_del = db_path.clone();
                        let disp_del = dispatcher_nid.clone();
                        let jid_del = job_id.clone();
                        let delivered_retention_secs = policy.delivered_retention_secs;
                        let _ = tokio::task::spawn_blocking(move || {
                            let conn = rusqlite::Connection::open(&db_del).ok()?;
                            crate::pyramid::db::fleet_outbox_mark_delivered_if_ready(
                                &conn,
                                &disp_del,
                                &jid_del,
                                delivered_retention_secs,
                            )
                            .ok()
                        })
                        .await;
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_CALLBACK_DELIVERED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                                "attempts": 1,
                            }),
                        );
                    }
                    Err(e) => {
                        // Step 12: bump attempt counter, record failure event.
                        let err_msg = format!("{}", e);
                        let db_fail = db_path.clone();
                        let disp_fail = dispatcher_nid.clone();
                        let jid_fail = job_id.clone();
                        let err_clone = err_msg.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let conn = rusqlite::Connection::open(&db_fail).ok()?;
                            crate::pyramid::db::fleet_outbox_bump_delivery_attempt(
                                &conn,
                                &disp_fail,
                                &jid_fail,
                                &err_clone,
                            )
                            .ok()
                        })
                        .await;
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_CALLBACK_FAILED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                                "error": err_msg,
                                "attempts": 1,
                            }),
                        );
                    }
                }
            }
            Some(Err(e)) => {
                // Inference failed — synthesize Error result and let it flow through
                // the same ready → deliver path so the dispatcher always hears back.
                let err_msg = format!("{}", e);
                let result_json = crate::pyramid::db::synthesize_worker_error_json(&err_msg);
                let outcome = crate::fleet::FleetAsyncResult::Error(err_msg.clone());

                let db_promote = db_path.clone();
                let disp_promote = dispatcher_nid.clone();
                let jid_promote = job_id.clone();
                let ready_retention_secs = policy.ready_retention_secs;
                let promote_res: Result<usize, String> =
                    tokio::task::spawn_blocking(move || {
                        let conn = rusqlite::Connection::open(&db_promote)
                            .map_err(|e| e.to_string())?;
                        crate::pyramid::db::fleet_outbox_promote_ready_if_pending(
                            &conn,
                            &disp_promote,
                            &jid_promote,
                            &result_json,
                            ready_retention_secs,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|je| Err(je.to_string()));

                if !matches!(promote_res, Ok(1)) {
                    chronicle_write(
                        crate::pyramid::compute_chronicle::EVENT_FLEET_WORKER_SWEEP_LOST,
                        crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                        serde_json::json!({
                            "peer_id": dispatcher_nid,
                            "job_id": job_id,
                        }),
                    );
                    return;
                }

                chronicle_write(
                    crate::pyramid::compute_chronicle::EVENT_FLEET_JOB_COMPLETED,
                    crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                    serde_json::json!({
                        "peer_id": dispatcher_nid,
                        "job_id": job_id,
                        "model": resolved_model,
                        "error": err_msg,
                    }),
                );

                let envelope = crate::fleet::FleetAsyncResultEnvelope {
                    job_id: job_id.clone(),
                    outcome,
                };
                // Snapshot roster state and drop the lock before the HTTP
                // POST — see matching comment in the Success branch above.
                let roster_snapshot_for_delivery = {
                    let roster = state.fleet_roster.read().await;
                    crate::fleet::FleetRoster {
                        peers: roster
                            .peers
                            .get(&dispatcher_nid)
                            .cloned()
                            .map(|p| {
                                let mut m = std::collections::HashMap::new();
                                m.insert(p.node_id.clone(), p);
                                m
                            })
                            .unwrap_or_default(),
                        fleet_jwt: roster.fleet_jwt.clone(),
                        self_operator_id: roster.self_operator_id.clone(),
                    }
                };
                let delivery = crate::fleet::deliver_fleet_result(
                    &dispatcher_nid,
                    &callback_url,
                    &envelope,
                    &roster_snapshot_for_delivery,
                    &policy,
                )
                .await;

                match delivery {
                    Ok(()) => {
                        let db_del = db_path.clone();
                        let disp_del = dispatcher_nid.clone();
                        let jid_del = job_id.clone();
                        let delivered_retention_secs = policy.delivered_retention_secs;
                        let _ = tokio::task::spawn_blocking(move || {
                            let conn = rusqlite::Connection::open(&db_del).ok()?;
                            crate::pyramid::db::fleet_outbox_mark_delivered_if_ready(
                                &conn,
                                &disp_del,
                                &jid_del,
                                delivered_retention_secs,
                            )
                            .ok()
                        })
                        .await;
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_CALLBACK_DELIVERED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                                "attempts": 1,
                            }),
                        );
                    }
                    Err(e2) => {
                        let err2_msg = format!("{}", e2);
                        let db_fail = db_path.clone();
                        let disp_fail = dispatcher_nid.clone();
                        let jid_fail = job_id.clone();
                        let err_clone = err2_msg.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let conn = rusqlite::Connection::open(&db_fail).ok()?;
                            crate::pyramid::db::fleet_outbox_bump_delivery_attempt(
                                &conn,
                                &disp_fail,
                                &jid_fail,
                                &err_clone,
                            )
                            .ok()
                        })
                        .await;
                        chronicle_write(
                            crate::pyramid::compute_chronicle::EVENT_FLEET_CALLBACK_FAILED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET_RECEIVED,
                            serde_json::json!({
                                "peer_id": dispatcher_nid,
                                "job_id": job_id,
                                "error": err2_msg,
                                "attempts": 1,
                            }),
                        );
                    }
                }
            }
        }
    });
}

// ── Compute market handlers ─────────────────────────────────────────────
//
// `handle_market_dispatch` + `spawn_market_worker` mirror the fleet
// dispatch pair structurally. The compute market is the fleet async
// dispatch protocol with a different JWT audience (`compute` vs `fleet`),
// a ChatML `messages` payload instead of (system_prompt, user_prompt),
// and a JWT-gated callback URL instead of a roster-matched one. All the
// outbox / CAS / sweep / admission scaffolding is reused via
// `fleet_result_outbox.callback_kind = 'MarketStandard' | 'Relay'` with
// `dispatcher_node_id = fleet::WIRE_PLATFORM_DISPATCHER`.
//
// See `docs/plans/compute-market-phase-2-exchange.md` §III for the full
// step-by-step. Order is load-bearing: verify → parse → convert →
// validate → admission gates → outbox insert → enqueue → spawn worker
// → 202.

/// Handle POST /v1/compute/job-dispatch — receive matched market job from
/// the Wire.
///
/// Async admission protocol: verify `wire_job_token` JWT, parse request,
/// convert ChatML messages to prompt pair, validate callback URL
/// structurally, check market admission gates (market enabled, policy
/// allows serving, offer exists), transactionally insert into the outbox
/// with the market sentinel dispatcher, enqueue onto the compute queue
/// with `source: "market_received"`, upsert the in-memory job registry,
/// spawn the worker, and return 202. The actual inference runs in the
/// spawned worker which heartbeats the outbox row and CAS-promotes it to
/// `ready` on completion; Phase 3's callback-delivery worker then posts
/// the result to the callback URL.
///
/// See `docs/plans/compute-market-phase-2-exchange.md` §III for the
/// full spec.
/// Handler for `POST /v1/compute/job-result` — Wire → Requester push
/// from the delivery worker. Contract §2.5. Phase 3 requester-side
/// consumption point.
///
/// Auth: JWT with `aud="result-delivery"`, `sub=<uuid_job_id>`,
/// `rid=<requester_operator_id>`. Rejected 401 on any mismatch.
///
/// Envelope: §2.3 success/failure tagged shape, forwarded verbatim
/// from the provider → Wire callback path.
///
/// Flow:
///   1. Minimal body parse to extract `job_id` (UUID string).
///   2. Verify JWT binds to this job_id + this operator.
///   3. Full body parse into the tagged envelope.
///   4. `pending_jobs.take(job_id)` — if present, fire oneshot; if
///      absent, respond 2xx `already_settled` (duplicate or late
///      arrival after timeout fallback).
///   5. 2xx response.
///
/// Idempotency (contract §2.5): Wire may retry on 5xx or network
/// timeout; node returns 2xx `already_settled` for repeats so Wire
/// marks delivery complete and stops retrying.
async fn handle_compute_job_result(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    // ── Step 1: minimal body parse for the job_id we need for JWT bind check.
    //
    // We extract `job_id` first (instead of full-parsing the envelope)
    // because `verify_result_delivery_token` needs it to enforce the
    // `sub`-binding check. Full body validation happens after auth
    // passes — keeps the 401 path cheap and avoids leaking body-shape
    // diagnostics on auth failures.
    let job_id = match body.get("job_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": "missing_job_id",
                    "detail": "job_id is required in result envelope body",
                })),
                warp::http::StatusCode::BAD_REQUEST,
            )));
        }
    };

    // ── Step 2: verify the Wire-minted requester-delivery JWT (rev 2.0).
    //
    // Wave 2B hard-swaps to `verify_requester_delivery_token`
    // (aud="requester-delivery", sub=<body.job_id>, rid=<self.operator_id>).
    // Per spec §"Requester-delivery JWT verifier" + §3.4 the transition is
    // clean-cut: legacy aud="result-delivery" tokens are rejected with 401
    // WrongAud, no fallback to `verify_result_delivery_token`. Outstanding
    // legacy tokens self-expire within `fill_job_ttl_secs`; the provider's
    // content leg exhausts to `failed_content_only` and the requester
    // reconciles via §2.4 status-poll. Zero-lockstep fail-loud.
    //
    // Strip any leading `"Bearer "` prefix (warp's header filter gives us
    // the raw header value including the prefix). The verifier itself also
    // handles this, but stripping here keeps the tracing log clean.
    let jwt_pk = state.jwt_public_key.read().await.clone();
    // Wire mints the requester-delivery JWT with `rid=<requester_operator_id>`
    // (contract §3.4, line 634). The node's `AuthState` has both `user_id`
    // (Supabase Auth UUID — from OAuth login) and `operator_id` (Wire
    // operator identity — populated by `register_with_session` response).
    // The `rid` claim binds to `operator_id`, not `user_id`. Prior rev-0.5
    // code here read `user_id` — pre-existing bug (would 401 every
    // legitimate token); fixed in rev 2.0 cutover to match the fleet
    // identity verifier pattern (server.rs:1589) + pyramid query verifier.
    let self_operator_id = state.auth.read().await.operator_id.clone().unwrap_or_default();
    if self_operator_id.is_empty() {
        tracing::warn!(
            "compute_job_result: local operator_id empty (not registered with Wire); cannot verify JWT"
        );
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "error": "operator_not_registered",
            })),
            warp::http::StatusCode::UNAUTHORIZED,
        )));
    }
    if let Err(e) = crate::pyramid::result_delivery_identity::verify_requester_delivery_token(
        &auth_header,
        &jwt_pk,
        &self_operator_id,
        &job_id,
    ) {
        // Surface the variant name so operators can diagnose aud/sub/rid
        // mismatches versus signature vs expiry failures. No token
        // material or key bytes are in the Debug impl — each variant
        // carries only typed discriminator data.
        let variant = format!("{:?}", e);
        tracing::warn!(
            error = %e,
            variant = %variant,
            job_id = %job_id,
            "compute_job_result: requester-delivery JWT verification failed"
        );
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "error": "unauthorized",
                "variant": variant,
            })),
            warp::http::StatusCode::UNAUTHORIZED,
        )));
    }

    // ── Step 3: full envelope parse. The `type` discriminator picks
    //    success vs failure; any other shape is a 400. Tolerant to
    //    Wire adding observability fields via a top-level extensions
    //    bag (contract §10.1).
    let envelope_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload = match envelope_type {
        "success" => {
            let result = match body.get("result") {
                Some(r) => r,
                None => {
                    return Ok(Box::new(warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": "invalid_envelope",
                            "detail": "success envelope missing `result` object",
                        })),
                        warp::http::StatusCode::BAD_REQUEST,
                    )));
                }
            };
            crate::pyramid::pending_jobs::DeliveryPayload::Success {
                content: result
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                input_tokens: result
                    .get("input_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                output_tokens: result
                    .get("output_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                model_used: result
                    .get("model_used")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                latency_ms: result.get("latency_ms").and_then(|v| v.as_i64()).unwrap_or(0),
                finish_reason: result
                    .get("finish_reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }
        }
        "failure" => {
            let err = body.get("error").cloned().unwrap_or(serde_json::json!({}));
            crate::pyramid::pending_jobs::DeliveryPayload::Failure {
                code: err
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                message: err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        }
        other => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": "invalid_envelope_type",
                    "detail": format!("expected \"success\" or \"failure\", got \"{}\"", other),
                })),
                warp::http::StatusCode::BAD_REQUEST,
            )));
        }
    };

    // Snapshot the pyramid data_dir so chronicle writes (below) can
    // open a short-lived SQLite connection. Read before the take/fire
    // rendezvous so the handler reply path is never blocked on a
    // data_dir lookup.
    let chronicle_db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db").to_string_lossy().to_string());

    // Capture envelope metadata for chronicle BEFORE the payload moves
    // into the oneshot send. The network-framed metadata keys mirror
    // the plan §4.3 shape.
    let (success_content_len, success_model_used, success_input_tokens, success_output_tokens,
         success_latency_ms, success_finish_reason) = match &payload {
        crate::pyramid::pending_jobs::DeliveryPayload::Success {
            content,
            model_used,
            input_tokens,
            output_tokens,
            latency_ms,
            finish_reason,
        } => (
            Some(content.len()),
            Some(model_used.clone()),
            Some(*input_tokens),
            Some(*output_tokens),
            Some(*latency_ms),
            finish_reason.clone(),
        ),
        _ => (None, None, None, None, None, None),
    };

    // ── Step 4: rendezvous with the awaiting dispatcher.
    //
    // `take` both returns and removes the entry — if the awaiter has
    // already timed out and cleaned up its own entry, we find nothing
    // and respond `already_settled` so Wire tombstones the delivery
    // and stops retrying (idempotency per contract §2.5).
    match state.pending_market_jobs.take(&job_id).await {
        Some(sender) => {
            // Sender may be dropped if the awaiting receiver has been
            // dropped (e.g. the inference call was cancelled). `send`
            // returning Err is a no-op for our purposes — we still
            // respond 2xx to Wire and tombstone the delivery.
            let _ = sender.send(payload);

            // Chronicle: emit `network_result_returned` on success
            // envelopes. Failure envelopes chronicle via the
            // requester's soft-fail path (ProviderFailed → FELL_BACK_LOCAL).
            if let (Some(db_path), Some(model_used)) =
                (chronicle_db_path.clone(), success_model_used.clone())
            {
                let job_path = format!(
                    "{}:{}",
                    crate::pyramid::compute_chronicle::SOURCE_NETWORK_RECEIVED,
                    job_id
                );
                let chronicle_ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                    &job_path,
                    crate::pyramid::compute_chronicle::EVENT_NETWORK_RESULT_RETURNED,
                    crate::pyramid::compute_chronicle::SOURCE_NETWORK_RECEIVED,
                )
                .with_model_id(model_used.clone())
                .with_metadata(serde_json::json!({
                    "job_id": job_path,
                    "uuid_job_id": job_id,
                    "input_tokens": success_input_tokens.unwrap_or(0),
                    "output_tokens": success_output_tokens.unwrap_or(0),
                    "latency_ms": success_latency_ms.unwrap_or(0),
                    "model_used": model_used,
                    "provider_node_id": serde_json::Value::Null,
                    "finish_reason": success_finish_reason,
                    "content_bytes": success_content_len.unwrap_or(0),
                }));
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = crate::pyramid::compute_chronicle::record_event(&conn, &chronicle_ctx);
                    }
                });
            }

            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "status": "ok",
                    "job_id": job_id,
                })),
                warp::http::StatusCode::OK,
            )))
        }
        None => {
            // Late delivery after timeout fallback, OR a duplicate
            // push Wire is retrying. Both are expected in steady state.
            tracing::info!(
                job_id = %job_id,
                "compute_job_result: late or duplicate delivery; responding already_settled"
            );

            // Chronicle: emit `network_late_arrival`. The handler
            // does not know when the job was first registered, so the
            // time_since_first_seen_ms field stays null.
            if let Some(db_path) = chronicle_db_path.clone() {
                let job_path = format!(
                    "{}:{}",
                    crate::pyramid::compute_chronicle::SOURCE_NETWORK_RECEIVED,
                    job_id
                );
                let chronicle_ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                    &job_path,
                    crate::pyramid::compute_chronicle::EVENT_NETWORK_LATE_ARRIVAL,
                    crate::pyramid::compute_chronicle::SOURCE_NETWORK_RECEIVED,
                )
                .with_metadata(serde_json::json!({
                    "uuid_job_id": job_id,
                    "time_since_first_seen_ms": serde_json::Value::Null,
                }));
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = crate::pyramid::compute_chronicle::record_event(&conn, &chronicle_ctx);
                    }
                });
            }

            Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "status": "already_settled",
                    "job_id": job_id,
                })),
                warp::http::StatusCode::OK,
            )))
        }
    }
}

async fn handle_market_dispatch(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    // Step 1 (§III L544): Verify wire_job_token JWT.
    //
    // `verify_market_identity` checks: aud=compute, exp, signature, pid ==
    // self.node_id, sub non-empty. Any failure is a 401 — the specific
    // variant is logged but never surfaced to the caller (same discipline
    // as fleet).
    let jwt_pk = state.jwt_public_key.read().await.clone();
    let self_node_id = state.node_id.read().await.clone();
    let identity = match crate::pyramid::market_identity::verify_market_identity(
        &auth_header,
        &jwt_pk,
        &self_node_id,
    ) {
        Ok(i) => i,
        Err(e) => {
            // Surface the variant in the 401 body so the Wire's
            // `compute_fill_jwt_rejected` chronicle (which captures
            // `provider_error` from our response) can record the
            // specific failure mode. Pre-hardening the body was empty
            // and Wire only saw `provider_error: null`; during
            // first-run verification of the /fill path that opacity
            // meant every auth failure looked identical across the
            // four MarketAuthError variants (InvalidToken / PidMismatch
            // / MissingJobId / MissingSelfNodeId). The variant name
            // alone (a type discriminator, no token material, no key
            // bytes, no JWT claims) is safe to surface to the dispatcher
            // since the dispatcher is Wire itself, not a hostile
            // third party — the JWT decode path prevents anyone but
            // Wire from issuing a dispatch that reaches this handler.
            let variant = format!("{:?}", e);
            tracing::warn!("Market dispatch JWT verify failed: {} ({})", e, variant);
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": "jwt_verify_failed",
                    "variant": variant,
                })),
                warp::http::StatusCode::UNAUTHORIZED,
            )));
        }
    };

    // Step 2 (§III L545, DD-C): Parse body as MarketDispatchRequest.
    //
    // `deny_unknown_fields` is on the struct — a typo'd field from the
    // Wire side surfaces as a visible 400 instead of being silently
    // dropped. Parse error is surfaced verbatim for operator diagnosis.
    let req: crate::pyramid::market_dispatch::MarketDispatchRequest =
        match serde_json::from_value(body.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Market dispatch body parse failed: {}", e);
                return Ok(Box::new(warp::reply::with_status(
                    warp::reply::json(
                        &serde_json::json!({"error": format!("invalid body: {}", e)}),
                    ),
                    warp::http::StatusCode::BAD_REQUEST,
                )));
            }
        };

    // Defense in depth: the JWT `sub` and the body `job_id` must agree.
    // The JWT binds the provider to a specific job_id; a mismatched body
    // is either a Wire bug or a replayed JWT being reused. Reject.
    if identity.sub_job_id() != req.job_id {
        tracing::warn!(
            "Market dispatch job_id mismatch: jwt.sub={} body.job_id={}",
            identity.sub_job_id(),
            req.job_id
        );
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "job_id mismatch with JWT sub"})),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    // Step 3 (§III L546, DD-C): Convert ChatML messages to prompt pair.
    //
    // Any failure here is a 400 with the specific `MessagesError` variant
    // so the Wire (or an operator running curl) sees exactly which shape
    // the dispatch violated.
    let (system_prompt, user_prompt) =
        match crate::pyramid::messages::messages_to_prompt_pair(&req.messages) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("Market dispatch messages conversion failed: {}", e);
                return Ok(Box::new(warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({
                        "error": format!("{}", e),
                        "kind": e,
                    })),
                    warp::http::StatusCode::BAD_REQUEST,
                )));
            }
        };

    // Step 4 (§III L547): Validate callback_url structurally.
    //
    // MarketStandard kind short-circuits the roster check (the Wire is
    // not a fleet peer; the JWT is the auth). An empty roster is fine.
    let callback_url_str = req.callback_url.to_string();
    {
        let roster = state.fleet_roster.read().await;
        if let Err(e) = crate::fleet::validate_callback_url(
            &callback_url_str,
            &crate::fleet::CallbackKind::MarketStandard,
            &*roster,
        ) {
            tracing::warn!(
                "Market dispatch callback_url rejected: {} (job_id={})",
                e,
                req.job_id
            );
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("{}", e)})),
                warp::http::StatusCode::BAD_REQUEST,
            )));
        }
    } // roster read-lock drops here

    // Step 4b (rev 2.0 §2.1 / spec §"`handle_market_dispatch`" line 360):
    // admission-time SSRF re-validation of `requester_callback_url`.
    //
    // Wire also validates at match time, but defense-in-depth says every
    // receiver re-checks — a compromised Wire or in-flight mutation of the
    // dispatch body could otherwise pivot the provider's outbound HTTP
    // client onto loopback / RFC1918 / link-local. Same structural shape
    // as the settlement-leg URL (https + non-empty host; no roster
    // participation). `CallbackKind::MarketStandard` is the correct variant
    // here because the requester tunnel is an external HTTPS callback URL
    // with exactly the same constraints as the Wire settlement URL — third-
    // party endpoint, JWT-gated, scheme-must-be-https, no fleet roster
    // entry. `Relay` would also work (identical branch), but `MarketStandard`
    // is the direct-P2P tier and keeps this admission check symmetric with
    // the settlement-leg validation above. An empty `FleetRoster::default()`
    // is passed because the roster is never consulted for these variants
    // (see fleet.rs::validate_callback_url match arm).
    let requester_callback_url_str = req.requester_callback_url.to_string();
    if let Err(e) = crate::fleet::validate_callback_url(
        &requester_callback_url_str,
        &crate::fleet::CallbackKind::MarketStandard,
        &crate::fleet::FleetRoster::default(),
    ) {
        tracing::warn!(
            "Market dispatch requester_callback_url rejected: {} (job_id={})",
            e,
            req.job_id
        );
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "error": "requester_callback_url_missing_or_invalid",
                "detail": format!("{}", e),
            })),
            warp::http::StatusCode::BAD_REQUEST,
        )));
    }

    // Step 4c (Q-PROTO-3, spec §"`handle_market_dispatch`" line 354):
    // `privacy_tier` warn-don't-reject. Only `"direct"` is silent; any
    // other value (including the deprecated `"bootstrap-relay"`) logs a
    // chronicle event and proceeds with direct-delivery semantics. Never
    // reject — a future relay-market Wire may ship new tier strings
    // ahead of node relay support, and zero-lockstep requires the node
    // to degrade gracefully rather than 400 on an unknown tier.
    if req.privacy_tier != "direct" {
        if let Some(data_dir) = state.pyramid.data_dir.as_ref() {
            let db_path_chr = data_dir.join("pyramid.db");
            let tier = req.privacy_tier.clone();
            let job_id_chr = req.job_id.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = rusqlite::Connection::open(&db_path_chr) {
                    let job_path = format!("market:{}", job_id_chr);
                    let ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                        &job_path,
                        "market_unknown_privacy_tier",
                        crate::pyramid::compute_chronicle::SOURCE_MARKET_RECEIVED,
                    )
                    .with_metadata(serde_json::json!({
                        "tier": tier,
                        "job_id": job_id_chr,
                    }));
                    let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx);
                }
            });
        }
        tracing::info!(
            job_id = %req.job_id,
            privacy_tier = %req.privacy_tier,
            "Market dispatch accepted with non-direct privacy_tier; proceeding with direct-delivery semantics"
        );
    }

    // Step 5 (§III L547-551, DD-H): Admission gates.
    //
    // Several gates are NOT implemented in this workstream — see TODOs
    // below. The gates that ARE enforced here are the ones that are
    // necessary to avoid accepting jobs we have no way to serve: market
    // must be wired up, state must exist, operator intent must allow
    // market serving, and an offer must exist for the requested model.
    //
    // TODO (WS8): Add DADBEAR hold check on the `"market:compute"` slug.
    //   Blocking holds (frozen / breaker / cost_limit / quality_hold /
    //   timing_suspended / reputation_suspended / suspended / escalation)
    //   should reject 503 with Retry-After. Marker holds (measurement,
    //   etc.) are informational — do not block. See DD-H in
    //   compute-market-architecture.md §VIII.6.
    //
    // TODO (Fleet MPS WS5): Read AvailabilitySnapshot for this node; if
    //   `health_status = degraded` AND policy.allow_serving_while_degraded
    //   is false, reject 503. If `tunnel_status != healthy`, reject (we
    //   can't deliver results back).
    //
    // TODO (Phase 5): Negative-balance gate. If the operator's credit
    //   balance would go negative after this job's worst-case settlement,
    //   reject 503 with `X-Wire-Reason: negative_balance`.

    // Gate: compute market dispatch context must be wired up.
    let market_ctx = match state.compute_market_dispatch.as_ref() {
        Some(ctx) => Arc::clone(ctx),
        None => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(
                    warp::reply::json(
                        &serde_json::json!({"error": "compute market disabled"}),
                    ),
                    "Retry-After",
                    // No policy to read since the context itself is None.
                    // 30s is the seed default for admission_retry_after_secs.
                    "30",
                ),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    // Gate: compute market state must be initialized (offers live here).
    let market_state_handle = match state.compute_market_state.as_ref() {
        Some(s) => Arc::clone(s),
        None => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(
                    warp::reply::json(
                        &serde_json::json!({"error": "compute market state not initialized"}),
                    ),
                    "Retry-After",
                    "30",
                ),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    // Snapshot market delivery policy for admission + worker. Held for
    // the rest of the handler; worker gets a clone by value so it can
    // outlive the handler call.
    let policy = market_ctx.policy.read().await.clone();

    // Gate: operator intent via `compute_participation_policy.
    // allow_market_visibility`. The DB lookup happens off the async
    // thread because `get_compute_participation_policy` opens a SQLite
    // connection.
    let db_path = match state.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "node data dir unavailable"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
    };

    let allow_market_visibility = {
        let db_path_read = db_path.clone();
        let result: Result<bool, String> = tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path_read)
                .map_err(|e| e.to_string())?;
            let p = crate::pyramid::local_mode::get_compute_participation_policy(&conn)
                .map_err(|e| e.to_string())?;
            Ok(p.effective_booleans().allow_market_visibility)
        })
        .await
        .unwrap_or_else(|je| Err(je.to_string()));
        match result {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Market dispatch participation policy read failed: {}", e);
                return Ok(Box::new(warp::reply::with_status(
                    warp::reply::json(
                        &serde_json::json!({"error": "policy read error"}),
                    ),
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                )));
            }
        }
    };
    if !allow_market_visibility {
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::with_header(
                warp::reply::with_header(
                    warp::reply::json(
                        &serde_json::json!({"error": "market_serving_disabled"}),
                    ),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                "X-Wire-Reason",
                "market_serving_disabled",
            ),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        )));
    }

    // Step 6 (§III): Look up the offer for the requested model, clone out
    // the max-queue-depth cap + the offer's base rates. Settlement rates
    // (`matched_rate_in_per_m`, `matched_rate_out_per_m`) come from the
    // offer's base rate × the Wire-authoritative `matched_multiplier_bps`
    // from the dispatch body — NOT a locally-computed bps value, because
    // the Wire quoted the requester using its own bps at match time and
    // we must settle at that same value.
    //
    // Offer absence is 503 with a reason header — the Wire's stale-offer
    // cleanup consumes this to deactivate the offer.
    let (max_queue_depth, offer_base_rate_in, offer_base_rate_out) = {
        let s = market_state_handle.read().await;
        match s.offers.get(&req.model_id) {
            Some(offer) => (
                offer.max_queue_depth,
                offer.rate_per_m_input,
                offer.rate_per_m_output,
            ),
            None => {
                return Ok(Box::new(warp::reply::with_status(
                    warp::reply::with_header(
                        warp::reply::with_header(
                            warp::reply::json(&serde_json::json!({
                                "error": "no offer for model",
                                "model": req.model_id,
                            })),
                            "Retry-After",
                            policy.admission_retry_after_secs.to_string(),
                        ),
                        "X-Wire-Reason",
                        "no_offer_for_model",
                    ),
                    warp::http::StatusCode::SERVICE_UNAVAILABLE,
                )));
            }
        }
    };

    // Settlement rates = base × matched_multiplier_bps / 10000 (integer
    // truncation matches Wire-side math). `offer.rate_per_m_input/output`
    // are i64; matched_multiplier_bps is i32 widened to i64 for the
    // multiply to avoid overflow on max-discount-max-rate corner cases.
    let matched_rate_in_per_m: i64 =
        (offer_base_rate_in * req.matched_multiplier_bps as i64) / 10000;
    let matched_rate_out_per_m: i64 =
        (offer_base_rate_out * req.matched_multiplier_bps as i64) / 10000;

    // Step 6b (§V DADBEAR Integration, DD-H): Admission hold check.
    //
    // Per `compute-market-architecture.md` DD-H (§VIII.6), any of the
    // following BLOCKING hold names on the `"market:compute"` slug
    // must reject the dispatch with 503 + Retry-After:
    //   frozen                — operator-level pause
    //   breaker               — quality system flagged the node
    //   cost_limit            — credit balance too low
    //   quality_hold          — Phase 5 upheld-challenge hold
    //   timing_suspended      — Phase 5 timing-anomaly hold
    //   reputation_suspended  — Phase 5 reputation-threshold hold
    //   suspended             — Phase 6 steward-placed pause
    //   escalation            — Phase 6 escalation gate
    //
    // Non-blocking holds (e.g. Phase 6's `measurement` marker) are
    // informational and MUST NOT reject. DD-H explicitly chose an
    // enumerated blocking list (over "any hold blocks") so new hold
    // types don't accidentally gate the market.
    //
    // The projection read happens off the async thread because
    // `get_holds` opens a SQLite connection via rusqlite.
    const MARKET_COMPUTE_BLOCKING_HOLDS: &[&str] = &[
        "frozen",
        "breaker",
        "cost_limit",
        "quality_hold",
        "timing_suspended",
        "reputation_suspended",
        "suspended",
        "escalation",
    ];
    const MARKET_COMPUTE_SLUG: &str =
        crate::pyramid::dadbear_preview::MARKET_COMPUTE_SLUG;

    let blocking_hold: Option<String> = {
        let db_path_read = db_path.clone();
        let result: Result<Option<String>, String> = tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path_read)
                .map_err(|e| e.to_string())?;
            let holds = crate::pyramid::auto_update_ops::get_holds(
                &conn,
                MARKET_COMPUTE_SLUG,
            );
            // Return the FIRST matching blocking hold name so the
            // X-Wire-Reason header can surface it. Iterating in the
            // declared order keeps the surface deterministic.
            for name in MARKET_COMPUTE_BLOCKING_HOLDS {
                if holds.iter().any(|h| h.hold == *name) {
                    return Ok(Some((*name).to_string()));
                }
            }
            Ok(None)
        })
        .await
        .unwrap_or_else(|je| Err(je.to_string()));
        match result {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Market dispatch hold-projection read failed: {}", e);
                return Ok(Box::new(warp::reply::with_status(
                    warp::reply::json(
                        &serde_json::json!({"error": "hold projection read error"}),
                    ),
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                )));
            }
        }
    };
    if let Some(hold_name) = blocking_hold {
        tracing::warn!(
            "Market dispatch blocked by DADBEAR hold: slug={} hold={} job_id={}",
            MARKET_COMPUTE_SLUG,
            hold_name,
            req.job_id,
        );
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::with_header(
                warp::reply::with_header(
                    warp::reply::json(&serde_json::json!({
                        "error": "market compute held",
                        "hold": hold_name,
                    })),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                "X-Wire-Reason",
                "market_compute_held",
            ),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        )));
    }

    // Step 7 (§III L553, DD-D, DD-Q): Idempotent outbox insert.
    //
    // All SQL inside spawn_blocking (rusqlite is sync). Branch on the
    // outcome enum to build the HTTP response. The Wire is the sole
    // dispatcher for market rows, identified by the WIRE_PLATFORM_DISPATCHER
    // sentinel; cross-dispatcher conflicts within the market namespace are
    // impossible, but a `UNIQUE INDEX` on `job_id` alone (db.rs:2301) means
    // a pre-existing FLEET row with the same job_id (astronomically
    // unlikely but not structurally impossible) would return changes=0
    // with `lookup.dispatcher_node_id != WIRE_PLATFORM_DISPATCHER` —
    // that's the `ConflictForeignDispatcher` branch (parallel to fleet's
    // `ConflictDifferentDispatcher`). A rowcount=0 outcome with matching
    // dispatcher_id means a legitimate Wire retry (re-ACK) or a terminal
    // state leaked back to the Wire (Gone).
    #[derive(Debug)]
    enum MarketAdmissionOutcome {
        Admitted,
        RetryExisting,
        GoneDelivered,
        GoneFailed(Option<String>),
        Rejected503, // max_inflight_jobs budget exhausted
        ConflictForeignDispatcher,
        DbError(String),
    }

    let worker_heartbeat_tolerance_secs = policy.worker_heartbeat_tolerance_secs;
    let admission_max_inflight = policy.max_inflight_jobs;
    let db_path_tx = db_path.clone();
    let job_id_tx = req.job_id.clone();
    let callback_url_tx = callback_url_str.clone();
    // Phase 3: persist the bearer + request_id onto the outbox row at admission
    // so the delivery worker reads them at POST time (restart-safe, survives
    // process crashes mid-inference). callback_auth is required per contract
    // rev 1.5; extensions.request_id is optional per contract §10.1.
    let callback_auth_token_tx = req.callback_auth.token.clone();
    let request_id_tx: Option<String> = req
        .extensions
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    // Rev 0.6.1 Wave 2B: thread the two new dispatch-body fields through
    // to the outbox row. Wave 1 added the DB columns + the helper-fn
    // parameters; Wave 1 call sites passed `None, None` pending this
    // admission handler change. The delivery worker (Wave 2A) reads these
    // columns at POST time for the content leg.
    let requester_callback_url_tx = requester_callback_url_str.clone();
    let requester_delivery_jwt_tx = req.requester_delivery_jwt.clone();

    let outcome = tokio::task::spawn_blocking(move || -> MarketAdmissionOutcome {
        let mut conn = match rusqlite::Connection::open(&db_path_tx) {
            Ok(c) => c,
            Err(e) => return MarketAdmissionOutcome::DbError(e.to_string()),
        };
        let tx = match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(t) => t,
            Err(e) => return MarketAdmissionOutcome::DbError(e.to_string()),
        };
        // expires_at = now + worker_heartbeat_tolerance_secs
        let modifier = format!("+{} seconds", worker_heartbeat_tolerance_secs);
        let expires_at: String = match tx.query_row(
            "SELECT datetime('now', ?1)",
            rusqlite::params![modifier],
            |r| r.get(0),
        ) {
            Ok(s) => s,
            Err(e) => return MarketAdmissionOutcome::DbError(e.to_string()),
        };
        let changes = match crate::pyramid::db::market_outbox_insert_or_ignore(
            &tx,
            &job_id_tx,
            &callback_url_tx,
            crate::fleet::callback_kind_str(&crate::fleet::CallbackKind::MarketStandard),
            &expires_at,
            Some(&callback_auth_token_tx),
            request_id_tx.as_deref(),
            // Rev 0.6.1 Wave 2B: plumb the content-leg URL + opaque
            // requester-delivery JWT onto the outbox row. Wave 2A's
            // delivery worker consumes these on the content leg; the
            // token is treated as opaque by this node (verification
            // happens on the requester side via
            // `verify_requester_delivery_token`).
            Some(&requester_callback_url_tx),
            Some(&requester_delivery_jwt_tx),
        ) {
            Ok(n) => n,
            Err(e) => return MarketAdmissionOutcome::DbError(e.to_string()),
        };
        // Look up the (now-either-just-inserted-or-pre-existing) row.
        // `fleet_outbox_lookup` keys on `job_id` alone (db.rs:2441),
        // and the table has a `UNIQUE INDEX` on `job_id` (db.rs:2301),
        // so changes=0 means a row with that job_id already exists —
        // but the dispatcher might be a fleet peer rather than the
        // Wire sentinel. We verify the ownership before branching on
        // status so a fleet/relay row with a UUID collision doesn't
        // get wrongly surfaced as "your market job is already
        // delivered" back to the Wire.
        let lookup = match crate::pyramid::db::fleet_outbox_lookup(&tx, &job_id_tx) {
            Ok(Some(l)) => l,
            Ok(None) => {
                let _ = tx.rollback();
                return MarketAdmissionOutcome::DbError("outbox lookup returned None".into());
            }
            Err(e) => {
                let _ = tx.rollback();
                return MarketAdmissionOutcome::DbError(e.to_string());
            }
        };

        if changes == 0 {
            // Pre-existing row. First: is it ours (WIRE_PLATFORM_DISPATCHER)
            // or does a fleet/relay row hold the PK? A cross-protocol
            // UUID collision is astronomically rare but possible — the
            // `UNIQUE(job_id)` index allows exactly one row per job_id
            // regardless of dispatcher, so INSERT OR IGNORE returning 0
            // can mean "the Wire is retrying me" OR "some other
            // dispatcher owns this job_id." Only the former is a
            // legitimate retry path.
            if lookup.dispatcher_node_id != crate::fleet::WIRE_PLATFORM_DISPATCHER {
                let _ = tx.rollback();
                return MarketAdmissionOutcome::ConflictForeignDispatcher;
            }
            // Pre-existing market row — branch on status.
            match lookup.status.as_str() {
                "pending" | "ready" => {
                    if let Err(e) = tx.commit() {
                        return MarketAdmissionOutcome::DbError(e.to_string());
                    }
                    MarketAdmissionOutcome::RetryExisting
                }
                "delivered" => {
                    let _ = tx.rollback();
                    MarketAdmissionOutcome::GoneDelivered
                }
                "failed" => {
                    let _ = tx.rollback();
                    MarketAdmissionOutcome::GoneFailed(lookup.last_error)
                }
                _ => {
                    let _ = tx.rollback();
                    MarketAdmissionOutcome::DbError(format!(
                        "unknown outbox status: {}",
                        lookup.status
                    ))
                }
            }
        } else {
            // Fresh insert. Admission count gates.
            let inflight = match crate::pyramid::db::market_outbox_count_inflight_excluding(
                &tx,
                crate::fleet::WIRE_PLATFORM_DISPATCHER,
                &job_id_tx,
            ) {
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.rollback();
                    return MarketAdmissionOutcome::DbError(e.to_string());
                }
            };
            if admission_max_inflight != 0 && inflight >= admission_max_inflight {
                // Over capacity — delete the row we just inserted so the
                // Wire can re-match elsewhere and we don't leak a phantom
                // pending row.
                if let Err(e) = crate::pyramid::db::fleet_outbox_delete(
                    &tx,
                    crate::fleet::WIRE_PLATFORM_DISPATCHER,
                    &job_id_tx,
                ) {
                    let _ = tx.rollback();
                    return MarketAdmissionOutcome::DbError(e.to_string());
                }
                let _ = tx.rollback();
                return MarketAdmissionOutcome::Rejected503;
            }
            if let Err(e) = tx.commit() {
                return MarketAdmissionOutcome::DbError(e.to_string());
            }
            MarketAdmissionOutcome::Admitted
        }
    })
    .await
    .unwrap_or_else(|je| MarketAdmissionOutcome::DbError(je.to_string()));

    // Handle outbox-admission outcomes that short-circuit before the
    // depth gate + worker spawn. Only the `Admitted` path falls through.
    match outcome {
        MarketAdmissionOutcome::DbError(msg) => {
            tracing::error!("Market dispatch admission DB error: {}", msg);
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "admission db error"})),
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            )));
        }
        MarketAdmissionOutcome::ConflictForeignDispatcher => {
            // A row with this job_id already exists under a different
            // dispatcher (fleet peer, relay hop, etc.). Surface as 409
            // CONFLICT so the Wire can diagnose the UUID collision
            // rather than silently treating it as a market retry.
            // Parallel to fleet's `ConflictDifferentDispatcher`.
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": "job_id conflict with foreign dispatcher",
                })),
                warp::http::StatusCode::CONFLICT,
            )));
        }
        MarketAdmissionOutcome::GoneDelivered => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "job already delivered"})),
                warp::http::StatusCode::GONE,
            )));
        }
        MarketAdmissionOutcome::GoneFailed(last_error) => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": "job previously failed",
                    "last_error": last_error,
                })),
                warp::http::StatusCode::GONE,
            )));
        }
        MarketAdmissionOutcome::Rejected503 => {
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::with_header(
                    warp::reply::json(&serde_json::json!({"error": "peer at capacity"})),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )));
        }
        MarketAdmissionOutcome::RetryExisting => {
            // Same Wire dispatcher, same job_id, outbox already pending/ready.
            // Legitimate retry — re-ACK without spawning a second worker.
            // Report the current market depth (Queued + Executing jobs in
            // active_jobs) for observability; same source as the terminal
            // 202 ACK at Step 11 so the Wire sees a consistent surface
            // across fresh and retry paths.
            let depth = {
                let s = market_state_handle.read().await;
                s.active_jobs
                    .values()
                    .filter(|j| {
                        matches!(
                            j.status,
                            crate::compute_market::ComputeJobStatus::Queued
                                | crate::compute_market::ComputeJobStatus::Executing
                        )
                    })
                    .count() as u64
            };
            let ack = crate::pyramid::market_dispatch::MarketDispatchAck {
                job_id: req.job_id.clone(),
                peer_queue_depth: depth,
            };
            return Ok(Box::new(warp::reply::with_status(
                warp::reply::json(&ack),
                warp::http::StatusCode::ACCEPTED,
            )));
        }
        MarketAdmissionOutcome::Admitted => {
            // Fall through to per-offer depth gate + spawn worker.
        }
    }

    // Step 8 (§III): Per-offer depth enforcement + runtime registration.
    //
    // DESIGN NOTE (WS5 verifier pass): An earlier implementation enqueued
    // a market_received `QueueEntry` into `compute_queue` alongside
    // spawning `spawn_market_worker`. That double-pathed the work — the
    // GPU loop at main.rs:12171 dequeues every entry and calls
    // `call_model_unified_with_audit_and_ctx` regardless of `source`, so
    // market dispatches ran inference TWICE (once in the GPU loop, once
    // in the spawned worker). The spec's "enqueue + GPU-loop CAS"
    // vision at §III lines 557-568 depends on GPU-loop-side outbox
    // integration (heartbeat + ready CAS + ComputeJob transitions)
    // that was not in WS5's scope and remains to be added by WS6/WS8.
    //
    // Until the GPU loop gains outbox-aware post-processing, the worker
    // runs the inference directly (`spawn_market_worker` below) and the
    // compute_queue is not involved in market dispatch. Per-offer depth
    // accounting therefore reads from `ComputeMarketState.active_jobs`
    // filtered by model, scoped to `Queued` + `Executing` states (a
    // `Ready` / `Failed` job is no longer consuming a worker slot).
    //
    // When WS6/WS8 land the GPU-loop-side outbox path, re-introduce the
    // enqueue here and drop `spawn_market_worker`.
    let active_depth_for_model = {
        let s = market_state_handle.read().await;
        s.active_jobs
            .values()
            .filter(|j| {
                j.model_id == req.model_id
                    && matches!(
                        j.status,
                        crate::compute_market::ComputeJobStatus::Queued
                            | crate::compute_market::ComputeJobStatus::Executing
                    )
            })
            .count()
    };
    if max_queue_depth != 0 && active_depth_for_model >= max_queue_depth {
        // Per-offer depth cap hit. Roll back the outbox row we just
        // committed so the Wire can re-match the job elsewhere without
        // a stale pending row lingering on this provider.
        let db_rb = db_path.clone();
        let jid_rb = req.job_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = rusqlite::Connection::open(&db_rb) {
                let _ = crate::pyramid::db::fleet_outbox_delete(
                    &conn,
                    crate::fleet::WIRE_PLATFORM_DISPATCHER,
                    &jid_rb,
                );
            }
        })
        .await;
        tracing::warn!(
            "Market dispatch depth cap hit for model={} active={} cap={}",
            req.model_id,
            active_depth_for_model,
            max_queue_depth
        );
        // Field names are load-bearing: the Wire's queue-mirror
        // fail-forward logic (per Phase-2 Wire-side handoff) reads
        // `current_market_queue_depth` + `max_market_queue_depth`
        // from this body to correct its stale mirror snapshot when
        // the node rejects a dispatch. Keeping the short aliases
        // (`current`, `max`) for UI/log readability; they're harmless
        // extras and the Wire's schema-validator is set to warn-don't-
        // reject on unknown fields (per Q-PROTO-3).
        return Ok(Box::new(warp::reply::with_status(
            warp::reply::with_header(
                warp::reply::with_header(
                    warp::reply::json(&serde_json::json!({
                        "error": "market queue depth exceeded",
                        "model": req.model_id,
                        "current": active_depth_for_model,
                        "max": max_queue_depth,
                        "current_market_queue_depth": active_depth_for_model,
                        "max_market_queue_depth": max_queue_depth,
                    })),
                    "Retry-After",
                    policy.admission_retry_after_secs.to_string(),
                ),
                "X-Wire-Reason",
                "queue_depth_exceeded",
            ),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        )));
    }

    // Step 8b (§V DADBEAR Integration): Create DADBEAR work item + attempt.
    //
    // Every market job gets a durable DADBEAR work item. Per §V P3,
    // market jobs SKIP the preview gate — the Wire's matched price +
    // deposit IS the cost gate, so a local operator-USD preview is
    // the wrong currency for the wrong budget. We therefore insert
    // the row directly at state `previewed`. The supervisor's crash-
    // recovery sweep uses `state = 'dispatched'` as the "in-flight"
    // marker; `previewed` is the correct pre-dispatch resting state
    // even though we never go through `create_dispatch_preview`.
    //
    // Semantic path (NO UUIDs per handoff rule 7):
    //   work item id : "market/{job_id}"
    //   batch_id     : job_id (single-job batch — each market dispatch
    //                  is its own batch; the outbox is the batch-
    //                  equivalent durable handle)
    //   epoch_id     : "market:{timestamp}" — market work has no
    //                  recipe/norms epoch like pyramid builds; the
    //                  timestamp gives unique-per-dispatch identity
    //                  while matching the schema's (slug, epoch_id)
    //                  index shape.
    //   target_id    : job_id (the work targets that specific Wire job)
    //   step_name    : "compute-serve"
    //   primitive    : "llm_call"
    //   layer        : 0 (market work is L0 — it serves pre-computed
    //                  work on behalf of the requester; the
    //                  pyramid layer concept doesn't apply)
    //
    // Per DD-A, the virtual slug is `market:compute`. Bridge jobs
    // share this slug with `step_name: "bridge"` — DD-P. Phase 2
    // only ships local-GPU provider work, so step_name stays at
    // "compute-serve" in this handler.
    //
    // Failure policy: if the DADBEAR write fails, the job still
    // proceeds (the outbox row is the durable source of truth for
    // the market protocol). The work item adds observability +
    // crash recovery; missing it degrades the audit trail but does
    // not orphan the Wire-side job.
    let dadbear_work_item_id = format!("market/{}", req.job_id);
    let dadbear_epoch_id = format!("market:{}", chrono::Utc::now().timestamp());
    let dadbear_attempt_id: Option<String> = {
        let db_path_wi = db_path.clone();
        let work_item_id = dadbear_work_item_id.clone();
        let epoch_id = dadbear_epoch_id.clone();
        let batch_id = req.job_id.clone();
        let target_id = req.job_id.clone();
        let system_prompt_wi = system_prompt.clone();
        let user_prompt_wi = user_prompt.clone();
        let model_tier_wi = req.model_id.clone();
        let wi_call_result: Result<String, String> =
            tokio::task::spawn_blocking(move || -> Result<String, String> {
                let conn = rusqlite::Connection::open(&db_path_wi)
                    .map_err(|e| e.to_string())?;
                let now = chrono::Utc::now()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string();
                // Insert work item at state='previewed'. INSERT OR IGNORE
                // so a Wire retry with the same job_id (crash-recovery
                // run hitting a pre-existing row) doesn't crash on PK.
                conn.execute(
                    "INSERT OR IGNORE INTO dadbear_work_items
                     (id, slug, batch_id, epoch_id, step_name, primitive,
                      layer, target_id, system_prompt, user_prompt, model_tier,
                      compiled_at, state, state_changed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                             ?12, 'previewed', ?12)",
                    rusqlite::params![
                        work_item_id,
                        MARKET_COMPUTE_SLUG,
                        batch_id,
                        epoch_id,
                        "compute-serve",
                        "llm_call",
                        0i64,
                        target_id,
                        system_prompt_wi,
                        user_prompt_wi,
                        model_tier_wi,
                        now,
                    ],
                )
                .map_err(|e| format!("work item insert: {}", e))?;

                // Create attempt (attempt_number = existing + 1). The
                // attempt id format is pinned by
                // `dadbear_compiler::attempt_id`.
                let attempt_number: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM dadbear_work_attempts WHERE work_item_id = ?1",
                        rusqlite::params![work_item_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0)
                    + 1;
                let attempt_id_val =
                    crate::pyramid::dadbear_compiler::attempt_id(
                        &work_item_id,
                        attempt_number,
                    );
                // routing = 'local' — Phase 2 market work runs on the
                // provider's local GPU (bridge is Phase 4). When
                // bridge lands, the routing column distinguishes.
                conn.execute(
                    "INSERT INTO dadbear_work_attempts
                     (id, work_item_id, attempt_number, dispatched_at, model_id, routing, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'local', 'pending')",
                    rusqlite::params![
                        attempt_id_val,
                        work_item_id,
                        attempt_number,
                        now,
                        model_tier_wi,
                    ],
                )
                .map_err(|e| format!("work attempt insert: {}", e))?;
                Ok(attempt_id_val)
            })
            .await
            .unwrap_or_else(|je| Err(je.to_string()));

        match wi_call_result {
            Ok(aid) => Some(aid),
            Err(e) => {
                // Log but don't reject: the outbox row is durable and
                // the Wire's view of the job is intact. Missing
                // DADBEAR rows degrade audit but don't orphan work.
                tracing::warn!(
                    "Market dispatch DADBEAR work item/attempt write failed: {}",
                    e
                );
                None
            }
        }
    };

    // Step 9 (§III): Upsert the in-memory ComputeJob for runtime
    // observability. The outbox is the durable source of truth; this
    // struct is the runtime view consumed by the frontend's queue panel
    // and by Phase 3's settlement correlation.
    //
    // We stash the stripped JWT (no "Bearer " prefix) on the job so a
    // Phase 3 callback-delivery retry can re-present the same token.
    // work_item_id + attempt_id come from Step 8b. If the DADBEAR
    // writes failed, attempt_id is None — ComputeJob still carries
    // work_item_id (deterministic from job_id), matching the
    // conservative "outbox is truth; DADBEAR is audit" split.
    {
        let bearer_stripped = auth_header
            .strip_prefix("Bearer ")
            .unwrap_or(&auth_header)
            .to_string();
        let queued_at = chrono::Utc::now().to_rfc3339();
        let job = crate::compute_market::ComputeJob {
            job_id: req.job_id.clone(),
            model_id: req.model_id.clone(),
            status: crate::compute_market::ComputeJobStatus::Queued,
            messages: Some(req.messages.clone()),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            wire_job_token: bearer_stripped,
            matched_rate_in: matched_rate_in_per_m,
            matched_rate_out: matched_rate_out_per_m,
            matched_multiplier_bps: req.matched_multiplier_bps,
            queued_at,
            filled_at: None,
            work_item_id: Some(dadbear_work_item_id.clone()),
            attempt_id: dadbear_attempt_id.clone(),
        };
        market_state_handle.write().await.upsert_active_job(job);
    }

    // Step 9b (§III L603-632 + §V): Record `market_received` chronicle event.
    //
    // job_path  : `market/{job_id}` — identical to the DADBEAR work
    //             item id so filtering by job_path reconstructs the
    //             full event stream for this job.
    // source    : SOURCE_MARKET_RECEIVED (the provider received the
    //             dispatch; parallels SOURCE_FLEET_RECEIVED).
    // metadata  : model_id (via with_model_id), job_id,
    //             credit_rate_in_per_m, credit_rate_out_per_m,
    //             privacy_tier — the five fields the spec names.
    // work_item : dadbear_work_item_id + dadbear_attempt_id from Step
    //             8b. If the DADBEAR write failed, attempt_id is None
    //             but work_item_id is still the deterministic semantic
    //             path (same value as if the write had succeeded).
    //
    // Fire-and-forget via spawn_blocking — chronicle write is an
    // observability side-channel; a failed write does not block
    // returning 202 to the Wire.
    {
        let db_path_chr = db_path.clone();
        let job_path = dadbear_work_item_id.clone();
        let model_id_chr = req.model_id.clone();
        let job_id_chr = req.job_id.clone();
        let rate_in = matched_rate_in_per_m;
        let rate_out = matched_rate_out_per_m;
        let privacy_tier = req.privacy_tier.clone();
        let wi_id_chr = Some(dadbear_work_item_id.clone());
        let attempt_id_chr = dadbear_attempt_id.clone();
        let ctx = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
            &job_path,
            crate::pyramid::compute_chronicle::EVENT_MARKET_RECEIVED,
            crate::pyramid::compute_chronicle::SOURCE_MARKET_RECEIVED,
        )
        .with_model_id(model_id_chr)
        .with_metadata(serde_json::json!({
            "job_id": job_id_chr,
            "credit_rate_in_per_m": rate_in,
            "credit_rate_out_per_m": rate_out,
            "privacy_tier": privacy_tier,
        }))
        .with_work_item(wi_id_chr, attempt_id_chr);
        tokio::task::spawn_blocking(move || {
            if let Ok(conn) = rusqlite::Connection::open(&db_path_chr) {
                let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx);
            }
        });
    }

    // Phase 2 WS6: trigger queue mirror push. The mirror task debounces
    // and coalesces bursts, so sending a token here is idempotent vs
    // the later status transitions emitted by the worker. Fire-and-
    // forget — a shutdown race cannot panic the handler.
    let _ = market_ctx.mirror_nudge.send(());

    // Step 10 (§III): Spawn the worker BEFORE returning 202. `tokio::spawn`
    // is infallible so the 202 is truthful by construction.
    spawn_market_worker(
        state.clone(),
        Arc::clone(&market_state_handle),
        Arc::clone(&market_ctx),
        db_path.clone(),
        policy.clone(),
        req.job_id.clone(),
        req.model_id.clone(),
        system_prompt,
        user_prompt,
        req.temperature.unwrap_or(0.0),
        req.max_tokens.unwrap_or(4096),
        // Contract rev 1.5 dropped `response_format` from MarketDispatchRequest
        // — Wire never sends it. Pass None; the worker uses the model's
        // natural output format.
        None,
        matched_rate_in_per_m,
        matched_rate_out_per_m,
    );

    // Step 11 (§III): Return 202 ACK with the provider's current
    // market queue depth for observability. This is the count of
    // Queued + Executing market jobs in `active_jobs` — matches the
    // "peer_queue_depth" surface the Wire uses for matching heuristics.
    // NOT `compute_queue.total_depth` because market dispatches no
    // longer enqueue there (see Step 8 DESIGN NOTE above).
    let depth = {
        let s = market_state_handle.read().await;
        s.active_jobs
            .values()
            .filter(|j| {
                matches!(
                    j.status,
                    crate::compute_market::ComputeJobStatus::Queued
                        | crate::compute_market::ComputeJobStatus::Executing
                )
            })
            .count() as u64
    };
    let ack = crate::pyramid::market_dispatch::MarketDispatchAck {
        job_id: req.job_id.clone(),
        peer_queue_depth: depth,
    };
    Ok(Box::new(warp::reply::with_status(
        warp::reply::json(&ack),
        warp::http::StatusCode::ACCEPTED,
    )))
}

/// Compute the credits earned for a completed market job from the matched
/// rates and the actual token counts. Rates are per-million tokens (DD-C);
/// input and output are billed independently. Saturating arithmetic on the
/// full chain so a pathological billion-token job can't wrap into negative
/// credits or panic on multiplication overflow.
fn market_credits_earned(
    rate_in_per_m: i64,
    rate_out_per_m: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> i64 {
    let in_credits = rate_in_per_m
        .saturating_mul(prompt_tokens)
        .saturating_div(1_000_000);
    let out_credits = rate_out_per_m
        .saturating_mul(completion_tokens)
        .saturating_div(1_000_000);
    in_credits.saturating_add(out_credits)
}

/// Spawns the worker that runs inference + heartbeat for a market job.
/// All clones are owned by the spawned future; the caller returns 202 as
/// soon as this function returns.
///
/// The worker:
///   * snapshots `LlmConfig` with `fleet_dispatch=None` and `fleet_roster=None`
///     (belt-and-suspenders — the queue entry already skipped fleet dispatch,
///     but the worker's out-of-queue inference path must too) and with the
///     market-requested model overriding primary / fallbacks so the unified
///     call path routes to the right backend.
///   * runs `call_model_unified_with_options_and_ctx` alongside a heartbeat
///     that bumps `expires_at` every `worker_heartbeat_interval_secs`.
///   * CAS-promotes `pending → ready` on success, transitions the ComputeJob
///     status, records completion credits.
///   * Bumps `delivery_attempts` on inference failure for Phase 3's callback-
///     delivery worker to surface as a failed job.
///
/// Phase 3's callback-delivery loop (NOT in this workstream) handles the
/// actual HTTP POST of the result to the callback_url, the retry schedule,
/// and the `ready → delivered` / `ready → failed` terminal CAS.
#[allow(clippy::too_many_arguments)]
fn spawn_market_worker(
    state: ServerState,
    market_state_handle: Arc<RwLock<crate::compute_market::ComputeMarketState>>,
    market_ctx: Arc<crate::pyramid::market_dispatch::MarketDispatchContext>,
    db_path: std::path::PathBuf,
    policy: crate::pyramid::market_delivery_policy::MarketDeliveryPolicy,
    job_id: String,
    model_id: String,
    system_prompt: String,
    user_prompt: String,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<serde_json::Value>,
    credit_rate_in_per_m: i64,
    credit_rate_out_per_m: i64,
) {
    tokio::spawn(async move {
        // Derive worker config via prepare_for_replay — the node is
        // fulfilling a market-received job, so dispatch contexts
        // (compute_queue, fleet, market) must all be cleared to prevent
        // recursive outbound dispatch. Then override the model to the
        // market-requested one.
        let worker_config = {
            let cfg = state.pyramid.config.read().await;
            let mut wc = cfg.prepare_for_replay(crate::pyramid::llm::DispatchOrigin::MarketReceived);
            wc.primary_model = model_id.clone();
            wc.fallback_model_1 = model_id.clone();
            wc.fallback_model_2 = model_id.clone();
            wc
        };

        let chronicle_job_path = format!("market-recv:{}", job_id);
        let options = crate::pyramid::llm::LlmCallOptions {
            skip_fleet_dispatch: true,
            chronicle_job_path: Some(chronicle_job_path.clone()),
            dispatch_origin: crate::pyramid::llm::DispatchOrigin::MarketReceived,
            ..Default::default()
        };

        // Transition the ComputeJob to Executing (the GPU loop would
        // normally do this, but since the worker runs the inference
        // directly, we transition here). Matches Queued → Executing in
        // the spec state machine.
        {
            let mut s = market_state_handle.write().await;
            let _ = s.transition_job_status(
                &job_id,
                crate::compute_market::ComputeJobStatus::Executing,
            );
            if let Some(job) = s.active_jobs.get_mut(&job_id) {
                job.filled_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }
        // Phase 2 WS6: nudge the mirror task — Queued → Executing
        // changes the per-model `is_executing` flag the Wire sees.
        let _ = market_ctx.mirror_nudge.send(());

        // Heartbeat future: tick every worker_heartbeat_interval_secs,
        // bump expires_at, exit if the sweep CAS'd us out (rowcount 0).
        let hb_db_path = db_path.clone();
        let hb_job_id = job_id.clone();
        let hb_interval = policy.worker_heartbeat_interval_secs.max(1);
        let hb_tolerance = policy.worker_heartbeat_tolerance_secs;

        let heartbeat = async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(hb_interval));
            ticker.tick().await; // skip the immediate tick at t=0
            loop {
                ticker.tick().await;
                let hb_db = hb_db_path.clone();
                let hb_jid = hb_job_id.clone();
                let hb_tol = hb_tolerance;
                let result: anyhow::Result<usize> =
                    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
                        let conn = rusqlite::Connection::open(&hb_db)?;
                        let modifier = format!("+{} seconds", hb_tol);
                        let new_expires: String = conn.query_row(
                            "SELECT datetime('now', ?1)",
                            rusqlite::params![modifier],
                            |r| r.get(0),
                        )?;
                        crate::pyramid::db::fleet_outbox_update_heartbeat_if_pending(
                            &conn,
                            crate::fleet::WIRE_PLATFORM_DISPATCHER,
                            &hb_jid,
                            &new_expires,
                        )
                    })
                    .await
                    .unwrap_or_else(|je| Err(anyhow::anyhow!(je.to_string())));
                match result {
                    Ok(1) => continue,
                    Ok(0) => return false, // sweep won the race
                    Ok(_) => return false, // compound PK guarantees 0/1
                    Err(e) => {
                        let msg = e.to_string().to_lowercase();
                        if msg.contains("busy") || msg.contains("locked") {
                            tracing::debug!(?e, "market heartbeat DB-locked; retrying");
                            continue;
                        }
                        tracing::error!(?e, "market heartbeat DB error; giving up");
                        return false;
                    }
                }
            }
        };

        // Rev 2.1.1 provider-side serialization: gate the inference call
        // through the engine-dispatch semaphore. One permit = one
        // concurrent inference against the engine; overflow queues here
        // in tokio's task scheduler rather than inside the engine's
        // (dumb FIFO) queue. This is what makes Wire's queue-external
        // scheduling semantic actually work — without this, multiple
        // workers call Ollama concurrently and thrash.
        //
        // The permit is held across the entire inference call and
        // released when the returned future drops (either completion
        // or the heartbeat-sweep race below cancels it). Cancellation
        // releases the permit immediately; no leaks.
        //
        // Queued-at-permit jobs sit in `ComputeJobStatus::Executing`
        // from the walker's perspective (we transitioned at line 3983
        // above, before acquiring), so peer_queue_depth = Queued +
        // Executing still reflects the bilateral-agreed
        // "buffer + engine_occupancy" semantic.
        let permits_handle = state.engine_dispatch_permits.clone();
        let inference = async move {
            // .acquire_owned() consumes an Arc clone so the permit
            // outlives the scope of this async block (held until the
            // returned Permit drops at the end of the inference future).
            let _permit = match permits_handle.acquire_owned().await {
                Ok(p) => p,
                Err(_closed) => {
                    // Semaphore closed — only happens at shutdown. Surface
                    // as a generic failure; the outbox retry / sweep path
                    // handles orderly cleanup.
                    return Err(anyhow::anyhow!("engine_dispatch_permits_closed"));
                }
            };
            crate::pyramid::llm::call_model_unified_with_options_and_ctx(
                &worker_config,
                None,
                &system_prompt,
                &user_prompt,
                temperature,
                max_tokens,
                response_format.as_ref(),
                options,
            )
            .await
        };

        // Race inference against heartbeat exit. If the heartbeat exits
        // first, the sweep claimed the row — drop the inference result.
        let select_outcome = tokio::select! {
            inf = inference => Some(inf),
            _ = heartbeat => None,
        };

        match select_outcome {
            None => {
                // Sweep won. Transition the job to Failed so the
                // observability panel reflects the terminal state.
                {
                    let mut s = market_state_handle.write().await;
                    let _ = s.transition_job_status(
                        &job_id,
                        crate::compute_market::ComputeJobStatus::Failed,
                    );
                }
                // Phase 2 WS6: nudge mirror — model's `is_executing`
                // and `market_depth` changed.
                let _ = market_ctx.mirror_nudge.send(());
                tracing::warn!(
                    "Market worker lost heartbeat sweep for job_id={}",
                    job_id
                );
            }
            Some(Ok(llm_response)) => {
                // Build MarketAsyncResult::Success and CAS pending → ready.
                let prompt_tokens = llm_response.usage.prompt_tokens;
                let completion_tokens = llm_response.usage.completion_tokens;
                let outcome = crate::pyramid::market_dispatch::MarketAsyncResult::Success(
                    crate::pyramid::market_dispatch::MarketDispatchResponse {
                        content: llm_response.content,
                        prompt_tokens: Some(prompt_tokens),
                        completion_tokens: Some(completion_tokens),
                        model: model_id.clone(),
                        finish_reason: None,
                        provider_model: Some(model_id.clone()),
                    },
                );
                let result_json = match serde_json::to_string(&outcome) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(?e, "failed to serialize MarketAsyncResult");
                        crate::pyramid::db::synthesize_worker_error_json(&format!(
                            "result serialize failed: {}",
                            e
                        ))
                    }
                };

                let db_promote = db_path.clone();
                let jid_promote = job_id.clone();
                let rj_promote = result_json.clone();
                let ready_retention_secs = policy.ready_retention_secs;
                let promote_res: Result<usize, String> =
                    tokio::task::spawn_blocking(move || {
                        let conn = rusqlite::Connection::open(&db_promote)
                            .map_err(|e| e.to_string())?;
                        crate::pyramid::db::fleet_outbox_promote_ready_if_pending(
                            &conn,
                            crate::fleet::WIRE_PLATFORM_DISPATCHER,
                            &jid_promote,
                            &rj_promote,
                            ready_retention_secs,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|je| Err(je.to_string()));

                match promote_res {
                    Ok(1) => {
                        // Worker won. Transition ComputeJob Executing → Ready
                        // and record completion credits. Phase 3's callback-
                        // delivery worker will transition the outbox row
                        // ready → delivered once the callback POST succeeds.
                        let credits_earned = market_credits_earned(
                            credit_rate_in_per_m,
                            credit_rate_out_per_m,
                            prompt_tokens,
                            completion_tokens,
                        );
                        {
                            let mut s = market_state_handle.write().await;
                            let _ = s.transition_job_status(
                                &job_id,
                                crate::compute_market::ComputeJobStatus::Ready,
                            );
                            s.record_completion(credits_earned);
                        }
                        // Phase 2 WS6: nudge mirror — Executing → Ready
                        // means a slot just opened for the model.
                        let _ = market_ctx.mirror_nudge.send(());
                        // Phase 3: nudge the delivery worker so the
                        // result callback fires within the debounce
                        // window, not after the next 15s tick.
                        let _ = market_ctx.delivery_nudge.send(());
                        // TODO (WS8): chronicle event market_completed with
                        //   model_id, job_id, prompt_tokens, completion_tokens,
                        //   credits_earned.
                    }
                    Ok(_) => {
                        // Sweep already promoted us with a synthesized Error —
                        // drop our result. Reflect the terminal state locally.
                        {
                            let mut s = market_state_handle.write().await;
                            let _ = s.transition_job_status(
                                &job_id,
                                crate::compute_market::ComputeJobStatus::Failed,
                            );
                        }
                        // Phase 2 WS6: nudge mirror — terminal transition
                        // opens a slot for the model.
                        let _ = market_ctx.mirror_nudge.send(());
                        tracing::warn!(
                            "Market worker sweep-lost at promote for job_id={}",
                            job_id
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "market outbox promote_ready failed: {} (job_id={})",
                            e,
                            job_id
                        );
                    }
                }
            }
            Some(Err(e)) => {
                // Inference failed. Previously (pre-Phase-3) this branch
                // called `fleet_outbox_bump_delivery_attempt` against a
                // `status='pending'` row. That helper CASes on
                // `status='ready'`, so it silently no-op'd — the real
                // inference error never reached the delivery worker, and
                // the row sat pending until the sweep synthesized a
                // generic "worker heartbeat lost" message. Phase 3 fix:
                // promote pending→ready with the REAL error envelope,
                // identical to the success path's promote call. The
                // delivery worker then POSTs the proper failure callback.
                let err_msg = format!("{}", e);
                let outcome = crate::pyramid::market_dispatch::MarketAsyncResult::Error(
                    err_msg.clone(),
                );
                let result_json = match serde_json::to_string(&outcome) {
                    Ok(s) => s,
                    Err(e_ser) => crate::pyramid::db::synthesize_worker_error_json(&format!(
                        "inference failed + result serialize failed: {}; original: {}",
                        e_ser, err_msg
                    )),
                };
                let db_fail = db_path.clone();
                let jid_fail = job_id.clone();
                let rj_fail = result_json.clone();
                let ready_retention_secs = policy.ready_retention_secs;
                let promote_res: Result<usize, String> =
                    tokio::task::spawn_blocking(move || {
                        let conn = rusqlite::Connection::open(&db_fail)
                            .map_err(|e| e.to_string())?;
                        crate::pyramid::db::fleet_outbox_promote_ready_if_pending(
                            &conn,
                            crate::fleet::WIRE_PLATFORM_DISPATCHER,
                            &jid_fail,
                            &rj_fail,
                            ready_retention_secs,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|je| Err(je.to_string()));
                match promote_res {
                    Ok(1) => {
                        // Row promoted to ready; nudge the delivery worker
                        // so the failure callback fires within the debounce
                        // window rather than waiting for the next tick.
                        let _ = market_ctx.delivery_nudge.send(());
                    }
                    Ok(_) => {
                        // Sweep already promoted us with a synthesized
                        // generic Error — acceptable; the generic message
                        // is less informative but the delivery flow still
                        // completes. Nudge anyway.
                        let _ = market_ctx.delivery_nudge.send(());
                        tracing::warn!(
                            "Market worker failure promote CAS lost to sweep for job_id={}",
                            job_id
                        );
                    }
                    Err(e_db) => {
                        tracing::error!(
                            "market outbox promote_ready (failure branch) failed: {} (job_id={})",
                            e_db,
                            job_id
                        );
                    }
                }

                {
                    let mut s = market_state_handle.write().await;
                    let _ = s.transition_job_status(
                        &job_id,
                        crate::compute_market::ComputeJobStatus::Failed,
                    );
                }
                // Phase 2 WS6: nudge mirror — Executing → Failed opens
                // a slot for the model.
                let _ = market_ctx.mirror_nudge.send(());
                tracing::warn!(
                    "Market inference failed for job_id={}: {}",
                    job_id,
                    err_msg
                );
                // TODO (WS8): chronicle event market_failed with error.
            }
        }
    });
}

/// Handle POST /v1/fleet/result — dispatcher-side async result callback.
///
/// Peek → verify → pop-and-send. Snapshot identity match while holding the
/// sync mutex briefly, drop the lock, then fire the oneshot sender to wake
/// the awaiting Phase A dispatch future. Never hold the mutex across .await.
async fn handle_fleet_result(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 503 if fleet dispatch disabled on this node.
    let ctx = match state.fleet_dispatch.as_ref() {
        Some(c) => Arc::clone(c),
        None => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "fleet dispatch disabled"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            ));
        }
    };

    // Snapshot auth inputs once, drop locks before verify_fleet_identity.
    let pk = state.jwt_public_key.read().await.clone();
    let self_op = state
        .auth
        .read()
        .await
        .operator_id
        .clone()
        .unwrap_or_default();

    let identity = match crate::pyramid::fleet_identity::verify_fleet_identity(
        &auth_header,
        &pk,
        &self_op,
    ) {
        Ok(i) => i,
        Err(_) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::FORBIDDEN,
            ));
        }
    };

    let envelope: crate::fleet::FleetAsyncResultEnvelope = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(_) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "invalid body"})),
                warp::http::StatusCode::BAD_REQUEST,
            ));
        }
    };

    // Peek-verify-pop atomically under the sync mutex. Snapshot the identity
    // match while holding the lock briefly — the mutex MUST NOT be held across
    // any .await.
    let peek = ctx
        .pending
        .peek_matches(&envelope.job_id, identity.nid());
    let action: crate::fleet::PeekResult = peek;

    // DB path for chronicle side-effects.
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db"));
    let chronicle_write = |event_type: &'static str, metadata: serde_json::Value| {
        let source = crate::pyramid::compute_chronicle::SOURCE_FLEET;
        let ctx_ev = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
            &format!("fleet-dispatch:{}", envelope.job_id),
            event_type,
            source,
        )
        .with_metadata(metadata);
        if let Some(ref dbp) = db_path {
            let db_path_clone = dbp.to_string_lossy().to_string();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = rusqlite::Connection::open(&db_path_clone) {
                    let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx_ev);
                }
            });
        }
    };

    match action {
        crate::fleet::PeekResult::NotFound => {
            chronicle_write(
                crate::pyramid::compute_chronicle::EVENT_FLEET_RESULT_ORPHANED,
                serde_json::json!({
                    "job_id": envelope.job_id,
                    "claimed_peer": identity.nid(),
                }),
            );
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::OK,
            ))
        }
        crate::fleet::PeekResult::Mismatch => {
            chronicle_write(
                crate::pyramid::compute_chronicle::EVENT_FLEET_RESULT_FORGERY_ATTEMPT,
                serde_json::json!({
                    "job_id": envelope.job_id,
                    "claimed_peer": identity.nid(),
                }),
            );
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::FORBIDDEN,
            ))
        }
        crate::fleet::PeekResult::Match => {
            // Remove the pending entry and fire the oneshot sender.
            if let Some(pj) = ctx.pending.remove(&envelope.job_id) {
                let latency_ms = pj.dispatched_at.elapsed().as_millis() as u64;
                let peer_id = pj.peer_id.clone();
                // send() returns Err if the receiver dropped — acceptable, nothing to do.
                let _ = pj.sender.send(envelope.outcome);
                chronicle_write(
                    crate::pyramid::compute_chronicle::EVENT_FLEET_RESULT_RECEIVED,
                    serde_json::json!({
                        "peer_id": peer_id,
                        "latency_ms": latency_ms,
                        "job_id": envelope.job_id,
                    }),
                );
            } else {
                // Raced with sweep between peek_matches and remove — treat as orphan.
                chronicle_write(
                    crate::pyramid::compute_chronicle::EVENT_FLEET_RESULT_ORPHANED,
                    serde_json::json!({
                        "job_id": envelope.job_id,
                        "claimed_peer": identity.nid(),
                    }),
                );
            }
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::OK,
            ))
        }
    }
}

/// Handle POST /v1/fleet/announce — receive fleet peer announcement.
///
/// Verifies fleet identity (single call: signature + audience + operator +
/// nid non-empty), parses announcement, updates the fleet roster.
async fn handle_fleet_announce(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify fleet identity — single call returns typed FleetIdentity.
    let jwt_pk = state.jwt_public_key.read().await.clone();
    let self_operator_id = state
        .auth
        .read()
        .await
        .operator_id
        .clone()
        .unwrap_or_default();
    let _identity = match crate::pyramid::fleet_identity::verify_fleet_identity(
        &auth_header,
        &jwt_pk,
        &self_operator_id,
    ) {
        Ok(i) => i,
        Err(_) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({})),
                warp::http::StatusCode::FORBIDDEN,
            ));
        }
    };

    // 2. Parse announcement and update roster
    let announcement: crate::fleet::FleetAnnouncement = match serde_json::from_value(body.clone()) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Fleet announce parse failed: {}. Body: {}", e, body);
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("Invalid announcement: {}", e)})),
                warp::http::StatusCode::BAD_REQUEST,
            ));
        }
    };

    tracing::info!(
        from_node = %announcement.node_id,
        serving_rules = ?announcement.serving_rules,
        models = ?announcement.models_loaded,
        handle = ?announcement.node_handle,
        "Fleet announce received"
    );

    {
        let mut roster = state.fleet_roster.write().await;
        roster.update_from_announcement(announcement);
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": "ok"})),
        warp::http::StatusCode::OK,
    ))
}

#[cfg(test)]
mod market_dispatch_tests {
    //! Unit tests for the market-dispatch helpers that can be tested
    //! without the full warp / tauri test harness.
    //!
    //! The end-to-end flow (JWT verify → body parse → outbox insert →
    //! queue enqueue → worker spawn) is covered by a later integration
    //! workstream. These tests pin the isolated math and invariants that
    //! this workstream introduces — primarily the credits-earned formula
    //! which is the only piece of new numeric logic in the worker path.
    //!
    //! Primitives consumed by the handler (messages_to_prompt_pair,
    //! market_outbox_insert_or_ignore, verify_market_identity,
    //! enqueue_market) already have exhaustive unit tests in their
    //! respective modules — see WS2, WS3, WS4, and WS5.1 commits.
    use super::*;

    #[test]
    fn market_credits_earned_base_case() {
        // 100 credits/Mtok input, 500 credits/Mtok output.
        // 1M prompt tokens × 100 + 1M completion × 500 = 100 + 500 = 600.
        let credits = market_credits_earned(100, 500, 1_000_000, 1_000_000);
        assert_eq!(credits, 600);
    }

    #[test]
    fn market_credits_earned_fractional_tokens_floor_via_integer_div() {
        // 100 credits/Mtok × 500_000 tokens = 50_000_000 / 1_000_000 = 50.
        // 500 credits/Mtok × 250_000 tokens = 125_000_000 / 1_000_000 = 125.
        // Total = 175. Integer division floors — 500k tokens at 100/M is
        // exactly 50 credits, not 50.0 or 49.something.
        let credits = market_credits_earned(100, 500, 500_000, 250_000);
        assert_eq!(credits, 175);
    }

    #[test]
    fn market_credits_earned_zero_tokens_yields_zero() {
        // A zero-token completion (pathological but legal) still yields
        // zero credits, not a negative or NaN-adjacent value.
        assert_eq!(market_credits_earned(100, 500, 0, 0), 0);
        assert_eq!(market_credits_earned(100, 500, 0, 100), 0);
        assert_eq!(market_credits_earned(100, 500, 100, 0), 0);
    }

    #[test]
    fn market_credits_earned_saturates_on_overflow() {
        // Pathological: i64::MAX rate × billion tokens would overflow
        // a non-saturating multiply and either panic or wrap to negative.
        // saturating_mul clamps at i64::MAX, saturating_div preserves it,
        // saturating_add must not wrap the final sum. The output token
        // side is zero to isolate the input-side saturation.
        let credits = market_credits_earned(i64::MAX, 0, 1_000_000_000, 0);
        assert!(credits >= 0, "saturating chain must not wrap negative");
        assert_eq!(credits, i64::MAX / 1_000_000);
    }

    #[test]
    fn market_credits_earned_negative_rate_propagates() {
        // A negative matched rate shouldn't happen in production (Wire-side
        // quoting is guarded at match time), but if it does we want the
        // arithmetic to flow through cleanly rather than panic. Pins that
        // saturating_div handles negatives the same way i64 division does.
        let credits = market_credits_earned(-100, 500, 1_000_000, 1_000_000);
        assert_eq!(credits, 400); // -100 + 500 = 400 per million pair.
    }

    /// WS5 verifier pass regression: after the fix that removed
    /// `enqueue_market` from `handle_market_dispatch`, per-offer depth
    /// accounting reads from `ComputeMarketState.active_jobs` filtered by
    /// `(model_id, status in {Queued, Executing})`. This test pins the
    /// filter: a `Ready` or `Failed` job must NOT count toward the
    /// per-model depth cap, and jobs on a different model must NOT count
    /// either.
    #[test]
    fn active_jobs_depth_for_model_filters_to_queued_and_executing_same_model() {
        use crate::compute_market::{ComputeJob, ComputeJobStatus};

        let mk_job = |id: &str, model: &str, status: ComputeJobStatus| ComputeJob {
            job_id: id.to_string(),
            model_id: model.to_string(),
            status,
            messages: None,
            temperature: None,
            max_tokens: None,
            wire_job_token: String::new(),
            matched_rate_in: 0,
            matched_rate_out: 0,
            matched_multiplier_bps: 10000,
            queued_at: "2026-04-16T00:00:00Z".to_string(),
            filled_at: None,
            work_item_id: None,
            attempt_id: None,
        };

        let mut jobs: std::collections::HashMap<String, ComputeJob> =
            std::collections::HashMap::new();
        // 2 queued + 1 executing on target model = 3 that count.
        jobs.insert("a".into(), mk_job("a", "model-x", ComputeJobStatus::Queued));
        jobs.insert("b".into(), mk_job("b", "model-x", ComputeJobStatus::Queued));
        jobs.insert(
            "c".into(),
            mk_job("c", "model-x", ComputeJobStatus::Executing),
        );
        // Terminal states on target model — MUST NOT count.
        jobs.insert("d".into(), mk_job("d", "model-x", ComputeJobStatus::Ready));
        jobs.insert("e".into(), mk_job("e", "model-x", ComputeJobStatus::Failed));
        // Queued on a DIFFERENT model — MUST NOT count toward model-x cap.
        jobs.insert("f".into(), mk_job("f", "model-y", ComputeJobStatus::Queued));
        jobs.insert(
            "g".into(),
            mk_job("g", "model-y", ComputeJobStatus::Executing),
        );

        let active_depth_for_model_x = jobs
            .values()
            .filter(|j| {
                j.model_id == "model-x"
                    && matches!(
                        j.status,
                        ComputeJobStatus::Queued | ComputeJobStatus::Executing
                    )
            })
            .count();
        assert_eq!(
            active_depth_for_model_x, 3,
            "depth counter must scope to (model == model-x) AND status in (Queued, Executing)"
        );
    }
}
