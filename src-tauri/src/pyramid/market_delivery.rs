//! Phase 3 rev 0.6.1 — Provider-side compute-market delivery worker.
//!
//! Implements the two-POST P2P delivery state machine per contract rev 2.0
//! (§2.3 + §2.6). After a market dispatch completes inference the provider
//! node must independently land TWO POSTs, each with its own URL, Bearer
//! token, and retry budget:
//!
//!   * **Content leg** — provider → requester direct (§2.6). URL and bearer
//!     come from the dispatch body: `requester_callback_url` +
//!     `requester_delivery_jwt`. Body includes `result.content`. Failure
//!     variant goes to the requester too (D4) so the requester can stop
//!     polling rather than wait forever.
//!   * **Settlement leg** — provider → Wire (§2.3). URL and bearer come
//!     from the dispatch body too (`callback_url` + `callback_auth.token`).
//!     Body is the §2.3 envelope with `result.content` OMITTED — Wire is
//!     zero-storage for content (§2.4).
//!
//! Spec: `docs/plans/compute-market-phase-3-provider-delivery-spec.md`
//! rev 0.6.1. Contract: `GoodNewsEveryone/docs/architecture/
//! wire-node-compute-market-contract.md` §§2.3, 2.6, 3.4, 10.5.
//!
//! # Architecture
//!
//! - **Trigger model:** `tokio::select!` on (a) nudge-channel recv, (b)
//!   periodic tick. Unchanged from rev 0.5.
//! - **Claim:** per-tick the worker issues TWO claim queries — one for
//!   content-leg eligibles, one for settlement-leg eligibles — via
//!   `market_outbox_claim_content_for_delivery` and
//!   `market_outbox_claim_settlement_for_delivery` (Wave 1A). Each leg's
//!   claim asks for up to `max_concurrent_deliveries` rows; the combined
//!   list is bounded-parallel POSTed at the `max_concurrent_deliveries`
//!   cap. Per contract §2.6 concurrency note, the cap is UNIFIED across
//!   legs (not per-leg) because both legs share the same outbound
//!   HTTP/socket budget.
//! - **POST:** `deliver_leg` is a pure per-leg function invoked twice per
//!   row (once for each leg) in bounded-parallel `for_each_concurrent`.
//!   Client config unchanged from rev 0.5: `redirect(Policy::none())`
//!   + per-POST timeout so a bearer can't cross-origin-leak on a DNS
//!   compromise.
//! - **Envelope adapter split:** `build_content_envelope` produces the
//!   §2.6 full shape (includes `result.content`); `build_settlement_envelope`
//!   produces the §2.3 shape minus `result.content`. Failure variants are
//!   identical across both (D4).
//! - **Retry classification per leg:**
//!     * Settlement leg reads `X-Wire-Retry` (Wire's protocol header per
//!       §2.2) and falls back to terminal-HTTP-code enum for pre-upgrade
//!       Wire.
//!     * Content leg does NOT read `X-Wire-Retry` (arbitrary requester
//!       HTTP doesn't standardize that header) — reads standard
//!       `Retry-After` if present, falls back to the
//!       `compute_delivery_policy.backoff_schedule_secs` table.
//! - **Per-leg retry budget:** sourced from `compute_delivery_policy`
//!   economic_parameter via `ComputeDeliveryPolicy::from_wire_parameters`
//!   at tick start; falls back to `contract_defaults()` when Wire
//!   hasn't shipped the key yet (zero-lockstep). Budgets are independent
//!   (Q-PROTO-6): flaky requester tunnel burning the content-leg budget
//!   does NOT exhaust the settlement-leg budget.
//! - **Row-level terminal composition** — `delivery_status` uses the same
//!   vocabulary as Wire's `wire_compute_jobs.delivery_status` per spec
//!   line 78 (`failed_content_only | failed_settlement_only | failed_both`).
//! - **Supervisor:** `supervise_delivery_loop` wraps the loop in
//!   `AssertUnwindSafe::catch_unwind`; unchanged from rev 0.5.
//! - **Redaction:** `CallbackAuth` has a custom `Debug` impl that elides
//!   `token`; the same no-`{:?}`-on-request discipline now applies to the
//!   `requester_delivery_jwt` string too (truncate + no `{:?}` on the POST
//!   body). Error messages truncated to `max_error_message_chars` before
//!   DB write or chronicle emit.
//!
//! # Invariants
//!
//! 1. Each leg's `*_lease_until > now()` on a ready row is owned by
//!    exactly one delivery worker tick. Per-leg claims are CAS-atomic
//!    (UPDATE ... RETURNING) so no TOCTOU window exists.
//! 2. On terminal row transition (delivered / failed_*), both legs'
//!    `*_lease_until` and `*_next_attempt_at` are cleared by the DB helper.
//! 3. Both envelope adapters (`build_content_envelope` +
//!    `build_settlement_envelope`) emit `body.job_id` as a UUID — never
//!    the handle-path — per §10.5 + Pillar J7. Debug-assertion guards
//!    the POST site (regression guard against a future write-path bug).
//! 4. The `callback_auth.token` bearer AND the `requester_delivery_jwt`
//!    never enter a log or chronicle metadata string. Enforced structurally
//!    (no `{:?}` on `MarketDispatchRequest` / `CallbackAuth` / the opaque
//!    JWT), and by the `requester_delivery_jwt_never_in_logs` unit test.
//!
//! # Relay deferral
//!
//! Relay rows (`callback_kind = 'Relay'`) are NOT claimed by this worker
//! for content-leg POST — the relay hop adds a second decryption layer
//! that's not implemented yet. The per-leg claim helpers already scope
//! `callback_kind = 'MarketStandard'`.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{stream::StreamExt, FutureExt};
use reqwest::header::HeaderMap;
use serde_json::json;
use tokio::sync::{mpsc, RwLock};

use crate::compute_market::ComputeDeliveryPolicy;
use crate::pyramid::compute_chronicle::{
    record_event, ChronicleEventContext, EVENT_MARKET_CONTENT_DELIVERY_ATTEMPT_FAILED,
    EVENT_MARKET_CONTENT_DELIVERY_FAILED, EVENT_MARKET_CONTENT_LEG_SUCCEEDED,
    EVENT_MARKET_DELIVERY_TASK_EXITED, EVENT_MARKET_DELIVERY_TASK_PANICKED,
    EVENT_MARKET_RESULT_DELIVERED, EVENT_MARKET_RESULT_DELIVERY_CAS_LOST,
    EVENT_MARKET_RESULT_DELIVERY_FAILED, EVENT_MARKET_SETTLEMENT_DELIVERY_ATTEMPT_FAILED,
    EVENT_MARKET_SETTLEMENT_DELIVERY_FAILED, EVENT_MARKET_SETTLEMENT_LEG_SUCCEEDED, SOURCE_MARKET,
};
use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;
use crate::pyramid::market_dispatch::MarketAsyncResult;

/// Marker for the pyramid_schema_versions row this worker's orphan-detection
/// heuristic keys against. Matches the INSERT OR IGNORE in
/// `db::init_pyramid_db`.
const MIGRATION_NAME: &str = "fleet_result_outbox_v2_callback_auth_token";

/// Hardcoded sanity cap on bearer length. Base64url-encoded 32-byte tokens
/// are ~43 chars; JWTs used for the content leg are bigger (typically
/// 200-400 bytes — EdDSA signature + claim payload). Lift the cap to 4096
/// so a full JWT fits; still guards against a hypothetical DoS where Wire
/// or a man-in-the-middle ships a multi-MB "token" that the Authorization
/// header build would copy.
const MAX_TOKEN_LEN: usize = 4096;

/// HTTP status codes that mean "retrying will not help" when Wire doesn't
/// ship the `X-Wire-Retry` header (pre-upgrade Wire). See spec §2 + Wire
/// compute-errors.ts classifier output enumeration.
const TERMINAL_HTTP_CODES_FALLBACK: &[u16] = &[400, 401, 403, 404, 409, 410, 413];

// ── Leg discriminator ───────────────────────────────────────────────────────

/// Which leg of the two-POST delivery state machine is being processed.
/// Threaded through `deliver_leg`, the envelope adapters, and the outcome
/// classifier so a single code path can serve both hops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    /// Content leg: provider → requester direct (§2.6). Body includes
    /// `result.content`; auth is `requester_delivery_jwt`; URL is
    /// `requester_callback_url`.
    Content,
    /// Settlement leg: provider → Wire (§2.3). Body OMITS `result.content`;
    /// auth is `callback_auth.token`; URL is `callback_url`.
    Settlement,
}

impl Leg {
    /// Human-friendly label used in chronicle metadata + error strings.
    /// Stable identifier — do not change lightly; downstream dashboards
    /// match on exact value.
    fn label(&self) -> &'static str {
        match self {
            Leg::Content => "content",
            Leg::Settlement => "settlement",
        }
    }
}

// ── Content-leg envelope (§2.6 — includes result.content) ──────────────────

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentEnvelope {
    Success {
        job_id: String,
        result: ContentResult,
    },
    Failure {
        job_id: String,
        error: CallbackError,
    },
}

#[derive(Debug, serde::Serialize)]
struct ContentResult {
    content: String,
    input_tokens: u64,
    output_tokens: u64,
    model_used: String,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

// ── Settlement-leg envelope (§2.3 — result.content OMITTED) ────────────────

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SettlementEnvelope {
    Success {
        job_id: String,
        result: SettlementResult,
    },
    Failure {
        job_id: String,
        error: CallbackError,
    },
}

/// §2.3 settlement result — same shape as ContentResult MINUS `content`.
/// If Wire receives `content` on a settlement POST it MAY drop-and-log
/// or MUST 400 with `settlement_carried_content` — this struct
/// structurally cannot emit it.
#[derive(Debug, serde::Serialize)]
struct SettlementResult {
    input_tokens: u64,
    output_tokens: u64,
    model_used: String,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

// ── Failure shape (identical across both legs per D4) ──────────────────────

#[derive(Debug, serde::Serialize)]
struct CallbackError {
    code: &'static str,
    message: String,
}

// ── X-Wire-Retry classification (contract §2 retry-intent protocol) ────────

enum RetryDecision {
    Terminal { source: &'static str },
    Retry { source: &'static str },
}

/// Classifier for the settlement leg. Reads Wire's explicit
/// `X-Wire-Retry` protocol header first; falls back to the terminal-HTTP
/// code enum for pre-upgrade Wire. Content-leg responses are NOT routed
/// through this function — the content leg uses `classify_retry_content`
/// which ignores `X-Wire-Retry` (not a requester-protocol header).
fn classify_retry_settlement(status: reqwest::StatusCode, headers: &HeaderMap) -> RetryDecision {
    if let Some(raw) = headers.get("X-Wire-Retry").and_then(|v| v.to_str().ok()) {
        match raw {
            "never" => return RetryDecision::Terminal { source: "x_wire_retry_never" },
            "transient" => return RetryDecision::Retry { source: "x_wire_retry_transient" },
            "backoff" => return RetryDecision::Retry { source: "x_wire_retry_backoff" },
            other => {
                tracing::warn!(
                    header_value = %other,
                    "X-Wire-Retry: unknown value; falling back to HTTP-code enumeration"
                );
            }
        }
    }
    if TERMINAL_HTTP_CODES_FALLBACK.contains(&status.as_u16()) {
        RetryDecision::Terminal { source: "http_code_fallback" }
    } else {
        RetryDecision::Retry { source: "http_code_fallback" }
    }
}

/// Classifier for the content leg. Ignores `X-Wire-Retry` (not a
/// requester-protocol header per spec lines 334-337) and decides based
/// on the HTTP status code alone. Terminal set is the same as the
/// settlement fallback — the same error classes (auth/NotFound/etc.)
/// are non-retriable regardless of who's on the other end.
fn classify_retry_content(status: reqwest::StatusCode) -> RetryDecision {
    if TERMINAL_HTTP_CODES_FALLBACK.contains(&status.as_u16()) {
        RetryDecision::Terminal { source: "http_code_fallback" }
    } else {
        RetryDecision::Retry { source: "http_code_fallback" }
    }
}

// ── Retry-After header parsing (RFC 7231 §7.1.3 — both forms) ───────────────

enum RetryAfterSource {
    HeaderSeconds,
    HeaderHttpDate,
    HeaderInvalid,
    Absent,
}

fn parse_retry_after_header(headers: &HeaderMap) -> (Option<u64>, RetryAfterSource) {
    let v = match headers.get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()) {
        Some(s) => s.trim(),
        None => return (None, RetryAfterSource::Absent),
    };

    if let Ok(secs) = v.parse::<u64>() {
        return (Some(secs), RetryAfterSource::HeaderSeconds);
    }

    if let Ok(target) = httpdate::parse_http_date(v) {
        let now = std::time::SystemTime::now();
        let delta = target.duration_since(now).map(|d| d.as_secs()).unwrap_or(0);
        return (Some(delta), RetryAfterSource::HeaderHttpDate);
    }

    tracing::warn!(
        header_value = %v,
        "Retry-After: neither integer-seconds nor HTTP-date; ignoring"
    );
    (None, RetryAfterSource::HeaderInvalid)
}

fn retry_after_source_label(src: &RetryAfterSource) -> &'static str {
    match src {
        RetryAfterSource::HeaderSeconds => "header_seconds",
        RetryAfterSource::HeaderHttpDate => "header_http_date",
        RetryAfterSource::HeaderInvalid => "header_invalid",
        RetryAfterSource::Absent => "computed_backoff",
    }
}

// ── Envelope adapters: MarketAsyncResult → per-leg envelope ────────────────

/// Content-leg adapter (§2.6 full shape including `result.content`).
/// Pure function; no I/O; callable from tests.
///
/// Invariant: `row.job_id` is a UUID. Handle-path lives only in
/// `callback_url` / `requester_callback_url`; body never carries it
/// per §10.5 + Pillar J7. Debug-assertion guards against a future
/// write-path bug that smuggles the handle-path into the outbox PK.
fn build_content_envelope(
    row: &crate::pyramid::db::OutboxRow,
    result: &MarketAsyncResult,
) -> ContentEnvelope {
    debug_assert!(
        uuid::Uuid::parse_str(&row.job_id).is_ok(),
        "OutboxRow.job_id must be UUID-format (contract §10.5); handle-path lives in callback_url"
    );

    match result {
        MarketAsyncResult::Success(resp) => {
            let input_tokens = resp.prompt_tokens.unwrap_or(0).max(0) as u64;
            let output_tokens = resp.completion_tokens.unwrap_or(0).max(0) as u64;
            let model_used = pick_model_used(resp);
            let latency_ms = row.inference_latency_ms.unwrap_or(0).max(0) as u64;
            ContentEnvelope::Success {
                job_id: row.job_id.clone(),
                result: ContentResult {
                    content: resp.content.clone(),
                    input_tokens,
                    output_tokens,
                    model_used,
                    latency_ms,
                    finish_reason: resp.finish_reason.clone(),
                },
            }
        }
        MarketAsyncResult::Error(msg) => ContentEnvelope::Failure {
            job_id: row.job_id.clone(),
            error: CallbackError { code: classify_failure_code(msg), message: msg.clone() },
        },
    }
}

/// Settlement-leg adapter (§2.3 shape MINUS `result.content`).
/// Pure function; structurally cannot emit `content` (the
/// `SettlementResult` struct has no such field). Failure variant is
/// identical to `build_content_envelope`'s Failure variant per D4 —
/// the same pinned §2.3 error code + message flows to both legs.
fn build_settlement_envelope(
    row: &crate::pyramid::db::OutboxRow,
    result: &MarketAsyncResult,
) -> SettlementEnvelope {
    debug_assert!(
        uuid::Uuid::parse_str(&row.job_id).is_ok(),
        "OutboxRow.job_id must be UUID-format (contract §10.5); handle-path lives in callback_url"
    );

    match result {
        MarketAsyncResult::Success(resp) => {
            let input_tokens = resp.prompt_tokens.unwrap_or(0).max(0) as u64;
            let output_tokens = resp.completion_tokens.unwrap_or(0).max(0) as u64;
            let model_used = pick_model_used(resp);
            let latency_ms = row.inference_latency_ms.unwrap_or(0).max(0) as u64;
            SettlementEnvelope::Success {
                job_id: row.job_id.clone(),
                result: SettlementResult {
                    input_tokens,
                    output_tokens,
                    model_used,
                    latency_ms,
                    finish_reason: resp.finish_reason.clone(),
                },
            }
        }
        MarketAsyncResult::Error(msg) => SettlementEnvelope::Failure {
            job_id: row.job_id.clone(),
            error: CallbackError { code: classify_failure_code(msg), message: msg.clone() },
        },
    }
}

/// Shared helper: model_used falls back through provider_model → model →
/// "unknown". Matches rev 0.5 semantics; Wire treats the field as
/// observability-only (non-load-bearing) so the "unknown" sentinel is
/// acceptable rather than terminal-failing.
fn pick_model_used(resp: &crate::pyramid::market_dispatch::MarketDispatchResponse) -> String {
    resp.provider_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if !resp.model.is_empty() { resp.model.clone() } else { "unknown".to_string() }
        })
}

/// Maps a MarketAsyncResult::Error(String) into one of the contract §2.3
/// pinned codes. Substring-matching on known failure-shape phrases; unknown
/// messages default to `model_error` (Wire's catch-all via mapFailureCodeToReason).
fn classify_failure_code(msg: &str) -> &'static str {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("worker heartbeat") || lower.contains("heartbeat lost") {
        "worker_heartbeat_lost"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "model_timeout"
    } else if lower.contains("out of memory") || lower.contains("oom") {
        "oom"
    } else if lower.contains("messages") && lower.contains("invalid") {
        "invalid_messages"
    } else {
        "model_error"
    }
}

// ── Token validation + string truncation ────────────────────────────────────

/// Defense-in-depth: reject obviously-malformed bearer tokens before
/// building the Authorization header. Accepts the base64url alphabet
/// (`[A-Za-z0-9_-]`) plus padding (`=`) and the JWT dot separator (`.`) —
/// content-leg bearers are EdDSA JWTs of the form `header.payload.signature`,
/// settlement-leg bearers are opaque base64url. A token containing whitespace
/// or control characters indicates corruption or injection; hit a terminal
/// chronicle rather than a mysterious 401.
fn is_valid_bearer(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= MAX_TOKEN_LEN
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '=' || c == '.')
}

/// UTF-8-boundary-safe truncation to the given character cap. Prevents
/// slicing a multi-byte char in half when we clip reqwest error strings.
fn truncate(s: &str, max_chars: u64) -> String {
    let cap = max_chars as usize;
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        s.chars().take(cap).collect::<String>() + "…"
    }
}

// ── Orphan detection helper (deploy-artifact vs token bug) ──────────────────

/// Returns true iff this row was admitted BEFORE the Phase 3 migration
/// that added `callback_auth_token`. Called only when the settlement-leg
/// token column is NULL; lets us emit a distinct terminal reason
/// (`orphaned_by_migration`) so operators see deploy artifacts as a
/// one-shot event rather than a token-plumbing bug.
async fn row_predates_migration(db_path: &std::path::Path, row_created_at: &str) -> bool {
    let db_path = db_path.to_path_buf();
    let created_at = row_created_at.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        match crate::pyramid::db::pyramid_schema_version_applied_at(&conn, MIGRATION_NAME) {
            Ok(Some(applied_at)) => created_at.as_str() < applied_at.as_str(),
            _ => false,
        }
    })
    .await
    .unwrap_or(false)
}

// ── Supervisor / loop entry points ──────────────────────────────────────────

/// Bundle of shared state the delivery loop needs.
pub struct DeliveryContext {
    pub db_path: PathBuf,
    pub policy: Arc<RwLock<MarketDeliveryPolicy>>,
    /// Wire parameters from the last-observed heartbeat response. Populated
    /// by the heartbeat self-heal path in main.rs. The delivery worker
    /// clones the HashMap per-tick and parses `ComputeDeliveryPolicy` out
    /// of it so the read lock is never held across `.await`.
    pub auth: Arc<RwLock<crate::auth::AuthState>>,
}

/// Spawn the supervisor task that owns the delivery loop. Mirrors the
/// pattern shipped in `market_mirror::spawn_market_mirror_task`.
pub fn spawn_market_delivery_task(
    ctx: DeliveryContext,
    rx: mpsc::UnboundedReceiver<()>,
) {
    tauri::async_runtime::spawn(async move {
        supervise_delivery_loop(ctx, rx).await;
    });
}

async fn supervise_delivery_loop(
    ctx: DeliveryContext,
    mut rx: mpsc::UnboundedReceiver<()>,
) {
    const PANIC_BACKOFF_SECS: u64 = 5;

    loop {
        let result = AssertUnwindSafe(delivery_loop(&ctx, &mut rx)).catch_unwind().await;
        match result {
            Ok(()) => {
                record_lifecycle_event(
                    &ctx,
                    EVENT_MARKET_DELIVERY_TASK_EXITED,
                    json!({ "reason": "channel_closed" }),
                )
                .await;
                tracing::info!(
                    "market delivery task exited cleanly (channel closed); supervisor stopping"
                );
                return;
            }
            Err(panic_payload) => {
                let message = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "panic in delivery_loop (payload not string)".to_string()
                };
                record_lifecycle_event(
                    &ctx,
                    EVENT_MARKET_DELIVERY_TASK_PANICKED,
                    json!({ "message": message, "backoff_secs": PANIC_BACKOFF_SECS }),
                )
                .await;
                tracing::error!(
                    panic = %message,
                    backoff_secs = PANIC_BACKOFF_SECS,
                    "market delivery task panicked; respawning"
                );
                tokio::time::sleep(Duration::from_secs(PANIC_BACKOFF_SECS)).await;
            }
        }
    }
}

async fn delivery_loop(
    ctx: &DeliveryContext,
    rx: &mut mpsc::UnboundedReceiver<()>,
) {
    tracing::info!("market delivery task started (rev 0.6.1 two-POST)");

    // One boot-push so a post-restart serving-true offer with ready rows
    // queued by the sweep gets processed immediately, not after the first
    // natural tick.
    tick(ctx).await;

    let interval_secs = ctx.policy.read().await.outbox_sweep_interval_secs.max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // consume the immediate tick; we already did boot

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                match maybe {
                    Some(()) => {
                        while rx.try_recv().is_ok() {}
                    }
                    None => {
                        return;
                    }
                }
            }
            _ = interval.tick() => {}
        }

        tick(ctx).await;
    }
}

/// One iteration: claim both legs + deliver the union in bounded-parallel.
async fn tick(ctx: &DeliveryContext) {
    let p = ctx.policy.read().await.clone();
    let lease_secs = p.callback_post_timeout_secs + p.lease_grace_secs;
    let max_concurrent = p.max_concurrent_deliveries;

    // Read Wire parameters snapshot for this tick; fall back to contract
    // defaults when the key isn't present or parses malformed. Clone
    // `wire_parameters` out of the AuthState RwLock so we don't hold a
    // read lock across the DB/HTTP awaits below (Pillar 9 discipline).
    let wire_parameters = ctx.auth.read().await.wire_parameters.clone();
    let delivery_policy =
        ComputeDeliveryPolicy::from_wire_parameters(&wire_parameters)
            .unwrap_or_else(ComputeDeliveryPolicy::contract_defaults);
    // `requester_delivery_jwt_ttl_secs` is a scalar (not part of
    // `compute_delivery_policy`) shipped via heartbeat `wire_parameters`.
    // Default per contract rev 2.0 §3.4 = fill_job_ttl_secs = 1800s. Used
    // by the content-leg 401 heuristic to distinguish
    // `terminal_http_401_likely_jwt_expired` from a plain 401 (cf. Pillar
    // 37 — semantic coupling to `ready_retention_secs` was a proxy hack;
    // reading the dedicated scalar removes it).
    let requester_delivery_jwt_ttl_secs = wire_parameters
        .get("requester_delivery_jwt_ttl_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(1800);

    // Two claim queries — one per leg. Each asks for up to max_concurrent;
    // the combined stream is then bounded-parallel at max_concurrent at the
    // for_each_concurrent level (§2.6 concurrency note — unified cap across
    // both legs because they share the same outbound HTTP/socket budget).
    let db_path = ctx.db_path.clone();
    let max_for_claim = max_concurrent;
    let claim_result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(Vec<crate::pyramid::db::OutboxRow>, Vec<crate::pyramid::db::OutboxRow>)> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let content = crate::pyramid::db::market_outbox_claim_content_for_delivery(
                &conn,
                lease_secs,
                max_for_claim,
            )
            .map_err(|e| anyhow::anyhow!("claim_content_for_delivery: {}", e))?;
            let settlement = crate::pyramid::db::market_outbox_claim_settlement_for_delivery(
                &conn,
                lease_secs,
                max_for_claim,
            )
            .map_err(|e| anyhow::anyhow!("claim_settlement_for_delivery: {}", e))?;
            Ok((content, settlement))
        },
    )
    .await;

    let (content_rows, settlement_rows) = match claim_result {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "delivery tick: claim query failed");
            return;
        }
        Err(je) => {
            tracing::warn!(err = %je, "delivery tick: claim join error");
            return;
        }
    };

    if content_rows.is_empty() && settlement_rows.is_empty() {
        return;
    }

    // Resolve rowids for each claimed row in a single blocking hop.
    // The per-leg DB helpers (Wave 1A) accept rowid rather than the
    // compound PK — easier SQLite CAS, but OutboxRow doesn't carry
    // rowid. We look them up here by (dispatcher_node_id, job_id) —
    // the outbox has a unique index on that pair.
    let jobs: Vec<(String, String, Leg)> = content_rows
        .iter()
        .map(|r| (r.dispatcher_node_id.clone(), r.job_id.clone(), Leg::Content))
        .chain(
            settlement_rows
                .iter()
                .map(|r| (r.dispatcher_node_id.clone(), r.job_id.clone(), Leg::Settlement)),
        )
        .collect();

    let db_path = ctx.db_path.clone();
    let rowid_map = tokio::task::spawn_blocking(
        move || -> anyhow::Result<std::collections::HashMap<(String, String), i64>> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let mut out = std::collections::HashMap::new();
            for (did, jid, _leg) in &jobs {
                let key = (did.clone(), jid.clone());
                if out.contains_key(&key) {
                    continue;
                }
                let rowid: Option<i64> = conn
                    .query_row(
                        "SELECT rowid FROM fleet_result_outbox
                         WHERE dispatcher_node_id = ?1 AND job_id = ?2",
                        rusqlite::params![did, jid],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(rid) = rowid {
                    out.insert(key, rid);
                }
            }
            Ok(out)
        },
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or_default();

    // Flatten into a single stream of (row, leg) pairs.
    let union: Vec<(crate::pyramid::db::OutboxRow, Leg, Option<i64>)> = content_rows
        .into_iter()
        .map(|r| {
            let rid = rowid_map.get(&(r.dispatcher_node_id.clone(), r.job_id.clone())).copied();
            (r, Leg::Content, rid)
        })
        .chain(settlement_rows.into_iter().map(|r| {
            let rid = rowid_map.get(&(r.dispatcher_node_id.clone(), r.job_id.clone())).copied();
            (r, Leg::Settlement, rid)
        }))
        .collect();

    let p_arc = Arc::new(p);
    let dp_arc = Arc::new(delivery_policy);
    futures_util::stream::iter(union)
        .for_each_concurrent(Some(max_concurrent as usize), |(row, leg, rowid)| {
            let p = Arc::clone(&p_arc);
            let dp = Arc::clone(&dp_arc);
            let ctx = ctx;
            let jwt_ttl = requester_delivery_jwt_ttl_secs;
            async move {
                match rowid {
                    Some(rid) => deliver_leg(ctx, row, leg, rid, &p, &dp, jwt_ttl).await,
                    None => {
                        tracing::warn!(
                            job_id = %row.job_id,
                            leg = leg.label(),
                            "delivery tick: rowid lookup failed; skipping leg"
                        );
                    }
                }
            }
        })
        .await;
}

/// Per-leg POST flow. Pure per-leg function; called twice per row (once
/// for each leg, from the `tick` unified stream). See spec lines 158-211.
///
/// `requester_delivery_jwt_ttl_secs` is the scalar from
/// `wire_parameters` (snapshot at tick start) used by the content-leg
/// 401 heuristic to distinguish `terminal_http_401_likely_jwt_expired`
/// from a plain `terminal_http_401`. Default = 1800 when Wire hasn't
/// shipped it yet (pre-rev-2.0).
#[allow(clippy::too_many_arguments)]
async fn deliver_leg(
    ctx: &DeliveryContext,
    row: crate::pyramid::db::OutboxRow,
    leg: Leg,
    rowid: i64,
    p: &MarketDeliveryPolicy,
    dp: &ComputeDeliveryPolicy,
    requester_delivery_jwt_ttl_secs: u64,
) {
    // 1. Result parse — bare MarketAsyncResult. Malformed is terminal
    //    (code bug, no retry). Both legs see the same parse failure
    //    independently — the other leg will also terminal-fail when
    //    its tick comes around. Row-level composition handles the
    //    dual-terminal flip.
    let async_result: MarketAsyncResult = match row
        .result_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
    {
        Some(r) => r,
        None => {
            let err = truncate(
                &format!(
                    "result_json parse failed or missing: {:?}",
                    row.result_json.as_deref().map(|s| &s[..s.len().min(80)])
                ),
                p.max_error_message_chars,
            );
            terminal_leg_fail(ctx, &row, rowid, leg, &err, "envelope_parse_failed", p).await;
            return;
        }
    };

    // 2. Resolve per-leg URL + Bearer.
    let (url, bearer, kind_for_ssrf) = match leg {
        Leg::Content => {
            let url = match row.requester_callback_url.as_deref() {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => {
                    terminal_leg_fail(
                        ctx,
                        &row,
                        rowid,
                        leg,
                        "requester_callback_url is missing",
                        "requester_callback_url_missing",
                        p,
                    )
                    .await;
                    return;
                }
            };
            let jwt = match row.requester_delivery_jwt.as_deref() {
                Some(t) if is_valid_bearer(t) => t.to_string(),
                _ => {
                    terminal_leg_fail(
                        ctx,
                        &row,
                        rowid,
                        leg,
                        "requester_delivery_jwt missing or malformed",
                        "requester_delivery_jwt_missing_or_invalid",
                        p,
                    )
                    .await;
                    return;
                }
            };
            (url, jwt, crate::fleet::CallbackKind::MarketStandard)
        }
        Leg::Settlement => {
            let url = row.callback_url.clone();
            let bearer = match row.callback_auth_token.as_deref() {
                Some(t) if is_valid_bearer(t) => t.to_string(),
                None if row_predates_migration(&ctx.db_path, &row.created_at).await => {
                    terminal_leg_fail(
                        ctx,
                        &row,
                        rowid,
                        leg,
                        "orphaned by migration",
                        "orphaned_by_migration",
                        p,
                    )
                    .await;
                    return;
                }
                _ => {
                    terminal_leg_fail(
                        ctx,
                        &row,
                        rowid,
                        leg,
                        "callback_auth_token missing or malformed",
                        "callback_auth_token_missing_or_malformed",
                        p,
                    )
                    .await;
                    return;
                }
            };
            (url, bearer, crate::fleet::CallbackKind::MarketStandard)
        }
    };

    // 3. SSRF re-validate URL. The MarketStandard CallbackKind variant
    //    ignores the roster (Wire + requester tunnels aren't roster
    //    peers) and only enforces the HTTPS + non-empty-host invariant.
    if let Err(e) = crate::fleet::validate_callback_url(
        &url,
        &kind_for_ssrf,
        &crate::fleet::FleetRoster::default(),
    ) {
        let msg = truncate(
            &format!("callback_url_validation_failed: {e}"),
            p.max_error_message_chars,
        );
        terminal_leg_fail(ctx, &row, rowid, leg, &msg, "callback_url_validation_failed", p)
            .await;
        return;
    }

    // 4. Serialize the per-leg envelope. Serialize errors are terminal
    //    (code bug; retrying won't help).
    let envelope_body = match leg {
        Leg::Content => {
            let env = build_content_envelope(&row, &async_result);
            match serde_json::to_string(&env) {
                Ok(s) => s,
                Err(e) => {
                    let err = truncate(
                        &format!("envelope serialize (content): {e}"),
                        p.max_error_message_chars,
                    );
                    terminal_leg_fail(ctx, &row, rowid, leg, &err, "envelope_parse_failed", p)
                        .await;
                    return;
                }
            }
        }
        Leg::Settlement => {
            let env = build_settlement_envelope(&row, &async_result);
            match serde_json::to_string(&env) {
                Ok(s) => s,
                Err(e) => {
                    let err = truncate(
                        &format!("envelope serialize (settlement): {e}"),
                        p.max_error_message_chars,
                    );
                    terminal_leg_fail(ctx, &row, rowid, leg, &err, "envelope_parse_failed", p)
                        .await;
                    return;
                }
            }
        }
    };

    let latency_ms_source = if row.inference_latency_ms.is_some() {
        "inference"
    } else {
        "sweep_synth"
    };

    // 5. POST. Client configured with redirect(Policy::none()) so the
    //    bearer can't leak cross-origin on a DNS compromise. Per-POST
    //    timeout bounds any single attempt.
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(p.callback_post_timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let err = truncate(&format!("http client build: {e}"), p.max_error_message_chars);
            tracing::error!(err = %err, leg = leg.label(),
                "delivery_worker: http client construction failed");
            return;
        }
    };

    let post_started = std::time::Instant::now();
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .body(envelope_body)
        .send()
        .await;
    let post_duration_ms = post_started.elapsed().as_millis() as i64;

    let (status, headers) = match response {
        Ok(resp) => (resp.status(), resp.headers().clone()),
        Err(net_err) => {
            // Network-level failure (TCP reset, DNS, timeout, TLS). Transient.
            // Display via {} not {:?} so any future reqwest upgrade that
            // serialized request headers doesn't leak the bearer.
            let err = truncate(&format!("network: {net_err}"), p.max_error_message_chars);
            transient_leg_fail(
                ctx,
                &row,
                rowid,
                leg,
                &err,
                None,
                RetryAfterSource::Absent,
                p,
                dp,
                post_duration_ms,
            )
            .await;
            return;
        }
    };

    // 6. Branch on outcome.
    if status.is_success() {
        leg_success(ctx, &row, rowid, leg, p, post_duration_ms, latency_ms_source).await;
        return;
    }

    let decision = match leg {
        Leg::Content => classify_retry_content(status),
        Leg::Settlement => classify_retry_settlement(status, &headers),
    };

    match decision {
        RetryDecision::Terminal { source } => {
            let code = status.as_u16();
            let reason =
                leg_terminal_http_reason(leg, code, &row, p, requester_delivery_jwt_ttl_secs);
            let err = truncate(&format!("terminal http {code}"), p.max_error_message_chars);
            terminal_leg_fail_with_extra(
                ctx,
                &row,
                rowid,
                leg,
                &err,
                &reason,
                p,
                Some(json!({
                    "retry_source": source,
                    "status_code": code,
                })),
            )
            .await;
        }
        RetryDecision::Retry { source } => {
            let code = status.as_u16();
            let err = truncate(&format!("http {code}"), p.max_error_message_chars);
            let (retry_after, retry_after_source) = parse_retry_after_header(&headers);
            let _ = source;
            transient_leg_fail(
                ctx,
                &row,
                rowid,
                leg,
                &err,
                retry_after,
                retry_after_source,
                p,
                dp,
                post_duration_ms,
            )
            .await;
        }
    }
}

/// 2xx path for either leg. Marks the leg's `*_posted_ok` flag with the
/// Wave 1A CAS helper; if that flip succeeds AND the OTHER leg is already
/// `*_posted_ok=1`, the row transitions `ready → delivered` and we emit
/// the summary `market_result_delivered` event. Otherwise emits the
/// per-leg success chronicle.
async fn leg_success(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    rowid: i64,
    leg: Leg,
    p: &MarketDeliveryPolicy,
    post_duration_ms: i64,
    latency_ms_source: &'static str,
) {
    let db_path = ctx.db_path.clone();
    let delivered_retention = p.delivered_retention_secs;
    // Flip leg + check both-complete in a single blocking hop. We ALSO
    // re-read the post-flip per-leg state so we can detect the
    // leg-succeeds-after-other-leg-already-terminal case (row must
    // transition to `failed` + emit failed_*_only summary — otherwise
    // the row rots in `ready` forever with one leg dead).
    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(bool, bool, (Option<String>, Option<String>, i64, i64))> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let flipped = match leg {
                Leg::Content => {
                    crate::pyramid::db::market_outbox_mark_content_posted_ok_if_ready(&conn, rowid)?
                }
                Leg::Settlement => {
                    crate::pyramid::db::market_outbox_mark_settlement_posted_ok_if_ready(
                        &conn, rowid,
                    )?
                }
            };
            // Even if we didn't flip (double-success race), re-check whether
            // both legs are now done — the other leg may have raced ahead and
            // we still want to flip the row if so.
            let both_done =
                crate::pyramid::db::market_outbox_check_both_legs_complete_and_mark_delivered(
                    &conn,
                    rowid,
                    delivered_retention,
                )?;
            // Snapshot per-leg state AFTER both-done CAS. Drives the
            // leg-succeeds-after-other-terminal detection below.
            let leg_state: (Option<String>, Option<String>, i64, i64) = conn.query_row(
                "SELECT content_last_error, settlement_last_error,
                        content_posted_ok, settlement_posted_ok
                   FROM fleet_result_outbox WHERE rowid = ?1",
                rusqlite::params![rowid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            Ok((flipped, both_done, leg_state))
        },
    )
    .await;

    let (flipped, both_done, leg_state) = match result {
        Ok(Ok(triple)) => triple,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, leg = leg.label(),
                "delivery: leg_success CAS failed");
            return;
        }
        Err(je) => {
            tracing::warn!(err = %je, leg = leg.label(),
                "delivery: leg_success join error");
            return;
        }
    };

    if !flipped {
        // CAS lost — the row already transitioned past `ready` OR the leg
        // was already flipped. Emit the CAS-lost chronicle so operator sees
        // the delivery attempt actually landed even though local state
        // raced. Counts as a successful POST from Wire/requester's view.
        emit(
            ctx,
            row,
            EVENT_MARKET_RESULT_DELIVERY_CAS_LOST,
            json!({
                "job_id": row.job_id,
                "request_id": row.request_id,
                "leg": leg.label(),
                "reason": "cas_lost_or_double_success",
                "duration_ms": post_duration_ms,
            }),
        )
        .await;
        return;
    }

    if both_done {
        // Final state — the row just transitioned `ready → delivered`.
        // Summary chronicle for the operator. Per-leg events for the
        // other leg have already been emitted on its own tick.
        let content_attempts = row.delivery_attempts + if leg == Leg::Content { 1 } else { 0 };
        let settlement_attempts =
            row.settlement_delivery_attempts + if leg == Leg::Settlement { 1 } else { 0 };
        emit(
            ctx,
            row,
            EVENT_MARKET_RESULT_DELIVERED,
            json!({
                "job_id": row.job_id,
                "request_id": row.request_id,
                "content_attempts": content_attempts,
                "settlement_attempts": settlement_attempts,
                "latency_ms": row.inference_latency_ms.unwrap_or(0),
                "latency_ms_source": latency_ms_source,
                "last_leg": leg.label(),
                "total_duration_ms": post_duration_ms,
            }),
        )
        .await;
        return;
    }

    // Not both-done. Two sub-cases:
    //   (a) Other leg is still in flight — normal per-leg success chronicle,
    //       row stays in `ready` waiting for the other leg.
    //   (b) Other leg is ALREADY terminal (has `*_last_error` stamped by a
    //       prior terminal_leg_fail call AND `*_posted_ok=0`). In this case
    //       the row must transition to `failed` with delivery_status
    //       `failed_content_only` or `failed_settlement_only` — otherwise it
    //       rots in `ready` forever because the dead leg's
    //       `*_next_attempt_at` is a 10-year-future sentinel.
    let (content_last_error, settlement_last_error, content_ok, settlement_ok) = leg_state;
    let other_leg_terminal = match leg {
        Leg::Content => settlement_last_error.is_some() && settlement_ok == 0,
        Leg::Settlement => content_last_error.is_some() && content_ok == 0,
    };

    if other_leg_terminal {
        // Row-level terminal composition: this leg succeeded, other leg
        // terminal-failed earlier. Flip row to `failed` with the correct
        // failed_*_only delivery_status.
        let terminal_status = match leg {
            Leg::Content => "failed_settlement_only",
            Leg::Settlement => "failed_content_only",
        };
        let other_error_text = match leg {
            Leg::Content => settlement_last_error.clone().unwrap_or_default(),
            Leg::Settlement => content_last_error.clone().unwrap_or_default(),
        };
        let last_err = truncate(
            &format!("{}: {}", terminal_status, other_error_text),
            p.max_error_message_chars,
        );
        let db_path = ctx.db_path.clone();
        let dispatcher = row.dispatcher_node_id.clone();
        let job_id = row.job_id.clone();
        let retention = p.failed_retention_secs;
        let cas = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = rusqlite::Connection::open(&db_path)?;
            Ok(crate::pyramid::db::market_outbox_mark_failed_with_error_cas(
                &conn,
                &dispatcher,
                &job_id,
                &last_err,
                retention,
            )?)
        })
        .await;
        if let Ok(Ok(n)) = cas {
            if n >= 1 {
                emit(
                    ctx,
                    row,
                    EVENT_MARKET_RESULT_DELIVERY_FAILED,
                    json!({
                        "job_id": row.job_id,
                        "request_id": row.request_id,
                        "delivery_status": terminal_status,
                        "terminal_leg": match leg {
                            Leg::Content => Leg::Settlement.label(),
                            Leg::Settlement => Leg::Content.label(),
                        },
                        "content_error": content_last_error,
                        "settlement_error": settlement_last_error,
                        "final_error": other_error_text,
                        "succeeded_leg": leg.label(),
                    }),
                )
                .await;
            }
        }
        // Still emit the per-leg success — the leg did succeed, and
        // dashboards should surface that the content was delivered even
        // if the other leg is dead.
        let event = match leg {
            Leg::Content => EVENT_MARKET_CONTENT_LEG_SUCCEEDED,
            Leg::Settlement => EVENT_MARKET_SETTLEMENT_LEG_SUCCEEDED,
        };
        let attempts = match leg {
            Leg::Content => row.delivery_attempts + 1,
            Leg::Settlement => row.settlement_delivery_attempts + 1,
        };
        emit(
            ctx,
            row,
            event,
            json!({
                "job_id": row.job_id,
                "request_id": row.request_id,
                "leg": leg.label(),
                "attempts": attempts,
                "duration_ms": post_duration_ms,
                "latency_ms_source": latency_ms_source,
            }),
        )
        .await;
        return;
    }

    // Other leg is still in flight — normal per-leg success chronicle.
    let event = match leg {
        Leg::Content => EVENT_MARKET_CONTENT_LEG_SUCCEEDED,
        Leg::Settlement => EVENT_MARKET_SETTLEMENT_LEG_SUCCEEDED,
    };
    let attempts = match leg {
        Leg::Content => row.delivery_attempts + 1,
        Leg::Settlement => row.settlement_delivery_attempts + 1,
    };
    emit(
        ctx,
        row,
        event,
        json!({
            "job_id": row.job_id,
            "request_id": row.request_id,
            "leg": leg.label(),
            "attempts": attempts,
            "duration_ms": post_duration_ms,
            "latency_ms_source": latency_ms_source,
        }),
    )
    .await;
}

/// Terminal HTTP reason helper. Distinguishes the 401-likely-secret-expired
/// (settlement) / 401-likely-jwt-expired (content) cases from a plain
/// `terminal_http_401`, using the row's `created_at` + the relevant TTL
/// knob.
///
/// Content leg reads the dedicated `requester_delivery_jwt_ttl_secs`
/// scalar from `wire_parameters` (§3.4; default 1800s). Settlement leg
/// uses `MarketDeliveryPolicy::ready_retention_secs` as the expiry proxy
/// per rev 0.5 semantics (Wire's callback_secret rotation is bounded by
/// the same retention horizon).
fn leg_terminal_http_reason(
    leg: Leg,
    code: u16,
    row: &crate::pyramid::db::OutboxRow,
    p: &MarketDeliveryPolicy,
    requester_delivery_jwt_ttl_secs: u64,
) -> String {
    if code != 401 {
        return format!("terminal_http_{code}");
    }
    let row_age_secs = age_secs_from_created_at(&row.created_at);
    match leg {
        Leg::Settlement => {
            if row_age_secs > p.ready_retention_secs as i64 {
                "terminal_http_401_likely_secret_expired".to_string()
            } else {
                "terminal_http_401".to_string()
            }
        }
        Leg::Content => {
            // Wire ships `requester_delivery_jwt_ttl_secs` as a scalar in
            // `wire_parameters` (contract §3.4; default =
            // fill_job_ttl_secs = 1800s when the key is absent). Use the
            // live value, not a proxy — operator tuning via
            // economic_parameter supersession takes effect immediately.
            if row_age_secs > requester_delivery_jwt_ttl_secs as i64 {
                "terminal_http_401_likely_jwt_expired".to_string()
            } else {
                "terminal_http_401".to_string()
            }
        }
    }
}

/// Per-leg transient failure path. Bumps the leg's attempt counter,
/// schedules backoff via `compute_delivery_policy.backoff_for_attempt`,
/// emits the per-leg attempt_failed chronicle. If the bump puts the leg
/// at its `max_attempts_*` budget (Q-PROTO-6), terminal-fails the leg
/// instead.
#[allow(clippy::too_many_arguments)]
async fn transient_leg_fail(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    rowid: i64,
    leg: Leg,
    err_msg: &str,
    retry_after: Option<u64>,
    retry_after_source: RetryAfterSource,
    p: &MarketDeliveryPolicy,
    dp: &ComputeDeliveryPolicy,
    _post_duration_ms: i64,
) {
    let prior_attempts = match leg {
        Leg::Content => row.delivery_attempts,
        Leg::Settlement => row.settlement_delivery_attempts,
    };
    let new_attempts = prior_attempts + 1;

    let leg_budget = match leg {
        Leg::Content => dp.max_attempts_content,
        Leg::Settlement => dp.max_attempts_settlement,
    } as i64;

    // Budget exhausted → terminal with per-leg reason.
    if new_attempts >= leg_budget {
        let reason = match leg {
            Leg::Content => "max_attempts_content",
            Leg::Settlement => "max_attempts_settlement",
        };
        let err = truncate(
            &format!("{err_msg} (max_attempts_{} exceeded)", leg.label()),
            p.max_error_message_chars,
        );
        terminal_leg_fail(ctx, row, rowid, leg, &err, reason, p).await;
        return;
    }

    // Compute backoff: Retry-After header if present, else policy schedule.
    let schedule_secs = dp.backoff_for_attempt(new_attempts as u32) as u64;
    let backoff_secs = retry_after.unwrap_or(schedule_secs);

    let db_path = ctx.db_path.clone();
    let err_copy = err_msg.to_string();
    let bump_result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        match leg {
            Leg::Content => crate::pyramid::db::market_outbox_bump_content_attempt_with_backoff(
                &conn,
                rowid,
                backoff_secs,
                &err_copy,
            )?,
            Leg::Settlement => {
                crate::pyramid::db::market_outbox_bump_settlement_attempt_with_backoff(
                    &conn,
                    rowid,
                    backoff_secs,
                    &err_copy,
                )?
            }
        };
        Ok(())
    })
    .await;
    if let Ok(Err(e)) = bump_result {
        tracing::warn!(err = %e, leg = leg.label(), "delivery: bump_attempt failed");
    }

    let event = match leg {
        Leg::Content => EVENT_MARKET_CONTENT_DELIVERY_ATTEMPT_FAILED,
        Leg::Settlement => EVENT_MARKET_SETTLEMENT_DELIVERY_ATTEMPT_FAILED,
    };
    emit(
        ctx,
        row,
        event,
        json!({
            "job_id": row.job_id,
            "request_id": row.request_id,
            "leg": leg.label(),
            "attempt": new_attempts,
            "error": err_msg,
            "backoff_secs": backoff_secs,
            "retry_after_source": retry_after_source_label(&retry_after_source),
        }),
    )
    .await;
}

/// Per-leg terminal failure. Marks the row terminal IFF both legs are now
/// terminal; otherwise records the leg's terminal state on its error
/// column and clears its lease — the row stays `ready` so the other leg
/// keeps independently making progress per spec line 71.
async fn terminal_leg_fail(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    rowid: i64,
    leg: Leg,
    err_msg: &str,
    reason: &str,
    p: &MarketDeliveryPolicy,
) {
    terminal_leg_fail_with_extra(ctx, row, rowid, leg, err_msg, reason, p, None).await;
}

/// Full-fat terminal-leg-fail — allows extra metadata (status_code,
/// retry_source) to be threaded into the chronicle event for the HTTP
/// terminal code path without double-emitting.
async fn terminal_leg_fail_with_extra(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    rowid: i64,
    leg: Leg,
    err_msg: &str,
    reason: &str,
    p: &MarketDeliveryPolicy,
    extra_metadata: Option<serde_json::Value>,
) {
    // 1. Mark the leg's terminal state: flip its `*_last_error`, clear its
    //    lease, and stamp a far-future `*_next_attempt_at` so this leg is
    //    never reclaimed (even if the row stays `ready` for the other
    //    leg's sake). We reuse the `bump_*_attempt_with_backoff` helper
    //    with a huge backoff as the "leg is dead" sentinel — the leg-
    //    specific terminal state vocabulary lives implicitly in
    //    `*_last_error` being set AND never being eligible to claim.
    const LEG_DEAD_BACKOFF_SECS: u64 = 60 * 60 * 24 * 365 * 10; // 10y ≈ "never"

    let db_path = ctx.db_path.clone();
    let err_copy = err_msg.to_string();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        match leg {
            Leg::Content => crate::pyramid::db::market_outbox_bump_content_attempt_with_backoff(
                &conn,
                rowid,
                LEG_DEAD_BACKOFF_SECS,
                &err_copy,
            )?,
            Leg::Settlement => {
                crate::pyramid::db::market_outbox_bump_settlement_attempt_with_backoff(
                    &conn,
                    rowid,
                    LEG_DEAD_BACKOFF_SECS,
                    &err_copy,
                )?
            }
        };
        Ok(())
    })
    .await;

    // 2. Decide row-level composition: is the OTHER leg already terminal?
    //    We peek at the freshly-updated row.
    let db_path = ctx.db_path.clone();
    let leg_state = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(Option<String>, Option<String>, i64, i64)> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let tup: (Option<String>, Option<String>, i64, i64) = conn.query_row(
                "SELECT content_last_error, settlement_last_error,
                        content_posted_ok, settlement_posted_ok
                   FROM fleet_result_outbox WHERE rowid = ?1",
                rusqlite::params![rowid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            Ok(tup)
        },
    )
    .await
    .ok()
    .and_then(|r| r.ok());

    let (content_terminal, settlement_terminal, final_delivery_status) = match leg_state {
        Some((content_err, settlement_err, content_ok, settlement_ok)) => {
            let c_term = content_err.is_some() && content_ok == 0;
            let s_term = settlement_err.is_some() && settlement_ok == 0;
            let status = match (c_term, s_term, content_ok, settlement_ok) {
                (true, true, _, _) => Some("failed_both"),
                (true, false, _, 1) => Some("failed_content_only"),
                (false, true, 1, _) => Some("failed_settlement_only"),
                _ => None,
            };
            (c_term, s_term, status)
        }
        None => (false, false, None),
    };

    // 3. Emit per-leg terminal chronicle.
    let per_leg_event = match leg {
        Leg::Content => EVENT_MARKET_CONTENT_DELIVERY_FAILED,
        Leg::Settlement => EVENT_MARKET_SETTLEMENT_DELIVERY_FAILED,
    };
    let attempts = match leg {
        Leg::Content => row.delivery_attempts + 1,
        Leg::Settlement => row.settlement_delivery_attempts + 1,
    };
    let mut per_leg_meta = json!({
        "job_id": row.job_id,
        "request_id": row.request_id,
        "leg": leg.label(),
        "attempts": attempts,
        "final_error": err_msg,
        "reason": reason,
    });
    if let Some(extra) = extra_metadata {
        if let (serde_json::Value::Object(ref mut base), serde_json::Value::Object(extra_map)) =
            (&mut per_leg_meta, extra)
        {
            for (k, v) in extra_map {
                base.insert(k, v);
            }
        }
    }
    emit(ctx, row, per_leg_event, per_leg_meta).await;

    // 4. If BOTH legs are terminal, flip the row to failed with the
    //    `failed_both` terminal status and emit the row-level summary.
    //    If only this leg is terminal and the OTHER leg is already 2xx,
    //    flip to `failed_content_only` / `failed_settlement_only`.
    if let Some(terminal_status) = final_delivery_status {
        let db_path = ctx.db_path.clone();
        let last_err = format!("{}: {}", terminal_status, err_msg);
        let dispatcher = row.dispatcher_node_id.clone();
        let job_id = row.job_id.clone();
        let retention = p.failed_retention_secs;
        let cas = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = rusqlite::Connection::open(&db_path)?;
            Ok(crate::pyramid::db::market_outbox_mark_failed_with_error_cas(
                &conn,
                &dispatcher,
                &job_id,
                &last_err,
                retention,
            )?)
        })
        .await;
        if let Ok(Ok(n)) = cas {
            if n >= 1 && terminal_status == "failed_both" {
                emit(
                    ctx,
                    row,
                    EVENT_MARKET_RESULT_DELIVERY_FAILED,
                    json!({
                        "job_id": row.job_id,
                        "request_id": row.request_id,
                        "delivery_status": terminal_status,
                        "content_terminal": content_terminal,
                        "settlement_terminal": settlement_terminal,
                        "content_error": row.content_last_error,
                        "settlement_error": row.settlement_last_error,
                    }),
                )
                .await;
            } else if n >= 1 {
                // Mixed-terminal (failed_content_only / failed_settlement_only)
                // — row is terminal but only one leg died. Reuse the same
                // row-level summary event with a distinct delivery_status.
                emit(
                    ctx,
                    row,
                    EVENT_MARKET_RESULT_DELIVERY_FAILED,
                    json!({
                        "job_id": row.job_id,
                        "request_id": row.request_id,
                        "delivery_status": terminal_status,
                        "terminal_leg": leg.label(),
                        "final_error": err_msg,
                        "reason": reason,
                    }),
                )
                .await;
            }
        }
    }
}

// ── Chronicle emission helpers ──────────────────────────────────────────────

fn age_secs_from_created_at(created_at: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%dT%H:%M:%S%.f"))
        .map(|ndt| {
            let created = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                ndt,
                chrono::Utc,
            );
            chrono::Utc::now().signed_duration_since(created).num_seconds()
        })
        .unwrap_or(0)
}

async fn emit(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    event_type: &'static str,
    metadata: serde_json::Value,
) {
    let db_path = ctx.db_path.clone();
    let job_path = format!("market-recv:{}", row.job_id);
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let ctx_ev = ChronicleEventContext::minimal(&job_path, event_type, SOURCE_MARKET)
            .with_metadata(metadata);
        let _ = record_event(&conn, &ctx_ev);
        Ok(())
    })
    .await;
}

async fn record_lifecycle_event(
    ctx: &DeliveryContext,
    event_type: &'static str,
    metadata: serde_json::Value,
) {
    let db_path = ctx.db_path.clone();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let job_path = format!("market/delivery/{}", chrono::Utc::now().timestamp());
        let ctx_ev = ChronicleEventContext::minimal(&job_path, event_type, SOURCE_MARKET)
            .with_metadata(metadata);
        let _ = record_event(&conn, &ctx_ev);
        Ok(())
    })
    .await;
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::OutboxRow;
    use crate::pyramid::market_dispatch::MarketDispatchResponse;

    fn sample_row() -> OutboxRow {
        OutboxRow {
            dispatcher_node_id: "wire-platform".into(),
            job_id: "4f93e9f4-5e7a-4a2a-9a6c-6d0c9c5d0b9a".into(),
            status: "ready".into(),
            callback_url: "https://wire.example/api/v1/compute/settlement/playful%2F109%2F7".into(),
            result_json: Some(
                r#"{"kind":"Success","data":{"content":"hi","prompt_tokens":10,"completion_tokens":3,"model":"gemma4:26b","provider_model":"gemma4:26b","finish_reason":"stop"}}"#
                    .into(),
            ),
            delivery_attempts: 0,
            last_attempt_at: None,
            expires_at: "2099-01-01 00:00:00".into(),
            created_at: "2026-04-20 16:00:00".into(),
            callback_auth_token: Some("abcDEFghi-_123".into()),
            delivery_lease_until: None,
            delivery_next_attempt_at: None,
            inference_latency_ms: Some(450),
            request_id: Some("req-abc-123".into()),
            requester_callback_url: Some("https://newsbleach.example/v1/compute/job-result".into()),
            requester_delivery_jwt: Some("eyJhbGciOiJFZERTQSJ9.payload.sig".into()),
            content_posted_ok: 0,
            content_lease_until: None,
            content_next_attempt_at: None,
            content_last_error: None,
            settlement_posted_ok: 0,
            settlement_delivery_attempts: 0,
            settlement_lease_until: None,
            settlement_next_attempt_at: None,
            settlement_last_error: None,
        }
    }

    fn success_result() -> MarketAsyncResult {
        MarketAsyncResult::Success(MarketDispatchResponse {
            content: "the answer".into(),
            prompt_tokens: Some(7),
            completion_tokens: Some(3),
            model: "gemma4:26b".into(),
            finish_reason: Some("stop".into()),
            provider_model: Some("gemma4:26b".into()),
        })
    }

    // ── Leg enum ─────────────────────────────────────────────────────────────

    #[test]
    fn leg_labels_are_stable_snake_case() {
        // Dashboards + chronicle queries key on these literals.
        assert_eq!(Leg::Content.label(), "content");
        assert_eq!(Leg::Settlement.label(), "settlement");
    }

    // ── Content envelope (§2.6) ──────────────────────────────────────────────

    #[test]
    fn content_envelope_success_includes_content() {
        let row = sample_row();
        let env = build_content_envelope(&row, &success_result());
        let json_str = serde_json::to_string(&env).unwrap();
        assert!(json_str.contains("\"content\":\"the answer\""), "got: {json_str}");
        assert!(json_str.contains("\"input_tokens\":7"));
        assert!(json_str.contains("\"output_tokens\":3"));
        assert!(json_str.contains("\"latency_ms\":450"));
        assert!(json_str.contains("\"type\":\"success\""));
    }

    #[test]
    fn content_envelope_failure_mirrors_settlement_shape() {
        // D4: failure variant goes to BOTH legs with identical shape.
        let row = sample_row();
        let err = MarketAsyncResult::Error("worker heartbeat lost".into());
        let content = build_content_envelope(&row, &err);
        let settlement = build_settlement_envelope(&row, &err);
        let cj = serde_json::to_value(&content).unwrap();
        let sj = serde_json::to_value(&settlement).unwrap();
        // Identical: type, job_id, error.code, error.message.
        assert_eq!(cj, sj,
            "failure envelope must be identical across legs per D4; content={cj} settlement={sj}");
    }

    #[test]
    fn content_envelope_none_tokens_map_to_zero() {
        let row = sample_row();
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "".into(),
            prompt_tokens: None,
            completion_tokens: None,
            model: "gemma4:26b".into(),
            finish_reason: None,
            provider_model: None,
        });
        let env = build_content_envelope(&row, &result);
        let json_str = serde_json::to_string(&env).unwrap();
        assert!(json_str.contains("\"input_tokens\":0"));
        assert!(json_str.contains("\"output_tokens\":0"));
    }

    // ── Settlement envelope (§2.3 — content MUST NOT appear) ─────────────────

    #[test]
    fn settlement_envelope_never_serializes_content() {
        // Contract §2.3: "result.content MUST NOT appear on this endpoint".
        // Structural guard — SettlementResult has no `content` field.
        let row = sample_row();
        let env = build_settlement_envelope(&row, &success_result());
        let json_str = serde_json::to_string(&env).unwrap();
        assert!(
            !json_str.contains("content"),
            "settlement envelope MUST NOT contain the string 'content' anywhere: {json_str}"
        );
        // Sanity: other §2.3 required fields are present.
        assert!(json_str.contains("\"input_tokens\":7"));
        assert!(json_str.contains("\"output_tokens\":3"));
        assert!(json_str.contains("\"model_used\":"));
        assert!(json_str.contains("\"latency_ms\":"));
        assert!(json_str.contains("\"type\":\"success\""));
    }

    #[test]
    fn settlement_envelope_failure_serializes_snake_case_type() {
        // Use a message that doesn't substring-match any of the pinned
        // §2.3 codes (`classify_failure_code` looks for "heartbeat",
        // "timeout", "oom", "invalid messages"). A plain "kaboom" would
        // sneak into the oom bucket via "oom" substring, so picking a
        // phrase that falls through to the catch-all is intentional.
        let row = sample_row();
        let env =
            build_settlement_envelope(&row, &MarketAsyncResult::Error("garbled response".into()));
        let json_str = serde_json::to_string(&env).unwrap();
        assert!(json_str.contains("\"type\":\"failure\""), "got: {json_str}");
        assert!(
            json_str.contains("\"code\":\"model_error\""),
            "expected code=model_error in settlement failure envelope; got: {json_str}"
        );
    }

    // ── §10.5 UUID invariant (both adapters) ─────────────────────────────────

    #[test]
    fn both_envelopes_emit_uuid_job_id_not_handle_path() {
        let row = sample_row();
        let content = build_content_envelope(&row, &success_result());
        let settlement = build_settlement_envelope(&row, &success_result());
        let cj = serde_json::to_string(&content).unwrap();
        let sj = serde_json::to_string(&settlement).unwrap();
        for body in [&cj, &sj] {
            assert!(
                body.contains("4f93e9f4-5e7a-4a2a-9a6c-6d0c9c5d0b9a"),
                "body.job_id must be the UUID; got: {body}"
            );
            assert!(
                !body.contains("playful%2F109%2F7"),
                "body.job_id MUST NOT leak handle-path: {body}"
            );
        }
    }

    // ── Failure classifier ─────────────────────────────────────────────────

    #[test]
    fn classify_failure_codes() {
        assert_eq!(classify_failure_code("worker heartbeat lost"), "worker_heartbeat_lost");
        assert_eq!(classify_failure_code("Ollama timeout after 30s"), "model_timeout");
        assert_eq!(classify_failure_code("Request timed out"), "model_timeout");
        assert_eq!(classify_failure_code("Out of memory on GPU"), "oom");
        assert_eq!(classify_failure_code("messages: invalid role 'assistant'"), "invalid_messages");
        assert_eq!(classify_failure_code("garbled LLM response"), "model_error");
    }

    // ── Retry classification — per-leg split ─────────────────────────────────

    #[test]
    fn classify_retry_settlement_reads_x_wire_retry_header() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Wire-Retry", "never".parse().unwrap());
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        match classify_retry_settlement(status, &headers) {
            RetryDecision::Terminal { source } => assert_eq!(source, "x_wire_retry_never"),
            _ => panic!("explicit X-Wire-Retry: never must produce Terminal"),
        }
    }

    #[test]
    fn classify_retry_content_ignores_x_wire_retry_header() {
        // Content leg: requester HTTP doesn't standardize X-Wire-Retry.
        // If the requester happens to emit it, we ignore it and decide
        // purely on the HTTP status. 500 → Retry (not in terminal set).
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        match classify_retry_content(status) {
            RetryDecision::Retry { .. } => {}
            _ => panic!("500 on content leg must be Retry (header ignored)"),
        }
    }

    #[test]
    fn classify_retry_fallback_terminal_codes_both_legs() {
        // Both classifiers treat the contract-pinned terminal HTTP codes
        // as Terminal under fallback.
        let headers = HeaderMap::new();
        for code in TERMINAL_HTTP_CODES_FALLBACK {
            let status = reqwest::StatusCode::from_u16(*code).unwrap();
            assert!(
                matches!(classify_retry_settlement(status, &headers), RetryDecision::Terminal { .. }),
                "settlement: HTTP {code} must be terminal"
            );
            assert!(
                matches!(classify_retry_content(status), RetryDecision::Terminal { .. }),
                "content: HTTP {code} must be terminal"
            );
        }
    }

    #[test]
    fn classify_retry_settlement_unknown_header_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Wire-Retry", "schedule-next-eclipse".parse().unwrap());
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        match classify_retry_settlement(status, &headers) {
            RetryDecision::Retry { source } => assert_eq!(source, "http_code_fallback"),
            _ => panic!("unknown header must fall through to HTTP-code decision"),
        }
    }

    // ── Retry-After parsing ────────────────────────────────────────────────

    #[test]
    fn retry_after_integer_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "5".parse().unwrap());
        let (secs, src) = parse_retry_after_header(&headers);
        assert_eq!(secs, Some(5));
        assert!(matches!(src, RetryAfterSource::HeaderSeconds));
    }

    #[test]
    fn retry_after_http_date_future() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2099 07:28:00 GMT".parse().unwrap(),
        );
        let (secs, src) = parse_retry_after_header(&headers);
        assert!(secs.is_some());
        assert!(matches!(src, RetryAfterSource::HeaderHttpDate));
    }

    #[test]
    fn retry_after_invalid_value() {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "not-a-valid-thing".parse().unwrap());
        let (secs, src) = parse_retry_after_header(&headers);
        assert_eq!(secs, None);
        assert!(matches!(src, RetryAfterSource::HeaderInvalid));
    }

    #[test]
    fn retry_after_absent() {
        let headers = HeaderMap::new();
        let (secs, src) = parse_retry_after_header(&headers);
        assert_eq!(secs, None);
        assert!(matches!(src, RetryAfterSource::Absent));
    }

    // ── Bearer validation ──────────────────────────────────────────────────

    #[test]
    fn bearer_accepts_base64url_and_jwt() {
        // Base64url + padding (settlement-leg bearers).
        assert!(is_valid_bearer("abcDEFghi123_-"));
        assert!(is_valid_bearer("abcDEFghi123="));
        assert!(is_valid_bearer("a"));
        // JWT shape (content-leg bearers). Three base64url segments
        // separated by dots.
        assert!(is_valid_bearer("eyJhbGciOiJFZERTQSJ9.payload.sig"));
    }

    #[test]
    fn bearer_rejects_empty_too_long_and_control_chars() {
        assert!(!is_valid_bearer(""));
        let too_long: String = "a".repeat(MAX_TOKEN_LEN + 1);
        assert!(!is_valid_bearer(&too_long));
        assert!(!is_valid_bearer("abc\r\nhost: evil"));
        assert!(!is_valid_bearer("abc def"));
        assert!(!is_valid_bearer("abc\0def"));
    }

    // ── Truncate ────────────────────────────────────────────────────────────

    #[test]
    fn truncate_respects_char_boundary() {
        let input = "αβγδε".repeat(10);
        let t = truncate(&input, 5);
        assert_eq!(t.chars().count(), 6); // 5 chars + "…"
        assert!(t.ends_with("…"));
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("short", 100), "short");
    }

    // ── CallbackAuth debug redaction ────────────────────────────────────────

    #[test]
    fn callback_auth_debug_redacts_token() {
        let auth = crate::pyramid::market_dispatch::CallbackAuth {
            kind: "bearer".into(),
            token: "SHOULD_NEVER_APPEAR_IN_LOGS_12345".into(),
        };
        let debug_str = format!("{auth:?}");
        assert!(!debug_str.contains("SHOULD_NEVER_APPEAR_IN_LOGS_12345"),
            "token must be redacted in Debug; got: {debug_str}");
        assert!(debug_str.contains("<redacted>"));
        assert!(debug_str.contains("bearer"));
    }

    // ── Terminal HTTP reason composition ────────────────────────────────────

    #[test]
    fn leg_terminal_http_reason_401_settlement_old_row() {
        let mut row = sample_row();
        // Fake old row — created far in the past so age > ready_retention.
        row.created_at = "2020-01-01 00:00:00".into();
        let p = MarketDeliveryPolicy::default();
        let reason = leg_terminal_http_reason(Leg::Settlement, 401, &row, &p, 1800);
        assert_eq!(reason, "terminal_http_401_likely_secret_expired");
    }

    #[test]
    fn leg_terminal_http_reason_401_content_old_row() {
        let mut row = sample_row();
        row.created_at = "2020-01-01 00:00:00".into();
        let p = MarketDeliveryPolicy::default();
        let reason = leg_terminal_http_reason(Leg::Content, 401, &row, &p, 1800);
        assert_eq!(reason, "terminal_http_401_likely_jwt_expired");
    }

    #[test]
    fn leg_terminal_http_reason_fresh_row_is_plain_401() {
        // Row created just now → too young to blame secret expiry.
        let row = sample_row(); // created_at = 2026-04-20 (recent)
        let p = MarketDeliveryPolicy::default();
        // MarketDeliveryPolicy::contract_defaults might not exist; fall back
        // to a constructed policy by reading the field. Use a liberal
        // ready_retention to avoid flakiness.
        let mut p2 = p.clone();
        p2.ready_retention_secs = 24 * 60 * 60 * 365 * 100; // 100y → any row is "fresh"
        let reason = leg_terminal_http_reason(Leg::Settlement, 401, &row, &p2, 1800);
        assert_eq!(reason, "terminal_http_401");
    }

    #[test]
    fn leg_terminal_http_reason_non_401_passthrough() {
        let row = sample_row();
        let p = MarketDeliveryPolicy::default();
        assert_eq!(
            leg_terminal_http_reason(Leg::Content, 404, &row, &p, 1800),
            "terminal_http_404"
        );
        assert_eq!(
            leg_terminal_http_reason(Leg::Settlement, 413, &row, &p, 1800),
            "terminal_http_413"
        );
    }

    #[test]
    fn leg_terminal_http_reason_content_uses_jwt_ttl_scalar_not_ready_retention() {
        // Pillar 37 check: the content-leg 401 heuristic must consume
        // `requester_delivery_jwt_ttl_secs` (scalar from wire_parameters),
        // NOT piggyback on `ready_retention_secs` (which covers a different
        // semantic). Row is 10 minutes old; jwt_ttl=5min → should classify
        // as `likely_jwt_expired`. With jwt_ttl=60min (same row) → plain 401.
        // This test would FAIL under the rev-0.6.1-wave-2A proxy-via-
        // ready_retention_secs implementation (both cases would have
        // returned the same answer).
        let mut row = sample_row();
        let now = chrono::Utc::now();
        let ten_min_ago = now - chrono::Duration::minutes(10);
        row.created_at = ten_min_ago.format("%Y-%m-%d %H:%M:%S").to_string();
        let p = MarketDeliveryPolicy::default();

        // jwt_ttl = 5min (300s); row age ~= 10min → age > ttl → likely_expired.
        let reason_short = leg_terminal_http_reason(Leg::Content, 401, &row, &p, 300);
        assert_eq!(
            reason_short, "terminal_http_401_likely_jwt_expired",
            "age > ttl must classify as jwt-expired"
        );

        // jwt_ttl = 60min (3600s); row age ~= 10min → age < ttl → plain 401.
        let reason_long = leg_terminal_http_reason(Leg::Content, 401, &row, &p, 3600);
        assert_eq!(
            reason_long, "terminal_http_401",
            "age < ttl must classify as plain 401"
        );
    }
}
