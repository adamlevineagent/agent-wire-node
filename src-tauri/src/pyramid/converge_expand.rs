// Converge Expander — unrolls `recursive_cluster: true` into bounded conditional steps.
//
// Each unrolled round consists of:
// 1. A classify step (LLM grouping with fallback to positional groups of 3)
//    using cluster_model / cluster_response_schema
// 2. A fallback transform step (positional groups of 3) for classifier failure
// 3. A missing-assignment repair transform step
// 4. A for_each reduce step (per-group synthesis)
//
// Special behaviors:
// - shortcut_at: 4 — if <=4 nodes remain, skip classification, directly synthesize to apex
// - classifier fallback — if LLM classification fails, fall back to positional groups of 3
// - missing-assignment repair — unassigned nodes go to the smallest cluster
// - sibling-cluster context injection into synthesis prompts
// - max_rounds bound with early termination via `when` conditionals

use super::execution_plan::{
    ClassifyFallback, ContextEntry, ConvergeMetadata, ConvergeRole, CostEstimate, ErrorPolicy,
    IterationDirective, IterationMode, IterationShape, ModelRequirements, Step, StepOperation,
    StorageDirective, StorageKind, TransformSpec,
};
use anyhow::Result;
use serde_json::json;

/// Configuration for converge expansion, extracted from the ChainStep.
#[derive(Debug, Clone)]
pub struct ConvergeConfig {
    /// The items to reduce (reference to a prior step's output)
    pub over: String,
    /// Maximum number of unrolled rounds
    pub max_rounds: u32,
    /// Threshold below which we skip classification and directly synthesize
    pub shortcut_at: u32,
    /// The synthesis instruction (prompt content)
    pub reduce_instruction: String,
    /// The classification instruction (prompt content)
    pub classify_instruction: String,
    /// Model for classify steps
    pub classify_model: Option<String>,
    /// Response schema for classify steps
    pub classify_response_schema: Option<serde_json::Value>,
    /// Model for reduce steps
    pub reduce_model: Option<String>,
    /// Response schema for reduce steps
    pub reduce_response_schema: Option<serde_json::Value>,
    /// Model tier for reduce steps
    pub reduce_model_tier: Option<String>,
    /// Temperature for reduce steps
    pub reduce_temperature: Option<f32>,
    /// Error policy string from the chain
    pub error_policy: ErrorPolicy,
    /// Node ID pattern for generated nodes (e.g., "L{depth}-{index:03}")
    pub node_id_pattern: Option<String>,
    /// Starting depth for generated nodes
    pub starting_depth: Option<i64>,
    /// Context entries for reduce steps (web-edge loading, etc.)
    pub context_entries: Vec<ContextEntry>,
}

/// Expand a converge configuration into a flat sequence of conditional IR steps.
///
/// Produces:
/// 1. A shortcut step: if count(input) <= shortcut_at, synthesize directly to apex
/// 2. For each round 0..max_rounds:
///    a. classify step with `when: count($prev) > shortcut_at`
///    b. fallback transform: positional groups of 3 (fallback for classify failure)
///    c. missing-assignment repair transform
///    d. reduce step: for_each over classified groups with sibling-cluster context
/// 3. A final coalesce step that picks whichever round achieved convergence
pub fn expand_converge(prefix: &str, config: &ConvergeConfig) -> Result<Vec<Step>> {
    let mut steps = Vec::new();

    // ── Shortcut step: if <=shortcut_at nodes, skip classification and synthesize directly ──
    let shortcut_id = format!("{prefix}_shortcut");
    steps.push(Step {
        id: shortcut_id.clone(),
        operation: StepOperation::Llm,
        primitive: Some("synthesize".to_string()),
        depends_on: vec![], // will be wired by the adapter
        iteration: Some(IterationDirective {
            mode: IterationMode::Single,
            over: None,
            concurrency: None,
            accumulate: None,
            shape: None,
        }),
        input: json!({
            "nodes": format!("${}", config.over.trim_start_matches('$')),
            "merge_mode": "direct_apex",
        }),
        instruction: Some(config.reduce_instruction.clone()),
        instruction_map: None,
        compact_inputs: false,
        output_schema: config.reduce_response_schema.clone(),
        constraints: None,
        error_policy: config.error_policy.clone(),
        model_requirements: ModelRequirements {
            tier: config.reduce_model_tier.clone(),
            model: config.reduce_model.clone(),
            temperature: config.reduce_temperature,
        },
        storage_directive: config
            .node_id_pattern
            .as_ref()
            .map(|pattern| StorageDirective {
                kind: StorageKind::Node,
                depth: config.starting_depth.map(|d| d + 1),
                node_id_pattern: Some(pattern.clone()),
                target: None,
            }),
        cost_estimate: CostEstimate {
            billable_calls: 1,
            estimated_output_nodes: 1,
        },
        action_id: None,
        rust_function: None,
        transform: None,
        when: Some(format!(
            "count(${}) <= {}",
            config.over.trim_start_matches('$'),
            config.shortcut_at
        )),
        context: config.context_entries.clone(),
        response_schema: config.reduce_response_schema.clone(),
        source_step_name: Some(prefix.to_string()),
        converge_metadata: Some(ConvergeMetadata {
            converge_id: prefix.to_string(),
            round: None,
            role: ConvergeRole::Shortcut,
            max_rounds: config.max_rounds,
            shortcut_at: config.shortcut_at,
            classify_fallback: None,
        }),
        metadata: None,
        scope: None,
    });

    // ── Unrolled rounds ──
    let mut prev_output_ref = format!("${}", config.over.trim_start_matches('$'));

    for round in 0..config.max_rounds {
        let round_prefix = format!("{prefix}_r{round}");
        let target_depth = config.starting_depth.map(|d| d + 1 + round as i64);

        // Guard: only run this round if we still have > shortcut_at items.
        // Since shortcut_at >= 1 always, this also implies count > 1.
        let round_guard = format!("count({prev_output_ref}) > {}", config.shortcut_at);

        // ── Step A: Classify — LLM groups current nodes into semantic clusters ──
        let classify_id = format!("{round_prefix}_classify");
        steps.push(Step {
            id: classify_id.clone(),
            operation: StepOperation::Llm,
            primitive: Some("classify".to_string()),
            depends_on: if round == 0 {
                vec![] // wired by adapter
            } else {
                vec![format!("{prefix}_r{}_reduce", round - 1)]
            },
            iteration: Some(IterationDirective {
                mode: IterationMode::Single,
                over: None,
                concurrency: None,
                accumulate: None,
                shape: None,
            }),
            input: json!({
                "nodes": prev_output_ref,
            }),
            instruction: Some(config.classify_instruction.clone()),
            instruction_map: None,
            compact_inputs: true, // classifier gets compact summaries
            output_schema: config.classify_response_schema.clone(),
            constraints: None,
            error_policy: ErrorPolicy::Retry(3),
            model_requirements: ModelRequirements {
                tier: None,
                model: config.classify_model.clone(),
                temperature: Some(0.3),
            },
            storage_directive: None,
            cost_estimate: CostEstimate {
                billable_calls: 1,
                estimated_output_nodes: 0,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: Some(round_guard.clone()),
            context: vec![],
            response_schema: config.classify_response_schema.clone(),
            source_step_name: Some(prefix.to_string()),
            converge_metadata: Some(ConvergeMetadata {
                converge_id: prefix.to_string(),
                round: Some(round),
                role: ConvergeRole::Classify,
                max_rounds: config.max_rounds,
                shortcut_at: config.shortcut_at,
                classify_fallback: Some(ClassifyFallback::Positional(3)),
            }),
            metadata: Some(json!({
                "target_depth": target_depth,
            })),
            scope: None,
        });

        // ── Step B: Fallback transform — positional groups of 3 if classify fails ──
        let fallback_id = format!("{round_prefix}_fallback");
        steps.push(Step {
            id: fallback_id.clone(),
            operation: StepOperation::Transform,
            primitive: Some("classify".to_string()),
            depends_on: vec![classify_id.clone()],
            iteration: None,
            input: json!({
                "nodes": prev_output_ref,
                "classify_output": format!("${classify_id}"),
            }),
            instruction: None,
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Skip,
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: Some(TransformSpec {
                function: "coalesce".to_string(),
                args: json!({
                    "values": [
                        "$classify_output.clusters",
                        {
                            "fallback": "positional_groups_of_3",
                            "source": "$nodes",
                        }
                    ]
                }),
            }),
            when: Some(round_guard.clone()),
            context: vec![],
            response_schema: None,
            source_step_name: Some(prefix.to_string()),
            converge_metadata: Some(ConvergeMetadata {
                converge_id: prefix.to_string(),
                round: Some(round),
                role: ConvergeRole::ClassifyFallback,
                max_rounds: config.max_rounds,
                shortcut_at: config.shortcut_at,
                classify_fallback: Some(ClassifyFallback::Positional(3)),
            }),
            metadata: Some(json!({
                "target_depth": target_depth,
            })),
            scope: None,
        });

        // ── Step C: Missing-assignment repair transform ──
        let repair_id = format!("{round_prefix}_repair");
        steps.push(Step {
            id: repair_id.clone(),
            operation: StepOperation::Transform,
            primitive: Some("classify".to_string()),
            depends_on: vec![fallback_id.clone()],
            iteration: None,
            input: json!({
                "clusters": format!("${fallback_id}"),
                "all_node_ids": format!("{prev_output_ref}[*].node_id"),
            }),
            instruction: None,
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Skip,
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: Some(TransformSpec {
                function: "ensure_array".to_string(),
                args: json!({
                    "value": "$clusters",
                    "repair": "missing_assignment_to_smallest",
                    "all_ids": "$all_node_ids",
                }),
            }),
            when: Some(round_guard.clone()),
            context: vec![],
            response_schema: None,
            source_step_name: Some(prefix.to_string()),
            converge_metadata: Some(ConvergeMetadata {
                converge_id: prefix.to_string(),
                round: Some(round),
                role: ConvergeRole::Repair,
                max_rounds: config.max_rounds,
                shortcut_at: config.shortcut_at,
                classify_fallback: None,
            }),
            metadata: Some(json!({
                "target_depth": target_depth,
            })),
            scope: None,
        });

        // ── Step D: Reduce — per-group synthesis with sibling-cluster context ──
        let reduce_id = format!("{round_prefix}_reduce");

        // Build context entries: include sibling-cluster injection + any passed-through entries
        let mut reduce_context = config.context_entries.clone();
        reduce_context.push(ContextEntry {
            label: "sibling_clusters".to_string(),
            reference: Some(format!("${repair_id}")),
            loader: Some("sibling_cluster_context".to_string()),
            params: None,
        });

        steps.push(Step {
            id: reduce_id.clone(),
            operation: StepOperation::Llm,
            primitive: Some("synthesize".to_string()),
            depends_on: vec![repair_id.clone()],
            iteration: Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some(format!("${repair_id}")),
                concurrency: Some(5),
                accumulate: None,
                shape: Some(IterationShape::ConvergeReduce),
            }),
            input: json!({
                "clusters": format!("${repair_id}"),
                "nodes": prev_output_ref,
            }),
            instruction: Some(config.reduce_instruction.clone()),
            instruction_map: None,
            compact_inputs: false,
            output_schema: config.reduce_response_schema.clone(),
            constraints: None,
            error_policy: config.error_policy.clone(),
            model_requirements: ModelRequirements {
                tier: config.reduce_model_tier.clone(),
                model: config.reduce_model.clone(),
                temperature: config.reduce_temperature,
            },
            storage_directive: config
                .node_id_pattern
                .as_ref()
                .map(|pattern| StorageDirective {
                    kind: StorageKind::Node,
                    depth: target_depth,
                    node_id_pattern: Some(pattern.clone()),
                    target: None,
                }),
            cost_estimate: CostEstimate {
                // Estimate: each round produces ~N/3 groups, each needing 1 call
                billable_calls: 4,
                estimated_output_nodes: 4,
            },
            action_id: None,
            rust_function: None,
            transform: None,
            when: Some(round_guard),
            context: reduce_context,
            response_schema: config.reduce_response_schema.clone(),
            source_step_name: Some(prefix.to_string()),
            converge_metadata: Some(ConvergeMetadata {
                converge_id: prefix.to_string(),
                round: Some(round),
                role: ConvergeRole::Reduce,
                max_rounds: config.max_rounds,
                shortcut_at: config.shortcut_at,
                classify_fallback: None,
            }),
            metadata: None,
            scope: None,
        });

        // Next round reads from the reduce output
        prev_output_ref = format!("${reduce_id}");
    }

    Ok(steps)
}

/// Count the total number of steps that expand_converge produces.
/// Useful for cost estimation: 1 (shortcut) + max_rounds * 4 (classify + fallback + repair + reduce)
pub fn expanded_step_count(max_rounds: u32) -> usize {
    1 + (max_rounds as usize * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ConvergeConfig {
        ConvergeConfig {
            over: "$thread_syntheses".to_string(),
            max_rounds: 6,
            shortcut_at: 4,
            reduce_instruction: "Synthesize these nodes".to_string(),
            classify_instruction: "Group these into clusters".to_string(),
            classify_model: Some("qwen/qwen3.5-flash-02-23".to_string()),
            classify_response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "clusters": { "type": "array" }
                }
            })),
            reduce_model: None,
            reduce_response_schema: None,
            reduce_model_tier: Some("mid".to_string()),
            reduce_temperature: Some(0.3),
            error_policy: ErrorPolicy::Retry(3),
            node_id_pattern: Some("L{depth}-{index:03}".to_string()),
            starting_depth: Some(1),
            context_entries: vec![],
        }
    }

    #[test]
    fn config_creation() {
        let config = test_config();
        assert_eq!(config.max_rounds, 6);
        assert_eq!(config.shortcut_at, 4);
    }

    #[test]
    fn expand_produces_correct_step_count() {
        let config = test_config();
        let steps = expand_converge("upper_layer_synthesis", &config).unwrap();
        // 1 shortcut + 6 rounds * 4 steps per round = 25
        assert_eq!(steps.len(), expanded_step_count(6));
        assert_eq!(steps.len(), 25);
    }

    #[test]
    fn shortcut_step_has_correct_when_guard() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let shortcut = &steps[0];
        assert_eq!(shortcut.id, "uls_shortcut");
        assert!(shortcut
            .when
            .as_ref()
            .unwrap()
            .contains("count($thread_syntheses) <= 4"));
        assert_eq!(shortcut.operation, StepOperation::Llm);
    }

    #[test]
    fn classify_steps_use_cluster_model() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let classify_r0 = steps.iter().find(|s| s.id == "uls_r0_classify").unwrap();
        assert_eq!(
            classify_r0.model_requirements.model.as_deref(),
            Some("qwen/qwen3.5-flash-02-23")
        );
        assert_eq!(classify_r0.operation, StepOperation::Llm);
    }

    #[test]
    fn fallback_step_is_transform() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let fallback = steps.iter().find(|s| s.id == "uls_r0_fallback").unwrap();
        assert_eq!(fallback.operation, StepOperation::Transform);
        assert!(fallback.transform.is_some());
    }

    #[test]
    fn repair_step_exists() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let repair = steps.iter().find(|s| s.id == "uls_r0_repair").unwrap();
        assert_eq!(repair.operation, StepOperation::Transform);
        let cm = repair.converge_metadata.as_ref().unwrap();
        assert_eq!(cm.role, ConvergeRole::Repair);
        assert_eq!(cm.round, Some(0));
    }

    #[test]
    fn reduce_steps_have_converge_reduce_shape() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let reduce = steps.iter().find(|s| s.id == "uls_r0_reduce").unwrap();
        assert_eq!(reduce.operation, StepOperation::Llm);
        let iteration = reduce.iteration.as_ref().unwrap();
        assert_eq!(iteration.mode, IterationMode::Parallel);
        assert_eq!(iteration.shape, Some(IterationShape::ConvergeReduce));
    }

    #[test]
    fn reduce_steps_have_sibling_cluster_context() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let reduce = steps.iter().find(|s| s.id == "uls_r0_reduce").unwrap();
        let has_sibling_ctx = reduce.context.iter().any(|c| c.label == "sibling_clusters");
        assert!(
            has_sibling_ctx,
            "reduce step must have sibling_clusters context"
        );
    }

    #[test]
    fn round_dependencies_chain_correctly() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();

        // Round 0 classify has no deps (wired by adapter)
        let r0_classify = steps.iter().find(|s| s.id == "uls_r0_classify").unwrap();
        assert!(r0_classify.depends_on.is_empty());

        // Round 0 fallback depends on classify
        let r0_fallback = steps.iter().find(|s| s.id == "uls_r0_fallback").unwrap();
        assert_eq!(r0_fallback.depends_on, vec!["uls_r0_classify"]);

        // Round 0 repair depends on fallback
        let r0_repair = steps.iter().find(|s| s.id == "uls_r0_repair").unwrap();
        assert_eq!(r0_repair.depends_on, vec!["uls_r0_fallback"]);

        // Round 0 reduce depends on repair
        let r0_reduce = steps.iter().find(|s| s.id == "uls_r0_reduce").unwrap();
        assert_eq!(r0_reduce.depends_on, vec!["uls_r0_repair"]);

        // Round 1 classify depends on round 0 reduce
        let r1_classify = steps.iter().find(|s| s.id == "uls_r1_classify").unwrap();
        assert_eq!(r1_classify.depends_on, vec!["uls_r0_reduce"]);
    }

    #[test]
    fn when_guards_reference_previous_round_output() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();

        // Round 0 guards reference the original input
        let r0_classify = steps.iter().find(|s| s.id == "uls_r0_classify").unwrap();
        assert!(r0_classify
            .when
            .as_ref()
            .unwrap()
            .contains("$thread_syntheses"));

        // Round 1 guards reference round 0 reduce output
        let r1_classify = steps.iter().find(|s| s.id == "uls_r1_classify").unwrap();
        assert!(r1_classify
            .when
            .as_ref()
            .unwrap()
            .contains("$uls_r0_reduce"));
    }

    #[test]
    fn fewer_rounds_fewer_steps() {
        let mut config = test_config();
        config.max_rounds = 2;
        let steps = expand_converge("uls", &config).unwrap();
        assert_eq!(steps.len(), expanded_step_count(2));
        assert_eq!(steps.len(), 9); // 1 + 2*4
    }

    #[test]
    fn storage_directives_on_reduce_steps_have_correct_depth() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();

        let r0_reduce = steps.iter().find(|s| s.id == "uls_r0_reduce").unwrap();
        let sd = r0_reduce.storage_directive.as_ref().unwrap();
        assert_eq!(sd.kind, StorageKind::Node);
        assert_eq!(sd.depth, Some(2)); // starting_depth(1) + 1 + round(0)

        let r1_reduce = steps.iter().find(|s| s.id == "uls_r1_reduce").unwrap();
        let sd = r1_reduce.storage_directive.as_ref().unwrap();
        assert_eq!(sd.depth, Some(3)); // starting_depth(1) + 1 + round(1)
    }

    #[test]
    fn classify_helper_steps_record_target_depth_metadata() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();

        for step_id in ["uls_r0_classify", "uls_r0_fallback", "uls_r0_repair"] {
            let step = steps.iter().find(|s| s.id == step_id).unwrap();
            assert_eq!(
                step.metadata
                    .as_ref()
                    .and_then(|meta| meta.get("target_depth"))
                    .and_then(|depth| depth.as_i64()),
                Some(2),
                "{step_id} should carry the logical target depth for cleanup/resume",
            );
        }
    }

    #[test]
    fn shortcut_step_has_storage_directive() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let shortcut = &steps[0];
        let sd = shortcut.storage_directive.as_ref().unwrap();
        assert_eq!(sd.kind, StorageKind::Node);
        assert_eq!(sd.depth, Some(2)); // starting_depth(1) + 1
    }

    #[test]
    fn classify_steps_have_fallback_metadata() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let classify = steps.iter().find(|s| s.id == "uls_r0_classify").unwrap();
        let cm = classify.converge_metadata.as_ref().unwrap();
        assert_eq!(cm.role, ConvergeRole::Classify);
        assert!(matches!(
            cm.classify_fallback,
            Some(ClassifyFallback::Positional(3))
        ));
        assert_eq!(cm.converge_id, "uls");
        assert_eq!(cm.max_rounds, 6);
        assert_eq!(cm.shortcut_at, 4);
    }

    #[test]
    fn all_steps_have_source_step_name() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        for step in &steps {
            assert_eq!(
                step.source_step_name.as_deref(),
                Some("uls"),
                "step {} missing source_step_name",
                step.id,
            );
        }
    }

    #[test]
    fn all_steps_have_converge_metadata() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        for step in &steps {
            assert!(
                step.converge_metadata.is_some(),
                "step {} missing converge_metadata",
                step.id,
            );
        }
    }

    #[test]
    fn shortcut_converge_metadata_has_no_round() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let shortcut = &steps[0];
        let cm = shortcut.converge_metadata.as_ref().unwrap();
        assert_eq!(cm.role, ConvergeRole::Shortcut);
        assert_eq!(cm.round, None);
    }

    #[test]
    fn reduce_steps_have_response_schema_from_config() {
        let mut config = test_config();
        config.reduce_response_schema = Some(json!({"type": "object"}));
        let steps = expand_converge("uls", &config).unwrap();
        let reduce = steps.iter().find(|s| s.id == "uls_r0_reduce").unwrap();
        assert_eq!(
            reduce.response_schema.as_ref().unwrap(),
            &json!({"type": "object"})
        );
    }

    #[test]
    fn classify_steps_have_response_schema_from_config() {
        let config = test_config();
        let steps = expand_converge("uls", &config).unwrap();
        let classify = steps.iter().find(|s| s.id == "uls_r0_classify").unwrap();
        assert!(
            classify.response_schema.is_some(),
            "classify should have response_schema"
        );
    }
}
