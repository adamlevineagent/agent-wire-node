use serde::{Deserialize, Serialize};

/// Auth state — stores Supabase session + Wire node registration + operator session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthState {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub node_id: Option<String>,       // wire node ID after registration
    #[serde(default)]
    pub api_token: Option<String>,     // gne_live_ machine token from register-with-session
    pub first_started_at: Option<String>, // node age — first ever login
    #[serde(default)]
    pub operator_session_token: Option<String>,
    #[serde(default)]
    pub operator_id: Option<String>,
    #[serde(default)]
    pub operator_session_expires_at: Option<String>,
}

impl AuthState {
    pub fn is_authenticated(&self) -> bool {
        self.access_token.is_some()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
}

/// Wire node registration response
#[derive(Debug, Deserialize)]
pub struct NodeRegistrationResponse {
    pub node_id: String,
    pub jwt_public_key: Option<String>,
}

// --- Magic Link Auth --------------------------------------------------------

/// Send a magic link to the user's email via Wire's Supabase
/// The link redirects to the node's local HTTP server for token capture
pub async fn send_magic_link(
    supabase_url: &str,
    supabase_key: &str,
    email: &str,
    _server_port: u16,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    let redirect_url = "https://newsbleach.com/auth/wire-node-callback";
    let url = format!(
        "{}/auth/v1/otp?redirect_to={}",
        supabase_url,
        urlencoding::encode(redirect_url)
    );

    let body = serde_json::json!({
        "email": email
    });

    let resp = client
        .post(&url)
        .header("apikey", supabase_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error sending magic link: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Magic link failed ({}): {}", status, text));
    }

    tracing::info!("Magic link sent to {}", email);
    Ok(())
}

/// Verify a magic link by extracting its token from the pasted URL
pub async fn verify_magic_link_token(
    supabase_url: &str,
    supabase_key: &str,
    magic_link_url: &str,
    _email: &str,
) -> Result<AuthState, String> {
    let url = reqwest::Url::parse(magic_link_url)
        .map_err(|e| format!("Invalid URL: {}", e))?;

    let token_hash = url.query_pairs()
        .find(|(k, _)| k == "token_hash" || k == "token")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| "No token found in magic link URL".to_string())?;

    let link_type = url.query_pairs()
        .find(|(k, _)| k == "type")
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| "magiclink".to_string());

    tracing::info!("Verifying magic link token_hash (type={}, hash_len={})", link_type, token_hash.len());

    let client = reqwest::Client::new();
    let verify_url = format!("{}/auth/v1/verify", supabase_url);

    let body = serde_json::json!({
        "type": link_type,
        "token_hash": token_hash,
    });

    let resp = client
        .post(&verify_url)
        .header("apikey", supabase_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Verification request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Token verification failed ({}): {}", status, text));
    }

    let login_resp: LoginResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse verify response: {}", e))?;

    tracing::info!("Magic link verified for user {}", login_resp.user.id);

    Ok(AuthState {
        access_token: Some(login_resp.access_token),
        refresh_token: Some(login_resp.refresh_token),
        user_id: Some(login_resp.user.id),
        email: login_resp.user.email,
        node_id: None,
        api_token: None,
        first_started_at: None,
        operator_session_token: None,
        operator_id: None,
        operator_session_expires_at: None,
    })
}

// --- OTP Code Verification --------------------------------------------------

/// Verify a 6-digit OTP code from email
pub async fn verify_otp(
    supabase_url: &str,
    supabase_key: &str,
    email: &str,
    otp_code: &str,
) -> Result<AuthState, String> {
    let client = reqwest::Client::new();
    let verify_url = format!("{}/auth/v1/verify", supabase_url);

    let body = serde_json::json!({
        "type": "email",
        "token": otp_code,
        "email": email,
    });

    tracing::info!("Verifying OTP code for {}", email);

    let resp = client
        .post(&verify_url)
        .header("apikey", supabase_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OTP verification request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("OTP verification failed ({}): {}", status, text));
    }

    let login_resp: LoginResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse OTP verify response: {}", e))?;

    tracing::info!("OTP verified for user {}", login_resp.user.id);

    Ok(AuthState {
        access_token: Some(login_resp.access_token),
        refresh_token: Some(login_resp.refresh_token),
        user_id: Some(login_resp.user.id),
        email: login_resp.user.email,
        node_id: None,
        api_token: None,
        first_started_at: None,
        operator_session_token: None,
        operator_id: None,
        operator_session_expires_at: None,
    })
}

// --- Password Auth (fallback) -----------------------------------------------

/// Login to Supabase with email/password
pub async fn login(
    supabase_url: &str,
    supabase_key: &str,
    email: &str,
    password: &str,
) -> Result<AuthState, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/auth/v1/token?grant_type=password", supabase_url);

    let body = serde_json::json!({
        "email": email,
        "password": password,
    });

    let resp = client
        .post(&url)
        .header("apikey", supabase_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Login failed ({}): {}", status, text));
    }

    let login_resp: LoginResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse login response: {}", e))?;

    Ok(AuthState {
        access_token: Some(login_resp.access_token),
        refresh_token: Some(login_resp.refresh_token),
        user_id: Some(login_resp.user.id),
        email: login_resp.user.email,
        node_id: None,
        api_token: None,
        first_started_at: None,
        operator_session_token: None,
        operator_id: None,
        operator_session_expires_at: None,
    })
}

// --- Token Refresh ----------------------------------------------------------

/// Refresh access token using a refresh_token
pub async fn refresh_session(
    supabase_url: &str,
    supabase_key: &str,
    refresh_token: &str,
) -> Result<(String, String), String> {
    let client = reqwest::Client::new();
    let url = format!("{}/auth/v1/token?grant_type=refresh_token", supabase_url);

    let body = serde_json::json!({ "refresh_token": refresh_token });

    let resp = client
        .post(&url)
        .header("apikey", supabase_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {}", e))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Refresh failed: {}", text));
    }

    let lr: LoginResponse = resp.json().await
        .map_err(|e| format!("Refresh parse error: {}", e))?;

    tracing::info!("Session refreshed successfully");
    Ok((lr.access_token, lr.refresh_token))
}

// --- Wire Node Registration -------------------------------------------------

/// Register this machine as a Wire node on first login.
/// POST to /api/v1/node/register, stores jwt_public_key from response.
pub async fn register_wire_node(
    api_url: &str,
    access_token: &str,
    node_name: &str,
    _storage_cap_gb: f64,
) -> Result<NodeRegistrationResponse, String> {
    let client = reqwest::Client::new();

    let url = format!("{}/api/v1/node/register", api_url);
    let body = serde_json::json!({
        "name": node_name,
        "capabilities": ["cache", "verify", "grade", "enrich", "storage"],
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Node registration failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Registration failed ({}): {}", status, text));
    }

    let registration: NodeRegistrationResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse registration response: {}", e))?;

    tracing::info!("Wire node registered: {}", registration.node_id);
    Ok(registration)
}

// --- Session-based Registration ---------------------------------------------

/// Response from POST /api/v1/node/register-with-session
#[derive(Debug, Deserialize)]
pub struct SessionRegistrationResponse {
    pub api_token: String,
    pub node_id: String,
    pub agent_id: String,
    pub operator_id: String,
    pub jwt_public_key: Option<String>,
}

/// Register this desktop node using a Supabase session token.
/// POST to /api/v1/node/register-with-session
/// Returns a gne_live_ machine token for all subsequent Wire API calls.
pub async fn register_with_session(
    api_url: &str,
    supabase_access_token: &str,
    node_name: &str,
) -> Result<SessionRegistrationResponse, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/register-with-session", api_url);

    let body = serde_json::json!({
        "supabase_access_token": supabase_access_token,
        "name": node_name,
        "capabilities": ["cache", "verify", "grade", "enrich", "storage"],
    });

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Session registration request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Session registration failed ({}): {}", status, text));
    }

    let reg: SessionRegistrationResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse session registration response: {}", e))?;

    tracing::info!("Session registration complete: node_id={}, agent_id={}", reg.node_id, reg.agent_id);
    Ok(reg)
}

// --- Heartbeat --------------------------------------------------------------

/// Send heartbeat to Wire API with tunnel_url
pub async fn heartbeat(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    tunnel_url: Option<&str>,
    app_version: Option<&str>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/heartbeat", api_url);

    let mut body = serde_json::json!({
        "node_id": node_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    match tunnel_url {
        Some(turl) if !turl.is_empty() => {
            body["tunnel_url"] = serde_json::Value::String(turl.to_string());
        }
        _ => {
            // Explicitly send null to clear stale tunnel_url on the server
            body["tunnel_url"] = serde_json::Value::Null;
        }
    }
    if let Some(ver) = app_version {
        body["app_version"] = serde_json::Value::String(ver.to_string());
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Heartbeat failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Heartbeat failed ({}): {}", status, text);
        return Err(format!("Heartbeat failed ({}): {}", status, text));
    }

    let response_body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or(serde_json::json!({}));

    tracing::debug!("Heartbeat sent for node {}", node_id);
    Ok(response_body)
}

/// Authenticate from deep link tokens (agentwire://auth/callback#access_token=...&refresh_token=...)
pub async fn set_tokens_from_deep_link(
    supabase_url: &str,
    supabase_anon_key: &str,
    access_token: &str,
    refresh_token: &str,
    app_state: &crate::AppState,
) {
    tracing::info!("Processing deep link auth tokens");

    // Fetch user info from Supabase using the access token
    let client = reqwest::Client::new();
    let user_url = format!("{}/auth/v1/user", supabase_url);
    let user_result = client
        .get(&user_url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("apikey", supabase_anon_key)
        .send()
        .await;

    let (user_id, email) = match user_result {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(user) => (
                    user["id"].as_str().map(|s| s.to_string()),
                    user["email"].as_str().map(|s| s.to_string()),
                ),
                Err(e) => {
                    tracing::error!("Failed to parse user response: {}", e);
                    return; // Don't store unverified tokens
                }
            }
        }
        Ok(resp) => {
            tracing::error!("Failed to fetch user info: {} — not storing tokens", resp.status());
            return; // Don't store tokens if user verification failed
        }
        Err(e) => {
            tracing::error!("Network error fetching user info: {} — not storing tokens", e);
            return; // Don't store tokens on network failure
        }
    };

    tracing::info!("Deep link auth: user_id={:?}, email={:?}", user_id, email);

    // Only update auth state after successful user verification
    let mut auth = app_state.auth.write().await;
    auth.access_token = Some(access_token.to_string());
    auth.refresh_token = Some(refresh_token.to_string());
    auth.user_id = user_id;
    auth.email = email;
}
