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
