// Wire Node — Main Entry Point
//
// Sets up:
// - Tauri app with system tray
// - Commands exposed to the React frontend
// - Background tasks (HTTP server, document sync, heartbeat, tunnel, market daemon, credit reporting)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod vocabulary;

use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_updater::UpdaterExt;

use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};
use futures_util::FutureExt;
use tokio::io::AsyncBufReadExt;
use tokio::sync::RwLock;

use wire_node_lib::{
    auth, credits, market, messaging, retention, server, sync, tunnel, work, AppState, SharedState,
    WireNodeConfig,
};

// --- Auth Token Helper ------------------------------------------------------

async fn get_api_token(auth: &Arc<RwLock<auth::AuthState>>) -> Result<String, String> {
    let auth = auth.read().await;
    auth.api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "No API token — please log in".to_string())
}

// --- Tauri Commands ---------------------------------------------------------

#[tauri::command]
async fn send_magic_link(
    state: tauri::State<'_, SharedState>,
    email: String,
) -> Result<(), String> {
    let config = state.config.read().await;
    let config = &*config;
    auth::send_magic_link(
        &config.supabase_url,
        &config.supabase_anon_key,
        &email,
        config.server_port,
    )
    .await
}

#[tauri::command]
async fn verify_magic_link(
    state: tauri::State<'_, SharedState>,
    magic_link_url: String,
    email: String,
) -> Result<String, String> {
    let config = state.config.read().await;
    let config = &*config;
    let auth_state = auth::verify_magic_link_token(
        &config.supabase_url,
        &config.supabase_anon_key,
        &magic_link_url,
        &email,
    )
    .await?;

    let user_id = auth_state.user_id.clone().unwrap_or_default();

    // Register with Wire using Supabase session token — propagate errors
    let supabase_token = auth_state.access_token.clone().unwrap_or_default();
    let (nh, nt) = match &state.node_identity {
        Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
        None => (config.node_name(), String::new()),
    };
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &nh, &nt).await?;

    // If handle changed due to 409 retry, update node_identity.json
    if let Some(ref new_handle) = registration.node_handle {
        if let Some(ref ni) = state.node_identity {
            let mut updated = ni.clone();
            updated.node_handle = new_handle.clone();
            let _ = updated.save(&config.data_dir());
        }
    }

    let node_id = Some(registration.node_id.clone());
    let api_token = Some(registration.api_token.clone());

    let mut auth_write = state.auth.write().await;
    let first_started = auth_write
        .first_started_at
        .clone()
        .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
    *auth_write = auth::AuthState {
        node_id: node_id.clone(),
        api_token: api_token.clone(),
        first_started_at: first_started.clone(),
        operator_handle: registration.operator_handle.clone(),
        jwt_public_key: registration.jwt_public_key.clone(),
        ..auth_state
    };

    let mut cr = state.credits.write().await;
    cr.init_session();
    cr.first_started_at = first_started;

    save_session(&config, &auth_write);
    drop(auth_write);
    drop(cr);

    // Start Cloudflare Tunnel in background
    if let Some(ref nid) = node_id {
        if let Some(ref token) = api_token {
            let tunnel_state = state.tunnel_state.clone();
            let data_dir = config.data_dir();
            let api_url = config.tunnel_api_url.clone();
            let token = token.clone();
            let nid = nid.clone();

            tauri::async_runtime::spawn(async move {
                start_tunnel_flow(tunnel_state, data_dir, &api_url, &token, &nid).await;
            });
        }
    }

    // Auto-acquire operator session (best-effort, don't block login)
    {
        let state_ref: &AppState = &state;
        try_acquire_operator_session(state_ref).await;
    }

    tracing::info!("Wire Node loaded, ready to serve");
    Ok(user_id)
}

#[tauri::command]
async fn verify_otp(
    state: tauri::State<'_, SharedState>,
    email: String,
    otp_code: String,
) -> Result<String, String> {
    let config = state.config.read().await;
    let config = &*config;
    let auth_state = auth::verify_otp(
        &config.supabase_url,
        &config.supabase_anon_key,
        &email,
        &otp_code,
    )
    .await?;

    let user_id = auth_state.user_id.clone().unwrap_or_default();

    // Register with Wire using Supabase session token — propagate errors
    let supabase_token = auth_state.access_token.clone().unwrap_or_default();
    let (nh, nt) = match &state.node_identity {
        Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
        None => (config.node_name(), String::new()),
    };
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &nh, &nt).await?;

    // If handle changed due to 409 retry, update node_identity.json
    if let Some(ref new_handle) = registration.node_handle {
        if let Some(ref ni) = state.node_identity {
            let mut updated = ni.clone();
            updated.node_handle = new_handle.clone();
            let _ = updated.save(&config.data_dir());
        }
    }

    let node_id = Some(registration.node_id.clone());
    let api_token = Some(registration.api_token.clone());

    let mut auth_write = state.auth.write().await;
    let first_started = auth_write
        .first_started_at
        .clone()
        .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
    *auth_write = auth::AuthState {
        node_id: node_id.clone(),
        api_token: api_token.clone(),
        first_started_at: first_started.clone(),
        operator_handle: registration.operator_handle.clone(),
        jwt_public_key: registration.jwt_public_key.clone(),
        ..auth_state
    };

    let mut cr = state.credits.write().await;
    cr.init_session();
    cr.first_started_at = first_started;

    save_session(&config, &auth_write);
    drop(auth_write);
    drop(cr);

    // Start Cloudflare Tunnel in background
    if let Some(ref nid) = node_id {
        if let Some(ref token) = api_token {
            let tunnel_state = state.tunnel_state.clone();
            let data_dir = config.data_dir();
            let api_url = config.tunnel_api_url.clone();
            let token = token.clone();
            let nid = nid.clone();

            tauri::async_runtime::spawn(async move {
                start_tunnel_flow(tunnel_state, data_dir, &api_url, &token, &nid).await;
            });
        }
    }

    // Auto-acquire operator session (best-effort, don't block login)
    {
        let state_ref: &AppState = &state;
        try_acquire_operator_session(state_ref).await;
    }

    tracing::info!("OTP verified, Wire Node loaded");
    Ok(user_id)
}

#[tauri::command]
async fn login(
    state: tauri::State<'_, SharedState>,
    email: String,
    password: String,
) -> Result<String, String> {
    let config = state.config.read().await;
    let config = &*config;
    let auth_state = auth::login(
        &config.supabase_url,
        &config.supabase_anon_key,
        &email,
        &password,
    )
    .await?;

    let user_id = auth_state.user_id.clone().unwrap_or_default();

    // Register with Wire using Supabase session token
    let supabase_token = auth_state.access_token.clone().unwrap_or_default();
    let (nh, nt) = match &state.node_identity {
        Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
        None => (config.node_name(), String::new()),
    };
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &nh, &nt).await?;

    // If handle changed due to 409 retry, update node_identity.json
    if let Some(ref new_handle) = registration.node_handle {
        if let Some(ref ni) = state.node_identity {
            let mut updated = ni.clone();
            updated.node_handle = new_handle.clone();
            let _ = updated.save(&config.data_dir());
        }
    }

    let node_id = Some(registration.node_id.clone());
    let api_token = Some(registration.api_token.clone());

    let mut auth_write = state.auth.write().await;
    let first_started = auth_write
        .first_started_at
        .clone()
        .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
    *auth_write = auth::AuthState {
        node_id: node_id.clone(),
        api_token: api_token.clone(),
        first_started_at: first_started.clone(),
        operator_handle: registration.operator_handle.clone(),
        jwt_public_key: registration.jwt_public_key.clone(),
        ..auth_state
    };

    let mut cr = state.credits.write().await;
    cr.init_session();
    cr.first_started_at = first_started;

    save_session(&config, &auth_write);
    drop(auth_write);
    drop(cr);

    // Start Cloudflare Tunnel in background
    if let Some(ref nid) = node_id {
        if let Some(ref token) = api_token {
            let tunnel_state = state.tunnel_state.clone();
            let data_dir = config.data_dir();
            let api_url = config.tunnel_api_url.clone();
            let token = token.clone();
            let nid = nid.clone();

            tauri::async_runtime::spawn(async move {
                start_tunnel_flow(tunnel_state, data_dir, &api_url, &token, &nid).await;
            });
        }
    }

    // Auto-acquire operator session (best-effort, don't block login)
    {
        let state_ref: &AppState = &state;
        try_acquire_operator_session(state_ref).await;
    }

    Ok(user_id)
}

// --- Operator Session Commands -----------------------------------------------

/// Acquire an operator session from the Wire API using the current Supabase access token
#[tauri::command]
async fn get_operator_session(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let auth = state.auth.read().await;
    let access_token = auth.access_token.clone().ok_or("Not authenticated")?;
    let config = state.config.read().await;
    let api_url = config.api_url.clone();
    drop(config);
    drop(auth);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/operator/auth/session", api_url))
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("Session endpoint returned {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    // Store in auth state
    let mut auth = state.auth.write().await;
    auth.operator_session_token = body["session_token"].as_str().map(String::from);
    auth.operator_id = body["operator_id"].as_str().map(String::from);
    auth.operator_session_expires_at = body["expires_at"].as_str().map(String::from);

    // Save to session file
    let config = state.config.read().await;
    save_session(&config, &auth);

    Ok(body)
}

/// Helper: build and send an HTTP request, returning (status, body_value).
/// Checks status BEFORE parsing JSON — non-JSON error responses (nginx 502, raw 401)
/// produce a text fallback instead of misleading JSON parse errors.
/// Thin delegation to the shared `http_utils::send_api_request`. The
/// impl moved there so the warp HTTP handlers (which can't reach into
/// main.rs symbols) can call it too. Signature preserved for all
/// existing IPC callers.
async fn send_api_request(
    api_url: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
    extra_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<(reqwest::StatusCode, serde_json::Value), String> {
    wire_node_lib::http_utils::send_api_request(api_url, method, path, token, body, extra_headers)
        .await
}

/// Make an authenticated API call using the operator session token.
/// Proactively refreshes session if close to expiry. Retries once on 401.
#[tauri::command]
async fn operator_api_call(
    state: tauri::State<'_, SharedState>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    // Proactive expiry check — refresh before the request if within 5 minutes of expiry
    {
        let auth = state.auth.read().await;
        if let Some(ref expires_at) = auth.operator_session_expires_at {
            if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expires_at) {
                let now = chrono::Utc::now();
                if expiry.signed_duration_since(now) < chrono::Duration::minutes(5) {
                    drop(auth);
                    tracing::info!("Operator session near expiry, proactively refreshing");
                    try_acquire_operator_session(&state).await;
                }
            }
        }
    }

    let (token, api_url) = {
        let auth = state.auth.read().await;
        let token = auth
            .operator_session_token
            .clone()
            .ok_or("No operator session")?;
        let config = state.config.read().await;
        let api_url = config.api_url.clone();
        (token, api_url)
    };

    match send_api_request(&api_url, &method, &path, &token, body.as_ref(), None).await {
        Ok((_status, result)) => Ok(result),
        Err(e) if e.contains("401") => {
            // 401 — try refreshing operator session and retry once
            tracing::info!("operator_api_call got 401, refreshing session and retrying");
            try_acquire_operator_session(&state).await;
            let auth = state.auth.read().await;
            let new_token = auth
                .operator_session_token
                .clone()
                .ok_or("Session expired — please re-authenticate")?;
            drop(auth);
            let (_status, result) =
                send_api_request(&api_url, &method, &path, &new_token, body.as_ref(), None).await?;
            Ok(result)
        }
        Err(e) => Err(e),
    }
}

/// Make an authenticated API call using the Wire agent API token (gne_live_*).
/// Supports custom headers (required for mesh endpoints' X-Wire-Thread).
/// Handles fresh-install (no token yet) by attempting registration.
/// Retries on 401 by re-registering with a refreshed Supabase session.
#[tauri::command]
async fn wire_api_call(
    state: tauri::State<'_, SharedState>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
    headers: Option<std::collections::HashMap<String, String>>,
) -> Result<serde_json::Value, String> {
    let (api_url, mut token) = {
        let config = state.config.read().await;
        let api_url = config.api_url.clone();
        drop(config);
        let token = get_api_token(&state.auth).await;
        (api_url, token)
    };

    // Fresh-install handling: if no api_token, try to register first
    if token.is_err() {
        tracing::info!("wire_api_call: no api_token, attempting registration");
        let registered = attempt_wire_registration(&state).await;
        if registered {
            token = get_api_token(&state.auth).await;
        }
        if token.is_err() {
            return Err("Wire agent not registered — please log in first".to_string());
        }
    }

    let token_str = token.unwrap();
    match send_api_request(
        &api_url,
        &method,
        &path,
        &token_str,
        body.as_ref(),
        headers.as_ref(),
    )
    .await
    {
        Ok((_status, result)) => Ok(result),
        Err(e) if e.contains("401") => {
            // 401 on wire token — attempt re-registration with refreshed Supabase session
            tracing::info!("wire_api_call got 401, attempting re-registration");
            let registered = attempt_wire_registration(&state).await;
            if !registered {
                return Err("Wire authentication failed — please re-authenticate".to_string());
            }
            let new_token = get_api_token(&state.auth)
                .await
                .map_err(|_| "Wire re-registration succeeded but no token available".to_string())?;
            let (_status, result) = send_api_request(
                &api_url,
                &method,
                &path,
                &new_token,
                body.as_ref(),
                headers.as_ref(),
            )
            .await?;
            Ok(result)
        }
        Err(e) => Err(e),
    }
}

/// Attempt to (re-)register the Wire agent, refreshing the Supabase session if needed.
/// Returns true if a valid api_token was obtained.
async fn attempt_wire_registration(state: &AppState) -> bool {
    let (access_token, api_url, node_name, supabase_url, supabase_key, refresh_token) = {
        let auth = state.auth.read().await;
        let config = state.config.read().await;
        (
            auth.access_token.clone(),
            config.api_url.clone(),
            config.node_name(),
            config.supabase_url.clone(),
            config.supabase_anon_key.clone(),
            auth.refresh_token.clone(),
        )
    };

    let (nh, nt) = match &state.node_identity {
        Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
        None => (node_name.clone(), String::new()),
    };

    // Try registration with current access token
    if let Some(ref at) = access_token {
        match auth::register_with_session(&api_url, at, &nh, &nt).await {
            Ok(reg) => {
                let mut auth = state.auth.write().await;
                auth.api_token = Some(reg.api_token.clone());
                auth.node_id = Some(reg.node_id.clone());
                // Propagate operator_id so fleet routing has same-operator identity.
                if auth.operator_id.is_none() {
                    auth.operator_id = Some(reg.operator_id.clone());
                }
                // Propagate operator_handle from registration response.
                if reg.operator_handle.is_some() {
                    auth.operator_handle = reg.operator_handle.clone();
                }
                // If handle changed due to 409 retry, update node_identity.json
                if let Some(ref new_handle) = reg.node_handle {
                    if let Some(ref ni) = state.node_identity {
                        let mut updated = ni.clone();
                        updated.node_handle = new_handle.clone();
                        let config = state.config.read().await;
                        let _ = updated.save(&config.data_dir());
                    }
                }
                let config = state.config.read().await;
                save_session(&config, &auth);
                tracing::info!("Wire registration succeeded (existing session)");
                return true;
            }
            Err(e) => tracing::warn!("Wire registration failed with current session: {}", e),
        }
    }

    // Current session failed — try refreshing Supabase tokens first
    if let Some(ref rt) = refresh_token {
        match auth::refresh_session(&supabase_url, &supabase_key, rt).await {
            Ok((new_access, new_refresh)) => {
                // CRITICAL: write refreshed tokens to AuthState BEFORE re-registering
                // Also persist to disk immediately so tokens survive a crash/restart
                {
                    let mut auth = state.auth.write().await;
                    auth.access_token = Some(new_access.clone());
                    auth.refresh_token = Some(new_refresh);
                    let config = state.config.read().await;
                    save_session(&config, &auth);
                }

                // Now register with the fresh access token
                match auth::register_with_session(&api_url, &new_access, &nh, &nt).await {
                    Ok(reg) => {
                        let mut auth = state.auth.write().await;
                        auth.api_token = Some(reg.api_token.clone());
                        auth.node_id = Some(reg.node_id.clone());
                        // Propagate operator_id so fleet routing has same-operator identity.
                        if auth.operator_id.is_none() {
                            auth.operator_id = Some(reg.operator_id.clone());
                        }
                        // Propagate operator_handle from registration response.
                        if reg.operator_handle.is_some() {
                            auth.operator_handle = reg.operator_handle.clone();
                        }
                        // If handle changed due to 409 retry, update node_identity.json
                        if let Some(ref new_handle) = reg.node_handle {
                            if let Some(ref ni) = state.node_identity {
                                let mut updated = ni.clone();
                                updated.node_handle = new_handle.clone();
                                let config_r = state.config.read().await;
                                let _ = updated.save(&config_r.data_dir());
                            }
                        }
                        let config = state.config.read().await;
                        save_session(&config, &auth);
                        tracing::info!("Wire registration succeeded after session refresh");
                        return true;
                    }
                    Err(e) => tracing::warn!("Wire registration failed after refresh: {}", e),
                }
            }
            Err(e) => tracing::warn!("Supabase session refresh failed: {}", e),
        }
    }

    false
}

/// Try to acquire an operator session (best-effort, non-blocking).
/// Called after successful login/verify flows.
async fn try_acquire_operator_session(state: &AppState) {
    let auth = state.auth.read().await;
    let access_token = match auth.access_token.clone() {
        Some(t) => t,
        None => return,
    };
    let config = state.config.read().await;
    let api_url = config.api_url.clone();
    drop(config);
    drop(auth);

    let client = reqwest::Client::new();
    match client
        .post(format!("{}/api/v1/operator/auth/session", api_url))
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                let mut auth = state.auth.write().await;
                auth.operator_session_token = body["session_token"].as_str().map(String::from);
                auth.operator_id = body["operator_id"].as_str().map(String::from);
                auth.operator_session_expires_at = body["expires_at"].as_str().map(String::from);
                let config = state.config.read().await;
                save_session(&config, &auth);
                tracing::info!("Operator session acquired for {:?}", auth.operator_id);
            }
            Err(e) => tracing::warn!("Failed to parse operator session response: {}", e),
        },
        Ok(resp) => tracing::warn!("Operator session endpoint returned {}", resp.status()),
        Err(e) => tracing::warn!("Failed to acquire operator session: {}", e),
    }
}

#[tauri::command]
async fn get_auth_state(state: tauri::State<'_, SharedState>) -> Result<auth::AuthState, String> {
    let auth = state.auth.read().await;
    Ok(auth.clone())
}

#[tauri::command]
async fn get_wire_identity_status(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let auth = state.auth.read().await;
    // Check if we have a valid API token (gne_live_* machine token from registration)
    if let Some(ref token) = auth.api_token {
        if !token.is_empty() {
            return Ok("connected".to_string());
        }
    }
    // We have a Supabase session but no Wire API token — identity is missing/not registered
    if auth.access_token.is_some() {
        return Ok("expired".to_string());
    }
    Ok("missing".to_string())
}

#[tauri::command]
async fn logout(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let mut auth = state.auth.write().await;
    *auth = auth::AuthState::default();
    let cfg = state.config.read().await;
    let session_path = session_file_path(&cfg);
    let _ = std::fs::remove_file(&session_path);
    tracing::info!("Logged out, session cleared");
    Ok(())
}

#[tauri::command]
async fn get_config(state: tauri::State<'_, SharedState>) -> Result<WireNodeConfig, String> {
    let mut cfg = state.config.read().await.clone();
    // Overlay runtime values that aren't in the static config
    let auth = state.auth.read().await;
    if let Some(ref nid) = auth.node_id {
        cfg.node_id = nid.clone();
    }
    Ok(cfg)
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
    direction: sync::SyncDirection,
) -> Result<(), String> {
    let cfg = state.config.read().await;
    let mut ss = state.sync_state.write().await;
    sync::link_folder(&mut ss, &folder_path, &corpus_slug, direction)?;
    sync::save_sync_state(&cfg.data_dir(), &ss);
    Ok(())
}

#[tauri::command]
async fn unlink_folder(
    state: tauri::State<'_, SharedState>,
    folder_path: String,
) -> Result<(), String> {
    let cfg = state.config.read().await;
    let mut ss = state.sync_state.write().await;
    sync::unlink_folder(&mut ss, &folder_path)?;
    sync::save_sync_state(&cfg.data_dir(), &ss);
    Ok(())
}

#[tauri::command]
async fn get_sync_status(state: tauri::State<'_, SharedState>) -> Result<sync::SyncState, String> {
    let ss = state.sync_state.read().await;
    Ok(ss.clone())
}

#[derive(serde::Deserialize)]
struct CorporaListResponse {
    items: Vec<CorpusInfo>,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
struct CorpusInfo {
    slug: String,
    title: String,
    visibility: Option<String>,
    document_count: Option<i64>,
}

#[tauri::command]
async fn list_my_corpora(state: tauri::State<'_, SharedState>) -> Result<Vec<CorpusInfo>, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;
    let url = format!(
        "{}/api/v1/wire/corpora?steward=me&limit=100",
        config.api_url
    );

    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch corpora: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Failed to list corpora ({status}): {body}"));
    }

    let parsed: CorporaListResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse corpora response: {}", e))?;
    Ok(parsed.items)
}

#[tauri::command]
async fn list_public_corpora(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<CorpusInfo>, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;
    let url = format!(
        "{}/api/v1/wire/corpora?visibility=public&limit=50",
        config.api_url
    );

    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch public corpora: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Failed to list public corpora ({status}): {body}"));
    }

    let parsed: CorporaListResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse public corpora response: {}", e))?;
    Ok(parsed.items)
}

#[tauri::command]
async fn create_corpus(
    state: tauri::State<'_, SharedState>,
    slug: String,
    title: String,
) -> Result<CorpusInfo, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;
    let url = format!("{}/api/v1/wire/corpora", config.api_url);

    let body = serde_json::json!({
        "slug": slug,
        "title": title,
        "visibility": "private",
        "material_class": "precursor",
    });

    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to create corpus: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Failed to create corpus ({status}): {body}"));
    }

    let corpus: CorpusInfo = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse create corpus response: {}", e))?;
    Ok(corpus)
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
async fn get_work_stats(state: tauri::State<'_, SharedState>) -> Result<work::WorkStats, String> {
    let ws = state.work_stats.read().await;
    Ok(ws.clone())
}

#[tauri::command]
async fn retry_tunnel(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let api_token = get_api_token(&state.auth).await?;

    let node_id = {
        let auth = state.auth.read().await;
        auth.node_id.clone()
    };
    let nid = node_id.ok_or("No node_id - log in first")?;

    let cfg = state.config.read().await;
    let data_dir = cfg.data_dir();
    let tunnel_json = data_dir.join("tunnel.json");
    let _ = std::fs::remove_file(&tunnel_json);

    let tunnel_state = state.tunnel_state.clone();
    let api_url = cfg.tunnel_api_url.clone();

    tracing::info!("Retrying tunnel provisioning...");
    start_tunnel_flow(tunnel_state, data_dir, &api_url, &api_token, &nid).await;

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
    let api_token = get_api_token(&state.auth).await?;
    let auth = state.auth.read().await;
    let node_id = auth.node_id.as_deref().ok_or("No node registered")?;
    let cfg = state.config.read().await;
    messaging::get_messages(&cfg.api_url, &api_token, node_id).await
}

#[tauri::command]
async fn send_message(
    state: tauri::State<'_, SharedState>,
    body: String,
    message_type: String,
    subject: Option<String>,
) -> Result<(), String> {
    let api_token = get_api_token(&state.auth).await?;
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
        let cfg = state.config.read().await;
        let health = messaging::check_health(
            &cfg.cache_dir(),
            cfg.storage_cap_gb,
            // TunnelUrl: !Deref<Target=str>, so as_deref() doesn't work.
            // .as_ref().map(|u| u.as_str()) yields Option<&str> (same shape as before).
            tunnel_url.as_ref().map(|u| u.as_str()),
            last_sync.as_deref(),
        )
        .await;
        Some(messaging::collect_diagnostics(
            &health,
            env!("CARGO_PKG_VERSION"),
            tunnel_url.as_ref().map(|u| u.as_str()),
            node_id,
        ))
    } else {
        None
    };

    let cfg = state.config.read().await;
    messaging::send_message(
        &cfg.api_url,
        &api_token,
        node_id,
        &body,
        &message_type,
        subject.as_deref(),
        metadata,
    )
    .await
}

#[tauri::command]
async fn dismiss_message(
    state: tauri::State<'_, SharedState>,
    message_id: String,
) -> Result<(), String> {
    let api_token = get_api_token(&state.auth).await?;
    let cfg = state.config.read().await;
    messaging::dismiss_message(&cfg.api_url, &api_token, &message_id).await
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
    let cfg = state.config.read().await;
    Ok(messaging::check_health(
        &cfg.cache_dir(),
        cfg.storage_cap_gb,
        // TunnelUrl: !Deref. Borrow → &str for check_health's Option<&str> arg.
        tunnel_url.as_ref().map(|u| u.as_str()),
        last_sync.as_deref(),
    )
    .await)
}

// --- Update Commands --------------------------------------------------------

#[derive(serde::Serialize)]
struct UpdateInfo {
    available: bool,
    version: Option<String>,
    body: Option<String>,
}

#[tauri::command]
async fn check_for_update(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    let updater = app
        .updater()
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
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|e| format!("Updater not available: {}", e))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("Update check failed: {}", e))?
        .ok_or_else(|| "No update available".to_string())?;

    tracing::info!("Downloading update v{}...", update.version);

    update
        .download_and_install(
            |chunk_len, _content_len| {
                tracing::debug!("Downloaded {} bytes", chunk_len);
            },
            || {
                tracing::info!("Update download complete, installing...");
            },
        )
        .await
        .map_err(|e| format!("Update install failed: {}", e))?;

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
        ss.sync_progress = Some("Checking...".to_string());
    }

    // Note: we track state entirely in sync_state.cached_documents (updated per-file during sync)
    // No separate all_cached vec needed.

    for (folder_path, linked) in &linked_folders {
        let corpus_slug = &linked.corpus_slug;
        let direction = &linked.direction;
        tracing::info!(
            "Syncing folder {} -> corpus {} ({:?})",
            folder_path,
            corpus_slug,
            direction
        );

        // Fetch remote document list
        let remote_docs =
            match sync::fetch_corpus_documents(&config.api_url, token, corpus_slug).await {
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

        // Deduplicate remote docs by effective_path — keep the latest version
        // (prevents duplicate UI entries when multiple remote docs share a path)
        let remote_docs = {
            let mut seen: std::collections::HashMap<String, sync::DocumentInfo> =
                std::collections::HashMap::new();
            for doc in remote_docs {
                let path = doc.effective_path();
                seen.entry(path).or_insert(doc);
            }
            seen.into_values().collect::<Vec<_>>()
        };

        // Compute diff
        let diff = sync::compute_diff(&local_docs, &remote_docs);

        tracing::info!(
            "Sync diff for {}: {} to push, {} to pull, {} to update",
            corpus_slug,
            diff.to_push.len(),
            diff.to_pull.len(),
            diff.to_update.len()
        );

        // Build initial file list with statuses and push to sync_state immediately
        // so the UI can show what's pending
        {
            let mut initial_docs: Vec<sync::CachedDocument> = Vec::new();

            // In-sync files
            for local_doc in &local_docs {
                let effective_paths: Vec<String> =
                    remote_docs.iter().map(|r| r.effective_path()).collect();
                if let Some((idx, _)) = effective_paths
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.as_str() == local_doc.relative_path.as_str())
                {
                    let remote_doc = &remote_docs[idx];
                    if local_doc.body_hash == remote_doc.body_hash {
                        initial_docs.push(sync::CachedDocument {
                            document_id: remote_doc.id.clone(),
                            corpus_slug: corpus_slug.clone(),
                            source_path: local_doc.relative_path.clone(),
                            body_hash: local_doc.body_hash.clone(),
                            file_size_bytes: local_doc.size,
                            cached_at: chrono::Utc::now().to_rfc3339(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            document_status: remote_doc.status.clone(),
                        });
                    }
                }
            }

            // Hash-matched files (same content, different path) — always InSync
            for (local_doc, remote_doc) in &diff.hash_matched {
                initial_docs.push(sync::CachedDocument {
                    document_id: remote_doc.id.clone(),
                    corpus_slug: corpus_slug.clone(),
                    source_path: local_doc.relative_path.clone(),
                    body_hash: local_doc.body_hash.clone(),
                    file_size_bytes: local_doc.size,
                    cached_at: chrono::Utc::now().to_rfc3339(),
                    sync_status: sync::FileStatus::InSync,
                    error_message: None,
                    document_status: remote_doc.status.clone(),
                });
            }

            // Files to pull — only relevant for Download/Both directions
            if *direction != sync::SyncDirection::Upload {
                for remote_doc in &diff.to_pull {
                    initial_docs.push(sync::CachedDocument {
                        document_id: remote_doc.id.clone(),
                        corpus_slug: corpus_slug.clone(),
                        source_path: remote_doc.effective_path(),
                        body_hash: remote_doc.body_hash.clone(),
                        file_size_bytes: 0,
                        cached_at: String::new(),
                        sync_status: sync::FileStatus::NeedsPull,
                        error_message: None,
                        document_status: remote_doc.status.clone(),
                    });
                }
            }

            // Files to push — only relevant for Upload/Both directions
            if *direction != sync::SyncDirection::Download {
                for local_doc in &diff.to_push {
                    initial_docs.push(sync::CachedDocument {
                        document_id: String::new(),
                        corpus_slug: corpus_slug.clone(),
                        source_path: local_doc.relative_path.clone(),
                        body_hash: local_doc.body_hash.clone(),
                        file_size_bytes: local_doc.size,
                        cached_at: String::new(),
                        sync_status: sync::FileStatus::NeedsPush,
                        error_message: None,
                        document_status: None, // not yet on server
                    });
                }
            }

            // Files to update (exist on both sides with different hashes)
            for (local_doc, remote_doc) in &diff.to_update {
                let status = match direction {
                    sync::SyncDirection::Upload => sync::FileStatus::NeedsPush,
                    sync::SyncDirection::Download => sync::FileStatus::NeedsPull,
                    sync::SyncDirection::Both => sync::FileStatus::NeedsPush, // will be resolved during sync
                };
                initial_docs.push(sync::CachedDocument {
                    document_id: remote_doc.id.clone(),
                    corpus_slug: corpus_slug.clone(),
                    source_path: local_doc.relative_path.clone(),
                    body_hash: local_doc.body_hash.clone(),
                    file_size_bytes: local_doc.size,
                    cached_at: String::new(),
                    sync_status: status,
                    error_message: None,
                    document_status: remote_doc.status.clone(),
                });
            }

            // Deduplicate initial_docs by (source_path, corpus_slug)
            {
                let mut seen: std::collections::HashMap<(String, String), usize> =
                    std::collections::HashMap::new();
                for (i, doc) in initial_docs.iter().enumerate() {
                    seen.insert((doc.source_path.clone(), doc.corpus_slug.clone()), i);
                }
                let mut keep_indices: Vec<usize> = seen.into_values().collect();
                keep_indices.sort();
                initial_docs = keep_indices
                    .into_iter()
                    .map(|i| initial_docs[i].clone())
                    .collect();
            }

            let total_actions = match direction {
                sync::SyncDirection::Upload => diff.to_push.len() + diff.to_update.len(),
                sync::SyncDirection::Download => diff.to_pull.len() + diff.to_update.len(),
                sync::SyncDirection::Both => {
                    diff.to_pull.len() + diff.to_push.len() + diff.to_update.len()
                }
            };
            let mut ss = sync_state.write().await;
            // Remove ONLY this corpus's entries, then add the new ones
            // This preserves entries from other corpora/folders
            ss.cached_documents
                .retain(|c| c.corpus_slug != *corpus_slug);
            ss.cached_documents.extend(initial_docs);
            ss.sync_progress = if total_actions > 0 {
                Some(format!("0/{} synced", total_actions))
            } else {
                Some("All in sync".to_string())
            };
        }

        // Now perform actual sync operations, updating state after each file

        let total_actions = match direction {
            sync::SyncDirection::Upload => diff.to_push.len() + diff.to_update.len(),
            sync::SyncDirection::Download => diff.to_pull.len() + diff.to_update.len(),
            sync::SyncDirection::Both => {
                diff.to_pull.len() + diff.to_push.len() + diff.to_update.len()
            }
        };
        let mut completed = 0usize;
        // Throttle delay between API calls to avoid rate limiting on large corpora
        let throttle = std::time::Duration::from_millis(200);
        // Track document IDs created/modified during this sync cycle so the
        // "remotely deleted" check doesn't erroneously remove them
        let mut synced_doc_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        // Pre-populate with hash-matched doc IDs (content exists, path mismatched)
        for (_local_doc, remote_doc) in &diff.hash_matched {
            synced_doc_ids.insert(remote_doc.id.clone());
        }
        // Build a remote body_hash → document lookup so we can resolve 409s
        let remote_by_hash: std::collections::HashMap<&str, &sync::DocumentInfo> = remote_docs
            .iter()
            .map(|d| (d.body_hash.as_str(), d))
            .collect();

        // Upload direction: push new and updated local files, skip pull
        if *direction == sync::SyncDirection::Upload {
            for local_doc in &diff.to_push {
                // Mark as Pushing
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                        c.source_path == local_doc.relative_path && c.corpus_slug == *corpus_slug
                    }) {
                        cd.sync_status = sync::FileStatus::Pushing;
                    }
                    ss.sync_progress = Some(format!("Pushing {}", local_doc.relative_path));
                }

                tokio::time::sleep(throttle).await;
                match sync::push_document(&config.api_url, token, corpus_slug, local_doc).await {
                    Ok(doc_id) => {
                        completed += 1;
                        synced_doc_ids.insert(doc_id.clone());
                        let cached = sync::CachedDocument {
                            document_id: doc_id,
                            corpus_slug: corpus_slug.clone(),
                            source_path: local_doc.relative_path.clone(),
                            body_hash: local_doc.body_hash.clone(),
                            file_size_bytes: local_doc.size,
                            cached_at: chrono::Utc::now().to_rfc3339(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            document_status: Some("draft".to_string()),
                        };
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                    Err(e) => {
                        completed += 1;
                        let is_duplicate = e.contains("409") || e.contains("Duplicate");
                        if is_duplicate {
                            tracing::info!(
                                "Skipped {} (already exists remotely)",
                                local_doc.relative_path
                            );
                            // Resolve the matching remote doc by body_hash so we can mark InSync
                            let resolved_id = remote_by_hash
                                .get(local_doc.body_hash.as_str())
                                .map(|rd| rd.id.clone())
                                .unwrap_or_default();
                            if !resolved_id.is_empty() {
                                synced_doc_ids.insert(resolved_id.clone());
                            }
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                if !resolved_id.is_empty() {
                                    // We found the remote doc — mark as InSync
                                    cd.document_id = resolved_id;
                                    cd.sync_status = sync::FileStatus::InSync;
                                    cd.error_message = None;
                                } else {
                                    cd.sync_status = sync::FileStatus::Skipped;
                                    cd.error_message = Some("Already exists on server".to_string());
                                }
                            }
                        } else {
                            tracing::warn!("Push failed for {}: {}", local_doc.relative_path, e);
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                cd.sync_status = sync::FileStatus::Error;
                                cd.error_message = Some(e.clone());
                            }
                        }
                        {
                            let mut ss = sync_state.write().await;
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                }
            }

            for (local_doc, remote_doc) in &diff.to_update {
                // Mark as Pushing
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                        c.source_path == local_doc.relative_path && c.corpus_slug == *corpus_slug
                    }) {
                        cd.sync_status = sync::FileStatus::Pushing;
                    }
                    ss.sync_progress = Some(format!("Updating {}", local_doc.relative_path));
                }

                tokio::time::sleep(throttle).await;
                match sync::update_document(&config.api_url, token, &remote_doc.id, local_doc).await
                {
                    Ok(_) => {
                        completed += 1;
                        synced_doc_ids.insert(remote_doc.id.clone());
                        let cached = sync::CachedDocument {
                            document_id: remote_doc.id.clone(),
                            corpus_slug: corpus_slug.clone(),
                            source_path: local_doc.relative_path.clone(),
                            body_hash: local_doc.body_hash.clone(),
                            file_size_bytes: local_doc.size,
                            cached_at: chrono::Utc::now().to_rfc3339(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            document_status: remote_doc.status.clone(),
                        };
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                    Err(e) => {
                        // If PATCH fails (e.g., published doc), try creating a version instead
                        if e.contains("Cannot modify body") || e.contains("403") {
                            tracing::info!(
                                "Document {} is published, creating new version",
                                remote_doc.id
                            );
                            let local_path =
                                std::path::Path::new(folder_path).join(&local_doc.relative_path);
                            if let Ok(body) = std::fs::read_to_string(&local_path) {
                                match sync::create_version(
                                    &config.api_url,
                                    token,
                                    &remote_doc.id,
                                    &body,
                                    local_doc.relative_path.as_str(),
                                )
                                .await
                                {
                                    Ok(new_id) => {
                                        completed += 1;
                                        synced_doc_ids.insert(new_id.clone());
                                        let cached = sync::CachedDocument {
                                            document_id: new_id,
                                            corpus_slug: corpus_slug.clone(),
                                            source_path: local_doc.relative_path.clone(),
                                            body_hash: local_doc.body_hash.clone(),
                                            file_size_bytes: local_doc.size,
                                            cached_at: chrono::Utc::now().to_rfc3339(),
                                            sync_status: sync::FileStatus::InSync,
                                            error_message: None,
                                            document_status: Some("draft".to_string()),
                                        };
                                        {
                                            let mut ss = sync_state.write().await;
                                            if let Some(cd) =
                                                ss.cached_documents.iter_mut().find(|c| {
                                                    c.source_path == local_doc.relative_path
                                                        && c.corpus_slug == *corpus_slug
                                                })
                                            {
                                                *cd = cached.clone();
                                            }
                                            ss.sync_progress = Some(format!(
                                                "{}/{} synced",
                                                completed, total_actions
                                            ));
                                        }
                                        // State already updated in ss.cached_documents above
                                    }
                                    Err(ve) => {
                                        tracing::warn!(
                                            "Version creation failed for {}: {}",
                                            remote_doc.id,
                                            ve
                                        );
                                        completed += 1;
                                        let mut ss = sync_state.write().await;
                                        if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                            c.source_path == local_doc.relative_path
                                                && c.corpus_slug == *corpus_slug
                                        }) {
                                            cd.sync_status = sync::FileStatus::Error;
                                            cd.error_message = Some(ve);
                                        }
                                    }
                                }
                            }
                        } else {
                            tracing::warn!("Update failed for {}: {}", local_doc.relative_path, e);
                            completed += 1;
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                cd.sync_status = sync::FileStatus::Error;
                                cd.error_message = Some(e.clone());
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                }
            }
        }

        // Download direction: pull missing remote docs, skip push
        if *direction == sync::SyncDirection::Download {
            let sync_root = std::path::Path::new(folder_path);
            for remote_doc in &diff.to_pull {
                let effective = remote_doc.effective_path();
                // Mark as Pulling
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss
                        .cached_documents
                        .iter_mut()
                        .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                    {
                        cd.sync_status = sync::FileStatus::Pulling;
                    }
                    ss.sync_progress = Some(format!("Pulling {}", effective));
                }

                tokio::time::sleep(throttle).await;
                match sync::pull_document(
                    &config.api_url,
                    token,
                    remote_doc,
                    sync_root,
                    corpus_slug,
                )
                .await
                {
                    Ok(cached) => {
                        completed += 1;
                        let cached = sync::CachedDocument {
                            corpus_slug: corpus_slug.clone(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            ..cached
                        };
                        // Update in sync_state immediately
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == cached.source_path && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                        // State already updated in ss.cached_documents above
                    }
                    Err(e) => {
                        tracing::warn!("Pull failed for {}: {}", remote_doc.id, e);
                        completed += 1;
                        let mut ss = sync_state.write().await;
                        if let Some(cd) = ss
                            .cached_documents
                            .iter_mut()
                            .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                        {
                            cd.sync_status = sync::FileStatus::Error;
                            cd.error_message = Some(e.clone());
                        }
                        ss.sync_progress = Some(format!("{}/{} synced", completed, total_actions));
                    }
                }
            }

            // Handle updates in download direction too
            for (_local_doc, remote_doc) in &diff.to_update {
                let effective = remote_doc.effective_path();
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss
                        .cached_documents
                        .iter_mut()
                        .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                    {
                        cd.sync_status = sync::FileStatus::Pulling;
                    }
                    ss.sync_progress = Some(format!("Updating {}", effective));
                }

                tokio::time::sleep(throttle).await;
                match sync::pull_document(
                    &config.api_url,
                    token,
                    remote_doc,
                    sync_root,
                    corpus_slug,
                )
                .await
                {
                    Ok(cached) => {
                        completed += 1;
                        let cached = sync::CachedDocument {
                            corpus_slug: corpus_slug.clone(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            ..cached
                        };
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == cached.source_path && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                        // State already updated in ss.cached_documents above
                    }
                    Err(e) => {
                        tracing::warn!("Pull update failed for {}: {}", remote_doc.id, e);
                        completed += 1;
                        let mut ss = sync_state.write().await;
                        if let Some(cd) = ss
                            .cached_documents
                            .iter_mut()
                            .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                        {
                            cd.sync_status = sync::FileStatus::Error;
                            cd.error_message = Some(e.clone());
                        }
                        ss.sync_progress = Some(format!("{}/{} synced", completed, total_actions));
                    }
                }
            }
        }

        // Bidirectional sync: push local-only, pull remote-only, resolve conflicts
        if *direction == sync::SyncDirection::Both {
            let sync_root = std::path::Path::new(folder_path);

            // Phase 1: Push local-only files
            for local_doc in &diff.to_push {
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                        c.source_path == local_doc.relative_path && c.corpus_slug == *corpus_slug
                    }) {
                        cd.sync_status = sync::FileStatus::Pushing;
                    }
                    ss.sync_progress = Some(format!("Pushing {}", local_doc.relative_path));
                }

                tokio::time::sleep(throttle).await;
                match sync::push_document(&config.api_url, token, corpus_slug, local_doc).await {
                    Ok(doc_id) => {
                        completed += 1;
                        synced_doc_ids.insert(doc_id.clone());
                        let cached = sync::CachedDocument {
                            document_id: doc_id,
                            corpus_slug: corpus_slug.clone(),
                            source_path: local_doc.relative_path.clone(),
                            body_hash: local_doc.body_hash.clone(),
                            file_size_bytes: local_doc.size,
                            cached_at: chrono::Utc::now().to_rfc3339(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            document_status: Some("draft".to_string()),
                        };
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                    Err(e) => {
                        completed += 1;
                        let is_duplicate = e.contains("409") || e.contains("Duplicate");
                        if is_duplicate {
                            tracing::info!(
                                "Skipped {} (already exists remotely)",
                                local_doc.relative_path
                            );
                            let resolved_id = remote_by_hash
                                .get(local_doc.body_hash.as_str())
                                .map(|rd| rd.id.clone())
                                .unwrap_or_default();
                            if !resolved_id.is_empty() {
                                synced_doc_ids.insert(resolved_id.clone());
                            }
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                if !resolved_id.is_empty() {
                                    cd.document_id = resolved_id;
                                    cd.sync_status = sync::FileStatus::InSync;
                                    cd.error_message = None;
                                } else {
                                    cd.sync_status = sync::FileStatus::Skipped;
                                    cd.error_message = Some("Already exists on server".to_string());
                                }
                            }
                        } else {
                            tracing::warn!("Push failed for {}: {}", local_doc.relative_path, e);
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == local_doc.relative_path
                                    && c.corpus_slug == *corpus_slug
                            }) {
                                cd.sync_status = sync::FileStatus::Error;
                                cd.error_message = Some(e.clone());
                            }
                        }
                        {
                            let mut ss = sync_state.write().await;
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                }
            }

            // Phase 2: Pull remote-only files
            for remote_doc in &diff.to_pull {
                let effective = remote_doc.effective_path();
                {
                    let mut ss = sync_state.write().await;
                    if let Some(cd) = ss
                        .cached_documents
                        .iter_mut()
                        .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                    {
                        cd.sync_status = sync::FileStatus::Pulling;
                    }
                    ss.sync_progress = Some(format!("Pulling {}", effective));
                }

                tokio::time::sleep(throttle).await;
                match sync::pull_document(
                    &config.api_url,
                    token,
                    remote_doc,
                    sync_root,
                    corpus_slug,
                )
                .await
                {
                    Ok(cached) => {
                        completed += 1;
                        let cached = sync::CachedDocument {
                            corpus_slug: corpus_slug.clone(),
                            sync_status: sync::FileStatus::InSync,
                            error_message: None,
                            ..cached
                        };
                        {
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == cached.source_path && c.corpus_slug == *corpus_slug
                            }) {
                                *cd = cached.clone();
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                        // State already updated in ss.cached_documents above
                    }
                    Err(e) => {
                        tracing::warn!("Pull failed for {}: {}", remote_doc.id, e);
                        completed += 1;
                        let effective = remote_doc.effective_path();
                        let mut ss = sync_state.write().await;
                        if let Some(cd) = ss
                            .cached_documents
                            .iter_mut()
                            .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                        {
                            cd.sync_status = sync::FileStatus::Error;
                            cd.error_message = Some(e.clone());
                        }
                        ss.sync_progress = Some(format!("{}/{} synced", completed, total_actions));
                    }
                }
            }

            // Phase 3: Handle conflicts (files that exist both sides with different hashes)
            for (local_doc, remote_doc) in &diff.to_update {
                let local_path = std::path::Path::new(folder_path).join(&local_doc.relative_path);
                let local_mtime = std::fs::metadata(&local_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    });

                // Parse remote updated_at if available
                let remote_time = remote_doc
                    .updated_at
                    .as_ref()
                    .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                    .map(|dt| dt.timestamp() as u64);

                let pull_wins = match (local_mtime, remote_time) {
                    (Some(local_t), Some(remote_t)) => remote_t > local_t,
                    (None, Some(_)) => true,  // no local mtime, trust remote
                    (Some(_), None) => false, // no remote time, trust local
                    (None, None) => true,     // default: server wins
                };

                if pull_wins {
                    // Save local as .conflict before overwriting
                    if local_path.exists() {
                        let conflict_path = local_path.with_extension(format!(
                            "{}.conflict",
                            local_path.extension().unwrap_or_default().to_string_lossy()
                        ));
                        let _ = std::fs::copy(&local_path, &conflict_path);
                    }

                    // Pull remote version
                    {
                        let effective = remote_doc.effective_path();
                        let mut ss = sync_state.write().await;
                        if let Some(cd) = ss
                            .cached_documents
                            .iter_mut()
                            .find(|c| c.source_path == effective && c.corpus_slug == *corpus_slug)
                        {
                            cd.sync_status = sync::FileStatus::Pulling;
                        }
                        ss.sync_progress = Some(format!("Pulling {}", effective));
                    }

                    tokio::time::sleep(throttle).await;
                    match sync::pull_document(
                        &config.api_url,
                        token,
                        remote_doc,
                        sync_root,
                        corpus_slug,
                    )
                    .await
                    {
                        Ok(cached) => {
                            completed += 1;
                            let cached = sync::CachedDocument {
                                corpus_slug: corpus_slug.clone(),
                                sync_status: sync::FileStatus::InSync,
                                error_message: None,
                                ..cached
                            };
                            {
                                let mut ss = sync_state.write().await;
                                if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                    c.source_path == cached.source_path
                                        && c.corpus_slug == *corpus_slug
                                }) {
                                    *cd = cached.clone();
                                }
                                ss.sync_progress =
                                    Some(format!("{}/{} synced", completed, total_actions));
                            }
                            // State already updated in ss.cached_documents above
                        }
                        Err(e) => {
                            tracing::warn!("Pull failed for {}: {}", remote_doc.id, e);
                            completed += 1;
                            let effective = remote_doc.effective_path();
                            let mut ss = sync_state.write().await;
                            if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                c.source_path == effective && c.corpus_slug == *corpus_slug
                            }) {
                                cd.sync_status = sync::FileStatus::Error;
                                cd.error_message = Some(e.clone());
                            }
                            ss.sync_progress =
                                Some(format!("{}/{} synced", completed, total_actions));
                        }
                    }
                } else {
                    // Push local version
                    {
                        let mut ss = sync_state.write().await;
                        if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                            c.source_path == local_doc.relative_path
                                && c.corpus_slug == *corpus_slug
                        }) {
                            cd.sync_status = sync::FileStatus::Pushing;
                        }
                        ss.sync_progress = Some(format!("Pushing {}", local_doc.relative_path));
                    }

                    tokio::time::sleep(throttle).await;
                    match sync::update_document(&config.api_url, token, &remote_doc.id, local_doc)
                        .await
                    {
                        Ok(_) => {
                            completed += 1;
                            let cached = sync::CachedDocument {
                                document_id: remote_doc.id.clone(),
                                corpus_slug: corpus_slug.clone(),
                                source_path: local_doc.relative_path.clone(),
                                body_hash: local_doc.body_hash.clone(),
                                file_size_bytes: local_doc.size,
                                cached_at: chrono::Utc::now().to_rfc3339(),
                                sync_status: sync::FileStatus::InSync,
                                error_message: None,
                                document_status: remote_doc.status.clone(),
                            };
                            {
                                let mut ss = sync_state.write().await;
                                if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                    c.source_path == local_doc.relative_path
                                        && c.corpus_slug == *corpus_slug
                                }) {
                                    *cd = cached.clone();
                                }
                                ss.sync_progress =
                                    Some(format!("{}/{} synced", completed, total_actions));
                            }
                            // State already updated in ss.cached_documents above
                        }
                        Err(e) => {
                            // If PATCH fails (e.g., published doc), try creating a version instead
                            if e.contains("Cannot modify body") || e.contains("403") {
                                tracing::info!(
                                    "Document {} is published, creating new version",
                                    remote_doc.id
                                );
                                let local_body_path = std::path::Path::new(folder_path)
                                    .join(&local_doc.relative_path);
                                if let Ok(body) = std::fs::read_to_string(&local_body_path) {
                                    match sync::create_version(
                                        &config.api_url,
                                        token,
                                        &remote_doc.id,
                                        &body,
                                        local_doc.relative_path.as_str(),
                                    )
                                    .await
                                    {
                                        Ok(new_id) => {
                                            completed += 1;
                                            let cached = sync::CachedDocument {
                                                document_id: new_id,
                                                corpus_slug: corpus_slug.clone(),
                                                source_path: local_doc.relative_path.clone(),
                                                body_hash: local_doc.body_hash.clone(),
                                                file_size_bytes: local_doc.size,
                                                cached_at: chrono::Utc::now().to_rfc3339(),
                                                sync_status: sync::FileStatus::InSync,
                                                error_message: None,
                                                document_status: Some("draft".to_string()),
                                            };
                                            {
                                                let mut ss = sync_state.write().await;
                                                if let Some(cd) =
                                                    ss.cached_documents.iter_mut().find(|c| {
                                                        c.source_path == local_doc.relative_path
                                                            && c.corpus_slug == *corpus_slug
                                                    })
                                                {
                                                    *cd = cached.clone();
                                                }
                                                ss.sync_progress = Some(format!(
                                                    "{}/{} synced",
                                                    completed, total_actions
                                                ));
                                            }
                                            // State already updated in ss.cached_documents above
                                        }
                                        Err(ve) => {
                                            tracing::warn!(
                                                "Version creation failed for {}: {}",
                                                remote_doc.id,
                                                ve
                                            );
                                            completed += 1;
                                            let mut ss = sync_state.write().await;
                                            if let Some(cd) =
                                                ss.cached_documents.iter_mut().find(|c| {
                                                    c.source_path == local_doc.relative_path
                                                        && c.corpus_slug == *corpus_slug
                                                })
                                            {
                                                cd.sync_status = sync::FileStatus::Error;
                                                cd.error_message = Some(ve);
                                            }
                                        }
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    "Update failed for {}: {}",
                                    local_doc.relative_path,
                                    e
                                );
                                completed += 1;
                                let mut ss = sync_state.write().await;
                                if let Some(cd) = ss.cached_documents.iter_mut().find(|c| {
                                    c.source_path == local_doc.relative_path
                                        && c.corpus_slug == *corpus_slug
                                }) {
                                    cd.sync_status = sync::FileStatus::Error;
                                    cd.error_message = Some(e.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Detect remotely deleted documents — only meaningful for Download/Both directions
        // For Upload dirs, the local folder is the source of truth, not the server.
        // Also skip documents that were just created/synced during THIS cycle (not in pre-sync remote_paths).
        if *direction != sync::SyncDirection::Upload {
            let remote_paths: std::collections::HashSet<String> =
                remote_docs.iter().map(|d| d.effective_path()).collect();

            let mut ss = sync_state.write().await;
            let before_len = ss.cached_documents.len();
            ss.cached_documents.retain(|cd| {
                if cd.corpus_slug == *corpus_slug && !cd.document_id.is_empty() {
                    // Don't remove docs that were just synced in this cycle
                    if synced_doc_ids.contains(&cd.document_id) {
                        return true;
                    }
                    if !remote_paths.contains(&cd.source_path) {
                        tracing::info!(
                            "Document {} ({}) removed remotely, clearing from cache",
                            cd.document_id,
                            cd.source_path
                        );
                        return false;
                    }
                }
                true
            });
            let removed = before_len - ss.cached_documents.len();
            if removed > 0 {
                tracing::info!(
                    "Filtered out {} remotely-deleted documents from corpus {}",
                    removed,
                    corpus_slug
                );
            }
        }
    }

    // Compute total size from synced documents only (not the entire folder tree).
    // The old approach used get_cache_size on each linked folder, which recursively
    // summed ALL files including node_modules, .next, etc. — producing wildly wrong totals.
    let total_size: u64 = {
        let ss = sync_state.read().await;
        ss.cached_documents.iter().map(|d| d.file_size_bytes).sum()
    };

    {
        let mut ss = sync_state.write().await;

        // Final dedup of cached_documents by (source_path, corpus_slug) — keep last
        {
            let mut seen: std::collections::HashMap<(String, String), usize> =
                std::collections::HashMap::new();
            for (i, doc) in ss.cached_documents.iter().enumerate() {
                seen.insert((doc.source_path.clone(), doc.corpus_slug.clone()), i);
            }
            let mut keep_indices: Vec<usize> = seen.into_values().collect();
            keep_indices.sort();
            ss.cached_documents = keep_indices
                .into_iter()
                .map(|i| ss.cached_documents[i].clone())
                .collect();
        }

        ss.total_size_bytes = total_size;
        ss.last_sync_at = Some(chrono::Utc::now().to_rfc3339());
        ss.is_syncing = false;
        ss.sync_progress = None;
        sync::save_sync_state(&config.data_dir(), &ss);
    }

    tracing::info!("Sync complete");
    Ok(())
}

#[tauri::command]
async fn sync_content(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let api_token = get_api_token(&state.auth).await?;
    let cfg = state.config.read().await;
    do_sync(&cfg, &api_token, &state.sync_state, &state.credits).await
}

#[tauri::command]
async fn set_auto_sync(
    state: tauri::State<'_, SharedState>,
    enabled: bool,
    interval_secs: Option<u64>,
) -> Result<(), String> {
    let mut ss = state.sync_state.write().await;
    ss.auto_sync_enabled = enabled;
    if let Some(interval) = interval_secs {
        ss.auto_sync_interval_secs = interval.max(60); // minimum 1 minute
    }
    let cfg = state.config.read().await;
    sync::save_sync_state(&cfg.data_dir(), &ss);
    tracing::info!(
        "Auto-sync set: enabled={}, interval={}s",
        ss.auto_sync_enabled,
        ss.auto_sync_interval_secs
    );
    Ok(())
}

// --- Version & Diff Commands ------------------------------------------------

#[tauri::command]
async fn open_file(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("File not found: {}", path));
    }
    open::that(&path).map_err(|e| format!("Failed to open file: {}", e))
}

#[tauri::command]
async fn fetch_document_versions(
    state: tauri::State<'_, SharedState>,
    document_id: String,
) -> Result<sync::VersionHistoryResponse, String> {
    let api_token = get_api_token(&state.auth).await?;
    let cfg = state.config.read().await;
    sync::fetch_version_history(&cfg.api_url, &api_token, &document_id).await
}

#[tauri::command]
async fn compute_diff(
    state: tauri::State<'_, SharedState>,
    old_doc_id: String,
    new_doc_id: String,
) -> Result<Vec<sync::DiffHunk>, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;

    // Fetch both document bodies
    let client = reqwest::Client::new();

    let old_resp = client
        .get(format!(
            "{}/api/v1/wire/documents/{}/body",
            config.api_url, old_doc_id
        ))
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch old document: {}", e))?;

    if !old_resp.status().is_success() {
        return Err(format!(
            "Failed to fetch old document: {}",
            old_resp.status()
        ));
    }
    let old_body = old_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read old document: {}", e))?;

    let new_resp = client
        .get(format!(
            "{}/api/v1/wire/documents/{}/body",
            config.api_url, new_doc_id
        ))
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch new document: {}", e))?;

    if !new_resp.status().is_success() {
        return Err(format!(
            "Failed to fetch new document: {}",
            new_resp.status()
        ));
    }
    let new_body = new_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read new document: {}", e))?;

    // Check size limits (50K words max)
    let word_count = old_body.split_whitespace().count() + new_body.split_whitespace().count();
    if word_count > 100_000 {
        return Err("Documents too large for diff (>50K words each). Download both versions to compare manually.".to_string());
    }

    Ok(sync::compute_word_diff(&old_body, &new_body))
}

#[tauri::command]
async fn update_document_status(
    state: tauri::State<'_, SharedState>,
    document_id: String,
    status: String,
) -> Result<serde_json::Value, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/{}", config.api_url, document_id);

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "status": status }))
        .send()
        .await
        .map_err(|e| format!("Status update failed: {}", e))?;

    if !resp.status().is_success() {
        let status_code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Status update failed ({}): {}", status_code, text));
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    tracing::info!("Document {} status changed to {}", document_id, status);
    Ok(result)
}

#[tauri::command]
async fn bulk_publish(
    app: tauri::AppHandle,
    state: tauri::State<'_, SharedState>,
    corpus_slug: String,
) -> Result<serde_json::Value, String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;

    // Fetch all draft documents for this corpus
    let docs = sync::fetch_corpus_documents(&config.api_url, &api_token, &corpus_slug).await?;
    let draft_ids: Vec<String> = docs
        .iter()
        .filter(|d| {
            d.status.as_deref() != Some("published") && d.status.as_deref() != Some("retracted")
        })
        .map(|d| d.id.clone())
        .collect();

    let total = draft_ids.len();
    if total == 0 {
        return Ok(
            serde_json::json!({ "published": 0, "errors": 0, "total": 0, "message": "No draft documents to publish" }),
        );
    }

    // Use the server's bulk endpoint instead of one-by-one PATCH calls.
    // Process in batches of 200 to avoid request size limits.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut published = 0usize;
    let mut errors = 0usize;
    let mut error_details: Vec<String> = Vec::new();
    let batch_size = 200;

    for (batch_idx, chunk) in draft_ids.chunks(batch_size).enumerate() {
        let url = format!(
            "{}/api/v1/wire/corpora/{}/bulk",
            config.api_url, corpus_slug
        );
        tracing::info!(
            "Bulk publish batch {}: {} documents ({}–{}/{})",
            batch_idx + 1,
            chunk.len(),
            batch_idx * batch_size + 1,
            batch_idx * batch_size + chunk.len(),
            total
        );

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "action": "publish",
                "document_ids": chunk,
            }))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let batch_applied = body["applied"].as_u64().unwrap_or(0) as usize;
                let batch_errors = body["errors"].as_array().map(|a| a.len()).unwrap_or(0);
                published += batch_applied;
                errors += batch_errors;
                if let Some(errs) = body["errors"].as_array() {
                    for err in errs {
                        error_details.push(err.to_string());
                    }
                }
            }
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                errors += chunk.len();
                error_details.push(format!(
                    "Batch {} failed ({}): {}",
                    batch_idx + 1,
                    status,
                    text
                ));
                tracing::warn!(
                    "Bulk publish batch {} failed ({}): {}",
                    batch_idx + 1,
                    status,
                    text
                );
            }
            Err(e) => {
                errors += chunk.len();
                error_details.push(format!("Batch {} error: {}", batch_idx + 1, e));
                tracing::error!("Bulk publish batch {} error: {}", batch_idx + 1, e);
            }
        }

        // Emit progress event so the frontend can show a progress bar
        let _ = app.emit(
            "bulk-publish-progress",
            serde_json::json!({
                "corpus_slug": corpus_slug,
                "published": published,
                "errors": errors,
                "total": total,
                "batch": batch_idx + 1,
            }),
        );
    }

    tracing::info!(
        "Bulk publish complete: {}/{} published, {} errors",
        published,
        total,
        errors
    );
    Ok(serde_json::json!({
        "published": published,
        "errors": errors,
        "total": total,
        "error_details": error_details,
    }))
}

#[tauri::command]
async fn pin_version(
    state: tauri::State<'_, SharedState>,
    document_id: String,
    folder_path: String,
) -> Result<(), String> {
    let api_token = get_api_token(&state.auth).await?;
    let config = state.config.read().await;
    let config = &*config;

    // Check storage quota
    let versions_dir = std::path::Path::new(&folder_path).join(".versions");
    let current_size = if versions_dir.exists() {
        sync::get_cache_size(&versions_dir).await
    } else {
        0
    };

    let quota_bytes = {
        let ss = state.sync_state.read().await;
        ss.storage_quota_mb * 1024 * 1024
    };

    if current_size > quota_bytes {
        return Err(format!(
            "Storage quota exceeded ({:.1} MB / {} MB). Unpin older versions first.",
            current_size as f64 / (1024.0 * 1024.0),
            quota_bytes / (1024 * 1024)
        ));
    }

    // Fetch the document info
    let client = reqwest::Client::new();
    let doc_resp = client
        .get(format!(
            "{}/api/v1/wire/documents/{}",
            config.api_url, document_id
        ))
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch document: {}", e))?;

    if !doc_resp.status().is_success() {
        return Err(format!("Failed to fetch document: {}", doc_resp.status()));
    }
    let doc_info: serde_json::Value = doc_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse document: {}", e))?;

    let body_resp = client
        .get(format!(
            "{}/api/v1/wire/documents/{}/body",
            config.api_url, document_id
        ))
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch document body: {}", e))?;

    if !body_resp.status().is_success() {
        return Err(format!(
            "Failed to fetch document body: {}",
            body_resp.status()
        ));
    }
    let body = body_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;

    // Save to .versions/
    let version_num = doc_info["version_number"].as_i64().unwrap_or(1);
    let source_path = doc_info["source_path"].as_str().unwrap_or(&document_id);
    let stem = std::path::Path::new(source_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| document_id.clone());
    let ext = std::path::Path::new(source_path)
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "md".to_string());

    let version_file = versions_dir.join(format!("{}.v{}.{}", stem, version_num, ext));

    tokio::fs::create_dir_all(&versions_dir)
        .await
        .map_err(|e| format!("Failed to create .versions dir: {}", e))?;
    tokio::fs::write(&version_file, &body)
        .await
        .map_err(|e| format!("Failed to save version: {}", e))?;

    // Track in sync state
    {
        let mut ss = state.sync_state.write().await;
        if !ss.pinned_versions.contains(&document_id) {
            ss.pinned_versions.push(document_id);
        }
        sync::save_sync_state(&config.data_dir(), &ss);
    }

    Ok(())
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
            ts.status = tunnel::TunnelConnectionStatus::Error(format!(
                "Failed to download cloudflared: {}",
                e
            ));
            return;
        }
    }

    // Step 2: Provision tunnel (or use persisted token)
    // Guard: if the persisted tunnel belongs to a different node_id (e.g. two
    // machines that previously shared an identity), discard and re-provision.
    let ts = if let Some(ref persisted_ts) = persisted {
        // TunnelUrl: !Deref. Use .as_ref().map(|u| u.as_str()) to get
        // Option<&str> so starts_with works in the closure below.
        let stale = persisted_ts
            .tunnel_url
            .as_ref()
            .map(|u| u.as_str())
            .map_or(false, |url| {
                // Tunnel URLs are https://node-{nodeId}.agent-wire.com — if the
                // embedded node_id doesn't match ours, these credentials belong
                // to a different node and must not be reused.
                let expected_prefix = format!("https://node-{}.", node_id);
                !url.starts_with(&expected_prefix)
            });
        if stale {
            tracing::warn!(
                "Persisted tunnel belongs to a different node (url={:?}, current node={}). Re-provisioning.",
                persisted_ts.tunnel_url, node_id
            );
            // Delete stale tunnel.json so we never pick it up again
            let _ = std::fs::remove_file(data_dir.join("tunnel.json"));
            provision_new_tunnel(&tunnel_state, api_url, access_token, node_id).await
        } else if persisted_ts.tunnel_token.is_some() {
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
            ts.status =
                tunnel::TunnelConnectionStatus::Error(format!("Provisioning failed: {}", e));
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

// --- Compose Drafts ----------------------------------------------------------

fn compose_drafts_path(config: &WireNodeConfig) -> std::path::PathBuf {
    config.data_dir().join("compose_drafts.json")
}

#[tauri::command]
async fn save_compose_draft(
    state: tauri::State<'_, SharedState>,
    draft: serde_json::Value,
) -> Result<(), String> {
    let cfg = state.config.read().await;
    let path = compose_drafts_path(&cfg);

    // Load existing drafts
    let mut drafts: Vec<serde_json::Value> = if path.exists() {
        let data =
            std::fs::read_to_string(&path).map_err(|e| format!("Failed to read drafts: {}", e))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&data).unwrap_or(serde_json::json!([]));
        match parsed {
            serde_json::Value::Array(arr) => arr,
            _ => vec![],
        }
    } else {
        vec![]
    };

    // If draft has an "id", replace existing; otherwise append
    if let Some(draft_id) = draft.get("id").and_then(|v| v.as_str()) {
        if let Some(pos) = drafts
            .iter()
            .position(|d| d.get("id").and_then(|v| v.as_str()) == Some(draft_id))
        {
            drafts[pos] = draft;
        } else {
            drafts.push(draft);
        }
    } else {
        drafts.push(draft);
    }

    let json = serde_json::to_string_pretty(&drafts).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    std::fs::write(&path, json).map_err(|e| format!("Failed to write drafts: {}", e))?;
    tracing::info!("Compose draft saved to {:?}", path);
    Ok(())
}

#[tauri::command]
async fn get_compose_drafts(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let cfg = state.config.read().await;
    let path = compose_drafts_path(&cfg);
    if !path.exists() {
        return Ok(serde_json::json!([]));
    }
    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read drafts: {}", e))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse drafts: {}", e))?;
    Ok(parsed)
}

#[tauri::command]
async fn delete_compose_draft(
    state: tauri::State<'_, SharedState>,
    draft_id: String,
) -> Result<(), String> {
    let cfg = state.config.read().await;
    let path = compose_drafts_path(&cfg);
    if !path.exists() {
        return Ok(());
    }

    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read drafts: {}", e))?;
    let parsed: serde_json::Value = serde_json::from_str(&data).unwrap_or(serde_json::json!([]));
    let drafts: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter(|d| d.get("id").and_then(|v| v.as_str()) != Some(&draft_id))
            .collect(),
        _ => vec![],
    };

    let json = serde_json::to_string_pretty(&drafts).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write drafts: {}", e))?;
    Ok(())
}

// --- Wire Handle Cache -------------------------------------------------------

fn handle_cache_path(config: &WireNodeConfig) -> std::path::PathBuf {
    config.data_dir().join("handle_cache.json")
}

#[tauri::command]
async fn cache_wire_handles(
    state: tauri::State<'_, SharedState>,
    handles: serde_json::Value,
) -> Result<(), String> {
    let cfg = state.config.read().await;
    let path = handle_cache_path(&cfg);
    let wrapper = serde_json::json!({
        "handles": handles,
        "cached_at": chrono::Utc::now().to_rfc3339(),
    });
    let json = serde_json::to_string_pretty(&wrapper).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    std::fs::write(&path, json).map_err(|e| format!("Failed to write handle cache: {}", e))?;
    tracing::info!("Wire handle cache saved to {:?}", path);
    Ok(())
}

#[tauri::command]
async fn get_cached_wire_handles(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let cfg = state.config.read().await;
    let path = handle_cache_path(&cfg);
    if !path.exists() {
        return Ok(serde_json::json!({ "handles": [], "cached_at": null }));
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read handle cache: {}", e))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse handle cache: {}", e))?;
    Ok(parsed)
}

// --- Planner ----------------------------------------------------------------

const PLANNER_FALLBACK_PROMPT: &str = r#"You are the Wire Node intent planner. You take a user's natural language intent and produce a structured execution plan.

## Available Commands

{{VOCABULARY}}

Use ONLY the command names from the vocabulary above. The executor handles all HTTP details — you never specify methods, paths, or URLs. Each step has exactly one of: command (with args), or navigate. Steps execute independently — no data flow between steps. Steps support on_error: "abort" | "continue" (default "continue").

Your response must be valid JSON with this structure:
{
  "plan_id": "uuid",
  "intent": "the original user text",
  "steps": [{ "id": "step-1", "description": "what this step does", "estimated_cost": null, "command": "cmd_name", "args": {}, "on_error": "continue" }],
  "total_estimated_cost": null,
  "ui_schema": [{ "type": "widget_type", "field": "field_name", "label": "Label" }]
}
"#;

const PLANNER_WIDGET_CATALOG: &str = r#"[
  { "type": "corpus_selector", "description": "Dropdown to pick a corpus/pyramid slug" },
  { "type": "text_input", "description": "Free-text input field" },
  { "type": "cost_preview", "description": "Shows estimated cost before confirming" },
  { "type": "toggle", "description": "Boolean on/off switch" },
  { "type": "checkbox", "description": "Same as toggle" },
  { "type": "agent_selector", "description": "Dropdown to pick an agent from the registry" },
  { "type": "confirmation", "description": "Confirm/cancel button pair" },
  { "type": "select", "description": "Dropdown with custom options" }
]"#;

/// Single-stage planner: loads the FULL vocabulary (all categories) and generates a plan in one LLM call.
#[tauri::command]
async fn planner_call(
    state: tauri::State<'_, SharedState>,
    intent: String,
    context: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::llm;

    let config = {
        let cfg = state.pyramid.config.read().await;
        cfg.clone()
    };

    // Load vocabulary from YAML registry — try local files first, fall back to bundled
    let vocab_dir = state.pyramid.chains_dir.join("vocabulary_yaml");
    let vocab_registry = match vocabulary::load_from_directory(&vocab_dir) {
        Ok(reg) if !reg.domains.is_empty() => reg,
        _ => {
            tracing::info!("Using bundled vocabulary (no local YAML files found)");
            vocabulary::load_bundled()
        }
    };
    let full_vocabulary = vocab_registry.to_prompt_text();
    tracing::info!(
        "Vocabulary loaded: {} domains, {} commands, prompt text {} chars",
        vocab_registry.domains.len(),
        vocab_registry.domains.iter().map(|d| d.commands.len()).sum::<usize>(),
        full_vocabulary.len(),
    );

    // Load planner system prompt from chains_dir with inline fallback
    let prompt_path = state
        .pyramid
        .chains_dir
        .join("prompts/planner/planner-system.md");
    let system_template = match std::fs::read_to_string(&prompt_path) {
        Ok(contents) => contents,
        Err(_) => {
            tracing::warn!("planner-system.md not found — using inline fallback");
            PLANNER_FALLBACK_PROMPT.to_string()
        }
    };

    // Replace template placeholders
    let context_json =
        serde_json::to_string_pretty(&context).unwrap_or_else(|_| "{}".to_string());
    let system_prompt = system_template
        .replace("{{VOCABULARY}}", &full_vocabulary)
        .replace("{{WIDGET_CATALOG}}", PLANNER_WIDGET_CATALOG)
        .replace("{{CONTEXT}}", &context_json);

    tracing::info!(
        "Planner system prompt: {} chars, user intent: {} chars",
        system_prompt.len(),
        intent.len(),
    );

    // Call LLM for plan generation — single call with full vocabulary
    let response = llm::call_model_unified(
        &config,
        &system_prompt,
        &intent,
        0.3,
        100_000, // Pillar 43: max_tokens is a safety ceiling, not a behavior control
        Some(&serde_json::json!({"type": "json_object"})), // Pillar 43: prompt controls output, not max_tokens
    )
    .await
    .map_err(|e| format!("Planner LLM call failed: {}", e))?;

    // Parse response
    let plan = llm::extract_json(&response.content)
        .map_err(|e| format!("Failed to parse planner response: {}", e))?;

    // Validate required fields
    match plan.get("steps") {
        Some(steps) if steps.is_array() && !steps.as_array().unwrap().is_empty() => {}
        _ => return Err("Planner response missing non-empty 'steps' array".to_string()),
    }
    if !plan.get("ui_schema").map_or(false, |u| u.is_array()) {
        return Err("Planner response missing 'ui_schema' array".to_string());
    }

    Ok(plan)
}

/// Return the vocabulary dispatch table to the frontend.
/// The frontend uses this to translate named commands into API calls.
#[tauri::command]
async fn get_vocabulary_registry(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let vocab_dir = state.pyramid.chains_dir.join("vocabulary_yaml");
    let registry = match vocabulary::load_from_directory(&vocab_dir) {
        Ok(reg) if !reg.domains.is_empty() => reg,
        _ => vocabulary::load_bundled(),
    };
    Ok(registry.to_frontend_registry())
}

fn onboarding_file_path(config: &WireNodeConfig) -> std::path::PathBuf {
    config.data_dir().join("onboarding.json")
}

#[tauri::command]
async fn get_node_name(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let cfg = state.config.read().await;
    Ok(cfg.node_name())
}

#[tauri::command]
async fn is_onboarded(state: tauri::State<'_, SharedState>) -> Result<bool, String> {
    let cfg = state.config.read().await;
    let path = onboarding_file_path(&cfg);
    Ok(path.exists())
}

#[tauri::command]
async fn save_onboarding(
    state: tauri::State<'_, SharedState>,
    node_name: String,
    storage_cap_gb: f64,
    mesh_hosting_enabled: bool,
    auto_update_enabled: Option<bool>,
) -> Result<(), String> {
    let auto_update = auto_update_enabled.unwrap_or(false);
    let onboarding = serde_json::json!({
        "node_name": node_name,
        "storage_cap_gb": storage_cap_gb,
        "mesh_hosting_enabled": mesh_hosting_enabled,
        "auto_update_enabled": auto_update,
        "completed_at": chrono::Utc::now().to_rfc3339(),
    });

    // Write to disk
    let path = {
        let cfg = state.config.read().await;
        onboarding_file_path(&cfg)
    };
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    std::fs::write(&path, serde_json::to_string_pretty(&onboarding).unwrap())
        .map_err(|e| format!("Failed to save onboarding: {}", e))?;

    // Update in-memory config so changes take effect immediately
    {
        let mut cfg = state.config.write().await;
        cfg.storage_cap_gb = storage_cap_gb;
        cfg.mesh_hosting_enabled = mesh_hosting_enabled;
        cfg.auto_update_enabled = auto_update;
    }

    tracing::info!(
        "Onboarding saved: name={}, storage={}GB, mesh={}, auto_update={}",
        node_name,
        storage_cap_gb,
        mesh_hosting_enabled,
        auto_update
    );

    Ok(())
}

#[tauri::command]
async fn get_logs(state: tauri::State<'_, SharedState>) -> Result<Vec<String>, String> {
    let cfg = state.config.read().await;
    let log_path = cfg.data_dir().join("wire-node.log");
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_default();
    let lines: Vec<String> = content.lines().rev().take(500).map(String::from).collect();
    Ok(lines)
}

// --- Pyramid Commands -------------------------------------------------------

use wire_node_lib::pyramid::db as pyramid_db;
use wire_node_lib::pyramid::faq as pyramid_faq;
use wire_node_lib::pyramid::query as pyramid_query;
use wire_node_lib::pyramid::types::*;

#[tauri::command]
async fn pyramid_list_slugs(state: tauri::State<'_, SharedState>) -> Result<Vec<SlugInfo>, String> {
    let conn = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::slug::list_slugs(&conn).map_err(|e| e.to_string())
}

/// Per-slug publication status for the frontend Pyramids tab.
#[derive(serde::Serialize)]
struct PyramidPublicationInfo {
    slug: String,
    node_count: i64,
    unpublished_count: i64,
    last_published_build_id: Option<String>,
    current_build_id: Option<String>,
    last_built_at: Option<String>,
    /// WS-ONLINE-D: Whether this pyramid is pinned from a remote source.
    pinned: bool,
    /// WS-ONLINE-D: Source tunnel URL if pinned.
    source_tunnel_url: Option<String>,
}

#[tauri::command]
async fn pyramid_get_publication_status(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<PyramidPublicationInfo>, String> {
    let conn = state.pyramid.reader.lock().await;
    let slugs = wire_node_lib::pyramid::slug::list_slugs(&conn).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for s in slugs {
        if s.archived_at.is_some() {
            continue;
        }
        let unpublished = pyramid_db::count_unpublished_nodes(&conn, &s.slug).unwrap_or(0);
        let last_pub = pyramid_db::get_last_published_build_id(&conn, &s.slug).unwrap_or(None);
        let current = pyramid_db::get_current_build_id(&conn, &s.slug).unwrap_or(None);
        let pinned = pyramid_db::is_pinned(&conn, &s.slug).unwrap_or(false);
        let source_tunnel_url = if pinned {
            pyramid_db::get_source_tunnel_url(&conn, &s.slug).unwrap_or(None)
        } else {
            None
        };
        result.push(PyramidPublicationInfo {
            slug: s.slug,
            node_count: s.node_count,
            unpublished_count: unpublished,
            last_published_build_id: last_pub,
            current_build_id: current,
            last_built_at: s.last_built_at,
            pinned,
            source_tunnel_url,
        });
    }
    Ok(result)
}

#[tauri::command]
async fn pyramid_apex(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<NodeWithWebEdges, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_query::get_apex_with_edges(&conn, &slug)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No apex node found".to_string())
}

#[tauri::command]
async fn pyramid_node(
    state: tauri::State<'_, SharedState>,
    slug: String,
    node_id: String,
) -> Result<NodeWithWebEdges, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_query::get_node_with_edges(&conn, &slug, &node_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Node not found".to_string())
}

#[tauri::command]
async fn pyramid_tree(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<Vec<TreeNode>, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_query::get_tree(&conn, &slug).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_drill(
    state: tauri::State<'_, SharedState>,
    slug: String,
    node_id: String,
    // Phase 18b L7: optional flag the frontend sets when the drill is
    // launched from a search result. When true, the IPC fires
    // `search_hit` IN ADDITION TO the standard `user_drill` so the
    // demand signal subsystem can distinguish a direct drill from a
    // search-then-drill flow. Defaults to `false` (the bare drill).
    from_search: Option<bool>,
) -> Result<DrillResult, String> {
    let result = {
        let conn = state.pyramid.reader.lock().await;
        pyramid_query::drill(&conn, &slug, &node_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Node not found".to_string())?
    };

    // Phase 12: fire-and-forget user_drill demand signal recording.
    // The IPC drill is always user-initiated (desktop UI), so we
    // always record `user_drill` with source "user".
    //
    // Phase 18b L7: when `from_search = true`, also record `search_hit`
    // so the search→drill flow shows up as a distinct demand signal.
    let writer = state.pyramid.writer.clone();
    let slug_for_signal = slug.clone();
    let node_for_signal = node_id.clone();
    let from_search_flag = from_search.unwrap_or(false);
    tokio::spawn(async move {
        let conn = writer.lock().await;
        let policy = match wire_node_lib::pyramid::db::load_active_evidence_policy(
            &conn,
            Some(&slug_for_signal),
        ) {
            Ok(p) => p,
            Err(_) => return,
        };
        let _ = wire_node_lib::pyramid::demand_signal::record_demand_signal(
            &conn,
            &slug_for_signal,
            &node_for_signal,
            "user_drill",
            Some("user"),
            &policy,
        );
        if from_search_flag {
            let _ = wire_node_lib::pyramid::demand_signal::record_demand_signal(
                &conn,
                &slug_for_signal,
                &node_for_signal,
                "search_hit",
                Some("user"),
                &policy,
            );
        }
    });

    Ok(result)
}

/// Phase 12: Re-evaluate all deferred evidence questions against the
/// current active evidence_policy. Called by the ToolsMode policy
/// editor after a supersession, or manually by the user via the
/// "Apply to all deferred" button.
#[tauri::command]
async fn pyramid_reevaluate_deferred_questions(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<ReevaluateDeferredResult, String> {
    use wire_node_lib::pyramid::db;
    use wire_node_lib::pyramid::triage::{resolve_decision, TriageDecision, TriageFacts};
    use wire_node_lib::pyramid::types::LayerQuestion;

    let writer = state.pyramid.writer.clone();
    // Acquire the writer lock OUTSIDE the spawn_blocking (async),
    // then hold it across the sync block by passing the locked guard
    // through an owned path. Simpler: do the whole thing in the async
    // context by using a blocking-safe DB path read instead.
    let conn = writer.lock().await;
    let result = tokio::task::block_in_place(move || -> Result<ReevaluateDeferredResult, String> {
        let policy = db::load_active_evidence_policy(&conn, Some(&slug))
            .map_err(|e| format!("load policy: {e}"))?;
        let deferred = db::list_all_deferred(&conn, &slug).map_err(|e| e.to_string())?;
        let mut result = ReevaluateDeferredResult {
            evaluated: 0,
            activated: 0,
            still_deferred: 0,
            skipped: 0,
        };
        // Phase 12 wanderer fix: evaluate has_demand_signals once at
        // slug granularity, not per-question. Per-node aggregation
        // by question.question_id never matched because question_id
        // is a q-{sha256} hash while demand signals land on
        // L{layer}-{seq} node ids. See
        // evidence_answering::run_triage_gate for the matching fix
        // and rationale.
        let slug_has_demand_signals = policy.demand_signals.iter().any(|rule| {
            let w = rule.window.trim();
            let window = if w.starts_with('-') || w.contains(' ') {
                w.to_string()
            } else {
                let (num_part, unit_part): (String, String) =
                    w.chars().partition(|c| c.is_ascii_digit());
                let n: i64 = num_part.parse().unwrap_or(14);
                let (n, unit) = match unit_part.as_str() {
                    "d" => (n, "days"),
                    "h" => (n, "hours"),
                    "w" => (n * 7, "days"),
                    "m" => (n, "minutes"),
                    _ => (n, "days"),
                };
                format!("-{} {}", n, unit)
            };
            db::sum_slug_demand_weight(&conn, &slug, &rule.r#type, &window)
                .unwrap_or(0.0)
                >= rule.threshold
        });

        for row in deferred {
            result.evaluated += 1;
            let question: LayerQuestion = match serde_json::from_str(&row.question_json) {
                Ok(q) => q,
                Err(_) => continue,
            };
            let facts = TriageFacts {
                question: &question,
                target_node_distilled: None,
                target_node_depth: Some(question.layer),
                is_first_build: false,
                is_stale_check: true,
                has_demand_signals: slug_has_demand_signals,
                evidence_question_trivial: None,
                evidence_question_high_value: None,
            };
            match resolve_decision(&policy, &facts).map_err(|e| e.to_string())? {
                TriageDecision::Answer { .. } => {
                    if db::remove_deferred(&conn, &slug, &question.question_id).is_ok() {
                        result.activated += 1;
                    }
                }
                TriageDecision::Defer { check_interval, .. } => {
                    let _ = db::update_deferred_next_check(
                        &conn,
                        &slug,
                        &question.question_id,
                        &check_interval,
                        policy.contribution_id.as_deref(),
                    );
                    result.still_deferred += 1;
                }
                TriageDecision::Skip { .. } => {
                    if db::remove_deferred(&conn, &slug, &question.question_id).is_ok() {
                        result.skipped += 1;
                    }
                }
            }
        }
        Ok(result)
    });
    result
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReevaluateDeferredResult {
    evaluated: u64,
    activated: u64,
    still_deferred: u64,
    skipped: u64,
}

#[tauri::command]
async fn pyramid_list_question_overlays(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<Vec<wire_node_lib::pyramid::db::QuestionOverlayInfo>, String> {
    let conn = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::db::list_question_overlays(&conn, &slug).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_search(
    state: tauri::State<'_, SharedState>,
    slug: String,
    term: String,
) -> Result<Vec<SearchHit>, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_query::search(&conn, &slug, &term).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_get_references(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let references = pyramid_db::get_slug_references(&conn, &slug).map_err(|e| e.to_string())?;
    let referrers = pyramid_db::get_slug_referrers(&conn, &slug).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "references": references,
        "referrers": referrers,
    }))
}

#[tauri::command]
async fn pyramid_get_composed_view(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let view = pyramid_query::get_composed_view(&conn, &slug).map_err(|e| e.to_string())?;
    serde_json::to_value(&view).map_err(|e| e.to_string())
}

/// Post-build seeding: populate auto_update_config, file_hashes, and start engine + watcher.
/// Called after a successful pyramid build to auto-enable DADBEAR.
async fn post_build_seed(
    pyramid_state: &std::sync::Arc<wire_node_lib::pyramid::PyramidState>,
    slug: &str,
    content_type: &ContentType,
) -> Result<(), String> {
    let db_path = pyramid_state
        .data_dir
        .as_ref()
        .expect("data_dir not set")
        .join("pyramid.db")
        .to_string_lossy()
        .to_string();

    // WS-ONLINE-E: Update cached emergent price after build
    {
        let conn = pyramid_state.writer.lock().await;
        if let Err(e) = pyramid_db::update_cached_emergent_price(&conn, slug) {
            tracing::warn!(
                "Failed to update cached emergent price for '{}': {}",
                slug,
                e
            );
        }
    }

    // Get slug info for source paths
    let source_paths: Vec<String> = {
        let conn = pyramid_state.reader.lock().await;
        match wire_node_lib::pyramid::slug::get_slug(&conn, slug) {
            Ok(Some(info)) => wire_node_lib::pyramid::slug::resolve_validated_source_paths(
                &info.source_path,
                &info.content_type,
                pyramid_state.data_dir.as_deref(),
            )
            .unwrap_or_default()
            .into_iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
            _ => Vec::new(),
        }
    };

    // Determine extensions and config files based on content type, and hash files
    let (extensions_json, config_files_json) = match &content_type {
        ContentType::Code => {
            // Re-walk the source dirs to compute hashes and collect extensions
            let db_path_clone = db_path.clone();
            let slug_owned = slug.to_string();
            let source_paths_clone = source_paths.clone();
            tokio::task::spawn_blocking(move || {
                let conn = rusqlite::Connection::open(&db_path_clone).map_err(|e| e.to_string())?;
                let code_exts: Vec<String> = wire_node_lib::pyramid::ingest::code_extensions()
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();
                let config_fnames: Vec<String> = wire_node_lib::pyramid::ingest::config_files()
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();

                // Hash each tracked file
                for path_str in &source_paths_clone {
                    let dir = std::path::Path::new(path_str);
                    if !dir.is_dir() {
                        continue;
                    }
                    hash_source_files(&conn, &slug_owned, dir, &code_exts, &config_fnames)?;
                }

                let exts_json = serde_json::to_string(&code_exts).unwrap_or("[]".to_string());
                let configs_json =
                    serde_json::to_string(&config_fnames).unwrap_or("[]".to_string());
                Ok::<(String, String), String>((exts_json, configs_json))
            })
            .await
            .map_err(|e| format!("Spawn blocking failed: {e}"))??
        }
        ContentType::Document => {
            let db_path_clone = db_path.clone();
            let slug_owned = slug.to_string();
            let source_paths_clone = source_paths.clone();
            tokio::task::spawn_blocking(move || {
                let conn = rusqlite::Connection::open(&db_path_clone).map_err(|e| e.to_string())?;
                let doc_exts: Vec<String> = wire_node_lib::pyramid::ingest::doc_extensions()
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();

                for path_str in &source_paths_clone {
                    let dir = std::path::Path::new(path_str);
                    if !dir.is_dir() {
                        continue;
                    }
                    hash_source_files(&conn, &slug_owned, dir, &doc_exts, &[])?;
                }

                let exts_json = serde_json::to_string(&doc_exts).unwrap_or("[]".to_string());
                Ok::<(String, String), String>((exts_json, "[]".to_string()))
            })
            .await
            .map_err(|e| format!("Spawn blocking failed: {e}"))??
        }
        ContentType::Conversation | ContentType::Vine | ContentType::Question => {
            // Conversations, vines, and question pyramids don't use file watching
            ("[]".to_string(), "[]".to_string())
        }
    };

    // Insert auto_update_config defaults
    {
        let conn = pyramid_state.writer.lock().await;
        wire_node_lib::pyramid::db::insert_auto_update_config_defaults(
            &conn,
            slug,
            &extensions_json,
            &config_files_json,
        )
        .map_err(|e| e.to_string())?;
    }

    // Backfill node_ids in pyramid_file_hashes
    {
        let db_path_clone = db_path.clone();
        let slug_owned = slug.to_string();
        let ct = content_type.clone();
        tokio::task::spawn_blocking(move || {
            if matches!(ct, ContentType::Conversation | ContentType::Vine) {
                return Ok::<(), String>(()); // skip for conversations and vines
            }
            let conn = rusqlite::Connection::open(&db_path_clone).map_err(|e| e.to_string())?;
            backfill_node_ids(&conn, &slug_owned).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Spawn blocking failed: {e}"))??;
    }

    // ── Vocabulary refresh: populate canonical identity catalog from apex ──
    {
        let conn = pyramid_state.writer.lock().await;
        match wire_node_lib::pyramid::vocabulary::refresh_vocabulary(&conn, slug) {
            Ok((_, count)) => tracing::info!("Post-build: refreshed vocabulary for '{}' ({} entries)", slug, count),
            Err(e) => tracing::warn!("Post-build: vocabulary refresh failed for '{}': {}", slug, e),
        }
    }

    tracing::info!("Post-build seeding complete for slug='{}'", slug);

    // ── Conversations/Vines: create DADBEAR watch config + start loop ──
    if matches!(content_type, ContentType::Conversation | ContentType::Vine) {
        // Auto-create DADBEAR watch config for conversation source folder
        if !source_paths.is_empty() {
            let conn = pyramid_state.writer.lock().await;
            for path_str in &source_paths {
                let source_dir = std::path::Path::new(path_str);
                // Use the parent directory if source_path is a file
                let watch_dir = if source_dir.is_file() {
                    source_dir.parent().unwrap_or(source_dir).to_string_lossy().to_string()
                } else {
                    path_str.clone()
                };
                let config = wire_node_lib::pyramid::types::DadbearWatchConfig {
                    id: 0,
                    slug: slug.to_string(),
                    source_path: watch_dir.clone(),
                    content_type: format!("{:?}", content_type).to_lowercase(),
                    scan_interval_secs: 10,
                    debounce_secs: 30,
                    session_timeout_secs: 1800,
                    batch_size: 1,
                    enabled: true,
                    last_scan_at: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                };
                match wire_node_lib::pyramid::db::save_dadbear_config_with_contributions(&conn, &config) {
                    Ok(_) => tracing::info!("Post-build: DADBEAR watch config created for '{}' → '{}'", slug, watch_dir),
                    Err(e) => tracing::warn!("Post-build: DADBEAR config creation failed for '{}': {}", slug, e),
                }
            }
        }

        // Start the DADBEAR extend loop if not already running
        {
            let mut dadbear = pyramid_state.dadbear_handle.lock().await;
            if dadbear.is_none() {
                let db_path_clone = db_path.clone();
                let bus = pyramid_state.build_event_bus.clone();
                let handle = wire_node_lib::pyramid::dadbear_extend::start_dadbear_extend_loop(
                    pyramid_state.clone(), db_path_clone, bus,
                );
                *dadbear = Some(handle);
                tracing::info!("Post-build: DADBEAR extend loop started");
            }
        }

        return Ok(());
    }

    // Start stale engine + file watcher
    // Phase 3 fix pass: clone the live LlmConfig (with provider_registry +
    // credential_store) so PyramidStaleEngine carries the registry path
    // through every dispatched helper, instead of pulling raw api_key/model
    // strings that drop both runtime handles.
    // Phase 12 verifier fix: attach cache_access so stale-path helpers
    // that use make_step_ctx_from_llm_config (e.g. faq::run_faq_category_meta_pass
    // dispatched from drain_and_dispatch) reach the step cache.
    let (base_config, model) = {
        let cfg = pyramid_state.config.read().await;
        let base_id = format!("stale-{}", slug);
        let with_cache = pyramid_state.attach_cache_access(cfg.clone(), slug, &base_id);
        // walker-v3 W3a (Pattern 4): resolve the model for this
        // out-of-step-ctx bootstrap via walker_resolver reading the
        // active walker_provider_openrouter contribution. The
        // `.unwrap_or_else(config.primary_model)` legacy fallback stays
        // until W3c deletes the field; cargo-check will then surface
        // this closure for cleanup. A future phase may instead wire
        // DispatchDecision::synthetic_for_preview(..) here.
        let resolved = {
            let conn = pyramid_state.reader.lock().await;
            wire_node_lib::pyramid::walker_resolver::first_openrouter_model_from_db(&conn)
        };
        let model = resolved.unwrap_or_else(|| {
            tracing::warn!(
                event = "pattern4_no_openrouter_model",
                "walker-v3: Pattern-4 site found no walker_provider_openrouter model; stamping '<unknown>' — downstream dispatch will surface no-model-available",
            );
            "<unknown>".to_string()
        });
        (with_cache, model)
    };

    let config = {
        let conn = pyramid_state.reader.lock().await;
        wire_node_lib::pyramid::db::get_auto_update_config(&conn, slug)
            .ok_or_else(|| format!("No DADBEAR config for slug '{}'", slug))?
    };

    let defer_maintenance = {
        let cfg = pyramid_state.config.read().await;
        cfg.dispatch_policy
            .as_ref()
            .map(|p| p.build_coordination.defer_maintenance_during_build)
            .unwrap_or(false)
    };
    let mut engine = wire_node_lib::pyramid::stale_engine::PyramidStaleEngine::new(
        slug,
        config,
        &db_path,
        base_config,
        &model,
        pyramid_state.operational.as_ref().clone(),
        pyramid_state.build_event_bus.clone(),
        pyramid_state.active_build.clone(),
        defer_maintenance,
    );
    engine.start_poll_loop();

    let mut engines = pyramid_state.stale_engines.lock().await;
    // Abort old engine's poll loop to prevent orphan tasks (M5 fix)
    if let Some(old_engine) = engines.get_mut(slug) {
        old_engine.abort_poll_loop();
    }
    engines.insert(slug.to_string(), engine);
    drop(engines);

    if !source_paths.is_empty() {
        let mut watcher =
            wire_node_lib::pyramid::watcher::PyramidFileWatcher::new(slug, source_paths, &pyramid_state.operational.tier2);

        // Create mutation channel and wire it to the stale engine
        let (mutation_tx, mut mutation_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, i32)>();
        watcher.set_mutation_sender(mutation_tx);

        // Spawn receiver task to bridge watcher -> engine notifications
        let ps = pyramid_state.clone();
        tokio::spawn(async move {
            while let Some((slug, layer)) = mutation_rx.recv().await {
                let mut engines = ps.stale_engines.lock().await;
                if let Some(engine) = engines.get_mut(&slug) {
                    engine.notify_mutation(layer);
                }
            }
        });

        match watcher.start(&db_path) {
            Ok(()) => {
                tracing::info!("Post-build: file watcher started for '{}'", slug);
                let mut watchers = pyramid_state.file_watchers.lock().await;
                watchers.insert(slug.to_string(), watcher);
            }
            Err(e) => {
                tracing::warn!(
                    "Post-build: failed to start file watcher for '{}': {}",
                    slug,
                    e
                );
            }
        }
    }

    Ok(())
}

/// Hash source files in a directory and write to pyramid_file_hashes.
fn hash_source_files(
    conn: &rusqlite::Connection,
    slug: &str,
    dir: &std::path::Path,
    extensions: &[String],
    config_filenames: &[String],
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    fn walk_and_hash(
        conn: &rusqlite::Connection,
        slug: &str,
        base: &std::path::Path,
        current: &std::path::Path,
        extensions: &[String],
        config_filenames: &[String],
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(current).map_err(|e| e.to_string())?;
        let skip_dirs = [
            ".git",
            "node_modules",
            "target",
            "dist",
            "build",
            ".next",
            "__pycache__",
            ".cache",
        ];

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with('.') && path.is_dir() {
                continue;
            }

            if path.is_dir() {
                if !skip_dirs.contains(&name_str.as_ref()) {
                    walk_and_hash(conn, slug, base, &path, extensions, config_filenames)?;
                }
                continue;
            }

            let fname = name_str.to_string();
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();

            let is_code = extensions.iter().any(|e| e == &ext);
            let is_config = config_filenames.iter().any(|cf| cf == &fname);

            if !is_code && !is_config {
                continue;
            }

            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hash = hex::encode(hasher.finalize());

            let abs_path = path.to_string_lossy().to_string();
            wire_node_lib::pyramid::db::upsert_file_hash(conn, slug, &abs_path, &hash, 1, "[]")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    walk_and_hash(conn, slug, dir, dir, extensions, config_filenames)
}

/// Backfill node_ids in pyramid_file_hashes using L0 node ordering.
/// L0 node IDs are zero-padded (e.g. C-L0-003), so lexicographic ORDER BY preserves chunk order.
fn backfill_node_ids(conn: &rusqlite::Connection, slug: &str) -> Result<(), String> {
    // Get all L0 nodes in order
    let mut stmt = conn
        .prepare("SELECT id FROM live_pyramid_nodes WHERE slug = ?1 AND depth = 0 ORDER BY id ASC")
        .map_err(|e| e.to_string())?;
    let node_ids: Vec<String> = stmt
        .query_map(rusqlite::params![slug], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Get all file hashes in order (by file_path for deterministic mapping)
    let mut file_stmt = conn
        .prepare("SELECT file_path FROM pyramid_file_hashes WHERE slug = ?1 ORDER BY file_path ASC")
        .map_err(|e| e.to_string())?;
    let file_paths: Vec<String> = file_stmt
        .query_map(rusqlite::params![slug], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Map: each file gets one chunk, so chunk_index i -> node_ids[i]
    // Since 1 file = 1 chunk for code/doc pyramids, the mapping is straightforward
    for (i, file_path) in file_paths.iter().enumerate() {
        if i < node_ids.len() {
            let node_ids_json = serde_json::to_string(&[&node_ids[i]]).unwrap_or("[]".to_string());
            conn.execute(
                "UPDATE pyramid_file_hashes SET node_ids = ?1 WHERE slug = ?2 AND file_path = ?3",
                rusqlite::params![node_ids_json, slug, file_path],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    tracing::info!(
        "Backfilled node_ids for slug='{}': {} files, {} L0 nodes",
        slug,
        file_paths.len(),
        node_ids.len()
    );
    Ok(())
}

// ── Walker v3 build-starter guards (Phase 0a-2 §2.17.1) ────────────────────
//
// Plan §2.17.1: "every current starter (HTTP build routes, Tauri
// pyramid_build, question-build spawn, folder-ingestion initial-build
// spawn, DADBEAR manual trigger, stale-engine startup reconciliation,
// and any future spawn_*build* helper) must route through the same
// guard helper so boot ordering and runtime gating cannot drift apart."
//
// Phase 0a-2 instruments the Tauri IPC build-starter commands here —
// `pyramid_build`, `pyramid_question_build`, `pyramid_rebuild` — which
// are the five-or-so entry points a user can hit before boot finishes.
// Every call routes through `guard_app_ready(&state.app_mode)` and
// refuses if AppMode != Ready.
//
// TODO(walker-v3 Phase 0b): extend the guard to the rest of the
// inventory the plan calls out. These sites live in `pyramid/*.rs` and
// are out of scope for WS5 (file scope restriction):
//
//   - `folder_ingestion::spawn_initial_builds` (invoked from main.rs
//     ~L4401) — calls `question_build::spawn_question_build` for each
//     ingestion apex. The lib-side helper has no AppState reference;
//     WS5 can't plumb the guard without touching pyramid/*.rs.
//   - `public_html/routes_ask.rs` — the /ask HTTP surface spawns
//     question builds for public questions.
//   - `dadbear_supervisor::start_dadbear_supervisor` — the runtime
//     supervisor dispatches work items the compiler emits; manual
//     triggers bypass the Tauri IPC layer.
//   - `server::init_stale_engines` — stale-engine startup
//     reconciliation ticks rebuild tasks 3s after boot. The 3s sleep
//     happens to cover the typical boot window in practice, but the
//     guard should be explicit so quarantine state can refuse stale
//     rebuilds.
//
// Each of those will get `guard_app_ready(&state.app_mode).await?`
// added at its entry in Phase 0b, when the WS plan covers pyramid/*.rs
// edits. Until then, they rely on the implicit ordering: the boot
// coordinator spawns BEFORE `init_stale_engines`'s 3s delay elapses,
// and BEFORE the HTTP server accepts requests (see
// `server::start_server` call site), so the practical window in which
// a build-starter could run against `Booting` state is <100ms in
// happy-path boot.
#[tauri::command]
async fn pyramid_build(
    state: tauri::State<'_, SharedState>,
    slug: String,
    from_depth: Option<i64>,
    stop_after: Option<String>,
    force_from: Option<String>,
) -> Result<BuildStatus, String> {
    // Walker v3 §2.17.1: every build-starter routes through the Ready guard.
    wire_node_lib::guard_app_ready(&state.app_mode)
        .await
        .map_err(|e| e.to_string())?;

    // Verify slug exists
    {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?;
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    let status = Arc::new(tokio::sync::RwLock::new(BuildStatus {
        slug: slug.clone(),
        status: "running".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
        failures: 0,
        steps: vec![],
    }));

    // Use write lock for atomic check-and-set (prevents TOCTOU race where two
    // rapid build requests both pass the "is already running" check).
    let layer_state_for_build = {
        let mut active = state.pyramid.active_build.write().await;
        if let Some(handle) = active.get(&slug) {
            let s = handle.status.read().await;
            let is_terminal = s.is_terminal();
            drop(s);
            if !handle.cancel.is_cancelled() && !is_terminal {
                return Err("Build already running for this slug".to_string());
            }
        }

        let layer_state = std::sync::Arc::new(tokio::sync::RwLock::new(
            wire_node_lib::pyramid::types::BuildLayerState::default(),
        ));
        let layer_state_for_build = layer_state.clone();
        let handle = wire_node_lib::pyramid::BuildHandle {
            slug: slug.clone(),
            cancel: cancel.clone(),
            status: status.clone(),
            layer_state,
            started_at: std::time::Instant::now(),
        };
        active.insert(slug.clone(), handle);
        layer_state_for_build
    };

    let writer = state.pyramid.writer.clone();
    let build_status = status.clone();
    // Create a build-scoped PyramidState with its own reader connection so the
    // build doesn't compete with CLI/frontend queries for the shared reader Mutex.
    let pyramid_state = state
        .pyramid
        .with_build_reader()
        .map_err(|e| format!("Failed to create build reader: {e}"))?;

    let build_task_handle = tokio::spawn(async move {
        let start = std::time::Instant::now();

        // Create mpsc channel for WriteOps (used by legacy build path)
        let (write_tx, mut write_rx) =
            tokio::sync::mpsc::channel::<wire_node_lib::pyramid::build::WriteOp>(256);

        // Spawn the writer task
        let writer_handle = {
            let writer_conn = writer.clone();
            tokio::spawn(async move {
                while let Some(op) = write_rx.recv().await {
                    let result = {
                        let conn = writer_conn.lock().await;
                        match op {
                            wire_node_lib::pyramid::build::WriteOp::SaveNode {
                                ref node,
                                ref topics_json,
                            } => wire_node_lib::pyramid::db::save_node(
                                &conn,
                                node,
                                topics_json.as_deref(),
                            ),
                            wire_node_lib::pyramid::build::WriteOp::SaveStep {
                                ref slug,
                                ref step_type,
                                chunk_index,
                                depth,
                                ref node_id,
                                ref output_json,
                                ref model,
                                elapsed,
                            } => wire_node_lib::pyramid::db::save_step(
                                &conn,
                                slug,
                                step_type,
                                chunk_index,
                                depth,
                                node_id,
                                output_json,
                                model,
                                elapsed,
                            ),
                            wire_node_lib::pyramid::build::WriteOp::UpdateParent {
                                ref slug,
                                ref node_id,
                                ref parent_id,
                            } => wire_node_lib::pyramid::db::update_parent(
                                &conn, slug, node_id, parent_id,
                            ),
                            wire_node_lib::pyramid::build::WriteOp::UpdateStats { ref slug } => {
                                wire_node_lib::pyramid::db::update_slug_stats(&conn, slug)
                            }
                            wire_node_lib::pyramid::build::WriteOp::UpdateFileHash { ref slug, ref file_path, ref node_id } => {
                                wire_node_lib::pyramid::db::append_node_id_to_file_hash(&conn, slug, file_path, node_id)
                            }
                            wire_node_lib::pyramid::build::WriteOp::Flush { done } => {
                                let _ = done.send(());
                                Ok(())
                            }
                        }
                    };
                    if let Err(e) = result {
                        tracing::error!("WriteOp failed: {e}");
                    }
                }
            })
        };

        // Create progress channel — tee'd onto build_event_bus so the public
        // web surface can subscribe per-slug while the desktop UI continues to
        // drain `progress_rx` exactly as before.
        let (progress_tx, raw_progress_rx) =
            tokio::sync::mpsc::channel::<BuildProgress>(64);
        let mut progress_rx = wire_node_lib::pyramid::event_bus::tee_build_progress_to_bus(
            &pyramid_state.build_event_bus,
            slug.clone(),
            raw_progress_rx,
        );
        let progress_status = build_status.clone();
        let progress_start = start;
        let progress_handle = tokio::spawn(async move {
            while let Some(prog) = progress_rx.recv().await {
                let mut s = progress_status.write().await;
                s.progress = prog;
                s.elapsed_seconds = progress_start.elapsed().as_secs_f64();
            }
        });

        // Create layer event channel for build visualization v2
        let (layer_tx, mut layer_rx) =
            tokio::sync::mpsc::channel::<wire_node_lib::pyramid::types::LayerEvent>(256);
        let layer_drain_state = layer_state_for_build;
        let layer_drain_handle = tokio::spawn(async move {
            use wire_node_lib::pyramid::types::{LayerEvent, LayerProgress, LogEntry, NodeStatus};
            while let Some(event) = layer_rx.recv().await {
                let mut state = layer_drain_state.write().await;
                match event {
                    LayerEvent::Discovered { depth, step_name, estimated_nodes } => {
                        state.layers.push(LayerProgress {
                            depth, step_name, estimated_nodes,
                            completed_nodes: 0, failed_nodes: 0,
                            status: "pending".into(),
                            nodes: if estimated_nodes <= 50 { Some(Vec::new()) } else { None },
                        });
                    }
                    LayerEvent::NodeCompleted { depth, step_name, node_id, label } => {
                        if let Some(layer) = state.layers.iter_mut().find(|l| l.depth == depth && l.step_name == step_name) {
                            layer.completed_nodes += 1;
                            layer.status = "active".into();
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus { node_id, status: "complete".into(), label });
                            }
                        }
                    }
                    LayerEvent::NodeFailed { depth, step_name, node_id } => {
                        if let Some(layer) = state.layers.iter_mut().find(|l| l.depth == depth && l.step_name == step_name) {
                            layer.failed_nodes += 1;
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus { node_id, status: "failed".into(), label: None });
                            }
                        }
                    }
                    LayerEvent::LayerCompleted { depth, step_name } => {
                        if let Some(layer) = state.layers.iter_mut().find(|l| l.depth == depth && l.step_name == step_name) {
                            layer.status = "complete".into();
                        }
                    }
                    LayerEvent::NodeStarted { depth, step_name, node_id, .. } => {
                        if let Some(layer) = state.layers.iter_mut().find(|l| l.depth == depth && l.step_name == step_name) {
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus { node_id, status: "pending".into(), label: None });
                            }
                        }
                    }
                    LayerEvent::StepStarted { step_name } => {
                        state.current_step = Some(step_name);
                    }
                    LayerEvent::Log { elapsed_secs, message } => {
                        state.log.push_back(LogEntry { elapsed_secs, message });
                        if state.log.len() > 200 { state.log.pop_front(); }
                    }
                }
            }
        });

        // Unified build dispatch — chain engine or legacy based on feature flag
        let result = wire_node_lib::pyramid::build_runner::run_build_from(
            &pyramid_state,
            &slug,
            from_depth.unwrap_or(0),
            stop_after.as_deref(),
            force_from.as_deref(),
            &cancel,
            Some(progress_tx.clone()),
            &write_tx,
            Some(layer_tx.clone()),
        )
        .await;

        // Read content_type for post-build seeding (before dropping channels)
        let content_type = {
            let conn = pyramid_state.reader.lock().await;
            wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
                .ok()
                .flatten()
                .map(|info| info.content_type)
        };

        // Drop senders so tasks finish
        drop(write_tx);
        drop(progress_tx);
        drop(layer_tx);
        let _ = writer_handle.await;
        let _ = progress_handle.await;
        let _ = layer_drain_handle.await;

        // Update final status
        {
            let mut s = build_status.write().await;
            if cancel.is_cancelled() {
                s.status = "cancelled".to_string();
            } else {
                match result {
                    Ok((_apex_id, failures, activities)) => {
                        s.failures = failures;
                        s.steps = activities;
                        if failures > 0 {
                            s.status = "complete_with_errors".to_string();
                            tracing::warn!(
                                "Build completed for '{}' with {failures} node failure(s)",
                                slug
                            );
                        } else {
                            s.status = "complete".to_string();
                        }
                        s.progress = BuildProgress {
                            done: s.progress.total,
                            total: s.progress.total,
                        };
                    }
                    Err(ref e) => {
                        s.status = "failed".to_string();
                        s.progress = BuildProgress {
                            done: s.progress.total,
                            total: s.progress.total,
                        };
                        tracing::error!("Build failed for '{}': {e}", slug);
                    }
                }
            }
            s.elapsed_seconds = start.elapsed().as_secs_f64();
        }

        // ── Post-build seeding: auto_update_config, file_hashes, engine + watcher ──
        // Only seed if build succeeded (not cancelled, not failed)
        {
            let status_check = build_status.read().await;
            let should_seed = matches!(
                status_check.status.as_str(),
                "complete" | "complete_with_errors"
            );
            drop(status_check);

            if should_seed {
                if let Some(ref ct) = content_type {
                    if let Err(e) = post_build_seed(&pyramid_state, &slug, ct).await {
                        tracing::error!("Post-build seeding failed for '{}': {}", slug, e);
                    }
                }
            }
        }
    });

    // Monitor: catch panics in the build task and set status to "failed"
    let monitor_status = status.clone();
    tokio::spawn(async move {
        if let Err(e) = build_task_handle.await {
            tracing::error!("pyramid_build task panicked: {e:?}");
            let mut s = monitor_status.write().await;
            if s.status == "running" {
                s.status = "failed".to_string();
            }
        }
    });

    let s = status.read().await;
    Ok(s.clone())
}

#[tauri::command]
async fn pyramid_build_status(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<BuildStatus, String> {
    let active = state.pyramid.active_build.read().await;
    if let Some(handle) = active.get(&slug) {
        let mut s = handle.status.read().await.clone();
        // Compute elapsed live instead of using the cached (possibly stale) value
        if s.status == "running" {
            s.elapsed_seconds = handle.started_at.elapsed().as_secs_f64();
        }
        return Ok(s);
    }

    Ok(BuildStatus {
        slug,
        status: "idle".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
        failures: 0,
        steps: vec![],
    })
}

#[tauri::command]
async fn pyramid_build_progress_v2(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<wire_node_lib::pyramid::types::BuildProgressV2, String> {
    let active = state.pyramid.active_build.read().await;
    if let Some(handle) = active.get(&slug) {
        let status = handle.status.read().await;
        let layer_state = handle.layer_state.read().await;
        Ok(wire_node_lib::pyramid::types::BuildProgressV2 {
            done: status.progress.done,
            total: status.progress.total,
            layers: layer_state.layers.clone(),
            current_step: layer_state.current_step.clone(),
            log: layer_state.log.iter().cloned().collect(),
        })
    } else {
        Ok(wire_node_lib::pyramid::types::BuildProgressV2 {
            done: 0,
            total: 0,
            layers: vec![],
            current_step: None,
            log: vec![],
        })
    }
}

#[tauri::command]
async fn pyramid_ingest(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Look up slug info
    let slug_info = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?
    };

    let source_path = slug_info.source_path.clone();
    let content_type = slug_info.content_type.clone();
    let slug_clone = slug.clone();
    let writer = state.pyramid.writer.clone();

    // Parse source_path as JSON array, falling back to single-path for backward compat
    let paths = wire_node_lib::pyramid::slug::resolve_validated_source_paths(
        &source_path,
        &content_type,
        state.pyramid.data_dir.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    tokio::task::spawn_blocking(move || {
        let conn = writer.blocking_lock();
        for path in &paths {
            match content_type {
                ContentType::Code => {
                    let _ = wire_node_lib::pyramid::ingest::ingest_code(&conn, &slug_clone, path)
                        .map_err(|e| e.to_string())?;
                }
                ContentType::Conversation => {
                    wire_node_lib::pyramid::ingest::ingest_conversation(&conn, &slug_clone, path)
                        .map_err(|e| e.to_string())?;
                }
                ContentType::Document => {
                    let _ = wire_node_lib::pyramid::ingest::ingest_docs(&conn, &slug_clone, path)
                        .map_err(|e| e.to_string())?;
                }
                ContentType::Vine => {
                    return Err("Use vine-specific build endpoint for vine ingestion".to_string());
                }
                ContentType::Question => {
                    return Err("Question pyramids do not support direct ingestion".to_string());
                }
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("Ingest task panicked: {e}"))?
    .map_err(|e| e.to_string())?;

    // Count chunks
    let conn = state.pyramid.reader.lock().await;
    let chunk_count = pyramid_db::count_chunks(&conn, &slug).unwrap_or(0);

    Ok(serde_json::json!({
        "slug": slug,
        "chunks": chunk_count,
        "status": "ingested"
    }))
}

// walker-v3 W3a (Cluster 1 / Option A — stub model writes): `pyramid_set_config`
// is retained as a credentials + flags write path (api_key / auth_token /
// use_ir_executor / auto_execute) because frontend PyramidFirstRun and
// PyramidSettings still ride it. Model-slug writes (primary_model /
// fallback_model_{1,2}) are now a hard error directing operators to edit
// the walker_provider_openrouter contribution via Tools > Create. Phase 6
// retires the whole IPC once the Settings UI rewrites to edit walker_*
// contributions directly.
// TODO(walker-v3 Phase 6): delete pyramid_set_config entirely — the
// api_key / auth_token writes move to a dedicated credential-only IPC,
// and auto_execute / use_ir_executor move to per-feature toggle IPCs.
#[tauri::command]
async fn pyramid_set_config(
    state: tauri::State<'_, SharedState>,
    api_key: Option<String>,
    auth_token: Option<String>,
    primary_model: Option<String>,
    fallback_model_1: Option<String>,
    fallback_model_2: Option<String>,
    use_ir_executor: Option<bool>,
    auto_execute: Option<bool>,
) -> Result<(), String> {
    // walker-v3 W3a: model-slug writes are no longer accepted — models
    // are resolved from the active walker_provider_openrouter
    // contribution at dispatch time. Surface a directed error so the
    // operator knows where to edit instead.
    if primary_model.is_some() || fallback_model_1.is_some() || fallback_model_2.is_some() {
        return Err(
            "Model selection moved to the walker_provider_openrouter contribution. \
             Edit it via Tools > Create (schema: walker_provider_openrouter) or \
             POST /config-contributions; this legacy field write was retired in \
             walker-v3 W3a and will be deleted in Phase 6."
                .to_string(),
        );
    }
    // Update in-memory LLM config (credentials + flags only now).
    {
        let mut config = state.pyramid.config.write().await;
        if let Some(ref key) = api_key {
            config.api_key = key.clone();
        }
        if let Some(ref token) = auth_token {
            config.auth_token = token.clone();
        }
    }

    // auto_execute is stored in the PyramidConfig file, not in-memory LlmConfig
    if let Some(ae) = auto_execute {
        if let Some(ref data_dir) = state.pyramid.data_dir {
            let config_path = data_dir.join("pyramid_config.json");
            if let Ok(contents) = std::fs::read_to_string(&config_path) {
                if let Ok(mut pconfig) = serde_json::from_str::<serde_json::Value>(&contents) {
                    pconfig["auto_execute"] = serde_json::Value::Bool(ae);
                    let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&pconfig).unwrap_or_default());
                }
            }
        }
    }

    if let Some(use_ir) = use_ir_executor {
        state
            .pyramid
            .use_ir_executor
            .store(use_ir, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("IR executor toggled to: {use_ir}");
    }

    // Write API key to credential store (single source of truth).
    // The in-memory LlmConfig.api_key was already updated above as a
    // read-through cache for cold-path guards.
    if let Some(ref key) = api_key {
        if !key.is_empty() {
            if let Err(e) = state.pyramid.credential_store.set("OPENROUTER_KEY", key) {
                tracing::warn!("Failed to write API key to credential store: {e}");
            }
        }
    }

    // Keep partner module's api_key in sync so Dennis uses the new key
    // without requiring an app restart.
    if let Some(ref key) = api_key {
        if !key.is_empty() {
            let mut partner_config = state.partner.llm_config.write().await;
            partner_config.api_key = key.clone();
        }
    }

    // Persist non-secret config to disk. Deliberately not updating
    // openrouter_api_key — credential store is SOT. Stale value
    // preserved as boot-time migration source.
    //
    // W3c: PyramidConfig no longer carries primary_model /
    // fallback_model_{1,2}. Model selection lives in
    // walker_provider_openrouter. We still write the other non-secret
    // fields through.
    if let Some(ref data_dir) = state.pyramid.data_dir {
        let mut pyramid_config = wire_node_lib::pyramid::PyramidConfig::load(data_dir);
        let config = state.pyramid.config.read().await;
        pyramid_config.auth_token = config.auth_token.clone();
        pyramid_config.use_ir_executor = state
            .pyramid
            .use_ir_executor
            .load(std::sync::atomic::Ordering::Relaxed);
        pyramid_config.save(data_dir).map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[tauri::command]
async fn pyramid_create_slug(
    state: tauri::State<'_, SharedState>,
    slug: String,
    content_type: String,
    source_path: String,
    referenced_slugs: Option<Vec<String>>,
) -> Result<SlugInfo, String> {
    let ct = ContentType::from_str(&content_type).ok_or_else(|| {
        format!(
            "Invalid content_type: '{}'. Use 'code', 'document', 'conversation', or 'vine'",
            content_type
        )
    })?;
    let normalized_source_path = wire_node_lib::pyramid::slug::normalize_and_validate_source_path(
        &source_path,
        &ct,
        state.pyramid.data_dir.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    {
        let conn = state.pyramid.writer.lock().await;
        let info =
            wire_node_lib::pyramid::slug::create_slug(&conn, &slug, &ct, &normalized_source_path)
                .map_err(|e| e.to_string())?;

        if let Some(refs) = &referenced_slugs {
            if !refs.is_empty() {
                use wire_node_lib::pyramid::db as pyramid_db;
                if let Err(e) = pyramid_db::save_slug_references(&conn, &info.slug, refs) {
                    tracing::warn!(slug = %info.slug, error = %e, "failed to save slug references");
                }
            }
        }
        drop(conn);

        if let Err(e) = ensure_dadbear_running(&*state, &info.slug).await {
            tracing::warn!(slug = %info.slug, error = %e, "ensure_dadbear_running failed after create_slug");
        }

        Ok(info)
    }
}

// ── Phase 17: Recursive folder ingestion IPCs ───────────────────────────────

#[derive(serde::Deserialize)]
struct IngestFolderInput {
    target_folder: String,
    include_claude_code: bool,
    dry_run: bool,
    /// Optional override for the conversation scan root. When `None`,
    /// the active `folder_ingestion_heuristics` contribution's
    /// `claude_code_conversation_path` is used (seed default:
    /// `~/.claude/projects`). When `Some`, the provided path replaces
    /// it for this ingest only — the contribution is NOT modified.
    /// Lets a user point at Cursor's conversation cache, a backup,
    /// or any other directory that follows the same encoded-subdir
    /// naming convention.
    #[serde(default)]
    conversation_path_override: Option<String>,
}

#[derive(serde::Serialize)]
struct IngestFolderOutput {
    plan: wire_node_lib::pyramid::folder_ingestion::IngestionPlan,
    result: Option<wire_node_lib::pyramid::folder_ingestion::IngestionResult>,
}

/// Phase 17: recursive folder ingestion entry point.
///
/// `dry_run = true` returns the planned operations without executing
/// them. `dry_run = false` executes the plan and returns both the
/// plan and the execution result.
#[tauri::command]
async fn pyramid_ingest_folder(
    state: tauri::State<'_, SharedState>,
    input: IngestFolderInput,
) -> Result<IngestFolderOutput, String> {
    use wire_node_lib::pyramid::folder_ingestion;

    let target = std::path::PathBuf::from(&input.target_folder);
    if !target.exists() {
        return Err(format!("target folder does not exist: {}", input.target_folder));
    }

    // Load the active folder_ingestion_heuristics contribution (or
    // fall back to bundled defaults if no contribution is synced).
    let mut config = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::db::load_active_folder_ingestion_heuristics(&conn)
            .map_err(|e| format!("load_active_folder_ingestion_heuristics: {}", e))?
    };

    // Apply per-call conversation-path override without mutating the
    // persisted contribution. Trimmed-empty values are ignored so the
    // frontend can send empty strings safely.
    if let Some(override_path) = input.conversation_path_override.as_ref() {
        let trimmed = override_path.trim();
        if !trimmed.is_empty() {
            config.claude_code_conversation_path = trimmed.to_string();
        }
    }

    let plan = folder_ingestion::plan_ingestion(&target, &config, input.include_claude_code)
        .map_err(|e| format!("plan_ingestion: {}", e))?;

    if input.dry_run {
        return Ok(IngestFolderOutput { plan, result: None });
    }

    let result = folder_ingestion::execute_plan(&state.pyramid, plan.clone())
        .await
        .map_err(|e| format!("execute_plan: {}", e))?;

    // Phase 17 wanderer fix: trigger first builds for every slug the plan
    // just created. Without this, code/document pyramids sit idle forever
    // because Pipeline B's `fire_ingest_chain` rejects those content types,
    // and topical vines never get a first build because
    // `notify_vine_of_child_completion` only re-enqueues work against
    // EXISTING vine apex nodes. See `folder_ingestion::spawn_initial_builds`
    // for the full reasoning. The dispatch runs in a background task so
    // the IPC returns immediately after DB writes land.
    if result.errors.is_empty() || !result.pyramids_created.is_empty() || !result.vines_created.is_empty() {
        folder_ingestion::spawn_initial_builds(&state.pyramid, &plan);
    }

    Ok(IngestFolderOutput {
        plan,
        result: Some(result),
    })
}

/// Phase 17: pre-flight IPC for the AddWorkspace wizard. Returns the
/// list of `~/.claude/projects/` directories whose encoded path
/// matches the target folder or any of its subfolders. Used by the
/// UI to populate the "Include Claude Code conversations" checkbox
/// list before the dry-run plan.
#[tauri::command]
async fn pyramid_find_claude_code_conversations(
    state: tauri::State<'_, SharedState>,
    target_folder: String,
    conversation_path_override: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::folder_ingestion::ClaudeCodeConversationDir>, String> {
    use wire_node_lib::pyramid::folder_ingestion;

    let target = std::path::PathBuf::from(&target_folder);
    if !target.exists() {
        return Err(format!("target folder does not exist: {}", target_folder));
    }

    let mut config = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::db::load_active_folder_ingestion_heuristics(&conn)
            .map_err(|e| format!("load_active_folder_ingestion_heuristics: {}", e))?
    };

    // Same per-call override as `pyramid_ingest_folder`. Empty/whitespace
    // strings fall through to the persisted setting.
    if let Some(override_path) = conversation_path_override.as_ref() {
        let trimmed = override_path.trim();
        if !trimmed.is_empty() {
            config.claude_code_conversation_path = trimmed.to_string();
        }
    }

    Ok(folder_ingestion::describe_claude_code_dirs(&target, &config))
}

#[tauri::command]
async fn pyramid_delete_slug(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<(), String> {
    let maybe_handle = {
        let active = state.pyramid.active_build.read().await;
        active
            .get(&slug)
            .map(|handle| (handle.cancel.clone(), handle.status.clone()))
    };

    if let Some((cancel, status)) = maybe_handle {
        let s = status.read().await;
        if s.is_running() && !cancel.is_cancelled() {
            return Err("Cannot delete slug while build is running".to_string());
        }
    }

    let conn = state.pyramid.writer.lock().await;
    let result =
        wire_node_lib::pyramid::slug::archive_slug(&conn, &slug, Some(&state.pyramid.build_event_bus)).map_err(|e| e.to_string());
    drop(conn);

    result
}

// --- S1: IPC-only mutation commands (moved from HTTP) -------------------------

/// IPC equivalent of POST /auth/complete — updates auth state from the frontend.
/// The HTTP endpoint is retained only for the magic-link callback browser page.
#[tauri::command]
async fn auth_complete_ipc(
    state: tauri::State<'_, SharedState>,
    access_token: String,
    refresh_token: Option<String>,
    user_id: Option<String>,
    email: Option<String>,
) -> Result<(), String> {
    tracing::info!("Auth complete via IPC - user_id={:?}", user_id);

    let mut auth = state.auth.write().await;
    auth.access_token = Some(access_token);
    auth.refresh_token = refresh_token;
    auth.user_id = user_id;
    auth.email = email;
    // Preserve api_token and node_id from previous registration

    tracing::info!("Auth state updated via IPC");
    Ok(())
}

/// IPC equivalent of DELETE /pyramid/:slug/purge — CASCADE DELETE of a slug.
#[tauri::command]
async fn pyramid_purge_slug(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<(), String> {
    // Don't allow purging a slug with an active build
    let maybe_handle = {
        let active = state.pyramid.active_build.read().await;
        active
            .get(&slug)
            .map(|handle| (handle.cancel.clone(), handle.status.clone()))
    };

    if let Some((cancel, status)) = maybe_handle {
        let s = status.read().await;
        if s.is_running() && !cancel.is_cancelled() {
            return Err("Cannot purge slug while build is running".to_string());
        }
    }

    let conn = state.pyramid.writer.lock().await;
    let result = wire_node_lib::pyramid::slug::purge_slug(&conn, &slug).map_err(|e| e.to_string());
    drop(conn);

    if result.is_ok() {
        let mut active = state.pyramid.active_build.write().await;
        active.remove(&slug);
    }

    result
}

/// IPC equivalent of POST /pyramid/:slug/archive — archive a slug (state mutation).
#[tauri::command]
async fn pyramid_archive_slug(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<(), String> {
    // Don't allow archiving a slug with an active build
    let maybe_handle = {
        let active = state.pyramid.active_build.read().await;
        active
            .get(&slug)
            .map(|handle| (handle.cancel.clone(), handle.status.clone()))
    };

    if let Some((cancel, status)) = maybe_handle {
        let s = status.read().await;
        if s.is_running() && !cancel.is_cancelled() {
            return Err("Cannot archive slug while build is running".to_string());
        }
    }

    let conn = state.pyramid.writer.lock().await;
    let result =
        wire_node_lib::pyramid::slug::archive_slug(&conn, &slug, Some(&state.pyramid.build_event_bus)).map_err(|e| e.to_string());
    drop(conn);

    result
}

/// Set the access tier for a pyramid slug (WS-ONLINE-E).
///
/// Mutations are IPC-only (S1 security model). Sets the access_tier, optional
/// price override, and optional allowed_circles JSON array for a slug.
#[tauri::command]
async fn pyramid_set_access_tier(
    state: tauri::State<'_, SharedState>,
    slug: String,
    tier: String,
    price: Option<i64>,
    circles: Option<String>,
) -> Result<(), String> {
    // Validate tier value
    match tier.as_str() {
        "public" | "circle-scoped" | "priced" | "embargoed" => {}
        _ => {
            return Err(format!(
            "Invalid access tier '{}'. Must be one of: public, circle-scoped, priced, embargoed",
            tier
        ))
        }
    }

    // Validate circles JSON if provided
    if let Some(ref c) = circles {
        let _: Vec<String> = serde_json::from_str(c)
            .map_err(|e| format!("Invalid circles JSON (must be array of strings): {}", e))?;
    }

    let conn = state.pyramid.writer.lock().await;
    pyramid_db::set_access_tier(&conn, &slug, &tier, price, circles.as_deref())
        .map_err(|e| e.to_string())?;

    tracing::info!(
        slug = %slug,
        tier = %tier,
        price = ?price,
        circles = ?circles,
        "Access tier updated via IPC"
    );

    Ok(())
}

/// Set the absorption mode for a pyramid slug (WS-ONLINE-G).
///
/// Mutations are IPC-only (S1 security model). Sets the absorption mode and optional
/// chain ID, plus rate limit and daily spend cap for absorb-all mode.
#[tauri::command]
async fn pyramid_set_absorption_mode(
    state: tauri::State<'_, SharedState>,
    slug: String,
    mode: String,
    chain_id: Option<String>,
    rate_limit: Option<u32>,
    daily_cap: Option<u64>,
) -> Result<(), String> {
    // Validate mode value
    match mode.as_str() {
        "open" | "absorb-all" | "absorb-selective" => {}
        _ => {
            return Err(format!(
                "Invalid absorption mode '{}'. Must be one of: open, absorb-all, absorb-selective",
                mode
            ))
        }
    }

    // For absorb-selective, chain_id is required
    if mode == "absorb-selective" && chain_id.is_none() {
        return Err("absorb-selective mode requires a chain_id".to_string());
    }

    // Save absorption mode to DB
    let conn = state.pyramid.writer.lock().await;
    pyramid_db::set_absorption_mode(&conn, &slug, &mode, chain_id.as_deref())
        .map_err(|e| e.to_string())?;
    drop(conn);

    // Save rate limit config to pyramid_config.json if provided
    if rate_limit.is_some() || daily_cap.is_some() {
        let data_dir = state
            .pyramid
            .data_dir
            .as_ref()
            .ok_or_else(|| "data_dir not set".to_string())?;
        let mut cfg = wire_node_lib::pyramid::PyramidConfig::load(data_dir);
        if let Some(rl) = rate_limit {
            cfg.absorption_rate_limit_per_operator = rl;
        }
        if let Some(dc) = daily_cap {
            cfg.absorption_daily_spend_cap = dc;
        }
        cfg.save(data_dir).map_err(|e| e.to_string())?;
    }

    tracing::info!(
        slug = %slug,
        mode = %mode,
        chain_id = ?chain_id,
        rate_limit = ?rate_limit,
        daily_cap = ?daily_cap,
        "Absorption mode updated via IPC"
    );

    Ok(())
}

/// Get the absorption config for a pyramid slug (WS-ONLINE-G).
#[tauri::command]
async fn pyramid_get_absorption_config(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let (mode, chain_id) =
        pyramid_db::get_absorption_mode(&conn, &slug).map_err(|e| e.to_string())?;

    let (rate_limit, daily_cap) = if let Some(ref data_dir) = state.pyramid.data_dir {
        let cfg = wire_node_lib::pyramid::PyramidConfig::load(data_dir);
        (
            cfg.absorption_rate_limit_per_operator,
            cfg.absorption_daily_spend_cap,
        )
    } else {
        (3u32, 100u64)
    };

    Ok(serde_json::json!({
        "mode": mode,
        "chain_id": chain_id,
        "rate_limit_per_operator": rate_limit,
        "daily_spend_cap": daily_cap,
    }))
}

/// Get the access tier and cached emergent price for a pyramid slug (WS-ONLINE-E).
#[tauri::command]
async fn pyramid_get_access_tier(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let (tier, price, circles) =
        pyramid_db::get_access_tier(&conn, &slug).map_err(|e| e.to_string())?;
    let emergent_price =
        pyramid_db::get_cached_emergent_price(&conn, &slug).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "access_tier": tier,
        "access_price": price,
        "allowed_circles": circles.and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok()),
        "cached_emergent_price": emergent_price,
    }))
}

/// IPC equivalent of POST /pyramid/:slug/build/question — decomposed question build.
#[tauri::command]
async fn pyramid_question_build(
    state: tauri::State<'_, SharedState>,
    slug: String,
    question: String,
    granularity: Option<u32>,
    max_depth: Option<u32>,
    from_depth: Option<i64>,
    characterization: Option<wire_node_lib::pyramid::types::CharacterizationResult>,
) -> Result<serde_json::Value, String> {
    // Walker v3 §2.17.1: every build-starter routes through the Ready guard.
    wire_node_lib::guard_app_ready(&state.app_mode)
        .await
        .map_err(|e| e.to_string())?;

    pyramid_question_build_inner(
        &state,
        slug,
        question,
        granularity.unwrap_or(3),
        max_depth.unwrap_or(3),
        from_depth.unwrap_or(0),
        characterization,
    )
    .await
}

/// Shared inner implementation for question builds — callable from both
/// the Tauri IPC command and the rebuild path. Thin wrapper around the
/// lib-side `pyramid::question_build::spawn_question_build`.
async fn pyramid_question_build_inner(
    state: &SharedState,
    slug: String,
    question: String,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
    characterization: Option<wire_node_lib::pyramid::types::CharacterizationResult>,
) -> Result<serde_json::Value, String> {
    let (json, _completion_rx) = wire_node_lib::pyramid::question_build::spawn_question_build(
        &state.pyramid,
        slug,
        question,
        granularity,
        max_depth,
        from_depth,
        characterization,
    )
    .await?;
    Ok(json)
}

/// Rebuild a pyramid using the question from its last build.
/// This is the sole rebuild path — all pyramids are question pyramids.
#[tauri::command]
async fn pyramid_rebuild(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Walker v3 §2.17.1: every build-starter routes through the Ready guard.
    wire_node_lib::guard_app_ready(&state.app_mode)
        .await
        .map_err(|e| e.to_string())?;

    // Look up the question from the last build record, or fall back to
    // the default apex question for the slug's content type. This makes
    // rebuild work for pre-existing pyramids that were built before the
    // question pipeline was introduced.
    let question = {
        let conn = state.pyramid.reader.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT question, build_id FROM pyramid_builds \
                 WHERE slug = ?1 AND question IS NOT NULL AND question != '' \
                 ORDER BY rowid DESC LIMIT 1",
            )
            .map_err(|e| format!("Failed to query build history: {e}"))?;
        let result: Result<(String, String), _> = stmt.query_row(
            rusqlite::params![&slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((q, _)) => q,
            Err(_) => {
                // No previous question build — derive default from content type
                let slug_info = wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
                    .map_err(|e| format!("Failed to look up slug: {e}"))?
                    .ok_or_else(|| format!("Slug '{}' not found", slug))?;
                let default_q = match slug_info.content_type {
                    wire_node_lib::pyramid::types::ContentType::Code => {
                        "What are the key systems, patterns, and architecture of this codebase?"
                    }
                    wire_node_lib::pyramid::types::ContentType::Document => {
                        "What are the key concepts, decisions, and relationships in these documents?"
                    }
                    wire_node_lib::pyramid::types::ContentType::Conversation => {
                        "What happened during this conversation? What was discussed, \
                         what decisions were made, how did the discussion evolve, \
                         and what are the key takeaways?"
                    }
                    wire_node_lib::pyramid::types::ContentType::Vine => {
                        "What are the key themes and structure across the children of this folder collection?"
                    }
                    wire_node_lib::pyramid::types::ContentType::Question => {
                        "What are the most important answers this material can provide?"
                    }
                };
                tracing::info!(
                    slug = %slug,
                    content_type = ?slug_info.content_type,
                    "pyramid_rebuild: no prior question build, using default apex question"
                );
                default_q.to_string()
            }
        }
    };

    tracing::info!(
        slug = %slug,
        question = %question,
        "pyramid_rebuild: re-triggering question build from stored params"
    );

    pyramid_question_build_inner(
        &state,
        slug,
        question,
        3,  // granularity default
        3,  // max_depth default
        0,  // from_depth default
        None, // characterization: auto
    )
    .await
}

/// IPC equivalent of POST /pyramid/:slug/build/preview — preview decomposition.
#[tauri::command]
async fn pyramid_question_preview(
    state: tauri::State<'_, SharedState>,
    slug: String,
    question: String,
    granularity: Option<u32>,
    max_depth: Option<u32>,
) -> Result<serde_json::Value, String> {
    let granularity = granularity.unwrap_or(3);
    let max_depth = max_depth.unwrap_or(3);

    if question.trim().is_empty() {
        return Err("question cannot be empty".to_string());
    }

    // Validate slug exists
    {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?;
    }

    match wire_node_lib::pyramid::build_runner::preview_decomposed_build(
        &state.pyramid,
        &slug,
        &question,
        granularity,
        max_depth,
    )
    .await
    {
        Ok((tree, preview)) => Ok(serde_json::json!({
            "slug": slug,
            "question": question,
            "preview": preview,
            "question_tree": tree,
        })),
        Err(e) => Err(format!("Preview failed: {}", e)),
    }
}

/// IPC equivalent of POST /pyramid/:slug/characterize — characterize source material.
#[tauri::command]
async fn pyramid_characterize(
    state: tauri::State<'_, SharedState>,
    slug: String,
    question: String,
    source_path: Option<String>,
) -> Result<serde_json::Value, String> {
    if question.trim().is_empty() {
        return Err("question cannot be empty".to_string());
    }

    // Validate slug exists and get source_path
    let resolved_source_path = {
        let conn = state.pyramid.reader.lock().await;
        match wire_node_lib::pyramid::slug::get_slug(&conn, &slug) {
            Ok(Some(s)) => source_path.unwrap_or(s.source_path),
            Ok(None) => return Err(format!("Slug '{}' not found", slug)),
            Err(e) => return Err(e.to_string()),
        }
    };

    // Phase 12 verifier fix: attach cache_access so characterize retrofit
    // reaches the step cache.
    let llm_config = state
        .pyramid
        .llm_config_with_cache(&slug, &format!("characterize-{}", slug))
        .await;

    match wire_node_lib::pyramid::characterize::characterize(
        &resolved_source_path,
        &question,
        &llm_config,
        &state.pyramid.operational.tier1,
        Some(&state.pyramid.chains_dir),
    )
    .await
    {
        Ok(result) => serde_json::to_value(&result).map_err(|e| e.to_string()),
        Err(e) => Err(format!("Characterization failed: {}", e)),
    }
}

/// Run dual-executor parity test on a code or document slug.
#[tauri::command]
async fn pyramid_parity_run(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let report = wire_node_lib::pyramid::parity::run_parity_test(&state.pyramid, &slug)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&report).map_err(|e| e.to_string())
}

/// IPC equivalent of POST /pyramid/:slug/meta — run all meta passes.
#[tauri::command]
async fn pyramid_meta_run(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Verify slug exists
    {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?;
    }

    // Phase 3 fix pass: clone the live LlmConfig (with provider_registry +
    // credential_store) instead of pulling raw api_key/model strings, so
    // run_all_meta_passes stays on the registry path.
    // Phase 12 verifier fix: attach cache_access so meta retrofit sites
    // reach the step cache.
    let base_config = state
        .pyramid
        .llm_config_with_cache(&slug, &format!("meta-{}", slug))
        .await;
    // walker-v3 W3a (Pattern 4): resolve via walker_resolver reading
    // the active walker_provider_openrouter contribution. Legacy
    // fallback removed by W3c once config.primary_model dies.
    let resolved = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::walker_resolver::first_openrouter_model_from_db(&conn)
    };
    let model = resolved.unwrap_or_else(|| {
        tracing::warn!(
            event = "pattern4_no_openrouter_model",
            "walker-v3: Pattern-4 site found no walker_provider_openrouter model; stamping '<unknown>'",
        );
        "<unknown>".to_string()
    });

    let reader = state.pyramid.reader.clone();
    let writer = state.pyramid.writer.clone();

    match wire_node_lib::pyramid::meta::run_all_meta_passes(
        &reader,
        &writer,
        &slug,
        &base_config,
        &model,
    )
    .await
    {
        Ok(quickstart) => Ok(serde_json::json!({
            "slug": slug,
            "status": "complete",
            "quickstart": quickstart,
        })),
        Err(e) => Err(format!("Meta run failed: {}", e)),
    }
}

/// IPC equivalent of POST /pyramid/:slug/crystallize — manually trigger a delta check.
#[tauri::command]
async fn pyramid_crystallize(
    state: tauri::State<'_, SharedState>,
    slug: String,
    changed_node_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::crystallization;
    use wire_node_lib::pyramid::event_chain::PyramidEvent;

    // Load config and build subscriptions while holding the lock, then release
    let subscriptions = {
        let conn = state.pyramid.reader.lock().await;
        let config = crystallization::load_config(&conn, &slug).unwrap_or_default();
        crystallization::build_crystallization_subscriptions(&config)
    };

    // Register subscriptions in-memory only
    for sub in subscriptions {
        let _ = state.pyramid.event_bus.subscribe_memory_only(sub).await;
    }

    // Emit StaleDetected event
    let event = PyramidEvent::StaleDetected {
        slug: slug.clone(),
        node_ids: changed_node_ids.clone(),
        layer: 0,
    };
    match state.pyramid.event_bus.emit_memory_only(event).await {
        Ok(invocation_ids) => Ok(serde_json::json!({
            "slug": slug,
            "triggered": true,
            "changed_node_ids": changed_node_ids,
            "invocation_ids": invocation_ids,
        })),
        Err(e) => Err(format!("Crystallize trigger failed: {}", e)),
    }
}

/// IPC equivalent of POST /pyramid/:slug/publish — publish pyramid to Wire.
#[tauri::command]
async fn pyramid_publish(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::wire_publish;

    // Validate slug exists
    {
        let conn = state.pyramid.reader.lock().await;
        pyramid_db::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("slug '{}' not found", slug))?;
    }

    // Use session api_token (gne_live_...) for Wire auth, not local HTTP auth_token
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let wire_auth = get_api_token(&state.auth).await?;

    let publisher = wire_publish::PyramidPublisher::new(wire_url, wire_auth);

    // Phase 1: Load all nodes + evidence weights from DB
    let (nodes_by_depth, evidence_weights) = {
        let conn = state.pyramid.reader.lock().await;
        let slug_info = pyramid_db::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("slug '{}' not found", slug))?;

        let mut result = Vec::new();
        for depth in 0..=slug_info.max_depth {
            let nodes = pyramid_db::get_nodes_at_depth(&conn, &slug, depth)
                .map_err(|e| format!("failed to load nodes at depth {}: {}", depth, e))?;
            if !nodes.is_empty() {
                result.push((depth, nodes));
            }
        }

        let mut ev_weights: std::collections::HashMap<
            String,
            std::collections::HashMap<String, f64>,
        > = std::collections::HashMap::new();
        for (_depth, nodes) in &result {
            for node in nodes {
                if let Ok(links) =
                    pyramid_db::get_keep_evidence_for_target_cross(&conn, &slug, &node.id)
                {
                    if !links.is_empty() {
                        let mut child_weights = std::collections::HashMap::new();
                        for link in links {
                            if let Some(w) = link.weight {
                                child_weights.insert(link.source_node_id, w);
                            }
                        }
                        if !child_weights.is_empty() {
                            ev_weights.insert(node.id.clone(), child_weights);
                        }
                    }
                }
            }
        }

        (result, ev_weights)
    };

    if nodes_by_depth.is_empty() {
        return Err(format!("no nodes found for slug '{}'", slug));
    }

    // Phase 2: Publish nodes via HTTP
    match publisher
        .publish_pyramid_idempotent(
            &slug,
            &nodes_by_depth,
            &std::collections::HashMap::new(),
            &evidence_weights,
        )
        .await
    {
        Ok(result) => {
            // Phase 3+4: Persist ID mappings, build_id, and metadata in a single writer lock
            let persisted_build_id: Option<String>;
            let persisted_metadata_id: Option<String>;
            {
                let writer = state.pyramid.writer.lock().await;

                // Phase 3: ID mappings
                if let Err(e) = wire_publish::init_id_map_table(&writer) {
                    tracing::warn!(error = %e, "failed to init id_map table");
                }
                for mapping in &result.id_mappings {
                    let uuid = mapping
                        .wire_uuid
                        .as_deref()
                        .unwrap_or(&mapping.wire_handle_path);
                    if let Err(e) =
                        wire_publish::save_id_mapping(&writer, &slug, &mapping.local_id, uuid)
                    {
                        tracing::warn!(
                            local_id = %mapping.local_id,
                            error = %e,
                            "failed to persist ID mapping"
                        );
                    }
                }

                // Phase 4: Persist last_published_build_id and metadata_contribution_id
                // Read build_id inside the same writer lock to avoid TOCTOU race
                // (a build could complete between a separate read and write)
                persisted_build_id =
                    pyramid_db::get_current_build_id(&writer, &slug).unwrap_or(None);
                if let Some(ref build_id) = persisted_build_id {
                    if let Err(e) =
                        pyramid_db::set_last_published_build_id(&writer, &slug, build_id)
                    {
                        tracing::warn!(
                            slug = %slug,
                            build_id = %build_id,
                            error = %e,
                            "failed to update last_published_build_id after IPC publish"
                        );
                    }
                }
                if let Some(ref apex_uuid) = result.apex_wire_uuid {
                    if let Err(e) =
                        pyramid_db::set_slug_metadata_contribution_id(&writer, &slug, apex_uuid)
                    {
                        tracing::warn!(
                            slug = %slug,
                            apex_uuid = %apex_uuid,
                            error = %e,
                            "failed to update metadata_contribution_id after IPC publish"
                        );
                    }
                }
            }
            persisted_metadata_id = result.apex_wire_uuid.clone();

            tracing::info!(
                slug = %slug,
                node_count = result.node_count,
                apex_uuid = ?result.apex_wire_uuid,
                build_id = ?persisted_build_id,
                "pyramid published to Wire"
            );

            // Build return value with the persisted fields included
            let mut value = serde_json::to_value(&result).map_err(|e| e.to_string())?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "last_published_build_id".to_string(),
                    serde_json::json!(persisted_build_id),
                );
                obj.insert(
                    "metadata_contribution_id".to_string(),
                    serde_json::json!(persisted_metadata_id),
                );
            }
            Ok(value)
        }
        Err(e) => {
            tracing::warn!(slug = %slug, error = %e, "publish failed");
            Err(format!("failed to publish pyramid: {}", e))
        }
    }
}

/// IPC equivalent of POST /pyramid/:slug/publish/question-set — publish question set to Wire.
#[tauri::command]
async fn pyramid_publish_question_set(
    state: tauri::State<'_, SharedState>,
    slug: String,
    description: Option<String>,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::wire_publish;

    // Validate slug exists and get its content type
    let content_type = {
        let conn = state.pyramid.reader.lock().await;
        let info = pyramid_db::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("slug '{}' not found", slug))?;
        info.content_type
    };

    // Load the question set YAML for this content type
    let chains_dir = state.pyramid.chains_dir.clone();
    let qs_path = chains_dir
        .join("questions")
        .join(format!("{}.yaml", content_type.as_str()));

    let qs_yaml = std::fs::read_to_string(&qs_path).map_err(|e| {
        format!(
            "question set not found for content type '{}': {}",
            content_type.as_str(),
            e
        )
    })?;

    let question_set: wire_node_lib::pyramid::question_yaml::QuestionSet =
        serde_yaml::from_str(&qs_yaml)
            .map_err(|e| format!("failed to parse question set YAML: {}", e))?;

    // Use session api_token (gne_live_...) for Wire auth, not local HTTP auth_token
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let wire_auth = get_api_token(&state.auth).await?;

    let publisher = wire_publish::PyramidPublisher::new(wire_url, wire_auth);
    let desc = description.unwrap_or_else(|| {
        format!(
            "Question set for {} content type ({} questions, v{})",
            question_set.r#type,
            question_set.questions.len(),
            question_set.version,
        )
    });

    match publisher.publish_question_set(&question_set, &desc).await {
        Ok(result) => {
            tracing::info!(
                slug = %slug,
                wire_uuid = %result.wire_uuid,
                "question set published to Wire"
            );
            serde_json::to_value(&result).map_err(|e| e.to_string())
        }
        Err(e) => {
            tracing::warn!(slug = %slug, error = %e, "question set publish failed");
            Err(format!("failed to publish question set: {}", e))
        }
    }
}

/// IPC equivalent of POST /pyramid/:slug/check-staleness — run crystallization staleness pipeline.
#[tauri::command]
async fn pyramid_check_staleness(
    state: tauri::State<'_, SharedState>,
    slug: String,
    files: Option<Vec<wire_node_lib::pyramid::staleness_bridge::FileChangeEntry>>,
    threshold: Option<f64>,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::staleness_bridge;

    let threshold = threshold.unwrap_or(state.pyramid.operational.tier2.staleness_threshold);
    let dequeue_cap = state.pyramid.operational.tier2.staleness_queue_dequeue_cap;

    // Determine changed files: explicit or auto-detect
    let (changed_files, source) = {
        let explicit = files
            .as_ref()
            .filter(|f| !f.is_empty())
            .map(|f| staleness_bridge::entries_to_changed_files(f));

        if let Some(files) = explicit {
            (files, "explicit".to_string())
        } else {
            let conn = state.pyramid.reader.lock().await;
            let files = staleness_bridge::auto_detect_changed_files(&conn, &slug)
                .map_err(|e| format!("failed to auto-detect changed files: {}", e))?;
            (files, "auto_detect_pending_mutations".to_string())
        }
    };

    let files_processed = changed_files.len();

    // Run the staleness pipeline via spawn_blocking
    let conn = state.pyramid.writer.clone();
    let slug_owned = slug.clone();
    let result = tokio::task::spawn_blocking(move || {
        let c = conn.blocking_lock();
        staleness_bridge::run_staleness_check(&c, &slug_owned, &changed_files, threshold, dequeue_cap)
    })
    .await;

    match result {
        Ok(Ok((report, queued_items))) => {
            let response = staleness_bridge::CheckStalenessResponse {
                source,
                files_processed,
                report,
                queued_items,
            };
            serde_json::to_value(&response).map_err(|e| e.to_string())
        }
        Ok(Err(e)) => {
            tracing::warn!(slug = %slug, error = %e, "staleness check failed");
            Err(format!("staleness check failed: {}", e))
        }
        Err(e) => {
            tracing::warn!(slug = %slug, error = %e, "staleness check panicked");
            Err(format!("staleness check panicked: {}", e))
        }
    }
}

/// Phase 3a: return the resolved chain YAML for a given slug as JSON.
///
/// Resolution order: per-slug assignment → content-type default → fallback.
/// The chain YAML file is loaded, parsed, and returned as a serde_json::Value
/// so the frontend can render step timelines, inspect primitives, etc.
#[tauri::command]
async fn pyramid_get_build_chain(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::{chain_loader, chain_registry, db as pyramid_db};

    let reader = state.pyramid.reader.lock().await;

    // 1. Get content_type and evidence_mode for the slug
    let slug_info = pyramid_db::get_slug(&reader, &slug)
        .map_err(|e| format!("failed to get slug info: {e}"))?
        .ok_or_else(|| format!("slug '{}' not found", slug))?;

    let content_type_str = slug_info.content_type.as_str();
    // Default evidence_mode to "deep" — the most common build mode
    let evidence_mode = "deep";

    // 2. Resolve chain_id via the three-tier resolver
    let chain_id = chain_registry::resolve_chain_for_slug(&reader, &slug, content_type_str, evidence_mode)
        .map_err(|e| format!("failed to resolve chain: {e}"))?;
    drop(reader);

    // 3. Discover chains and find the YAML file path
    let chains_dir = state.pyramid.chains_dir.clone();
    let all_chains = chain_loader::discover_chains(&chains_dir)
        .map_err(|e| format!("failed to discover chains: {e}"))?;

    let meta = all_chains
        .iter()
        .find(|m| m.id == chain_id)
        .ok_or_else(|| {
            format!(
                "chain '{}' not found in chains directory ({})",
                chain_id,
                chains_dir.display()
            )
        })?;

    // 4. Load the raw YAML and parse to JSON
    let yaml_path = std::path::Path::new(&meta.file_path);
    let raw_yaml = std::fs::read_to_string(yaml_path)
        .map_err(|e| format!("failed to read chain file: {e}"))?;

    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&raw_yaml)
        .map_err(|e| format!("failed to parse chain YAML: {e}"))?;

    let json_value = serde_json::to_value(yaml_value)
        .map_err(|e| format!("failed to convert YAML to JSON: {e}"))?;

    // 5. Derive max_depth for the frontend layout.
    //
    // Question and conversation builds have a configured max_depth (the
    // $max_depth chain variable, which the executor defaults to 3).
    // Mechanical builds (code/document) have emergent depth — return null
    // so the frontend doesn't override the actual depth range.
    //
    // Resolution order:
    //   a) Parse the chain YAML steps for a recursive_decompose step with
    //      an explicit numeric max_depth input.
    //   b) If the input is a variable ref ("$max_depth"), use the executor's
    //      resolution default (3) — matching chain_executor.rs logic.
    //   c) For mechanical content types (code, document, vine), return null.
    let max_depth: Option<u64> = match slug_info.content_type {
        wire_node_lib::pyramid::types::ContentType::Question
        | wire_node_lib::pyramid::types::ContentType::Conversation => {
            // Try to extract from the chain YAML's recursive_decompose step
            let from_yaml = json_value
                .get("steps")
                .and_then(|s| s.as_array())
                .and_then(|steps| {
                    steps.iter().find_map(|step| {
                        let prim = step.get("primitive").and_then(|p| p.as_str());
                        if prim == Some("recursive_decompose") {
                            step.get("input")
                                .and_then(|inp| inp.get("max_depth"))
                                .and_then(|md| {
                                    // If it's a number, use it directly
                                    if let Some(n) = md.as_u64() {
                                        return Some(n);
                                    }
                                    // If it's a variable ref like "$max_depth", the
                                    // executor resolves it with a default of 3
                                    // (see chain_executor.rs execute_recursive_decompose)
                                    if md.as_str().map_or(false, |s| s.starts_with('$')) {
                                        return Some(3);
                                    }
                                    None
                                })
                        } else {
                            None
                        }
                    })
                });
            // Fall back to 3 for question/conversation builds — this is the
            // executor default in chain_executor.rs and pyramid_rebuild.
            Some(from_yaml.unwrap_or(3))
        }
        // Mechanical builds: depth is emergent from the corpus, not configured
        _ => None,
    };

    Ok(serde_json::json!({
        "chain_id": chain_id,
        "content_type": content_type_str,
        "evidence_mode": evidence_mode,
        "file_path": meta.file_path,
        "chain": json_value,
        "max_depth": max_depth,
    }))
}

/// IPC equivalent of POST /pyramid/chain/import — import a chain or question set from the Wire.
#[tauri::command]
async fn pyramid_chain_import(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
    import_type: Option<String>,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::wire_import;

    let import_type = import_type.as_deref().unwrap_or("chain");
    let contribution_id = contribution_id.trim();

    if contribution_id.is_empty() {
        return Err("contribution_id is required".to_string());
    }

    // Use session api_token for Wire auth
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let wire_auth = get_api_token(&state.auth).await?;

    let client = wire_import::WireImportClient::new(wire_url, wire_auth, None);

    match import_type {
        "chain" => {
            let chain = client
                .fetch_chain(contribution_id)
                .await
                .map_err(|e| format!("failed to import chain: {}", e))?;

            let writer = state.pyramid.writer.lock().await;
            wire_import::save_imported_chain(&writer, &chain)
                .map_err(|e| format!("failed to persist chain: {}", e))?;
            drop(writer);

            Ok(serde_json::json!({
                "ok": true,
                "contribution_id": chain.id,
                "title": chain.title,
                "content_type": chain.content_type,
                "import_type": "chain",
            }))
        }
        "question_set" => {
            let qs = client
                .fetch_question_set(contribution_id)
                .await
                .map_err(|e| format!("failed to import question set: {}", e))?;

            let writer = state.pyramid.writer.lock().await;
            wire_import::save_imported_question_set(&writer, &qs)
                .map_err(|e| format!("failed to persist question set: {}", e))?;
            drop(writer);

            Ok(serde_json::json!({
                "ok": true,
                "contribution_id": qs.id,
                "title": qs.title,
                "content_type": null,
                "import_type": "question_set",
            }))
        }
        other => Err(format!(
            "invalid import_type '{}': expected 'chain' or 'question_set'",
            other
        )),
    }
}

/// WS-ONLINE-D: Query a remote pyramid via the local HTTP pyramid API.
#[tauri::command]
async fn pyramid_remote_query(
    state: tauri::State<'_, SharedState>,
    tunnel_url: String,
    slug: String,
    action: String,
    params: Option<std::collections::HashMap<String, String>>,
) -> Result<serde_json::Value, String> {
    let auth_token = {
        let config = state.pyramid.config.read().await;
        config.auth_token.clone()
    };

    let client = reqwest::Client::new();
    let mut body = serde_json::json!({
        "tunnel_url": tunnel_url,
        "slug": slug,
        "action": action,
    });
    if let Some(p) = params {
        body["params"] = serde_json::to_value(p).unwrap_or_default();
    }

    let resp = client
        .post("http://localhost:8765/pyramid/remote-query")
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Remote query failed: {}", e))?;

    let status = resp.status();
    let result: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("Remote query returned {}: {}", status.as_u16(), result));
    }

    Ok(result)
}

/// WS-ONLINE-D: Pin a remote pyramid — pulls full export and stores locally.
#[tauri::command]
async fn pyramid_pin_remote(
    state: tauri::State<'_, SharedState>,
    tunnel_url: String,
    slug: String,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::wire_import::RemotePyramidClient;

    let tunnel_url = tunnel_url.trim().to_string();
    let slug = slug.trim().to_string();

    if tunnel_url.is_empty() {
        return Err("tunnel_url is required".to_string());
    }
    if slug.is_empty() {
        return Err("slug is required".to_string());
    }

    // Use session api_token for Wire auth
    let wire_auth = get_api_token(&state.auth).await?;

    let wire_server_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());

    // Pull remote pyramid export
    let client = RemotePyramidClient::new(tunnel_url.clone(), wire_auth, wire_server_url);
    let nodes = client
        .pull_remote_pyramid(&slug)
        .await
        .map_err(|e| format!("failed to pull remote pyramid: {}", e))?;

    let node_count = nodes.len();

    // Insert into local SQLite
    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::slug::pin_remote_pyramid(&writer, &slug, &tunnel_url, &nodes)
        .map_err(|e| format!("failed to pin pyramid: {}", e))?;
    drop(writer);

    // Register in sync state so auto-refresh timer picks it up (WS-ONLINE-D)
    {
        let mut sync = state.pyramid_sync_state.lock().await;
        sync.pin_pyramid(slug.clone(), tunnel_url.clone());
    }

    Ok(serde_json::json!({
        "ok": true,
        "slug": slug,
        "tunnel_url": tunnel_url,
        "node_count": node_count,
    }))
}

/// WS-ONLINE-D: Unpin a pyramid — clears pinned flag but never deletes node data (Pillar 1).
#[tauri::command]
async fn pyramid_unpin(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let slug = slug.trim().to_string();
    if slug.is_empty() {
        return Err("slug is required".to_string());
    }

    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::slug::unpin_pyramid(&writer, &slug)
        .map_err(|e| format!("failed to unpin pyramid: {}", e))?;
    drop(writer);

    // Deregister from sync state so auto-refresh stops polling (WS-ONLINE-D)
    {
        let mut sync = state.pyramid_sync_state.lock().await;
        sync.unpin_pyramid(&slug);
    }

    Ok(serde_json::json!({
        "ok": true,
        "slug": slug,
        "message": "unpinned (node data preserved)"
    }))
}

/// IPC equivalent of POST /partner/message — send message, get response + brain state.
#[tauri::command]
async fn partner_send_message(
    state: tauri::State<'_, SharedState>,
    session_id: String,
    message: String,
) -> Result<serde_json::Value, String> {
    match wire_node_lib::partner::conversation::handle_message(
        &state.partner,
        &session_id,
        &message,
    )
    .await
    {
        Ok(response) => serde_json::to_value(&response).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// IPC equivalent of POST /partner/session/new — create a new session.
#[tauri::command]
async fn partner_session_new(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
    is_lobby: Option<bool>,
) -> Result<serde_json::Value, String> {
    let is_lobby = is_lobby.unwrap_or(slug.is_none());
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let session = wire_node_lib::partner::Session {
        id: session_id.clone(),
        slug,
        is_lobby,
        conversation_buffer: Vec::new(),
        session_topics: Vec::new(),
        hydrated_node_ids: Vec::new(),
        lifted_results: Vec::new(),
        dennis_state: wire_node_lib::partner::DennisState::Idle,
        warm_cursor: 0,
        created_at: now.clone(),
        last_active_at: now,
    };

    // Save to DB
    {
        let db = state.partner.partner_db.lock().await;
        wire_node_lib::partner::save_session(&db, &session).map_err(|e| e.to_string())?;
    }

    // Add to in-memory cache
    {
        let mut sessions = state.partner.sessions.lock().await;
        sessions.insert(session_id.clone(), session.clone());
    }

    serde_json::to_value(&session).map_err(|e| e.to_string())
}

/// WS-L: generate (or return cached) ASCII banner art for the given slug.
/// Operator-triggered only per A11 — never lazy on anonymous web requests.
#[tauri::command]
async fn pyramid_generate_ascii_banner(
    slug: String,
    state: tauri::State<'_, SharedState>,
) -> Result<String, String> {
    wire_node_lib::pyramid::public_html::ascii_art::generate_banner_for_slug(
        state.pyramid.clone(),
        &slug,
    )
    .await
}

/// Owner-mode bridge: mint a one-time `web_sessions` row whose
/// supabase_user_id carries the local-operator sentinel, then return a
/// tunnel URL that consumes the token, sets the wire_session cookie, and
/// redirects to the requested pyramid. The auth filter recognizes the
/// sentinel and grants LocalOperator privileges (full operator access,
/// billing-exempt) to every subsequent request.
///
/// The desktop app's "Open as owner" button invokes this command, then
/// shells out to the returned URL via tauri::api::shell::open. Tokens
/// expire after 60 seconds (one-time consume + short TTL).
#[tauri::command]
async fn pyramid_open_web_as_owner(
    slug: Option<String>,
    state: tauri::State<'_, SharedState>,
) -> Result<String, String> {
    use wire_node_lib::pyramid::public_html::auth::LOCAL_OPERATOR_SENTINEL_PREFIX;
    use wire_node_lib::pyramid::public_html::web_sessions;

    // Determine tunnel URL: read from tunnel_state (set by tunnel.rs at start).
    let tunnel_url = {
        let ts = state.tunnel_state.read().await;
        ts.tunnel_url.clone()
    };
    // TunnelUrl::parse rejects empty strings by construction, so a Some(_)
    // implies a non-empty URL — the is_empty() check on the old String form
    // is now redundant. Pattern-match on the Option directly.
    let base = match tunnel_url {
        Some(u) => u,
        None => {
            return Err(
                "No tunnel URL is set. Make sure the Cloudflare tunnel is running."
                    .to_string(),
            );
        }
    };

    // Build the owner-mode supabase_user_id sentinel (operator email if known,
    // else a synthetic id). Email is purely informational on this row.
    let (operator_email, operator_id_part) = {
        let auth = state.auth.read().await;
        let email = auth
            .email
            .clone()
            .unwrap_or_else(|| "owner@local".to_string());
        let id = auth
            .operator_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        (email, id)
    };
    let sentinel_user_id = format!("{}{}", LOCAL_OPERATOR_SENTINEL_PREFIX, operator_id_part);

    // Insert a 60-second TTL row into web_sessions; the returned token IS
    // the cookie value the bridge will set.
    let token = {
        let conn = state.pyramid.writer.lock().await;
        web_sessions::create(&conn, &sentinel_user_id, &operator_email, 60)
            .map_err(|e| format!("failed to mint owner session: {}", e))?
    };

    let return_slug = slug
        .filter(|s| {
            !s.is_empty()
                && !s.starts_with('_')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .unwrap_or_default();

    // TunnelUrl::endpoint() constructs a root-served URL with the given
    // absolute path; it handles scheme/host/port correctly and replaces any
    // stray path on the base. The trim_end_matches('/') dance is unneeded
    // because TunnelUrl already normalizes trailing slashes at parse time
    // and endpoint() builds the path portion from scratch.
    let url = if return_slug.is_empty() {
        format!("{}?token={}", base.endpoint("/p/_owner_login"), token)
    } else {
        format!(
            "{}?token={}&return={}",
            base.endpoint("/p/_owner_login"),
            token,
            return_slug
        )
    };
    Ok(url)
}

/// Stage 0 web-surface UX: shell out to the system default browser via the
/// tauri-plugin-shell plugin. The Tauri 2 webview blocks `window.open`, so
/// the React side must round-trip through this command to actually open URLs.
#[tauri::command]
async fn open_url_in_browser(
    url: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    app.shell()
        .open(&url, None)
        .map_err(|e| format!("Failed to open URL in browser: {}", e))
}

/// Returns the public tunnel URL for a slug (or the index path when slug is None).
/// Errors when the tunnel hasn't come up yet so the UI can prompt the user to retry.
#[tauri::command]
async fn pyramid_get_public_url(
    slug: Option<String>,
    state: tauri::State<'_, SharedState>,
) -> Result<String, String> {
    let tunnel_url = {
        let ts = state.tunnel_state.read().await;
        ts.tunnel_url.clone()
    };
    // TunnelUrl::parse rejects empty strings, so Some(_) already guarantees
    // non-empty — the prior is_empty filter is redundant with the newtype.
    let base = tunnel_url
        .ok_or_else(|| "Tunnel is not running. Click 'Retry Tunnel' in the header.".to_string())?;
    let path = match slug {
        Some(s) if !s.is_empty() => format!("/p/{}", s),
        _ => "/p/".to_string(),
    };
    // endpoint() builds scheme://host[:port]/<path> without needing a
    // defensive trim_end_matches('/') — the newtype normalizes at parse time.
    Ok(base.endpoint(&path))
}

/// Returns the cached ASCII banner for a slug, or None if none has been generated.
/// Used by the drawer to pre-populate the inline banner preview without forcing
/// a regeneration round-trip to Grok-4.2.
#[tauri::command]
async fn pyramid_get_cached_banner(
    slug: String,
    state: tauri::State<'_, SharedState>,
) -> Result<Option<String>, String> {
    Ok(wire_node_lib::pyramid::public_html::ascii_art::get_banner_for_slug(
        &state.pyramid,
        &slug,
    )
    .await)
}

#[tauri::command]
async fn pyramid_get_config(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let config = state.pyramid.config.read().await;
    // Read auto_execute from PyramidConfig file (not in-memory LlmConfig)
    let auto_execute = state.pyramid.data_dir.as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("pyramid_config.json")).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["auto_execute"].as_bool())
        .unwrap_or(false);

    // walker-v3 W3c (Cluster 1): synthesize the legacy model triple from
    // the active walker_provider_openrouter contribution so existing
    // frontend Settings views keep rendering. No legacy fallback — when
    // the contribution isn't present the Settings UI shows null for
    // those fields and the operator follows up via Tools > Create.
    let (primary_syn, fb1_syn, fb2_syn) = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::walker_resolver::synthesize_legacy_model_triple_from_db(&conn)
    };
    Ok(serde_json::json!({
        "api_key_set": !config.api_key.is_empty(),
        "auth_token_set": !config.auth_token.is_empty(),
        "primary_model": primary_syn,
        "fallback_model_1": fb1_syn,
        "fallback_model_2": fb2_syn,
        "auto_execute": auto_execute,
    }))
}

/// Return the pyramid auth token for use in frontend HTTP fetch calls.
#[tauri::command]
async fn pyramid_get_auth_token(
    state: tauri::State<'_, SharedState>,
) -> Result<String, String> {
    let config = state.pyramid.config.read().await;
    Ok(config.auth_token.clone())
}

/// List every model profile available to apply. Walks data_dir/profiles/
/// and the legacy ~/.gemini/wire-node/profiles/ fallback. Returns sorted
/// profile names (without `.json`).
#[tauri::command]
async fn pyramid_list_profiles(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<String>, String> {
    let data_dir = state
        .pyramid
        .data_dir
        .clone()
        .ok_or_else(|| "data_dir not configured".to_string())?;
    Ok(wire_node_lib::pyramid::PyramidConfig::list_profiles(&data_dir))
}

/// Apply a model profile by name. Mutates the in-memory PyramidConfig +
/// LLM config so subsequent builds use the new model selection. The
/// change is in-memory only — profiles are layered overrides, not
/// persisted as the new defaults. (To persist, the operator edits the
/// pyramid_config.json directly.)
#[tauri::command]
async fn pyramid_apply_profile(
    state: tauri::State<'_, SharedState>,
    profile: String,
) -> Result<(), String> {
    let data_dir = state
        .pyramid
        .data_dir
        .clone()
        .ok_or_else(|| "data_dir not configured".to_string())?;

    // Load the on-disk pyramid_config.json, apply the profile patch in
    // memory, then update the live LlmConfig that the build pipeline
    // reads. We don't write the merged config back to disk — profiles
    // are non-destructive overlays.
    let config_path = data_dir.join("pyramid_config.json");
    let mut pyramid_config: wire_node_lib::pyramid::PyramidConfig =
        if config_path.exists() {
            std::fs::read_to_string(&config_path)
                .map_err(|e| format!("read pyramid_config.json: {}", e))
                .and_then(|s| {
                    serde_json::from_str(&s)
                        .map_err(|e| format!("parse pyramid_config.json: {}", e))
                })?
        } else {
            wire_node_lib::pyramid::PyramidConfig::default()
        };

    pyramid_config
        .apply_profile(&profile, &data_dir)
        .map_err(|e| e.to_string())?;

    // Push the new LlmConfig into the running state so the next build
    // sees it. The api_key + auth_token come from the live config, not
    // the profile (profiles only override model selection + tier params).
    //
    // Phase 3: preserve the provider_registry + credential_store that
    // were attached at app startup — the profile apply only mutates
    // model selection and tier params, not the backing registry.
    let new_llm = pyramid_config.to_llm_config_with_runtime(
        state.pyramid.provider_registry.clone(),
        state.pyramid.credential_store.clone(),
    );
    {
        let mut live = state.pyramid.config.write().await;
        let previous_live = live.clone();
        *live = new_llm.with_runtime_overlays_from(&previous_live);
    }

    // walker-v3 W3c: operator-visible tracing log pulls the triple from
    // the active walker_provider_openrouter contribution. No legacy
    // fallback — LlmConfig.primary_model + friends are deleted.
    // TODO(walker-v3 Phase 6): pyramid_apply_profile itself may retire
    // entirely if profile semantics fold into walker_slot_policy /
    // walker_provider_* contribution overlays.
    let (primary_syn, fb1_syn, fb2_syn) = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::walker_resolver::synthesize_legacy_model_triple_from_db(&conn)
    };
    tracing::info!(
        profile = %profile,
        primary = ?primary_syn,
        fallback_1 = ?fb1_syn,
        fallback_2 = ?fb2_syn,
        "applied model profile",
    );
    Ok(())
}

/// Test an OpenRouter API key server-side so the key never touches the renderer.
/// Reads directly from the credential store (single source of truth).
#[tauri::command]
async fn pyramid_test_api_key(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let api_key = state
        .pyramid
        .credential_store
        .resolve_var("OPENROUTER_KEY")
        .map(|s| s.raw_clone())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err("No API key configured".to_string());
    }
    let client = reqwest::Client::new();
    let resp = client
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    if resp.status().is_success() {
        Ok("API key is valid".to_string())
    } else {
        Err(format!(
            "API key test failed: {} {}",
            resp.status().as_u16(),
            resp.status().canonical_reason().unwrap_or("Unknown")
        ))
    }
}

/// Test a remote tunnel connection server-side so the renderer cannot fetch arbitrary URLs.
#[tauri::command]
async fn test_remote_connection(url: String) -> Result<serde_json::Value, String> {
    let url = url.trim().trim_end_matches('/').to_string();
    let health_url = format!("{}/health", url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&health_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
    if resp.status().is_success() {
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Invalid response: {}", e))?;
        Ok(data)
    } else {
        Err(format!("Failed: HTTP {}", resp.status().as_u16()))
    }
}

#[tauri::command]
fn get_home_dir() -> Result<String, String> {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
async fn pyramid_build_cancel(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<(), String> {
    let active = state.pyramid.active_build.read().await;
    if let Some(handle) = active.get(&slug) {
        let s = handle.status.read().await;
        if s.is_running() && !handle.cancel.is_cancelled() {
            drop(s);
            handle.cancel.cancel();
            return Ok(());
        }
    }
    Err("No active build to cancel".to_string())
}

/// Force-reset a build that has been running for > 30 minutes (stuck build).
#[tauri::command]
async fn pyramid_build_force_reset(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<(), String> {
    let active = state.pyramid.active_build.read().await;
    if let Some(handle) = active.get(&slug) {
        let elapsed = handle.started_at.elapsed().as_secs();
        if elapsed < 1800 {
            return Err(format!(
                "Build has only been running for {}s — force reset requires 30+ minutes",
                elapsed
            ));
        }
        let mut s = handle.status.write().await;
        if s.status == "running" {
            s.status = "failed".to_string();
            // Also cancel the token so any still-running work stops
            handle.cancel.cancel();
            tracing::warn!("Force-reset build for '{}' after {}s", slug, elapsed);
            return Ok(());
        }
    }
    Err("No active running build to force-reset".to_string())
}

#[tauri::command]
async fn pyramid_vine_build(
    state: tauri::State<'_, SharedState>,
    vine_slug: String,
    jsonl_dirs: Vec<String>,
) -> Result<String, String> {
    use std::path::PathBuf;
    use wire_node_lib::pyramid::vine;

    let dirs: Vec<PathBuf> = jsonl_dirs.iter().map(PathBuf::from).collect();

    // Validate slug
    let vine_slug_clean = wire_node_lib::pyramid::slug::slugify(&vine_slug);
    wire_node_lib::pyramid::slug::validate_slug(&vine_slug_clean).map_err(|e| e.to_string())?;

    // Check for concurrent vine build on same slug
    {
        let builds = state.pyramid.vine_builds.lock().await;
        if let Some(handle) = builds.get(&vine_slug_clean) {
            if handle.status == "running" {
                return Err(format!(
                    "Vine build already running for '{}'",
                    vine_slug_clean
                ));
            }
        }
    }

    // Register vine build
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut builds = state.pyramid.vine_builds.lock().await;
        builds.insert(
            vine_slug_clean.clone(),
            wire_node_lib::pyramid::VineBuildHandle {
                cancel: cancel.clone(),
                status: "running".to_string(),
                error: None,
            },
        );
    }

    // Use a build-scoped reader so the vine build doesn't compete with
    // CLI/frontend queries for the shared reader Mutex.
    let pyramid_state = state
        .pyramid
        .with_build_reader()
        .map_err(|e| format!("Failed to create build reader: {e}"))?;

    let slug_for_task = vine_slug_clean.clone();
    let vine_builds = state.pyramid.vine_builds.clone();

    let vine_build_handle = tokio::spawn(async move {
        let result = vine::build_vine(&pyramid_state, &slug_for_task, &dirs, "deep", &cancel).await;
        let mut builds = vine_builds.lock().await;
        if let Some(handle) = builds.get_mut(&slug_for_task) {
            match result {
                Ok(_) => {
                    handle.status = "complete".to_string();
                }
                Err(e) => {
                    handle.status = "failed".to_string();
                    handle.error = Some(e.to_string());
                }
            }
        }
    });

    // Monitor: catch panics in the vine build task and set status to "failed"
    let monitor_vine_builds = state.pyramid.vine_builds.clone();
    let monitor_vine_slug = vine_slug_clean.clone();
    tokio::spawn(async move {
        if let Err(e) = vine_build_handle.await {
            tracing::error!("pyramid_vine_build task panicked: {e:?}");
            let mut builds = monitor_vine_builds.lock().await;
            if let Some(handle) = builds.get_mut(&monitor_vine_slug) {
                if handle.status == "running" {
                    handle.status = "failed".to_string();
                    handle.error = Some(format!("Build task panicked: {e:?}"));
                }
            }
        }
    });

    Ok(format!("Vine build started for '{}'", vine_slug_clean))
}

#[tauri::command]
async fn pyramid_vine_build_status(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let builds = state.pyramid.vine_builds.lock().await;
    if let Some(handle) = builds.get(&slug) {
        Ok(serde_json::json!({
            "vine_slug": slug,
            "status": handle.status,
            "error": handle.error,
        }))
    } else {
        Ok(serde_json::json!({
            "vine_slug": slug,
            "status": "not_found",
            "error": null,
        }))
    }
}

#[tauri::command]
async fn pyramid_vine_bunches(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let bunches = pyramid_db::list_vine_bunches(&conn, &slug).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&bunches).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_eras(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let eras =
        pyramid_db::get_annotations_by_type(&conn, &slug, "era").map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&eras).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_decisions(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let faqs = pyramid_db::get_faq_nodes_by_prefix(&conn, &slug, "FAQ-vine-decision-")
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&faqs).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_entities(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let faqs = pyramid_db::get_faq_nodes_by_prefix(&conn, &slug, "FAQ-vine-entity-")
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&faqs).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_threads(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let threads: Vec<_> = conn.prepare(
        "SELECT thread_id, thread_name, current_canonical_id, depth, delta_count FROM pyramid_threads WHERE slug = ?1"
    ).map_err(|e| e.to_string())?
    .query_map(rusqlite::params![slug], |row| {
        Ok(serde_json::json!({
            "thread_id": row.get::<_, String>(0)?,
            "thread_name": row.get::<_, String>(1)?,
            "canonical_id": row.get::<_, String>(2)?,
            "depth": row.get::<_, i64>(3)?,
            "delta_count": row.get::<_, i64>(4)?,
        }))
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let edges: Vec<_> = conn.prepare(
        "SELECT thread_a_id, thread_b_id, relationship, relevance FROM pyramid_web_edges WHERE slug = ?1 AND relevance > 0.1"
    ).map_err(|e| e.to_string())?
    .query_map(rusqlite::params![slug], |row| {
        Ok(serde_json::json!({
            "thread_a": row.get::<_, String>(0)?,
            "thread_b": row.get::<_, String>(1)?,
            "relationship": row.get::<_, String>(2)?,
            "relevance": row.get::<_, f64>(3)?,
        }))
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(serde_json::json!({ "threads": threads, "edges": edges }))
}

#[tauri::command]
async fn pyramid_vine_drill(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let annotations = pyramid_db::get_annotations_by_type(&conn, &slug, "directory")
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&annotations).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_corrections(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let corrections = pyramid_query::corrections(&conn, &slug).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&corrections).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn pyramid_vine_integrity(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    use wire_node_lib::pyramid::vine;

    let pyramid_state = Arc::new(wire_node_lib::pyramid::PyramidState {
        reader: state.pyramid.reader.clone(),
        writer: state.pyramid.writer.clone(),
        config: state.pyramid.config.clone(),
        active_build: state.pyramid.active_build.clone(),
        data_dir: state.pyramid.data_dir.clone(),
        stale_engines: state.pyramid.stale_engines.clone(),
        file_watchers: state.pyramid.file_watchers.clone(),
        vine_builds: state.pyramid.vine_builds.clone(),
        use_chain_engine: std::sync::atomic::AtomicBool::new(
            state
                .pyramid
                .use_chain_engine
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
        use_ir_executor: std::sync::atomic::AtomicBool::new(
            state
                .pyramid
                .use_ir_executor
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
        event_bus: state.pyramid.event_bus.clone(),
        operational: state.pyramid.operational.clone(),
        chains_dir: state.pyramid.chains_dir.clone(),
        remote_query_rate_limiter: state.pyramid.remote_query_rate_limiter.clone(),
        absorption_gate: state.pyramid.absorption_gate.clone(),
        build_event_bus: state.pyramid.build_event_bus.clone(),
        supabase_url: state.pyramid.supabase_url.clone(),
        supabase_anon_key: state.pyramid.supabase_anon_key.clone(),
        csrf_secret: state.pyramid.csrf_secret,
        dadbear_handle: state.pyramid.dadbear_handle.clone(),
        dadbear_supervisor_handle: state.pyramid.dadbear_supervisor_handle.clone(),
        dadbear_in_flight: state.pyramid.dadbear_in_flight.clone(),
        provider_registry: state.pyramid.provider_registry.clone(),
        credential_store: state.pyramid.credential_store.clone(),
        schema_registry: state.pyramid.schema_registry.clone(),
        cross_pyramid_router: state.pyramid.cross_pyramid_router.clone(),
        ollama_pull_cancel: state.pyramid.ollama_pull_cancel.clone(),
        ollama_pull_in_progress: state.pyramid.ollama_pull_in_progress.clone(),
    });

    let summary = vine::run_integrity_check(&pyramid_state, &slug)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "vine_slug": slug,
        "summary": summary,
    }))
}

#[tauri::command]
async fn pyramid_vine_rebuild_upper(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<String, String> {
    use wire_node_lib::pyramid::vine;

    let pyramid_state = Arc::new(wire_node_lib::pyramid::PyramidState {
        reader: state.pyramid.reader.clone(),
        writer: state.pyramid.writer.clone(),
        config: state.pyramid.config.clone(),
        active_build: state.pyramid.active_build.clone(),
        data_dir: state.pyramid.data_dir.clone(),
        stale_engines: state.pyramid.stale_engines.clone(),
        file_watchers: state.pyramid.file_watchers.clone(),
        vine_builds: state.pyramid.vine_builds.clone(),
        use_chain_engine: std::sync::atomic::AtomicBool::new(
            state
                .pyramid
                .use_chain_engine
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
        use_ir_executor: std::sync::atomic::AtomicBool::new(
            state
                .pyramid
                .use_ir_executor
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
        event_bus: state.pyramid.event_bus.clone(),
        operational: state.pyramid.operational.clone(),
        chains_dir: state.pyramid.chains_dir.clone(),
        remote_query_rate_limiter: state.pyramid.remote_query_rate_limiter.clone(),
        absorption_gate: state.pyramid.absorption_gate.clone(),
        build_event_bus: state.pyramid.build_event_bus.clone(),
        supabase_url: state.pyramid.supabase_url.clone(),
        supabase_anon_key: state.pyramid.supabase_anon_key.clone(),
        csrf_secret: state.pyramid.csrf_secret,
        dadbear_handle: state.pyramid.dadbear_handle.clone(),
        dadbear_supervisor_handle: state.pyramid.dadbear_supervisor_handle.clone(),
        dadbear_in_flight: state.pyramid.dadbear_in_flight.clone(),
        provider_registry: state.pyramid.provider_registry.clone(),
        credential_store: state.pyramid.credential_store.clone(),
        schema_registry: state.pyramid.schema_registry.clone(),
        cross_pyramid_router: state.pyramid.cross_pyramid_router.clone(),
        ollama_pull_cancel: state.pyramid.ollama_pull_cancel.clone(),
        ollama_pull_in_progress: state.pyramid.ollama_pull_in_progress.clone(),
    });

    let cancel = tokio_util::sync::CancellationToken::new();
    vine::force_rebuild_vine_upper(&pyramid_state, &slug, &cancel)
        .await
        .map_err(|e| e.to_string())
}

// --- DADBEAR IPC Commands ---------------------------------------------------

#[tauri::command]
async fn pyramid_auto_update_config_get(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    match pyramid_db::get_auto_update_config(&conn, &slug) {
        Some(config) => serde_json::to_value(&config).map_err(|e| e.to_string()),
        None => Err(format!("No auto-update config for slug '{}'", slug)),
    }
}

#[tauri::command]
async fn pyramid_auto_update_config_set(
    state: tauri::State<'_, SharedState>,
    slug: String,
    debounce_minutes: Option<i32>,
    min_changed_files: Option<i32>,
    runaway_threshold: Option<f64>,
    auto_update: Option<bool>,
) -> Result<serde_json::Value, String> {
    let (result, should_resume_breaker) = {
        let mut conn = state.pyramid.writer.lock().await;
        let mut should_resume_breaker = false;

        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(d) = debounce_minutes {
            if d < 1 {
                return Err("debounce_minutes must be >= 1".to_string());
            }
            sets.push(format!("debounce_minutes = ?{}", params.len() + 1));
            params.push(Box::new(d));
        }
        if let Some(m) = min_changed_files {
            sets.push(format!("min_changed_files = ?{}", params.len() + 1));
            params.push(Box::new(m));
        }
        if let Some(r) = runaway_threshold {
            if r <= 0.0 || r > 1.0 {
                return Err("runaway_threshold must be > 0.0 and <= 1.0".to_string());
            }
            sets.push(format!("runaway_threshold = ?{}", params.len() + 1));
            params.push(Box::new(r));
        }
        if let Some(a) = auto_update {
            sets.push(format!("auto_update = ?{}", params.len() + 1));
            params.push(Box::new(if a { 1i32 } else { 0i32 }));
        }

        if sets.is_empty() {
            return Err("No fields to update".to_string());
        }

        // Resolve current norms, apply user's changes, supersede the contribution.
        let mut norms = wire_node_lib::pyramid::config_contributions::resolve_dadbear_norms(
            &conn, Some(&slug),
        ).unwrap_or_default();

        // Apply the user's changes to the resolved norms
        if let Some(d) = debounce_minutes {
            norms.debounce_secs = (d as i64) * 60;
        }
        if let Some(m) = min_changed_files {
            norms.min_changed_files = m as i64;
        }
        if let Some(r) = runaway_threshold {
            norms.runaway_threshold = r;
        }

        // Serialize the updated norms as YAML
        let yaml_content = serde_yaml::to_string(&norms).map_err(|e| e.to_string())?;

        // Find the active dadbear_norms contribution for this slug and supersede it,
        // or create a new one if none exists.
        let existing = wire_node_lib::pyramid::config_contributions::load_active_config_contribution(
            &conn, "dadbear_norms", Some(&slug),
        ).ok().flatten();

        if let Some(old_contrib) = existing {
            // Supersede with updated values
            wire_node_lib::pyramid::config_contributions::supersede_config_contribution(
                &mut conn,
                &old_contrib.contribution_id,
                &yaml_content,
                "Updated via DADBEAR panel config save",
                "local",
                Some("operator"),
            ).map_err(|e: anyhow::Error| e.to_string())?;
        } else {
            // Create new per-slug norms contribution
            wire_node_lib::pyramid::config_contributions::create_config_contribution(
                &conn,
                "dadbear_norms",
                Some(&slug),
                &yaml_content,
                Some("Created via DADBEAR panel config save"),
                "local",
                Some("operator"),
                "active",
            ).map_err(|e: anyhow::Error| e.to_string())?;
        }

        // Handle auto_update toggle: if turning off, place a frozen hold;
        // if turning on, clear frozen hold.
        if let Some(a) = auto_update {
            if !a {
                wire_node_lib::pyramid::auto_update_ops::freeze(
                    &conn, &state.pyramid.build_event_bus, &slug,
                ).map_err(|e: anyhow::Error| e.to_string())?;
            } else {
                wire_node_lib::pyramid::auto_update_ops::unfreeze(
                    &conn, &state.pyramid.build_event_bus, &slug,
                ).map_err(|e: anyhow::Error| e.to_string())?;
            }
        }

        // Check if breaker should auto-clear
        let result = match pyramid_db::get_auto_update_config(&conn, &slug) {
            Some(config) => {
                if config.breaker_tripped
                    && !wire_node_lib::pyramid::watcher::check_runaway(&conn, &slug, &config)
                {
                    wire_node_lib::pyramid::auto_update_ops::resume_breaker(
                        &conn,
                        &state.pyramid.build_event_bus,
                        &slug,
                    )
                    .map_err(|e: anyhow::Error| e.to_string())?;
                    should_resume_breaker = true;
                }

                let refreshed =
                    pyramid_db::get_auto_update_config(&conn, &slug).unwrap_or(config);
                serde_json::to_value(&refreshed).map_err(|e| e.to_string())
            }
            None => Err(format!("No config for slug '{}'", slug)),
        };

        (result, should_resume_breaker)
    };

    if should_resume_breaker {
        let mut engines = state.pyramid.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug) {
            engine.resume_breaker();
        }
    }

    result
}

/// Idempotently start the DADBEAR stale engine + file watcher for a slug.
///
/// Called after pyramid creation (from `pyramid_create_slug` and the HTTP
/// `handle_create_slug` route) so newly-created pyramids begin stale
/// detection in the same session, not at next app restart. vine/question
/// content types short-circuit: they have no on-disk source to watch.
///
/// Assumes `pyramid_dadbear_config` has already been seeded by
/// `create_slug` — reads it back to construct the live engine config.
async fn ensure_dadbear_running(
    state: &SharedState,
    slug: &str,
) -> Result<(), String> {
    let slug_info = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?
    };

    let ct = slug_info.content_type.as_str();
    if ct == "question" || ct == "vine" {
        return Ok(());
    }

    let config = {
        let conn = state.pyramid.reader.lock().await;
        match pyramid_db::get_auto_update_config(&conn, slug) {
            Some(c) => c,
            None => return Ok(()),
        }
    };

    let mut engines = state.pyramid.stale_engines.lock().await;
    if engines.contains_key(slug) {
        return Ok(());
    }

    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .expect("data_dir not set")
        .join("pyramid.db")
        .to_string_lossy()
        .to_string();

    let (base_config, model, defer_maintenance) = {
        let cfg = state.pyramid.config.read().await;
        let stale_build_id = format!("stale-{}", slug);
        let with_cache = state
            .pyramid
            .attach_cache_access(cfg.clone(), slug, &stale_build_id);
        // walker-v3 W3a (Pattern 4): resolve via walker_resolver reading
        // the active walker_provider_openrouter contribution. Legacy
        // fallback removed by W3c once config.primary_model dies.
        let resolved = {
            let conn = state.pyramid.reader.lock().await;
            wire_node_lib::pyramid::walker_resolver::first_openrouter_model_from_db(&conn)
        };
        let model = resolved.unwrap_or_else(|| {
            tracing::warn!(
                event = "pattern4_no_openrouter_model",
                "walker-v3: Pattern-4 site found no walker_provider_openrouter model; stamping '<unknown>' — downstream dispatch will surface no-model-available",
            );
            "<unknown>".to_string()
        });
        let defer = cfg.dispatch_policy
            .as_ref()
            .map(|p| p.build_coordination.defer_maintenance_during_build)
            .unwrap_or(false);
        (with_cache, model, defer)
    };

    let mut engine = wire_node_lib::pyramid::stale_engine::PyramidStaleEngine::new(
        slug,
        config,
        &db_path,
        base_config,
        &model,
        state.pyramid.operational.as_ref().clone(),
        state.pyramid.build_event_bus.clone(),
        state.pyramid.active_build.clone(),
        defer_maintenance,
    );
    engine.start_poll_loop();
    engines.insert(slug.to_string(), engine);
    tracing::info!(slug = %slug, "DADBEAR engine started via ensure_dadbear_running");

    drop(engines);
    let source_paths: Vec<String> =
        wire_node_lib::pyramid::slug::resolve_validated_source_paths(
            &slug_info.source_path,
            &slug_info.content_type,
            state.pyramid.data_dir.as_deref(),
        )
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    if !source_paths.is_empty() {
        let mut watcher =
            wire_node_lib::pyramid::watcher::PyramidFileWatcher::new(
                slug,
                source_paths,
                &state.pyramid.operational.tier2,
            );
        let (mutation_tx, mut mutation_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, i32)>();
        watcher.set_mutation_sender(mutation_tx);

        let ps = state.pyramid.clone();
        tokio::spawn(async move {
            while let Some((s, layer)) = mutation_rx.recv().await {
                let mut engs = ps.stale_engines.lock().await;
                if let Some(eng) = engs.get_mut(&s) {
                    eng.notify_mutation(layer);
                }
            }
        });

        match watcher.start(&db_path) {
            Ok(()) => {
                tracing::info!(slug = %slug, "File watcher started via ensure_dadbear_running");
                let mut watchers = state.pyramid.file_watchers.lock().await;
                watchers.insert(slug.to_string(), watcher);
            }
            Err(e) => {
                tracing::warn!(slug = %slug, error = %e, "File watcher failed to start via ensure_dadbear_running");
            }
        }
    }

    Ok(())
}

#[tauri::command]
async fn pyramid_auto_update_freeze(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let mut engines = state.pyramid.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug) {
        engine.freeze();
    } else {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::auto_update_ops::freeze(&conn, &state.pyramid.build_event_bus, &slug)
            .map_err(|e: anyhow::Error| e.to_string())?;
        // Old WAL drain removed — holds projection anti-join prevents dispatch while frozen.
    }
    let mut watchers = state.pyramid.file_watchers.lock().await;
    if let Some(watcher) = watchers.get_mut(&slug) {
        watcher.pause();
    }
    Ok(serde_json::json!({"status": "frozen", "slug": slug}))
}

#[tauri::command]
async fn pyramid_auto_update_unfreeze(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let mut engines = state.pyramid.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug) {
        engine.unfreeze();
    } else {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::auto_update_ops::unfreeze(&conn, &state.pyramid.build_event_bus, &slug)
            .map_err(|e: anyhow::Error| e.to_string())?;
    }
    drop(engines);

    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .expect("data_dir not set")
        .join("pyramid.db")
        .to_string_lossy()
        .to_string();
    let mut watchers = state.pyramid.file_watchers.lock().await;
    if let Some(watcher) = watchers.get_mut(&slug) {
        watcher.resume(&db_path);
    }

    Ok(serde_json::json!({"status": "unfrozen", "slug": slug}))
}

#[tauri::command]
async fn pyramid_auto_update_status(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    match pyramid_db::get_auto_update_status(&conn, &slug).map_err(|e| e.to_string())? {
        Some(mut status) => {
            // Enrich with phase tracking fields from the live engine
            let engines = state.pyramid.stale_engines.lock().await;
            if let Some(engine) = engines.get(&slug) {
                let phase = engine.current_phase.lock().unwrap().clone();
                let phase_detail = engine.phase_detail.lock().unwrap().clone();
                let timer_fires_at = engine.timer_fires_at.lock().unwrap().clone();
                let last_result_summary = engine.last_result_summary.lock().unwrap().clone();
                status["phase"] = serde_json::json!(phase);
                status["phase_detail"] = serde_json::json!(phase_detail);
                status["timer_fires_at"] = serde_json::json!(timer_fires_at);
                status["last_result_summary"] = serde_json::json!(last_result_summary);
            } else {
                status["phase"] = serde_json::json!("idle");
                status["phase_detail"] = serde_json::json!(null);
                status["timer_fires_at"] = serde_json::json!(null);
                status["last_result_summary"] = serde_json::json!(null);
            }
            Ok(status)
        }
        None => Err(format!("No auto-update config for slug '{}'", slug)),
    }
}

#[tauri::command]
async fn pyramid_stale_log(
    state: tauri::State<'_, SharedState>,
    slug: String,
    limit: Option<i64>,
    layer: Option<i32>,
    stale_only: Option<bool>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.pyramid.reader.lock().await;
    // Three-state filter: Some(true) = stale only, Some(false) = not-stale only, None = all
    let stale_filter = match stale_only {
        Some(true) => Some("yes"),
        Some(false) => Some("no"),
        None => None,
    };
    pyramid_db::get_stale_log(&conn, &slug, layer, stale_filter, limit.unwrap_or(100), 0)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_cost_summary(
    state: tauri::State<'_, SharedState>,
    slug: String,
    window: Option<String>,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_db::get_cost_summary(&conn, &slug, window.as_deref()).map_err(|e| e.to_string())
}

// ── Phase 13: Build Viz Expansion IPC ────────────────────────────────

/// Phase 13: fetch every cache entry for a given build so the
/// frontend can pre-populate the step timeline on mount. Used when a
/// user opens a running build viz — the viz seeds its step timeline
/// from the cache table, then listens for live events going forward.
///
/// `build_id` is optional. When absent, the backend resolves the
/// latest build for the slug by walking `pyramid_step_cache` newest-
/// first. This is the common path for the PyramidBuildViz "open on
/// the current/latest build" flow where the UI has no build_id in
/// scope at mount time.
#[tauri::command]
async fn pyramid_step_cache_for_build(
    state: tauri::State<'_, SharedState>,
    slug: String,
    build_id: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::db::CacheEntrySummary>, String> {
    let conn = state.pyramid.reader.lock().await;
    match build_id.as_deref() {
        Some(bid) if !bid.is_empty() => {
            pyramid_db::list_cache_entries_for_build(&conn, &slug, bid)
                .map_err(|e| e.to_string())
        }
        _ => pyramid_db::list_cache_entries_for_latest_build(&conn, &slug)
            .map_err(|e| e.to_string()),
    }
}

/// Phase 13: reroll a node or intermediate cache entry with a
/// user-provided note. Exactly one of `node_id` or `cache_key` must
/// be provided. Returns the new cache entry id, any manifest id
/// that was written, and the new content.
#[tauri::command]
async fn pyramid_reroll_node(
    state: tauri::State<'_, SharedState>,
    slug: String,
    node_id: Option<String>,
    cache_key: Option<String>,
    note: String,
    force_fresh: Option<bool>,
) -> Result<wire_node_lib::pyramid::reroll::RerollOutput, String> {
    use wire_node_lib::pyramid::reroll::{reroll_node, RerollInput};

    let input = RerollInput {
        slug: slug.clone(),
        node_id,
        cache_key,
        note,
        force_fresh: force_fresh.unwrap_or(true),
    };

    // Attach the cache plumbing (DB path + bus) to the LlmConfig so
    // the reroll's LLM call flows through the content-addressable
    // cache and emits the full event set.
    let build_id = format!(
        "reroll-{}-{}",
        slug,
        chrono::Utc::now().timestamp_millis()
    );
    let llm_config = state.pyramid.llm_config_with_cache(&slug, &build_id).await;

    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db").to_string_lossy().into_owned())
        .ok_or_else(|| "reroll: no data_dir configured".to_string())?;

    let bus = state.pyramid.build_event_bus.clone();
    reroll_node(input, llm_config, db_path, bus)
        .await
        .map_err(|e| e.to_string())
}

/// Phase 13: list every active build across every slug. Seeds the
/// CrossPyramidTimeline frontend on mount; subsequent updates flow
/// via the `cross-build-event` Tauri channel.
///
/// Progress counts (`completed_steps` / `total_steps`) are read directly
/// from each `BuildHandle`'s live status — the same source that
/// `pyramid_build_progress_v2` exposes to the pyramid surface drawer.
/// This guarantees the Builds tab mirrors the per-pyramid "done/total"
/// that the drawer shows (e.g. "source_extract 7/21") instead of
/// re-deriving counts from downstream tables.
///
/// TODO(ui-debt/followup): this command only returns live builds. The
/// product direction is to evolve this surface into a durable,
/// topical history of ALL jobs (cross-pyramid chronicle mirror,
/// reverse-chronological, paginated). Needs a storage decision
/// (pyramid_builds? derive from cross-build-event log?), retention
/// strategy, and shared renderer with the per-pyramid Chronicle.
/// Out of scope for the UI-debt cleanup pass that added this comment.
#[tauri::command]
async fn pyramid_active_builds(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::pyramid::db::ActiveBuildRow>, String> {
    let active_map = state.pyramid.active_build.read().await;
    if active_map.is_empty() {
        return Ok(Vec::new());
    }

    let conn = state.pyramid.reader.lock().await;
    let mut rows: Vec<wire_node_lib::pyramid::db::ActiveBuildRow> =
        Vec::with_capacity(active_map.len());

    for (slug, handle) in active_map.iter() {
        let status_guard = handle.status.read().await;
        let status = status_guard.status.clone();
        let started_at = format!(
            "{}s ago",
            handle.started_at.elapsed().as_secs()
        );
        let build_id = status_guard.slug.clone();
        let completed_steps = status_guard.progress.done;
        let total_steps = status_guard.progress.total;
        drop(status_guard);

        // current_step lives on layer_state, not status. Read it
        // separately so the Builds tab can show per-step context
        // matching the drawer.
        let current_step = {
            let layer_state = handle.layer_state.read().await;
            layer_state.current_step.clone()
        };

        match pyramid_db::build_active_build_summary(
            &conn,
            slug,
            &build_id,
            &status,
            &started_at,
            current_step.as_deref(),
            completed_steps,
            total_steps,
        ) {
            Ok(row) => rows.push(row),
            Err(e) => {
                tracing::warn!(
                    slug = %slug,
                    error = %e,
                    "failed to build active build summary row"
                );
            }
        }
    }

    Ok(rows)
}

#[derive(serde::Deserialize)]
struct CostRollupArgs {
    range: String,
    from: Option<String>,
    to: Option<String>,
}

#[derive(serde::Serialize)]
struct CostRollupResponse {
    total_estimated: f64,
    total_actual: f64,
    buckets: Vec<wire_node_lib::pyramid::db::CostRollupBucket>,
    from: String,
    to: String,
}

/// Phase 13: aggregate `pyramid_cost_log` across all slugs for a
/// given date range. The range parameter accepts `today`, `week`,
/// `month`, or `custom` (with explicit `from` / `to` ISO strings).
/// The frontend pivots the returned buckets into three views
/// (by pyramid / by provider / by operation).
#[tauri::command]
async fn pyramid_cost_rollup(
    state: tauri::State<'_, SharedState>,
    range: String,
    from: Option<String>,
    to: Option<String>,
) -> Result<serde_json::Value, String> {
    let args = CostRollupArgs { range, from, to };
    let (from_iso, to_iso) = resolve_cost_rollup_range(&args).map_err(|e| e.to_string())?;

    let conn = state.pyramid.reader.lock().await;
    let buckets = pyramid_db::cost_rollup(&conn, &from_iso, &to_iso)
        .map_err(|e| e.to_string())?;
    drop(conn);

    let total_estimated: f64 = buckets.iter().map(|b| b.estimated).sum();
    let total_actual: f64 = buckets.iter().map(|b| b.actual).sum();

    let resp = CostRollupResponse {
        total_estimated,
        total_actual,
        buckets,
        from: from_iso,
        to: to_iso,
    };

    serde_json::to_value(&resp).map_err(|e| e.to_string())
}

/// Parse the IPC's `range` parameter into an ISO `(from, to)`
/// window pair. Custom ranges are capped at 1 year per the spec.
fn resolve_cost_rollup_range(args: &CostRollupArgs) -> anyhow::Result<(String, String)> {
    use chrono::{Duration, Utc};
    let now = Utc::now();
    let (from, to) = match args.range.as_str() {
        "today" => {
            let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
            (
                chrono::DateTime::<Utc>::from_naive_utc_and_offset(start, Utc),
                now,
            )
        }
        "week" => (now - Duration::days(7), now),
        "month" => (now - Duration::days(30), now),
        "custom" => {
            let from_str = args
                .from
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("range=custom requires `from`"))?;
            let to_str = args
                .to
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("range=custom requires `to`"))?;
            let from_dt = chrono::DateTime::parse_from_rfc3339(from_str)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| anyhow::anyhow!("invalid `from` ISO timestamp: {}", e))?;
            let to_dt = chrono::DateTime::parse_from_rfc3339(to_str)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| anyhow::anyhow!("invalid `to` ISO timestamp: {}", e))?;
            if to_dt - from_dt > Duration::days(366) {
                return Err(anyhow::anyhow!(
                    "custom range exceeds 1-year cap — split the query"
                ));
            }
            if to_dt <= from_dt {
                return Err(anyhow::anyhow!("custom range has `to` <= `from`"));
            }
            (from_dt, to_dt)
        }
        other => {
            return Err(anyhow::anyhow!(
                "unknown range `{}` (expected today|week|month|custom)",
                other
            ))
        }
    };
    Ok((from.format("%Y-%m-%d %H:%M:%S").to_string(), to.format("%Y-%m-%d %H:%M:%S").to_string()))
}

/// Phase 13 + Phase 18c: bulk-pause DADBEAR across pyramids matching
/// Phase 7 (canonical): pause delegates to freeze_all via the holds projection.
/// The old `enabled` column approach is removed — pausing now places a 'frozen' hold.
///
/// Scopes (per `cross-pyramid-observability.md` "Pause-All Semantics"):
/// - `"all"` — freeze every unfrozen pyramid (Phase 13)
/// - `"folder"` — freeze pyramids whose `source_path` is exactly `scope_value`
///   or a descendant. `scope_value` is required. (Phase 18c L9)
/// - `"circle"` — DEFERRED. The local DB has no `pyramid_metadata`
///   table with `circle_id` to query against; circle membership lives
///   only in the Wire JWT claim layer. Returns an error pointing the
///   caller at the deferral note.
#[tauri::command]
async fn pyramid_pause_dadbear_all(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
) -> Result<serde_json::Value, String> {
    if scope == "circle" {
        return Err(
            "pyramid_pause_dadbear_all: scope `circle` is deferred — local DB has no \
             circle_id column on pyramid_metadata. Tracked in deferral-ledger.md as a \
             follow-up to Phase 18c."
                .to_string(),
        );
    }
    // Delegate to freeze_all which uses holds projection (canonical path).
    pyramid_freeze_all(state, scope, scope_value).await
}

/// Phase 7 (canonical): resume delegates to unfreeze_all via the holds projection.
/// The old `enabled` column approach is removed — resuming clears the 'frozen' hold.
#[tauri::command]
async fn pyramid_resume_dadbear_all(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
) -> Result<serde_json::Value, String> {
    if scope == "circle" {
        return Err(
            "pyramid_resume_dadbear_all: scope `circle` is deferred — see \
             deferral-ledger.md for the follow-up note."
                .to_string(),
        );
    }
    // Delegate to unfreeze_all which uses holds projection (canonical path).
    pyramid_unfreeze_all(state, scope, scope_value).await
}

/// Phase 18c (L9): list distinct `source_path` values across all
/// DADBEAR configs. Powers the folder dropdown in the Pause All scope
/// picker. Sorted alphabetically; duplicates collapsed.
#[tauri::command]
async fn pyramid_list_dadbear_source_paths(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<String>, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_db::list_dadbear_source_paths(&conn).map_err(|e| e.to_string())
}

/// Phase 18c (L9): live count helper for the scope picker. Returns
/// the number of rows that WOULD be flipped by a pause-all call with
/// the given scope, without mutating any rows. The frontend calls
/// this every time the user changes the scope picker so the
/// confirmation modal can show "Pause N pyramid(s)" with the right N.
///
/// `target_state = "pause"` counts rows currently enabled (would
/// flip to disabled). `target_state = "resume"` counts rows currently
/// disabled. Anything else is an error.
#[tauri::command]
async fn pyramid_count_dadbear_scope(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
    target_state: String,
) -> Result<serde_json::Value, String> {
    let target_pause = match target_state.as_str() {
        "pause" => true,
        "resume" => false,
        other => {
            return Err(format!(
                "pyramid_count_dadbear_scope: unknown target_state `{}` (expected pause|resume)",
                other
            ))
        }
    };
    if !matches!(scope.as_str(), "all" | "folder" | "circle") {
        return Err(format!(
            "pyramid_count_dadbear_scope: unknown scope `{}` (expected all|folder|circle)",
            scope
        ));
    }
    let conn = state.pyramid.reader.lock().await;
    let count = pyramid_db::count_dadbear_scope(
        &conn,
        &scope,
        scope_value.as_deref(),
        target_pause,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "count": count }))
}

// ── Phase 7: Legacy v1 oversight handlers removed ──────────────────────────
//
// The following v1 handlers were removed in Phase 7 (legacy cleanup):
//   - `pyramid_dadbear_overview` → replaced by `pyramid_dadbear_overview_v2`
//   - `pyramid_dadbear_activity_log` → replaced by `pyramid_dadbear_activity_v2`
//
// The v1 types (DadbearOverviewRow, DadbearOverviewTotals, DadbearOverviewResponse,
// DadbearActivityEntry) and their handlers read from legacy tables
// (pyramid_auto_update_config, pyramid_pending_mutations).
// The v2 handlers read from canonical tables (dadbear_work_items,
// dadbear_holds_projection, dadbear_observation_events, etc.).

/// Shared activity entry struct used by the v2 activity handler.
/// (Originally defined alongside the v1 handlers, moved here in Phase 7.)
#[derive(serde::Serialize)]
struct DadbearActivityEntry {
    timestamp: String,
    event_type: String,
    slug: String,
    target_id: Option<String>,
    details: Option<String>,
}

// ── Phase 6 (Canonical): Work-item-centric overview + activity v2 ──────────
//
// New IPC handlers that read from the canonical tables (dadbear_work_items,
// dadbear_holds_projection, dadbear_observation_events, dadbear_dispatch_previews,
// dadbear_work_attempts, dadbear_compilation_state). Coexist alongside the old
// handlers until Phase 7 drops legacy tables.

#[derive(serde::Serialize)]
struct WorkItemOverviewHold {
    hold: String,
    held_since: String,
    reason: Option<String>,
}

#[derive(serde::Serialize)]
struct WorkItemOverviewRow {
    slug: String,
    display_name: String,
    holds: Vec<WorkItemOverviewHold>,
    derived_status: String, // 'active' | 'paused' | 'breaker' | 'held'
    epoch_id: String,
    recipe_version: Option<String>,
    pending_observations: i64,
    compiled_items: i64,
    blocked_items: i64,
    previewed_items: i64,
    dispatched_items: i64,
    completed_items_24h: i64,
    applied_items_24h: i64,
    failed_items_24h: i64,
    stale_items: i64,
    preview_total_cost_usd: f64,
    actual_cost_24h_usd: f64,
    last_compilation_at: Option<String>,
    last_dispatch_at: Option<String>,
}

#[derive(serde::Serialize)]
struct WorkItemOverviewTotals {
    active_count: i64,
    paused_count: i64,
    breaker_count: i64,
    total_compiled: i64,
    total_dispatched: i64,
    total_blocked: i64,
    total_cost_24h_usd: f64,
}

#[derive(serde::Serialize)]
struct WorkItemOverviewResponse {
    pyramids: Vec<WorkItemOverviewRow>,
    totals: WorkItemOverviewTotals,
}

/// Phase 6 (Canonical): work-item-centric overview. Reads from the canonical
/// tables (holds_projection, work_items, dispatch_previews, work_attempts,
/// compilation_state) instead of the legacy tables. Returns `WorkItemOverviewRow`
/// per configured slug.
#[tauri::command]
async fn pyramid_dadbear_overview_v2(
    state: tauri::State<'_, SharedState>,
) -> Result<WorkItemOverviewResponse, String> {
    let conn = state.pyramid.reader.lock().await;

    // 1. ALL non-archived slugs (from pyramid_slugs, the canonical source).
    // Not just pyramid_dadbear_config — that's only the watch config cache
    // and misses pyramids that have builds/costs but no DADBEAR watch setup.
    let slugs: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT slug FROM pyramid_slugs WHERE slug NOT LIKE '%--bunch-%' AND archived_at IS NULL ORDER BY slug")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let collected: Vec<_> = rows.filter_map(|r| r.ok()).collect();
        collected
    };

    let mut pyramids: Vec<WorkItemOverviewRow> = Vec::with_capacity(slugs.len());
    let mut total_active: i64 = 0;
    let mut total_paused: i64 = 0;
    let mut total_breaker: i64 = 0;
    let mut total_compiled: i64 = 0;
    let mut total_dispatched: i64 = 0;
    let mut total_blocked: i64 = 0;
    let mut total_cost_24h: f64 = 0.0;

    for slug in &slugs {
        // 2. Active holds for this slug
        let holds: Vec<WorkItemOverviewHold> = {
            let mut stmt = conn
                .prepare(
                    "SELECT hold, held_since, reason FROM dadbear_holds_projection WHERE slug = ?1",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![slug], |row| {
                    Ok(WorkItemOverviewHold {
                        hold: row.get(0)?,
                        held_since: row.get(1)?,
                        reason: row.get(2)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            let collected: Vec<_> = rows.filter_map(|r| r.ok()).collect();
            collected
        };

        // Derive status from holds: breaker > frozen/cost_limit > active
        let has_breaker = holds.iter().any(|h| h.hold == "breaker");
        let has_frozen = holds.iter().any(|h| h.hold == "frozen");
        let derived_status = if has_breaker {
            "breaker"
        } else if has_frozen {
            "paused"
        } else if !holds.is_empty() {
            "held"
        } else {
            "active"
        };

        match derived_status {
            "breaker" => total_breaker += 1,
            "paused" | "held" => total_paused += 1,
            _ => total_active += 1,
        }

        // 3. Compilation state (epoch + recipe)
        let (epoch_id, recipe_version, last_compiled_obs_id): (String, Option<String>, i64) = conn
            .query_row(
                "SELECT epoch_id, recipe_contribution_id, COALESCE(last_compiled_observation_id, 0)
                 FROM dadbear_compilation_state WHERE slug = ?1",
                rusqlite::params![slug],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or_else(|_| ("none".to_string(), None, 0));

        // 4. Pending observations (above the compilation cursor)
        let pending_observations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_observation_events
                 WHERE slug = ?1 AND id > ?2",
                rusqlite::params![slug, last_compiled_obs_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // 5. Work item counts by state
        let compiled_items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items WHERE slug = ?1 AND state = 'compiled'",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let blocked_items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items WHERE slug = ?1 AND state = 'blocked'",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let previewed_items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items WHERE slug = ?1 AND state = 'previewed'",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let dispatched_items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items WHERE slug = ?1 AND state = 'dispatched'",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let stale_items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items WHERE slug = ?1 AND state = 'stale'",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // 24h window items
        let completed_items_24h: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items
                 WHERE slug = ?1 AND state = 'completed'
                   AND completed_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let applied_items_24h: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items
                 WHERE slug = ?1 AND state = 'applied'
                   AND applied_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let failed_items_24h: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_items
                 WHERE slug = ?1 AND state = 'failed'
                   AND state_changed_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // 6. Preview cost (committed previews, last 24h)
        let preview_total_cost_usd: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(total_cost_usd), 0.0) FROM dadbear_dispatch_previews
                 WHERE slug = ?1 AND committed_at IS NOT NULL
                   AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        // 7. Actual cost from work attempts (completed, last 24h)
        let actual_cost_24h_usd: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM dadbear_work_attempts
                 WHERE work_item_id IN (SELECT id FROM dadbear_work_items WHERE slug = ?1)
                   AND status = 'completed'
                   AND completed_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        // 8. Timing
        let last_compilation_at: Option<String> = conn
            .query_row(
                "SELECT MAX(compiled_at) FROM dadbear_work_items WHERE slug = ?1",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let last_dispatch_at: Option<String> = conn
            .query_row(
                "SELECT MAX(dispatched_at) FROM dadbear_work_attempts
                 WHERE work_item_id IN (SELECT id FROM dadbear_work_items WHERE slug = ?1)",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(None);

        total_compiled += compiled_items;
        total_dispatched += dispatched_items;
        total_blocked += blocked_items;
        total_cost_24h += actual_cost_24h_usd;

        pyramids.push(WorkItemOverviewRow {
            slug: slug.clone(),
            display_name: slug.clone(),
            holds,
            derived_status: derived_status.to_string(),
            epoch_id,
            recipe_version,
            pending_observations,
            compiled_items,
            blocked_items,
            previewed_items,
            dispatched_items,
            completed_items_24h,
            applied_items_24h,
            failed_items_24h,
            stale_items,
            preview_total_cost_usd,
            actual_cost_24h_usd,
            last_compilation_at,
            last_dispatch_at,
        });
    }

    Ok(WorkItemOverviewResponse {
        pyramids,
        totals: WorkItemOverviewTotals {
            active_count: total_active,
            paused_count: total_paused,
            breaker_count: total_breaker,
            total_compiled,
            total_dispatched,
            total_blocked,
            total_cost_24h_usd: total_cost_24h,
        },
    })
}

/// Phase 6 (Canonical): unified activity timeline for a single slug.
/// Reads from the canonical event streams (observation_events, work_items,
/// work_attempts, hold_events) instead of legacy tables.
#[tauri::command]
async fn pyramid_dadbear_activity_v2(
    state: tauri::State<'_, SharedState>,
    slug: String,
    limit: Option<i64>,
) -> Result<Vec<DadbearActivityEntry>, String> {
    let conn = state.pyramid.reader.lock().await;
    let limit = limit.unwrap_or(100).clamp(1, 500);

    let mut entries: Vec<DadbearActivityEntry> = Vec::new();

    // 1. Recent observation events
    {
        let mut stmt = conn
            .prepare(
                "SELECT detected_at, event_type, source, source_path, file_path, target_node_id
                 FROM dadbear_observation_events
                 WHERE slug = ?1
                 ORDER BY detected_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, limit], |row| {
                let detected_at: String = row.get(0)?;
                let event_type: String = row.get(1)?;
                let source: String = row.get(2)?;
                let source_path: Option<String> = row.get(3)?;
                let file_path: Option<String> = row.get(4)?;
                let target_node_id: Option<String> = row.get(5)?;
                Ok(DadbearActivityEntry {
                    timestamp: detected_at,
                    event_type: format!("observation_{}", event_type),
                    slug: slug.clone(),
                    target_id: target_node_id.or(file_path),
                    details: Some(
                        serde_json::json!({
                            "source": source,
                            "source_path": source_path,
                        })
                        .to_string(),
                    ),
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
    }

    // 2. Recent work item state transitions
    {
        let mut stmt = conn
            .prepare(
                "SELECT state_changed_at, state, primitive, layer, target_id, id
                 FROM dadbear_work_items
                 WHERE slug = ?1
                 ORDER BY state_changed_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, limit], |row| {
                let changed_at: String = row.get(0)?;
                let state: String = row.get(1)?;
                let primitive: String = row.get(2)?;
                let layer: i64 = row.get(3)?;
                let target_id: Option<String> = row.get(4)?;
                let work_item_id: String = row.get(5)?;
                Ok(DadbearActivityEntry {
                    timestamp: changed_at,
                    event_type: format!("work_item_{}", state),
                    slug: slug.clone(),
                    target_id,
                    details: Some(
                        serde_json::json!({
                            "primitive": primitive,
                            "layer": layer,
                            "work_item_id": work_item_id,
                        })
                        .to_string(),
                    ),
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
    }

    // 3. Recent dispatch attempts
    {
        let mut stmt = conn
            .prepare(
                "SELECT a.dispatched_at, a.status, a.model_id, a.routing, a.cost_usd,
                        a.work_item_id, a.error
                 FROM dadbear_work_attempts a
                 JOIN dadbear_work_items w ON a.work_item_id = w.id
                 WHERE w.slug = ?1
                 ORDER BY a.dispatched_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, limit], |row| {
                let dispatched_at: String = row.get(0)?;
                let status: String = row.get(1)?;
                let model_id: String = row.get(2)?;
                let routing: String = row.get(3)?;
                let cost_usd: Option<f64> = row.get(4)?;
                let work_item_id: String = row.get(5)?;
                let error: Option<String> = row.get(6)?;
                Ok(DadbearActivityEntry {
                    timestamp: dispatched_at,
                    event_type: format!("attempt_{}", status),
                    slug: slug.clone(),
                    target_id: Some(work_item_id),
                    details: Some(
                        serde_json::json!({
                            "model": model_id,
                            "routing": routing,
                            "cost_usd": cost_usd,
                            "error": error,
                        })
                        .to_string(),
                    ),
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
    }

    // 4. Recent hold events
    {
        let mut stmt = conn
            .prepare(
                "SELECT created_at, hold, action, reason
                 FROM dadbear_hold_events
                 WHERE slug = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, limit], |row| {
                let created_at: String = row.get(0)?;
                let hold: String = row.get(1)?;
                let action: String = row.get(2)?;
                let reason: Option<String> = row.get(3)?;
                Ok(DadbearActivityEntry {
                    timestamp: created_at,
                    event_type: format!("hold_{}", action),
                    slug: slug.clone(),
                    target_id: Some(hold),
                    details: reason.map(|r| {
                        serde_json::json!({ "reason": r }).to_string()
                    }),
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
    }

    // Sort merged events by timestamp DESC and truncate.
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit as usize);
    Ok(entries)
}

/// Per-slug pause. Freezes via `auto_update_ops::freeze` so the per-card
/// and global pause share the same mechanism (holds projection).
/// Also syncs in-memory stale engine + file watcher, matching pyramid_freeze_all.
#[tauri::command]
async fn pyramid_dadbear_pause(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::auto_update_ops::freeze(
            &conn,
            &state.pyramid.build_event_bus,
            &slug,
        )
        .map_err(|e| e.to_string())?;
    }
    // Sync in-memory state
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug) {
            engine.freeze();
        }
    }
    {
        let mut watchers = state.pyramid.file_watchers.lock().await;
        if let Some(watcher) = watchers.get_mut(&slug) {
            watcher.pause();
        }
    }
    Ok(serde_json::json!({ "ok": true, "affected": 1 }))
}

/// Per-slug resume. Unfreezes via `auto_update_ops::unfreeze` — mirror of
/// `pyramid_dadbear_pause`. Syncs stale engine + file watcher.
#[tauri::command]
async fn pyramid_dadbear_resume(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::auto_update_ops::unfreeze(
            &conn,
            &state.pyramid.build_event_bus,
            &slug,
        )
        .map_err(|e| e.to_string())?;
    }
    // Sync in-memory state
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug) {
            engine.unfreeze();
        }
    }
    {
        let db_path = state
            .pyramid
            .data_dir
            .as_ref()
            .expect("data_dir not set")
            .join("pyramid.db")
            .to_string_lossy()
            .to_string();
        let mut watchers = state.pyramid.file_watchers.lock().await;
        if let Some(watcher) = watchers.get_mut(&slug) {
            watcher.resume(&db_path);
        }
    }
    Ok(serde_json::json!({ "ok": true, "affected": 1 }))
}

/// Phase 15: acknowledge a single orphan broadcast row after review.
/// Stamps `acknowledged_at` + `acknowledgment_reason` so the Oversight
/// page can stop surfacing the row in its red-banner counter. The
/// counterpart to Phase 11's `pyramid_list_orphan_broadcasts`.
#[tauri::command]
async fn pyramid_acknowledge_orphan_broadcast(
    state: tauri::State<'_, SharedState>,
    orphan_id: i64,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.writer.lock().await;
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let affected = conn
        .execute(
            "UPDATE pyramid_orphan_broadcasts
                SET acknowledged_at = ?1,
                    acknowledgment_reason = ?2
              WHERE id = ?3 AND acknowledged_at IS NULL",
            rusqlite::params![now, reason, orphan_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": true, "affected": affected }))
}

// ── WS-3: Evidence Density ──────────────────────────────────────────────────

#[tauri::command]
async fn pyramid_evidence_density(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_db::get_evidence_density(&conn, &slug).map_err(|e| e.to_string())
}

// ── Live Pyramid Theatre: Audit IPC Commands ────────────────────────────────

#[tauri::command]
async fn pyramid_build_live_nodes(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<Vec<wire_node_lib::pyramid::types::LiveNodeInfo>, String> {
    let conn = state.pyramid.reader.lock().await;
    // build_id not needed — returns all non-superseded nodes for the slug
    pyramid_db::get_build_live_nodes(&conn, &slug, "").map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_node_audit(
    state: tauri::State<'_, SharedState>,
    slug: String,
    node_id: String,
) -> Result<Vec<wire_node_lib::pyramid::types::LlmAuditRecord>, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_db::get_node_audit_records(&conn, &slug, &node_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_audit_by_id(
    state: tauri::State<'_, SharedState>,
    audit_id: i64,
) -> Result<Option<wire_node_lib::pyramid::types::LlmAuditRecord>, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_db::get_llm_audit_by_id(&conn, audit_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_audit_cleanup(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.writer.lock().await;
    let deleted = pyramid_db::cleanup_old_audit_records(&conn, &slug).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"deleted": deleted, "slug": slug}))
}

#[tauri::command]
async fn pyramid_breaker_resume(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let mut engines = state.pyramid.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug) {
        engine.resume_breaker();
        Ok(serde_json::json!({"status": "resumed", "slug": slug}))
    } else {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::auto_update_ops::resume_breaker(&conn, &state.pyramid.build_event_bus, &slug)
            .map_err(|e: anyhow::Error| e.to_string())?;
        Ok(
            serde_json::json!({"status": "resumed", "slug": slug, "note": "No active engine, breaker cleared in DB"}),
        )
    }
}

#[tauri::command]
async fn pyramid_freeze_all(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
) -> Result<serde_json::Value, String> {
    let affected_slugs = {
        let conn = state.pyramid.writer.lock().await;
        let slugs = wire_node_lib::pyramid::auto_update_ops::freeze_all(
            &conn,
            &state.pyramid.build_event_bus,
            &scope,
            scope_value.as_deref(),
        )
        .map_err(|e: anyhow::Error| e.to_string())?;
        // Drain pending mutations for slugs without an in-memory engine.
        // (Engines drain their own WAL inside engine.freeze() below.)
        // Must happen while we still hold the writer connection.
        let engines = state.pyramid.stale_engines.lock().await;
        // Old WAL drain removed — holds projection anti-join prevents dispatch while frozen.
        drop(engines);
        slugs
        // conn (writer lock) dropped here
    };

    // Ghost-engine fix: freeze in-memory stale engines + pause file watchers
    // for every slug that was actually frozen. Without this, the stale engine
    // poll loop continues to fire drain_and_dispatch for already-debounced
    // mutations despite the DB-level freeze.
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        for slug in &affected_slugs {
            if let Some(engine) = engines.get_mut(slug) {
                engine.freeze();
            }
        }
    }
    {
        let mut watchers = state.pyramid.file_watchers.lock().await;
        for slug in &affected_slugs {
            if let Some(watcher) = watchers.get_mut(slug) {
                watcher.pause();
            }
        }
    }

    let affected = affected_slugs.len();
    Ok(serde_json::json!({ "affected": affected }))
}

#[tauri::command]
async fn pyramid_unfreeze_all(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.writer.lock().await;
    let affected_slugs = wire_node_lib::pyramid::auto_update_ops::unfreeze_all(
        &conn,
        &state.pyramid.build_event_bus,
        &scope,
        scope_value.as_deref(),
    )
    .map_err(|e: anyhow::Error| e.to_string())?;
    drop(conn);

    // Ghost-engine fix: unfreeze in-memory stale engines + resume file
    // watchers for every slug that was actually unfrozen. Without this,
    // the stale engine stays frozen in memory and ignores new mutations.
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        for slug in &affected_slugs {
            if let Some(engine) = engines.get_mut(slug) {
                engine.unfreeze();
            }
        }
    }
    {
        let db_path = state
            .pyramid
            .data_dir
            .as_ref()
            .expect("data_dir not set")
            .join("pyramid.db")
            .to_string_lossy()
            .to_string();
        let mut watchers = state.pyramid.file_watchers.lock().await;
        for slug in &affected_slugs {
            if let Some(watcher) = watchers.get_mut(slug) {
                watcher.resume(&db_path);
            }
        }
    }

    let affected = affected_slugs.len();
    Ok(serde_json::json!({ "affected": affected }))
}

#[tauri::command]
async fn pyramid_count_freeze_scope(
    state: tauri::State<'_, SharedState>,
    scope: String,
    scope_value: Option<String>,
    target_state: String,
) -> Result<serde_json::Value, String> {
    let target_frozen = target_state == "freeze"; // "freeze" = count unfrozen ones (would be frozen)
    let conn = state.pyramid.reader.lock().await;
    let count = wire_node_lib::pyramid::auto_update_ops::count_freeze_scope(
        &conn,
        &scope,
        scope_value.as_deref(),
        target_frozen,
    )
    .map_err(|e: anyhow::Error| e.to_string())?;
    Ok(serde_json::json!({ "count": count }))
}

#[tauri::command]
async fn pyramid_dadbear_configs_for_slug(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let conn = state.pyramid.reader.lock().await;
    let configs =
        pyramid_db::get_dadbear_configs(&conn, &slug).map_err(|e| e.to_string())?;
    serde_json::to_value(&configs).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_auto_update_run_now(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Guard: reject if frozen or breaker-tripped
    {
        let engines = state.pyramid.stale_engines.lock().await;
        if let Some(engine) = engines.get(&slug) {
            if engine.frozen || engine.breaker_tripped {
                return Err("Cannot run now: pyramid is frozen or breaker is tripped".into());
            }
        }
    }

    // Extract what we need from the engine while briefly holding the lock, then release.
    // Phase 3 retired `api_key` from PyramidStaleEngine in favor of
    // `base_config: LlmConfig`, which preserves the provider_registry
    // + credential_store runtime handles. The old `api_key`/`model`
    // call into drain_and_dispatch was left dead by the fix pass;
    // Phase 4 picks it up here under the "fix all bugs found" rule.
    let (db_path, base_config, model, semaphore, phase_arc, detail_arc, summary_arc, defer_maintenance) = {
        let engines = state.pyramid.stale_engines.lock().await;
        let engine = engines
            .get(&slug)
            .ok_or("No active stale engine for this pyramid")?;
        (
            engine.db_path.clone(),
            engine.base_config.clone(),
            engine.model.clone(),
            engine.concurrent_helpers.clone(),
            engine.current_phase.clone(),
            engine.phase_detail.clone(),
            engine.last_result_summary.clone(),
            engine.defer_maintenance_during_build.clone(),
        )
    };
    // Lock released here — no mutex held across LLM calls

    // Drain and dispatch each layer sequentially: L0 → L1 → L2 → L3
    for layer in 0..=3 {
        let _ = wire_node_lib::pyramid::stale_engine::drain_and_dispatch(
            &slug,
            layer,
            0,
            &db_path,
            semaphore.clone(),
            &base_config,
            &model,
            phase_arc.clone(),
            detail_arc.clone(),
            summary_arc.clone(),
            &state.pyramid.operational,
            Some(&state.pyramid.build_event_bus),
            state.pyramid.active_build.clone(),
            defer_maintenance.clone(),
        )
        .await;
    }
    Ok(serde_json::json!({"status": "completed", "slug": slug}))
}

#[tauri::command]
async fn pyramid_auto_update_l0_sweep(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Get slug info for reconciliation
    let (source_paths, content_type) = {
        let conn = state.pyramid.reader.lock().await;
        match wire_node_lib::pyramid::slug::get_slug(&conn, &slug) {
            Ok(Some(info)) => {
                let paths: Vec<String> = serde_json::from_str(&info.source_path)
                    .unwrap_or_else(|_| vec![info.source_path.clone()]);
                (paths, info.content_type.as_str().to_string())
            }
            _ => (Vec::new(), String::new()),
        }
    };
    let ingested_extensions: Vec<String> = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::db::get_ingested_extensions(&conn, &slug).unwrap_or_default()
    };

    let (tracked_files, enqueued, already_pending, r_new, r_changed, r_deleted) = {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::routes::enqueue_full_l0_sweep_with_reconciliation(
            &conn, &slug, &source_paths, &ingested_extensions, &content_type,
        )
    };

    // Phase 3 fix — `engine.api_key` is retired; use `base_config`.
    let (db_path, base_config, model, semaphore, phase_arc, detail_arc, summary_arc, defer_maintenance) = {
        let engines = state.pyramid.stale_engines.lock().await;
        let engine = engines
            .get(&slug)
            .ok_or("No active stale engine for this pyramid")?;
        (
            engine.db_path.clone(),
            engine.base_config.clone(),
            engine.model.clone(),
            engine.concurrent_helpers.clone(),
            engine.current_phase.clone(),
            engine.phase_detail.clone(),
            engine.last_result_summary.clone(),
            engine.defer_maintenance_during_build.clone(),
        )
    };

    for layer in 0..=3 {
        let _ = wire_node_lib::pyramid::stale_engine::drain_and_dispatch(
            &slug,
            layer,
            0,
            &db_path,
            semaphore.clone(),
            &base_config,
            &model,
            phase_arc.clone(),
            detail_arc.clone(),
            summary_arc.clone(),
            &state.pyramid.operational,
            Some(&state.pyramid.build_event_bus),
            state.pyramid.active_build.clone(),
            defer_maintenance.clone(),
        )
        .await;
    }

    Ok(serde_json::json!({
        "status": "completed",
        "slug": slug,
        "tracked_files": tracked_files,
        "enqueued": enqueued,
        "already_pending": already_pending,
        "reconciliation": {
            "new_files": r_new,
            "changed_files": r_changed,
            "deleted_files": r_deleted,
        },
    }))
}

#[tauri::command]
async fn pyramid_breaker_archive_and_rebuild(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Get old slug info
    let slug_info = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?
    };

    // Freeze old pyramid, remove engine and watcher
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug) {
            engine.freeze();
        }
        let mut watchers = state.pyramid.file_watchers.lock().await;
        if let Some(watcher) = watchers.get_mut(&slug) {
            watcher.stop();
        }
        watchers.remove(&slug);
        engines.remove(&slug);
    }

    // Create new slug with date suffix
    let date_suffix = chrono::Utc::now().format("%Y%m%d").to_string();
    let new_slug = format!("{}-{}", slug, date_suffix);

    {
        let conn = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::slug::create_slug(
            &conn,
            &new_slug,
            &slug_info.content_type,
            &slug_info.source_path,
        )
        .map_err(|e| e.to_string())?;
        // Old auto_update_config INSERT removed — table dropped.
        // Contribution existence in pyramid_dadbear_config is the enable gate.
    }

    Ok(serde_json::json!({
        "status": "created",
        "old_slug": slug,
        "new_slug": new_slug,
        "note": "Old pyramid archived. Call pyramid_build(new_slug) to start full build.",
    }))
}

// --- Annotations IPC Commands -------------------------------------------------

#[tauri::command]
async fn pyramid_annotations_recent(
    state: tauri::State<'_, SharedState>,
    slug: String,
    limit: Option<i64>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.pyramid.reader.lock().await;
    let lim = limit.unwrap_or(10);
    let mut stmt = conn
        .prepare(
            "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at
             FROM pyramid_annotations WHERE slug = ?1
             ORDER BY created_at DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![slug, lim], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "slug": row.get::<_, String>(1)?,
                "node_id": row.get::<_, String>(2)?,
                "annotation_type": row.get::<_, String>(3)?,
                "content": row.get::<_, String>(4)?,
                "question_context": row.get::<_, Option<String>>(5)?,
                "author": row.get::<_, String>(6)?,
                "created_at": row.get::<_, String>(7)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| e.to_string())?);
    }
    Ok(results)
}

// --- FAQ Directory IPC Commands -----------------------------------------------

#[tauri::command]
async fn pyramid_faq_directory(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Phase 3 fix pass: clone the live LlmConfig (with provider_registry +
    // credential_store) so faq::get_faq_directory stays on the registry path.
    // Phase 12 verifier fix: attach cache_access so faq retrofit sites
    // reach the step cache.
    let base_config = state
        .pyramid
        .llm_config_with_cache(&slug, &format!("faq-dir-{}", slug))
        .await;
    // walker-v3 W3a (Pattern 4): resolve via walker_resolver reading
    // the active walker_provider_openrouter contribution. Legacy
    // fallback removed by W3c once config.primary_model dies.
    let resolved = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::walker_resolver::first_openrouter_model_from_db(&conn)
    };
    let model = resolved.unwrap_or_else(|| {
        tracing::warn!(
            event = "pattern4_no_openrouter_model",
            "walker-v3: Pattern-4 site found no walker_provider_openrouter model; stamping '<unknown>'",
        );
        "<unknown>".to_string()
    });

    let directory = pyramid_faq::get_faq_directory(
        &state.pyramid.reader,
        &state.pyramid.writer,
        &slug,
        &base_config,
        &model,
        &state.pyramid.operational.tier2,
    )
    .await
    .map_err(|e| e.to_string())?;

    serde_json::to_value(&directory).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_faq_category_drill(
    state: tauri::State<'_, SharedState>,
    slug: String,
    category_id: String,
) -> Result<serde_json::Value, String> {
    let entry = pyramid_faq::drill_faq_category(&state.pyramid.reader, &slug, &category_id)
        .await
        .map_err(|e| e.to_string())?;

    serde_json::to_value(&entry).map_err(|e| e.to_string())
}

// --- Phase 3: Credential IPC commands ---------------------------------------
//
// Mirrors `docs/specs/credentials-and-secrets.md` §IPC Contract. These
// commands never return credential values over IPC — only masked
// previews, metadata, and status. Mutations accept plaintext values
// but those are immediately handed to the store's atomic write path
// and dropped from scope.

#[derive(serde::Serialize)]
struct CredentialPreview {
    key: String,
    masked_preview: String,
}

#[tauri::command]
async fn pyramid_list_credentials(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<CredentialPreview>, String> {
    let previews = state
        .pyramid
        .credential_store
        .list_with_masked_previews();
    Ok(previews
        .into_iter()
        .map(|(key, masked_preview)| CredentialPreview {
            key,
            masked_preview,
        })
        .collect())
}

#[tauri::command]
async fn pyramid_set_credential(
    state: tauri::State<'_, SharedState>,
    key: String,
    value: String,
) -> Result<(), String> {
    state
        .pyramid
        .credential_store
        .set(&key, &value)
        .map_err(|e| e.to_string())?;

    // Keep in-memory caches in sync when the OpenRouter key changes.
    if key == "OPENROUTER_KEY" {
        state.pyramid.config.write().await.api_key = value.clone();
        state.partner.llm_config.write().await.api_key = value;
    }

    Ok(())
}

#[tauri::command]
async fn pyramid_delete_credential(
    state: tauri::State<'_, SharedState>,
    key: String,
) -> Result<(), String> {
    state
        .pyramid
        .credential_store
        .delete(&key)
        .map_err(|e| e.to_string())?;

    // Clear in-memory caches when the OpenRouter key is removed.
    if key == "OPENROUTER_KEY" {
        state.pyramid.config.write().await.api_key = String::new();
        state.partner.llm_config.write().await.api_key = String::new();
    }

    Ok(())
}

#[tauri::command]
async fn pyramid_credentials_file_status(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::credentials::CredentialFileStatus, String> {
    state
        .pyramid
        .credential_store
        .file_status()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_fix_credentials_permissions(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::credentials::CredentialFileStatus, String> {
    state
        .pyramid
        .credential_store
        .ensure_safe_permissions()
        .map_err(|e| e.to_string())?;
    state
        .pyramid
        .credential_store
        .file_status()
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct CredentialReferenceRow {
    key: String,
    defined: bool,
    referenced_by: Vec<String>,
}

#[tauri::command]
async fn pyramid_credential_references(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<CredentialReferenceRow>, String> {
    // Cross-reference every provider row for its `api_key_ref` so the
    // UI can surface missing credentials with a clear "referenced by"
    // list. Phase 5 will extend this to scan config contributions too.
    let registry = state.pyramid.provider_registry.clone();
    let providers = registry.list_providers();
    let store = &state.pyramid.credential_store;

    // Build a map: key_name → list of provider descriptions.
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for provider in &providers {
        if let Some(key_ref) = &provider.api_key_ref {
            // Handle bare "OPENROUTER_KEY" and "${OPENROUTER_KEY}" shapes.
            let names: Vec<String> = if key_ref.contains("${") {
                wire_node_lib::pyramid::credentials::CredentialStore::collect_references(key_ref)
            } else {
                vec![key_ref.clone()]
            };
            for name in names {
                map.entry(name).or_default().push(format!(
                    "provider `{}` ({})",
                    provider.display_name, provider.id
                ));
            }
        }
    }

    // Also include every currently-defined key even if nothing references it.
    for key in store.keys() {
        map.entry(key).or_default();
    }

    Ok(map
        .into_iter()
        .map(|(key, referenced_by)| CredentialReferenceRow {
            defined: store.contains(&key),
            key,
            referenced_by,
        })
        .collect())
}

// --- Phase 3: Provider registry IPC commands --------------------------------

#[tauri::command]
async fn pyramid_list_providers(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::pyramid::provider::Provider>, String> {
    Ok(state.pyramid.provider_registry.list_providers())
}

#[tauri::command]
async fn pyramid_save_provider(
    state: tauri::State<'_, SharedState>,
    provider: wire_node_lib::pyramid::provider::Provider,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .save_provider(&writer, provider)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_delete_provider(
    state: tauri::State<'_, SharedState>,
    id: String,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .delete_provider(&writer, &id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_test_provider(
    state: tauri::State<'_, SharedState>,
    id: String,
) -> Result<serde_json::Value, String> {
    // Resolve the provider row, instantiate, and confirm the
    // credential reference is defined. We deliberately do NOT make
    // a real HTTP call here — a real "ping" endpoint is Phase 10 UI
    // scope. The v1 test only surfaces missing-credential errors so
    // the user knows which key to set in Settings → Credentials.
    let provider = state
        .pyramid
        .provider_registry
        .get_provider(&id)
        .ok_or_else(|| format!("provider `{id}` not found"))?;
    let (_impl, secret) = state
        .pyramid
        .provider_registry
        .instantiate_provider(&provider)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "ok": true,
        "provider_id": provider.id,
        "provider_type": provider.provider_type.as_str(),
        "credential_defined": secret.is_some(),
        "chat_completions_url_resolved": true,
    }))
}

#[tauri::command]
async fn pyramid_get_tier_routing(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::pyramid::provider::TierRoutingEntry>, String> {
    Ok(state.pyramid.provider_registry.list_tier_routing())
}

#[tauri::command]
async fn pyramid_save_tier_routing(
    state: tauri::State<'_, SharedState>,
    entry: wire_node_lib::pyramid::provider::TierRoutingEntry,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .save_tier_routing(&writer, entry)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_delete_tier_routing(
    state: tauri::State<'_, SharedState>,
    tier_name: String,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .delete_tier_routing(&writer, &tier_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_get_step_overrides(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
    chain_id: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::provider::StepOverride>, String> {
    let all = state.pyramid.provider_registry.list_step_overrides();
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|o| {
            slug.as_deref().map_or(true, |s| o.slug == s)
                && chain_id.as_deref().map_or(true, |c| o.chain_id == c)
        })
        .collect();
    Ok(filtered)
}

#[tauri::command]
async fn pyramid_save_step_override(
    state: tauri::State<'_, SharedState>,
    override_row: wire_node_lib::pyramid::provider::StepOverride,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .save_step_override(&writer, override_row)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_delete_step_override(
    state: tauri::State<'_, SharedState>,
    slug: String,
    chain_id: String,
    step_name: String,
    field_name: String,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    state
        .pyramid
        .provider_registry
        .delete_step_override(&writer, &slug, &chain_id, &step_name, &field_name)
        .map_err(|e| e.to_string())
}

// --- Phase 18a: Local Mode toggle (L1 + L5 + L2) ----------------------------
//
// Per `docs/specs/provider-registry.md` §382-395 + §559-561 and
// `docs/plans/deferral-ledger.md` entries L1/L2/L5. The Local Mode
// toggle is a single switch in Settings that routes every model
// tier through a local Ollama instance. Three IPC commands cover the
// surface plus a probe helper plus a credential preview helper for
// L2.

#[tauri::command]
async fn pyramid_get_local_mode_status(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Wanderer fix (Phase 18a): split the synchronous DB snapshot
    // from the async Ollama probe so the reader lock is released
    // BEFORE the network round-trip. Holding the tokio::sync::Mutex
    // across `probe_ollama().await` would block every other
    // reader-bound IPC for up to 5 seconds while the probe ran.
    let snapshot = {
        let reader = state.pyramid.reader.lock().await;
        let snapshot =
            wire_node_lib::pyramid::local_mode::load_status_snapshot(&reader)
                .map_err(|e| e.to_string())?;
        drop(reader);
        snapshot
    };
    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

#[tauri::command]
async fn pyramid_enable_local_mode(
    state: tauri::State<'_, SharedState>,
    base_url: String,
    model: Option<String>,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Active build guard (AD-1): refuse if any build is in progress.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot change model routing while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Phase 18a fix-pass: split the async probe phase from the sync
    // DB commit phase so the IPC handler's future stays `Send`.
    // Holding a `&mut Connection` across `.await` in an async Tauri
    // command fails the compiler's Send check on the binary crate
    // (rusqlite::Connection is !Sync). `cargo check --lib` did not
    // catch this; only the full binary build elaborates the command
    // futures. See `enable_local_mode`'s module docs for the split.
    let plan = wire_node_lib::pyramid::local_mode::prepare_enable_local_mode(base_url, model)
        .await
        .map_err(|e| e.to_string())?;
    let snapshot = {
        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::local_mode::commit_enable_local_mode(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
            plan,
        )
        .map_err(|e| e.to_string())?;
        let snapshot = wire_node_lib::pyramid::local_mode::load_status_snapshot(&writer)
            .map_err(|e| e.to_string())?;
        drop(writer);
        snapshot
    };
    // After the registry refresh inside commit_enable_local_mode,
    // rebuild the cascade model fields on the live LlmConfig so
    // call_model_unified sends the right model name (e.g. an Ollama
    // model instead of an OpenRouter slug).
    rebuild_cascade_from_registry(&state).await;

    // Refresh reachability OUTSIDE the lock so the probe doesn't
    // block concurrent reader-bound IPCs. The snapshot already has
    // the just-committed state, so this only updates the `reachable`
    // + `available_models` fields with fresh data.
    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

#[tauri::command]
async fn pyramid_disable_local_mode(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Active build guard (AD-1): refuse if any build is in progress.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot change model routing while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Phase 18a fix-pass: same split-phase pattern as enable. The
    // sync commit runs under the writer lock; the async reachability
    // refresh runs after the lock drops so the command future stays
    // `Send` on the multi-threaded Tauri runtime.
    let snapshot = {
        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::local_mode::commit_disable_local_mode(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
        )
        .map_err(|e| e.to_string())?;
        let snapshot = wire_node_lib::pyramid::local_mode::load_status_snapshot(&writer)
            .map_err(|e| e.to_string())?;
        drop(writer);
        snapshot
    };

    // Same rebuild as enable — switching back to OpenRouter needs
    // the cascade models updated from the restored tier routing.
    rebuild_cascade_from_registry(&state).await;

    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

/// After a provider-registry refresh (local mode toggle, tier routing
/// contribution apply, etc.), re-resolve the cascade model fields on the
/// live LlmConfig from the current tier routing table.
///
/// IPC layer wrapper — delegates to the lib-level function so HTTP
/// route handlers (in `pyramid::routes_operator`) can call the same
/// logic without a round-trip through the Tauri State API.
async fn rebuild_cascade_from_registry(state: &tauri::State<'_, SharedState>) {
    wire_node_lib::pyramid::local_mode::rebuild_cascade_from_registry(&state.pyramid).await;
}

#[tauri::command]
async fn pyramid_probe_ollama(
    base_url: String,
) -> Result<wire_node_lib::pyramid::local_mode::OllamaProbeResult, String> {
    Ok(wire_node_lib::pyramid::local_mode::probe_ollama(&base_url).await)
}

/// Phase 2 daemon control plane (AD-2): fetch rich model details for a
/// single model via `/api/show`. Returns `OllamaModelInfo` with
/// context_window and architecture filled in. Used by the frontend to
/// lazy-load per-model detail cards without blocking the model list on
/// N serial `/api/show` calls.
#[tauri::command]
async fn pyramid_get_model_details(
    base_url: String,
    model: String,
) -> Result<wire_node_lib::pyramid::local_mode::OllamaModelInfo, String> {
    let normalized =
        wire_node_lib::pyramid::local_mode::normalize_base_url(&base_url).map_err(|e| e.to_string())?;
    wire_node_lib::pyramid::local_mode::fetch_model_details(&normalized, &model)
        .await
        .map_err(|e| e.to_string())
}

/// Phase 1 daemon control plane (AD-1): hot-swap the active Ollama model
/// without disable/re-enable. Split-phase pattern: async prepare (probe),
/// sync commit (writer lock), async follow-up (cascade rebuild).
#[tauri::command]
async fn pyramid_switch_local_model(
    state: tauri::State<'_, SharedState>,
    model: String,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Active build guard (AD-1): check BEFORE the slow Ollama probe.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot change model routing while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Phase 1: async prepare — read base_url from state, probe context.
    let base_url = {
        let reader = state.pyramid.reader.lock().await;
        let row = wire_node_lib::pyramid::db::load_local_mode_state(&reader)
            .map_err(|e| e.to_string())?;
        if !row.enabled {
            return Err("Local mode is not enabled — cannot switch model".to_string());
        }
        row.ollama_base_url
            .unwrap_or_else(|| "http://localhost:11434/v1".to_string())
    };

    let (validated_model, detected_context) =
        wire_node_lib::pyramid::local_mode::prepare_switch_local_model(&base_url, &model)
            .await
            .map_err(|e| e.to_string())?;

    // Phase 2: sync commit under writer lock.
    let snapshot = {
        // Re-check active builds inside the lock (TOCTOU: a build may have
        // started during the Ollama probe window).
        {
            let active = state.pyramid.active_build.read().await;
            if !active.is_empty() {
                return Err(
                    "A build started while probing the model — \
                     cannot change routing while a build is in progress."
                        .to_string(),
                );
            }
        }

        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::local_mode::commit_switch_local_model(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
            validated_model,
            detected_context,
        )
        .map_err(|e| e.to_string())?;
        let snapshot = wire_node_lib::pyramid::local_mode::load_status_snapshot(&writer)
            .map_err(|e| e.to_string())?;
        drop(writer);
        snapshot
    };

    // Phase 3: async follow-up — rebuild cascade models.
    rebuild_cascade_from_registry(&state).await;

    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

/// Phase 4 Daemon Control Plane (AD-3): pull an Ollama model with streaming
/// progress. Concurrent pull guard prevents multiple simultaneous pulls.
/// Progress events are broadcast on the build event bus with slug `__ollama__`.
#[tauri::command]
async fn pyramid_ollama_pull_model(
    state: tauri::State<'_, SharedState>,
    model: String,
) -> Result<(), String> {
    // Read base_url from the local mode state row.
    let base_url = {
        let reader = state.pyramid.reader.lock().await;
        let row = wire_node_lib::pyramid::db::load_local_mode_state(&reader)
            .map_err(|e| e.to_string())?;
        row.ollama_base_url
            .unwrap_or_else(|| "http://localhost:11434/v1".to_string())
    };
    let normalized = wire_node_lib::pyramid::local_mode::normalize_base_url(&base_url)
        .map_err(|e| e.to_string())?;

    // Concurrent pull guard: refuse if another pull is active.
    {
        let mut guard = state.pyramid.ollama_pull_in_progress.lock().await;
        if let Some(ref active_model) = *guard {
            return Err(format!(
                "A pull is already in progress for model '{}' — wait for it to complete or cancel it.",
                active_model
            ));
        }
        *guard = Some(model.clone());
    }

    // Reset the cancel flag before starting.
    state
        .pyramid
        .ollama_pull_cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);

    // Run the pull. On completion (success or error), clear the in-progress guard.
    let result = wire_node_lib::pyramid::local_mode::pull_ollama_model(
        &normalized,
        &model,
        &state.pyramid.build_event_bus,
        &state.pyramid.ollama_pull_cancel,
    )
    .await;

    // Always clear the in-progress guard, regardless of success/failure.
    {
        let mut guard = state.pyramid.ollama_pull_in_progress.lock().await;
        *guard = None;
    }

    result.map_err(|e| e.to_string())
}

/// Phase 4 Daemon Control Plane (AD-3): cancel an in-flight Ollama model pull.
/// Sets the cancellation flag — the pull loop checks it between chunks and
/// drops the response stream. Returns immediately; the pull IPC will return
/// an error once it observes the flag.
#[tauri::command]
async fn pyramid_ollama_cancel_pull(
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    state
        .pyramid
        .ollama_pull_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Phase 4 Daemon Control Plane: delete an Ollama model. Refuses to delete
/// the currently-active model. Returns a refreshed model list after deletion.
#[tauri::command]
async fn pyramid_ollama_delete_model(
    state: tauri::State<'_, SharedState>,
    model: String,
) -> Result<wire_node_lib::pyramid::local_mode::OllamaProbeResult, String> {
    // Read the active model and base_url from state.
    let (base_url, active_model) = {
        let reader = state.pyramid.reader.lock().await;
        let row = wire_node_lib::pyramid::db::load_local_mode_state(&reader)
            .map_err(|e| e.to_string())?;
        let base = row
            .ollama_base_url
            .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
        (base, row.ollama_model)
    };

    // Guard: refuse to delete the active model.
    if let Some(ref active) = active_model {
        if active == &model {
            return Err(format!(
                "Cannot delete model '{}' — it is the currently active model. \
                 Switch to a different model first.",
                model
            ));
        }
    }

    let normalized = wire_node_lib::pyramid::local_mode::normalize_base_url(&base_url)
        .map_err(|e| e.to_string())?;

    // Delete the model.
    wire_node_lib::pyramid::local_mode::delete_ollama_model(&normalized, &model)
        .await
        .map_err(|e| e.to_string())?;

    // Re-probe to return a refreshed model list.
    Ok(wire_node_lib::pyramid::local_mode::probe_ollama(&normalized).await)
}

/// Phase 3 daemon control plane (AD-4): set or clear the context window override.
/// Sync-only — no Ollama probe needed. Split-phase: active-build guard,
/// sync commit (writer lock), async follow-up (cascade rebuild).
#[tauri::command]
async fn pyramid_set_context_override(
    state: tauri::State<'_, SharedState>,
    limit: Option<usize>,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Active build guard.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot change context override while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Sync commit under writer lock.
    let snapshot = {
        // Re-check active builds inside the lock (TOCTOU guard).
        {
            let active = state.pyramid.active_build.read().await;
            if !active.is_empty() {
                return Err(
                    "A build started while acquiring the lock — \
                     cannot change context override while a build is in progress."
                        .to_string(),
                );
            }
        }

        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::local_mode::set_context_override(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
            limit,
        )
        .map_err(|e| e.to_string())?;
        let snapshot = wire_node_lib::pyramid::local_mode::load_status_snapshot(&writer)
            .map_err(|e| e.to_string())?;
        drop(writer);
        snapshot
    };

    // Async follow-up: rebuild cascade models.
    rebuild_cascade_from_registry(&state).await;

    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

/// Phase 3 daemon control plane (AD-5): set or clear the concurrency override.
/// Updates BOTH build_strategy AND dispatch_policy contributions in lockstep.
/// Sync-only — no Ollama probe needed. Split-phase: active-build guard,
/// sync commit (writer lock), async follow-up (cascade rebuild).
#[tauri::command]
async fn pyramid_set_concurrency_override(
    state: tauri::State<'_, SharedState>,
    concurrency: Option<usize>,
) -> Result<wire_node_lib::pyramid::local_mode::LocalModeStatus, String> {
    // Active build guard.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot change concurrency override while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Sync commit under writer lock.
    let snapshot = {
        // Re-check active builds inside the lock (TOCTOU guard).
        {
            let active = state.pyramid.active_build.read().await;
            if !active.is_empty() {
                return Err(
                    "A build started while acquiring the lock — \
                     cannot change concurrency override while a build is in progress."
                        .to_string(),
                );
            }
        }

        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::local_mode::set_concurrency_override(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
            concurrency,
        )
        .map_err(|e| e.to_string())?;
        let snapshot = wire_node_lib::pyramid::local_mode::load_status_snapshot(&writer)
            .map_err(|e| e.to_string())?;
        drop(writer);
        snapshot
    };

    // Async follow-up: rebuild cascade models.
    rebuild_cascade_from_registry(&state).await;

    Ok(wire_node_lib::pyramid::local_mode::refresh_status_reachability(snapshot).await)
}

/// Phase 6 daemon control plane (AD-6): read experimental territory markers.
/// Returns the active contribution's YAML as JSON, or a default (all locked).
#[tauri::command]
async fn pyramid_get_experimental_territory(
    state: tauri::State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::local_mode::get_experimental_territory(&reader)
        .map_err(|e| e.to_string())
}

/// Phase 6 daemon control plane (AD-6): set experimental territory markers.
/// Creates or supersedes the `experimental_territory` contribution.
#[tauri::command]
async fn pyramid_set_experimental_territory(
    state: tauri::State<'_, SharedState>,
    territory: serde_json::Value,
) -> Result<String, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::local_mode::set_experimental_territory(
        &mut writer,
        &state.pyramid.build_event_bus,
        territory,
    )
    .map_err(|e| e.to_string())?;
    Ok("Territory updated".to_string())
}

/// Fleet MPS WS1: read the current compute participation policy.
#[tauri::command]
async fn pyramid_get_compute_participation_policy(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::local_mode::ComputeParticipationPolicy, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::local_mode::get_compute_participation_policy(&reader)
        .map_err(|e| e.to_string())
}

/// Fleet MPS WS1: create or supersede the compute participation policy.
#[tauri::command]
async fn pyramid_set_compute_participation_policy(
    state: tauri::State<'_, SharedState>,
    policy: wire_node_lib::pyramid::local_mode::ComputeParticipationPolicy,
) -> Result<String, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::local_mode::set_compute_participation_policy(
        &mut writer,
        &state.pyramid.build_event_bus,
        &policy,
    )
    .map_err(|e| e.to_string())?;
    Ok("Compute participation policy updated".to_string())
}

// ═════════════════════════════════════════════════════════════════════════
// Phase 2 WS7: Compute Market IPC commands
//
// Offer management + market surface browsing + serving toggle. Each IPC
// mutates `ComputeMarketState` (wrapped in `Arc<RwLock<>>` on AppState)
// and persists to `${app_data_dir}/compute_market_state.json` on success.
// Wire-side calls go through `send_api_request` (shares the same auth /
// re-registration path as every other Wire API call).
//
// Per `docs/plans/compute-market-phase-2-exchange.md` §III "Offer Management IPC"
// (lines 573-601). The semantic distinction between `is_serving`
// (runtime toggle via `compute_market_enable` / `_disable`) and
// `allow_market_visibility` (durable operator intent via the
// `compute_participation_policy` contribution) is load-bearing: a node
// with allow_market_visibility=false will NOT publish regardless of
// is_serving; the policy gate takes precedence.
// ═════════════════════════════════════════════════════════════════════════

// The IPC-layer `ComputeOfferRequest`, `validate_model_loaded`, and
// `persist_compute_market_state` helpers moved into
// `wire_node_lib::pyramid::compute_market_ops` so the new HTTP routes
// can share the same validation + persistence path. IPC commands below
// now delegate to that module; the frontend payload shape is preserved
// verbatim (the ops module's `OfferRequest` has the same field names
// and serde defaults).

/// IPC wrapper for the frontend: re-exports the ops module's
/// `OfferRequest` so existing Tauri frontend callers see the same
/// deserialization semantics.
type ComputeOfferRequest = wire_node_lib::pyramid::compute_market_ops::OfferRequest;

/// Create a new offer on this node and publish it to the Wire. Thin
/// delegation to `compute_market_ops::create_offer` (the shared
/// IPC+HTTP path). Returns the Wire-assigned offer_id.
#[tauri::command]
async fn compute_offer_create(
    state: tauri::State<'_, SharedState>,
    offer: ComputeOfferRequest,
) -> Result<String, String> {
    wire_node_lib::pyramid::compute_market_ops::create_offer(
        offer,
        &state.auth,
        &state.config,
        &state.compute_market_state,
        &state.compute_market_dispatch,
        &state.pyramid,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Update an existing offer. The Wire accepts the same POST endpoint
/// with UPSERT semantics via UNIQUE(node_id, model_id, provider_type),
/// so update is the same code path as create.
#[tauri::command]
async fn compute_offer_update(
    state: tauri::State<'_, SharedState>,
    offer: ComputeOfferRequest,
) -> Result<String, String> {
    compute_offer_create(state, offer).await
}

/// Remove an offer. Thin delegation — active jobs on this offer
/// continue to completion; only new matches are prevented.
#[tauri::command]
async fn compute_offer_remove(
    state: tauri::State<'_, SharedState>,
    model_id: String,
) -> Result<(), String> {
    wire_node_lib::pyramid::compute_market_ops::remove_offer(
        &model_id,
        &state.auth,
        &state.config,
        &state.compute_market_state,
        &state.compute_market_dispatch,
        &state.pyramid,
    )
    .await
    .map_err(|e| e.to_string())
}

/// List all offers this node has published.
#[tauri::command]
async fn compute_offers_list(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::compute_market::ComputeOffer>, String> {
    Ok(wire_node_lib::pyramid::compute_market_ops::list_offers(&state.compute_market_state).await)
}

/// Fetch the market surface from the Wire — per-model aggregation.
/// Read-only.
#[tauri::command]
async fn compute_market_surface(
    state: tauri::State<'_, SharedState>,
    model_id: Option<String>,
) -> Result<serde_json::Value, String> {
    wire_node_lib::pyramid::compute_market_ops::market_surface(
        model_id.as_deref(),
        &state.auth,
        &state.config,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Toggle the runtime `is_serving` flag on. Does NOT modify the durable
/// `compute_participation_policy.allow_market_visibility` — that gate
/// takes precedence.
#[tauri::command]
async fn compute_market_enable(
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    wire_node_lib::pyramid::compute_market_ops::set_serving(
        true,
        &state.compute_market_state,
        &state.compute_market_dispatch,
        &state.pyramid,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Toggle the runtime `is_serving` flag off.
#[tauri::command]
async fn compute_market_disable(
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    wire_node_lib::pyramid::compute_market_ops::set_serving(
        false,
        &state.compute_market_state,
        &state.compute_market_dispatch,
        &state.pyramid,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Return the full `ComputeMarketState` for observability.
#[tauri::command]
async fn compute_market_get_state(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::compute_market::ComputeMarketState, String> {
    Ok(wire_node_lib::pyramid::compute_market_ops::get_state(&state.compute_market_state).await)
}

/// Read the active pyramid viz config contribution.
/// Tries slug-scoped first, then global, then returns a default.
#[tauri::command]
async fn pyramid_get_viz_config(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
) -> Result<serde_json::Value, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::viz_config::get_pyramid_viz_config(&reader, slug.as_deref())
        .map_err(|e| e.to_string())
}

/// Creates or supersedes the `pyramid_viz_config` contribution.
#[tauri::command]
async fn pyramid_set_viz_config(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
    config: serde_json::Value,
) -> Result<String, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::viz_config::set_pyramid_viz_config(
        &mut writer,
        &state.pyramid.build_event_bus,
        slug.as_deref(),
        config,
    )
    .map_err(|e| e.to_string())?;
    Ok("Viz config updated".to_string())
}

/// Phase 3b: Returns visual encoding data for the three-axis system
/// (brightness, saturation, border thickness) plus evidence link graph.
#[tauri::command]
async fn pyramid_get_visual_encoding_data(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    let reader = state.pyramid.reader.lock().await;
    let data = pyramid_query::get_visual_encoding_data(&reader, &slug)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&data).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct PreviewPullContributionResponse {
    yaml: String,
    schema_type: Option<String>,
    title: String,
    description: String,
    required_credentials: Vec<String>,
    missing_credentials: Vec<String>,
}

#[tauri::command]
async fn pyramid_preview_pull_contribution(
    state: tauri::State<'_, SharedState>,
    wire_contribution_id: String,
) -> Result<PreviewPullContributionResponse, String> {
    // Phase 18a (L2): fetch a Wire contribution and scan its YAML
    // for `${VAR}` references against the local credentials store.
    // Used by the Discover panel's pull confirmation flow so users
    // see "this contribution needs OPENAI_API_KEY which you haven't
    // set" BEFORE the pull lands.
    let wire_auth = get_api_token(&state.auth).await?;
    let publisher = wire_publisher_from_state(&state, wire_auth);
    let full = publisher
        .fetch_contribution(&wire_contribution_id)
        .await
        .map_err(|e| e.to_string())?;

    let required: Vec<String> =
        wire_node_lib::pyramid::credentials::CredentialStore::collect_references(
            &full.yaml_content,
        );
    let store = &state.pyramid.credential_store;
    let missing: Vec<String> = required
        .iter()
        .filter(|name| !store.contains(name))
        .cloned()
        .collect();

    Ok(PreviewPullContributionResponse {
        yaml: full.yaml_content,
        schema_type: full.schema_type,
        title: full.title,
        description: full.description,
        required_credentials: required,
        missing_credentials: missing,
    })
}

// --- Phase 11: Provider Health + Broadcast Oversight IPC commands -----------
//
// Per `docs/specs/evidence-triage-and-dadbear.md` Part 3 (Provider
// Health Alerting). These endpoints back the Phase 15 DADBEAR
// Oversight Page. They return health snapshots and allow the admin
// to acknowledge alerts.
//
// `pyramid_list_orphan_broadcasts` is a Phase 11 bonus (originally
// flagged as optional in the brief) — trivial to ship here and the
// test harness needs it to assert orphan insertion during a fresh
// install scenario.

#[tauri::command]
async fn pyramid_provider_health(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::pyramid::db::ProviderHealthEntry>, String> {
    let conn = state.pyramid.reader.lock().await;
    // 24h window is what the spec's Oversight page shows by default.
    wire_node_lib::pyramid::db::list_provider_health(&conn, 86_400)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_acknowledge_provider_health(
    state: tauri::State<'_, SharedState>,
    provider_id: String,
) -> Result<(), String> {
    let writer = state.pyramid.writer.lock().await;
    let bus = state.pyramid.build_event_bus.clone();
    wire_node_lib::pyramid::provider_health::acknowledge_provider(
        &writer,
        &provider_id,
        Some(&bus),
    )
    .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct OrphanBroadcastRow {
    id: i64,
    received_at: String,
    provider_id: Option<String>,
    generation_id: Option<String>,
    session_id: Option<String>,
    pyramid_slug: Option<String>,
    build_id: Option<String>,
    step_name: Option<String>,
    model: Option<String>,
    cost_usd: Option<f64>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    acknowledged_at: Option<String>,
    acknowledgment_reason: Option<String>,
}

#[tauri::command]
async fn pyramid_list_orphan_broadcasts(
    state: tauri::State<'_, SharedState>,
    limit: Option<i64>,
    include_acknowledged: Option<bool>,
) -> Result<Vec<OrphanBroadcastRow>, String> {
    let conn = state.pyramid.reader.lock().await;
    let limit = limit.unwrap_or(100).clamp(1, 1000);
    let include = include_acknowledged.unwrap_or(false);
    let sql = if include {
        "SELECT id, received_at, provider_id, generation_id, session_id,
                pyramid_slug, build_id, step_name, model, cost_usd,
                tokens_in, tokens_out, acknowledged_at, acknowledgment_reason
         FROM pyramid_orphan_broadcasts
         ORDER BY received_at DESC
         LIMIT ?1"
    } else {
        "SELECT id, received_at, provider_id, generation_id, session_id,
                pyramid_slug, build_id, step_name, model, cost_usd,
                tokens_in, tokens_out, acknowledged_at, acknowledgment_reason
         FROM pyramid_orphan_broadcasts
         WHERE acknowledged_at IS NULL
         ORDER BY received_at DESC
         LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![limit], |row| {
            Ok(OrphanBroadcastRow {
                id: row.get(0)?,
                received_at: row.get(1)?,
                provider_id: row.get(2)?,
                generation_id: row.get(3)?,
                session_id: row.get(4)?,
                pyramid_slug: row.get(5)?,
                build_id: row.get(6)?,
                step_name: row.get(7)?,
                model: row.get(8)?,
                cost_usd: row.get(9)?,
                tokens_in: row.get(10)?,
                tokens_out: row.get(11)?,
                acknowledged_at: row.get(12)?,
                acknowledgment_reason: row.get(13)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// --- Phase 4: Config Contribution Foundation IPC commands -------------------
//
// Per `docs/specs/config-contribution-and-wire-sharing.md`. These
// endpoints cover the contribution lifecycle (create, supersede,
// read, version history, rollback) and the agent-proposal flow
// (propose, pending, accept, reject). Wire publication endpoints
// (`pyramid_publish_to_wire`, `pyramid_pull_wire_config`,
// `pyramid_search_wire_configs`) are Phase 5 / Phase 10 scope.
// Generative config endpoints (`pyramid_generate_config`,
// `pyramid_refine_config`, `pyramid_reroll_config`) are Phase 9 / 13.
//
// Notes enforcement is per the spec's Notes Capture Lifecycle table:
// `pyramid_supersede_config`, `pyramid_propose_config`, and
// `pyramid_rollback_config` all require a non-empty, non-whitespace
// note. Empty notes are rejected at the IPC boundary with a clear
// error string.

#[derive(serde::Serialize)]
struct CreateConfigContributionResponse {
    contribution_id: String,
}

#[derive(serde::Serialize)]
struct SupersedeConfigResponse {
    new_contribution_id: String,
}

#[derive(serde::Serialize)]
struct RejectProposalResponse {
    ok: bool,
}

#[tauri::command]
async fn pyramid_create_config_contribution(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
    yaml_content: String,
    note: Option<String>,
    source: Option<String>,
) -> Result<CreateConfigContributionResponse, String> {
    let writer = state.pyramid.writer.lock().await;
    let source_val = source.unwrap_or_else(|| "local".to_string());
    let contribution_id =
        wire_node_lib::pyramid::config_contributions::create_config_contribution(
            &writer,
            &schema_type,
            slug.as_deref(),
            &yaml_content,
            note.as_deref(),
            &source_val,
            Some("user"),
            "active",
        )
        .map_err(|e| e.to_string())?;

    // Phase 4 invariant: every write path to pyramid_config_contributions
    // that lands as `active` MUST sync to operational tables immediately.
    // Per the spec: "Write path: always write to pyramid_config_contributions
    // first, then sync to operational tables." Without this call the
    // operational tables stay stale and the executor reads prior values.
    let contribution =
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &writer,
            &contribution_id,
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contribution disappeared immediately after create".to_string())?;
    wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
        &writer,
        &state.pyramid.build_event_bus,
        &contribution,
    )
    .map_err(|e| e.to_string())?;

    Ok(CreateConfigContributionResponse { contribution_id })
}

#[tauri::command]
async fn pyramid_supersede_config(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
    new_yaml_content: String,
    note: String,
) -> Result<SupersedeConfigResponse, String> {
    // Notes enforcement per Notes Capture Lifecycle: reject empty
    // or whitespace-only notes at the IPC boundary.
    wire_node_lib::pyramid::config_contributions::validate_note(&note)?;

    let mut writer = state.pyramid.writer.lock().await;
    let new_contribution_id =
        wire_node_lib::pyramid::config_contributions::supersede_config_contribution(
            &mut writer,
            &contribution_id,
            &new_yaml_content,
            &note,
            "local",
            Some("user"),
        )
        .map_err(|e| e.to_string())?;

    // Phase 4 invariant: sync the newly-active contribution to its
    // operational table so the executor sees the new value on its
    // next read. See `pyramid_create_config_contribution` for the
    // rationale — same invariant applies to every write path that
    // produces an `active` contribution.
    let contribution =
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &writer,
            &new_contribution_id,
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contribution disappeared immediately after supersede".to_string())?;
    wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
        &writer,
        &state.pyramid.build_event_bus,
        &contribution,
    )
    .map_err(|e| e.to_string())?;

    Ok(SupersedeConfigResponse { new_contribution_id })
}

/// Wave 4 task 29 (walker-re-plan-wire-2.1 §8): flattened UI view of
/// the `MarketSurfaceCache` snapshot. Returns `[]` on cold cache (pre-
/// tunnel fresh install, or first 60s after boot before the initial
/// poll lands) — callers treat `[]` as "not yet populated" not "no
/// models exist on the network". Shape defined by `PyramidMarketModel`
/// in `market_surface_cache.rs`.
#[tauri::command]
async fn pyramid_market_models(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<wire_node_lib::pyramid::market_surface_cache::PyramidMarketModel>, String> {
    let cache_opt = {
        let cfg = state.pyramid.config.read().await;
        cfg.market_surface_cache.clone()
    };
    match cache_opt {
        Some(cache) => Ok(cache.snapshot_ui_models().await),
        None => Ok(Vec::new()),
    }
}

#[tauri::command]
async fn pyramid_active_config_contribution(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
) -> Result<Option<wire_node_lib::pyramid::config_contributions::ConfigContribution>, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::config_contributions::load_active_config_contribution(
        &reader,
        &schema_type,
        slug.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_config_version_history(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::config_contributions::ConfigContribution>, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::config_contributions::load_config_version_history(
        &reader,
        &schema_type,
        slug.as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// Phase 5 (Config History + Rollback): efficient config history query.
/// Single SQL query (O(1) regardless of chain length) returning
/// most-recent-first entries capped by `limit`. Replaces the
/// `pyramid_config_version_history` IPC for the frontend timeline
/// where full `ConfigContribution` fields are unnecessary.
#[tauri::command]
async fn pyramid_get_config_history(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    limit: usize,
) -> Result<Vec<wire_node_lib::pyramid::config_contributions::ConfigHistoryEntry>, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::config_contributions::load_config_history(
        &reader,
        &schema_type,
        limit,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_propose_config(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
    yaml_content: String,
    note: String,
    agent_name: String,
) -> Result<CreateConfigContributionResponse, String> {
    // Agent proposals require a non-empty note per the spec.
    wire_node_lib::pyramid::config_contributions::validate_note(&note)?;

    let writer = state.pyramid.writer.lock().await;
    let contribution_id =
        wire_node_lib::pyramid::config_contributions::create_config_contribution(
            &writer,
            &schema_type,
            slug.as_deref(),
            &yaml_content,
            Some(&note),
            "agent",
            Some(&agent_name),
            "proposed",
        )
        .map_err(|e| e.to_string())?;
    Ok(CreateConfigContributionResponse { contribution_id })
}

#[tauri::command]
async fn pyramid_pending_proposals(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::config_contributions::ConfigContribution>, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::config_contributions::list_pending_proposals(
        &reader,
        slug.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_accept_proposal(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
) -> Result<CreateConfigContributionResponse, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::config_contributions::accept_proposal(&mut writer, &contribution_id)
        .map_err(|e| e.to_string())?;

    // Phase 4 invariant: accept transitions the proposal to `active`.
    // Sync the now-active contribution to its operational table so the
    // executor sees the newly-accepted value. Without this the
    // operational table keeps returning the prior (now-superseded)
    // row — which is exactly the bug the contribution pattern was
    // meant to eliminate.
    let contribution =
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &writer,
            &contribution_id,
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contribution disappeared immediately after accept".to_string())?;
    wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
        &writer,
        &state.pyramid.build_event_bus,
        &contribution,
    )
    .map_err(|e| e.to_string())?;

    Ok(CreateConfigContributionResponse {
        contribution_id,
    })
}

#[tauri::command]
async fn pyramid_reject_proposal(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
    reason: Option<String>,
) -> Result<RejectProposalResponse, String> {
    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::config_contributions::reject_proposal(
        &writer,
        &contribution_id,
        reason.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    Ok(RejectProposalResponse { ok: true })
}

/// Phase 5 (Config History + Rollback): roll back to a previous config
/// version. Creates a new superseding contribution with the target's
/// YAML content, syncs to operational, and refreshes the provider
/// registry.
///
/// Guards:
/// - **Active build:** refuse if any build is in progress.
/// - **Local mode:** refuse rollback of tier_routing / build_strategy
///   while local mode is enabled (AD-7, prevents state splits).
/// - **Schema validation:** parse the target YAML before committing
///   to catch schema evolution breakage.
#[tauri::command]
async fn pyramid_rollback_config(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
) -> Result<String, String> {
    // Active build guard: refuse if any build is in progress.
    {
        let active = state.pyramid.active_build.read().await;
        if !active.is_empty() {
            return Err(
                "Cannot roll back configuration while a build is in progress — \
                 wait for it to complete or cancel it."
                    .to_string(),
            );
        }
    }

    // Delegate to the library function which handles: local mode
    // guard, schema validation, supersession, sync to operational,
    // and registry refresh.
    {
        let mut writer = state.pyramid.writer.lock().await;
        wire_node_lib::pyramid::config_contributions::rollback_config(
            &mut writer,
            &state.pyramid.build_event_bus,
            &state.pyramid.provider_registry,
            &contribution_id,
        )
        .map_err(|e| e.to_string())?;
    }

    // Rebuild the cascade model fields on the live LlmConfig so
    // call_model_unified sends the correct model name for whichever
    // provider is now active after the rollback.
    rebuild_cascade_from_registry(&state).await;

    Ok(format!("Rolled back to {contribution_id}"))
}

// --- Phase 5: Wire Contribution Publication IPC ----------------------------
//
// Per `docs/specs/wire-contribution-mapping.md` → "Publish IPC" section.
// Two commands ship in Phase 5:
//
//   pyramid_dry_run_publish — build a DryRunReport for ToolsMode to
//     render inline. Pure local call; no network, no auth required.
//
//   pyramid_publish_to_wire — call the Wire's `/api/v1/contribute`
//     endpoint with the canonical YAML metadata + derived_from
//     allocation. Requires `confirm: true`. Writes the
//     `WirePublicationState` back to the contribution row on success.
//
// Both commands operate on a single `contribution_id` — the caller
// looks up the row, builds the metadata from its
// `wire_native_metadata_json` column, and dispatches to `wire_publish.rs`.

#[tauri::command]
async fn pyramid_dry_run_publish(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
) -> Result<wire_node_lib::pyramid::wire_publish::DryRunReport, String> {
    // Load the contribution.
    let contribution = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &reader,
            &contribution_id,
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("contribution {contribution_id} not found"))?
    };

    // Deserialize canonical metadata from the JSON column.
    let metadata =
        wire_node_lib::pyramid::wire_native_metadata::WireNativeMetadata::from_json(
            &contribution.wire_native_metadata_json,
        )
        .map_err(|e| format!("failed to parse wire_native_metadata_json: {e}"))?;

    // The publisher only needs the URL + auth for the real publish
    // path; dry-run is purely local. Construct a zero-auth publisher
    // so we don't require the user to be signed in just to preview.
    let publisher = wire_node_lib::pyramid::wire_publish::PyramidPublisher::new(
        "https://dry-run.invalid".to_string(),
        String::new(),
    );

    publisher
        .dry_run_publish(
            &contribution.contribution_id,
            &contribution.schema_type,
            &contribution.yaml_content,
            &metadata,
        )
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct PublishToWireResponse {
    wire_contribution_id: String,
    handle_path: Option<String>,
    wire_type: String,
    sections_published: Vec<String>,
    /// Phase 18c (L4): set to the number of cache entries that were
    /// attached to the publication when the user opted in to
    /// `include_cache_manifest`. `None` means the user did not opt in
    /// (the default-OFF privacy gate). `Some(0)` means they opted in
    /// but the pyramid had no cached LLM outputs to ship. The
    /// frontend uses this to surface "cache manifest included
    /// (N entries)" in the success state.
    cache_manifest_entries: Option<u64>,
}

#[tauri::command]
async fn pyramid_publish_to_wire(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
    confirm: bool,
    // Phase 18c (L4): user opt-in for the cache manifest. Defaults to
    // false so any caller that hasn't been updated still gets the
    // Phase 7 safe-default behavior (cache manifest withheld). The
    // PublishPreviewModal flips this to true when the "Include cache
    // manifest" checkbox is checked.
    //
    // Phase 18a fix-pass: removed a bogus `#[serde(default)]`
    // attribute that the implementer added here. That attribute is
    // for struct fields, not function parameters, and `serde` is a
    // crate not an attribute macro in main.rs's scope. Tauri already
    // treats `Option<T>` command parameters as optional — if the
    // frontend omits the field, it arrives as `None`. The former
    // attribute did nothing except trip the binary compile, which
    // only surfaced when the full `cargo check` elaborated the
    // command futures — `cargo check --lib` does not compile main.rs.
    include_cache_manifest: Option<bool>,
) -> Result<PublishToWireResponse, String> {
    if !confirm {
        return Err(
            "publish_to_wire called with confirm=false; require explicit user confirmation"
                .to_string(),
        );
    }

    let opt_in_cache = include_cache_manifest.unwrap_or(false);

    // Load the contribution.
    let contribution = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &reader,
            &contribution_id,
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("contribution {contribution_id} not found"))?
    };

    // Deserialize canonical metadata.
    let metadata =
        wire_node_lib::pyramid::wire_native_metadata::WireNativeMetadata::from_json(
            &contribution.wire_native_metadata_json,
        )
        .map_err(|e| format!("failed to parse wire_native_metadata_json: {e}"))?;

    // Refuse to publish draft metadata without an explicit force flag.
    // (Phase 5 ships the hard refusal; Phase 10's UI can add the
    // "publish as draft" override.)
    if matches!(
        metadata.maturity,
        wire_node_lib::pyramid::wire_native_metadata::WireMaturity::Draft
    ) {
        return Err(
            "contribution maturity is `draft` — promote to design/canon before publishing (Phase 5 refuses draft publishes; Phase 10 adds the override)"
                .to_string(),
        );
    }

    // Build the publisher. Uses the session API token for Wire auth,
    // same pattern as pyramid publication paths elsewhere in main.rs.
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let wire_auth = get_api_token(&state.auth).await?;
    let publisher =
        wire_node_lib::pyramid::wire_publish::PyramidPublisher::new(wire_url, wire_auth);

    // Phase 18c (L4): build the cache manifest BEFORE the network call
    // so a manifest-build failure doesn't leave the contribution
    // half-published. The manifest is then attached to the publish
    // payload via the publisher (see publish_contribution_with_metadata).
    //
    // The slug lives on the contribution row. If the contribution is
    // not slug-bound (a free-floating config contribution with no
    // pyramid attached), we cannot export a cache manifest — those
    // pyramids have no cached LLM outputs in the first place.
    let cache_manifest = if opt_in_cache {
        match contribution.slug.as_deref() {
            Some(slug) if !slug.is_empty() => {
                // Use the contribution_id as a synthetic wire_pyramid_id
                // for the manifest header — this ties the manifest to the
                // exact publication request.
                //
                // Phase 18a fix-pass: `export_cache_manifest` is now a
                // sync fn (the `async` was vestigial). We take the
                // reader lock, call it synchronously inside a block,
                // and drop the lock before any subsequent `.await`.
                let manifest_result = {
                    let conn = state.pyramid.reader.lock().await;
                    let out = publisher.export_cache_manifest(
                        &conn,
                        slug,
                        &contribution.contribution_id,
                        None,
                        true,
                    );
                    drop(conn);
                    out
                };
                manifest_result
                    .map_err(|e| format!("failed to export cache manifest: {e}"))?
            }
            _ => {
                tracing::warn!(
                    contribution_id = %contribution.contribution_id,
                    "include_cache_manifest=true but contribution is not slug-bound; \
                     cache manifest skipped"
                );
                None
            }
        }
    } else {
        None
    };
    let cache_manifest_entry_count = cache_manifest.as_ref().map(|m| {
        m.nodes
            .iter()
            .map(|n| n.cache_entries.len() as u64)
            .sum::<u64>()
    });

    let outcome = publisher
        .publish_contribution_with_metadata_and_cache(
            &contribution.contribution_id,
            &contribution.schema_type,
            &contribution.yaml_content,
            &metadata,
            cache_manifest.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())?;

    // Write the publication state back into the contribution row.
    let pub_state = wire_node_lib::pyramid::wire_native_metadata::WirePublicationState {
        wire_contribution_id: Some(outcome.wire_contribution_id.clone()),
        handle_path: outcome.handle_path.clone(),
        chain_root: None,
        chain_head: None,
        published_at: Some(chrono::Utc::now().to_rfc3339()),
        last_resolved_derived_from: outcome.resolved_derived_from.clone(),
    };
    let pub_state_json = serde_json::to_string(&pub_state)
        .map_err(|e| format!("failed to serialize wire_publication_state: {e}"))?;

    {
        let writer = state.pyramid.writer.lock().await;
        writer
            .execute(
                "UPDATE pyramid_config_contributions
                 SET wire_publication_state_json = ?1,
                     wire_contribution_id = ?2
                 WHERE contribution_id = ?3",
                rusqlite::params![
                    pub_state_json,
                    outcome.wire_contribution_id,
                    contribution.contribution_id
                ],
            )
            .map_err(|e| e.to_string())?;
    }

    tracing::info!(
        contribution_id = %contribution.contribution_id,
        wire_contribution_id = %outcome.wire_contribution_id,
        cache_manifest_attached = opt_in_cache && cache_manifest_entry_count.is_some(),
        cache_manifest_entries = ?cache_manifest_entry_count,
        "phase 18c L4: contribution published with cache opt-in state recorded"
    );

    Ok(PublishToWireResponse {
        wire_contribution_id: outcome.wire_contribution_id,
        handle_path: outcome.handle_path,
        wire_type: outcome.wire_type,
        sections_published: outcome.sections_published,
        cache_manifest_entries: cache_manifest_entry_count,
    })
}

// --- Phase 14: Wire discovery + recommendations + update polling IPC -------
//
// Per `docs/specs/wire-discovery-ranking.md` → "IPC Contract" (line 216).
// Phase 14 ships seven new IPC commands + two aliases for the
// Phase 10 stub names so the existing Discover placeholder can be
// swapped to the real call without a rename.
//
//   pyramid_wire_discover              — ranked search (alias: pyramid_search_wire_configs)
//   pyramid_wire_recommendations       — per-pyramid similarity suggestions
//   pyramid_wire_update_available      — list pending Wire supersession updates
//   pyramid_wire_auto_update_toggle    — set per-schema_type auto-update flag
//   pyramid_wire_auto_update_status    — read current auto-update flags
//   pyramid_wire_pull_latest           — pull a superseding contribution
//   pyramid_wire_acknowledge_update    — dismiss an update badge
//   pyramid_search_wire_configs        — Phase 10 stub alias for pyramid_wire_discover
//   pyramid_pull_wire_config           — Phase 10 stub alias for pyramid_wire_pull_latest
//
// All IPCs use the same auth pattern as Phase 5's publish flow
// (`get_api_token(&state.auth)` + `WIRE_URL` env). Discovery endpoints
// that fall through to a Wire 404 / 501 return empty results, so the
// UI renders an empty state instead of crashing when the Wire server
// hasn't shipped discovery endpoints yet.

fn wire_publisher_from_state(
    state: &SharedState,
    wire_auth: String,
) -> wire_node_lib::pyramid::wire_publish::PyramidPublisher {
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let _ = state; // keep the state borrow explicit so this fn reads like the other helpers
    wire_node_lib::pyramid::wire_publish::PyramidPublisher::new(wire_url, wire_auth)
}

#[tauri::command]
async fn pyramid_wire_discover(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    query: Option<String>,
    tags: Option<Vec<String>>,
    limit: Option<u32>,
    sort_by: Option<String>,
) -> Result<Vec<wire_node_lib::pyramid::wire_discovery::DiscoveryResult>, String> {
    let wire_auth = get_api_token(&state.auth).await.unwrap_or_default();
    let publisher = wire_publisher_from_state(&state, wire_auth);
    let sort = wire_node_lib::pyramid::wire_discovery::DiscoverSortBy::from_str_lax(
        sort_by.as_deref(),
    );
    let tags_ref: Option<&[String]> = tags.as_deref();

    // Load the weights synchronously, then drop the reader BEFORE the
    // HTTP await — Connection is !Send, so holding it across an await
    // fails the Tauri command Send bound.
    let weights = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::wire_discovery::load_ranking_weights(&reader)
    };

    wire_node_lib::pyramid::wire_discovery::discover(
        &publisher,
        weights,
        &schema_type,
        query.as_deref(),
        tags_ref,
        limit.unwrap_or(20),
        sort,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Phase 10 stub name. Shipped as an alias so the existing Discover
/// placeholder can call the real IPC without a rename.
#[tauri::command]
async fn pyramid_search_wire_configs(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    query: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<Vec<wire_node_lib::pyramid::wire_discovery::DiscoveryResult>, String> {
    pyramid_wire_discover(state, schema_type, query, tags, Some(20), None).await
}

#[tauri::command]
async fn pyramid_wire_recommendations(
    state: tauri::State<'_, SharedState>,
    slug: String,
    schema_type: String,
    limit: Option<u32>,
) -> Result<Vec<wire_node_lib::pyramid::wire_discovery::Recommendation>, String> {
    // Spec §Validation at the IPC boundary (line 288):
    // "pyramid_wire_recommendations requires an existing slug (not NULL)
    // — global recommendations are not meaningful because similarity
    // needs a pyramid profile". Tauri deserializes a missing JS field
    // or an empty string to an empty String here, so we reject both.
    if slug.trim().is_empty() {
        return Err(
            "slug is required — recommendations need a pyramid profile to compute similarity"
                .to_string(),
        );
    }
    let wire_auth = get_api_token(&state.auth).await.unwrap_or_default();
    let publisher = wire_publisher_from_state(&state, wire_auth);
    let profile = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::wire_discovery::build_pyramid_profile(&reader, &slug)
            .map_err(|e| e.to_string())?
    };
    wire_node_lib::pyramid::wire_discovery::compute_recommendations(
        &publisher,
        &profile,
        &schema_type,
        limit.unwrap_or(5),
    )
    .await
    .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct WireUpdateEntry {
    local_contribution_id: String,
    schema_type: String,
    slug: Option<String>,
    latest_wire_contribution_id: String,
    chain_length_delta: i64,
    changes_summary: Option<String>,
    author_handles: Vec<String>,
    checked_at: String,
}

#[tauri::command]
async fn pyramid_wire_update_available(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
) -> Result<Vec<WireUpdateEntry>, String> {
    let reader = state.pyramid.reader.lock().await;
    let rows = wire_node_lib::pyramid::db::list_pending_wire_updates(&reader, slug.as_deref())
        .map_err(|e| e.to_string())?;

    // Enrich each row with the matching schema_type + slug from the
    // contribution table (the cache row doesn't carry them directly).
    let mut out: Vec<WireUpdateEntry> = Vec::with_capacity(rows.len());
    for row in rows {
        let contribution =
            wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
                &reader,
                &row.local_contribution_id,
            )
            .map_err(|e| e.to_string())?;
        let Some(c) = contribution else {
            continue;
        };
        let author_handles: Vec<String> = row
            .author_handles_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        out.push(WireUpdateEntry {
            local_contribution_id: row.local_contribution_id,
            schema_type: c.schema_type,
            slug: c.slug,
            latest_wire_contribution_id: row.latest_wire_contribution_id,
            chain_length_delta: row.chain_length_delta,
            changes_summary: row.changes_summary,
            author_handles,
            checked_at: row.checked_at,
        });
    }
    Ok(out)
}

#[derive(serde::Serialize)]
struct ToggleOk {
    ok: bool,
}

#[tauri::command]
async fn pyramid_wire_auto_update_toggle(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    enabled: bool,
) -> Result<ToggleOk, String> {
    // Load current settings (or default to empty when none exists yet).
    let current = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::wire_discovery::load_auto_update_settings(&reader)
    };
    let mut next = current.clone();
    next.enabled_by_schema.insert(schema_type.clone(), enabled);
    let new_yaml = next.to_yaml();

    // Write a new contribution. Use supersede if an active one exists,
    // otherwise create fresh.
    let prior_id: Option<String> = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::config_contributions::load_active_config_contribution(
            &reader,
            "wire_auto_update_settings",
            None,
        )
        .map_err(|e| e.to_string())?
        .map(|c| c.contribution_id)
    };

    let mut writer = state.pyramid.writer.lock().await;
    let note = format!(
        "Set auto-update for {} = {}",
        schema_type, enabled
    );
    let new_contribution_id = if let Some(prior) = prior_id {
        wire_node_lib::pyramid::config_contributions::supersede_config_contribution(
            &mut writer,
            &prior,
            &new_yaml,
            &note,
            "local",
            Some("user"),
        )
        .map_err(|e| e.to_string())?
    } else {
        wire_node_lib::pyramid::config_contributions::create_config_contribution(
            &writer,
            "wire_auto_update_settings",
            None,
            &new_yaml,
            Some(&note),
            "local",
            Some("user"),
            "active",
        )
        .map_err(|e| e.to_string())?
    };

    // Sync to operational tables (invalidates caches, signals the poller).
    if let Some(contribution) =
        wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &writer,
            &new_contribution_id,
        )
        .map_err(|e| e.to_string())?
    {
        wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
            &writer,
            &state.pyramid.build_event_bus,
            &contribution,
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(ToggleOk { ok: true })
}

#[derive(serde::Serialize)]
struct AutoUpdateSettingEntry {
    schema_type: String,
    enabled: bool,
}

#[tauri::command]
async fn pyramid_wire_auto_update_status(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<AutoUpdateSettingEntry>, String> {
    let reader = state.pyramid.reader.lock().await;
    let settings = wire_node_lib::pyramid::wire_discovery::load_auto_update_settings(&reader);
    let entries: Vec<AutoUpdateSettingEntry> = settings
        .enabled_by_schema
        .iter()
        .map(|(schema_type, enabled)| AutoUpdateSettingEntry {
            schema_type: schema_type.clone(),
            enabled: *enabled,
        })
        .collect();
    Ok(entries)
}

#[derive(serde::Serialize)]
struct PullLatestResponse {
    new_local_contribution_id: String,
    activated: bool,
}

#[tauri::command]
async fn pyramid_wire_pull_latest(
    state: tauri::State<'_, SharedState>,
    local_contribution_id: String,
    latest_wire_contribution_id: String,
) -> Result<PullLatestResponse, String> {
    let wire_auth = get_api_token(&state.auth).await?;
    let publisher = wire_publisher_from_state(&state, wire_auth);

    // Resolve the prior local contribution for slug info.
    let slug: Option<String> = {
        let reader = state.pyramid.reader.lock().await;
        let row = wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
            &reader,
            &local_contribution_id,
        )
        .map_err(|e| e.to_string())?;
        row.and_then(|c| c.slug)
    };

    let mut writer = state.pyramid.writer.lock().await;
    let options = wire_node_lib::pyramid::wire_pull::PullOptions {
        latest_wire_contribution_id: &latest_wire_contribution_id,
        local_contribution_id_to_supersede: Some(&local_contribution_id),
        activate: true,
        slug: slug.as_deref(),
    };
    let outcome = wire_node_lib::pyramid::wire_pull::pull_wire_contribution(
        &mut writer,
        &publisher,
        &state.pyramid.credential_store,
        &state.pyramid.build_event_bus,
        options,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Delete the cache entry — the pull is done, no further badge needed.
    let _ = wire_node_lib::pyramid::db::delete_wire_update_cache(
        &writer,
        &local_contribution_id,
    );

    Ok(PullLatestResponse {
        new_local_contribution_id: outcome.new_local_contribution_id,
        activated: outcome.activated,
    })
}

/// Phase 10 stub name. Shipped as an alias so the Discover placeholder
/// can call the real pull IPC without a rename. Takes a
/// `wire_contribution_id` + an optional slug and pulls a fresh
/// contribution (no supersession — for brand-new schema types).
#[tauri::command]
async fn pyramid_pull_wire_config(
    state: tauri::State<'_, SharedState>,
    wire_contribution_id: String,
    slug: Option<String>,
    activate: Option<bool>,
) -> Result<PullLatestResponse, String> {
    let wire_auth = get_api_token(&state.auth).await?;
    let publisher = wire_publisher_from_state(&state, wire_auth);

    let mut writer = state.pyramid.writer.lock().await;
    let options = wire_node_lib::pyramid::wire_pull::PullOptions {
        latest_wire_contribution_id: &wire_contribution_id,
        local_contribution_id_to_supersede: None,
        activate: activate.unwrap_or(false),
        slug: slug.as_deref(),
    };
    let outcome = wire_node_lib::pyramid::wire_pull::pull_wire_contribution(
        &mut writer,
        &publisher,
        &state.pyramid.credential_store,
        &state.pyramid.build_event_bus,
        options,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(PullLatestResponse {
        new_local_contribution_id: outcome.new_local_contribution_id,
        activated: outcome.activated,
    })
}

#[tauri::command]
async fn pyramid_wire_acknowledge_update(
    state: tauri::State<'_, SharedState>,
    local_contribution_id: String,
) -> Result<ToggleOk, String> {
    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::db::acknowledge_wire_update(&writer, &local_contribution_id)
        .map_err(|e| e.to_string())?;
    Ok(ToggleOk { ok: true })
}

// --- Phase 8: YAML-to-UI renderer IPC ---------------------------------------
//
// Per `docs/specs/yaml-to-ui-renderer.md` → "Backend Contract" section
// (~line 407). Three commands cover the renderer's backend surface:
//
//   pyramid_get_schema_annotation(schema_type)
//     Loads the active `schema_annotation` contribution matching the
//     given target config type. Returns a `SchemaAnnotation` whose
//     `fields` map describes how each field should render. Returns
//     `None` when no annotation is present — the frontend can fall
//     back to a generic key/value editor.
//
//   yaml_renderer_resolve_options(source)
//     Resolves a named dynamic option source (`tier_registry`,
//     `provider_list`, `model_list:{provider}`, `node_fields`,
//     `chain_list`, `prompt_files`) to a concrete `OptionValue` list.
//     Called once per unique source at mount time; results are cached
//     in the frontend hook.
//
//   yaml_renderer_estimate_cost(provider, model, avg_input_tokens,
//                               avg_output_tokens)
//     Computes a per-call USD estimate from the tier routing table's
//     pricing_json column. Returns 0.0 when the (provider, model) pair
//     isn't found — the UI can show "cost unavailable" in that case.
//
// Phase 4/5 alignment: schema annotations live in
// `pyramid_config_contributions` (not disk). Phase 8 extended
// `wire_migration.rs` so the on-disk `chains/schemas/*.schema.yaml`
// files are seeded as contributions on first run — from that point
// forward, runtime reads go through the contributions table only.

#[tauri::command]
async fn pyramid_get_schema_annotation(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
) -> Result<Option<wire_node_lib::pyramid::yaml_renderer::SchemaAnnotation>, String> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::yaml_renderer::load_schema_annotation_for(&reader, &schema_type)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn yaml_renderer_resolve_options(
    state: tauri::State<'_, SharedState>,
    source: String,
) -> Result<Vec<wire_node_lib::pyramid::yaml_renderer::OptionValue>, String> {
    // Phase 18a (L5): the `model_list:{provider_id}` branch is now
    // network-bound for Ollama-shaped providers. Route that branch
    // to the connection-free async resolver and drop the rusqlite
    // lock before the round trip so a non-Send `&Connection` never
    // crosses an await point. The DB-only branches stay on the
    // synchronous resolver as before.
    let registry = state.pyramid.provider_registry.clone();
    if let Some(provider_id) = source.strip_prefix("model_list:") {
        return Ok(
            wire_node_lib::pyramid::yaml_renderer::resolve_model_list_only(
                &registry,
                provider_id,
            )
            .await,
        );
    }
    let reader = state.pyramid.reader.lock().await;
    let result = wire_node_lib::pyramid::yaml_renderer::resolve_option_source(
        &reader, &registry, &source,
    );
    drop(reader);
    result.map_err(|e| e.to_string())
}

#[tauri::command]
async fn yaml_renderer_estimate_cost(
    state: tauri::State<'_, SharedState>,
    provider: String,
    model: String,
    avg_input_tokens: u64,
    avg_output_tokens: u64,
) -> Result<f64, String> {
    Ok(wire_node_lib::pyramid::yaml_renderer::estimate_cost(
        &state.pyramid.provider_registry,
        &provider,
        &model,
        avg_input_tokens,
        avg_output_tokens,
    ))
}

// ── Phase 9: Generative config IPC ─────────────────────────────────────────
//
// Per `docs/specs/generative-config-pattern.md` → "IPC Contract" section
// (~line 300) and `docs/specs/config-contribution-and-wire-sharing.md` →
// "IPC Contract (Full)" section for the canonical signatures.
//
// Six commands wrap the Phase 9 backend logic:
//
//   pyramid_generate_config(schema_type, slug?, intent)
//     Generates a new config YAML from an intent string. Creates a
//     draft contribution via Phase 4's CRUD layer and returns the
//     contribution_id + YAML body for the renderer.
//
//   pyramid_refine_config(contribution_id, current_yaml, note)
//     Refines an existing contribution with a user note. Rejects
//     empty notes at the IPC boundary per the Notes Capture
//     Lifecycle. Creates a new draft supersession.
//
//   pyramid_accept_config(schema_type, slug?, yaml?, triggering_note?)
//     Promotes a draft (or accepts an inline YAML payload) to active
//     and runs sync_config_to_operational. Returns the full
//     AcceptConfigResponse shape with sync_result metadata.
//
//   pyramid_active_config(schema_type, slug?)
//     Returns the active contribution for a (type, slug) pair. Thin
//     wrapper over Phase 4's `load_active_config_contribution`.
//
//   pyramid_config_versions(schema_type, slug?)
//     Returns the full version history chain. Thin wrapper over
//     Phase 4's `load_config_version_history`.
//
//   pyramid_config_schemas()
//     Returns the Phase 9 schema registry's compact summary list —
//     every schema_type the bundled manifest (or user contributions)
//     has registered.
//
// Every LLM call inside these handlers goes through Phase 6's
// cache-aware entry point (`call_model_unified_with_options_and_ctx`)
// with a fully populated StepContext. Every contribution write goes
// through Phase 4's CRUD helpers. Neither path is bypassed.

#[tauri::command]
async fn pyramid_generate_config(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
    intent: String,
) -> Result<wire_node_lib::pyramid::generative_config::GenerateConfigResponse, String> {
    let llm_config = state.pyramid.config.read().await.clone();
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db").to_string_lossy().to_string())
        .unwrap_or_default();

    // Phase 1: load inputs from the DB, then drop the lock so the
    // LLM await doesn't hold a non-Send rusqlite::Connection across
    // task scheduling points.
    let inputs = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::generative_config::load_generation_inputs(
            &reader,
            &state.pyramid.schema_registry,
            &schema_type,
            slug.as_deref(),
            &intent,
        )
        .map_err(|e| e.to_string())?
    };

    // Phase 2: run the LLM call with no DB lock held.
    let llm_output = wire_node_lib::pyramid::generative_config::run_generation_llm_call(
        &llm_config,
        &state.pyramid.build_event_bus,
        &state.pyramid.provider_registry,
        &db_path,
        &inputs,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Phase 3: persist the draft via the writer lock.
    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::generative_config::persist_generated_draft(
        &writer,
        &inputs,
        &llm_output,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_refine_config(
    state: tauri::State<'_, SharedState>,
    contribution_id: String,
    current_yaml: String,
    note: String,
) -> Result<wire_node_lib::pyramid::generative_config::RefineConfigResponse, String> {
    // Notes enforcement at the IPC boundary per the Notes Capture
    // Lifecycle. The backend helper re-validates defensively but the
    // IPC layer rejects empty/whitespace-only notes here so the user
    // never burns an LLM round-trip on a request that would fail to
    // save.
    wire_node_lib::pyramid::config_contributions::validate_note(&note)?;

    let llm_config = state.pyramid.config.read().await.clone();
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db").to_string_lossy().to_string())
        .unwrap_or_default();

    // Phase 1: load inputs from the DB, then drop the lock.
    let inputs = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::generative_config::load_refinement_inputs(
            &reader,
            &state.pyramid.schema_registry,
            &contribution_id,
            &current_yaml,
            &note,
        )
        .map_err(|e| e.to_string())?
    };

    // Phase 2: run the LLM call with no DB lock held.
    let llm_output = wire_node_lib::pyramid::generative_config::run_refinement_llm_call(
        &llm_config,
        &state.pyramid.build_event_bus,
        &state.pyramid.provider_registry,
        &db_path,
        &inputs,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Phase 3: persist the refined draft via the writer lock.
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::generative_config::persist_refined_draft(
        &mut writer,
        &inputs,
        &llm_output,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_accept_config(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
    yaml: Option<serde_json::Value>,
    triggering_note: Option<String>,
) -> Result<wire_node_lib::pyramid::generative_config::AcceptConfigResponse, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::generative_config::accept_config_draft(
        &mut writer,
        &state.pyramid.build_event_bus,
        &state.pyramid.schema_registry,
        schema_type,
        slug,
        yaml,
        triggering_note,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_active_config(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
) -> Result<
    Option<wire_node_lib::pyramid::generative_config::ActiveConfigResponse>,
    String,
> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::generative_config::active_config_for(
        &reader,
        &schema_type,
        slug.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_config_versions(
    state: tauri::State<'_, SharedState>,
    schema_type: String,
    slug: Option<String>,
) -> Result<
    Vec<wire_node_lib::pyramid::config_contributions::ConfigContribution>,
    String,
> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::generative_config::config_version_history_for(
        &reader,
        &schema_type,
        slug.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_config_schemas(
    state: tauri::State<'_, SharedState>,
) -> Result<
    Vec<wire_node_lib::pyramid::schema_registry::ConfigSchemaSummary>,
    String,
> {
    Ok(wire_node_lib::pyramid::generative_config::list_config_schemas(
        &state.pyramid.schema_registry,
    ))
}

// ── Phase 18d: Schema Migration UI IPC commands ─────────────────────────
//
// Per `docs/plans/phase-18d-workstream-prompt.md` and ledger entry L6.
// Four commands surface the `needs_migration` flag (set by Phase 9's
// `flag_configs_needing_migration` helper inside Phase 4's
// schema_definition dispatcher branch) and execute the LLM-assisted
// migration flow:
//
//   pyramid_list_configs_needing_migration()
//     Lists every active contribution flagged needing migration, plus
//     the schema_definition contribution_ids that bracket the
//     migration (current + prior). Powers the Needs Migration tab.
//
//   pyramid_propose_config_migration(input)
//     Loads the flagged contribution + the prior/current schema
//     definitions, calls the bundled migrate_config skill via the
//     cache-aware LLM entry point, persists the result as a draft
//     contribution. Mirrors Phase 9's pyramid_generate_config 3-phase
//     shape (load → LLM → persist).
//
//   pyramid_accept_config_migration(input)
//     Promotes the draft to active, supersedes the original flagged
//     row, runs sync_config_to_operational so the operational table
//     picks up the migrated YAML, and clears the needs_migration flag
//     on the new active row.
//
//   pyramid_reject_config_migration(input)
//     Deletes the draft. Original flagged row stays active and stays
//     flagged so the user can re-propose later.
//
// User review is mandatory — there is no auto-apply path. The flow
// always goes draft → review → accept, matching Phase 9's pattern.

#[derive(serde::Deserialize)]
struct ProposeMigrationInput {
    #[serde(rename = "contributionId", alias = "contribution_id")]
    contribution_id: String,
    #[serde(default, rename = "userNote", alias = "user_note")]
    user_note: Option<String>,
}

#[derive(serde::Deserialize)]
struct AcceptMigrationInput {
    #[serde(rename = "draftId", alias = "draft_id")]
    draft_id: String,
    #[serde(default, rename = "acceptNote", alias = "accept_note")]
    accept_note: Option<String>,
}

#[derive(serde::Deserialize)]
struct RejectMigrationInput {
    #[serde(rename = "draftId", alias = "draft_id")]
    draft_id: String,
}

#[tauri::command]
async fn pyramid_list_configs_needing_migration(
    state: tauri::State<'_, SharedState>,
) -> Result<
    Vec<wire_node_lib::pyramid::migration_config::NeedsMigrationEntry>,
    String,
> {
    let reader = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::migration_config::list_configs_needing_migration(&reader)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_propose_config_migration(
    state: tauri::State<'_, SharedState>,
    input: ProposeMigrationInput,
) -> Result<wire_node_lib::pyramid::migration_config::MigrationProposal, String> {
    let llm_config = state.pyramid.config.read().await.clone();
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db").to_string_lossy().to_string())
        .unwrap_or_default();

    // Phase 1: load inputs from the DB. Drop the read lock before the
    // LLM await so we don't pin a non-Send rusqlite::Connection across
    // a task scheduling point. Mirrors Phase 9's
    // pyramid_generate_config / pyramid_refine_config 3-phase shape.
    let inputs = {
        let reader = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::migration_config::load_migration_inputs(
            &reader,
            &input.contribution_id,
            input.user_note.as_deref(),
        )
        .map_err(|e| e.to_string())?
    };

    // Phase 2: run the LLM call with no DB lock held.
    let llm_output = wire_node_lib::pyramid::migration_config::run_migration_llm_call(
        &llm_config,
        &state.pyramid.build_event_bus,
        &state.pyramid.provider_registry,
        &db_path,
        &inputs,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Phase 3: persist the proposal via the writer lock.
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::migration_config::persist_migration_proposal(
        &mut writer,
        &inputs,
        &llm_output,
        &state.pyramid.build_event_bus,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_accept_config_migration(
    state: tauri::State<'_, SharedState>,
    input: AcceptMigrationInput,
) -> Result<wire_node_lib::pyramid::migration_config::AcceptMigrationOutcome, String> {
    let mut writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::migration_config::accept_config_migration(
        &mut writer,
        &state.pyramid.build_event_bus,
        &state.pyramid.schema_registry,
        &input.draft_id,
        input.accept_note.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_reject_config_migration(
    state: tauri::State<'_, SharedState>,
    input: RejectMigrationInput,
) -> Result<wire_node_lib::pyramid::migration_config::RejectMigrationOutcome, String> {
    let writer = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::migration_config::reject_config_migration(&writer, &input.draft_id)
        .map_err(|e| e.to_string())
}

// ── Phase 7: Cache warming on pyramid import IPC commands ─────────────────
//
// Per `docs/specs/cache-warming-and-import.md` "IPC Contract" section
// (~line 374). Three commands cover the import lifecycle:
//
//   pyramid_import_pyramid(wire_pyramid_id, target_slug, source_path,
//                          manifest_json)
//     Kicks off the staleness-check + cache-population flow. Returns an
//     ImportReport with the four counters ToolsMode renders in its
//     "first build will cost ~$X" preview.
//
//   pyramid_import_progress(target_slug)
//     Polled by the frontend during long imports. Returns the current
//     ImportState row fields + a 0.0..=1.0 progress ratio derived from
//     (nodes_processed, cache_entries_validated).
//
//   pyramid_import_cancel(target_slug)
//     Rolls back the import: deletes any cache rows the import wrote
//     (filtered by `build_id LIKE 'import:%'`) and the in-flight state
//     row. Idempotent — cancelling a slug that was never imported is a
//     no-op. Per spec "Cleanup" section ~line 345.
//
// The manifest is supplied by the caller rather than downloaded here.
// Phase 10's ImportPyramidWizard will own the manifest download via
// the existing WireImportClient + a new pyramid-manifest endpoint.

#[derive(serde::Serialize)]
struct ImportPyramidResponse {
    imported_nodes: u64,
    cache_entries_valid: u64,
    cache_entries_stale: u64,
    nodes_needing_rebuild: u64,
    nodes_with_valid_cache: u64,
}

#[derive(serde::Serialize)]
struct ImportProgressResponse {
    status: String,
    progress: f64,
    nodes_imported: i64,
    cache_entries_validated: i64,
    cache_entries_inserted: i64,
    error_message: Option<String>,
}

#[derive(serde::Serialize)]
struct ImportCancelResponse {
    cancelled: bool,
    /// Whether any partial state existed for the slug at cancel time.
    /// `false` is an idempotent no-op cancel.
    state_row_existed: bool,
    /// Number of `pyramid_step_cache` rows the rollback deleted. Counts
    /// only rows whose `build_id` matches the import's synthetic
    /// `import:` prefix — locally-built rows are not touched.
    cache_rows_rolled_back: u64,
}

#[tauri::command]
async fn pyramid_import_pyramid(
    state: tauri::State<'_, SharedState>,
    wire_pyramid_id: String,
    target_slug: String,
    source_path: String,
    manifest_json: String,
) -> Result<ImportPyramidResponse, String> {
    // Parse the manifest up-front so we fail fast on bad input without
    // touching the DB.
    let manifest: wire_node_lib::pyramid::pyramid_import::CacheManifest =
        serde_json::from_str(&manifest_json)
            .map_err(|e| format!("failed to parse cache manifest JSON: {e}"))?;

    let writer = state.pyramid.writer.lock().await;

    let report = wire_node_lib::pyramid::pyramid_import::import_pyramid(
        &writer,
        &state.pyramid.build_event_bus,
        &wire_pyramid_id,
        &target_slug,
        &source_path,
        &manifest,
    )
    .map_err(|e| e.to_string())?;

    Ok(ImportPyramidResponse {
        imported_nodes: report.nodes_with_valid_cache,
        cache_entries_valid: report.cache_entries_valid,
        cache_entries_stale: report.cache_entries_stale,
        nodes_needing_rebuild: report.nodes_needing_rebuild,
        nodes_with_valid_cache: report.nodes_with_valid_cache,
    })
}

#[tauri::command]
async fn pyramid_import_progress(
    state: tauri::State<'_, SharedState>,
    target_slug: String,
) -> Result<Option<ImportProgressResponse>, String> {
    let reader = state.pyramid.reader.lock().await;
    let state_row = wire_node_lib::pyramid::db::load_import_state(&reader, &target_slug)
        .map_err(|e| e.to_string())?;

    let Some(row) = state_row else {
        return Ok(None);
    };

    // Progress semantics per the spec: weight node progress and
    // cache-entry progress equally. Either total may be None while the
    // manifest is still downloading — in that case progress is 0.
    let progress = {
        let node_frac = match row.nodes_total {
            Some(total) if total > 0 => {
                (row.nodes_processed as f64) / (total as f64)
            }
            _ => 0.0,
        };
        let entry_frac = match row.cache_entries_total {
            Some(total) if total > 0 => {
                (row.cache_entries_validated as f64) / (total as f64)
            }
            _ => 0.0,
        };
        (node_frac * 0.5 + entry_frac * 0.5).clamp(0.0, 1.0)
    };

    Ok(Some(ImportProgressResponse {
        status: row.status,
        progress,
        nodes_imported: row.nodes_processed,
        cache_entries_validated: row.cache_entries_validated,
        cache_entries_inserted: row.cache_entries_inserted,
        error_message: row.error_message,
    }))
}

#[tauri::command]
async fn pyramid_import_cancel(
    state: tauri::State<'_, SharedState>,
    target_slug: String,
) -> Result<ImportCancelResponse, String> {
    let writer = state.pyramid.writer.lock().await;
    let report = wire_node_lib::pyramid::pyramid_import::cancel_pyramid_import(
        &writer,
        &target_slug,
    )
    .map_err(|e| e.to_string())?;
    Ok(ImportCancelResponse {
        cancelled: true,
        state_row_existed: report.state_row_existed,
        cache_rows_rolled_back: report.cache_rows_rolled_back,
    })
}

// --- Phase 6: Multi-Window + Nesting ----------------------------------------

/// Creates a new pyramid surface window.  When `slug` is provided the window
/// opens pre-bound to that pyramid; otherwise it opens as a blank surface the
/// user can attach later.  Returns the unique Tauri window label.
#[tauri::command]
async fn pyramid_open_window(
    app: tauri::AppHandle,
    slug: Option<String>,
) -> Result<String, String> {
    // Unique label — only the first segment of a v4 UUID (8 hex chars) to keep it short.
    let label = format!(
        "pyramid-surface-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap()
    );

    // Build the frontend URL with query params so React knows what to render.
    let url = if let Some(ref s) = slug {
        format!("index.html?window=pyramid-surface&slug={}", s)
    } else {
        "index.html?window=pyramid-surface".to_string()
    };

    let _window = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(url.into()),
    )
    .title(if let Some(ref s) = slug {
        format!("Pyramid: {}", s)
    } else {
        "Pyramid Surface".to_string()
    })
    .inner_size(1000.0, 700.0)
    .resizable(true)
    .decorations(true)
    .build()
    .map_err(|e| e.to_string())?;

    Ok(label)
}

/// Closes a pyramid surface window by its Tauri label.  Silently succeeds if
/// the window has already been closed.
#[tauri::command]
async fn pyramid_close_window(
    app: tauri::AppHandle,
    label: String,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&label) {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Returns the window context (label, whether it is a pyramid surface, and the
/// bound slug if any).  The frontend calls this on mount to decide what to
/// render.
#[tauri::command]
async fn pyramid_get_window_context(
    window: tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    let label = window.label().to_string();

    // The webview URL contains the query params we set during creation
    // (e.g. tauri://localhost/index.html?window=pyramid-surface&slug=foo).
    let url = window.url().map_err(|e| e.to_string())?;

    let is_pyramid_surface = url
        .query_pairs()
        .any(|(k, v)| k == "window" && v == "pyramid-surface");

    let slug = url
        .query_pairs()
        .find(|(k, _)| k == "slug")
        .map(|(_, v)| v.to_string());

    Ok(serde_json::json!({
        "label": label,
        "isPyramidSurface": is_pyramid_surface,
        "slug": slug,
    }))
}

// --- S2-5: Chronicle Post-Build Review IPCs ---------------------------------

/// Returns the most recent build_id for a slug. Uses `pyramid_builds`
/// (which has timestamps) rather than `pyramid_step_cache` so we get
/// the authoritative build_id even when the cache was pruned.
#[tauri::command]
async fn pyramid_latest_build_id(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<Option<String>, String> {
    let conn = state.pyramid.reader.lock().await;
    let row: Option<String> = match conn.query_row(
        "SELECT build_id FROM pyramid_builds
         WHERE slug = ?1
         ORDER BY started_at DESC
         LIMIT 1",
        rusqlite::params![slug],
        |row| row.get(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.to_string()),
    };
    Ok(row)
}

/// Returns a chronologically sorted array of build operations for a
/// given slug + build_id. Merges records from:
///   - pyramid_llm_audit (LLM calls)
///   - pyramid_evidence (KEEP/DISCONNECT verdicts)
///   - pyramid_gaps (gap reports)
///   - pyramid_config_contributions WHERE schema_type='reconciliation_result'
/// Each record is a chronicle entry with timestamp, kind, category,
/// headline, optional detail, and optional node_id.
#[tauri::command]
async fn pyramid_get_build_chronicle(
    state: tauri::State<'_, SharedState>,
    slug: String,
    build_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.pyramid.reader.lock().await;

    let mut entries: Vec<serde_json::Value> = Vec::new();

    // 1. LLM calls from pyramid_llm_audit
    {
        let mut stmt = conn
            .prepare(
                // Walker Re-Plan Wire 2.1 Wave 1 task 11: `provider_id`
                // projected alongside `model` so the chronicle can surface
                // routing analytics (which branch served the call: fleet /
                // market / pool). Legacy rows read NULL.
                "SELECT created_at, model, prompt_tokens, completion_tokens,
                        latency_ms, step_name, status, node_id, call_purpose, cache_hit,
                        provider_id
                 FROM pyramid_llm_audit
                 WHERE slug = ?1 AND build_id = ?2
                 ORDER BY created_at ASC
                 LIMIT 5000",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, build_id], |row| {
                let ts: String = row.get(0)?;
                let model: String = row.get(1)?;
                let prompt_tokens: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
                let completion_tokens: i64 = row.get::<_, Option<i64>>(3)?.unwrap_or(0);
                let latency_ms: i64 = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
                let step_name: String = row.get(5)?;
                let status: String = row.get(6)?;
                let node_id: Option<String> = row.get(7)?;
                let call_purpose: String = row.get(8)?;
                let cache_hit: i64 = row.get::<_, Option<i64>>(9)?.unwrap_or(0);
                let provider_id: Option<String> = row.get::<_, Option<String>>(10)?;
                let total_tokens = prompt_tokens + completion_tokens;
                let headline = if cache_hit == 1 {
                    format!("Cache hit: {} ({})", step_name, call_purpose)
                } else {
                    format!(
                        "LLM {}: {} {}tok {}ms [{}]",
                        status, model, total_tokens, latency_ms, step_name
                    )
                };
                let (category, kind) = if cache_hit == 1 {
                    ("cache", "mechanical")  // cache hit = no intelligence, served from disk
                } else {
                    ("llm", "decision")      // LLM call = intelligence working
                };
                Ok(serde_json::json!({
                    "timestamp": ts,
                    "kind": kind,
                    "category": category,
                    "headline": headline,
                    "detail": format!(
                        "Model: {}, Purpose: {}, Prompt: {}tok, Completion: {}tok, Latency: {}ms, Step: {}, Status: {}{}",
                        model, call_purpose, prompt_tokens, completion_tokens,
                        latency_ms, step_name, status,
                        if cache_hit == 1 { " (cache hit)" } else { "" }
                    ),
                    "node_id": node_id,
                    "provider_id": provider_id,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            entries.push(row.map_err(|e| e.to_string())?);
        }
    }

    // 2. Evidence verdicts from pyramid_evidence
    {
        let mut stmt = conn
            .prepare(
                "SELECT created_at, source_node_id, target_node_id, verdict, weight, reason
                 FROM pyramid_evidence
                 WHERE slug = ?1 AND build_id = ?2
                 ORDER BY created_at ASC
                 LIMIT 5000",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, build_id], |row| {
                let ts: String = row.get(0)?;
                let source: String = row.get(1)?;
                let target: String = row.get(2)?;
                let verdict: String = row.get(3)?;
                let weight: Option<f64> = row.get(4)?;
                let reason: Option<String> = row.get(5)?;
                let weight_str = weight
                    .map(|w| format!(" (w={:.2})", w))
                    .unwrap_or_default();
                Ok(serde_json::json!({
                    "timestamp": ts,
                    "kind": "decision",
                    "category": "verdict",
                    "headline": format!("{} {} \u{2192} {}{}", verdict, source, target, weight_str),
                    "detail": reason,
                    "node_id": target,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            entries.push(row.map_err(|e| e.to_string())?);
        }
    }

    // 3. Gap reports from pyramid_gaps
    //    build_id is nullable; filter by it when present, fall back to slug-only
    {
        let mut stmt = conn
            .prepare(
                "SELECT created_at, description, layer, resolved, question_id, build_id
                 FROM pyramid_gaps
                 WHERE slug = ?1 AND (build_id = ?2 OR build_id IS NULL)
                 ORDER BY created_at ASC
                 LIMIT 2000",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, build_id], |row| {
                let ts: String = row.get(0)?;
                let description: String = row.get(1)?;
                let layer: i64 = row.get(2)?;
                let resolved: i64 = row.get::<_, Option<i64>>(3)?.unwrap_or(0);
                let question_id: String = row.get(4)?;
                let gap_build_id: Option<String> = row.get(5)?;
                let resolved_str = if resolved == 1 { " [resolved]" } else { "" };
                Ok(serde_json::json!({
                    "timestamp": ts,
                    "kind": "decision",
                    "category": "gap",
                    "headline": format!("Gap L{}: {}{}", layer, description, resolved_str),
                    "detail": format!(
                        "Question: {}, Layer: {}, Resolved: {}, Build: {}",
                        question_id, layer, resolved == 1,
                        gap_build_id.as_deref().unwrap_or("(unscoped)")
                    ),
                    "node_id": serde_json::Value::Null,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            entries.push(row.map_err(|e| e.to_string())?);
        }
    }

    // 4. Reconciliation summaries from pyramid_config_contributions
    //    The build_id is embedded in yaml_content; filter by triggering_note
    //    which contains the build_id string.
    {
        let pattern = format!("%{}%", build_id);
        let mut stmt = conn
            .prepare(
                "SELECT created_at, yaml_content, triggering_note
                 FROM pyramid_config_contributions
                 WHERE slug = ?1
                   AND schema_type = 'reconciliation_result'
                   AND triggering_note LIKE ?2
                 ORDER BY created_at ASC
                 LIMIT 100",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![slug, pattern], |row| {
                let ts: String = row.get(0)?;
                let yaml_content: String = row.get(1)?;
                let note: Option<String> = row.get(2)?;

                // Parse yaml_content to extract orphan/central/gap counts
                let headline = if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&yaml_content) {
                    let orphans = doc.get("orphans")
                        .and_then(|v| v.as_sequence())
                        .map(|s| s.len())
                        .unwrap_or(0);
                    let central = doc.get("central_nodes")
                        .and_then(|v| v.as_sequence())
                        .map(|s| s.len())
                        .unwrap_or(0);
                    let gaps = doc.get("gaps")
                        .and_then(|v| v.as_sequence())
                        .map(|s| s.len())
                        .unwrap_or(0);
                    format!("Reconciliation: {} orphans, {} central, {} gaps", orphans, central, gaps)
                } else {
                    "Reconciliation summary".to_string()
                };

                Ok(serde_json::json!({
                    "timestamp": ts,
                    "kind": "decision",
                    "category": "reconciliation",
                    "headline": headline,
                    "detail": note,
                    "node_id": serde_json::Value::Null,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            entries.push(row.map_err(|e| e.to_string())?);
        }
    }

    // Sort all entries by timestamp (ISO 8601 strings sort lexicographically)
    entries.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        ta.cmp(tb)
    });

    Ok(entries)
}

// --- Compute Chronicle IPC ---------------------------------------------------

#[tauri::command]
async fn get_compute_events(
    state: tauri::State<'_, SharedState>,
    slug: Option<String>,
    build_id: Option<String>,
    chain_name: Option<String>,
    content_type: Option<String>,
    step_name: Option<String>,
    primitive: Option<String>,
    depth: Option<i64>,
    model_id: Option<String>,
    source: Option<String>,
    event_type: Option<String>,
    after: Option<String>,
    before: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<wire_node_lib::pyramid::compute_chronicle::ComputeEvent>, String> {
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        let filters = wire_node_lib::pyramid::compute_chronicle::ChronicleQueryFilters {
            slug,
            build_id,
            chain_name,
            content_type,
            step_name,
            primitive,
            depth,
            model_id,
            source,
            event_type,
            after,
            before,
            limit: limit.unwrap_or(100),
            offset: offset.unwrap_or(0),
        };
        wire_node_lib::pyramid::compute_chronicle::query_events(&conn, &filters)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_compute_summary(
    state: tauri::State<'_, SharedState>,
    period_start: String,
    period_end: String,
    group_by: String,
) -> Result<Vec<wire_node_lib::pyramid::compute_chronicle::ComputeSummary>, String> {
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        wire_node_lib::pyramid::compute_chronicle::query_summary(
            &conn,
            &period_start,
            &period_end,
            &group_by,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_compute_timeline(
    state: tauri::State<'_, SharedState>,
    start: String,
    end: String,
    bucket_size_minutes: i64,
) -> Result<Vec<wire_node_lib::pyramid::compute_chronicle::TimelineBucket>, String> {
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        wire_node_lib::pyramid::compute_chronicle::query_timeline(
            &conn, &start, &end, bucket_size_minutes,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_chronicle_dimensions(
    state: tauri::State<'_, SharedState>,
) -> Result<wire_node_lib::pyramid::compute_chronicle::ChronicleDimensions, String> {
    let db_path = state
        .pyramid
        .data_dir
        .as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        wire_node_lib::pyramid::compute_chronicle::query_distinct_dimensions(&conn)
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- Fleet Roster IPC --------------------------------------------------------

#[tauri::command]
async fn get_fleet_roster(state: tauri::State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let roster = state.fleet_roster.read().await;
    serde_json::to_value(&*roster).map_err(|e| e.to_string())
}

// --- App Setup --------------------------------------------------------------

fn main() {
    let mut config = WireNodeConfig::default();

    // Load saved settings from onboarding.json
    let onboarding_path = config.data_dir().join("onboarding.json");
    if let Ok(data) = std::fs::read_to_string(&onboarding_path) {
        if let Ok(saved) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(cap) = saved.get("storage_cap_gb").and_then(|v| v.as_f64()) {
                config.storage_cap_gb = cap;
            }
            if let Some(mesh) = saved.get("mesh_hosting_enabled").and_then(|v| v.as_bool()) {
                config.mesh_hosting_enabled = mesh;
            }
            if let Some(auto) = saved.get("auto_update_enabled").and_then(|v| v.as_bool()) {
                config.auto_update_enabled = auto;
            }
        }
    }

    // Load or generate node identity BEFORE any registration attempt.
    // Uses gethostname (POSIX) for hostname detection, not env vars.
    let _ = std::fs::create_dir_all(config.data_dir());
    let node_identity = auth::NodeIdentity::load_or_generate(&config.data_dir());

    // Set up logging to both stdout and a log file
    let log_path = config.data_dir().join("wire-node.log");
    let _ = std::fs::create_dir_all(config.data_dir());
    // Truncate log on startup to keep it manageable
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .expect("Failed to open log file");

    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(log_file)),
        )
        .init();

    // Try loading a saved session
    let initial_auth = load_session(&config).unwrap_or_default();
    if initial_auth.email.is_some() {
        tracing::info!("Loaded saved session for {:?}", initial_auth.email);
    }

    // Initialize credit tracker — load persisted cumulative stats
    let stats_path = config.data_dir().join("stats.json");
    let mut initial_credits = credits::CreditTracker::load_from_file(&stats_path);
    if initial_credits.documents_served > 0 {
        tracing::info!(
            "Loaded persisted stats: {} documents served",
            initial_credits.documents_served
        );
    }
    if let Some(ref fsa) = initial_auth.first_started_at {
        initial_credits.first_started_at = Some(fsa.clone());
    }
    initial_credits.init_session();

    // Load persisted tunnel state
    let data_dir = config.data_dir();
    let initial_tunnel = tunnel::load_tunnel_state(&data_dir).unwrap_or_default();

    // Hoist auth + tunnel into shared Arcs early so the ConfigSynced
    // listener (spawned before AppState construction) can announce to
    // fleet with real node_id and tunnel_url.
    let shared_auth: Arc<RwLock<auth::AuthState>> = Arc::new(RwLock::new(initial_auth.clone()));
    let shared_tunnel: Arc<RwLock<tunnel::TunnelState>> = Arc::new(RwLock::new(initial_tunnel));

    // Hoist the shared WireNodeConfig and compute-market pending-jobs
    // map so the Phase B market integration has the same handles the
    // AppState will expose. Both must be the same Arc / self-Arc'd
    // handle as AppState's fields — the inbound /v1/compute/job-result
    // handler looks up in AppState.pending_market_jobs, and the gate
    // reads AppState.config.api_url for dispatch.
    let shared_config: Arc<RwLock<WireNodeConfig>> =
        Arc::new(RwLock::new(config.clone()));
    let shared_pending_market_jobs: wire_node_lib::pyramid::pending_jobs::PendingJobs =
        wire_node_lib::pyramid::pending_jobs::PendingJobs::new();

    // Shared JWT public key and node ID for the server module.
    // Prefer the key from AuthState (persisted in session.json) over the config default.
    let jwt_public_key = Arc::new(RwLock::new(
        initial_auth.jwt_public_key.clone().unwrap_or_else(|| config.jwt_public_key.clone())
    ));
    // Same precedence as jwt_public_key — AuthState (session.json) over
    // config.node_id. Caught during the first-ever /fill attempt on
    // 2026-04-20: BEHEM's config.node_id was empty (older boot), api_token
    // was valid so the startup re-register path below didn't run, leaving
    // node_id_shared empty for the process lifetime. `verify_market_identity`
    // then saw `self_node_id=""` on every /v1/compute/job-dispatch and
    // returned `MissingSelfNodeId` → 401 with empty body.
    //
    // AuthState.node_id is written on every register + persisted to
    // session.json, so reading it here gives the authoritative value
    // the heartbeat loop would use. No equivalent heal-from-heartbeat
    // path exists for node_id (the heartbeat response doesn't echo it),
    // so getting this init right is load-bearing.
    let node_id_shared = Arc::new(RwLock::new(
        initial_auth.node_id.clone().unwrap_or_else(|| config.node_id.clone())
    ));

    // Initialize pyramid SQLite database (reader + writer connections)
    let pyramid_db_path = config.data_dir().join("pyramid.db");
    let _ = std::fs::create_dir_all(config.data_dir());

    let pyramid_writer = rusqlite::Connection::open(&pyramid_db_path)
        .expect("Failed to open pyramid.db writer connection");
    wire_node_lib::pyramid::db::init_pyramid_db(&pyramid_writer)
        .expect("Failed to initialize pyramid schema on writer");

    let pyramid_reader = rusqlite::Connection::open(&pyramid_db_path)
        .expect("Failed to open pyramid.db reader connection");
    wire_node_lib::pyramid::db::init_pyramid_db(&pyramid_reader)
        .expect("Failed to initialize pyramid schema on reader");

    // ── Phase 5 wanderer fix: stash pyramid.db path for prompt cache ──
    //
    // The Phase 5 PromptCache uses ephemeral reader connections opened
    // from this stashed path whenever `chain_loader::resolve_prompt_refs`
    // needs to fault a prompt in from the contribution store. Without
    // this call the cache stays cold and `chain_loader` would fall
    // through to disk on every lookup — the Phase 5 contribution-backed
    // prompt lookup would be dead code. Must be set BEFORE any chain
    // load attempt, which is safe here because the migration and
    // subsequent chain execution both happen after this point.
    wire_node_lib::pyramid::prompt_cache::set_global_prompt_cache_db_path(
        pyramid_db_path.clone(),
    );

    // Load pyramid config from disk (or use defaults)
    let pyramid_config = wire_node_lib::pyramid::PyramidConfig::load(&config.data_dir());
    tracing::info!(
        "Pyramid config loaded (api_key set: {})",
        !pyramid_config.openrouter_api_key.is_empty()
    );

    // Resolve chains directory: in dev mode, use the source tree so prompt .md
    // files are read live; in release mode, use the data_dir copy.
    #[cfg(debug_assertions)]
    let chains_dir = {
        let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chains");
        if src.exists() {
            src.canonicalize().unwrap_or(src)
        } else {
            config.data_dir().join("chains")
        }
    };
    #[cfg(not(debug_assertions))]
    let chains_dir = config.data_dir().join("chains");

    tracing::info!("chains_dir resolved to {:?}", chains_dir);

    // Sync chain files: source tree → data dir if available, else bootstrap with embedded defaults.
    // In debug mode, chains_dir already points to the source tree so no sync needed.
    // In release mode, we check if the source tree exists alongside the binary.
    #[cfg(debug_assertions)]
    let source_chains_for_sync: Option<&std::path::Path> = None; // chains_dir IS the source tree
    #[cfg(not(debug_assertions))]
    let source_chains_for_sync = {
        let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chains");
        if src.exists() { Some(src) } else { None }
    };
    if let Err(e) = wire_node_lib::pyramid::chain_loader::ensure_default_chains(
        &chains_dir,
        source_chains_for_sync.as_deref(),
    ) {
        tracing::warn!("Failed to sync chain files: {e}");
    }

    // ── Phase 5: migrate on-disk prompts + chains to contributions ──────
    //
    // Phase 5 replaces the on-disk prompt/chain resolution path with
    // `pyramid_config_contributions` rows + a PromptCache. On first run
    // (or the first run after a Phase 5 upgrade), this walks the chains
    // directory above and inserts one `skill` row per `.md` prompt and
    // one `custom_chain` row per chain YAML. Idempotent via a sentinel
    // row — subsequent runs short-circuit on its presence.
    //
    // Failure mode: per-file failures are logged and the run proceeds.
    // A whole-run failure (e.g. DB error) is logged at WARN but the
    // app still boots — the chain loader's on-disk fallback path keeps
    // the executor working until the migration can be re-run.
    match wire_node_lib::pyramid::wire_migration::migrate_prompts_and_chains_to_contributions(
        &pyramid_writer,
        &chains_dir,
    ) {
        Ok(report) if report.ran => {
            tracing::info!(
                prompts_inserted = report.prompts_inserted,
                chains_inserted = report.chains_inserted,
                prompts_failed = report.prompts_failed,
                chains_failed = report.chains_failed,
                marker_written = report.marker_written,
                "Phase 5 prompt+chain migration completed"
            );
        }
        Ok(_report) => {
            tracing::debug!("Phase 5 prompt+chain migration: already migrated (sentinel present)");
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Phase 5 prompt+chain migration failed; on-disk fallback still active"
            );
        }
    }

    // ── Phase 3: credential store + provider registry ────────────────────
    //
    // Load the `.credentials` file from the data dir (creates an empty
    // store on first run). Then hydrate the provider registry from the
    // DB; the first `init_pyramid_db` call above will have seeded the
    // default OpenRouter provider + four tier routing entries.
    let credential_store: std::sync::Arc<wire_node_lib::pyramid::credentials::CredentialStore> =
        match wire_node_lib::pyramid::credentials::CredentialStore::load(&config.data_dir()) {
            Ok(store) => std::sync::Arc::new(store),
            Err(e) => {
                tracing::error!(
                    "failed to load .credentials file ({}): LLM calls requiring credentials will fail until this is resolved. \
                     Visit Settings → Credentials to fix the file permissions.",
                    e
                );
                // Load from a throwaway path so the app still boots. The
                // user can use Settings → Credentials to repair.
                let fallback_path = config.data_dir().join(".credentials");
                std::sync::Arc::new(
                    wire_node_lib::pyramid::credentials::CredentialStore::load_from_path(
                        fallback_path,
                    )
                    .unwrap_or_else(|_| {
                        // As a last resort, create an in-memory empty store
                        // rooted at the expected path so resolve_var still
                        // returns the spec's clear error.
                        wire_node_lib::pyramid::credentials::CredentialStore::load_from_path(
                            config.data_dir().join(".credentials.fallback"),
                        )
                        .expect("unreachable: empty store construction")
                    }),
                )
            }
        };

    // ── Boot-time migration: seed .credentials from pyramid_config.json ──
    //
    // Users upgrading to Phase 3 have their OpenRouter API key in the old
    // pyramid_config.json but the credential store is empty. Seed once so
    // the provider registry can resolve it at build time.
    if !pyramid_config.openrouter_api_key.is_empty()
        && !credential_store.contains("OPENROUTER_KEY")
    {
        match credential_store.set("OPENROUTER_KEY", &pyramid_config.openrouter_api_key) {
            Ok(()) => tracing::info!(
                "Migrated OpenRouter API key from pyramid_config.json → .credentials"
            ),
            Err(e) => tracing::warn!(
                "Failed to migrate OpenRouter API key to .credentials: {e}"
            ),
        }
    }

    let provider_registry = std::sync::Arc::new(
        wire_node_lib::pyramid::provider::ProviderRegistry::new(credential_store.clone()),
    );
    {
        // Open a one-shot connection to hydrate the in-memory registry
        // from the DB. The default seed rows were already inserted by
        // `init_pyramid_db` above, so this picks up Adam's 4 tier
        // routing entries on first run.
        match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                if let Err(e) = provider_registry.load_from_db(&conn) {
                    tracing::error!(
                        "failed to hydrate provider registry from DB: {}; LLM routing will be broken until fixed",
                        e
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    "failed to open pyramid connection for registry hydration: {}",
                    e
                );
            }
        }
    }

    // Phase 9: hydrate the schema registry from pyramid_config_contributions.
    // Runs AFTER the Phase 5+9 migration above so bundled manifest entries
    // are visible. The registry is held on PyramidState as an Arc so IPC
    // handlers (pyramid_config_schemas, pyramid_generate_config, etc.) can
    // share a single view without cloning on every call.
    let schema_registry: std::sync::Arc<
        wire_node_lib::pyramid::schema_registry::SchemaRegistry,
    > = {
        let registry = match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                wire_node_lib::pyramid::schema_registry::SchemaRegistry::hydrate_from_contributions(&conn)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "failed to hydrate Phase 9 schema registry: {}; starting empty",
                            e
                        );
                        wire_node_lib::pyramid::schema_registry::SchemaRegistry::new()
                    })
            }
            Err(e) => {
                tracing::warn!(
                    "failed to open pyramid connection for schema registry hydration: {}",
                    e
                );
                wire_node_lib::pyramid::schema_registry::SchemaRegistry::new()
            }
        };
        std::sync::Arc::new(registry)
    };

    let pyramid_state = Arc::new(wire_node_lib::pyramid::PyramidState {
        reader: Arc::new(tokio::sync::Mutex::new(pyramid_reader)),
        writer: Arc::new(tokio::sync::Mutex::new(pyramid_writer)),
        config: Arc::new(RwLock::new(
            pyramid_config.to_llm_config_with_runtime(
                provider_registry.clone(),
                credential_store.clone(),
            ),
        )),
        active_build: Arc::new(RwLock::new(std::collections::HashMap::new())),
        data_dir: Some(config.data_dir()),
        stale_engines: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        file_watchers: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        vine_builds: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        use_chain_engine: std::sync::atomic::AtomicBool::new(pyramid_config.use_chain_engine),
        use_ir_executor: std::sync::atomic::AtomicBool::new(pyramid_config.use_ir_executor),
        event_bus: Arc::new(wire_node_lib::pyramid::event_chain::LocalEventBus::new()),
        operational: Arc::new(pyramid_config.operational.clone()),
        chains_dir: chains_dir.clone(),
        remote_query_rate_limiter: Arc::new(tokio::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        absorption_gate: Arc::new(tokio::sync::Mutex::new(
            wire_node_lib::pyramid::AbsorptionGate::new(),
        )),
        build_event_bus: Arc::new(
            wire_node_lib::pyramid::event_bus::BuildEventBus::new(),
        ),
        supabase_url: Some(config.supabase_url.clone()),
        supabase_anon_key: Some(config.supabase_anon_key.clone()),
        csrf_secret: {
            let a = *uuid::Uuid::new_v4().as_bytes();
            let b = *uuid::Uuid::new_v4().as_bytes();
            let mut s = [0u8; 32];
            s[..16].copy_from_slice(&a);
            s[16..].copy_from_slice(&b);
            s
        },
        dadbear_handle: Arc::new(tokio::sync::Mutex::new(None)),
        dadbear_supervisor_handle: Arc::new(tokio::sync::Mutex::new(None)),
        // Phase 1 fix: shared per-config DADBEAR in-flight flag map.
        // Consulted by both the tick loop and `trigger_for_slug` so two
        // concurrent `run_tick_for_config` calls for the same config cannot
        // both reach dispatch (the HTTP-trigger-vs-auto-dispatch race).
        dadbear_in_flight: Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        provider_registry: provider_registry.clone(),
        credential_store: credential_store.clone(),
        schema_registry: schema_registry.clone(),
        cross_pyramid_router: Arc::new(
            wire_node_lib::pyramid::cross_pyramid_router::CrossPyramidEventRouter::new(),
        ),
        // Phase 4 Daemon Control Plane: Ollama pull state.
        ollama_pull_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ollama_pull_in_progress: Arc::new(tokio::sync::Mutex::new(None)),
    });

    // Load persisted event subscriptions into the in-memory event bus
    {
        let reader = pyramid_state.reader.blocking_lock();
        if let Err(e) = pyramid_state.event_bus.load_from_db_sync(&reader) {
            tracing::warn!("Failed to load event subscriptions from DB: {e}");
        }
    }

    // Phase 1 compute queue: construct early so it can be wired onto
    // the LlmConfig alongside dispatch_policy + provider_pools.
    let compute_queue_handle = wire_node_lib::compute_queue::ComputeQueueHandle::new();

    // Fleet roster: construct early so it can be wired onto LlmConfig
    // alongside compute_queue. Same Arc<RwLock<>> pattern.
    let fleet_roster = Arc::new(RwLock::new(wire_node_lib::fleet::FleetRoster::default()));

    // ── Async fleet dispatch: hydrate FleetDeliveryPolicy + construct ────
    // FleetDispatchContext (Init Ordering steps 2-5 of async-fleet-dispatch).
    //
    // 1. Read the singleton `pyramid_fleet_delivery_policy` row. Fall back to
    //    `FleetDeliveryPolicy::default()` (bootstrap sentinels) when the row
    //    is missing, malformed, or the DB connection fails — defaults are
    //    intentionally conservative and let the node accept dispatches while
    //    the seed/contribution lands.
    // 2. Run peer startup recovery BEFORE sweep loops start, converting any
    //    `pending` outbox rows left behind by a prior crash into `ready` with
    //    a synthesized worker error payload. Without this, a crashed worker
    //    could strand rows in `pending` forever.
    // 3. Construct `FleetDispatchContext` and wire it onto the live
    //    `LlmConfig.fleet_dispatch` overlay (same Arc<RwLock<>> pattern as
    //    fleet_roster + compute_queue above).
    let initial_fleet_delivery_policy = {
        match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                // Read policy — fall through to defaults on any failure.
                let policy = match wire_node_lib::pyramid::fleet_delivery_policy::read_fleet_delivery_policy(&conn) {
                    Ok(Some(p)) => {
                        tracing::info!("Fleet delivery policy loaded from DB");
                        p
                    }
                    Ok(None) => {
                        tracing::info!(
                            "No fleet_delivery_policy row; using bootstrap defaults (seed will land once contribution is synced)"
                        );
                        wire_node_lib::pyramid::fleet_delivery_policy::FleetDeliveryPolicy::default()
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to read fleet_delivery_policy from DB: {e}; using bootstrap defaults"
                        );
                        wire_node_lib::pyramid::fleet_delivery_policy::FleetDeliveryPolicy::default()
                    }
                };
                // Peer startup recovery: stuck pending → ready with synth error.
                match wire_node_lib::pyramid::db::fleet_outbox_startup_recovery(
                    &conn,
                    policy.ready_retention_secs,
                ) {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "Fleet outbox startup recovery: promoted {n} stuck pending row(s) to ready"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Fleet outbox startup recovery failed: {e}");
                    }
                }
                policy
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to open pyramid connection for fleet_delivery_policy hydration: {e}; using bootstrap defaults"
                );
                wire_node_lib::pyramid::fleet_delivery_policy::FleetDeliveryPolicy::default()
            }
        }
    };

    let fleet_dispatch_ctx = Arc::new(wire_node_lib::fleet::FleetDispatchContext {
        tunnel_state: shared_tunnel.clone(),
        fleet_roster: fleet_roster.clone(),
        pending: Arc::new(wire_node_lib::fleet::PendingFleetJobs::new()),
        policy: Arc::new(tokio::sync::RwLock::new(initial_fleet_delivery_policy)),
    });

    // Phase A: hydrate dispatch policy + provider pools from DB.
    // Uses a one-shot connection (same pattern as registry/schema hydration
    // above). When a policy exists, the per-provider pools replace the
    // global LOCAL_PROVIDER_SEMAPHORE and global rate_limit_wait.
    //
    // Pillar 37 / Local Mode toggle fix (2026-04-21): the authored YAML
    // is read unchanged, then `apply_local_mode_overlay` filters non-local
    // `route_to` entries when `pyramid_local_mode_state.enabled = true`.
    // The operator's authored `dispatch_policy` contribution is never
    // superseded by Local Mode.
    {
        match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                if let Ok(Some(yaml_str)) = wire_node_lib::pyramid::db::read_dispatch_policy(&conn) {
                    match serde_yaml::from_str::<wire_node_lib::pyramid::dispatch_policy::DispatchPolicyYaml>(&yaml_str) {
                        Ok(yaml) => {
                            let local_mode_enabled =
                                wire_node_lib::pyramid::db::load_local_mode_state(&conn)
                                    .map(|row| row.enabled)
                                    .unwrap_or(false);
                            let effective_yaml =
                                wire_node_lib::pyramid::dispatch_policy::apply_local_mode_overlay(
                                    yaml,
                                    local_mode_enabled,
                                );
                            let policy = wire_node_lib::pyramid::dispatch_policy::DispatchPolicy::from_yaml(&effective_yaml);
                            let pools = wire_node_lib::pyramid::provider_pools::ProviderPools::new(&policy);
                            let mut cfg = pyramid_state.config.blocking_write();
                            cfg.dispatch_policy = Some(std::sync::Arc::new(policy));
                            cfg.provider_pools = Some(std::sync::Arc::new(pools));
                            cfg.compute_queue = Some(compute_queue_handle.clone());
                            cfg.fleet_roster = Some(fleet_roster.clone());
                            cfg.fleet_dispatch = Some(Arc::clone(&fleet_dispatch_ctx));
                            cfg.compute_market_context = Some(
                                wire_node_lib::pyramid::compute_market_ctx::ComputeMarketRequesterContext {
                                    auth: shared_auth.clone(),
                                    config: shared_config.clone(),
                                    pending_jobs: shared_pending_market_jobs.clone(),
                                    tunnel_state: shared_tunnel.clone(),
                                },
                            );
                            tracing::info!("Dispatch policy loaded from DB — per-provider pools active, compute queue wired");
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse dispatch policy YAML: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to open pyramid connection for dispatch policy hydration: {e}");
            }
        }
        // Even when no dispatch policy exists (e.g. fresh install), wire
        // the compute queue onto the LlmConfig so local builds route
        // through the queue from the start.
        {
            let mut cfg = pyramid_state.config.blocking_write();
            if cfg.compute_queue.is_none() {
                cfg.compute_queue = Some(compute_queue_handle.clone());
            }
            if cfg.fleet_roster.is_none() {
                cfg.fleet_roster = Some(fleet_roster.clone());
            }
            if cfg.fleet_dispatch.is_none() {
                cfg.fleet_dispatch = Some(Arc::clone(&fleet_dispatch_ctx));
            }
            if cfg.compute_market_context.is_none() {
                cfg.compute_market_context = Some(
                    wire_node_lib::pyramid::compute_market_ctx::ComputeMarketRequesterContext {
                        auth: shared_auth.clone(),
                        config: shared_config.clone(),
                        pending_jobs: shared_pending_market_jobs.clone(),
                        tunnel_state: shared_tunnel.clone(),
                    },
                );
            }
            // Walker rev 2.1: MarketSurfaceCache polls /api/v1/compute/market-surface
            // every 60s; walker consults it on the "market" branch as an advisory
            // pre-filter. `/quote` remains the authoritative viability check.
            if cfg.market_surface_cache.is_none() {
                let cache = std::sync::Arc::new(
                    wire_node_lib::pyramid::market_surface_cache::MarketSurfaceCache::new(
                        shared_auth.clone(),
                        shared_config.clone(),
                    ),
                );
                let cache_for_poller = cache.clone();
                tauri::async_runtime::spawn(async move {
                    wire_node_lib::pyramid::market_surface_cache::MarketSurfaceCache::spawn_poller(
                        cache_for_poller,
                    );
                });
                cfg.market_surface_cache = Some(cache);
            }
        }
    }

    // Phase 1 daemon control plane (AD-8 Part 1): ConfigSynced listener for
    // dispatch_policy. When a dispatch_policy contribution is synced to the
    // operational table, rebuild the in-memory ProviderPools and update the
    // stale engine's defer_maintenance AtomicBool. Without this listener,
    // creating a dispatch_policy contribution writes YAML but the live
    // LlmConfig.provider_pools stays None — the pool wiring is dead on arrival.
    {
        let ps = pyramid_state.clone();
        let db_path = pyramid_db_path.to_string_lossy().to_string();
        let config_fleet_roster = fleet_roster.clone();
        let config_compute_queue = compute_queue_handle.clone();
        let config_auth = shared_auth.clone();
        let config_tunnel = shared_tunnel.clone();
        let config_node_identity = node_identity.clone();
        let config_fleet_dispatch = Arc::clone(&fleet_dispatch_ctx);
        let mut rx = ps.build_event_bus.tx.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let wire_node_lib::pyramid::event_bus::TaggedKind::ConfigSynced {
                            schema_type,
                            ..
                        } = &event.kind
                        {
                            if schema_type == "dispatch_policy" {
                                // Read the updated YAML from the operational table.
                                let yaml_opt = {
                                    let conn_result = wire_node_lib::pyramid::db::open_pyramid_connection(
                                        std::path::Path::new(&db_path),
                                    );
                                    match conn_result {
                                        Ok(conn) => wire_node_lib::pyramid::db::read_dispatch_policy(&conn)
                                            .ok()
                                            .flatten(),
                                        Err(_) => None,
                                    }
                                };
                                if let Some(yaml_str) = yaml_opt {
                                    match serde_yaml::from_str::<wire_node_lib::pyramid::dispatch_policy::DispatchPolicyYaml>(&yaml_str) {
                                        Ok(yaml) => {
                                            // Pillar 37 / Local Mode toggle fix: apply the
                                            // derived-view overlay before constructing the
                                            // runtime policy. When `pyramid_local_mode_state.
                                            // enabled = true`, non-local `route_to` entries
                                            // are filtered out and `defer_maintenance_during_
                                            // build` is pinned on. The authored contribution
                                            // YAML (re-read above from the operational table)
                                            // is never mutated by Local Mode.
                                            let local_mode_enabled = {
                                                let reader = ps.reader.lock().await;
                                                wire_node_lib::pyramid::db::load_local_mode_state(&reader)
                                                    .map(|row| row.enabled)
                                                    .unwrap_or(false)
                                            };
                                            let yaml = wire_node_lib::pyramid::dispatch_policy::apply_local_mode_overlay(
                                                yaml,
                                                local_mode_enabled,
                                            );
                                            let policy = wire_node_lib::pyramid::dispatch_policy::DispatchPolicy::from_yaml(&yaml);
                                            let pools = wire_node_lib::pyramid::provider_pools::ProviderPools::new(&policy);

                                            // Update stale engines' defer_maintenance atomic.
                                            let new_defer = policy.build_coordination.defer_maintenance_during_build;
                                            {
                                                let engines = ps.stale_engines.lock().await;
                                                for engine in engines.values() {
                                                    engine.defer_maintenance_during_build.store(
                                                        new_defer,
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                }
                                            }

                                            // Re-announce to fleet when dispatch policy changes.
                                            // Derive new serving_rules from the updated policy.
                                            {
                                                let loaded_models = {
                                                    let reader = ps.reader.lock().await;
                                                    match wire_node_lib::pyramid::db::load_local_mode_state(&reader) {
                                                        Ok(row) if row.enabled => {
                                                            row.ollama_model.into_iter().collect::<Vec<_>>()
                                                        }
                                                        _ => Vec::new(),
                                                    }
                                                };
                                                let serving_rules = wire_node_lib::fleet::derive_serving_rules(&policy, &loaded_models);
                                                let roster = config_fleet_roster.read().await;
                                                if !roster.peers.is_empty() {
                                                    let total_queue_depth = {
                                                        let q = config_compute_queue.queue.lock().await;
                                                        q.total_depth()
                                                    };
                                                    // Queue depths for observability
                                                    let queue_depths = {
                                                        let q = config_compute_queue.queue.lock().await;
                                                        q.all_depths()
                                                    };
                                                    // Note: node_id and operator_id come from the roster's
                                                    // self_operator_id; for a full announcement we'd need
                                                    // auth state too. Use the fleet_jwt presence as a
                                                    // signal that we have auth.
                                                    if roster.fleet_jwt.is_some() {
                                                        // FleetAnnouncement.tunnel_url is TunnelUrl (WS9, no Default).
                                                        // Without a tunnel URL we have nothing meaningful to announce — skip
                                                        // just the announce block (NOT the whole ConfigSynced handler,
                                                        // which still needs to apply the dispatch_policy write below).
                                                        let tunnel_url_opt = {
                                                            let ts = config_tunnel.read().await;
                                                            ts.tunnel_url.clone()
                                                        };
                                                        if let Some(tunnel_url) = tunnel_url_opt {
                                                            let auth = config_auth.read().await;
                                                            let self_node_id = auth.node_id.clone().unwrap_or_default();
                                                            let self_operator_id = auth.operator_id.clone().unwrap_or_default();
                                                            let self_operator_handle = auth.operator_handle.clone();
                                                            drop(auth);
                                                            let self_node_handle = Some(config_node_identity.node_handle.clone());
                                                            let announcement = wire_node_lib::fleet::FleetAnnouncement {
                                                                node_id: self_node_id,
                                                                name: None,
                                                                node_handle: self_node_handle,
                                                                operator_handle: self_operator_handle,
                                                                tunnel_url,
                                                                models_loaded: loaded_models,
                                                                serving_rules,
                                                                queue_depths,
                                                                total_queue_depth,
                                                                operator_id: self_operator_id,
                                                            };
                                                            wire_node_lib::fleet::announce_to_fleet(&roster, &announcement).await;
                                                            tracing::info!("ConfigSynced: re-announced to fleet with updated serving_rules");
                                                        } else {
                                                            tracing::debug!("ConfigSynced: skipping fleet announce — no tunnel URL yet");
                                                        }
                                                    }
                                                }
                                            }

                                            // Write dispatch_policy + provider_pools onto the live LlmConfig.
                                            let mut cfg = ps.config.write().await;
                                            cfg.dispatch_policy = Some(std::sync::Arc::new(policy));
                                            cfg.provider_pools = Some(std::sync::Arc::new(pools));
                                            tracing::info!(
                                                "ConfigSynced: dispatch_policy reloaded — provider_pools rebuilt, defer_maintenance={}",
                                                new_defer,
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!("ConfigSynced: failed to parse dispatch_policy YAML: {e}");
                                        }
                                    }
                                }
                            } else if schema_type == "fleet_delivery_policy" {
                                // Async fleet dispatch: reload the operational
                                // policy into the live `FleetDispatchContext`.
                                // Runs in parallel to the `dispatch_policy`
                                // branch above; both arms are independent.
                                let yaml_opt = {
                                    let conn_result = wire_node_lib::pyramid::db::open_pyramid_connection(
                                        std::path::Path::new(&db_path),
                                    );
                                    match conn_result {
                                        Ok(conn) => wire_node_lib::pyramid::fleet_delivery_policy::read_fleet_delivery_policy(&conn)
                                            .ok()
                                            .flatten(),
                                        Err(_) => None,
                                    }
                                };
                                if let Some(new_policy) = yaml_opt {
                                    *config_fleet_dispatch.policy.write().await = new_policy;
                                    tracing::info!("ConfigSynced: fleet_delivery_policy reloaded");
                                } else {
                                    tracing::warn!("ConfigSynced: fleet_delivery_policy broadcast but no row readable");
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!("ConfigSynced listener lagged by {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!("ConfigSynced listener: bus closed, exiting");
                        break;
                    }
                }
            }
        });
    }

    // ── Async fleet dispatch: best-effort seed of fleet_delivery_policy ──
    //
    // If no `fleet_delivery_policy` contribution exists yet, insert one
    // from the embedded seed YAML and sync it to the operational table.
    // The listener above is already installed — the `ConfigSynced` it
    // broadcasts will be received and will refresh the live
    // `FleetDispatchContext.policy` value.
    //
    // Best-effort: any failure is logged and swallowed. The bootstrap
    // sentinel defaults that the `FleetDispatchContext` was constructed
    // with above are conservative and let the node keep functioning
    // while this lands on a later boot.
    {
        const SEED_FLEET_DELIVERY_POLICY_YAML: &str =
            include_str!("../../docs/seeds/fleet_delivery_policy.yaml");
        match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                match wire_node_lib::pyramid::config_contributions::load_active_config_contribution(
                    &conn,
                    "fleet_delivery_policy",
                    None,
                ) {
                    Ok(Some(_)) => {
                        tracing::debug!(
                            "fleet_delivery_policy contribution already present — skipping seed"
                        );
                    }
                    Ok(None) => {
                        match wire_node_lib::pyramid::config_contributions::create_config_contribution(
                            &conn,
                            "fleet_delivery_policy",
                            None,
                            SEED_FLEET_DELIVERY_POLICY_YAML,
                            Some("bundled seed at first boot"),
                            "bundled",
                            Some("system"),
                            "active",
                        ) {
                            Ok(contribution_id) => {
                                match wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
                                    &conn,
                                    &contribution_id,
                                ) {
                                    Ok(Some(contribution)) => {
                                        if let Err(e) = wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
                                            &conn,
                                            &pyramid_state.build_event_bus,
                                            &contribution,
                                        ) {
                                            tracing::warn!(
                                                "fleet_delivery_policy seed: sync_config_to_operational failed: {e}"
                                            );
                                        } else {
                                            tracing::info!(
                                                "fleet_delivery_policy seeded from docs/seeds/fleet_delivery_policy.yaml (contribution_id={contribution_id})"
                                            );
                                        }
                                    }
                                    Ok(None) => {
                                        tracing::warn!(
                                            "fleet_delivery_policy seed: contribution {contribution_id} not found after create"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "fleet_delivery_policy seed: load_contribution_by_id failed: {e}"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "fleet_delivery_policy seed: create_config_contribution failed: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "fleet_delivery_policy seed: load_active_config_contribution failed: {e}"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "fleet_delivery_policy seed: failed to open pyramid connection: {e}"
                );
            }
        }
    }

    // ── Compute market: best-effort seed of market_delivery_policy ──
    //
    // Shape-parallel to the fleet_delivery_policy seed block above. If no
    // `market_delivery_policy` contribution exists yet, insert one from
    // the embedded seed YAML and sync it to the singleton operational
    // table `pyramid_market_delivery_policy`. Phase 2 WS1+ will construct
    // a `MarketDispatchContext` that reads this table at boot and holds
    // the runtime `Arc<RwLock<MarketDeliveryPolicy>>` — at which point a
    // ConfigSynced reload branch parallel to the fleet one above will be
    // added to this main.rs.
    //
    // Seeding now (ahead of the runtime context) is harmless: the
    // operational row lands, the contribution is tracked, and the Phase
    // 2+ code inherits a populated table on its first read. Without this
    // seed, operators would hit "no contribution to supersede" when they
    // first try to tune the market policy.
    //
    // Best-effort: any failure is logged and swallowed. Per-node nodes
    // that fail to seed will fall through to `MarketDeliveryPolicy::
    // default()` (which `default_matches_seed_yaml` enforces as
    // byte-equivalent to the seed).
    {
        const SEED_MARKET_DELIVERY_POLICY_YAML: &str =
            include_str!("../../docs/seeds/market_delivery_policy.yaml");
        match wire_node_lib::pyramid::db::open_pyramid_connection(&pyramid_db_path) {
            Ok(conn) => {
                match wire_node_lib::pyramid::config_contributions::load_active_config_contribution(
                    &conn,
                    "market_delivery_policy",
                    None,
                ) {
                    Ok(Some(_)) => {
                        tracing::debug!(
                            "market_delivery_policy contribution already present — skipping seed"
                        );
                    }
                    Ok(None) => {
                        match wire_node_lib::pyramid::config_contributions::create_config_contribution(
                            &conn,
                            "market_delivery_policy",
                            None,
                            SEED_MARKET_DELIVERY_POLICY_YAML,
                            Some("bundled seed at first boot"),
                            "bundled",
                            Some("system"),
                            "active",
                        ) {
                            Ok(contribution_id) => {
                                match wire_node_lib::pyramid::config_contributions::load_contribution_by_id(
                                    &conn,
                                    &contribution_id,
                                ) {
                                    Ok(Some(contribution)) => {
                                        if let Err(e) = wire_node_lib::pyramid::config_contributions::sync_config_to_operational(
                                            &conn,
                                            &pyramid_state.build_event_bus,
                                            &contribution,
                                        ) {
                                            tracing::warn!(
                                                "market_delivery_policy seed: sync_config_to_operational failed: {e}"
                                            );
                                        } else {
                                            tracing::info!(
                                                "market_delivery_policy seeded from docs/seeds/market_delivery_policy.yaml (contribution_id={contribution_id})"
                                            );
                                        }
                                    }
                                    Ok(None) => {
                                        tracing::warn!(
                                            "market_delivery_policy seed: contribution {contribution_id} not found after create"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "market_delivery_policy seed: load_contribution_by_id failed: {e}"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "market_delivery_policy seed: create_config_contribution failed: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "market_delivery_policy seed: load_active_config_contribution failed: {e}"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "market_delivery_policy seed: failed to open pyramid connection: {e}"
                );
            }
        }
    }

    // ── Async fleet dispatch: spawn sweep loops (Init Ordering step 9) ──
    //
    // Two sweeps, one context. Both are fire-and-forget until app exit.
    //
    // 1. `pending_jobs_sweep_loop` — dispatcher-side orphan sweep. Evicts
    //    stale `PendingFleetJob` entries whose callback never arrived.
    //    Dropping the entry drops its `oneshot::Sender`, waking the
    //    Phase A await with `RecvError` so it falls through to local.
    //
    // 2. `fleet_outbox_sweep_loop` — peer-side outbox transitions + retry.
    //    Predicate A transitions rows by `expires_at`; Predicate B
    //    retries `ready` rows whose backoff has elapsed. See
    //    `pyramid::fleet_outbox_sweep` for the exact state machine.
    //
    // Spec ordering: these come up BEFORE warp `start_server` below so
    // there's no window where dispatch routes accept work that has no
    // retry machinery attached.
    {
        let ctx_clone = Arc::clone(&fleet_dispatch_ctx);
        let db_path_for_pending_sweep = Some(pyramid_db_path.to_path_buf());
        tauri::async_runtime::spawn(async move {
            wire_node_lib::fleet::pending_jobs_sweep_loop(
                ctx_clone,
                db_path_for_pending_sweep,
            )
            .await;
        });
    }
    {
        let ctx_clone = Arc::clone(&fleet_dispatch_ctx);
        let db_path_clone = pyramid_db_path.to_path_buf();
        tauri::async_runtime::spawn(async move {
            wire_node_lib::pyramid::fleet_outbox_sweep::fleet_outbox_sweep_loop(
                db_path_clone,
                ctx_clone,
            )
            .await;
        });
    }

    tracing::info!(
        "Pyramid engine initialized at {:?}, ir_executor={}",
        pyramid_db_path,
        pyramid_config.use_ir_executor
    );

    // ── Phase 1 Compute Queue: GPU processing loop ──────────────────
    //
    // MUST start consuming BEFORE any producer (stale engine, builds,
    // DADBEAR) tries to enqueue. If this runs after stale engine init,
    // enqueued items block forever with no consumer.
    //
    // The loop waits on the Notify signal, then drains all available
    // items from the round-robin queue. Each item is a complete LLM
    // call context; the loop executes it through the existing LLM path
    // with compute_queue: None (prevents re-enqueue) and
    // skip_concurrency_gate: true (bypasses semaphore).
    {
        let queue_handle = compute_queue_handle.clone();
        let bus = pyramid_state.build_event_bus.clone();
        let chronicle_db_path = pyramid_db_path.to_string_lossy().to_string();
        tauri::async_runtime::spawn(async move {
            tracing::info!("Compute queue GPU processing loop started");
            loop {
                queue_handle.notify.notified().await;
                // Drain all available items.
                loop {
                    let entry = {
                        let mut q = queue_handle.queue.lock().await;
                        q.dequeue_next()
                    };
                    match entry {
                        Some(mut entry) => {
                            let start = std::time::Instant::now();
                            let model_id = entry.model_id.clone();

                            // Use explicit entry.source instead of inferring from step_ctx.
                            let job_source = entry.source.clone();

                            // Emit QueueJobStarted event.
                            let _ = bus.tx.send(wire_node_lib::pyramid::event_bus::TaggedBuildEvent {
                                slug: "__compute__".to_string(),
                                kind: wire_node_lib::pyramid::event_bus::TaggedKind::QueueJobStarted {
                                    model_id: model_id.clone(),
                                    source: job_source.clone(),
                                },
                            });

                            // WP-2: Chronicle started event
                            {
                                let queue_wait_ms = entry.enqueued_at.elapsed().as_millis() as u64;
                                let db_path = chronicle_db_path.clone();
                                let source = job_source.clone();
                                let job_path = entry.job_path.clone();
                                let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
                                    wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                        sc, &job_path, "started", &source,
                                    )
                                } else {
                                    wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                                        &job_path, "started", &source,
                                    )
                                    .with_model_id(model_id.clone())
                                };
                                let chronicle_ctx = chronicle_ctx
                                    .with_metadata(serde_json::json!({ "queue_wait_ms": queue_wait_ms }))
                                    .with_work_item(entry.work_item_id.clone(), entry.attempt_id.clone());
                                tokio::task::spawn_blocking(move || {
                                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                        let _ = wire_node_lib::pyramid::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                    }
                                });
                            }

                            // Thread chronicle_job_path through LlmCallOptions so cloud
                            // fallthrough events (WP-8) share the same job_path.
                            entry.options.chronicle_job_path = Some(entry.job_path.clone());

                            // Execute through the existing LLM path.
                            // entry.config has compute_queue: None (won't re-enqueue).
                            // entry.options has skip_concurrency_gate: true.
                            //
                            // Panic guard: if the LLM call panics, catch it and
                            // convert to an error so the GPU loop survives and
                            // continues draining. Without this, a single panic
                            // kills the loop and all subsequent callers hang.
                            let result = std::panic::AssertUnwindSafe(
                                wire_node_lib::pyramid::llm::call_model_unified_with_audit_and_ctx(
                                    &entry.config,
                                    entry.step_ctx.as_ref(),
                                    None, // no audit context in queue replay
                                    &entry.system_prompt,
                                    &entry.user_prompt,
                                    entry.temperature,
                                    entry.max_tokens,
                                    entry.response_format.as_ref(),
                                    entry.options.clone(),
                                )
                            )
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|panic_payload| {
                                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                    format!("GPU loop: LLM call panicked: {s}")
                                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                                    format!("GPU loop: LLM call panicked: {s}")
                                } else {
                                    "GPU loop: LLM call panicked (unknown payload)".to_string()
                                };
                                tracing::error!("{}", msg);
                                Err(anyhow::anyhow!("{}", msg))
                            });

                            let elapsed_ms = start.elapsed().as_millis() as u64;

                            // WP-3/WP-4: Chronicle completed/failed events
                            match &result {
                                Ok(response) => {
                                    let db_path = chronicle_db_path.clone();
                                    let source = job_source.clone();
                                    let job_path = entry.job_path.clone();
                                    let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
                                        wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                            sc, &job_path, "completed", &source,
                                        )
                                    } else {
                                        wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                                            &job_path, "completed", &source,
                                        )
                                        .with_model_id(model_id.clone())
                                    };
                                    let chronicle_ctx = chronicle_ctx
                                        .with_metadata(serde_json::json!({
                                            "latency_ms": elapsed_ms,
                                            "tokens_prompt": response.usage.prompt_tokens,
                                            "tokens_completion": response.usage.completion_tokens,
                                            "cost_usd": response.actual_cost_usd,
                                            "generation_id": response.generation_id,
                                        }))
                                        .with_work_item(entry.work_item_id.clone(), entry.attempt_id.clone());
                                    tokio::task::spawn_blocking(move || {
                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                            let _ = wire_node_lib::pyramid::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                        }
                                    });
                                }
                                Err(e) => {
                                    let db_path = chronicle_db_path.clone();
                                    let source = job_source.clone();
                                    let job_path = entry.job_path.clone();
                                    let error_msg = e.to_string();
                                    let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
                                        wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                            sc, &job_path, "failed", &source,
                                        )
                                    } else {
                                        wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                                            &job_path, "failed", &source,
                                        )
                                        .with_model_id(model_id.clone())
                                    };
                                    let chronicle_ctx = chronicle_ctx
                                        .with_metadata(serde_json::json!({
                                            "error": error_msg,
                                            "latency_ms": elapsed_ms,
                                        }))
                                        .with_work_item(entry.work_item_id.clone(), entry.attempt_id.clone());
                                    tokio::task::spawn_blocking(move || {
                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                            let _ = wire_node_lib::pyramid::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                        }
                                    });
                                }
                            }

                            // Emit QueueJobCompleted event.
                            let _ = bus.tx.send(wire_node_lib::pyramid::event_bus::TaggedBuildEvent {
                                slug: "__compute__".to_string(),
                                kind: wire_node_lib::pyramid::event_bus::TaggedKind::QueueJobCompleted {
                                    model_id: model_id.clone(),
                                    latency_ms: elapsed_ms,
                                },
                            });

                            // Send result back to the waiting caller.
                            let _ = entry.result_tx.send(result);
                        }
                        None => break, // All queues drained, go back to notify.wait
                    }
                }
            }
        });
    }

    // Start DADBEAR extend loop if any enabled watch configs exist.
    // Deferred via tauri::async_runtime::spawn because Tauri's setup() callback
    // runs before the Tokio runtime is fully available for tokio::spawn.
    {
        let ps = pyramid_state.clone();
        let db_path_str = pyramid_db_path.to_string_lossy().to_string();
        tauri::async_runtime::spawn(async move {
            let reader = ps.reader.lock().await;
            let has_configs = wire_node_lib::pyramid::db::get_enabled_dadbear_configs(&reader)
                .map(|c| !c.is_empty())
                .unwrap_or(false);
            drop(reader);
            if has_configs {
                let bus = ps.build_event_bus.clone();
                let handle = wire_node_lib::pyramid::dadbear_extend::start_dadbear_extend_loop(
                    ps.clone(), db_path_str, bus,
                );
                let mut dh = ps.dadbear_handle.lock().await;
                *dh = Some(handle);
                tracing::info!("DADBEAR extend loop started on app launch (existing configs found)");
            }
        });
    }

    // ── Phase 5: DADBEAR Runtime Supervisor ─────────────────────────────
    //
    // The supervisor runs ALONGSIDE the existing extend loop during the
    // transition period (Phases 5–7). It handles dispatch + result
    // application for work items created by the compiler (Phase 3).
    // MUST be spawned AFTER the GPU processing loop.
    {
        let ps = pyramid_state.clone();
        let cq = compute_queue_handle.clone();
        let db_path_str = pyramid_db_path.to_string_lossy().to_string();
        let bus = pyramid_state.build_event_bus.clone();
        tauri::async_runtime::spawn(async move {
            let handle = wire_node_lib::pyramid::dadbear_supervisor::start_dadbear_supervisor(
                ps.clone(), cq, db_path_str, bus,
            );
            let mut sh = ps.dadbear_supervisor_handle.lock().await;
            *sh = Some(handle);
            tracing::info!("DADBEAR supervisor started (Phase 5)");
        });
    }

    // Phase 11: broadcast leak detection sweep.
    //
    // Runs periodically to flip synchronous cost_log rows whose
    // broadcast confirmation never arrived past the grace period to
    // `reconciliation_status = 'broadcast_missing'`. The loop uses
    // the same per-app cancellation pattern as the DADBEAR extend
    // loop so it drops cleanly on app exit. See
    // `docs/specs/evidence-triage-and-dadbear.md` Part 4.
    {
        let ps = pyramid_state.clone();
        tauri::async_runtime::spawn(async move {
            use wire_node_lib::pyramid::openrouter_webhook::run_leak_sweep;
            use wire_node_lib::pyramid::provider_health::CostReconciliationPolicy;
            // TODO(Phase 12/15): load the interval + grace period
            // from the active `dadbear_policy` contribution via the
            // config registry. Until then we use the spec defaults.
            let policy = CostReconciliationPolicy::default();
            let interval =
                std::time::Duration::from_secs(policy.broadcast_audit_interval_secs as u64);
            // Wait one full interval before the first run so the
            // app has time to ingest any initial cost_log rows.
            tokio::time::sleep(interval).await;
            loop {
                let bus = ps.build_event_bus.clone();
                let result = {
                    let conn = ps.writer.lock().await;
                    run_leak_sweep(&conn, &policy, Some(&bus))
                };
                match result {
                    Ok(n) if n > 0 => tracing::info!(
                        rows_flipped = n,
                        "broadcast leak detection sweep flipped rows"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "broadcast leak detection sweep failed"),
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    // WS-ONLINE-H: spawn payment token redemption sweeper.
    //
    // Expires stale unredeemed payment tokens (past their JWT TTL) and
    // retries pending tokens with exponential backoff. Runs every 30s,
    // matching the server-side token expiry cron cadence.
    {
        let ps = pyramid_state.clone();
        tauri::async_runtime::spawn(async move {
            wire_node_lib::pyramid::payment_redeemer::spawn_redemption_sweeper(ps).await;
        });
        tracing::info!("WS-ONLINE-H payment redemption sweeper spawned");
    }

    // Phase 14: spawn the Wire update poller.
    //
    // Runs as a background tokio task that periodically asks the Wire
    // for supersession updates against every locally-pulled
    // contribution. Writes new entries to `pyramid_wire_update_cache`,
    // emits `WireUpdateAvailable` events, and auto-pulls updates for
    // schema types with auto-update enabled (subject to the credential
    // safety gate).
    //
    // The poller reads its interval from the `wire_update_polling`
    // bundled contribution (default 6 hours) on every iteration, so a
    // supersession of that contribution takes effect without a
    // restart. The task handle is held in-scope for the lifetime of
    // the app — it aborts on app exit.
    {
        let ps = pyramid_state.clone();
        let wire_url = std::env::var("WIRE_URL")
            .unwrap_or_else(|_| "https://newsbleach.com".to_string());
        // We intentionally leak the handle here (forget) — the
        // background task lives for the whole app lifetime, matching
        // the other background workers above (dadbear, leak sweep).
        let handle = wire_node_lib::pyramid::wire_update_poller::spawn_wire_update_poller(
            ps, wire_url,
        );
        std::mem::forget(handle);
        tracing::info!("Phase 14 Wire update poller spawned");
    }

    // WS-E: spawn the web_sessions sweeper (idempotent OnceLock guard).
    wire_node_lib::pyramid::public_html::web_sessions::spawn_sweeper(pyramid_state.clone());

    // Initialize partner (Dennis) state with its own pyramid reader and partner.db
    let partner_db_path = config.data_dir().join("partner.db");
    let partner_db_conn = wire_node_lib::partner::open_partner_db(&partner_db_path)
        .expect("Failed to open partner.db");

    let partner_pyramid_reader = rusqlite::Connection::open(&pyramid_db_path)
        .expect("Failed to open pyramid.db partner reader connection");
    wire_node_lib::pyramid::db::init_pyramid_db(&partner_pyramid_reader)
        .expect("Failed to initialize pyramid schema on partner reader");

    let partner_state = Arc::new(wire_node_lib::partner::PartnerState {
        sessions: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        pyramid: pyramid_state.clone(),
        pyramid_reader: Arc::new(tokio::sync::Mutex::new(partner_pyramid_reader)),
        partner_db: Arc::new(tokio::sync::Mutex::new(partner_db_conn)),
        llm_config: tokio::sync::RwLock::new(wire_node_lib::partner::PartnerLlmConfig {
            api_key: credential_store
                .resolve_var("OPENROUTER_KEY")
                .map(|s| s.raw_clone())
                .unwrap_or_else(|_| pyramid_config.openrouter_api_key.clone()),
            partner_model: pyramid_config.partner_model.clone(),
        }),
        warm_in_progress: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    });

    tracing::info!("Partner (Dennis) initialized at {:?}", partner_db_path);

    // Build pyramid sync state and load pinned pyramids from DB (WS-ONLINE-D)
    let pyramid_sync_state = {
        let mut pss = wire_node_lib::pyramid::sync::PyramidSyncState::new();
        // Hydrate pinned pyramids from DB so auto-refresh works across restarts.
        // Use blocking_lock() since main() is synchronous — safe at startup, no contention.
        let reader = pyramid_state.reader.blocking_lock();
        match wire_node_lib::pyramid::db::list_pinned_pyramids(&reader) {
            Ok(pinned) => {
                for (slug, tunnel_url) in pinned {
                    tracing::info!(slug = %slug, "restoring pinned pyramid for auto-refresh");
                    pss.pin_pyramid(slug, tunnel_url);
                }
            }
            Err(e) => {
                tracing::warn!("failed to load pinned pyramids on startup: {}", e);
            }
        }
        drop(reader);
        Arc::new(tokio::sync::Mutex::new(pss))
    };

    // ── Phase 2 WS7: Construct compute market state + dispatch context ──
    //
    // Read the persisted market state from disk; on missing / malformed /
    // schema-version-mismatch, fall back to a fresh default (cold-start
    // rebuild). Load the market delivery policy from the operational
    // table seeded in WS0; on unavailable, fall back to the Default
    // sentinel (matches the bundled seed YAML per the
    // default_matches_seed_yaml test).
    let compute_market_state_init = wire_node_lib::compute_market::ComputeMarketState::load(
        &config.data_dir(),
    )
    .unwrap_or_default();
    let market_delivery_policy_init = {
        let db_path = config.data_dir().join("pyramid.db");
        match wire_node_lib::pyramid::db::open_pyramid_connection(&db_path) {
            Ok(conn) => {
                wire_node_lib::pyramid::market_delivery_policy::read_market_delivery_policy(&conn)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            }
            Err(e) => {
                tracing::warn!(
                    "market_delivery_policy: failed to open pyramid DB for boot read: {e}; falling back to Default"
                );
                Default::default()
            }
        }
    };
    let compute_market_state_shared =
        Arc::new(RwLock::new(compute_market_state_init));
    // Phase 2 WS6: construct the queue-mirror nudge channel BEFORE the
    // dispatch context so every mutation site has a live sender from
    // boot. The receiver is handed to `spawn_market_mirror_task` below
    // once all the state it needs (compute_queue, auth, tunnel, db_path)
    // is resolvable. Unbounded — mutation sites use `.send(()).ok()` so
    // a shutdown race never panics.
    let (market_mirror_nudge_tx, market_mirror_nudge_rx) =
        tokio::sync::mpsc::unbounded_channel::<()>();
    // Phase 3: second nudge channel for the market delivery worker. Fired
    // whenever a market outbox row transitions into `ready` (worker success,
    // worker failure-now-fixed-to-promote, sweep heartbeat-lost synth). The
    // supervise_delivery_loop task consumes the receiver; sender lives on
    // MarketDispatchContext so every mutation site can send(()).ok() without
    // caring about shutdown races.
    let (market_delivery_nudge_tx, market_delivery_nudge_rx) =
        tokio::sync::mpsc::unbounded_channel::<()>();
    let compute_market_dispatch_shared = Arc::new(
        wire_node_lib::pyramid::market_dispatch::MarketDispatchContext {
            tunnel_state: shared_tunnel.clone(),
            pending: Arc::new(
                wire_node_lib::pyramid::market_dispatch::PendingMarketJobs::new(),
            ),
            policy: Arc::new(RwLock::new(market_delivery_policy_init)),
            mirror_nudge: market_mirror_nudge_tx,
            delivery_nudge: market_delivery_nudge_tx,
        },
    );

    // Walker v3 Phase 0a-2 §2.17.1: construct the AppMode handle BEFORE
    // AppState so every build-starter can reach it via `state.app_mode`.
    // Always starts at `Booting`; flipped to `Ready` by the boot
    // coordinator (see `boot::run_walker_cache_boot` in the Tauri setup
    // block below, canonical §2.17 step 9).
    let app_mode_handle = wire_node_lib::app_mode::new_app_mode();

    let state = Arc::new(AppState {
        auth: shared_auth.clone(),
        sync_state: Arc::new(RwLock::new(
            sync::load_sync_state(&config.data_dir()).unwrap_or_default(),
        )),
        credits: Arc::new(RwLock::new(initial_credits)),
        tunnel_state: shared_tunnel.clone(),
        market_state: Arc::new(RwLock::new(
            market::load_market_state(&config.data_dir()).unwrap_or_default(),
        )),
        work_stats: Arc::new(RwLock::new(work::WorkStats::default())),
        config: shared_config.clone(),
        pyramid: pyramid_state,
        partner: partner_state,
        pyramid_sync_state: pyramid_sync_state,
        compute_queue: compute_queue_handle.clone(),
        fleet_roster: fleet_roster.clone(),
        compute_market_state: compute_market_state_shared.clone(),
        compute_market_dispatch: compute_market_dispatch_shared.clone(),
        node_identity: Some(node_identity.clone()),
        // Phase 3 requester-side: fresh empty pending-jobs map at
        // boot. In-memory only; node restart loses any in-flight
        // dispatches by design. See pyramid::pending_jobs for the
        // semantics rationale.
        pending_market_jobs: shared_pending_market_jobs.clone(),
        // Walker v3 §2.17.1: in-memory AppMode state machine. Starts at
        // `Booting`. The boot coordinator (spawned in `setup()` below)
        // flips to `Ready` only after the scope_cache_reloader is live.
        app_mode: app_mode_handle.clone(),
    });

    // ── Phase 2 WS6: queue mirror push task + market outbox sweep ──
    //
    // Both are fire-and-forget until app exit, mirroring the fleet
    // sweep spawn pattern above. The mirror task consumes the nudge
    // channel receiver constructed earlier (every mutation site has
    // the sender bundled in `MarketDispatchContext.mirror_nudge`).
    // The market sweep is shape-parallel to `fleet_outbox_sweep_loop`
    // but scoped to `callback_kind != 'Fleet'` rows.
    //
    // Ordering: these come up BEFORE warp `start_server` below so the
    // dispatch-handler nudges at that point have a live receiver and
    // the outbox sweep catches any rows recovered from startup.
    {
        let ctx = wire_node_lib::pyramid::market_mirror::MirrorTaskContext {
            market_state: compute_market_state_shared.clone(),
            dispatch: compute_market_dispatch_shared.clone(),
            compute_queue: compute_queue_handle.clone(),
            auth: shared_auth.clone(),
            tunnel: shared_tunnel.clone(),
            api_url: config.api_url.clone(),
            db_path: pyramid_db_path.to_path_buf(),
            node_id_override: None,
        };
        wire_node_lib::pyramid::market_mirror::spawn_market_mirror_task(
            ctx,
            market_mirror_nudge_rx,
        );
    }
    {
        // Phase 3 startup recovery: clear any stale delivery_lease_until
        // stamps left over from a prior process that died mid-POST.
        // Fire-and-forget spawn — even if the delivery task's first tick
        // fires before this lands, stale leases just delay reclaim by
        // the lease_duration (~35s default), not block delivery
        // correctness.
        //
        // Rev 0.6.1 Wave 2B: additionally clear per-leg leases
        // (`content_lease_until`, `settlement_lease_until`) via
        // `market_outbox_startup_recovery_clear_leg_leases`. The rev 0.5
        // helper above only clears the old single `delivery_lease_until`
        // column (dead per spec line 84 but kept for back-compat); rev
        // 0.6 moves to per-leg columns so the new helper is additive.
        // Double-clear is idempotent — the rev 0.5 helper touches a
        // different column and is safe to leave in place.
        let db_path_recover = pyramid_db_path.to_path_buf();
        tauri::async_runtime::spawn(async move {
            let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let conn = rusqlite::Connection::open(&db_path_recover)?;
                // Legacy (rev 0.5) single-lease column — kept for safety
                // in case any row still has it set from a pre-migration
                // instance. No-op on rows where the column is already NULL.
                let n_legacy =
                    wire_node_lib::pyramid::db::market_outbox_delivery_startup_recovery(&conn)?;
                if n_legacy > 0 {
                    tracing::info!(
                        recovered = n_legacy,
                        "market delivery startup: cleared stale legacy (rev 0.5) leases"
                    );
                }
                // Rev 0.6 per-leg leases — the live recovery path. Clears
                // `content_lease_until` + `settlement_lease_until` on any
                // ready MarketStandard/Relay row so the delivery task
                // reclaims immediately after restart.
                let n_legs =
                    wire_node_lib::pyramid::db::market_outbox_startup_recovery_clear_leg_leases(
                        &conn,
                    )?;
                if n_legs > 0 {
                    tracing::info!(
                        recovered = n_legs,
                        "market delivery startup: cleared stale per-leg leases (content + settlement)"
                    );
                }
                Ok(())
            })
            .await;
            if let Err(e) = res {
                tracing::warn!(err = %e, "market delivery startup recovery join error");
            }
        });
    }
    {
        // Phase 3: spawn the supervised delivery loop. Consumes the
        // receiver constructed upstream alongside the mirror_nudge
        // receiver. Supervisor lives for process lifetime; panic → 5s
        // backoff → respawn; clean channel close → loud exit event.
        let delivery_ctx = wire_node_lib::pyramid::market_delivery::DeliveryContext {
            db_path: pyramid_db_path.to_path_buf(),
            policy: compute_market_dispatch_shared.policy.clone(),
            auth: shared_auth.clone(),
        };
        wire_node_lib::pyramid::market_delivery::spawn_market_delivery_task(
            delivery_ctx,
            market_delivery_nudge_rx,
        );
    }
    {
        // Phase 3: ConfigSynced hot-reload for `market_delivery_policy`.
        // Mirror of the fleet_delivery_policy ConfigSynced arm higher up
        // in main.rs, spawned here because `compute_market_dispatch_shared`
        // is constructed AFTER the primary ConfigSynced listener
        // (~line 11905) boots — a separate listener on the same
        // build_event_bus subscriber is the minimal-disruption wiring.
        //
        // Without this, the three new policy fields added in rev 0.5
        // (lease_grace_secs, max_concurrent_deliveries,
        // max_error_message_chars) would require node restart to take
        // effect — invalidating the Pillar 37 tunability claim the spec
        // makes. In-scope per the rev 0.4 audit response (item A).
        let config_market_dispatch = Arc::clone(&compute_market_dispatch_shared);
        let config_bus = state.pyramid.build_event_bus.clone();
        let config_db_path = pyramid_db_path.to_path_buf();
        let mut rx = config_bus.tx.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let wire_node_lib::pyramid::event_bus::TaggedKind::ConfigSynced {
                            schema_type,
                            ..
                        } = event.kind
                        {
                            if schema_type == "market_delivery_policy" {
                                let yaml_opt =
                                    match wire_node_lib::pyramid::db::open_pyramid_connection(&config_db_path) {
                                        Ok(conn) => {
                                            wire_node_lib::pyramid::market_delivery_policy::read_market_delivery_policy(&conn)
                                                .ok()
                                                .flatten()
                                        }
                                        Err(_) => None,
                                    };
                                if let Some(new_policy) = yaml_opt {
                                    *config_market_dispatch.policy.write().await = new_policy;
                                    tracing::info!(
                                        "ConfigSynced: market_delivery_policy reloaded"
                                    );
                                } else {
                                    tracing::warn!(
                                        "ConfigSynced: market_delivery_policy broadcast but no row readable"
                                    );
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            "market_delivery_policy ConfigSynced listener lagged by {n} events"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(
                            "market_delivery_policy ConfigSynced listener: bus closed, exiting"
                        );
                        break;
                    }
                }
            }
        });
    }
    {
        let policy_handle = compute_market_dispatch_shared.policy.clone();
        let db_path_clone = pyramid_db_path.to_path_buf();
        let delivery_nudge_for_sweep = compute_market_dispatch_shared.delivery_nudge.clone();
        tauri::async_runtime::spawn(async move {
            wire_node_lib::pyramid::fleet_outbox_sweep::market_outbox_sweep_loop(
                db_path_clone,
                policy_handle,
                Some(delivery_nudge_for_sweep),
            )
            .await;
        });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // TODO: updater pubkey in tauri.conf.json is empty — needs a real keypair
        // before production release. The pubkey must match the signing key used for releases.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state.clone())
        .setup(move |app| {
            let state = state.clone();

            // ── Walker v3 Phase 0a-2 §2.17 boot coordinator ───────────────
            //
            // Runs the walker-v3-owned portion of the canonical 11-step
            // boot sequence: step 3 (initial ScopeCache), step 4/5
            // (migration phase scaffold + post-migration rebuild — both
            // stubbed until Phase 0b/§5.3), step 6 (spawn
            // scope_cache_reloader), step 7 (ConfigSynced →
            // RebuildTrigger bridge), step 9 (transition AppMode →
            // Ready). Steps 1-2 (DB open + bundled manifest walk via the
            // envelope writer) + 8 (stale_engine rehydrate) + 10-11
            // (HTTP listeners + DADBEAR + chain executor) already ran /
            // are queued above/below this spawn.
            //
            // The coordinator is called via `tauri::async_runtime::spawn`
            // so it yields to the reactor — the Tauri setup() callback
            // itself is synchronous + non-async, which is why we defer.
            // Any build-starter that lands before the coordinator flips
            // AppMode → Ready is refused by `guard_app_ready` with a
            // "node is not Ready" error (fail-fast, not hang).
            //
            // §2.17.3: if the DB probe fails, the coordinator returns
            // `BootResult::Aborted`. We log `boot_aborted` and leave
            // AppMode in `Booting` — build-starters will keep refusing
            // and the operator sees the error in the log.
            {
                let boot_db_path = pyramid_db_path
                    .to_string_lossy()
                    .to_string();
                let boot_app_mode = state.app_mode.clone();
                let boot_event_bus = state.pyramid.build_event_bus.clone();
                // Leak the handles into process-lifetime tasks — main.rs
                // owns the Tauri event loop, and the reloader/relay/bridge
                // all need to outlive setup(). Dropping the handles ends
                // the tasks (JoinHandle drops abort on drop in tokio), so
                // we hold them on a Box::leak'd slot.
                tauri::async_runtime::spawn(async move {
                    match wire_node_lib::boot::run_walker_cache_boot(
                        boot_db_path,
                        boot_app_mode,
                        boot_event_bus,
                    )
                    .await
                    {
                        wire_node_lib::boot::BootResult::Ok(handles) => {
                            // Stash handles so they outlive setup().
                            // Box::leak is deliberate — process-lifetime
                            // ownership, no clean shutdown path yet.
                            let _: &'static mut wire_node_lib::boot::BootHandles =
                                Box::leak(Box::new(handles));
                            tracing::info!(
                                event = "boot_complete",
                                "walker-v3 boot coordinator complete; AppMode=Ready"
                            );
                        }
                        wire_node_lib::boot::BootResult::Aborted(reason) => {
                            tracing::error!(
                                event = "boot_aborted",
                                reason = %reason,
                                "walker-v3 boot coordinator aborted; node will refuse build-starters"
                            );
                        }
                    }
                });
            }

            // --- Phase 13: cross-pyramid event forwarder ---
            //
            // Subscribe once to the shared build event bus and re-emit
            // every event via Tauri's `cross-build-event` channel so
            // the CrossPyramidTimeline frontend can listen once across
            // all slugs.
            {
                let router = state.pyramid.cross_pyramid_router.clone();
                let bus = state.pyramid.build_event_bus.clone();
                let app_handle = app.handle().clone();
                wire_node_lib::pyramid::cross_pyramid_router::CrossPyramidEventRouter::spawn_tauri_forwarder(
                    router,
                    bus,
                    app_handle,
                );
            }

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
                    "quit" => app.exit(0),
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
                                    let raw_val = parts.next()?.to_string();
                                    let val = match urlencoding::decode(&raw_val) {
                                        Ok(decoded) => decoded.into_owned(),
                                        Err(_) => raw_val,
                                    };
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

                                    // Read access token without holding write lock
                                    let supabase_token = {
                                        let auth = s.auth.read().await;
                                        auth.access_token.clone().unwrap_or_default()
                                    };

                                    // Call register_with_session WITHOUT holding any lock
                                    let (nh, nt) = match &s.node_identity {
                                        Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
                                        None => (c.node_name(), String::new()),
                                    };
                                    let registration = match auth::register_with_session(
                                        &c.api_url,
                                        &supabase_token,
                                        &nh,
                                        &nt,
                                    ).await {
                                        Ok(reg) => Some(reg),
                                        Err(e) => {
                                            tracing::error!("Wire registration after deep link failed: {}", e);
                                            None
                                        }
                                    };

                                    // If handle changed due to 409 retry, update node_identity.json
                                    if let Some(ref reg) = registration {
                                        if let Some(ref new_handle) = reg.node_handle {
                                            if let Some(ref ni) = s.node_identity {
                                                let mut updated = ni.clone();
                                                updated.node_handle = new_handle.clone();
                                                let _ = updated.save(&c.data_dir());
                                            }
                                        }
                                    }

                                    let node_id = registration.as_ref().map(|r| r.node_id.clone());
                                    let api_token = registration.as_ref().map(|r| r.api_token.clone());

                                    // Now briefly acquire write lock to update state
                                    {
                                        let mut auth_write = s.auth.write().await;
                                        auth_write.node_id = node_id.clone();
                                        auth_write.api_token = api_token.clone();
                                        // Propagate operator_handle from registration response.
                                        if let Some(ref reg) = registration {
                                            if reg.operator_handle.is_some() {
                                                auth_write.operator_handle = reg.operator_handle.clone();
                                            }
                                        }
                                        let first_started = auth_write.first_started_at.clone()
                                            .or_else(|| Some(chrono::Utc::now().to_rfc3339()));
                                        auth_write.first_started_at = first_started.clone();

                                        save_session(&c, &auth_write);

                                        let mut cr = s.credits.write().await;
                                        cr.init_session();
                                        cr.first_started_at = first_started;
                                    }

                                    // Start tunnel
                                    if let Some(ref nid) = node_id {
                                        if let Some(ref token) = api_token {
                                            let ts = s.tunnel_state.clone();
                                            let data_dir = c.data_dir();
                                            let token = token.clone();
                                            let nid = nid.clone();
                                            let api_url = c.tunnel_api_url.clone();
                                            tauri::async_runtime::spawn(async move {
                                                start_tunnel_flow(ts, data_dir, &api_url, &token, &nid).await;
                                            });
                                        }
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
                let srv_cfg = server_state.config.read().await;
                let server_port = srv_cfg.server_port;
                let cache_dir = srv_cfg.cache_dir();
                drop(srv_cfg);
                // Async fleet dispatch: pull the live overlay off pyramid.config.
                // Set at startup (Init Ordering step 5); `None` only if that wiring
                // was bypassed. ServerState is a plain struct, no mutation needed.
                let fleet_dispatch = server_state
                    .pyramid
                    .config
                    .read()
                    .await
                    .fleet_dispatch
                    .clone();
                // Phase 2 WS7: compute_market_dispatch + compute_market_state
                // are constructed at AppState boot above. Both are Some() —
                // the dispatch handler's admission-gate short-circuit for
                // None is now only reached in test fixtures that construct
                // ServerState manually without going through AppState.
                let compute_market_dispatch = Some(server_state.compute_market_dispatch.clone());
                let compute_market_state = Some(server_state.compute_market_state.clone());
                server::start_server(
                    server_port,
                    cache_dir,
                    server_state.credits.clone(),
                    server_state.auth.clone(),
                    server_state.sync_state.clone(),
                    server_state.tunnel_state.clone(),
                    jwt_pk,
                    nid_shared,
                    server_state.pyramid.clone(),
                    server_state.partner.clone(),
                    server_state.fleet_roster.clone(),
                    server_state.compute_queue.clone(),
                    fleet_dispatch,
                    compute_market_dispatch,
                    compute_market_state,
                    server_state.config.clone(),
                    server_state.work_stats.clone(),
                    server_state.pending_market_jobs.clone(),
                ).await;
            });

            // --- Pyramid Sync Timer (WS-ONLINE-A) ---
            // Ticks every 60s checking for unpublished pyramid builds.
            // If a linked pyramid has a new completed build, auto-publishes to Wire.
            // Uses shared pyramid_sync_state from AppState (also feeds WS-ONLINE-D).
            let sync_pyramid_state = state.pyramid.clone();
            let sync_tunnel_state = state.tunnel_state.clone();
            let pyramid_sync_state_shared = state.pyramid_sync_state.clone();
            tauri::async_runtime::spawn(async move {
                // Wait for startup to complete before starting sync timer
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    // Read current tunnel URL for metadata publication (WS-ONLINE-B).
                    // pyramid_sync_tick still takes Option<String>; convert the
                    // newtype via as_str().to_string() so discovery metadata
                    // publication keeps its existing wire format.
                    let tunnel_url = {
                        let ts = sync_tunnel_state.read().await;
                        ts.tunnel_url.as_ref().map(|u| u.as_str().to_string())
                    };
                    wire_node_lib::pyramid::sync::pyramid_sync_tick(
                        &sync_pyramid_state,
                        &pyramid_sync_state_shared,
                        tunnel_url,
                    )
                    .await;
                }
            });

            // --- Pinned Pyramid Refresh Timer (WS-ONLINE-D) ---
            // Ticks every 5 minutes checking pinned remote pyramids for new builds.
            // If a remote build_id changed, re-pulls the full export into local SQLite.
            let pinned_pyramid_state = state.pyramid.clone();
            let pinned_sync_state = state.pyramid_sync_state.clone();
            let pinned_auth_state = state.auth.clone();
            tauri::async_runtime::spawn(async move {
                // Wait for startup + tunnel establishment before first pinned refresh
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
                interval.tick().await; // consume the immediate first tick
                loop {
                    interval.tick().await;
                    // Read session JWT each tick (it may be refreshed between ticks)
                    let wire_jwt = {
                        let auth = pinned_auth_state.read().await;
                        auth.api_token.clone().unwrap_or_default()
                    };
                    wire_node_lib::pyramid::sync::pinned_pyramid_refresh_tick(
                        &pinned_pyramid_state,
                        &pinned_sync_state,
                        wire_jwt,
                    )
                    .await;
                }
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
                            // Briefly acquire write lock to update tokens
                            let has_api_token = {
                                let mut auth_write = startup_state.auth.write().await;
                                auth_write.access_token = Some(new_access.clone());
                                auth_write.refresh_token = Some(new_refresh);
                                auth_write.api_token.as_ref()
                                    .map(|t| !t.is_empty()).unwrap_or(false)
                            };
                            // Write lock dropped here

                            if !has_api_token {
                                // Register with Wire using refreshed Supabase session token — no lock held
                                let (nh, nt) = match &startup_state.node_identity {
                                    Some(ni) => (ni.node_handle.clone(), ni.node_token.clone()),
                                    None => (startup_config.node_name(), String::new()),
                                };
                                match auth::register_with_session(
                                    &startup_config.api_url,
                                    &new_access,
                                    &nh,
                                    &nt,
                                ).await {
                                    Ok(reg) => {
                                        tracing::info!("Wire node registered on startup: {}", reg.node_id);
                                        // If handle changed due to 409 retry, update node_identity.json
                                        if let Some(ref new_handle) = reg.node_handle {
                                            if let Some(ref ni) = startup_state.node_identity {
                                                let mut updated = ni.clone();
                                                updated.node_handle = new_handle.clone();
                                                let _ = updated.save(&startup_config.data_dir());
                                            }
                                        }
                                        // Briefly acquire write lock to store registration results
                                        {
                                            let mut auth_write = startup_state.auth.write().await;
                                            auth_write.node_id = Some(reg.node_id.clone());
                                            auth_write.api_token = Some(reg.api_token.clone());
                                            // Propagate operator_id from registration response.
                                            // Without this, fleet routing can't verify same-operator
                                            // identity if session.json was lost or this is a fresh start.
                                            if auth_write.operator_id.is_none() {
                                                auth_write.operator_id = Some(reg.operator_id.clone());
                                            }
                                            // Propagate operator_handle from registration response.
                                            if reg.operator_handle.is_some() {
                                                auth_write.operator_handle = reg.operator_handle.clone();
                                            }
                                        }
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

                            {
                                let auth_write = startup_state.auth.read().await;
                                save_session(&startup_config, &auth_write);
                            }
                            tracing::info!("Token refreshed on startup");
                        }
                        Err(e) => {
                            tracing::warn!("Token refresh failed: {}", e);
                        }
                    }
                }

                // Start tunnel and initial sync — use api_token from auth state
                let (node_id, api_token) = {
                    let auth = startup_state.auth.read().await;
                    (auth.node_id.clone(), auth.api_token.clone())
                };
                if let (Some(nid), Some(ref token)) = (&node_id, &api_token) {
                    if !token.is_empty() {
                        let tunnel_state = startup_state.tunnel_state.clone();
                        let data_dir = startup_config.data_dir();
                        let api_url = startup_config.tunnel_api_url.clone();
                        start_tunnel_flow(tunnel_state, data_dir, &api_url, token, nid).await;
                    }
                }

                // Initial sync
                if let Some(ref token) = api_token {
                    if !token.is_empty() {
                        match do_sync(&startup_config, token, &startup_state.sync_state, &startup_state.credits).await {
                            Ok(_) => tracing::info!("Initial sync complete"),
                            Err(e) => tracing::warn!("Initial sync failed: {}", e),
                        }
                    }
                }
            });

            // --- Auto-sync loop (checks auto_sync_enabled + interval from settings) ---
            let sync_loop_state = state.clone();
            let sync_loop_config = config.clone();
            tauri::async_runtime::spawn(async move {
                // Initial delay before first auto-sync check
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                loop {
                    let (auto_enabled, interval_secs) = {
                        let ss = sync_loop_state.sync_state.read().await;
                        (ss.auto_sync_enabled, ss.auto_sync_interval_secs)
                    };

                    if auto_enabled {
                        let token = {
                            let auth = sync_loop_state.auth.read().await;
                            auth.api_token.clone()
                        };
                        if let Some(ref token) = token {
                            if !token.is_empty() {
                                let is_already_syncing = {
                                    let ss = sync_loop_state.sync_state.read().await;
                                    ss.is_syncing
                                };
                                if !is_already_syncing {
                                    tracing::info!("Auto-sync starting...");
                                    let _ = do_sync(&sync_loop_config, token, &sync_loop_state.sync_state, &sync_loop_state.credits).await;
                                }
                            }
                        }
                    }

                    let sleep_secs = if auto_enabled { interval_secs.max(60) } else { 30 };
                    tokio::time::sleep(tokio::time::Duration::from_secs(sleep_secs)).await;
                }
            });

            // --- Heartbeat loop (every 60s) with market/retention handling ---
            let heartbeat_state = state.clone();
            let heartbeat_config = config.clone();
            let heartbeat_app = app.handle().clone();
            let heartbeat_jwt_pk = jwt_public_key.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

                    // Read api_token from auth state each iteration
                    let (api_token, node_id) = {
                        let auth = heartbeat_state.auth.read().await;
                        (auth.api_token.clone(), auth.node_id.clone())
                    };
                    let api_token = match api_token {
                        Some(ref t) if !t.is_empty() => t.clone(),
                        _ => continue,
                    };

                    if let Some(node_id) = &node_id {
                        let token = &api_token;
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
                            // TunnelUrl: !Deref. Borrow the inner &str for
                            // heartbeat's Option<&str> param; same shape as
                            // the old as_deref() call produced.
                            tunnel_url.as_ref().map(|u| u.as_str()),
                            version.as_deref(),
                        ).await;

                        match result {
                            Ok(response) => {
                                // Handle retention challenges from heartbeat
                                if let Some(challenges) = response.get("retention_challenges") {
                                    if let Ok(challenges) = serde_json::from_value::<Vec<retention::RetentionChallenge>>(challenges.clone()) {
                                        if !challenges.is_empty() {
                                            if let Ok(passed) = retention::handle_retention_challenges(
                                                &heartbeat_config.api_url,
                                                token,
                                                node_id,
                                                &challenges,
                                                &heartbeat_config.cache_dir(),
                                            ).await {
                                                if passed > 0 {
                                                    let mut cr = heartbeat_state.credits.write().await;
                                                    cr.retention_challenges_passed += passed as u64;
                                                }
                                            }
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

                                // Update credit balance from server
                                if let Some(balance) = response.get("credit_balance").and_then(|v| v.as_f64()) {
                                    let mut cr = heartbeat_state.credits.write().await;
                                    cr.server_credit_balance = balance;
                                }

                                // Handle market surface from heartbeat (server sends "storage_market")
                                let market_value = response.get("storage_market")
                                    .or_else(|| response.get("market_surface"));
                                if let Some(market_surface) = market_value {
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
                                            market::save_market_state(&heartbeat_config.data_dir(), &ms);
                                        }
                                    }
                                }

                                // ── Extract operator_handle from heartbeat ──────
                                // The heartbeat response may include operator_handle
                                // so the node always has the latest even if claimed
                                // between heartbeats.
                                if let Some(oh) = response.get("operator_handle").and_then(|v| v.as_str()) {
                                    let mut auth = heartbeat_state.auth.write().await;
                                    auth.operator_handle = Some(oh.to_string());
                                }

                                // ── JWT public key from heartbeat (self-healing) ────
                                // Ensures nodes pick up the key without re-registration.
                                if let Some(pk) = response.get("jwt_public_key").and_then(|v| v.as_str()) {
                                    if !pk.is_empty() {
                                        let mut shared_pk = heartbeat_jwt_pk.write().await;
                                        if shared_pk.is_empty() || *shared_pk != pk {
                                            *shared_pk = pk.to_string();
                                            // Also persist to AuthState → session.json
                                            let mut auth = heartbeat_state.auth.write().await;
                                            auth.jwt_public_key = Some(pk.to_string());
                                            save_session(&heartbeat_config, &auth);
                                            tracing::info!("JWT public key updated from heartbeat");
                                        }
                                    }
                                }

                                // ── Wire parameters from heartbeat (Phase 3) ───────
                                // Wire ships a keyed `wire_parameters` map on every
                                // heartbeat response, projected from an allow-list
                                // economic_parameter contribution on its side.
                                // Subsystems (delivery worker, future: market-surface
                                // filter, match policy) read AuthState.wire_parameters
                                // and fall back to contract-default constants on
                                // missing keys. Zero-lockstep with Wire's upgrade:
                                // nodes running pre-Wire-upgrade just see the field
                                // absent and use defaults.
                                //
                                // Emit market_wire_parameters_updated on any diff so
                                // operators see Wire supersessions land. Cold-boot
                                // population also emits (old = empty, new = populated).
                                if let Some(wp_obj) = response.get("wire_parameters").and_then(|v| v.as_object()) {
                                    let new_map: std::collections::HashMap<String, serde_json::Value> =
                                        wp_obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                    let mut auth = heartbeat_state.auth.write().await;
                                    if auth.wire_parameters != new_map {
                                        let diff: Vec<(String, serde_json::Value, serde_json::Value)> =
                                            new_map.iter()
                                                .filter(|(k, v)| auth.wire_parameters.get(k.as_str()) != Some(v))
                                                .map(|(k, v)| (
                                                    k.clone(),
                                                    auth.wire_parameters.get(k.as_str()).cloned().unwrap_or(serde_json::Value::Null),
                                                    v.clone(),
                                                ))
                                                .collect();
                                        auth.wire_parameters = new_map;
                                        save_session(&heartbeat_config, &auth);
                                        drop(auth);
                                        // Fire-and-forget chronicle emit. Non-fatal on
                                        // failure; observability aid only.
                                        let db_path_chr = heartbeat_config.data_dir().join("pyramid.db");
                                        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                                            let conn = rusqlite::Connection::open(&db_path_chr)?;
                                            let ctx_ev = wire_node_lib::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                                                &format!("market/wire_parameters/{}", chrono::Utc::now().timestamp()),
                                                wire_node_lib::pyramid::compute_chronicle::EVENT_MARKET_WIRE_PARAMETERS_UPDATED,
                                                wire_node_lib::pyramid::compute_chronicle::SOURCE_MARKET,
                                            ).with_metadata(serde_json::json!({
                                                "changed_keys": diff.iter().map(|(k, _, _)| k.clone()).collect::<Vec<_>>(),
                                                "diff": diff.iter().map(|(k, o, n)| serde_json::json!({
                                                    "key": k, "old": o, "new": n,
                                                })).collect::<Vec<_>>(),
                                            }));
                                            let _ = wire_node_lib::pyramid::compute_chronicle::record_event(&conn, &ctx_ev);
                                            Ok(())
                                        });
                                        tracing::info!("wire_parameters updated from heartbeat");
                                    }
                                }

                                // ── Fleet roster from heartbeat ──────────────────
                                // Extract fleet_roster array and fleet_jwt from
                                // the heartbeat response. Update the shared roster.
                                if let Some(fleet_array) = response.get("fleet_roster").and_then(|v| v.as_array()) {
                                    let entries: Vec<wire_node_lib::fleet::HeartbeatFleetEntry> = fleet_array
                                        .iter()
                                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                        .collect();
                                    let jwt = response
                                        .get("fleet_jwt")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());

                                    {
                                        let mut roster = heartbeat_state.fleet_roster.write().await;
                                        roster.update_from_heartbeat(entries, jwt);

                                        // Set operator_id on roster if not yet set.
                                        if roster.self_operator_id.is_none() {
                                            let auth = heartbeat_state.auth.read().await;
                                            roster.self_operator_id = auth.operator_id.clone();
                                        }
                                    }

                                    // Announce to fleet peers on every heartbeat
                                    // that has peers. Peers need fresh model lists
                                    // and queue depths — only announcements carry
                                    // these; the heartbeat roster is tunnel/name only.
                                    {
                                        let roster = heartbeat_state.fleet_roster.read().await;
                                        if !roster.peers.is_empty() {
                                            let auth = heartbeat_state.auth.read().await;
                                            let self_node_id = auth.node_id.clone().unwrap_or_default();
                                            let self_operator_id = auth.operator_id.clone().unwrap_or_default();
                                            let self_operator_handle = auth.operator_handle.clone();
                                            drop(auth);

                                            let self_node_handle = heartbeat_state.node_identity
                                                .as_ref()
                                                .map(|ni| ni.node_handle.clone());

                                            // FleetAnnouncement.tunnel_url is TunnelUrl (WS9). Without a
                                            // tunnel URL we have nothing meaningful to announce.
                                            let tunnel_url_opt = {
                                                let ts = heartbeat_state.tunnel_state.read().await;
                                                ts.tunnel_url.clone()
                                            };
                                            let tunnel_url = match tunnel_url_opt {
                                                Some(u) => u,
                                                None => {
                                                    tracing::debug!("Heartbeat: skipping fleet announce — no tunnel URL");
                                                    continue;
                                                }
                                            };

                                            // Read loaded models from local mode state (Ollama).
                                            // The DB stores the active model; for fleet routing,
                                            // this is the model this node can serve.
                                            let models_loaded = {
                                                let reader = heartbeat_state.pyramid.reader.lock().await;
                                                match wire_node_lib::pyramid::db::load_local_mode_state(&reader) {
                                                    Ok(row) if row.enabled => {
                                                        row.ollama_model.into_iter().collect::<Vec<_>>()
                                                    }
                                                    _ => Vec::new(),
                                                }
                                            };

                                            // Read real queue depths from compute queue.
                                            let queue_depths = {
                                                let q = heartbeat_state.compute_queue.queue.lock().await;
                                                q.all_depths()
                                            };

                                            // Derive serving_rules from dispatch policy + loaded models
                                            let serving_rules = {
                                                let cfg = heartbeat_state.pyramid.config.read().await;
                                                if let Some(ref policy) = cfg.dispatch_policy {
                                                    wire_node_lib::fleet::derive_serving_rules(policy, &models_loaded)
                                                } else {
                                                    vec![]
                                                }
                                            };
                                            let total_queue_depth = {
                                                let q = heartbeat_state.compute_queue.queue.lock().await;
                                                q.total_depth()
                                            };
                                            let announcement = wire_node_lib::fleet::FleetAnnouncement {
                                                node_id: self_node_id,
                                                name: None,
                                                node_handle: self_node_handle,
                                                operator_handle: self_operator_handle,
                                                tunnel_url,
                                                models_loaded,
                                                serving_rules,
                                                queue_depths,
                                                total_queue_depth,
                                                operator_id: self_operator_id,
                                            };
                                            wire_node_lib::fleet::announce_to_fleet(&roster, &announcement).await;
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
                    let (pending_serves, node_id, api_token) = {
                        let auth = credit_state.auth.read().await;
                        let node_id = auth.node_id.clone();
                        let api_token = auth.api_token.clone();
                        drop(auth);
                        let mut cr = credit_state.credits.write().await;
                        let pending = cr.take_pending_serves();
                        (pending, node_id, api_token)
                    };

                    if let (Some(ref nid), Some(ref token)) = (&node_id, &api_token) {
                        if !token.is_empty() && !pending_serves.is_empty() {
                            match credits::report_serves(&credit_config.api_url, token, nid, &pending_serves).await {
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
                    // Sync achievement counters from WorkStats and MarketState
                    {
                        let ws = stats_save_state.work_stats.read().await;
                        let ms = stats_save_state.market_state.read().await;
                        let mut cr = stats_save_state.credits.write().await;
                        cr.tick_uptime();
                        cr.total_jobs_completed = ws.total_jobs_completed;
                        cr.documents_hosted = ms.hosted_documents.len() as u64;
                        cr.bytes_hosted = ms.total_hosted_bytes;
                        // Count unique corpora from hosted documents
                        let corpora: std::collections::HashSet<&str> = ms.hosted_documents.values()
                            .map(|d| d.corpus_id.as_str())
                            .collect();
                        cr.unique_corpora_hosted = corpora.len() as u64;
                        if tick_count % 5 == 0 {
                            let path = stats_save_config.data_dir().join("stats.json");
                            cr.save_to_file(&path);
                        }
                    }
                }
            });

            // --- Work polling loop (every 5s, with exponential backoff) ---
            let work_state = state.clone();
            let work_config = config.clone();
            tauri::async_runtime::spawn(async move {
                // Wait for auth to be ready
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                let initial_interval = 5_000u64; // 5 seconds
                let max_interval = 30_000u64; // 30 seconds
                let mut consecutive_errors: u32 = 0;

                loop {
                    // Get auth credentials
                    let (api_token, node_id) = {
                        let auth = work_state.auth.read().await;
                        (auth.api_token.clone(), auth.node_id.clone())
                    };

                    let token = match api_token.as_deref() {
                        Some(t) if !t.is_empty() => t.to_string(),
                        _ => {
                            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                            continue;
                        }
                    };

                    let nid = match node_id.as_deref() {
                        Some(n) if !n.is_empty() => n.to_string(),
                        _ => {
                            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                            continue;
                        }
                    };

                    // Update polling status
                    {
                        let mut ws = work_state.work_stats.write().await;
                        ws.is_polling = true;
                    }

                    // Poll for work
                    match work::poll_work(&work_config.api_url, &token, &nid).await {
                        Ok(Some(work_item)) => {
                            consecutive_errors = 0;
                            let work_type = work_item.work_type.clone();
                            let work_id = work_item.id.clone();
                            tracing::info!("Work received: {} ({}...)", work_type, &work_id[..8.min(work_id.len())]);

                            // Execute the work
                            let result = work::execute_work(&work_item).await;

                            // Submit the result
                            match work::submit_result(&work_config.api_url, &token, &work_id, &result.data).await {
                                Ok(submission) => {
                                    let credits = submission.credits_awarded;
                                    tracing::info!("Work completed: {} +{:.0} credits", work_type, credits);

                                    // Update work stats
                                    {
                                        let mut ws = work_state.work_stats.write().await;
                                        ws.total_jobs_completed += 1;
                                        ws.total_credits_earned += credits;
                                        ws.session_jobs_completed += 1;
                                        ws.session_credits_earned += credits;
                                        ws.consecutive_errors = 0;
                                        ws.last_work_at = Some(chrono::Utc::now().to_rfc3339());
                                    }

                                    // Record in activity feed
                                    {
                                        let mut cr = work_state.credits.write().await;
                                        cr.record_work_event(&work_type, &work_id, credits);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Work submit failed: {}", e);
                                }
                            }

                            // Don't sleep after work — immediately poll for more
                            continue;
                        }
                        Ok(None) => {
                            // No work available — wait before polling again
                            consecutive_errors = 0;
                            tokio::time::sleep(tokio::time::Duration::from_millis(initial_interval)).await;
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            let backoff = std::cmp::min(
                                initial_interval * 2u64.pow(consecutive_errors),
                                max_interval,
                            );

                            // Update error count in stats
                            {
                                let mut ws = work_state.work_stats.write().await;
                                ws.consecutive_errors = consecutive_errors;
                            }

                            if consecutive_errors == 1 {
                                tracing::warn!("Work poll error: {}", e);
                            } else if consecutive_errors % 10 == 0 {
                                tracing::warn!("Work poll errors: {} consecutive (backing off to {}s)", consecutive_errors, backoff / 1000);
                            }

                            tokio::time::sleep(tokio::time::Duration::from_millis(backoff)).await;
                        }
                    }
                }
            });

            // --- Initialize stale engines for auto-update (Phase 7) ---
            let stale_init_state = state.clone();
            tauri::async_runtime::spawn(async move {
                // Small delay to let DB initialize
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                server::init_stale_engines(&stale_init_state.pyramid).await;
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
            verify_otp,
            login,
            get_auth_state,
            get_wire_identity_status,
            logout,
            get_config,
            set_config,
            link_folder,
            unlink_folder,
            get_sync_status,
            list_my_corpora,
            list_public_corpora,
            create_corpus,
            sync_content,
            set_auto_sync,
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
            get_node_name,
            save_onboarding,
            get_logs,
            open_file,
            fetch_document_versions,
            compute_diff,
            pin_version,
            update_document_status,
            bulk_publish,
            get_work_stats,
            get_operator_session,
            operator_api_call,
            wire_api_call,
            get_home_dir,
            pyramid_list_slugs,
            pyramid_get_publication_status,
            pyramid_apex,
            pyramid_node,
            pyramid_tree,
            pyramid_drill,
            pyramid_reevaluate_deferred_questions,
            pyramid_list_question_overlays,
            pyramid_search,
            pyramid_get_references,
            pyramid_get_composed_view,
            pyramid_build,
            pyramid_build_status,
            pyramid_build_progress_v2,
            pyramid_build_cancel,
            pyramid_build_force_reset,
            pyramid_vine_build,
            pyramid_vine_build_status,
            pyramid_vine_bunches,
            pyramid_vine_eras,
            pyramid_vine_decisions,
            pyramid_vine_entities,
            pyramid_vine_threads,
            pyramid_vine_drill,
            pyramid_vine_corrections,
            pyramid_vine_integrity,
            pyramid_vine_rebuild_upper,
            pyramid_ingest,
            pyramid_set_config,
            pyramid_create_slug,
            pyramid_ingest_folder,
            pyramid_find_claude_code_conversations,
            pyramid_delete_slug,
            pyramid_get_config,
            pyramid_get_auth_token,
            pyramid_list_profiles,
            pyramid_apply_profile,
            pyramid_generate_ascii_banner,
            pyramid_open_web_as_owner,
            open_url_in_browser,
            pyramid_get_public_url,
            pyramid_get_cached_banner,
            pyramid_test_api_key,
            test_remote_connection,
            get_app_version,
            pyramid_auto_update_config_get,
            pyramid_auto_update_config_set,
            pyramid_auto_update_freeze,
            pyramid_auto_update_unfreeze,
            pyramid_auto_update_status,
            pyramid_stale_log,
            pyramid_cost_summary,
            pyramid_evidence_density,
            pyramid_build_live_nodes,
            pyramid_node_audit,
            pyramid_audit_by_id,
            pyramid_audit_cleanup,
            pyramid_breaker_resume,
            pyramid_freeze_all,
            pyramid_unfreeze_all,
            pyramid_count_freeze_scope,
            pyramid_dadbear_configs_for_slug,
            pyramid_auto_update_run_now,
            pyramid_auto_update_l0_sweep,
            pyramid_breaker_archive_and_rebuild,
            pyramid_annotations_recent,
            pyramid_faq_directory,
            pyramid_faq_category_drill,
            // S1: IPC-only mutation commands (moved from HTTP)
            auth_complete_ipc,
            pyramid_purge_slug,
            pyramid_archive_slug,
            pyramid_set_access_tier,
            pyramid_get_access_tier,
            pyramid_set_absorption_mode,
            pyramid_get_absorption_config,
            pyramid_question_build,
            pyramid_rebuild,
            pyramid_question_preview,
            pyramid_characterize,
            pyramid_parity_run,
            pyramid_meta_run,
            pyramid_crystallize,
            pyramid_publish,
            pyramid_publish_question_set,
            pyramid_check_staleness,
            pyramid_chain_import,
            // Phase 3a: chain introspection
            pyramid_get_build_chain,
            // WS-ONLINE-D: Remote pyramid commands
            pyramid_remote_query,
            pyramid_pin_remote,
            pyramid_unpin,
            partner_send_message,
            partner_session_new,
            // Phase 4d: Wire handle cache persistence
            cache_wire_handles,
            get_cached_wire_handles,
            // Phase 5c: Compose drafts
            save_compose_draft,
            get_compose_drafts,
            delete_compose_draft,
            // Sprint 1.5b: Single-stage intent planner (full vocabulary)
            planner_call,
            get_vocabulary_registry,
            // Phase 3: Credentials & Provider Registry
            pyramid_list_credentials,
            pyramid_set_credential,
            pyramid_delete_credential,
            pyramid_credentials_file_status,
            pyramid_fix_credentials_permissions,
            pyramid_credential_references,
            pyramid_list_providers,
            pyramid_save_provider,
            pyramid_delete_provider,
            pyramid_test_provider,
            pyramid_get_tier_routing,
            pyramid_save_tier_routing,
            pyramid_delete_tier_routing,
            pyramid_get_step_overrides,
            pyramid_save_step_override,
            pyramid_delete_step_override,
            // Phase 18a: Local Mode toggle (L1 + L5 + L2)
            pyramid_get_local_mode_status,
            pyramid_enable_local_mode,
            pyramid_disable_local_mode,
            pyramid_probe_ollama,
            // Phase 2 daemon control plane: rich model details
            pyramid_get_model_details,
            // Phase 1 daemon control plane: hot-swap model
            pyramid_switch_local_model,
            // Phase 4 daemon control plane: model pull + delete
            pyramid_ollama_pull_model,
            pyramid_ollama_cancel_pull,
            pyramid_ollama_delete_model,
            // Phase 3 daemon control plane: context + concurrency overrides
            pyramid_set_context_override,
            pyramid_set_concurrency_override,
            // Phase 6 daemon control plane: experimental territory markers
            pyramid_get_experimental_territory,
            pyramid_set_experimental_territory,
            // Fleet MPS WS1: compute participation policy
            pyramid_get_compute_participation_policy,
            pyramid_set_compute_participation_policy,
            // Phase 2 WS7: compute market IPCs.
            compute_offer_create,
            compute_offer_update,
            compute_offer_remove,
            compute_offers_list,
            compute_market_surface,
            compute_market_enable,
            compute_market_disable,
            compute_market_get_state,
            // Pyramid visualization config
            pyramid_get_viz_config,
            pyramid_set_viz_config,
            // Phase 3b: Visual encoding data
            pyramid_get_visual_encoding_data,
            pyramid_preview_pull_contribution,
            // Phase 11: Broadcast webhook + provider health oversight
            pyramid_provider_health,
            pyramid_acknowledge_provider_health,
            pyramid_list_orphan_broadcasts,
            // Phase 4: Config Contribution Foundation
            pyramid_create_config_contribution,
            pyramid_supersede_config,
            pyramid_active_config_contribution,
            pyramid_market_models,
            pyramid_config_version_history,
            pyramid_get_config_history,
            pyramid_propose_config,
            pyramid_pending_proposals,
            pyramid_accept_proposal,
            pyramid_reject_proposal,
            pyramid_rollback_config,
            // Phase 5: Wire Contribution Publication
            pyramid_dry_run_publish,
            pyramid_publish_to_wire,
            // Phase 14: Wire Discovery + Ranking + Update Polling
            pyramid_wire_discover,
            pyramid_search_wire_configs,
            pyramid_wire_recommendations,
            pyramid_wire_update_available,
            pyramid_wire_auto_update_toggle,
            pyramid_wire_auto_update_status,
            pyramid_wire_pull_latest,
            pyramid_pull_wire_config,
            pyramid_wire_acknowledge_update,
            // Phase 8: YAML-to-UI renderer
            pyramid_get_schema_annotation,
            yaml_renderer_resolve_options,
            yaml_renderer_estimate_cost,
            // Phase 9: Generative config pattern
            pyramid_generate_config,
            pyramid_refine_config,
            pyramid_accept_config,
            pyramid_active_config,
            pyramid_config_versions,
            pyramid_config_schemas,
            // Phase 18d: Schema migration UI (claims L6 from deferral-ledger.md)
            pyramid_list_configs_needing_migration,
            pyramid_propose_config_migration,
            pyramid_accept_config_migration,
            pyramid_reject_config_migration,
            // Phase 7: Cache Warming on Pyramid Import
            pyramid_import_pyramid,
            pyramid_import_progress,
            pyramid_import_cancel,
            // Phase 13: Build Viz Expansion + Reroll + Cross-Pyramid
            pyramid_step_cache_for_build,
            pyramid_reroll_node,
            pyramid_active_builds,
            pyramid_cost_rollup,
            pyramid_pause_dadbear_all,
            pyramid_resume_dadbear_all,
            // Phase 18c (L9): scoped pause/resume helper IPCs
            pyramid_list_dadbear_source_paths,
            pyramid_count_dadbear_scope,
            // Phase 15: DADBEAR Oversight Page (v1 overview + activity_log removed in Phase 7)
            pyramid_dadbear_pause,
            pyramid_dadbear_resume,
            // Phase 6 (Canonical): work-item-centric oversight v2
            pyramid_dadbear_overview_v2,
            pyramid_dadbear_activity_v2,
            pyramid_acknowledge_orphan_broadcast,
            // Phase 6: Multi-Window + Nesting
            pyramid_open_window,
            pyramid_close_window,
            pyramid_get_window_context,
            // S2-5: Chronicle Post-Build Review
            pyramid_latest_build_id,
            pyramid_get_build_chronicle,
            // Fleet roster IPC
            get_fleet_roster,
            // Compute Chronicle IPC
            get_compute_events,
            get_compute_summary,
            get_compute_timeline,
            get_chronicle_dimensions,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wire Node");
}
