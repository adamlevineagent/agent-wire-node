use std::sync::Arc;
use tokio::sync::RwLock;
use warp::Filter;
use warp::Reply;
use serde::{Serialize, Deserialize};

use crate::credits::CreditTracker;
use crate::auth::AuthState;
use crate::sync::SyncState;
use crate::tunnel;
use crate::pyramid;
use crate::pyramid::stale_engine::PyramidStaleEngine;
use crate::pyramid::types::AutoUpdateConfig;
use crate::pyramid::watcher::PyramidFileWatcher;
use crate::partner;

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
        let deleted_processed = conn.execute(
            "DELETE FROM pyramid_pending_mutations
             WHERE processed = 1 AND detected_at < datetime('now', '-30 days')",
            [],
        ).unwrap_or(0);

        let deleted_runaway = conn.execute(
            "DELETE FROM pyramid_pending_mutations
             WHERE processed = 0 AND cascade_depth >= 10 AND detected_at < datetime('now', '-30 days')",
            [],
        ).unwrap_or(0);

        if deleted_processed > 0 || deleted_runaway > 0 {
            tracing::info!(
                "WAL cleanup: removed {} processed and {} runaway mutations older than 30 days",
                deleted_processed, deleted_runaway
            );
        }
    }

    // Get API key and model from pyramid config
    let (api_key, model) = {
        let config = pyramid_state.config.read().await;
        (config.api_key.clone(), config.primary_model.clone())
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
            "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                    runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
             FROM pyramid_auto_update_config WHERE auto_update = 1"
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
            tracing::info!("Pyramid '{}' is frozen — skipping engine and watcher on startup", slug);
            continue;
        }

        // Create the engine
        let mut engine = PyramidStaleEngine::new(
            &slug, config.clone(), &db_path, &api_key, &model,
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
                        slug, layer, count
                    );
                    engine.notify_mutation(layer);
                }
            }
        }

        // Start the WAL poll loop (belt-and-suspenders for timer re-arm)
        engine.start_poll_loop();

        engines.insert(slug.clone(), engine);

        // Create and start file watcher
        let source_paths: Vec<String> = {
            let conn = pyramid_state.reader.lock().await;
            // Get source paths from slug info
            match pyramid::slug::get_slug(&conn, &slug) {
                Ok(Some(info)) => {
                    serde_json::from_str(&info.source_path)
                        .unwrap_or_else(|_| vec![info.source_path.clone()])
                }
                _ => Vec::new(),
            }
        };

        if !source_paths.is_empty() {
            let mut watcher = PyramidFileWatcher::new(&slug, source_paths);
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

    // Phase 7: Initialize stale engines for auto-update pyramids
    init_stale_engines(&state.pyramid).await;

    // CORS headers for browser access
    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Range", "Authorization", "Access-Control-Request-Private-Network"]);

    // GET /health
    let health = {
        let state = state.clone();
        warp::path("health")
            .and(warp::get())
            .and_then(move || {
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
                                            let resp = warp::http::Response::builder()
                                                .status(206)
                                                .header("Content-Type", "application/octet-stream")
                                                .header("Content-Length", slice.len().to_string())
                                                .header("Content-Range", format!("bytes {}-{}/{}", start, end, file_size))
                                                .header("Accept-Ranges", "bytes")
                                                .header("Access-Control-Allow-Origin", "*")
                                                .header("X-Served-By", "wire-node")
                                                .body(slice)
                                                .unwrap();
                                            return Ok(Reply::into_response(resp));
                                        }
                                    }

                                    // Full response
                                    let resp = warp::http::Response::builder()
                                        .status(200)
                                        .header("Content-Type", "application/octet-stream")
                                        .header("Content-Length", file_size.to_string())
                                        .header("Accept-Ranges", "bytes")
                                        .header("Access-Control-Allow-Origin", "*")
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

    // POST /auth/complete — receives tokens from the callback page
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
            .and(warp::body::json())
            .and_then(move |origin: Option<String>, body: AuthCompleteRequest| {
                let state = state.clone();
                async move {
                    // Restrict to trusted origins only — prevents arbitrary web pages
                    // from overwriting auth state via cross-origin POST
                    let allowed = match origin.as_deref() {
                        None => true, // No origin header = same-origin or non-browser client
                        Some(o) if o.starts_with("https://newsbleach.com") => true,
                        Some(o) if o.starts_with("http://localhost") || o.starts_with("http://127.0.0.1") => true,
                        Some(o) if o == "tauri://localhost" => true,
                        Some(o) => {
                            tracing::warn!("Auth complete rejected from origin: {}", o);
                            false
                        }
                    };

                    if !allowed {
                        return Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"error": "forbidden"})));
                    }

                    tracing::info!("Auth callback received - user_id={:?}", body.user_id);

                    let mut auth = state.auth.write().await;
                    auth.access_token = Some(body.access_token);
                    auth.refresh_token = body.refresh_token;
                    auth.user_id = body.user_id;
                    auth.email = body.email;
                    // Preserve api_token and node_id from previous registration

                    tracing::info!("Auth state updated via magic link callback");
                    Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"status": "ok"})))
                }
            })
    };

    // GET /stats — node stats for dashboard
    let stats = {
        let state = state.clone();
        warp::path("stats")
            .and(warp::get())
            .and_then(move || {
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

    // Explicit OPTIONS preflight handler
    let preflight = warp::options()
        .map(|| {
            warp::http::Response::builder()
                .status(204)
                .header("Access-Control-Allow-Origin", "*")
                .header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
                .header("Access-Control-Allow-Headers", "Content-Type, Range, Authorization")
                .header("Access-Control-Allow-Private-Network", "true")
                .body("")
                .unwrap()
        });

    // Pyramid Knowledge Engine routes
    let pyramid_routes = pyramid::routes::pyramid_routes(state.pyramid.clone());

    // Partner (Dennis) routes
    let partner_routes = partner::routes::partner_routes(state.partner.clone());

    let routes = preflight
        .or(pyramid_routes)
        .or(partner_routes)
        .or(auth_callback.or(auth_complete).or(health).or(tunnel_debug).or(documents).or(stats))
        .with(cors);

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
async fn find_cached_document(cache_dir: &std::path::Path, document_id: &str) -> Option<std::path::PathBuf> {
    let target_filename = format!("{}.body", document_id);

    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false) {
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
            if entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false) {
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
    if parts.len() != 2 { return None; }

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

/// Verify a JWT using Ed25519 (EdDSA) public key
fn verify_jwt(token: &str, public_key_pem: &str) -> Result<DocumentClaims, String> {
    use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};

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
