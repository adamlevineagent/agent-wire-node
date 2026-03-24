// pyramid/chain_engine.rs — Chain definition data model + validator
//
// Defines the YAML-driven chain schema for configurable pyramid build pipelines.
// See docs/plans/action-chain-refactor-v3.md for full specification.

use serde::{Deserialize, Serialize};

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
    // Meta
    "price",
    "metabolize",
    "embody",
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
pub struct ChainStep {
    pub name: String,
    pub primitive: String,
    #[serde(default)]
    pub instruction: Option<String>,
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
    #[serde(default)]
    pub pair_adjacent: bool,
    #[serde(default)]
    pub recursive_pair: bool,
    #[serde(default)]
    pub batch_threshold: Option<usize>,
    #[serde(default)]
    pub merge_instruction: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub on_error: Option<String>,
    #[serde(default)]
    pub save_as: Option<String>,
    #[serde(default)]
    pub node_id_pattern: Option<String>,
    #[serde(default)]
    pub depth: Option<i64>,
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

const VALID_CONTENT_TYPES: &[&str] = &["conversation", "code", "document"];
const VALID_MODEL_TIERS: &[&str] = &["low", "mid", "high", "max"];

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

        // LLM steps (non-mechanical) must have instruction
        if !step.mechanical && step.instruction.is_none() {
            errors.push(format!(
                "{}: LLM step must specify instruction",
                prefix
            ));
        }

        // on_error validation
        if let Some(ref on_err) = step.on_error {
            validate_on_error(on_err, &format!("{}.on_error", prefix), &mut errors);
        }

        // recursive_pair and pair_adjacent are mutually exclusive
        if step.recursive_pair && step.pair_adjacent {
            errors.push(format!(
                "{}: recursive_pair and pair_adjacent are mutually exclusive",
                prefix
            ));
        }

        // sequential only valid with for_each
        if step.sequential && step.for_each.is_none() {
            errors.push(format!(
                "{}: sequential requires for_each",
                prefix
            ));
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
            if let Some(inner) = other.strip_prefix("retry(").and_then(|s| s.strip_suffix(')')) {
                match inner.parse::<u32>() {
                    Ok(n) if (1..=10).contains(&n) => {}
                    Ok(n) => {
                        errors.push(format!(
                            "{}: retry count must be 1-10, got {}",
                            field, n
                        ));
                    }
                    Err(_) => {
                        errors.push(format!(
                            "{}: invalid retry expression \"{}\"",
                            field, other
                        ));
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
                pair_adjacent: false,
                recursive_pair: false,
                batch_threshold: None,
                merge_instruction: None,
                when: None,
                on_error: None,
                save_as: None,
                node_id_pattern: None,
                depth: None,
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
        assert!(result.errors.iter().any(|e| e.contains("duplicate step name")));
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
        assert!(result.errors.iter().any(|e| e.contains("mutually exclusive")));
    }

    #[test]
    fn sequential_without_for_each_fails() {
        let mut chain = minimal_chain();
        chain.steps[0].sequential = true;
        let result = validate_chain(&chain);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("sequential requires for_each")));
    }
}
