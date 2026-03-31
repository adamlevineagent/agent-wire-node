// pyramid/config_helper.rs — Shared helper for building single-model LlmConfig instances.
// DADBEAR test edit — if you see this in the stale log, the bear is awake.
//
// Consolidates the `config_for_model()` function that was duplicated across
// delta.rs, webbing.rs, and meta.rs.

use crate::pyramid::llm::LlmConfig;
use crate::pyramid::types::TokenUsage;
use crate::pyramid::Tier1Config;

/// Build an LlmConfig targeting a specific model (no cascade — uses model as primary).
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
