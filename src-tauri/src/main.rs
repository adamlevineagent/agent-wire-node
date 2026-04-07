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
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &config.node_name()).await?;

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
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &config.node_name()).await?;

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
    let registration =
        auth::register_with_session(&config.api_url, &supabase_token, &config.node_name()).await?;

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
async fn send_api_request(
    api_url: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
    extra_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<(reqwest::StatusCode, serde_json::Value), String> {
    let client = reqwest::Client::new();
    let url = format!("{}{}", api_url, path);
    let mut req = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PATCH" => client.patch(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => return Err("Invalid method".to_string()),
    };
    req = req.header("Authorization", format!("Bearer {}", token));
    if let Some(headers) = extra_headers {
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
    }
    if let Some(b) = body {
        req = req.json(b);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();

    // Check status BEFORE attempting JSON parse
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        // Try to parse as JSON for structured errors, fall back to text
        let error_value = serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|_| serde_json::json!({ "error": text, "status": status.as_u16() }));
        return Err(format!("API error {}: {}", status.as_u16(), error_value));
    }

    let result: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok((status, result))
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

    // Try registration with current access token
    if let Some(ref at) = access_token {
        match auth::register_with_session(&api_url, at, &node_name).await {
            Ok(reg) => {
                let mut auth = state.auth.write().await;
                auth.api_token = Some(reg.api_token.clone());
                auth.node_id = Some(reg.node_id.clone());
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
                match auth::register_with_session(&api_url, &new_access, &node_name).await {
                    Ok(reg) => {
                        let mut auth = state.auth.write().await;
                        auth.api_token = Some(reg.api_token.clone());
                        auth.node_id = Some(reg.node_id.clone());
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
            tunnel_url.as_deref(),
            last_sync.as_deref(),
        )
        .await;
        Some(messaging::collect_diagnostics(
            &health,
            env!("CARGO_PKG_VERSION"),
            tunnel_url.as_deref(),
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
        tunnel_url.as_deref(),
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
) -> Result<DrillResult, String> {
    let conn = state.pyramid.reader.lock().await;
    pyramid_query::drill(&conn, &slug, &node_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Node not found".to_string())
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

    tracing::info!("Post-build seeding complete for slug='{}'", slug);

    // Skip engine + watcher for conversations and vines (no file watching)
    if matches!(content_type, ContentType::Conversation | ContentType::Vine) {
        return Ok(());
    }

    // Start stale engine + file watcher
    let (api_key, model) = {
        let cfg = pyramid_state.config.read().await;
        (cfg.api_key.clone(), cfg.primary_model.clone())
    };

    let config = {
        let conn = pyramid_state.reader.lock().await;
        conn.query_row(
            "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                    runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
             FROM pyramid_auto_update_config WHERE slug = ?1",
            rusqlite::params![slug],
            |row| {
                Ok(wire_node_lib::pyramid::types::AutoUpdateConfig {
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
            },
        )
        .map_err(|e| e.to_string())?
    };

    let mut engine = wire_node_lib::pyramid::stale_engine::PyramidStaleEngine::new(
        slug,
        config,
        &db_path,
        &api_key,
        &model,
        pyramid_state.operational.as_ref().clone(),
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

#[tauri::command]
async fn pyramid_build(
    state: tauri::State<'_, SharedState>,
    slug: String,
    from_depth: Option<i64>,
    stop_after: Option<String>,
    force_from: Option<String>,
) -> Result<BuildStatus, String> {
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
    // Update in-memory LLM config
    {
        let mut config = state.pyramid.config.write().await;
        if let Some(ref key) = api_key {
            config.api_key = key.clone();
        }
        if let Some(ref token) = auth_token {
            config.auth_token = token.clone();
        }
        if let Some(ref model) = primary_model {
            config.primary_model = model.clone();
        }
        if let Some(ref model) = fallback_model_1 {
            config.fallback_model_1 = model.clone();
        }
        if let Some(ref model) = fallback_model_2 {
            config.fallback_model_2 = model.clone();
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

    // Persist to disk
    if let Some(ref data_dir) = state.pyramid.data_dir {
        let mut pyramid_config = wire_node_lib::pyramid::PyramidConfig::load(data_dir);
        let config = state.pyramid.config.read().await;
        pyramid_config.openrouter_api_key = config.api_key.clone();
        pyramid_config.auth_token = config.auth_token.clone();
        pyramid_config.primary_model = config.primary_model.clone();
        pyramid_config.fallback_model_1 = config.fallback_model_1.clone();
        pyramid_config.fallback_model_2 = config.fallback_model_2.clone();
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
    let conn = state.pyramid.writer.lock().await;
    let info =
        wire_node_lib::pyramid::slug::create_slug(&conn, &slug, &ct, &normalized_source_path)
            .map_err(|e| e.to_string())?;

    // Save cross-references if provided (question pyramids referencing base slugs)
    if let Some(refs) = &referenced_slugs {
        if !refs.is_empty() {
            use wire_node_lib::pyramid::db as pyramid_db;
            if let Err(e) = pyramid_db::save_slug_references(&conn, &info.slug, refs) {
                tracing::warn!(slug = %info.slug, error = %e, "failed to save slug references");
            }
        }
    }

    Ok(info)
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
        wire_node_lib::pyramid::slug::archive_slug(&conn, &slug).map_err(|e| e.to_string());
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
        wire_node_lib::pyramid::slug::archive_slug(&conn, &slug).map_err(|e| e.to_string());
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
    wire_node_lib::pyramid::question_build::spawn_question_build(
        &state.pyramid,
        slug,
        question,
        granularity,
        max_depth,
        from_depth,
        characterization,
    )
    .await
}

/// Rebuild a pyramid using the question from its last build.
/// This is the sole rebuild path — all pyramids are question pyramids.
#[tauri::command]
async fn pyramid_rebuild(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Look up the question from the last build record
    let (question, _build_id) = {
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
            Ok(r) => r,
            Err(_) => {
                return Err(format!(
                    "No previous question build found for '{}'. Use the question build flow instead.",
                    slug
                ));
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

    let llm_config = state.pyramid.config.read().await.clone();

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

    // Get LLM config
    let (api_key, model) = {
        let config = state.pyramid.config.read().await;
        (config.api_key.clone(), config.primary_model.clone())
    };

    let reader = state.pyramid.reader.clone();
    let writer = state.pyramid.writer.clone();

    match wire_node_lib::pyramid::meta::run_all_meta_passes(
        &reader, &writer, &slug, &api_key, &model,
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
    let base = match tunnel_url {
        Some(u) if !u.is_empty() => u,
        _ => {
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

    let url = if return_slug.is_empty() {
        format!("{}/p/_owner_login?token={}", base.trim_end_matches('/'), token)
    } else {
        format!(
            "{}/p/_owner_login?token={}&return={}",
            base.trim_end_matches('/'),
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
    let base = tunnel_url
        .filter(|u| !u.is_empty())
        .ok_or_else(|| "Tunnel is not running. Click 'Retry Tunnel' in the header.".to_string())?;
    let path = match slug {
        Some(s) if !s.is_empty() => format!("/p/{}", s),
        _ => "/p/".to_string(),
    };
    Ok(format!("{}{}", base.trim_end_matches('/'), path))
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

    Ok(serde_json::json!({
        "api_key_set": !config.api_key.is_empty(),
        "auth_token_set": !config.auth_token.is_empty(),
        "primary_model": config.primary_model,
        "fallback_model_1": config.fallback_model_1,
        "fallback_model_2": config.fallback_model_2,
        "auto_execute": auto_execute,
    }))
}

/// Test an OpenRouter API key server-side so the key never touches the renderer.
#[tauri::command]
async fn pyramid_test_api_key(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let api_key = {
        let config = state.pyramid.config.read().await;
        config.api_key.clone()
    };
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
        let result = vine::build_vine(&pyramid_state, &slug_for_task, &dirs, &cancel).await;
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
        let conn = state.pyramid.writer.lock().await;
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

        let slug_idx = params.len() + 1;
        params.push(Box::new(slug.clone()));
        let sql = format!(
            "UPDATE pyramid_auto_update_config SET {} WHERE slug = ?{}",
            sets.join(", "),
            slug_idx
        );

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let result = match conn.execute(&sql, param_refs.as_slice()) {
            Ok(0) => Err(format!("No auto-update config for slug '{}'", slug)),
            Ok(_) => match pyramid_db::get_auto_update_config(&conn, &slug) {
                Some(config) => {
                    if config.breaker_tripped
                        && !wire_node_lib::pyramid::watcher::check_runaway(&conn, &slug, &config)
                    {
                        let _ = conn.execute(
                            "UPDATE pyramid_auto_update_config
                                 SET breaker_tripped = 0, breaker_tripped_at = NULL
                                 WHERE slug = ?1",
                            rusqlite::params![slug],
                        );
                        should_resume_breaker = true;
                    }

                    let refreshed =
                        pyramid_db::get_auto_update_config(&conn, &slug).unwrap_or(config);
                    serde_json::to_value(&refreshed).map_err(|e| e.to_string())
                }
                None => Ok(serde_json::json!({"status": "updated"})),
            },
            Err(e) => Err(e.to_string()),
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

#[tauri::command]
async fn pyramid_auto_update_config_init(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Validate slug exists and get its info
    let slug_info = {
        let conn = state.pyramid.reader.lock().await;
        wire_node_lib::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?
    };

    // DADBEAR is only meaningful for code/document pyramids with source files
    let ct = slug_info.content_type.as_str();
    if ct == "question" || ct == "conversation" || ct == "vine" {
        return Err(format!(
            "DADBEAR auto-update is not supported for {} pyramids",
            ct
        ));
    }

    // INSERT OR IGNORE — idempotent, safe if config already exists
    {
        let conn = state.pyramid.writer.lock().await;
        pyramid_db::insert_auto_update_config_defaults(&conn, &slug, "[]", "[]")
            .map_err(|e| e.to_string())?;
    }

    // Read back the config (possibly pre-existing)
    let config = {
        let conn = state.pyramid.reader.lock().await;
        pyramid_db::get_auto_update_config(&conn, &slug)
            .ok_or_else(|| "Failed to initialize DADBEAR config".to_string())?
    };

    // Start the stale engine if one isn't already running for this slug
    {
        let mut engines = state.pyramid.stale_engines.lock().await;
        if !engines.contains_key(&slug) {
            let db_path = state
                .pyramid
                .data_dir
                .as_ref()
                .expect("data_dir not set")
                .join("pyramid.db")
                .to_string_lossy()
                .to_string();
            let (api_key, model) = {
                let cfg = state.pyramid.config.read().await;
                (cfg.api_key.clone(), cfg.primary_model.clone())
            };

            let mut engine = wire_node_lib::pyramid::stale_engine::PyramidStaleEngine::new(
                &slug,
                config.clone(),
                &db_path,
                &api_key,
                &model,
                state.pyramid.operational.as_ref().clone(),
            );
            engine.start_poll_loop();
            engines.insert(slug.clone(), engine);
            tracing::info!(slug = %slug, "DADBEAR engine started via config_init");

            // Start file watcher if source paths exist
            drop(engines); // release engines lock before watcher setup
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
                        &slug,
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
                        tracing::info!(slug = %slug, "File watcher started via config_init");
                        let mut watchers = state.pyramid.file_watchers.lock().await;
                        watchers.insert(slug.clone(), watcher);
                    }
                    Err(e) => {
                        tracing::warn!(slug = %slug, error = %e, "File watcher failed to start via config_init");
                    }
                }
            }
        }
    }

    serde_json::to_value(&config).map_err(|e| e.to_string())
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
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET frozen = 1, frozen_at = ?1 WHERE slug = ?2",
            rusqlite::params![now, slug],
        );
        let _ = conn.execute(
            "UPDATE pyramid_pending_mutations SET processed = 1 WHERE processed = 0 AND slug = ?1",
            rusqlite::params![slug],
        );
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
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET frozen = 0, frozen_at = NULL WHERE slug = ?1",
            rusqlite::params![slug],
        );
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
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET breaker_tripped = 0, breaker_tripped_at = NULL WHERE slug = ?1",
            rusqlite::params![slug],
        );
        Ok(
            serde_json::json!({"status": "resumed", "slug": slug, "note": "No active engine, breaker cleared in DB"}),
        )
    }
}

#[tauri::command]
async fn pyramid_auto_update_run_now(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<serde_json::Value, String> {
    // Extract what we need from the engine while briefly holding the lock, then release
    let (db_path, api_key, model, semaphore, phase_arc, detail_arc, summary_arc) = {
        let engines = state.pyramid.stale_engines.lock().await;
        let engine = engines
            .get(&slug)
            .ok_or("No active stale engine for this pyramid")?;
        (
            engine.db_path.clone(),
            engine.api_key.clone(),
            engine.model.clone(),
            engine.concurrent_helpers.clone(),
            engine.current_phase.clone(),
            engine.phase_detail.clone(),
            engine.last_result_summary.clone(),
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
            &api_key,
            &model,
            phase_arc.clone(),
            detail_arc.clone(),
            summary_arc.clone(),
            &state.pyramid.operational,
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

    let (db_path, api_key, model, semaphore, phase_arc, detail_arc, summary_arc) = {
        let engines = state.pyramid.stale_engines.lock().await;
        let engine = engines
            .get(&slug)
            .ok_or("No active stale engine for this pyramid")?;
        (
            engine.db_path.clone(),
            engine.api_key.clone(),
            engine.model.clone(),
            engine.concurrent_helpers.clone(),
            engine.current_phase.clone(),
            engine.phase_detail.clone(),
            engine.last_result_summary.clone(),
        )
    };

    for layer in 0..=3 {
        let _ = wire_node_lib::pyramid::stale_engine::drain_and_dispatch(
            &slug,
            layer,
            0,
            &db_path,
            semaphore.clone(),
            &api_key,
            &model,
            phase_arc.clone(),
            detail_arc.clone(),
            summary_arc.clone(),
            &state.pyramid.operational,
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
        let _ = conn.execute(
            "INSERT OR IGNORE INTO pyramid_auto_update_config (slug) VALUES (?1)",
            rusqlite::params![new_slug],
        );
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
    let config = state.pyramid.config.read().await;
    let api_key = config.api_key.clone();
    let model = config.primary_model.clone();
    drop(config);

    let directory = pyramid_faq::get_faq_directory(
        &state.pyramid.reader,
        &state.pyramid.writer,
        &slug,
        &api_key,
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

    // Shared JWT public key and node ID for the server module
    let jwt_public_key = Arc::new(RwLock::new(config.jwt_public_key.clone()));
    let node_id_shared = Arc::new(RwLock::new(config.node_id.clone()));

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

    let pyramid_state = Arc::new(wire_node_lib::pyramid::PyramidState {
        reader: Arc::new(tokio::sync::Mutex::new(pyramid_reader)),
        writer: Arc::new(tokio::sync::Mutex::new(pyramid_writer)),
        config: Arc::new(RwLock::new(pyramid_config.to_llm_config())),
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
    });

    // Load persisted event subscriptions into the in-memory event bus
    {
        let reader = pyramid_state.reader.blocking_lock();
        if let Err(e) = pyramid_state.event_bus.load_from_db_sync(&reader) {
            tracing::warn!("Failed to load event subscriptions from DB: {e}");
        }
    }

    tracing::info!(
        "Pyramid engine initialized at {:?}, ir_executor={}",
        pyramid_db_path,
        pyramid_config.use_ir_executor
    );

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
            api_key: pyramid_config.openrouter_api_key.clone(),
            partner_model: pyramid_config.partner_model.clone(),
        }),
        warm_in_progress: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    });

    tracing::info!("Partner (Dennis) initialized at {:?}", partner_db_path);

    let state = Arc::new(AppState {
        auth: Arc::new(RwLock::new(initial_auth.clone())),
        sync_state: Arc::new(RwLock::new(
            sync::load_sync_state(&config.data_dir()).unwrap_or_default(),
        )),
        credits: Arc::new(RwLock::new(initial_credits)),
        tunnel_state: Arc::new(RwLock::new(initial_tunnel)),
        market_state: Arc::new(RwLock::new(
            market::load_market_state(&config.data_dir()).unwrap_or_default(),
        )),
        work_stats: Arc::new(RwLock::new(work::WorkStats::default())),
        config: Arc::new(RwLock::new(config.clone())),
        pyramid: pyramid_state,
        partner: partner_state,
    });

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
                                    let registration = match auth::register_with_session(
                                        &c.api_url,
                                        &supabase_token,
                                        &c.node_name(),
                                    ).await {
                                        Ok(reg) => Some(reg),
                                        Err(e) => {
                                            tracing::error!("Wire registration after deep link failed: {}", e);
                                            None
                                        }
                                    };

                                    let node_id = registration.as_ref().map(|r| r.node_id.clone());
                                    let api_token = registration.as_ref().map(|r| r.api_token.clone());

                                    // Now briefly acquire write lock to update state
                                    {
                                        let mut auth_write = s.auth.write().await;
                                        auth_write.node_id = node_id.clone();
                                        auth_write.api_token = api_token.clone();
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
                ).await;
            });

            // --- Pyramid Sync Timer (WS-ONLINE-A) ---
            // Ticks every 60s checking for unpublished pyramid builds.
            // If a linked pyramid has a new completed build, auto-publishes to Wire.
            let sync_pyramid_state = state.pyramid.clone();
            let sync_tunnel_state = state.tunnel_state.clone();
            let pyramid_sync_state = std::sync::Arc::new(
                tokio::sync::Mutex::new(
                    wire_node_lib::pyramid::sync::PyramidSyncState::new()
                )
            );
            let pyramid_sync_state_shared = pyramid_sync_state.clone();
            tauri::async_runtime::spawn(async move {
                // Wait for startup to complete before starting sync timer
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    // Read current tunnel URL for metadata publication (WS-ONLINE-B)
                    let tunnel_url = {
                        let ts = sync_tunnel_state.read().await;
                        ts.tunnel_url.clone()
                    };
                    wire_node_lib::pyramid::sync::pyramid_sync_tick(
                        &sync_pyramid_state,
                        &pyramid_sync_state_shared,
                        tunnel_url,
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
                                match auth::register_with_session(
                                    &startup_config.api_url,
                                    &new_access,
                                    &startup_config.node_name(),
                                ).await {
                                    Ok(reg) => {
                                        tracing::info!("Wire node registered on startup: {}", reg.node_id);
                                        // Briefly acquire write lock to store registration results
                                        {
                                            let mut auth_write = startup_state.auth.write().await;
                                            auth_write.node_id = Some(reg.node_id.clone());
                                            auth_write.api_token = Some(reg.api_token.clone());
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
                            tunnel_url.as_deref(),
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
            pyramid_delete_slug,
            pyramid_get_config,
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
            pyramid_auto_update_config_init,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wire Node");
}
