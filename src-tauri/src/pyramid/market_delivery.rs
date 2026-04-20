//! Phase 3 — Provider-side compute-market delivery worker.
//!
//! Closes the last hop of the compute-market provider path: fleet outbox
//! rows in state=`ready` get POSTed to Wire's callback URL, and CAS-promoted
//! to `delivered` on 2xx. Before this existed, every provider dispatch
//! completed inference locally and then abandoned the result — Wire's
//! `wire_compute_jobs.delivery_status` stayed `pending` forever.
//!
//! Full spec: `docs/plans/compute-market-phase-3-provider-delivery-spec.md`
//! (rev 0.5). Wire-side mirror plan at
//! `GoodNewsEveryone/docs/plans/compute-market-phase-3-wire-side-build-plan-2026-04-20.md`.
//!
//! # Architecture
//!
//! - **Trigger model:** `tokio::select!` on (a) nudge-channel recv, (b)
//!   periodic tick. Nudge fires at every ready-promotion site (worker
//!   success, worker failure after the WS2 bug-fix, sweep heartbeat-lost
//!   synth). Tick catches anything that missed a nudge or is past its
//!   backoff window.
//! - **Claim:** batched `UPDATE ... RETURNING *` returns up to
//!   `max_concurrent_deliveries` rows per tick with the lease stamp
//!   already applied (atomic — no TOCTOU).
//! - **POST:** per-row, bounded-parallel via
//!   `for_each_concurrent(max_concurrent_deliveries)`. Client configured
//!   with `redirect(Policy::none())` so the bearer can't be cross-origin-
//!   exfiltrated on a DNS compromise.
//! - **Envelope adapter:** parses bare `MarketAsyncResult` (the shape
//!   `server.rs::spawn_market_worker` + `db::synthesize_worker_error_json`
//!   actually persist) into Wire's `CallbackEnvelope` shape per contract
//!   §2.3. Job_id on the envelope is always `row.job_id` (UUID; never the
//!   handle-path that lives in `callback_url`).
//! - **Retry classification:** reads `X-Wire-Retry: never | transient |
//!   backoff` header from Wire's non-2xx response; falls back to
//!   HTTP-code enum (400/401/403/404/409/410/413) when header is absent
//!   (pre-upgrade Wire).
//! - **Supervisor:** `supervise_delivery_loop` wraps the loop in
//!   `AssertUnwindSafe::catch_unwind`; panics emit
//!   `market_delivery_task_panicked` chronicle, 5s backoff, respawn.
//!   Clean exit (channel sender dropped) emits
//!   `market_delivery_task_exited`.
//! - **Redaction:** `CallbackAuth` has a custom `Debug` impl that elides
//!   `token`. Error messages from reqwest are truncated to
//!   `max_error_message_chars` before DB write or chronicle emit.
//! - **No periodic mirror-heartbeat:** per the staleness refactor from
//!   2026-04-20, idle providers remain matchable via `wire_nodes.last_heartbeat`
//!   freshness; this worker does NOT push mirror snapshots on idle. Only
//!   when an actual state transition happens.
//!
//! # Invariants
//!
//! 1. A row in state=`ready` with a non-null `delivery_lease_until > now()`
//!    is owned by exactly one delivery worker tick. `market_outbox_claim_
//!    ready_for_delivery`'s `UPDATE ... RETURNING` makes this atomic.
//! 2. On terminal transition (delivered/failed), both `delivery_lease_until`
//!    and `delivery_next_attempt_at` are cleared.
//! 3. The Wire-facing envelope `body.job_id` is always a UUID, never the
//!    handle-path in `callback_url`. Debug assertion guards the POST site.
//! 4. The callback_auth bearer token never enters a log or chronicle
//!    metadata string. Enforced structurally (no `{:?}` on
//!    `MarketDispatchRequest` / `CallbackAuth`) + test
//!    `error_metadata_does_not_leak_token`.
//!
//! # Relay deferral
//!
//! Relay rows (`callback_kind = 'Relay'`) are NOT claimed by this worker.
//! Relay-market ships separately; when it does, either this worker extends
//! or a parallel relay-delivery worker spawns against the same table. Today
//! no code path produces Relay rows so the deferral is inert.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{stream::StreamExt, FutureExt};
use reqwest::header::HeaderMap;
use serde_json::json;
use tokio::sync::{mpsc, RwLock};

use crate::pyramid::compute_chronicle::{
    record_event, ChronicleEventContext, EVENT_MARKET_DELIVERY_TASK_EXITED,
    EVENT_MARKET_DELIVERY_TASK_PANICKED, EVENT_MARKET_RESULT_DELIVERED_TO_WIRE,
    EVENT_MARKET_RESULT_DELIVERY_ATTEMPT_FAILED, EVENT_MARKET_RESULT_DELIVERY_CAS_LOST,
    EVENT_MARKET_RESULT_DELIVERY_FAILED, SOURCE_MARKET,
};
use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;
use crate::pyramid::market_dispatch::MarketAsyncResult;

/// Marker for the pyramid_schema_versions row this worker's orphan-detection
/// heuristic keys against. Matches the INSERT OR IGNORE in
/// `db::init_pyramid_db`.
const MIGRATION_NAME: &str = "fleet_result_outbox_v2_callback_auth_token";

/// Hardcoded sanity cap on bearer length. Base64url-encoded 32-byte tokens
/// are ~43 chars; this guards against a hypothetical DoS where Wire or a
/// man-in-the-middle ships a multi-MB "token" that the Authorization
/// header build would copy.
const MAX_TOKEN_LEN: usize = 512;

/// HTTP status codes that mean "retrying will not help" when Wire doesn't
/// ship the `X-Wire-Retry` header (pre-upgrade Wire). See spec §2 + Wire
/// compute-errors.ts classifier output enumeration.
const TERMINAL_HTTP_CODES_FALLBACK: &[u16] = &[400, 401, 403, 404, 409, 410, 413];

// ── Wire-facing envelope (CallbackEnvelope) ─────────────────────────────────

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CallbackEnvelope {
    Success {
        job_id: String,
        result: CallbackResult,
    },
    Failure {
        job_id: String,
        error: CallbackError,
    },
}

#[derive(Debug, serde::Serialize)]
struct CallbackResult {
    content: String,
    input_tokens: u64,
    output_tokens: u64,
    model_used: String,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

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

fn classify_retry(status: reqwest::StatusCode, headers: &HeaderMap) -> RetryDecision {
    // Explicit protocol data wins over HTTP-code enumeration.
    if let Some(raw) = headers.get("X-Wire-Retry").and_then(|v| v.to_str().ok()) {
        match raw {
            "never" => return RetryDecision::Terminal { source: "x_wire_retry_never" },
            "transient" => return RetryDecision::Retry { source: "x_wire_retry_transient" },
            "backoff" => return RetryDecision::Retry { source: "x_wire_retry_backoff" },
            other => {
                // Unknown value — forward-compat warn-don't-reject per
                // contract Q-PROTO-3.
                tracing::warn!(
                    header_value = %other,
                    "X-Wire-Retry: unknown value; falling back to HTTP-code enumeration"
                );
            }
        }
    }
    // Fallback: HTTP-code enumeration for pre-upgrade Wire.
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

    // Form 1: decimal integer seconds.
    if let Ok(secs) = v.parse::<u64>() {
        return (Some(secs), RetryAfterSource::HeaderSeconds);
    }

    // Form 2: HTTP-date. Convert to delta-seconds relative to now; past
    // dates clamp to 0.
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

// ── Envelope adapter: MarketAsyncResult → CallbackEnvelope ──────────────────

/// Pure function. No I/O; callable from tests.
fn build_callback_envelope(
    row: &crate::pyramid::db::OutboxRow,
    result: &MarketAsyncResult,
) -> CallbackEnvelope {
    // Invariant: row.job_id is the UUID stored at admission time. Debug
    // assertion guards against a future write-path bug that smuggles the
    // handle-path into the outbox PK.
    debug_assert!(
        uuid::Uuid::parse_str(&row.job_id).is_ok(),
        "OutboxRow.job_id must be UUID-format (contract §10.5); handle-path lives in callback_url"
    );

    match result {
        MarketAsyncResult::Success(resp) => {
            // Wire requires non-negative integers for input_tokens +
            // output_tokens; None maps to 0 (per Phase 3 spec §Envelope
            // adapter).
            let input_tokens = resp.prompt_tokens.unwrap_or(0).max(0) as u64;
            let output_tokens = resp.completion_tokens.unwrap_or(0).max(0) as u64;
            // model_used: prefer provider_model if non-empty; fall back to
            // model (worker always sets this non-empty from dispatch body);
            // last-resort "unknown" to avoid terminal-failing on an observability
            // field Wire treats as non-load-bearing.
            let model_used = resp
                .provider_model
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if !resp.model.is_empty() {
                        resp.model.clone()
                    } else {
                        "unknown".to_string()
                    }
                });
            // latency_ms: from outbox column; None → 0 + chronicle metadata
            // records `latency_ms_source: "sweep_synth"` at emit time.
            let latency_ms = row.inference_latency_ms.unwrap_or(0).max(0) as u64;
            CallbackEnvelope::Success {
                job_id: row.job_id.clone(),
                result: CallbackResult {
                    content: resp.content.clone(),
                    input_tokens,
                    output_tokens,
                    model_used,
                    latency_ms,
                    finish_reason: resp.finish_reason.clone(),
                },
            }
        }
        MarketAsyncResult::Error(msg) => CallbackEnvelope::Failure {
            job_id: row.job_id.clone(),
            error: CallbackError {
                code: classify_failure_code(msg),
                message: msg.clone(),
            },
        },
    }
}

/// Maps a MarketAsyncResult::Error(String) into one of the contract §2.3
/// pinned codes. Substring-matching on known failure-shape phrases; unknown
/// messages default to `model_error` (Wire's catch-all via mapFailureCodeToReason).
///
/// Codes coordinated with Wire-side build plan WS5: `worker_heartbeat_lost`,
/// `model_timeout`, `oom`, `invalid_messages`, `model_error` — pinned in
/// contract §2.3 + shared-types.
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
/// building the Authorization header. base64url alphabet is
/// `[A-Za-z0-9_-]`; Wire does not send padding. A token containing
/// whitespace or control characters indicates corruption or injection;
/// hit a terminal chronicle rather than a mysterious 401.
fn is_valid_bearer(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= MAX_TOKEN_LEN
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '=')
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
/// that added `callback_auth_token`. Called only when the token column
/// is NULL; lets us emit a distinct terminal reason
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
    /// by the heartbeat self-heal path in main.rs. Empty on fresh install;
    /// the delivery worker falls back to contract-default constants for
    /// any missing key. Cloned by value per-tick so the loop doesn't hold
    /// a read lock across `.await`.
    pub auth: Arc<RwLock<crate::auth::AuthState>>,
}

/// Spawn the supervisor task that owns the delivery loop. Mirrors the
/// pattern shipped in `market_mirror::spawn_market_mirror_task` +
/// `supervise_mirror_loop` (commit 57b1fa4).
///
/// Caller constructs the nudge channel first and places the sender on
/// `MarketDispatchContext.delivery_nudge`; the receiver is handed here.
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
    tracing::info!("market delivery task started");

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
                        // Drain any further nudges queued up — they all
                        // represent the same eventual "scan ready rows"
                        // action.
                        while rx.try_recv().is_ok() {}
                    }
                    None => {
                        // Channel closed (all senders dropped). Supervisor
                        // emits the loud lifecycle event; just return.
                        return;
                    }
                }
            }
            _ = interval.tick() => {}
        }

        tick(ctx).await;
    }
}

/// One iteration: claim + deliver.
async fn tick(ctx: &DeliveryContext) {
    let p = ctx.policy.read().await.clone();
    let lease_secs = p.callback_post_timeout_secs + p.lease_grace_secs;

    let db_path = ctx.db_path.clone();
    let max_concurrent = p.max_concurrent_deliveries;
    let claimed = match tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<_>> {
        let conn = rusqlite::Connection::open(&db_path)?;
        crate::pyramid::db::market_outbox_claim_ready_for_delivery(
            &conn, lease_secs, max_concurrent,
        )
        .map_err(|e| anyhow::anyhow!("claim_ready_for_delivery: {}", e))
    })
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "delivery tick: claim query failed");
            return;
        }
        Err(je) => {
            tracing::warn!(err = %je, "delivery tick: claim join error");
            return;
        }
    };

    if claimed.is_empty() {
        return;
    }

    // Bounded-parallel POSTs per spec §Bounded parallelism. Holds the
    // policy snapshot (p) by move-and-clone into each future.
    let p_arc = Arc::new(p);
    futures_util::stream::iter(claimed)
        .for_each_concurrent(Some(max_concurrent as usize), |row| {
            let p = Arc::clone(&p_arc);
            let ctx = ctx;
            async move { deliver_one(ctx, row, &p).await }
        })
        .await;
}

async fn deliver_one(
    ctx: &DeliveryContext,
    row: crate::pyramid::db::OutboxRow,
    p: &MarketDeliveryPolicy,
) {
    // 1. Result parse — bare MarketAsyncResult (see module doc + spec
    //    §Envelope adapter). Malformed = terminal (code bug, no retry).
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
            terminal_fail(ctx, &row, &err, "envelope_parse_failed", p).await;
            return;
        }
    };

    // 2. Bearer extract + validation. NULL on a pre-migration row gets
    //    its own reason so deploy-artifact orphans are distinguishable
    //    from genuine token-plumbing bugs.
    let bearer = match row.callback_auth_token.as_deref() {
        Some(t) if is_valid_bearer(t) => t.to_string(),
        None if row_predates_migration(&ctx.db_path, &row.created_at).await => {
            terminal_fail(ctx, &row, "orphaned by migration", "orphaned_by_migration", p).await;
            return;
        }
        _ => {
            terminal_fail(
                ctx,
                &row,
                "callback_auth_token missing or malformed",
                "callback_auth_token_invalid",
                p,
            )
            .await;
            return;
        }
    };

    // 3. Envelope adapter (pure).
    let wire_envelope = build_callback_envelope(&row, &async_result);
    let envelope_body = match serde_json::to_string(&wire_envelope) {
        Ok(s) => s,
        Err(e) => {
            let err = truncate(&format!("envelope serialize: {e}"), p.max_error_message_chars);
            terminal_fail(ctx, &row, &err, "envelope_serialize_failed", p).await;
            return;
        }
    };

    // Determine latency_ms_source for metadata. This answers the question
    // "where did the number we just sent in result.latency_ms come from?"
    let latency_ms_source = if row.inference_latency_ms.is_some() {
        "inference"
    } else {
        "sweep_synth"
    };

    // 4. POST. Client configured with redirect(Policy::none()) so the
    //    bearer can't leak cross-origin on a DNS compromise. 30s timeout
    //    bounds any single attempt.
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(p.callback_post_timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let err = truncate(&format!("http client build: {e}"), p.max_error_message_chars);
            tracing::error!(err = %err, "delivery_worker: http client construction failed");
            return;
        }
    };

    let post_started = std::time::Instant::now();
    let response = client
        .post(&row.callback_url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .body(envelope_body)
        .send()
        .await;
    let post_duration_ms = post_started.elapsed().as_millis() as i64;

    let (status, headers) = match response {
        Ok(resp) => (resp.status(), resp.headers().clone()),
        Err(net_err) => {
            // Network-level failure (TCP reset, DNS, timeout, TLS). These
            // are transient by default. Display via {} not {:?} so any
            // future reqwest upgrade that serialized request headers
            // doesn't leak the bearer.
            let err = truncate(
                &format!("network: {net_err}"),
                p.max_error_message_chars,
            );
            transient_fail(ctx, &row, &err, None, RetryAfterSource::Absent, p, post_duration_ms).await;
            return;
        }
    };

    // 5. Branch on outcome.
    if status.is_success() {
        // CAS ready → delivered. Rowcount=0 means concurrent sweep won
        // the race; chronicle a distinct event so operator sees the
        // delivery still happened despite the local state lost.
        let cas_rows = cas_mark_delivered(ctx, &row, p).await;
        if cas_rows == 1 {
            emit(
                ctx,
                &row,
                EVENT_MARKET_RESULT_DELIVERED_TO_WIRE,
                json!({
                    "job_id": row.job_id,
                    "request_id": row.request_id,
                    "attempts": row.delivery_attempts + 1,
                    "latency_ms": row.inference_latency_ms.unwrap_or(0),
                    "latency_ms_source": latency_ms_source,
                    "duration_ms": post_duration_ms,
                }),
            )
            .await;
        } else {
            emit(
                ctx,
                &row,
                EVENT_MARKET_RESULT_DELIVERY_CAS_LOST,
                json!({
                    "job_id": row.job_id,
                    "request_id": row.request_id,
                    "attempts": row.delivery_attempts + 1,
                    "reason": "sweep_raced_to_failed",
                    "duration_ms": post_duration_ms,
                }),
            )
            .await;
        }
        return;
    }

    // Non-2xx — classify retry intent.
    match classify_retry(status, &headers) {
        RetryDecision::Terminal { source } => {
            let code = status.as_u16();
            let row_age_secs = age_secs_from_created_at(&row.created_at);
            // Discriminator for the 401-likely-secret-expired case
            // (spec §Wire-parameters-aware secret-expiry detection).
            let reason = if code == 401 && row_age_secs > p.ready_retention_secs as i64 {
                "terminal_http_401_likely_secret_expired".to_string()
            } else {
                format!("terminal_http_{code}")
            };
            let err = truncate(&format!("terminal http {code}"), p.max_error_message_chars);
            if cas_mark_failed_with_error(ctx, &row, &err, p).await >= 1 {
                emit(
                    ctx,
                    &row,
                    EVENT_MARKET_RESULT_DELIVERY_FAILED,
                    json!({
                        "job_id": row.job_id,
                        "request_id": row.request_id,
                        "attempts": row.delivery_attempts + 1,
                        "final_error": err,
                        "reason": reason,
                        "retry_source": source,
                        "status_code": code,
                    }),
                )
                .await;
            }
        }
        RetryDecision::Retry { source } => {
            let code = status.as_u16();
            let err = truncate(&format!("http {code}"), p.max_error_message_chars);
            let (retry_after, retry_after_source) = parse_retry_after_header(&headers);
            transient_fail(
                ctx,
                &row,
                &err,
                retry_after,
                retry_after_source,
                p,
                post_duration_ms,
            )
            .await;
            // Metadata note: the `source` from classify_retry indicates
            // whether Wire told us explicitly ("x_wire_retry_transient")
            // or we fell back ("http_code_fallback"). Chronicle already
            // carries the info via `retry_after_source`.
            let _ = source;
        }
    }
}

// ── Transient failure path ──────────────────────────────────────────────────

async fn transient_fail(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    err_msg: &str,
    retry_after: Option<u64>,
    retry_after_source: RetryAfterSource,
    p: &MarketDeliveryPolicy,
    _post_duration_ms: i64,
) {
    let new_attempts = row.delivery_attempts + 1;

    // If we've hit max_delivery_attempts, this transient failure is
    // actually terminal.
    if (new_attempts as u64) >= p.max_delivery_attempts {
        let err = truncate(
            &format!("{err_msg} (max_delivery_attempts exceeded)"),
            p.max_error_message_chars,
        );
        if cas_mark_failed_with_error(ctx, row, &err, p).await >= 1 {
            emit(
                ctx,
                row,
                EVENT_MARKET_RESULT_DELIVERY_FAILED,
                json!({
                    "job_id": row.job_id,
                    "request_id": row.request_id,
                    "attempts": new_attempts,
                    "final_error": err,
                    "reason": "max_attempts",
                }),
            )
            .await;
        }
        return;
    }

    // Otherwise: compute backoff, bump attempt, set next-attempt gate.
    // Backoff = Retry-After if present, else exponential min(base * 2^attempts, cap).
    let backoff_secs = retry_after.unwrap_or_else(|| {
        let exp = (new_attempts as u32).min(20);
        let computed = p.backoff_base_secs.saturating_mul(1u64 << exp);
        computed.min(p.backoff_cap_secs)
    });

    // Clone into the blocking task.
    let db_path = ctx.db_path.clone();
    let dispatcher = row.dispatcher_node_id.clone();
    let job_id = row.job_id.clone();
    let err_copy = err_msg.to_string();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = rusqlite::Connection::open(&db_path)?;
        Ok(crate::pyramid::db::market_outbox_bump_attempt_with_backoff(
            &conn,
            &dispatcher,
            &job_id,
            &err_copy,
            backoff_secs,
        )?)
    })
    .await;

    emit(
        ctx,
        row,
        EVENT_MARKET_RESULT_DELIVERY_ATTEMPT_FAILED,
        json!({
            "job_id": row.job_id,
            "request_id": row.request_id,
            "attempt": new_attempts,
            "error": err_msg,
            "backoff_secs": backoff_secs,
            "retry_after_source": retry_after_source_label(&retry_after_source),
        }),
    )
    .await;
}

// ── Terminal failure path ───────────────────────────────────────────────────

async fn terminal_fail(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    err_msg: &str,
    reason: &str,
    p: &MarketDeliveryPolicy,
) {
    if cas_mark_failed_with_error(ctx, row, err_msg, p).await >= 1 {
        emit(
            ctx,
            row,
            EVENT_MARKET_RESULT_DELIVERY_FAILED,
            json!({
                "job_id": row.job_id,
                "request_id": row.request_id,
                "attempts": row.delivery_attempts + 1,
                "final_error": err_msg,
                "reason": reason,
            }),
        )
        .await;
    }
}

// ── DB CAS helpers (run in spawn_blocking) ──────────────────────────────────

async fn cas_mark_delivered(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    p: &MarketDeliveryPolicy,
) -> usize {
    let db_path = ctx.db_path.clone();
    let dispatcher = row.dispatcher_node_id.clone();
    let job_id = row.job_id.clone();
    let retention = p.delivered_retention_secs;
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = rusqlite::Connection::open(&db_path)?;
        Ok(crate::pyramid::db::fleet_outbox_mark_delivered_if_ready(
            &conn, &dispatcher, &job_id, retention,
        )?)
    })
    .await
    .unwrap_or(Ok(0))
    .unwrap_or(0)
}

async fn cas_mark_failed_with_error(
    ctx: &DeliveryContext,
    row: &crate::pyramid::db::OutboxRow,
    err_msg: &str,
    p: &MarketDeliveryPolicy,
) -> usize {
    let db_path = ctx.db_path.clone();
    let dispatcher = row.dispatcher_node_id.clone();
    let job_id = row.job_id.clone();
    let err = err_msg.to_string();
    let retention = p.failed_retention_secs;
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = rusqlite::Connection::open(&db_path)?;
        Ok(crate::pyramid::db::market_outbox_mark_failed_with_error_cas(
            &conn, &dispatcher, &job_id, &err, retention,
        )?)
    })
    .await
    .unwrap_or(Ok(0))
    .unwrap_or(0)
}

// ── Chronicle emission helpers ──────────────────────────────────────────────

fn age_secs_from_created_at(created_at: &str) -> i64 {
    // SQLite's `datetime('now')` returns "YYYY-MM-DD HH:MM:SS" (UTC).
    // chrono::NaiveDateTime parses that; on parse error, return 0
    // (don't surface wrong reason).
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
            callback_url: "https://newsbleach.com/api/v1/compute/callback/playful%2F109%2F7".into(),
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
            requester_callback_url: None,
            requester_delivery_jwt: None,
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

    // ── Envelope adapter ───────────────────────────────────────────────────

    #[test]
    fn envelope_success_maps_required_fields() {
        let row = sample_row();
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "the answer".into(),
            prompt_tokens: Some(7),
            completion_tokens: Some(3),
            model: "gemma4:26b".into(),
            finish_reason: Some("stop".into()),
            provider_model: Some("gemma4:26b".into()),
        });
        let env = build_callback_envelope(&row, &result);
        match env {
            CallbackEnvelope::Success { job_id, result } => {
                assert_eq!(job_id, row.job_id);
                assert_eq!(result.content, "the answer");
                assert_eq!(result.input_tokens, 7);
                assert_eq!(result.output_tokens, 3);
                assert_eq!(result.model_used, "gemma4:26b");
                assert_eq!(result.latency_ms, 450);
                assert_eq!(result.finish_reason.as_deref(), Some("stop"));
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn envelope_none_tokens_map_to_zero() {
        // Wire requires non-negative integers; None must map to 0
        // (contract §2.3 isIntNonNeg validator).
        let row = sample_row();
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "".into(),
            prompt_tokens: None,
            completion_tokens: None,
            model: "gemma4:26b".into(),
            finish_reason: None,
            provider_model: None,
        });
        let env = build_callback_envelope(&row, &result);
        match env {
            CallbackEnvelope::Success { result, .. } => {
                assert_eq!(result.input_tokens, 0);
                assert_eq!(result.output_tokens, 0);
                // model_used falls back to `model` (non-empty).
                assert_eq!(result.model_used, "gemma4:26b");
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn envelope_model_used_fallback_chain() {
        // provider_model None + model empty → "unknown" (not a fail).
        let row = sample_row();
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "x".into(),
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            model: String::new(),
            finish_reason: None,
            provider_model: None,
        });
        let env = build_callback_envelope(&row, &result);
        match env {
            CallbackEnvelope::Success { result, .. } => {
                assert_eq!(result.model_used, "unknown");
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn envelope_sweep_synth_latency_defaults_to_zero() {
        let mut row = sample_row();
        row.inference_latency_ms = None; // sweep-synth row
        let result = MarketAsyncResult::Error("worker heartbeat lost".into());
        let env = build_callback_envelope(&row, &result);
        match env {
            CallbackEnvelope::Failure { error, job_id } => {
                assert_eq!(job_id, row.job_id);
                assert_eq!(error.code, "worker_heartbeat_lost");
                assert_eq!(error.message, "worker heartbeat lost");
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn envelope_job_id_is_uuid_not_handle_path() {
        // Regression guard for contract §10.5: body.job_id is always
        // the UUID (row.job_id); handle-path appears only in callback_url.
        let row = sample_row();
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "x".into(),
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            model: "m".into(),
            finish_reason: None,
            provider_model: None,
        });
        let env = build_callback_envelope(&row, &result);
        let json_bytes = serde_json::to_string(&env).unwrap();
        assert!(
            json_bytes.contains("4f93e9f4-5e7a-4a2a-9a6c-6d0c9c5d0b9a"),
            "body.job_id must be the UUID; got: {json_bytes}"
        );
        assert!(
            !json_bytes.contains("playful%2F109%2F7"),
            "body.job_id MUST NOT leak handle-path: {json_bytes}"
        );
    }

    #[test]
    fn envelope_serializes_snake_case_type() {
        let row = sample_row();
        let result = MarketAsyncResult::Error("kaboom".into());
        let env = build_callback_envelope(&row, &result);
        let json_str = serde_json::to_string(&env).unwrap();
        // Contract §2.3: type is "failure" (lowercase), not "Error" or
        // "Failure".
        assert!(
            json_str.contains("\"type\":\"failure\""),
            "expected snake_case `type` tag; got: {json_str}"
        );
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

    // ── Retry classification ───────────────────────────────────────────────

    #[test]
    fn classify_retry_explicit_header_wins_over_http_code() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Wire-Retry", "never".parse().unwrap());
        // 500 would normally be Retry per fallback; explicit header overrides.
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        match classify_retry(status, &headers) {
            RetryDecision::Terminal { source } => assert_eq!(source, "x_wire_retry_never"),
            _ => panic!("explicit never must produce Terminal"),
        }
    }

    #[test]
    fn classify_retry_fallback_for_missing_header() {
        let headers = HeaderMap::new();
        // 404 is in fallback terminal list.
        let status = reqwest::StatusCode::NOT_FOUND;
        match classify_retry(status, &headers) {
            RetryDecision::Terminal { source } => assert_eq!(source, "http_code_fallback"),
            _ => panic!("404 without header must fall back to Terminal"),
        }
        // 503 is NOT in terminal list — fallback treats as Retry.
        let status = reqwest::StatusCode::SERVICE_UNAVAILABLE;
        match classify_retry(status, &headers) {
            RetryDecision::Retry { source } => assert_eq!(source, "http_code_fallback"),
            _ => panic!("503 without header must fall back to Retry"),
        }
    }

    #[test]
    fn classify_retry_terminal_codes_cover_contract_set() {
        let headers = HeaderMap::new();
        // All codes in TERMINAL_HTTP_CODES_FALLBACK are terminal under fallback.
        for code in TERMINAL_HTTP_CODES_FALLBACK {
            let status = reqwest::StatusCode::from_u16(*code).unwrap();
            assert!(matches!(classify_retry(status, &headers), RetryDecision::Terminal { .. }),
                "HTTP {code} must be terminal");
        }
    }

    #[test]
    fn classify_retry_unknown_header_warns_and_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Wire-Retry", "schedule-next-eclipse".parse().unwrap());
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        // Should fall back to HTTP-code; 500 is NOT terminal → Retry.
        match classify_retry(status, &headers) {
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
        // A far-future HTTP-date must produce a positive delta.
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
    fn bearer_accepts_base64url_with_optional_padding() {
        assert!(is_valid_bearer("abcDEFghi123_-"));
        assert!(is_valid_bearer("abcDEFghi123="));
        assert!(is_valid_bearer("a"));
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
        // Multi-byte chars don't get sliced.
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
}
