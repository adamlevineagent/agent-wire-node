// pyramid/config_helper.rs — Shared helper for building single-model LlmConfig instances.
// DADBEAR test edit — if you see this in the stale log, the bear is awake.
//
// Consolidates the `config_for_model()` function that was duplicated across
// delta.rs, webbing.rs, and meta.rs.

use crate::pyramid::llm::LlmConfig;
use crate::pyramid::types::TokenUsage;

/// Build an LlmConfig targeting a specific model (no cascade — uses model as primary).
pub fn config_for_model(api_key: &str, model: &str) -> LlmConfig {
    LlmConfig {
        api_key: api_key.to_string(),
        auth_token: String::new(),
        primary_model: model.to_string(),
        // Set fallbacks to the same model so the cascade stays on-model.
        fallback_model_1: model.to_string(),
        fallback_model_2: model.to_string(),
        primary_context_limit: 120_000,
        fallback_1_context_limit: 900_000,
    }
}

/// Estimate USD cost from token usage. Mercury 2 pricing: $0.19/M input, $0.75/M output.
pub fn estimate_cost(usage: &TokenUsage) -> f64 {
    let input_cost = usage.prompt_tokens as f64 * 0.19 / 1_000_000.0;
    let output_cost = usage.completion_tokens as f64 * 0.75 / 1_000_000.0;
    input_cost + output_cost
}
