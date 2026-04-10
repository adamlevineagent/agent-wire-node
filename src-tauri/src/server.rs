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
/// Also runs WAL cleanup on startup. Called after pyramid DB is initialized.
pub async fn init_stale_engines(pyramid_state: &Arc<pyramid::PyramidState>) {
    // WAL cleanup: delete processed mutations older than 30 days
    {
        let conn = pyramid_state.writer.lock().await;
        let deleted_processed = conn
            .execute(
                "DELETE FROM pyramid_pending_mutations
             WHERE processed = 1 AND detected_at < datetime('now', '-30 days')",
                [],
            )
            .unwrap_or(0);

        let deleted_runaway = conn.execute(
            "DELETE FROM pyramid_pending_mutations
             WHERE processed = 0 AND cascade_depth >= 10 AND detected_at < datetime('now', '-30 days')",
            [],
        ).unwrap_or(0);

        if deleted_processed > 0 || deleted_runaway > 0 {
            tracing::info!(
                "WAL cleanup: removed {} processed and {} runaway mutations older than 30 days",
                deleted_processed,
                deleted_runaway
            );
        }
    }

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
    let (base_config, model) = {
        let config = pyramid_state.config.read().await;
        (config.clone(), config.primary_model.clone())
    };

    // Get the DB path from data_dir
    let db_path = match &pyramid_state.data_dir {
        Some(dir) => dir.join("pyramid.db").to_string_lossy().to_string(),
        None => {
            tracing::warn!("No data_dir set on PyramidState, skipping stale engine initialization");
            return;
        }
    };

    // Load all pyramid configs where auto_update = 1
    let configs: Vec<AutoUpdateConfig> = {
        let conn = pyramid_state.reader.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT c.slug, c.auto_update, c.debounce_minutes, c.min_changed_files,
                    c.runaway_threshold, c.breaker_tripped, c.breaker_tripped_at, c.frozen, c.frozen_at
             FROM pyramid_auto_update_config c
             JOIN pyramid_slugs s ON s.slug = c.slug
             WHERE c.auto_update = 1 AND s.archived_at IS NULL",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to query auto_update configs: {}", e);
                return;
            }
        };

        stmt.query_map([], |row| {
            Ok(AutoUpdateConfig {
                slug: row.get(0)?,
                auto_update: row.get::<_, i32>(1)? != 0,
                debounce_minutes: row.get(2)?,
                min_changed_files: row.get(3)?,
                runaway_threshold: row.get(4)?,
                breaker_tripped: row.get::<_, i32>(5)? != 0,
                breaker_tripped_at: row.get(6)?,
                frozen: row.get::<_, i32>(7)? != 0,
                frozen_at: row.get(8)?,
            })
        })
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
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

        // Create the engine
        let mut engine = PyramidStaleEngine::new(
            &slug,
            config.clone(),
            &db_path,
            base_config.clone(),
            &model,
            pyramid_state.operational.as_ref().clone(),
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

        // Check for unprocessed WAL entries and feed them into the engine's layer timers
        {
            let conn = pyramid_state.reader.lock().await;
            for layer in 0..=3 {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM pyramid_pending_mutations
                         WHERE processed = 0 AND slug = ?1 AND layer = ?2",
                        rusqlite::params![slug, layer],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if count > 0 {
                    tracing::info!(
                        "Pyramid '{}' layer {} has {} unprocessed WAL entries — starting timer",
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

    let routes = preflight
        .or(public_html_with_cors)
        .or(openrouter_webhook_with_cors)
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
    #[allow(dead_code)]
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
