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
    RequestMetadata, ResolvedTier,
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
        self
    }
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

    // ── Phase 1 Compute Queue: Transparent routing ─────────────────
    //
    // When the config has a compute_queue attached AND the caller is
    // NOT the GPU loop itself (skip_concurrency_gate == false), enqueue
    // the call and block on the oneshot result. This is the single
    // interception point that routes ALL unified LLM calls through the
    // per-model FIFO queue without changing any caller.
    if let Some(ref queue_handle) = config.compute_queue {
        if !options.skip_concurrency_gate {
            // Derive queue routing key from the resolved model in ctx,
            // or fall back to the config's primary model.
            let queue_model_id = ctx
                .and_then(|c| c.resolved_model_id.clone())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| config.primary_model.clone());

            let (tx, rx) = tokio::sync::oneshot::channel();

            // Clone config WITHOUT queue handle (prevents re-enqueue loop)
            // and WITHOUT fleet roster (prevents fleet re-dispatch).
            let mut gpu_config = config.clone();
            gpu_config.compute_queue = None;
            gpu_config.fleet_roster = None;

            // Set skip_concurrency_gate on the forwarded options so the
            // GPU loop's execution bypasses semaphore/pool acquisition.
            // Determine source and job_path BEFORE moving options.
            // If options.chronicle_job_path is set (fleet_received path), use it.
            let entry_source = if options.skip_fleet_dispatch && options.chronicle_job_path.is_some() {
                "fleet_received".to_string()
            } else {
                "local".to_string()
            };
            let chronicle_job_path_val = options.chronicle_job_path.clone().unwrap_or_else(|| {
                super::compute_chronicle::generate_job_path(ctx, None, &queue_model_id, &entry_source)
            });
            let entry_chronicle_jp = options.chronicle_job_path.clone();

            let mut gpu_options = options;
            gpu_options.skip_concurrency_gate = true;

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

            // Emit QueueJobEnqueued event via BuildEventBus.
            // Use slug "__compute__" (reserved non-pyramid slug).
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

            // Block until the GPU loop processes this item and sends result.
            return rx
                .await
                .map_err(|_| anyhow!("compute queue: GPU loop dropped the job"))?;
        }
    }

    // ── Phase 6: Cache lookup path ──────────────────────────────────
    //
    // Delegated to `try_cache_lookup_or_key`, which is shared with
    // `call_model_via_registry`. When it returns `CacheProbeOutcome::Hit`
    // the cached response short-circuits the HTTP path entirely.
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

    // ── Phase 18b: Audit pending row insert ─────────────────────────
    //
    // Mirror the legacy `call_model_audited` flow: insert a pending row
    // BEFORE the HTTP call so a crash mid-call leaves a trace. The row
    // is updated to 'complete' or 'failed' below. When no AuditContext
    // was supplied this is a no-op and the function reduces to the
    // pre-Phase-18b cache-aware path.
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
    if let Some(ref route) = resolved_route {
        if !options.skip_fleet_dispatch && !route.matched_rule_name.is_empty() {
            let has_fleet = route.providers.iter().any(|e| e.provider_id == "fleet");
            tracing::info!(
                has_fleet,
                rule = %route.matched_rule_name,
                fleet_roster_present = config.fleet_roster.is_some(),
                provider_count = route.providers.len(),
                providers = ?route.providers.iter().map(|p| &p.provider_id).collect::<Vec<_>>(),
                "Fleet Phase A: entry check"
            );
            if has_fleet {
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
                        "Fleet Phase A: checking roster for rule match"
                    );
                    for (pid, peer) in &roster.peers {
                        tracing::info!(
                            peer_id = %pid,
                            serving_rules = ?peer.serving_rules,
                            models = ?peer.models_loaded,
                            handle = ?peer.handle_path,
                            stale = %(chrono::Utc::now() - peer.last_seen).num_seconds() > 120,
                            "Fleet peer state"
                        );
                    }
                    if let Some(peer) = roster.find_peer_for_rule(&route.matched_rule_name) {
                        let jwt = roster.fleet_jwt.clone().unwrap_or_default();
                        if !jwt.is_empty() {
                            // (fleet dispatch proceeds below)
                            let peer_clone = peer.clone();
                            let rule_name = route.matched_rule_name.clone();
                            // Fleet timeout reads from the matched rule's escalation config
                            let fleet_timeout_secs = route.max_wait_secs;
                            drop(roster); // release lock before async

                            // WP-5: Chronicle fleet_dispatched event
                            let fleet_job_path = super::compute_chronicle::generate_job_path(
                                ctx, None, &config.primary_model, "fleet",
                            );
                            let fleet_start = std::time::Instant::now();
                            let fleet_db_path = ctx
                                .map(|c| c.db_path.clone())
                                .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
                            {
                                let chronicle_ctx = if let Some(sc) = ctx {
                                    super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                        sc, &fleet_job_path, "fleet_dispatched", "fleet",
                                    )
                                } else {
                                    super::compute_chronicle::ChronicleEventContext::minimal(
                                        &fleet_job_path, "fleet_dispatched", "fleet",
                                    )
                                    .with_model_id(config.primary_model.clone())
                                };
                                let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                                    "peer_id": peer_clone.node_id,
                                    "peer_name": peer_clone.name,
                                    "rule_name": rule_name,
                                    "timeout_secs": fleet_timeout_secs,
                                }));
                                if let Some(ref db_path) = fleet_db_path {
                                    let db_path = db_path.clone();
                                    let chronicle_ctx = chronicle_ctx.clone();
                                    tokio::task::spawn_blocking(move || {
                                        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                                            let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                                        }
                                    });
                                }
                            }

                            match crate::fleet::fleet_dispatch_by_rule(
                                &peer_clone,
                                &rule_name,
                                system_prompt,
                                user_prompt,
                                temperature,
                                _max_tokens,
                                response_format,
                                &jwt,
                                fleet_timeout_secs,
                            )
                            .await
                            {
                                Ok(fleet_resp) => {
                                    // WP-6: Chronicle fleet_returned event
                                    {
                                        let chronicle_ctx = if let Some(sc) = ctx {
                                            super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                                                sc, &fleet_job_path, "fleet_returned", "fleet",
                                            )
                                        } else {
                                            super::compute_chronicle::ChronicleEventContext::minimal(
                                                &fleet_job_path, "fleet_returned", "fleet",
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

                                    // Return with fleet provenance on the LlmResponse
                                    return Ok(LlmResponse {
                                        content: fleet_resp.content,
                                        usage: super::types::TokenUsage {
                                            prompt_tokens: fleet_resp.prompt_tokens.unwrap_or(0),
                                            completion_tokens: fleet_resp.completion_tokens.unwrap_or(0),
                                        },
                                        generation_id: None,
                                        actual_cost_usd: None, // fleet is free (same operator)
                                        provider_id: Some("fleet".to_string()),
                                        fleet_peer_id: Some(peer_clone.handle_path.clone()
                                            .unwrap_or_else(|| peer_clone.node_id.clone())),
                                        fleet_peer_model: fleet_resp.peer_model.clone(),
                                    });
                                }
                                Err(e) => {
                                    // Chronicle fleet_dispatch_failed event
                                    {
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
                                            "error": e.to_string(),
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
                                    }

                                    // Remove dead peer, continue to pool providers
                                    let mut roster_w = roster_handle.write().await;
                                    roster_w.remove_peer(&peer_clone.node_id);
                                    warn!("Fleet dispatch failed, trying pool providers: {}", e);
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

    // Filter "fleet" from providers before pool loop (fleet already tried or skipped)
    if let Some(ref mut route) = resolved_route {
        route.providers.retain(|e| e.provider_id != "fleet");
    }

    // Phase D: pool acquire with timeout-based provider escalation.
    // When a dispatch route exists, try each provider in the preference
    // chain with escalation_timeout_secs per hop. On the last provider
    // in the chain, wait up to max_wait_secs. When no route exists,
    // fall back to the single-provider acquire.
    let (_escalation_permit, effective_provider_id) = if let (Some(pools), Some(route)) = (&config.provider_pools, &resolved_route) {
        if route.bypass_pool {
            (None::<tokio::sync::OwnedSemaphorePermit>, provider_id.clone())
        } else if route.providers.is_empty() {
            // No providers in route — use default provider, no pool
            (None, provider_id.clone())
        } else {
            // Try providers in order with escalation timeout
            let mut acquired: Option<tokio::sync::OwnedSemaphorePermit> = None;
            let mut eff_provider = provider_id.clone();
            for (i, entry) in route.providers.iter().enumerate() {
                let is_last = i == route.providers.len() - 1;
                let timeout_secs = if is_last {
                    route.max_wait_secs
                } else {
                    route.escalation_timeout_secs
                };
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    pools.acquire(&entry.provider_id),
                ).await {
                    Ok(Ok(permit)) => {
                        acquired = Some(permit);
                        eff_provider = entry.provider_id.clone();
                        break;
                    }
                    Ok(Err(_)) => continue, // provider not in pools
                    Err(_) => {
                        tracing::info!(
                            provider = %entry.provider_id,
                            "Escalating: pool acquire timed out after {}s, trying next provider",
                            timeout_secs,
                        );
                        continue; // timeout, try next
                    }
                }
            }
            (acquired, eff_provider)
        }
    } else if let Some(pools) = &config.provider_pools {
        // Pools exist but no route — use default provider
        let permit = pools.acquire(&provider_id).await.ok();
        (permit, provider_id.clone())
    } else {
        (None, provider_id.clone())
    };

    // Phase D: when escalation changed the provider, re-instantiate the
    // provider impl (different URL, different headers, different auth).
    // Also pick up the route entry's model_id override if set.
    let mut escalation_model_override: Option<String> = None;
    if effective_provider_id != provider_id {
        if let Some(registry) = &config.provider_registry {
            let provider_row = registry.get_provider(&effective_provider_id)
                .ok_or_else(|| anyhow!("escalated provider '{}' not registered", effective_provider_id))?;
            let (impl_box, sec) = registry.instantiate_provider(&provider_row)?;
            provider_type = provider_row.provider_type;
            provider_impl = impl_box;
            secret = sec;
        }
        // Check if the matched route entry specifies a model override
        if let Some(route) = &resolved_route {
            for entry in &route.providers {
                if entry.provider_id == effective_provider_id {
                    if let Some(ref model) = entry.model_id {
                        escalation_model_override = Some(model.clone());
                    }
                    break;
                }
            }
        }
    }

    // Phase 11 wanderer fix: provider_id used for the health hook. Use
    // the effective provider (may differ from initial after escalation).
    let health_provider_id = effective_provider_id.clone();

    // Model selection based on INPUT size only — max_tokens (output budget) is
    // irrelevant to whether the prompt fits in the model's context window.
    let est_input_tokens = estimate_tokens_llm(system_prompt, user_prompt).await;

    let mut use_model = if let Some(ref model) = escalation_model_override {
        // Phase D: escalated route entry specified a model — use it
        info!("[escalation->{}]", short_name(model));
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
    let url = provider_impl.chat_completions_url();
    let built_headers = provider_impl.prepare_headers(secret.as_ref())?;

    // Scale timeout with prompt size: base + increment_secs per chars_per_increment, capped at max.
    // Local providers (Ollama) get a 5x base timeout since large models on
    // consumer hardware are slower than cloud APIs. The semaphore already
    // serializes calls, so longer timeouts don't cause contention.
    let prompt_chars = system_prompt.len() + user_prompt.len();
    let local_timeout_scale = if provider_type == ProviderType::OpenaiCompat { 5 } else { 1 };
    let timeout = compute_timeout(
        prompt_chars,
        &options,
        config.base_timeout_secs * local_timeout_scale,
        config.max_timeout_secs * local_timeout_scale,
        config.timeout_chars_per_increment,
        config.timeout_increment_secs,
    );

    for attempt in 0..config.max_retries {
        // Compute effective max_tokens: model context limit minus input, capped at 48K output.
        // Works around OpenRouter counting max_tokens as reserved space.
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

        // Phase 11: build RequestMetadata from the StepContext so the
        // provider's augment_request_body hook receives build_id /
        // slug / step_name / depth / chunk_index. When ctx is None
        // (legacy callers) we fall back to RequestMetadata::default
        // so the OpenRouter trace object is empty — still valid,
        // just uncorrelated at the broadcast webhook.
        let metadata = ctx
            .map(RequestMetadata::from_step_context)
            .unwrap_or_default();
        provider_impl.augment_request_body(&mut body, &metadata);

        // Rate limit: when provider pools are configured, rate limiting is
        // handled per-pool inside pools.acquire(). Otherwise fall back to the
        // global sliding-window limiter.
        if config.provider_pools.is_none() {
            rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;
        }

        // Phase 13: emit LlmCallStarted once per HTTP dispatch. We
        // emit inside the retry loop so every attempt gets its own
        // timeline entry — the UI can render a "retrying" status
        // without guessing. The cache_key may be absent for legacy
        // call sites without a cache-usable ctx; in that case we
        // pass an empty string so the event is still emitted but the
        // correlation key is empty.
        let cache_key_for_event = cache_lookup
            .as_ref()
            .map(|l| l.cache_key.clone())
            .unwrap_or_default();
        emit_llm_call_started(ctx, &use_model, &cache_key_for_event);

        // Phase D: the per-provider pool permit is now acquired BEFORE the
        // retry loop via the escalation path (`_escalation_permit`). It is
        // held across all retry attempts so we don't re-enter the escalation
        // chain on each retry. The global semaphore fallback remains for the
        // no-pools / no-escalation-permit case (tests, pre-init).
        //
        // Phase 1 compute queue: when skip_concurrency_gate is true (GPU
        // loop execution), skip ALL concurrency gates — the queue already
        // serialized access.
        let _local_permit = if options.skip_concurrency_gate {
            None
        } else if _escalation_permit.is_none() && provider_type == ProviderType::OpenaiCompat {
            Some(LOCAL_PROVIDER_SEMAPHORE.acquire().await.map_err(|e| anyhow!("local provider semaphore closed: {e}"))?)
        } else {
            None
        };

        let mut request = client.post(&url).timeout(timeout);
        for (k, v) in &built_headers {
            request = request.header(k, v);
        }
        let resp = request.json(&body).send().await;
        // Drop local permit after the HTTP call completes (before response
        // parsing) so the next caller can proceed. The escalation permit
        // is dropped at function exit (after all retries are exhausted or
        // the call succeeds).
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
                    // Phase 13: per-attempt retry event.
                    let backoff_ms = (config.retry_base_sleep_secs as i64) * 1000;
                    emit_step_retry(
                        ctx,
                        attempt as i64,
                        config.max_retries as i64,
                        &format!("request error: {}", e),
                        backoff_ms,
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                // Phase 11 wanderer fix: feed the provider health state
                // machine so the oversight page reflects the outage.
                // Connection failure is a single-occurrence `down`
                // signal per `provider_health::record_provider_error`.
                maybe_record_provider_error(
                    ctx,
                    &health_provider_id,
                    super::provider_health::ProviderErrorKind::ConnectionFailure,
                );
                // Phase 13: terminal error event so the UI can flip
                // the step row to `failed`.
                let err_msg = format!(
                    "Request failed after {} attempts (timeout={}s): {}",
                    config.max_retries,
                    timeout.as_secs(),
                    e
                );
                emit_step_error(ctx, &err_msg);
                maybe_fail_audit(audit, audit_id, &err_msg).await;
                return Err(anyhow!(err_msg));
            }
        };

        let status = resp.status().as_u16();

        // HTTP 400: read body, only cascade on context-exceeded errors
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
                if response_format.is_some() {
                    warn!(
                        "[LLM] Context exceeded with response_format set — cascading from {} to {} (structured output may not be supported on fallback model)",
                        short_name(&prev_model),
                        short_name(&use_model),
                    );
                } else {
                    warn!(
                        "[LLM] Context exceeded on {}, cascading to {}",
                        short_name(&prev_model),
                        short_name(&use_model),
                    );
                }
                continue;
            } else {
                // Not context-related 400 — fall through to retry/backoff on same model
                warn!(
                    "[LLM] HTTP 400 (not context-exceeded) from {}: retrying on same model",
                    short_name(&use_model),
                );
                if attempt + 1 < config.max_retries {
                    let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                let err_msg = format!(
                    "HTTP 400 (not context-exceeded) after {} attempts: {}",
                    config.max_retries,
                    &body_400[..body_400.len().min(500)],
                );
                maybe_fail_audit(audit, audit_id, &err_msg).await;
                return Err(anyhow!(err_msg));
            }
        }

        // Retryable HTTP errors with exponential backoff (status codes from config)
        if config.retryable_status_codes.contains(&status) {
            let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
            info!("  HTTP {}, waiting {}s...", status, wait);
            // Phase 11 wanderer fix: feed the provider health state
            // machine on every 5xx observation, even when the call is
            // about to retry. The state machine itself handles the
            // threshold-based degrade decision so individual blips
            // don't flap the health flag.
            if status >= 500 {
                maybe_record_provider_error(
                    ctx,
                    &health_provider_id,
                    super::provider_health::ProviderErrorKind::Http5xx,
                );
            }
            // Phase 13: step retry event.
            emit_step_retry(
                ctx,
                attempt as i64,
                config.max_retries as i64,
                &format!("HTTP {} retry", status),
                (wait as i64) * 1000,
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        // Other non-success status
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt + 1 < config.max_retries {
                info!("  HTTP {}, retry {}...", status, attempt + 1);
                // Phase 13: step retry event.
                emit_step_retry(
                    ctx,
                    attempt as i64,
                    config.max_retries as i64,
                    &format!("HTTP {} retry", status),
                    (config.retry_base_sleep_secs as i64) * 1000,
                );
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                continue;
            }
            // Phase 11 wanderer fix: record the terminal 5xx so the
            // health state machine sees it. Non-5xx final errors
            // (401/403/404) are NOT fed into the health hook — they
            // indicate auth/config mistakes, not provider failure.
            if status >= 500 {
                maybe_record_provider_error(
                    ctx,
                    &health_provider_id,
                    super::provider_health::ProviderErrorKind::Http5xx,
                );
            }
            let err_msg = format!(
                "HTTP {} after {} attempts: {}",
                status,
                config.max_retries,
                &body_text[..body_text.len().min(200)]
            );
            emit_step_error(ctx, &err_msg);
            maybe_fail_audit(audit, audit_id, &err_msg).await;
            return Err(anyhow!(err_msg));
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
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                let err_msg = format!(
                    "Failed to read response after {} attempts: {}",
                    config.max_retries, e
                );
                emit_step_error(ctx, &err_msg);
                maybe_fail_audit(audit, audit_id, &err_msg).await;
                return Err(anyhow!(err_msg));
            }
        };

        // Delegate to the provider trait for response parsing. Every
        // provider returns the same `ParsedLlmResponse` shape so the
        // retry + debug-logging branches below are provider-agnostic.
        let parsed: ParsedLlmResponse = match provider_impl.parse_response(&body_text) {
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
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                let err_msg = format!(
                    "Failed to parse response after {} attempts: {}",
                    config.max_retries, e
                );
                emit_step_error(ctx, &err_msg);
                maybe_fail_audit(audit, audit_id, &err_msg).await;
                return Err(anyhow!(err_msg));
            }
        };

        let usage = parsed.usage.clone();
        let generation_id = parsed.generation_id.clone();
        let finish_reason_str = parsed
            .finish_reason
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Always log finish_reason so it shows up in normal tracing
        info!(
            "[LLM] provider={} model={} finish_reason={} prompt_tokens={} completion_tokens={}",
            provider_type.as_str(),
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
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                continue;
            }
            let err_msg = format!(
                "Model returned empty content after {} attempts",
                config.max_retries
            );
            emit_step_error(ctx, &err_msg);
            maybe_fail_audit(audit, audit_id, &err_msg).await;
            return Err(anyhow!(err_msg));
        }

        let response = LlmResponse {
            content: parsed.content,
            usage,
            generation_id,
            actual_cost_usd: parsed.actual_cost_usd,
            // Legacy path without a provider registry: tag the row as
            // coming from the provider impl's type so the webhook
            // correlator has a non-null grouping key for the leak
            // sweep. Phase 11's registry path (below) sets a real
            // provider_id from the resolved registry row.
            provider_id: Some(provider_type.as_str().to_string()),
            fleet_peer_id: None,
            fleet_peer_model: None,
        };

        // ── Phase 6: Cache store path ──────────────────────────────
        //
        // Delegated to `try_cache_store`, which is shared with
        // `call_model_via_registry`. No-op when no ctx was attached or
        // when the lookup phase didn't compute a key.
        try_cache_store(ctx, cache_lookup.as_ref(), &response, call_started);

        // Phase 13: emit LlmCallCompleted on the success exit. The
        // actual cost from OpenRouter is preferred when present;
        // otherwise we fall back to a heuristic estimate based on
        // token counts so the running total is never empty.
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

        // WP-8: Chronicle cloud_returned event.
        // Cloud detection: ProviderType::Openrouter is cloud (not local).
        // ProviderType::OpenaiCompat is local (Ollama).
        if provider_type == ProviderType::Openrouter {
            let cloud_job_path = saved_chronicle_job_path.clone().unwrap_or_else(|| {
                super::compute_chronicle::generate_job_path(
                    ctx, None, &use_model, "cloud",
                )
            });
            let chronicle_ctx = if let Some(sc) = ctx {
                super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                    sc, &cloud_job_path, "cloud_returned", "cloud",
                )
            } else {
                super::compute_chronicle::ChronicleEventContext::minimal(
                    &cloud_job_path, "cloud_returned", "cloud",
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
                .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
            if let Some(db_path) = db_path {
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                    }
                });
            }
        }

        // ── Phase 18b: Audit complete row write ──────────────────────
        //
        // Update the pending row inserted at the top of the function with
        // the response, parsed_ok=true, token usage, latency, and the
        // generation_id. No-op when no AuditContext was supplied. The
        // row was inserted with `cache_hit = 0` (default) so the
        // wire-call vs cache-hit distinction stays correct.
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
            );
        }

        return Ok(response);
    }

    let err_msg = "Max retries exceeded".to_string();
    emit_step_error(ctx, &err_msg);
    maybe_fail_audit(audit, audit_id, &err_msg).await;
    Err(anyhow!(err_msg))
}

/// Phase 18b: helper for the inner function's terminal-error sites.
/// When an audit row was inserted at the top of the function, this
/// flips it to `status = 'failed'` so the audit trail isn't left with
/// a dangling pending row. Acquires the audit conn lock for the
/// duration of the UPDATE.
async fn maybe_fail_audit(
    audit: Option<&AuditContext>,
    audit_id: Option<i64>,
    error_message: &str,
) {
    if let (Some(audit_ctx), Some(id)) = (audit, audit_id) {
        let conn = audit_ctx.conn.lock().await;
        let _ = super::db::fail_llm_audit(&conn, id, error_message);
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

/// Shared cache probe path used by both `call_model_unified_with_options_and_ctx`
/// and `call_model_via_registry` (Phase 6 fix pass). Keeps the cache
/// hook point exactly once regardless of which HTTP retry loop is
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

/// Shared cache store path used by both
/// `call_model_unified_with_options_and_ctx` and `call_model_via_registry`.
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

// ── Registry-aware call path (Phase 3) ─────────────────────────────────────

/// Call the LLM via the provider registry: resolve a `tier_name` to
/// a provider + model, then issue the request with rich observability
/// metadata. This is the spec's preferred entry point for
/// chain-executor callers that have `(slug, chain_id, step_name)` in
/// scope and want per-step overrides to apply.
///
/// The `tier_name` is resolved against `pyramid_tier_routing`, with
/// a per-step override lookup in `pyramid_step_overrides` if
/// `slug`/`chain_id`/`step_name` are provided. The caller still
/// passes `system_prompt` / `user_prompt` / `temperature` /
/// `max_tokens` because those are per-call decisions (Phase 4/6 will
/// move them into contributions).
///
/// Errors:
/// * `tier <name> is not defined in pyramid_tier_routing` — user
///   needs to add the tier via Settings → Model Routing.
/// * `config references credential ${...}` — the provider's
///   `api_key_ref` resolves to a variable that isn't in the
///   credentials file. Points the user at Settings → Credentials.
///
/// Phase 6 fix pass: accepts `Option<&StepContext>` and performs the
/// same cache lookup / write that `call_model_unified_with_options_and_ctx`
/// does via the shared `try_cache_lookup_or_key` / `try_cache_store`
/// helpers. When the caller threads a cache-usable ctx, the
/// content-addressable cache short-circuits the HTTP path on a hit and
/// writes a new row on a miss. When `ctx` is `None` (or not
/// cache-usable) this function behaves exactly like the pre-Phase-6
/// registry path.
#[allow(clippy::too_many_arguments)]
pub async fn call_model_via_registry(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    tier_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    metadata: RequestMetadata,
) -> Result<LlmResponse> {
    // ── Phase 6 (fix pass): Cache lookup path ───────────────────────
    //
    // Identical entry point to `call_model_unified_with_options_and_ctx`
    // so the two HTTP paths share a single cache hook. A valid hit
    // short-circuits and never touches the registry or the HTTP path.
    let cache_lookup = match try_cache_lookup_or_key(ctx, system_prompt, user_prompt) {
        CacheProbeOutcome::Hit(response) => return Ok(response),
        CacheProbeOutcome::MissOrBypass(lookup) => lookup,
    };

    let call_started = std::time::Instant::now();

    let registry = config
        .provider_registry
        .as_ref()
        .ok_or_else(|| anyhow!("call_model_via_registry requires an LlmConfig with a provider_registry attached"))?;

    let resolved: ResolvedTier = registry.resolve_tier(
        tier_name,
        metadata.slug.as_deref(),
        metadata.chain_id.as_deref(),
        metadata.step_name.as_deref(),
    )?;

    let (provider_impl, secret) = registry.instantiate_provider(&resolved.provider)?;
    let provider_type = resolved.provider.provider_type;
    let url = provider_impl.chat_completions_url();
    let headers = provider_impl.prepare_headers(secret.as_ref())?;
    let client = &*HTTP_CLIENT;

    let est_input_tokens = estimate_tokens_llm(system_prompt, user_prompt).await;
    let context_limit = resolved
        .tier
        .context_limit
        .unwrap_or(config.primary_context_limit);
    // Output budget is the smaller of (context - input), the tier's
    // max_completion_tokens cap (if specified), and a sane 48K ceiling.
    let mut effective_max_tokens = context_limit
        .saturating_sub(est_input_tokens)
        .min(48_000)
        .max(1024);
    if let Some(cap) = resolved.tier.max_completion_tokens {
        effective_max_tokens = effective_max_tokens.min(cap);
    }
    // Honor the caller's explicit max_tokens if it's smaller (i.e., the
    // caller is asking for a short response even though the model can
    // produce more). Never raise it above effective_max_tokens.
    if max_tokens > 0 && max_tokens < effective_max_tokens {
        effective_max_tokens = max_tokens;
    }

    let prompt_chars = system_prompt.len() + user_prompt.len();
    let local_timeout_scale = if provider_type == ProviderType::OpenaiCompat { 5 } else { 1 };
    let timeout = compute_timeout(
        prompt_chars,
        &LlmCallOptions::default(),
        config.base_timeout_secs * local_timeout_scale,
        config.max_timeout_secs * local_timeout_scale,
        config.timeout_chars_per_increment,
        config.timeout_increment_secs,
    );

    let cache_key_for_event = cache_lookup
        .as_ref()
        .map(|l| l.cache_key.clone())
        .unwrap_or_default();

    for attempt in 0..config.max_retries {
        let mut body = serde_json::json!({
            "model": resolved.tier.model_id,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": effective_max_tokens
        });
        if let Some(rf) = response_format {
            if provider_impl.supports_response_format() && resolved.tier.supports_response_format() {
                body.as_object_mut()
                    .unwrap()
                    .insert("response_format".to_string(), rf.clone());
            }
        }
        provider_impl.augment_request_body(&mut body, &metadata);

        // Rate limiting: per-pool when available, global fallback otherwise.
        if config.provider_pools.is_none() {
            rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;
        }

        // Phase 13: emit LlmCallStarted once per HTTP dispatch.
        emit_llm_call_started(ctx, &resolved.tier.model_id, &cache_key_for_event);

        // Per-provider concurrency pool (Phase A dispatch).
        let _pool_permit: Option<tokio::sync::OwnedSemaphorePermit> = if let Some(pools) = &config.provider_pools {
            pools.acquire(&resolved.provider.id).await.ok()
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
        for (k, v) in &headers {
            request = request.header(k, v);
        }
        let resp = match request.json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    info!(
                        "  [via-registry:{}] request error (timeout={}s, err={}), retry {}...",
                        tier_name,
                        timeout.as_secs(),
                        e,
                        attempt + 1
                    );
                    emit_step_retry(
                        ctx,
                        attempt as i64,
                        config.max_retries as i64,
                        &format!("request error: {}", e),
                        (config.retry_base_sleep_secs as i64) * 1000,
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(
                        config.retry_base_sleep_secs,
                    ))
                    .await;
                    continue;
                }
                // Phase 11: connection failure → provider health state
                // machine sees it as a hard `down` signal on a single
                // occurrence. Fire-and-forget: we don't block the
                // caller on the DB write.
                maybe_record_provider_error(
                    ctx,
                    &resolved.provider.id,
                    super::provider_health::ProviderErrorKind::ConnectionFailure,
                );
                let err_msg = format!(
                    "Request to tier `{}` failed after {} attempts: {}",
                    tier_name, config.max_retries, e
                );
                emit_step_error(ctx, &err_msg);
                return Err(anyhow!(err_msg));
            }
        };
        // Release permits before response parsing.
        drop(_pool_permit);
        drop(_local_permit);

        let status = resp.status().as_u16();
        if config.retryable_status_codes.contains(&status) {
            let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
            info!(
                "  [via-registry:{}] HTTP {}, waiting {}s...",
                tier_name, status, wait
            );
            // Phase 11: HTTP 5xx is a provider-side failure signal
            // even when the call will retry. Degrades only after
            // the count-within-window threshold, so single blips
            // don't trigger an alert.
            if status >= 500 {
                maybe_record_provider_error(
                    ctx,
                    &resolved.provider.id,
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
            continue;
        }
        // Phase C fix: HTTP 400 context-exceeded cascade.
        // If the 400 body mentions context/token limits, try the next
        // provider in the dispatch policy's route_to chain (if available).
        if status == 400 {
            let body_400 = resp.text().await.unwrap_or_default();
            warn!(
                "[via-registry:{}] HTTP 400 — body: {}",
                tier_name,
                &body_400[..body_400.len().min(500)],
            );
            let body_lower = body_400.to_lowercase();
            let is_context_exceeded = body_lower.contains("context")
                || body_lower.contains("too many tokens")
                || body_lower.contains("token limit");

            if is_context_exceeded {
                // Check if the dispatch policy has additional providers to try.
                if let Some(ref policy) = config.dispatch_policy {
                    use crate::pyramid::dispatch_policy::WorkType;
                    let route = policy.resolve_route(
                        WorkType::Build,
                        tier_name,
                        metadata.step_name.as_deref().unwrap_or(""),
                        ctx.map(|c| c.depth),
                    );
                    // Find the current provider in the route and try the next one.
                    if let Some(pos) = route
                        .providers
                        .iter()
                        .position(|r| r.provider_id == resolved.provider.id)
                    {
                        if pos + 1 < route.providers.len() {
                            let next = &route.providers[pos + 1];
                            warn!(
                                "[via-registry:{}] context exceeded on provider {}, cascading to {}",
                                tier_name, resolved.provider.id, next.provider_id,
                            );
                            // Attempt the next provider via a recursive call is
                            // not feasible here (different resolved tier). Log
                            // the cascade and return the error so the caller can
                            // handle retry at a higher level.
                        }
                    }
                }
                let err_msg = format!(
                    "HTTP 400 context-exceeded from tier `{}` (provider={}): {}",
                    tier_name,
                    provider_type.as_str(),
                    &body_400[..body_400.len().min(200)]
                );
                emit_step_error(ctx, &err_msg);
                return Err(anyhow!(err_msg));
            }

            // Non-context 400: fall through to generic error handling.
            let err_msg = format!(
                "HTTP {} from tier `{}` (provider={}): {}",
                status,
                tier_name,
                provider_type.as_str(),
                &body_400[..body_400.len().min(200)]
            );
            emit_step_error(ctx, &err_msg);
            return Err(anyhow!(err_msg));
        }
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if status >= 500 {
                maybe_record_provider_error(
                    ctx,
                    &resolved.provider.id,
                    super::provider_health::ProviderErrorKind::Http5xx,
                );
            }
            let err_msg = format!(
                "HTTP {} from tier `{}` (provider={}): {}",
                status,
                tier_name,
                provider_type.as_str(),
                &body_text[..body_text.len().min(200)]
            );
            emit_step_error(ctx, &err_msg);
            return Err(anyhow!(err_msg));
        }

        let body_text = resp.text().await?;
        let parsed = provider_impl.parse_response(&body_text)?;

        if parsed.content.is_empty() {
            if attempt + 1 < config.max_retries {
                info!(
                    "  [via-registry:{}] empty content, retry {}...",
                    tier_name,
                    attempt + 1
                );
                emit_step_retry(
                    ctx,
                    attempt as i64,
                    config.max_retries as i64,
                    "empty content",
                    (config.retry_base_sleep_secs as i64) * 1000,
                );
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs))
                    .await;
                continue;
            }
            let err_msg = format!(
                "tier `{}` returned empty content after {} attempts",
                tier_name, config.max_retries
            );
            emit_step_error(ctx, &err_msg);
            return Err(anyhow!(err_msg));
        }

        info!(
            "[LLM-TIER] tier={} provider={} model={} prompt_tokens={} completion_tokens={} cost={:?}",
            tier_name,
            provider_type.as_str(),
            resolved.tier.model_id,
            parsed.usage.prompt_tokens,
            parsed.usage.completion_tokens,
            parsed.actual_cost_usd,
        );

        let response = LlmResponse {
            content: parsed.content,
            usage: parsed.usage,
            generation_id: parsed.generation_id,
            actual_cost_usd: parsed.actual_cost_usd,
            provider_id: Some(resolved.provider.id.clone()),
            fleet_peer_id: None,
            fleet_peer_model: None,
        };

        // ── Phase 6 (fix pass): Cache store path ───────────────────
        try_cache_store(ctx, cache_lookup.as_ref(), &response, call_started);

        // Phase 13: emit LlmCallCompleted on success.
        let cost_usd = response
            .actual_cost_usd
            .unwrap_or_else(|| super::config_helper::estimate_cost(&response.usage));
        let latency_ms = call_started.elapsed().as_millis() as i64;
        emit_llm_call_completed(
            ctx,
            &resolved.tier.model_id,
            &cache_key_for_event,
            &response.usage,
            cost_usd,
            latency_ms,
        );

        // WP-8 (registry path): Chronicle cloud_returned event.
        if provider_type == ProviderType::Openrouter {
            let cloud_job_path = super::compute_chronicle::generate_job_path(
                ctx, None, &resolved.tier.model_id, "cloud",
            );
            let chronicle_ctx = if let Some(sc) = ctx {
                super::compute_chronicle::ChronicleEventContext::from_step_ctx(
                    sc, &cloud_job_path, "cloud_returned", "cloud",
                )
            } else {
                super::compute_chronicle::ChronicleEventContext::minimal(
                    &cloud_job_path, "cloud_returned", "cloud",
                )
                .with_model_id(resolved.tier.model_id.clone())
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
                .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
            if let Some(db_path) = db_path {
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = super::compute_chronicle::record_event(&conn, &chronicle_ctx);
                    }
                });
            }
        }

        return Ok(response);
    }

    let err_msg = format!("Max retries exceeded for tier `{}`", tier_name);
    emit_step_error(ctx, &err_msg);
    Err(anyhow!(err_msg))
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
        assert!(rebuilt.cache_access.is_none());
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
}
