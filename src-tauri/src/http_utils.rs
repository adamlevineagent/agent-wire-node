// http_utils.rs — Shared HTTP utilities for warp route handlers
//
// Contains auth helpers, JSON reply helpers, and error types
// shared between pyramid and partner route modules.

use warp::Reply;

// ── Auth helpers ─────────────────────────────────────────────────────

/// Constant-time string comparison to prevent timing attacks on auth tokens.
/// Pads to max length so timing does not leak length information.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let max_len = a.len().max(b.len());
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut acc = (a.len() != b.len()) as u8; // mismatch if lengths differ
    for i in 0..max_len {
        let x = if i < a_bytes.len() { a_bytes[i] } else { 0xFF };
        let y = if i < b_bytes.len() { b_bytes[i] } else { 0x00 };
        acc |= x ^ y;
    }
    acc == 0
}

#[derive(Debug)]
pub struct Unauthorized;
impl warp::reject::Reject for Unauthorized {}

/// Shared rejection recovery — maps our custom rejection types onto
/// proper HTTP status codes with JSON error bodies.
///
/// Without this attached to the root router, warp falls back to its
/// default rejection handler which doesn't know about `Unauthorized` /
/// `RateLimited` (our custom types) and returns 404 Not Found, making
/// auth failures look indistinguishable from missing routes. Apply via
/// `routes.recover(handle_rejection)` at the top of `start_server`.
pub async fn handle_rejection(
    err: warp::Rejection,
) -> Result<warp::reply::Response, warp::Rejection> {
    use warp::Reply;
    if err.find::<Unauthorized>().is_some() {
        return Ok(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"error": "unauthorized — provide a valid Bearer token"}),
            ),
            warp::http::StatusCode::UNAUTHORIZED,
        )
        .into_response());
    }
    if err.find::<crate::pyramid::routes::RateLimited>().is_some() {
        return Ok(warp::reply::with_status(
            warp::reply::json(
                &serde_json::json!({"error": "rate limit exceeded, try again later"}),
            ),
            warp::http::StatusCode::TOO_MANY_REQUESTS,
        )
        .into_response());
    }
    // Fall through so warp's default handler still produces 404 / 405 /
    // 400 / etc. for the rejections we don't own.
    Err(err)
}

// ── JSON reply helpers ──────────────────────────────────────────────

pub fn json_error(status: warp::http::StatusCode, msg: &str) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"error": msg})),
        status,
    )
    .into_response()
}

pub fn json_ok<T: serde::Serialize>(val: &T) -> warp::reply::Response {
    warp::reply::json(val).into_response()
}

// ── Wire API client ─────────────────────────────────────────────────
//
// Shared reqwest-based helper for calling the Wire's JSON API with a
// Bearer token. Lives here (not in main.rs) so both the Tauri IPC layer
// and the warp HTTP route handlers can use it without cross-crate
// plumbing. (main.rs is the binary; lib-level modules can't reach it.)
//
// No 401-refresh logic — that lives one layer up in main.rs's
// `operator_api_call` IPC which owns the operator-session lifecycle.
// Compute market calls go through this raw helper because they use
// `api_token` (machine token) which is stable per-boot.

/// Call a Wire API endpoint with Bearer-token auth. On non-2xx, returns
/// `Err(String)` with the status code + parsed body for logging. On
/// success, returns `(status, parsed_json_response)`.
pub async fn send_api_request(
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

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let error_value = serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|_| serde_json::json!({ "error": text, "status": status.as_u16() }));
        return Err(format!("API error {}: {}", status.as_u16(), error_value));
    }

    // Handle empty-body 2xx (e.g. 204 No Content from DELETE routes).
    // `resp.json()` would fail with "error decoding response body" on
    // empty payloads, which the caller would mistake for an upstream
    // failure even though the operation succeeded. Return Null in that
    // case so callers can still match on `status.is_success()`.
    let body_text = resp.text().await.unwrap_or_default();
    if body_text.trim().is_empty() {
        return Ok((status, serde_json::Value::Null));
    }
    let result: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| format!("response body not valid JSON: {e}"))?;
    Ok((status, result))
}

// ---------------------------------------------------------------------------
// send_api_request_with_hints — structured-error variant used by the
// compute-market RPC path (/quote, /purchase, /fill). Captures the
// `X-Wire-Retry` and `X-Retriable` response headers on error so the
// walker classifier can honor Wire's retry intent (per Wire's
// WS4-commented contract at compute-errors.ts: "node-side classify_retry
// reads X-Wire-Retry first, falls back to HTTP code if absent").
//
// Existing `send_api_request` callers are unchanged — they don't need
// retry hints and their Err(String) shape stays the same.
// ---------------------------------------------------------------------------

/// Wire's two retry-intent headers. Both are optional; absence = fall
/// back to HTTP-code heuristic.
#[derive(Debug, Clone, Default)]
pub struct RetryHints {
    /// `X-Wire-Retry: never | transient | backoff | <other>`. Wire sets
    /// this on every 4xx/5xx via `retryIntentForStatus`. Node's
    /// classifier reads this FIRST and falls back to slug/HTTP-code
    /// logic only when absent.
    pub x_wire_retry: Option<String>,
    /// `X-Retriable: true` — explicitly set on Wire's P0410 (transient
    /// conflict) responses. Orthogonal to `x_wire_retry` for legacy
    /// compat but usually matches `x_wire_retry: transient`.
    pub x_retriable: Option<bool>,
}

/// Structured error from a Wire API call. Carries enough to run
/// classification downstream — status, body (parsed), and retry hints.
///
/// Implements `Display` in the same shape the legacy `Err(String)`
/// path used (`"API error {code}: {body}"`) so downstream parsers
/// that grep the message string keep working unchanged.
#[derive(Debug, Clone)]
pub struct ApiErrorWithHints {
    pub status: u16,
    pub body: serde_json::Value,
    pub hints: RetryHints,
}

impl std::fmt::Display for ApiErrorWithHints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "API error {}: {}", self.status, self.body)
    }
}

impl From<ApiErrorWithHints> for String {
    fn from(e: ApiErrorWithHints) -> String {
        e.to_string()
    }
}

/// Call a Wire API endpoint and capture retry-intent headers on error.
///
/// On 2xx: returns `(status, parsed_json)` — identical to `send_api_request`.
/// On non-2xx: returns `Err(ApiErrorWithHints)` with status + parsed body
/// + X-Wire-Retry / X-Retriable headers.
/// On transport failure (connect, timeout, etc.): returns
/// `Err(ApiErrorWithHints { status: 0, body: {"transport": "..."}, hints: default })`
/// so the caller's classification pipeline has a single shape to match.
pub async fn send_api_request_with_hints(
    api_url: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
    extra_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<(reqwest::StatusCode, serde_json::Value), ApiErrorWithHints> {
    let client = reqwest::Client::new();
    let url = format!("{}{}", api_url, path);
    let mut req = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PATCH" => client.patch(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => {
            return Err(ApiErrorWithHints {
                status: 0,
                body: serde_json::json!({ "error": "invalid_method" }),
                hints: RetryHints::default(),
            })
        }
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

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(ApiErrorWithHints {
                status: 0,
                body: serde_json::json!({ "transport": e.to_string() }),
                hints: RetryHints::default(),
            });
        }
    };
    let status = resp.status();
    let hints = extract_retry_hints(resp.headers());

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let body_value = serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|_| serde_json::json!({ "error": text, "status": status.as_u16() }));
        return Err(ApiErrorWithHints {
            status: status.as_u16(),
            body: body_value,
            hints,
        });
    }

    let body_text = resp.text().await.unwrap_or_default();
    if body_text.trim().is_empty() {
        return Ok((status, serde_json::Value::Null));
    }
    let result: serde_json::Value = match serde_json::from_str(&body_text) {
        Ok(v) => v,
        Err(e) => {
            return Err(ApiErrorWithHints {
                status: status.as_u16(),
                body: serde_json::json!({ "parse_error": e.to_string(), "raw": body_text }),
                hints,
            });
        }
    };
    Ok((status, result))
}

fn extract_retry_hints(headers: &reqwest::header::HeaderMap) -> RetryHints {
    let x_wire_retry = headers
        .get("x-wire-retry")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let x_retriable = headers
        .get("x-retriable")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| match s.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        });
    RetryHints {
        x_wire_retry,
        x_retriable,
    }
}
