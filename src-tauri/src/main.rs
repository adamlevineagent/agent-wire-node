// Wire Node — Main Entry Point
//
// Sets up:
// - Tauri app with system tray
// - Commands exposed to the React frontend
// - Background tasks (HTTP server, document sync, heartbeat, tunnel, market daemon, credit reporting)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_deep_link::DeepLinkExt;

use std::sync::Arc;
use tauri::Manager;
use tauri::tray::{TrayIconBuilder, MouseButton, MouseButtonState, TrayIconEvent};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tokio::sync::RwLock;
use tokio::io::AsyncBufReadExt;

use wire_node_lib::{
    AppState, WireNodeConfig, SharedState,
    auth, sync, server, credits, tunnel, messaging, market, retention,
};

// --- Tauri Commands ---------------------------------------------------------

#[tauri::command]
async fn send_magic_link(
    state: tauri::State<'_, SharedState>,
    email: String,
) -> Result<(), String> {
    let config = &state.config;
    auth::send_magic_link(
        &config.supabase_url,
        &config.supabase_anon_key,
        &email,
        config.server_port,
    ).await
}

#[tauri::command]
async fn verify_magic_link(
    state: tauri::State<'_, SharedState>,
    magic_link_url: String,
    email: String,
) -> Result<String, String> {
    let config = &state.config;
    let auth_state = auth::verify_magic_link_token(
        &config.supabase_url,
        &config.supabase_anon_key,
        &magic_link_url,
        &email,
    ).await?;

    let user_id = auth_state.user_id.clone().unwrap_or_default();

    // Register as Wire node using machine token (api_token)
    let registration = auth::register_wire_node(
        &config.api_url,
        &config.api_token,
        &config.node_name(),
        config.storage_cap_gb,
    ).await.ok();

    let node_id = registration.as_ref().map(|r| r.node_id.clone());

    let mut auth_write = state.auth.write().await;
    let first_started = auth_write.first_started_at.clone()
        .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
    *auth_write = auth::AuthState {
        node_id: node_id.clone(),
        first_started_at: first_started.clone(),
        ..auth_state
    };

    let mut cr = state.credits.write().await;
    cr.init_session();
    cr.first_started_at = first_started;

    save_session(&config, &auth_write);

    // Start Cloudflare Tunnel in background — use api_token for Wire API calls
    if let Some(ref nid) = node_id {
        let tunnel_state = state.tunnel_state.clone();
        let data_dir = config.data_dir();
        let api_url = config.tunnel_api_url.clone();
        let api_token = config.api_token.clone();
        let nid = nid.clone();

        tauri::async_runtime::spawn(async move {
            start_tunnel_flow(tunnel_state, data_dir, &api_url, &api_token, &nid).await;
        });
    }

    tracing::info!("Wire Node loaded, ready to serve");
    Ok(user_id)
}

#[tauri::command]
async fn login(
    state: tauri::State<'_, SharedState>,
    email: String,
    password: String,
) -> Result<String, String> {
    let config = &state.config;
    let auth_state = auth::login(
        &config.supabase_url,
        &config.supabase_anon_key,
        &email,
        &password,
    ).await?;

    let user_id = auth_state.user_id.clone().unwrap_or_default();

    // Register as Wire node using machine token (api_token)
    let registration = auth::register_wire_node(
        &config.api_url,
        &config.api_token,
        &config.node_name(),
        config.storage_cap_gb,
    ).await?;

    let mut auth_write = state.auth.write().await;
    *auth_write = auth::AuthState {
        node_id: Some(registration.node_id.clone()),
        ..auth_state
    };

    let mut cr = state.credits.write().await;
    cr.init_session();

    // Start Cloudflare Tunnel in background — use api_token for Wire API calls
    let tunnel_state = state.tunnel_state.clone();
    let data_dir = config.data_dir();
    let api_url = config.tunnel_api_url.clone();
    let api_token = config.api_token.clone();
    let node_id = registration.node_id.clone();

    tauri::async_runtime::spawn(async move {
        start_tunnel_flow(tunnel_state, data_dir, &api_url, &api_token, &node_id).await;
    });

    Ok(user_id)
}

#[tauri::command]
async fn get_auth_state(state: tauri::State<'_, SharedState>) -> Result<auth::AuthState, String> {
    let auth = state.auth.read().await;
    Ok(auth.clone())
}

#[tauri::command]
async fn logout(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let mut auth = state.auth.write().await;
    *auth = auth::AuthState::default();
    let session_path = session_file_path(&state.config);
    let _ = std::fs::remove_file(&session_path);
    tracing::info!("Logged out, session cleared");
    Ok(())
}

#[tauri::command]
async fn get_config(state: tauri::State<'_, SharedState>) -> Result<WireNodeConfig, String> {
    Ok(state.config.clone())
}

#[tauri::command]
async fn set_config(
    _state: tauri::State<'_, SharedState>,
    _config: WireNodeConfig,
) -> Result<(), String> {
    // Config is immutable at runtime — save to disk for next launch
    // The frontend can persist specific settings via save_onboarding
    Ok(())
}

#[tauri::command]
async fn link_folder(
    state: tauri::State<'_, SharedState>,
    folder_path: String,
    corpus_slug: String,
) -> Result<(), String> {
    let mut ss = state.sync_state.write().await;
    sync::link_folder(&mut ss, &folder_path, &corpus_slug)
}

#[tauri::command]
async fn unlink_folder(
    state: tauri::State<'_, SharedState>,
    folder_path: String,
) -> Result<(), String> {
    let mut ss = state.sync_state.write().await;
    sync::unlink_folder(&mut ss, &folder_path)
}

#[tauri::command]
async fn get_sync_status(state: tauri::State<'_, SharedState>) -> Result<sync::SyncState, String> {
    let ss = state.sync_state.read().await;
    Ok(ss.clone())
}

#[tauri::command]
async fn get_credits(
    state: tauri::State<'_, SharedState>,
) -> Result<credits::DashboardStats, String> {
    let cr = state.credits.read().await;
    Ok(cr.dashboard_stats())
}

#[tauri::command]
async fn get_market_surface(
    state: tauri::State<'_, SharedState>,
) -> Result<market::MarketState, String> {
    let ms = state.market_state.read().await;
    Ok(ms.clone())
}

#[tauri::command]
async fn retry_tunnel(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let api_token = &state.config.api_token;
    if api_token.is_empty() {
        return Err("No API token configured".to_string());
    }

    let node_id = {
        let auth = state.auth.read().await;
        auth.node_id.clone()
    };
    let nid = node_id.ok_or("No node_id - log in first")?;

    let data_dir = state.config.data_dir();
    let tunnel_json = data_dir.join("tunnel.json");
    let _ = std::fs::remove_file(&tunnel_json);

    let tunnel_state = state.tunnel_state.clone();
    let api_url = state.config.tunnel_api_url.clone();

    tracing::info!("Retrying tunnel provisioning...");
    start_tunnel_flow(tunnel_state, data_dir, &api_url, api_token, &nid).await;

    let ts = state.tunnel_state.read().await;
    match &ts.status {
        tunnel::TunnelConnectionStatus::Connected => Ok("Tunnel connected!".to_string()),
        tunnel::TunnelConnectionStatus::Connecting => Ok("Tunnel connecting...".to_string()),
        tunnel::TunnelConnectionStatus::Error(e) => Err(format!("Tunnel failed: {}", e)),
        _ => Ok(format!("Tunnel status: {:?}", ts.status)),
    }
}

#[tauri::command]
async fn get_tunnel_status(
    state: tauri::State<'_, SharedState>,
) -> Result<tunnel::TunnelState, String> {
    let ts = state.tunnel_state.read().await;
    Ok(ts.clone())
}

// --- Messaging Commands -----------------------------------------------------

#[tauri::command]
async fn get_messages(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<messaging::WireMessage>, String> {
    let api_token = &state.config.api_token;
    if api_token.is_empty() {
        return Err("No API token configured".to_string());
    }
    let auth = state.auth.read().await;
    let node_id = auth.node_id.as_deref().ok_or("No node registered")?;
    messaging::get_messages(&state.config.api_url, api_token, node_id).await
}

#[tauri::command]
async fn send_message(
    state: tauri::State<'_, SharedState>,
    body: String,
    message_type: String,
    subject: Option<String>,
) -> Result<(), String> {
    let api_token = &state.config.api_token;
    if api_token.is_empty() {
        return Err("No API token configured".to_string());
    }
    let auth = state.auth.read().await;
    let node_id = auth.node_id.as_deref().ok_or("No node registered")?;

    let metadata = if message_type == "bug_report" {
        let tunnel_url = {
            let ts = state.tunnel_state.read().await;
            ts.tunnel_url.clone()
        };
        let last_sync = {
            let ss = state.sync_state.read().await;
            ss.last_sync_at.clone()
        };
        let health = messaging::check_health(
            &state.config.cache_dir(),
            state.config.storage_cap_gb,
            tunnel_url.as_deref(),
            last_sync.as_deref(),
        ).await;
        Some(messaging::collect_diagnostics(
            &health,
            env!("CARGO_PKG_VERSION"),
            tunnel_url.as_deref(),
            node_id,
        ))
    } else {
        None
    };

    messaging::send_message(
        &state.config.api_url, api_token, node_id,
        &body, &message_type, subject.as_deref(), metadata,
    ).await
}

#[tauri::command]
async fn dismiss_message(
    state: tauri::State<'_, SharedState>,
    message_id: String,
) -> Result<(), String> {
    let api_token = &state.config.api_token;
    if api_token.is_empty() {
        return Err("No API token configured".to_string());
    }
    messaging::dismiss_message(&state.config.api_url, api_token, &message_id).await
}

#[tauri::command]
async fn get_health_status(
    state: tauri::State<'_, SharedState>,
) -> Result<messaging::HealthStatus, String> {
    let tunnel_url = {
        let ts = state.tunnel_state.read().await;
        ts.tunnel_url.clone()
    };
    let last_sync = {
        let ss = state.sync_state.read().await;
        ss.last_sync_at.clone()
    };
    Ok(messaging::check_health(
        &state.config.cache_dir(),
        state.config.storage_cap_gb,
        tunnel_url.as_deref(),
        last_sync.as_deref(),
    ).await)
}

// --- Update Commands --------------------------------------------------------

#[derive(serde::Serialize)]
struct UpdateInfo {
    available: bool,
    version: Option<String>,
    body: Option<String>,
}

#[tauri::command]
async fn check_for_update(
    app: tauri::AppHandle,
) -> Result<UpdateInfo, String> {
    let updater = app.updater()
        .map_err(|e| format!("Updater not available: {}", e))?;

    match updater.check().await {
        Ok(Some(update)) => Ok(UpdateInfo {
            available: true,
            version: Some(update.version.clone()),
            body: update.body.clone(),
        }),
        Ok(None) => Ok(UpdateInfo {
            available: false,
            version: None,
            body: None,
        }),
        Err(e) => {
            tracing::warn!("Update check failed: {}", e);
            Ok(UpdateInfo {
                available: false,
                version: None,
                body: None,
            })
        }
    }
}

#[tauri::command]
async fn install_update(
    app: tauri::AppHandle,
) -> Result<(), String> {
    let updater = app.updater()
        .map_err(|e| format!("Updater not available: {}", e))?;

    let update = updater.check().await
        .map_err(|e| format!("Update check failed: {}", e))?
        .ok_or_else(|| "No update available".to_string())?;

    tracing::info!("Downloading update v{}...", update.version);

    update.download_and_install(
        |chunk_len, _content_len| {
            tracing::debug!("Downloaded {} bytes", chunk_len);
        },
        || {
            tracing::info!("Update download complete, installing...");
        },
    ).await.map_err(|e| format!("Update install failed: {}", e))?;

    tracing::info!("Restarting app...");
    app.restart();
}

// --- Document Sync ----------------------------------------------------------

/// Run document sync for all linked folders
async fn do_sync(
    config: &WireNodeConfig,
    token: &str,
    sync_state: &Arc<RwLock<sync::SyncState>>,
    _credits: &Arc<RwLock<credits::CreditTracker>>,
) -> Result<(), String> {
    let linked_folders = {
        let ss = sync_state.read().await;
        ss.linked_folders.clone()
    };

    if linked_folders.is_empty() {
        tracing::debug!("No linked folders, skipping sync");
        return Ok(());
    }

    {
        let mut ss = sync_state.write().await;
        ss.is_syncing = true;
    }

    let mut all_cached: Vec<sync::CachedDocument> = Vec::new();

    for (folder_path, corpus_slug) in &linked_folders {
        tracing::info!("Syncing folder {} -> corpus {}", folder_path, corpus_slug);

        // Fetch remote document list
        let remote_docs = match sync::fetch_corpus_documents(&config.api_url, token, corpus_slug).await {
            Ok(docs) => docs,
            Err(e) => {
                tracing::warn!("Failed to fetch corpus {}: {}", corpus_slug, e);
                continue;
            }
        };

        // Scan local folder
        let local_docs = match sync::scan_local_folder(folder_path) {
            Ok(docs) => docs,
            Err(e) => {
                tracing::warn!("Failed to scan folder {}: {}", folder_path, e);
                continue;
            }
        };

        // Compute diff
        let diff = sync::compute_diff(&local_docs, &remote_docs);

        tracing::info!(
            "Sync diff for {}: {} to push, {} to pull, {} to update",
            corpus_slug, diff.to_push.len(), diff.to_pull.len(), diff.to_update.len()
        );

        // Push new documents
        for local_doc in &diff.to_push {
            match sync::push_document(&config.api_url, token, corpus_slug, local_doc).await {
                Ok(doc_id) => {
                    all_cached.push(sync::CachedDocument {
                        document_id: doc_id,
                        corpus_slug: corpus_slug.clone(),
                        source_path: local_doc.relative_path.clone(),
                        body_hash: local_doc.body_hash.clone(),
                        file_size_bytes: local_doc.size,
                        cached_at: chrono::Utc::now().to_rfc3339(),
                    });
                }
                Err(e) => tracing::warn!("Push failed for {}: {}", local_doc.relative_path, e),
            }
        }

        // Update changed documents
        for (local_doc, remote_doc) in &diff.to_update {
            match sync::update_document(&config.api_url, token, &remote_doc.id, local_doc).await {
                Ok(_) => {
                    all_cached.push(sync::CachedDocument {
                        document_id: remote_doc.id.clone(),
                        corpus_slug: corpus_slug.clone(),
                        source_path: local_doc.relative_path.clone(),
                        body_hash: local_doc.body_hash.clone(),
                        file_size_bytes: local_doc.size,
                        cached_at: chrono::Utc::now().to_rfc3339(),
                    });
                }
                Err(e) => tracing::warn!("Update failed for {}: {}", local_doc.relative_path, e),
            }
        }

        // Pull missing documents
        let sync_root = std::path::Path::new(folder_path);
        for remote_doc in &diff.to_pull {
            match sync::pull_document(&config.api_url, token, remote_doc, sync_root).await {
                Ok(cached) => {
                    all_cached.push(sync::CachedDocument {
                        corpus_slug: corpus_slug.clone(),
                        ..cached
                    });
                }
                Err(e) => tracing::warn!("Pull failed for {}: {}", remote_doc.id, e),
            }
        }
    }

    let total_size = sync::get_cache_size(&config.cache_dir()).await;
    {
        let mut ss = sync_state.write().await;
        ss.cached_documents = all_cached;
        ss.total_size_bytes = total_size;
        ss.last_sync_at = Some(chrono::Utc::now().to_rfc3339());
        ss.is_syncing = false;
    }

    tracing::info!("Sync complete");
    Ok(())
}

#[tauri::command]
async fn sync_content(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let api_token = &state.config.api_token;
    if api_token.is_empty() {
        return Err("No API token configured".to_string());
    }
    do_sync(&state.config, api_token, &state.sync_state, &state.credits).await
}

// --- Tunnel Flow ------------------------------------------------------------

async fn start_tunnel_flow(
    tunnel_state: Arc<RwLock<tunnel::TunnelState>>,
    data_dir: std::path::PathBuf,
    api_url: &str,
    access_token: &str,
    node_id: &str,
) {
    let persisted = tunnel::load_tunnel_state(&data_dir);

    // Step 1: Download cloudflared if needed
    {
        let mut ts = tunnel_state.write().await;
        ts.status = tunnel::TunnelConnectionStatus::Downloading;
    }

    match tunnel::download_cloudflared(&data_dir).await {
        Ok(_) => tracing::info!("cloudflared binary ready"),
        Err(e) => {
            tracing::error!("Failed to download cloudflared: {}", e);
            let mut ts = tunnel_state.write().await;
            ts.status = tunnel::TunnelConnectionStatus::Error(
                format!("Failed to download cloudflared: {}", e)
            );
            return;
        }
    }

    // Step 2: Provision tunnel (or use persisted token)
    let ts = if let Some(ref persisted_ts) = persisted {
        if persisted_ts.tunnel_token.is_some() {
            tracing::info!("Using persisted tunnel credentials");
            let mut ts = tunnel_state.write().await;
            ts.tunnel_id = persisted_ts.tunnel_id.clone();
            ts.tunnel_url = persisted_ts.tunnel_url.clone();
            ts.tunnel_token = persisted_ts.tunnel_token.clone();
            ts.status = tunnel::TunnelConnectionStatus::Connecting;
            ts.clone()
        } else {
            provision_new_tunnel(&tunnel_state, api_url, access_token, node_id).await
        }
    } else {
        provision_new_tunnel(&tunnel_state, api_url, access_token, node_id).await
    };

    let tunnel_token = match ts.tunnel_token {
        Some(ref t) => t.clone(),
        None => {
            tracing::error!("No tunnel token available");
            return;
        }
    };

    tunnel::save_tunnel_state(&data_dir, &ts);

    // Step 3: Start cloudflared
    {
        let mut tstate = tunnel_state.write().await;
        tstate.status = tunnel::TunnelConnectionStatus::Connecting;
    }

    match tunnel::start_tunnel(&data_dir, &tunnel_token).await {
        Ok(mut child) => {
            tracing::info!("cloudflared started, monitoring...");

            let status = tunnel::monitor_tunnel_output(&mut child).await;
            {
                let mut tstate = tunnel_state.write().await;
                tstate.status = status.clone();
            }

            match status {
                tunnel::TunnelConnectionStatus::Connected => {
                    tracing::info!("Tunnel connected!");
                }
                tunnel::TunnelConnectionStatus::Connecting => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                    match child.try_wait() {
                        Ok(Some(exit_status)) => {
                            let msg = format!("cloudflared exited immediately ({})", exit_status);
                            tracing::error!("{}", msg);
                            let mut tstate = tunnel_state.write().await;
                            tstate.status = tunnel::TunnelConnectionStatus::Error(msg);
                        }
                        _ => {
                            let mut tstate = tunnel_state.write().await;
                            tstate.status = tunnel::TunnelConnectionStatus::Connected;
                            tracing::info!("Tunnel assumed connected (process alive)");
                        }
                    }
                }
                tunnel::TunnelConnectionStatus::Error(ref e) => {
                    tracing::error!("Tunnel error: {}", e);
                }
                _ => {}
            }

            if let Some(stdout) = child.stdout.take() {
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stdout).lines();
                    while let Ok(Some(_)) = reader.next_line().await {}
                });
            }

            tokio::spawn(async move {
                let _ = child.wait().await;
                tracing::warn!("cloudflared process exited");
            });
        }
        Err(e) => {
            tracing::error!("Failed to start cloudflared: {}", e);
            let mut tstate = tunnel_state.write().await;
            tstate.status = tunnel::TunnelConnectionStatus::Error(e);
        }
    }
}

async fn provision_new_tunnel(
    tunnel_state: &Arc<RwLock<tunnel::TunnelState>>,
    api_url: &str,
    access_token: &str,
    node_id: &str,
) -> tunnel::TunnelState {
    {
        let mut ts = tunnel_state.write().await;
        ts.status = tunnel::TunnelConnectionStatus::Provisioning;
    }

    match tunnel::provision_tunnel(api_url, access_token, node_id).await {
        Ok(new_ts) => {
            let mut ts = tunnel_state.write().await;
            *ts = new_ts.clone();
            tracing::info!("Tunnel provisioned: {:?}", new_ts.tunnel_url);
            new_ts
        }
        Err(e) => {
            tracing::error!("Tunnel provisioning failed: {}", e);
            let mut ts = tunnel_state.write().await;
            ts.status = tunnel::TunnelConnectionStatus::Error(
                format!("Provisioning failed: {}", e)
            );
            ts.clone()
        }
    }
}

// --- Session Persistence ----------------------------------------------------

fn session_file_path(config: &WireNodeConfig) -> std::path::PathBuf {
    config.data_dir().join("session.json")
}

fn save_session(config: &WireNodeConfig, auth: &auth::AuthState) {
    let path = session_file_path(config);
    if let Ok(json) = serde_json::to_string_pretty(auth) {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = std::fs::write(&path, json);
        tracing::info!("Session saved to {:?}", path);
    }
}

fn load_session(config: &WireNodeConfig) -> Option<auth::AuthState> {
    let path = session_file_path(config);
    let data = std::fs::read_to_string(&path).ok()?;
    let auth: auth::AuthState = serde_json::from_str(&data).ok()?;
    if auth.is_authenticated() {
        tracing::info!("Loaded saved session for {:?}", auth.email);
        Some(auth)
    } else {
        None
    }
}

fn onboarding_file_path(config: &WireNodeConfig) -> std::path::PathBuf {
    config.data_dir().join("onboarding.json")
}

#[tauri::command]
async fn is_onboarded(state: tauri::State<'_, SharedState>) -> Result<bool, String> {
    let path = onboarding_file_path(&state.config);
    Ok(path.exists())
}

#[tauri::command]
async fn save_onboarding(
    state: tauri::State<'_, SharedState>,
    node_name: String,
    storage_cap_gb: f64,
    mesh_hosting_enabled: bool,
) -> Result<(), String> {
    let config = &state.config;

    let onboarding = serde_json::json!({
        "node_name": node_name,
        "storage_cap_gb": storage_cap_gb,
        "mesh_hosting_enabled": mesh_hosting_enabled,
        "completed_at": chrono::Utc::now().to_rfc3339(),
    });

    let path = onboarding_file_path(config);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    std::fs::write(&path, serde_json::to_string_pretty(&onboarding).unwrap())
        .map_err(|e| format!("Failed to save onboarding: {}", e))?;

    tracing::info!("Onboarding saved: name={}, storage={}GB, mesh={}",
        node_name, storage_cap_gb, mesh_hosting_enabled);

    Ok(())
}

// --- App Setup --------------------------------------------------------------

fn main() {
    tracing_subscriber::fmt::init();

    let config = WireNodeConfig::default();

    // Try loading a saved session
    let initial_auth = load_session(&config).unwrap_or_default();
    if initial_auth.email.is_some() {
        tracing::info!("Loaded saved session for {:?}", initial_auth.email);
    }

    // Initialize credit tracker — load persisted cumulative stats
    let stats_path = config.data_dir().join("stats.json");
    let mut initial_credits = credits::CreditTracker::load_from_file(&stats_path);
    if initial_credits.documents_served > 0 {
        tracing::info!("Loaded persisted stats: {} documents served", initial_credits.documents_served);
    }
    if let Some(ref fsa) = initial_auth.first_started_at {
        initial_credits.first_started_at = Some(fsa.clone());
    }
    initial_credits.init_session();

    // Load persisted tunnel state
    let data_dir = config.data_dir();
    let initial_tunnel = tunnel::load_tunnel_state(&data_dir)
        .unwrap_or_default();

    // Shared JWT public key and node ID for the server module
    let jwt_public_key = Arc::new(RwLock::new(config.jwt_public_key.clone()));
    let node_id_shared = Arc::new(RwLock::new(config.node_id.clone()));

    let state = Arc::new(AppState {
        auth: Arc::new(RwLock::new(initial_auth.clone())),
        sync_state: Arc::new(RwLock::new(sync::SyncState::default())),
        credits: Arc::new(RwLock::new(initial_credits)),
        tunnel_state: Arc::new(RwLock::new(initial_tunnel)),
        market_state: Arc::new(RwLock::new(market::MarketState::default())),
        config: config.clone(),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(state.clone())
        .setup(move |app| {
            let state = state.clone();

            // --- System Tray ---
            let show_item = MenuItemBuilder::with_id("show", "Show Wire Node").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Quit Wire Node").build(app)?;
            let tray_menu = MenuBuilder::new(app)
                .item(&show_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .menu(&tray_menu)
                .tooltip("Wire Node")
                .icon(app.default_window_icon().unwrap().clone())
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => std::process::exit(0),
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            // --- Deep Link Handler: agentwire://auth/callback#access_token=...&refresh_token=... ---
            let deep_link_state = state.clone();
            let dl_config = config.clone();
            app.deep_link().on_open_url(move |_event| {
                let urls = _event.urls();
                for url in urls {
                    let url_str = url.to_string();
                    tracing::info!("Deep link received: {}", &url_str);

                    if url_str.starts_with("agentwire://auth/callback") {
                        if let Some(hash_pos) = url_str.find('#') {
                            let fragment = &url_str[hash_pos + 1..];
                            let params: std::collections::HashMap<String, String> = fragment
                                .split('&')
                                .filter_map(|pair| {
                                    let mut parts = pair.splitn(2, '=');
                                    let key = parts.next()?.to_string();
                                    let val = parts.next()?.to_string();
                                    Some((key, val))
                                })
                                .collect();

                            if let (Some(access_token), Some(refresh_token)) =
                                (params.get("access_token"), params.get("refresh_token"))
                            {
                                let at = access_token.clone();
                                let rt = refresh_token.clone();
                                let s = deep_link_state.clone();
                                let c = dl_config.clone();
                                tauri::async_runtime::spawn(async move {
                                    auth::set_tokens_from_deep_link(
                                        &c.supabase_url,
                                        &c.supabase_anon_key,
                                        &at,
                                        &rt,
                                        &s,
                                    ).await;

                                    let mut auth_write = s.auth.write().await;

                                    // Register as Wire node using machine token (api_token)
                                    let registration = auth::register_wire_node(
                                        &c.api_url,
                                        &c.api_token,
                                        &c.node_name(),
                                        c.storage_cap_gb,
                                    ).await.ok();

                                    let node_id = registration.as_ref().map(|r| r.node_id.clone());
                                    auth_write.node_id = node_id.clone();
                                    let first_started = auth_write.first_started_at.clone()
                                        .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
                                    auth_write.first_started_at = first_started.clone();

                                    save_session(&c, &auth_write);

                                    if let Some(ref nid) = node_id {
                                        let ts = s.tunnel_state.clone();
                                        let data_dir = c.data_dir();
                                        let api_token = c.api_token.clone();
                                        let nid = nid.clone();
                                        let api_url = c.tunnel_api_url.clone();

                                        let mut cr = s.credits.write().await;
                                        cr.init_session();
                                        cr.first_started_at = first_started;
                                        drop(auth_write);
                                        drop(cr);

                                        tauri::async_runtime::spawn(async move {
                                            start_tunnel_flow(ts, data_dir, &api_url, &api_token, &nid).await;
                                        });
                                    }
                                });
                            }
                        }
                    }
                }
            });

            // --- Start HTTP server ---
            let server_state = state.clone();
            let jwt_pk = jwt_public_key.clone();
            let nid_shared = node_id_shared.clone();
            tauri::async_runtime::spawn(async move {
                server::start_server(
                    server_state.config.server_port,
                    server_state.config.cache_dir(),
                    server_state.credits.clone(),
                    server_state.auth.clone(),
                    server_state.sync_state.clone(),
                    server_state.tunnel_state.clone(),
                    jwt_pk,
                    nid_shared,
                ).await;
            });

            // --- Startup: refresh token, register node, start tunnel ---
            let startup_state = state.clone();
            let startup_config = config.clone();
            let startup_jwt_pk = jwt_public_key.clone();
            let startup_nid = node_id_shared.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

                // Refresh token
                let refresh_token_owned = {
                    let auth = startup_state.auth.read().await;
                    auth.refresh_token.clone()
                };
                if let Some(ref rt) = refresh_token_owned {
                    match auth::refresh_session(&startup_config.supabase_url, &startup_config.supabase_anon_key, rt).await {
                        Ok((new_access, new_refresh)) => {
                            let mut auth_write = startup_state.auth.write().await;
                            auth_write.access_token = Some(new_access);
                            auth_write.refresh_token = Some(new_refresh);

                            // Register Wire node using machine token (api_token)
                            if !startup_config.api_token.is_empty() {
                                match auth::register_wire_node(
                                    &startup_config.api_url,
                                    &startup_config.api_token,
                                    &startup_config.node_name(),
                                    startup_config.storage_cap_gb,
                                ).await {
                                    Ok(reg) => {
                                        tracing::info!("Wire node registered on startup: {}", reg.node_id);
                                        auth_write.node_id = Some(reg.node_id.clone());
                                        // Update shared JWT public key and node ID
                                        {
                                            let mut pk = startup_jwt_pk.write().await;
                                            match reg.jwt_public_key {
                                                Some(ref key) => *pk = key.clone(),
                                                None => tracing::warn!("Server returned null jwt_public_key — JWT verification will be unavailable"),
                                            }
                                        }
                                        {
                                            let mut nid = startup_nid.write().await;
                                            *nid = reg.node_id;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Node registration failed: {}", e);
                                    }
                                }
                            }

                            save_session(&startup_config, &auth_write);
                            drop(auth_write);
                            tracing::info!("Token refreshed on startup");
                        }
                        Err(e) => {
                            tracing::warn!("Token refresh failed: {}", e);
                        }
                    }
                }

                // Start tunnel — use api_token for Wire API calls
                let node_id = {
                    let auth = startup_state.auth.read().await;
                    auth.node_id.clone()
                };
                if let Some(nid) = node_id {
                    if !startup_config.api_token.is_empty() {
                        let tunnel_state = startup_state.tunnel_state.clone();
                        let data_dir = startup_config.data_dir();
                        let api_url = startup_config.tunnel_api_url.clone();
                        start_tunnel_flow(tunnel_state, data_dir, &api_url, &startup_config.api_token, &nid).await;
                    }
                }

                // Initial sync — use machine token (api_token) for Wire API calls
                if !startup_config.api_token.is_empty() {
                    match do_sync(&startup_config, &startup_config.api_token, &startup_state.sync_state, &startup_state.credits).await {
                        Ok(_) => tracing::info!("Initial sync complete"),
                        Err(e) => tracing::warn!("Initial sync failed: {}", e),
                    }
                }
            });

            // --- Periodic document sync loop (every 30 minutes) ---
            let sync_loop_state = state.clone();
            let sync_loop_config = config.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(30 * 60)).await;
                    if !sync_loop_config.api_token.is_empty() {
                        tracing::info!("Periodic sync starting...");
                        let _ = do_sync(&sync_loop_config, &sync_loop_config.api_token, &sync_loop_state.sync_state, &sync_loop_state.credits).await;
                    }
                }
            });

            // --- Heartbeat loop (every 60s) with market/retention handling ---
            let heartbeat_state = state.clone();
            let heartbeat_config = config.clone();
            let heartbeat_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

                    // Use machine token (api_token) for all Wire API calls
                    let api_token = &heartbeat_config.api_token;
                    if api_token.is_empty() {
                        continue;
                    }

                    let node_id = {
                        let auth = heartbeat_state.auth.read().await;
                        auth.node_id.clone()
                    };

                    if let Some(node_id) = &node_id {
                        let token = api_token;
                        let tunnel_url = {
                            let ts = heartbeat_state.tunnel_state.read().await;
                            match ts.status {
                                tunnel::TunnelConnectionStatus::Connected |
                                tunnel::TunnelConnectionStatus::Connecting => ts.tunnel_url.clone(),
                                _ => None,
                            }
                        };

                        let version = heartbeat_app.config().version.clone();

                        let result = auth::heartbeat(
                            &heartbeat_config.api_url,
                            token,
                            node_id,
                            tunnel_url.as_deref(),
                            version.as_deref(),
                        ).await;

                        match result {
                            Ok(response) => {
                                // Handle retention challenges from heartbeat
                                if let Some(challenges) = response.get("retention_challenges") {
                                    if let Ok(challenges) = serde_json::from_value::<Vec<retention::RetentionChallenge>>(challenges.clone()) {
                                        if !challenges.is_empty() {
                                            let _ = retention::handle_retention_challenges(
                                                &heartbeat_config.api_url,
                                                token,
                                                node_id,
                                                &challenges,
                                                &heartbeat_config.cache_dir(),
                                            ).await;
                                        }
                                    }
                                }

                                // Handle purge directives from heartbeat
                                if let Some(purges) = response.get("purge_directives") {
                                    if let Ok(directives) = serde_json::from_value::<Vec<retention::PurgeDirective>>(purges.clone()) {
                                        if !directives.is_empty() {
                                            let _ = retention::handle_purge_directives(
                                                &heartbeat_config.api_url,
                                                token,
                                                node_id,
                                                &directives,
                                                &heartbeat_config.cache_dir(),
                                            ).await;
                                        }
                                    }
                                }

                                // Handle market surface from heartbeat
                                if let Some(market_surface) = response.get("market_surface") {
                                    if let Ok(opportunities) = serde_json::from_value::<Vec<market::MarketOpportunity>>(market_surface.clone()) {
                                        if !opportunities.is_empty() {
                                            let mut ms = heartbeat_state.market_state.write().await;
                                            market::evaluate_opportunities(
                                                &heartbeat_config.api_url,
                                                token,
                                                node_id,
                                                &opportunities,
                                                &mut ms,
                                                &heartbeat_config.cache_dir(),
                                                heartbeat_config.storage_cap_gb,
                                                heartbeat_config.mesh_hosting_enabled,
                                            ).await;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                // Heartbeat uses machine token (api_token), not Supabase session.
                                // No token refresh needed — just log the error.
                                tracing::warn!("Heartbeat error: {}", e);
                            }
                        }
                    }
                }
            });

            // --- Credit reporting loop (every 60s) ---
            let credit_state = state.clone();
            let credit_config = config.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                loop {
                    let (pending_serves, node_id) = {
                        let auth = credit_state.auth.read().await;
                        let node_id = auth.node_id.clone();
                        drop(auth);
                        let mut cr = credit_state.credits.write().await;
                        let pending = cr.take_pending_serves();
                        (pending, node_id)
                    };

                    let api_token = &credit_config.api_token;
                    if let Some(ref nid) = node_id {
                        if !api_token.is_empty() && !pending_serves.is_empty() {
                            match credits::report_serves(&credit_config.api_url, api_token, nid, &pending_serves).await {
                                Ok(_) => tracing::debug!("Reported {} serves", pending_serves.len()),
                                Err(e) => tracing::warn!("Serve report error: {}", e),
                            }
                        }
                    }

                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                }
            });

            // --- Uptime tick + periodic stats save ---
            let stats_save_state = state.clone();
            let stats_save_config = config.clone();
            tauri::async_runtime::spawn(async move {
                let mut tick_count: u64 = 0;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    tick_count += 1;
                    let mut cr = stats_save_state.credits.write().await;
                    cr.tick_uptime();
                    if tick_count % 5 == 0 {
                        let path = stats_save_config.data_dir().join("stats.json");
                        cr.save_to_file(&path);
                    }
                }
            });

            // --- Background auto-update check (every 6 hours) ---
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                loop {
                    match update_handle.updater() {
                        Ok(updater) => {
                            match updater.check().await {
                                Ok(Some(update)) => {
                                    tracing::info!("Update available: v{}", update.version);
                                    // Auto-install
                                    match update.download_and_install(
                                        |_, _| {},
                                        || { tracing::info!("Update downloaded, installing..."); },
                                    ).await {
                                        Ok(_) => {
                                            tracing::info!("Update installed, restarting...");
                                            update_handle.restart();
                                        }
                                        Err(e) => tracing::warn!("Auto-update install failed: {}", e),
                                    }
                                }
                                Ok(None) => tracing::debug!("No update available"),
                                Err(e) => tracing::debug!("Update check failed: {}", e),
                            }
                        }
                        Err(e) => tracing::debug!("Updater not available: {}", e),
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(6 * 3600)).await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send_magic_link,
            verify_magic_link,
            login,
            get_auth_state,
            logout,
            get_config,
            set_config,
            link_folder,
            unlink_folder,
            get_sync_status,
            sync_content,
            get_credits,
            get_market_surface,
            retry_tunnel,
            get_tunnel_status,
            get_messages,
            send_message,
            dismiss_message,
            get_health_status,
            check_for_update,
            install_update,
            is_onboarded,
            save_onboarding,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wire Node");
}
