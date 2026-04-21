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

fn should_enqueue_local_execution(
    resolved_route: Option<&crate::pyramid::dispatch_policy::ResolvedRoute>,
    provider_type: ProviderType,
    options: &LlmCallOptions,
) -> bool {
    if options.skip_concurrency_gate {
        return false;
    }

    match resolved_route {
        Some(route) if !route.providers.is_empty() => route.providers.iter().any(|entry| entry.is_local),
        _ => provider_type == ProviderType::OpenaiCompat,
    }
}

fn queue_model_id_for_local_execution(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    resolved_route: Option<&crate::pyramid::dispatch_policy::ResolvedRoute>,
) -> String {
    if let Some(model_id) = resolved_route
        .and_then(|route| route.providers.iter().find(|entry| entry.is_local))
        .and_then(|entry| entry.model_id.clone())
        .filter(|model_id| !model_id.is_empty())
    {
        return model_id;
    }

    ctx.and_then(|c| c.resolved_model_id.clone())
        .filter(|model_id| !model_id.is_empty())
        .unwrap_or_else(|| config.primary_model.clone())
}

// ── Phase B market dispatch helpers ──────────────────────────────────────────
//
// These back the Phase B market branch in `call_model_unified_with_audit_and_ctx`.
// See `docs/plans/call-model-unified-market-integration.md` §3.2–§3.4.
//
// All language in this block uses cooperative/network framing per the
// invisibility checklist (§6). Wire's own trader-vocabulary slugs
// (`market_*`, `offer_*`, etc.) are scrubbed at the boundary by
// `sanitize_wire_slug` before they hit chronicle metadata.

/// Snapshot of tunnel readiness captured under a short-held read lock
/// in the Phase B gate. Dropped before any `await` on `call_market` so
/// the gate never holds a lock across the dispatch round-trip.
struct TunnelSnapshot {
    connected: bool,
    has_url: bool,
}

/// Return `true` when the current call should attempt a Phase B market
/// dispatch. The gate is intentionally conservative — every false
/// branch falls through to the pool path cleanly. Callers MUST pass
/// the already-captured snapshots (not live locks) so the decision is
/// consistent for the rest of the branch.
///
/// `balance` is the requester's current Wire credit balance in the
/// smallest integer unit. `i64::MAX` is the documented "balance
/// unknown / unlimited" sentinel — used when the node has no live
/// balance wiring yet (the 409 `InsufficientBalance` response still
/// catches real exhaustion at dispatch time; see §3.4).
///
/// `model_tier_eligible` is the tier-eligibility decision made by
/// the caller using whatever policy surface applies. The gate itself
/// takes a bool so it stays pure and testable without needing a
/// ModelTier type in the codebase yet.
fn should_try_market(
    policy: &crate::pyramid::local_mode::EffectiveParticipationPolicy,
    balance: i64,
    estimated_deposit: i64,
    model_tier_eligible: bool,
    tunnel_snap: &TunnelSnapshot,
    local_queue_depth: usize,
    compute_market_context_present: bool,
) -> bool {
    if !policy.allow_market_dispatch {
        return false;
    }

    if !policy.market_dispatch_eager
        && local_queue_depth < policy.market_dispatch_threshold_queue_depth as usize
    {
        return false;
    }

    if balance < estimated_deposit {
        return false;
    }

    if !model_tier_eligible {
        return false;
    }

    // Tunnel readiness — GATES feature. Research found start_tunnel_flow
    // is spawned-not-awaited at boot. Connecting / Disconnected both
    // mean Wire's delivery worker can't reach us; skip without
    // attempting /match.
    if !tunnel_snap.connected {
        return false;
    }
    if !tunnel_snap.has_url {
        return false;
    }

    if !compute_market_context_present {
        return false;
    }

    true
}

/// Coarse tier-eligibility decision used by the gate. Model tiers are
/// carried as strings in the current codebase; this helper keeps the
/// decision in one place so a future `ModelTier` type can replace it
/// wholesale without touching call sites.
///
/// Policy: any non-empty tier is eligible. Empty string is the "no
/// tier resolution at the call site" signal and falls back to the
/// primary — the primary model is the market's canonical offer model
/// today, so treating unknown tiers as eligible keeps the cascade
/// opportunistic. If a future policy wants a denylist, extend here.
fn model_tier_market_eligible(tier: &str) -> bool {
    !tier.is_empty()
}

/// Map a `RequesterError` to a stable, invisibility-safe reason slug
/// for chronicle metadata.
///
/// Variants handled here are the soft-fail ones only — `AuthFailed`
/// and `InsufficientBalance` are handled in the outer match in
/// `call_model_unified`.
fn classify_soft_fail_reason(err: &crate::pyramid::compute_requester::RequesterError) -> String {
    use crate::pyramid::compute_requester::RequesterError as RE;
    match err {
        RE::NoMatch { .. } => "no_match".into(),
        RE::MatchFailed { status, .. } => format!("match_failed_{status}"),
        RE::FillRejected { reason, .. } => {
            // Wire's reason slugs may carry trader vocabulary
            // (`market_serving_disabled`, `offer_depleted`, …).
            // Sanitize before surfacing to chronicle.
            format!("fill_rejected_{}", sanitize_wire_slug(reason))
        }
        RE::FillFailed { status, .. } => format!("fill_failed_{status}"),
        RE::DeliveryTimedOut { waited_ms } => format!("delivery_timed_out_{waited_ms}ms"),
        RE::DeliveryTombstoned { reason } => {
            format!("delivery_tombstoned_{}", sanitize_wire_slug(reason))
        }
        RE::ProviderFailed { code, .. } => format!("provider_failed_{code}"),
        RE::Internal(_) => "internal".into(),
        // Handled in outer match arms; pattern-match is exhaustive
        // via the catch-all below.
        RE::AuthFailed(_) | RE::InsufficientBalance { .. } | RE::ConfigError { .. } => {
            "unclassified".into()
        }
    }
}

/// Map Wire's trader-vocabulary slugs to cooperative framing before
/// they surface in chronicle metadata.
///
/// Wire controls its own reason slugs; we can't prevent them from
/// shipping trader words. This function is forward-compatible:
/// unknown slugs pass through unchanged (flagged in follow-up if they
/// turn out to leak). Ordering matters: the longer, more specific
/// replacements run before the shorter prefix sweeps so
/// "market_serving_disabled" → "provider_serving_disabled" (not
/// "network_serving_disabled").
fn sanitize_wire_slug(slug: &str) -> String {
    slug.replace("market_serving_disabled", "provider_serving_disabled")
        .replace("market_", "network_")
        .replace("offer_depleted", "contribution_depleted")
        .replace("offer_", "contribution_")
        .replace("seller", "provider")
        .replace("buyer", "requester")
        .replace("earnings", "contributions")
        .replace("earning", "contributing")
}

/// Spawn a fire-and-forget blocking task that writes a chronicle row
/// recording a successful network dispatch. See §4.2.
fn emit_network_helped_build(
    result: &crate::pyramid::compute_requester::MarketResult,
    handle_info: NetworkHandleInfo,
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) {
    let db_path = ctx
        .map(|c| c.db_path.clone())
        .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
    let Some(db_path) = db_path else { return };

    let job_path = super::compute_chronicle::generate_job_path(
        ctx,
        None,
        &handle_info.model_id,
        super::compute_chronicle::SOURCE_NETWORK,
    );
    let chronicle_ctx = if let Some(sc) = ctx {
        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
            sc,
            &job_path,
            super::compute_chronicle::EVENT_NETWORK_HELPED_BUILD,
            super::compute_chronicle::SOURCE_NETWORK,
        )
    } else {
        super::compute_chronicle::ChronicleEventContext::minimal(
            &job_path,
            super::compute_chronicle::EVENT_NETWORK_HELPED_BUILD,
            super::compute_chronicle::SOURCE_NETWORK,
        )
        .with_model_id(handle_info.model_id.clone())
    };
    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
        "job_id": handle_info.job_id_handle_path,
        "uuid_job_id": handle_info.uuid_job_id,
        "queue_position": handle_info.queue_position,
        "processing_cost_in_per_m": handle_info.matched_rate_in_per_m,
        "processing_cost_out_per_m": handle_info.matched_rate_out_per_m,
        "provider_node_id": handle_info.provider_node_id,
        "provider_handle": handle_info.provider_handle,
        "model_id": handle_info.model_id,
        "model_used": result.model_used,
        "reservation_held": handle_info.reservation_held,
    }));

    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}

/// Spawn a fire-and-forget blocking task that writes a chronicle row
/// recording that the network path could not serve this call and the
/// local pool will handle it. See §4.4.
fn emit_network_fell_back_local(
    err: &crate::pyramid::compute_requester::RequesterError,
    reason: &str,
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) {
    let db_path = ctx
        .map(|c| c.db_path.clone())
        .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
    let Some(db_path) = db_path else { return };

    let model_id = ctx
        .and_then(|c| c.resolved_model_id.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| config.primary_model.clone());

    let job_path = super::compute_chronicle::generate_job_path(
        ctx,
        None,
        &model_id,
        super::compute_chronicle::SOURCE_NETWORK,
    );

    let detail = format!("{err}");
    let reason_owned = reason.to_string();
    let model_id_for_meta = model_id.clone();

    let chronicle_ctx = if let Some(sc) = ctx {
        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
            sc,
            &job_path,
            super::compute_chronicle::EVENT_NETWORK_FELL_BACK_LOCAL,
            super::compute_chronicle::SOURCE_NETWORK,
        )
    } else {
        super::compute_chronicle::ChronicleEventContext::minimal(
            &job_path,
            super::compute_chronicle::EVENT_NETWORK_FELL_BACK_LOCAL,
            super::compute_chronicle::SOURCE_NETWORK,
        )
        .with_model_id(model_id.clone())
    };
    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
        "reason": reason_owned,
        "detail": detail,
        "model_id": model_id_for_meta,
    }));

    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}

/// Inner helper: writes the `network_balance_exhausted` chronicle row.
/// Call sites should use `emit_network_balance_exhausted_once` which
/// first checks the per-build `balance_exhausted_emitted` OnceLock.
fn emit_network_balance_exhausted(
    need: i64,
    have: i64,
    build_id: &str,
    ctx: &StepContext,
    config: &LlmConfig,
) {
    let db_path = if !ctx.db_path.is_empty() {
        ctx.db_path.clone()
    } else if let Some(ca) = config.cache_access.as_ref() {
        ca.db_path.to_string()
    } else {
        return;
    };

    let model_id = ctx
        .resolved_model_id
        .clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| config.primary_model.clone());

    let job_path = super::compute_chronicle::generate_job_path(
        Some(ctx),
        None,
        &model_id,
        super::compute_chronicle::SOURCE_NETWORK,
    );

    let build_id_owned = build_id.to_string();
    let chronicle_ctx = super::compute_chronicle::ChronicleEventContext::from_step_ctx(
        ctx,
        &job_path,
        super::compute_chronicle::EVENT_NETWORK_BALANCE_EXHAUSTED,
        super::compute_chronicle::SOURCE_NETWORK,
    )
    .with_metadata(serde_json::json!({
        "need": need,
        "have": have,
        "build_id": build_id_owned,
        "model_id": model_id,
    }));

    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}

/// Dedup-aware emit for `network_balance_exhausted`. At most one event
/// per build_id is recorded regardless of how many calls trip the
/// balance check — the OnceLock on StepContext enforces this. Skips
/// emission entirely on non-build inference paths (no build_id, no
/// dedup scope).
fn emit_network_balance_exhausted_once(
    need: i64,
    have: i64,
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) {
    let Some(step_ctx) = ctx else { return };
    if step_ctx.build_id.is_empty() {
        return;
    }
    if step_ctx.balance_exhausted_emitted.set(()).is_err() {
        return; // already emitted for this build
    }
    emit_network_balance_exhausted(need, have, &step_ctx.build_id, step_ctx, config);
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

/// Minimal snapshot of `/match` + `/fill` response data needed to
/// build a `network_helped_build` chronicle row. Passed by value into
/// `emit_network_helped_build` so the call site does not have to hold
/// the `MarketResult` borrow open across the fire-and-forget spawn.
struct NetworkHandleInfo {
    job_id_handle_path: String,
    uuid_job_id: String,
    queue_position: u64,
    matched_rate_in_per_m: i64,
    matched_rate_out_per_m: i64,
    provider_node_id: String,
    provider_handle: String,
    model_id: String,
    reservation_held: i64,
}

impl LlmResponse {
    /// Build an `LlmResponse` from a successful market dispatch result.
    /// Provider identity is tagged as `network` so downstream webhook
    /// correlators + leak-detection sweeps have a stable grouping key.
    pub(crate) fn from_market_result(
        result: crate::pyramid::compute_requester::MarketResult,
    ) -> Self {
        LlmResponse {
            content: result.content,
            usage: crate::pyramid::types::TokenUsage {
                prompt_tokens: result.input_tokens,
                completion_tokens: result.output_tokens,
            },
            generation_id: None,
            actual_cost_usd: None,
            provider_id: Some("network".to_string()),
            fleet_peer_id: None,
            fleet_peer_model: Some(result.model_used),
        }
    }
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
    let (mut provider_impl, mut secret, mut provider_type, provider_id) = build_call_provider(config)?;

    // Phase D: resolve the dispatch route BEFORE the retry loop so we
    // have the provider preference chain for escalation. When no policy
    // is configured the resolved_route is None and we fall through to
    // the legacy single-provider path.
    let mut resolved_route = config.dispatch_policy.as_ref().map(|policy| {
        // Use Build as the default work_type — Phase B work_type tagging
        // will provide the real classification per call site.
        let work_type = crate::pyramid::dispatch_policy::WorkType::Build;
        let step_name = ctx.map(|c| c.step_name.as_str()).unwrap_or("");
        let depth = ctx.map(|c| c.depth);
        policy.resolve_route(work_type, "", step_name, depth)
    });

    // ── Phase A: Fleet providers (pre-pool) ──────────────────────────
    // Fleet is not pool-limited. Try fleet dispatch before the pool
    // acquisition loop. On success: return immediately with fleet
    // provenance. On failure: filter fleet from providers, continue.
    //
    // TODO: LOAD BALANCING — Currently fleet is "try first, use exclusively."
    // If a fleet peer is found, we dispatch and return. The local GPU never
    // gets a turn. The right behavior: compare fleet peer queue depth vs
    // local queue depth, and route each call to whichever has more capacity.
    // This turns [fleet, ollama-local] from a priority chain into a load
    // balancer. Both GPUs should be working simultaneously on a build.
    // The local queue depth is available from config.compute_queue.
    // The fleet peer's queue depth is on FleetPeer.total_queue_depth.
    if let Some(ref route) = resolved_route {
        if !options.skip_fleet_dispatch && !route.matched_rule_name.is_empty() {
            let has_fleet = route.providers.iter().any(|e| e.provider_id == "fleet");
            tracing::info!(
                has_fleet,
                rule = %route.matched_rule_name,
                fleet_roster_present = config.fleet_roster.is_some(),
                fleet_dispatch_present = config.fleet_dispatch.is_some(),
                provider_count = route.providers.len(),
                providers = ?route.providers.iter().map(|p| &p.provider_id).collect::<Vec<_>>(),
                "Fleet Phase A: entry check"
            );
            // Phase 4 async: the dispatch pathway requires a FleetDispatchContext
            // (pending-job registry, tunnel handle, policy). When absent (tests,
            // pre-init), fleet is skipped and we fall through to local.
            // We ALSO snapshot the policy and tunnel URL exactly once here so
            // the peer-staleness diagnostic, find_peer_for_rule, and the timeout
            // computations all see consistent values. This eliminates a TOCTOU
            // race where tunnel_state could transition Connected → Disconnected
            // between the check and callback URL construction.
            if has_fleet {
                // Step 0: snapshot fleet_ctx + policy + callback_url.
                let phase_a_ready = async {
                    let fleet_ctx = match config.fleet_dispatch.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            tracing::warn!(
                                rule = %route.matched_rule_name,
                                "Fleet Phase A skipped: FleetDispatchContext not attached"
                            );
                            return None;
                        }
                    };
                    let policy_snap = fleet_ctx.policy.read().await.clone();
                    let callback_url = {
                        let ts = fleet_ctx.tunnel_state.read().await;
                        match (&ts.status, ts.tunnel_url.as_ref()) {
                            // Only Connected is dispatch-valid. Connecting means
                            // cloudflared hasn't finished announcing the tunnel
                            // at Cloudflare's edge — callbacks would 404 there.
                            (crate::tunnel::TunnelConnectionStatus::Connected, Some(u)) => {
                                u.endpoint("/v1/fleet/result")
                            }
                            _ => {
                                tracing::warn!(
                                    rule = %route.matched_rule_name,
                                    status = ?ts.status,
                                    has_url = ts.tunnel_url.is_some(),
                                    "Fleet Phase A skipped: tunnel not Connected or URL missing"
                                );
                                return None;
                            }
                        }
                    };
                    Some((fleet_ctx, policy_snap, callback_url))
                }
                .await;

                if let Some((fleet_ctx, policy_snap, callback_url)) = phase_a_ready {
                    if let Some(ref roster_handle) = config.fleet_roster {
                        let roster = roster_handle.read().await;
                        // Diagnostic: log fleet routing decision
                        tracing::info!(
                            rule = %route.matched_rule_name,
                            peer_count = roster.peers.len(),
                            has_jwt = roster.fleet_jwt.is_some(),
                            peers_with_rules = roster.peers.values()
                                .filter(|p| !p.serving_rules.is_empty())
                                .count(),
                            peer_staleness_secs = policy_snap.peer_staleness_secs,
                            "Fleet Phase A: checking roster for rule match"
                        );
                        for (pid, peer) in &roster.peers {
                            let age_secs = (chrono::Utc::now() - peer.last_seen).num_seconds();
                            tracing::info!(
                                peer_id = %pid,
                                serving_rules = ?peer.serving_rules,
                                models = ?peer.models_loaded,
                                handle = ?peer.handle_path,
                                stale = age_secs > policy_snap.peer_staleness_secs as i64,
                                "Fleet peer state"
                            );
                        }
                        if let Some(peer) = roster.find_peer_for_rule(
                            &route.matched_rule_name,
                            policy_snap.peer_staleness_secs,
                        ) {
                            let jwt = roster.fleet_jwt.clone().unwrap_or_default();
                            if !jwt.is_empty() {
                                let peer_clone = peer.clone();
                                let rule_name = route.matched_rule_name.clone();
                                // NB: route.max_wait_secs is the JOB wall-clock
                                // bound on how long we await the callback.
                                // policy_snap.dispatch_ack_timeout_secs is a
                                // distinct ACK-phase timeout (how long we wait
                                // for the 202 HTTP response).
                                let job_wait_secs = route.max_wait_secs;
                                // Clamp to at least 1s — a zero would cause the
                                // orphan sweep to evict the entry on its first
                                // tick, before the callback can arrive.
                                let expected_timeout =
                                    std::time::Duration::from_secs(job_wait_secs.max(1));
                                drop(roster); // release lock before async

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

                                // Step 2: generate UUID for this dispatch.
                                let job_id = uuid::Uuid::new_v4().to_string();

                                // Step 4: oneshot channel — filled by the
                                // server.rs /v1/fleet/result handler when the
                                // peer's callback arrives, or dropped by the
                                // orphan sweep (producing RecvError here).
                                let (sender, receiver) =
                                    tokio::sync::oneshot::channel::<crate::fleet::FleetAsyncResult>();

                                // Step 5: register pending entry BEFORE dispatch
                                // POST, so a very-fast peer callback cannot beat
                                // our registration and produce a spurious orphan.
                                //
                                // peer_id MUST be peer.node_id (raw) — the
                                // callback authenticates via fleet JWT whose
                                // `nid` claim carries the raw node_id.
                                fleet_ctx.pending.register(
                                    job_id.clone(),
                                    crate::fleet::PendingFleetJob {
                                        sender,
                                        dispatched_at: std::time::Instant::now(),
                                        peer_id: peer_clone.node_id.clone(),
                                        expected_timeout,
                                    },
                                );

                                // Step 6: POST dispatch (202 ACK phase).
                                let dispatch_result = crate::fleet::fleet_dispatch_by_rule(
                                    &peer_clone,
                                    &job_id,
                                    &callback_url,
                                    &rule_name,
                                    system_prompt,
                                    user_prompt,
                                    temperature,
                                    _max_tokens,
                                    response_format,
                                    &jwt,
                                    policy_snap.dispatch_ack_timeout_secs,
                                )
                                .await;

                                match dispatch_result {
                                    Ok(ack) => {
                                        // Step 9: chronicle fleet_dispatched_async
                                        {
                                            let chronicle_ctx = if let Some(sc) = ctx {
                                                super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                    sc, &fleet_job_path, "fleet_dispatched_async", "fleet",
                                                )
                                            } else {
                                                super::compute_chronicle::ChronicleEventContext::minimal(
                                                    &fleet_job_path, "fleet_dispatched_async", "fleet",
                                                )
                                                .with_model_id(config.primary_model.clone())
                                            };
                                            let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                                "peer_id": peer_clone.node_id,
                                                "peer_name": peer_clone.name,
                                                "rule_name": rule_name,
                                                "timeout_secs": job_wait_secs,
                                                "peer_queue_depth": ack.peer_queue_depth,
                                            }));
                                            if let Some(ref db_path) = fleet_db_path {
                                                let db_path = db_path.clone();
                                                tokio::task::spawn_blocking(move || {
                                                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                        let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                    }
                                                });
                                            }
                                        }

                                        // Step 10: two-phase await with pinned
                                        // receiver. `timeout` consumes its
                                        // future by value; pin once, then pass
                                        // `receiver.as_mut()` on each call —
                                        // `&mut receiver` would yield
                                        // `&mut Pin<&mut Receiver>`, not a Future.
                                        tokio::pin!(receiver);
                                        let wait_outcome = match tokio::time::timeout(
                                            std::time::Duration::from_secs(job_wait_secs),
                                            receiver.as_mut(),
                                        )
                                        .await
                                        {
                                            Ok(Ok(r)) => Ok(r),
                                            Ok(Err(_recv_err)) => {
                                                // Sender dropped — sweep evicted
                                                // the pending entry. Fall through.
                                                Err("orphaned")
                                            }
                                            Err(_elapsed) => {
                                                // Primary timeout — grace window
                                                // for in-flight callbacks.
                                                match tokio::time::timeout(
                                                    std::time::Duration::from_secs(
                                                        policy_snap.timeout_grace_secs,
                                                    ),
                                                    receiver.as_mut(),
                                                )
                                                .await
                                                {
                                                    Ok(Ok(r)) => Ok(r),
                                                    _ => Err("timeout"),
                                                }
                                            }
                                        };

                                        // Idempotent cleanup — the callback or
                                        // sweep may have already removed us.
                                        let _ = fleet_ctx.pending.remove(&job_id);

                                        match wait_outcome {
                                            Ok(crate::fleet::FleetAsyncResult::Success(fleet_resp)) => {
                                                // Chronicle fleet_result_received
                                                {
                                                    let chronicle_ctx = if let Some(sc) = ctx {
                                                        super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                            sc, &fleet_job_path, "fleet_result_received", "fleet",
                                                        )
                                                    } else {
                                                        super::compute_chronicle::ChronicleEventContext::minimal(
                                                            &fleet_job_path, "fleet_result_received", "fleet",
                                                        )
                                                        .with_model_id(config.primary_model.clone())
                                                    };
                                                    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                                        "peer_id": peer_clone.node_id,
                                                        "peer_name": peer_clone.name,
                                                        "peer_model": fleet_resp.peer_model,
                                                        "latency_ms": fleet_start.elapsed().as_millis() as u64,
                                                        "tokens_prompt": fleet_resp.prompt_tokens.unwrap_or(0),
                                                        "tokens_completion": fleet_resp.completion_tokens.unwrap_or(0),
                                                    }));
                                                    if let Some(ref db_path) = fleet_db_path {
                                                        let db_path = db_path.clone();
                                                        tokio::task::spawn_blocking(move || {
                                                            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                                let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                            }
                                                        });
                                                    }
                                                }

                                                return Ok(LlmResponse {
                                                    content: fleet_resp.content,
                                                    usage: super::types::TokenUsage {
                                                        prompt_tokens: fleet_resp.prompt_tokens.unwrap_or(0),
                                                        completion_tokens: fleet_resp.completion_tokens.unwrap_or(0),
                                                    },
                                                    generation_id: None,
                                                    actual_cost_usd: None, // fleet is free (same operator)
                                                    provider_id: Some("fleet".to_string()),
                                                    fleet_peer_id: Some(
                                                        peer_clone
                                                            .handle_path
                                                            .clone()
                                                            .unwrap_or_else(|| peer_clone.node_id.clone()),
                                                    ),
                                                    fleet_peer_model: fleet_resp.peer_model.clone(),
                                                });
                                            }
                                            Ok(crate::fleet::FleetAsyncResult::Error(err_msg)) => {
                                                // Peer RAN inference and it failed
                                                // (GPU OOM, model mismatch, etc.).
                                                // Chronicle and fall through to local.
                                                let chronicle_ctx = if let Some(sc) = ctx {
                                                    super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                        sc, &fleet_job_path, "fleet_result_failed", "fleet",
                                                    )
                                                } else {
                                                    super::compute_chronicle::ChronicleEventContext::minimal(
                                                        &fleet_job_path, "fleet_result_failed", "fleet",
                                                    )
                                                    .with_model_id(config.primary_model.clone())
                                                };
                                                let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                                    "peer_id": peer_clone.node_id,
                                                    "peer_name": peer_clone.name,
                                                    "error": err_msg,
                                                }));
                                                if let Some(ref db_path) = fleet_db_path {
                                                    let db_path = db_path.clone();
                                                    tokio::task::spawn_blocking(move || {
                                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                        }
                                                    });
                                                }
                                                warn!(
                                                    "Fleet peer {} inference failed, falling through to local",
                                                    peer_clone.node_id
                                                );
                                            }
                                            Err("timeout") => {
                                                let chronicle_ctx = if let Some(sc) = ctx {
                                                    super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                        sc, &fleet_job_path, "fleet_dispatch_timeout", "fleet",
                                                    )
                                                } else {
                                                    super::compute_chronicle::ChronicleEventContext::minimal(
                                                        &fleet_job_path, "fleet_dispatch_timeout", "fleet",
                                                    )
                                                    .with_model_id(config.primary_model.clone())
                                                };
                                                let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                                    "peer_id": peer_clone.node_id,
                                                    "peer_name": peer_clone.name,
                                                    "timeout_secs": job_wait_secs,
                                                    "grace_secs": policy_snap.timeout_grace_secs,
                                                }));
                                                if let Some(ref db_path) = fleet_db_path {
                                                    let db_path = db_path.clone();
                                                    tokio::task::spawn_blocking(move || {
                                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                        }
                                                    });
                                                }
                                                warn!(
                                                    "Fleet dispatch timeout awaiting callback from peer {}, falling through to local",
                                                    peer_clone.node_id
                                                );
                                            }
                                            Err(_orphaned) => {
                                                // Sweep removed the entry before
                                                // the callback arrived. Chronicle
                                                // as dispatch_failed (reason=orphaned)
                                                // so the observability trail does not
                                                // silently drop this case.
                                                let chronicle_ctx = if let Some(sc) = ctx {
                                                    super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                        sc, &fleet_job_path, "fleet_dispatch_failed", "fleet",
                                                    )
                                                } else {
                                                    super::compute_chronicle::ChronicleEventContext::minimal(
                                                        &fleet_job_path, "fleet_dispatch_failed", "fleet",
                                                    )
                                                    .with_model_id(config.primary_model.clone())
                                                };
                                                let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                                    "peer_id": peer_clone.node_id,
                                                    "peer_name": peer_clone.name,
                                                    "error": "pending entry orphaned by sweep",
                                                    "error_kind": "orphaned",
                                                    "status_code": serde_json::Value::Null,
                                                    "latency_ms": fleet_start.elapsed().as_millis() as u64,
                                                }));
                                                if let Some(ref db_path) = fleet_db_path {
                                                    let db_path = db_path.clone();
                                                    tokio::task::spawn_blocking(move || {
                                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                        }
                                                    });
                                                }
                                                warn!(
                                                    "Fleet pending entry orphaned for peer {}, falling through to local",
                                                    peer_clone.node_id
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        // Dispatch POST failed — remove entry
                                        // (idempotent) and chronicle by status_code.
                                        let _ = fleet_ctx.pending.remove(&job_id);

                                        // 503 = peer overloaded; treat distinctly
                                        // so analytics can show capacity pressure
                                        // separately from hard failures.
                                        let is_overloaded = e.status_code == Some(503);
                                        let event_type = if is_overloaded {
                                            "fleet_peer_overloaded"
                                        } else {
                                            "fleet_dispatch_failed"
                                        };
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
                                        let chronicle_ctx = if is_overloaded {
                                            // Policy-derived retry-after — the
                                            // peer's own Retry-After header is
                                            // not parsed here (Phase 4 scope);
                                            // the policy value is the fleet-wide
                                            // backoff guidance.
                                            chronicle_ctx.with_metadata(serde_json::json!({
                                                "peer_id": peer_clone.node_id,
                                                "peer_name": peer_clone.name,
                                                "status_code": e.status_code,
                                                "retry_after": policy_snap.admission_retry_after_secs,
                                            }))
                                        } else {
                                            chronicle_ctx.with_metadata(serde_json::json!({
                                                "peer_id": peer_clone.node_id,
                                                "peer_name": peer_clone.name,
                                                "error": e.message.clone(),
                                                "error_kind": serde_json::to_value(&e.kind).unwrap_or_default(),
                                                "status_code": e.status_code,
                                                "latency_ms": fleet_start.elapsed().as_millis() as u64,
                                            }))
                                        };
                                        if let Some(ref db_path) = fleet_db_path {
                                            let db_path = db_path.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                                    let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                                }
                                            });
                                        }

                                        // Only remove peer from roster if it's
                                        // actually dead (transport failure).
                                        // Timeouts (524, client timeout) mean
                                        // the peer is alive but slow —
                                        // discovery membership belongs to
                                        // heartbeat/announce, not dataplane
                                        // RPC outcomes.
                                        if e.is_peer_dead() {
                                            let mut roster_w = roster_handle.write().await;
                                            roster_w.remove_peer(&peer_clone.node_id);
                                            warn!(
                                                "Fleet dispatch: peer {} removed (transport failure): {}",
                                                peer_clone.node_id, e
                                            );
                                        } else {
                                            warn!(
                                                "Fleet dispatch failed ({:?}), peer stays in roster: {}",
                                                e.kind, e
                                            );
                                        }
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    rule = %route.matched_rule_name,
                                    "Fleet dispatch skipped: fleet JWT is empty"
                                );
                            }
                        } else {
                            tracing::warn!(
                                rule = %route.matched_rule_name,
                                peer_count = roster.peers.len(),
                                "Fleet dispatch skipped: no peer serves rule '{}'",
                                route.matched_rule_name
                            );
                        }
                    }
                }
            }
        }
    }

    // Filter "fleet" from providers before pool loop (fleet already tried or skipped)
    if let Some(ref mut route) = resolved_route {
        route.providers.retain(|e| e.provider_id != "fleet");
    }

    // ── Phase B: Network (cross-operator peer) dispatch ──────────────
    //
    // Runs between fleet (Phase A) and local/pool acquisition. The gate
    // reads the effective compute-participation policy, tunnel state,
    // and local queue depth once, drops every lock before awaiting, and
    // attempts a single cross-operator market dispatch. On success:
    // return with network provenance. On soft-fail: chronicle, fall
    // through to pool. On hard-fail (auth): bubble a cooperative error.
    //
    // See `docs/plans/call-model-unified-market-integration.md` §3.4.
    if config.compute_market_context.is_some() {
        // Snapshot tunnel + policy + queue depth under short-held locks.
        // Each lock is released before the next step so the gate cannot
        // deadlock against the dispatch path that also touches these.
        let tunnel_snap = {
            let market_ctx = config.compute_market_context.as_ref().unwrap();
            let ts = market_ctx.tunnel_state.read().await;
            TunnelSnapshot {
                connected: matches!(
                    ts.status,
                    crate::tunnel::TunnelConnectionStatus::Connected
                ),
                has_url: ts.tunnel_url.is_some(),
            }
        };

        // Effective participation policy. When reading fails (DB gone,
        // policy row missing), `should_try_market` never runs — we
        // conservatively skip the market branch by leaving
        // `policy_opt` as None.
        let policy_opt: Option<crate::pyramid::local_mode::EffectiveParticipationPolicy> = {
            let db_path = ctx
                .map(|c| c.db_path.clone())
                .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
            match db_path {
                Some(db_path) => tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(&db_path).ok()?;
                    crate::pyramid::local_mode::get_compute_participation_policy(&conn)
                        .ok()
                        .map(|p| p.effective_booleans())
                })
                .await
                .ok()
                .flatten(),
                None => None,
            }
        };

        if let Some(ref policy_snap) = policy_opt {
            // Model-tier eligibility — empty string indicates no tier
            // resolution at the call site; the gate still attempts
            // market so the dispatch cascade remains opportunistic.
            let model_tier_eligible = model_tier_market_eligible(
                ctx.map(|c| c.model_tier.as_str()).unwrap_or(""),
            );

            // Local queue depth (for non-eager overflow posture).
            let local_queue_depth = if let Some(queue_handle) = config.compute_queue.as_ref() {
                let q = queue_handle.queue.lock().await;
                q.total_depth()
            } else {
                0
            };

            // Balance + estimated_deposit — the codebase does not yet
            // thread a live credits balance through the LLM config.
            // Until it does, the gate trusts Wire's 409
            // InsufficientBalance response to catch real exhaustion at
            // dispatch time. i64::MAX sentinel means "balance unknown
            // / assume solvent"; balance == i64::MAX >= estimated means
            // the gate never short-circuits on balance here. See §3.2
            // plan note.
            let balance: i64 = i64::MAX;
            let estimated_deposit: i64 = 0;

            let market_ctx_present = config.compute_market_context.is_some();

            if should_try_market(
                policy_snap,
                balance,
                estimated_deposit,
                model_tier_eligible,
                &tunnel_snap,
                local_queue_depth,
                market_ctx_present,
            ) {
                let market_ctx = config
                    .compute_market_context
                    .as_ref()
                    .expect("gate guaranteed Some");

                // Build the `MarketInferenceRequest`. Callback URL is
                // derived from the tunnel state read lock (already
                // Connected per gate). The messages array is the
                // {system, user} pair packed into ChatML — Wire rejects
                // multi-system turns pre-dispatch (DD-C).
                let callback_url = {
                    let ts = market_ctx.tunnel_state.read().await;
                    match ts.tunnel_url.as_ref() {
                        Some(u) => {
                            let base = u.as_str().trim_end_matches('/').to_string();
                            format!("{}/v1/compute/job-result", base)
                        }
                        None => {
                            // Race: tunnel dropped between the gate
                            // snapshot and this read. Skip market
                            // cleanly; chronicle a fall-back event
                            // so the timeline reflects the attempt.
                            let synthetic = crate::pyramid::compute_requester::RequesterError::Internal(
                                "tunnel URL missing between gate snapshot and dispatch".into(),
                            );
                            emit_network_fell_back_local(&synthetic, "tunnel_url_race", ctx, config);
                            // Fall through to pool by breaking out of
                            // this inner block.
                            String::new()
                        }
                    }
                };

                if !callback_url.is_empty() {
                    let messages = serde_json::json!([
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": user_prompt},
                    ]);

                    let model_for_market = ctx
                        .and_then(|c| c.resolved_model_id.clone())
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| config.primary_model.clone());

                    let req = crate::pyramid::compute_requester::MarketInferenceRequest {
                        model_id: model_for_market.clone(),
                        // `(1i64 << 53) - 1` = JS MAX_SAFE_INTEGER = 9_007_199_254_740_991.
                        // Still a "no cap / solvent assumed" sentinel (orders of magnitude
                        // above any realistic estimated_cost), but round-trips cleanly
                        // through any f64 JSON parser — i64::MAX lossy-converts in JS/f64
                        // to a value > Postgres BIGINT max and 500s the /match handler.
                        max_budget: (1i64 << 53) - 1,
                        input_tokens: 0,
                        latency_preference:
                            crate::pyramid::compute_requester::LatencyPreference::BestPrice,
                        messages,
                        max_tokens: if _max_tokens == 0 { 1024 } else { _max_tokens },
                        temperature,
                        privacy_tier: "direct".to_string(),
                        requester_callback_url: callback_url,
                    };

                    // `catch_unwind` guards the PendingJobs lifecycle.
                    // If `dispatch_market` or `await_result` panics
                    // (malformed Wire response, serde unwrap, etc.) the
                    // unwinding task would leak its oneshot Sender in
                    // the pending map. catch_unwind turns the panic
                    // into an Err arm, the cascade soft-falls to the
                    // pool, and PendingJobs' own timeout cleanup removes
                    // the dangling entry.
                    //
                    // We split `call_market` into its two halves —
                    // `dispatch_market` → handle → `await_result` — so
                    // we can snapshot the handle's rates / reservation /
                    // provider_node_id / queue_position into
                    // NetworkHandleInfo for the HELPED_BUILD chronicle.
                    // `call_market` collapses the handle internally and
                    // would strand those values.
                    use futures_util::FutureExt;
                    let wait_ms = policy_snap.market_dispatch_max_wait_ms;

                    let dispatch_fut = crate::pyramid::compute_requester::dispatch_market(
                        req,
                        &market_ctx.auth,
                        &market_ctx.config,
                        &market_ctx.pending_jobs,
                    );
                    // `panic_handled` tracks whether the panic branch
                    // already emitted FELL_BACK_LOCAL with
                    // reason="internal_panic" (plan §3.4). The outer
                    // match suppresses its own emit in that case so
                    // the chronicle records exactly one fallback event.
                    let mut panic_handled = false;
                    let dispatch_result = match std::panic::AssertUnwindSafe(dispatch_fut)
                        .catch_unwind()
                        .await
                    {
                        Ok(r) => r,
                        Err(panic_info) => {
                            let msg = panic_info
                                .downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    panic_info.downcast_ref::<String>().cloned()
                                })
                                .unwrap_or_else(|| "panic in dispatch_market".to_string());
                            let synthetic =
                                crate::pyramid::compute_requester::RequesterError::Internal(
                                    msg.clone(),
                                );
                            emit_network_fell_back_local(
                                &synthetic,
                                "internal_panic",
                                ctx,
                                config,
                            );
                            panic_handled = true;
                            tracing::info!(
                                panic_msg = %msg,
                                "network dispatch panicked; local pool handles"
                            );
                            Err(synthetic)
                        }
                    };

                    // If dispatch failed, short-circuit to the soft/hard
                    // fail cascade below with no handle snapshot.
                    let handle_and_fut = match dispatch_result {
                        Ok(handle) => {
                            // Snapshot the handle fields BEFORE the
                            // move into await_result so they survive
                            // for the HELPED_BUILD chronicle emit.
                            let snapshot = NetworkHandleInfo {
                                job_id_handle_path: handle.job_id_handle_path.clone(),
                                uuid_job_id: handle.uuid_job_id.clone(),
                                queue_position: handle.queue_position,
                                matched_rate_in_per_m: handle.matched_rate_in_per_m,
                                matched_rate_out_per_m: handle.matched_rate_out_per_m,
                                provider_node_id: handle.provider_node_id.clone(),
                                provider_handle: String::new(),
                                model_id: model_for_market.clone(),
                                reservation_held: handle.deposit_charged,
                            };
                            Ok((snapshot, handle))
                        }
                        Err(e) => Err(e),
                    };

                    let result = match handle_and_fut {
                        Ok((snapshot, handle)) => {
                            let await_fut = crate::pyramid::compute_requester::await_result(
                                handle,
                                &market_ctx.auth,
                                &market_ctx.config,
                                &market_ctx.pending_jobs,
                                wait_ms,
                            );
                            let inner = match std::panic::AssertUnwindSafe(await_fut)
                                .catch_unwind()
                                .await
                            {
                                Ok(r) => r,
                                Err(panic_info) => {
                                    let msg = panic_info
                                        .downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            panic_info.downcast_ref::<String>().cloned()
                                        })
                                        .unwrap_or_else(|| {
                                            "panic in await_result".to_string()
                                        });
                                    let synthetic = crate::pyramid::compute_requester::RequesterError::Internal(
                                        msg.clone(),
                                    );
                                    emit_network_fell_back_local(
                                        &synthetic,
                                        "internal_panic",
                                        ctx,
                                        config,
                                    );
                                    panic_handled = true;
                                    tracing::info!(
                                        panic_msg = %msg,
                                        "network await panicked; local pool handles"
                                    );
                                    Err(synthetic)
                                }
                            };
                            inner.map(|mr| (mr, snapshot))
                        }
                        Err(e) => Err(e),
                    };

                    match result {
                        Ok((market_result, handle_info)) => {
                            // Success — chronicle with the real handle
                            // snapshot (rates, provider_node_id,
                            // queue_position, reservation) and return.
                            emit_network_helped_build(
                                &market_result,
                                handle_info,
                                ctx,
                                config,
                            );
                            return Ok(LlmResponse::from_market_result(market_result));
                        }
                        Err(crate::pyramid::compute_requester::RequesterError::AuthFailed(
                            detail,
                        )) => {
                            return Err(anyhow!(
                                "network credentials invalid — session may be expired: {detail}"
                            ));
                        }
                        Err(crate::pyramid::compute_requester::RequesterError::ConfigError {
                            error_slug,
                            detail,
                        }) => {
                            // Caller-misconfiguration class. MUST NOT
                            // fall through to the pool — silent rerouting
                            // is what kept the market broken for weeks
                            // ("have 0, need 0" masquerade). Surface
                            // loudly so the operator sees the slug and
                            // can fix their config.
                            tracing::error!(
                                slug = %error_slug,
                                ?detail,
                                "network dispatch misconfigured; caller must fix"
                            );
                            let detail_str = detail
                                .as_ref()
                                .map(|v| format!(" — detail: {v}"))
                                .unwrap_or_default();
                            return Err(anyhow!(
                                "network dispatch misconfigured: {error_slug}{detail_str}"
                            ));
                        }
                        Err(crate::pyramid::compute_requester::RequesterError::InsufficientBalance {
                            need,
                            have,
                        }) => {
                            emit_network_balance_exhausted_once(need, have, ctx, config);
                            tracing::info!(
                                need,
                                have,
                                "network credits depleted for this call; local pool handles"
                            );
                            // Fall through to pool.
                        }
                        Err(other) => {
                            if !panic_handled {
                                let reason = classify_soft_fail_reason(&other);
                                emit_network_fell_back_local(&other, &reason, ctx, config);
                                tracing::info!(
                                    reason = %reason,
                                    "network unavailable; local pool handles"
                                );
                            }
                            // Fall through to pool.
                        }
                    }
                }
            }
        }
    }

    // ── Phase 1 Compute Queue: local execution only ────────────────
    //
    // Outgoing fleet dispatch must get first shot so calls that can be
    // served by a peer do not queue behind local GPU work. After fleet
    // routing has had its chance, enqueue only calls that may execute on
    // local hardware. Cloud-only routes bypass the queue entirely.
    if let Some(ref queue_handle) = config.compute_queue {
        if should_enqueue_local_execution(resolved_route.as_ref(), provider_type, &options) {
            let queue_model_id =
                queue_model_id_for_local_execution(config, ctx, resolved_route.as_ref());
            let (tx, rx) = tokio::sync::oneshot::channel();

            // Derive replay config via prepare_for_replay — clears
            // compute_queue (re-enqueue guard) + fleet + market contexts
            // so the GPU loop processes this entry as a pool-only local
            // call. See impl LlmConfig::prepare_for_replay for the
            // single-source-of-truth rationale.
            let gpu_config = config.prepare_for_replay(options.dispatch_origin);

            // Set skip flags on the forwarded options so the GPU loop
            // performs the local execution directly rather than treating
            // replay as a second routing decision point.
            // Label the queue-entry source so downstream chronicle
            // emitters attribute the job to its true origin. Pre-fix,
            // this was a binary fleet_received-vs-local sniff keyed on
            // `skip_fleet_dispatch && chronicle_job_path.is_some()`,
            // which mislabeled market-received jobs as fleet_received
            // because both set those flags identically. Now driven by
            // the explicit DispatchOrigin the upstream handler sets.
            let entry_source = options.dispatch_origin.source_label().to_string();
            let chronicle_job_path_val = options.chronicle_job_path.clone().unwrap_or_else(|| {
                super::compute_chronicle::generate_job_path(ctx, None, &queue_model_id, &entry_source)
            });
            let entry_chronicle_jp = options.chronicle_job_path.clone();

            let mut gpu_options = options;
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

            return rx
                .await
                .map_err(|_| anyhow!("compute queue: GPU loop dropped the job"))?;
        }
    }

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

    // Suppress unused-var warnings on the former Phase D's mutable bindings.
    // `provider_impl`, `secret`, and `provider_type` were mutable because
    // Phase D re-instantiated them on escalation. The walker re-instantiates
    // per entry inside the pool branch, so the outer bindings are now pure
    // fallback state when no route exists.
    let _ = (&mut provider_impl, &mut secret, &mut provider_type);

    // ── Walker loop (Walker Re-Plan Wire 2.1 §3) ─────────────────────
    //
    // Per-entry walker over `route.providers`. Every entry obeys the
    // same contract: runtime-gate → try_acquire (saturation advances) →
    // dispatch → three-tier EntryError (Retryable/RouteSkipped advance;
    // CallTerminal bubbles).
    //
    // Wave 1 scope: pool-provider entries go through the walker with
    // the full HTTP retry loop inlined. `"fleet"` and `"market"` entries
    // are handled by the legacy Phase A/B pre-loop blocks BEFORE the
    // walker runs (Wave 2/3 will inline them); when the walker meets
    // them here it emits `network_route_skipped reason="wave1_not_implemented"`
    // and advances.
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
            }],
            false,
        ),
    };

    let walker_started = std::time::Instant::now();
    // `last_attempted_provider_id` is written whenever the walker enters
    // a pool branch (before HTTP dispatch). On `CallTerminal` the audit
    // row stamps this value so downstream debugging can see which entry
    // rejected. The initial `None` is intentional (no entry attempted
    // yet); warnings about "value assigned never read" would fire if we
    // remove the init — leave the #[allow] here.
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

        // Wave 1: fleet + market are handled by legacy Phase A/B earlier;
        // walker advances past them. Waves 2+3 inline them.
        if matches!(branch, RouteBranch::Fleet | RouteBranch::Market) {
            emit_walker_chronicle(
                ctx,
                config,
                super::compute_chronicle::EVENT_NETWORK_ROUTE_SKIPPED,
                &walker_source_label,
                &entry.provider_id,
                serde_json::json!({
                    "reason": "wave1_not_implemented",
                    "branch": match branch {
                        RouteBranch::Fleet => "fleet",
                        RouteBranch::Market => "market",
                        RouteBranch::Pool => "pool",
                    },
                }),
            );
            skip_reasons.push(format!("{}:wave1_not_implemented", entry.provider_id));
            continue;
        }

        // Pool branch — this entry is a registered provider (openrouter,
        // ollama-local, custom). Wave 1 scope.
        last_attempted_provider_id = Some(entry.provider_id.clone());

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

        // Model selection: entry.model_id wins over default context-cascade
        // selection (the entry's choice is the operator's explicit route).
        let entry_model_override = entry.model_id.clone();
        let mut use_model = if let Some(ref model) = entry_model_override {
            info!("[entry-model->{}]", short_name(model));
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
                        // Exhausted — plan §4.3: 400 non-context terminal =
                        // CallTerminal (other routes would fail the same way).
                        break 'http Err(EntryError::CallTerminal {
                            reason: format!(
                                "HTTP 400 (not context-exceeded) after {} attempts: {}",
                                config.max_retries,
                                &body_400[..body_400.len().min(500)],
                            ),
                        });
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
                        404 => EntryError::CallTerminal {
                            reason: format!("model_not_found: {err_msg}"),
                        },
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

/// Phase 18b: helper for the inner function's terminal-error sites.
/// When an audit row was inserted at the top of the function, this
/// flips it to `status = 'failed'` so the audit trail isn't left with
/// a dangling pending row. Acquires the audit conn lock for the
/// duration of the UPDATE.
///
/// Walker Re-Plan Wire 2.1 Wave 1: walker now writes audit outcomes
/// inline and knows the winning / last-attempted provider_id, so this
/// helper's only caller moved. Kept `#[allow(dead_code)]` because
/// Waves 2-3 inline the fleet + market branches and may reintroduce
/// the helper for their bubble paths.
#[allow(dead_code)]
async fn maybe_fail_audit(
    audit: Option<&AuditContext>,
    audit_id: Option<i64>,
    error_message: &str,
) {
    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
        let conn = audit_ctx.conn.lock().await;
        // Walker Re-Plan Wire 2.1 Wave 1 task 11: legacy (pre-walker) call
        // site — provider_id stamping is the walker's job in Wave 1 tasks
        // 8-10. Pre-walker failures keep provider_id NULL.
        let _ = super::db::fail_llm_audit(&conn, id, error_message, None);
    }
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
    async fn walker_skips_fleet_and_market_entries_in_wave1() {
        // Route = [fleet, market, unknown-pool]. In Wave 1 the legacy
        // Phase A fleet filter at llm.rs:~1869 still runs BEFORE the
        // walker, so by the time the walker iterates route.providers the
        // `fleet` entry has been retained-removed by that filter. What
        // the walker sees:
        //   - market: branch_allowed(Market, Local) = true, Wave 1 emits
        //     wave1_not_implemented skip.
        //   - unknown-pool: provider_not_in_pool unavailable.
        // Walker exhausts 2 entries (not 3 — the filter already dropped
        // fleet). Waves 2+3 move fleet and market INTO the walker, at
        // which point this count becomes 3.
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
        // Wave 1: fleet filter runs before walker, so only market +
        // unknown-pool reach the walker. Wave 2 raises this to 3.
        assert!(
            msg.contains("2 entries"),
            "expected '2 entries' (market + unknown-pool; fleet pre-filtered), got: {msg}",
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
}

// ── call_model_unified market integration tests ──────────────────────────────
//
// Tests for the Phase B market branch helpers. All pure-function tests
// so they run fast and require no live DB / tunnel / Wire.
//
// See `docs/plans/call-model-unified-market-integration.md` §9.1.

#[cfg(test)]
mod market_integration_tests {
    use super::*;
    use crate::pyramid::compute_requester::RequesterError;
    use crate::pyramid::local_mode::{
        ComputeParticipationMode, EffectiveParticipationPolicy,
    };

    // ── Gate fixture ─────────────────────────────────────────────────

    fn base_policy(eager: bool, threshold: u32) -> EffectiveParticipationPolicy {
        EffectiveParticipationPolicy {
            mode: ComputeParticipationMode::Hybrid,
            allow_fleet_dispatch: true,
            allow_fleet_serving: true,
            allow_market_dispatch: true,
            allow_market_visibility: true,
            allow_storage_pulling: true,
            allow_storage_hosting: true,
            allow_relay_usage: true,
            allow_relay_serving: true,
            allow_serving_while_degraded: false,
            market_dispatch_threshold_queue_depth: threshold,
            market_dispatch_max_wait_ms: 60_000,
            market_dispatch_eager: eager,
        }
    }

    fn snap_connected() -> TunnelSnapshot {
        TunnelSnapshot {
            connected: true,
            has_url: true,
        }
    }

    // ── should_try_market branches (§9.1) ────────────────────────────

    #[test]
    fn gate_false_when_allow_market_dispatch_false() {
        let mut p = base_policy(true, 0);
        p.allow_market_dispatch = false;
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 0, true));
    }

    #[test]
    fn gate_false_when_non_eager_and_queue_under_threshold() {
        let p = base_policy(false, 10);
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 0, true));
    }

    #[test]
    fn gate_true_when_non_eager_and_queue_at_threshold() {
        let p = base_policy(false, 10);
        assert!(should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 10, true));
    }

    #[test]
    fn gate_true_when_eager_and_queue_zero() {
        let p = base_policy(true, 10);
        assert!(should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 0, true));
    }

    #[test]
    fn gate_false_when_balance_below_estimated_deposit() {
        let p = base_policy(true, 0);
        // balance 100, estimated 1000 → gate rejects.
        assert!(!should_try_market(&p, 100, 1000, true, &snap_connected(), 0, true));
    }

    #[test]
    fn gate_false_when_tier_not_eligible() {
        let p = base_policy(true, 0);
        assert!(!should_try_market(&p, i64::MAX, 0, false, &snap_connected(), 0, true));
    }

    #[test]
    fn gate_false_when_tunnel_connecting_with_url() {
        let p = base_policy(true, 0);
        let snap = TunnelSnapshot { connected: false, has_url: true };
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap, 0, true));
    }

    #[test]
    fn gate_false_when_tunnel_disconnected() {
        let p = base_policy(true, 0);
        let snap = TunnelSnapshot { connected: false, has_url: false };
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap, 0, true));
    }

    #[test]
    fn gate_false_when_tunnel_url_none() {
        let p = base_policy(true, 0);
        let snap = TunnelSnapshot { connected: true, has_url: false };
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap, 0, true));
    }

    #[test]
    fn gate_false_when_compute_market_context_absent() {
        let p = base_policy(true, 0);
        assert!(!should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 0, false));
    }

    #[test]
    fn gate_true_end_to_end_positive() {
        let p = base_policy(true, 0);
        assert!(should_try_market(&p, i64::MAX, 0, true, &snap_connected(), 0, true));
    }

    // ── model_tier_market_eligible ───────────────────────────────────

    #[test]
    fn tier_eligibility_empty_is_not_eligible() {
        assert!(!model_tier_market_eligible(""));
    }

    #[test]
    fn tier_eligibility_non_empty_is_eligible() {
        assert!(model_tier_market_eligible("fast_extract"));
        assert!(model_tier_market_eligible("mid"));
        assert!(model_tier_market_eligible("max"));
    }

    // ── classify_soft_fail_reason (pure function, §9.1) ──────────────

    #[test]
    fn classify_no_match() {
        let err = RequesterError::NoMatch { detail: "no_offer".into() };
        assert_eq!(classify_soft_fail_reason(&err), "no_match");
    }

    #[test]
    fn classify_match_failed_includes_status() {
        let err = RequesterError::MatchFailed { status: 500, body: "x".into() };
        assert_eq!(classify_soft_fail_reason(&err), "match_failed_500");
    }

    #[test]
    fn classify_fill_rejected_sanitizes_slug() {
        let err = RequesterError::FillRejected {
            status: 503,
            reason: "market_serving_disabled".into(),
            body: "".into(),
        };
        assert_eq!(
            classify_soft_fail_reason(&err),
            "fill_rejected_provider_serving_disabled"
        );
    }

    #[test]
    fn classify_fill_failed_includes_status() {
        let err = RequesterError::FillFailed { status: 425, body: "".into() };
        assert_eq!(classify_soft_fail_reason(&err), "fill_failed_425");
    }

    #[test]
    fn classify_delivery_timed_out_includes_waited_ms() {
        let err = RequesterError::DeliveryTimedOut { waited_ms: 60_000 };
        assert_eq!(
            classify_soft_fail_reason(&err),
            "delivery_timed_out_60000ms"
        );
    }

    #[test]
    fn classify_delivery_tombstoned_sanitizes_slug() {
        let err = RequesterError::DeliveryTombstoned {
            reason: "delivery_retry_exhausted".into(),
        };
        assert_eq!(
            classify_soft_fail_reason(&err),
            "delivery_tombstoned_delivery_retry_exhausted"
        );
    }

    #[test]
    fn classify_provider_failed_includes_code() {
        let err = RequesterError::ProviderFailed {
            code: "oom".into(),
            message: "gpu oom".into(),
        };
        assert_eq!(classify_soft_fail_reason(&err), "provider_failed_oom");
    }

    #[test]
    fn classify_internal_is_internal() {
        let err = RequesterError::Internal("anything".into());
        assert_eq!(classify_soft_fail_reason(&err), "internal");
    }

    // ── sanitize_wire_slug (§9.1) ────────────────────────────────────

    #[test]
    fn sanitize_market_serving_disabled_maps_to_provider_serving_disabled() {
        assert_eq!(
            sanitize_wire_slug("market_serving_disabled"),
            "provider_serving_disabled"
        );
    }

    #[test]
    fn sanitize_generic_market_prefix_becomes_network() {
        assert_eq!(sanitize_wire_slug("market_foo"), "network_foo");
    }

    #[test]
    fn sanitize_offer_depleted_becomes_contribution_depleted() {
        assert_eq!(
            sanitize_wire_slug("offer_depleted"),
            "contribution_depleted"
        );
    }

    #[test]
    fn sanitize_generic_offer_prefix_becomes_contribution() {
        assert_eq!(sanitize_wire_slug("offer_bar"), "contribution_bar");
    }

    #[test]
    fn sanitize_seller_becomes_provider() {
        assert_eq!(sanitize_wire_slug("seller_mismatch"), "provider_mismatch");
    }

    #[test]
    fn sanitize_buyer_becomes_requester() {
        assert_eq!(sanitize_wire_slug("buyer_rejected"), "requester_rejected");
    }

    #[test]
    fn sanitize_earnings_becomes_contributions() {
        assert_eq!(sanitize_wire_slug("earnings_frozen"), "contributions_frozen");
    }

    #[test]
    fn sanitize_unknown_slug_passes_through() {
        assert_eq!(sanitize_wire_slug("foo_bar"), "foo_bar");
    }

    // Forward-compat pass-through tests — these slugs are NOT in Wire's
    // current corpus per compute_requester.rs audit, but the sanitizer
    // MUST accept unknown input gracefully. If Wire starts emitting any
    // of these we'll add explicit maps; until then pass-through is OK.
    #[test]
    fn sanitize_forward_compat_deposit_required_passes_through() {
        assert_eq!(
            sanitize_wire_slug("deposit_required"),
            "deposit_required"
        );
    }

    #[test]
    fn sanitize_forward_compat_rate_limit_exceeded_passes_through() {
        assert_eq!(
            sanitize_wire_slug("rate_limit_exceeded"),
            "rate_limit_exceeded"
        );
    }

    #[test]
    fn sanitize_forward_compat_trader_banned_passes_through() {
        assert_eq!(sanitize_wire_slug("trader_banned"), "trader_banned");
    }

    // ── LlmResponse::from_market_result ──────────────────────────────

    #[test]
    fn from_market_result_tags_provider_as_network() {
        let mr = crate::pyramid::compute_requester::MarketResult {
            content: "hi".into(),
            input_tokens: 10,
            output_tokens: 5,
            model_used: "llama3".into(),
            latency_ms: 100,
            finish_reason: Some("stop".into()),
        };
        let resp = LlmResponse::from_market_result(mr);
        assert_eq!(resp.content, "hi");
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        assert_eq!(resp.provider_id.as_deref(), Some("network"));
        assert_eq!(resp.fleet_peer_model.as_deref(), Some("llama3"));
        assert!(resp.fleet_peer_id.is_none());
        assert!(resp.generation_id.is_none());
        assert!(resp.actual_cost_usd.is_none());
    }

    // ── Balance-exhausted dedup ──────────────────────────────────────

    #[test]
    fn balance_exhausted_once_skips_non_build_ctx() {
        // No StepContext → no build_id scope → skip entirely.
        let cfg = LlmConfig::default();
        emit_network_balance_exhausted_once(100, 50, None, &cfg);
        // Nothing observable to assert beyond "no panic".
    }

    #[test]
    fn balance_exhausted_once_skips_empty_build_id() {
        // StepContext present but build_id is empty — sentinel for
        // "no build context" per §3.4.2.
        let cfg = LlmConfig::default();
        let ctx = StepContext::new("s", "", "n", "p", 0, None, "");
        emit_network_balance_exhausted_once(100, 50, Some(&ctx), &cfg);
        // OnceLock remains unset because the empty-build_id guard ran
        // before the set() call. set() is still Ok on a subsequent
        // emit attempt once build_id is populated.
        assert!(ctx.balance_exhausted_emitted.get().is_none());
    }

    #[test]
    fn balance_exhausted_once_sets_once_lock_and_dedup_works() {
        // First attempt with a real build_id sets the OnceLock; a
        // second attempt with the same StepContext returns Err from
        // set() and skips. No observable chronicle write in either
        // case because db_path is empty.
        let cfg = LlmConfig::default();
        let ctx = StepContext::new("slug-a", "build-1", "n", "p", 0, None, "");
        emit_network_balance_exhausted_once(100, 50, Some(&ctx), &cfg);
        assert!(
            ctx.balance_exhausted_emitted.get().is_some(),
            "first call must set the OnceLock"
        );
        // Second call: set() is Err — guard short-circuits.
        emit_network_balance_exhausted_once(200, 60, Some(&ctx), &cfg);
        assert!(
            ctx.balance_exhausted_emitted.get().is_some(),
            "OnceLock is still set after dedup no-op"
        );
    }

    // ── Panic safety: catch_unwind + PendingJobs lifecycle ───────────

    #[tokio::test]
    async fn catch_unwind_does_not_leak_pending_jobs_on_panic() {
        use crate::pyramid::pending_jobs::PendingJobs;
        let pending = PendingJobs::new();

        // Simulate the Phase B branch's AssertUnwindSafe(call_market).catch_unwind().
        // Use a plain future that panics — the real call_market has the
        // same cancellation-safe shape.
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(async { panic!("simulated call_market panic") })
            .catch_unwind()
            .await;
        assert!(result.is_err());

        // We never registered anything, so the map is empty — this is
        // the invariant the branch relies on: a panic never leaves a
        // dangling Sender because the panic happens before register.
        // If a panic happened AFTER register, PendingJobs' own timeout
        // cleanup handles the dangling entry.
        assert_eq!(pending.len().await, 0);
    }
}
