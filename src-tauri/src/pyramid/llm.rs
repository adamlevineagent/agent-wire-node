// pyramid/llm.rs — LLM call surface with pluggable provider registry.
//
// Unified entry point: `call_model_unified` returns content + usage + generation_id.
// The legacy `call_model`, `call_model_with_usage`, and `call_model_structured`
// are thin wrappers for backward compatibility.
//
// Phase 3 refactor: the hardcoded OpenRouter URL, headers, and response
// parsing have been moved to `pyramid::provider`. `LlmConfig` now carries
// an optional `provider_registry` + `credential_store` reference so every
// call site that passes an `LlmConfig` transparently goes through the
// provider trait. When the registry is unset (e.g., unit tests or
// pre-Phase-3 boot paths), we synthesize an `OpenRouterProvider` from the
// legacy `LlmConfig` fields so the codebase remains callable during
// transitional states.
//
// The hardcoded OpenRouter chat-completions URL no longer lives in
// this file — it is encoded once, inside
// `OpenRouterProvider::chat_completions_url` in `provider.rs`, as the
// trait impl's default base URL.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use super::credentials::{CredentialStore, ResolvedSecret};
use super::event_bus::{TaggedBuildEvent, TaggedKind};
use super::provider::{
    LlmProvider, OpenRouterProvider, ParsedLlmResponse, ProviderRegistry, ProviderType,
    RequestMetadata,
};
use super::step_context::{
    compute_cache_key, compute_inputs_hash, verify_cache_hit, CacheEntry, CacheHitResult,
    StepContext,
};
use super::types::TokenUsage;

// ── Global rate limiter: configurable sliding window ────────────────────────

static RATE_LIMITER: LazyLock<TokioMutex<VecDeque<std::time::Instant>>> =
    LazyLock::new(|| TokioMutex::new(VecDeque::new()));

/// Global semaphore for local LLM providers (Ollama).
///
/// Phase 1 compute queue: set to usize::MAX (effectively a no-op).
/// The per-model FIFO queue in ComputeQueueManager is now the real
/// serializer. The semaphore stays at usize::MAX (not deleted) so
/// tests that don't construct ProviderPools or a ComputeQueueHandle
/// still compile and fall through without blocking.
static LOCAL_PROVIDER_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(usize::MAX));

/// Shared HTTP client — reuses TCP connections and TLS sessions across all LLM calls.
/// `pub(crate)` so Ollama API calls in `local_mode.rs` reuse the same client
/// instead of creating `reqwest::Client::new()` per call (Phase 0 fix).
pub(crate) static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(8)
        .build()
        .expect("failed to build shared reqwest::Client")
});

/// Wait until we have capacity in the sliding window before making an LLM call.
/// Parameters come from Tier1Config (llm_rate_limit_max_requests, llm_rate_limit_window_secs).
async fn rate_limit_wait(max_requests: usize, window_secs: f64) {
    if max_requests == 0 {
        return; // rate limiting disabled
    }
    loop {
        let now = std::time::Instant::now();
        let mut window = RATE_LIMITER.lock().await;

        // Evict entries older than the window
        while let Some(&oldest) = window.front() {
            if now.duration_since(oldest).as_secs_f64() >= window_secs {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() < max_requests {
            window.push_back(now);
            return;
        }

        // Window full — compute how long until the oldest entry expires
        let oldest = window[0];
        let wait = window_secs - now.duration_since(oldest).as_secs_f64();
        drop(window); // release lock while sleeping
        if wait > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait + 0.05)).await;
        }
    }
}

// ── Walker chronicle emitters (Walker Re-Plan Wire 2.1 §5) ──────────────────
//
// Per-entry walker events. Source label is derived from the call's
// `DispatchOrigin::source_label()` so queue-replayed dispatches record
// under their true origin instead of hardcoding `"network"`.
//
// All emitters are fire-and-forget: they do not block the walker on DB
// write, matching the existing `emit_network_*` pattern.

fn walker_chronicle_db_path(
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) -> Option<String> {
    ctx.map(|c| c.db_path.clone())
        .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()))
}

fn walker_resolved_model(
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) -> String {
    ctx.and_then(|c| c.resolved_model_id.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| config.primary_model.clone())
}

/// Map a walker-market classified `reason` slug to its specific
/// per-slug chronicle event constant, when one exists. Additive
/// emission — the caller still emits the generic walker chronicle
/// event on top of whatever this returns. Operator telemetry that
/// keys on the specific event name (e.g. `network_quote_expired`)
/// sees its row; dashboards keying on generic walker events
/// (`network_route_skipped` / `network_route_retryable_fail`)
/// keep working. See plan §4.2 + `feedback_no_integrity_demotion`:
/// we don't silently demote one channel because another exists.
///
/// Covers the seven market-branch slugs declared in
/// `compute_chronicle.rs` but previously un-emitted from live code.
/// Unknown / unmapped slugs return `None` — caller just emits the
/// generic event in that case.
fn map_market_slug_to_specific_event(reason: &str) -> Option<&'static str> {
    // Reason strings classified from Wire slugs in
    // `compute_quote_flow::classify_rev21_slug` plus the stage-tagged
    // auth reasons produced by `read_api_creds` on 401. Match on the
    // leading token so reasons carrying extra context (e.g.
    // "unknown_slug:foo") still match the primary slug.
    let primary = reason.split(|c: char| c == ':' || c == '(').next().unwrap_or(reason);
    match primary {
        // /purchase 401 `quote_jwt_expired` (or bare `quote_expired`).
        "quote_jwt_expired" | "quote_expired" => {
            Some(super::compute_chronicle::EVENT_NETWORK_QUOTE_EXPIRED)
        }
        // /purchase 409 `quote_already_purchased` — idempotent replay.
        "quote_already_purchased" => {
            Some(super::compute_chronicle::EVENT_NETWORK_PURCHASE_RECOVERED)
        }
        // /quote 409 `budget_exceeded`. Also emitted from the
        // MarketSurfaceCache pre-quote rate check when that gains a
        // per-entry `max_budget_credits` to compare against.
        "budget_exceeded" => {
            Some(super::compute_chronicle::EVENT_NETWORK_RATE_ABOVE_BUDGET)
        }
        // /fill 409 `dispatch_deadline_exceeded` — reservation expired.
        "dispatch_deadline_exceeded" => {
            Some(super::compute_chronicle::EVENT_NETWORK_DISPATCH_DEADLINE_MISSED)
        }
        // /purchase 409 `provider_queue_full` or /fill
        // `provider_depth_exceeded` — both mean "provider saturated".
        "provider_queue_full" | "provider_depth_exceeded" => {
            Some(super::compute_chronicle::EVENT_NETWORK_PROVIDER_SATURATED)
        }
        // /quote 409 `insufficient_balance` or /purchase 409
        // `balance_depleted` — Wire balance below reservation.
        "insufficient_balance" | "balance_depleted" => {
            Some(super::compute_chronicle::EVENT_NETWORK_BALANCE_INSUFFICIENT_FOR_MARKET)
        }
        // Any 401-level auth failure across the three stages —
        // stage-tagged reasons from `read_api_creds` plus Wire's
        // explicit 401 slug `unauthorized`.
        "quote_auth_failed"
        | "purchase_auth_failed"
        | "fill_auth_failed"
        | "unauthorized" => Some(super::compute_chronicle::EVENT_NETWORK_AUTH_EXPIRED),
        _ => None,
    }
}

fn emit_walker_chronicle(
    ctx: Option<&StepContext>,
    config: &LlmConfig,
    event_type: &'static str,
    source: &str,
    entry_provider_id: &str,
    metadata: serde_json::Value,
) {
    let Some(db_path) = walker_chronicle_db_path(ctx, config) else {
        return;
    };
    let model_id = walker_resolved_model(ctx, config);
    let job_path = super::compute_chronicle::generate_job_path(
        ctx, None, &model_id, source,
    );
    let source_owned = source.to_string();
    let entry_owned = entry_provider_id.to_string();
    let chronicle_ctx = if let Some(sc) = ctx {
        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
            sc, &job_path, event_type, &source_owned,
        )
    } else {
        super::compute_chronicle::ChronicleEventContext::minimal(
            &job_path, event_type, &source_owned,
        )
        .with_model_id(model_id.clone())
    };
    // Merge entry_provider_id + model_id into metadata so queries can filter
    // by either without every call site duplicating those fields.
    let mut meta = metadata;
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "entry_provider_id".to_string(),
            serde_json::Value::String(entry_owned),
        );
        obj.entry("model_id".to_string())
            .or_insert_with(|| serde_json::Value::String(model_id.clone()));
    }
    let chronicle_ctx = chronicle_ctx.with_metadata(meta);
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}

// ── dispatch_fleet_entry (Wave 2 — walker fleet branch helper) ───────────────

/// Arguments bundle for `dispatch_fleet_entry`.
///
/// Every field has already been precondition-validated by the walker's
/// runtime gate (branch_allowed / skip_fleet_dispatch / tunnel-Connected /
/// roster-present / jwt-non-empty / peer-found). The helper is the
/// dispatch half: register pending → POST /v1/fleet/dispatch → two-phase
/// oneshot await → chronicle by outcome → roster cleanup on peer-dead.
///
/// Returns a three-tier `EntryError` so the walker can advance or bubble
/// per §4.1 of the walker-re-plan. Fleet failures are never `CallTerminal`
/// — other route entries may still succeed.
struct FleetDispatchArgs<'a> {
    config: &'a LlmConfig,
    ctx: Option<&'a StepContext>,
    fleet_ctx: std::sync::Arc<crate::fleet::FleetDispatchContext>,
    policy_snap: crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy,
    callback_url: String,
    roster_handle: Arc<tokio::sync::RwLock<crate::fleet::FleetRoster>>,
    peer: crate::fleet::FleetPeer,
    jwt: String,
    rule_name: String,
    job_wait_secs: u64,
    system_prompt: &'a str,
    user_prompt: &'a str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&'a serde_json::Value>,
}

async fn dispatch_fleet_entry(
    args: FleetDispatchArgs<'_>,
) -> std::result::Result<LlmResponse, EntryError> {
    let FleetDispatchArgs {
        config,
        ctx,
        fleet_ctx,
        policy_snap,
        callback_url,
        roster_handle,
        peer,
        jwt,
        rule_name,
        job_wait_secs,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
    } = args;

    let fleet_job_path = super::compute_chronicle::generate_job_path(
        ctx, None, &config.primary_model, "fleet",
    );
    let fleet_start = std::time::Instant::now();
    let fleet_db_path = ctx
        .map(|c| c.db_path.clone())
        .or_else(|| {
            config
                .cache_access
                .as_ref()
                .map(|ca| ca.db_path.to_string())
        });

    // Clamp to at least 1s — a zero would cause the orphan sweep to evict
    // the entry on its first tick, before the callback can arrive.
    let expected_timeout = std::time::Duration::from_secs(job_wait_secs.max(1));

    let job_id = uuid::Uuid::new_v4().to_string();

    // Oneshot channel — filled by server.rs /v1/fleet/result handler when
    // the peer's callback arrives, or dropped by the orphan sweep.
    let (sender, receiver) =
        tokio::sync::oneshot::channel::<crate::fleet::FleetAsyncResult>();

    // Register pending entry BEFORE dispatch POST so a very-fast peer
    // callback cannot beat our registration and produce a spurious orphan.
    // peer_id MUST be peer.node_id (raw) — the callback authenticates via
    // fleet JWT whose `nid` claim carries the raw node_id.
    fleet_ctx.pending.register(
        job_id.clone(),
        crate::fleet::PendingFleetJob {
            sender,
            dispatched_at: std::time::Instant::now(),
            peer_id: peer.node_id.clone(),
            expected_timeout,
        },
    );

    let dispatch_result = crate::fleet::fleet_dispatch_by_rule(
        &peer,
        &job_id,
        &callback_url,
        &rule_name,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        &jwt,
        policy_snap.dispatch_ack_timeout_secs,
    )
    .await;

    let spawn_chronicle =
        |event_type: &'static str, metadata: serde_json::Value| {
            if let Some(ref db_path) = fleet_db_path {
                let db_path = db_path.clone();
                let chronicle_ctx = if let Some(sc) = ctx {
                    super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                        sc, &fleet_job_path, event_type, "fleet",
                    )
                } else {
                    super::compute_chronicle::ChronicleEventContext::minimal(
                        &fleet_job_path, event_type, "fleet",
                    )
                    .with_model_id(config.primary_model.clone())
                };
                let chronicle_ctx = chronicle_ctx.with_metadata(metadata);
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                    }
                });
            }
        };

    match dispatch_result {
        Ok(ack) => {
            spawn_chronicle(
                "fleet_dispatched_async",
                serde_json::json!({
                    "peer_id": peer.node_id,
                    "peer_name": peer.name,
                    "rule_name": rule_name,
                    "timeout_secs": job_wait_secs,
                    "peer_queue_depth": ack.peer_queue_depth,
                }),
            );

            // Two-phase await with pinned receiver. `timeout` consumes its
            // future by value; pin once, then pass `receiver.as_mut()` on
            // each call.
            tokio::pin!(receiver);
            let wait_outcome = match tokio::time::timeout(
                std::time::Duration::from_secs(job_wait_secs),
                receiver.as_mut(),
            )
            .await
            {
                Ok(Ok(r)) => Ok(r),
                Ok(Err(_recv_err)) => Err("orphaned"),
                Err(_elapsed) => {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(policy_snap.timeout_grace_secs),
                        receiver.as_mut(),
                    )
                    .await
                    {
                        Ok(Ok(r)) => Ok(r),
                        _ => Err("timeout"),
                    }
                }
            };

            // Idempotent cleanup — callback or sweep may have already
            // removed this entry.
            let _ = fleet_ctx.pending.remove(&job_id);

            match wait_outcome {
                Ok(crate::fleet::FleetAsyncResult::Success(fleet_resp)) => {
                    spawn_chronicle(
                        "fleet_result_received",
                        serde_json::json!({
                            "peer_id": peer.node_id,
                            "peer_name": peer.name,
                            "peer_model": fleet_resp.peer_model,
                            "latency_ms": fleet_start.elapsed().as_millis() as u64,
                            "tokens_prompt": fleet_resp.prompt_tokens.unwrap_or(0),
                            "tokens_completion": fleet_resp.completion_tokens.unwrap_or(0),
                        }),
                    );

                    Ok(LlmResponse {
                        content: fleet_resp.content,
                        usage: super::types::TokenUsage {
                            prompt_tokens: fleet_resp.prompt_tokens.unwrap_or(0),
                            completion_tokens: fleet_resp.completion_tokens.unwrap_or(0),
                        },
                        generation_id: None,
                        actual_cost_usd: None, // fleet is free (same operator)
                        provider_id: Some("fleet".to_string()),
                        fleet_peer_id: Some(
                            peer.handle_path.clone().unwrap_or_else(|| peer.node_id.clone()),
                        ),
                        fleet_peer_model: fleet_resp.peer_model.clone(),
                    })
                }
                Ok(crate::fleet::FleetAsyncResult::Error(err_msg)) => {
                    // Peer RAN inference and it failed (GPU OOM, model
                    // mismatch, etc.). RouteSkipped — peer couldn't help.
                    spawn_chronicle(
                        "fleet_result_failed",
                        serde_json::json!({
                            "peer_id": peer.node_id,
                            "peer_name": peer.name,
                            "error": err_msg,
                        }),
                    );
                    warn!(
                        "Fleet peer {} inference failed, walker advancing: {}",
                        peer.node_id, err_msg
                    );
                    Err(EntryError::RouteSkipped {
                        reason: "fleet_result_failed".to_string(),
                    })
                }
                Err("timeout") => {
                    spawn_chronicle(
                        "fleet_dispatch_timeout",
                        serde_json::json!({
                            "peer_id": peer.node_id,
                            "peer_name": peer.name,
                            "timeout_secs": job_wait_secs,
                            "grace_secs": policy_snap.timeout_grace_secs,
                        }),
                    );
                    warn!(
                        "Fleet dispatch timeout awaiting callback from peer {}, walker advancing",
                        peer.node_id
                    );
                    // Per §4.1: timeout → Retryable per plan. But walker
                    // advances in all fleet-failure modes anyway; Retryable
                    // + RouteSkipped are semantically identical from the
                    // walker's POV. Use Retryable to honor the plan's
                    // classification (distinct chronicle slug for
                    // telemetry).
                    Err(EntryError::Retryable {
                        reason: "fleet_dispatch_timeout".to_string(),
                    })
                }
                Err(_orphaned) => {
                    spawn_chronicle(
                        "fleet_dispatch_failed",
                        serde_json::json!({
                            "peer_id": peer.node_id,
                            "peer_name": peer.name,
                            "error": "pending entry orphaned by sweep",
                            "error_kind": "orphaned",
                            "status_code": serde_json::Value::Null,
                            "latency_ms": fleet_start.elapsed().as_millis() as u64,
                        }),
                    );
                    warn!(
                        "Fleet pending entry orphaned for peer {}, walker advancing",
                        peer.node_id
                    );
                    Err(EntryError::Retryable {
                        reason: "fleet_dispatch_orphaned".to_string(),
                    })
                }
            }
        }
        Err(e) => {
            // Dispatch POST failed — remove pending entry (idempotent) and
            // chronicle by status_code.
            let _ = fleet_ctx.pending.remove(&job_id);

            let is_overloaded = e.status_code == Some(503);
            let event_type = if is_overloaded {
                "fleet_peer_overloaded"
            } else {
                "fleet_dispatch_failed"
            };
            let metadata = if is_overloaded {
                serde_json::json!({
                    "peer_id": peer.node_id,
                    "peer_name": peer.name,
                    "status_code": e.status_code,
                    "retry_after": policy_snap.admission_retry_after_secs,
                })
            } else {
                serde_json::json!({
                    "peer_id": peer.node_id,
                    "peer_name": peer.name,
                    "error": e.message.clone(),
                    "error_kind": serde_json::to_value(&e.kind).unwrap_or_default(),
                    "status_code": e.status_code,
                    "latency_ms": fleet_start.elapsed().as_millis() as u64,
                })
            };
            spawn_chronicle(event_type, metadata);

            let peer_dead = e.is_peer_dead();
            if peer_dead {
                let mut roster_w = roster_handle.write().await;
                roster_w.remove_peer(&peer.node_id);
                warn!(
                    "Fleet dispatch: peer {} removed (transport failure): {}",
                    peer.node_id, e
                );
            } else {
                warn!(
                    "Fleet dispatch failed ({:?}), peer stays in roster: {}",
                    e.kind, e
                );
            }

            let reason = if is_overloaded {
                "fleet_peer_overloaded"
            } else if peer_dead {
                "fleet_peer_dead"
            } else {
                "fleet_dispatch_failed"
            };
            Err(EntryError::RouteSkipped {
                reason: reason.to_string(),
            })
        }
    }
}

// ── dispatch_market_entry (Wave 3b — walker market branch helper) ───────────
//
// Walker's market branch per plan §4.2 — three-RPC /quote → /purchase → /fill
// back-to-back, with register-BEFORE-fill race-fix baked into the call order.
//
// Runtime gate precondition (verified at the call site in the walker body):
//   - branch_allowed(Market, origin) passed
//   - compute_market_context present
//   - tunnel Connected + tunnel_url set
//   - optional MarketSurfaceCache consulted upstream (advisory)
//
// Chronicle emits inside this helper:
//   - network_quoted on /quote 200
//   - network_purchased on /purchase 200
//   - Route skip / terminal event vocab handled at the walker outer match.
//
// Error mapping: returns three-tier EntryError; all three RPC calls already
// classify via compute_quote_flow::classify_rev21_http_error.

struct MarketDispatchArgs<'a> {
    config: &'a LlmConfig,
    ctx: Option<&'a StepContext>,
    market_ctx: &'a crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext,
    model_id: String,
    max_budget: i64,
    /// Safety-rail timeout for the /fill await when Wire's
    /// `dispatch_deadline_at` can't be parsed (should never happen in
    /// practice; defensive fallback only). Canonical path uses
    /// `purchase_resp.dispatch_deadline_at + small grace` — see
    /// [compute-market-saturation-decisions-2026-04-21.md D3]: the
    /// deadline is a load-bearing economic contract, not a node-local
    /// timer.
    max_wait_ms: u64,
    /// Wall-clock patience budget per chunk for the saturation-retry
    /// loop. Drawn from
    /// `compute_participation_policy.market_saturation_patience_secs`.
    /// When the cumulative backoff across retries exceeds this,
    /// `dispatch_market_entry` bubbles RouteSkipped with reason
    /// `market_saturation_patience_exhausted` and the cascade advances.
    market_saturation_patience_secs: u64,
    max_tokens: i64,
    temperature: f32,
    input_tokens_est: i64,
    system_prompt: &'a str,
    user_prompt: &'a str,
    callback_url: String,
    walker_source_label: &'a str,
    entry_provider_id: &'a str,
}

/// Fallback backoff when Wire returns `all_offers_saturated_for_model`
/// but `min_expected_drain_ms` is absent from the detail (cohort has no
/// observations yet — fresh offers, <10 settled jobs). 15s mirrors a
/// representative single-GPU LLM serve time; chosen to be "short enough
/// that a fresh-offer cohort drains within a few retries, long enough
/// not to hammer Wire."
const SATURATION_BACKOFF_FALLBACK_MS: u64 = 15_000;

/// Grace appended to Wire's `dispatch_deadline_at` when computing
/// walker's /fill-await tokio timeout. Covers clock skew + callback
/// transit latency between provider → Wire → requester tunnel.
const DISPATCH_DEADLINE_GRACE_SECS: u64 = 10;

/// If `err` is `all_offers_saturated_for_model` (per body.error slug),
/// return Some(Duration) to back off before re-quoting. Walker should
/// sleep that long then re-enter the /quote → /purchase loop for the
/// same market entry.
///
/// Backoff derivation (in order):
/// 1. `detail.min_expected_drain_ms` from Wire's structured detail
///    (the cohort-shortest head-of-queue completion — "when next slot
///    opens somewhere"). Authoritative.
/// 2. `SATURATION_BACKOFF_FALLBACK_MS` when detail is absent or the
///    field is None (cohort lacks observations).
///
/// Returns None for every other error slug — caller falls through to
/// standard classification.
fn saturation_backoff_from_api_err(
    err: &crate::http_utils::ApiErrorWithHints,
) -> Option<std::time::Duration> {
    use crate::pyramid::compute_quote_flow::AllOffersSaturatedDetail;

    // The slug check is a belt-and-suspenders against header-stripping
    // proxies. Under the canonical Wire path, X-Wire-Retry: transient
    // also pairs with the slug; either signal alone is enough here.
    let slug = err
        .body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if slug != "all_offers_saturated_for_model" {
        return None;
    }

    // Parse the structured detail for min_expected_drain_ms.
    let detail_value = err.body.get("detail")?;
    let detail: Option<AllOffersSaturatedDetail> =
        serde_json::from_value(detail_value.clone()).ok();
    let drain_ms = detail
        .and_then(|d| d.min_expected_drain_ms)
        .map(|ms| ms.max(0.0) as u64)
        .unwrap_or(SATURATION_BACKOFF_FALLBACK_MS);
    Some(std::time::Duration::from_millis(
        drain_ms.max(1_000), // floor at 1s so we never hammer
    ))
}

/// Emit the `market_backoff_waiting` chronicle event and sleep for the
/// backoff duration, IF the wait would still leave us under the
/// patience deadline. Returns true if the sleep completed and walker
/// should retry; false if patience is exhausted (walker should give up
/// on market for this chunk and advance the cascade).
async fn apply_saturation_backoff(
    ctx: Option<&StepContext>,
    config: &LlmConfig,
    walker_source_label: &str,
    entry_provider_id: &str,
    attempt: u32,
    backoff: std::time::Duration,
    patience_deadline: std::time::Instant,
    min_expected_drain_ms: Option<u64>,
) -> bool {
    let now = std::time::Instant::now();
    let next_attempt_at = now + backoff;
    if next_attempt_at > patience_deadline {
        // Retry would land past patience — concede now.
        return false;
    }
    let next_attempt_utc =
        chrono::Utc::now() + chrono::Duration::from_std(backoff).unwrap_or_default();
    emit_walker_chronicle(
        ctx,
        config,
        super::compute_chronicle::EVENT_MARKET_BACKOFF_WAITING,
        walker_source_label,
        entry_provider_id,
        serde_json::json!({
            "attempt": attempt,
            "backoff_ms": backoff.as_millis() as u64,
            "next_attempt_at": next_attempt_utc.to_rfc3339(),
            "min_expected_drain_ms": min_expected_drain_ms,
            "branch": "market",
        }),
    );
    tokio::time::sleep(backoff).await;
    true
}

/// Parse Wire's `dispatch_deadline_at` (RFC 3339 UTC) into a Duration
/// from now. Returns None on parse failure or if the deadline is
/// already in the past; caller falls back to the safety-rail
/// max_wait_ms.
fn duration_until_deadline(dispatch_deadline_at: &str) -> Option<std::time::Duration> {
    let deadline = chrono::DateTime::parse_from_rfc3339(dispatch_deadline_at).ok()?;
    let now = chrono::Utc::now();
    let delta = deadline.with_timezone(&chrono::Utc).signed_duration_since(now);
    if delta.num_milliseconds() <= 0 {
        return None;
    }
    delta.to_std().ok()
}

async fn dispatch_market_entry(
    args: MarketDispatchArgs<'_>,
) -> std::result::Result<LlmResponse, EntryError> {
    use crate::pyramid::compute_quote_flow as cqf;

    let MarketDispatchArgs {
        config,
        ctx,
        market_ctx,
        model_id,
        max_budget,
        max_wait_ms,
        market_saturation_patience_secs,
        max_tokens,
        temperature,
        input_tokens_est,
        system_prompt,
        user_prompt,
        callback_url,
        walker_source_label,
        entry_provider_id,
    } = args;

    // Saturation-retry loop patience budget. Drawn from
    // compute_participation_policy; walker accumulates elapsed backoff
    // across /quote → saturation → sleep → re-quote iterations. When
    // the next scheduled sleep would land past this deadline, walker
    // concedes market for this chunk and the cascade advances.
    let patience_deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(market_saturation_patience_secs);
    let mut saturation_attempt: u32 = 0;

    // Snapshot node_id from AuthState — canonical runtime identity,
    // populated at registration and kept live via heartbeat/session.
    // WireNodeConfig.node_id is a static-config surface that is rarely
    // populated in the running state (other surfaces like fleet announce
    // also read from auth.node_id). Without this field Wire 400s with
    // `multiple_nodes_require_explicit_node_id` for any operator who
    // owns more than one node, so we always send it.
    let requester_node_id = {
        let auth = market_ctx.auth.read().await;
        auth.node_id.clone().filter(|s| !s.is_empty())
    };
    let requester_node_id = match requester_node_id {
        Some(id) => id,
        None => {
            return Err(EntryError::RouteSkipped {
                reason: "requester_node_id_unavailable".into(),
            });
        }
    };

    // ── Saturation-retry loop ────────────────────────────────────────
    //
    // Canonical market posture (per bilateral decision D2 + D4 in
    // compute-market-saturation-decisions-2026-04-21.md): saturation
    // means "busy, come back later" — NOT unavailable. Walker MUST
    // retry the same entry while patience allows; fallback only fires
    // on true unavailability (`no_offer_for_model` — 404 /
    // X-Wire-Retry: never / CallTerminal).
    //
    // Loop iterations do the /quote → /purchase handshake. On
    // `all_offers_saturated_for_model` at either step, walker backs
    // off for `min_expected_drain_ms` (from Wire's structured detail)
    // and re-enters the loop. /fill and onward are post-loop — once
    // we've successfully /purchased, the job is reserved and
    // execution proceeds.
    let (quote_resp, purchase_resp) = loop {
        // ── /quote ────────────────────────────────────────────────────
        let quote_body = cqf::ComputeQuoteBody {
            model_id: model_id.clone(),
            input_tokens_est,
            max_tokens,
            latency_preference: cqf::LatencyPreference::BestPrice,
            max_budget,
            // Always present (belt + suspenders — Wire auto-infers
            // when operator owns 1 node, requires explicit value
            // when >1).
            requester_node_id: Some(requester_node_id.clone()),
        };

        let quote_resp = match cqf::quote(&market_ctx.auth, &market_ctx.config, quote_body).await {
            Ok(r) => r,
            Err(api_err) => {
                if let Some(backoff) = saturation_backoff_from_api_err(&api_err) {
                    saturation_attempt += 1;
                    let min_drain_ms = api_err
                        .body
                        .get("detail")
                        .and_then(|d| d.get("min_expected_drain_ms"))
                        .and_then(|v| v.as_f64())
                        .map(|ms| ms as u64);
                    if !apply_saturation_backoff(
                        ctx,
                        config,
                        walker_source_label,
                        entry_provider_id,
                        saturation_attempt,
                        backoff,
                        patience_deadline,
                        min_drain_ms,
                    )
                    .await
                    {
                        return Err(EntryError::RouteSkipped {
                            reason: "market_saturation_patience_exhausted".into(),
                        });
                    }
                    continue;
                }
                return Err(cqf::classify_wire_error(&api_err, "quote"));
            }
        };

        // network_quoted chronicle (on successful /quote only).
        emit_walker_chronicle(
            ctx,
            config,
            super::compute_chronicle::EVENT_NETWORK_QUOTED,
            walker_source_label,
            entry_provider_id,
            serde_json::json!({
                "quote_id": quote_resp.quote_id,
                "rate_in_per_m": quote_resp.price_breakdown.matched_rate_in_per_m,
                "rate_out_per_m": quote_resp.price_breakdown.matched_rate_out_per_m,
                "reservation_fee": quote_resp.price_breakdown.reservation_fee,
                "estimated_total": quote_resp.price_breakdown.estimated_total,
                "queue_position": quote_resp.price_breakdown.queue_position,
                "model_id": model_id,
            }),
        );

        // TODO (Wire rev 2.1.1 pre-gate): once the contracts crate
        // surfaces `typical_serve_ms_p50_7d` on the offer row + the
        // market-surface cache, compare
        // `queue_position × typical_serve_ms_p50_7d` against
        // `compute_purchase_dispatch_window_s - margin` and skip this
        // offer (treat as saturation: sleep `min_expected_drain_ms`
        // then re-quote) when the head-of-queue wait would exceed the
        // dispatch window. Walker never pays a reservation fee on a
        // purchase it can't rationally hit. This is D3 in the
        // bilateral decision doc — the economic contract of a static
        // deadline is what keeps reservation-fee speculation rational.
        // Implementing the pre-gate without these data sources would
        // either over-skip (false negatives) or be a no-op. Dormant
        // scaffolding until Wire's rev lands.

        // ── /purchase ─────────────────────────────────────────────────
        let purchase_body = cqf::ComputePurchaseBody {
            quote_jwt: quote_resp.quote_jwt.clone(),
            trigger: cqf::ComputePurchaseTrigger::Immediate,
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
        };

        let purchase_resp = match cqf::purchase(
            &market_ctx.auth,
            &market_ctx.config,
            &quote_resp.quote_jwt,
            purchase_body,
        )
        .await
        {
            Ok(r) => r,
            Err(api_err) => {
                if let Some(backoff) = saturation_backoff_from_api_err(&api_err) {
                    saturation_attempt += 1;
                    let min_drain_ms = api_err
                        .body
                        .get("detail")
                        .and_then(|d| d.get("min_expected_drain_ms"))
                        .and_then(|v| v.as_f64())
                        .map(|ms| ms as u64);
                    if !apply_saturation_backoff(
                        ctx,
                        config,
                        walker_source_label,
                        entry_provider_id,
                        saturation_attempt,
                        backoff,
                        patience_deadline,
                        min_drain_ms,
                    )
                    .await
                    {
                        return Err(EntryError::RouteSkipped {
                            reason: "market_saturation_patience_exhausted".into(),
                        });
                    }
                    continue;
                }
                return Err(cqf::classify_wire_error(&api_err, "purchase"));
            }
        };

        break (quote_resp, purchase_resp);
    };

    emit_walker_chronicle(
        ctx,
        config,
        super::compute_chronicle::EVENT_NETWORK_PURCHASED,
        walker_source_label,
        entry_provider_id,
        serde_json::json!({
            "uuid_job_id": purchase_resp.uuid_job_id,
            "request_id": purchase_resp.request_id,
            "job_id": purchase_resp.job_id,
            "dispatch_deadline_at": purchase_resp.dispatch_deadline_at,
            "model_id": model_id,
        }),
    );

    // ── RACE FIX: register PendingJobs entry BEFORE /fill ─────────────
    //
    // If we registered inside await_result (former Wave 3a shape), a
    // fast provider callback could beat registration — sender missing,
    // payload dropped. Registering here closes the window at call site.
    let rx = cqf::register_pending(&market_ctx.pending_jobs, &purchase_resp.uuid_job_id).await;

    // ── /fill ─────────────────────────────────────────────────────────
    let messages = serde_json::json!([
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": user_prompt},
    ]);

    let fill_request = cqf::ComputeFillRequest {
        body: cqf::ComputeFillBody {
            job_id: purchase_resp.job_id.clone(),
            messages,
            // Absence means "use max_tokens_quoted" per rev 2.1 spec §2.3;
            // we only pass a ceiling when the caller explicitly set one.
            max_tokens: if max_tokens > 0 { Some(max_tokens) } else { None },
            temperature,
            relay_count: 0,
            privacy_tier: "direct".to_string(),
            input_token_count: input_tokens_est,
            requester_callback_url: callback_url,
        },
        request_id: purchase_resp.request_id.clone(),
        idempotency_key: purchase_resp.request_id.clone(),
    };

    // Fire /fill; on error the registered rx is dropped when this fn
    // returns (we also explicitly take() below for timeouts inside
    // await_result). If /fill errors, clean up the pending entry so a
    // late push hits already_settled.
    if let Err(api_err) = cqf::fill(&market_ctx.auth, &market_ctx.config, fill_request).await {
        let _ = market_ctx.pending_jobs.take(&purchase_resp.uuid_job_id).await;
        return Err(cqf::classify_wire_error(&api_err, "fill"));
    }

    // ── Await oneshot ────────────────────────────────────────────────
    //
    // Canonical deadline (per D3): parse Wire's
    // `purchase_resp.dispatch_deadline_at` (RFC 3339) and await until
    // that + a small grace (DISPATCH_DEADLINE_GRACE_SECS). Wire owns
    // the deadline; walker has no independent timer.
    //
    // Fallback: if the deadline string fails to parse (shouldn't
    // happen in practice — Wire always emits RFC 3339 UTC — but we're
    // defensive), use the legacy `max_wait_ms` safety rail so the
    // await doesn't become unbounded. A parse failure also logs a
    // warning so operators notice the contract drift.
    let timeout = match duration_until_deadline(&purchase_resp.dispatch_deadline_at) {
        Some(d) => d + std::time::Duration::from_secs(DISPATCH_DEADLINE_GRACE_SECS),
        None => {
            tracing::warn!(
                dispatch_deadline_at = %purchase_resp.dispatch_deadline_at,
                "dispatch_deadline_at parse failed or already elapsed; falling back to max_wait_ms safety rail",
            );
            std::time::Duration::from_millis(max_wait_ms)
        }
    };
    cqf::await_result(rx, &purchase_resp.uuid_job_id, &market_ctx.pending_jobs, timeout).await
}

// ── Response types ───────────────────────────────────────────────────────────

/// Unified response from the LLM client. Every call returns content, token usage,
/// and the OpenRouter generation ID (for cost observatory lookups).
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The text content returned by the model.
    pub content: String,
    /// Token usage from the API response (prompt + completion tokens).
    pub usage: TokenUsage,
    /// OpenRouter generation ID (the top-level `id` field in the response JSON).
    /// Used for cost observatory correlation. None if the API didn't return one.
    pub generation_id: Option<String>,
    /// Phase 11: authoritative synchronous cost in USD from the
    /// provider's response body (`usage.cost` for OpenRouter). `None`
    /// for Ollama local (zero) and for providers that don't report
    /// cost. Feeds `pyramid_cost_log.actual_cost` and the broadcast
    /// webhook's discrepancy comparison.
    pub actual_cost_usd: Option<f64>,
    /// Phase 11: provider id resolved at call time (e.g., "openrouter",
    /// "ollama-local"). Feeds `pyramid_cost_log.provider_id` so the
    /// leak-detection sweep and provider-health state machine can
    /// group rows per provider.
    pub provider_id: Option<String>,
    /// Fleet provenance: node_id of the peer that served this call.
    /// None for non-fleet calls.
    pub fleet_peer_id: Option<String>,
    /// Fleet provenance: model the peer actually used (returned in
    /// the fleet dispatch response). None for non-fleet calls.
    pub fleet_peer_model: Option<String>,
}

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub auth_token: String,
    pub primary_model: String,
    pub fallback_model_1: String,
    pub fallback_model_2: String,
    pub primary_context_limit: usize,
    pub fallback_1_context_limit: usize,
    /// Max retry attempts for LLM calls (loaded from Tier1Config).
    pub max_retries: u32,
    /// Base timeout in seconds for LLM calls (loaded from Tier2Config).
    pub base_timeout_secs: u64,
    /// Maximum timeout in seconds for LLM calls (loaded from Tier2Config).
    pub max_timeout_secs: u64,
    /// HTTP status codes that trigger a retry with exponential backoff.
    pub retryable_status_codes: Vec<u16>,
    /// Base sleep duration (seconds) between retries before exponential backoff.
    pub retry_base_sleep_secs: u64,
    /// Number of prompt characters per timeout increment (for scaling formula).
    pub timeout_chars_per_increment: usize,
    /// Seconds added per increment of chars in the timeout scaling formula.
    pub timeout_increment_secs: u64,
    /// Max LLM requests per sliding window (0 = disabled).
    pub rate_limit_max_requests: usize,
    /// Sliding window duration in seconds for rate limiting.
    pub rate_limit_window_secs: f64,
    /// When true, log full LLM response bodies for failed/truncated calls to the debug log file.
    pub llm_debug_logging: bool,
    /// Custom aliases mapping a "model_tier" string to a specific model.
    ///
    /// Phase 3 NOTE: this field is legacy. The `provider_registry` +
    /// `pyramid_tier_routing` table now carry the canonical tier → model
    /// mapping. `model_aliases` remains as a transitional escape hatch
    /// for code paths that want to override a tier lookup before the
    /// registry is fully populated; Phase 4 will retire it.
    pub model_aliases: std::collections::HashMap<String, String>,
    /// Phase 3: optional provider registry. When present, LLM calls
    /// resolve their provider + model via this registry instead of the
    /// hardcoded OpenRouter URL + cascade. Unset in unit tests and in
    /// the narrow window between app startup and DB init.
    pub provider_registry: Option<Arc<ProviderRegistry>>,
    /// Phase 3: optional credential store. Threaded here alongside the
    /// provider registry so call sites that hold an `LlmConfig`
    /// reference can resolve `${VAR_NAME}` substitutions without
    /// touching the database.
    pub credential_store: Option<Arc<CredentialStore>>,
    /// Phase 12: optional cache plumbing shared across every LLM call
    /// that uses this config. When `Some`, the Phase 12 retrofit sweep
    /// can construct a StepContext inline at each call site using
    /// `cache_access.db_path` + `cache_access.bus` without requiring
    /// additional parameters. Unset in unit tests and in call sites
    /// that intentionally bypass the cache (e.g. diagnostics, ASCII art,
    /// semantic search).
    pub cache_access: Option<CacheAccess>,
    /// Dispatch policy for routing LLM calls to providers.
    /// When Some, routing rules determine which provider handles each call.
    /// When None (tests, pre-init), fall through to legacy behavior.
    pub dispatch_policy: Option<std::sync::Arc<crate::pyramid::dispatch_policy::DispatchPolicy>>,
    /// Per-provider concurrency pools. When Some, replaces the global
    /// LOCAL_PROVIDER_SEMAPHORE with per-provider semaphores.
    /// When None (tests, pre-init), fall through to global semaphore.
    pub provider_pools: Option<std::sync::Arc<crate::pyramid::provider_pools::ProviderPools>>,
    /// Phase 1 compute queue handle. When Some, LLM calls are enqueued
    /// to the per-model FIFO queue and processed by the GPU loop.
    /// When None (tests, pre-init), calls go straight to HTTP.
    pub compute_queue: Option<crate::compute_queue::ComputeQueueHandle>,
    /// Fleet roster handle. When Some, fleet peers are checked BEFORE the
    /// local compute queue — if a peer has the model loaded with capacity,
    /// the call is dispatched to the peer via HTTP. On failure, falls
    /// through to the local queue. When None (tests, pre-init), fleet
    /// routing is skipped.
    pub fleet_roster: Option<Arc<tokio::sync::RwLock<crate::fleet::FleetRoster>>>,
    /// Fleet dispatch context (Phase 4 async fleet dispatch). Carries the
    /// pending-job registry, a handle to the node's tunnel state (for
    /// callback URL construction), and the operational delivery policy.
    /// When Some, fleet dispatch uses the async callback protocol. When
    /// None (tests, pre-init), fleet dispatch is skipped and the call
    /// falls through to local execution.
    pub fleet_dispatch: Option<std::sync::Arc<crate::fleet::FleetDispatchContext>>,
    /// Phase B compute market requester context. Carries the auth +
    /// node config handles, the requester-side pending-jobs map, and
    /// the tunnel state handle needed by the Phase B branch in
    /// `call_model_unified`. When None (tests, pre-init, or tester
    /// builds with market disabled at the policy layer), the market
    /// branch is skipped and execution falls through to pool
    /// acquisition. See
    /// `docs/plans/call-model-unified-market-integration.md` §3.5.
    pub compute_market_context: Option<crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext>,
    /// Rev 2.1 `/api/v1/compute/market-surface` cache (Wave 3).
    /// Walker consults this on the `"market"` branch as an advisory
    /// pre-filter — `/quote` remains the authoritative viability check.
    /// Populated by a Tokio polling task spawned from `main.rs` at boot
    /// (60s cadence aligned with Wire's `Cache-Control: max-age=60`).
    /// `None` in tests / pre-init — walker treats a missing cache as
    /// "cold" and advances silently per plan §5.1.
    pub market_surface_cache: Option<std::sync::Arc<crate::pyramid::market_surface_cache::MarketSurfaceCache>>,
}

/// Phase 12: cache plumbing that lives on an LlmConfig so every call
/// site holding `&LlmConfig` has the pieces it needs to construct a
/// cache-usable StepContext without additional parameters.
///
/// `slug` scopes the cache row (one slug per build); `build_id`
/// stamps the provenance column; `db_path` is the on-disk SQLite
/// file the cache reads and writes go through; `bus` is the tagged
/// build event bus for `CacheHit` / `CacheMiss` emission.
///
/// Cloned via Arc internally so attaching to every derived config is
/// cheap (two Arc bumps — bus + db_path are held as Arc<str>).
#[derive(Clone)]
pub struct CacheAccess {
    pub slug: String,
    pub build_id: String,
    pub db_path: Arc<str>,
    pub bus: Option<Arc<super::event_bus::BuildEventBus>>,
    /// Chain strategy name — set to Some only by the chain executor path.
    /// Default None; stale engine, evidence answering, tests leave as None.
    pub chain_name: Option<String>,
    /// Content type — set alongside chain_name by the chain executor path.
    pub content_type: Option<String>,
}

impl CacheAccess {
    /// Builder: set chain context on a CacheAccess instance.
    /// Only the chain executor call sites use this; all others leave
    /// chain_name/content_type as None.
    pub fn with_chain_context(mut self, chain_name: String, content_type: String) -> Self {
        self.chain_name = Some(chain_name);
        self.content_type = Some(content_type);
        self
    }
}

impl std::fmt::Debug for CacheAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheAccess")
            .field("slug", &self.slug)
            .field("build_id", &self.build_id)
            .field("db_path", &self.db_path)
            .field("bus", &self.bus.as_ref().map(|_| "<bus>"))
            .field("chain_name", &self.chain_name)
            .field("content_type", &self.content_type)
            .finish()
    }
}

// `LlmConfig` carries secrets in `api_key` + `auth_token`. Derive-on
// `Debug` would log those by default; override it so nothing sensitive
// appears in error dumps or `tracing::debug!` output.
impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("api_key", &"[redacted]")
            .field("auth_token", &"[redacted]")
            .field("primary_model", &self.primary_model)
            .field("fallback_model_1", &self.fallback_model_1)
            .field("fallback_model_2", &self.fallback_model_2)
            .field("primary_context_limit", &self.primary_context_limit)
            .field("fallback_1_context_limit", &self.fallback_1_context_limit)
            .field("max_retries", &self.max_retries)
            .field("base_timeout_secs", &self.base_timeout_secs)
            .field("max_timeout_secs", &self.max_timeout_secs)
            .field("retryable_status_codes", &self.retryable_status_codes)
            .field("retry_base_sleep_secs", &self.retry_base_sleep_secs)
            .field(
                "timeout_chars_per_increment",
                &self.timeout_chars_per_increment,
            )
            .field("timeout_increment_secs", &self.timeout_increment_secs)
            .field("rate_limit_max_requests", &self.rate_limit_max_requests)
            .field("rate_limit_window_secs", &self.rate_limit_window_secs)
            .field("llm_debug_logging", &self.llm_debug_logging)
            .field("model_aliases", &self.model_aliases)
            .field(
                "provider_registry",
                &self.provider_registry.as_ref().map(|_| "<registry>"),
            )
            .field(
                "credential_store",
                &self.credential_store.as_ref().map(|_| "<store>"),
            )
            .field("cache_access", &self.cache_access)
            .field("dispatch_policy", &self.dispatch_policy.as_ref().map(|_| "<policy>"))
            .field("provider_pools", &self.provider_pools.as_ref().map(|_| "<pools>"))
            .field("compute_queue", &self.compute_queue.as_ref().map(|_| "<queue>"))
            .field("fleet_roster", &self.fleet_roster.as_ref().map(|_| "<fleet>"))
            .field("fleet_dispatch", &self.fleet_dispatch.as_ref().map(|_| "<fleet_dispatch>"))
            .field(
                "compute_market_context",
                &self.compute_market_context.as_ref().map(|_| "<compute_market_context>"),
            )
            .field(
                "market_surface_cache",
                &self.market_surface_cache.as_ref().map(|_| "<market_surface_cache>"),
            )
            .finish()
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            auth_token: String::new(),
            primary_model: "inception/mercury-2".into(),
            fallback_model_1: "qwen/qwen3.5-flash-02-23".into(),
            fallback_model_2: "x-ai/grok-4.20-beta".into(),
            primary_context_limit: 120_000,
            fallback_1_context_limit: 900_000,
            max_retries: 5,
            base_timeout_secs: 120,
            max_timeout_secs: 600,
            retryable_status_codes: vec![429, 403, 502, 503],
            retry_base_sleep_secs: 1,
            timeout_chars_per_increment: 100_000,
            timeout_increment_secs: 60,
            rate_limit_max_requests: 20,
            rate_limit_window_secs: 5.0,
            llm_debug_logging: false,
            model_aliases: std::collections::HashMap::new(),
            provider_registry: None,
            credential_store: None,
            cache_access: None,
            dispatch_policy: None,
            provider_pools: None,
            compute_queue: None,
            fleet_roster: None,
            fleet_dispatch: None,
            compute_market_context: None,
            market_surface_cache: None,
        }
    }
}

impl LlmConfig {
    /// Clone this config with a different primary model. Preserves
    /// `provider_registry`, `credential_store`, and every other field —
    /// use this instead of `config_helper::config_for_model` whenever you
    /// have a live `LlmConfig` (e.g. from `PyramidState.config`) and need
    /// a variant pinned to a specific model.
    ///
    /// `config_for_model(api_key, model)` (now deprecated) ends in
    /// `..Default::default()`, which silently zeroes the new
    /// `provider_registry` and `credential_store` fields. Every helper
    /// that uses it bypasses the Phase 3 provider registry +
    /// `.credentials` file. `clone_with_model_override` preserves both
    /// runtime handles by construction so the maintenance subsystem
    /// stays on the registry path.
    /// Phase 12: clone this config with cache plumbing attached so
    /// every LLM call that uses the returned config flows through
    /// the content-addressable cache. `db_path` is the SQLite file
    /// the cache reads/writes go through; `bus` is the tagged build
    /// event bus; `slug` + `build_id` are stamped on every cache row.
    pub fn clone_with_cache_access(
        &self,
        slug: impl Into<String>,
        build_id: impl Into<String>,
        db_path: impl Into<Arc<str>>,
        bus: Option<Arc<super::event_bus::BuildEventBus>>,
    ) -> Self {
        let mut cloned = self.clone();
        cloned.cache_access = Some(CacheAccess {
            slug: slug.into(),
            build_id: build_id.into(),
            db_path: db_path.into(),
            bus,
            chain_name: None,
            content_type: None,
        });
        cloned
    }

    pub fn clone_with_model_override(&self, model: &str) -> Self {
        let mut cloned = self.clone();
        cloned.primary_model = model.to_string();
        // Pin both fallbacks to the same model so the cascade stays
        // on-model — mirrors the legacy `config_for_model` semantics.
        cloned.fallback_model_1 = model.to_string();
        cloned.fallback_model_2 = model.to_string();
        cloned
    }

    /// Merge process-scoped runtime wiring from the currently-live config.
    ///
    /// Rebuilds from `PyramidConfig` intentionally start from durable
    /// profile/config data, which means runtime-only attachments like
    /// dispatch policy handles, queue wiring, and fleet roster pointers
    /// must be carried forward from the live process state. Keeping that
    /// contract here avoids multiple profile-apply entry points drifting
    /// out of sync as new runtime fields are added.
    ///
    /// TODO(architecture): `LlmConfig` still mixes durable user config with
    /// process-scoped runtime wiring. The 100-year fix is to split those into
    /// separate types so profile/config rebuilds never need overlay logic at all.
    ///
    /// `cache_access` is intentionally excluded because it is build-scoped
    /// ephemeral state, not global process wiring.
    pub fn with_runtime_overlays_from(mut self, live: &Self) -> Self {
        if self.api_key.is_empty() {
            self.api_key = live.api_key.clone();
        }
        if self.auth_token.is_empty() {
            self.auth_token = live.auth_token.clone();
        }
        if self.provider_registry.is_none() {
            self.provider_registry = live.provider_registry.clone();
        }
        if self.credential_store.is_none() {
            self.credential_store = live.credential_store.clone();
        }
        if self.dispatch_policy.is_none() {
            self.dispatch_policy = live.dispatch_policy.clone();
        }
        if self.provider_pools.is_none() {
            self.provider_pools = live.provider_pools.clone();
        }
        if self.compute_queue.is_none() {
            self.compute_queue = live.compute_queue.clone();
        }
        if self.fleet_roster.is_none() {
            self.fleet_roster = live.fleet_roster.clone();
        }
        if self.fleet_dispatch.is_none() {
            self.fleet_dispatch = live.fleet_dispatch.clone();
        }
        if self.compute_market_context.is_none() {
            self.compute_market_context = live.compute_market_context.clone();
        }
        self
    }

    /// Derive a replay config from this config. Single source of truth for
    /// which dispatch-routing fields are cleared. The key insight: whenever
    /// `prepare_for_replay` is called, the OUTER dispatch decision has
    /// already been made. The inner (replayed) call should be pool-only —
    /// it has no business re-dispatching to fleet or market.
    ///
    /// Origin-independent by design: for `Local` (compute_queue replay from
    /// the outer walker), the outer walker already tried fleet + market
    /// before the enqueue decision. For `FleetReceived` / `MarketReceived`
    /// (inbound-job worker spawn), the node is the provider fulfilling
    /// someone else's work — no outbound dispatch should happen.
    ///
    /// Takes `origin` for observability (emitted via `tracing::debug` at each
    /// call) and for future use if an origin-specific carve-out becomes
    /// necessary. Call-site intent is explicit.
    pub fn prepare_for_replay(&self, origin: DispatchOrigin) -> Self {
        tracing::debug!(?origin, "preparing replay config");
        let mut cfg = self.clone();
        cfg.compute_queue = None;
        cfg.fleet_dispatch = None;
        cfg.fleet_roster = None;
        cfg.compute_market_context = None;
        cfg
    }
}

/// Origin classifier for a dispatch that arrived at this node from
/// elsewhere. Used for chronicle `source` labeling so that market-
/// received and fleet-received jobs don't both end up tagged
/// `fleet_received` when they flow through the compute-queue path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchOrigin {
    /// Own build (this operator initiated the call from a chain).
    #[default]
    Local,
    /// Received from a fleet peer via the fleet-dispatch JWT path.
    FleetReceived,
    /// Received from the Wire compute market (handle_market_dispatch).
    MarketReceived,
}

impl DispatchOrigin {
    /// The chronicle `source` label used on pyramid_compute_events rows
    /// emitted from this entry point. Matches the constants in
    /// `compute_chronicle.rs` (`SOURCE_LOCAL`, `SOURCE_FLEET_RECEIVED`,
    /// `SOURCE_MARKET_RECEIVED`).
    pub fn source_label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::FleetReceived => "fleet_received",
            Self::MarketReceived => "market_received",
        }
    }
}

// ── Walker route-branch classification (§2.5.2) ─────────────────────────────

/// Walker's three-way classification of a `RouteEntry.provider_id`. Walker
/// dispatch behavior branches on this: `Fleet` goes through fleet peer
/// lookup + JWT dispatch; `Market` goes through the Wire compute market
/// three-RPC flow; `Pool` goes through the provider pool + HTTP retry
/// path that today handles openrouter, ollama-local, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteBranch {
    Fleet,
    Market,
    Pool,
}

/// Classify a route entry's `provider_id` into a [`RouteBranch`].
///
/// `"fleet"` and `"market"` are walker sentinel ids (plan §2 — "walker adds
/// new"); every other id is a real provider pool (openrouter, ollama-local,
/// remote-5090, etc.) that lives in `ProviderPools`.
pub fn classify_branch(provider_id: &str) -> RouteBranch {
    match provider_id {
        "fleet" => RouteBranch::Fleet,
        "market" => RouteBranch::Market,
        _ => RouteBranch::Pool,
    }
}

/// Decides whether a route branch is allowed for an execution context with
/// the given [`DispatchOrigin`]. Single source of truth for the "inbound
/// jobs don't re-dispatch" invariant — an operator fulfilling a fleet- or
/// market-received job must not recursively re-dispatch that call to
/// another peer or back out to the market.
///
/// `Pool` is always allowed: even inbound jobs need local execution to
/// produce a response. `Fleet` and `Market` branches are only taken when
/// the dispatch originated locally on this operator (`Local`); inbound
/// contexts (`FleetReceived`, `MarketReceived`) skip them.
pub fn branch_allowed(branch: RouteBranch, origin: DispatchOrigin) -> bool {
    match (branch, origin) {
        (RouteBranch::Pool, _) => true,
        (RouteBranch::Fleet | RouteBranch::Market, DispatchOrigin::Local) => true,
        (
            RouteBranch::Fleet | RouteBranch::Market,
            DispatchOrigin::FleetReceived | DispatchOrigin::MarketReceived,
        ) => false,
    }
}

// ── Walker entry-error taxonomy (§2.5.3) ────────────────────────────────────

/// Three-tier failure taxonomy a single walker entry can produce. Plan's
/// earlier "Retryable vs Terminal" split conflated two different terminal
/// semantics; `RouteSkipped` carves out the "wrong resource for this call,
/// try the next one" case so that e.g. a market `insufficient_balance`
/// rejection doesn't bubble to the caller while fleet + openrouter are
/// still untried.
///
/// Walker semantics:
/// - [`Retryable`](EntryError::Retryable) and
///   [`RouteSkipped`](EntryError::RouteSkipped) both cause the walker to
///   advance to the next `RouteEntry`. They emit distinct chronicle events
///   so operators can tell transient retry pressure apart from "this
///   resource can't help with this call."
/// - [`CallTerminal`](EntryError::CallTerminal) bubbles to the caller:
///   the walker writes `network_route_terminal_fail` + `fail_audit` and
///   returns an `Err`. Reserved for failures that would fail on every
///   route identically (walker bugs, caller-config bugs, auth/operator
///   failures).
///
/// Not `Clone` / `Copy` — carried by value through the walker result path
/// and dropped on success. Each variant has a `reason` string for
/// chronicle-event metadata.
#[derive(Debug)]
pub enum EntryError {
    /// Same route class, retry-after-delay kind of failure. Rare at walker
    /// scope — walker usually advances rather than sleeping.
    Retryable { reason: String },
    /// This route branch can't serve this call — advance to next entry.
    /// Examples: market `insufficient_balance`, missing openrouter key,
    /// fleet peer dead, dispatch-deadline missed.
    RouteSkipped { reason: String },
    /// This entire call is doomed regardless of route. Bubble to caller.
    /// Examples: `max_tokens_exceeds_quote` (walker bug), 400
    /// `multi_system_messages` (caller bug), `/fill` 401
    /// (auth/operator bug), any walker internal invariant violation.
    CallTerminal { reason: String },
}

impl EntryError {
    /// Short variant tag used in chronicle event metadata.
    pub fn variant_tag(&self) -> &'static str {
        match self {
            EntryError::Retryable { .. } => "retryable",
            EntryError::RouteSkipped { .. } => "route_skipped",
            EntryError::CallTerminal { .. } => "call_terminal",
        }
    }

    /// Access the reason string without destructuring.
    pub fn reason(&self) -> &str {
        match self {
            EntryError::Retryable { reason }
            | EntryError::RouteSkipped { reason }
            | EntryError::CallTerminal { reason } => reason,
        }
    }
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.variant_tag(), self.reason())
    }
}

impl std::error::Error for EntryError {}

/// Truncate a string to at most `max` BYTES while respecting UTF-8 char
/// boundaries. Returns an owned String. Never panics on multi-byte input.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk back from max until we land on a char boundary.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Classify a pool-branch HTTP 400 response body into the three-tier
/// EntryError taxonomy (plan §4.3).
///
/// - Provider-level model rejection (OpenRouter / OpenAI-compat style
///   messages like `"gemma4:26b is not a valid model ID"`, `"model not
///   found"`, `"unsupported model"`, `"invalid model"`) → `RouteSkipped`.
///   Other routes with a different model_id could still succeed.
/// - Feature-unsupported (`"not supported"`, `"unsupported"`) →
///   `RouteSkipped`. Same reasoning — a different provider or a different
///   model may support the feature.
/// - Otherwise (malformed JSON, multi-system-turns, schema violations) →
///   `CallTerminal`. Every route would reject the same way.
///
/// The check is case-insensitive on the body text. Defensive empty-body
/// behavior: classify as `CallTerminal` (nothing to inspect, can't prove
/// another route would fare differently, and bubbling the exhaustion is
/// the conservative default).
pub(crate) fn classify_pool_400(body: &str) -> EntryError {
    let body_lower = body.to_lowercase();
    let truncated = truncate_utf8(body, 200);

    let is_provider_model_rejection = body_lower.contains("not a valid model")
        || body_lower.contains("model not found")
        || body_lower.contains("unsupported model")
        || body_lower.contains("invalid model");
    if is_provider_model_rejection {
        return EntryError::RouteSkipped {
            reason: format!("provider_rejected_model: {truncated}"),
        };
    }

    let is_feature_unsupported =
        body_lower.contains("not supported") || body_lower.contains("unsupported");
    if is_feature_unsupported {
        return EntryError::RouteSkipped {
            reason: format!("provider_feature_unsupported: {truncated}"),
        };
    }

    EntryError::CallTerminal {
        reason: format!("body_shape_error: {truncated}"),
    }
}

/// Classify a pool-branch HTTP 404 response body. 404 on pool providers
/// is most commonly "model not found" (OpenRouter returns 404 for an
/// unknown slug) — same argument as the 400 path: a sibling route with a
/// different model could still succeed. Fall through to `CallTerminal`
/// for genuinely structural 404s (unknown route path, etc.).
pub(crate) fn classify_pool_404(body: &str) -> EntryError {
    let body_lower = body.to_lowercase();
    let truncated = truncate_utf8(body, 200);

    if body_lower.contains("not a valid model")
        || body_lower.contains("model not found")
        || body_lower.contains("no such model")
        || body_lower.contains("unknown model")
        || body_lower.contains("unsupported model")
        || body_lower.contains("invalid model")
    {
        return EntryError::RouteSkipped {
            reason: format!("provider_rejected_model: {truncated}"),
        };
    }

    EntryError::CallTerminal {
        reason: format!("model_not_found: {truncated}"),
    }
}

/// Resolve the model string for a single walker route entry (Option C
/// hybrid — post-ship C1 fix).
///
/// Priority:
///   1. `entry.model_id` — explicit per-route operator override.
///   2. Tier-routing lookup — `entry.tier_name` → TierRoutingEntry; the
///      row's `provider_id` MUST match `entry.provider_id` or we fall
///      through (never cross-provider a mismatched slug).
///   3. `primary_model_fallback` — caller passes the legacy
///      `config.primary_model` (or whatever context-cascade value the
///      walker settled on); used when neither 1 nor 2 produced a model.
///
/// Exposed as `pub(crate)` for unit tests. The walker inlines this same
/// logic in the dispatch loop to keep the single log line about which
/// priority fired, but the resolver here is the authoritative spec.
pub(crate) fn resolve_route_model(
    entry: &crate::pyramid::dispatch_policy::RouteEntry,
    registry: Option<&std::sync::Arc<crate::pyramid::provider::ProviderRegistry>>,
    primary_model_fallback: &str,
) -> String {
    if let Some(ref explicit) = entry.model_id {
        return explicit.clone();
    }
    if let Some(tier_name) = entry.tier_name.as_deref() {
        if let Some(reg) = registry {
            if let Some(tier_row) = reg.get_tier(tier_name) {
                if tier_row.provider_id == entry.provider_id {
                    return tier_row.model_id;
                }
            }
        }
    }
    primary_model_fallback.to_string()
}

#[derive(Debug, Clone, Default)]
pub struct LlmCallOptions {
    pub min_timeout_secs: Option<u64>,
    /// When true, the GPU processing loop bypasses semaphore/pool
    /// acquisition. Set by the queue consumer; callers never set this.
    pub skip_concurrency_gate: bool,
    /// When true, skip fleet dispatch (prevents re-dispatch loop).
    /// Set by the fleet handler on the receiving node.
    pub skip_fleet_dispatch: bool,
    /// Pre-assigned job_path from the GPU loop for cloud fallthrough.
    /// When Some, WP-8 uses this value instead of generating a new path,
    /// preserving lifecycle grouping with queue events.
    pub chronicle_job_path: Option<String>,
    /// Where this dispatch came from. Set by `handle_fleet_dispatch`
    /// and `handle_market_dispatch` when they invoke the unified call
    /// on behalf of a remote requester. Drives the chronicle `source`
    /// label on the compute-queue entry so provider-side history
    /// distinguishes market-received from fleet-received jobs.
    pub dispatch_origin: DispatchOrigin,
}

// ── Provider synthesis (Phase 3 bridge) ──────────────────────────────────────

/// Build a concrete `LlmProvider` trait object for a call. When the
/// config has a provider registry attached, we look up the default
/// `openrouter` provider row and instantiate it through the registry
/// (which resolves the `${VAR_NAME}` credential references). When the
/// registry is absent (unit tests or the narrow transitional state
/// before DB init), we synthesize an `OpenRouterProvider` from the
/// legacy `LlmConfig.api_key` field so the existing call sites that
/// construct an `LlmConfig::default()` and go straight to HTTP still
/// work.
///
/// Returns `(provider_impl, optional_secret, provider_type)`.
/// `provider_type` is used for tracing so the logs record which
/// backend handled the call.
pub(crate) fn build_call_provider(
    config: &LlmConfig,
) -> Result<(Box<dyn LlmProvider>, Option<ResolvedSecret>, ProviderType, String)> {
    if let Some(registry) = &config.provider_registry {
        // Use the active provider: ollama-local when local mode is on,
        // openrouter otherwise. active_provider_id() checks which
        // non-openrouter providers are enabled.
        let provider_id = registry.active_provider_id();
        let provider = registry
            .get_provider(&provider_id)
            .ok_or_else(|| anyhow!("provider '{}' is not registered — run DB init", provider_id))?;
        let (impl_box, secret) = registry.instantiate_provider(&provider)?;
        let provider_type = provider.provider_type;
        return Ok((impl_box, secret, provider_type, provider_id));
    }

    // Transitional fallback path: no registry, no credential store.
    // Build an `OpenRouterProvider` directly from the legacy api_key
    // field. This is only hit by unit tests and the narrow window
    // between app start and DB init; production boots always attach a
    // registry.
    let provider = OpenRouterProvider {
        id: "openrouter".into(),
        display_name: "OpenRouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        extra_headers: vec![],
    };
    let secret = if config.api_key.is_empty() {
        None
    } else {
        Some(ResolvedSecret::new(config.api_key.clone()))
    };
    Ok((Box::new(provider), secret, ProviderType::Openrouter, "openrouter".to_string()))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Resolve the context limit for the current model based on config.
fn resolve_context_limit(model: &str, config: &LlmConfig) -> usize {
    if model == config.primary_model {
        config.primary_context_limit
    } else if model == config.fallback_model_1 {
        config.fallback_1_context_limit
    } else {
        // fallback_model_2 or unknown — use the largest limit
        config.fallback_1_context_limit.max(config.primary_context_limit)
    }
}

/// Estimate token count for pre-flight model selection using tiktoken cl100k_base.
/// Falls back to len/4 if the tokenizer fails to initialize.
///
/// Runs on the blocking thread pool (8MB stack) via spawn_blocking because
/// tiktoken's fancy-regex engine is recursive and overflows the 2MB async
/// worker thread stack on large inputs (observed at 699+ doc prompts).
async fn estimate_tokens_llm(system_prompt: &str, user_prompt: &str) -> usize {
    let sys = system_prompt.to_string();
    let usr = user_prompt.to_string();
    tokio::task::spawn_blocking(move || {
        use std::sync::OnceLock;
        static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
        let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok());
        match bpe {
            Some(encoder) => {
                encoder.encode_with_special_tokens(&sys).len()
                    + encoder.encode_with_special_tokens(&usr).len()
            }
            None => (sys.len() + usr.len()) / 4,
        }
    })
    .await
    .unwrap_or_else(|_| (system_prompt.len() + user_prompt.len()) / 4)
}

/// Short model name for logging (part after the slash).
fn short_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn compute_timeout(
    prompt_chars: usize,
    options: &LlmCallOptions,
    base_secs: u64,
    max_secs: u64,
    chars_per_increment: usize,
    increment_secs: u64,
) -> std::time::Duration {
    let increments = if chars_per_increment > 0 {
        (prompt_chars / chars_per_increment) as u64
    } else {
        0
    };
    let derived_secs = std::cmp::min(max_secs, base_secs + increments * increment_secs);
    let timeout_secs = options.min_timeout_secs.unwrap_or(0).max(derived_secs);
    std::time::Duration::from_secs(timeout_secs)
}

// NOTE: The legacy `parse_openrouter_response_body` +
// `sanitize_json_candidate` helpers were removed in Phase 3. Their
// responsibilities moved to
// `pyramid::provider::OpenRouterProvider::parse_response`, which is the
// single place that encodes the OpenRouter JSON envelope shape. The
// provider's test suite covers the same SSE / prefixed-json fixtures
// the old tests exercised.

// ── Unified entry point ──────────────────────────────────────────────────────

/// Unified LLM call: returns content + usage + generation_id in a single response.
///
/// This is the canonical entry point. All other `call_model*` functions delegate here.
/// Supports optional `response_format` for structured output enforcement.
pub async fn call_model_unified(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
) -> Result<LlmResponse> {
    call_model_unified_with_options(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        LlmCallOptions::default(),
    )
    .await
}

pub async fn call_model_unified_with_options(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    options: LlmCallOptions,
) -> Result<LlmResponse> {
    // Delegate to the ctx-aware variant with `None` so legacy callers
    // (including tests and the pre-init boot window) bypass the cache
    // entirely. The cache is opt-in via StepContext presence.
    call_model_unified_with_options_and_ctx(
        config,
        None,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        options,
    )
    .await
}

/// Phase 6: StepContext-aware variant of `call_model_unified_with_options`.
///
/// When `ctx` is `Some(&StepContext)` AND the context carries a resolved
/// model id + a non-empty prompt hash, this function consults
/// `pyramid_step_cache` BEFORE making the HTTP request. On a valid cache
/// hit the cached response is returned directly (and `CacheHit` is
/// emitted on the event bus if one is attached). On a cache miss the
/// HTTP retry loop runs and the successful response is persisted to the
/// cache before returning.
///
/// When `ctx` is `None` (or its cache fields are unpopulated), this
/// function is behaviorally identical to the pre-Phase-6 code path — no
/// cache read, no cache write. This preserves backward compatibility for
/// every call site that has not yet been retrofitted.
///
/// ## Correctness gates
///
/// * `verify_cache_hit` is checked on every hit. All four mismatch
///   variants + corruption detection are exact per the spec. A non-Valid
///   result deletes the stale row and falls through to HTTP (and emits
///   `CacheHitVerificationFailed`).
/// * `ctx.force_fresh` bypasses the cache read path entirely and routes
///   through `supersede_cache_entry` on write so the prior row is
///   preserved as a `supersedes_cache_id` chain link.
/// * Cache writes use the DB path stashed on the StepContext — NOT the
///   writer mutex — because the cache is content-addressable and
///   `INSERT OR REPLACE` on a unique key is safe without serialization.
///
/// ## Phase 18b
///
/// This function now accepts an internal `audit: Option<&AuditContext>`
/// parameter at the end of the signature via the new
/// `call_model_unified_with_audit_and_ctx` entry point. The legacy
/// public signature (no audit) is preserved here as a thin wrapper that
/// passes `None`. Retrofit call sites that previously bypassed the
/// cache by calling `call_model_audited` should be migrated to
/// `call_model_unified_with_audit_and_ctx` so the cache becomes
/// reachable from the audited path.
#[allow(clippy::too_many_arguments)]
pub async fn call_model_unified_with_options_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    options: LlmCallOptions,
) -> Result<LlmResponse> {
    call_model_unified_with_audit_and_ctx(
        config,
        ctx,
        None,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        options,
    )
    .await
}

/// Phase 18b: cache + audit unified entry point.
///
/// Threads BOTH a `StepContext` (for cache lookup/storage) and an
/// `AuditContext` (for the Live Pyramid Theatre audit trail) through
/// a single call path. Retrofit call sites that previously bypassed
/// the cache by calling `call_model_audited` should be migrated to
/// this entry point.
///
/// When the call serves from cache, an audit row is still written —
/// stamped `cache_hit = true` — so the audit trail remains contiguous
/// and the DADBEAR Oversight page / cost reconciliation can show the
/// savings without losing audit-completeness.
///
/// When `audit` is `None`, behavior is identical to
/// `call_model_unified_with_options_and_ctx`. When `ctx` is `None` or
/// not cache-usable, the cache is bypassed but the audit trail is
/// still written via the existing pending → complete dance.
#[allow(clippy::too_many_arguments)]
pub async fn call_model_unified_with_audit_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    audit: Option<&AuditContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    _max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    options: LlmCallOptions,
) -> Result<LlmResponse> {
    // Save chronicle_job_path before it might move into the queue path.
    let saved_chronicle_job_path = options.chronicle_job_path.clone();

    // ── Phase 6: Cache lookup path ──────────────────────────────────
    //
    // Delegated to `try_cache_lookup_or_key`. When it returns
    // `CacheProbeOutcome::Hit` the cached response short-circuits the
    // HTTP path entirely.
    //
    // Phase 18b: cache hits still write an audit row stamped as such
    // (when an AuditContext is supplied) so the audit trail remains
    // contiguous and DADBEAR Oversight can show cache savings.
    let probe_started = std::time::Instant::now();
    let cache_lookup = match try_cache_lookup_or_key(ctx, system_prompt, user_prompt) {
        CacheProbeOutcome::Hit(response) => {
            if let Some(audit_ctx) = audit {
                let model_for_row = ctx
                    .and_then(|c| c.resolved_model_id.clone())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| config.primary_model.clone());
                let latency_ms = probe_started.elapsed().as_millis() as i64;
                let conn = audit_ctx.conn.lock().await;
                let _ = super::db::insert_llm_audit_cache_hit(
                    &conn,
                    &audit_ctx.slug,
                    &audit_ctx.build_id,
                    audit_ctx.node_id.as_deref(),
                    &audit_ctx.step_name,
                    &audit_ctx.call_purpose,
                    audit_ctx.depth,
                    &model_for_row,
                    system_prompt,
                    user_prompt,
                    &response.content,
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    latency_ms,
                    response.generation_id.as_deref(),
                );
            }
            return Ok(response);
        }
        CacheProbeOutcome::MissOrBypass(lookup) => lookup,
    };

    // Resolve the provider trait impl + credential for this call. The
    // registry path is preferred; if no registry is attached to the
    // config we synthesize an `OpenRouterProvider` from the legacy
    // fields. Either way the resulting `Box<dyn LlmProvider>` owns the
    // URL, headers, and response parser — `llm.rs` no longer encodes
    // any of that.
    //
    // `_provider_impl` + `_secret` are unused today — the walker re-instantiates
    // per-entry (Wave 1), and the Phase B market pre-loop (Wave 3 will inline)
    // does not touch them. Underscore-prefixed to silence unused-var warnings.
    let (_provider_impl, _secret, provider_type, provider_id) = build_call_provider(config)?;

    // Phase D: resolve the dispatch route BEFORE the retry loop so we
    // have the provider preference chain for escalation. When no policy
    // is configured the resolved_route is None and we fall through to
    // the legacy single-provider path.
    let resolved_route = config.dispatch_policy.as_ref().map(|policy| {
        // Use Build as the default work_type — Phase B work_type tagging
        // will provide the real classification per call site.
        let work_type = crate::pyramid::dispatch_policy::WorkType::Build;
        let step_name = ctx.map(|c| c.step_name.as_str()).unwrap_or("");
        let depth = ctx.map(|c| c.depth);
        policy.resolve_route(work_type, "", step_name, depth)
    });



    // ── Phase 18b: Audit pending row insert ─────────────────────────
    //
    // Mirror the legacy `call_model_audited` flow: insert a pending row
    // BEFORE the HTTP call so a crash mid-call leaves a trace. The row
    // is updated to 'complete' or 'failed' below. Queueing and fleet
    // dispatch both happen earlier, so this row now tracks only the
    // actual execution path that will perform the provider HTTP call.
    let audit_id: Option<i64> = if let Some(audit_ctx) = audit {
        let conn = audit_ctx.conn.lock().await;
        super::db::insert_llm_audit_pending(
            &conn,
            &audit_ctx.slug,
            &audit_ctx.build_id,
            audit_ctx.node_id.as_deref(),
            &audit_ctx.step_name,
            &audit_ctx.call_purpose,
            audit_ctx.depth,
            &config.primary_model,
            system_prompt,
            user_prompt,
        )
        .ok()
    } else {
        None
    };

    let call_started = std::time::Instant::now();

    // Note: `provider_type` from `build_call_provider` is used by the
    // synthetic-entry fallback inside the walker (pool branch). Former
    // Phase D re-instantiated on escalation — the walker re-instantiates
    // per entry inside the pool branch, so the outer bindings are
    // read-only now (no `mut` needed).

    // ── Walker loop (Walker Re-Plan Wire 2.1 §3) ─────────────────────
    //
    // Per-entry walker over `route.providers`. Every entry obeys the
    // same contract: runtime-gate → try_acquire (saturation advances) →
    // dispatch → three-tier EntryError (Retryable/RouteSkipped advance;
    // CallTerminal bubbles).
    //
    // Wave 3b scope: pool + fleet + market all inline in the walker.
    // Phase B market pre-loop deleted; the rev-2.0 `compute_requester`
    // module was removed in Wave 5. Market branch uses
    // compute_quote_flow::{quote, purchase, register_pending, fill,
    // await_result} — register-BEFORE-fill closes the Wave 3a race.
    //
    // `escalation_timeout_secs` retired (plan §2). `try_acquire_owned`
    // is non-blocking — saturation advances immediately rather than
    // waiting N seconds for a specific pool to drain.

    // Estimate input tokens once — same prompts across all entries.
    let est_input_tokens = estimate_tokens_llm(system_prompt, user_prompt).await;

    // Precompute cache key (used by every attempt's LlmCallStarted).
    let cache_key_for_event = cache_lookup
        .as_ref()
        .map(|l| l.cache_key.clone())
        .unwrap_or_default();

    // Synthetic entry list: when no route or route is empty, fall back to
    // a single-entry walker over the default provider (preserves pre-walker
    // behavior for tests + pre-init callers). When route.bypass_pool is
    // true, suppress `try_acquire_owned` for this walker invocation.
    let (walker_entries, walker_bypass_pool): (
        Vec<crate::pyramid::dispatch_policy::RouteEntry>,
        bool,
    ) = match resolved_route.as_ref() {
        Some(r) if !r.providers.is_empty() => (r.providers.clone(), r.bypass_pool),
        _ => (
            vec![crate::pyramid::dispatch_policy::RouteEntry {
                provider_id: provider_id.clone(),
                model_id: None,
                tier_name: None,
                is_local: provider_type == ProviderType::OpenaiCompat,
                max_budget_credits: None,
            }],
            false,
        ),
    };

    let walker_started = std::time::Instant::now();
    // `last_attempted_provider_id` is written whenever the walker enters
    // a pool branch (before HTTP dispatch). On `CallTerminal` the audit
    // row stamps this value so downstream debugging can see which entry
    // rejected. Compiler warns when the walker exhausts without any pool
    // attempt (fleet/market-only routes) because the write is then never
    // read; the #[allow] covers that case.
    #[allow(unused_assignments)]
    let mut last_attempted_provider_id: Option<String> = None;
    let mut skip_reasons: Vec<String> = Vec::new();
    let walker_source_label = options.dispatch_origin.source_label().to_string();

    let walker_entries_total = walker_entries.len();
    for (entry_idx, entry) in walker_entries.iter().enumerate() {
        let branch = classify_branch(&entry.provider_id);

        // 1) Runtime gate — origin-based default (§2.5.2).
        if !branch_allowed(branch, options.dispatch_origin) {
            // Structural — log-only, NOT chronicle. See plan §3 — queue
            // replays would flood `pyramid_compute_events`.
            tracing::debug!(entry = %entry.provider_id, "walker: replay_guard skip");
            skip_reasons.push(format!("{}:replay_guard", entry.provider_id));
            continue;
        }

        // Wave 3b: market branch — inline three-RPC /quote → /purchase →
        // /fill per plan §4.2. Runtime gate + advisory cache consult +
        // dispatch_market_entry (which enforces register-BEFORE-fill).
        if matches!(branch, RouteBranch::Market) {
            // ── Runtime gate ─────────────────────────────────────────
            //
            // branch_allowed(Market, origin) already passed above.
            // Remaining gate checks per plan §4.2.

            // compute_market_context must be present (Local-origin
            // replays may still carry it after prepare_for_replay).
            let market_ctx = match config.compute_market_context.as_ref() {
                Some(c) => c,
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "no_market_context" }),
                    );
                    skip_reasons
                        .push(format!("{}:no_market_context", entry.provider_id));
                    continue;
                }
            };

            // Tunnel readiness — inlined from retired should_try_market.
            // Connected + tunnel_url Some required before we can advertise
            // a callback URL to Wire. Callback URL is captured here.
            let callback_url = {
                let ts = market_ctx.tunnel_state.read().await;
                let connected = matches!(
                    ts.status,
                    crate::tunnel::TunnelConnectionStatus::Connected
                );
                if !connected {
                    None
                } else {
                    ts.tunnel_url.as_ref().map(|u| {
                        let base = u.as_str().trim_end_matches('/').to_string();
                        format!("{}/v1/compute/job-result", base)
                    })
                }
            };
            let callback_url = match callback_url {
                Some(u) => u,
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "tunnel_not_connected" }),
                    );
                    skip_reasons
                        .push(format!("{}:tunnel_not_connected", entry.provider_id));
                    continue;
                }
            };

            // Resolve the canonical model id for this entry.
            let market_model_id = entry
                .model_id
                .clone()
                .filter(|m| !m.is_empty())
                .or_else(|| {
                    ctx.and_then(|c| c.resolved_model_id.clone())
                        .filter(|m| !m.is_empty())
                })
                .unwrap_or_else(|| config.primary_model.clone());

            // Advisory cache pre-check (plan §4.2 "Acquire"). Cache is
            // advisory only; /quote is authoritative. Missing cache →
            // proceed (cold-start should not block the only market
            // path). If cache present AND active_offers == 0 → advance.
            if let Some(cache) = config.market_surface_cache.as_ref() {
                if let Some(model_entry) = cache.get_model(&market_model_id).await {
                    if model_entry.active_offers == 0 {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_MODEL_UNAVAILABLE,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({
                                "reason": "no_offers_for_model",
                                "model_id": market_model_id,
                            }),
                        );
                        skip_reasons
                            .push(format!("{}:no_offers_for_model", entry.provider_id));
                        continue;
                    }
                }
                // Cache miss / cold → fall through and let /quote speak.
            }

            // Read the participation policy's market_dispatch_max_wait_ms
            // out of the DB once per market entry. On any failure we fall
            // back to the default (60s) — absence of a policy row means
            // "use defaults", not "block market".
            let (market_max_wait_ms, market_saturation_patience_secs): (u64, u64) = {
                let db_path = ctx
                    .map(|c| c.db_path.clone())
                    .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
                match db_path {
                    Some(dbp) => tokio::task::spawn_blocking(move || {
                        rusqlite::Connection::open(&dbp)
                            .ok()
                            .and_then(|conn| {
                                crate::pyramid::local_mode::get_compute_participation_policy(&conn)
                                    .ok()
                            })
                            .map(|p| {
                                let eff = p.effective_booleans();
                                (
                                    eff.market_dispatch_max_wait_ms,
                                    eff.market_saturation_patience_secs,
                                )
                            })
                    })
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or((900_000, 3600)),
                    None => (900_000, 3600),
                }
            };

            last_attempted_provider_id = Some(entry.provider_id.clone());

            // ── Dispatch via helper ──────────────────────────────────
            //
            // Per-entry `max_budget_credits` cap fed into Wire's /quote
            // `max_budget` field. Absent → NO_BUDGET_CAP sentinel
            // (2^53 - 1, JS Number.MAX_SAFE_INTEGER; round-trips f64
            // cleanly). Wire's 409 budget_exceeded fires when the
            // estimated total exceeds this — walker advances via
            // EntryError::RouteSkipped, network_rate_above_budget
            // chronicle, next entry tried.
            let max_budget = entry
                .max_budget_credits
                .unwrap_or(crate::pyramid::dispatch_policy::NO_BUDGET_CAP);

            let max_tokens_i64 = if _max_tokens == 0 {
                0i64
            } else {
                _max_tokens as i64
            };

            let market_outcome = dispatch_market_entry(MarketDispatchArgs {
                config,
                ctx,
                market_ctx,
                model_id: market_model_id.clone(),
                max_budget,
                max_wait_ms: market_max_wait_ms,
                market_saturation_patience_secs,
                max_tokens: max_tokens_i64,
                temperature,
                input_tokens_est: est_input_tokens as i64,
                system_prompt,
                user_prompt,
                callback_url,
                walker_source_label: &walker_source_label,
                entry_provider_id: &entry.provider_id,
            })
            .await;

            match market_outcome {
                Ok(response) => {
                    let latency_ms = call_started.elapsed().as_millis() as i64;
                    let walker_ms = walker_started.elapsed().as_millis() as i64;

                    // Optional cache store on success.
                    try_cache_store(ctx, cache_lookup.as_ref(), &response, call_started);

                    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                        let conn = audit_ctx.conn.lock().await;
                        let _ = super::db::complete_llm_audit(
                            &conn,
                            id,
                            &response.content,
                            true,
                            response.usage.prompt_tokens,
                            response.usage.completion_tokens,
                            latency_ms,
                            response.generation_id.as_deref(),
                            Some(entry.provider_id.as_str()),
                        );
                    }

                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_WALKER_RESOLVED,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({
                            "latency_ms": latency_ms,
                            "total_walker_ms": walker_ms,
                            "attempts": entry_idx + 1,
                            "branch": "market",
                        }),
                    );

                    return Ok(response);
                }
                Err(EntryError::Retryable { reason }) => {
                    // Per-slug specific event FIRST (additive) so
                    // operator telemetry keyed on e.g.
                    // `network_quote_expired` lights up; generic
                    // `network_route_retryable_fail` follows for
                    // dashboards keyed on the walker frame-of-reference.
                    if let Some(specific) = map_market_slug_to_specific_event(&reason) {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            specific,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({
                                "reason": reason,
                                "branch": "market",
                                "classification": "retryable",
                            }),
                        );
                    }
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_RETRYABLE_FAIL,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason, "branch": "market" }),
                    );
                    skip_reasons
                        .push(format!("{}:retryable({})", entry.provider_id, reason));
                    continue;
                }
                Err(EntryError::RouteSkipped { reason }) => {
                    if let Some(specific) = map_market_slug_to_specific_event(&reason) {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            specific,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({
                                "reason": reason,
                                "branch": "market",
                                "classification": "route_skipped",
                            }),
                        );
                    }
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason, "branch": "market" }),
                    );
                    skip_reasons
                        .push(format!("{}:route_skipped({})", entry.provider_id, reason));
                    continue;
                }
                Err(EntryError::CallTerminal { reason }) => {
                    // CallTerminal also covers stage-tagged auth
                    // reasons (`quote_auth_failed` / `fill_auth_failed`)
                    // that should surface as `network_auth_expired` for
                    // operator telemetry before the generic terminal
                    // event + step-error bubble.
                    if let Some(specific) = map_market_slug_to_specific_event(&reason) {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            specific,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({
                                "reason": reason.clone(),
                                "branch": "market",
                                "classification": "call_terminal",
                            }),
                        );
                    }
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_TERMINAL_FAIL,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason.clone(), "branch": "market" }),
                    );
                    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                        let conn = audit_ctx.conn.lock().await;
                        let _ = super::db::fail_llm_audit(
                            &conn,
                            id,
                            &reason,
                            last_attempted_provider_id.as_deref(),
                        );
                    }
                    emit_step_error(ctx, &reason);
                    return Err(anyhow!(reason));
                }
            }
        }

        // Wave 2: fleet branch — origin-gated + skip-fleet-dispatch-gated;
        // snapshots fleet_ctx + policy + callback_url once (TOCTOU-safe);
        // finds peer via roster; dispatches via `dispatch_fleet_entry`;
        // three-tier classification per §4.1.
        if matches!(branch, RouteBranch::Fleet) {
            // Explicit per-call override (tests / scheduled replays).
            // The primary fleet-replay guard is `branch_allowed(Fleet, origin)`
            // above; this flag stays as a secondary explicit override.
            if options.skip_fleet_dispatch {
                tracing::debug!(
                    entry = %entry.provider_id,
                    "walker: fleet_replay_guard skip (skip_fleet_dispatch)",
                );
                skip_reasons.push(format!("{}:fleet_replay_guard", entry.provider_id));
                continue;
            }

            // Rule-scoped by design — fleet serves rule names, not ad-hoc calls.
            let route_ref = match resolved_route.as_ref() {
                Some(r) if !r.matched_rule_name.is_empty() => r,
                _ => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "no_rule_match" }),
                    );
                    skip_reasons.push(format!("{}:no_rule_match", entry.provider_id));
                    continue;
                }
            };

            // Snapshot fleet_ctx + policy + callback_url atomically.
            let fleet_ctx = match config.fleet_dispatch.as_ref() {
                Some(c) => c.clone(),
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "fleet_ctx_missing" }),
                    );
                    skip_reasons
                        .push(format!("{}:fleet_ctx_missing", entry.provider_id));
                    continue;
                }
            };
            let policy_snap = fleet_ctx.policy.read().await.clone();
            let callback_url = {
                let ts = fleet_ctx.tunnel_state.read().await;
                match (&ts.status, ts.tunnel_url.as_ref()) {
                    (crate::tunnel::TunnelConnectionStatus::Connected, Some(u)) => {
                        Some(u.endpoint("/v1/fleet/result"))
                    }
                    _ => None,
                }
            };
            let callback_url = match callback_url {
                Some(u) => u,
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "tunnel_not_connected" }),
                    );
                    skip_reasons
                        .push(format!("{}:tunnel_not_connected", entry.provider_id));
                    continue;
                }
            };

            let roster_handle = match config.fleet_roster.as_ref() {
                Some(r) => r.clone(),
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "fleet_roster_missing" }),
                    );
                    skip_reasons
                        .push(format!("{}:fleet_roster_missing", entry.provider_id));
                    continue;
                }
            };

            // Acquire: non-blocking peer lookup. No permit held — fleet
            // is not pool-limited.
            let (peer, jwt) = {
                let roster = roster_handle.read().await;
                match roster.find_peer_for_rule(
                    &route_ref.matched_rule_name,
                    policy_snap.peer_staleness_secs,
                ) {
                    Some(peer) => {
                        let jwt = roster.fleet_jwt.clone().unwrap_or_default();
                        (peer.clone(), jwt)
                    }
                    None => {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "reason": "no_fleet_peer" }),
                        );
                        skip_reasons
                            .push(format!("{}:no_fleet_peer", entry.provider_id));
                        continue;
                    }
                }
            };
            if jwt.is_empty() {
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({ "reason": "jwt_unavailable" }),
                );
                skip_reasons.push(format!("{}:jwt_unavailable", entry.provider_id));
                continue;
            }

            last_attempted_provider_id = Some(entry.provider_id.clone());

            let rule_name = route_ref.matched_rule_name.clone();
            let job_wait_secs = route_ref.max_wait_secs;

            let fleet_outcome = dispatch_fleet_entry(FleetDispatchArgs {
                config,
                ctx,
                fleet_ctx,
                policy_snap,
                callback_url,
                roster_handle,
                peer,
                jwt,
                rule_name,
                job_wait_secs,
                system_prompt,
                user_prompt,
                temperature,
                max_tokens: _max_tokens,
                response_format,
            })
            .await;

            match fleet_outcome {
                Ok(response) => {
                    let latency_ms = call_started.elapsed().as_millis() as i64;
                    let walker_ms = walker_started.elapsed().as_millis() as i64;

                    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                        let conn = audit_ctx.conn.lock().await;
                        let _ = super::db::complete_llm_audit(
                            &conn,
                            id,
                            &response.content,
                            true,
                            response.usage.prompt_tokens,
                            response.usage.completion_tokens,
                            latency_ms,
                            response.generation_id.as_deref(),
                            Some(entry.provider_id.as_str()),
                        );
                    }

                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_WALKER_RESOLVED,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({
                            "latency_ms": latency_ms,
                            "total_walker_ms": walker_ms,
                            "attempts": entry_idx + 1,
                        }),
                    );

                    return Ok(response);
                }
                Err(EntryError::Retryable { reason }) => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_RETRYABLE_FAIL,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason }),
                    );
                    skip_reasons
                        .push(format!("{}:retryable({})", entry.provider_id, reason));
                    continue;
                }
                Err(EntryError::RouteSkipped { reason }) => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason }),
                    );
                    skip_reasons.push(format!(
                        "{}:route_skipped({})",
                        entry.provider_id, reason
                    ));
                    continue;
                }
                Err(EntryError::CallTerminal { reason }) => {
                    // Per §4.1 plan: no fleet branch returns CallTerminal.
                    // Defensive: bubble for unknown-variant future-proofing.
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_TERMINAL_FAIL,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": reason.clone() }),
                    );
                    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                        let conn = audit_ctx.conn.lock().await;
                        let _ = super::db::fail_llm_audit(
                            &conn,
                            id,
                            &reason,
                            last_attempted_provider_id.as_deref(),
                        );
                    }
                    emit_step_error(ctx, &reason);
                    return Err(anyhow!(reason));
                }
            }
        }

        // Pool branch — this entry is a registered provider (openrouter,
        // ollama-local, custom). Wave 1 scope.
        last_attempted_provider_id = Some(entry.provider_id.clone());

        // ── Per-entry local-execution gate (walker §4.4, post-ship fix) ─
        //
        // When this pool entry is flagged `is_local: true` and a compute
        // queue is attached, hand the call off to the GPU loop via the
        // queue rather than running the HTTP retry path inline. This used
        // to live BEFORE the walker loop, which short-circuited every
        // production route containing any is_local entry and made the
        // market + fleet branches unreachable for the bundled seed
        // (see plan §13 post-ship finding, 2026-04-21).
        //
        // Gating: skip_concurrency_gate suppresses re-enqueueing for the
        // GPU-loop replay (inner walker sets it via prepare_for_replay).
        // Route.bypass_pool similarly skips the queue hop.
        if entry.is_local
            && !options.skip_concurrency_gate
            && !walker_bypass_pool
        {
            if let Some(ref queue_handle) = config.compute_queue {
                let queue_model_id = entry
                    .model_id
                    .clone()
                    .filter(|m| !m.is_empty())
                    .or_else(|| ctx.and_then(|c| c.resolved_model_id.clone()))
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| config.primary_model.clone());

                let (tx, rx) = tokio::sync::oneshot::channel();

                // Derive replay config via prepare_for_replay — clears
                // compute_queue (re-enqueue guard) + fleet + market
                // contexts so the GPU loop processes this entry as a
                // pool-only local call. See impl LlmConfig::prepare_for_replay.
                let gpu_config = config.prepare_for_replay(options.dispatch_origin);

                // Label the queue-entry source so downstream chronicle
                // emitters attribute the job to its true origin.
                let entry_source = options.dispatch_origin.source_label().to_string();
                let chronicle_job_path_val = options
                    .chronicle_job_path
                    .clone()
                    .unwrap_or_else(|| {
                        super::compute_chronicle::generate_job_path(
                            ctx,
                            None,
                            &queue_model_id,
                            &entry_source,
                        )
                    });
                let entry_chronicle_jp = options.chronicle_job_path.clone();

                // Clone options into the queue entry; walker continues to
                // use the outer `options` on subsequent iterations if the
                // GPU-loop replay returns an error we can't recover from.
                let mut gpu_options = options.clone();
                gpu_options.skip_concurrency_gate = true;
                gpu_options.skip_fleet_dispatch = true;

                let depth = {
                    let mut q = queue_handle.queue.lock().await;
                    q.enqueue_local(
                        &queue_model_id,
                        crate::compute_queue::QueueEntry {
                            result_tx: tx,
                            config: gpu_config,
                            system_prompt: system_prompt.to_string(),
                            user_prompt: user_prompt.to_string(),
                            temperature,
                            max_tokens: _max_tokens,
                            response_format: response_format.cloned(),
                            options: gpu_options,
                            step_ctx: ctx.cloned(), // Law 4: StepContext flows through
                            model_id: queue_model_id.clone(),
                            enqueued_at: std::time::Instant::now(),
                            work_item_id: None, // Non-DADBEAR path
                            attempt_id: None,
                            source: entry_source.clone(),
                            job_path: chronicle_job_path_val.clone(),
                            chronicle_job_path: entry_chronicle_jp,
                        },
                    );
                    q.queue_depth(&queue_model_id)
                };

                // WP-1: Chronicle enqueue event
                {
                    let db_path = ctx
                        .map(|c| c.db_path.clone())
                        .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
                    let chronicle_ctx = if let Some(sc) = ctx {
                        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                            sc, &chronicle_job_path_val, "enqueued", &entry_source,
                        )
                    } else {
                        super::compute_chronicle::ChronicleEventContext::minimal(
                            &chronicle_job_path_val, "enqueued", &entry_source,
                        )
                        .with_model_id(queue_model_id.clone())
                    };
                    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                        "queue_depth": depth,
                        "queue_model_depth": depth,
                    }));
                    if let Some(db_path) = db_path {
                        tokio::task::spawn_blocking(move || {
                            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                            }
                        });
                    }
                }

                if let Some(step) = ctx {
                    if let Some(ref bus) = step.bus {
                        let _ = bus.tx.send(super::event_bus::TaggedBuildEvent {
                            slug: "__compute__".to_string(),
                            kind: super::event_bus::TaggedKind::QueueJobEnqueued {
                                model_id: queue_model_id.clone(),
                                queue_depth: depth,
                            },
                        });
                    }
                }

                queue_handle.notify.notify_one();

                // Await GPU-loop result. Classification rationale:
                //   - Ok(response): local pool resolved the call — audit
                //     complete, emit walker_resolved, return.
                //   - Err from the GPU loop (or dropped sender): the local
                //     path has already consumed its one chance; advancing
                //     the outer walker wouldn't re-try local (the replay
                //     guard + skip flags prevent it). The inner walker has
                //     already written terminal chronicle/audit events for
                //     this job. Classify as CallTerminal so the outer
                //     walker bubbles the error rather than masking the
                //     failure behind other route entries that can't help.
                match rx.await {
                    Ok(Ok(response)) => {
                        let latency_ms = call_started.elapsed().as_millis() as i64;
                        let walker_ms = walker_started.elapsed().as_millis() as i64;

                        if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                            let conn = audit_ctx.conn.lock().await;
                            let _ = super::db::complete_llm_audit(
                                &conn,
                                id,
                                &response.content,
                                true,
                                response.usage.prompt_tokens,
                                response.usage.completion_tokens,
                                latency_ms,
                                response.generation_id.as_deref(),
                                Some(entry.provider_id.as_str()),
                            );
                        }

                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_WALKER_RESOLVED,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({
                                "latency_ms": latency_ms,
                                "total_walker_ms": walker_ms,
                                "attempts": entry_idx + 1,
                            }),
                        );

                        return Ok(response);
                    }
                    Ok(Err(err)) => {
                        let reason = format!("{err}");
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_TERMINAL_FAIL,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "reason": reason.clone() }),
                        );
                        if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                            let conn = audit_ctx.conn.lock().await;
                            let _ = super::db::fail_llm_audit(
                                &conn,
                                id,
                                &reason,
                                last_attempted_provider_id.as_deref(),
                            );
                        }
                        emit_step_error(ctx, &reason);
                        return Err(anyhow!(reason));
                    }
                    Err(_) => {
                        let reason = "compute queue: GPU loop dropped the job".to_string();
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_TERMINAL_FAIL,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "reason": reason.clone() }),
                        );
                        if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                            let conn = audit_ctx.conn.lock().await;
                            let _ = super::db::fail_llm_audit(
                                &conn,
                                id,
                                &reason,
                                last_attempted_provider_id.as_deref(),
                            );
                        }
                        emit_step_error(ctx, &reason);
                        return Err(anyhow!(reason));
                    }
                }
            }
        }

        // 2) Re-instantiate provider impl + credential for this entry.
        //
        // Registry path is preferred. When absent (tests / pre-init), the
        // only entry the walker will see is the synthetic default — use
        // the outer `provider_impl` + `secret` + `provider_type` state
        // that `build_call_provider` already populated. We have to move
        // those values out of the outer bindings, but they live across
        // iterations, so clone via rebuild on each iteration.
        let (entry_provider_impl, entry_secret, entry_provider_type): (
            Box<dyn LlmProvider>,
            Option<ResolvedSecret>,
            ProviderType,
        ) = if let Some(registry) = &config.provider_registry {
            match registry.get_provider(&entry.provider_id) {
                Some(row) => match registry.instantiate_provider(&row) {
                    Ok((impl_box, sec)) => (impl_box, sec, row.provider_type),
                    Err(e) => {
                        // Credentials substitution failed — treat as
                        // AcquireError::Unavailable("credentials_missing")
                        // per plan §4.3. Advance.
                        tracing::debug!(
                            entry = %entry.provider_id,
                            error = %e,
                            "walker: credentials_missing",
                        );
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "reason": "credentials_missing" }),
                        );
                        skip_reasons
                            .push(format!("{}:credentials_missing", entry.provider_id));
                        continue;
                    }
                },
                None => {
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "provider_not_registered" }),
                    );
                    skip_reasons
                        .push(format!("{}:provider_not_registered", entry.provider_id));
                    continue;
                }
            }
        } else {
            // No registry path — fall back to a fresh build_call_provider
            // instantiation. This path only triggers in tests / pre-init
            // where route.providers was synthesized from the default.
            match build_call_provider(config) {
                Ok((b, s, pt, _pid)) => (b, s, pt),
                Err(e) => {
                    tracing::debug!(
                        entry = %entry.provider_id,
                        error = %e,
                        "walker: build_call_provider failed (no registry)",
                    );
                    emit_walker_chronicle(
                        ctx,
                        config,
                        super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                        &walker_source_label,
                        &entry.provider_id,
                        serde_json::json!({ "reason": "provider_build_failed" }),
                    );
                    skip_reasons
                        .push(format!("{}:provider_build_failed", entry.provider_id));
                    continue;
                }
            }
        };

        // 3) Try acquire capacity. Non-blocking per plan §2 / §7.
        //    Saturation / Unavailable → advance.
        //    Skipped when `options.skip_concurrency_gate` (GPU queue replay)
        //    or when the resolved route is bypass_pool.
        let _entry_permit: Option<tokio::sync::OwnedSemaphorePermit> =
            if options.skip_concurrency_gate || walker_bypass_pool {
                None
            } else if let Some(pools) = &config.provider_pools {
                match pools.try_acquire_owned(&entry.provider_id) {
                    Ok(permit) => Some(permit),
                    Err(crate::pyramid::provider_pools::AcquireError::Saturated) => {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_SATURATED,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "capacity_kind": "pool_semaphore" }),
                        );
                        skip_reasons.push(format!("{}:saturated", entry.provider_id));
                        continue;
                    }
                    Err(crate::pyramid::provider_pools::AcquireError::Unavailable(reason)) => {
                        emit_walker_chronicle(
                            ctx,
                            config,
                            super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                            &walker_source_label,
                            &entry.provider_id,
                            serde_json::json!({ "reason": reason }),
                        );
                        skip_reasons
                            .push(format!("{}:unavailable({})", entry.provider_id, reason));
                        continue;
                    }
                }
            } else {
                None
            };

        // 4) Dispatch — HTTP retry loop relocated from former Phase D.
        let health_provider_id = entry.provider_id.clone();

        // Model selection — Option C hybrid (post-ship C1 fix):
        //   1. `entry.model_id` — explicit per-route operator override.
        //   2. `tier_routing(entry.tier_name)` — keyed on tier_name; we
        //      additionally require the tier row's provider_id to match
        //      `entry.provider_id` so we never smuggle an openrouter slug
        //      into an ollama-local route (the original C1 bug).
        //   3. Context-cascade on `config.primary_model` / fallbacks —
        //      legacy fallback preserved for backward compat.
        //
        // The resolved model drives the HTTP body, context-limit lookup,
        // and every downstream audit/chronicle emit for this entry.
        let entry_model_override = entry.model_id.clone();
        let tier_routed_model: Option<String> = if entry_model_override.is_some() {
            None
        } else {
            entry.tier_name.as_deref().and_then(|tier_name| {
                config.provider_registry.as_ref().and_then(|reg| {
                    reg.get_tier(tier_name).and_then(|tier_row| {
                        if tier_row.provider_id == entry.provider_id {
                            Some(tier_row.model_id)
                        } else {
                            // Tier row exists but is for a different
                            // provider — treat as "no tier override for
                            // this entry" rather than cross-providering
                            // an incompatible slug (C1 regression guard).
                            tracing::debug!(
                                entry_provider = %entry.provider_id,
                                tier = %tier_name,
                                tier_provider = %tier_row.provider_id,
                                "walker: tier_routing row does not match entry provider; ignoring",
                            );
                            None
                        }
                    })
                })
            })
        };
        let mut use_model = if let Some(ref model) = entry_model_override {
            info!("[entry-model->{}]", short_name(model));
            model.clone()
        } else if let Some(ref model) = tier_routed_model {
            info!("[tier-model->{}]", short_name(model));
            model.clone()
        } else if est_input_tokens > config.fallback_1_context_limit {
            info!("[fallback->{}]", short_name(&config.fallback_model_2));
            config.fallback_model_2.clone()
        } else if est_input_tokens > config.primary_context_limit {
            info!("[fallback->{}]", short_name(&config.fallback_model_1));
            config.fallback_model_1.clone()
        } else {
            config.primary_model.clone()
        };

        let client = &*HTTP_CLIENT;
        let url = entry_provider_impl.chat_completions_url();
        let built_headers = match entry_provider_impl.prepare_headers(entry_secret.as_ref()) {
            Ok(h) => h,
            Err(e) => {
                // Header prep failure is config-level — advance with
                // RouteSkipped semantic. (Classified as unavailable in
                // chronicle for operator clarity.)
                tracing::debug!(
                    entry = %entry.provider_id,
                    error = %e,
                    "walker: prepare_headers failed",
                );
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_NETWORK_ROUTE_UNAVAILABLE,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({ "reason": "prepare_headers_failed" }),
                );
                skip_reasons
                    .push(format!("{}:prepare_headers_failed", entry.provider_id));
                continue;
            }
        };

        let prompt_chars = system_prompt.len() + user_prompt.len();
        let local_timeout_scale = if entry_provider_type == ProviderType::OpenaiCompat {
            5
        } else {
            1
        };
        let timeout = compute_timeout(
            prompt_chars,
            &options,
            config.base_timeout_secs * local_timeout_scale,
            config.max_timeout_secs * local_timeout_scale,
            config.timeout_chars_per_increment,
            config.timeout_increment_secs,
        );

        // HTTP retry loop — produces either a success LlmResponse or an
        // EntryError. Relocated verbatim from the former Phase D block
        // (lines 2485-2830) with the two behavioral changes per plan §4.3:
        //   - Terminal-for-this-entry 4xx now emit EntryError::RouteSkipped
        //     or CallTerminal rather than bubbling with `return Err`.
        //   - Retries exhausted on retryable statuses → EntryError::Retryable.
        // Context-exceeded cascade (mutates `use_model`) stays as-is.
        let http_outcome: std::result::Result<LlmResponse, EntryError> = 'http: {
            let mut attempt = 0u32;
            loop {
                if attempt >= config.max_retries {
                    break 'http Err(EntryError::Retryable {
                        reason: "max_retries_exceeded".into(),
                    });
                }
                // Compute effective max_tokens.
                let model_limit = resolve_context_limit(&use_model, config);
                let effective_max_tokens = model_limit
                    .saturating_sub(est_input_tokens)
                    .min(48_000)
                    .max(1024);

                let mut body = serde_json::json!({
                    "model": use_model,
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": user_prompt}
                    ],
                    "temperature": temperature,
                    "max_tokens": effective_max_tokens
                });
                if let Some(rf) = response_format {
                    body.as_object_mut()
                        .unwrap()
                        .insert("response_format".to_string(), rf.clone());
                }

                let metadata = ctx
                    .map(RequestMetadata::from_step_context)
                    .unwrap_or_default();
                entry_provider_impl.augment_request_body(&mut body, &metadata);

                if config.provider_pools.is_none() {
                    rate_limit_wait(
                        config.rate_limit_max_requests,
                        config.rate_limit_window_secs,
                    )
                    .await;
                }

                emit_llm_call_started(ctx, &use_model, &cache_key_for_event);

                // Local-provider global fallback semaphore kept for the
                // no-pools / no-permit case.
                let _local_permit = if options.skip_concurrency_gate {
                    None
                } else if _entry_permit.is_none()
                    && entry_provider_type == ProviderType::OpenaiCompat
                {
                    match LOCAL_PROVIDER_SEMAPHORE.acquire().await {
                        Ok(p) => Some(p),
                        Err(e) => {
                            break 'http Err(EntryError::Retryable {
                                reason: format!("local_provider_semaphore_closed: {e}"),
                            });
                        }
                    }
                } else {
                    None
                };

                let mut request = client.post(&url).timeout(timeout);
                for (k, v) in &built_headers {
                    request = request.header(k, v);
                }
                let resp = request.json(&body).send().await;
                drop(_local_permit);

                let resp = match resp {
                    Ok(r) => r,
                    Err(e) => {
                        if attempt + 1 < config.max_retries {
                            info!(
                                "  request error (timeout={}s, err={}), retry {}...",
                                timeout.as_secs(),
                                e,
                                attempt + 1
                            );
                            let backoff_ms = (config.retry_base_sleep_secs as i64) * 1000;
                            emit_step_retry(
                                ctx,
                                attempt as i64,
                                config.max_retries as i64,
                                &format!("request error: {}", e),
                                backoff_ms,
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(
                                config.retry_base_sleep_secs,
                            ))
                            .await;
                            attempt += 1;
                            continue;
                        }
                        maybe_record_provider_error(
                            ctx,
                            &health_provider_id,
                            super::provider_health::ProviderErrorKind::ConnectionFailure,
                        );
                        break 'http Err(EntryError::Retryable {
                            reason: format!(
                                "request failed after {} attempts (timeout={}s): {}",
                                config.max_retries,
                                timeout.as_secs(),
                                e
                            ),
                        });
                    }
                };

                let status = resp.status().as_u16();

                // HTTP 400: cascade on context-exceeded, otherwise retry
                // on same entry/model a few times then CallTerminal.
                if status == 400 {
                    let body_400 = resp.text().await.unwrap_or_default();
                    warn!(
                        "[LLM] HTTP 400 from {} — body: {}",
                        short_name(&use_model),
                        &body_400[..body_400.len().min(500)],
                    );

                    let body_lower = body_400.to_lowercase();
                    let is_context_exceeded = body_lower.contains("context")
                        || body_lower.contains("too many tokens")
                        || body_lower.contains("token limit");

                    if is_context_exceeded && use_model != config.fallback_model_2 {
                        let prev_model = use_model.clone();
                        if use_model == config.primary_model {
                            use_model = config.fallback_model_1.clone();
                        } else {
                            use_model = config.fallback_model_2.clone();
                        }
                        warn!(
                            "[LLM] Context exceeded on {}, cascading to {}",
                            short_name(&prev_model),
                            short_name(&use_model),
                        );
                        attempt += 1;
                        continue;
                    } else if attempt + 1 < config.max_retries {
                        let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        attempt += 1;
                        continue;
                    } else {
                        // Exhausted — plan §4.3: nuanced 400 classification.
                        // Provider-level model rejections (OpenRouter
                        // "not a valid model ID", "model not found", etc.)
                        // become RouteSkipped so the walker advances to
                        // the next route entry with a different model_id.
                        // Feature-unsupported likewise. Genuine body-shape
                        // errors (malformed JSON, multi-system-turns,
                        // schema violations) become CallTerminal because
                        // every route would fail the same way.
                        let classified = classify_pool_400(&body_400);
                        let prefix = format!(
                            "HTTP 400 (not context-exceeded) after {} attempts",
                            config.max_retries,
                        );
                        let wrapped = match classified {
                            EntryError::RouteSkipped { reason } => EntryError::RouteSkipped {
                                reason: format!("{prefix}: {reason}"),
                            },
                            EntryError::CallTerminal { reason } => EntryError::CallTerminal {
                                reason: format!("{prefix}: {reason}"),
                            },
                            // classify_pool_400 never returns Retryable, but
                            // stay total over the enum defensively.
                            EntryError::Retryable { reason } => EntryError::Retryable {
                                reason: format!("{prefix}: {reason}"),
                            },
                        };
                        break 'http Err(wrapped);
                    }
                }

                // Retryable status codes — exponential backoff on same entry.
                if config.retryable_status_codes.contains(&status) {
                    let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
                    info!("  HTTP {}, waiting {}s...", status, wait);
                    if status >= 500 {
                        maybe_record_provider_error(
                            ctx,
                            &health_provider_id,
                            super::provider_health::ProviderErrorKind::Http5xx,
                        );
                    }
                    emit_step_retry(
                        ctx,
                        attempt as i64,
                        config.max_retries as i64,
                        &format!("HTTP {} retry", status),
                        (wait as i64) * 1000,
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    attempt += 1;
                    continue;
                }

                // Other non-success status — retry a few times then classify.
                if !resp.status().is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    if attempt + 1 < config.max_retries {
                        info!("  HTTP {}, retry {}...", status, attempt + 1);
                        emit_step_retry(
                            ctx,
                            attempt as i64,
                            config.max_retries as i64,
                            &format!("HTTP {} retry", status),
                            (config.retry_base_sleep_secs as i64) * 1000,
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(
                            config.retry_base_sleep_secs,
                        ))
                        .await;
                        attempt += 1;
                        continue;
                    }
                    if status >= 500 {
                        maybe_record_provider_error(
                            ctx,
                            &health_provider_id,
                            super::provider_health::ProviderErrorKind::Http5xx,
                        );
                    }
                    // Plan §4.3: 401/403 = RouteSkipped (credentials stale
                    // for THIS provider; other routes still viable).
                    // 404 = CallTerminal (model not found — structural).
                    // Other non-success = Retryable (walker advances).
                    let err_msg = format!(
                        "HTTP {} after {} attempts: {}",
                        status,
                        config.max_retries,
                        &body_text[..body_text.len().min(200)]
                    );
                    let classified = match status {
                        401 | 403 => EntryError::RouteSkipped {
                            reason: format!("credentials_stale: {err_msg}"),
                        },
                        404 => {
                            // Plan §4.3: 404 is ambiguous — "model not
                            // found" bodies become RouteSkipped so a
                            // sibling route with a different model can
                            // still succeed; genuinely structural 404s
                            // fall through to CallTerminal.
                            let inner = classify_pool_404(&body_text);
                            match inner {
                                EntryError::RouteSkipped { reason } => EntryError::RouteSkipped {
                                    reason: format!("{err_msg} [{reason}]"),
                                },
                                _ => EntryError::CallTerminal {
                                    reason: format!("model_not_found: {err_msg}"),
                                },
                            }
                        }
                        _ => EntryError::Retryable { reason: err_msg },
                    };
                    break 'http Err(classified);
                }

                let body_text = match resp.text().await {
                    Ok(text) => text,
                    Err(e) => {
                        if attempt + 1 < config.max_retries {
                            info!(
                                "  response-read error (timeout={}s, err={}), retry {}...",
                                timeout.as_secs(),
                                e,
                                attempt + 1
                            );
                            emit_step_retry(
                                ctx,
                                attempt as i64,
                                config.max_retries as i64,
                                &format!("response read error: {}", e),
                                (config.retry_base_sleep_secs as i64) * 1000,
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(
                                config.retry_base_sleep_secs,
                            ))
                            .await;
                            attempt += 1;
                            continue;
                        }
                        break 'http Err(EntryError::Retryable {
                            reason: format!(
                                "failed to read response after {} attempts: {}",
                                config.max_retries, e
                            ),
                        });
                    }
                };

                let parsed: ParsedLlmResponse =
                    match entry_provider_impl.parse_response(&body_text) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(
                                "[LLM] response envelope parse failed on {} attempt {}: {}",
                                short_name(&use_model),
                                attempt + 1,
                                e
                            );
                            if config.llm_debug_logging {
                                let preview_len = body_text.len().min(2000);
                                warn!(
                                    "[LLM-DEBUG] Raw response body that failed envelope parse (model={}, len={}):\n{}",
                                    short_name(&use_model),
                                    body_text.len(),
                                    &body_text[..preview_len],
                                );
                            }
                            if attempt + 1 < config.max_retries {
                                info!("  parse error, retry {}...", attempt + 1);
                                emit_step_retry(
                                    ctx,
                                    attempt as i64,
                                    config.max_retries as i64,
                                    &format!("parse error: {}", e),
                                    (config.retry_base_sleep_secs as i64) * 1000,
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(
                                    config.retry_base_sleep_secs,
                                ))
                                .await;
                                attempt += 1;
                                continue;
                            }
                            break 'http Err(EntryError::Retryable {
                                reason: format!(
                                    "failed to parse response after {} attempts: {}",
                                    config.max_retries, e
                                ),
                            });
                        }
                    };

                let usage = parsed.usage.clone();
                let generation_id = parsed.generation_id.clone();
                let finish_reason_str = parsed
                    .finish_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());

                info!(
                    "[LLM] provider={} model={} finish_reason={} prompt_tokens={} completion_tokens={}",
                    entry_provider_type.as_str(),
                    short_name(&use_model),
                    finish_reason_str,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                );

                if config.llm_debug_logging {
                    let content_len = parsed.content.len();
                    if finish_reason_str != "stop" || content_len > 20_000 {
                        let preview = &parsed.content[..parsed.content.len().min(2000)];
                        warn!(
                            "[LLM-DEBUG] Abnormal response (model={}, finish_reason={}, content_len={}, prompt_tokens={}, completion_tokens={}):\n{}",
                            short_name(&use_model),
                            finish_reason_str,
                            content_len,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            preview,
                        );
                    }
                }

                if parsed.content.is_empty() {
                    if attempt + 1 < config.max_retries {
                        info!("  empty content, retry {}...", attempt + 1);
                        emit_step_retry(
                            ctx,
                            attempt as i64,
                            config.max_retries as i64,
                            "empty content",
                            (config.retry_base_sleep_secs as i64) * 1000,
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(
                            config.retry_base_sleep_secs,
                        ))
                        .await;
                        attempt += 1;
                        continue;
                    }
                    break 'http Err(EntryError::Retryable {
                        reason: format!(
                            "model returned empty content after {} attempts",
                            config.max_retries
                        ),
                    });
                }

                let response = LlmResponse {
                    content: parsed.content,
                    usage,
                    generation_id,
                    actual_cost_usd: parsed.actual_cost_usd,
                    provider_id: Some(entry_provider_type.as_str().to_string()),
                    fleet_peer_id: None,
                    fleet_peer_model: None,
                };

                // Cache store on success.
                try_cache_store(ctx, cache_lookup.as_ref(), &response, call_started);

                let cost_usd = response
                    .actual_cost_usd
                    .unwrap_or_else(|| super::config_helper::estimate_cost(&response.usage));
                let latency_ms = call_started.elapsed().as_millis() as i64;
                emit_llm_call_completed(
                    ctx,
                    &use_model,
                    &cache_key_for_event,
                    &response.usage,
                    cost_usd,
                    latency_ms,
                );

                // WP-8 cloud_returned chronicle (unchanged).
                if entry_provider_type == ProviderType::Openrouter {
                    let cloud_job_path =
                        saved_chronicle_job_path.clone().unwrap_or_else(|| {
                            super::compute_chronicle::generate_job_path(
                                ctx, None, &use_model, "cloud",
                            )
                        });
                    let chronicle_ctx = if let Some(sc) = ctx {
                        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                            sc,
                            &cloud_job_path,
                            "cloud_returned",
                            "cloud",
                        )
                    } else {
                        super::compute_chronicle::ChronicleEventContext::minimal(
                            &cloud_job_path,
                            "cloud_returned",
                            "cloud",
                        )
                        .with_model_id(use_model.clone())
                    };
                    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                        "provider_id": response.provider_id,
                        "latency_ms": latency_ms,
                        "tokens_prompt": response.usage.prompt_tokens,
                        "tokens_completion": response.usage.completion_tokens,
                        "cost_usd": cost_usd,
                        "generation_id": response.generation_id,
                        "actual_cost_usd": response.actual_cost_usd,
                    }));
                    let db_path = ctx
                        .map(|c| c.db_path.clone())
                        .or_else(|| {
                            config.cache_access.as_ref().map(|ca| ca.db_path.to_string())
                        });
                    if let Some(db_path) = db_path {
                        tokio::task::spawn_blocking(move || {
                            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                let _ = super::compute_chronicle::record_event(
                                    &conn,
                                    &chronicle_ctx,
                                );
                            }
                        });
                    }
                }

                break 'http Ok(response);
            }
        };

        // Drop entry permit before the outcome handling so subsequent
        // walker iterations (or waiters on the same pool) can proceed.
        drop(_entry_permit);

        match http_outcome {
            Ok(response) => {
                let latency_ms = call_started.elapsed().as_millis() as i64;
                let walker_ms = walker_started.elapsed().as_millis() as i64;

                // Audit complete row — stamp winning entry's provider_id.
                if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                    let conn = audit_ctx.conn.lock().await;
                    let _ = super::db::complete_llm_audit(
                        &conn,
                        id,
                        &response.content,
                        true,
                        response.usage.prompt_tokens,
                        response.usage.completion_tokens,
                        latency_ms,
                        response.generation_id.as_deref(),
                        Some(entry.provider_id.as_str()),
                    );
                }

                // walker_resolved chronicle.
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_WALKER_RESOLVED,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({
                        "latency_ms": latency_ms,
                        "total_walker_ms": walker_ms,
                        "attempts": entry_idx + 1,
                    }),
                );

                return Ok(response);
            }
            Err(EntryError::Retryable { reason }) => {
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_NETWORK_ROUTE_RETRYABLE_FAIL,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({ "reason": reason }),
                );
                skip_reasons.push(format!("{}:retryable({})", entry.provider_id, reason));
                continue;
            }
            Err(EntryError::RouteSkipped { reason }) => {
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({ "reason": reason }),
                );
                skip_reasons.push(format!("{}:route_skipped({})", entry.provider_id, reason));
                continue;
            }
            Err(EntryError::CallTerminal { reason }) => {
                emit_walker_chronicle(
                    ctx,
                    config,
                    super::compute_chronicle::EVENT_NETWORK_ROUTE_TERMINAL_FAIL,
                    &walker_source_label,
                    &entry.provider_id,
                    serde_json::json!({ "reason": reason.clone() }),
                );
                if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
                    let conn = audit_ctx.conn.lock().await;
                    let _ = super::db::fail_llm_audit(
                        &conn,
                        id,
                        &reason,
                        last_attempted_provider_id.as_deref(),
                    );
                }
                emit_step_error(ctx, &reason);
                return Err(anyhow!(reason));
            }
        }
    }

    // Walker exhausted — no entry produced a viable dispatch.
    emit_walker_chronicle(
        ctx,
        config,
        super::compute_chronicle::EVENT_WALKER_EXHAUSTED,
        &walker_source_label,
        // entry_provider_id slot carries a summary marker for this event.
        "(exhausted)",
        serde_json::json!({
            "entries_tried": walker_entries_total,
            "skip_reasons": skip_reasons,
        }),
    );
    let err_msg = format!(
        "no viable route — all {} entries exhausted",
        walker_entries_total
    );
    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
        let conn = audit_ctx.conn.lock().await;
        let _ = super::db::fail_llm_audit(&conn, id, "no viable route", None);
    }
    emit_step_error(ctx, &err_msg);
    Err(anyhow!(err_msg))
}

// ── Phase 6: Cache support types and helpers ────────────────────────────────

/// Components computed once per cached LLM call so the lookup + store
/// paths share the same values.
struct CacheLookupResult {
    resolved_model: String,
    inputs_hash: String,
    cache_key: String,
}

/// Serialize an `LlmResponse` into the JSON string stored in
/// `pyramid_step_cache.output_json`. Kept as a helper so the cache
/// format is consistent between writes and reads, and so a future
/// schema bump has exactly one place to touch.
fn serialize_response_for_cache(response: &LlmResponse) -> String {
    serde_json::json!({
        "content": response.content,
        "usage": {
            "prompt_tokens": response.usage.prompt_tokens,
            "completion_tokens": response.usage.completion_tokens,
        },
        "generation_id": response.generation_id,
        "actual_cost_usd": response.actual_cost_usd,
        "provider_id": response.provider_id,
    })
    .to_string()
}

/// Parse a cached row's `output_json` back into an `LlmResponse`.
/// Returns an error if any required field is missing — the caller
/// treats this as a corruption signal and deletes the row.
fn parse_cached_response(cached: &super::step_context::CachedStepOutput) -> Result<LlmResponse> {
    let value: serde_json::Value = serde_json::from_str(&cached.output_json)
        .map_err(|e| anyhow!("cached output_json parse failed: {}", e))?;
    let content = value
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("cached entry missing `content` string"))?
        .to_string();
    let prompt_tokens = value
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion_tokens = value
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let generation_id = value
        .get("generation_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let actual_cost_usd = value.get("actual_cost_usd").and_then(|v| v.as_f64());
    let provider_id = value
        .get("provider_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(LlmResponse {
        content,
        usage: TokenUsage {
            prompt_tokens,
            completion_tokens,
        },
        generation_id,
        actual_cost_usd,
        provider_id,
        fleet_peer_id: value.get("fleet_peer_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
        fleet_peer_model: value.get("fleet_peer_model").and_then(|v| v.as_str()).map(|s| s.to_string()),
    })
}

/// Emit a cache-related event on the bus attached to a StepContext, if
/// any. No-op when the context has no bus.
fn emit_cache_event(ctx: &StepContext, kind: TaggedKind) {
    if let Some(bus) = ctx.bus.as_ref() {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: ctx.slug.clone(),
            kind,
        });
    }
}

/// Phase 13: emit an arbitrary TaggedKind on the ctx's bus if present.
/// Mirrors `emit_cache_event` but without restricting to the
/// cache-related variants. Used by the LLM call path for
/// `LlmCallStarted` / `LlmCallCompleted` / `StepRetry` / `StepError`.
/// Private to llm.rs — call sites in other modules have their own
/// emission helpers that thread the bus differently.
fn emit_build_event(ctx: &StepContext, kind: TaggedKind) {
    if let Some(bus) = ctx.bus.as_ref() {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: ctx.slug.clone(),
            kind,
        });
    }
}

/// Phase 13: helper for the retry loop to emit `StepRetry` on each
/// attempt. Called from inside the retry path only when an HTTP error,
/// 5xx response, parse failure, or empty-content retry triggers a
/// backoff. `attempt` is 0-indexed internally but we emit 1-indexed
/// for the UI (attempt 1 = "first retry after initial failure").
fn emit_step_retry(
    ctx: Option<&StepContext>,
    attempt: i64,
    max_attempts: i64,
    error: &str,
    backoff_ms: i64,
) {
    let Some(sc) = ctx else {
        return;
    };
    emit_build_event(
        sc,
        TaggedKind::StepRetry {
            slug: sc.slug.clone(),
            build_id: sc.build_id.clone(),
            step_name: sc.step_name.clone(),
            attempt: attempt + 1,
            max_attempts,
            error: error.to_string(),
            backoff_ms,
        },
    );
}

/// Phase 13: helper to emit `StepError` after retries are exhausted or
/// when a fatal error occurs outside the retry loop.
fn emit_step_error(ctx: Option<&StepContext>, error: &str) {
    let Some(sc) = ctx else {
        return;
    };
    emit_build_event(
        sc,
        TaggedKind::StepError {
            slug: sc.slug.clone(),
            build_id: sc.build_id.clone(),
            step_name: sc.step_name.clone(),
            error: error.to_string(),
            depth: sc.depth,
            chunk_index: sc.chunk_index,
        },
    );
}

/// Phase 13: emit `LlmCallStarted` for every HTTP dispatch (including
/// retries — each attempt is a distinct network call). Gated on the
/// presence of a StepContext + a resolved model id; without those we
/// have no primary key for the timeline row.
fn emit_llm_call_started(ctx: Option<&StepContext>, model_id: &str, cache_key: &str) {
    let Some(sc) = ctx else {
        return;
    };
    emit_build_event(
        sc,
        TaggedKind::LlmCallStarted {
            slug: sc.slug.clone(),
            build_id: sc.build_id.clone(),
            step_name: sc.step_name.clone(),
            primitive: sc.primitive.clone(),
            model_tier: sc.model_tier.clone(),
            model_id: model_id.to_string(),
            cache_key: cache_key.to_string(),
            depth: sc.depth,
            chunk_index: sc.chunk_index,
        },
    );
}

/// Phase 13: emit `LlmCallCompleted` after a successful response parse.
fn emit_llm_call_completed(
    ctx: Option<&StepContext>,
    model_id: &str,
    cache_key: &str,
    usage: &TokenUsage,
    cost_usd: f64,
    latency_ms: i64,
) {
    let Some(sc) = ctx else {
        return;
    };
    emit_build_event(
        sc,
        TaggedKind::LlmCallCompleted {
            slug: sc.slug.clone(),
            build_id: sc.build_id.clone(),
            step_name: sc.step_name.clone(),
            cache_key: cache_key.to_string(),
            tokens_prompt: usage.prompt_tokens,
            tokens_completion: usage.completion_tokens,
            cost_usd,
            latency_ms,
            model_id: model_id.to_string(),
        },
    );
}

/// Result of a cache probe performed by `try_cache_lookup_or_key`.
///
/// `Hit` carries a fully-formed `LlmResponse` — the caller must return
/// it without going to HTTP. `MissOrBypass` carries an optional
/// `CacheLookupResult` that the cache-store path can use after a
/// successful HTTP call (`None` means no StepContext was provided, or
/// the ctx was not cache-usable).
enum CacheProbeOutcome {
    Hit(LlmResponse),
    MissOrBypass(Option<CacheLookupResult>),
}

/// Shared cache probe path (Phase 6 fix pass). Keeps the cache hook
/// point exactly once regardless of which HTTP retry loop is
/// upstream of it.
///
/// Behavior:
/// * `ctx` is `None` or not cache-usable → returns
///   `MissOrBypass(None)` without touching the DB. The caller proceeds
///   to HTTP with no cache write.
/// * `ctx.force_fresh` is true → skips the read but returns
///   `MissOrBypass(Some(lookup))` so the store path can still supersede
///   any prior row.
/// * Cache hit with a `Valid` verification → returns `Hit(response)`;
///   caller returns directly to its own caller without going to HTTP.
/// * Cache hit with a non-Valid verification → deletes the stale row,
///   emits `CacheHitVerificationFailed`, returns
///   `MissOrBypass(Some(lookup))` so the store path refreshes it.
/// * Cache miss → emits `CacheMiss`, returns
///   `MissOrBypass(Some(lookup))`.
/// * DB probe error → logs, returns `MissOrBypass(Some(lookup))`.
fn try_cache_lookup_or_key(
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
) -> CacheProbeOutcome {
    let sc = match ctx {
        Some(sc) if sc.cache_is_usable() => sc,
        _ => return CacheProbeOutcome::MissOrBypass(None),
    };

    let resolved_model = sc
        .resolved_model_id
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let inputs_hash = compute_inputs_hash(system_prompt, user_prompt);
    let cache_key = compute_cache_key(&inputs_hash, &sc.prompt_hash, &resolved_model);

    let lookup = CacheLookupResult {
        resolved_model,
        inputs_hash,
        cache_key,
    };

    if sc.force_fresh {
        info!(
            "[LLM-CACHE] FORCE-FRESH slug={} step={} depth={} key={}",
            sc.slug, sc.step_name, sc.depth, &lookup.cache_key[..16]
        );
        return CacheProbeOutcome::MissOrBypass(Some(lookup));
    }

    // Open an ephemeral connection for the cache read. We deliberately
    // go outside the writer mutex — the cache is content-addressable
    // and SELECT is always safe.
    //
    // Phase 12 verifier fix: `tokio::task::block_in_place` panics on a
    // current_thread runtime. `#[tokio::test]` uses current_thread by
    // default, and several legacy integration tests (dadbear_extend,
    // etc.) do not mark themselves multi_thread. Previously this path
    // was only hit when the caller supplied a cache-aware ctx, which
    // in practice meant only the Phase 6 chain_executor dispatch
    // paths — and those tests did NOT hit `block_in_place` because
    // they short-circuited earlier. Phase 12 broadens the set of
    // dispatch sites that populate cache_access so this path is now
    // reachable from dadbear_extend's integration tests.
    //
    // If we're on a current_thread runtime, run the probe synchronously
    // (the DB open + SELECT are both fast and blocking is already what
    // we're doing — `block_in_place` just tells the scheduler it's OK
    // to block its worker). Falling through to the sync path is
    // equivalent for correctness and works on either runtime flavor.
    let probe_body = || -> Result<Option<super::step_context::CachedStepOutput>> {
        let conn = super::db::open_pyramid_connection(std::path::Path::new(&sc.db_path))?;
        super::db::check_cache(&conn, &sc.slug, &lookup.cache_key)
    };
    let probe = match tokio::runtime::Handle::try_current() {
        Ok(h) => match h.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(probe_body)
            }
            // CurrentThread (incl. the default `#[tokio::test]`): run
            // the blocking probe inline. The DB open + SELECT are
            // sub-millisecond; running them on the scheduler thread is
            // fine for tests and for the narrow app-startup window.
            _ => probe_body(),
        },
        Err(_) => probe_body(),
    };

    match probe {
        Ok(Some(cached)) => {
            let verdict = verify_cache_hit(
                &cached,
                &lookup.inputs_hash,
                &sc.prompt_hash,
                &lookup.resolved_model,
            );
            match verdict {
                CacheHitResult::Valid => match parse_cached_response(&cached) {
                    Ok(response) => {
                        emit_cache_event(
                            sc,
                            TaggedKind::CacheHit {
                                slug: sc.slug.clone(),
                                step_name: sc.step_name.clone(),
                                cache_key: lookup.cache_key.clone(),
                                chunk_index: sc.chunk_index,
                                depth: sc.depth,
                            },
                        );
                        info!(
                            "[LLM-CACHE] HIT slug={} step={} depth={} key={}",
                            sc.slug,
                            sc.step_name,
                            sc.depth,
                            &lookup.cache_key[..16]
                        );
                        CacheProbeOutcome::Hit(response)
                    }
                    Err(e) => {
                        // Corruption detected at parse time — treat as
                        // verification failure and fall through.
                        warn!(
                            "[LLM-CACHE] cached output_json parsed as JSON but structure was \
                             unusable: {}",
                            e
                        );
                        // Phase 12 verifier fix: runtime-flavor-aware delete.
                        let delete_body = || -> Result<()> {
                            let conn = super::db::open_pyramid_connection(std::path::Path::new(
                                &sc.db_path,
                            ))?;
                            super::db::delete_cache_entry(&conn, &sc.slug, &lookup.cache_key)
                        };
                        let _ = match tokio::runtime::Handle::try_current() {
                            Ok(h) => match h.runtime_flavor() {
                                tokio::runtime::RuntimeFlavor::MultiThread => {
                                    tokio::task::block_in_place(delete_body)
                                }
                                _ => delete_body(),
                            },
                            Err(_) => delete_body(),
                        };
                        emit_cache_event(
                            sc,
                            TaggedKind::CacheHitVerificationFailed {
                                slug: sc.slug.clone(),
                                step_name: sc.step_name.clone(),
                                cache_key: lookup.cache_key.clone(),
                                reason: "unusable_structure".to_string(),
                            },
                        );
                        CacheProbeOutcome::MissOrBypass(Some(lookup))
                    }
                },
                other => {
                    let reason = other.reason_tag().to_string();
                    warn!(
                        "[LLM-CACHE] verification failed ({}) — deleting stale row for slug={} \
                         cache_key={}",
                        reason, sc.slug, lookup.cache_key
                    );
                    // Phase 12 verifier fix: runtime-flavor-aware delete.
                    let delete_body = || -> Result<()> {
                        let conn = super::db::open_pyramid_connection(std::path::Path::new(
                            &sc.db_path,
                        ))?;
                        super::db::delete_cache_entry(&conn, &sc.slug, &lookup.cache_key)
                    };
                    let _ = match tokio::runtime::Handle::try_current() {
                        Ok(h) => match h.runtime_flavor() {
                            tokio::runtime::RuntimeFlavor::MultiThread => {
                                tokio::task::block_in_place(delete_body)
                            }
                            _ => delete_body(),
                        },
                        Err(_) => delete_body(),
                    };
                    emit_cache_event(
                        sc,
                        TaggedKind::CacheHitVerificationFailed {
                            slug: sc.slug.clone(),
                            step_name: sc.step_name.clone(),
                            cache_key: lookup.cache_key.clone(),
                            reason,
                        },
                    );
                    CacheProbeOutcome::MissOrBypass(Some(lookup))
                }
            }
        }
        Ok(None) => {
            emit_cache_event(
                sc,
                TaggedKind::CacheMiss {
                    slug: sc.slug.clone(),
                    step_name: sc.step_name.clone(),
                    cache_key: lookup.cache_key.clone(),
                    chunk_index: sc.chunk_index,
                    depth: sc.depth,
                },
            );
            CacheProbeOutcome::MissOrBypass(Some(lookup))
        }
        Err(e) => {
            warn!(
                "[LLM-CACHE] probe failed for slug={} cache_key={}: {} — falling through to HTTP",
                sc.slug, lookup.cache_key, e
            );
            CacheProbeOutcome::MissOrBypass(Some(lookup))
        }
    }
}

/// Shared cache store path.
/// No-op when either ctx or lookup is absent (which means the caller
/// did not opt into the cache on this request).
///
/// Force-fresh writes route through `supersede_cache_entry` so the
/// prior row is retained as a supersession chain link. Non-force-fresh
/// writes go through `store_cache` (INSERT OR REPLACE on the
/// content-addressable unique key).
fn try_cache_store(
    ctx: Option<&StepContext>,
    lookup: Option<&CacheLookupResult>,
    response: &LlmResponse,
    call_started: std::time::Instant,
) {
    let (sc, lookup) = match (ctx, lookup) {
        (Some(sc), Some(lookup)) => (sc, lookup),
        _ => return,
    };

    let latency_ms = call_started.elapsed().as_millis() as i64;
    let chunk_index = sc.chunk_index.unwrap_or(-1);
    let token_usage_json = serde_json::to_string(&serde_json::json!({
        "prompt_tokens": response.usage.prompt_tokens,
        "completion_tokens": response.usage.completion_tokens,
    }))
    .ok();
    let output_json = serialize_response_for_cache(response);
    let entry = CacheEntry {
        slug: sc.slug.clone(),
        build_id: sc.build_id.clone(),
        step_name: sc.step_name.clone(),
        chunk_index,
        depth: sc.depth,
        cache_key: lookup.cache_key.clone(),
        inputs_hash: lookup.inputs_hash.clone(),
        prompt_hash: sc.prompt_hash.clone(),
        model_id: lookup.resolved_model.clone(),
        output_json,
        token_usage_json,
        cost_usd: None,
        latency_ms: Some(latency_ms),
        force_fresh: sc.force_fresh,
        supersedes_cache_id: None,
        // Phase 13: the normal cache-store path doesn't attach a note.
        // Only the reroll IPC attaches a note, and it calls
        // `supersede_cache_entry` directly rather than going through
        // the LLM retry loop's store path.
        note: None,
    };
    let db_path = sc.db_path.clone();
    let slug_for_write = sc.slug.clone();
    let cache_key_for_write = lookup.cache_key.clone();
    let force_fresh = sc.force_fresh;
    // Phase 12 verifier fix: runtime-flavor-aware wrapper so tests on
    // current_thread runtime don't panic. See the matching comment in
    // `try_cache_lookup_or_key`.
    let store_body = move || -> Result<()> {
        let conn = super::db::open_pyramid_connection(std::path::Path::new(&db_path))?;
        if force_fresh {
            super::db::supersede_cache_entry(
                &conn,
                &slug_for_write,
                &cache_key_for_write,
                &entry,
            )?;
        } else {
            super::db::store_cache(&conn, &entry)?;
        }
        Ok(())
    };
    let store_result = match tokio::runtime::Handle::try_current() {
        Ok(h) => match h.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(store_body)
            }
            _ => store_body(),
        },
        Err(_) => store_body(),
    };
    if let Err(e) = store_result {
        warn!(
            "[LLM-CACHE] store failed for slug={} cache_key={}: {}",
            sc.slug, lookup.cache_key, e
        );
    }
}

// ── Backward-compatible wrappers ─────────────────────────────────────────────

/// Call OpenRouter with automatic model cascade and retry logic.
/// Falls back to larger-context models when input exceeds primary model's limit.
/// Retries on 429/403/502/503, null content, and JSON parse failures.
///
/// Returns only the content string. For usage/generation_id, use `call_model_unified`.
pub async fn call_model(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<String> {
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
    )
    .await?;
    Ok(resp.content)
}

/// Phase 12 retrofit wrapper: `call_model` with a StepContext threaded
/// through the cache-aware path. When `ctx` is Some and cache-usable,
/// the call becomes cache-reachable (lookup before HTTP, store after).
/// When `ctx` is None, behavior is identical to `call_model`.
pub async fn call_model_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<String> {
    let resp = call_model_unified_with_options_and_ctx(
        config,
        ctx,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
        LlmCallOptions::default(),
    )
    .await?;
    Ok(resp.content)
}

/// Call OpenRouter with automatic model cascade and retry logic.
/// Same as `call_model()` but also returns token usage from the API response.
///
/// For generation_id as well, use `call_model_unified`.
pub async fn call_model_with_usage(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<(String, TokenUsage)> {
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
    )
    .await?;
    Ok((resp.content, resp.usage))
}

/// Phase 12 retrofit wrapper: `call_model_with_usage` with a StepContext
/// threaded through the cache-aware path. On a cache hit the stored
/// usage (when available in the row's `token_usage_json`) is returned
/// to the caller; otherwise behaves exactly like `call_model_with_usage`.
pub async fn call_model_with_usage_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<(String, TokenUsage)> {
    let resp = call_model_unified_with_options_and_ctx(
        config,
        ctx,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
        LlmCallOptions::default(),
    )
    .await?;
    Ok((resp.content, resp.usage))
}

/// Phase 12 retrofit wrapper: `call_model_unified` with a StepContext
/// threaded through the cache-aware path. Equivalent to
/// `call_model_unified_with_options_and_ctx` with default options.
pub async fn call_model_unified_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
) -> Result<LlmResponse> {
    call_model_unified_with_options_and_ctx(
        config,
        ctx,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        LlmCallOptions::default(),
    )
    .await
}


/// Call OpenRouter with structured output enforcement via JSON schema.
///
/// Returns only the content string. For usage/generation_id, use `call_model_unified`
/// with a manually constructed `response_format`.
pub async fn call_model_structured(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_schema: &serde_json::Value,
    schema_name: &str,
) -> Result<String> {
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": schema_name,
            "strict": true,
            "schema": response_schema
        }
    });
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        Some(&response_format),
    )
    .await?;
    Ok(resp.content)
}

/// Phase 12 retrofit wrapper: `call_model_structured` with a
/// StepContext threaded through the cache-aware path.
#[allow(clippy::too_many_arguments)]
pub async fn call_model_structured_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_schema: &serde_json::Value,
    schema_name: &str,
) -> Result<String> {
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": schema_name,
            "strict": true,
            "schema": response_schema
        }
    });
    let resp = call_model_unified_with_options_and_ctx(
        config,
        ctx,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        Some(&response_format),
        LlmCallOptions::default(),
    )
    .await?;
    Ok(resp.content)
}

// ── Audited LLM Call (Live Pyramid Theatre) ─────────────────────────────────

use rusqlite::Connection;
use tokio::sync::Mutex as TokioMutexSync;

/// Context for recording LLM calls to the audit trail. Thread through build
/// pipelines to capture prompt/response for the Inspector modal.
#[derive(Debug, Clone)]
pub struct AuditContext {
    pub conn: Arc<TokioMutexSync<Connection>>,
    pub slug: String,
    pub build_id: String,
    pub node_id: Option<String>,
    pub step_name: String,
    pub call_purpose: String,
    pub depth: Option<i64>,
}

impl AuditContext {
    /// Create a child context for a different node/purpose while sharing the connection.
    pub fn for_node(&self, node_id: &str, call_purpose: &str, depth: i64) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            slug: self.slug.clone(),
            build_id: self.build_id.clone(),
            node_id: Some(node_id.to_string()),
            step_name: self.step_name.clone(),
            call_purpose: call_purpose.to_string(),
            depth: Some(depth),
        }
    }

    pub fn with_step(&self, step_name: &str) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            slug: self.slug.clone(),
            build_id: self.build_id.clone(),
            node_id: self.node_id.clone(),
            step_name: step_name.to_string(),
            call_purpose: self.call_purpose.clone(),
            depth: self.depth,
        }
    }
}

/// Phase 18b: legacy entry point retained as a thin deprecated wrapper.
///
/// Historically this function inserted its own pending audit row and
/// then called `call_model_unified`, bypassing the Phase 6 cache. That
/// meant audited LLM calls (the only kind Wire Node makes during
/// production builds) re-burned tokens on every re-run.
///
/// Phase 18b retired the duplicate audit-write path. The
/// `call_model_unified_with_audit_and_ctx` entry point now threads BOTH
/// the audit context AND a Phase 6 StepContext through a single
/// implementation that:
///
///   1. Probes the cache and serves cache hits with a `cache_hit = true`
///      audit row, OR
///   2. Falls through to the existing pending-row → HTTP call →
///      complete-row dance for wire calls.
///
/// This wrapper preserves the legacy `(LlmResponse, audit_id)` return
/// shape so existing callers compile, but the returned id is `0` —
/// production callers always pattern-match `(resp, _)` and ignore it.
/// New retrofit sites should call `call_model_unified_with_audit_and_ctx`
/// directly so they can thread a `StepContext` for cache reachability.
///
/// LEAVING THIS WRAPPER IN PLACE WITHOUT THREADING A StepContext IS A
/// CACHE GAP. Every production call site MUST migrate to the unified
/// entry point.
#[deprecated(
    note = "Phase 18b: prefer `call_model_unified_with_audit_and_ctx` so the cache is reachable. \
            This wrapper passes ctx=None and re-burns tokens on every call."
)]
pub async fn call_model_audited(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    audit: &AuditContext,
) -> Result<(LlmResponse, i64)> {
    let resp = call_model_unified_with_audit_and_ctx(
        config,
        None,
        Some(audit),
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        LlmCallOptions::default(),
    )
    .await?;
    // Phase 18b: the audit row id is no longer surfaced — the cache-hit
    // path inserts a single complete row in one statement and the
    // wire-call path goes through pending → complete inside
    // `call_model_unified_with_audit_and_ctx`. Production callers ignore
    // the returned id; tests that need it should query
    // `pyramid_llm_audit` by `(slug, build_id)`.
    Ok((resp, 0))
}

// ── JSON extraction ──────────────────────────────────────────────────────────

/// Extract JSON from a response that may include markdown fences or thinking tags.
pub fn extract_json(text: &str) -> Result<Value> {
    let mut text = text.trim().to_string();

    // Strip <think>...</think> tags
    static THINK_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());
    text = THINK_RE.replace_all(&text, "").trim().to_string();

    // Remove markdown fences (``` lines)
    if text.contains("```") {
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().starts_with("```"))
            .collect();
        text = lines.join("\n").trim().to_string();
    }

    // Find JSON delimiters — try both object {…} and array […]
    let obj_start = text.find('{');
    let obj_end = text.rfind('}');
    let arr_start = text.find('[');
    let arr_end = text.rfind(']');

    // Pick the outermost valid JSON range (object or array, whichever starts first)
    let (start, end) = match ((obj_start, obj_end), (arr_start, arr_end)) {
        ((Some(os), Some(oe)), (Some(as_), Some(ae))) if oe >= os && ae >= as_ => {
            if os <= as_ {
                (os, oe)
            } else {
                (as_, ae)
            }
        }
        ((Some(os), Some(oe)), _) if oe >= os => (os, oe),
        (_, (Some(as_), Some(ae))) if ae >= as_ => (as_, ae),
        _ => {
            return Err(anyhow!(
                "No JSON found in: {}",
                &text[..text.len().min(200)]
            ))
        }
    };

    let slice = &text[start..=end];

    // Try parsing as-is
    if let Ok(v) = serde_json::from_str::<Value>(slice) {
        return Ok(v);
    }

    // Fix trailing commas and retry
    static COMMA_BRACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*}").unwrap());
    static COMMA_BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*]").unwrap());
    let fixed = COMMA_BRACE.replace_all(slice, "}");
    let fixed = COMMA_BRACKET.replace_all(&fixed, "]");

    if let Ok(v) = serde_json::from_str::<Value>(&fixed) {
        return Ok(v);
    }

    Err(anyhow!(
        "No JSON found in: {}",
        &text[..text.len().min(200)]
    ))
}

// ── Direct (non-cascading) entry point ─────────────────────────────────────

/// Call a specific OpenRouter model directly, bypassing the default 3-tier cascade.
///
/// Used for ASCII-art generation (WS-L) where the cascade would always pick
/// Mercury-2, which empirically fails at this task. The caller pins a specific
/// model_id (e.g. `x-ai/grok-4.20-beta`) and receives the raw content string.
///
/// Unlike `call_model_unified`, this function:
///   * Never cascades on HTTP 400 / context-exceeded.
///   * Takes no `temperature` / `response_format` (art generation is freeform).
///   * Uses a fixed conservative timeout (`base_timeout_secs`).
///
/// Retries on transient errors (`retryable_status_codes`, network, null content)
/// up to `config.max_retries`, same as the unified path.
pub async fn call_model_direct(
    config: &LlmConfig,
    model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
) -> Result<String> {
    let (provider_impl, secret, provider_type, provider_id) = build_call_provider(config)?;
    let client = &*HTTP_CLIENT;
    let url = provider_impl.chat_completions_url();
    let built_headers = provider_impl.prepare_headers(secret.as_ref())?;
    let local_timeout_scale = if provider_type == ProviderType::OpenaiCompat { 5 } else { 1 };
    let timeout = std::time::Duration::from_secs(config.base_timeout_secs * local_timeout_scale);

    for attempt in 0..config.max_retries {
        let mut body = serde_json::json!({
            "model": model_id,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "max_tokens": max_tokens
        });
        provider_impl.augment_request_body(&mut body, &RequestMetadata::default());

        // Rate limiting: per-pool when available, global fallback otherwise.
        if config.provider_pools.is_none() {
            rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;
        }

        // Per-provider concurrency pool (Phase A dispatch).
        let _pool_permit: Option<tokio::sync::OwnedSemaphorePermit> = if let Some(pools) = &config.provider_pools {
            pools.acquire(&provider_id).await.ok()
        } else {
            None
        };
        // Global semaphore fallback (for tests/pre-init without pools)
        let _local_permit = if _pool_permit.is_none() && provider_type == ProviderType::OpenaiCompat {
            Some(LOCAL_PROVIDER_SEMAPHORE.acquire().await.map_err(|e| anyhow!("local provider semaphore closed: {e}"))?)
        } else {
            None
        };

        let mut request = client.post(&url).timeout(timeout);
        for (k, v) in &built_headers {
            request = request.header(k, v);
        }
        let resp = request.json(&body).send().await;
        drop(_pool_permit);
        drop(_local_permit);

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    info!("  [direct:{}] request error ({}), retry {}...", short_name(model_id), e, attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("call_model_direct({}) request failed: {}", model_id, e));
            }
        };

        let status = resp.status().as_u16();
        if config.retryable_status_codes.contains(&status) {
            let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
            info!("  [direct:{}] HTTP {}, waiting {}s...", short_name(model_id), status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt + 1 < config.max_retries {
                info!("  [direct:{}] HTTP {}, retry {}...", short_name(model_id), status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                continue;
            }
            return Err(anyhow!("HTTP {} after {} attempts: {}", status, config.max_retries, body_text));
        }

        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("Failed to read response: {}", e));
            }
        };

        let parsed = match provider_impl.parse_response(&body_text) {
            Ok(p) => p,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    warn!(
                        "[direct:{}] parse error, retry {}: {}",
                        short_name(model_id),
                        attempt + 1,
                        e
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs))
                        .await;
                    continue;
                }
                return Err(anyhow!(
                    "parse failed after {} attempts: {}",
                    config.max_retries,
                    e
                ));
            }
        };

        if parsed.content.is_empty() {
            if attempt + 1 < config.max_retries {
                info!(
                    "  [direct:{}] empty content, retry {}...",
                    short_name(model_id),
                    attempt + 1
                );
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs))
                    .await;
                continue;
            }
            return Err(anyhow!(
                "empty content after {} attempts",
                config.max_retries
            ));
        }
        return Ok(parsed.content);
    }

    Err(anyhow!("call_model_direct({}): max retries exceeded", model_id))
}

// ── Phase 11: Provider health hook ──────────────────────────────────────────
//
// Fire-and-forget helper that records a provider error into the
// health state machine when the LLM call path has a StepContext in
// scope. We open a fresh side connection from `ctx.db_path` so we
// don't contend for the writer mutex inside the hot call loop; the
// write is small, idempotent, and already guarded by a count-based
// threshold in `record_provider_error`.
fn maybe_record_provider_error(
    ctx: Option<&StepContext>,
    provider_id: &str,
    kind: super::provider_health::ProviderErrorKind,
) {
    let Some(ctx) = ctx else {
        return;
    };
    if ctx.db_path.is_empty() {
        return;
    }
    let db_path = ctx.db_path.clone();
    let provider_id = provider_id.to_string();
    // Spawn into the rayon-friendly blocking pool; failures are
    // logged and swallowed. This must never return an error to the
    // LLM call loop — the health hook is a best-effort signal.
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(conn) = rusqlite::Connection::open(&db_path) else {
            return;
        };
        let policy = super::provider_health::CostReconciliationPolicy::default();
        if let Err(e) = super::provider_health::record_provider_error(
            &conn,
            &provider_id,
            kind,
            &policy,
            None,
        ) {
            tracing::debug!(
                provider_id = provider_id.as_str(),
                error = %e,
                "maybe_record_provider_error: health update failed (non-critical)"
            );
        }
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_overlays_preserve_fleet_and_other_runtime_wiring() {
        let unique_suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let credentials_path = std::env::temp_dir()
            .join(format!("wire-node-credentials-{}.yaml", unique_suffix));
        let credential_store = std::sync::Arc::new(
            crate::pyramid::credentials::CredentialStore::load_from_path(credentials_path)
                .unwrap(),
        );
        let provider_registry = std::sync::Arc::new(
            crate::pyramid::provider::ProviderRegistry::new(credential_store.clone()),
        );

        let policy_yaml: crate::pyramid::dispatch_policy::DispatchPolicyYaml =
            serde_yaml::from_str(
                r#"
version: 1
provider_pools:
  fleet:
    concurrency: 1
routing_rules:
  - name: ollama-catchall
    match_config: {}
    route_to:
      - provider_id: fleet
      - provider_id: ollama
        is_local: true
"#,
            )
            .unwrap();
        let dispatch_policy = std::sync::Arc::new(
            crate::pyramid::dispatch_policy::DispatchPolicy::from_yaml(&policy_yaml),
        );
        let provider_pools = std::sync::Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(dispatch_policy.as_ref()),
        );
        let compute_queue = crate::compute_queue::ComputeQueueHandle::new();
        let fleet_roster =
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::fleet::FleetRoster::default()));
        let tunnel_state_for_dispatch =
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::tunnel::TunnelState::default()));
        let fleet_dispatch = std::sync::Arc::new(crate::fleet::FleetDispatchContext {
            tunnel_state: tunnel_state_for_dispatch.clone(),
            fleet_roster: fleet_roster.clone(),
            pending: std::sync::Arc::new(crate::fleet::PendingFleetJobs::new()),
            policy: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy::default(),
            )),
        });

        let live = LlmConfig {
            api_key: "live-api-key".into(),
            auth_token: "live-auth-token".into(),
            provider_registry: Some(provider_registry.clone()),
            credential_store: Some(credential_store.clone()),
            cache_access: Some(CacheAccess {
                slug: "live-slug".into(),
                build_id: "live-build".into(),
                db_path: std::sync::Arc::<str>::from("/tmp/live.db"),
                bus: None,
                chain_name: None,
                content_type: None,
            }),
            dispatch_policy: Some(dispatch_policy.clone()),
            provider_pools: Some(provider_pools.clone()),
            compute_queue: Some(compute_queue.clone()),
            fleet_roster: Some(fleet_roster.clone()),
            fleet_dispatch: Some(fleet_dispatch.clone()),
            ..Default::default()
        };

        let rebuilt = LlmConfig::default().with_runtime_overlays_from(&live);

        assert_eq!(rebuilt.api_key, "live-api-key");
        assert_eq!(rebuilt.auth_token, "live-auth-token");
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.provider_registry.as_ref().unwrap(),
            &provider_registry,
        ));
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.credential_store.as_ref().unwrap(),
            &credential_store,
        ));
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.dispatch_policy.as_ref().unwrap(),
            &dispatch_policy,
        ));
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.provider_pools.as_ref().unwrap(),
            &provider_pools,
        ));
        assert!(std::sync::Arc::ptr_eq(
            &rebuilt.compute_queue.as_ref().unwrap().queue,
            &compute_queue.queue,
        ));
        assert!(std::sync::Arc::ptr_eq(
            &rebuilt.compute_queue.as_ref().unwrap().notify,
            &compute_queue.notify,
        ));
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.fleet_roster.as_ref().unwrap(),
            &fleet_roster,
        ));
        assert!(std::sync::Arc::ptr_eq(
            rebuilt.fleet_dispatch.as_ref().unwrap(),
            &fleet_dispatch,
        ));
        assert!(rebuilt.cache_access.is_none());
    }

    // ── Walker Re-Plan Wire 2.1 Wave 1 tests (§8 tasks 8-10) ────────────
    //
    // Three tests exercise the walker's core advancement paths without
    // standing up an actual HTTP server. Every test drives the walker
    // via `call_model_unified_with_audit_and_ctx` with a ResolvedRoute
    // and asserts the observable outcome: an `Err` with a stable
    // substring. Chronicle emission is fire-and-forget into SQLite;
    // tests do not assert on the row (no DB path set).
    //
    // Fixture notes:
    //   - LlmConfig carries a DispatchPolicy sufficient to make the
    //     walker's entry iteration fire. No `provider_registry` is
    //     attached → walker's registry-lookup branch returns None and
    //     the fallback branch fires build_call_provider(), which the
    //     test drives via an empty api_key so no real HTTP happens.
    //   - Pool semaphore at concurrency 0 = permanently-saturated.

    fn walker_test_policy(
        pool_concurrency: usize,
        route_entries: Vec<&str>,
    ) -> std::sync::Arc<crate::pyramid::dispatch_policy::DispatchPolicy> {
        use crate::pyramid::dispatch_policy::*;
        let mut pool_configs = std::collections::BTreeMap::new();
        pool_configs.insert(
            "openrouter-test".into(),
            ProviderPoolConfig {
                concurrency: pool_concurrency,
                rate_limit: None,
            },
        );
        let route_to = route_entries
            .into_iter()
            .map(|pid| RouteEntry {
                provider_id: pid.to_string(),
                model_id: None,
                tier_name: None,
                is_local: false,
                max_budget_credits: None,
            })
            .collect();
        let policy = DispatchPolicy {
            rules: vec![RoutingRule {
                name: "walker_test".into(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to,
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };
        std::sync::Arc::new(policy)
    }

    fn walker_test_config(
        policy: std::sync::Arc<crate::pyramid::dispatch_policy::DispatchPolicy>,
    ) -> LlmConfig {
        let pools = std::sync::Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(policy.as_ref()),
        );
        LlmConfig {
            api_key: String::new(),
            auth_token: String::new(),
            primary_model: "test-primary".into(),
            fallback_model_1: "test-fallback1".into(),
            fallback_model_2: "test-fallback2".into(),
            dispatch_policy: Some(policy),
            provider_pools: Some(pools),
            max_retries: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn walker_exhausts_when_no_entry_viable() {
        // Single pool entry whose provider_id is NOT in pools → walker
        // hits AcquireError::Unavailable("provider_not_in_pool") and
        // advances; after one entry the walker exhausts.
        let policy = walker_test_policy(1, vec!["unknown-provider"]);
        let config = walker_test_config(policy);

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust — no viable route");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route' in error, got: {msg}",
        );
        assert!(
            msg.contains("1 entries"),
            "expected '1 entries' in error, got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_skips_fleet_and_market_entries_in_wave2() {
        // Route = [fleet, market, unknown-pool]. Walker sees all 3 entries:
        //   - fleet: runtime gate fails (fleet_ctx missing) → advance with
        //     fleet_ctx_missing unavailable.
        //   - market: runtime gate fails (compute_market_context absent in
        //     test fixture) → advance with no_market_context unavailable.
        //   - unknown-pool: provider_not_in_pool unavailable.
        // Walker exhausts 3 entries.
        let policy = walker_test_policy(1, vec!["fleet", "market", "unknown-pool"]);
        let config = walker_test_config(policy);

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust — no viable route");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route' in error, got: {msg}",
        );
        // Wave 2: fleet now walks (runtime-gate-fails) instead of
        // pre-filter dropping. Walker sees all 3 entries.
        assert!(
            msg.contains("3 entries"),
            "expected '3 entries' (fleet + market + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_advances_on_pool_saturation() {
        // Pool configured with concurrency=0 → permanently saturated.
        // Walker's try_acquire_owned → AcquireError::Saturated → advance.
        // Single-entry route → walker exhausts.
        let policy = walker_test_policy(0, vec!["openrouter-test"]);
        let config = walker_test_config(policy);

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust on saturated pool");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route' in error, got: {msg}",
        );
    }

    // ── Wave 2: walker fleet branch tests ────────────────────────────────────

    #[tokio::test]
    async fn walker_fleet_branch_advances_on_no_peer() {
        // Route = [fleet, unknown-pool]. Fleet context is absent (test
        // config) so the fleet branch runtime gate emits
        // `fleet_ctx_missing` and advances; unknown-pool hits
        // provider_not_in_pool unavailable; walker exhausts 2 entries.
        //
        // Compile-time assertion: this test would have failed against
        // the Phase A pre-loop because the legacy fleet_filter retain
        // would have removed the fleet entry before the walker saw it,
        // yielding "1 entries" exhausted. Wave 2 deletes the pre-loop,
        // so the walker counts all entries.
        let policy = walker_test_policy(1, vec!["fleet", "unknown-pool"]);
        let config = walker_test_config(policy);

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust when fleet has no peer");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (fleet walks + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_fleet_branch_respects_skip_fleet_dispatch() {
        // With skip_fleet_dispatch = true the walker's fleet branch
        // short-circuits with `fleet_replay_guard` and advances; the
        // unknown-pool entry then exhausts. Total 2 entries walked.
        let policy = walker_test_policy(1, vec!["fleet", "unknown-pool"]);
        let config = walker_test_config(policy);

        let mut options = LlmCallOptions::default();
        options.skip_fleet_dispatch = true;

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            options,
        )
        .await;

        let err = result.expect_err("walker should exhaust with fleet skipped");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (fleet skipped + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_fleet_branch_respects_branch_allowed() {
        // dispatch_origin = FleetReceived → branch_allowed(Fleet, _) is
        // false; the walker's generic runtime-gate skip fires BEFORE
        // the fleet branch body runs (log-only, no chronicle). The
        // unknown-pool entry still walks. Walker exhausts 2 entries.
        let policy = walker_test_policy(1, vec!["fleet", "unknown-pool"]);
        let config = walker_test_config(policy);

        let mut options = LlmCallOptions::default();
        options.dispatch_origin = DispatchOrigin::FleetReceived;

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            options,
        )
        .await;

        let err = result.expect_err("walker should exhaust under FleetReceived origin");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (fleet replay-gated + unknown-pool), got: {msg}",
        );
    }

    // ── Walker Re-Plan Wire 2.1 Wave 3b tests (market branch) ───────────
    //
    // These tests drive the walker's market branch without standing up a
    // live Wire server. The strategy matches the Wave 2 fleet tests: wire
    // up enough of the runtime gate to exercise the early-exit paths, and
    // assert the observable walker outcome.
    //
    // Full /quote → /purchase → /fill success-path coverage lives in the
    // compute_quote_flow module tests (register_pending + await_result
    // round-trips). The race-fix invariant is asserted there via
    // `register_pending_returns_receiver_before_fill_can_race`.

    #[tokio::test]
    async fn walker_market_branch_advances_when_no_market_context() {
        // Route = [market, unknown-pool]. Walker's market runtime gate
        // finds compute_market_context absent → emits route_unavailable
        // with reason="no_market_context" → advance. Unknown-pool then
        // hits provider_not_in_pool → advance. Walker exhausts.
        let policy = walker_test_policy(1, vec!["market", "unknown-pool"]);
        let config = walker_test_config(policy);

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust — no market ctx + no pool");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (market + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_market_branch_respects_branch_allowed_on_replay() {
        // Market must NOT walk under a non-Local origin. Even if
        // compute_market_context is present, branch_allowed(Market,
        // FleetReceived) returns false and the walker's generic gate
        // skips the entry before the market body runs.
        let policy = walker_test_policy(1, vec!["market", "unknown-pool"]);
        let config = walker_test_config(policy);

        let mut options = LlmCallOptions::default();
        options.dispatch_origin = DispatchOrigin::MarketReceived;

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            options,
        )
        .await;

        let err = result.expect_err("walker should exhaust under MarketReceived origin");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (market replay-gated + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_market_branch_advances_on_tunnel_disconnected() {
        // compute_market_context present but tunnel state is Disconnected
        // (default). Runtime gate fails with reason="tunnel_not_connected"
        // → advance. Unknown-pool then exhausts.
        use crate::auth::AuthState;
        use crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext;
        use crate::pyramid::pending_jobs::PendingJobs;
        use crate::WireNodeConfig;

        let policy = walker_test_policy(1, vec!["market", "unknown-pool"]);
        let mut config = walker_test_config(policy);

        // Tunnel state defaults to Disconnected with no URL.
        let auth = std::sync::Arc::new(tokio::sync::RwLock::new(AuthState::default()));
        let wire_cfg = std::sync::Arc::new(tokio::sync::RwLock::new(WireNodeConfig::default()));
        let tunnel = std::sync::Arc::new(
            tokio::sync::RwLock::new(crate::tunnel::TunnelState::default()),
        );
        config.compute_market_context = Some(ComputeMarketRequesterContext {
            auth,
            config: wire_cfg,
            pending_jobs: PendingJobs::new(),
            tunnel_state: tunnel,
        });

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let err = result.expect_err("walker should exhaust — tunnel not connected");
        let msg = format!("{err}");
        assert!(
            msg.contains("no viable route"),
            "expected 'no viable route', got: {msg}",
        );
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (market gate-failed + unknown-pool), got: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_market_dispatch_args_struct_compiles() {
        // Compile-time assertion: MarketDispatchArgs has the expected
        // shape. If a future refactor drops a field that callers rely on,
        // this test breaks at the construction site with a clear diff.
        // (Runtime execution is exercised end-to-end via the market
        // branch fixtures above + compute_quote_flow race-fix tests.)
        use crate::auth::AuthState;
        use crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext;
        use crate::pyramid::pending_jobs::PendingJobs;
        use crate::WireNodeConfig;

        let auth = std::sync::Arc::new(tokio::sync::RwLock::new(AuthState::default()));
        let wire_cfg = std::sync::Arc::new(tokio::sync::RwLock::new(WireNodeConfig::default()));
        let tunnel = std::sync::Arc::new(
            tokio::sync::RwLock::new(crate::tunnel::TunnelState::default()),
        );
        let mkt = ComputeMarketRequesterContext {
            auth,
            config: wire_cfg,
            pending_jobs: PendingJobs::new(),
            tunnel_state: tunnel,
        };
        let cfg = LlmConfig::default();
        let _args = MarketDispatchArgs {
            config: &cfg,
            ctx: None,
            market_ctx: &mkt,
            model_id: "test-model".into(),
            max_budget: (1i64 << 53) - 1,
            max_wait_ms: 60_000,
            market_saturation_patience_secs: 3600,
            max_tokens: 0,
            temperature: 0.0,
            input_tokens_est: 0,
            system_prompt: "sys",
            user_prompt: "usr",
            callback_url: "https://tunnel/v1/compute/job-result".into(),
            walker_source_label: "network",
            entry_provider_id: "market",
        };
        // Don't actually dispatch — we'd need a live Wire server. The
        // struct construction is the load-bearing assertion.
    }

    #[test]
    fn test_llm_response_from_openrouter_json() {
        // Simulates parsing the fields that call_model_unified extracts
        let data: Value = serde_json::json!({
            "id": "gen-abc123def456",
            "choices": [{
                "message": {
                    "content": "Hello, world!"
                }
            }],
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 7
            }
        });

        let content = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap();

        let usage = TokenUsage {
            prompt_tokens: data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            completion_tokens: data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };

        let generation_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        assert_eq!(content, "Hello, world!");
        assert_eq!(usage.prompt_tokens, 42);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(generation_id.as_deref(), Some("gen-abc123def456"));
    }

    #[test]
    fn test_generation_id_missing_gracefully() {
        // OpenRouter may omit the id field in some error/edge cases
        let data: Value = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "response text"
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        });

        let generation_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        assert_eq!(generation_id, None);
    }

    #[test]
    fn test_usage_missing_gracefully() {
        // If usage block is absent, we fall back to zeros
        let data: Value = serde_json::json!({
            "id": "gen-xyz",
            "choices": [{
                "message": {
                    "content": "ok"
                }
            }]
        });

        let usage = TokenUsage {
            prompt_tokens: data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            completion_tokens: data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };

        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
    }

    // Phase 3: prefixed-json and SSE envelope parsing live in
    // `pyramid::provider::OpenRouterProvider::parse_response`. The
    // corresponding coverage is in `pyramid::provider::tests`.

    #[test]
    fn test_extract_json_basic() {
        let input = r#"Here is the result: {"key": "value"} done"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_extract_json_with_think_tags() {
        let input = r#"<think>reasoning here</think>{"answer": 42}"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["answer"], 42);
    }

    #[test]
    fn test_extract_json_with_markdown_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        let result = extract_json(input).unwrap();
        assert_eq!(result["a"], 1);
    }

    #[test]
    fn test_extract_json_trailing_comma() {
        let input = r#"{"items": ["a", "b",]}"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["items"][0], "a");
    }

    #[test]
    fn test_compute_timeout_respects_min_timeout_floor() {
        let defaults = LlmConfig::default();
        let timeout = compute_timeout(
            33_000,
            &LlmCallOptions {
                min_timeout_secs: Some(420),
                ..Default::default()
            },
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 420);
    }

    #[test]
    fn test_compute_timeout_scales_with_prompt_size() {
        let defaults = LlmConfig::default();
        // 200k chars = 2 increments * 60s = 120s added to base 120s = 240s
        let timeout = compute_timeout(
            200_000,
            &LlmCallOptions::default(),
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 240);
    }

    #[test]
    fn test_compute_timeout_capped_at_max() {
        let defaults = LlmConfig::default();
        // Very large prompt should be capped at max_timeout_secs (600)
        let timeout = compute_timeout(
            10_000_000,
            &LlmCallOptions::default(),
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 600);
    }

    // ── Phase 6: Cache hit / force-fresh end-to-end ─────────────────────

    /// Build a temp pyramid DB with a slug and the cache table ready to
    /// receive entries. Returns the path so the LLM call can re-open it.
    fn temp_pyramid_db_with_slug(slug: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp db file");
        let conn = super::super::db::open_pyramid_db(file.path()).expect("open pyramid db");
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'document', '/tmp/source')",
            rusqlite::params![slug],
        )
        .expect("insert slug");
        file
    }

    fn pre_populate_cache(
        db_path: &std::path::Path,
        slug: &str,
        cache_key: &str,
        inputs_hash: &str,
        prompt_hash: &str,
        model_id: &str,
        content: &str,
    ) {
        let conn = super::super::db::open_pyramid_db(db_path).expect("reopen db");
        let entry = super::super::step_context::CacheEntry {
            slug: slug.into(),
            build_id: "build-1".into(),
            step_name: "test_step".into(),
            chunk_index: -1,
            depth: 0,
            cache_key: cache_key.into(),
            inputs_hash: inputs_hash.into(),
            prompt_hash: prompt_hash.into(),
            model_id: model_id.into(),
            output_json: serde_json::json!({
                "content": content,
                "usage": {"prompt_tokens": 11, "completion_tokens": 22},
                "generation_id": "gen-cached-1"
            })
            .to_string(),
            token_usage_json: None,
            cost_usd: None,
            latency_ms: Some(7),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
        };
        super::super::db::store_cache(&conn, &entry).expect("seed cache row");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_cache_hit_returns_cached_response_without_http() {
        // The cache hit path returns BEFORE any HTTP work runs. With a
        // pre-populated row, no provider/registry/credentials needed.
        let db = temp_pyramid_db_with_slug("test-slug");
        let system = "system prompt";
        let user = "user prompt";
        let model_id = "test/model-1";
        let prompt_hash = "phash-test-1";

        let inputs_hash = compute_inputs_hash(system, user);
        let cache_key = compute_cache_key(&inputs_hash, prompt_hash, model_id);
        pre_populate_cache(
            db.path(),
            "test-slug",
            &cache_key,
            &inputs_hash,
            prompt_hash,
            model_id,
            "cached content (should be returned without HTTP)",
        );

        let ctx = StepContext::new(
            "test-slug",
            "build-1",
            "test_step",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", model_id)
        .with_prompt_hash(prompt_hash);

        // No provider_registry, no credentials — the cache hit short-
        // circuits before `build_call_provider` runs, so an empty
        // LlmConfig is fine.
        let cfg = LlmConfig::default();
        let response = call_model_unified_with_options_and_ctx(
            &cfg,
            Some(&ctx),
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await
        .expect("cache hit must return Ok");
        assert_eq!(
            response.content,
            "cached content (should be returned without HTTP)"
        );
        assert_eq!(response.usage.prompt_tokens, 11);
        assert_eq!(response.usage.completion_tokens, 22);
        assert_eq!(response.generation_id.as_deref(), Some("gen-cached-1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_cache_lookup_skipped_without_step_context() {
        // When no StepContext is provided the cache layer is bypassed.
        // We confirm this by NOT pre-populating any row and observing
        // that the call fails on HTTP (no provider registry attached
        // and no api_key, so the synth fallback hits a network error).
        // The key correctness check is that the function does NOT
        // return a 'no cached row found' error — that would mean it
        // tried to consult the cache without a ctx.
        let cfg = LlmConfig::default();
        let result = call_model_unified_with_options_and_ctx(
            &cfg,
            None,
            "system",
            "user",
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await;
        assert!(
            result.is_err(),
            "no ctx + no api key should error on HTTP path, not cache"
        );
        let err = result.unwrap_err().to_string();
        // The error is from the HTTP retry loop, NOT a cache-layer
        // error. We assert it doesn't mention cache-related words.
        assert!(
            !err.contains("cache_key") && !err.contains("verify_cache_hit"),
            "no-ctx path must not consult the cache: err={}",
            err
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_force_fresh_bypasses_cache_lookup() {
        // With force_fresh = true, the cache lookup is skipped even
        // when a row exists. We pre-populate a row, set force_fresh,
        // and confirm the call falls through to HTTP (which will
        // error because there's no real provider). The proof that we
        // bypassed the cache: the response is NOT the cached content.
        let db = temp_pyramid_db_with_slug("test-slug");
        let system = "system";
        let user = "user prompt force fresh";
        let model_id = "test/model-1";
        let prompt_hash = "phash-test-2";
        let inputs_hash = compute_inputs_hash(system, user);
        let cache_key = compute_cache_key(&inputs_hash, prompt_hash, model_id);
        pre_populate_cache(
            db.path(),
            "test-slug",
            &cache_key,
            &inputs_hash,
            prompt_hash,
            model_id,
            "stale cached content",
        );

        let ctx = StepContext::new(
            "test-slug",
            "build-1",
            "test_step",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", model_id)
        .with_prompt_hash(prompt_hash)
        .with_force_fresh(true);

        let cfg = LlmConfig::default();
        // Reduce retries so the test fails fast.
        let mut cfg = cfg;
        cfg.max_retries = 1;
        cfg.base_timeout_secs = 1;
        cfg.retryable_status_codes = vec![];
        cfg.retry_base_sleep_secs = 0;

        let result = call_model_unified_with_options_and_ctx(
            &cfg,
            Some(&ctx),
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await;
        // The HTTP path failed (no real provider) — that's the proof
        // that force_fresh did NOT use the cache.
        assert!(
            result.is_err(),
            "force_fresh + no real provider must hit the HTTP path and error"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_cache_hit_verification_failure_deletes_stale_row() {
        // Pre-populate a row whose stored inputs_hash does NOT match
        // what compute_inputs_hash will produce. The verifier rejects
        // it and the row is deleted.
        let db = temp_pyramid_db_with_slug("test-slug");
        let system = "system";
        let user = "user content for mismatch";
        let model_id = "test/model-mm";
        let prompt_hash = "phash-mm";

        let real_inputs_hash = compute_inputs_hash(system, user);
        let cache_key = compute_cache_key(&real_inputs_hash, prompt_hash, model_id);

        // The row stores a wrong inputs_hash but matches on cache_key
        // (we control both — this simulates the rare collision /
        // concurrent-writer mismatch scenario).
        pre_populate_cache(
            db.path(),
            "test-slug",
            &cache_key,
            "WRONG-INPUTS-HASH",
            prompt_hash,
            model_id,
            "should-not-be-returned",
        );

        let ctx = StepContext::new(
            "test-slug",
            "build-1",
            "test_step",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", model_id)
        .with_prompt_hash(prompt_hash);

        let mut cfg = LlmConfig::default();
        cfg.max_retries = 1;
        cfg.base_timeout_secs = 1;
        cfg.retryable_status_codes = vec![];
        cfg.retry_base_sleep_secs = 0;

        let _ = call_model_unified_with_options_and_ctx(
            &cfg,
            Some(&ctx),
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await;
        // After the verification-failure path, the row should be
        // gone — re-check the DB directly.
        let conn = super::super::db::open_pyramid_db(db.path()).unwrap();
        let row = super::super::db::check_cache(&conn, "test-slug", &cache_key).unwrap();
        assert!(
            row.is_none(),
            "verification-failed row must be deleted from the cache"
        );
    }

    // ── Phase 18b L8: cache + audit unified path ─────────────────────────

    /// Build a tokio-mutex-wrapped audit Connection on the given DB path.
    /// The cache + audit unified function locks this guard to write the
    /// audit row, so the test can verify the row landed.
    fn audit_conn_for(
        db_path: &std::path::Path,
        slug: &str,
    ) -> std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>> {
        let conn = super::super::db::open_pyramid_db(db_path).expect("open audit conn");
        // Make sure the slug row exists for FK-like wiring (not a real
        // FK in the schema, but matches what the production code does).
        let _ = conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'document', '/tmp/source')",
            rusqlite::params![slug],
        );
        std::sync::Arc::new(tokio::sync::Mutex::new(conn))
    }

    /// Helper: count rows in `pyramid_llm_audit` for a given slug, with
    /// an optional `cache_hit` filter (`Some(true)` for cache-hit rows,
    /// `Some(false)` for wire-call rows, `None` for any).
    fn count_audit_rows(
        db_path: &std::path::Path,
        slug: &str,
        cache_hit_filter: Option<bool>,
    ) -> i64 {
        let conn = super::super::db::open_pyramid_db(db_path).expect("reopen for count");
        match cache_hit_filter {
            Some(flag) => {
                let v = if flag { 1 } else { 0 };
                conn.query_row(
                    "SELECT COUNT(*) FROM pyramid_llm_audit
                     WHERE slug = ?1 AND cache_hit = ?2",
                    rusqlite::params![slug, v],
                    |r| r.get(0),
                )
                .unwrap_or(0)
            }
            None => conn
                .query_row(
                    "SELECT COUNT(*) FROM pyramid_llm_audit WHERE slug = ?1",
                    rusqlite::params![slug],
                    |r| r.get(0),
                )
                .unwrap_or(0),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_phase18b_audited_cache_hit_writes_cache_hit_audit_row() {
        // L8 acceptance: when an audited LLM call serves from cache,
        // the unified entry point still writes a single audit row
        // stamped `cache_hit = 1`. The cached response is returned
        // without making an HTTP call.
        let db = temp_pyramid_db_with_slug("p18b-l8");
        let system = "audited cache hit system";
        let user = "audited cache hit user";
        let model_id = "test/model-l8";
        let prompt_hash = "phash-l8";

        let inputs_hash = compute_inputs_hash(system, user);
        let cache_key = compute_cache_key(&inputs_hash, prompt_hash, model_id);
        pre_populate_cache(
            db.path(),
            "p18b-l8",
            &cache_key,
            &inputs_hash,
            prompt_hash,
            model_id,
            "cached-l8-content",
        );

        let ctx = StepContext::new(
            "p18b-l8",
            "build-l8",
            "evidence_pre_map",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", model_id)
        .with_prompt_hash(prompt_hash);

        let audit = AuditContext {
            conn: audit_conn_for(db.path(), "p18b-l8"),
            slug: "p18b-l8".to_string(),
            build_id: "build-l8".to_string(),
            node_id: None,
            step_name: "evidence_pre_map".to_string(),
            call_purpose: "test_l8_cache_hit".to_string(),
            depth: Some(0),
        };

        // Baseline: no audit rows yet for this slug.
        assert_eq!(count_audit_rows(db.path(), "p18b-l8", None), 0);

        let cfg = LlmConfig::default();
        let response = call_model_unified_with_audit_and_ctx(
            &cfg,
            Some(&ctx),
            Some(&audit),
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await
        .expect("cache hit must return Ok");

        // The cached content is returned, NOT something HTTP-fetched.
        assert_eq!(response.content, "cached-l8-content");
        assert_eq!(response.usage.prompt_tokens, 11);
        assert_eq!(response.usage.completion_tokens, 22);

        // The audit row landed and is stamped as a cache hit.
        assert_eq!(
            count_audit_rows(db.path(), "p18b-l8", Some(true)),
            1,
            "exactly one cache_hit=1 audit row"
        );
        assert_eq!(
            count_audit_rows(db.path(), "p18b-l8", Some(false)),
            0,
            "no wire-call rows"
        );
        assert_eq!(
            count_audit_rows(db.path(), "p18b-l8", None),
            1,
            "exactly one audit row total"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_phase18b_audited_cache_miss_falls_through_to_pending_path() {
        // L8 secondary: when there is NO matching cached row but an
        // AuditContext is supplied, the unified entry point inserts a
        // pending audit row, then attempts the HTTP call. With no
        // provider configured the HTTP path errors, and the audit row
        // is flipped to `failed` via maybe_fail_audit. The test
        // confirms an audit row exists, that it's NOT a cache_hit row,
        // and that the call returned an error (not a cached response).
        let db = temp_pyramid_db_with_slug("p18b-l8-miss");
        let system = "audited miss system";
        let user = "audited miss user";

        let ctx = StepContext::new(
            "p18b-l8-miss",
            "build-miss",
            "evidence_pre_map",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", "test/model-miss")
        .with_prompt_hash("phash-miss");

        let audit = AuditContext {
            conn: audit_conn_for(db.path(), "p18b-l8-miss"),
            slug: "p18b-l8-miss".to_string(),
            build_id: "build-miss".to_string(),
            node_id: None,
            step_name: "evidence_pre_map".to_string(),
            call_purpose: "test_l8_cache_miss".to_string(),
            depth: Some(0),
        };

        let mut cfg = LlmConfig::default();
        cfg.max_retries = 1;
        cfg.base_timeout_secs = 1;
        cfg.retryable_status_codes = vec![];
        cfg.retry_base_sleep_secs = 0;

        let _ = call_model_unified_with_audit_and_ctx(
            &cfg,
            Some(&ctx),
            Some(&audit),
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await;

        // Even though the HTTP call errored, the pending audit row was
        // written before the call started, then flipped to 'failed' by
        // maybe_fail_audit. The cache_hit flag is 0 because this was
        // not a cache hit.
        let total = count_audit_rows(db.path(), "p18b-l8-miss", None);
        let cache_hits = count_audit_rows(db.path(), "p18b-l8-miss", Some(true));
        let wire_calls = count_audit_rows(db.path(), "p18b-l8-miss", Some(false));
        assert_eq!(total, 1, "one audit row total (the pending → failed row)");
        assert_eq!(cache_hits, 0, "no cache_hit rows on a miss");
        assert_eq!(wire_calls, 1, "exactly one wire-call audit row");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_phase18b_unified_no_audit_matches_legacy_cache_path() {
        // Regression: when audit is None, the unified entry point must
        // behave identically to the pre-Phase-18b
        // `call_model_unified_with_options_and_ctx`. We pre-populate
        // the cache and assert the cache hit returns the cached
        // response without writing any audit row.
        let db = temp_pyramid_db_with_slug("p18b-l8-noaudit");
        let system = "noaudit system";
        let user = "noaudit user";
        let model_id = "test/model-noaudit";
        let prompt_hash = "phash-noaudit";

        let inputs_hash = compute_inputs_hash(system, user);
        let cache_key = compute_cache_key(&inputs_hash, prompt_hash, model_id);
        pre_populate_cache(
            db.path(),
            "p18b-l8-noaudit",
            &cache_key,
            &inputs_hash,
            prompt_hash,
            model_id,
            "noaudit-cached",
        );

        let ctx = StepContext::new(
            "p18b-l8-noaudit",
            "build-1",
            "test_step",
            "extract",
            0,
            None,
            db.path().to_string_lossy().to_string(),
        )
        .with_model_resolution("fast_extract", model_id)
        .with_prompt_hash(prompt_hash);

        let cfg = LlmConfig::default();
        let response = call_model_unified_with_audit_and_ctx(
            &cfg,
            Some(&ctx),
            None,
            system,
            user,
            0.2,
            4096,
            None,
            LlmCallOptions::default(),
        )
        .await
        .expect("cache hit returns Ok");
        assert_eq!(response.content, "noaudit-cached");

        // No audit rows landed because audit was None.
        assert_eq!(count_audit_rows(db.path(), "p18b-l8-noaudit", None), 0);
    }

    // ── prepare_for_replay tests ─────────────────────────────────────────────
    //
    // Walker re-plan Wire 2.1 §2.5.1: prepare_for_replay is the single
    // source of truth for which dispatch-routing fields get cleared before
    // a replay or inbound-job worker runs. Origin-independent: all three
    // origins clear compute_queue + fleet_dispatch + fleet_roster +
    // compute_market_context so the inner (replayed) call is pool-only.

    fn build_live_config_with_all_dispatch_handles_for_test() -> LlmConfig {
        let policy_yaml: crate::pyramid::dispatch_policy::DispatchPolicyYaml =
            serde_yaml::from_str(
                r#"
version: 1
provider_pools:
  openrouter:
    concurrency: 1
routing_rules:
  - name: default
    match_config: {}
    route_to:
      - provider_id: openrouter
"#,
            )
            .unwrap();
        let dispatch_policy = std::sync::Arc::new(
            crate::pyramid::dispatch_policy::DispatchPolicy::from_yaml(&policy_yaml),
        );
        let provider_pools = std::sync::Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(dispatch_policy.as_ref()),
        );
        let compute_queue = crate::compute_queue::ComputeQueueHandle::new();
        let fleet_roster = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::fleet::FleetRoster::default(),
        ));
        let tunnel_state = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::tunnel::TunnelState::default(),
        ));
        let fleet_dispatch = std::sync::Arc::new(crate::fleet::FleetDispatchContext {
            tunnel_state: tunnel_state.clone(),
            fleet_roster: fleet_roster.clone(),
            pending: std::sync::Arc::new(crate::fleet::PendingFleetJobs::new()),
            policy: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy::default(),
            )),
        });
        let auth = std::sync::Arc::new(tokio::sync::RwLock::new(crate::auth::AuthState::default()));
        let node_config = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::WireNodeConfig::default(),
        ));
        let pending_jobs = crate::pyramid::pending_jobs::PendingJobs::new();
        let compute_market_context = crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext {
            auth,
            config: node_config,
            pending_jobs,
            tunnel_state,
        };

        LlmConfig {
            dispatch_policy: Some(dispatch_policy),
            provider_pools: Some(provider_pools),
            compute_queue: Some(compute_queue),
            fleet_roster: Some(fleet_roster),
            fleet_dispatch: Some(fleet_dispatch),
            compute_market_context: Some(compute_market_context),
            ..Default::default()
        }
    }

    fn assert_all_dispatch_handles_cleared(cfg: &LlmConfig) {
        assert!(cfg.compute_queue.is_none(), "compute_queue must be cleared");
        assert!(cfg.fleet_dispatch.is_none(), "fleet_dispatch must be cleared");
        assert!(cfg.fleet_roster.is_none(), "fleet_roster must be cleared");
        assert!(
            cfg.compute_market_context.is_none(),
            "compute_market_context must be cleared"
        );
    }

    fn assert_durable_fields_preserved(live: &LlmConfig, replay: &LlmConfig) {
        assert!(std::sync::Arc::ptr_eq(
            replay.dispatch_policy.as_ref().unwrap(),
            live.dispatch_policy.as_ref().unwrap(),
        ));
        assert!(std::sync::Arc::ptr_eq(
            replay.provider_pools.as_ref().unwrap(),
            live.provider_pools.as_ref().unwrap(),
        ));
    }

    #[test]
    fn prepare_for_replay_local_clears_all_dispatch_handles() {
        let live = build_live_config_with_all_dispatch_handles_for_test();
        let replay = live.prepare_for_replay(DispatchOrigin::Local);
        assert_all_dispatch_handles_cleared(&replay);
        assert_durable_fields_preserved(&live, &replay);
    }

    #[test]
    fn prepare_for_replay_fleet_received_clears_all_dispatch_handles() {
        let live = build_live_config_with_all_dispatch_handles_for_test();
        let replay = live.prepare_for_replay(DispatchOrigin::FleetReceived);
        assert_all_dispatch_handles_cleared(&replay);
        assert_durable_fields_preserved(&live, &replay);
    }

    #[test]
    fn prepare_for_replay_market_received_clears_all_dispatch_handles() {
        let live = build_live_config_with_all_dispatch_handles_for_test();
        let replay = live.prepare_for_replay(DispatchOrigin::MarketReceived);
        assert_all_dispatch_handles_cleared(&replay);
        assert_durable_fields_preserved(&live, &replay);
    }

    // ── classify_branch + branch_allowed (§2.5.2) ────────────────────────────

    #[test]
    fn classify_branch_maps_sentinels_to_walker_branches() {
        assert_eq!(classify_branch("fleet"), RouteBranch::Fleet);
        assert_eq!(classify_branch("market"), RouteBranch::Market);
        assert_eq!(classify_branch("openrouter"), RouteBranch::Pool);
        assert_eq!(classify_branch("ollama-local"), RouteBranch::Pool);
        assert_eq!(classify_branch("remote-5090"), RouteBranch::Pool);
        assert_eq!(classify_branch(""), RouteBranch::Pool);
    }

    #[test]
    fn branch_allowed_pool_always_ok() {
        assert!(branch_allowed(RouteBranch::Pool, DispatchOrigin::Local));
        assert!(branch_allowed(RouteBranch::Pool, DispatchOrigin::FleetReceived));
        assert!(branch_allowed(RouteBranch::Pool, DispatchOrigin::MarketReceived));
    }

    #[test]
    fn branch_allowed_fleet_only_from_local() {
        assert!(branch_allowed(RouteBranch::Fleet, DispatchOrigin::Local));
        assert!(!branch_allowed(
            RouteBranch::Fleet,
            DispatchOrigin::FleetReceived
        ));
        assert!(!branch_allowed(
            RouteBranch::Fleet,
            DispatchOrigin::MarketReceived
        ));
    }

    #[test]
    fn branch_allowed_market_only_from_local() {
        assert!(branch_allowed(RouteBranch::Market, DispatchOrigin::Local));
        assert!(!branch_allowed(
            RouteBranch::Market,
            DispatchOrigin::FleetReceived
        ));
        assert!(!branch_allowed(
            RouteBranch::Market,
            DispatchOrigin::MarketReceived
        ));
    }

    // ── EntryError taxonomy (§2.5.3) ─────────────────────────────────────────

    #[test]
    fn entry_error_variant_tags_match_chronicle_vocab() {
        let r = EntryError::Retryable {
            reason: "transient 503".into(),
        };
        let s = EntryError::RouteSkipped {
            reason: "insufficient_balance".into(),
        };
        let t = EntryError::CallTerminal {
            reason: "multi_system_messages".into(),
        };

        assert_eq!(r.variant_tag(), "retryable");
        assert_eq!(s.variant_tag(), "route_skipped");
        assert_eq!(t.variant_tag(), "call_terminal");
    }

    #[test]
    fn entry_error_reason_accessor_uniform_across_variants() {
        assert_eq!(
            EntryError::Retryable {
                reason: "r".into()
            }
            .reason(),
            "r"
        );
        assert_eq!(
            EntryError::RouteSkipped {
                reason: "s".into()
            }
            .reason(),
            "s"
        );
        assert_eq!(
            EntryError::CallTerminal {
                reason: "t".into()
            }
            .reason(),
            "t"
        );
    }

    #[test]
    fn entry_error_display_matches_variant_tag_colon_reason() {
        let e = EntryError::RouteSkipped {
            reason: "insufficient_balance".into(),
        };
        assert_eq!(e.to_string(), "route_skipped: insufficient_balance");
    }

    // ──────────────────────────────────────────────────────────────────────
    // Per-slug chronicle event mapping — covers the 7 market-specific event
    // constants declared in compute_chronicle.rs and wired in the walker
    // market branch.
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn map_market_slug_quote_jwt_expired_is_quote_expired_event() {
        assert_eq!(
            map_market_slug_to_specific_event("quote_jwt_expired"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_QUOTE_EXPIRED),
        );
    }

    #[test]
    fn map_market_slug_bare_quote_expired_is_quote_expired_event() {
        assert_eq!(
            map_market_slug_to_specific_event("quote_expired"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_QUOTE_EXPIRED),
        );
    }

    #[test]
    fn map_market_slug_quote_already_purchased_is_purchase_recovered() {
        assert_eq!(
            map_market_slug_to_specific_event("quote_already_purchased"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_PURCHASE_RECOVERED),
        );
    }

    #[test]
    fn map_market_slug_budget_exceeded_is_rate_above_budget() {
        assert_eq!(
            map_market_slug_to_specific_event("budget_exceeded"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_RATE_ABOVE_BUDGET),
        );
    }

    #[test]
    fn map_market_slug_dispatch_deadline_exceeded_is_deadline_missed() {
        assert_eq!(
            map_market_slug_to_specific_event("dispatch_deadline_exceeded"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_DISPATCH_DEADLINE_MISSED),
        );
    }

    #[test]
    fn map_market_slug_provider_queue_full_is_provider_saturated() {
        assert_eq!(
            map_market_slug_to_specific_event("provider_queue_full"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_PROVIDER_SATURATED),
        );
    }

    #[test]
    fn map_market_slug_provider_depth_exceeded_is_provider_saturated() {
        // /fill-stage saturation aliases to the same operator-facing event
        // as /purchase-stage `provider_queue_full` — both mean "provider
        // can't take any more right now."
        assert_eq!(
            map_market_slug_to_specific_event("provider_depth_exceeded"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_PROVIDER_SATURATED),
        );
    }

    #[test]
    fn map_market_slug_insufficient_balance_is_balance_insufficient() {
        assert_eq!(
            map_market_slug_to_specific_event("insufficient_balance"),
            Some(
                super::super::compute_chronicle::EVENT_NETWORK_BALANCE_INSUFFICIENT_FOR_MARKET,
            ),
        );
    }

    #[test]
    fn map_market_slug_balance_depleted_is_balance_insufficient() {
        assert_eq!(
            map_market_slug_to_specific_event("balance_depleted"),
            Some(
                super::super::compute_chronicle::EVENT_NETWORK_BALANCE_INSUFFICIENT_FOR_MARKET,
            ),
        );
    }

    #[test]
    fn map_market_slug_stage_auth_failures_are_auth_expired() {
        for reason in [
            "quote_auth_failed",
            "purchase_auth_failed",
            "fill_auth_failed",
            "unauthorized",
        ] {
            assert_eq!(
                map_market_slug_to_specific_event(reason),
                Some(super::super::compute_chronicle::EVENT_NETWORK_AUTH_EXPIRED),
                "reason `{}` should map to network_auth_expired",
                reason,
            );
        }
    }

    #[test]
    fn map_market_slug_unknown_returns_none() {
        assert_eq!(map_market_slug_to_specific_event("totally_new_slug"), None);
        assert_eq!(map_market_slug_to_specific_event(""), None);
    }

    #[test]
    fn map_market_slug_leading_token_match_handles_wrapped_reasons() {
        // classify_rev21_slug wraps unknown slugs as "unknown_slug:<raw>".
        // Primary-token match drops the suffix and keys on the first token,
        // so a future Wire slug we happen to recognize still fires.
        assert_eq!(
            map_market_slug_to_specific_event("budget_exceeded:extra_detail"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_RATE_ABOVE_BUDGET),
        );
        // And the skip_reasons format pattern `retryable(reason)` — the
        // helper is called with the inner reason directly in live code,
        // but test the leading-paren cut too for defense-in-depth.
        assert_eq!(
            map_market_slug_to_specific_event("quote_jwt_expired(context)"),
            Some(super::super::compute_chronicle::EVENT_NETWORK_QUOTE_EXPIRED),
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Post-ship walker bug fixes (W1 + C1) — classify_pool_400 +
    // resolve_route_model + cascade-crossing regression guard.
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn classify_pool_400_openrouter_model_rejection_is_route_skipped() {
        // The exact body shape OpenRouter returned in Mac post-ship smoke.
        let body = r#"{"error":{"message":"gemma4:26b is not a valid model ID"}}"#;
        match classify_pool_400(body) {
            EntryError::RouteSkipped { reason } => {
                assert!(
                    reason.contains("provider_rejected_model"),
                    "reason should tag provider_rejected_model, got: {reason}"
                );
            }
            other => panic!("expected RouteSkipped, got {other:?}"),
        }
    }

    #[test]
    fn classify_pool_400_model_not_found_is_route_skipped() {
        let body = r#"{"error":"model not found: gpt-x"}"#;
        match classify_pool_400(body) {
            EntryError::RouteSkipped { reason } => {
                assert!(reason.contains("provider_rejected_model"), "got: {reason}");
            }
            other => panic!("expected RouteSkipped, got {other:?}"),
        }
    }

    #[test]
    fn classify_pool_400_feature_unsupported_is_route_skipped() {
        let body = r#"{"error":{"message":"response_format not supported by this model"}}"#;
        match classify_pool_400(body) {
            EntryError::RouteSkipped { reason } => {
                assert!(
                    reason.contains("provider_feature_unsupported"),
                    "got: {reason}"
                );
            }
            other => panic!("expected RouteSkipped, got {other:?}"),
        }
    }

    #[test]
    fn classify_pool_400_bad_json_is_call_terminal() {
        let body = "Bad JSON: unexpected token at position 42";
        match classify_pool_400(body) {
            EntryError::CallTerminal { reason } => {
                assert!(reason.contains("body_shape_error"), "got: {reason}");
            }
            other => panic!("expected CallTerminal, got {other:?}"),
        }
    }

    #[test]
    fn classify_pool_400_empty_body_is_call_terminal() {
        match classify_pool_400("") {
            EntryError::CallTerminal { reason } => {
                assert!(reason.contains("body_shape_error"), "got: {reason}");
            }
            other => panic!("expected CallTerminal on empty body, got {other:?}"),
        }
    }

    #[test]
    fn truncate_utf8_respects_char_boundary_no_panic() {
        // Four-byte scalar ("💥" = U+1F4A5) placed so a naive byte-slice
        // at max=2 would cut through the middle of it.
        let s = "💥abc";
        assert_eq!(s.len(), 7); // 4 + 3
        let out = truncate_utf8(s, 2);
        // Must not panic; must not return an invalid-UTF8 byte string.
        // The walk-back lands on byte 0 (no char boundary up to 2).
        assert!(out.is_empty() || out == "💥");
        // Longer truncation past the scalar keeps it intact.
        let out2 = truncate_utf8(s, 5);
        assert!(out2.starts_with('💥'));
    }

    #[test]
    fn truncate_utf8_under_max_returns_as_is() {
        assert_eq!(truncate_utf8("short", 200), "short");
    }

    #[test]
    fn classify_pool_400_truncates_long_bodies_without_utf8_panic() {
        // Build a long body whose first 200 bytes end mid-scalar.
        let mut body = String::new();
        for _ in 0..100 {
            body.push('💥');
        }
        // "not a valid model" appears early so it routes to RouteSkipped,
        // but the truncation still has to not panic on the long tail.
        let body = format!("not a valid model: {body}");
        let _ = classify_pool_400(&body); // no panic
    }

    #[test]
    fn resolve_route_model_priority_1_explicit_entry_model_id() {
        use crate::pyramid::dispatch_policy::RouteEntry;
        let entry = RouteEntry {
            provider_id: "openrouter".into(),
            model_id: Some("explicit/model".into()),
            tier_name: Some("some_tier".into()),
            is_local: false,
            max_budget_credits: None,
        };
        // Registry passed as None — explicit override must win regardless.
        let got = resolve_route_model(&entry, None, "stale/primary");
        assert_eq!(got, "explicit/model");
    }

    #[test]
    fn resolve_route_model_priority_2_tier_routing_match() {
        use crate::pyramid::dispatch_policy::RouteEntry;
        use crate::pyramid::provider::{Provider, ProviderRegistry, ProviderType, TierRoutingEntry};
        use crate::pyramid::credentials::CredentialStore;
        use std::sync::Arc;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        std::mem::forget(tmp);

        let registry = Arc::new(ProviderRegistry::new(store));
        // tier_routing has FK → provider_id; ensure provider row exists.
        registry
            .save_provider(
                &conn,
                Provider {
                    id: "openrouter".into(),
                    display_name: "OR".into(),
                    provider_type: ProviderType::Openrouter,
                    base_url: "https://openrouter.ai/api/v1".into(),
                    api_key_ref: Some("OPENROUTER_KEY".into()),
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();
        registry
            .save_tier_routing(
                &conn,
                TierRoutingEntry {
                    tier_name: "my_tier".into(),
                    provider_id: "openrouter".into(),
                    model_id: "tier/model-from-routing".into(),
                    context_limit: Some(128_000),
                    max_completion_tokens: None,
                    pricing_json: "{}".into(),
                    supported_parameters_json: None,
                    notes: None,
                },
            )
            .unwrap();

        let entry = RouteEntry {
            provider_id: "openrouter".into(),
            model_id: None,
            tier_name: Some("my_tier".into()),
            is_local: false,
            max_budget_credits: None,
        };
        let got = resolve_route_model(&entry, Some(&registry), "stale/primary");
        assert_eq!(got, "tier/model-from-routing");
    }

    #[test]
    fn resolve_route_model_priority_2_skips_when_tier_provider_mismatch() {
        // Tier routing row is for a DIFFERENT provider than the route
        // entry. Resolver must NOT cross-provider an incompatible slug —
        // it falls through to the primary_model fallback. This is the
        // regression guard for the original C1 bug.
        use crate::pyramid::dispatch_policy::RouteEntry;
        use crate::pyramid::provider::{Provider, ProviderRegistry, ProviderType, TierRoutingEntry};
        use crate::pyramid::credentials::CredentialStore;
        use std::sync::Arc;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        std::mem::forget(tmp);

        let registry = Arc::new(ProviderRegistry::new(store));
        registry
            .save_provider(
                &conn,
                Provider {
                    id: "ollama-local".into(),
                    display_name: "Ollama".into(),
                    provider_type: ProviderType::OpenaiCompat,
                    base_url: "http://127.0.0.1:11434/v1".into(),
                    api_key_ref: None,
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();
        registry
            .save_tier_routing(
                &conn,
                TierRoutingEntry {
                    tier_name: "local_tier".into(),
                    provider_id: "ollama-local".into(),
                    model_id: "gemma4:26b".into(),
                    context_limit: Some(32_000),
                    max_completion_tokens: None,
                    pricing_json: "{}".into(),
                    supported_parameters_json: None,
                    notes: None,
                },
            )
            .unwrap();

        let entry = RouteEntry {
            provider_id: "openrouter".into(),
            model_id: None,
            tier_name: Some("local_tier".into()),
            is_local: false,
            max_budget_credits: None,
        };
        let got = resolve_route_model(&entry, Some(&registry), "fallback/primary");
        // Must fall through — NOT "gemma4:26b".
        assert_eq!(got, "fallback/primary");
    }

    #[test]
    fn resolve_route_model_priority_3_primary_fallback() {
        use crate::pyramid::dispatch_policy::RouteEntry;
        let entry = RouteEntry {
            provider_id: "openrouter".into(),
            model_id: None,
            tier_name: None,
            is_local: false,
            max_budget_credits: None,
        };
        let got = resolve_route_model(&entry, None, "fallback/primary");
        assert_eq!(got, "fallback/primary");
    }

    #[tokio::test]
    async fn walker_advances_past_openrouter_400_model_rejection_to_ollama_local() {
        // Integration-style regression guard for the cascade-crossing
        // bug Adam caught. Two pool entries:
        //   - openrouter (mockito 400 with "not a valid model ID" body)
        //   - ollama-local (mockito 200 with a valid chat-completions body)
        // Walker must classify the 400 as RouteSkipped (W1) and advance
        // to ollama-local, which succeeds.
        use crate::pyramid::credentials::CredentialStore;
        use crate::pyramid::dispatch_policy::{
            BuildCoordinationConfig, DispatchPolicy, EscalationConfig, MatchConfig,
            ProviderPoolConfig, RouteEntry, RoutingRule,
        };
        use crate::pyramid::provider::{Provider, ProviderRegistry, ProviderType};
        use std::sync::Arc;

        // Spin up mockito servers. Server::new_async is the recommended
        // constructor for tokio tests.
        let mut or_server = mockito::Server::new_async().await;
        let mut ol_server = mockito::Server::new_async().await;

        let _or_mock = or_server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"error":{"message":"gemma4:26b is not a valid model ID"}}"#,
            )
            .expect_at_least(1)
            .create_async()
            .await;

        let ol_body = r#"{
            "id":"resp-1",
            "model":"gemma4:26b",
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"hello from ollama"},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}
        }"#;
        let _ol_mock = ol_server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ol_body)
            .expect_at_least(1)
            .create_async()
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        store.set("OPENROUTER_KEY", "sk-or-v1-test").unwrap();
        std::mem::forget(tmp);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let registry = Arc::new(ProviderRegistry::new(store));
        // OpenRouter-style provider pointing at mock 1.
        registry
            .save_provider(
                &conn,
                Provider {
                    id: "openrouter".into(),
                    display_name: "OpenRouter (test)".into(),
                    provider_type: ProviderType::Openrouter,
                    base_url: or_server.url(),
                    api_key_ref: Some("OPENROUTER_KEY".into()),
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();
        // OpenAI-compat provider pointing at mock 2 (ollama-local shape).
        registry
            .save_provider(
                &conn,
                Provider {
                    id: "ollama-local".into(),
                    display_name: "Ollama (test)".into(),
                    provider_type: ProviderType::OpenaiCompat,
                    base_url: ol_server.url(),
                    api_key_ref: None,
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();

        // DispatchPolicy with both entries + pools configured.
        let mut pool_configs = std::collections::BTreeMap::new();
        pool_configs.insert(
            "openrouter".into(),
            ProviderPoolConfig {
                concurrency: 1,
                rate_limit: None,
            },
        );
        pool_configs.insert(
            "ollama-local".into(),
            ProviderPoolConfig {
                concurrency: 1,
                rate_limit: None,
            },
        );
        let policy = Arc::new(DispatchPolicy {
            rules: vec![RoutingRule {
                name: "cascade_test".into(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to: vec![
                    RouteEntry {
                        provider_id: "openrouter".into(),
                        model_id: Some("openai/gpt-4o-mini".into()),
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    },
                    RouteEntry {
                        provider_id: "ollama-local".into(),
                        model_id: Some("gemma4:26b".into()),
                        tier_name: None,
                        is_local: true,
                        max_budget_credits: None,
                    },
                ],
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        });
        let pools = Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(policy.as_ref()),
        );

        let config = LlmConfig {
            api_key: "sk-or-v1-test".into(),
            auth_token: String::new(),
            primary_model: "openai/gpt-4o-mini".into(),
            fallback_model_1: "openai/gpt-4o-mini".into(),
            fallback_model_2: "openai/gpt-4o-mini".into(),
            provider_registry: Some(registry.clone()),
            dispatch_policy: Some(policy),
            provider_pools: Some(pools),
            max_retries: 1,
            retry_base_sleep_secs: 0,
            ..Default::default()
        };

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let resp = result.expect(
            "walker must advance past openrouter 400 (W1) and succeed on ollama-local (C1)",
        );
        assert!(
            resp.content.contains("hello from ollama"),
            "expected ollama-local response content, got: {:?}",
            resp.content
        );
    }

    // ── Post-ship: walker queue short-circuit regression guard ──────────
    //
    // The pre-walker compute_queue block (deleted in this commit) fired
    // whenever `config.compute_queue.is_some()` + any route entry had
    // `is_local: true`. Production bundled seed has ollama-local at
    // position 4 with is_local:true, so every outer dispatch short-
    // circuited to the queue and the market + fleet branches above it
    // in the route were never walked. These tests pin the fix: market
    // runs before a local pool enqueue when both appear in the route.

    /// Helper: DispatchPolicy with a production-shape route that
    /// includes `is_local: true` on a pool entry. Mirrors the bundled
    /// seed's shape so the regression is exercised against a real-world
    /// configuration rather than the convenient `[market, unknown-pool]`
    /// shape existing walker tests used.
    fn walker_test_policy_with_local_pool(
        pool_concurrency: usize,
        route_entries: Vec<(&str, bool)>,
    ) -> std::sync::Arc<crate::pyramid::dispatch_policy::DispatchPolicy> {
        use crate::pyramid::dispatch_policy::*;
        let mut pool_configs = std::collections::BTreeMap::new();
        for (pid, _) in &route_entries {
            if !matches!(*pid, "market" | "fleet") {
                pool_configs.insert(
                    (*pid).to_string(),
                    ProviderPoolConfig {
                        concurrency: pool_concurrency,
                        rate_limit: None,
                    },
                );
            }
        }
        let route_to = route_entries
            .into_iter()
            .map(|(pid, is_local)| RouteEntry {
                provider_id: pid.to_string(),
                model_id: None,
                tier_name: None,
                is_local,
                max_budget_credits: None,
            })
            .collect();
        let policy = DispatchPolicy {
            rules: vec![RoutingRule {
                name: "walker_test_prod_shape".into(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to,
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };
        std::sync::Arc::new(policy)
    }

    /// Query pyramid_compute_events for (id, event_type, entry_provider_id)
    /// triples in rowid order. `entry_provider_id` is extracted from the
    /// JSON metadata blob `emit_walker_chronicle` writes. `enqueued` events
    /// written by the local-queue path don't go through `emit_walker_chronicle`
    /// — they use ChronicleEventContext directly — so `model_id` is used to
    /// distinguish them.
    fn read_chronicle_trail(db_path: &str) -> Vec<(i64, String, String)> {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, event_type, COALESCE(model_id, ''), COALESCE(metadata, '')
                 FROM pyramid_compute_events ORDER BY timestamp ASC, id ASC",
            )
            .unwrap();
        let rows: Vec<(i64, String, String)> = stmt
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                let ev: String = r.get(1)?;
                let model: String = r.get(2)?;
                let meta: String = r.get(3)?;
                // Pull `entry_provider_id` out of the metadata JSON when
                // present; fall back to model_id so the test has SOMETHING
                // to key on for the direct-chronicle `enqueued` rows.
                let tag = serde_json::from_str::<serde_json::Value>(&meta)
                    .ok()
                    .and_then(|v| {
                        v.get("entry_provider_id")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or(model);
                Ok((id, ev, tag))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows
    }

    /// Helper: build an LlmConfig against a tempdir-backed pyramid DB so
    /// walker chronicle events land on disk and the test can read them
    /// back in rowid order. Returns (config, db_file) — the NamedTempFile
    /// must live for the duration of the test.
    fn walker_test_config_with_queue(
        policy: std::sync::Arc<crate::pyramid::dispatch_policy::DispatchPolicy>,
    ) -> (LlmConfig, tempfile::NamedTempFile) {
        use std::sync::Arc;
        let db = temp_pyramid_db_with_slug("walker-short-circuit-test");
        let db_path: Arc<str> = db.path().to_string_lossy().to_string().into();

        let pools = Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(policy.as_ref()),
        );
        let queue = crate::compute_queue::ComputeQueueHandle::new();

        let config = LlmConfig {
            api_key: String::new(),
            auth_token: String::new(),
            primary_model: "walker-short-circuit/test-model".into(),
            fallback_model_1: "fallback1".into(),
            fallback_model_2: "fallback2".into(),
            dispatch_policy: Some(policy),
            provider_pools: Some(pools),
            compute_queue: Some(queue),
            cache_access: Some(CacheAccess {
                slug: "walker-short-circuit-test".into(),
                build_id: "build-1".into(),
                db_path,
                bus: None,
                chain_name: None,
                content_type: None,
            }),
            max_retries: 1,
            ..Default::default()
        };
        (config, db)
    }

    #[tokio::test]
    async fn walker_market_runs_before_local_pool_enqueue() {
        // Production-shape regression — route `[market, ollama-local(is_local:true)]`
        // with compute_queue attached and `compute_market_context = None`.
        //
        // Pre-fix behavior: the pre-walker block saw `any(is_local) &&
        // compute_queue.is_some()` and enqueued IMMEDIATELY; market was
        // never walked. The `enqueued` chronicle row was the ONLY row.
        //
        // Post-fix behavior: walker iterates the route. Market runs
        // first, emits `network_route_unavailable` with
        // reason=`no_market_context`, then advances. Local pool entry
        // reaches the new in-walker gate and enqueues. Both rows are
        // present and the market row comes FIRST.
        //
        // No GPU loop is consuming the queue, so the call hangs on
        // `rx.await`. We wrap in `tokio::time::timeout` and allow the
        // timeout to fire after the chronicle events have been emitted
        // (spawn_blocking → DB flush).

        let policy = walker_test_policy_with_local_pool(
            1,
            vec![("market", false), ("ollama-local", true)],
        );
        let (config, db) = walker_test_config_with_queue(policy);
        let db_path = db.path().to_string_lossy().to_string();

        let call_fut = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        );

        // The queue enqueue blocks forever without a GPU loop. Time out
        // after a short grace period — by then the market branch has
        // emitted its chronicle event and the enqueue has written its
        // `enqueued` row.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(400), call_fut).await;

        // spawn_blocking DB writes may still be in flight. Drain.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let rows = read_chronicle_trail(&db_path);
        assert!(
            !rows.is_empty(),
            "expected chronicle events in the trail, got none",
        );

        // Find the market route-unavailable row and the enqueued row.
        let market_idx = rows.iter().position(|(_, ev, tag)| {
            ev == "network_route_unavailable" && tag == "market"
        });
        let enqueued_idx = rows.iter().position(|(_, ev, _)| ev == "enqueued");

        let market_idx = market_idx.expect(
            "expected a `network_route_unavailable` event for the market entry \
             — pre-fix this row is absent because the pre-walker short-circuit \
             enqueued before market ran. Trail: {rows:?}",
        );
        let enqueued_idx = enqueued_idx
            .expect("expected an `enqueued` event for the local pool entry");

        assert!(
            market_idx < enqueued_idx,
            "expected market route_unavailable BEFORE enqueued; got market_idx={market_idx}, \
             enqueued_idx={enqueued_idx}; trail={rows:?}",
        );

        // Tie-down: market row's reason is specifically `no_market_context`.
        // Re-read raw metadata to assert the classification slug.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let meta: String = conn
            .query_row(
                "SELECT metadata FROM pyramid_compute_events
                 WHERE event_type = 'network_route_unavailable' AND id = ?1",
                rusqlite::params![rows[market_idx].0],
                |r| r.get(0),
            )
            .unwrap();
        let meta_json: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(
            meta_json.get("reason").and_then(|v| v.as_str()),
            Some("no_market_context"),
            "expected reason=no_market_context on market row; got {meta}",
        );
    }

    /// Spawn a fake GPU loop against a ComputeQueueHandle that pops the
    /// first entry and fires `result_tx.send(Ok(canned_response))`.
    /// Returns the JoinHandle so the test can await it / drop it at end.
    fn spawn_fake_gpu_loop(
        handle: crate::compute_queue::ComputeQueueHandle,
        canned_content: &'static str,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                handle.notify.notified().await;
                let entry_opt = {
                    let mut q = handle.queue.lock().await;
                    q.dequeue_next()
                };
                if let Some(entry) = entry_opt {
                    let response = LlmResponse {
                        content: canned_content.to_string(),
                        usage: super::super::types::TokenUsage {
                            prompt_tokens: 1,
                            completion_tokens: 1,
                        },
                        generation_id: None,
                        actual_cost_usd: None,
                        provider_id: Some("ollama-local".into()),
                        fleet_peer_id: None,
                        fleet_peer_model: None,
                    };
                    let _ = entry.result_tx.send(Ok(response));
                    return;
                }
            }
        })
    }

    #[tokio::test]
    async fn walker_cascades_through_market_fleet_openrouter_to_local_queue() {
        // Cascade-through-all-branches: route = [market, fleet,
        // openrouter(mockito 400), ollama-local(fake GPU loop)].
        //
        // Exact rev-2.1 slug reproduction (`no_offer_for_model`,
        // `no_fleet_peer`, `provider_rejected_model`) would require
        // mockito for Wire /quote + a configured fleet roster; that's
        // separate test infrastructure. This test exercises the cascade
        // end-to-end through all four branch types with the simpler
        // skip reasons that fall out of missing contexts — still catches
        // any future regression that reintroduces a pre-walker
        // short-circuit across the branch set.
        //
        // Expected chronicle ordering (by timestamp):
        //   1. market → network_route_unavailable(no_market_context)
        //   2. fleet → network_route_unavailable(fleet_ctx_missing)
        //   3. openrouter → network_route_skipped(provider_rejected_model)
        //   4. ollama-local → enqueued + walker_resolved
        use crate::pyramid::credentials::CredentialStore;
        use crate::pyramid::provider::{Provider, ProviderRegistry, ProviderType};
        use std::sync::Arc;

        // Mockito for the openrouter 400.
        let mut or_server = mockito::Server::new_async().await;
        let _or_mock = or_server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"message":"foo is not a valid model ID"}}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        // Credential store + provider registry.
        let cred_tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(cred_tmp.path()).unwrap());
        store.set("OR_KEY", "sk-or-test").unwrap();
        std::mem::forget(cred_tmp);

        let reg_conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&reg_conn).unwrap();
        let registry = Arc::new(ProviderRegistry::new(store));
        registry
            .save_provider(
                &reg_conn,
                Provider {
                    id: "openrouter".into(),
                    display_name: "OpenRouter (cascade test)".into(),
                    provider_type: ProviderType::Openrouter,
                    base_url: or_server.url(),
                    api_key_ref: Some("OR_KEY".into()),
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();
        registry
            .save_provider(
                &reg_conn,
                Provider {
                    id: "ollama-local".into(),
                    display_name: "Ollama (cascade test)".into(),
                    provider_type: ProviderType::OpenaiCompat,
                    // base_url is irrelevant — fake GPU loop never
                    // actually calls provider HTTP.
                    base_url: "http://127.0.0.1:1".into(),
                    api_key_ref: None,
                    auto_detect_context: false,
                    supports_broadcast: false,
                    broadcast_config_json: None,
                    config_json: "{}".into(),
                    enabled: true,
                },
            )
            .unwrap();

        // Build route with all four branch types.
        use crate::pyramid::dispatch_policy::*;
        let mut pool_configs = std::collections::BTreeMap::new();
        pool_configs.insert(
            "openrouter".into(),
            ProviderPoolConfig {
                concurrency: 1,
                rate_limit: None,
            },
        );
        pool_configs.insert(
            "ollama-local".into(),
            ProviderPoolConfig {
                concurrency: 1,
                rate_limit: None,
            },
        );
        let policy = Arc::new(DispatchPolicy {
            rules: vec![RoutingRule {
                name: "cascade_all_branches".into(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to: vec![
                    RouteEntry {
                        provider_id: "market".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    },
                    RouteEntry {
                        provider_id: "fleet".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    },
                    RouteEntry {
                        provider_id: "openrouter".into(),
                        model_id: Some("openai/gpt-4o-mini".into()),
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    },
                    RouteEntry {
                        provider_id: "ollama-local".into(),
                        model_id: Some("gemma4:26b".into()),
                        tier_name: None,
                        is_local: true,
                        max_budget_credits: None,
                    },
                ],
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        });
        let pools = Arc::new(
            crate::pyramid::provider_pools::ProviderPools::new(policy.as_ref()),
        );

        // Tempdir DB for chronicle observation.
        let db_file = temp_pyramid_db_with_slug("walker-cascade-test");
        let db_path: Arc<str> = db_file.path().to_string_lossy().to_string().into();
        let db_path_str = db_file.path().to_string_lossy().to_string();

        // Fake GPU loop.
        let queue = crate::compute_queue::ComputeQueueHandle::new();
        let _gpu_handle = spawn_fake_gpu_loop(queue.clone(), "cascade ok from fake gpu");

        let config = LlmConfig {
            api_key: "sk-or-test".into(),
            auth_token: String::new(),
            primary_model: "openai/gpt-4o-mini".into(),
            fallback_model_1: "openai/gpt-4o-mini".into(),
            fallback_model_2: "openai/gpt-4o-mini".into(),
            provider_registry: Some(registry.clone()),
            dispatch_policy: Some(policy),
            provider_pools: Some(pools),
            compute_queue: Some(queue),
            cache_access: Some(CacheAccess {
                slug: "walker-cascade-test".into(),
                build_id: "build-cascade".into(),
                db_path,
                bus: None,
                chain_name: None,
                content_type: None,
            }),
            max_retries: 1,
            retry_base_sleep_secs: 0,
            ..Default::default()
        };

        let result = call_model_unified_with_audit_and_ctx(
            &config,
            None,
            None,
            "sys",
            "usr",
            0.0,
            16,
            None,
            LlmCallOptions::default(),
        )
        .await;

        let resp = result.expect("walker must resolve via ollama-local after cascading through market+fleet+openrouter");
        assert!(
            resp.content.contains("cascade ok from fake gpu"),
            "expected fake GPU loop response content, got: {:?}",
            resp.content,
        );

        // Let the fire-and-forget spawn_blocking chronicle writes flush.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Verify chronicle trail. Order by timestamp (see
        // walker_market_runs_before_local_pool_enqueue for why id won't
        // do — spawn_blocking writes race on the SQLite lock).
        let rows = read_chronicle_trail(&db_path_str);

        let find = |want_event: &str, want_tag: &str| -> Option<usize> {
            rows.iter().position(|(_, ev, tag)| ev == want_event && tag == want_tag)
        };

        let market_i = find("network_route_unavailable", "market")
            .expect(&format!("market route_unavailable missing — trail={rows:?}"));
        let fleet_i = find("network_route_unavailable", "fleet")
            .expect(&format!("fleet route_unavailable missing — trail={rows:?}"));
        let openrouter_i = find("network_route_skipped", "openrouter")
            .expect(&format!("openrouter route_skipped missing — trail={rows:?}"));
        let resolved_i = find("walker_resolved", "ollama-local")
            .expect(&format!("walker_resolved for ollama-local missing — trail={rows:?}"));

        assert!(
            market_i < fleet_i && fleet_i < openrouter_i && openrouter_i < resolved_i,
            "expected cascade ordering market<fleet<openrouter<resolved; got \
             market={market_i}, fleet={fleet_i}, openrouter={openrouter_i}, \
             resolved={resolved_i}; trail={rows:?}",
        );
    }

    #[tokio::test]
    async fn walker_production_shape_outer_call_reaches_market_branch() {
        // Guard against future regressions re-introducing the pre-walker
        // short-circuit. This test asserts the OUTER walker — not a
        // queue-replay inner walker — reaches the market branch when
        // the route has both a market entry and a local pool entry.
        //
        // Complements the `prepare_for_replay` tests that already cover
        // the inner-walker case (replay clears compute_market_context +
        // skip_concurrency_gate: true, so market's `no_market_context`
        // emit is the expected inner behavior; outer behavior is
        // "market runs").

        let policy = walker_test_policy_with_local_pool(
            1,
            vec![("market", false), ("ollama-local", true)],
        );
        let (config, db) = walker_test_config_with_queue(policy);
        let db_path = db.path().to_string_lossy().to_string();

        let mut options = LlmCallOptions::default();
        options.dispatch_origin = DispatchOrigin::Local; // outer call

        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            call_model_unified_with_audit_and_ctx(
                &config, None, None, "sys", "usr", 0.0, 16, None, options,
            ),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let rows = read_chronicle_trail(&db_path);
        assert!(
            rows.iter()
                .any(|(_, ev, tag)| ev == "network_route_unavailable" && tag == "market"),
            "outer walker must reach market branch — trail={rows:?}",
        );
    }
}

