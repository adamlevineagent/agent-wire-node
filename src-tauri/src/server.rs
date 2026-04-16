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
) {
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

    // Fleet CORS: permissive (peer-to-peer via Cloudflare tunnels).
    let fleet_cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"]);
    let fleet_routes = fleet_dispatch_route
        .or(fleet_announce_route)
        .or(fleet_result_route)
        .with(fleet_cors);

    let routes = preflight
        .or(public_html_with_cors)
        .or(openrouter_webhook_with_cors)
        .or(fleet_routes)
        .or(pyramid_routes
            .or(partner_routes)
            .or(auth_callback
                .or(auth_complete)
                .or(health)
                .or(tunnel_debug)
                .or(documents)
                .or(stats))
            .with(cors));

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
        // Build worker config with fleet recursion bypass fields zeroed.
        let fleet_config = {
            let cfg = state.pyramid.config.read().await;
            let mut fc = cfg.clone();
            fc.primary_model = resolved_model.clone();
            fc.fallback_model_1 = resolved_model.clone();
            fc.fallback_model_2 = resolved_model.clone();
            fc.fleet_dispatch = None; // prevent Phase A re-entry
            fc.fleet_roster = None;   // belt-and-suspenders — no peer candidates
            fc
        };

        let chronicle_job_path = format!("fleet-recv:{}:{}", dispatcher_nid, job_id);
        let options = crate::pyramid::llm::LlmCallOptions {
            skip_fleet_dispatch: true,
            chronicle_job_path: Some(chronicle_job_path.clone()),
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
