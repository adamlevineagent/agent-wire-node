// pyramid/question_yaml.rs — Question YAML v3.0 data types
//
// Serde-deserializable types for the v3 question YAML format used in
// `chains/questions/`. These are the front-end AST — the question compiler
// (P2.1) will lower these into ExecutionPlan IR.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete question set parsed from a v3 YAML file.
///
/// Each file in `chains/questions/` (e.g., `code.yaml`, `document.yaml`)
/// deserializes into one `QuestionSet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionSet {
    /// Content type this question sequence targets: "code", "document", "conversation".
    pub r#type: String,
    /// Format version — must be "3.0" for the v3 question format.
    pub version: String,
    /// Default model/retry/temperature applied to all questions unless overridden.
    pub defaults: QuestionDefaults,
    /// Ordered list of questions. Execution proceeds top-to-bottom, with
    /// dependencies implied by the creates/about chain.
    pub questions: Vec<Question>,
}

/// Default execution parameters applied to all questions in a set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionDefaults {
    /// Default LLM model identifier (e.g., "inception/mercury-2").
    #[serde(default)]
    pub model: Option<String>,
    /// Default temperature for LLM calls.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Default retry count on failure.
    #[serde(default)]
    pub retry: Option<u32>,
}

/// A single question in the sequence.
///
/// The `ask` field is the natural-language question (also serves as documentation).
/// The `about` field declares scope (what the question is asked of).
/// The `creates` field declares what the answer produces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    /// The natural language question — also serves as inline documentation.
    pub ask: String,
    /// Scope declaration: what this question is asked of.
    /// See `RECOGNIZED_SCOPES` for valid values.
    pub about: String,
    /// Output type declaration: what the answer produces.
    /// See `RECOGNIZED_CREATES` for valid values.
    pub creates: String,
    /// Path to the prompt instruction file for the LLM.
    /// Supports `prompts/...` references resolved relative to chains_dir.
    pub prompt: String,
    /// Optional prompt used for recursive clustering/classify phases.
    /// When absent, converge/classify phases fall back to `prompt`.
    #[serde(default)]
    pub cluster_prompt: Option<String>,
    /// Override the default model for this question.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional model override used for recursive clustering/classify phases.
    #[serde(default)]
    pub cluster_model: Option<String>,
    /// Override the default temperature for this question.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Concurrency: how many parallel LLM calls for "each"-scoped questions.
    #[serde(default)]
    pub parallel: Option<u32>,
    /// How many times to retry on failure (overrides defaults.retry).
    #[serde(default)]
    pub retry: Option<u32>,
    /// If true, failure skips this question instead of aborting the build.
    #[serde(default)]
    pub optional: Option<bool>,
    /// Alternative prompts for different file/doc types.
    /// Keys are human-readable labels (e.g., "config files"), values are prompt paths.
    #[serde(default)]
    pub variants: Option<HashMap<String, String>>,
    /// Guardrails for the answer (min/max group counts, size limits).
    #[serde(default)]
    pub constraints: Option<QuestionConstraints>,
    /// Prior answers to include as additional input.
    /// References like "L0 web edges", "sibling headlines", "L0 classification tags".
    #[serde(default)]
    pub context: Option<Vec<String>>,
    /// Configuration for ordered processing with accumulated context.
    #[serde(default)]
    pub sequential_context: Option<SequentialContextConfig>,
    /// Number of lines to send per item instead of full content.
    /// Used for cheap pre-classification where headers/titles suffice.
    #[serde(default)]
    pub preview_lines: Option<u32>,
}

/// Guardrails for question outputs — the engine enforces these on LLM responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionConstraints {
    /// Minimum number of clusters/threads/groups.
    #[serde(default)]
    pub min_groups: Option<u32>,
    /// Maximum number of clusters/threads/groups.
    #[serde(default)]
    pub max_groups: Option<u32>,
    /// Maximum items assigned to any single group.
    #[serde(default)]
    pub max_items_per_group: Option<u32>,
}

/// Configuration for sequential (ordered) processing with accumulated context.
///
/// When present, items are processed in order (parallel is ignored),
/// each LLM call receives accumulated context from prior items,
/// and a running summary grows up to `max_chars`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequentialContextConfig {
    /// Processing mode — currently only "accumulate" is supported.
    pub mode: String,
    /// Maximum size of accumulated context in characters.
    #[serde(default)]
    pub max_chars: Option<usize>,
    /// Natural language description of what to carry forward
    /// (e.g., "summary of prior chunks so far").
    #[serde(default)]
    pub carry: Option<String>,
}

// ── Recognized values ───────────────────────────────────────────────────────

/// Valid `about:` scope values from the v3 format spec.
///
/// The "the first N lines of each file" scope is matched via prefix
/// since N is variable (extracted from `preview_lines`).
pub const RECOGNIZED_SCOPES: &[&str] = &[
    "each file individually",
    "each chunk individually",
    // "the first N lines of each file" — matched via starts_with, see is_recognized_scope()
    "all L0 nodes at once",
    "all L0 topics at once",
    "each L1 topic's assigned L0 nodes",
    "each L1 thread's assigned L0 nodes",
    "each L1 topic's assigned L0 nodes, ordered chronologically",
    "each L1 thread's assigned L0 nodes, ordered chronologically",
    "all L1 nodes at once",
    "all L2 nodes at once",
    "all top-level nodes at once",
];

/// Check whether an `about:` value is a recognized scope.
///
/// Handles the variable "the first N lines of each file" pattern.
pub fn is_recognized_scope(scope: &str) -> bool {
    if RECOGNIZED_SCOPES.contains(&scope) {
        return true;
    }
    // Match "the first N lines of each file" where N is a positive integer
    if let Some(rest) = scope.strip_prefix("the first ") {
        if let Some(suffix) = rest.strip_suffix(" lines of each file") {
            return suffix.parse::<u32>().is_ok();
        }
    }
    false
}

/// Valid `creates:` output type values from the v3 format spec.
pub const RECOGNIZED_CREATES: &[&str] = &[
    "L0 nodes",
    "L0 classification tags",
    "L1 topic assignments",
    "L1 thread assignments",
    "L1 nodes",
    "L2 nodes",
    "web edges between L0 nodes",
    "web edges between L1 nodes",
    "web edges between L2 nodes",
    "apex",
];

/// Check whether a `creates:` value is a recognized output type.
pub fn is_recognized_creates(creates: &str) -> bool {
    RECOGNIZED_CREATES.contains(&creates)
}

/// Metadata for a discovered question set (without fully loading prompts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionSetMetadata {
    /// Content type: "code", "document", "conversation".
    pub content_type: String,
    /// Format version (should be "3.0").
    pub version: String,
    /// Number of questions in the set.
    pub question_count: usize,
    /// Absolute path to the YAML file.
    pub file_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_scopes_include_all_static_values() {
        assert!(is_recognized_scope("each file individually"));
        assert!(is_recognized_scope("each chunk individually"));
        assert!(is_recognized_scope("all L0 nodes at once"));
        assert!(is_recognized_scope("all L0 topics at once"));
        assert!(is_recognized_scope("each L1 topic's assigned L0 nodes"));
        assert!(is_recognized_scope("each L1 thread's assigned L0 nodes"));
        assert!(is_recognized_scope(
            "each L1 topic's assigned L0 nodes, ordered chronologically"
        ));
        assert!(is_recognized_scope(
            "each L1 thread's assigned L0 nodes, ordered chronologically"
        ));
        assert!(is_recognized_scope("all L1 nodes at once"));
        assert!(is_recognized_scope("all L2 nodes at once"));
        assert!(is_recognized_scope("all top-level nodes at once"));
    }

    #[test]
    fn recognized_scopes_handle_preview_lines_pattern() {
        assert!(is_recognized_scope("the first 20 lines of each file"));
        assert!(is_recognized_scope("the first 1 lines of each file"));
        assert!(is_recognized_scope("the first 100 lines of each file"));
        assert!(!is_recognized_scope("the first abc lines of each file"));
        assert!(!is_recognized_scope("the first lines of each file"));
    }

    #[test]
    fn unrecognized_scope_rejected() {
        assert!(!is_recognized_scope("each banana individually"));
        assert!(!is_recognized_scope("all L3 nodes at once"));
        assert!(!is_recognized_scope(""));
    }

    #[test]
    fn recognized_creates_include_all_values() {
        assert!(is_recognized_creates("L0 nodes"));
        assert!(is_recognized_creates("L0 classification tags"));
        assert!(is_recognized_creates("L1 topic assignments"));
        assert!(is_recognized_creates("L1 thread assignments"));
        assert!(is_recognized_creates("L1 nodes"));
        assert!(is_recognized_creates("L2 nodes"));
        assert!(is_recognized_creates("web edges between L0 nodes"));
        assert!(is_recognized_creates("web edges between L1 nodes"));
        assert!(is_recognized_creates("web edges between L2 nodes"));
        assert!(is_recognized_creates("apex"));
    }

    #[test]
    fn unrecognized_creates_rejected() {
        assert!(!is_recognized_creates("L3 nodes"));
        assert!(!is_recognized_creates("web edges between L3 nodes"));
        assert!(!is_recognized_creates(""));
        assert!(!is_recognized_creates("something random"));
    }

    #[test]
    fn question_set_deserializes_minimal() {
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: inception/mercury-2
questions:
  - ask: "What does this file do?"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
"#;
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(qs.r#type, "code");
        assert_eq!(qs.version, "3.0");
        assert_eq!(qs.defaults.model.as_deref(), Some("inception/mercury-2"));
        assert_eq!(qs.questions.len(), 1);
        assert_eq!(qs.questions[0].ask, "What does this file do?");
        assert_eq!(qs.questions[0].about, "each file individually");
        assert_eq!(qs.questions[0].creates, "L0 nodes");
    }

    #[test]
    fn question_set_deserializes_with_all_optional_fields() {
        let yaml = r#"
type: document
version: "3.0"
defaults:
  model: inception/mercury-2
  temperature: 0.3
  retry: 2
questions:
  - ask: "Classify this document"
    about: the first 20 lines of each file
    creates: L0 classification tags
    prompt: prompts/doc/classify.md
    cluster_prompt: prompts/doc/recluster.md
    parallel: 8
    preview_lines: 20
    model: custom-model
    cluster_model: cluster-model
    temperature: 0.5
    retry: 3
    optional: true
    variants:
      config files: prompts/doc/config_classify.md
    constraints:
      min_groups: 8
      max_groups: 15
      max_items_per_group: 12
    context:
      - L0 web edges
      - sibling headlines
    sequential_context:
      mode: accumulate
      max_chars: 8000
      carry: summary of prior chunks so far
"#;
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        let q = &qs.questions[0];
        assert_eq!(q.parallel, Some(8));
        assert_eq!(q.preview_lines, Some(20));
        assert_eq!(q.model.as_deref(), Some("custom-model"));
        assert_eq!(q.cluster_model.as_deref(), Some("cluster-model"));
        assert_eq!(
            q.cluster_prompt.as_deref(),
            Some("prompts/doc/recluster.md")
        );
        assert_eq!(q.temperature, Some(0.5));
        assert_eq!(q.retry, Some(3));
        assert_eq!(q.optional, Some(true));

        let variants = q.variants.as_ref().unwrap();
        assert_eq!(
            variants.get("config files").unwrap(),
            "prompts/doc/config_classify.md"
        );

        let constraints = q.constraints.as_ref().unwrap();
        assert_eq!(constraints.min_groups, Some(8));
        assert_eq!(constraints.max_groups, Some(15));
        assert_eq!(constraints.max_items_per_group, Some(12));

        let ctx = q.context.as_ref().unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0], "L0 web edges");
        assert_eq!(ctx[1], "sibling headlines");

        let seq = q.sequential_context.as_ref().unwrap();
        assert_eq!(seq.mode, "accumulate");
        assert_eq!(seq.max_chars, Some(8000));
        assert_eq!(seq.carry.as_deref(), Some("summary of prior chunks so far"));
    }
}
