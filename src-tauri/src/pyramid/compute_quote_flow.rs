//! Rev 2.1 three-RPC compute-market client: `/quote` → `/purchase` → `/fill`.
//!
//! Walker's market branch (plan §4.2) invokes these four public entry points
//! back-to-back. This module replaced the rev-2.0 `dispatch_market` /
//! `call_market` / `call_match` / `call_fill` / `resolve_uuid_from_handle`
//! surface (the `compute_requester` module was deleted in Wave 5).
//!
//! Wave 3b status: LIVE. `quote`, `purchase`, `fill`, `register_pending`,
//! and `await_result` are all wired into the walker's market branch
//! (plan §4.2). The legacy Phase B pre-loop is gone.
//! `register_pending` must be called BEFORE `fill` so the provider
//! callback cannot race the registration (Wave 3a friction-log RACE-1).
//!
//! # Rev 2.1 UUID resolution — deliberately no `resolve_uuid_from_purchase`
//!
//! Per bilateral contract §1.6b, `/purchase` 200 returns
//! `{ job_id, uuid_job_id, request_id, dispatch_deadline_at }`. Both
//! `request_id` and `uuid_job_id` are surfaced directly — the walker reads
//! `purchase_resp.uuid_job_id` for the [`PendingJobs`] key (the inbound
//! `/v1/compute/job-result` envelope carries the DB-row UUID) and
//! `purchase_resp.request_id` for the `/fill` body's idempotency token.
//!
//! The rev-2.0 `resolve_uuid_from_handle` helper that issued a follow-up
//! `GET /api/v1/compute/jobs/:handle_path` to recover the UUID is **dead**
//! in rev 2.1. Plan §8 Wave 0 task 8 explicitly forbids reintroducing it.
//!
//! # Error classification
//!
//! Every failure is mapped to one of the three [`EntryError`] tiers by
//! [`classify_rev21_slug`]. Walker semantic per plan §2.5.3 + §4.2:
//!
//! | Tier | Walker response |
//! |------|-----------------|
//! | `Retryable`    | advance to next entry; `network_route_retryable_fail` |
//! | `RouteSkipped` | advance to next entry; `network_route_skipped` |
//! | `CallTerminal` | bubble to caller; `network_route_terminal_fail` + `fail_audit` |

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::AuthState;
use crate::http_utils::{send_api_request, send_api_request_with_hints, ApiErrorWithHints};
use crate::pyramid::llm::{EntryError, LlmResponse};
use crate::pyramid::pending_jobs::{DeliveryPayload, PendingJobs};
use crate::pyramid::types::TokenUsage;
use crate::WireNodeConfig;

// Re-export the rev 2.1 body/response types from the contracts crate so
// callers can `use crate::pyramid::compute_quote_flow::{ComputeQuoteBody, ...}`
// without caring whether the shape lives upstream or locally. If a type
// ever drifts from the contracts crate, change the re-export to a local
// struct here — no call-site churn.
pub use agent_wire_contracts::{
    ComputePurchaseBody, ComputePurchaseResponse, ComputePurchaseTrigger, ComputeQuoteBody,
    ComputeQuotePriceBreakdown, ComputeQuoteResponse, LatencyPreference,
};

// ---------------------------------------------------------------------------
// ComputeFillBody — declared locally; the contracts crate (rev a9e356d3)
// has not yet exported a `ComputeFillBody` type. Shape confirmed by
// Wire-dev's Q4 answer and spec §1.8 of
// `compute-market-quote-primitive-spec-2026-04-20.md`:
//
//   - `job_id`: handle-path from `/purchase` response.
//   - `request_id`: UUID from `/purchase.request_id` (stable across offer
//     supersession; also serves as the idempotency reference).
//   - `messages`: ChatML array (validated server-side).
//   - `max_tokens`: OPTIONAL in rev 2.1 (§2.3). When absent, Wire uses the
//     `max_tokens_quoted` claim persisted at `/purchase` time. When
//     present and `> max_tokens_quoted`, Wire 400s with
//     `max_tokens_exceeds_quote`.
//   - `temperature`: f32 in 0.0..=2.0.
//   - `relay_count`: integer, default 0 (direct tunnel).
//   - `privacy_tier`: `"bootstrap-relay" | "direct"`.
//   - `input_token_count`: i64. Pre-counted by caller.
//   - `requester_callback_url`: HTTPS URL on this node's tunnel.
//   - `idempotency_key`: body-level idempotency token; sent ALSO as the
//     `Idempotency-Key` HTTP header.
//
// The Wire side runs a strict-allowed-field check on the `/fill` body —
// any extra top-level field 400s. Keep this struct minimal. When / if
// the contracts crate publishes a `ComputeFillBody` upstream, swap the
// local struct for a `pub use agent_wire_contracts::ComputeFillBody;`.
// ---------------------------------------------------------------------------

/// `/api/v1/compute/fill` request body (rev 2.1) — STRICT wire schema.
///
/// Wire's validator at `src/app/api/v1/compute/fill/route.ts:193-198`
/// whitelists exactly these 8 field names and 400s with `unknown_field`
/// on any extra top-level key. This struct is the wire-canonical projection:
/// nothing on it that isn't explicitly allowed by Wire.
///
/// Walker-side state (request_id for PendingJobs keying, idempotency_key
/// for the HTTP header) lives on [`ComputeFillRequest`], which wraps this
/// body. Callers construct a request; `fill()` serializes `request.body`
/// for the HTTP body and reads `request.idempotency_key` into the header.
///
/// Declared locally because the `agent-wire-contracts` rev pinned in
/// `Cargo.toml` (a9e356d3) does not yet export this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeFillBody {
    pub job_id: String,
    pub messages: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    pub temperature: f32,
    pub relay_count: i64,
    pub privacy_tier: String,
    pub input_token_count: i64,
    pub requester_callback_url: String,
}

/// Detail payload carried on `all_offers_saturated_for_model` (P0410, 409)
/// responses from Wire's `plan_compute_match` (rev 2.1.1). Shape MATCHES
/// `agent_wire_contracts::AllOffersSaturatedDetail` verbatim — declared
/// locally because the contracts crate rev pinned in `Cargo.toml`
/// (a9e356d3) was cut before the rev 2.1.1 type landed. Swap to
/// `pub use agent_wire_contracts::AllOffersSaturatedDetail;` as soon
/// as the contracts crate rev bumps (same pattern as ComputeFillBody).
///
/// Field semantics (per bilateral decision doc
/// compute-market-saturation-decisions-2026-04-21.md):
/// - `offer_count`: size of the saturated cohort.
/// - `min_current_queue_depth`: shortest queue in the cohort.
/// - `max_queue_depth_across_offers`: largest max_queue_depth in the
///   cohort (informational ceiling on waiting-room).
/// - `min_expected_drain_ms`: Wire-computed `min(typical_serve_ms_p50)`
///   across the cohort — shortest head-of-queue completion time.
///   Walker uses as backoff FLOOR. Option-typed: `None` when the cohort
///   lacks observations (fresh offers, <10 settled jobs). Walker falls
///   back to a policy-local default in that case.
/// - `median_typical_serve_ms_p50`: cohort median serve time for
///   walker's own horizon math. Option-typed for the same reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllOffersSaturatedDetail {
    pub model_id: String,
    pub offer_count: i64,
    pub min_current_queue_depth: i64,
    pub max_queue_depth_across_offers: i64,
    pub min_expected_drain_ms: Option<f64>,
    pub median_typical_serve_ms_p50: Option<f64>,
}

/// Walker-side bundling of the wire body plus the two fields the walker
/// needs but Wire does NOT accept in the body:
///
/// - `request_id`: PendingJobs keying + dispatch tracking. Wire reads this
///   from the reserved job record (state from `/purchase`), not the body.
/// - `idempotency_key`: sent as the `Idempotency-Key` HTTP header by
///   [`fill()`]; never serialized into the body.
///
/// Keeping these on a wrapper (rather than on [`ComputeFillBody`]) is
/// load-bearing: the body struct must strictly mirror Wire's allowed-field
/// whitelist or `/fill` returns 400 `unknown_field`.
#[derive(Debug, Clone)]
pub struct ComputeFillRequest {
    pub body: ComputeFillBody,
    pub request_id: String,
    pub idempotency_key: String,
}

// ---------------------------------------------------------------------------
// Public stubs — bodies in Wave 3.
// ---------------------------------------------------------------------------

/// POST `/api/v1/compute/quote`. Returns a signed quote JWT + price
/// breakdown. Stateless price query — no idempotency header.
#[allow(dead_code)]
pub async fn quote(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    body: ComputeQuoteBody,
) -> Result<ComputeQuoteResponse, ApiErrorWithHints> {
    let (api_url, token) = read_api_creds_raw(auth, config, "quote").await?;
    let body_json = serde_json::to_value(&body).map_err(|e| ApiErrorWithHints {
        status: 0,
        body: serde_json::json!({ "error": "quote_body_serialize", "detail": e.to_string() }),
        hints: Default::default(),
    })?;
    let (_status, resp) = send_api_request_with_hints(
        &api_url,
        "POST",
        "/api/v1/compute/quote",
        &token,
        Some(&body_json),
        None,
    )
    .await?;
    serde_json::from_value(resp.clone()).map_err(|e| ApiErrorWithHints {
        status: 0,
        body: serde_json::json!({
            "error": "quote_response_parse",
            "detail": e.to_string(),
            "raw": resp,
        }),
        hints: Default::default(),
    })
}

/// POST `/api/v1/compute/purchase`. Commits a `quote_jwt` into a reserved
/// job, returning the DB-row `uuid_job_id` (used as the [`PendingJobs`]
/// key) and a stable `request_id` (used as the `/fill` idempotency token).
/// Body in Wave 3.
///
/// `quote_jwt` is passed separately from `body` for call-site clarity —
/// plan §4.2 shows the walker pulling it off `ComputeQuoteResponse` and
/// inserting it into [`ComputePurchaseBody`] alongside a fresh idempotency
/// UUID. The parameter matches that intent.
#[allow(dead_code)]
pub async fn purchase(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    quote_jwt: &str,
    body: ComputePurchaseBody,
) -> Result<ComputePurchaseResponse, ApiErrorWithHints> {
    let (api_url, token) = read_api_creds_raw(auth, config, "purchase").await?;

    // Honor the `quote_jwt` param as authoritative (prompt) — overwrite
    // whatever the caller placed in `body.quote_jwt` so the param is the
    // source of truth at this layer. Also ensure `idempotency_key` is
    // populated: prompt + plan §4.2 both mint a fresh UUID per call.
    let mut body = body;
    body.quote_jwt = quote_jwt.to_string();
    if body.idempotency_key.is_none() {
        body.idempotency_key = Some(uuid::Uuid::new_v4().to_string());
    }
    let idem_key = body
        .idempotency_key
        .clone()
        .expect("idempotency_key set just above");

    let body_json = serde_json::to_value(&body).map_err(|e| ApiErrorWithHints {
        status: 0,
        body: serde_json::json!({ "error": "purchase_body_serialize", "detail": e.to_string() }),
        hints: Default::default(),
    })?;

    // HTTP header: spec §2.2 keeps `idempotency_key` as a body field; we
    // mirror it into the `Idempotency-Key` header as well so ops tooling
    // + Wire middleware can key on either. Wire replay matching at
    // launch is keyed on `(operator_id, idempotency_key)` per spec.
    let mut headers = HashMap::new();
    headers.insert("Idempotency-Key".to_string(), idem_key);

    let (_status, resp) = send_api_request_with_hints(
        &api_url,
        "POST",
        "/api/v1/compute/purchase",
        &token,
        Some(&body_json),
        Some(&headers),
    )
    .await?;
    // Idempotent-replay with matching key: Wire returns cached 200
    // (spec §2.2). That path hits Ok here. The error path is classified
    // by the caller (dispatch_market_entry) via classify_wire_error — it
    // also owns saturation-backoff peek logic for `all_offers_saturated_for_model`.
    serde_json::from_value(resp.clone()).map_err(|e| ApiErrorWithHints {
        status: 0,
        body: serde_json::json!({
            "error": "purchase_response_parse",
            "detail": e.to_string(),
            "raw": resp,
        }),
        hints: Default::default(),
    })
}

/// POST `/api/v1/compute/fill`. Dispatches the ChatML messages + callback
/// URL to Wire, which forwards to the matched provider. Body in Wave 3.
///
/// Wire's strict-allowed-field check means every field on
/// [`ComputeFillBody`] is required (modulo the one `#[serde(skip_...)]`
/// on `max_tokens`); extras 400.
#[allow(dead_code)]
pub async fn fill(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    request: ComputeFillRequest,
) -> Result<(), ApiErrorWithHints> {
    let (api_url, token) = read_api_creds_raw(auth, config, "fill").await?;

    // `/fill` idempotency is sent ONLY as the `Idempotency-Key` HTTP header.
    // Wire's allowed-field whitelist rejects an `idempotency_key` key in the
    // body — hence the split between `ComputeFillRequest` (walker-side
    // bundle) and `ComputeFillBody` (wire-canonical body). A retry with
    // the same idempotency key lands on the existing dispatch record.
    let mut headers = HashMap::new();
    headers.insert(
        "Idempotency-Key".to_string(),
        request.idempotency_key.clone(),
    );

    let body_json = serde_json::to_value(&request.body).map_err(|e| ApiErrorWithHints {
        status: 0,
        body: serde_json::json!({ "error": "fill_body_serialize", "detail": e.to_string() }),
        hints: Default::default(),
    })?;

    match send_api_request_with_hints(
        &api_url,
        "POST",
        "/api/v1/compute/fill",
        &token,
        Some(&body_json),
        Some(&headers),
    )
    .await
    {
        Ok((_status, _resp)) => Ok(()),
        Err(api_err) => {
            // Special-case 409 `fill_already_submitted` (idempotency
            // replay — provider already accepted an earlier /fill with
            // the same request_id). Provider will deliver the result via
            // the existing pending-job oneshot; treat as Ok.
            if extract_error_slug_from_body(&api_err.body).as_deref()
                == Some("fill_already_submitted")
            {
                return Ok(());
            }
            Err(api_err)
        }
    }
}

/// Register a pending oneshot keyed on `uuid_job_id` BEFORE calling
/// [`fill`].
///
/// Walker market branch (plan §4.2) must register the receiver before
/// `/fill` POSTs so an instant provider callback cannot race ahead of
/// registration and land on an empty PendingJobs map. The returned
/// receiver is then handed to [`await_result`] after `/fill` returns.
///
/// The prior shape — `await_result` registering internally — had the
/// registration happening AFTER `/fill`, opening a TOCTOU window
/// bounded only by provider inference latency. Wave 3b race-fix splits
/// the two so `register_pending → fill → await_result(rx, ...)` closes
/// the window at the call site.
pub async fn register_pending(
    pending_jobs: &PendingJobs,
    uuid_job_id: &str,
) -> tokio::sync::oneshot::Receiver<DeliveryPayload> {
    pending_jobs.register(uuid_job_id.to_string()).await
}

/// Await the inbound `/v1/compute/job-result` envelope keyed by
/// `uuid_job_id` (the DB-row UUID surfaced on `ComputePurchaseResponse`).
///
/// Wave 3b race-fix: `rx` is now passed in by the caller, registered
/// via [`register_pending`] BEFORE the `/fill` POST. On timeout this
/// function still calls `pending_jobs.take(uuid_job_id)` to drop the
/// sender so a late push sees `already_settled` instead of firing a
/// dropped channel.
#[allow(dead_code)]
pub async fn await_result(
    rx: tokio::sync::oneshot::Receiver<DeliveryPayload>,
    uuid_job_id: &str,
    pending_jobs: &PendingJobs,
    timeout: Duration,
) -> Result<LlmResponse, EntryError> {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(payload)) => match payload {
            DeliveryPayload::Success {
                content,
                input_tokens,
                output_tokens,
                model_used,
                latency_ms: _,
                finish_reason: _,
            } => Ok(LlmResponse {
                content,
                usage: TokenUsage {
                    prompt_tokens: input_tokens,
                    completion_tokens: output_tokens,
                },
                generation_id: None,
                actual_cost_usd: None,
                provider_id: Some(format!("market:{}", model_used)),
                fleet_peer_id: None,
                fleet_peer_model: None,
            }),
            DeliveryPayload::Failure { code, message } => {
                // Provider's inference failed. Other routes may succeed
                // on the same walker pass — advance.
                Err(EntryError::RouteSkipped {
                    reason: format!("provider_returned_error:{code}:{message}"),
                })
            }
        },
        Ok(Err(_recv_err)) => {
            // Sender dropped without sending — shouldn't happen in the
            // normal flow. Best-effort cleanup + transient retry.
            let _ = pending_jobs.take(uuid_job_id).await;
            Err(EntryError::Retryable {
                reason: "fill_result_channel_closed".into(),
            })
        }
        Err(_elapsed) => {
            // Timeout elapsed — drop our PendingJobs entry so a late
            // delivery push hits `already_settled` at the inbound
            // handler instead of firing a dropped channel.
            let _ = pending_jobs.take(uuid_job_id).await;
            Err(EntryError::Retryable {
                reason: "fill_result_timeout".into(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers (shared across the four RPC bodies).
// ---------------------------------------------------------------------------

/// Read `(api_url, api_token)` out of the shared auth + config state.
/// Missing token → `CallTerminal` with a stage-tagged reason so callers
/// can differentiate `quote_auth_failed` vs `purchase_auth_failed` vs
/// `fill_auth_failed` in telemetry.
async fn read_api_creds(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    stage: &str,
) -> Result<(String, String), EntryError> {
    let cfg = config.read().await;
    let auth_r = auth.read().await;
    let token = auth_r
        .api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| EntryError::CallTerminal {
            reason: format!("{stage}_auth_failed:no_api_token"),
        })?;
    Ok((cfg.api_url.clone(), token))
}

/// ApiErrorWithHints variant of `read_api_creds` — used by the three
/// rev 2.1 RPC functions (quote, purchase, fill) which return
/// `Result<_, ApiErrorWithHints>` so `dispatch_market_entry` can classify
/// AND peek at the raw body for structured detail (saturation-retry
/// backoff inputs). Semantics identical to `read_api_creds`; only the
/// Err shape differs.
async fn read_api_creds_raw(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    stage: &str,
) -> Result<(String, String), ApiErrorWithHints> {
    let cfg = config.read().await;
    let auth_r = auth.read().await;
    let token = auth_r
        .api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiErrorWithHints {
            status: 401,
            body: serde_json::json!({
                "error": format!("{stage}_auth_failed"),
                "detail": "no_api_token"
            }),
            hints: Default::default(),
        })?;
    Ok((cfg.api_url.clone(), token))
}

/// Extract the `error` slug from a parsed JSON body (ApiErrorWithHints.body
/// shape). Returns None when the body doesn't carry `error` as a
/// non-empty string.
fn extract_error_slug_from_body(body: &serde_json::Value) -> Option<String> {
    body.get("error")
        .and_then(|e| e.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Classify an `ApiErrorWithHints` from a Wire compute RPC call.
///
/// Consults `X-Wire-Retry` header FIRST, per Wire's WS4-commented
/// contract (compute-errors.ts top-of-file): node's classify_retry reads
/// the header with precedence over HTTP code / slug heuristics. Falls
/// through to slug-based classification (`classify_rev21_slug`) only
/// when the header is absent or carries an unrecognized value.
///
/// Header semantics:
/// - `never` → CallTerminal. Wire has declared "do not retry"; walker
///   bubbles rather than burning cascade entries.
/// - `transient` → Retryable. Condition expected to clear imminently
///   (e.g., `all_offers_saturated_for_model`, FOR-UPDATE race on
///   `provider_queue_full`). Walker's saturation-retry loop (in
///   `call_model_unified`) decides backoff.
/// - `backoff` → Retryable. Same as `transient` from classification
///   perspective; the distinction is operational (Wire hinting at
///   longer-than-immediate retry). Walker's retry loop honors backoff
///   inputs from the structured detail.
/// - Absent or unrecognized → fall through to slug-based classifier.
///
/// `X-Retriable: true` (the orthogonal legacy header) is not consulted
/// directly here; Wire always pairs it with `X-Wire-Retry: transient`
/// so the intent is the same through this function.
pub(crate) fn classify_wire_error(err: &ApiErrorWithHints, stage: &str) -> EntryError {
    // 1. Honor X-Wire-Retry header with precedence.
    if let Some(hint) = err.hints.x_wire_retry.as_deref() {
        match hint {
            "never" => {
                // Wire said "do not retry" — CallTerminal.
                // Preserve slug in the reason for telemetry.
                let slug =
                    extract_error_slug_from_body(&err.body).unwrap_or_else(|| stage.to_string());
                return EntryError::CallTerminal { reason: slug };
            }
            "transient" | "backoff" => {
                // Retryable — walker's entry-level retry loop decides
                // how to honor it. Preserve slug so the loop can match
                // on `all_offers_saturated_for_model` specifically and
                // parse the structured detail for backoff inputs.
                let slug = extract_error_slug_from_body(&err.body)
                    .unwrap_or_else(|| format!("{stage}_transient"));
                return EntryError::Retryable { reason: slug };
            }
            _ => {
                // Unrecognized hint value — fall through to
                // slug/HTTP-code classification (defensive: Wire may
                // ship a new hint value before node knows about it).
            }
        }
    }

    // 2. No header hint (or unrecognized value) — fall through to the
    //    existing slug-then-HTTP-code classification ladder.
    let err_str = err.to_string();
    classify_rev21_http_error(&err_str, stage)
}

/// Parse `send_api_request`'s error string (format: `"API error {code}: {body}"`)
/// into an `EntryError` tier. Extracts the error slug from the JSON body
/// when possible and runs it through [`classify_rev21_slug`]; falls back
/// to stage-tagged tiers for transport-level failures.
fn classify_rev21_http_error(err_str: &str, stage: &str) -> EntryError {
    // send_api_request format on !is_success: "API error {code}: {body}"
    if let Some(rest) = err_str.strip_prefix("API error ") {
        if let Some((code_str, body)) = rest.split_once(':') {
            if let Ok(status) = code_str.trim().parse::<u16>() {
                let body = body.trim();
                // 401 without a JSON slug is a bare auth failure — map
                // per-stage per the prompt's table.
                //
                //   /quote   401 → CallTerminal(quote_auth_failed)   (prompt)
                //   /purchase 401 generic → RouteSkipped             (plan §4.2)
                //   /fill    401 → CallTerminal(fill_auth_failed)    (prompt)
                //
                // When the body carries a named slug, slug classification
                // wins (e.g. 401 `quote_jwt_expired` → RouteSkipped).
                let slug = extract_error_slug(body);
                if let Some(slug) = slug {
                    return classify_rev21_slug(&slug);
                }
                return match (status, stage) {
                    (401, "quote") => EntryError::CallTerminal {
                        reason: "quote_auth_failed".into(),
                    },
                    (401, "purchase") => EntryError::RouteSkipped {
                        reason: "purchase_auth_failed".into(),
                    },
                    (401, "fill") => EntryError::CallTerminal {
                        reason: "fill_auth_failed".into(),
                    },
                    (400, _) => EntryError::CallTerminal {
                        reason: format!("{stage}_bad_request"),
                    },
                    (403, _) => EntryError::CallTerminal {
                        reason: format!("{stage}_forbidden"),
                    },
                    (404, _) => EntryError::RouteSkipped {
                        reason: format!("{stage}_not_found"),
                    },
                    (503, _) => EntryError::RouteSkipped {
                        reason: format!("{stage}_service_unavailable"),
                    },
                    _ => EntryError::RouteSkipped {
                        reason: format!("{stage}_http_{status}"),
                    },
                };
            }
        }
    }
    // Non-"API error …" prefix → transport / serde / io failure. Retryable.
    EntryError::Retryable {
        reason: format!("{stage}_network:{err_str}"),
    }
}

/// Pull the `error` field out of an "API error {code}: {body}" string
/// when the body is JSON. Returns None for non-JSON bodies.
fn extract_slug_from_http_error(err_str: &str) -> Option<String> {
    let rest = err_str.strip_prefix("API error ")?;
    let (_code, body) = rest.split_once(':')?;
    extract_error_slug(body.trim())
}

/// Parse a response body as JSON and return `body.error` as a String.
fn extract_error_slug(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")
        .and_then(|e| e.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Error-slug classification (plan §4.2 table).
// ---------------------------------------------------------------------------

/// Map a Wire-returned rev 2.1 error slug to an [`EntryError`] tier.
///
/// Walker (Wave 3) consumes this from each of the three RPC error paths.
/// Unknown slugs fall through to `RouteSkipped` — conservative default
/// (advance rather than bubble) so an unexpected Wire error doesn't doom
/// the whole chain. Known-terminal slugs are explicitly listed below with
/// rationale in the doc-comment on each arm.
///
/// The `reason` string on the returned variant is the slug itself —
/// Wave 3's `dispatch_market_entry` can enrich it with `{need, have}` /
/// `{requested, quoted}` / etc. when the response body carries those
/// fields. Skeleton-only mapping here.
#[allow(dead_code)]
fn classify_rev21_slug(slug: &str) -> EntryError {
    let reason = slug.to_string();
    match slug {
        // ── /quote error slugs (plan §4.2 + spec §2.1 error matrix) ───────
        //
        // No matching offer for the model — market cannot serve this
        // call, but other routes (fleet, pool) may. Advance.
        "no_offer_for_model" => EntryError::RouteSkipped { reason },
        // Estimated total exceeds the per-entry `max_budget_credits`
        // ceiling. Market too expensive for this entry; advance.
        "budget_exceeded" => EntryError::RouteSkipped { reason },
        // Requester's Wire balance is below the reservation + worst-case
        // deposit. Fleet is free + openrouter bills separately, so other
        // routes may still serve. Advance.
        "insufficient_balance" => EntryError::RouteSkipped { reason },
        // Wire-side platform outage. Transient from the market's
        // perspective, but node walker v1 advances rather than sleeping —
        // other routes may serve without Wire. RouteSkipped per prompt.
        "platform_unavailable" => EntryError::RouteSkipped { reason },
        // Wire-operator-level config bug: a named economic_parameter
        // Wire needs is missing. All market dispatches will 503 with the
        // same slug until the operator fixes it. Bubble rather than
        // silently round-robin through it on every walker pass.
        "economic_parameter_missing" => EntryError::CallTerminal { reason },
        // Walker built a malformed body — same bug would fire on every
        // route that routes through Wire. Bubble.
        "invalid_body" => EntryError::CallTerminal { reason },
        // New slug (rev 2.1.1): market has offers for this model but ALL
        // are at/above their `max_queue_depth`. Wire pairs with
        // X-Wire-Retry: transient so classify_wire_error will normally
        // route this through the header path. This arm covers the
        // slug-only path (e.g., future proxies that strip headers) and
        // the fall-through case. Retryable — walker's saturation-retry
        // loop in call_model_unified reads the structured detail (via
        // AllOffersSaturatedDetail) for backoff inputs. Distinct from
        // `no_offer_for_model` (404, X-Wire-Retry: never, CallTerminal):
        // saturation means busy, absence means unavailable.
        "all_offers_saturated_for_model" => EntryError::Retryable { reason },
        // Wire-identity-binding errors are MARKET-SPECIFIC: fleet / openrouter /
        // ollama-local dispatches never touch Wire's node or agent registration,
        // so a 400 on requester_node_id or agent binding doesn't doom them.
        // Same reasoning as `unauthorized` below — advance, let other routes
        // serve, operator telemetry carries the raw slug for diagnosis.
        //
        // (The real fix is walker always injecting `requester_node_id` into
        // `/quote` bodies; tracked as a chip. Until then, RouteSkipped keeps
        // the cascade productive instead of killing builds on multi-node
        // operator accounts.)
        "multiple_nodes_require_explicit_node_id" => EntryError::RouteSkipped { reason },
        "no_node_for_agent" => EntryError::RouteSkipped { reason },
        // Operator consent not granted. No alternate route will satisfy
        // Wire until operator fixes the agent binding. Bubble.
        "agent_unconfirmed" => EntryError::CallTerminal { reason },
        // Wire returned 401 with this slug explicitly (bare auth failure
        // on the token). Walker advances — fleet + openrouter use
        // separate credentials, so Wire 401 doesn't doom them.
        "unauthorized" => EntryError::RouteSkipped { reason },

        // ── /purchase error slugs (spec §2.2 error matrix) ────────────────
        //
        // Quote lost the winning-offer race between /quote and /purchase.
        // Walker v1 does NOT re-quote same entry — advance. (Plan §4.2
        // tags this Retryable but prompt + walker v1 semantics say
        // RouteSkipped; we advance rather than sleep-and-retry.)
        "quote_no_longer_winning" => EntryError::RouteSkipped { reason },
        // Idempotent-replay mismatch — different walker attempt already
        // purchased. Hand the work back for fresh route selection.
        // (Matching-idempotency-key replay is handled as cached-200 at
        // the HTTP-response layer, not here.)
        "quote_already_purchased" => EntryError::RouteSkipped { reason },
        // Quote JWT expired between mint and /purchase. v1 advances.
        "quote_jwt_expired" => EntryError::RouteSkipped { reason },
        // Quote JWT malformed — walker built a bad body. Bubble.
        "quote_jwt_invalid" => EntryError::CallTerminal { reason },
        // JWT `rid` ≠ authed operator — caller-config bug affecting every
        // market dispatch from this node until resolved. Bubble.
        "quote_operator_mismatch" => EntryError::CallTerminal { reason },
        // The only supported trigger at launch is `immediate`. Walker
        // passed something else — walker bug. Bubble.
        "trigger_not_supported" => EntryError::CallTerminal { reason },
        // Provider's reserved-depth cap hit. Same class as /fill
        // provider_depth_exceeded. Advance.
        "provider_queue_full" => EntryError::RouteSkipped { reason },
        // 403 operator_mismatch (generic, not tied to the JWT rid check).
        // Identity-binding bug — bubble.
        "operator_mismatch" => EntryError::CallTerminal { reason },

        // ── /fill error slugs (spec §2.3 + contract §1.8) ─────────────────
        //
        // We lost the dispatch slot (reservation expired before /fill).
        // Reservation fee already consumed at /purchase; no refund.
        // Advance.
        "dispatch_deadline_exceeded" => EntryError::RouteSkipped { reason },
        // Provider's local depth saturated. Advance; other routes may
        // serve.
        "provider_depth_exceeded" => EntryError::RouteSkipped { reason },
        "provider_dispatch_conflict" => EntryError::RouteSkipped { reason },
        // Walker passed `max_tokens > max_tokens_quoted`. Walker bug;
        // same bug would fire on every route. Bubble.
        "max_tokens_exceeds_quote" => EntryError::CallTerminal { reason },
        // ChatML validation — multiple system turns, unknown fields, etc.
        // Walker body-shape bug; bubble.
        "multiple_system_messages" => EntryError::CallTerminal { reason },
        "multiple_system_turns" => EntryError::CallTerminal { reason },
        "unknown_field" => EntryError::CallTerminal { reason },
        // Idempotent-replay of /fill with same request_id — provider
        // already accepted. Not an error at walker scope; handled at
        // HTTP-response layer as Ok(()). Slug-classifier path is
        // defensive only (shouldn't reach here from a 2xx response).
        "fill_already_submitted" => EntryError::RouteSkipped { reason },

        // ── Unknown slugs: conservative advance ───────────────────────────
        //
        // Forward-compat: if Wire introduces a new slug we haven't mapped,
        // treat as RouteSkipped (advance) rather than bubbling. The
        // reason carries the raw slug so operator telemetry still shows
        // what Wire actually returned.
        _ => EntryError::RouteSkipped {
            reason: format!("unknown_slug:{}", slug),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests — compile-only skeleton assertion. Bodies in Wave 3.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_rev21_slug: one test per tier ─────────────────────────

    /// Skeleton compile + one-slug smoke. Preserved from Wave 0.
    #[test]
    fn classify_rev21_slug_maps_insufficient_balance_to_route_skipped() {
        match classify_rev21_slug("insufficient_balance") {
            EntryError::RouteSkipped { reason } => assert_eq!(reason, "insufficient_balance"),
            other => panic!(
                "expected RouteSkipped for insufficient_balance, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn classify_rev21_slug_insufficient_balance_is_route_skipped() {
        assert!(matches!(
            classify_rev21_slug("insufficient_balance"),
            EntryError::RouteSkipped { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_operator_mismatch_is_call_terminal() {
        // Both the JWT-rid variant and the generic operator_mismatch
        // should bubble — they signal identity-binding bugs that no
        // other route will fix.
        assert!(matches!(
            classify_rev21_slug("quote_operator_mismatch"),
            EntryError::CallTerminal { .. }
        ));
        assert!(matches!(
            classify_rev21_slug("operator_mismatch"),
            EntryError::CallTerminal { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_platform_unavailable_is_route_skipped() {
        // Prompt explicitly overrides plan §4.2's Retryable tag: walker
        // v1 advances to next route rather than sleeping for the retry
        // window.
        assert!(matches!(
            classify_rev21_slug("platform_unavailable"),
            EntryError::RouteSkipped { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_unknown_default_is_route_skipped() {
        let out = classify_rev21_slug("some_future_slug_we_dont_know");
        match out {
            EntryError::RouteSkipped { reason } => {
                assert!(
                    reason.starts_with("unknown_slug:"),
                    "expected unknown_slug prefix, got {reason}"
                );
                assert!(reason.contains("some_future_slug_we_dont_know"));
            }
            other => panic!("expected RouteSkipped for unknown slug, got {:?}", other),
        }
    }

    #[test]
    fn classify_rev21_slug_economic_parameter_missing_is_call_terminal() {
        // Wire-operator-level config bug; every market dispatch would
        // 503 the same way until resolved. Bubble rather than loop.
        assert!(matches!(
            classify_rev21_slug("economic_parameter_missing"),
            EntryError::CallTerminal { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_max_tokens_exceeds_quote_is_call_terminal() {
        assert!(matches!(
            classify_rev21_slug("max_tokens_exceeds_quote"),
            EntryError::CallTerminal { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_dispatch_deadline_exceeded_is_route_skipped() {
        assert!(matches!(
            classify_rev21_slug("dispatch_deadline_exceeded"),
            EntryError::RouteSkipped { .. }
        ));
    }

    // ── classify_rev21_http_error: slug-from-body + stage fallback ─────

    #[test]
    fn classify_http_error_routes_slug_through_slug_classifier() {
        let err =
            "API error 409: {\"error\":\"insufficient_balance\",\"detail\":{\"need\":10,\"have\":0}}"
                .to_string();
        let out = classify_rev21_http_error(&err, "quote");
        assert!(matches!(out, EntryError::RouteSkipped { .. }));
    }

    #[test]
    fn classify_http_error_401_on_quote_without_slug_is_call_terminal() {
        // 401 with a non-JSON body → stage-tagged terminal per prompt.
        let err = "API error 401: unauthorized-raw-text".to_string();
        let out = classify_rev21_http_error(&err, "quote");
        match out {
            EntryError::CallTerminal { reason } => assert_eq!(reason, "quote_auth_failed"),
            other => panic!("expected CallTerminal quote_auth_failed, got {:?}", other),
        }
    }

    #[test]
    fn classify_http_error_401_on_fill_without_slug_is_call_terminal() {
        let err = "API error 401: bad-token".to_string();
        let out = classify_rev21_http_error(&err, "fill");
        match out {
            EntryError::CallTerminal { reason } => assert_eq!(reason, "fill_auth_failed"),
            other => panic!("expected CallTerminal fill_auth_failed, got {:?}", other),
        }
    }

    #[test]
    fn classify_http_error_401_on_purchase_without_slug_is_route_skipped() {
        // Plan §4.2: purchase 401 generic → advance. Wire auth distinct
        // from fleet + openrouter so other routes may still serve.
        let err = "API error 401: token-expired".to_string();
        let out = classify_rev21_http_error(&err, "purchase");
        assert!(matches!(out, EntryError::RouteSkipped { .. }));
    }

    #[test]
    fn classify_http_error_non_api_error_prefix_is_retryable() {
        // Transport / io / dns failure path.
        let err = "reqwest: connection refused".to_string();
        let out = classify_rev21_http_error(&err, "fill");
        match out {
            EntryError::Retryable { reason } => assert!(reason.starts_with("fill_network:")),
            other => panic!("expected Retryable fill_network, got {:?}", other),
        }
    }

    // ── extract_error_slug ─────────────────────────────────────────────

    #[test]
    fn extract_error_slug_pulls_error_field() {
        let body = r#"{"error":"quote_jwt_expired","detail":{}}"#;
        assert_eq!(
            extract_error_slug(body),
            Some("quote_jwt_expired".to_string())
        );
    }

    #[test]
    fn extract_error_slug_returns_none_on_non_json() {
        assert!(extract_error_slug("not-json").is_none());
        assert!(extract_error_slug("").is_none());
        assert!(extract_error_slug("{\"error\":\"\"}").is_none());
    }

    #[test]
    fn extract_slug_from_http_error_roundtrip() {
        let err = "API error 409: {\"error\":\"quote_no_longer_winning\"}";
        assert_eq!(
            extract_slug_from_http_error(err),
            Some("quote_no_longer_winning".to_string())
        );
    }

    // ── await_result: timeout / closed-channel surfaces ────────────────

    #[tokio::test]
    async fn await_result_timeout_returns_retryable() {
        let pending = PendingJobs::new();
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let rx = register_pending(&pending, uuid).await;
        let out = await_result(rx, uuid, &pending, Duration::from_millis(1)).await;
        match out {
            Err(EntryError::Retryable { reason }) => assert_eq!(reason, "fill_result_timeout"),
            other => panic!("expected Retryable timeout, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn await_result_success_payload_produces_llm_response() {
        let pending = PendingJobs::new();
        let uuid = "550e8400-e29b-41d4-a716-446655440001";

        // Register BEFORE spawning the waiter — this mirrors the walker's
        // race-fixed call order: register → fill → await_result.
        let rx = register_pending(&pending, uuid).await;

        let pending_clone = pending.clone();
        let wait_handle = tokio::spawn(async move {
            await_result(rx, uuid, &pending_clone, Duration::from_millis(500)).await
        });

        // Sender is present immediately — race-fix removed the post-spawn
        // settling window.
        let sender = pending
            .take(uuid)
            .await
            .expect("sender should be registered before await_result spawn");
        sender
            .send(DeliveryPayload::Success {
                content: "hi".into(),
                input_tokens: 7,
                output_tokens: 3,
                model_used: "llama3:70b".into(),
                latency_ms: 42,
                finish_reason: Some("stop".into()),
            })
            .expect("send");

        let out = wait_handle.await.expect("task").expect("Ok");
        assert_eq!(out.content, "hi");
        assert_eq!(out.usage.prompt_tokens, 7);
        assert_eq!(out.usage.completion_tokens, 3);
        assert_eq!(out.provider_id.as_deref(), Some("market:llama3:70b"));
    }

    #[tokio::test]
    async fn await_result_failure_payload_is_route_skipped() {
        let pending = PendingJobs::new();
        let uuid = "550e8400-e29b-41d4-a716-446655440002";

        let rx = register_pending(&pending, uuid).await;

        let pending_clone = pending.clone();
        let wait_handle = tokio::spawn(async move {
            await_result(rx, uuid, &pending_clone, Duration::from_millis(500)).await
        });

        let sender = pending.take(uuid).await.expect("sender");
        sender
            .send(DeliveryPayload::Failure {
                code: "provider_error".into(),
                message: "oom".into(),
            })
            .expect("send");

        let out = wait_handle.await.expect("task");
        match out {
            Err(EntryError::RouteSkipped { reason }) => {
                assert!(reason.starts_with("provider_returned_error:"));
                assert!(reason.contains("provider_error"));
                assert!(reason.contains("oom"));
            }
            other => panic!("expected RouteSkipped provider_returned_error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn register_pending_returns_receiver_before_fill_can_race() {
        // Race-fix invariant: the receiver must be live on the map
        // immediately after register_pending returns, so a provider
        // callback that arrives before /fill completes still finds a
        // registered sender to fire.
        let pending = PendingJobs::new();
        let uuid = "550e8400-e29b-41d4-a716-446655440003";

        let _rx = register_pending(&pending, uuid).await;
        assert_eq!(
            pending.len().await,
            1,
            "register_pending must install the sender synchronously"
        );
    }

    // ── Body serialization surface ─────────────────────────────────────

    #[test]
    fn compute_fill_body_serializes_only_wire_allowed_fields() {
        // Wire's /fill allowed-field whitelist (route.ts:193-198) is
        // exactly these 8 names. Any extra top-level key 400s with
        // `unknown_field`. This test pins the struct shape to the
        // whitelist so a field drift (either direction) fails CI
        // rather than production /fill.
        let body = ComputeFillBody {
            job_id: "playful/106/42".into(),
            messages: serde_json::json!([{"role": "user", "content": "hi"}]),
            max_tokens: None,
            temperature: 0.7,
            relay_count: 0,
            privacy_tier: "direct".into(),
            input_token_count: 12,
            requester_callback_url: "https://tunnel/v1/compute/job-result".into(),
        };
        let v = serde_json::to_value(&body).expect("serialize");
        let obj = v.as_object().expect("body must serialize as object");

        // Every wire-allowed field present (except max_tokens, which
        // spec §2.3 says MUST be omitted when the caller doesn't set a
        // ceiling so Wire uses `max_tokens_quoted`).
        assert!(obj.contains_key("job_id"));
        assert!(obj.contains_key("messages"));
        assert!(!obj.contains_key("max_tokens"),
            "max_tokens must be omitted when None per spec §2.3");
        assert!(obj.contains_key("temperature"));
        assert!(obj.contains_key("relay_count"));
        assert!(obj.contains_key("privacy_tier"));
        assert!(obj.contains_key("input_token_count"));
        assert!(obj.contains_key("requester_callback_url"));

        // Walker-side fields MUST NOT appear in the wire body — they
        // live on `ComputeFillRequest`, not `ComputeFillBody`.
        assert!(!obj.contains_key("request_id"),
            "request_id is walker-internal; Wire reads it from the reserved job record");
        assert!(!obj.contains_key("idempotency_key"),
            "idempotency_key goes in the Idempotency-Key header, never the body");

        // No OTHER top-level keys beyond the whitelist.
        let allowed = [
            "job_id", "messages", "max_tokens", "temperature", "relay_count",
            "privacy_tier", "input_token_count", "requester_callback_url",
        ];
        for key in obj.keys() {
            assert!(allowed.contains(&key.as_str()),
                "unexpected field in /fill body: {} — Wire's strict validator will 400 unknown_field", key);
        }
    }

    #[test]
    fn compute_quote_body_serializes_requester_node_id_when_present() {
        // Walker always injects requester_node_id from auth.node_id so
        // Wire's /quote cannot 400 with
        // `multiple_nodes_require_explicit_node_id` for operators who
        // own >1 node (contract §3.2). Verifies the serialized body
        // carries the exact field name and value Wire expects.
        let body = ComputeQuoteBody {
            model_id: "gemma4:26b".into(),
            input_tokens_est: 512,
            max_tokens: 200,
            latency_preference: LatencyPreference::BestPrice,
            max_budget: 5000,
            requester_node_id: Some("node-uuid-abc123".into()),
        };
        let v = serde_json::to_value(&body).expect("serialize");
        assert_eq!(
            v.get("requester_node_id").and_then(|s| s.as_str()),
            Some("node-uuid-abc123"),
            "requester_node_id must be in the /quote body verbatim"
        );
    }

    #[test]
    fn compute_quote_body_omits_requester_node_id_when_none() {
        // Serde `skip_serializing_if` on the contracts-crate field means
        // None drops the field entirely (Wire's Zod `.strict()` rejects
        // `null`). Walker should never send None in production, but the
        // type-level contract still matters for forward compat.
        let body = ComputeQuoteBody {
            model_id: "x".into(),
            input_tokens_est: 1,
            max_tokens: 1,
            latency_preference: LatencyPreference::BestPrice,
            max_budget: 1,
            requester_node_id: None,
        };
        let v = serde_json::to_value(&body).expect("serialize");
        assert!(
            v.get("requester_node_id").is_none(),
            "requester_node_id must be omitted when None"
        );
    }

    #[test]
    fn compute_fill_body_emits_max_tokens_when_set() {
        let body = ComputeFillBody {
            job_id: "h".into(),
            messages: serde_json::json!([]),
            max_tokens: Some(500),
            temperature: 0.0,
            relay_count: 0,
            privacy_tier: "direct".into(),
            input_token_count: 0,
            requester_callback_url: "https://x".into(),
        };
        let v = serde_json::to_value(&body).expect("serialize");
        assert_eq!(v.get("max_tokens").and_then(|n| n.as_i64()), Some(500));
    }

    #[test]
    fn compute_fill_request_carries_walker_state_without_leaking_to_body() {
        // The wrapper holds request_id + idempotency_key for walker use
        // (PendingJobs keying + HTTP header construction); neither field
        // appears in the body's serialization.
        let request = ComputeFillRequest {
            body: ComputeFillBody {
                job_id: "playful/106/42".into(),
                messages: serde_json::json!([]),
                max_tokens: None,
                temperature: 0.0,
                relay_count: 0,
                privacy_tier: "direct".into(),
                input_token_count: 0,
                requester_callback_url: "https://x".into(),
            },
            request_id: "req-uuid-walker-only".into(),
            idempotency_key: "req-uuid-walker-only".into(),
        };
        let body_json = serde_json::to_value(&request.body).expect("serialize body");
        let obj = body_json.as_object().unwrap();
        assert!(!obj.contains_key("request_id"));
        assert!(!obj.contains_key("idempotency_key"));
        // Walker-side fields must still be reachable on the wrapper.
        assert_eq!(request.request_id, "req-uuid-walker-only");
        assert_eq!(request.idempotency_key, "req-uuid-walker-only");
    }

    // ── X-Wire-Retry header precedence + all_offers_saturated_for_model ──

    use crate::http_utils::RetryHints;

    fn make_api_err(
        status: u16,
        body: serde_json::Value,
        x_wire_retry: Option<&str>,
        x_retriable: Option<bool>,
    ) -> ApiErrorWithHints {
        ApiErrorWithHints {
            status,
            body,
            hints: RetryHints {
                x_wire_retry: x_wire_retry.map(|s| s.to_string()),
                x_retriable,
            },
        }
    }

    #[test]
    fn classify_wire_error_x_wire_retry_never_forces_call_terminal() {
        // Wire said "do not retry" — even if the slug would normally
        // RouteSkip or even Retry, X-Wire-Retry: never wins. The slug is
        // preserved in the reason for telemetry.
        let err = make_api_err(
            404,
            serde_json::json!({ "error": "no_offer_for_model" }),
            Some("never"),
            None,
        );
        match classify_wire_error(&err, "quote") {
            EntryError::CallTerminal { reason } => assert_eq!(reason, "no_offer_for_model"),
            other => panic!("expected CallTerminal on X-Wire-Retry: never, got {:?}", other),
        }
    }

    #[test]
    fn classify_wire_error_x_wire_retry_transient_is_retryable() {
        // Transient = retry. Walker's entry-level retry loop decides
        // backoff from the structured detail. This is the saturation
        // path's happy case (Wire pairs 409 with X-Wire-Retry: transient).
        let err = make_api_err(
            409,
            serde_json::json!({
                "error": "all_offers_saturated_for_model",
                "detail": {
                    "model_id": "gemma4:26b",
                    "offer_count": 1,
                    "min_current_queue_depth": 8,
                    "max_queue_depth_across_offers": 8,
                    "min_expected_drain_ms": 15000,
                    "median_typical_serve_ms_p50": 15000
                }
            }),
            Some("transient"),
            Some(true),
        );
        match classify_wire_error(&err, "purchase") {
            EntryError::Retryable { reason } => {
                assert_eq!(reason, "all_offers_saturated_for_model")
            }
            other => panic!("expected Retryable, got {:?}", other),
        }
    }

    #[test]
    fn classify_wire_error_x_wire_retry_backoff_is_retryable() {
        let err = make_api_err(
            503,
            serde_json::json!({ "error": "platform_unavailable" }),
            Some("backoff"),
            None,
        );
        assert!(matches!(
            classify_wire_error(&err, "quote"),
            EntryError::Retryable { .. }
        ));
    }

    #[test]
    fn classify_wire_error_no_hint_falls_through_to_slug() {
        // No X-Wire-Retry header — classifier uses the existing slug
        // path. `max_tokens_exceeds_quote` is CallTerminal per the slug
        // table; verify the fall-through reaches it.
        let err = make_api_err(
            400,
            serde_json::json!({ "error": "max_tokens_exceeds_quote" }),
            None,
            None,
        );
        assert!(matches!(
            classify_wire_error(&err, "fill"),
            EntryError::CallTerminal { .. }
        ));
    }

    #[test]
    fn classify_wire_error_unrecognized_hint_falls_through_to_slug() {
        // Forward-compat: Wire ships an unexpected hint string. Walker
        // doesn't crash; falls through to slug classification.
        let err = make_api_err(
            409,
            serde_json::json!({ "error": "insufficient_balance" }),
            Some("some_new_hint_we_dont_know"),
            None,
        );
        assert!(matches!(
            classify_wire_error(&err, "quote"),
            EntryError::RouteSkipped { .. }
        ));
    }

    #[test]
    fn classify_rev21_slug_all_offers_saturated_is_retryable() {
        // Slug-only path (no header) — the new saturation slug still
        // classifies correctly when routed through the slug classifier
        // (defensive path: proxies that strip headers, tests).
        assert!(matches!(
            classify_rev21_slug("all_offers_saturated_for_model"),
            EntryError::Retryable { .. }
        ));
    }

    #[test]
    fn all_offers_saturated_detail_deserializes_from_contracts_crate() {
        // Shape pinned to agent-wire-contracts::AllOffersSaturatedDetail
        // (rev 2.1.1). Walker treats `min_expected_drain_ms` as
        // Option<f64> — NULL when Wire's cohort has no observations.
        let full = serde_json::json!({
            "model_id": "gemma4:26b",
            "offer_count": 3,
            "min_current_queue_depth": 7,
            "max_queue_depth_across_offers": 8,
            "min_expected_drain_ms": 12000.0,
            "median_typical_serve_ms_p50": 15000.0
        });
        let detail: AllOffersSaturatedDetail = serde_json::from_value(full).unwrap();
        assert_eq!(detail.model_id, "gemma4:26b");
        assert_eq!(detail.min_expected_drain_ms, Some(12000.0));
        assert_eq!(detail.median_typical_serve_ms_p50, Some(15000.0));

        // Cohort-lacks-observations shape — min_expected_drain_ms null.
        // Walker falls back to a policy-defined floor when absent.
        let sparse = serde_json::json!({
            "model_id": "gemma4:26b",
            "offer_count": 1,
            "min_current_queue_depth": 8,
            "max_queue_depth_across_offers": 8,
            "min_expected_drain_ms": null,
            "median_typical_serve_ms_p50": null
        });
        let detail: AllOffersSaturatedDetail = serde_json::from_value(sparse).unwrap();
        assert_eq!(detail.min_expected_drain_ms, None);
        assert_eq!(detail.median_typical_serve_ms_p50, None);
    }
}
