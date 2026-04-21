//! Rev 2.1 three-RPC compute-market client: `/quote` → `/purchase` → `/fill`.
//!
//! Walker's market branch (plan §4.2) invokes these four public entry points
//! back-to-back. This module replaces the rev-2.0 `dispatch_market` /
//! `call_market` / `call_match` / `call_fill` / `resolve_uuid_from_handle`
//! surface in `compute_requester.rs` (slated for removal in Wave 5 per
//! plan §2).
//!
//! Wave 0 status: SKELETON ONLY. All four RPCs stub with
//! `unimplemented!("Wave 3")`. Wave 3 fills the HTTP bodies. Call sites
//! are still routed through `compute_requester` today — nothing in this
//! module is exercised until Wave 3 lands the walker market branch.
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

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::AuthState;
use crate::pyramid::llm::{EntryError, LlmResponse};
use crate::pyramid::pending_jobs::PendingJobs;
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

/// `/api/v1/compute/fill` request body (rev 2.1). Declared locally because
/// the `agent-wire-contracts` rev pinned in `Cargo.toml` (a9e356d3) does
/// not yet export this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeFillBody {
    pub job_id: String,
    pub request_id: String,
    pub messages: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    pub temperature: f32,
    pub relay_count: i64,
    pub privacy_tier: String,
    pub input_token_count: i64,
    pub requester_callback_url: String,
    pub idempotency_key: String,
}

// ---------------------------------------------------------------------------
// Public stubs — bodies in Wave 3.
// ---------------------------------------------------------------------------

/// POST `/api/v1/compute/quote`. Returns a signed quote JWT + price
/// breakdown. Body in Wave 3.
#[allow(dead_code)]
pub async fn quote(
    _auth: &Arc<RwLock<AuthState>>,
    _config: &Arc<RwLock<WireNodeConfig>>,
    _body: ComputeQuoteBody,
) -> Result<ComputeQuoteResponse, EntryError> {
    unimplemented!("Wave 3")
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
    _auth: &Arc<RwLock<AuthState>>,
    _config: &Arc<RwLock<WireNodeConfig>>,
    _quote_jwt: &str,
    _body: ComputePurchaseBody,
) -> Result<ComputePurchaseResponse, EntryError> {
    unimplemented!("Wave 3")
}

/// POST `/api/v1/compute/fill`. Dispatches the ChatML messages + callback
/// URL to Wire, which forwards to the matched provider. Body in Wave 3.
///
/// Wire's strict-allowed-field check means every field on
/// [`ComputeFillBody`] is required (modulo the one `#[serde(skip_...)]`
/// on `max_tokens`); extras 400.
#[allow(dead_code)]
pub async fn fill(
    _auth: &Arc<RwLock<AuthState>>,
    _config: &Arc<RwLock<WireNodeConfig>>,
    _body: ComputeFillBody,
) -> Result<(), EntryError> {
    unimplemented!("Wave 3")
}

/// Await the inbound `/v1/compute/job-result` envelope keyed by
/// `uuid_job_id` (the DB-row UUID surfaced on `ComputePurchaseResponse`).
/// Body in Wave 3.
#[allow(dead_code)]
pub async fn await_result(
    _pending_jobs: &PendingJobs,
    _uuid_job_id: &str,
    _timeout: Duration,
) -> Result<LlmResponse, EntryError> {
    unimplemented!("Wave 3")
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
        // ── /quote error slugs (plan §4.2 first block) ────────────────────
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
        // Wire-side platform outage / missing economic_parameter. Honor
        // `X-Wire-Retry`; for v1 walker advances rather than loops.
        "platform_unavailable" => EntryError::Retryable { reason },
        "economic_parameter_missing" => EntryError::Retryable { reason },
        // Walker built a malformed body — same bug would fire on every
        // route that routes through Wire. Bubble.
        "invalid_body" => EntryError::CallTerminal { reason },
        "multiple_nodes_require_explicit_node_id" => EntryError::CallTerminal { reason },
        "no_node_for_agent" => EntryError::CallTerminal { reason },
        // Operator consent not granted. No alternate route will satisfy
        // Wire until operator fixes the agent binding. Bubble.
        "agent_unconfirmed" => EntryError::CallTerminal { reason },

        // ── /purchase error slugs ─────────────────────────────────────────
        //
        // Quote lost the winning-offer race between /quote and /purchase.
        // Treat as transient; advance (v1 does NOT re-quote same entry).
        "quote_no_longer_winning" => EntryError::Retryable { reason },
        // Idempotent-replay mismatch — different walker attempt already
        // purchased. Hand the work back for fresh route selection.
        "quote_already_purchased" => EntryError::RouteSkipped { reason },
        // Quote JWT expired between mint and /purchase. v1 advances; does
        // not re-quote.
        "quote_jwt_expired" => EntryError::RouteSkipped { reason },
        // Quote JWT malformed — walker built a bad body. Bubble.
        "quote_jwt_invalid" => EntryError::CallTerminal { reason },
        // JWT `rid` ≠ authed operator — caller-config bug affecting every
        // market dispatch from this node until resolved. Bubble.
        "quote_operator_mismatch" => EntryError::CallTerminal { reason },

        // ── /fill error slugs ─────────────────────────────────────────────
        //
        // We lost the dispatch slot (another /fill won, or Wire expired
        // the reservation). Advance.
        "dispatch_deadline_exceeded" => EntryError::RouteSkipped { reason },
        // Provider's local depth saturated. Advance; other routes may
        // serve. Honor `X-Wire-Retry` on Wave 3 implementation.
        "provider_depth_exceeded" => EntryError::RouteSkipped { reason },
        "provider_dispatch_conflict" => EntryError::RouteSkipped { reason },
        // Walker passed `max_tokens > max_tokens_quoted`. Walker bug;
        // same bug would fire on every route. Bubble.
        "max_tokens_exceeds_quote" => EntryError::CallTerminal { reason },

        // ── Unknown slugs: conservative advance ───────────────────────────
        _ => EntryError::RouteSkipped { reason },
    }
}

// ---------------------------------------------------------------------------
// Tests — compile-only skeleton assertion. Bodies in Wave 3.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Skeleton compile + one-slug smoke. Full slug-table coverage lands
    /// in Wave 3 once bodies exist to exercise it end-to-end.
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
}
