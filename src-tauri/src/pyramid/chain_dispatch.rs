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
use super::llm::{self, LlmConfig};
use super::naming::headline_from_analysis;
use super::types::{Correction, Decision, PyramidNode, Term, Topic};

// ── Step context ────────────────────────────────────────────────────────────

/// Context available to all chain steps during execution.
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
        info!(
            "[CHAIN] step '{}' → mechanical fn '{}'",
            step.name, fn_name
        );
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
async fn dispatch_llm(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &ChainDefaults,
    ctx: &StepContext,
) -> Result<Value> {
    let temperature = resolve_temperature(step, defaults);
    let _model = resolve_model(step, defaults, &ctx.config);
    let max_tokens: usize = 4096;

    // Build user prompt from resolved input
    let user_prompt = serde_json::to_string_pretty(resolved_input)
        .unwrap_or_else(|_| resolved_input.to_string());

    info!(
        "[CHAIN] step '{}' → LLM (temp={}, prompt_len={})",
        step.name,
        temperature,
        user_prompt.len()
    );

    // First attempt at configured temperature
    let response = llm::call_model(&ctx.config, system_prompt, &user_prompt, temperature, max_tokens).await?;

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
                llm::call_model(&ctx.config, system_prompt, &user_prompt, 0.1, max_tokens).await?;

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
fn dispatch_mechanical(
    function_name: &str,
    input: &Value,
    ctx: &StepContext,
) -> Result<Value> {
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
    // Extract distilled text (try "orientation" first, then "distilled")
    let distilled = output
        .get("orientation")
        .or_else(|| output.get("distilled"))
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
        children: Vec::new(),
        parent_id: None,
        superseded_by: None,
        created_at: String::new(),
    })
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
    fn test_is_known_mechanical_function() {
        assert!(is_known_mechanical_function("extract_import_graph"));
        assert!(is_known_mechanical_function("cluster_by_imports"));
        assert!(!is_known_mechanical_function("nonexistent_function"));
    }

    #[test]
    fn test_dispatch_mechanical_unknown_fn() {
        let ctx = StepContext {
            db_reader: Arc::new(Mutex::new(
                Connection::open_in_memory().unwrap(),
            )),
            db_writer: Arc::new(Mutex::new(
                Connection::open_in_memory().unwrap(),
            )),
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
            db_reader: Arc::new(Mutex::new(
                Connection::open_in_memory().unwrap(),
            )),
            db_writer: Arc::new(Mutex::new(
                Connection::open_in_memory().unwrap(),
            )),
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
            pair_adjacent: false,
            recursive_pair: false,
            batch_threshold: None,
            merge_instruction: None,
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
            pair_adjacent: false,
            recursive_pair: false,
            batch_threshold: None,
            merge_instruction: None,
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
}
