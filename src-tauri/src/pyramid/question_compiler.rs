// pyramid/question_compiler.rs — Question YAML v3.0 → ExecutionPlan IR compiler (P2.1)
//
// Compiles a QuestionSet (parsed from question YAML v3.0) into an ExecutionPlan
// that the IR executor can run. This is the second IR front-end alongside the
// Defaults Adapter.
//
// Core mapping:
//   about: → iteration mode + input reference
//   creates: → storage directive
//   context: → ContextEntry with loaders
//   variants: → instruction_map
//   constraints: → constraints field
//   optional: true → ErrorPolicy::Skip
//   parallel: N → Parallel with concurrency
//   sequential_context: → Sequential with AccumulatorConfig

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use super::converge_expand::{expand_converge, ConvergeConfig};
use super::execution_plan::{
    AccumulatorConfig, Constraint, ContextEntry, CostEstimate, ErrorPolicy, ExecutionPlan,
    IterationDirective, IterationMode, IterationShape, ModelRequirements, Step, StepOperation,
    StorageDirective, StorageKind,
};
use super::question_yaml::{Question, QuestionSet};

/// Compile a question set into an ExecutionPlan.
///
/// The `chains_dir` is used for resolving prompt file paths (the prompts
/// should already be resolved by `question_loader::load_question_set`,
/// but we keep the parameter for consistency and potential re-resolution).
pub fn compile_question_set(qs: &QuestionSet, _chains_dir: &Path) -> Result<ExecutionPlan> {
    let mut all_steps: Vec<Step> = Vec::new();

    // Track what each step creates, so we can wire dependencies.
    // Key: creates value (e.g., "L0 nodes"), Value: step ID that creates it.
    let mut creates_to_step: HashMap<String, String> = HashMap::new();

    // Track converge terminal steps for dependency wiring (same as defaults_adapter).
    let mut converge_terminal_map: HashMap<String, Vec<String>> = HashMap::new();

    let default_model = qs.defaults.model.clone();
    let default_temperature = qs.defaults.temperature;
    let default_retry = qs.defaults.retry.unwrap_or(2);

    for (idx, question) in qs.questions.iter().enumerate() {
        let step_id = derive_step_id(&question.creates, idx);

        // Determine dependencies from the about→creates chain
        let depends_on = compute_depends_on(question, &creates_to_step, &converge_terminal_map);

        // Check if this is a converge expansion (creates L2 nodes uses recluster pattern)
        if question.creates == "L2 nodes" {
            let converge_steps = compile_converge_question(
                question,
                &step_id,
                &depends_on,
                &default_model,
                default_temperature,
                default_retry,
            )?;

            // Record terminals for subsequent steps
            let mut terminals = Vec::new();
            let shortcut_id = format!("{}_shortcut", step_id);
            if converge_steps.iter().any(|s| s.id == shortcut_id) {
                terminals.push(shortcut_id);
            }
            if let Some(last_reduce) = converge_steps.iter().rev().find(|s| {
                s.converge_metadata
                    .as_ref()
                    .map(|m| m.role == super::execution_plan::ConvergeRole::Reduce)
                    .unwrap_or(false)
            }) {
                terminals.push(last_reduce.id.clone());
            }
            converge_terminal_map.insert(step_id.clone(), terminals);

            creates_to_step.insert(question.creates.clone(), step_id);
            all_steps.extend(converge_steps);
        } else {
            let step = compile_question_step(
                question,
                &step_id,
                &depends_on,
                &default_model,
                default_temperature,
                default_retry,
            )?;
            creates_to_step.insert(question.creates.clone(), step_id);
            all_steps.push(step);
        }
    }

    assign_apex_storage_depth(&mut all_steps, apex_depth_for_question_set(qs));

    // Rewrite dependencies: replace references to converge step IDs with their
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
        source_chain_id: Some(format!("{}-questions", qs.r#type)),
        source_content_type: Some(qs.r#type.clone()),
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

// ── Step ID derivation ─────────────────────────────────────────────────────

/// Derive a step ID from the `creates:` field.
fn derive_step_id(creates: &str, _idx: usize) -> String {
    match creates {
        "L0 nodes" => "l0_extract".to_string(),
        "L0 classification tags" => "l0_classification".to_string(),
        "L1 topic assignments" | "L1 thread assignments" => "clustering".to_string(),
        "L1 nodes" => "l1_synthesis".to_string(),
        "L2 nodes" => "l2_synthesis".to_string(),
        "web edges between L0 nodes" => "l0_webbing".to_string(),
        "web edges between L1 nodes" => "l1_webbing".to_string(),
        "web edges between L2 nodes" => "l2_webbing".to_string(),
        "apex" => "apex".to_string(),
        other => {
            // Fallback: slugify the creates value
            other.to_lowercase().replace(' ', "_").replace('/', "_")
        }
    }
}

fn conceptual_depth_for_creates(creates: &str) -> Option<i64> {
    match creates {
        "L0 nodes" | "L0 classification tags" | "web edges between L0 nodes" => Some(0),
        "L1 topic assignments"
        | "L1 thread assignments"
        | "L1 nodes"
        | "web edges between L1 nodes" => Some(1),
        "L2 nodes" | "web edges between L2 nodes" => Some(2),
        "apex" => None,
        _ => None,
    }
}

fn source_depth_for_scope(scope: &str) -> Option<i64> {
    match scope {
        "all L0 nodes at once" | "all L0 topics at once" => Some(0),
        "all L1 nodes at once" => Some(1),
        "all L2 nodes at once" => Some(2),
        _ => None,
    }
}

fn apex_depth_for_question_set(qs: &QuestionSet) -> i64 {
    qs.questions
        .iter()
        .filter_map(|q| conceptual_depth_for_creates(&q.creates))
        .max()
        .unwrap_or(0)
        + 1
}

fn is_apex_storage(sd: &StorageDirective) -> bool {
    sd.kind == StorageKind::Node
        && sd
            .node_id_pattern
            .as_deref()
            .map(|pattern| pattern.eq_ignore_ascii_case("APEX"))
            .unwrap_or(false)
}

fn assign_apex_storage_depth(steps: &mut [Step], apex_depth: i64) {
    for step in steps {
        if let Some(storage) = step.storage_directive.as_mut() {
            if is_apex_storage(storage) {
                storage.depth = Some(apex_depth);
            }
        }
    }
}

// ── Dependency computation ─────────────────────────────────────────────────

/// Compute the `depends_on` list for a question based on its `about:` scope.
///
/// Each `about:` scope implies a dependency on the step that produces the
/// referenced data. For example, "about: all L0 nodes at once" depends on
/// the step that "creates: L0 nodes".
fn compute_depends_on(
    question: &Question,
    creates_to_step: &HashMap<String, String>,
    converge_terminal_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut deps = Vec::new();

    // Primary dependency: what the about scope references
    let implied_deps = match question.about.as_str() {
        "each file individually" | "each chunk individually" => vec![],
        s if s.starts_with("the first ") && s.ends_with(" lines of each file") => vec![],
        "all L0 nodes at once" => vec!["L0 nodes"],
        "all L0 topics at once" => vec!["L0 nodes"],
        "each L1 topic's assigned L0 nodes" | "each L1 thread's assigned L0 nodes" => {
            vec!["L1 topic assignments", "L1 thread assignments", "L0 nodes"]
        }
        "each L1 topic's assigned L0 nodes, ordered chronologically"
        | "each L1 thread's assigned L0 nodes, ordered chronologically" => {
            vec!["L1 topic assignments", "L1 thread assignments", "L0 nodes"]
        }
        "all L1 nodes at once" => vec!["L1 nodes"],
        "all L2 nodes at once" => vec!["L2 nodes"],
        "all top-level nodes at once" => vec!["L2 nodes", "L1 nodes"],
        _ => vec![],
    };

    for creates_key in implied_deps {
        if let Some(step_id) = creates_to_step.get(creates_key) {
            // Check if this step was converge-expanded
            if let Some(terminals) = converge_terminal_map.get(step_id) {
                for terminal in terminals {
                    if !deps.contains(terminal) {
                        deps.push(terminal.clone());
                    }
                }
            } else if !deps.contains(step_id) {
                deps.push(step_id.clone());
            }
        }
    }

    // Context dependencies: context references may also add deps
    if let Some(ref ctx) = question.context {
        for entry in ctx {
            let ctx_creates = match entry.as_str() {
                "L0 web edges" => Some("web edges between L0 nodes"),
                "L1 web edges" => Some("web edges between L1 nodes"),
                "L2 web edges" => Some("web edges between L2 nodes"),
                "L0 classification tags" => Some("L0 classification tags"),
                "sibling headlines" => None, // runtime-resolved, no extra dep
                _ => None,
            };
            if let Some(creates_key) = ctx_creates {
                if let Some(step_id) = creates_to_step.get(creates_key) {
                    if let Some(terminals) = converge_terminal_map.get(step_id) {
                        for terminal in terminals {
                            if !deps.contains(terminal) {
                                deps.push(terminal.clone());
                            }
                        }
                    } else if !deps.contains(step_id) {
                        deps.push(step_id.clone());
                    }
                }
            }
        }
    }

    deps
}

// ── Single question step compilation ───────────────────────────────────────

fn compile_question_step(
    question: &Question,
    step_id: &str,
    depends_on: &[String],
    default_model: &Option<String>,
    default_temperature: Option<f32>,
    default_retry: u32,
) -> Result<Step> {
    let iteration = compile_about_to_iteration(question)?;
    let input = compile_about_to_input(question);
    let storage = compile_creates_to_storage(&question.creates)?;
    let error_policy = compile_error_policy(question, default_retry);
    let model_reqs = compile_model_requirements(question, default_model, default_temperature);
    let context = compile_context(question);
    let constraints = compile_constraints(question);
    let instruction_map = compile_variants(question);
    let cost = estimate_cost(question);

    // Clustering steps need a response_schema for structured JSON output
    // and a high-tier model for large context windows (30+ L0 nodes).
    // Webbing steps also need a response_schema for structured edge output.
    let (response_schema, model_reqs) = if matches!(
        question.creates.as_str(),
        "L1 topic assignments" | "L1 thread assignments"
    ) {
        let schema = json!({
            "type": "object",
            "properties": {
                "threads": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "2-6 word thread label"
                            },
                            "description": {
                                "type": "string",
                                "description": "1-2 sentence description of the subsystem"
                            },
                            "assignments": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 12,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "source_node": {
                                            "type": "string",
                                            "description": "Assigned L0 node ID copied EXACTLY from the input"
                                        },
                                        "topic_index": {
                                            "type": "integer",
                                            "description": "Index of the topic within the topic inventory"
                                        },
                                        "topic_name": {
                                            "type": "string",
                                            "description": "Original headline or topic label"
                                        }
                                    },
                                    "required": ["source_node", "topic_index", "topic_name"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["name", "description", "assignments"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["threads"],
            "additionalProperties": false
        });
        // Override model requirements: use high tier for large context
        let reqs = ModelRequirements {
            tier: Some("high".to_string()),
            model: model_reqs.model,
            temperature: model_reqs.temperature,
        };
        (Some(schema), reqs)
    } else if question.creates.starts_with("web edges") {
        let schema = json!({
            "type": "object",
            "properties": {
                "edges": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "source": { "type": "string" },
                            "target": { "type": "string" },
                            "relationship": { "type": "string" },
                            "shared_resources": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "strength": { "type": "number" }
                        },
                        "required": ["source", "target", "relationship", "shared_resources", "strength"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["edges"],
            "additionalProperties": false
        });
        (Some(schema), model_reqs)
    } else {
        (None, model_reqs)
    };

    Ok(Step {
        id: step_id.to_string(),
        operation: StepOperation::Llm,
        primitive: Some(derive_primitive(&question.creates)),
        depends_on: depends_on.to_vec(),
        iteration,
        input,
        instruction: Some(question.prompt.clone()),
        instruction_map,
        compact_inputs: should_compact_input(question),
        output_schema: None,
        constraints,
        error_policy,
        model_requirements: model_reqs,
        storage_directive: storage,
        cost_estimate: cost,
        action_id: None,
        rust_function: None,
        transform: None,
        when: None,
        context,
        response_schema,
        source_step_name: Some(step_id.to_string()),
        converge_metadata: None,
        metadata: Some(json!({
            "question": question.ask,
            "about": question.about,
            "creates": question.creates,
        })),
        scope: None,
    })
}

// ── about: → IterationDirective ────────────────────────────────────────────

fn compile_about_to_iteration(question: &Question) -> Result<Option<IterationDirective>> {
    let scope = question.about.as_str();

    // Check for sequential_context override first
    if question.sequential_context.is_some() {
        let accumulate = question
            .sequential_context
            .as_ref()
            .map(|sc| AccumulatorConfig {
                field: sc
                    .carry
                    .clone()
                    .unwrap_or_else(|| "accumulator".to_string()),
                seed: None,
                max_chars: sc.max_chars,
                trim_to: None,
                trim_side: None,
            });

        return Ok(Some(IterationDirective {
            mode: IterationMode::Sequential,
            over: Some("$chunks".to_string()),
            concurrency: Some(1),
            accumulate,
            shape: Some(IterationShape::ForEach),
        }));
    }

    match scope {
        "each file individually" | "each chunk individually" => {
            let concurrency = question.parallel.unwrap_or(1) as usize;
            Ok(Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some("$chunks".to_string()),
                concurrency: Some(concurrency),
                accumulate: None,
                shape: Some(IterationShape::ForEach),
            }))
        }
        s if s.starts_with("the first ") && s.ends_with(" lines of each file") => {
            let concurrency = question.parallel.unwrap_or(1) as usize;
            Ok(Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some("$chunks".to_string()),
                concurrency: Some(concurrency),
                accumulate: None,
                shape: Some(IterationShape::ForEach),
            }))
        }
        "all L0 nodes at once"
        | "all L0 topics at once"
        | "all L1 nodes at once"
        | "all L2 nodes at once"
        | "all top-level nodes at once" => {
            // Single execution — no iteration
            Ok(None)
        }
        "each L1 topic's assigned L0 nodes" | "each L1 thread's assigned L0 nodes" => {
            let concurrency = question.parallel.unwrap_or(1) as usize;
            Ok(Some(IterationDirective {
                mode: IterationMode::Parallel,
                over: Some("$clustering.threads".to_string()),
                concurrency: Some(concurrency),
                accumulate: None,
                shape: Some(IterationShape::ForEach),
            }))
        }
        "each L1 topic's assigned L0 nodes, ordered chronologically"
        | "each L1 thread's assigned L0 nodes, ordered chronologically" => {
            let accumulate = Some(AccumulatorConfig {
                field: "chronological_context".to_string(),
                seed: None,
                max_chars: Some(8000),
                trim_to: None,
                trim_side: None,
            });
            Ok(Some(IterationDirective {
                mode: IterationMode::Sequential,
                over: Some("$clustering.threads".to_string()),
                concurrency: Some(1),
                accumulate,
                shape: Some(IterationShape::ForEach),
            }))
        }
        other => Err(anyhow!("unrecognized about scope: \"{}\"", other)),
    }
}

// ── about: → input reference ───────────────────────────────────────────────

fn compile_about_to_input(question: &Question) -> serde_json::Value {
    let scope = question.about.as_str();

    match scope {
        "each file individually" | "each chunk individually" => {
            if question.preview_lines.is_some() {
                json!({
                    "content": "$item",
                    "preview_lines": question.preview_lines,
                })
            } else {
                json!("$item")
            }
        }
        s if s.starts_with("the first ") && s.ends_with(" lines of each file") => {
            // Extract N from "the first N lines of each file"
            let n: u32 = s
                .strip_prefix("the first ")
                .and_then(|r| r.strip_suffix(" lines of each file"))
                .and_then(|n| n.parse().ok())
                .unwrap_or(20);
            json!({
                "content": "$item",
                "preview_lines": n,
            })
        }
        "all L0 nodes at once" => json!({ "nodes": "$l0_extract" }),
        "all L0 topics at once" => json!({ "topics": "$l0_extract" }),
        "each L1 topic's assigned L0 nodes"
        | "each L1 thread's assigned L0 nodes"
        | "each L1 topic's assigned L0 nodes, ordered chronologically"
        | "each L1 thread's assigned L0 nodes, ordered chronologically" => {
            json!("$item")
        }
        "all L1 nodes at once" => json!({ "nodes": "$l1_synthesis" }),
        "all L2 nodes at once" => json!({ "nodes": "$l2_synthesis" }),
        "all top-level nodes at once" => {
            // Reference the highest non-apex layer. The executor resolves
            // this at runtime — we use a generic reference.
            json!({ "nodes": "$top_level_nodes" })
        }
        _ => json!({}),
    }
}

// ── creates: → StorageDirective ────────────────────────────────────────────

fn compile_creates_to_storage(creates: &str) -> Result<Option<StorageDirective>> {
    match creates {
        "L0 nodes" => Ok(Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(0),
            node_id_pattern: Some("L0-{index:03}".to_string()),
            target: None,
        })),
        "L0 classification tags" => Ok(Some(StorageDirective {
            kind: StorageKind::StepOnly,
            depth: Some(0),
            node_id_pattern: None,
            target: Some("classification".to_string()),
        })),
        "L1 topic assignments" | "L1 thread assignments" => Ok(Some(StorageDirective {
            kind: StorageKind::StepOnly,
            depth: Some(1),
            node_id_pattern: None,
            target: Some("assignments".to_string()),
        })),
        "L1 nodes" => Ok(Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(1),
            node_id_pattern: Some("L1-{index:03}".to_string()),
            target: None,
        })),
        "L2 nodes" => {
            // L2 uses converge expansion, storage is handled there.
            // This should not be called for L2 — handled in compile_converge_question.
            Ok(Some(StorageDirective {
                kind: StorageKind::Node,
                depth: Some(2),
                node_id_pattern: Some("L2-{index:03}".to_string()),
                target: None,
            }))
        }
        "web edges between L0 nodes" => Ok(Some(StorageDirective {
            kind: StorageKind::WebEdges,
            depth: Some(0),
            node_id_pattern: None,
            target: None,
        })),
        "web edges between L1 nodes" => Ok(Some(StorageDirective {
            kind: StorageKind::WebEdges,
            depth: Some(1),
            node_id_pattern: None,
            target: None,
        })),
        "web edges between L2 nodes" => Ok(Some(StorageDirective {
            kind: StorageKind::WebEdges,
            depth: Some(2),
            node_id_pattern: None,
            target: None,
        })),
        "apex" => Ok(Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(99), // temporary placeholder, replaced after plan assembly
            node_id_pattern: Some("APEX".to_string()),
            target: None,
        })),
        other => Err(anyhow!("unrecognized creates value: \"{}\"", other)),
    }
}

// ── Error policy ───────────────────────────────────────────────────────────

fn compile_error_policy(question: &Question, default_retry: u32) -> ErrorPolicy {
    if question.optional == Some(true) {
        ErrorPolicy::Skip
    } else {
        let retry = question.retry.unwrap_or(default_retry);
        if retry > 0 {
            ErrorPolicy::Retry(retry)
        } else {
            ErrorPolicy::Abort
        }
    }
}

// ── Model requirements ─────────────────────────────────────────────────────

fn compile_model_requirements(
    question: &Question,
    default_model: &Option<String>,
    default_temperature: Option<f32>,
) -> ModelRequirements {
    ModelRequirements {
        tier: None,
        model: question.model.clone().or_else(|| default_model.clone()),
        temperature: Some(
            question
                .temperature
                .unwrap_or_else(|| default_temperature.unwrap_or(0.3)),
        ),
    }
}

// ── Context entries ────────────────────────────────────────────────────────

fn compile_context(question: &Question) -> Vec<ContextEntry> {
    let mut entries = Vec::new();

    if let Some(ref ctx) = question.context {
        for entry in ctx {
            match entry.as_str() {
                "L0 web edges" => {
                    entries.push(ContextEntry {
                        label: "web_edges".to_string(),
                        reference: None,
                        loader: Some("web_edge_summary".to_string()),
                        params: Some(json!({
                            "depth": 0,
                            "mode": "external",
                            "max_edges": 24,
                        })),
                    });
                }
                "L1 web edges" => {
                    entries.push(ContextEntry {
                        label: "web_edges".to_string(),
                        reference: None,
                        loader: Some("web_edge_summary".to_string()),
                        params: Some(json!({
                            "depth": 1,
                            "mode": "external",
                            "max_edges": 18,
                        })),
                    });
                }
                "L2 web edges" => {
                    entries.push(ContextEntry {
                        label: "web_edges".to_string(),
                        reference: None,
                        loader: Some("web_edge_summary".to_string()),
                        params: Some(json!({
                            "depth": 2,
                            "mode": "external",
                            "max_edges": 18,
                        })),
                    });
                }
                "L0 classification tags" => {
                    entries.push(ContextEntry {
                        label: "classification_tags".to_string(),
                        reference: Some("$l0_classification.output".to_string()),
                        loader: None,
                        params: None,
                    });
                }
                "sibling headlines" => {
                    entries.push(ContextEntry {
                        label: "sibling_headlines".to_string(),
                        reference: None,
                        loader: Some("sibling_cluster_context".to_string()),
                        params: None,
                    });
                }
                _ => {
                    // Unknown context reference — include as-is for forward compat
                    entries.push(ContextEntry {
                        label: entry.clone(),
                        reference: Some(format!("${}", entry.to_lowercase().replace(' ', "_"))),
                        loader: None,
                        params: None,
                    });
                }
            }
        }
    }

    entries
}

// ── Constraints ────────────────────────────────────────────────────────────

fn compile_constraints(question: &Question) -> Option<Vec<Constraint>> {
    let constraints = question.constraints.as_ref()?;
    let mut result = Vec::new();

    if let Some(min) = constraints.min_groups {
        result.push(Constraint {
            kind: "min_groups".to_string(),
            message: Some(format!("must produce at least {} groups", min)),
            expression: Some(format!("count($output.groups) >= {}", min)),
        });
    }
    if let Some(max) = constraints.max_groups {
        result.push(Constraint {
            kind: "max_groups".to_string(),
            message: Some(format!("must produce at most {} groups", max)),
            expression: Some(format!("count($output.groups) <= {}", max)),
        });
    }
    if let Some(max_per) = constraints.max_items_per_group {
        result.push(Constraint {
            kind: "max_items_per_group".to_string(),
            message: Some(format!("no group may have more than {} items", max_per)),
            expression: Some(format!(
                "max($output.groups[*].items.length) <= {}",
                max_per
            )),
        });
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

// ── Variants → instruction_map ─────────────────────────────────────────────

fn compile_variants(question: &Question) -> Option<HashMap<String, String>> {
    question.variants.clone()
}

// ── Primitive derivation ───────────────────────────────────────────────────

fn derive_primitive(creates: &str) -> String {
    match creates {
        "L0 nodes" => "extract".to_string(),
        "L0 classification tags" => "classify".to_string(),
        "L1 topic assignments" | "L1 thread assignments" => "classify".to_string(),
        "L1 nodes" => "synthesize".to_string(),
        "L2 nodes" => "synthesize".to_string(),
        "web edges between L0 nodes"
        | "web edges between L1 nodes"
        | "web edges between L2 nodes" => "web".to_string(),
        "apex" => "synthesize".to_string(),
        _ => "unknown".to_string(),
    }
}

// ── Compact input check ────────────────────────────────────────────────────

fn should_compact_input(question: &Question) -> bool {
    if matches!(question.about.as_str(), "all L0 topics at once") {
        return true;
    }

    matches!(
        question.creates.as_str(),
        "web edges between L0 nodes" | "web edges between L1 nodes" | "web edges between L2 nodes"
    )
}

// ── Cost estimation ────────────────────────────────────────────────────────

fn estimate_cost(question: &Question) -> CostEstimate {
    let is_foreach = matches!(
        question.about.as_str(),
        "each file individually"
            | "each chunk individually"
            | "each L1 topic's assigned L0 nodes"
            | "each L1 thread's assigned L0 nodes"
            | "each L1 topic's assigned L0 nodes, ordered chronologically"
            | "each L1 thread's assigned L0 nodes, ordered chronologically"
    ) || question.about.starts_with("the first ");

    let saves_node = matches!(
        question.creates.as_str(),
        "L0 nodes" | "L1 nodes" | "L2 nodes" | "apex"
    );

    if is_foreach {
        CostEstimate {
            billable_calls: 10,
            estimated_output_nodes: if saves_node { 10 } else { 0 },
        }
    } else if question.creates.starts_with("web edges") {
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

// ── Converge expansion for L2 nodes ────────────────────────────────────────

fn compile_converge_question(
    question: &Question,
    step_id: &str,
    depends_on: &[String],
    default_model: &Option<String>,
    default_temperature: Option<f32>,
    default_retry: u32,
) -> Result<Vec<Step>> {
    let error_policy = compile_error_policy(question, default_retry);
    let context_entries = compile_context(question);
    let classify_response_schema = if question.creates == "L2 nodes" {
        Some(json!({
            "type": "object",
            "properties": {
                "clusters": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "2-6 word cluster label — must be unique across all clusters"
                            },
                            "description": {
                                "type": "string",
                                "description": "1-2 sentences on what this architectural area covers"
                            },
                            "node_ids": {
                                "type": "array",
                                "minItems": 1,
                                "items": { "type": "string" },
                                "description": "IDs of nodes in this cluster (e.g., L1-000, L1-003)"
                            }
                        },
                        "required": ["name", "description", "node_ids"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["clusters"],
            "additionalProperties": false
        }))
    } else {
        None
    };

    let config = ConvergeConfig {
        over: "$l1_synthesis".to_string(),
        max_rounds: 8,
        shortcut_at: 4,
        reduce_instruction: question.prompt.clone(),
        classify_instruction: question
            .cluster_prompt
            .clone()
            .unwrap_or_else(|| question.prompt.clone()),
        classify_model: question
            .cluster_model
            .clone()
            .or_else(|| question.model.clone())
            .or_else(|| default_model.clone()),
        classify_response_schema,
        reduce_model: question.model.clone().or_else(|| default_model.clone()),
        reduce_response_schema: None,
        reduce_model_tier: None,
        reduce_temperature: Some(
            question
                .temperature
                .unwrap_or_else(|| default_temperature.unwrap_or(0.3)),
        ),
        error_policy,
        node_id_pattern: Some("L{depth}-{index:03}".to_string()),
        starting_depth: Some(source_depth_for_scope(&question.about).unwrap_or(1)),
        context_entries,
    };

    let mut converge_steps = expand_converge(step_id, &config)?;

    // Wire dependencies from the adapter level
    for step in &mut converge_steps {
        if step.depends_on.is_empty() {
            step.depends_on = depends_on.to_vec();
        }
    }

    Ok(converge_steps)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::question_yaml::QuestionDefaults;

    fn make_minimal_question_set(content_type: &str, questions: Vec<Question>) -> QuestionSet {
        QuestionSet {
            r#type: content_type.to_string(),
            version: "3.0".to_string(),
            defaults: QuestionDefaults {
                model: Some("inception/mercury-2".to_string()),
                temperature: Some(0.3),
                retry: Some(2),
            },
            questions,
        }
    }

    fn make_question(ask: &str, about: &str, creates: &str) -> Question {
        Question {
            ask: ask.to_string(),
            about: about.to_string(),
            creates: creates.to_string(),
            prompt: "test prompt content".to_string(),
            cluster_prompt: None,
            model: None,
            cluster_model: None,
            temperature: None,
            parallel: None,
            retry: None,
            optional: None,
            variants: None,
            constraints: None,
            context: None,
            sequential_context: None,
            preview_lines: None,
        }
    }

    fn compile_code_yaml() -> ExecutionPlan {
        let yaml = include_str!("../../../chains/questions/code.yaml");
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        compile_question_set(&qs, Path::new("/tmp")).unwrap()
    }

    fn compile_document_yaml() -> ExecutionPlan {
        let yaml = include_str!("../../../chains/questions/document.yaml");
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        compile_question_set(&qs, Path::new("/tmp")).unwrap()
    }

    // ── Code YAML compile tests ────────────────────────────────────────

    #[test]
    fn code_yaml_compiles_with_expected_steps() {
        let plan = compile_code_yaml();
        // 8 questions, but L2 nodes expands to converge (1 shortcut + 8 rounds * 4 = 33 steps)
        // So: l0_extract, clustering, l0_webbing, l1_synthesis, l1_webbing,
        //     l2_synthesis_shortcut + l2_synthesis_r0..r7 * 4, l2_webbing, apex
        // = 5 straight-line + 33 converge + 2 more = 7 straight + 33 converge = 40
        // Verify it validates without error (step count is a rough check)
        assert!(
            plan.steps.len() > 7,
            "should have more steps due to converge expansion"
        );

        // Check expected step names are present
        let ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"l0_extract"), "missing l0_extract");
        assert!(ids.contains(&"clustering"), "missing clustering");
        assert!(ids.contains(&"l0_webbing"), "missing l0_webbing");
        assert!(ids.contains(&"l1_synthesis"), "missing l1_synthesis");
        assert!(ids.contains(&"l1_webbing"), "missing l1_webbing");
        assert!(ids.contains(&"l2_webbing"), "missing l2_webbing");
        assert!(ids.contains(&"apex"), "missing apex");
    }

    #[test]
    fn document_yaml_has_classification_step() {
        let plan = compile_document_yaml();
        let ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(
            ids.contains(&"l0_classification"),
            "document should have classification step"
        );
        assert!(
            ids.contains(&"l0_extract"),
            "document should have extract step"
        );
    }

    #[test]
    fn document_yaml_has_more_steps_than_code() {
        let code_plan = compile_code_yaml();
        let doc_plan = compile_document_yaml();
        // Document has extra classification step + extra context deps
        let code_straight = code_plan
            .steps
            .iter()
            .filter(|s| s.converge_metadata.is_none())
            .count();
        let doc_straight = doc_plan
            .steps
            .iter()
            .filter(|s| s.converge_metadata.is_none())
            .count();
        assert!(
            doc_straight > code_straight,
            "document ({}) should have more straight-line steps than code ({})",
            doc_straight,
            code_straight
        );
    }

    // ── about: → iteration mode tests ──────────────────────────────────

    #[test]
    fn about_each_file_maps_to_parallel_foreach() {
        let q = make_question("test", "each file individually", "L0 nodes");
        let qs = make_minimal_question_set("code", vec![q]);
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = &plan.steps[0];
        let iter = step.iteration.as_ref().expect("should have iteration");
        assert_eq!(iter.mode, IterationMode::Parallel);
        assert_eq!(iter.over.as_deref(), Some("$chunks"));
        assert_eq!(iter.shape, Some(IterationShape::ForEach));
    }

    #[test]
    fn about_all_l0_topics_maps_to_single() {
        let q = make_question("test", "all L0 topics at once", "L1 topic assignments");
        let qs = make_minimal_question_set(
            "code",
            vec![
                make_question("extract", "each file individually", "L0 nodes"),
                q,
            ],
        );
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = plan.steps.iter().find(|s| s.id == "clustering").unwrap();
        assert!(
            step.iteration.is_none(),
            "single mode has no iteration directive"
        );
        assert!(
            step.compact_inputs,
            "clustering should request compact input shaping"
        );
        let assignment_schema = step
            .response_schema
            .as_ref()
            .and_then(|schema| schema.get("properties"))
            .and_then(|props| props.get("threads"))
            .and_then(|threads| threads.get("items"))
            .and_then(|thread| thread.get("properties"))
            .and_then(|props| props.get("assignments"))
            .and_then(|assignments| assignments.get("items"))
            .and_then(|item| item.get("required"))
            .and_then(|required| required.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            assignment_schema.iter().any(|entry| entry == "topic_index"),
            "clustering schema should require topic_index to match the code_cluster prompt"
        );
        assert!(
            assignment_schema.iter().any(|entry| entry == "topic_name"),
            "clustering schema should require topic_name to match the code_cluster prompt"
        );
        // Check input references L0 topics
        let input_str = serde_json::to_string(&step.input).unwrap();
        assert!(
            input_str.contains("topics"),
            "input should reference topics projection"
        );
    }

    #[test]
    fn about_each_l1_thread_maps_to_parallel_foreach_over_threads() {
        let q = make_question("test", "each L1 topic's assigned L0 nodes", "L1 nodes");
        let qs = make_minimal_question_set(
            "code",
            vec![
                make_question("extract", "each file individually", "L0 nodes"),
                make_question("cluster", "all L0 topics at once", "L1 topic assignments"),
                q,
            ],
        );
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = plan.steps.iter().find(|s| s.id == "l1_synthesis").unwrap();
        let iter = step.iteration.as_ref().expect("should have iteration");
        assert_eq!(iter.mode, IterationMode::Parallel);
        assert_eq!(iter.over.as_deref(), Some("$clustering.threads"));
    }

    // ── creates: → storage tests ───────────────────────────────────────

    #[test]
    fn creates_web_edges_maps_to_web_edges_storage() {
        let q = make_question("test", "all L0 nodes at once", "web edges between L0 nodes");
        let qs = make_minimal_question_set(
            "code",
            vec![
                make_question("extract", "each file individually", "L0 nodes"),
                q,
            ],
        );
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        let sd = step
            .storage_directive
            .as_ref()
            .expect("should have storage");
        assert_eq!(sd.kind, StorageKind::WebEdges);
        assert_eq!(sd.depth, Some(0));
    }

    #[test]
    fn webbing_steps_request_compact_input_shaping() {
        let qs = make_minimal_question_set(
            "code",
            vec![
                make_question("extract", "each file individually", "L0 nodes"),
                make_question("cluster", "all L0 topics at once", "L1 topic assignments"),
                make_question("synth", "each L1 topic's assigned L0 nodes", "L1 nodes"),
                make_question("web", "all L1 nodes at once", "web edges between L1 nodes"),
            ],
        );
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = plan.steps.iter().find(|s| s.id == "l1_webbing").unwrap();
        assert!(
            step.compact_inputs,
            "question-path webbing should request compact input shaping"
        );
    }

    #[test]
    fn creates_l2_nodes_triggers_converge_expansion() {
        let plan = compile_code_yaml();
        let converge_steps: Vec<&Step> = plan
            .steps
            .iter()
            .filter(|s| s.converge_metadata.is_some())
            .collect();
        assert!(
            !converge_steps.is_empty(),
            "L2 nodes should produce converge steps"
        );

        // Should have shortcut step
        let shortcut = converge_steps.iter().find(|s| s.id.contains("shortcut"));
        assert!(shortcut.is_some(), "converge should have shortcut step");
    }

    #[test]
    fn l2_converge_uses_cluster_prompt_and_cluster_model() {
        let question = Question {
            ask: "What are the major architectural domains?".to_string(),
            about: "all L1 nodes at once".to_string(),
            creates: "L2 nodes".to_string(),
            prompt: "reduce prompt".to_string(),
            cluster_prompt: Some("classify prompt".to_string()),
            model: Some("inception/mercury-2".to_string()),
            cluster_model: Some("qwen/qwen3.5-flash-02-23".to_string()),
            temperature: Some(0.2),
            parallel: None,
            retry: Some(3),
            optional: None,
            variants: None,
            constraints: None,
            context: None,
            sequential_context: None,
            preview_lines: None,
        };

        let qs = make_minimal_question_set("code", vec![question]);
        let plan = compile_question_set(&qs, Path::new(".")).expect("plan should compile");

        let classify = plan
            .steps
            .iter()
            .find(|s| s.id == "l2_synthesis_r0_classify")
            .expect("classify step");
        let reduce = plan
            .steps
            .iter()
            .find(|s| s.id == "l2_synthesis_r0_reduce")
            .expect("reduce step");

        assert_eq!(classify.instruction.as_deref(), Some("classify prompt"));
        assert_eq!(
            classify.model_requirements.model.as_deref(),
            Some("qwen/qwen3.5-flash-02-23")
        );
        assert!(classify.response_schema.is_some());

        assert_eq!(reduce.instruction.as_deref(), Some("reduce prompt"));
        assert_eq!(
            reduce.model_requirements.model.as_deref(),
            Some("inception/mercury-2")
        );
    }

    #[test]
    fn creates_apex_maps_to_single_step() {
        let plan = compile_code_yaml();
        let apex = plan.steps.iter().find(|s| s.id == "apex").unwrap();
        assert!(apex.iteration.is_none(), "apex should be single execution");
        let sd = apex
            .storage_directive
            .as_ref()
            .expect("apex should have storage");
        assert_eq!(sd.kind, StorageKind::Node);
        assert_eq!(sd.depth, Some(3));
    }

    #[test]
    fn apex_depth_tracks_highest_conceptual_layer() {
        let qs = make_minimal_question_set(
            "code",
            vec![
                make_question("Extract", "each file individually", "L0 nodes"),
                make_question("Cluster", "all L0 topics at once", "L1 thread assignments"),
                make_question(
                    "Synthesize",
                    "each L1 thread's assigned L0 nodes",
                    "L1 nodes",
                ),
                make_question("Apex", "all L1 nodes at once", "apex"),
            ],
        );
        let plan = compile_question_set(&qs, Path::new(".")).expect("plan should compile");
        let apex = plan.steps.iter().find(|s| s.id == "apex").unwrap();
        let sd = apex
            .storage_directive
            .as_ref()
            .expect("apex should have storage");

        assert_eq!(sd.depth, Some(2));
    }

    // ── variants → instruction_map ─────────────────────────────────────

    #[test]
    fn variants_populate_instruction_map() {
        let plan = compile_code_yaml();
        let extract = plan.steps.iter().find(|s| s.id == "l0_extract").unwrap();
        let imap = extract
            .instruction_map
            .as_ref()
            .expect("should have instruction_map");
        assert!(
            imap.contains_key("config files"),
            "should have config files variant"
        );
        assert!(
            imap.contains_key("frontend (.tsx, .jsx)"),
            "should have frontend variant"
        );
    }

    // ── constraints ────────────────────────────────────────────────────

    #[test]
    fn constraints_field_populated() {
        let plan = compile_code_yaml();
        let cluster = plan.steps.iter().find(|s| s.id == "clustering").unwrap();
        let constraints = cluster
            .constraints
            .as_ref()
            .expect("should have constraints");
        assert!(
            constraints.iter().any(|c| c.kind == "max_items_per_group"),
            "should have max_items_per_group"
        );
    }

    // ── context: → ContextEntry ────────────────────────────────────────

    #[test]
    fn context_l0_web_edges_maps_to_web_edge_summary_loader() {
        let plan = compile_code_yaml();
        let synth = plan.steps.iter().find(|s| s.id == "l1_synthesis").unwrap();
        let has_web_edge_ctx = synth
            .context
            .iter()
            .any(|c| c.loader.as_deref() == Some("web_edge_summary"));
        assert!(
            has_web_edge_ctx,
            "L1 synthesis should have web_edge_summary context"
        );
    }

    #[test]
    fn context_sibling_headlines_maps_to_sibling_cluster_context() {
        let plan = compile_code_yaml();
        // The L2 synthesis converge steps get sibling context from the expander,
        // but the question itself also declares "sibling headlines" in context.
        // Check that the converge reduce steps have it.
        let reduce_steps: Vec<&Step> = plan
            .steps
            .iter()
            .filter(|s| s.id.contains("_r0_reduce") && s.id.starts_with("l2"))
            .collect();
        if let Some(reduce) = reduce_steps.first() {
            let has_sibling = reduce
                .context
                .iter()
                .any(|c| c.loader.as_deref() == Some("sibling_cluster_context"));
            assert!(
                has_sibling,
                "converge reduce should have sibling_cluster_context"
            );
        }
    }

    // ── optional: true → ErrorPolicy::Skip ─────────────────────────────

    #[test]
    fn optional_true_maps_to_skip_policy() {
        let plan = compile_code_yaml();
        let l0_web = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        assert_eq!(l0_web.error_policy, ErrorPolicy::Skip);
    }

    // ── DAG validation ─────────────────────────────────────────────────

    #[test]
    fn dependencies_form_valid_dag() {
        let plan = compile_code_yaml();
        plan.validate()
            .expect("compiled plan should validate as a valid DAG");
    }

    #[test]
    fn document_plan_validates() {
        let plan = compile_document_yaml();
        plan.validate().expect("document plan should validate");
    }

    // ── Dependency wiring ──────────────────────────────────────────────

    #[test]
    fn apex_depends_on_l2_converge_terminals() {
        let plan = compile_code_yaml();
        let apex = plan.steps.iter().find(|s| s.id == "apex").unwrap();
        // Apex should depend on the L2 converge terminal steps
        assert!(!apex.depends_on.is_empty(), "apex should have dependencies");
        // Should depend on l2_webbing (which depends on l2 converge)
        // or directly on l2 converge terminals
        let depends_on_l2 = apex
            .depends_on
            .iter()
            .any(|d| d.contains("l2") || d.contains("webbing"));
        assert!(
            depends_on_l2,
            "apex should depend on L2 layer: {:?}",
            apex.depends_on
        );
    }

    #[test]
    fn l1_synthesis_depends_on_clustering_and_l0() {
        let plan = compile_code_yaml();
        let synth = plan.steps.iter().find(|s| s.id == "l1_synthesis").unwrap();
        assert!(
            synth.depends_on.contains(&"clustering".to_string()),
            "L1 synthesis should depend on clustering: {:?}",
            synth.depends_on
        );
    }

    // ── Sequential context ─────────────────────────────────────────────

    #[test]
    fn sequential_context_compiles_to_sequential_mode() {
        use crate::pyramid::question_yaml::SequentialContextConfig;

        let mut q = make_question("test", "each chunk individually", "L0 nodes");
        q.sequential_context = Some(SequentialContextConfig {
            mode: "accumulate".to_string(),
            max_chars: Some(8000),
            carry: Some("summary of prior chunks so far".to_string()),
        });
        let qs = make_minimal_question_set("conversation", vec![q]);
        let plan = compile_question_set(&qs, Path::new("/tmp")).unwrap();
        let step = &plan.steps[0];
        let iter = step.iteration.as_ref().expect("should have iteration");
        assert_eq!(iter.mode, IterationMode::Sequential);
        assert_eq!(iter.concurrency, Some(1));
        let acc = iter.accumulate.as_ref().expect("should have accumulator");
        assert_eq!(acc.max_chars, Some(8000));
    }

    // ── Preview lines ──────────────────────────────────────────────────

    #[test]
    fn preview_lines_scope_compiles_correctly() {
        let plan = compile_document_yaml();
        let classify = plan
            .steps
            .iter()
            .find(|s| s.id == "l0_classification")
            .unwrap();
        let iter = classify.iteration.as_ref().expect("should have iteration");
        assert_eq!(iter.mode, IterationMode::Parallel);
        // Input should contain preview_lines
        let input_str = serde_json::to_string(&classify.input).unwrap();
        assert!(
            input_str.contains("preview_lines") || input_str.contains("20"),
            "classification input should reference preview_lines: {}",
            input_str
        );
    }

    // ── Document-specific dependency chain ────────────────────────────

    #[test]
    fn document_l0_extract_depends_on_classification() {
        let plan = compile_document_yaml();
        let extract = plan.steps.iter().find(|s| s.id == "l0_extract").unwrap();
        assert!(
            extract
                .depends_on
                .contains(&"l0_classification".to_string()),
            "document L0 extract should depend on classification via context: {:?}",
            extract.depends_on
        );
    }

    #[test]
    fn document_l0_extract_has_classification_context_entry() {
        let plan = compile_document_yaml();
        let extract = plan.steps.iter().find(|s| s.id == "l0_extract").unwrap();
        let has_classification_ctx = extract.context.iter().any(|c| {
            c.label == "classification_tags"
                && c.reference.as_deref() == Some("$l0_classification.output")
        });
        assert!(
            has_classification_ctx,
            "document L0 extract should have classification_tags context entry: {:?}",
            extract.context
        );
    }

    #[test]
    fn document_clustering_depends_on_both_extract_and_classification() {
        let plan = compile_document_yaml();
        let cluster = plan.steps.iter().find(|s| s.id == "clustering").unwrap();
        assert!(
            cluster.depends_on.contains(&"l0_extract".to_string()),
            "clustering should depend on l0_extract: {:?}",
            cluster.depends_on
        );
        assert!(
            cluster
                .depends_on
                .contains(&"l0_classification".to_string()),
            "clustering should depend on l0_classification via context: {:?}",
            cluster.depends_on
        );
    }

    #[test]
    fn document_l1_synthesis_has_both_web_and_classification_context() {
        let plan = compile_document_yaml();
        let synth = plan.steps.iter().find(|s| s.id == "l1_synthesis").unwrap();
        let has_web_ctx = synth
            .context
            .iter()
            .any(|c| c.loader.as_deref() == Some("web_edge_summary"));
        let has_class_ctx = synth
            .context
            .iter()
            .any(|c| c.label == "classification_tags");
        assert!(
            has_web_ctx,
            "document L1 synthesis should have web_edge_summary context"
        );
        assert!(
            has_class_ctx,
            "document L1 synthesis should have classification_tags context"
        );
    }

    #[test]
    fn document_l0_webbing_is_optional() {
        let plan = compile_document_yaml();
        let webbing = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        assert_eq!(
            webbing.error_policy,
            ErrorPolicy::Skip,
            "document L0 webbing should be optional (Skip policy)"
        );
    }

    #[test]
    fn code_yaml_l0_webbing_uses_correct_model() {
        let plan = compile_code_yaml();
        let webbing = plan.steps.iter().find(|s| s.id == "l0_webbing").unwrap();
        assert_eq!(
            webbing.model_requirements.model.as_deref(),
            Some("inception/mercury-2"),
            "L0 webbing should use the default model unless overridden"
        );
    }

    #[test]
    fn code_yaml_clustering_has_max_items_per_group_constraint() {
        let plan = compile_code_yaml();
        let cluster = plan.steps.iter().find(|s| s.id == "clustering").unwrap();
        let constraints = cluster
            .constraints
            .as_ref()
            .expect("should have constraints");
        let max_per = constraints.iter().find(|c| c.kind == "max_items_per_group");
        assert!(
            max_per.is_some(),
            "should have max_items_per_group constraint"
        );
        // Code YAML uses max_items_per_group: 12
        let expr = max_per.unwrap().expression.as_ref().unwrap();
        assert!(
            expr.contains("12"),
            "max_items_per_group should reference 12: {}",
            expr
        );
    }
}
