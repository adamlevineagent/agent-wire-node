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

/// Shared HTTP client — reuses TCP connections and TLS sessions across all LLM calls.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
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
}

impl std::fmt::Debug for CacheAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheAccess")
            .field("slug", &self.slug)
            .field("build_id", &self.build_id)
            .field("db_path", &self.db_path)
            .field("bus", &self.bus.as_ref().map(|_| "<bus>"))
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
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LlmCallOptions {
    pub min_timeout_secs: Option<u64>,
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
) -> Result<(Box<dyn LlmProvider>, Option<ResolvedSecret>, ProviderType)> {
    if let Some(registry) = &config.provider_registry {
        // Prefer the tier-routing `fast_extract` entry's provider if
        // present; fall back to the `openrouter` seeded row. This keeps
        // the default path pointing at OpenRouter without hardcoding
        // its ID again here.
        let provider = registry
            .get_provider("openrouter")
            .ok_or_else(|| anyhow!("provider `openrouter` is not registered — run DB init"))?;
        let (impl_box, secret) = registry.instantiate_provider(&provider)?;
        let provider_type = provider.provider_type;
        return Ok((impl_box, secret, provider_type));
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
    Ok((Box::new(provider), secret, ProviderType::Openrouter))
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
    options: LlmCallOptions,
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
#[allow(clippy::too_many_arguments)]
pub async fn call_model_unified_with_options_and_ctx(
    config: &LlmConfig,
    ctx: Option<&StepContext>,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    _max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    options: LlmCallOptions,
) -> Result<LlmResponse> {
    // ── Phase 6: Cache lookup path ──────────────────────────────────
    //
    // Delegated to `try_cache_lookup_or_key`, which is shared with
    // `call_model_via_registry`. When it returns `CacheProbeOutcome::Hit`
    // the cached response short-circuits the HTTP path entirely.
    let cache_lookup = match try_cache_lookup_or_key(ctx, system_prompt, user_prompt) {
        CacheProbeOutcome::Hit(response) => return Ok(response),
        CacheProbeOutcome::MissOrBypass(lookup) => lookup,
    };

    let call_started = std::time::Instant::now();

    // Resolve the provider trait impl + credential for this call. The
    // registry path is preferred; if no registry is attached to the
    // config we synthesize an `OpenRouterProvider` from the legacy
    // fields. Either way the resulting `Box<dyn LlmProvider>` owns the
    // URL, headers, and response parser — `llm.rs` no longer encodes
    // any of that.
    let (provider_impl, secret, provider_type) = build_call_provider(config)?;

    // Phase 11 wanderer fix: provider_id used for the health hook. The
    // seeded registry row's `id` matches the provider_type tag on the
    // default install (`"openrouter"`), so using the type string here
    // resolves against the correct row in `pyramid_providers` for both
    // the registry and transitional fallback paths in `build_call_provider`.
    let health_provider_id = provider_type.as_str().to_string();

    // Model selection based on INPUT size only — max_tokens (output budget) is
    // irrelevant to whether the prompt fits in the model's context window.
    let est_input_tokens = estimate_tokens_llm(system_prompt, user_prompt).await;

    let mut use_model = if est_input_tokens > config.fallback_1_context_limit {
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

    // Scale timeout with prompt size: base + increment_secs per chars_per_increment, capped at max
    let prompt_chars = system_prompt.len() + user_prompt.len();
    let timeout = compute_timeout(
        prompt_chars,
        options,
        config.base_timeout_secs,
        config.max_timeout_secs,
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

        // Rate limit: wait for sliding window capacity
        rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;

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

        let mut request = client.post(&url).timeout(timeout);
        for (k, v) in &built_headers {
            request = request.header(k, v);
        }
        let resp = request.json(&body).send().await;

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
                return Err(anyhow!(
                    "HTTP 400 (not context-exceeded) after {} attempts: {}",
                    config.max_retries,
                    &body_400[..body_400.len().min(500)],
                ));
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

        return Ok(response);
    }

    let err_msg = "Max retries exceeded".to_string();
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
    let timeout = compute_timeout(
        prompt_chars,
        LlmCallOptions::default(),
        config.base_timeout_secs,
        config.max_timeout_secs,
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

        rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;

        // Phase 13: emit LlmCallStarted once per HTTP dispatch.
        emit_llm_call_started(ctx, &resolved.tier.model_id, &cache_key_for_event);

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

/// Audited LLM call: wraps `call_model_unified` with pre/post audit row writes.
///
/// 1. Inserts a pending audit row BEFORE the call
/// 2. Calls `call_model_unified`
/// 3. Updates the row with response + metrics AFTER
///
/// Returns `(LlmResponse, audit_row_id)`.
pub async fn call_model_audited(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    audit: &AuditContext,
) -> Result<(LlmResponse, i64)> {
    use super::db;

    // Insert pending row (async lock on tokio mutex)
    let audit_id = {
        let conn = audit.conn.lock().await;
        db::insert_llm_audit_pending(
            &conn,
            &audit.slug,
            &audit.build_id,
            audit.node_id.as_deref(),
            &audit.step_name,
            &audit.call_purpose,
            audit.depth,
            &config.primary_model,
            system_prompt,
            user_prompt,
        )?
    };

    let start = std::time::Instant::now();

    // Actual LLM call
    let result = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
    )
    .await;

    let latency_ms = start.elapsed().as_millis() as i64;

    match result {
        Ok(resp) => {
            let conn = audit.conn.lock().await;
            let _ = db::complete_llm_audit(
                &conn,
                audit_id,
                &resp.content,
                true,
                resp.usage.prompt_tokens,
                resp.usage.completion_tokens,
                latency_ms,
                resp.generation_id.as_deref(),
            );
            Ok((resp, audit_id))
        }
        Err(e) => {
            let conn = audit.conn.lock().await;
            let _ = db::fail_llm_audit(&conn, audit_id, &e.to_string());
            drop(conn);
            Err(e)
        }
    }
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
    let (provider_impl, secret, _) = build_call_provider(config)?;
    let client = &*HTTP_CLIENT;
    let url = provider_impl.chat_completions_url();
    let built_headers = provider_impl.prepare_headers(secret.as_ref())?;
    let timeout = std::time::Duration::from_secs(config.base_timeout_secs);

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

        rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;

        let mut request = client.post(&url).timeout(timeout);
        for (k, v) in &built_headers {
            request = request.header(k, v);
        }
        let resp = request.json(&body).send().await;

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
            LlmCallOptions {
                min_timeout_secs: Some(420),
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
            LlmCallOptions::default(),
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
            LlmCallOptions::default(),
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
}
