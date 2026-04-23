// pyramid/config_helper.rs — Shared helpers for LlmConfig + pricing math.
//
// W3c (walker v3, Phase 1): the former `config_for_model(api_key, model)`
// helper was deleted here. It wrote the five legacy
// `LlmConfig::{primary_model, fallback_model_{1,2}, primary_context_limit,
// fallback_1_context_limit}` fields, all of which retired in W3c. Per-call
// model overrides now flow through `LlmCallOptions.model_override` (§2.9
// "reqs.model" pattern), and per-tier resolution flows through the walker
// Decision or `provider_registry.resolve_tier` — both read at dispatch
// time, both without needing a fresh `LlmConfig` constructed per call.
//
// If a test needs an isolated `LlmConfig`, use `LlmConfig::default()`.

use crate::pyramid::types::TokenUsage;
use crate::pyramid::Tier1Config;

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
