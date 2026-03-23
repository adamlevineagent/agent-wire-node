// http_utils.rs — Shared HTTP utilities for warp route handlers
//
// Contains auth helpers, JSON reply helpers, and error types
// shared between pyramid and partner route modules.

use warp::Reply;

// ── Auth helpers ─────────────────────────────────────────────────────

/// Constant-time string comparison to prevent timing attacks on auth tokens.
pub fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[derive(Debug)]
pub struct Unauthorized;
impl warp::reject::Reject for Unauthorized {}

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
