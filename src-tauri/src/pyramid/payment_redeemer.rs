// payment_redeemer.rs — WS-ONLINE-H: Payment token redemption engine
//
// Handles the full lifecycle of payment token settlement:
//   1. Immediate post-query redeem via POST /api/v1/wire/payment-redeem
//   2. Background sweep: retries pending tokens with exponential backoff,
//      expires stale tokens past TTL
//
// The payment token JWT is self-authenticating — the serving_node_operator_id
// embedded in the token tells the Wire server who to credit. No additional
// auth headers are needed for the redeem call.

use std::sync::Arc;
use std::time::Duration;

use crate::pyramid::db;
use crate::pyramid::PyramidState;
use crate::server::PaymentTokenClaims;

// ── Operational constants ──────────────────────────────────────────────
//
// These are infrastructure parameters (like TCP retries), not economic
// or behavioral parameters. They do NOT belong in the contribution system.

/// HTTP timeout for the redeem call (seconds). Short — the query is already
/// served, this is fire-and-forget settlement.
const REDEEM_TIMEOUT_SECS: u64 = 10;

/// How often the background sweeper runs (seconds).
const SWEEP_INTERVAL_SECS: u64 = 30;

/// Base delay for exponential backoff (seconds). Actual delay per retry:
/// BASE_BACKOFF_SECS * 2^retry_count → 5, 10, 20, 40, 80s for retries 0–4.
const BASE_BACKOFF_SECS: u64 = 5;

// ── Outcome classification ─────────────────────────────────────────────

/// Result of a POST /api/v1/wire/payment-redeem call.
pub enum RedeemOutcome {
    /// 2xx — token successfully redeemed, credits transferred.
    Success {
        tx_id: String,
        stamp_credited: u64,
        access_credited: u64,
    },
    /// Network error, timeout, or 5xx — transient, worth retrying.
    Transient(String),
    /// 400, 401, 409 — token expired/invalid/already redeemed. Terminal.
    Permanent(String),
}

// ── Core HTTP call ─────────────────────────────────────────────────────

/// Attempt to redeem a payment token with the Wire server.
///
/// POST {wire_url}/api/v1/wire/payment-redeem
/// Body: { "payment_token": "<JWT>" }
///
/// No Authorization header — the token is self-authenticating.
pub async fn redeem_payment_token(
    wire_url: &str,
    payment_token: &str,
) -> RedeemOutcome {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(REDEEM_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return RedeemOutcome::Transient(format!("HTTP client build failed: {}", e)),
    };

    let url = format!(
        "{}/api/v1/wire/payment-redeem",
        wire_url.trim_end_matches('/')
    );

    let body = serde_json::json!({ "payment_token": payment_token });

    let response = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            if e.is_timeout() {
                return RedeemOutcome::Transient(format!("timeout after {}s", REDEEM_TIMEOUT_SECS));
            }
            return RedeemOutcome::Transient(format!("network error: {}", e));
        }
    };

    let status = response.status();

    if status.is_success() {
        // Parse success response: { tx_id, stamp_credited, access_credited }
        match response.json::<serde_json::Value>().await {
            Ok(body) => {
                let tx_id = body
                    .get("tx_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let stamp_credited = body
                    .get("stamp_credited")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let access_credited = body
                    .get("access_credited")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                RedeemOutcome::Success {
                    tx_id,
                    stamp_credited,
                    access_credited,
                }
            }
            Err(e) => {
                // Got 2xx but couldn't parse body — treat as success anyway.
                // The server processed the token; we just can't read the receipt.
                tracing::warn!(
                    error = %e,
                    "Payment redeem returned 2xx but response body unparseable"
                );
                RedeemOutcome::Success {
                    tx_id: "unparseable".to_string(),
                    stamp_credited: 0,
                    access_credited: 0,
                }
            }
        }
    } else if status.is_server_error() {
        // 5xx — Wire server issue, transient
        let body_text = response.text().await.unwrap_or_default();
        RedeemOutcome::Transient(format!(
            "server error {}: {}",
            status,
            body_text.chars().take(200).collect::<String>()
        ))
    } else {
        // 4xx — permanent failure (400 invalid, 401 rejected, 409 already redeemed/expired)
        let body_text = response.text().await.unwrap_or_default();
        RedeemOutcome::Permanent(format!(
            "{}: {}",
            status,
            body_text.chars().take(200).collect::<String>()
        ))
    }
}

// ── Post-query settlement ──────────────────────────────────────────────

/// Fire-and-forget payment redemption after serving a billable query.
///
/// Called via tokio::spawn from billable handlers. By this point, the nonce
/// has already been inserted into pyramid_unredeemed_tokens (insert-before-serve
/// for replay protection). This function only updates the existing row's status.
///
/// On success: marks the token as redeemed.
/// On transient failure: leaves the token as 'pending' for the background sweeper.
/// On permanent failure: marks the token as 'failed'.
pub async fn fire_and_forget_redeem(
    state: Arc<PyramidState>,
    wire_url: String,
    nonce: String,
    payment_token: String,
    slug: String,
    query_type: String,
) {
    match redeem_payment_token(&wire_url, &payment_token).await {
        RedeemOutcome::Success {
            tx_id,
            stamp_credited,
            access_credited,
        } => {
            tracing::info!(
                nonce = %nonce,
                tx_id = %tx_id,
                stamp = stamp_credited,
                access = access_credited,
                slug = %slug,
                query_type = %query_type,
                "Payment token redeemed (WS-ONLINE-H)"
            );
            let conn = state.writer.lock().await;
            if let Err(e) = db::mark_redeemed(&conn, &nonce) {
                tracing::warn!(nonce = %nonce, error = %e, "Failed to mark token as redeemed in DB");
            }
        }
        RedeemOutcome::Transient(reason) => {
            tracing::warn!(
                nonce = %nonce,
                slug = %slug,
                reason = %reason,
                "Payment redeem transient failure — sweeper will retry (WS-ONLINE-H)"
            );
            // Leave as 'pending' — the background sweeper will pick it up.
        }
        RedeemOutcome::Permanent(reason) => {
            tracing::warn!(
                nonce = %nonce,
                slug = %slug,
                reason = %reason,
                "Payment redeem permanent failure — marking failed (WS-ONLINE-H)"
            );
            let conn = state.writer.lock().await;
            if let Err(e) = db::mark_unredeemed_failed(&conn, &nonce) {
                tracing::warn!(nonce = %nonce, error = %e, "Failed to mark token as failed in DB");
            }
        }
    }
}

// ── Background sweeper ─────────────────────────────────────────────────

/// Background loop that retries unredeemed tokens and expires stale ones.
///
/// Spawned once at app startup via `tauri::async_runtime::spawn`.
/// Runs forever with SWEEP_INTERVAL_SECS between cycles.
///
/// Each cycle:
///   1. Expire tokens past their TTL (server already released credits)
///   2. Retry pending tokens with exponential backoff
pub async fn spawn_redemption_sweeper(state: Arc<PyramidState>) {
    let wire_url = std::env::var("WIRE_URL")
        .unwrap_or_else(|_| "https://newsbleach.com".to_string());

    // Wait one full interval before the first run (let the app finish starting)
    tokio::time::sleep(Duration::from_secs(SWEEP_INTERVAL_SECS)).await;

    loop {
        // ── Step 1: Expire stale tokens ────────────────────────────────
        {
            let conn = state.writer.lock().await;
            match db::expire_unredeemed_tokens(&conn) {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        count = n,
                        "Expired stale unredeemed payment tokens (WS-ONLINE-H)"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to expire unredeemed tokens");
                }
            }
        }
        // Writer lock dropped here

        // ── Step 2: Retry pending tokens ───────────────────────────────
        let tokens = {
            let conn = state.reader.lock().await;
            db::get_unredeemed_tokens(&conn).unwrap_or_default()
        };
        // Reader lock dropped here

        if !tokens.is_empty() {
            tracing::debug!(
                count = tokens.len(),
                "Sweeper found pending unredeemed tokens"
            );

            for token in &tokens {
                // Exponential backoff check
                if let Some(ref last_retry) = token.last_retry_at {
                    let backoff_secs = BASE_BACKOFF_SECS * (1u64 << token.retry_count.min(10) as u32);
                    if let Ok(last) = chrono::NaiveDateTime::parse_from_str(last_retry, "%Y-%m-%d %H:%M:%S") {
                        let elapsed = chrono::Utc::now()
                            .naive_utc()
                            .signed_duration_since(last)
                            .num_seconds();
                        if elapsed < backoff_secs as i64 {
                            continue; // Not due yet
                        }
                    }
                }

                match redeem_payment_token(&wire_url, &token.payment_token).await {
                    RedeemOutcome::Success {
                        tx_id,
                        stamp_credited,
                        access_credited,
                    } => {
                        tracing::info!(
                            nonce = %token.nonce,
                            tx_id = %tx_id,
                            stamp = stamp_credited,
                            access = access_credited,
                            retry = token.retry_count,
                            "Sweeper redeemed token (WS-ONLINE-H)"
                        );
                        let conn = state.writer.lock().await;
                        if let Err(e) = db::mark_redeemed(&conn, &token.nonce) {
                            tracing::warn!(nonce = %token.nonce, error = %e, "Sweeper failed to mark redeemed");
                        }
                    }
                    RedeemOutcome::Transient(reason) => {
                        tracing::warn!(
                            nonce = %token.nonce,
                            retry = token.retry_count,
                            reason = %reason,
                            "Sweeper redeem transient failure"
                        );
                        let conn = state.writer.lock().await;
                        if let Err(e) = db::increment_unredeemed_retry(&conn, &token.nonce) {
                            tracing::warn!(nonce = %token.nonce, error = %e, "Sweeper failed to increment retry");
                        }
                    }
                    RedeemOutcome::Permanent(reason) => {
                        tracing::warn!(
                            nonce = %token.nonce,
                            retry = token.retry_count,
                            reason = %reason,
                            "Sweeper redeem permanent failure — marking failed"
                        );
                        let conn = state.writer.lock().await;
                        if let Err(e) = db::mark_unredeemed_failed(&conn, &token.nonce) {
                            tracing::warn!(nonce = %token.nonce, error = %e, "Sweeper failed to mark failed");
                        }
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(SWEEP_INTERVAL_SECS)).await;
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Convert a JWT `exp` claim (Unix epoch seconds) to SQLite datetime string.
pub fn exp_to_sqlite_datetime(exp: u64) -> String {
    use chrono::{DateTime, Utc};
    match DateTime::<Utc>::from_timestamp(exp as i64, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => {
            // Fallback: 10 minutes from now (matches server-side TOKEN_TTL_SECONDS)
            let fallback = Utc::now() + chrono::Duration::seconds(600);
            fallback.format("%Y-%m-%d %H:%M:%S").to_string()
        }
    }
}

/// Insert a payment token into the unredeemed tokens table before serving a query.
///
/// This serves two purposes:
///   1. Replay protection — the UNIQUE constraint on nonce prevents a second query
///      with the same payment token from being served.
///   2. Retry queue — if the immediate redeem fails, the background sweeper
///      picks up the pending token.
///
/// Returns Ok(row_id) on success, or Err if the nonce already exists (replay).
pub fn insert_before_serve(
    conn: &rusqlite::Connection,
    claims: &PaymentTokenClaims,
    payment_token: &str,
    querier_operator_id: &str,
    slug: &str,
    query_type: &str,
) -> Result<i64, String> {
    let nonce = match claims.nonce.as_deref() {
        Some(n) if !n.is_empty() => n,
        _ => return Err("Payment token missing nonce — cannot guarantee replay protection".to_string()),
    };
    let expires_at = match claims.exp {
        Some(exp) => exp_to_sqlite_datetime(exp),
        None => {
            // No exp claim — use 10 min from now as fallback
            let fallback = chrono::Utc::now() + chrono::Duration::seconds(600);
            fallback.format("%Y-%m-%d %H:%M:%S").to_string()
        }
    };

    db::insert_unredeemed_token(
        conn,
        nonce,
        payment_token,
        querier_operator_id,
        slug,
        query_type,
        claims.stamp_amount as i64,
        claims.access_amount as i64,
        claims.total_amount as i64,
        &expires_at,
    )
    .map_err(|e| {
        let err_str = e.to_string();
        if err_str.contains("UNIQUE constraint failed") {
            format!("Payment token nonce already used: {}", nonce)
        } else {
            format!("Failed to record payment token: {}", err_str)
        }
    })
}
