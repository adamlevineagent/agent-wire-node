// pyramid/crystallization.rs — Crystallization chain patterns (P3.3)
//
// Re-expresses the stale engine's multi-step crystallization process as
// chain templates using the event bus from P3.2. Three chain templates:
//
//   delta_extraction  — file change → diff → update L0 → detect supersessions
//   belief_trace      — superseded entities → trace → classify impact → re-answer → cascade
//   gap_fill          — new apex question → decompose → diff → fill gaps → connect
//
// The existing stale engine is NOT modified. These chain patterns are a
// parallel, declarative path that makes crystallization auditable and
// composable via the event bus.

use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use super::event_chain::{EventSubscription, LocalEventBus, PyramidEvent};
use super::execution_plan::{
    CostEstimate, ErrorPolicy, ExecutionPlan, ModelRequirements, Step, StepOperation,
    StorageDirective, StorageKind,
};

// ── Per-Node Locking ─────────────────────────────────────────────────────────

/// Per-node mutex map to prevent concurrent delta processing on the same node.
///
/// DashMap provides lock-free concurrent access to the map itself, while each
/// node gets its own tokio::sync::Mutex to serialize delta processing.
/// This prevents concurrent deltas from dropping corrections on the same node.
#[derive(Debug, Clone)]
pub struct NodeLockMap {
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl NodeLockMap {
    /// Create a new empty lock map.
    pub fn new() -> Self {
        Self {
            locks: Arc::new(DashMap::new()),
        }
    }

    /// Acquire the lock for a given node_id. Returns a guard that releases
    /// the lock when dropped.
    ///
    /// If no lock exists for this node_id yet, one is created atomically.
    pub async fn acquire(&self, node_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = self
            .locks
            .entry(node_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        mutex.lock_owned().await
    }

    /// Remove the lock entry for a node that is no longer active.
    /// Only call this when you're certain no one holds or will request the lock.
    pub fn remove(&self, node_id: &str) {
        self.locks.remove(node_id);
    }

    /// Number of tracked nodes (for diagnostics).
    pub fn len(&self) -> usize {
        self.locks.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }

    /// Evict stale entries from the lock map.
    ///
    /// Removes entries where the `Arc<Mutex<()>>` strong count is 1, meaning
    /// only the map itself holds a reference and no active task is using or
    /// waiting on the lock. This is safe to call periodically (e.g. after a
    /// build completes) to prevent unbounded growth of the map over time.
    ///
    /// Returns the number of entries removed.
    pub fn cleanup(&self) -> usize {
        let before = self.locks.len();
        self.locks.retain(|_key, mutex| Arc::strong_count(mutex) > 1);
        before - self.locks.len()
    }
}

impl Default for NodeLockMap {
    fn default() -> Self {
        Self::new()
    }
}

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for crystallization chain behavior on a given pyramid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrystallizationConfig {
    /// Pyramid slug this config applies to.
    pub slug: String,
    /// Maximum cascade depth before supersession propagation stops (default 10).
    #[serde(default = "default_max_cascade_depth")]
    pub max_cascade_depth: u32,
    /// How many nodes to re-answer per pass (default 5).
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
}

fn default_max_cascade_depth() -> u32 {
    10
}
fn default_batch_size() -> u32 {
    5
}

impl Default for CrystallizationConfig {
    fn default() -> Self {
        Self {
            slug: String::new(),
            max_cascade_depth: default_max_cascade_depth(),
            batch_size: default_batch_size(),
        }
    }
}

// ── Chain Template IDs ───────────────────────────────────────────────────────

pub const TEMPLATE_DELTA_EXTRACTION: &str = "crystallization/delta-extraction";
pub const TEMPLATE_BELIEF_TRACE: &str = "crystallization/belief-trace";
pub const TEMPLATE_GAP_FILL: &str = "crystallization/gap-fill";

// Subscription IDs (deterministic so re-registration is idempotent)
const SUB_SUPERSESSION_CASCADE: &str = "crystallization-supersession-cascade";
const SUB_STALE_DETECTED: &str = "crystallization-stale-detected";
const SUB_NEW_APEX: &str = "crystallization-new-apex";

// ── Chain Template Builders ──────────────────────────────────────────────────

/// Build the delta extraction chain template as an ExecutionPlan.
///
/// Template A: delta_extraction
///   Input: changed file content + existing L0 node content
///   Step 1: LLM diff (extract what changed, what's new, what's removed)
///   Step 2: Update L0 node with new content
///   Step 3: Detect superseded entities (claims that changed)
///   Output: list of superseded entities + affected node IDs
///   On completion: emit SupersessionCascade event if superseded entities found
pub fn build_delta_extraction_template() -> ExecutionPlan {
    let steps = vec![
        // Step 1: LLM diff — compare new file content against existing L0 node
        Step {
            id: "extract_delta".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("delta_extraction".to_string()),
            depends_on: vec![],
            iteration: None,
            input: json!({
                "existing_l0": "$input.existing_l0_content",
                "new_content": "$input.new_file_content",
                "schema": "$input.canonical_schema"
            }),
            instruction: Some(
                "Compare the existing L0 extraction against the new file content. \
                 For each change, classify as ADDITION (new capability), \
                 MODIFICATION (same capability, different behavior), or \
                 SUPERSESSION (old claim is now false). Output JSON with \
                 changes array and unchanged list."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "changes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string", "enum": ["supersession", "addition", "modification"] },
                                "topic": { "type": "string" },
                                "old_belief": { "type": "string" },
                                "new_truth": { "type": "string" },
                                "supersedes_entities": { "type": "array", "items": { "type": "string" } },
                                "significance": { "type": "number" }
                            }
                        }
                    },
                    "unchanged": { "type": "array", "items": { "type": "string" } }
                }
            })),
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.1),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("delta_extraction.extract_delta".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 2: Update L0 node — transform step to merge delta into existing node
        Step {
            id: "update_l0".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("node_update".to_string()),
            depends_on: vec!["extract_delta".to_string()],
            iteration: None,
            input: json!({
                "existing_l0": "$input.existing_l0_content",
                "delta": "$extract_delta"
            }),
            instruction: Some(
                "Update the L0 node content incorporating the delta. \
                 Mark superseded beliefs with supersession metadata. \
                 Preserve the audit trail of what changed and why."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.1),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::Node,
                depth: Some(0),
                node_id_pattern: Some("$input.node_id".to_string()),
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 1,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("delta_extraction.update_l0".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 3: Detect superseded entities — extract the list of entities
        // whose claims are now false (for cascade propagation)
        Step {
            id: "detect_supersessions".to_string(),
            operation: StepOperation::Transform,
            primitive: Some("filter_supersessions".to_string()),
            depends_on: vec!["extract_delta".to_string()],
            iteration: None,
            input: json!({ "delta": "$extract_delta" }),
            instruction: None,
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Skip,
            model_requirements: ModelRequirements::default(),
            storage_directive: Some(StorageDirective {
                kind: StorageKind::Output,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 0,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: Some(super::execution_plan::TransformSpec {
                function: "filter".to_string(),
                args: json!({
                    "collection": "$extract_delta.changes",
                    "condition": "type == 'supersession'"
                }),
            }),
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("delta_extraction.detect_supersessions".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
    ];

    ExecutionPlan {
        id: Some(TEMPLATE_DELTA_EXTRACTION.to_string()),
        source_chain_id: Some(TEMPLATE_DELTA_EXTRACTION.to_string()),
        source_content_type: None,
        steps,
        total_estimated_nodes: 1,
        total_estimated_cost: CostEstimate {
            billable_calls: 2,
            estimated_output_nodes: 1,
        },
    }
}

/// Build the belief trace chain template as an ExecutionPlan.
///
/// Template B: belief_trace
///   Input: superseded entities + source node ID
///   Step 1: Query pyramid for nodes that reference superseded entities
///   Step 2: Classify impact (mandatory re-answer vs optional refresh vs no impact)
///   Step 3: For each mandatory node: re-synthesize with correction directive
///   Step 4: For each re-synthesized node: detect new supersessions
///   On completion: emit SupersessionCascade event with new supersessions
pub fn build_belief_trace_template() -> ExecutionPlan {
    let steps = vec![
        // Step 1: Find affected nodes by searching for superseded entity references
        Step {
            id: "find_affected_nodes".to_string(),
            operation: StepOperation::Mechanical,
            primitive: Some("entity_search".to_string()),
            depends_on: vec![],
            iteration: None,
            input: json!({
                "slug": "$input.slug",
                "superseded_entities": "$input.superseded_entities",
                "source_node_id": "$input.source_node_id"
            }),
            instruction: None,
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Abort,
            model_requirements: ModelRequirements::default(),
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 0,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: Some("search_by_entity_references".to_string()),
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("belief_trace.find_affected_nodes".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 2: Classify impact for each affected node
        Step {
            id: "classify_impact".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("impact_classification".to_string()),
            depends_on: vec!["find_affected_nodes".to_string()],
            iteration: Some(super::execution_plan::IterationDirective {
                mode: super::execution_plan::IterationMode::Parallel,
                over: Some("$find_affected_nodes.nodes".to_string()),
                concurrency: Some(3),
                accumulate: None,
                shape: Some(super::execution_plan::IterationShape::ForEach),
            }),
            input: json!({
                "node_content": "$item.content",
                "node_id": "$item.node_id",
                "superseded_entities": "$input.superseded_entities",
                "old_beliefs": "$input.old_beliefs"
            }),
            instruction: Some(
                "Classify the impact of superseded entities on this node. \
                 Does this node contain claims based on the superseded entities? \
                 Classify as: mandatory_reanswer (contains false claims), \
                 optional_refresh (references changed area but no false claims), \
                 or no_impact (unaffected)."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "node_id": { "type": "string" },
                    "impact": { "type": "string", "enum": ["mandatory_reanswer", "optional_refresh", "no_impact"] },
                    "false_claims": { "type": "array", "items": { "type": "string" } },
                    "correction_directives": { "type": "array", "items": { "type": "string" } }
                }
            })),
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.1),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 5, // estimate: avg 5 affected nodes
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("belief_trace.classify_impact".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 3: Filter to mandatory re-answer nodes only
        Step {
            id: "filter_mandatory".to_string(),
            operation: StepOperation::Transform,
            primitive: Some("filter_mandatory_reanswer".to_string()),
            depends_on: vec!["classify_impact".to_string()],
            iteration: None,
            input: json!({ "classifications": "$classify_impact" }),
            instruction: None,
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Skip,
            model_requirements: ModelRequirements::default(),
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: Some(super::execution_plan::TransformSpec {
                function: "filter".to_string(),
                args: json!({
                    "collection": "$classify_impact",
                    "condition": "impact == 'mandatory_reanswer'"
                }),
            }),
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("belief_trace.filter_mandatory".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 4: Re-synthesize mandatory nodes with correction directives
        Step {
            id: "re_synthesize".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("correction_synthesis".to_string()),
            depends_on: vec!["filter_mandatory".to_string()],
            iteration: Some(super::execution_plan::IterationDirective {
                mode: super::execution_plan::IterationMode::Parallel,
                over: Some("$filter_mandatory".to_string()),
                concurrency: Some(3),
                accumulate: None,
                shape: Some(super::execution_plan::IterationShape::ForEach),
            }),
            input: json!({
                "node_content": "$item.content",
                "node_id": "$item.node_id",
                "correction_directives": "$item.correction_directives",
                "updated_evidence": "$input.updated_evidence"
            }),
            instruction: Some(
                "Re-synthesize this node incorporating the correction directives. \
                 WARNING: The following claims in your current answer are now false \
                 and MUST be corrected. Update all affected claims and note any \
                 downstream implications."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.2),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::Node,
                depth: Some(1), // placeholder — actual depth resolved at runtime from node being updated
                node_id_pattern: Some("$item.node_id".to_string()),
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 3, // estimate: avg 3 mandatory re-answers
                estimated_output_nodes: 3,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("belief_trace.re_synthesize".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 5: Detect new supersessions from re-synthesized nodes
        Step {
            id: "detect_cascade".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("cascade_detection".to_string()),
            depends_on: vec!["re_synthesize".to_string()],
            iteration: Some(super::execution_plan::IterationDirective {
                mode: super::execution_plan::IterationMode::Parallel,
                over: Some("$re_synthesize".to_string()),
                concurrency: Some(3),
                accumulate: None,
                shape: Some(super::execution_plan::IterationShape::ForEach),
            }),
            input: json!({
                "old_content": "$item.old_content",
                "new_content": "$item.new_content",
                "node_id": "$item.node_id"
            }),
            instruction: Some(
                "Compare the old and new content of this re-synthesized node. \
                 Did the re-synthesis change any claims that OTHER nodes might \
                 depend on? If so, list the superseded entities."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "has_new_supersessions": { "type": "boolean" },
                    "superseded_entities": { "type": "array", "items": { "type": "string" } },
                    "node_id": { "type": "string" }
                }
            })),
            constraints: None,
            error_policy: ErrorPolicy::Skip,
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.1),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 3,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("belief_trace.detect_cascade".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
    ];

    ExecutionPlan {
        id: Some(TEMPLATE_BELIEF_TRACE.to_string()),
        source_chain_id: Some(TEMPLATE_BELIEF_TRACE.to_string()),
        source_content_type: None,
        steps,
        total_estimated_nodes: 3,
        total_estimated_cost: CostEstimate {
            billable_calls: 11,
            estimated_output_nodes: 3,
        },
    }
}

/// Build the gap fill chain template as an ExecutionPlan.
///
/// Template C: gap_fill (for multi-apex growth)
///   Input: new apex question + existing pyramid state
///   Step 1: Decompose question into sub-questions (reuse P2.2 decomposition)
///   Step 2: Diff sub-questions against existing pyramid questions
///   Step 3: For each gap: run targeted extraction/synthesis
///   Step 4: Connect new apex to existing + new nodes
pub fn build_gap_fill_template() -> ExecutionPlan {
    let steps = vec![
        // Step 1: Decompose the new apex question into sub-questions
        Step {
            id: "decompose_question".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("question_decomposition".to_string()),
            depends_on: vec![],
            iteration: None,
            input: json!({
                "question": "$input.question",
                "granularity": "$input.granularity",
                "existing_apex_summaries": "$input.existing_apex_summaries"
            }),
            instruction: Some(
                "Decompose this question into sub-questions that would need to be \
                 answered to fully address the apex question. Consider the existing \
                 pyramid structure and avoid duplicating questions already answered."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "sub_questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string" },
                                "layer": { "type": "integer" },
                                "scope": { "type": "string" }
                            }
                        }
                    }
                }
            })),
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("high_intelligence".to_string()),
                model: None,
                temperature: Some(0.3),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("gap_fill.decompose_question".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 2: Diff sub-questions against existing pyramid
        Step {
            id: "diff_questions".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("question_diff".to_string()),
            depends_on: vec!["decompose_question".to_string()],
            iteration: None,
            input: json!({
                "sub_questions": "$decompose_question.sub_questions",
                "existing_questions": "$input.existing_questions"
            }),
            instruction: Some(
                "Compare these sub-questions against the existing pyramid questions. \
                 For each sub-question, classify as: COVERED (already answered), \
                 PARTIAL (partially answered), or GAP (not answered at all). \
                 For COVERED questions, provide the existing node ID."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "covered": { "type": "array", "items": { "type": "object" } },
                    "partial": { "type": "array", "items": { "type": "object" } },
                    "gaps": { "type": "array", "items": { "type": "object" } }
                }
            })),
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.1),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::StepOnly,
                depth: None,
                node_id_pattern: None,
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("gap_fill.diff_questions".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 3: Fill gaps with targeted extraction/synthesis
        Step {
            id: "fill_gaps".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("targeted_synthesis".to_string()),
            depends_on: vec!["diff_questions".to_string()],
            iteration: Some(super::execution_plan::IterationDirective {
                mode: super::execution_plan::IterationMode::Parallel,
                over: Some("$diff_questions.gaps".to_string()),
                concurrency: Some(3),
                accumulate: None,
                shape: Some(super::execution_plan::IterationShape::ForEach),
            }),
            input: json!({
                "question": "$item.question",
                "scope": "$item.scope",
                "available_evidence": "$input.available_evidence"
            }),
            instruction: Some(
                "Answer this question using the available evidence from the pyramid's \
                 existing L0 nodes. If insufficient evidence exists, note what \
                 additional extraction would be needed."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("fast".to_string()),
                model: None,
                temperature: Some(0.2),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::Node,
                depth: Some(1), // placeholder — actual depth resolved at runtime from question layer
                node_id_pattern: Some("GAP-{index:03}".to_string()),
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 5, // estimate: avg 5 gaps
                estimated_output_nodes: 5,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("gap_fill.fill_gaps".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
        // Step 4: Synthesize the new apex connecting existing + new nodes
        Step {
            id: "synthesize_apex".to_string(),
            operation: StepOperation::Llm,
            primitive: Some("apex_synthesis".to_string()),
            depends_on: vec!["fill_gaps".to_string(), "diff_questions".to_string()],
            iteration: None,
            input: json!({
                "question": "$input.question",
                "covered_nodes": "$diff_questions.covered",
                "new_nodes": "$fill_gaps",
                "partial_nodes": "$diff_questions.partial"
            }),
            instruction: Some(
                "Synthesize an apex answer for the question by combining insights \
                 from existing covered nodes, newly created gap-fill nodes, and \
                 partially covered nodes. The apex should present a coherent \
                 answer that draws on all available evidence."
                    .to_string(),
            ),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements {
                tier: Some("high_intelligence".to_string()),
                model: None,
                temperature: Some(0.3),
            },
            storage_directive: Some(StorageDirective {
                kind: StorageKind::Node,
                depth: Some(3), // apex depth — typically the top of the pyramid
                node_id_pattern: Some("APEX-{slug}".to_string()),
                target: None,
            }),
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 1,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: Some("gap_fill.synthesize_apex".to_string()),
            converge_metadata: None,
            metadata: None,
            scope: None,
        },
    ];

    ExecutionPlan {
        id: Some(TEMPLATE_GAP_FILL.to_string()),
        source_chain_id: Some(TEMPLATE_GAP_FILL.to_string()),
        source_content_type: None,
        steps,
        total_estimated_nodes: 6,
        total_estimated_cost: CostEstimate {
            billable_calls: 8,
            estimated_output_nodes: 6,
        },
    }
}

// ── Subscription Setup ───────────────────────────────────────────────────────

/// Register crystallization event subscriptions on the event bus.
///
/// Wires the three chain templates to their triggering events:
///   - SupersessionCascade → belief_trace chain
///   - StaleDetected       → delta_extraction chain
///   - NewApexRequested    → gap_fill chain
///
/// Subscriptions are scoped to a specific slug if provided.
/// Uses deterministic IDs so re-registration is safe (idempotent — skips
/// if a subscription with the same ID already exists).
/// Build the list of crystallization subscriptions for a given config.
/// This is a pure function — no async, no DB.
pub fn build_crystallization_subscriptions(
    config: &CrystallizationConfig,
) -> Vec<EventSubscription> {
    let slug_filter = if config.slug.is_empty() {
        None
    } else {
        Some(config.slug.clone())
    };

    vec![
        EventSubscription {
            id: format!("{}-{}", SUB_SUPERSESSION_CASCADE, config.slug),
            event_type: "SupersessionCascade".to_string(),
            slug_filter: slug_filter.clone(),
            chain_template: TEMPLATE_BELIEF_TRACE.to_string(),
            max_cascade_depth: config.max_cascade_depth,
            enabled: true,
            created_at: String::new(),
        },
        EventSubscription {
            id: format!("{}-{}", SUB_STALE_DETECTED, config.slug),
            event_type: "StaleDetected".to_string(),
            slug_filter: slug_filter.clone(),
            chain_template: TEMPLATE_DELTA_EXTRACTION.to_string(),
            max_cascade_depth: config.max_cascade_depth,
            enabled: true,
            created_at: String::new(),
        },
        EventSubscription {
            id: format!("{}-{}", SUB_NEW_APEX, config.slug),
            event_type: "NewApexRequested".to_string(),
            slug_filter,
            chain_template: TEMPLATE_GAP_FILL.to_string(),
            max_cascade_depth: config.max_cascade_depth,
            enabled: true,
            created_at: String::new(),
        },
    ]
}

/// Register crystallization event subscriptions on the event bus.
///
/// Wires the three chain templates to their triggering events:
///   - SupersessionCascade → belief_trace chain
///   - StaleDetected       → delta_extraction chain
///   - NewApexRequested    → gap_fill chain
///
/// Subscriptions are scoped to a specific slug if provided.
/// Uses deterministic IDs so re-registration is safe (idempotent — skips
/// if a subscription with the same ID already exists).
///
/// DB persistence: if `conn` is provided, subscriptions are persisted to SQLite
/// BEFORE the async subscribe calls (avoids holding &Connection across awaits).
pub async fn setup_crystallization_subscriptions(
    bus: &LocalEventBus,
    config: &CrystallizationConfig,
    conn: Option<&Connection>,
) -> Result<Vec<String>> {
    let subscriptions = build_crystallization_subscriptions(config);

    // Persist to DB synchronously (before any awaits) if connection provided
    if let Some(conn) = conn {
        for sub in &subscriptions {
            let _ = super::event_chain::save_subscription(conn, sub);
        }
    }

    // Register in-memory (async, but no &Connection held across awaits)
    let mut registered = Vec::new();
    for sub in subscriptions {
        let sub_id = sub.id.clone();
        match bus.subscribe(sub, None).await {
            Ok(()) => {
                info!(sub_id = %sub_id, "registered crystallization subscription");
                registered.push(sub_id);
            }
            Err(e) => {
                // Duplicate ID is expected on re-registration — skip silently
                let msg = e.to_string();
                if msg.contains("already exists") {
                    info!(sub_id = %sub_id, "crystallization subscription already registered, skipping");
                    registered.push(sub_id);
                } else {
                    return Err(e);
                }
            }
        }
    }

    Ok(registered)
}

// ── Trigger ──────────────────────────────────────────────────────────────────

/// Trigger a crystallization delta check for the given slug and changed files.
///
/// Emits StaleDetected events for each changed file's L0 node, which
/// kicks off the chain-of-chains crystallization process via the event bus.
///
/// Returns the list of event invocation IDs.
pub async fn trigger_delta_check(
    bus: &LocalEventBus,
    slug: &str,
    changed_node_ids: &[String],
    conn: Option<&Connection>,
) -> Result<Vec<String>> {
    trigger_delta_check_with_locks(bus, slug, changed_node_ids, conn, None).await
}

/// Like `trigger_delta_check` but acquires per-node locks before emitting events.
///
/// When `node_locks` is provided, each node's lock is acquired before its delta
/// event is emitted, preventing concurrent delta processing from dropping corrections.
/// Locks are released after the event emission (the downstream handler should
/// also use `NodeLockMap` for end-to-end protection).
pub async fn trigger_delta_check_with_locks(
    bus: &LocalEventBus,
    slug: &str,
    changed_node_ids: &[String],
    conn: Option<&Connection>,
    node_locks: Option<&NodeLockMap>,
) -> Result<Vec<String>> {
    if changed_node_ids.is_empty() {
        return Ok(vec![]);
    }

    // Acquire per-node locks if a lock map is provided.
    // This serializes concurrent deltas targeting the same node.
    let _guards = if let Some(locks) = node_locks {
        let mut guards = Vec::with_capacity(changed_node_ids.len());
        for node_id in changed_node_ids {
            guards.push(locks.acquire(node_id).await);
        }
        Some(guards)
    } else {
        None
    };

    let event = PyramidEvent::StaleDetected {
        slug: slug.to_string(),
        node_ids: changed_node_ids.to_vec(),
        layer: 0,
    };

    let invocations = bus.emit(event, conn).await?;

    info!(
        slug = %slug,
        changed_count = changed_node_ids.len(),
        invocations = invocations.len(),
        "triggered crystallization delta check"
    );

    // Guards are dropped here, releasing per-node locks
    Ok(invocations)
}

// ── Status Query ─────────────────────────────────────────────────────────────

/// Status of an active or recent crystallization cascade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrystallizationStatus {
    /// Pyramid slug.
    pub slug: String,
    /// Number of event rounds observed.
    pub rounds: u32,
    /// Maximum cascade depth reached so far.
    pub max_depth_reached: u32,
    /// Total nodes affected across all rounds.
    pub nodes_affected: u32,
    /// Whether a cascade is currently active (events in the log within last 60s).
    pub active: bool,
    /// Breakdown of events by type.
    pub event_counts: CrystallizationEventCounts,
    /// ISO timestamp of the most recent event.
    pub last_event_at: Option<String>,
}

/// Event type counts within a crystallization cascade.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrystallizationEventCounts {
    pub stale_detected: u32,
    pub supersession_cascade: u32,
    pub new_apex_requested: u32,
    pub build_complete: u32,
}

/// Query the event log to show the current state of any active crystallization
/// cascade for the given slug.
pub async fn get_crystallization_status(bus: &LocalEventBus, slug: &str) -> CrystallizationStatus {
    let log = bus.get_log(200).await;

    let mut rounds: u32 = 0;
    let mut max_depth: u32 = 0;
    let mut nodes_affected: u32 = 0;
    let mut last_event_at: Option<String> = None;
    let mut counts = CrystallizationEventCounts::default();
    let mut active = false;

    let now = Utc::now();

    for entry in &log {
        if entry.slug != slug {
            continue;
        }

        rounds += 1;
        if entry.cascade_depth > max_depth {
            max_depth = entry.cascade_depth;
        }
        nodes_affected += entry.chain_invocations.len() as u32;

        match entry.event.as_str() {
            "StaleDetected" => counts.stale_detected += 1,
            "SupersessionCascade" => counts.supersession_cascade += 1,
            "NewApexRequested" => counts.new_apex_requested += 1,
            "BuildComplete" => counts.build_complete += 1,
            _ => {}
        }

        // Track most recent event
        if last_event_at.is_none()
            || entry.timestamp.as_str() > last_event_at.as_deref().unwrap_or("")
        {
            last_event_at = Some(entry.timestamp.clone());
        }

        // Consider active if last event was within 60 seconds
        if let Ok(event_time) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp) {
            let age = now.signed_duration_since(event_time);
            if age.num_seconds() < 60 {
                active = true;
            }
        }
    }

    CrystallizationStatus {
        slug: slug.to_string(),
        rounds,
        max_depth_reached: max_depth,
        nodes_affected,
        active,
        event_counts: counts,
        last_event_at,
    }
}

// ── DB Persistence ───────────────────────────────────────────────────────────

/// Create crystallization-specific tables. Called from `init_pyramid_db`.
pub fn init_crystallization_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_crystallization_config (
            slug TEXT PRIMARY KEY,
            max_cascade_depth INTEGER NOT NULL DEFAULT 10,
            batch_size INTEGER NOT NULL DEFAULT 5,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS pyramid_crystallization_runs (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            template TEXT NOT NULL,
            trigger_event TEXT NOT NULL,
            cascade_depth INTEGER NOT NULL DEFAULT 0,
            nodes_affected INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'running',
            started_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_crystallization_runs_slug
            ON pyramid_crystallization_runs(slug);
        CREATE INDEX IF NOT EXISTS idx_crystallization_runs_status
            ON pyramid_crystallization_runs(status);
        ",
    )?;
    Ok(())
}

/// Load crystallization config for a slug. Returns default if none exists.
pub fn load_config(conn: &Connection, slug: &str) -> Result<CrystallizationConfig> {
    let mut stmt = conn.prepare(
        "SELECT slug, max_cascade_depth, batch_size
         FROM pyramid_crystallization_config
         WHERE slug = ?1",
    )?;

    let result = stmt.query_row([slug], |row| {
        Ok(CrystallizationConfig {
            slug: row.get(0)?,
            max_cascade_depth: row.get::<_, i64>(1)? as u32,
            batch_size: row.get::<_, i64>(2)? as u32,
        })
    });

    match result {
        Ok(config) => Ok(config),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(CrystallizationConfig {
            slug: slug.to_string(),
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

/// Save crystallization config for a slug.
pub fn save_config(conn: &Connection, config: &CrystallizationConfig) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pyramid_crystallization_config
         (slug, max_cascade_depth, batch_size, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        rusqlite::params![
            config.slug,
            config.max_cascade_depth as i64,
            config.batch_size as i64,
        ],
    )?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::event_chain::init_event_tables;

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_event_tables(&conn).unwrap();
        init_crystallization_tables(&conn).unwrap();
        conn
    }

    // ── Template compilation tests ───────────────────────────────────────

    #[test]
    fn delta_extraction_template_compiles_to_valid_ir() {
        let plan = build_delta_extraction_template();
        plan.validate()
            .expect("delta_extraction should produce valid IR");
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.id.as_deref(), Some(TEMPLATE_DELTA_EXTRACTION));
    }

    #[test]
    fn belief_trace_template_compiles_to_valid_ir() {
        let plan = build_belief_trace_template();
        plan.validate()
            .expect("belief_trace should produce valid IR");
        assert_eq!(plan.steps.len(), 5);
        assert_eq!(plan.id.as_deref(), Some(TEMPLATE_BELIEF_TRACE));
    }

    #[test]
    fn gap_fill_template_compiles_to_valid_ir() {
        let plan = build_gap_fill_template();
        plan.validate().expect("gap_fill should produce valid IR");
        assert_eq!(plan.steps.len(), 4);
        assert_eq!(plan.id.as_deref(), Some(TEMPLATE_GAP_FILL));
    }

    #[test]
    fn delta_extraction_has_correct_step_dependencies() {
        let plan = build_delta_extraction_template();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            step_ids,
            ["extract_delta", "update_l0", "detect_supersessions"]
        );

        // update_l0 depends on extract_delta
        assert!(plan.steps[1]
            .depends_on
            .contains(&"extract_delta".to_string()));
        // detect_supersessions depends on extract_delta
        assert!(plan.steps[2]
            .depends_on
            .contains(&"extract_delta".to_string()));
        // extract_delta has no dependencies
        assert!(plan.steps[0].depends_on.is_empty());
    }

    #[test]
    fn belief_trace_has_correct_step_dependencies() {
        let plan = build_belief_trace_template();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            step_ids,
            [
                "find_affected_nodes",
                "classify_impact",
                "filter_mandatory",
                "re_synthesize",
                "detect_cascade"
            ]
        );

        // classify_impact depends on find_affected_nodes
        assert!(plan.steps[1]
            .depends_on
            .contains(&"find_affected_nodes".to_string()));
        // filter_mandatory depends on classify_impact
        assert!(plan.steps[2]
            .depends_on
            .contains(&"classify_impact".to_string()));
        // re_synthesize depends on filter_mandatory
        assert!(plan.steps[3]
            .depends_on
            .contains(&"filter_mandatory".to_string()));
        // detect_cascade depends on re_synthesize
        assert!(plan.steps[4]
            .depends_on
            .contains(&"re_synthesize".to_string()));
    }

    #[test]
    fn gap_fill_has_correct_step_dependencies() {
        let plan = build_gap_fill_template();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            step_ids,
            [
                "decompose_question",
                "diff_questions",
                "fill_gaps",
                "synthesize_apex"
            ]
        );

        // synthesize_apex depends on both fill_gaps and diff_questions
        assert!(plan.steps[3].depends_on.contains(&"fill_gaps".to_string()));
        assert!(plan.steps[3]
            .depends_on
            .contains(&"diff_questions".to_string()));
    }

    #[test]
    fn templates_produce_correct_step_counts() {
        assert_eq!(build_delta_extraction_template().steps.len(), 3);
        assert_eq!(build_belief_trace_template().steps.len(), 5);
        assert_eq!(build_gap_fill_template().steps.len(), 4);
    }

    #[test]
    fn templates_have_correct_cost_estimates() {
        let delta = build_delta_extraction_template();
        assert_eq!(delta.total_estimated_cost.billable_calls, 2);
        assert_eq!(delta.total_estimated_cost.estimated_output_nodes, 1);

        let belief = build_belief_trace_template();
        assert_eq!(belief.total_estimated_cost.billable_calls, 11);
        assert_eq!(belief.total_estimated_cost.estimated_output_nodes, 3);

        let gap = build_gap_fill_template();
        assert_eq!(gap.total_estimated_cost.billable_calls, 8);
        assert_eq!(gap.total_estimated_cost.estimated_output_nodes, 6);
    }

    // ── Subscription tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn subscription_setup_creates_correct_subscriptions() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "test-slug".to_string(),
            max_cascade_depth: 5,
            batch_size: 3,
        };

        let registered = setup_crystallization_subscriptions(&bus, &config, Some(&conn))
            .await
            .unwrap();

        assert_eq!(registered.len(), 3);
        assert_eq!(bus.subscription_count().await, 3);

        // Verify subscriptions target the right chain templates
        let subs = bus.get_subscriptions().await;
        let templates: Vec<&str> = subs.iter().map(|s| s.chain_template.as_str()).collect();
        assert!(templates.contains(&TEMPLATE_BELIEF_TRACE));
        assert!(templates.contains(&TEMPLATE_DELTA_EXTRACTION));
        assert!(templates.contains(&TEMPLATE_GAP_FILL));
    }

    #[tokio::test]
    async fn subscription_setup_is_idempotent() {
        let bus = LocalEventBus::new();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            ..Default::default()
        };

        // First registration
        let r1 = setup_crystallization_subscriptions(&bus, &config, None)
            .await
            .unwrap();
        assert_eq!(r1.len(), 3);

        // Second registration — should not error, should not double-subscribe
        let r2 = setup_crystallization_subscriptions(&bus, &config, None)
            .await
            .unwrap();
        assert_eq!(r2.len(), 3);
        assert_eq!(bus.subscription_count().await, 3);
    }

    #[tokio::test]
    async fn subscription_respects_slug_filter() {
        let bus = LocalEventBus::new();
        let config = CrystallizationConfig {
            slug: "only-this".to_string(),
            ..Default::default()
        };

        setup_crystallization_subscriptions(&bus, &config, None)
            .await
            .unwrap();

        let subs = bus.get_subscriptions().await;
        for sub in &subs {
            assert_eq!(sub.slug_filter.as_deref(), Some("only-this"));
        }
    }

    #[tokio::test]
    async fn subscription_respects_max_cascade_depth() {
        let bus = LocalEventBus::new();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            max_cascade_depth: 7,
            ..Default::default()
        };

        setup_crystallization_subscriptions(&bus, &config, None)
            .await
            .unwrap();

        let subs = bus.get_subscriptions().await;
        for sub in &subs {
            assert_eq!(sub.max_cascade_depth, 7);
        }
    }

    // ── Cascade depth enforcement tests ──────────────────────────────────

    #[tokio::test]
    async fn cascade_depth_prevents_runaway() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            max_cascade_depth: 3,
            ..Default::default()
        };

        setup_crystallization_subscriptions(&bus, &config, Some(&conn))
            .await
            .unwrap();

        // depth 2 < max 3 => should fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec!["entity-a".to_string()],
            source_node_id: "L0-001".to_string(),
            cascade_depth: 2,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 1, "depth 2 should fire (< max 3)");

        // depth 3 >= max 3 => should NOT fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec!["entity-b".to_string()],
            source_node_id: "L0-002".to_string(),
            cascade_depth: 3,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0, "depth 3 should NOT fire (>= max 3)");

        // depth 10 >> max 3 => should NOT fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec!["entity-c".to_string()],
            source_node_id: "L0-003".to_string(),
            cascade_depth: 10,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0, "depth 10 should NOT fire (>= max 3)");
    }

    // ── Trigger tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn trigger_delta_check_emits_stale_detected() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            ..Default::default()
        };

        setup_crystallization_subscriptions(&bus, &config, Some(&conn))
            .await
            .unwrap();

        let ids = trigger_delta_check(
            &bus,
            "test",
            &["L0-001".to_string(), "L0-002".to_string()],
            Some(&conn),
        )
        .await
        .unwrap();

        // Should have fired the StaleDetected subscription
        assert_eq!(ids.len(), 1);

        let log = bus.get_log(10).await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].event, "StaleDetected");
        assert_eq!(log[0].slug, "test");
    }

    #[tokio::test]
    async fn trigger_delta_check_empty_files_returns_empty() {
        let bus = LocalEventBus::new();
        let ids = trigger_delta_check(&bus, "test", &[], None).await.unwrap();
        assert!(ids.is_empty());
    }

    // ── Status query tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn status_returns_meaningful_results() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            ..Default::default()
        };

        setup_crystallization_subscriptions(&bus, &config, Some(&conn))
            .await
            .unwrap();

        // Emit a few events
        let _ = bus
            .emit(
                PyramidEvent::StaleDetected {
                    slug: "test".to_string(),
                    node_ids: vec!["n1".to_string()],
                    layer: 0,
                },
                Some(&conn),
            )
            .await;

        let _ = bus
            .emit(
                PyramidEvent::SupersessionCascade {
                    slug: "test".to_string(),
                    superseded_entities: vec!["e1".to_string()],
                    source_node_id: "n1".to_string(),
                    cascade_depth: 1,
                },
                Some(&conn),
            )
            .await;

        let status = get_crystallization_status(&bus, "test").await;

        assert_eq!(status.slug, "test");
        assert_eq!(status.rounds, 2);
        assert_eq!(status.max_depth_reached, 1);
        assert_eq!(status.event_counts.stale_detected, 1);
        assert_eq!(status.event_counts.supersession_cascade, 1);
        assert!(status.active); // events are fresh
        assert!(status.last_event_at.is_some());
    }

    #[tokio::test]
    async fn status_for_unknown_slug_returns_empty() {
        let bus = LocalEventBus::new();
        let status = get_crystallization_status(&bus, "nonexistent").await;

        assert_eq!(status.slug, "nonexistent");
        assert_eq!(status.rounds, 0);
        assert_eq!(status.max_depth_reached, 0);
        assert_eq!(status.nodes_affected, 0);
        assert!(!status.active);
        assert!(status.last_event_at.is_none());
    }

    // ── DB persistence tests ─────────────────────────────────────────────

    #[test]
    fn config_round_trips_through_db() {
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "my-pyramid".to_string(),
            max_cascade_depth: 7,
            batch_size: 10,
        };

        save_config(&conn, &config).unwrap();
        let loaded = load_config(&conn, "my-pyramid").unwrap();

        assert_eq!(loaded.slug, "my-pyramid");
        assert_eq!(loaded.max_cascade_depth, 7);
        assert_eq!(loaded.batch_size, 10);
    }

    #[test]
    fn config_returns_default_for_unknown_slug() {
        let conn = in_memory_db();
        let config = load_config(&conn, "unknown").unwrap();

        assert_eq!(config.slug, "unknown");
        assert_eq!(config.max_cascade_depth, 10);
        assert_eq!(config.batch_size, 5);
    }

    #[test]
    fn config_save_is_idempotent() {
        let conn = in_memory_db();
        let config = CrystallizationConfig {
            slug: "test".to_string(),
            max_cascade_depth: 5,
            batch_size: 3,
        };

        save_config(&conn, &config).unwrap();
        save_config(&conn, &config).unwrap(); // should not error

        let loaded = load_config(&conn, "test").unwrap();
        assert_eq!(loaded.max_cascade_depth, 5);
    }

    // ── Template content validation tests ────────────────────────────────

    #[test]
    fn delta_extraction_uses_correct_operations() {
        let plan = build_delta_extraction_template();
        assert!(matches!(plan.steps[0].operation, StepOperation::Llm));
        assert!(matches!(plan.steps[1].operation, StepOperation::Llm));
        assert!(matches!(plan.steps[2].operation, StepOperation::Transform));

        // Transform step must have a transform spec
        assert!(plan.steps[2].transform.is_some());
    }

    #[test]
    fn belief_trace_uses_mechanical_for_entity_search() {
        let plan = build_belief_trace_template();
        assert!(matches!(plan.steps[0].operation, StepOperation::Mechanical));
        assert_eq!(
            plan.steps[0].rust_function.as_deref(),
            Some("search_by_entity_references")
        );
    }

    #[test]
    fn belief_trace_has_parallel_iteration_on_classify_and_resynthesize() {
        let plan = build_belief_trace_template();

        // classify_impact (step 1) should iterate in parallel
        let classify = &plan.steps[1];
        let iter = classify
            .iteration
            .as_ref()
            .expect("classify should iterate");
        assert!(matches!(
            iter.mode,
            super::super::execution_plan::IterationMode::Parallel
        ));

        // re_synthesize (step 3) should iterate in parallel
        let resynth = &plan.steps[3];
        let iter = resynth.iteration.as_ref().expect("resynth should iterate");
        assert!(matches!(
            iter.mode,
            super::super::execution_plan::IterationMode::Parallel
        ));
    }

    #[test]
    fn gap_fill_uses_high_intelligence_for_decomposition_and_apex() {
        let plan = build_gap_fill_template();

        // decompose_question should use high_intelligence tier
        assert_eq!(
            plan.steps[0].model_requirements.tier.as_deref(),
            Some("high_intelligence")
        );
        // synthesize_apex should use high_intelligence tier
        assert_eq!(
            plan.steps[3].model_requirements.tier.as_deref(),
            Some("high_intelligence")
        );
        // fill_gaps uses fast tier (batch work)
        assert_eq!(
            plan.steps[2].model_requirements.tier.as_deref(),
            Some("fast")
        );
    }
}
