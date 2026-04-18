//! Compute market requester-side client — the three-step HTTP flow
//! this node calls when `pyramid_build` (or any caller) wants to
//! dispatch inference via the compute market instead of running it
//! locally.
//!
//! Flow:
//!
//! ```text
//!   call_market(request, max_wait)
//!     └─ dispatch_market(request)                           [§1+2 of Wire W3 spec]
//!         ├─ POST /api/v1/compute/match                     → {job_id (handle), request_id, rates, queue_position, reservation}
//!         └─ POST /api/v1/compute/fill                      → 200 ACK (provider accepted) or 503 (provider fault)
//!              └─ Register UUID job_id in PendingJobs       [shared with inbound handler at /v1/compute/job-result]
//!     └─ await_result(handle, max_wait)                     [§3 of Wire W4]
//!         ├─ oneshot receiver                               [awakened by inbound push]
//!         │    └─ Success / Failure → return
//!         └─ timeout
//!              ├─ Take+drop our PendingJobs entry           [late push → handler returns already_settled]
//!              └─ GET /api/v1/compute/jobs/:job_id          [status-only poll — did Wire give up?]
//!                   ├─ delivery_status="failed" / "expired_undelivered" → DeliveryTombstoned
//!                   └─ still "executing" / "delivering"     → DeliveryTimedOut
//! ```
//!
//! The match→fill pair uses the node's existing operator bearer
//! (`AuthState.api_token`) — same auth as the `/offers` routes. No JWT
//! minted on this side.
//!
//! Wire mints a `result-delivery` JWT per delivery attempt and pushes
//! the §2.3 envelope to the node's `/v1/compute/job-result` route. The
//! inbound handler verifies the JWT, looks up `PendingJobs` by UUID
//! job_id, and fires the oneshot registered here. See
//! `result_delivery_identity` and `pending_jobs` modules.
//!
//! The handle-path `job_id` returned by `/match` is used in the `/fill`
//! body and the `/jobs/:job_id` poll URL. The UUID `job_id` (extracted
//! from the handle-path via Wire's internal resolution or by capturing
//! both) is what the inbound push carries — the `PendingJobs` map is
//! keyed by UUID because that's what the inbound handler sees. Wire's
//! `/match` response carries the handle-path; the UUID is opaque to
//! this client EXCEPT that the inbound push body has it. To reconcile
//! at register time we use the handle-path (caller-visible) AND the
//! UUID (callback-visible) — but in practice the inbound handler only
//! sees the UUID, so that's the key. Wire returns both in `/jobs/:id`
//! (it resolves the handle-path to UUID internally) but NOT in `/match`
//! today per smoke doc — we'll need the UUID at `/fill` time, which
//! means either:
//!   (a) the `/fill` response carries the UUID (provider_node_id only
//!       today; spec ch§4.5), or
//!   (b) we poll `/jobs/:handle_path` after fill to get the UUID, or
//!   (c) we register PendingJobs keyed by handle-path and the inbound
//!       handler resolves handle→UUID before lookup.
//!
//! Pragmatic choice: (c) — PendingJobs is keyed by UUID per the
//! inbound handler's perspective; we capture the UUID at `/fill` time
//! by immediately issuing ONE `GET /jobs/:handle_path` after the 200
//! from `/fill` to retrieve the UUID, then register. That's one extra
//! round-trip per dispatch but it's small compared to the inference
//! time that's about to happen.
//!
//! **Simpler alternative we'll try first:** register by handle_path
//! initially; have the inbound handler resolve handle→UUID (Wire sends
//! both? or only UUID? smoke will tell). If inbound only sees UUID,
//! swap to the poll-for-UUID approach. Document the decision in the
//! commit that lands after smoke.

use crate::auth::AuthState;
use crate::http_utils::send_api_request;
use crate::pyramid::pending_jobs::{DeliveryPayload, PendingJobs};
use crate::WireNodeConfig;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Caller's request shape for a market dispatch. Node-local; not a
/// wire-protocol type.
#[derive(Debug, Clone)]
pub struct MarketInferenceRequest {
    /// Model identifier (e.g. `"llama3:70b"`, `"gemma4:26b"`). Must be
    /// a model that at least one provider has published an offer for.
    pub model_id: String,
    /// Ceiling on total spend for this match (reservation + worst-case
    /// deposit). Wire uses this to filter offers — only offers that
    /// fit inside the budget are considered.
    pub max_budget: i64,
    /// Input prompt size in tokens, pre-counted by the caller. Used
    /// by Wire for deposit sizing at match time.
    pub input_tokens: i64,
    /// Selection strategy — `"best_price"`, `"balanced"`, or
    /// `"lowest_latency"`. Defaults to `"best_price"` if absent.
    pub latency_preference: LatencyPreference,
    /// ChatML messages — `[{role: "system" | "user" | "assistant", content: "..."}, ...]`.
    /// Multiple system turns are invalid (DD-C); Wire rejects
    /// pre-dispatch with 400.
    pub messages: serde_json::Value,
    /// Completion cap. Required per DD-W28 — Wire's `fill_compute_job`
    /// raises if null.
    pub max_tokens: usize,
    /// 0.0..=2.0.
    pub temperature: f32,
    /// `"bootstrap-relay"` or `"direct"` per Q-PROTO-3. Unknown values
    /// are forwarded (warn-don't-reject on both sides).
    pub privacy_tier: String,
    /// Full HTTPS URL on this node's tunnel where Wire's delivery
    /// worker pushes the result envelope. Contract §2.5. Required.
    pub requester_callback_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyPreference {
    BestPrice,
    Balanced,
    LowestLatency,
}

impl LatencyPreference {
    fn as_wire_str(&self) -> &'static str {
        match self {
            Self::BestPrice => "best_price",
            Self::Balanced => "balanced",
            Self::LowestLatency => "lowest_latency",
        }
    }
}

/// Match + fill output — a handle to the in-flight job. Caller awaits
/// on `await_result(handle, max_wait)` or uses `call_market` for the
/// combined flow.
pub struct MarketRequestHandle {
    pub job_id_handle_path: String,
    pub request_id: String,
    pub matched_rate_in_per_m: i64,
    pub matched_rate_out_per_m: i64,
    pub matched_multiplier_bps: i64,
    pub reservation_fee: i64,
    pub estimated_deposit: i64,
    pub queue_position: u64,
    pub provider_node_id: String,
    pub peer_queue_depth: i64,
    pub deposit_charged: i64,
    pub estimated_output_tokens: i64,
    pub dispatch_timeout_ms: i64,
    /// The key the inbound `/v1/compute/job-result` handler will use
    /// to look up the awaiting oneshot. See `resolve_uuid_from_handle`
    /// for how this is captured.
    pub uuid_job_id: String,
    /// Receiver for the inbound push. Caller awaits this with timeout
    /// via `await_result`.
    result_rx: tokio::sync::oneshot::Receiver<DeliveryPayload>,
}

/// Successful market result — mirror of `MarketResult` in
/// compute_market_ops, but specific to the requester path.
#[derive(Debug, Clone)]
pub struct MarketResult {
    pub content: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model_used: String,
    pub latency_ms: i64,
    pub finish_reason: Option<String>,
}

/// Errors from the requester-side flow. Mapped to either silent
/// fallback (market capacity issues) or hard errors (auth, balance,
/// bad body) by `call_model_unified` per Phase 3 plan §5.
#[derive(Debug, thiserror::Error)]
pub enum RequesterError {
    /// `/match` returned 404 `no_offer_for_model` OR 503 with no
    /// viable match. Silent fallback to local at the caller.
    #[error("no market match: {detail}")]
    NoMatch { detail: String },

    /// `/match` returned 409 `insufficient_balance`. Surface to
    /// operator — not a silent fallback path.
    #[error("insufficient balance: need {need}, have {have}")]
    InsufficientBalance { need: i64, have: i64 },

    /// `/match` failed unexpectedly (500, malformed response, etc.).
    #[error("match failed: HTTP {status}: {body}")]
    MatchFailed { status: u16, body: String },

    /// `/fill` returned 503 with an `X-Wire-Reason`. The reason string
    /// drives silent-fallback vs surface-to-operator at the caller.
    #[error("fill rejected by provider: {reason} (HTTP {status})")]
    FillRejected {
        status: u16,
        reason: String,
        body: String,
    },

    /// `/fill` returned a non-503 error (401 auth failure, 425 in-flight
    /// idempotency collision, etc.).
    #[error("fill failed: HTTP {status}: {body}")]
    FillFailed { status: u16, body: String },

    /// Push didn't arrive within `max_wait_ms` AND Wire's `/jobs`
    /// poll says the job isn't terminally tombstoned (still executing
    /// or delivering). Caller treats as transient, falls back to local.
    #[error("delivery timed out after {waited_ms}ms")]
    DeliveryTimedOut { waited_ms: u64 },

    /// Push didn't arrive AND poll confirmed Wire gave up
    /// (delivery_status=failed or expired_undelivered). Different
    /// semantically from timeout — the content is gone for good.
    #[error("delivery tombstoned by Wire: {reason}")]
    DeliveryTombstoned { reason: String },

    /// The provider's inference returned a failure envelope (captured
    /// via push). Caller surfaces or falls back depending on the code.
    #[error("provider failed: {code}: {message}")]
    ProviderFailed { code: String, message: String },

    /// 401 on any Wire endpoint — operator session broken. Hard error,
    /// no silent fallback (would mask billing failures).
    #[error("auth failed: {0}")]
    AuthFailed(String),

    /// 400 from Wire with a structured `error` slug that indicates
    /// the *caller's* configuration is wrong, not a transient capacity
    /// issue. Examples: `multiple_nodes_require_explicit_node_id`,
    /// `no_node_for_agent`, `invalid_body`. Must NOT fall through to
    /// the local-pool fallback — config errors silently routing around
    /// is the same class of bug as the "have 0, need 0" masquerade
    /// that left the market silently broken for weeks. Surface loudly.
    #[error("compute-market misconfigured: {error_slug}")]
    ConfigError {
        error_slug: String,
        detail: Option<serde_json::Value>,
    },

    /// Catch-all for I/O, serde, timeout-during-http, etc.
    #[error("internal: {0}")]
    Internal(String),
}

// ═══════════════════════════════════════════════════════════════════
// dispatch_market — steps 1+2 of the four-beat flow
// ═══════════════════════════════════════════════════════════════════

/// Perform the `/match` + `/fill` round-trip. Returns a handle holding
/// the oneshot receiver. Caller proceeds to `await_result`.
pub async fn dispatch_market(
    req: MarketInferenceRequest,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    pending_jobs: &PendingJobs,
) -> Result<MarketRequestHandle, RequesterError> {
    // Basic input checks the Wire would 400 on anyway — fail fast here
    // to keep the error taxonomy clean.
    if req.model_id.trim().is_empty() {
        return Err(RequesterError::Internal("model_id empty".into()));
    }
    if !req.requester_callback_url.starts_with("https://") {
        return Err(RequesterError::Internal(
            "requester_callback_url must be https:// (DD-Q)".into(),
        ));
    }
    if req.messages.as_array().map_or(true, |a| a.is_empty()) {
        return Err(RequesterError::Internal(
            "messages must be a non-empty array".into(),
        ));
    }
    if req.max_tokens == 0 {
        return Err(RequesterError::Internal(
            "max_tokens must be >= 1 (DD-W28)".into(),
        ));
    }

    let (api_url, token, node_id) = {
        let cfg = config.read().await;
        let auth_r = auth.read().await;
        let token = auth_r
            .api_token
            .clone()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                RequesterError::AuthFailed("no api_token on AuthState".into())
            })?;
        let nid = auth_r.node_id.clone().filter(|s| !s.is_empty());
        (cfg.api_url.clone(), token, nid)
    };

    // ── Step 1: /match
    //
    // When the operator owns >1 node Wire requires `requester_node_id`
    // in the body (400 `multiple_nodes_require_explicit_node_id`
    // otherwise). Mirror of `create_offer`'s unconditional-send-when-
    // known strategy: no behavioral difference in the single-node path,
    // belt + suspenders against operators adding more nodes later.
    let match_resp = call_match(&api_url, &token, &req, node_id.as_deref()).await?;

    // ── Step 2: /fill
    let request_id = match_resp.request_id.clone();
    let fill_resp = call_fill(&api_url, &token, &req, &match_resp.job_id, &request_id).await?;

    // ── Step 2.5: resolve UUID job_id for the PendingJobs key.
    //
    // The inbound `/v1/compute/job-result` handler receives the UUID
    // `job_id` in the envelope body (contract §10.5 — dispatch body
    // stays UUID for Pillar-J7 privacy). Wire's `/jobs/:job_id` poll
    // returns the UUID alongside the handle-path; we capture it here
    // with a single extra round-trip so we can register PendingJobs
    // by UUID, matching what the inbound handler will look up.
    //
    // First smoke may surface a cheaper path (UUID in /match or /fill
    // response); adjust then. Starting with the extra poll keeps the
    // design obviously-correct — if Wire's /match adds UUID later, we
    // drop this round-trip.
    let uuid_job_id = resolve_uuid_from_handle(&api_url, &token, &match_resp.job_id).await?;

    // Register the pending entry BEFORE we return the handle so a
    // racing super-fast delivery push isn't dropped for a not-yet-
    // registered job_id. (Practically impossible given dispatch
    // timing, but cheap insurance.)
    let result_rx = pending_jobs.register(uuid_job_id.clone()).await;

    Ok(MarketRequestHandle {
        job_id_handle_path: match_resp.job_id,
        request_id,
        matched_rate_in_per_m: match_resp.matched_rate_in_per_m,
        matched_rate_out_per_m: match_resp.matched_rate_out_per_m,
        matched_multiplier_bps: match_resp.matched_multiplier_bps,
        reservation_fee: match_resp.reservation_fee,
        estimated_deposit: match_resp.estimated_deposit,
        queue_position: match_resp.queue_position,
        provider_node_id: fill_resp.provider_node_id,
        peer_queue_depth: fill_resp.peer_queue_depth,
        deposit_charged: fill_resp.deposit_charged,
        estimated_output_tokens: fill_resp.estimated_output_tokens,
        dispatch_timeout_ms: fill_resp.dispatch_timeout_ms,
        uuid_job_id,
        result_rx,
    })
}

// ═══════════════════════════════════════════════════════════════════
// await_result — steps 3+4 of the four-beat flow
// ═══════════════════════════════════════════════════════════════════

/// Block until the oneshot fires (push arrived) or `max_wait_ms`
/// expires. On timeout, removes the own entry from PendingJobs
/// (preventing a late push from firing a dropped channel) and
/// issues one `GET /jobs/:job_id` poll to classify the timeout as
/// `DeliveryTimedOut` vs `DeliveryTombstoned`.
pub async fn await_result(
    handle: MarketRequestHandle,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    pending_jobs: &PendingJobs,
    max_wait_ms: u64,
) -> Result<MarketResult, RequesterError> {
    let timeout = Duration::from_millis(max_wait_ms);
    let uuid_key = handle.uuid_job_id.clone();
    let handle_path_key = handle.job_id_handle_path.clone();

    match tokio::time::timeout(timeout, handle.result_rx).await {
        Ok(Ok(payload)) => match payload {
            DeliveryPayload::Success {
                content,
                input_tokens,
                output_tokens,
                model_used,
                latency_ms,
                finish_reason,
            } => Ok(MarketResult {
                content,
                input_tokens,
                output_tokens,
                model_used,
                latency_ms,
                finish_reason,
            }),
            DeliveryPayload::Failure { code, message } => {
                Err(RequesterError::ProviderFailed { code, message })
            }
        },
        Ok(Err(_)) => {
            // oneshot sender dropped without a send — shouldn't happen
            // in normal flow. Treat as delivery timed out; clean up
            // best-effort (entry should already be gone).
            let _ = pending_jobs.take(&uuid_key).await;
            Err(RequesterError::DeliveryTimedOut { waited_ms: 0 })
        }
        Err(_) => {
            // Timeout elapsed. Remove our entry first so a late push
            // from Wire's delivery worker hits the handler's
            // `already_settled` branch instead of firing a dropped
            // channel.
            let _ = pending_jobs.take(&uuid_key).await;

            // Classify: is this a soft timeout (still in flight on
            // Wire side) or a tombstone (Wire gave up)? One status
            // poll either way — status-only endpoint per contract §2.4.
            let (api_url, token) = {
                let cfg = config.read().await;
                let auth_r = auth.read().await;
                let token = auth_r
                    .api_token
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_default();
                (cfg.api_url.clone(), token)
            };
            if token.is_empty() {
                return Err(RequesterError::DeliveryTimedOut { waited_ms: max_wait_ms });
            }
            match poll_jobs_status(&api_url, &token, &handle_path_key).await {
                Ok(status) => {
                    // delivery_status ∈ {null, "pending", "delivering", "delivered", "failed", "expired_undelivered"}
                    match status.delivery_status.as_deref() {
                        Some("failed") => Err(RequesterError::DeliveryTombstoned {
                            reason: "delivery_status=failed".into(),
                        }),
                        Some("expired_undelivered") => Err(RequesterError::DeliveryTombstoned {
                            reason: "delivery_status=expired_undelivered".into(),
                        }),
                        _ => Err(RequesterError::DeliveryTimedOut {
                            waited_ms: max_wait_ms,
                        }),
                    }
                }
                Err(_) => {
                    // Can't reach Wire to classify — default to
                    // timeout (caller falls back silently).
                    Err(RequesterError::DeliveryTimedOut {
                        waited_ms: max_wait_ms,
                    })
                }
            }
        }
    }
}

/// Convenience: dispatch + await in one call.
pub async fn call_market(
    req: MarketInferenceRequest,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    pending_jobs: &PendingJobs,
    max_wait_ms: u64,
) -> Result<MarketResult, RequesterError> {
    let handle = dispatch_market(req, auth, config, pending_jobs).await?;
    await_result(handle, auth, config, pending_jobs, max_wait_ms).await
}

// ═══════════════════════════════════════════════════════════════════
// Low-level HTTP helpers
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct MatchResponse {
    job_id: String, // handle-path
    request_id: String,
    matched_rate_in_per_m: i64,
    matched_rate_out_per_m: i64,
    matched_multiplier_bps: i64,
    reservation_fee: i64,
    estimated_deposit: i64,
    queue_position: u64,
}

async fn call_match(
    api_url: &str,
    token: &str,
    req: &MarketInferenceRequest,
    requester_node_id: Option<&str>,
) -> Result<MatchResponse, RequesterError> {
    let mut body = serde_json::json!({
        "model_id": req.model_id,
        "max_budget": req.max_budget,
        "input_tokens": req.input_tokens,
        "latency_preference": req.latency_preference.as_wire_str(),
    });
    if let Some(nid) = requester_node_id {
        body["requester_node_id"] = serde_json::Value::String(nid.to_string());
    }
    let result = send_api_request(
        api_url,
        "POST",
        "/api/v1/compute/match",
        token,
        Some(&body),
        None,
    )
    .await;
    match result {
        Ok((_status, resp)) => serde_json::from_value(resp.clone()).map_err(|e| {
            RequesterError::MatchFailed {
                status: 200,
                body: format!("response parse: {e}: {resp}"),
            }
        }),
        Err(e) => Err(classify_match_error(e)),
    }
}

#[derive(Debug, Deserialize)]
struct FillResponse {
    #[allow(dead_code)]
    status: String,
    #[allow(dead_code)]
    job_id: String,
    provider_node_id: String,
    #[serde(default)]
    peer_queue_depth: i64,
    deposit_charged: i64,
    estimated_output_tokens: i64,
    dispatch_timeout_ms: i64,
}

async fn call_fill(
    api_url: &str,
    token: &str,
    req: &MarketInferenceRequest,
    job_id_handle: &str,
    request_id: &str,
) -> Result<FillResponse, RequesterError> {
    let body = serde_json::json!({
        "job_id": job_id_handle,
        "input_token_count": req.input_tokens,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "relay_count": 0,
        "privacy_tier": req.privacy_tier,
        "requester_callback_url": req.requester_callback_url,
        "messages": req.messages,
    });
    let mut headers = std::collections::HashMap::new();
    headers.insert("Idempotency-Key".to_string(), request_id.to_string());

    let result = send_api_request(
        api_url,
        "POST",
        "/api/v1/compute/fill",
        token,
        Some(&body),
        Some(&headers),
    )
    .await;
    match result {
        Ok((_status, resp)) => serde_json::from_value(resp.clone()).map_err(|e| {
            RequesterError::FillFailed {
                status: 200,
                body: format!("response parse: {e}: {resp}"),
            }
        }),
        Err(e) => Err(classify_fill_error(e)),
    }
}

/// Status-only poll of `/jobs/:job_id`. The handle-path is URL-encoded
/// on the way in (slashes → `%2F`).
async fn poll_jobs_status(
    api_url: &str,
    token: &str,
    job_id_handle: &str,
) -> Result<JobStatusResponse, RequesterError> {
    let path = format!(
        "/api/v1/compute/jobs/{}",
        urlencoding::encode(job_id_handle)
    );
    match send_api_request(api_url, "GET", &path, token, None, None).await {
        Ok((_, resp)) => serde_json::from_value(resp.clone())
            .map_err(|e| RequesterError::Internal(format!("jobs response parse: {e}: {resp}"))),
        Err(e) => Err(RequesterError::Internal(format!("jobs poll: {e}"))),
    }
}

/// Minimal status shape. `delivery_status` is the discriminator we
/// care about for the timeout-classification path.
#[derive(Debug, Deserialize)]
struct JobStatusResponse {
    #[allow(dead_code)]
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    delivery_status: Option<String>,
    // ... other fields (tokens, latency, role, etc.) exist on the
    // response but aren't load-bearing for this client's purposes
    // today. If we want to surface them via chronicle events later
    // they can be deserialized here with explicit fields.
}

/// One-shot lookup: given a handle-path `job_id`, return the UUID the
/// inbound push will use. Response parsing tolerates whatever field
/// name Wire uses for the UUID — smoke will lock it.
async fn resolve_uuid_from_handle(
    api_url: &str,
    token: &str,
    job_id_handle: &str,
) -> Result<String, RequesterError> {
    let path = format!(
        "/api/v1/compute/jobs/{}",
        urlencoding::encode(job_id_handle)
    );
    match send_api_request(api_url, "GET", &path, token, None, None).await {
        Ok((_, resp)) => {
            // Try common field names — whatever Wire surfaces. If none
            // match, fail loudly (RequesterError::Internal) so smoke
            // surfaces the mismatch immediately.
            for key in &["uuid_job_id", "uuid", "id", "job_uuid"] {
                if let Some(v) = resp.get(*key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        return Ok(v.to_string());
                    }
                }
            }
            Err(RequesterError::Internal(format!(
                "jobs/:job_id response did not include a UUID field (tried uuid_job_id, uuid, id, job_uuid): {resp}"
            )))
        }
        Err(e) => Err(RequesterError::Internal(format!("resolve UUID: {e}"))),
    }
}

// ═══════════════════════════════════════════════════════════════════
// Error classification helpers
// ═══════════════════════════════════════════════════════════════════

fn classify_match_error(e: String) -> RequesterError {
    // send_api_request format on !is_success: "API error {code}: {body}"
    if let Some(rest) = e.strip_prefix("API error ") {
        if let Some((code_str, body)) = rest.split_once(':') {
            if let Ok(status) = code_str.trim().parse::<u16>() {
                let body = body.trim().to_string();
                match status {
                    400 => {
                        // Caller-misconfiguration class: Wire rejected
                        // the request because something about THIS
                        // operator / node / body shape is wrong, not
                        // because of capacity. Must surface — silent
                        // fallback to pool would mask the config bug.
                        return classify_400_config_error(&body);
                    }
                    401 => return RequesterError::AuthFailed(body),
                    404 => {
                        // no_offer_for_model
                        return RequesterError::NoMatch { detail: body };
                    }
                    409 => {
                        // Only map to InsufficientBalance if the body
                        // actually says so. `parse_balance_detail`
                        // gates on `error == "insufficient_balance"`
                        // so unrelated 409s (e.g. `job_already_filled`)
                        // don't silently masquerade as "have 0, need 0".
                        // That masquerade is exactly how the market
                        // stayed broken for weeks.
                        match parse_balance_detail(&body) {
                            Some((need, have)) => {
                                return RequesterError::InsufficientBalance { need, have };
                            }
                            None => {
                                return RequesterError::MatchFailed { status, body };
                            }
                        }
                    }
                    _ => {
                        return RequesterError::MatchFailed { status, body };
                    }
                }
            }
        }
    }
    RequesterError::Internal(e)
}

fn classify_fill_error(e: String) -> RequesterError {
    if let Some(rest) = e.strip_prefix("API error ") {
        if let Some((code_str, body)) = rest.split_once(':') {
            if let Ok(status) = code_str.trim().parse::<u16>() {
                let body = body.trim().to_string();
                match status {
                    400 => {
                        return classify_400_config_error(&body);
                    }
                    401 => return RequesterError::AuthFailed(body),
                    503 => {
                        // `X-Wire-Reason` header isn't surfaced by
                        // send_api_request; Wire's body includes an
                        // `error` field carrying the reason slug
                        // (contract §2.2). Extract best-effort.
                        let reason = extract_error_field(&body);
                        return RequesterError::FillRejected {
                            status,
                            reason,
                            body,
                        };
                    }
                    _ => {
                        return RequesterError::FillFailed { status, body };
                    }
                }
            }
        }
    }
    RequesterError::Internal(e)
}

/// Parse Wire's 400-error body and classify as ConfigError. The 400
/// class covers caller-misconfiguration slugs that Wire returns when
/// the operator/node/body shape is wrong. Examples per structural-fix
/// plan §5.6: `multiple_nodes_require_explicit_node_id`,
/// `no_node_for_agent`, `invalid_body`, `privacy_field_rejected`.
///
/// Unknown `error` slugs fall through to MatchFailed so we surface
/// the raw body instead of silently classifying as config. Unknown
/// slugs are a forward-compat signal — Wire may add new 400 slugs
/// that we want to see in the operator log, not bury.
fn classify_400_config_error(body: &str) -> RequesterError {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let error_slug = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    match error_slug {
        Some(slug) if !slug.is_empty() => {
            let detail = parsed.and_then(|v| v.get("detail").cloned());
            RequesterError::ConfigError {
                error_slug: slug,
                detail,
            }
        }
        _ => RequesterError::MatchFailed {
            status: 400,
            body: body.to_string(),
        },
    }
}

/// Parse an `insufficient_balance` 409 body for `{need, have}`. Returns
/// `None` if the body isn't an insufficient-balance response — that
/// lets the caller classify non-balance 409s (e.g. `job_already_filled`)
/// as MatchFailed instead of fake-zero-balance InsufficientBalance.
///
/// Historical note: previous behavior was "best-effort parse, default
/// (0, 0) on fail." That indistinguishably collided with a real 0-credits
/// operator hitting insufficient-balance, and with any unrelated 409
/// that happened to not parse. Gating on the `error` discriminator
/// means unrelated 409s now surface correctly as MatchFailed.
fn parse_balance_detail(body: &str) -> Option<(i64, i64)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v.get("error")?.as_str()? != "insufficient_balance" {
        return None;
    }
    // Shared-types shape: `detail: {need, have}`. Legacy fallback:
    // top-level need/have for older Wire responses pre-shared-types.
    let need = v
        .pointer("/detail/need")
        .and_then(|n| n.as_i64())
        .or_else(|| v.get("need").and_then(|n| n.as_i64()))?;
    let have = v
        .pointer("/detail/have")
        .and_then(|n| n.as_i64())
        .or_else(|| v.get("have").and_then(|n| n.as_i64()))?;
    Some((need, have))
}

fn extract_error_field(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_match_error_404_is_no_match() {
        let err = classify_match_error(
            "API error 404: {\"error\":\"no_offer_for_model\",\"detail\":\"...\"}".into(),
        );
        matches!(err, RequesterError::NoMatch { .. });
    }

    #[test]
    fn classify_match_error_409_is_insufficient_balance() {
        let err = classify_match_error(
            "API error 409: {\"error\":\"insufficient_balance\",\"detail\":{\"need\":5000,\"have\":1200}}"
                .into(),
        );
        match err {
            RequesterError::InsufficientBalance { need, have } => {
                assert_eq!(need, 5000);
                assert_eq!(have, 1200);
            }
            other => panic!("expected InsufficientBalance, got {other:?}"),
        }
    }

    #[test]
    fn classify_match_error_401_is_auth_failed() {
        let err = classify_match_error("API error 401: {\"error\":\"no_operator_bound\"}".into());
        matches!(err, RequesterError::AuthFailed(_));
    }

    #[test]
    fn classify_fill_error_503_captures_reason() {
        let err = classify_fill_error(
            "API error 503: {\"error\":\"queue_depth_exceeded\",\"model\":\"llama3\"}".into(),
        );
        match err {
            RequesterError::FillRejected { status, reason, .. } => {
                assert_eq!(status, 503);
                assert_eq!(reason, "queue_depth_exceeded");
            }
            other => panic!("expected FillRejected, got {other:?}"),
        }
    }

    #[test]
    fn classify_fill_error_503_unknown_reason_falls_through_to_unknown() {
        let err = classify_fill_error("API error 503: not-json-body".into());
        match err {
            RequesterError::FillRejected { reason, .. } => assert_eq!(reason, "unknown"),
            other => panic!("expected FillRejected unknown, got {other:?}"),
        }
    }

    #[test]
    fn classify_fill_error_400_is_config_error() {
        // Post structural-fix: 400s carry a typed error slug (e.g.
        // `multiple_system_turns`) and must surface as ConfigError,
        // not FillFailed — the caller's cascade branch treats
        // ConfigError as hard-fail (no silent fallback to pool).
        let err = classify_fill_error(
            "API error 400: {\"error\":\"multiple_system_turns\"}".into(),
        );
        match err {
            RequesterError::ConfigError { error_slug, .. } => {
                assert_eq!(error_slug, "multiple_system_turns");
            }
            other => panic!("expected ConfigError, got {other:?}"),
        }
    }

    #[test]
    fn classify_match_error_400_is_config_error() {
        let err = classify_match_error(
            "API error 400: {\"error\":\"multiple_nodes_require_explicit_node_id\",\"detail\":{\"owned_node_count\":6}}"
                .into(),
        );
        match err {
            RequesterError::ConfigError { error_slug, detail } => {
                assert_eq!(error_slug, "multiple_nodes_require_explicit_node_id");
                assert!(detail.is_some());
            }
            other => panic!("expected ConfigError, got {other:?}"),
        }
    }

    #[test]
    fn classify_match_error_400_without_slug_falls_to_match_failed() {
        // Unknown/missing error slug on a 400 — surface as MatchFailed
        // with raw body so the operator log shows what Wire actually
        // returned instead of swallowing into generic ConfigError.
        let err = classify_match_error("API error 400: not-json-body".into());
        match err {
            RequesterError::MatchFailed { status: 400, .. } => {}
            other => panic!("expected MatchFailed 400, got {other:?}"),
        }
    }

    #[test]
    fn classify_match_error_409_non_balance_is_match_failed() {
        // Historical bug: any 409 was classified as
        // `InsufficientBalance { need: 0, have: 0 }` via the old
        // default-parse behavior. That masked real bugs. A 409 whose
        // error slug is NOT `insufficient_balance` must now fall
        // through to MatchFailed with the real body.
        let err = classify_match_error(
            "API error 409: {\"error\":\"job_already_filled\",\"detail\":{}}".into(),
        );
        match err {
            RequesterError::MatchFailed { status: 409, ref body } => {
                assert!(body.contains("job_already_filled"));
            }
            other => panic!("expected MatchFailed 409, got {other:?}"),
        }
    }

    #[test]
    fn parse_balance_detail_returns_none_on_non_balance_body() {
        let out = parse_balance_detail("{\"error\":\"insufficient_credit_reserve\"}");
        assert!(out.is_none(), "non-insufficient_balance body must return None");
    }

    #[test]
    fn parse_balance_detail_returns_none_on_malformed_body() {
        assert!(parse_balance_detail("not-json").is_none());
        assert!(parse_balance_detail("{}").is_none());
    }

    #[test]
    fn parse_balance_detail_returns_some_on_valid_body() {
        let out = parse_balance_detail(
            "{\"error\":\"insufficient_balance\",\"detail\":{\"need\":5000,\"have\":1200}}",
        );
        assert_eq!(out, Some((5000, 1200)));
    }

    #[test]
    fn parse_balance_detail_legacy_top_level_shape() {
        // Older Wire responses (pre-shared-types) put need/have at the
        // top level instead of under detail. Legacy fallback path.
        let out = parse_balance_detail(
            "{\"error\":\"insufficient_balance\",\"need\":500,\"have\":100}",
        );
        assert_eq!(out, Some((500, 100)));
    }

    #[test]
    fn classify_non_api_error_is_internal() {
        let err = classify_match_error("reqwest: connection refused".into());
        matches!(err, RequesterError::Internal(_));
    }
}
