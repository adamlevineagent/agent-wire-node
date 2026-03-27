// pyramid/chain_dispatch.rs — Step dispatcher for the chain runtime engine
//
// Routes chain steps to either LLM (via OpenRouter) or named Rust mechanical
// functions. Also provides node construction from LLM output and node ID
// generation from patterns.
//
// See docs/plans/action-chain-refactor-v3.md §Phase 4 for full specification.

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::chain_engine::{ChainDefaults, ChainStep};
use super::execution_plan::{ModelRequirements, Step, StepOperation};
use super::expression::ValueEnv;
use super::llm::{self, LlmConfig, LlmResponse};
use super::naming::headline_from_analysis;
use super::transform_runtime;
use super::types::{Correction, Decision, PyramidNode, Term, Topic};

// ── Step context ────────────────────────────────────────────────────────────

/// Context available to all chain steps during execution.
#[derive(Clone)]
pub struct StepContext {
    pub db_reader: Arc<Mutex<Connection>>,
    pub db_writer: Arc<Mutex<Connection>>,
    pub slug: String,
    pub config: LlmConfig,
}

// ── Top-level dispatcher ────────────────────────────────────────────────────

/// Dispatch a chain step to either LLM or mechanical execution.
///
/// For LLM steps: builds a user prompt from `resolved_input`, calls the model,
/// parses JSON from the response (with automatic retry at temp 0.1 on parse
/// failure).
///
/// For mechanical steps: dispatches to named Rust functions by `rust_function`.
pub async fn dispatch_step(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &ChainDefaults,
    ctx: &StepContext,
) -> Result<Value> {
    if step.mechanical {
        let fn_name = step
            .rust_function
            .as_deref()
            .ok_or_else(|| anyhow!("Mechanical step '{}' missing rust_function", step.name))?;
        info!("[CHAIN] step '{}' → mechanical fn '{}'", step.name, fn_name);
        dispatch_mechanical(fn_name, resolved_input, ctx)
    } else {
        dispatch_llm(step, resolved_input, system_prompt, defaults, ctx).await
    }
}

// ── LLM dispatch ────────────────────────────────────────────────────────────

/// Resolve the model string from step overrides, tier mapping, or defaults.
fn resolve_model(step: &ChainStep, defaults: &ChainDefaults, config: &LlmConfig) -> String {
    // Direct model override on step takes highest precedence
    if let Some(ref model) = step.model {
        return model.clone();
    }
    // Direct model override on defaults
    if let Some(ref model) = defaults.model {
        // But only if the step doesn't specify a tier
        if step.model_tier.is_none() {
            return model.clone();
        }
    }
    // Map tier to actual model
    let tier = step
        .model_tier
        .as_deref()
        .unwrap_or(defaults.model_tier.as_str());
    match tier {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            warn!(
                "[CHAIN] Unknown model_tier '{}', falling back to primary",
                other
            );
            config.primary_model.clone()
        }
    }
}

/// Resolve temperature from step override or defaults.
fn resolve_temperature(step: &ChainStep, defaults: &ChainDefaults) -> f32 {
    step.temperature.unwrap_or(defaults.temperature)
}

/// Dispatch a step to the LLM, with JSON-retry at temp 0.1 on parse failure.
///
/// If the step specifies a `model:` override, creates a modified LlmConfig
/// so the override is actually used by call_model().
async fn dispatch_llm(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &ChainDefaults,
    ctx: &StepContext,
) -> Result<Value> {
    let temperature = resolve_temperature(step, defaults);
    let resolved_model = resolve_model(step, defaults, &ctx.config);
    let resolved_limit = resolve_context_limit(step, defaults, &ctx.config);
    let max_tokens: usize = 100_000;

    // Apply model override: if the resolved model differs from the config's
    // primary model, create a modified config so call_model() uses it.
    // IMPORTANT: also override primary_context_limit so the cascade logic in
    // call_model_unified compares against the *resolved* model's capacity.
    let config_ref;
    let overridden_config;
    if resolved_model != ctx.config.primary_model
        || resolved_limit != ctx.config.primary_context_limit
    {
        overridden_config = LlmConfig {
            primary_model: resolved_model.clone(),
            primary_context_limit: resolved_limit,
            ..ctx.config.clone()
        };
        config_ref = &overridden_config;
    } else {
        config_ref = &ctx.config;
    }

    // Build user prompt from resolved input
    let user_prompt =
        serde_json::to_string_pretty(resolved_input).unwrap_or_else(|_| resolved_input.to_string());

    info!(
        "[CHAIN] step '{}' → LLM (temp={}, model={}, prompt_len={})",
        step.name,
        temperature,
        short_model_name(&resolved_model),
        user_prompt.len()
    );

    // If step has a response_schema, use structured outputs for guaranteed JSON
    if let Some(ref schema) = step.response_schema {
        let schema_name = step.name.replace('-', "_");
        info!(
            "[CHAIN] step '{}' → using structured output (schema: {})",
            step.name, schema_name
        );
        let response = llm::call_model_structured(
            config_ref,
            system_prompt,
            &user_prompt,
            temperature,
            max_tokens,
            schema,
            &schema_name,
        )
        .await?;
        return llm::extract_json(&response).map_err(|e| {
            anyhow!(
                "Step '{}': structured output JSON parse failed: {}",
                step.name,
                e
            )
        });
    }

    // Standard path: call model, parse JSON, retry at temp 0.1 on failure
    let response = llm::call_model(
        config_ref,
        system_prompt,
        &user_prompt,
        temperature,
        max_tokens,
    )
    .await?;

    match llm::extract_json(&response) {
        Ok(json) => {
            info!("[CHAIN] step '{}' → JSON parsed OK", step.name);
            Ok(json)
        }
        Err(_first_err) => {
            // JSON-retry guarantee: retry at temperature 0.1
            info!(
                "[CHAIN] step '{}' → JSON parse failed, retrying at temp 0.1",
                step.name
            );
            let retry_response =
                llm::call_model(config_ref, system_prompt, &user_prompt, 0.1, max_tokens).await?;

            llm::extract_json(&retry_response).map_err(|e| {
                anyhow!(
                    "Step '{}': JSON parse failed after retry at temp 0.1: {}",
                    step.name,
                    e
                )
            })
        }
    }
}

/// Short display name for a model string (last segment after /).
fn short_model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

// ── Mechanical dispatch ─────────────────────────────────────────────────────

/// Known mechanical function names for the v1 registry.
const MECHANICAL_FUNCTIONS: &[&str] = &[
    "extract_import_graph",
    "extract_mechanical_metadata",
    "cluster_by_imports",
    "cluster_by_entity_overlap",
];

/// Dispatch a mechanical step to a named Rust function.
///
/// For v1, the actual build.rs functions require signatures that don't match
/// the generic `(input: &Value, ctx: &StepContext) -> Result<Value>` contract.
/// The dispatch framework is established here; actual wiring happens in Phase 5
/// when the chain executor replaces the hardcoded build pipeline.
fn dispatch_mechanical(function_name: &str, input: &Value, ctx: &StepContext) -> Result<Value> {
    match function_name {
        "extract_import_graph" => {
            info!("[mechanical] extract_import_graph (placeholder)");
            // Phase 5: wire to build::extract_import_graph(conn, slug, writer_tx)
            // For now, return a stub that matches the ImportGraph shape
            Ok(serde_json::json!({
                "_mechanical": "extract_import_graph",
                "_status": "placeholder",
                "slug": ctx.slug,
                "input": input,
            }))
        }
        "extract_mechanical_metadata" => {
            info!("[mechanical] extract_mechanical_metadata (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "extract_mechanical_metadata",
                "_status": "placeholder",
                "slug": ctx.slug,
                "input": input,
            }))
        }
        "cluster_by_imports" => {
            info!("[mechanical] cluster_by_imports (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "cluster_by_imports",
                "_status": "placeholder",
                "input": input,
            }))
        }
        "cluster_by_entity_overlap" => {
            info!("[mechanical] cluster_by_entity_overlap (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "cluster_by_entity_overlap",
                "_status": "placeholder",
                "input": input,
            }))
        }
        unknown => Err(anyhow!("Unknown mechanical function: {}", unknown)),
    }
}

/// Check whether a function name is a known mechanical function.
pub fn is_known_mechanical_function(name: &str) -> bool {
    MECHANICAL_FUNCTIONS.contains(&name)
}

// ── Node builder ────────────────────────────────────────────────────────────

/// Convert LLM step output into a PyramidNode.
///
/// Maps standard LLM output fields (distilled, corrections, decisions, terms,
/// topics, headline, dead_ends, self_prompt/orientation) into the PyramidNode
/// struct. Follows the same pattern as `node_from_analysis` in build.rs.
pub fn build_node_from_output(
    output: &Value,
    node_id: &str,
    slug: &str,
    depth: i64,
    chunk_index: Option<i64>,
) -> Result<PyramidNode> {
    // Extract distilled text (try multiple field names for compatibility)
    let distilled = output
        .get("orientation")
        .or_else(|| output.get("distilled"))
        .or_else(|| output.get("purpose"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract self_prompt (try "orientation" first, then "self_prompt")
    let self_prompt = output
        .get("orientation")
        .or_else(|| output.get("self_prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract dead_ends
    let dead_ends: Vec<String> = output
        .get("dead_ends")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract topics (deserialize from JSON array)
    let topics: Vec<Topic> = output
        .get("topics")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Extract corrections from top-level and from topics
    let mut corrections: Vec<Correction> = Vec::new();
    let mut decisions: Vec<Decision> = Vec::new();

    // Top-level corrections
    if let Some(corrs) = output.get("corrections").and_then(|c| c.as_array()) {
        for c in corrs {
            corrections.push(Correction {
                wrong: c.get("wrong").and_then(|v| v.as_str()).unwrap_or("").into(),
                right: c.get("right").and_then(|v| v.as_str()).unwrap_or("").into(),
                who: c.get("who").and_then(|v| v.as_str()).unwrap_or("").into(),
            });
        }
    }

    // Top-level decisions
    if let Some(decs) = output.get("decisions").and_then(|d| d.as_array()) {
        for d in decs {
            decisions.push(Decision {
                decided: d
                    .get("decided")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                rejected: d
                    .get("rejected")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
            });
        }
    }

    // Also pull corrections/decisions from within topics
    if let Some(topics_arr) = output.get("topics").and_then(|t| t.as_array()) {
        for topic in topics_arr {
            if let Some(corrs) = topic.get("corrections").and_then(|c| c.as_array()) {
                for c in corrs {
                    corrections.push(Correction {
                        wrong: c.get("wrong").and_then(|v| v.as_str()).unwrap_or("").into(),
                        right: c.get("right").and_then(|v| v.as_str()).unwrap_or("").into(),
                        who: c.get("who").and_then(|v| v.as_str()).unwrap_or("").into(),
                    });
                }
            }
            if let Some(decs) = topic.get("decisions").and_then(|d| d.as_array()) {
                for d in decs {
                    decisions.push(Decision {
                        decided: d
                            .get("decided")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                        rejected: d
                            .get("rejected")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                    });
                }
            }
        }
    }

    // Extract terms
    let terms: Vec<Term> = output
        .get("terms")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Headline via shared naming utility
    let headline = headline_from_analysis(output, node_id);

    Ok(PyramidNode {
        id: node_id.to_string(),
        slug: slug.to_string(),
        depth,
        chunk_index,
        headline,
        distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends,
        self_prompt,
        children: output
            .get("source_nodes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| normalize_node_id(s)))
                    .collect()
            })
            .unwrap_or_default(),
        parent_id: None,
        superseded_by: None,
        created_at: String::new(),
    })
}

// ── Node ID normalization ────────────────────────────────────────────────────

/// Normalize a node ID to match the zero-padded format used by generate_node_id.
///
/// LLMs sometimes return unpadded IDs like "C-L0-70" when the actual node is
/// "C-L0-070". This function detects the pattern and zero-pads the numeric
/// suffix to 3 digits.
///
/// Examples:
/// - "C-L0-70" → "C-L0-070"
/// - "C-L0-5"  → "C-L0-005"
/// - "C-L0-070" → "C-L0-070" (already correct)
/// - "L1-003" → "L1-003" (already correct)
/// - "L2-1" → "L2-001"
pub(crate) fn normalize_node_id(id: &str) -> String {
    // Match patterns like "PREFIX-DIGITS" where prefix contains letters/hyphens
    if let Some(last_dash) = id.rfind('-') {
        let prefix = &id[..last_dash];
        let suffix = &id[last_dash + 1..];
        if let Ok(num) = suffix.parse::<u32>() {
            // Only pad if suffix is purely numeric and shorter than 3 digits
            if suffix.len() < 3 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return format!("{}-{:03}", prefix, num);
            }
        }
    }
    id.to_string()
}

// ── Node ID generation ──────────────────────────────────────────────────────

/// Generate a node ID from a pattern with substitution.
///
/// Supported placeholders:
/// - `{index:03}` → zero-padded index (3 digits)
/// - `{index:04}` → zero-padded index (4 digits)
/// - `{index}`    → unpadded index
/// - `{depth}`    → current depth
///
/// Examples:
/// - `"L0-{index:03}"` with index=5 → `"L0-005"`
/// - `"L{depth}-{index:03}"` with depth=3, index=2 → `"L3-002"`
pub fn generate_node_id(pattern: &str, index: usize, depth: Option<i64>) -> String {
    let mut result = pattern.to_string();

    // Replace {depth} first (before index patterns that might contain digits)
    if let Some(d) = depth {
        result = result.replace("{depth}", &d.to_string());
    }

    // Replace {index:0N} patterns (zero-padded)
    if let Some(start) = result.find("{index:0") {
        if let Some(end) = result[start..].find('}') {
            let spec = &result[start..start + end + 1];
            // Extract padding width from {index:0N}
            let width_str = &spec["{index:0".len()..spec.len() - 1];
            if let Ok(width) = width_str.parse::<usize>() {
                let formatted = format!("{:0>width$}", index, width = width);
                result = result.replace(spec, &formatted);
            }
        }
    }

    // Replace bare {index}
    result = result.replace("{index}", &index.to_string());

    result
}

// ── IR Dispatch Layer ────────────────────────────────────────────────────────
//
// New dispatch functions for the IR execution path (P1.4 Task B).
// These read from `execution_plan::Step` + `ModelRequirements` instead of
// `ChainStep` + `ChainDefaults`. The legacy functions above are untouched.

/// Resolve the model string from IR `ModelRequirements` and config.
///
/// Priority:
/// 1. `reqs.model` — direct model override on the step
/// 2. `reqs.tier` — mapped through config tiers
/// 3. Falls back to primary_model when tier is absent or unrecognized
pub fn resolve_ir_model(reqs: &ModelRequirements, config: &LlmConfig) -> String {
    // Direct model override takes highest precedence
    if let Some(ref model) = reqs.model {
        return model.clone();
    }
    // Map tier to actual model
    let tier = reqs.tier.as_deref().unwrap_or("mid");
    match tier {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            warn!(
                "[IR] Unknown model_tier '{}', falling back to primary",
                other
            );
            config.primary_model.clone()
        }
    }
}

/// Resolve the primary context limit (in estimated tokens) for an IR step's model.
///
/// When a step overrides the model via tier or direct model string, the context
/// limit must match the resolved model — otherwise the cascade logic in
/// `call_model_unified` compares input size against the *original* config's
/// primary_context_limit and may incorrectly fall back to a model that ignores
/// response_format/response_schema.
fn resolve_ir_context_limit(reqs: &ModelRequirements, config: &LlmConfig) -> usize {
    // Direct model override — we don't know the model's actual limit, so use a
    // generous 1M (covers most large-context models on OpenRouter).
    if reqs.model.is_some() {
        return 1_000_000;
    }
    let tier = reqs.tier.as_deref().unwrap_or("mid");
    match tier {
        "low" | "mid" => config.primary_context_limit,
        "high" => 1_000_000,
        "max" => 2_000_000,
        _ => config.primary_context_limit,
    }
}

/// Resolve the primary context limit for a legacy chain step's model.
///
/// Same purpose as `resolve_ir_context_limit` but for the legacy `ChainStep` /
/// `ChainDefaults` dispatch path.
fn resolve_context_limit(step: &ChainStep, defaults: &ChainDefaults, config: &LlmConfig) -> usize {
    // Direct model override on step or defaults
    if step.model.is_some() {
        return 1_000_000;
    }
    if step.model_tier.is_none() && defaults.model.is_some() {
        return 1_000_000;
    }
    let tier = step
        .model_tier
        .as_deref()
        .unwrap_or(defaults.model_tier.as_str());
    match tier {
        "low" | "mid" => config.primary_context_limit,
        "high" => 1_000_000,
        "max" => 2_000_000,
        _ => config.primary_context_limit,
    }
}

/// Resolve temperature from IR `ModelRequirements`, with a default of 0.3.
fn resolve_ir_temperature(reqs: &ModelRequirements) -> f32 {
    reqs.temperature.unwrap_or(0.3)
}

fn resolve_ir_max_tokens(step: &Step) -> usize {
    let _ = step;
    100_000
}

fn resolve_ir_llm_call_options(step: &Step) -> llm::LlmCallOptions {
    let min_timeout_secs = if step.response_schema.is_some() {
        match step.primitive.as_deref() {
            Some("classify") => Some(420),
            Some("web") => Some(240),
            _ => Some(180),
        }
    } else {
        None
    };

    llm::LlmCallOptions { min_timeout_secs }
}

/// Dispatch an IR Step to the appropriate execution path.
///
/// Routes based on `step.operation`:
/// - `Llm` → `dispatch_ir_llm`
/// - `Transform` → `transform_runtime::execute_transform`
/// - `Mechanical` → `dispatch_ir_mechanical`
/// - `Wire | Task | Game` → error (Phase 4)
///
/// For LLM steps, returns `(parsed_output, Some(LlmResponse))`.
/// For non-LLM steps, returns `(output, None)`.
pub async fn dispatch_ir_step(
    step: &Step,
    resolved_input: &Value,
    system_prompt: &str,
    ctx: &StepContext,
) -> Result<(Value, Option<LlmResponse>)> {
    match step.operation {
        StepOperation::Llm => {
            let (value, response) =
                dispatch_ir_llm(step, resolved_input, system_prompt, ctx).await?;
            Ok((value, Some(response)))
        }
        StepOperation::Transform => {
            let spec = step.transform.as_ref().ok_or_else(|| {
                anyhow!(
                    "IR step '{}' is Transform but has no transform spec",
                    step.id
                )
            })?;
            info!("[IR] step '{}' → transform '{}'", step.id, spec.function);
            let env = ValueEnv::new(resolved_input);
            let resolved_args = transform_runtime::resolve_transform_args(&spec.args, &env)?;
            let result =
                transform_runtime::execute_transform_function(&spec.function, &resolved_args)?;
            Ok((result, None))
        }
        StepOperation::Mechanical => {
            let result = dispatch_ir_mechanical(step, resolved_input, ctx)?;
            Ok((result, None))
        }
        StepOperation::Wire | StepOperation::Task | StepOperation::Game => Err(anyhow!(
            "IR step '{}': operation {:?} not implemented in local executor (Phase 4)",
            step.id,
            step.operation
        )),
    }
}

/// Dispatch an IR LLM step: resolve model from ModelRequirements, call
/// `call_model_unified`, parse JSON output, retry at temp 0.1 on parse failure.
///
/// Returns `(parsed_json, LlmResponse)` so the caller can log costs from the
/// LlmResponse (usage, generation_id).
pub async fn dispatch_ir_llm(
    step: &Step,
    resolved_input: &Value,
    system_prompt: &str,
    ctx: &StepContext,
) -> Result<(Value, LlmResponse)> {
    let temperature = resolve_ir_temperature(&step.model_requirements);
    let resolved_model = resolve_ir_model(&step.model_requirements, &ctx.config);
    let resolved_limit = resolve_ir_context_limit(&step.model_requirements, &ctx.config);
    let max_tokens = resolve_ir_max_tokens(step);
    let llm_options = resolve_ir_llm_call_options(step);

    // Apply model override: if the resolved model differs from the config's
    // primary model, create a modified config so call_model_unified uses it.
    // IMPORTANT: also override primary_context_limit so the cascade logic in
    // call_model_unified compares against the *resolved* model's capacity,
    // not the original config's (which may be much smaller).
    let config_ref;
    let overridden_config;
    if resolved_model != ctx.config.primary_model
        || resolved_limit != ctx.config.primary_context_limit
    {
        overridden_config = LlmConfig {
            primary_model: resolved_model.clone(),
            primary_context_limit: resolved_limit,
            ..ctx.config.clone()
        };
        config_ref = &overridden_config;
    } else {
        config_ref = &ctx.config;
    }

    let raw_input_len = serde_json::to_string(resolved_input)
        .unwrap_or_default()
        .len();
    info!(
        "[IR] step '{}' compact_inputs={}, raw_input_len={}",
        step.id, step.compact_inputs, raw_input_len
    );

    // Build user prompt from resolved input
    let user_prompt =
        serde_json::to_string_pretty(resolved_input).unwrap_or_else(|_| resolved_input.to_string());

    info!(
        "[IR] step '{}' → LLM (temp={}, model={}, ctx_limit={}, prompt_len={}, max_tokens={}, timeout_floor={:?})",
        step.id,
        temperature,
        short_model_name(&resolved_model),
        resolved_limit,
        user_prompt.len(),
        max_tokens,
        llm_options.min_timeout_secs
    );

    // If step has a response_schema, use structured outputs for guaranteed JSON
    if let Some(ref schema) = step.response_schema {
        let schema_name = step.id.replace('-', "_").replace('.', "_");
        info!(
            "[IR] step '{}' → using structured output (schema: {}, schema_type: {:?})",
            step.id,
            schema_name,
            schema.get("type").and_then(|v| v.as_str()),
        );
        let response_format = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": schema_name,
                "strict": true,
                "schema": schema
            }
        });
        let response = llm::call_model_unified_with_options(
            config_ref,
            system_prompt,
            &user_prompt,
            temperature,
            max_tokens,
            Some(&response_format),
            llm_options,
        )
        .await?;
        let parsed = llm::extract_json(&response.content).map_err(|e| {
            anyhow!(
                "IR step '{}': structured output JSON parse failed: {}",
                step.id,
                e
            )
        })?;
        return Ok((parsed, response));
    }

    // No response_schema — standard path without structured output enforcement
    info!(
        "[IR] step '{}' → no response_schema, using standard JSON extraction",
        step.id,
    );

    // Standard path: call model, parse JSON, retry at temp 0.1 on failure
    let response = llm::call_model_unified_with_options(
        config_ref,
        system_prompt,
        &user_prompt,
        temperature,
        max_tokens,
        None,
        llm_options,
    )
    .await?;

    match llm::extract_json(&response.content) {
        Ok(json) => {
            info!("[IR] step '{}' → JSON parsed OK", step.id);
            Ok((json, response))
        }
        Err(_first_err) => {
            // JSON-retry guarantee: retry at temperature 0.1
            info!(
                "[IR] step '{}' → JSON parse failed, retrying at temp 0.1",
                step.id
            );
            let retry_response = llm::call_model_unified_with_options(
                config_ref,
                system_prompt,
                &user_prompt,
                0.1,
                max_tokens,
                None,
                llm_options,
            )
            .await?;

            let parsed = llm::extract_json(&retry_response.content).map_err(|e| {
                anyhow!(
                    "IR step '{}': JSON parse failed after retry at temp 0.1: {}",
                    step.id,
                    e
                )
            })?;
            Ok((parsed, retry_response))
        }
    }
}

/// Dispatch an IR mechanical step: look up `step.rust_function` in the registry.
///
/// Same registry as the legacy `dispatch_mechanical` but reads from IR types.
pub fn dispatch_ir_mechanical(
    step: &Step,
    resolved_input: &Value,
    ctx: &StepContext,
) -> Result<Value> {
    let fn_name = step
        .rust_function
        .as_deref()
        .ok_or_else(|| anyhow!("IR mechanical step '{}' missing rust_function", step.id))?;
    info!("[IR] step '{}' → mechanical fn '{}'", step.id, fn_name);
    dispatch_mechanical(fn_name, resolved_input, ctx)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::execution_plan::{
        ErrorPolicy, ModelRequirements, Step, StepOperation, TransformSpec,
    };
    use super::*;

    #[test]
    fn test_generate_node_id_basic() {
        assert_eq!(generate_node_id("L0-{index:03}", 5, None), "L0-005");
        assert_eq!(generate_node_id("L0-{index:03}", 42, None), "L0-042");
        assert_eq!(generate_node_id("L0-{index:03}", 0, None), "L0-000");
    }

    #[test]
    fn test_generate_node_id_with_depth() {
        assert_eq!(
            generate_node_id("L{depth}-{index:03}", 2, Some(3)),
            "L3-002"
        );
        assert_eq!(
            generate_node_id("L{depth}-{index:03}", 0, Some(0)),
            "L0-000"
        );
    }

    #[test]
    fn test_generate_node_id_bare_index() {
        assert_eq!(generate_node_id("node-{index}", 7, None), "node-7");
    }

    #[test]
    fn test_generate_node_id_four_digit_pad() {
        assert_eq!(generate_node_id("N{index:04}", 3, None), "N0003");
    }

    #[test]
    fn test_normalize_node_id() {
        // Unpadded → padded
        assert_eq!(normalize_node_id("C-L0-70"), "C-L0-070");
        assert_eq!(normalize_node_id("C-L0-5"), "C-L0-005");
        assert_eq!(normalize_node_id("L2-1"), "L2-001");
        // Already padded → unchanged
        assert_eq!(normalize_node_id("C-L0-070"), "C-L0-070");
        assert_eq!(normalize_node_id("L1-003"), "L1-003");
        // No numeric suffix → unchanged
        assert_eq!(normalize_node_id("apex"), "apex");
    }

    #[test]
    fn test_is_known_mechanical_function() {
        assert!(is_known_mechanical_function("extract_import_graph"));
        assert!(is_known_mechanical_function("cluster_by_imports"));
        assert!(!is_known_mechanical_function("nonexistent_function"));
    }

    #[test]
    fn test_dispatch_mechanical_unknown_fn() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let result = dispatch_mechanical("nonexistent", &serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown mechanical function"));
    }

    #[test]
    fn test_dispatch_mechanical_known_fn() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test-slug".into(),
            config: LlmConfig::default(),
        };
        let input = serde_json::json!({"files": ["main.rs"]});
        let result = dispatch_mechanical("extract_import_graph", &input, &ctx).unwrap();
        assert_eq!(result["_mechanical"], "extract_import_graph");
        assert_eq!(result["_status"], "placeholder");
        assert_eq!(result["slug"], "test-slug");
    }

    #[test]
    fn test_build_node_from_output_minimal() {
        let output = serde_json::json!({
            "headline": "Test Node",
            "distilled": "A test distillation.",
            "topics": [],
            "corrections": [],
            "decisions": [],
            "terms": [],
            "dead_ends": [],
        });
        let node = build_node_from_output(&output, "L0-001", "test-slug", 0, Some(1)).unwrap();
        assert_eq!(node.id, "L0-001");
        assert_eq!(node.slug, "test-slug");
        assert_eq!(node.depth, 0);
        assert_eq!(node.chunk_index, Some(1));
        assert_eq!(node.headline, "Test Node");
        assert_eq!(node.distilled, "A test distillation.");
        assert!(node.children.is_empty());
        assert!(node.parent_id.is_none());
    }

    #[test]
    fn test_build_node_from_output_with_orientation() {
        // "orientation" takes precedence over "distilled"
        let output = serde_json::json!({
            "orientation": "Orientation text",
            "distilled": "Should not be used",
            "headline": "Node",
        });
        let node = build_node_from_output(&output, "L1-000", "s", 1, None).unwrap();
        assert_eq!(node.distilled, "Orientation text");
        assert_eq!(node.self_prompt, "Orientation text");
    }

    #[test]
    fn test_resolve_model_step_override() {
        let step = ChainStep {
            name: "test".into(),
            primitive: "compress".into(),
            model: Some("custom/model".into()),
            model_tier: None,
            instruction: Some("x".into()),
            mechanical: false,
            rust_function: None,
            input: None,
            output_schema: None,
            temperature: None,
            sequential: false,
            accumulate: None,
            for_each: None,
            concurrency: 1,
            pair_adjacent: false,
            recursive_pair: false,
            recursive_cluster: false,
            cluster_instruction: None,
            cluster_model: None,
            cluster_response_schema: None,
            target_clusters: None,
            response_schema: None,
            batch_threshold: None,
            merge_instruction: None,
            instruction_map: None,
            max_thread_size: None,
            context: None,
            compact_inputs: false,
            when: None,
            on_error: None,
            save_as: None,
            node_id_pattern: None,
            depth: None,
        };
        let defaults = ChainDefaults {
            model_tier: "mid".into(),
            model: None,
            temperature: 0.3,
            on_error: "retry(2)".into(),
        };
        let config = LlmConfig::default();
        assert_eq!(resolve_model(&step, &defaults, &config), "custom/model");
    }

    #[test]
    fn test_resolve_model_tier_mapping() {
        let make_step = |tier: &str| ChainStep {
            name: "test".into(),
            primitive: "compress".into(),
            model: None,
            model_tier: Some(tier.into()),
            instruction: Some("x".into()),
            mechanical: false,
            rust_function: None,
            input: None,
            output_schema: None,
            temperature: None,
            sequential: false,
            accumulate: None,
            for_each: None,
            concurrency: 1,
            pair_adjacent: false,
            recursive_pair: false,
            recursive_cluster: false,
            cluster_instruction: None,
            cluster_model: None,
            cluster_response_schema: None,
            target_clusters: None,
            response_schema: None,
            batch_threshold: None,
            merge_instruction: None,
            instruction_map: None,
            max_thread_size: None,
            context: None,
            compact_inputs: false,
            when: None,
            on_error: None,
            save_as: None,
            node_id_pattern: None,
            depth: None,
        };
        let defaults = ChainDefaults {
            model_tier: "mid".into(),
            model: None,
            temperature: 0.3,
            on_error: "retry(2)".into(),
        };
        let config = LlmConfig::default();

        assert_eq!(
            resolve_model(&make_step("low"), &defaults, &config),
            config.primary_model
        );
        assert_eq!(
            resolve_model(&make_step("mid"), &defaults, &config),
            config.primary_model
        );
        assert_eq!(
            resolve_model(&make_step("high"), &defaults, &config),
            config.fallback_model_1
        );
        assert_eq!(
            resolve_model(&make_step("max"), &defaults, &config),
            config.fallback_model_2
        );
    }

    // ── IR dispatch tests ───────────────────────────────────────────────────

    /// Helper to build a minimal IR Step for testing.
    fn ir_step(id: &str, op: StepOperation) -> Step {
        Step {
            id: id.to_string(),
            operation: op,
            primitive: None,
            depends_on: vec![],
            iteration: None,
            input: serde_json::json!({}),
            instruction: Some("test prompt".to_string()),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: super::super::execution_plan::CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: None,
            converge_metadata: None,
            metadata: None,
            scope: None,
        }
    }

    #[test]
    fn test_resolve_ir_model_direct_override() {
        let reqs = ModelRequirements {
            tier: Some("low".into()),
            model: Some("custom/my-model".into()),
            temperature: None,
        };
        let config = LlmConfig::default();
        // Direct model override wins over tier
        assert_eq!(resolve_ir_model(&reqs, &config), "custom/my-model");
    }

    #[test]
    fn test_resolve_ir_model_tier_mapping() {
        let config = LlmConfig::default();

        let make_reqs = |tier: &str| ModelRequirements {
            tier: Some(tier.into()),
            model: None,
            temperature: None,
        };

        assert_eq!(
            resolve_ir_model(&make_reqs("low"), &config),
            config.primary_model
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("mid"), &config),
            config.primary_model
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("high"), &config),
            config.fallback_model_1
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("max"), &config),
            config.fallback_model_2
        );
    }

    #[test]
    fn test_resolve_ir_model_default_tier() {
        // When tier is None, defaults to "mid" → primary_model
        let reqs = ModelRequirements::default();
        let config = LlmConfig::default();
        assert_eq!(resolve_ir_model(&reqs, &config), config.primary_model);
    }

    #[test]
    fn test_resolve_ir_model_unknown_tier() {
        let reqs = ModelRequirements {
            tier: Some("ultra".into()),
            model: None,
            temperature: None,
        };
        let config = LlmConfig::default();
        // Unknown tier falls back to primary
        assert_eq!(resolve_ir_model(&reqs, &config), config.primary_model);
    }

    #[test]
    fn test_resolve_ir_temperature_override() {
        let reqs = ModelRequirements {
            tier: None,
            model: None,
            temperature: Some(0.7),
        };
        assert_eq!(resolve_ir_temperature(&reqs), 0.7);
    }

    #[test]
    fn test_resolve_ir_temperature_default() {
        let reqs = ModelRequirements::default();
        assert_eq!(resolve_ir_temperature(&reqs), 0.3);
    }

    #[test]
    fn test_resolve_ir_timeout_floor_for_structured_classify() {
        let mut step = ir_step("clustering", StepOperation::Llm);
        step.primitive = Some("classify".to_string());
        step.response_schema = Some(serde_json::json!({"type": "object"}));

        assert_eq!(resolve_ir_max_tokens(&step), 100_000);
        assert_eq!(
            resolve_ir_llm_call_options(&step).min_timeout_secs,
            Some(420)
        );
    }

    #[test]
    fn test_resolve_ir_llm_defaults_for_unstructured_steps() {
        let step = ir_step("l1_synthesis", StepOperation::Llm);
        assert_eq!(resolve_ir_max_tokens(&step), 100_000);
        assert_eq!(resolve_ir_llm_call_options(&step).min_timeout_secs, None);
    }

    #[test]
    fn test_dispatch_ir_mechanical_routes_correctly() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "ir-test".into(),
            config: LlmConfig::default(),
        };
        let mut step = ir_step("mech_step", StepOperation::Mechanical);
        step.rust_function = Some("extract_import_graph".into());
        let input = serde_json::json!({"files": ["lib.rs"]});
        let result = dispatch_ir_mechanical(&step, &input, &ctx).unwrap();
        assert_eq!(result["_mechanical"], "extract_import_graph");
        assert_eq!(result["_status"], "placeholder");
        assert_eq!(result["slug"], "ir-test");
    }

    #[test]
    fn test_dispatch_ir_mechanical_missing_fn_name() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let step = ir_step("no_fn", StepOperation::Mechanical);
        // rust_function is None
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing rust_function"));
    }

    #[test]
    fn test_dispatch_ir_mechanical_unknown_fn() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let mut step = ir_step("bad_fn", StepOperation::Mechanical);
        step.rust_function = Some("nonexistent_fn".into());
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown mechanical function"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_routes() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let mut step = ir_step("count_step", StepOperation::Transform);
        step.transform = Some(TransformSpec {
            function: "count".into(),
            args: serde_json::json!({"collection": [1, 2, 3]}),
        });
        let (result, llm_resp) = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx)
            .await
            .unwrap();
        assert_eq!(result, serde_json::json!(3));
        assert!(llm_resp.is_none()); // transforms don't produce LlmResponse
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_resolves_args_against_input() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let mut step = ir_step("coalesce_step", StepOperation::Transform);
        step.transform = Some(TransformSpec {
            function: "coalesce".into(),
            args: serde_json::json!({
                "values": ["$primary", "$fallback"]
            }),
        });
        let input = serde_json::json!({
            "primary": null,
            "fallback": [1, 2, 3]
        });
        let (result, llm_resp) = dispatch_ir_step(&step, &input, "", &ctx).await.unwrap();
        assert_eq!(result, serde_json::json!([1, 2, 3]));
        assert!(llm_resp.is_none());
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_missing_spec() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let step = ir_step("bad_transform", StepOperation::Transform);
        // transform is None
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no transform spec"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_wire_not_implemented() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let step = ir_step("wire_step", StepOperation::Wire);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_task_not_implemented() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let step = ir_step("task_step", StepOperation::Task);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_game_not_implemented() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
        };
        let step = ir_step("game_step", StepOperation::Game);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_mechanical_routes() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "slug".into(),
            config: LlmConfig::default(),
        };
        let mut step = ir_step("mech", StepOperation::Mechanical);
        step.rust_function = Some("extract_mechanical_metadata".into());
        let (result, llm_resp) = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx)
            .await
            .unwrap();
        assert_eq!(result["_mechanical"], "extract_mechanical_metadata");
        assert!(llm_resp.is_none()); // mechanical steps don't produce LlmResponse
    }
}
