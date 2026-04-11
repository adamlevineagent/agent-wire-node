// pyramid/config_helper.rs — Shared helper for building single-model LlmConfig instances.
//
// **Phase 3 fix pass:** `config_for_model(api_key, model)` is DEPRECATED.
// Production code should use `LlmConfig::clone_with_model_override(&self,
// model)` instead — that helper preserves the `provider_registry` and
// `credential_store` runtime handles that the Phase 3 refactor added to
// `LlmConfig`. The legacy `config_for_model` builds a fresh `LlmConfig`
// via `..Default::default()`, which zeroes both runtime handles, so
// every caller silently bypasses the provider registry, the
// `pyramid_tier_routing` table, the `pyramid_step_overrides` table, and
// the `.credentials` file. The maintenance subsystem (~22 call sites
// across `stale_helpers*`, `faq.rs`, `delta.rs`, `meta.rs`, `webbing.rs`)
// was using this helper everywhere; the fix pass retired it from
// production code and re-routed everything through
// `clone_with_model_override`.
//
// `config_for_model` is retained ONLY for unit-test fixtures that don't
// have a live `PyramidState` to clone from. Do NOT call it in
// production code paths — the deprecation warning is wired up to make
// any new caller fail clippy.

use crate::pyramid::llm::LlmConfig;
use crate::pyramid::types::TokenUsage;
use crate::pyramid::Tier1Config;

/// **DEPRECATED — use `LlmConfig::clone_with_model_override` instead.**
///
/// Build an `LlmConfig` targeting a specific model from raw `(api_key,
/// model)` strings. This helper drops the Phase 3 `provider_registry`
/// and `credential_store` fields because it ends in
/// `..Default::default()`, so every LLM call routed through the
/// resulting config silently bypasses the provider registry and the
/// `.credentials` file. Any caller in production code is a bug — use
/// `LlmConfig::clone_with_model_override(&self, model)` to clone the
/// live `PyramidState.config` while overriding the primary model.
///
/// This function is retained for unit-test fixtures that build a fresh
/// `LlmConfig` from scratch without a `PyramidState`.
#[deprecated(
    note = "Drops provider_registry + credential_store. Use \
            LlmConfig::clone_with_model_override on the live config from \
            PyramidState.config (or thread an &LlmConfig down to the \
            helper) so registry-aware Phase 3 routing applies."
)]
pub fn config_for_model(api_key: &str, model: &str) -> LlmConfig {
    let defaults = Tier1Config::default();
    LlmConfig {
        api_key: api_key.to_string(),
        auth_token: String::new(),
        primary_model: model.to_string(),
        // Set fallbacks to the same model so the cascade stays on-model.
        fallback_model_1: model.to_string(),
        fallback_model_2: model.to_string(),
        primary_context_limit: defaults.primary_context_limit,
        fallback_1_context_limit: defaults.fallback_1_context_limit,
        max_retries: defaults.llm_max_retries,
        base_timeout_secs: 120,
        max_timeout_secs: 600,
        ..Default::default()
    }
}

/// Estimate USD cost from token usage using configurable per-million pricing.
pub fn estimate_cost(usage: &TokenUsage) -> f64 {
    let defaults = Tier1Config::default();
    estimate_cost_with_pricing(
        usage,
        defaults.default_input_price_per_million,
        defaults.default_output_price_per_million,
    )
}

/// Estimate USD cost with explicit per-million-token pricing.
pub fn estimate_cost_with_pricing(
    usage: &TokenUsage,
    input_price_per_million: f64,
    output_price_per_million: f64,
) -> f64 {
    let input_cost = usage.prompt_tokens as f64 * input_price_per_million / 1_000_000.0;
    let output_cost = usage.completion_tokens as f64 * output_price_per_million / 1_000_000.0;
    input_cost + output_cost
}
