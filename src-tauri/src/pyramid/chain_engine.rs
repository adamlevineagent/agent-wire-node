// pyramid/chain_engine.rs — Chain definition data model + validator
//
// Defines the YAML-driven chain schema for configurable pyramid build pipelines.
// See docs/plans/action-chain-refactor-v3.md for full specification.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Wire primitives (28 + escape hatch) ──────────────────────────────────

pub const VALID_PRIMITIVES: &[&str] = &[
    // Perception
    "ingest",
    "extract",
    "classify",
    "detect",
    // Judgment
    "evaluate",
    "compare",
    "verify",
    "calibrate",
    "interrogate",
    // Synthesis
    "pitch",
    "draft",
    "synthesize",
    "translate",
    "analogize",
    "compress",
    "fuse",
    // Adversarial
    "review",
    "fact_check",
    "rebut",
    "steelman",
    "strawman",
    // Temporal
    "timeline",
    "monitor",
    "decay",
    "diff",
    // Relational
    "relate",
    "cross_reference",
    "map",
    "web",
    // Meta
    "price",
    "metabolize",
    "embody",
    // Sub-chain flow control
    "container",
    "split",
    "loop",
    "gate",
    // Recipe primitives
    "recursive_decompose",
    "evidence_loop",
    "cross_build_input",
    "process_gaps",
    // Escape hatch
    "custom",
];

// ── Default value functions for serde ────────────────────────────────────

fn default_model_tier() -> String {
    "mid".into()
}

fn default_temperature() -> f32 {
    0.3
}

fn default_on_error() -> String {
    "retry(2)".into()
}

fn default_concurrency() -> usize {
    1
}

fn default_compact_inputs() -> bool {
    false
}

// ── Chain definition structs ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainDefinition {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub content_type: String,
    pub version: String,
    pub author: String,
    pub defaults: ChainDefaults,
    pub steps: Vec<ChainStep>,
    #[serde(default)]
    pub post_build: Vec<PostBuildRef>,
    /// WS-AUDIENCE-CONTRACT: optional top-level audience block. When absent,
    /// `Audience::default()` is used. Propagated into the resolution context
    /// as a structured JSON object at `$audience` / `audience` key.
    #[serde(default)]
    pub audience: super::types::Audience,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainDefaults {
    #[serde(default = "default_model_tier")]
    pub model_tier: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_on_error")]
    pub on_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DehydrateStep {
    pub drop: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStep {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub primitive: String,
    #[serde(default)]
    pub instruction: Option<String>,
    /// Get instruction from a prior step's output (e.g., `$extraction_schema.extraction_prompt`).
    #[serde(default)]
    pub instruction_from: Option<String>,
    #[serde(default)]
    pub instruction_map: Option<HashMap<String, String>>,
    #[serde(default)]
    pub mechanical: bool,
    #[serde(default)]
    pub rust_function: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub model_tier: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub accumulate: Option<serde_json::Value>,
    #[serde(default)]
    pub for_each: Option<String>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub max_thread_size: Option<usize>,
    #[serde(default)]
    pub pair_adjacent: bool,
    #[serde(default)]
    pub recursive_pair: bool,
    #[serde(default)]
    pub recursive_cluster: bool,
    #[serde(default)]
    pub cluster_instruction: Option<String>,
    #[serde(default)]
    pub cluster_model: Option<String>,
    #[serde(default)]
    pub cluster_response_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub target_clusters: Option<String>,
    #[serde(default)]
    pub response_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub batch_threshold: Option<usize>,
    #[serde(default)]
    pub merge_instruction: Option<String>,
    #[serde(default)]
    pub batch_size: Option<usize>,
    /// Token-aware batch sizing. When set, batches are filled greedily until
    /// either batch_max_tokens or batch_size would be exceeded.
    #[serde(default)]
    pub batch_max_tokens: Option<usize>,
    /// Field-level projection: only send these top-level fields from each item to the LLM.
    #[serde(default)]
    pub item_fields: Option<Vec<String>>,
    /// Adaptive per-item dehydration cascade. Progressively drops fields from
    /// oversized items until they fit within batch_max_tokens. Mutually exclusive
    /// with item_fields — dehydrate is adaptive, item_fields is uniform.
    #[serde(default)]
    pub dehydrate: Option<Vec<DehydrateStep>>,
    /// When set, skip clustering and go straight to synthesis if node count <= this threshold.
    /// When None, rely on apex_ready signal only (no hardcoded threshold).
    #[serde(default)]
    pub direct_synthesis_threshold: Option<usize>,
    /// What to do when clustering fails to converge: "retry", "force_merge", or "abort".
    #[serde(default)]
    pub convergence_fallback: Option<String>,
    /// What to do when a cluster LLM call fails: "positional(N)", "retry(N)", or "abort".
    #[serde(default)]
    pub cluster_on_error: Option<String>,
    /// Positional fallback group size (only used with cluster_on_error = "positional(N)").
    #[serde(default)]
    pub cluster_fallback_size: Option<usize>,
    /// Field-level projection for the clustering sub-call specifically.
    #[serde(default)]
    pub cluster_item_fields: Option<Vec<String>>,
    /// If a for_each item exceeds this token count, split it into sub-chunks.
    #[serde(default)]
    pub max_input_tokens: Option<usize>,
    /// How to split oversized items: "sections" (default), "lines", or "tokens".
    #[serde(default)]
    pub split_strategy: Option<String>,
    /// Token overlap between adjacent sub-chunks for context continuity.
    #[serde(default)]
    pub split_overlap_tokens: Option<usize>,
    /// Whether to merge sub-chunk extraction results into one output (default: true when max_input_tokens is set).
    #[serde(default)]
    pub split_merge: Option<bool>,
    /// Desired dispatch order for for_each items (e.g., "largest_first").
    /// Parsed from YAML but not yet implemented in the executor.
    #[serde(default)]
    pub dispatch_order: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub on_error: Option<String>,
    #[serde(default)]
    pub on_parse_error: Option<String>,
    #[serde(default)]
    pub heal_instruction: Option<String>,
    #[serde(default)]
    pub save_as: Option<String>,
    #[serde(default)]
    pub node_id_pattern: Option<String>,
    #[serde(default)]
    pub depth: Option<i64>,
    /// Per-item context injection: map of section-label → $step_ref.
    /// Resolved at forEach dispatch time. Array refs are auto-indexed by $index.
    /// Each entry appends a `## {LABEL} CONTEXT` section to the system prompt.
    #[serde(default)]
    pub context: Option<serde_json::Value>,
    #[serde(default = "default_compact_inputs")]
    pub compact_inputs: bool,
    /// 11-E: Declarative enrichments — specifies which runtime enrichments to apply.
    /// Replaces hardcoded step-name checks in chain_executor.rs.
    /// Valid values: "file_level_connections", "cross_thread_connections", "cross_subsystem_connections"
    #[serde(default)]
    pub enrichments: Vec<String>,
    /// Sub-chain: inner steps executed sequentially within this container step.
    #[serde(default)]
    pub steps: Option<Vec<ChainStep>>,
    /// Execution mode for primitives that support multiple behaviors (e.g., "delta" vs "fresh" for recursive_decompose).
    #[serde(default)]
    pub mode: Option<String>,
    /// Loop termination condition (evaluated via evaluate_when). Used with primitive: "loop".
    #[serde(default)]
    pub until: Option<String>,
    /// Gate break signal. When true on a "gate" primitive, exits the enclosing loop.
    /// Named break_loop because "break" is a Rust keyword.
    #[serde(default, rename = "break")]
    pub break_loop: Option<bool>,
}

impl Default for ChainStep {
    fn default() -> Self {
        Self {
            name: String::new(),
            primitive: String::new(),
            instruction: None,
            instruction_from: None,
            instruction_map: None,
            mechanical: false,
            rust_function: None,
            input: None,
            output_schema: None,
            model_tier: None,
            model: None,
            temperature: None,
            sequential: false,
            accumulate: None,
            for_each: None,
            concurrency: default_concurrency(),
            max_thread_size: None,
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
            batch_size: None,
            batch_max_tokens: None,
            item_fields: None,
            dehydrate: None,
            direct_synthesis_threshold: None,
            convergence_fallback: None,
            cluster_on_error: None,
            cluster_fallback_size: None,
            cluster_item_fields: None,
            max_input_tokens: None,
            split_strategy: None,
            split_overlap_tokens: None,
            split_merge: None,
            dispatch_order: None,
            when: None,
            on_error: None,
            on_parse_error: None,
            heal_instruction: None,
            save_as: None,
            node_id_pattern: None,
            depth: None,
            context: None,
            compact_inputs: false,
            enrichments: vec![],
            steps: None,
            mode: None,
            until: None,
            break_loop: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostBuildRef {
    pub chain: String,
    #[serde(default)]
    pub when: Option<String>,
}

/// Metadata for listing chains (doesn't include full step details).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainMetadata {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub version: String,
    pub author: String,
    pub step_count: usize,
    pub file_path: String,
    pub is_default: bool,
}

// ── Validation ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

const VALID_CONTENT_TYPES: &[&str] = &["conversation", "code", "document", "question"];
// Includes the legacy numeric tiers (low/mid/high/max) plus the semantic
// aliases used by the LLM-profile system (extractor/synth_heavy/web).
// See docs/semantic_aliasing_audit_results.md for the rationale.
const VALID_MODEL_TIERS: &[&str] = &[
    "low", "mid", "high", "max",
    "extractor", "synth_heavy", "web",
];

/// Validate a chain definition, returning errors and warnings.
pub fn validate_chain(def: &ChainDefinition) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Schema version
    if def.schema_version != 1 {
        errors.push(format!(
            "schema_version must be 1, got {}",
            def.schema_version
        ));
    }

    // Required top-level fields
    if def.id.is_empty() {
        errors.push("id must be non-empty".into());
    }
    if def.name.is_empty() {
        errors.push("name must be non-empty".into());
    }

    // Content type
    if !VALID_CONTENT_TYPES.contains(&def.content_type.as_str()) {
        errors.push(format!(
            "content_type must be one of {:?}, got \"{}\"",
            VALID_CONTENT_TYPES, def.content_type
        ));
    }

    // At least 1 step
    if def.steps.is_empty() {
        errors.push("chain must have at least 1 step".into());
    }

    // Validate defaults
    validate_on_error(&def.defaults.on_error, "defaults.on_error", &mut errors);
    if !VALID_MODEL_TIERS.contains(&def.defaults.model_tier.as_str()) {
        warnings.push(format!(
            "defaults.model_tier \"{}\" is not a standard tier ({:?})",
            def.defaults.model_tier, VALID_MODEL_TIERS
        ));
    }

    // Step name uniqueness
    let mut seen_names = std::collections::HashSet::new();
    for step in &def.steps {
        if !seen_names.insert(&step.name) {
            errors.push(format!("duplicate step name \"{}\"", step.name));
        }
    }

    // Per-step validation
    for (i, step) in def.steps.iter().enumerate() {
        let prefix = format!("step[{}] \"{}\"", i, step.name);

        // Valid primitive
        if !VALID_PRIMITIVES.contains(&step.primitive.as_str()) {
            errors.push(format!(
                "{}: unknown primitive \"{}\"",
                prefix, step.primitive
            ));
        }

        // Mechanical steps must have rust_function
        if step.mechanical && step.rust_function.is_none() {
            errors.push(format!(
                "{}: mechanical step must specify rust_function",
                prefix
            ));
        }

        // LLM steps (non-mechanical, non-orchestration, non-recipe) must have instruction
        let orchestration = matches!(
            step.primitive.as_str(),
            "container" | "loop" | "gate" | "split"
        );
        let recipe = matches!(
            step.primitive.as_str(),
            "cross_build_input" | "evidence_loop" | "process_gaps" | "recursive_decompose"
        );
        if !step.mechanical && !orchestration && !recipe && step.instruction.is_none() && step.instruction_from.is_none() {
            errors.push(format!("{}: LLM step must specify instruction or instruction_from", prefix));
        }

        // dehydrate and item_fields are mutually exclusive
        if step.dehydrate.is_some() && step.item_fields.is_some() {
            errors.push(format!(
                "{}: dehydrate and item_fields are mutually exclusive",
                prefix
            ));
        }

        // on_error validation
        if let Some(ref on_err) = step.on_error {
            validate_on_error(on_err, &format!("{}.on_error", prefix), &mut errors);
        }

        // recursive_pair, pair_adjacent, and recursive_cluster are mutually exclusive
        let mode_count = [
            step.recursive_pair,
            step.pair_adjacent,
            step.recursive_cluster,
        ]
        .iter()
        .filter(|&&b| b)
        .count();
        if mode_count > 1 {
            errors.push(format!(
                "{}: recursive_pair, pair_adjacent, and recursive_cluster are mutually exclusive",
                prefix
            ));
        }

        // recursive_cluster needs cluster_instruction
        if step.recursive_cluster && step.cluster_instruction.is_none() {
            errors.push(format!(
                "{}: recursive_cluster requires cluster_instruction",
                prefix
            ));
        }

        // sequential only valid with for_each
        if step.sequential && step.for_each.is_none() {
            errors.push(format!("{}: sequential requires for_each", prefix));
        }
        if step.concurrency == 0 {
            errors.push(format!("{}: concurrency must be >= 1", prefix));
        }
        if step.concurrency > 1 && step.for_each.is_none() && step.primitive != "web" {
            errors.push(format!("{}: concurrency > 1 requires for_each (or web primitive)", prefix));
        }
        if step.sequential && step.concurrency > 1 {
            errors.push(format!(
                "{}: sequential steps cannot use concurrency > 1",
                prefix
            ));
        }
        if step.accumulate.is_some() && step.concurrency > 1 {
            errors.push(format!(
                "{}: accumulate cannot be used with concurrency > 1",
                prefix
            ));
        }
        if let Some(max_thread_size) = step.max_thread_size {
            if max_thread_size == 0 {
                errors.push(format!("{}: max_thread_size must be >= 1", prefix));
            }
        }

        // model_tier warning
        if let Some(ref tier) = step.model_tier {
            if !VALID_MODEL_TIERS.contains(&tier.as_str()) {
                warnings.push(format!(
                    "{}: model_tier \"{}\" is not a standard tier",
                    prefix, tier
                ));
            }
        }

        // save_as validation
        if let Some(ref save) = step.save_as {
            const VALID_SAVE_AS: &[&str] = &["node", "web_edges", "step_only"];
            if !VALID_SAVE_AS.contains(&save.as_str()) {
                warnings.push(format!(
                    "{}: save_as \"{}\" is not a recognized value (expected one of {:?})",
                    prefix, save, VALID_SAVE_AS
                ));
            }
        }
    }

    let valid = errors.is_empty();
    ValidationResult {
        valid,
        errors,
        warnings,
    }
}

/// Validate an on_error value. Valid: "abort", "skip", "retry(N)" where N is
/// 1-10, "carry_left", "carry_up".
fn validate_on_error(value: &str, field: &str, errors: &mut Vec<String>) {
    match value {
        "abort" | "skip" | "carry_left" | "carry_up" => {}
        other => {
            // Check for retry(N)
            if let Some(inner) = other
                .strip_prefix("retry(")
                .and_then(|s| s.strip_suffix(')'))
            {
                match inner.parse::<u32>() {
                    Ok(n) if (1..=10).contains(&n) => {}
                    Ok(n) => {
                        errors.push(format!("{}: retry count must be 1-10, got {}", field, n));
                    }
                    Err(_) => {
                        errors.push(format!("{}: invalid retry expression \"{}\"", field, other));
                    }
                }
            } else {
                errors.push(format!(
                    "{}: invalid on_error value \"{}\". \
                     Must be abort, skip, retry(N), carry_left, or carry_up",
                    field, other
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_chain() -> ChainDefinition {
        ChainDefinition {
            schema_version: 1,
            id: "test-chain".into(),
            name: "Test Chain".into(),
            description: "A test chain".into(),
            content_type: "conversation".into(),
            version: "0.1.0".into(),
            author: "test".into(),
            defaults: ChainDefaults {
                model_tier: "mid".into(),
                model: None,
                temperature: 0.3,
                on_error: "retry(2)".into(),
            },
            steps: vec![ChainStep {
                name: "step1".into(),
                primitive: "compress".into(),
                instruction: Some("Do something".into()),
                ..Default::default()
            }],
            post_build: vec![],
        }
    }

    #[test]
    fn valid_minimal_chain_passes() {
        let result = validate_chain(&minimal_chain());
        assert!(result.valid, "errors: {:?}", result.errors);
    }

    #[test]
    fn bad_schema_version_fails() {
        let mut chain = minimal_chain();
        chain.schema_version = 2;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("schema_version")));
    }

    #[test]
    fn empty_steps_fails() {
        let mut chain = minimal_chain();
        chain.steps.clear();
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("at least 1 step")));
    }

    #[test]
    fn duplicate_step_names_fail() {
        let mut chain = minimal_chain();
        let step = chain.steps[0].clone();
        chain.steps.push(step);
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("duplicate step name")));
    }

    #[test]
    fn mechanical_without_rust_function_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].mechanical = true;
        chain.steps[0].rust_function = None;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("rust_function")));
    }

    #[test]
    fn llm_step_without_instruction_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].instruction = None;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("instruction")));
    }

    #[test]
    fn invalid_on_error_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].on_error = Some("explode".into());
        let result = validate_chain(&chain);
        assert!(!result.valid);
    }

    #[test]
    fn retry_out_of_range_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].on_error = Some("retry(15)".into());
        let result = validate_chain(&chain);
        assert!(!result.valid);
    }

    #[test]
    fn recursive_pair_and_pair_adjacent_mutually_exclusive() {
        let mut chain = minimal_chain();
        chain.steps[0].recursive_pair = true;
        chain.steps[0].pair_adjacent = true;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("mutually exclusive")));
    }

    #[test]
    fn sequential_without_for_each_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].sequential = true;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("sequential requires for_each")));
    }

    #[test]
    fn concurrency_without_for_each_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].concurrency = 4;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("concurrency > 1 requires for_each")));
    }

    #[test]
    fn sequential_with_concurrency_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].for_each = Some("$chunks".into());
        chain.steps[0].sequential = true;
        chain.steps[0].concurrency = 2;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("sequential steps cannot use concurrency > 1")));
    }
}
