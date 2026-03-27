// Defaults Adapter — compiles v2 defaults YAML (ChainDefinition) into IR (ExecutionPlan).
//
// This is the hardest compiler work in the program (P1.3).
// Key translations:
// - recursive_cluster: true → converge block → flat conditional steps
// - instruction_map → variant dispatch
// - compact_inputs → transform steps
// - hardcoded step-name enrichments → declarative context entries
// - mechanical: true → operation: "mechanical" with rust_function
// - sequential: true + accumulate → for_each_sequential with accumulator
//
// Phase 1 targets: code + document only. Conversation deferred.

use anyhow::Result;
use serde_json::{json, Value};

use super::chain_engine::{ChainDefaults, ChainDefinition, ChainStep};
use super::converge_expand::{expand_converge, ConvergeConfig};
use super::execution_plan::{
    AccumulatorConfig, ContextEntry, CostEstimate, ErrorPolicy, ExecutionPlan, IterationDirective,
    IterationMode, IterationShape, ModelRequirements, Step, StepOperation, StorageDirective,
    StorageKind,
};

/// Compile a v2 ChainDefinition into an ExecutionPlan (IR).
///
/// Returns an error for conversation chains (deferred from Phase 1).
pub fn compile_defaults(chain: &ChainDefinition) -> Result<ExecutionPlan> {
    if chain.content_type == "conversation" {
        anyhow::bail!(
            "conversation chains are not yet supported through the unified executor. \
             Conversation support is coming soon — use the legacy executor path for now."
        );
    }

    let mut all_steps: Vec<Step> = Vec::new();
    let step_names: Vec<String> = chain.steps.iter().map(|s| s.name.clone()).collect();

    // Track converge-expanded step names → their terminal (last reduce) step IDs.
    // Later steps that depend on the original converge step name need to instead
    // depend on the terminal step(s) of the converge expansion.
    let mut converge_terminal_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for (step_idx, chain_step) in chain.steps.iter().enumerate() {
        if chain_step.recursive_cluster {
            // ── Converge expansion ──
            let converge_steps = compile_converge_step(
                chain_step,
                &chain.defaults,
                &step_names,
                &chain.steps,
                step_idx,
                &chain.content_type,
            )?;

            // The terminal steps are the shortcut step AND the last round's reduce step.
            // A subsequent step needs to depend on both because either one could be
            // the actual producer (shortcut if <=shortcut_at nodes, last reduce otherwise).
            let mut terminals = Vec::new();
            let shortcut_id = format!("{}_shortcut", chain_step.name);
            if converge_steps.iter().any(|s| s.id == shortcut_id) {
                terminals.push(shortcut_id);
            }
            // Find the last reduce step
            if let Some(last_reduce) = converge_steps.iter().rev().find(|s| {
                s.converge_metadata
                    .as_ref()
                    .map(|m| m.role == super::execution_plan::ConvergeRole::Reduce)
                    .unwrap_or(false)
            }) {
                terminals.push(last_reduce.id.clone());
            }
            converge_terminal_map.insert(chain_step.name.clone(), terminals);

            all_steps.extend(converge_steps);
        } else {
            // ── Straight-line step ──
            let ir_step = compile_straight_line_step(
                chain_step,
                &chain.defaults,
                &step_names,
                step_idx,
                &chain.content_type,
            )?;
            all_steps.push(ir_step);
        }
    }

    // Rewrite dependencies: replace references to converge step names with their
    // terminal step IDs so the DAG has no dangling references.
    if !converge_terminal_map.is_empty() {
        for step in &mut all_steps {
            let mut new_deps = Vec::new();
            for dep in &step.depends_on {
                if let Some(terminals) = converge_terminal_map.get(dep) {
                    new_deps.extend(terminals.iter().cloned());
                } else {
                    new_deps.push(dep.clone());
                }
            }
            step.depends_on = new_deps;
        }
    }

    // Compute aggregate cost estimate
    let total_billable: u32 = all_steps
        .iter()
        .map(|s| s.cost_estimate.billable_calls)
        .sum();
    let total_nodes: u32 = all_steps
        .iter()
        .map(|s| s.cost_estimate.estimated_output_nodes)
        .sum();

    let plan = ExecutionPlan {
        id: None,
        source_chain_id: Some(chain.id.clone()),
        source_content_type: Some(chain.content_type.clone()),
        steps: all_steps,
        total_estimated_nodes: total_nodes,
        total_estimated_cost: CostEstimate {
            billable_calls: total_billable,
            estimated_output_nodes: total_nodes,
        },
    };

    plan.validate()?;
    Ok(plan)
}

// ── Straight-line step compilation ─────────────────────────────────────────

fn compile_straight_line_step(
    step: &ChainStep,
    defaults: &ChainDefaults,
    step_names: &[String],
    step_idx: usize,
    content_type: &str,
) -> Result<Step> {
    let operation = determine_operation(step);
    let error_policy = compile_error_policy(step, defaults)?;
    let iteration = compile_iteration(step);
    let depends_on = analyze_dependencies(step, step_names, step_idx);
    let storage = compile_storage(step);
    let model_reqs = compile_model_requirements(step, defaults);
    let cost = estimate_step_cost(step);
    let context = compile_context_entries(step, content_type);

    Ok(Step {
        id: step.name.clone(),
        operation,
        primitive: Some(step.primitive.clone()),
        depends_on,
        iteration,
        input: step.input.clone().unwrap_or_else(|| {
            // For forEach steps with no explicit input, default to "$item" so
            // the iterator's current item (e.g. chunk content) reaches the LLM.
            // Without this, the resolved input is `{}` and file content is lost.
            if step.for_each.is_some() {
                json!("$item")
            } else {
                json!({})
            }
        }),
        instruction: step.instruction.clone(),
        instruction_map: step.instruction_map.clone(),
        compact_inputs: step.compact_inputs,
        output_schema: step.response_schema.clone(),
        constraints: None,
        error_policy,
        model_requirements: model_reqs,
        storage_directive: storage,
        cost_estimate: cost,
        action_id: None,
        rust_function: step.rust_function.clone(),
        transform: None,
        when: step.when.clone(),
        context,
        response_schema: step.response_schema.clone(),
        source_step_name: Some(step.name.clone()),
        converge_metadata: None,
        metadata: Some({
            let mut meta = json!({
                "source_step_name": step.name,
                "primitive": step.primitive,
            });
            // Preserve fields that don't have dedicated IR Step fields yet
            // so the executor can still access them during the migration.
            if let Some(max_thread_size) = step.max_thread_size {
                meta["max_thread_size"] = json!(max_thread_size);
            }
            if let Some(ref target_clusters) = step.target_clusters {
                meta["target_clusters"] = json!(target_clusters);
            }
            if let Some(batch_threshold) = step.batch_threshold {
                meta["batch_threshold"] = json!(batch_threshold);
            }
            if let Some(ref merge_instruction) = step.merge_instruction {
                meta["merge_instruction"] = json!(merge_instruction);
            }
            meta
        }),
        scope: None,
    })
}

// ── Converge step compilation ──────────────────────────────────────────────

fn compile_converge_step(
    step: &ChainStep,
    defaults: &ChainDefaults,
    step_names: &[String],
    all_steps: &[ChainStep],
    step_idx: usize,
    content_type: &str,
) -> Result<Vec<Step>> {
    // Find what the converge step reads from. It depends on the previous step
    // that produces nodes at the depth this step starts at.
    let mut deps = analyze_dependencies(step, step_names, step_idx);
    let error_policy = compile_error_policy(step, defaults)?;
    let context_entries = compile_context_entries(step, content_type);

    // Determine the input reference. For recursive_cluster, the input is typically
    // the output of the prior step that produces nodes. If no explicit deps were
    // found via $references, scan backwards to find a node-producing step, since
    // the legacy executor reads from the DB at the starting depth.
    let over = if !deps.is_empty() {
        format!("${}", deps[0])
    } else {
        // Scan backwards for the most recent node-producing step at the converge
        // step's starting depth. This mirrors the executor reading from DB.
        let mut node_producer = None;
        for i in (0..step_idx).rev() {
            let prior = &all_steps[i];
            if prior.save_as.as_deref() == Some("node") {
                node_producer = Some(prior.name.clone());
                break;
            }
        }

        // Also depend on the immediately prior step for ordering (e.g. webbing
        // must complete before converge starts, since web edges feed context).
        if step_idx > 0 {
            let prior = step_names[step_idx - 1].clone();
            if !deps.contains(&prior) {
                deps.push(prior);
            }
        }

        if let Some(producer) = node_producer {
            if !deps.contains(&producer) {
                deps.push(producer.clone());
            }
            format!("${}", producer)
        } else {
            "$input".to_string()
        }
    };

    let config = ConvergeConfig {
        over: over.clone(),
        max_rounds: 8, // conservative default, matches unbounded legacy behavior
        shortcut_at: 4,
        reduce_instruction: step
            .instruction
            .clone()
            .unwrap_or_else(|| "Synthesize these nodes.".to_string()),
        classify_instruction: step
            .cluster_instruction
            .clone()
            .unwrap_or_else(|| "Group these nodes into 3-5 semantic clusters.".to_string()),
        classify_model: step.cluster_model.clone().or_else(|| step.model.clone()),
        classify_response_schema: step.cluster_response_schema.clone(),
        reduce_model: step.model.clone(),
        reduce_response_schema: step.response_schema.clone(),
        reduce_model_tier: step
            .model_tier
            .clone()
            .or_else(|| Some(defaults.model_tier.clone())),
        reduce_temperature: step.temperature.or(Some(defaults.temperature)),
        error_policy,
        node_id_pattern: step.node_id_pattern.clone(),
        starting_depth: step.depth,
        context_entries,
    };

    let mut converge_steps = expand_converge(&step.name, &config)?;

    // Wire the first non-shortcut step's dependencies to the adapter-level dependencies.
    // The shortcut step also needs deps wired.
    for s in &mut converge_steps {
        if s.depends_on.is_empty() {
            s.depends_on = deps.clone();
        }
    }

    Ok(converge_steps)
}

// ── Operation type determination ───────────────────────────────────────────

fn determine_operation(step: &ChainStep) -> StepOperation {
    if step.mechanical {
        StepOperation::Mechanical
    } else if step.primitive == "web" {
        StepOperation::Llm // web steps are LLM calls with web-specific post-processing
    } else {
        StepOperation::Llm
    }
}

// ── Error policy compilation ───────────────────────────────────────────────

fn compile_error_policy(step: &ChainStep, defaults: &ChainDefaults) -> Result<ErrorPolicy> {
    let policy_str = step.on_error.as_deref().unwrap_or(&defaults.on_error);
    super::execution_plan::parse_error_policy(policy_str)
}

// ── Iteration compilation ──────────────────────────────────────────────────

fn compile_iteration(step: &ChainStep) -> Option<IterationDirective> {
    if let Some(ref for_each) = step.for_each {
        if step.sequential {
            // Sequential iteration
            let accumulate = step.accumulate.as_ref().and_then(|acc| {
                let obj = acc.as_object()?;
                Some(AccumulatorConfig {
                    field: obj
                        .get("field")
                        .and_then(|v| v.as_str())
                        .unwrap_or("accumulator")
                        .to_string(),
                    seed: obj.get("seed").cloned(),
                    max_chars: obj
                        .get("max_chars")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                    trim_to: obj
                        .get("trim_to")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                    trim_side: obj
                        .get("trim_side")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                })
            });

            Some(IterationDirective {
                mode: IterationMode::Sequential,
                over: Some(for_each.clone()),
                concurrency: Some(1),
                accumulate,
                shape: Some(IterationShape::ForEach),
            })
        } else if step.pair_adjacent {
            Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some(for_each.clone()),
                concurrency: Some(step.concurrency),
                accumulate: None,
                shape: Some(IterationShape::PairAdjacent),
            })
        } else if step.recursive_pair {
            Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some(for_each.clone()),
                concurrency: Some(step.concurrency),
                accumulate: None,
                shape: Some(IterationShape::RecursivePair),
            })
        } else {
            // Standard parallel for_each
            Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some(for_each.clone()),
                concurrency: Some(step.concurrency),
                accumulate: None,
                shape: Some(IterationShape::ForEach),
            })
        }
    } else {
        // Single execution (no iteration)
        None
    }
}

// ── $reference dependency analysis ─────────────────────────────────────────

/// Analyze a step's input, for_each, and context fields to find $references
/// to prior steps. Returns the step names this step depends on.
fn analyze_dependencies(step: &ChainStep, step_names: &[String], step_idx: usize) -> Vec<String> {
    let mut deps = Vec::new();
    let prior_steps: &[String] = &step_names[..step_idx];

    // Check for_each reference
    if let Some(ref for_each) = step.for_each {
        if let Some(dep) = extract_step_ref(for_each, prior_steps) {
            if !deps.contains(&dep) {
                deps.push(dep);
            }
        }
    }

    // Check input references
    if let Some(ref input) = step.input {
        collect_refs_from_value(input, prior_steps, &mut deps);
    }

    // Check context references
    if let Some(ref ctx) = step.context {
        collect_refs_from_value(ctx, prior_steps, &mut deps);
    }

    // If no explicit dependencies found but this is not the first step,
    // depend on the immediately prior step (sequential ordering).
    if deps.is_empty() && step_idx > 0 {
        // Web steps that don't declare explicit input depend on the prior node-producing step
        if step.primitive == "web" && step.input.is_none() {
            // Find the most recent step that produces nodes at a compatible depth
            for i in (0..step_idx).rev() {
                deps.push(step_names[i].clone());
                break;
            }
        }
    }

    deps
}

/// Extract a step reference from a $reference string.
/// Returns the step name if it references a known prior step.
fn extract_step_ref(reference: &str, prior_steps: &[String]) -> Option<String> {
    let trimmed = reference.trim().trim_start_matches('$');
    // $step_name or $step_name.field or $step_name[*].field
    let base = trimmed.split('.').next()?.split('[').next()?;
    if prior_steps.contains(&base.to_string()) {
        Some(base.to_string())
    } else {
        None
    }
}

/// Recursively collect $references from a JSON value.
fn collect_refs_from_value(value: &Value, prior_steps: &[String], deps: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            if let Some(dep) = extract_step_ref(s, prior_steps) {
                if !deps.contains(&dep) {
                    deps.push(dep);
                }
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_refs_from_value(v, prior_steps, deps);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_refs_from_value(v, prior_steps, deps);
            }
        }
        _ => {}
    }
}

// ── Storage directive compilation ──────────────────────────────────────────

fn compile_storage(step: &ChainStep) -> Option<StorageDirective> {
    match step.save_as.as_deref() {
        Some("node") => Some(StorageDirective {
            kind: StorageKind::Node,
            depth: step.depth,
            node_id_pattern: step.node_id_pattern.clone(),
            target: None,
        }),
        Some("web_edges") => Some(StorageDirective {
            kind: StorageKind::WebEdges,
            depth: step.depth,
            node_id_pattern: None,
            target: None,
        }),
        Some(other) => Some(StorageDirective {
            kind: StorageKind::StepOnly,
            depth: step.depth,
            node_id_pattern: None,
            target: Some(other.to_string()),
        }),
        None => None,
    }
}

// ── Model requirements compilation ─────────────────────────────────────────

fn compile_model_requirements(step: &ChainStep, defaults: &ChainDefaults) -> ModelRequirements {
    ModelRequirements {
        tier: step
            .model_tier
            .clone()
            .or_else(|| Some(defaults.model_tier.clone())),
        model: step.model.clone().or_else(|| defaults.model.clone()),
        temperature: Some(step.temperature.unwrap_or(defaults.temperature)),
    }
}

// ── Cost estimation ────────────────────────────────────────────────────────

fn estimate_step_cost(step: &ChainStep) -> CostEstimate {
    if step.mechanical {
        return CostEstimate {
            billable_calls: 0,
            estimated_output_nodes: 0,
        };
    }

    let is_foreach = step.for_each.is_some();
    let saves_node = step.save_as.as_deref() == Some("node");

    if is_foreach {
        // For-each steps: estimate based on typical batch sizes
        CostEstimate {
            billable_calls: 10, // will be refined at runtime based on actual input size
            estimated_output_nodes: if saves_node { 10 } else { 0 },
        }
    } else if step.primitive == "web" {
        CostEstimate {
            billable_calls: 1,
            estimated_output_nodes: 0,
        }
    } else {
        CostEstimate {
            billable_calls: 1,
            estimated_output_nodes: if saves_node { 1 } else { 0 },
        }
    }
}

// ── Context entries compilation ────────────────────────────────────────────
//
// This replaces the three hardcoded step-name enrichments in chain_executor.rs:
// 1. thread_clustering → file_level_connections (web edges at depth 0)
// 2. thread_narrative → cross_thread_connections (web edges at depth 0, per-item)
// 3. upper_layer_synthesis → cross_subsystem_connections (sibling web edges)

fn compile_context_entries(step: &ChainStep, _content_type: &str) -> Vec<ContextEntry> {
    let mut entries = Vec::new();

    // Parse explicit context from YAML
    if let Some(ref ctx_value) = step.context {
        if let Some(obj) = ctx_value.as_object() {
            for (label, reference) in obj {
                if let Some(ref_str) = reference.as_str() {
                    entries.push(ContextEntry {
                        label: label.clone(),
                        reference: Some(ref_str.to_string()),
                        loader: None,
                        params: None,
                    });
                }
            }
        }
    }

    // ── Declarative replacements for hardcoded step-name enrichments ──
    //
    // These context entries tell the executor to load web edges and inject
    // them into prompts, replacing the `if step.name == "..."` checks.

    match step.name.as_str() {
        "thread_clustering" => {
            // Legacy: loads depth-0 web edges, summarizes internal connections
            // for the topic inventory being clustered.
            entries.push(ContextEntry {
                label: "file_level_connections".to_string(),
                reference: None,
                loader: Some("web_edge_summary".to_string()),
                params: Some(json!({
                    "depth": 0,
                    "mode": "internal",
                    "max_edges": 24,
                })),
            });
        }
        "thread_narrative" => {
            // Legacy: loads depth-0 web edges, summarizes external connections
            // for each thread's child nodes.
            entries.push(ContextEntry {
                label: "cross_thread_connections".to_string(),
                reference: None,
                loader: Some("web_edge_summary".to_string()),
                params: Some(json!({
                    "depth": 0,
                    "mode": "external",
                    "max_edges": 18,
                })),
            });
        }
        "upper_layer_synthesis" => {
            // Legacy: loads web edges at the current depth and summarizes
            // external connections for the cluster's nodes. This is injected
            // into the converge reduce steps via the context_entries passed
            // to ConvergeConfig. Sibling-cluster structural context is added
            // separately by the converge expander.
            entries.push(ContextEntry {
                label: "cross_subsystem_connections".to_string(),
                reference: None,
                loader: Some("web_edge_summary".to_string()),
                params: Some(json!({
                    "depth": "current",
                    "mode": "external",
                    "max_edges": 12,
                })),
            });
        }
        _ => {}
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::super::chain_engine::{ChainDefinition, ChainStep};
    use super::*;
    use std::collections::HashMap;

    fn make_defaults() -> ChainDefaults {
        ChainDefaults {
            model_tier: "mid".to_string(),
            model: None,
            temperature: 0.3,
            on_error: "retry(2)".to_string(),
        }
    }

    fn make_chain_step(name: &str, primitive: &str) -> ChainStep {
        ChainStep {
            name: name.to_string(),
            primitive: primitive.to_string(),
            instruction: Some("Do the thing".to_string()),
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
            concurrency: 1,
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
            when: None,
            on_error: None,
            save_as: None,
            node_id_pattern: None,
            depth: None,
            context: None,
            compact_inputs: false,
        }
    }

    fn make_code_chain() -> ChainDefinition {
        // Minimal code chain with the same structure as code.yaml
        ChainDefinition {
            schema_version: 1,
            id: "code-default".to_string(),
            name: "Code Pyramid".to_string(),
            description: "Code analysis pipeline".to_string(),
            content_type: "code".to_string(),
            version: "2.0.0".to_string(),
            author: "test".to_string(),
            defaults: make_defaults(),
            steps: vec![
                // Step 1: L0 code extraction
                {
                    let mut s = make_chain_step("l0_code_extract", "extract");
                    s.for_each = Some("$chunks".to_string());
                    s.concurrency = 8;
                    s.node_id_pattern = Some("C-L0-{index:03}".to_string());
                    s.depth = Some(0);
                    s.save_as = Some("node".to_string());
                    s.instruction_map = Some({
                        let mut m = HashMap::new();
                        m.insert(
                            "type:config".to_string(),
                            "Config extract prompt".to_string(),
                        );
                        m.insert(
                            "extension:.tsx".to_string(),
                            "Frontend extract prompt".to_string(),
                        );
                        m
                    });
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 2: L0 webbing
                {
                    let mut s = make_chain_step("l0_webbing", "web");
                    s.input = Some(json!({ "nodes": "$l0_code_extract" }));
                    s.depth = Some(0);
                    s.save_as = Some("web_edges".to_string());
                    s.compact_inputs = true;
                    s.model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.on_error = Some("skip".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s
                },
                // Step 3: Thread clustering
                {
                    let mut s = make_chain_step("thread_clustering", "classify");
                    s.input = Some(json!({ "topics": "$l0_code_extract" }));
                    s.model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 4: Thread narrative synthesis
                {
                    let mut s = make_chain_step("thread_narrative", "synthesize");
                    s.for_each = Some("$thread_clustering.threads".to_string());
                    s.concurrency = 5;
                    s.node_id_pattern = Some("L1-{index:03}".to_string());
                    s.depth = Some(1);
                    s.save_as = Some("node".to_string());
                    s
                },
                // Step 5: L1 webbing
                {
                    let mut s = make_chain_step("l1_webbing", "web");
                    s.input = Some(json!({ "nodes": "$thread_narrative" }));
                    s.depth = Some(1);
                    s.save_as = Some("web_edges".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("skip".to_string());
                    s
                },
                // Step 6: Upper layer synthesis (recursive_cluster)
                {
                    let mut s = make_chain_step("upper_layer_synthesis", "synthesize");
                    s.recursive_cluster = true;
                    s.cluster_instruction = Some("Group these into clusters".to_string());
                    s.cluster_model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.cluster_response_schema = Some(json!({ "type": "object" }));
                    s.depth = Some(1);
                    s.save_as = Some("node".to_string());
                    s.node_id_pattern = Some("L{depth}-{index:03}".to_string());
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 7: L2 webbing
                {
                    let mut s = make_chain_step("l2_webbing", "web");
                    s.depth = Some(2);
                    s.save_as = Some("web_edges".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("skip".to_string());
                    s
                },
            ],
            post_build: vec![],
        }
    }

    fn make_document_chain() -> ChainDefinition {
        ChainDefinition {
            schema_version: 1,
            id: "document-default".to_string(),
            name: "Document Pyramid".to_string(),
            description: "Document analysis pipeline".to_string(),
            content_type: "document".to_string(),
            version: "3.0.0".to_string(),
            author: "test".to_string(),
            defaults: make_defaults(),
            steps: vec![
                // Step 1: Pre-classify
                {
                    let mut s = make_chain_step("doc_classify", "classify");
                    s.input = Some(json!({ "headers": "$chunks", "header_lines": 20 }));
                    s.model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 2: L0 document extraction
                {
                    let mut s = make_chain_step("l0_doc_extract", "extract");
                    s.for_each = Some("$chunks".to_string());
                    s.context = Some(json!({ "classification": "$doc_classify" }));
                    s.concurrency = 8;
                    s.node_id_pattern = Some("D-L0-{index:03}".to_string());
                    s.depth = Some(0);
                    s.save_as = Some("node".to_string());
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 3: Subject clustering
                {
                    let mut s = make_chain_step("thread_clustering", "classify");
                    s.input = Some(
                        json!({ "topics": "$l0_doc_extract", "classification": "$doc_classify" }),
                    );
                    s.model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 4: Thread synthesis
                {
                    let mut s = make_chain_step("thread_narrative", "synthesize");
                    s.for_each = Some("$thread_clustering.threads".to_string());
                    s.context = Some(json!({ "classification": "$doc_classify" }));
                    s.concurrency = 5;
                    s.node_id_pattern = Some("L1-{index:03}".to_string());
                    s.depth = Some(1);
                    s.save_as = Some("node".to_string());
                    s
                },
                // Step 5: L1 webbing
                {
                    let mut s = make_chain_step("l1_webbing", "web");
                    s.input = Some(json!({ "nodes": "$thread_narrative" }));
                    s.depth = Some(1);
                    s.save_as = Some("web_edges".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("skip".to_string());
                    s
                },
                // Step 6: Upper layer synthesis
                {
                    let mut s = make_chain_step("upper_layer_synthesis", "synthesize");
                    s.recursive_cluster = true;
                    s.cluster_instruction = Some("Group these into clusters".to_string());
                    s.cluster_model = Some("qwen/qwen3.5-flash-02-23".to_string());
                    s.cluster_response_schema = Some(json!({ "type": "object" }));
                    s.depth = Some(1);
                    s.save_as = Some("node".to_string());
                    s.node_id_pattern = Some("L{depth}-{index:03}".to_string());
                    s.on_error = Some("retry(3)".to_string());
                    s
                },
                // Step 7: L2 webbing
                {
                    let mut s = make_chain_step("l2_webbing", "web");
                    s.depth = Some(2);
                    s.save_as = Some("web_edges".to_string());
                    s.response_schema = Some(json!({ "type": "object" }));
                    s.on_error = Some("skip".to_string());
                    s
                },
            ],
            post_build: vec![],
        }
    }

    // ── Conversation rejection ──

    #[test]
    fn conversation_chain_rejected() {
        let chain = ChainDefinition {
            schema_version: 1,
            id: "conversation-test".to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            content_type: "conversation".to_string(),
            version: "1.0".to_string(),
            author: "test".to_string(),
            defaults: make_defaults(),
            steps: vec![make_chain_step("step1", "compress")],
            post_build: vec![],
        };
        let result = compile_defaults(&chain);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("coming soon"));
    }

    // ── Code chain compilation ──

    #[test]
    fn code_chain_compiles_successfully() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).expect("code chain should compile");
        assert_eq!(plan.source_chain_id.as_deref(), Some("code-default"));
        assert_eq!(plan.source_content_type.as_deref(), Some("code"));
        // 6 straight-line steps + converge expansion (1 shortcut + 8 rounds * 4 = 33)
        // + l2_webbing = 6 + 33 = 39... but upper_layer_synthesis is replaced, so:
        // l0_code_extract(1) + l0_webbing(1) + thread_clustering(1) + thread_narrative(1) +
        // l1_webbing(1) + converge(33) + l2_webbing(1) = 39
        assert!(
            plan.steps.len() > 6,
            "should have more steps after converge expansion"
        );
    }

    #[test]
    fn code_chain_has_correct_straight_line_steps() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        // L0 extraction step
        let l0 = plan
            .steps
            .iter()
            .find(|s| s.id == "l0_code_extract")
            .unwrap();
        assert_eq!(l0.operation, StepOperation::Llm);
        assert!(l0.instruction_map.is_some());
        let imap = l0.instruction_map.as_ref().unwrap();
        assert!(imap.contains_key("type:config"));
        assert!(imap.contains_key("extension:.tsx"));

        // Iteration
        let iter = l0.iteration.as_ref().unwrap();
        assert_eq!(iter.mode, IterationMode::Parallel);
        assert_eq!(iter.over.as_deref(), Some("$chunks"));
        assert_eq!(iter.concurrency, Some(8));

        // Storage
        let storage = l0.storage_directive.as_ref().unwrap();
        assert_eq!(storage.kind, StorageKind::Node);
        assert_eq!(storage.depth, Some(0));
        assert_eq!(storage.node_id_pattern.as_deref(), Some("C-L0-{index:03}"));
    }

    #[test]
    fn code_chain_webbing_steps_have_correct_config() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        let l0_web = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        assert!(l0_web.compact_inputs);
        assert_eq!(l0_web.error_policy, ErrorPolicy::Skip);

        let storage = l0_web.storage_directive.as_ref().unwrap();
        assert_eq!(storage.kind, StorageKind::WebEdges);
        assert_eq!(storage.depth, Some(0));
    }

    #[test]
    fn code_chain_thread_clustering_has_web_edge_context() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        let tc = plan
            .steps
            .iter()
            .find(|s| s.id == "thread_clustering")
            .unwrap();
        let has_web_ctx = tc
            .context
            .iter()
            .any(|c| c.label == "file_level_connections");
        assert!(
            has_web_ctx,
            "thread_clustering should have file_level_connections context"
        );
    }

    #[test]
    fn code_chain_thread_clustering_has_response_schema() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        let tc = plan
            .steps
            .iter()
            .find(|s| s.id == "thread_clustering")
            .unwrap();
        assert!(
            tc.response_schema.is_some(),
            "thread_clustering IR step must have response_schema for structured LLM output"
        );
    }

    #[test]
    fn converge_classify_steps_have_response_schema() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        // All converge classify steps should have response_schema
        for step in &plan.steps {
            if step.id.contains("_classify") && step.operation == StepOperation::Llm {
                assert!(
                    step.response_schema.is_some(),
                    "converge classify step '{}' must have response_schema for structured LLM output",
                    step.id
                );
            }
        }
    }

    #[test]
    fn all_webbing_steps_have_response_schema() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        for step in &plan.steps {
            if step.id.contains("webbing") {
                assert!(
                    step.response_schema.is_some(),
                    "webbing step '{}' must have response_schema for structured LLM output",
                    step.id
                );
            }
        }
    }

    #[test]
    fn code_chain_thread_narrative_has_cross_thread_context() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        let tn = plan
            .steps
            .iter()
            .find(|s| s.id == "thread_narrative")
            .unwrap();
        let has_ctx = tn
            .context
            .iter()
            .any(|c| c.label == "cross_thread_connections");
        assert!(
            has_ctx,
            "thread_narrative should have cross_thread_connections context"
        );
    }

    #[test]
    fn real_yaml_thread_clustering_preserves_response_schema() {
        // Parse the actual code.yaml (not the synthetic test chain) to verify
        // that serde_yaml correctly deserializes the response_schema field
        // for the thread_clustering step with its full nested JSON Schema.
        let yaml = include_str!("../../../chains/defaults/code.yaml");
        let chain: ChainDefinition = serde_yaml::from_str(yaml).unwrap();

        let tc_step = chain
            .steps
            .iter()
            .find(|s| s.name == "thread_clustering")
            .expect("code.yaml must have a thread_clustering step");

        assert!(
            tc_step.response_schema.is_some(),
            "thread_clustering ChainStep.response_schema must be parsed from YAML"
        );

        // Verify the schema has the expected top-level structure
        let schema = tc_step.response_schema.as_ref().unwrap();
        assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
        assert!(
            schema.get("properties").is_some(),
            "schema must have properties"
        );
        assert!(
            schema.get("properties").unwrap().get("threads").is_some(),
            "schema must have 'threads' property"
        );

        // Now compile and verify it flows to the IR Step
        let plan = compile_defaults(&chain).unwrap();
        let tc_ir = plan
            .steps
            .iter()
            .find(|s| s.id == "thread_clustering")
            .expect("compiled plan must have thread_clustering step");

        assert!(
            tc_ir.response_schema.is_some(),
            "thread_clustering IR Step.response_schema must be populated after compilation"
        );

        // Verify the IR step schema matches the YAML schema
        let ir_schema = tc_ir.response_schema.as_ref().unwrap();
        assert_eq!(
            ir_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        assert!(
            ir_schema
                .get("properties")
                .unwrap()
                .get("threads")
                .is_some(),
            "IR step schema must have 'threads' property"
        );
    }

    #[test]
    fn code_chain_converge_expansion_produces_steps() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        // Should have shortcut step
        let shortcut = plan
            .steps
            .iter()
            .find(|s| s.id == "upper_layer_synthesis_shortcut");
        assert!(shortcut.is_some(), "should have shortcut step");

        // Should have round 0 classify
        let r0_classify = plan
            .steps
            .iter()
            .find(|s| s.id == "upper_layer_synthesis_r0_classify");
        assert!(r0_classify.is_some(), "should have round 0 classify step");

        // Should have round 0 reduce
        let r0_reduce = plan
            .steps
            .iter()
            .find(|s| s.id == "upper_layer_synthesis_r0_reduce");
        assert!(r0_reduce.is_some(), "should have round 0 reduce step");
    }

    #[test]
    fn code_chain_converge_steps_depend_on_prior_step() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        // The converge shortcut and round 0 classify should depend on thread_narrative
        // (the step that produces L1 nodes) or l1_webbing (the step right before)
        let shortcut = plan
            .steps
            .iter()
            .find(|s| s.id == "upper_layer_synthesis_shortcut")
            .unwrap();

        // Should depend on l1_webbing (the step immediately before upper_layer_synthesis)
        assert!(
            !shortcut.depends_on.is_empty(),
            "shortcut should have dependencies"
        );
    }

    // ── Document chain compilation ──

    #[test]
    fn document_chain_compiles_successfully() {
        let chain = make_document_chain();
        let plan = compile_defaults(&chain).expect("document chain should compile");
        assert_eq!(plan.source_chain_id.as_deref(), Some("document-default"));
        assert_eq!(plan.source_content_type.as_deref(), Some("document"));
        assert!(plan.steps.len() > 6);
    }

    #[test]
    fn document_chain_has_classification_context() {
        let chain = make_document_chain();
        let plan = compile_defaults(&chain).unwrap();

        // l0_doc_extract should have classification context from doc_classify
        let extract = plan
            .steps
            .iter()
            .find(|s| s.id == "l0_doc_extract")
            .unwrap();
        let has_ctx = extract.context.iter().any(|c| c.label == "classification");
        assert!(has_ctx, "l0_doc_extract should have classification context");
    }

    #[test]
    fn document_chain_thread_narrative_has_both_contexts() {
        let chain = make_document_chain();
        let plan = compile_defaults(&chain).unwrap();

        let tn = plan
            .steps
            .iter()
            .find(|s| s.id == "thread_narrative")
            .unwrap();
        // Should have both the YAML-declared classification context AND the
        // declarative cross_thread_connections enrichment
        let has_classification = tn.context.iter().any(|c| c.label == "classification");
        let has_cross_thread = tn
            .context
            .iter()
            .any(|c| c.label == "cross_thread_connections");
        assert!(has_classification, "should have classification context");
        assert!(
            has_cross_thread,
            "should have cross_thread_connections context"
        );
    }

    // ── $reference dependency analysis ──

    #[test]
    fn reference_analysis_finds_input_deps() {
        let step_names = vec![
            "l0_extract".to_string(),
            "l0_webbing".to_string(),
            "clustering".to_string(),
        ];
        let mut step = make_chain_step("clustering", "classify");
        step.input = Some(json!({ "topics": "$l0_extract" }));

        let deps = analyze_dependencies(&step, &step_names, 2);
        assert_eq!(deps, vec!["l0_extract"]);
    }

    #[test]
    fn reference_analysis_finds_for_each_deps() {
        let step_names = vec!["clustering".to_string(), "narrative".to_string()];
        let mut step = make_chain_step("narrative", "synthesize");
        step.for_each = Some("$clustering.threads".to_string());

        let deps = analyze_dependencies(&step, &step_names, 1);
        assert_eq!(deps, vec!["clustering"]);
    }

    #[test]
    fn reference_analysis_finds_context_deps() {
        let step_names = vec!["doc_classify".to_string(), "l0_extract".to_string()];
        let mut step = make_chain_step("l0_extract", "extract");
        step.for_each = Some("$chunks".to_string());
        step.context = Some(json!({ "classification": "$doc_classify" }));

        let deps = analyze_dependencies(&step, &step_names, 1);
        assert_eq!(deps, vec!["doc_classify"]);
    }

    #[test]
    fn reference_analysis_finds_multiple_deps() {
        let step_names = vec![
            "doc_classify".to_string(),
            "l0_extract".to_string(),
            "clustering".to_string(),
        ];
        let mut step = make_chain_step("clustering", "classify");
        step.input = Some(json!({
            "topics": "$l0_extract",
            "classification": "$doc_classify"
        }));

        let deps = analyze_dependencies(&step, &step_names, 2);
        assert!(deps.contains(&"l0_extract".to_string()));
        assert!(deps.contains(&"doc_classify".to_string()));
    }

    #[test]
    fn reference_analysis_ignores_unknown_refs() {
        let step_names = vec!["step1".to_string(), "step2".to_string()];
        let mut step = make_chain_step("step2", "extract");
        step.for_each = Some("$chunks".to_string()); // $chunks is external, not a step

        let deps = analyze_dependencies(&step, &step_names, 1);
        assert!(
            deps.is_empty(),
            "should not depend on $chunks (external input)"
        );
    }

    // ── Error policy ──

    #[test]
    fn error_policy_uses_step_override() {
        let defaults = make_defaults();
        let mut step = make_chain_step("s1", "extract");
        step.on_error = Some("skip".to_string());
        let policy = compile_error_policy(&step, &defaults).unwrap();
        assert_eq!(policy, ErrorPolicy::Skip);
    }

    #[test]
    fn error_policy_falls_back_to_defaults() {
        let defaults = make_defaults();
        let step = make_chain_step("s1", "extract");
        let policy = compile_error_policy(&step, &defaults).unwrap();
        assert_eq!(policy, ErrorPolicy::Retry(2));
    }

    // ── Mechanical steps ──

    #[test]
    fn mechanical_step_compiles_correctly() {
        let chain = ChainDefinition {
            schema_version: 1,
            id: "test".to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            content_type: "code".to_string(),
            version: "1.0".to_string(),
            author: "test".to_string(),
            defaults: make_defaults(),
            steps: vec![{
                let mut s = make_chain_step("extract_graph", "extract");
                s.mechanical = true;
                s.rust_function = Some("extract_import_graph".to_string());
                s.instruction = None; // mechanical steps don't need instruction
                s
            }],
            post_build: vec![],
        };

        let plan = compile_defaults(&chain).unwrap();
        let step = &plan.steps[0];
        assert_eq!(step.operation, StepOperation::Mechanical);
        assert_eq!(step.rust_function.as_deref(), Some("extract_import_graph"));
    }

    // ── Sequential with accumulate ──

    #[test]
    fn sequential_accumulate_compiles_correctly() {
        let chain = ChainDefinition {
            schema_version: 1,
            id: "test".to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            content_type: "code".to_string(),
            version: "1.0".to_string(),
            author: "test".to_string(),
            defaults: make_defaults(),
            steps: vec![{
                let mut s = make_chain_step("seq_step", "synthesize");
                s.sequential = true;
                s.for_each = Some("$items".to_string());
                s.accumulate = Some(json!({
                    "field": "running_summary",
                    "max_chars": 5000,
                    "trim_side": "start",
                }));
                s
            }],
            post_build: vec![],
        };

        let plan = compile_defaults(&chain).unwrap();
        let step = &plan.steps[0];
        let iter = step.iteration.as_ref().unwrap();
        assert_eq!(iter.mode, IterationMode::Sequential);
        assert_eq!(iter.concurrency, Some(1));

        let acc = iter.accumulate.as_ref().unwrap();
        assert_eq!(acc.field, "running_summary");
        assert_eq!(acc.max_chars, Some(5000));
        assert_eq!(acc.trim_side.as_deref(), Some("start"));
    }

    // ── Plan validation ──

    #[test]
    fn compiled_plan_passes_validation() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();
        // validate() is called internally, but let's call it again to be sure
        plan.validate().expect("compiled plan should be valid");
    }

    #[test]
    fn compiled_document_plan_passes_validation() {
        let chain = make_document_chain();
        let plan = compile_defaults(&chain).unwrap();
        plan.validate().expect("compiled plan should be valid");
    }

    // ── Cost estimates ──

    #[test]
    fn cost_estimates_are_nonzero() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();
        assert!(plan.total_estimated_cost.billable_calls > 0);
        assert!(plan.total_estimated_nodes > 0);
    }

    // ── Web step compilation ──

    #[test]
    fn web_steps_compile_as_llm_with_web_edges_storage() {
        let chain = make_code_chain();
        let plan = compile_defaults(&chain).unwrap();

        let l0_web = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        assert_eq!(l0_web.operation, StepOperation::Llm);
        assert_eq!(l0_web.primitive.as_deref(), Some("web"));
        let storage = l0_web.storage_directive.as_ref().unwrap();
        assert_eq!(storage.kind, StorageKind::WebEdges);
    }
}
