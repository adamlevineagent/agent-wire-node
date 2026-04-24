// pyramid/question_decomposition.rs — Question Decomposition Chain (P2.2)
//
// Takes a natural language apex question and decomposes it into a question tree
// via LLM calls, then converts that tree into a QuestionSet that the existing
// Question Compiler (P2.1) can compile to IR.
//
// Flow:
//   1. User provides apex question + config
//   2. decompose_question() calls LLM to break into sub-questions (bounded recursion)
//   3. question_tree_to_question_set() bridges to the QuestionSet format
//   4. question_compiler::compile_question_set() compiles to IR
//   5. execute_plan() runs the IR through the standard executor

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::db;
use super::event_bus::{TaggedBuildEvent, TaggedKind};
use super::llm::{self, AuditContext, LlmCallOptions, LlmConfig};
use super::question_yaml::{Question, QuestionDefaults, QuestionSet};
use super::step_context::make_step_ctx_from_llm_config;

fn default_decomposition_model_tier() -> String {
    "max".to_string()
}

fn default_decomposition_temperature() -> f32 {
    0.3
}

fn default_decomposition_max_tokens() -> usize {
    4096
}

fn default_sibling_review_max_tokens() -> usize {
    2048
}

fn audit_for(
    audit: Option<&AuditContext>,
    step_name: &str,
    depth: Option<i64>,
) -> Option<AuditContext> {
    audit.map(|a| AuditContext {
        conn: Arc::clone(&a.conn),
        slug: a.slug.clone(),
        build_id: a.build_id.clone(),
        node_id: None,
        step_name: step_name.to_string(),
        call_purpose: "question_decompose".to_string(),
        depth,
    })
}

fn effective_decompose_step_name<'a>(
    audit: Option<&'a AuditContext>,
    fallback: &'a str,
) -> &'a str {
    audit
        .and_then(|a| {
            let step_name = a.step_name.trim();
            if step_name.is_empty() {
                None
            } else {
                Some(step_name)
            }
        })
        .unwrap_or(fallback)
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for a question decomposition run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionConfig {
    /// The top-level question to decompose.
    pub apex_question: String,
    /// Content type: "code" or "document".
    pub content_type: String,
    /// Controls sub-question breadth/depth (1-5). Higher = more sub-questions.
    /// 1 = minimal (3-4 sub-questions), 5 = exhaustive (6-8 sub-questions).
    pub granularity: u32,
    /// Maximum decomposition depth (default 3). Each level is one LLM call.
    pub max_depth: u32,
    /// Summary of source files/folders for context. Optional but strongly recommended.
    /// Helps the LLM produce relevant sub-questions. Could be a directory listing,
    /// file count summary, or key filenames.
    pub folder_map: Option<String>,
    /// 11-G/H/Q: Path to the chains directory for loading .md prompts.
    /// When set, decomposition and horizontal review prompts are loaded from
    /// `chains/prompts/question/decompose.md` and `horizontal_review.md`.
    #[serde(skip)]
    pub chains_dir: Option<std::path::PathBuf>,
    /// WS13-A: Audience string flows into every LLM call in the pipeline.
    /// When present, decomposition prompts use the audience's vocabulary and perspective.
    pub audience: Option<String>,
    /// Chain YAML model tier for recursive decomposition calls.
    /// This used to be hardcoded to `max`; keep a default for legacy callers.
    #[serde(default = "default_decomposition_model_tier")]
    pub model_tier: String,
    /// Chain YAML temperature for decomposition calls.
    #[serde(default = "default_decomposition_temperature")]
    pub temperature: f32,
    /// Completion token cap for decomposition layer calls.
    #[serde(default = "default_decomposition_max_tokens")]
    pub max_tokens: usize,
    /// Completion token cap for sibling review calls.
    #[serde(default = "default_sibling_review_max_tokens")]
    pub sibling_review_max_tokens: usize,
}

/// 11-Q: Replace `{{variable}}` template placeholders in a prompt string.
/// Uses double-brace convention to avoid conflicts with single-brace format strings.
pub fn render_prompt_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let placeholder = format!("{{{{{}}}}}", key); // produces {{key}}
        result = result.replace(&placeholder, value);
    }
    result
}

impl Default for DecompositionConfig {
    fn default() -> Self {
        Self {
            apex_question: String::new(),
            content_type: "code".to_string(),
            granularity: 3,
            max_depth: 3,
            folder_map: None,
            chains_dir: None,
            audience: None,
            model_tier: default_decomposition_model_tier(),
            temperature: default_decomposition_temperature(),
            max_tokens: default_decomposition_max_tokens(),
            sibling_review_max_tokens: default_sibling_review_max_tokens(),
        }
    }
}

// ── Question Tree ─────────────────────────────────────────────────────────────

/// A decomposed question tree — the output of the architect phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionTree {
    /// The root question node.
    pub apex: QuestionNode,
    /// Content type this tree was built for.
    pub content_type: String,
    /// Config used to produce this tree.
    pub config: DecompositionConfig,
    /// Target audience for the pyramid output. Extracted from the characterization
    /// phase so it can flow into extraction, synthesis, and answering prompts.
    /// When set, all LLM prompts will be shaped for this audience (e.g. "a smart
    /// high school graduate, not a developer") instead of defaulting to technical jargon.
    #[serde(default)]
    pub audience: Option<String>,
}

/// A single node in the question tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionNode {
    /// Stable local handle ID, e.g. `Q-L2-003`.
    /// NOT produced by the LLM. The allocator assigns layer-shaped handles and
    /// keeps a semantic de-dupe map so a shared question still has one identity
    /// across DAG parents.
    #[serde(default)]
    pub id: String,
    /// The natural language question.
    pub question: String,
    /// Scope declaration — what this question is about.
    /// Leaf nodes: "each file individually" (L0 extraction).
    /// Non-leaf nodes: scope based on position (e.g., "all L0 topics at once", "all L1 nodes at once").
    pub about: String,
    /// What this question's answer produces (e.g., "L0 nodes", "L1 nodes", "apex").
    pub creates: String,
    /// Hint for the LLM prompt — what to emphasize when answering.
    pub prompt_hint: String,
    /// Child questions — empty for leaf nodes.
    pub children: Vec<QuestionNode>,
    /// Whether this is a terminal node (maps to L0 extraction).
    pub is_leaf: bool,
}

/// Preview of what a decomposition will produce — cost/time estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionPreview {
    /// Total number of question nodes in the tree.
    pub total_nodes: u32,
    /// Number of leaf nodes (each becomes an L0 extraction pass).
    pub leaf_nodes: u32,
    /// Estimated LLM calls for the decomposition phase itself.
    pub decomposition_llm_calls: u32,
    /// Estimated LLM calls for the full build (extraction + synthesis).
    pub estimated_build_llm_calls: u32,
    /// Human-readable tree summary.
    pub tree_summary: String,
    /// Estimated depth of the resulting pyramid.
    pub estimated_pyramid_depth: u32,
}

// ── Stable ID Assignment ──────────────────────────────────────────────────────

/// Assign deterministic IDs to every node in the question tree.
///
/// ID format: `Q-L{visual_layer}-{index:03}` where `visual_layer` is the
/// question's pyramid layer (`Q-L1-*` for evidence-facing leaf questions).
/// A semantic key still de-dupes repeated/shared question text before a new
/// handle is allocated.
///
/// This is intentionally independent of final tree height: decomposition can
/// persist a finalized subtree before sibling branches finish.
pub fn assign_question_ids(tree: &mut QuestionTree) {
    let max_tree_depth = compute_max_depth(&tree.apex);
    let mut allocator = QuestionIdAllocator::default();
    assign_ids_recursive(&mut tree.apex, 0, max_tree_depth, &mut allocator);
}

/// Compute the max depth of the tree (number of edges from root to deepest leaf).
fn compute_max_depth(node: &QuestionNode) -> u32 {
    if node.children.is_empty() {
        return 0;
    }
    node.children
        .iter()
        .map(|c| 1 + compute_max_depth(c))
        .max()
        .unwrap_or(0)
}

/// Recursively assign IDs. `tree_depth` is root-down storage depth:
/// apex/root = 0, first-level sub-questions = 1, and so on.
fn assign_ids_recursive(
    node: &mut QuestionNode,
    tree_depth: u32,
    max_tree_depth: u32,
    allocator: &mut QuestionIdAllocator,
) {
    assign_node_id(node, tree_depth, max_tree_depth, allocator);
    for child in &mut node.children {
        assign_ids_recursive(child, tree_depth + 1, max_tree_depth, allocator);
    }
}

fn assign_node_id(
    node: &mut QuestionNode,
    tree_depth: u32,
    max_tree_depth: u32,
    allocator: &mut QuestionIdAllocator,
) {
    let visual_layer = visual_layer_from_tree_depth(max_tree_depth + 1, tree_depth);
    node.id = allocator.get_or_allocate(&node.question, visual_layer);
}

#[derive(Debug, Default, Clone)]
struct QuestionIdAllocator {
    id_by_semantic_key: HashMap<String, String>,
    next_index_by_layer: HashMap<i64, usize>,
}

impl QuestionIdAllocator {
    fn register_existing(&mut self, question: &str, id: &str) {
        self.id_by_semantic_key
            .entry(normalize_question_identity(question))
            .or_insert_with(|| id.to_string());
        if let Some((layer, index)) = parse_question_handle_id(id) {
            let next = self.next_index_by_layer.entry(layer).or_insert(0);
            *next = (*next).max(index + 1);
        }
    }

    fn get_or_allocate(&mut self, question: &str, visual_layer: i64) -> String {
        let key = normalize_question_identity(question);
        if let Some(existing) = self.id_by_semantic_key.get(&key) {
            return existing.clone();
        }

        let next = self.next_index_by_layer.entry(visual_layer).or_insert(0);
        let id = format!("Q-L{}-{:03}", visual_layer, *next);
        *next += 1;
        self.id_by_semantic_key.insert(key, id.clone());
        id
    }
}

fn visual_layer_from_tree_depth(max_visual_layer: u32, tree_depth: u32) -> i64 {
    max_visual_layer.saturating_sub(tree_depth).max(1) as i64
}

fn parse_question_handle_id(id: &str) -> Option<(i64, usize)> {
    let rest = id.strip_prefix("Q-L")?;
    let (layer, index) = rest.split_once('-')?;
    Some((layer.parse().ok()?, index.parse().ok()?))
}

fn normalize_question_identity(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_end_matches('?')
        .to_lowercase()
}

// ── Layer Question Extraction ─────────────────────────────────────────────────

/// Extract per-layer question sets from a question tree.
///
/// Leaves are layer 1 (reserved for evidence answering). Their parents are layer 2.
/// Root/apex is the highest layer. Layer 0 is reserved for L0 extraction nodes.
/// Returns a HashMap<layer, Vec<LayerQuestion>>.
///
/// Requires `assign_question_ids` to have been called first (IDs must be populated).
pub fn extract_layer_questions(
    tree: &QuestionTree,
) -> std::collections::HashMap<i64, Vec<super::types::LayerQuestion>> {
    let max_depth = compute_max_depth(&tree.apex);
    let mut result: std::collections::HashMap<i64, Vec<super::types::LayerQuestion>> =
        std::collections::HashMap::new();
    let mut seen = HashSet::new();
    collect_layer_questions(&tree.apex, max_depth, 0, &mut result, &mut seen);
    result
}

fn collect_layer_questions(
    node: &QuestionNode,
    max_depth: u32,
    current_level: u32,
    result: &mut std::collections::HashMap<i64, Vec<super::types::LayerQuestion>>,
    seen: &mut HashSet<String>,
) {
    let depth = (max_depth.saturating_sub(current_level) + 1) as i64;

    if seen.insert(node.id.clone()) {
        result
            .entry(depth)
            .or_default()
            .push(super::types::LayerQuestion {
                question_id: node.id.clone(),
                question_text: node.question.clone(),
                layer: depth,
                about: node.about.clone(),
                creates: node.creates.clone(),
            });
    }

    for child in &node.children {
        collect_layer_questions(child, max_depth, current_level + 1, result, seen);
    }
}

// ── Delta Decomposition ───────────────────────────────────────────────────────

/// Result of delta decomposition: which questions are new vs reused.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaDecompositionResult {
    /// The full question tree for the new apex (includes both new and reused questions).
    pub tree: QuestionTree,
    /// IDs of questions from existing overlays that can be reused (their answer nodes
    /// are still valid for the new apex).
    pub reused_question_ids: Vec<String>,
    /// New questions that need evidence answering.
    pub new_questions: Vec<super::types::LayerQuestion>,
}

/// Delta decomposition: given existing overlay answers, determine what NEW questions
/// the new apex question needs vs what can be reused from existing answers.
///
/// This is the key to making second+ questions on the same pyramid fast: shared
/// sub-questions reuse existing answer nodes without re-running the LLM.
pub async fn decompose_question_delta(
    config: &DecompositionConfig,
    llm_config: &LlmConfig,
    existing_tree: &QuestionTree,
    existing_answers: &[super::types::PyramidNode],
    chains_dir: Option<&std::path::Path>,
    evidence_set_context: Option<&str>,
    gap_context: Option<&str>,
    audit: Option<&AuditContext>,
) -> Result<DeltaDecompositionResult> {
    if config.apex_question.trim().is_empty() {
        return Err(anyhow!("apex_question cannot be empty"));
    }

    // Build context about existing answers for the LLM
    let existing_context = existing_answers
        .iter()
        .map(|n| {
            let summary: String = n.distilled.chars().take(200).collect();
            format!("- [{}] {}: {}", n.id, n.headline, summary)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Collect existing question texts for reuse detection
    let mut existing_questions: Vec<(String, String)> = Vec::new(); // (question_id, question_text)
    collect_existing_questions(&existing_tree.apex, &mut existing_questions);

    let existing_q_context = existing_questions
        .iter()
        .map(|(id, q)| format!("- [{}] {}", id, q))
        .collect::<Vec<_>>()
        .join("\n");

    // Build optional context blocks for evidence sets and gaps
    let evidence_block = evidence_set_context
        .map(|ctx| format!("\n\nEXISTING EVIDENCE SETS:\n{}", ctx))
        .unwrap_or_default();
    let gap_block = gap_context
        .map(|ctx| format!("\n\nKNOWN EVIDENCE GAPS:\n{}", ctx))
        .unwrap_or_default();

    // Ask the LLM to decompose the new question, given what already exists
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/decompose_delta.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("existing_questions", &existing_q_context),
                ("existing_answers", &existing_context),
                ("evidence_set_context_block", &evidence_block),
                ("gap_context_block", &gap_block),
            ],
        ),
        None => {
            warn!("decompose_delta.md not found — using inline fallback");
            format!(
                r#"You are a question architect for knowledge pyramids. A knowledge pyramid already exists with answered questions. A NEW apex question is being asked about the SAME source material.

Your job: decompose the new question into sub-questions, but REUSE existing answered questions where they overlap.

EXISTING ANSWERED QUESTIONS:
{existing_q_context}

EXISTING ANSWER SUMMARIES:
{existing_context}
{evidence_block}{gap_block}

For the new apex question, produce sub-questions. For each sub-question, indicate whether it can be answered by an existing question (reuse) or needs fresh evidence gathering (new).

Respond in JSON:
{{
  "sub_questions": [
    {{
      "question": "the sub-question text",
      "reuse_id": "existing question ID if this reuses an existing answer, or null if new",
      "prompt_hint": "hint for how to answer this question",
      "is_leaf": true/false
    }}
  ]
}}

Return ONLY the JSON object."#
            )
        }
    };

    let user_prompt = format!(
        "New apex question: \"{}\"\n\nContent type: {}\n\n{}",
        config.apex_question,
        config.content_type,
        config
            .folder_map
            .as_deref()
            .unwrap_or("(no additional context)")
    );

    let step_name = effective_decompose_step_name(audit, "question_delta_decompose");
    let cache_ctx = make_step_ctx_from_llm_config(
        llm_config,
        step_name,
        "question_decompose",
        0,
        None,
        &system_prompt,
        &config.model_tier,
        None,
        None,
    )
    .await;
    let audit_ctx = audit_for(audit, step_name, Some(0));
    let response = llm::call_model_unified_with_audit_and_ctx(
        llm_config,
        cache_ctx.as_ref(),
        audit_ctx.as_ref(),
        &system_prompt,
        &user_prompt,
        config.temperature,
        config.max_tokens,
        None,
        LlmCallOptions::default(),
    )
    .await?;

    let json_value = llm::extract_json(&response.content)?;

    // Parse the response
    #[derive(Deserialize)]
    struct DeltaSubQuestion {
        question: String,
        reuse_id: Option<String>,
        prompt_hint: String,
        is_leaf: bool,
    }
    #[derive(Deserialize)]
    struct DeltaResponse {
        sub_questions: Vec<DeltaSubQuestion>,
    }

    let delta_resp: DeltaResponse = serde_json::from_value(json_value)
        .map_err(|e| anyhow!("Failed to parse delta decomposition response: {}", e))?;

    let mut reused_question_ids = Vec::new();
    let mut children = Vec::new();

    let existing_q_map: std::collections::HashMap<&str, &str> = existing_questions
        .iter()
        .map(|(id, q)| (id.as_str(), q.as_str()))
        .collect();

    for sq in &delta_resp.sub_questions {
        if let Some(ref reuse_id) = sq.reuse_id {
            if existing_q_map.contains_key(reuse_id.as_str()) {
                reused_question_ids.push(reuse_id.clone());
                // Create a placeholder node that references the existing answer
                children.push(QuestionNode {
                    id: reuse_id.clone(),
                    question: sq.question.clone(),
                    about: "reused from existing overlay".to_string(),
                    creates: "reused answer".to_string(),
                    prompt_hint: sq.prompt_hint.clone(),
                    children: vec![],
                    is_leaf: true,
                });
                continue;
            }
        }
        // New question — will need evidence answering
        children.push(QuestionNode {
            id: String::new(),
            question: sq.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: sq.prompt_hint.clone(),
            children: vec![],
            is_leaf: sq.is_leaf,
        });
    }

    let apex_node = QuestionNode {
        id: String::new(),
        question: config.apex_question.clone(),
        about: "all top-level nodes at once".to_string(),
        creates: "apex".to_string(),
        prompt_hint: "Synthesize all sub-answers into a comprehensive answer to the apex question."
            .to_string(),
        children,
        is_leaf: false,
    };

    let mut tree = QuestionTree {
        apex: apex_node,
        content_type: config.content_type.clone(),
        config: config.clone(),
        audience: existing_tree.audience.clone(),
    };

    assign_question_ids(&mut tree);

    // Extract new questions (those NOT in reused set)
    let all_layer_qs = extract_layer_questions(&tree);
    let reused_set: std::collections::HashSet<&str> =
        reused_question_ids.iter().map(|s| s.as_str()).collect();

    let new_questions: Vec<super::types::LayerQuestion> = all_layer_qs
        .into_iter()
        .flat_map(|(_, qs)| qs)
        .filter(|q| !reused_set.contains(q.question_id.as_str()))
        .filter(|q| q.layer < tree.apex.children.len() as i64) // exclude apex itself from evidence loop
        .collect();

    info!(
        apex = %config.apex_question,
        total_sub_questions = delta_resp.sub_questions.len(),
        reused = reused_question_ids.len(),
        new = new_questions.len(),
        "delta decomposition complete"
    );

    Ok(DeltaDecompositionResult {
        tree,
        reused_question_ids,
        new_questions,
    })
}

/// Collect all (question_id, question_text) pairs from a question tree.
fn collect_existing_questions(node: &QuestionNode, out: &mut Vec<(String, String)>) {
    if !node.id.is_empty() {
        out.push((node.id.clone(), node.question.clone()));
    }
    for child in &node.children {
        collect_existing_questions(child, out);
    }
}

// ── Decomposition ─────────────────────────────────────────────────────────────

/// Decompose an apex question into a question tree via LLM calls.
///
/// Uses the model tier resolved from the chain step YAML. Decomposition is
/// topology-shaping judgment work, but the operator can route it per chain.
///
/// The bounded unroll pattern limits recursion to `config.max_depth` levels.
/// Each level is a single LLM call that decomposes ALL questions at that level
/// simultaneously (so they can see each other and avoid overlap).
pub async fn decompose_question(
    config: &DecompositionConfig,
    llm_config: &LlmConfig,
    _tier1: &super::Tier1Config,
    tier2: &super::Tier2Config,
    audit: Option<&AuditContext>,
) -> Result<QuestionTree> {
    if config.apex_question.trim().is_empty() {
        return Err(anyhow!("apex_question cannot be empty"));
    }
    if config.max_depth == 0 {
        return Err(anyhow!("max_depth must be at least 1"));
    }

    let granularity = config.granularity.clamp(1, 5);
    let (min_subs, max_subs) = granularity_to_range(granularity, tier2);

    info!(
        apex = %config.apex_question,
        content_type = %config.content_type,
        granularity = granularity,
        max_depth = config.max_depth,
        "starting question decomposition"
    );

    // First call: apex → L1 sub-questions
    let sub_questions = call_decomposition_llm(
        &config.apex_question,
        &config.content_type,
        config.folder_map.as_deref(),
        min_subs,
        max_subs,
        1, // depth 1
        llm_config,
        config.chains_dir.as_deref(),
        config.audience.as_deref(),
        &config.model_tier,
        config.temperature,
        config.max_tokens,
        audit,
    )
    .await?;

    // Build children recursively (bounded by max_depth)
    let mut children = Vec::new();
    for (i, sq) in sub_questions.iter().enumerate() {
        info!(
            branch = i + 1,
            total = sub_questions.len(),
            question = %sq.question,
            is_leaf = sq.is_leaf,
            "decomposing L1 branch"
        );
        let child = build_subtree(
            sq,
            &config.content_type,
            config.folder_map.as_deref(),
            granularity,
            config.max_depth,
            2, // current depth (apex was 0, first decomposition was 1)
            llm_config,
            config.chains_dir.as_deref(),
            config.audience.as_deref(),
            &config.model_tier,
            config.temperature,
            config.max_tokens,
            config.sibling_review_max_tokens,
            tier2,
            audit,
        )
        .await?;
        let node_count = count_tree_nodes(&child);
        info!(
            branch = i + 1,
            question = %sq.question,
            nodes = node_count,
            "L1 branch complete"
        );
        children.push(child);
    }

    let apex_node = QuestionNode {
        id: String::new(),
        question: config.apex_question.clone(),
        about: "all top-level nodes at once".to_string(),
        creates: "apex".to_string(),
        prompt_hint: "Synthesize all sub-answers into a comprehensive answer to the apex question."
            .to_string(),
        children,
        is_leaf: false,
    };

    let mut tree = QuestionTree {
        apex: apex_node,
        content_type: config.content_type.clone(),
        config: config.clone(),
        audience: None, // Set by build_runner from characterization result
    };

    // Assign stable deterministic IDs to all question nodes
    assign_question_ids(&mut tree);

    Ok(tree)
}

// ── Incremental Decomposition ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FrontierQuestion {
    id: String,
    question: String,
    prompt_hint: String,
    tree_depth: u32,
}

#[derive(Debug, Clone)]
struct FrontierDecomposedQuestion {
    raw: RawDecomposedQuestion,
    parent_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct QuestionDagDraft {
    nodes: HashMap<String, QuestionNode>,
    depths: HashMap<String, u32>,
    parents_by_child: HashMap<String, Vec<String>>,
    children_by_parent: HashMap<String, Vec<String>>,
    apex_id: String,
    id_allocator: QuestionIdAllocator,
}

impl QuestionDagDraft {
    fn new(apex: QuestionNode) -> Self {
        let apex_id = apex.id.clone();
        let mut id_allocator = QuestionIdAllocator::default();
        id_allocator.register_existing(&apex.question, &apex.id);
        let mut nodes = HashMap::new();
        let mut depths = HashMap::new();
        depths.insert(apex_id.clone(), 0);
        nodes.insert(apex_id.clone(), apex);
        Self {
            nodes,
            depths,
            parents_by_child: HashMap::new(),
            children_by_parent: HashMap::new(),
            apex_id,
            id_allocator,
        }
    }

    fn upsert_node(&mut self, node: QuestionNode, tree_depth: u32) {
        self.id_allocator
            .register_existing(&node.question, &node.id);
        self.depths.entry(node.id.clone()).or_insert(tree_depth);
        self.nodes
            .entry(node.id.clone())
            .and_modify(|existing| {
                if existing.prompt_hint.trim().is_empty() && !node.prompt_hint.trim().is_empty() {
                    existing.prompt_hint = node.prompt_hint.clone();
                }
                existing.about = node.about.clone();
                existing.creates = node.creates.clone();
                existing.is_leaf = existing.is_leaf && node.is_leaf;
            })
            .or_insert(node);
    }

    fn get_or_allocate_id(
        &mut self,
        question: &str,
        tree_depth: u32,
        max_visual_layer: u32,
    ) -> String {
        let visual_layer = visual_layer_from_tree_depth(max_visual_layer, tree_depth);
        self.id_allocator.get_or_allocate(question, visual_layer)
    }

    fn add_edge(&mut self, parent_id: &str, child_id: &str) {
        push_unique(
            self.children_by_parent
                .entry(parent_id.to_string())
                .or_default(),
            child_id.to_string(),
        );
        push_unique(
            self.parents_by_child
                .entry(child_id.to_string())
                .or_default(),
            parent_id.to_string(),
        );
    }

    fn materialize_node(&self, node_id: &str, path: &mut HashSet<String>) -> Result<QuestionNode> {
        if !path.insert(node_id.to_string()) {
            return self
                .nodes
                .get(node_id)
                .cloned()
                .ok_or_else(|| anyhow!("question DAG references missing node {node_id}"));
        }
        let mut node = self
            .nodes
            .get(node_id)
            .cloned()
            .ok_or_else(|| anyhow!("question DAG references missing node {node_id}"))?;
        node.children = self
            .children_by_parent
            .get(node_id)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|child_id| self.materialize_node(child_id, path).ok())
            .collect();
        path.remove(node_id);
        Ok(node)
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn node_scope_for_tree_depth(tree_depth: u32, is_leaf: bool) -> (String, String) {
    if is_leaf {
        return ("each file individually".to_string(), "L0 nodes".to_string());
    }
    scope_for_depth(tree_depth + 1)
}

fn frontier_from_ids(dag: &QuestionDagDraft, ids: &[String]) -> Vec<FrontierQuestion> {
    ids.iter()
        .filter_map(|id| {
            let node = dag.nodes.get(id)?;
            Some(FrontierQuestion {
                id: id.clone(),
                question: node.question.clone(),
                prompt_hint: node.prompt_hint.clone(),
                tree_depth: *dag.depths.get(id).unwrap_or(&0),
            })
        })
        .collect()
}

fn question_node_from_row(row: &db::QuestionNodeRow) -> QuestionNode {
    QuestionNode {
        id: row.question_id.clone(),
        question: row.question.clone(),
        about: row.about.clone(),
        creates: row.creates.clone(),
        prompt_hint: row.prompt_hint.clone(),
        children: Vec::new(),
        is_leaf: row.is_leaf,
    }
}

fn question_dag_from_rows(
    rows: Vec<db::QuestionNodeRow>,
    edges: Vec<db::QuestionEdgeRow>,
) -> Result<QuestionDagDraft> {
    let apex_row = rows
        .iter()
        .min_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.parent_id.is_some().cmp(&b.parent_id.is_some()))
                .then_with(|| a.question_id.cmp(&b.question_id))
        })
        .ok_or_else(|| anyhow!("cannot reconstruct question DAG from empty rows"))?;

    let mut dag = QuestionDagDraft::new(question_node_from_row(apex_row));
    for row in &rows {
        dag.upsert_node(question_node_from_row(row), row.depth);
    }

    let node_ids = rows
        .iter()
        .map(|row| row.question_id.clone())
        .collect::<HashSet<_>>();
    let mut edge_count = 0usize;
    for edge in edges {
        if node_ids.contains(&edge.parent_question_id) && node_ids.contains(&edge.child_question_id)
        {
            dag.add_edge(&edge.parent_question_id, &edge.child_question_id);
            edge_count += 1;
        }
    }

    if edge_count == 0 {
        for row in &rows {
            for child_id in db::question_row_child_ids(row) {
                if node_ids.contains(&child_id) {
                    dag.add_edge(&row.question_id, &child_id);
                }
            }
        }
    }

    Ok(dag)
}

fn undecomposed_frontier_ids(dag: &QuestionDagDraft, max_depth: u32) -> Vec<String> {
    let mut ids = dag
        .nodes
        .iter()
        .filter_map(|(id, node)| {
            let depth = *dag.depths.get(id).unwrap_or(&0);
            let has_children = dag
                .children_by_parent
                .get(id)
                .map(|children| !children.is_empty())
                .unwrap_or(false);
            (!node.is_leaf && depth < max_depth && !has_children).then_some((depth, id.clone()))
        })
        .collect::<Vec<_>>();
    ids.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ids.into_iter().map(|(_, id)| id).collect()
}

/// Decompose an apex question incrementally, persisting each node to the DB
/// as it comes back from the LLM.
///
/// If a partial tree already exists in the DB (from a previous killed run),
/// it resumes from where it left off — only decomposing branch nodes that
/// don't have children yet.
///
/// Returns the fully assembled QuestionTree.
pub async fn decompose_question_incremental(
    config: &DecompositionConfig,
    llm_config: &LlmConfig,
    writer: Arc<Mutex<Connection>>,
    slug: &str,
    _tier1: &super::Tier1Config,
    tier2: &super::Tier2Config,
    audit: Option<&AuditContext>,
) -> Result<QuestionTree> {
    if config.apex_question.trim().is_empty() {
        return Err(anyhow!("apex_question cannot be empty"));
    }
    if config.max_depth == 0 {
        return Err(anyhow!("max_depth must be at least 1"));
    }

    let granularity = config.granularity.clamp(1, 5);
    let (min_subs, max_subs) = granularity_to_range(granularity, tier2);

    // Check for existing partial tree
    let existing_count = {
        let conn = writer.lock().await;
        db::count_question_nodes(&conn, slug)?
    };

    let (mut dag, mut frontier_ids) = if existing_count > 0 {
        let (rows, edges) = {
            let conn = writer.lock().await;
            let rows = db::load_question_nodes_as_tree(&conn, slug)?
                .ok_or_else(|| anyhow!("no nodes found despite count > 0"))?;
            let edges = db::load_question_edges(&conn, slug)?;
            (rows, edges)
        };
        let dag = question_dag_from_rows(rows, edges)?;
        let frontier_ids = undecomposed_frontier_ids(&dag, config.max_depth);
        if frontier_ids.is_empty() {
            info!(
                slug = slug,
                existing_nodes = existing_count,
                "question DAG already fully decomposed, reconstructing"
            );
            let apex = dag.materialize_node(&dag.apex_id, &mut HashSet::new())?;
            return Ok(QuestionTree {
                apex,
                content_type: config.content_type.clone(),
                config: config.clone(),
                audience: None,
            });
        }
        info!(
            slug = slug,
            existing_nodes = existing_count,
            frontier = frontier_ids.len(),
            "resuming decomposition from existing partial question DAG"
        );
        (dag, frontier_ids)
    } else {
        // Fresh decomposition — no existing nodes
        info!(
            apex = %config.apex_question,
            content_type = %config.content_type,
            granularity = granularity,
            max_depth = config.max_depth,
            slug = slug,
            "starting incremental question decomposition"
        );

        let apex_about = "all top-level nodes at once".to_string();
        let mut id_allocator = QuestionIdAllocator::default();
        let apex_id = id_allocator.get_or_allocate(&config.apex_question, config.max_depth as i64);
        let dag = QuestionDagDraft::new(QuestionNode {
            id: apex_id,
            question: config.apex_question.clone(),
            about: apex_about,
            creates: "apex".to_string(),
            prompt_hint:
                "Synthesize all sub-answers into a comprehensive answer to the apex question."
                    .to_string(),
            children: Vec::new(),
            is_leaf: false,
        });

        save_question_dag_to_db(&dag, slug, &writer, Some(llm_config), true).await?;

        let frontier_ids = vec![dag.apex_id.clone()];
        (dag, frontier_ids)
    };
    let mut layer_index = 0u32;

    while !frontier_ids.is_empty() {
        let frontier = frontier_from_ids(&dag, &frontier_ids)
            .into_iter()
            .filter(|parent| {
                dag.nodes
                    .get(&parent.id)
                    .map(|node| !node.is_leaf && parent.tree_depth < config.max_depth)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        if frontier.is_empty() {
            break;
        }

        let parent_depth = frontier[0].tree_depth;
        let decomposition_depth = parent_depth + 1;
        info!(
            slug = slug,
            layer = layer_index,
            parent_count = frontier.len(),
            parent_depth = parent_depth,
            "decomposing question frontier"
        );

        let layer_children =
            match call_frontier_decomposition_llm(
                &frontier,
                &config.content_type,
                config.folder_map.as_deref(),
                min_subs,
                max_subs,
                decomposition_depth,
                llm_config,
                config.chains_dir.as_deref(),
                config.audience.as_deref(),
                &config.model_tier,
                config.temperature,
                config.max_tokens,
                audit,
            )
            .await
            {
                Ok(children) => children,
                Err(err) => {
                    warn!(
                        slug = slug,
                        layer = layer_index,
                        error = %err,
                        "frontier decomposition failed; falling back to per-parent decomposition"
                    );
                    let mut children = Vec::new();
                    for parent in &frontier {
                        let per_parent = call_decomposition_llm(
                            &parent.question,
                            &config.content_type,
                            config.folder_map.as_deref(),
                            min_subs,
                            max_subs,
                            decomposition_depth,
                            llm_config,
                            config.chains_dir.as_deref(),
                            config.audience.as_deref(),
                            &config.model_tier,
                            config.temperature,
                            config.max_tokens,
                            audit,
                        )
                        .await?;
                        children.extend(per_parent.into_iter().map(|raw| {
                            FrontierDecomposedQuestion {
                                raw,
                                parent_ids: vec![parent.id.clone()],
                            }
                        }));
                    }
                    children
                }
            };

        let allowed_parent_ids: HashSet<String> =
            frontier.iter().map(|parent| parent.id.clone()).collect();
        let child_tree_depth = parent_depth + 1;
        let mut next_ids = Vec::new();
        let mut parents_with_children = HashSet::new();

        for child in layer_children {
            let parent_ids = child
                .parent_ids
                .into_iter()
                .filter(|parent_id| allowed_parent_ids.contains(parent_id))
                .collect::<Vec<_>>();
            if parent_ids.is_empty() {
                continue;
            }

            let forced_leaf = child.raw.is_leaf || child_tree_depth + 1 >= config.max_depth;
            let (about, creates) = node_scope_for_tree_depth(child_tree_depth, forced_leaf);
            let node_id =
                dag.get_or_allocate_id(&child.raw.question, child_tree_depth, config.max_depth);
            let node = QuestionNode {
                id: node_id.clone(),
                question: child.raw.question,
                about,
                creates,
                prompt_hint: child.raw.prompt_hint,
                children: Vec::new(),
                is_leaf: forced_leaf,
            };
            dag.upsert_node(node, child_tree_depth);
            for parent_id in parent_ids {
                dag.add_edge(&parent_id, &node_id);
                parents_with_children.insert(parent_id);
            }
            if !forced_leaf {
                push_unique(&mut next_ids, node_id);
            }
        }

        for parent in &frontier {
            if !parents_with_children.contains(&parent.id) {
                if let Some(node) = dag.nodes.get_mut(&parent.id) {
                    node.is_leaf = true;
                    node.about = "each file individually".to_string();
                    node.creates = "L0 nodes".to_string();
                }
            }
        }

        save_question_dag_to_db(&dag, slug, &writer, Some(llm_config), false).await?;
        frontier_ids = next_ids;
        layer_index += 1;
    }

    let apex = dag.materialize_node(&dag.apex_id, &mut HashSet::new())?;
    let tree = QuestionTree {
        apex,
        content_type: config.content_type.clone(),
        config: config.clone(),
        audience: None,
    };

    // Persist the final DAG and refresh the legacy parent_id/children_json
    // projection in the same pass.
    save_question_dag_to_db(&dag, slug, &writer, Some(llm_config), true).await?;

    let total_nodes = {
        let conn = writer.lock().await;
        db::count_question_nodes(&conn, slug)?
    };
    info!(
        slug = slug,
        total_nodes = total_nodes,
        "incremental decomposition complete — all nodes persisted"
    );

    Ok(tree)
}

/// Save all nodes in a tree to the DB in a single blocking transaction.
/// Collects all nodes in-memory first, then acquires the lock once and writes
/// them all inside a transaction — avoids per-node async lock round-trips that
/// caused backpressure hangs.
async fn save_question_dag_to_db(
    dag: &QuestionDagDraft,
    slug: &str,
    writer: &Arc<Mutex<Connection>>,
    llm_config: Option<&LlmConfig>,
    clear_existing: bool,
) -> Result<()> {
    let mut ids = dag.nodes.keys().cloned().collect::<Vec<_>>();
    ids.sort_by(|a, b| {
        dag.depths
            .get(a)
            .unwrap_or(&0)
            .cmp(dag.depths.get(b).unwrap_or(&0))
            .then_with(|| a.cmp(b))
    });

    let mut all_nodes = Vec::new();
    for id in ids {
        let Some(base_node) = dag.nodes.get(&id) else {
            continue;
        };
        let mut node = base_node.clone();
        node.children = dag
            .children_by_parent
            .get(&id)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|child_id| dag.nodes.get(child_id).cloned())
            .collect();
        let parent_id = dag
            .parents_by_child
            .get(&id)
            .and_then(|parents| parents.first())
            .cloned();
        let depth = *dag.depths.get(&id).unwrap_or(&0);
        all_nodes.push((node, parent_id, depth));
    }

    let children_by_parent = dag.children_by_parent.clone();
    let node_count = all_nodes.len();
    info!(
        nodes = node_count,
        slug = slug,
        "save_question_dag_to_db: batching DAG nodes and canonical edges"
    );

    let conn = writer.clone();
    let slug_owned = slug.to_string();
    let produced_events = tokio::task::spawn_blocking(move || {
        let c = conn.blocking_lock();
        c.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<(String, String, i64)>> {
            let mut produced_events = Vec::new();
            for (ref n, _, d) in &all_nodes {
                let existed = match c.query_row(
                    "SELECT 1 FROM pyramid_question_nodes WHERE slug = ?1 AND question_id = ?2",
                    rusqlite::params![slug_owned, n.id],
                    |_| Ok(true),
                ) {
                    Ok(existed) => existed,
                    Err(rusqlite::Error::QueryReturnedNoRows) => false,
                    Err(e) => return Err(e.into()),
                };
                if !existed {
                    produced_events.push((n.id.clone(), n.question.clone(), *d as i64));
                }
            }
            if clear_existing {
                db::clear_question_nodes(&c, &slug_owned, None)?;
                db::clear_question_edges(&c, &slug_owned, None)?;
            }
            for (ref n, ref pid, d) in &all_nodes {
                db::save_question_node(&c, &slug_owned, n, pid.as_deref(), *d)?;
            }
            for (ref n, _, _) in &all_nodes {
                let child_ids = children_by_parent.get(&n.id).cloned().unwrap_or_default();
                db::save_question_edges_for_parent(&c, &slug_owned, None, &n.id, &child_ids)?;
            }
            Ok(produced_events)
        })();
        match result {
            Ok(produced_events) => {
                c.execute_batch("COMMIT")?;
                Ok(produced_events)
            }
            Err(e) => {
                let _ = c.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| anyhow!("save_question_dag_to_db panicked: {e}"))??;

    emit_question_node_events(slug, llm_config, produced_events);

    Ok(())
}

fn emit_question_node_events(
    slug: &str,
    llm_config: Option<&LlmConfig>,
    nodes: Vec<(String, String, i64)>,
) {
    let Some(cache) = llm_config.and_then(|cfg| cfg.cache_access.as_ref()) else {
        return;
    };
    let Some(bus) = cache.bus.as_ref() else {
        return;
    };

    let build_prefix = format!("decompose-{slug}-");
    let step_name = cache
        .build_id
        .strip_prefix(&build_prefix)
        .filter(|s| !s.is_empty())
        .unwrap_or("question_decompose")
        .to_string();

    for (node_id, headline, depth) in nodes {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: slug.to_string(),
            kind: TaggedKind::NodeProduced {
                slug: slug.to_string(),
                build_id: cache.build_id.clone(),
                step_name: step_name.clone(),
                node_id,
                headline,
                depth,
            },
        });
    }
}

/// Recursively build a subtree for a decomposed question.
///
/// If the question is marked as a leaf (or we've hit max_depth), returns a leaf node.
/// Otherwise, decomposes further.
async fn build_subtree(
    raw: &RawDecomposedQuestion,
    content_type: &str,
    folder_map: Option<&str>,
    granularity: u32,
    max_depth: u32,
    current_depth: u32,
    llm_config: &LlmConfig,
    chains_dir: Option<&std::path::Path>,
    audience: Option<&str>,
    model_tier: &str,
    temperature: f32,
    max_tokens: usize,
    sibling_review_max_tokens: usize,
    tier2: &super::Tier2Config,
    audit: Option<&AuditContext>,
) -> Result<QuestionNode> {
    // Terminal conditions: marked as leaf, or depth exceeded
    if raw.is_leaf || current_depth >= max_depth {
        return Ok(QuestionNode {
            id: String::new(),
            question: raw.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: raw.prompt_hint.clone(),
            children: vec![],
            is_leaf: true,
        });
    }

    // Only decompose further if granularity warrants it
    if granularity <= 2 && current_depth >= 2 {
        return Ok(QuestionNode {
            id: String::new(),
            question: raw.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: raw.prompt_hint.clone(),
            children: vec![],
            is_leaf: true,
        });
    }

    let (min_subs, max_subs) = granularity_to_range(granularity, tier2);

    info!(
        depth = current_depth,
        question = %raw.question,
        "decomposing sub-question"
    );
    let sub_questions = call_decomposition_llm(
        &raw.question,
        content_type,
        folder_map,
        min_subs,
        max_subs,
        current_depth,
        llm_config,
        chains_dir,
        audience,
        model_tier,
        temperature,
        max_tokens,
        audit,
    )
    .await?;
    info!(
        depth = current_depth,
        question = %raw.question,
        sub_count = sub_questions.len(),
        "sub-questions returned"
    );

    if sub_questions.is_empty() {
        // If the LLM returned no sub-questions, treat as leaf
        return Ok(QuestionNode {
            id: String::new(),
            question: raw.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: raw.prompt_hint.clone(),
            children: vec![],
            is_leaf: true,
        });
    }

    let mut children = Vec::new();
    for sq in sub_questions {
        let child = Box::pin(build_subtree(
            &sq,
            content_type,
            folder_map,
            granularity,
            max_depth,
            current_depth + 1,
            llm_config,
            chains_dir,
            audience,
            model_tier,
            temperature,
            max_tokens,
            sibling_review_max_tokens,
            tier2,
            audit,
        ))
        .await?;
        children.push(child);
    }

    // Horizontal review: deduplicate siblings at every depth, not just depth 1
    if children.len() > 1 {
        let (merged, leafed) = horizontal_review_siblings(
            &mut children,
            llm_config,
            chains_dir,
            model_tier,
            sibling_review_max_tokens,
            audit,
        )
        .await?;
        if merged > 0 || leafed > 0 {
            info!(
                depth = current_depth,
                merged,
                marked_as_leaf = leafed,
                remaining = children.len(),
                "horizontal review at depth {}",
                current_depth
            );
        }
    }

    // Non-leaf: this node synthesizes its children
    let (about, creates) = scope_for_depth(current_depth);

    Ok(QuestionNode {
        id: String::new(),
        question: raw.question.clone(),
        about,
        creates,
        prompt_hint: raw.prompt_hint.clone(),
        children,
        is_leaf: false,
    })
}

// ── LLM call for decomposition ────────────────────────────────────────────────

/// Raw output from the decomposition LLM — before tree assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawDecomposedQuestion {
    question: String,
    prompt_hint: String,
    is_leaf: bool,
}

/// Call the LLM to decompose a question into sub-questions.
///
/// Uses the chain-resolved model tier because decomposition routing is YAML-driven.
/// 11-G: Loads system prompt from chains/prompts/question/decompose.md when chains_dir is set.
/// 11-Q: Uses {{variable}} template substitution for content_type and depth.
async fn call_decomposition_llm(
    parent_question: &str,
    content_type: &str,
    folder_map: Option<&str>,
    min_subs: u32,
    max_subs: u32,
    depth: u32,
    llm_config: &LlmConfig,
    chains_dir: Option<&std::path::Path>,
    audience: Option<&str>,
    model_tier: &str,
    temperature: f32,
    max_tokens: usize,
    audit: Option<&AuditContext>,
) -> Result<Vec<RawDecomposedQuestion>> {
    let folder_context = folder_map.unwrap_or("(no folder map provided)");

    let depth_str = depth.to_string();
    let audience_str = audience.unwrap_or("");
    let audience_block = if audience_str.is_empty() {
        String::new()
    } else {
        format!("AUDIENCE: The person asking this question is {audience_str}. Decompose into sub-questions they would naturally ask, using their vocabulary and perspective.")
    };
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/decompose.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("content_type", content_type),
                ("depth", &depth_str),
                ("min_subs", &min_subs.to_string()),
                ("max_subs", &max_subs.to_string()),
                ("audience_block", &audience_block),
            ],
        ),
        None => {
            warn!("decompose.md not found — using inline fallback");
            format!(
                r#"You are decomposing a question into sub-questions to build a knowledge pyramid.

WHAT YOU ARE DOING:
You are helping build a layered understanding of a topic. The source material is "{content_type}" content. The original question will be answered by synthesizing answers to your sub-questions. Your sub-questions will either be answered directly from source material (leaves) or further decomposed by another instance of you (branches).

HOW TO THINK ABOUT IT:
- Ask yourself: "What are the genuinely distinct facets of this question that require separate investigation?"
- Each sub-question should cover territory that NO other sibling covers
- If a question can be answered by reading source files directly, it is a leaf — do not decompose further
- If a question requires combining insights from multiple sources, it is a branch
- Prefer FEWER, more focused questions over many overlapping ones
- It is completely fine to produce just 1 or 2 sub-questions if that is what the question needs
- It is also fine to say this question is already specific enough and return zero sub-questions (empty array)
- The goal is the MINIMUM decomposition needed to fully answer the parent question — no more

WHAT TO AVOID:
- Do NOT pad with extra questions just to fill a quota — there is no quota
- Do NOT create questions that overlap significantly with each other
- Do NOT create questions that rephrase the parent in slightly different words
- Do NOT decompose a question that is already answerable from source material

You are at decomposition depth {depth}. Deeper depth means the questions should be MORE specific and MORE likely to be leaves.

{audience_hint}

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (what to focus on when answering),
  "is_leaf": boolean

Return ONLY the JSON array. An empty array [] is valid if the parent question needs no decomposition."#,
                audience_hint = if audience_str.is_empty() {
                    String::new()
                } else {
                    format!("The person asking this question is {audience_str}. Decompose into sub-questions they would naturally ask, using their vocabulary and perspective.")
                },
            )
        }
    };

    let user_prompt = format!(
        r#"Parent question: "{parent_question}"

Source material:
{folder_context}

What are the genuinely distinct sub-questions needed to answer this? Only create sub-questions that cover unique territory."#,
    );

    // Model selection is controlled by YAML chain definitions, not Rust overrides.
    // See Inviolable #4: "YAML is the single source of truth for model selection."

    // Try up to 2 times on parse failure
    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        let step_name = effective_decompose_step_name(audit, "question_decompose_layer");
        let cache_ctx = make_step_ctx_from_llm_config(
            llm_config,
            step_name,
            "question_decompose",
            depth as i64,
            None,
            &system_prompt,
            model_tier,
            None,
            None,
        )
        .await;
        let audit_ctx = audit_for(audit, step_name, Some(depth as i64));
        let response = llm::call_model_unified_with_audit_and_ctx(
            llm_config,
            cache_ctx.as_ref(),
            audit_ctx.as_ref(),
            &system_prompt,
            &user_prompt,
            temp,
            max_tokens,
            None,
            LlmCallOptions::default(),
        )
        .await?;

        info!(
            depth = depth,
            attempt = attempt,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "decomposition LLM call complete"
        );

        match parse_decomposition_response(&response.content) {
            Ok(questions) => {
                if questions.is_empty() {
                    warn!(
                        depth = depth,
                        "decomposition returned empty array, retrying"
                    );
                    continue;
                }
                return Ok(questions);
            }
            Err(e) => {
                warn!(
                    depth = depth,
                    attempt = attempt,
                    error = %e,
                    "failed to parse decomposition response, retrying"
                );
                continue;
            }
        }
    }

    Err(anyhow!(
        "failed to get valid decomposition after retries for question: {}",
        parent_question
    ))
}

async fn call_frontier_decomposition_llm(
    frontier: &[FrontierQuestion],
    content_type: &str,
    folder_map: Option<&str>,
    min_subs: u32,
    max_subs: u32,
    depth: u32,
    llm_config: &LlmConfig,
    chains_dir: Option<&std::path::Path>,
    audience: Option<&str>,
    model_tier: &str,
    temperature: f32,
    max_tokens: usize,
    audit: Option<&AuditContext>,
) -> Result<Vec<FrontierDecomposedQuestion>> {
    if frontier.is_empty() {
        return Ok(Vec::new());
    }

    let folder_context = folder_map.unwrap_or("(no folder map provided)");
    let depth_str = depth.to_string();
    let audience_str = audience.unwrap_or("");
    let audience_block = if audience_str.is_empty() {
        String::new()
    } else {
        format!("AUDIENCE: The person asking this question is {audience_str}. Use their vocabulary and perspective.")
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/decompose_frontier.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("content_type", content_type),
                ("depth", &depth_str),
                ("min_subs", &min_subs.to_string()),
                ("max_subs", &max_subs.to_string()),
                ("audience_block", &audience_block),
            ],
        ),
        None => {
            warn!("decompose_frontier.md not found — using inline fallback");
            format!(
                r#"You are building a question DAG for a knowledge pyramid.

You will receive a whole frontier layer of parent questions. Produce the canonical next layer of child questions for the entire frontier at once.

Rules:
- A child question must be listed once even if it helps answer multiple parents.
- Use parent_ids to attach that canonical child to every parent it supports.
- Keep siblings across the whole layer non-overlapping.
- Prefer the minimum set of focused questions needed to answer the parent frontier.
- If a child can be answered directly from source material, set is_leaf true.
- If it needs another synthesis layer, set is_leaf false.
- Suggested breadth is {min_subs}-{max_subs} children per parent, but do not pad.

You are at decomposition depth {depth} for "{content_type}" source material.
{audience_block}

Respond with JSON only:
[
  {{
    "question": "canonical child question",
    "prompt_hint": "what answering should focus on",
    "is_leaf": true,
    "parent_ids": ["parent question id", "..."]
  }}
]"#
            )
        }
    };

    let parent_lines = frontier
        .iter()
        .enumerate()
        .map(|(idx, parent)| {
            format!(
                "[{idx}] id={} depth={} question=\"{}\" hint=\"{}\"",
                parent.id, parent.tree_depth, parent.question, parent.prompt_hint
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let user_prompt = format!(
        r#"Parent frontier:
{parent_lines}

Source material:
{folder_context}

Create the next canonical child-question layer for this whole frontier. When one child question supports multiple parents, include all of those parent_ids on the one child object."#
    );

    let allowed_parent_ids: HashSet<String> = frontier.iter().map(|p| p.id.clone()).collect();
    let parent_index_to_id: HashMap<usize, String> = frontier
        .iter()
        .enumerate()
        .map(|(idx, p)| (idx, p.id.clone()))
        .collect();

    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };
        let step_name = effective_decompose_step_name(audit, "question_decompose_frontier");
        let cache_ctx = make_step_ctx_from_llm_config(
            llm_config,
            step_name,
            "question_decompose",
            depth as i64,
            None,
            &system_prompt,
            model_tier,
            None,
            None,
        )
        .await;
        let audit_ctx = audit_for(audit, step_name, Some(depth as i64));
        let response = llm::call_model_unified_with_audit_and_ctx(
            llm_config,
            cache_ctx.as_ref(),
            audit_ctx.as_ref(),
            &system_prompt,
            &user_prompt,
            temp,
            max_tokens,
            None,
            LlmCallOptions::default(),
        )
        .await?;

        info!(
            depth = depth,
            parents = frontier.len(),
            attempt = attempt,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "frontier decomposition LLM call complete"
        );

        match parse_frontier_decomposition_response(
            &response.content,
            &allowed_parent_ids,
            &parent_index_to_id,
        ) {
            Ok(children) if !children.is_empty() => return Ok(children),
            Ok(_) => {
                warn!(
                    depth = depth,
                    attempt = attempt,
                    "frontier decomposition response had no valid children, retrying"
                );
            }
            Err(e) => {
                warn!(
                    depth = depth,
                    attempt = attempt,
                    error = %e,
                    "failed to parse frontier decomposition response, retrying"
                );
            }
        }
    }

    Err(anyhow!(
        "failed to get valid frontier decomposition after retries at depth {}",
        depth
    ))
}

/// Parse the LLM response into decomposed questions.
fn parse_decomposition_response(content: &str) -> Result<Vec<RawDecomposedQuestion>> {
    // Try to parse as JSON directly
    let trimmed = content.trim();

    // Handle markdown code fences
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner.strip_suffix("```").unwrap_or(inner).trim()
    } else {
        trimmed
    };

    // Try to find JSON array in the response
    let json_str = if json_str.starts_with('[') {
        json_str
    } else if let Some(start) = json_str.find('[') {
        if let Some(end) = json_str.rfind(']') {
            &json_str[start..=end]
        } else {
            json_str
        }
    } else {
        json_str
    };

    let parsed: Vec<Value> = serde_json::from_str(json_str)
        .map_err(|e| anyhow!("failed to parse decomposition JSON: {}", e))?;

    let mut questions = Vec::new();
    for item in parsed {
        let question = item
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing 'question' field in decomposition output"))?
            .to_string();

        let prompt_hint = item
            .get("prompt_hint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let is_leaf = item.get("is_leaf").and_then(Value::as_bool).unwrap_or(true); // Default to leaf if not specified

        questions.push(RawDecomposedQuestion {
            question,
            prompt_hint,
            is_leaf,
        });
    }

    Ok(questions)
}

fn parse_frontier_decomposition_response(
    content: &str,
    allowed_parent_ids: &HashSet<String>,
    parent_index_to_id: &HashMap<usize, String>,
) -> Result<Vec<FrontierDecomposedQuestion>> {
    let json_str = extract_json_payload(content);
    let parsed: Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow!("failed to parse frontier decomposition JSON: {}", e))?;
    let items = if let Some(arr) = parsed.as_array() {
        arr.clone()
    } else if let Some(arr) = parsed.get("children").and_then(Value::as_array) {
        arr.clone()
    } else if let Some(arr) = parsed.get("questions").and_then(Value::as_array) {
        arr.clone()
    } else {
        return Err(anyhow!(
            "frontier decomposition output must be an array or object.children"
        ));
    };

    let mut out = Vec::new();
    for item in items {
        let question = item
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing 'question' field in frontier decomposition output"))?
            .to_string();
        let prompt_hint = item
            .get("prompt_hint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let is_leaf = item.get("is_leaf").and_then(Value::as_bool).unwrap_or(true);
        let mut parent_ids = Vec::new();

        collect_parent_refs_from_value(
            item.get("parent_id"),
            allowed_parent_ids,
            parent_index_to_id,
            &mut parent_ids,
        );
        collect_parent_refs_from_value(
            item.get("parent_ids"),
            allowed_parent_ids,
            parent_index_to_id,
            &mut parent_ids,
        );
        collect_parent_refs_from_value(
            item.get("parents"),
            allowed_parent_ids,
            parent_index_to_id,
            &mut parent_ids,
        );
        collect_parent_refs_from_value(
            item.get("parent_indices"),
            allowed_parent_ids,
            parent_index_to_id,
            &mut parent_ids,
        );

        if parent_ids.is_empty() && allowed_parent_ids.len() == 1 {
            if let Some(parent_id) = allowed_parent_ids.iter().next() {
                parent_ids.push(parent_id.clone());
            }
        }

        if parent_ids.is_empty() {
            return Err(anyhow!(
                "frontier child '{}' did not reference any valid parent_ids",
                question
            ));
        }

        out.push(FrontierDecomposedQuestion {
            raw: RawDecomposedQuestion {
                question,
                prompt_hint,
                is_leaf,
            },
            parent_ids,
        });
    }

    Ok(out)
}

fn collect_parent_refs_from_value(
    value: Option<&Value>,
    allowed_parent_ids: &HashSet<String>,
    parent_index_to_id: &HashMap<usize, String>,
    out: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::Array(arr) => {
            for item in arr {
                collect_parent_refs_from_value(
                    Some(item),
                    allowed_parent_ids,
                    parent_index_to_id,
                    out,
                );
            }
        }
        Value::String(s) => {
            if allowed_parent_ids.contains(s) {
                push_unique(out, s.clone());
            } else if let Some(stripped) = s.strip_prefix('P') {
                if let Ok(idx) = stripped.parse::<usize>() {
                    if let Some(parent_id) = parent_index_to_id.get(&idx) {
                        push_unique(out, parent_id.clone());
                    }
                }
            } else if let Ok(idx) = s.parse::<usize>() {
                if let Some(parent_id) = parent_index_to_id.get(&idx) {
                    push_unique(out, parent_id.clone());
                }
            }
        }
        Value::Number(n) => {
            if let Some(idx) = n.as_u64().and_then(|n| usize::try_from(n).ok()) {
                if let Some(parent_id) = parent_index_to_id.get(&idx) {
                    push_unique(out, parent_id.clone());
                }
            }
        }
        _ => {}
    }
}

fn extract_json_payload(content: &str) -> &str {
    let trimmed = content.trim();
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner.strip_suffix("```").unwrap_or(inner).trim()
    } else {
        trimmed
    };

    if json_str.starts_with('[') || json_str.starts_with('{') {
        return json_str;
    }
    let array_start = json_str.find('[');
    let object_start = json_str.find('{');
    match (array_start, object_start) {
        (Some(a), Some(o)) if a < o => json_str
            .rfind(']')
            .map(|end| &json_str[a..=end])
            .unwrap_or(json_str),
        (Some(a), None) => json_str
            .rfind(']')
            .map(|end| &json_str[a..=end])
            .unwrap_or(json_str),
        (_, Some(o)) => json_str
            .rfind('}')
            .map(|end| &json_str[o..=end])
            .unwrap_or(json_str),
        _ => json_str,
    }
}

// ── Tree → QuestionSet conversion ─────────────────────────────────────────────

/// Convert a QuestionTree into a QuestionSet that the existing Question Compiler
/// (P2.1) can process.
///
/// This bridges P2.2 output to P2.1 input. The resulting QuestionSet has the
/// same shape as a hand-authored question YAML, so compile_question_set works
/// unchanged.
pub fn question_tree_to_question_set(
    tree: &QuestionTree,
    content_type: &str,
    chains_dir: &Path,
) -> Result<QuestionSet> {
    let mut questions = Vec::new();

    // Collect all leaf questions first — these become L0 extraction
    // Then collect synthesis questions layer by layer

    // Phase 1: L0 extraction (all leaves)
    let leaves = collect_leaves(&tree.apex);
    if leaves.is_empty() {
        return Err(anyhow!("question tree has no leaf nodes"));
    }

    // Create a combined L0 extraction question from all leaf questions.
    // The prompt_hints from leaves become the extraction targets.
    let extraction_hints: Vec<String> = leaves
        .iter()
        .map(|l| format!("- {}: {}", l.question, l.prompt_hint))
        .collect();

    let extraction_prompt = format!(
        "Extract information from this source file relevant to the following questions:\n{}",
        extraction_hints.join("\n")
    );
    let extraction_targets = format!(
        "Focus the extraction on details that help answer these sub-questions:\n{}",
        extraction_hints.join("\n")
    );
    let combined_prompt = match load_decomposition_prompt(chains_dir, content_type, "extract") {
        Some(base_prompt) => append_prompt_section(
            &base_prompt,
            "Additional extraction targets from question decomposition",
            &extraction_targets,
        ),
        None => extraction_prompt,
    };
    let extract_variants = build_extract_variants(chains_dir, content_type, &extraction_targets);

    questions.push(Question {
        ask: format!("Extract information relevant to: {}", tree.apex.question),
        about: "each file individually".to_string(),
        creates: "L0 nodes".to_string(),
        prompt: combined_prompt,
        cluster_prompt: None,
        model: None,
        cluster_model: None,
        temperature: None,
        parallel: Some(8),
        retry: Some(2),
        optional: None,
        variants: extract_variants,
        constraints: None,
        context: None,
        sequential_context: None,
        preview_lines: None,
    });

    // Phase 2: Clustering (assign L0 nodes to threads based on sub-question structure)
    // Use the tree's L1 children as thread guidance
    let l1_children = &tree.apex.children;
    let thread_hint = l1_children
        .iter()
        .map(|c| c.question.as_str())
        .collect::<Vec<_>>()
        .join("; ");

    let classify_prompt = match load_decomposition_prompt(chains_dir, content_type, "cluster") {
        Some(base_prompt) => append_prompt_section(
            &base_prompt,
            "Question decomposition guidance",
            &format!(
                "Prefer threads that help answer these high-level aspects:\n- {}\n\n\
                 Aim for approximately {min}-{max} threads, unless the evidence clearly supports fewer or more.",
                l1_children
                    .iter()
                    .map(|c| c.question.as_str())
                    .collect::<Vec<_>>()
                    .join("\n- "),
                min = l1_children.len().max(3),
                max = l1_children.len().max(5) + 3,
            ),
        ),
        None => format!(
            "Classify L0 nodes into {min}-{max} threads based on these aspects: {thread_hint}\n\
             Each thread should correspond to one of the identified sub-questions.",
            min = l1_children.len().max(3),
            max = l1_children.len().max(5) + 3,
        ),
    };

    questions.push(Question {
        ask: format!(
            "How should the extracted information be grouped to answer: {}",
            tree.apex.question
        ),
        about: "all L0 topics at once".to_string(),
        creates: "L1 thread assignments".to_string(),
        prompt: classify_prompt,
        cluster_prompt: None,
        model: None,
        cluster_model: None,
        temperature: Some(0.3),
        parallel: None,
        retry: Some(2),
        optional: None,
        variants: None,
        constraints: None,
        context: None,
        sequential_context: None,
        preview_lines: None,
    });

    // Phase 3: L1 synthesis (one per thread, answering sub-questions)
    let l1_prompt = match load_decomposition_prompt(chains_dir, content_type, "thread") {
        Some(base_prompt) => append_prompt_section(
            &base_prompt,
            "Question-guided synthesis",
            "Answer the sub-question for this thread based on the assigned L0 evidence. \
             Favor details that directly help answer the decomposed question tree.",
        ),
        None => {
            "Synthesize the assigned L0 nodes into a comprehensive answer for this thread's question.".to_string()
        }
    };

    questions.push(Question {
        ask: "Synthesize each thread's L0 evidence into a sub-answer.".to_string(),
        about: "each L1 thread's assigned L0 nodes".to_string(),
        creates: "L1 nodes".to_string(),
        prompt: l1_prompt,
        cluster_prompt: None,
        model: None,
        cluster_model: None,
        temperature: None,
        parallel: Some(4),
        retry: Some(2),
        optional: None,
        variants: None,
        constraints: None,
        context: None,
        sequential_context: None,
        preview_lines: None,
    });

    // Phase 4: L1 webbing
    questions.push(Question {
        ask: "What connections exist between the thread-level answers?".to_string(),
        about: "all L1 nodes at once".to_string(),
        creates: "web edges between L1 nodes".to_string(),
        prompt: load_decomposition_prompt(chains_dir, content_type, "web").unwrap_or_else(|| {
            "Identify shared resources, dependencies, and connections between L1 threads."
                .to_string()
        }),
        cluster_prompt: None,
        model: None,
        cluster_model: None,
        temperature: None,
        parallel: None,
        retry: Some(2),
        optional: Some(true),
        variants: None,
        constraints: None,
        context: None,
        sequential_context: None,
        preview_lines: None,
    });

    // Phase 5: L2 synthesis (convergence — if enough L1 nodes)
    // Only add L2 if the tree has enough breadth
    let needs_l2 = l1_children.len() > 4;
    if needs_l2 {
        let l2_cluster_prompt = load_decomposition_prompt(chains_dir, content_type, "recluster")
            .map(|base_prompt| {
                append_prompt_section(
                    &base_prompt,
                    "Question decomposition guidance",
                    &format!(
                        "Organize the L1 syntheses into higher-level groups that clarify the apex question:\n{}",
                        tree.apex.question
                    ),
                )
            })
            .unwrap_or_else(|| {
                "Cluster the L1 thread syntheses into higher-level groups.".to_string()
            });

        let l2_reduce_prompt = load_decomposition_prompt(chains_dir, content_type, "distill")
            .map(|base_prompt| {
                append_prompt_section(
                    &base_prompt,
                    "Higher-level synthesis guidance",
                    &format!(
                        "Synthesize each L1 cluster into a substantive higher-level node that helps answer:\n{}",
                        tree.apex.question
                    ),
                )
            })
            .unwrap_or_else(|| {
                "Synthesize each L1 cluster into a higher-level node.".to_string()
            });

        questions.push(Question {
            ask: format!(
                "Group and synthesize the thread answers toward: {}",
                tree.apex.question
            ),
            about: "all L1 nodes at once".to_string(),
            creates: "L2 nodes".to_string(),
            prompt: l2_reduce_prompt,
            cluster_prompt: Some(l2_cluster_prompt),
            model: None,
            cluster_model: None, // resolved from tier routing at dispatch time
            temperature: None,
            parallel: None,
            retry: Some(2),
            optional: None,
            variants: None,
            constraints: None,
            context: None,
            sequential_context: None,
            preview_lines: None,
        });
    }

    // Phase 6: Apex synthesis
    let apex_scope = if needs_l2 {
        "all top-level nodes at once"
    } else {
        "all L1 nodes at once"
    };

    questions.push(Question {
        ask: tree.apex.question.clone(),
        about: apex_scope.to_string(),
        creates: "apex".to_string(),
        prompt: load_decomposition_prompt(chains_dir, content_type, "distill")
            .map(|base_prompt| {
                append_prompt_section(
                    &base_prompt,
                    "Apex question",
                    &format!(
                        "Synthesize all available evidence into a direct answer to:\n{}",
                        tree.apex.question
                    ),
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "Answer the apex question comprehensively by synthesizing all sub-answers: {}",
                    tree.apex.question
                )
            }),
        cluster_prompt: None,
        model: None,
        cluster_model: None,
        temperature: None,
        parallel: None,
        retry: Some(2),
        optional: None,
        variants: None,
        constraints: None,
        context: None,
        sequential_context: None,
        preview_lines: None,
    });

    Ok(QuestionSet {
        r#type: content_type.to_string(),
        version: "3.0".to_string(),
        defaults: QuestionDefaults {
            model: None,
            temperature: Some(0.3),
            retry: Some(2),
        },
        questions,
    })
}

fn append_prompt_section(prompt: &str, heading: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return prompt.to_string();
    }

    let section = format!("## {heading}\n{body}");
    if prompt.trim().is_empty() {
        return section;
    }

    if let Some(idx) = prompt.rfind("/no_think") {
        let (before, after) = prompt.split_at(idx);
        return format!(
            "{}\n\n{}\n\n{}",
            before.trim_end(),
            section,
            after.trim_start()
        );
    }

    format!("{}\n\n{}", prompt.trim_end(), section)
}

fn build_extract_variants(
    chains_dir: &Path,
    content_type: &str,
    extraction_targets: &str,
) -> Option<HashMap<String, String>> {
    if content_type != "code" {
        return None;
    }

    let mut variants = HashMap::new();

    if let Some(prompt) = load_prompt_candidates(chains_dir, &["prompts/code/config_extract.md"]) {
        variants.insert(
            "config files".to_string(),
            append_prompt_section(
                &prompt,
                "Additional extraction targets from question decomposition",
                extraction_targets,
            ),
        );
    }

    if let Some(prompt) = load_prompt_candidates(
        chains_dir,
        &[
            "prompts/code/code_extract_frontend.md",
            "prompts/code/frontend_extract.md",
        ],
    ) {
        variants.insert(
            "frontend (.tsx, .jsx)".to_string(),
            append_prompt_section(
                &prompt,
                "Additional extraction targets from question decomposition",
                extraction_targets,
            ),
        );
    }

    (!variants.is_empty()).then_some(variants)
}

fn load_decomposition_prompt(chains_dir: &Path, content_type: &str, kind: &str) -> Option<String> {
    let candidates: &[&str] = match (content_type, kind) {
        ("code", "extract") => &["prompts/code/code_extract.md", "prompts/code/extract.md"],
        ("code", "cluster") => &["prompts/code/code_cluster.md", "prompts/code/cluster.md"],
        ("code", "thread") => &[
            "prompts/code/code_thread.md",
            "prompts/code/thread.md",
            "prompts/code/thread_synthesis.md",
        ],
        ("code", "web") => &["prompts/code/code_web.md", "prompts/code/web.md"],
        ("code", "recluster") => &[
            "prompts/code/code_recluster.md",
            "prompts/code/recluster.md",
        ],
        ("code", "distill") => &["prompts/code/code_distill.md", "prompts/code/distill.md"],
        ("document", "extract") => &["prompts/document/doc_extract.md", "prompts/doc/extract.md"],
        ("document", "cluster") => &["prompts/document/doc_cluster.md", "prompts/doc/cluster.md"],
        ("document", "thread") => &[
            "prompts/document/doc_thread.md",
            "prompts/doc/thread.md",
            "prompts/doc/thread_synthesis.md",
        ],
        ("document", "web") => &["prompts/document/doc_web.md", "prompts/doc/web.md"],
        ("document", "recluster") => &[
            "prompts/document/doc_recluster.md",
            "prompts/doc/recluster.md",
        ],
        ("document", "distill") => &["prompts/document/doc_distill.md", "prompts/doc/distill.md"],
        ("conversation", "cluster") => &[
            "prompts/conversation/conv_cluster.md",
            "prompts/conversation/cluster.md",
        ],
        ("conversation", "thread") => &[
            "prompts/conversation/conv_thread.md",
            "prompts/conversation/thread.md",
        ],
        ("conversation", "web") => &[
            "prompts/conversation/conv_web.md",
            "prompts/conversation/web.md",
        ],
        ("conversation", "recluster") => &[
            "prompts/conversation/conv_recluster.md",
            "prompts/conversation/recluster.md",
        ],
        ("conversation", "distill") => &[
            "prompts/conversation/conv_distill.md",
            "prompts/conversation/distill.md",
        ],
        _ => &[],
    };

    load_prompt_candidates(chains_dir, candidates)
}

fn load_prompt_candidates(chains_dir: &Path, candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        let rel = candidate.strip_prefix("prompts/").unwrap_or(candidate);
        let path = chains_dir.join("prompts").join(rel);
        std::fs::read_to_string(path).ok()
    })
}

/// Collect all leaf nodes from a question tree.
fn collect_leaves(node: &QuestionNode) -> Vec<&QuestionNode> {
    let mut seen = HashSet::new();
    collect_leaves_deduped(node, &mut seen)
}

fn collect_leaves_deduped<'a>(
    node: &'a QuestionNode,
    seen: &mut HashSet<String>,
) -> Vec<&'a QuestionNode> {
    if node.is_leaf {
        return if seen.insert(node.id.clone()) {
            vec![node]
        } else {
            Vec::new()
        };
    }
    let mut leaves = Vec::new();
    for child in &node.children {
        leaves.extend(collect_leaves_deduped(child, seen));
    }
    leaves
}

// ── Preview ───────────────────────────────────────────────────────────────────

/// Generate a preview of what the decomposition will produce.
///
/// Returns estimated node counts, LLM calls, and a human-readable tree summary.
/// This is the "cost/time preview" — shown to the user before building.
pub fn preview_decomposition(tree: &QuestionTree) -> DecompositionPreview {
    let (total, leaves) = count_nodes(&tree.apex);
    let depth = tree_depth(&tree.apex);

    // Decomposition LLM calls: one per non-leaf level
    let decomposition_calls = count_non_leaf_levels(&tree.apex);

    // Build LLM calls estimate:
    // - L0 extraction: leaf_count * estimated_file_count (unknown, estimate 50)
    // - Clustering: 1
    // - L1 synthesis: number of L1 children
    // - L1 webbing: 1
    // - L2 synthesis (if needed): convergence rounds (~3)
    // - Apex: 1
    let estimated_file_count = 50u32; // conservative default
    let l1_count = tree.apex.children.len() as u32;
    let needs_l2 = l1_count > 4;
    let build_calls = estimated_file_count  // L0 extraction
        + 1                                  // clustering
        + l1_count                           // L1 synthesis
        + 1                                  // L1 webbing
        + if needs_l2 { 3 } else { 0 }      // L2 convergence
        + 1; // apex

    let summary = format_tree_summary(&tree.apex, 0);

    DecompositionPreview {
        total_nodes: total,
        leaf_nodes: leaves,
        decomposition_llm_calls: decomposition_calls,
        estimated_build_llm_calls: build_calls,
        tree_summary: summary,
        estimated_pyramid_depth: if needs_l2 { depth + 1 } else { depth },
    }
}

/// Count total nodes and leaf nodes in a tree.
fn count_nodes(node: &QuestionNode) -> (u32, u32) {
    if node.is_leaf {
        return (1, 1);
    }
    let mut total = 1u32;
    let mut leaves = 0u32;
    for child in &node.children {
        let (t, l) = count_nodes(child);
        total += t;
        leaves += l;
    }
    (total, leaves)
}

/// Compute the depth of a question tree.
fn tree_depth(node: &QuestionNode) -> u32 {
    if node.children.is_empty() {
        return 1;
    }
    1 + node.children.iter().map(tree_depth).max().unwrap_or(0)
}

/// Count non-leaf levels (each is one decomposition LLM call).
fn count_non_leaf_levels(node: &QuestionNode) -> u32 {
    if node.is_leaf || node.children.is_empty() {
        return 0;
    }
    1 + node
        .children
        .iter()
        .map(count_non_leaf_levels)
        .max()
        .unwrap_or(0)
}

/// Format a human-readable tree summary.
fn format_tree_summary(node: &QuestionNode, indent: usize) -> String {
    let prefix = "  ".repeat(indent);
    let leaf_marker = if node.is_leaf { " [leaf]" } else { "" };
    let mut out = format!("{}{}{}\n", prefix, node.question, leaf_marker);
    for child in &node.children {
        out.push_str(&format_tree_summary(child, indent + 1));
    }
    out
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map granularity (1-5) to a (min, max) range for sub-question count.
fn count_tree_nodes(node: &QuestionNode) -> usize {
    1 + node.children.iter().map(count_tree_nodes).sum::<usize>()
}

/// Horizontal review: after decomposing siblings, the LLM reviews all of them
/// together to (1) merge overlapping questions and (2) decide which are already
/// specific enough to be leaves (stopping further decomposition).
///
/// This is the key intelligence gate — instead of blindly recursing into every
/// branch, we ask "given the full picture at this level, are we done?"
///
/// Returns (merges_applied, newly_marked_leaves).
/// 11-H: Loads system prompt from chains/prompts/question/horizontal_review.md when available.
async fn horizontal_review_siblings(
    siblings: &mut Vec<QuestionNode>,
    llm_config: &LlmConfig,
    chains_dir: Option<&std::path::Path>,
    model_tier: &str,
    max_tokens: usize,
    audit: Option<&AuditContext>,
) -> Result<(usize, usize)> {
    if siblings.len() <= 1 {
        return Ok((0, 0));
    }

    let questions_list: Vec<String> = siblings
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let leaf_status = if s.is_leaf { " [LEAF]" } else { " [BRANCH]" };
            format!("  [{}]{} {}", i, leaf_status, s.question)
        })
        .collect();
    let questions_text = questions_list.join("\n");

    // 11-H: Load horizontal review prompt from .md file, fall back to inline
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/horizontal_review.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(loaded) => loaded,
        None => {
            warn!("horizontal_review.md not found — using inline fallback");
            r#"You are reviewing a set of sibling questions that together answer a parent question.

YOUR ONLY JOB: Check if any two questions cover essentially the same territory and should be merged.

For each pair that overlaps significantly:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative with merges — only merge when two questions would produce nearly identical answers from the same evidence.

IMPORTANT: Do NOT convert branches to leaves. If a question is marked as a branch, it stays a branch. The branch/leaf designation reflects the question's role in the pyramid structure, not just its complexity.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}]
}

The merges array can be empty if no questions overlap. Return ONLY the JSON object."#.to_string()
        }
    };

    let user_prompt = format!(
        "Review these sibling sub-questions:\n\n{questions_text}\n\n\
         Which should be merged? Which branches are specific enough to be leaves?"
    );

    let cache_ctx = make_step_ctx_from_llm_config(
        llm_config,
        "question_sibling_review",
        "question_decompose",
        0,
        None,
        &system_prompt,
        model_tier,
        None,
        None,
    )
    .await;
    let audit_ctx = audit_for(audit, "question_sibling_review", Some(0));
    let response = llm::call_model_unified_with_audit_and_ctx(
        llm_config,
        cache_ctx.as_ref(),
        audit_ctx.as_ref(),
        &system_prompt,
        &user_prompt,
        0.1,
        max_tokens,
        None,
        LlmCallOptions::default(),
    )
    .await?;

    let review: serde_json::Value = match llm::extract_json(&response.content) {
        Ok(v) => v,
        Err(_) => return Ok((0, 0)),
    };

    // ── Apply merges ────────────────────────────────────────────────────
    let merges = review
        .get("merges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut removed_indices: Vec<usize> = Vec::new();
    for merge in &merges {
        let keep_idx = merge.get("keep").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let remove_idx = merge.get("remove").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let merged_q = merge
            .get("merged_question")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if keep_idx >= siblings.len() || remove_idx >= siblings.len() || keep_idx == remove_idx {
            continue;
        }
        if removed_indices.contains(&remove_idx) || removed_indices.contains(&keep_idx) {
            continue;
        }

        if !merged_q.is_empty() {
            siblings[keep_idx].question = merged_q.to_string();
        }

        let removed_children: Vec<QuestionNode> = siblings[remove_idx].children.drain(..).collect();
        siblings[keep_idx].children.extend(removed_children);

        info!(
            keep = keep_idx,
            remove = remove_idx,
            merged = merged_q,
            "horizontal review: merging overlapping siblings"
        );
        removed_indices.push(remove_idx);
    }

    removed_indices.sort_unstable();
    removed_indices.reverse();
    for idx in &removed_indices {
        siblings.remove(*idx);
    }
    let merge_count = removed_indices.len();

    // ── Apply leaf marks ────────────────────────────────────────────────
    let leaf_indices: Vec<usize> = review
        .get("mark_as_leaf")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default();

    let mut leaf_count = 0;
    for &idx in &leaf_indices {
        // Indices may have shifted due to merges — adjust
        // Only mark if index is still valid and not already a leaf
        if idx < siblings.len() && !siblings[idx].is_leaf {
            siblings[idx].is_leaf = true;
            siblings[idx].children.clear();
            siblings[idx].about = "each file individually".to_string();
            siblings[idx].creates = "L0 nodes".to_string();
            info!(
                idx = idx,
                question = %siblings[idx].question,
                "horizontal review: marking as leaf — specific enough for direct answering"
            );
            leaf_count += 1;
        }
    }

    Ok((merge_count, leaf_count))
}

fn granularity_to_range(granularity: u32, tier2: &super::Tier2Config) -> (u32, u32) {
    // These are hints passed to the LLM but the prompt no longer forces them.
    // The LLM decides how many sub-questions are genuinely needed.
    // Values loaded from OperationalConfig.tier2.granularity_ranges.
    let ranges = &tier2.granularity_ranges;
    let idx = granularity as usize;
    if idx < ranges.len() {
        ranges[idx]
    } else {
        ranges[0] // default fallback
    }
}

/// Determine the about/creates scope based on tree depth.
///
/// Depth 1 = L1 synthesis, Depth 2+ = L2 synthesis (or deeper if extended).
fn scope_for_depth(depth: u32) -> (String, String) {
    match depth {
        1 => (
            "each L1 thread's assigned L0 nodes".to_string(),
            "L1 nodes".to_string(),
        ),
        _ => (
            format!("all L{} nodes", depth - 1),
            format!("L{} nodes", depth),
        ),
    }
}

/// Build a folder map string from a source path by listing files.
///
/// Used when the caller doesn't provide a folder_map but we have a source_path.
/// Returns a summary of file names, extensions, and directory structure.
pub fn build_folder_map(source_path: &str) -> Option<String> {
    let path = std::path::Path::new(source_path);
    if !path.exists() {
        return None;
    }
    // Single-file sources (e.g. a conversation .jsonl, a single document) get
    // a one-line map. Without this, conversation pyramids hard-fail in
    // characterize because their source is a file, not a directory.
    if path.is_file() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| source_path.to_string());
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        return Some(format!(
            "Source file: {}\nName: {}\nSize: {} bytes",
            source_path, name, size
        ));
    }
    if !path.is_dir() {
        return None;
    }

    let mut entries = Vec::new();
    if let Ok(walker) = walkdir(path, 3) {
        for entry in walker {
            entries.push(entry);
        }
    }

    if entries.is_empty() {
        return None;
    }

    // Limit to first 200 entries
    entries.truncate(200);
    let summary = entries.join("\n");

    Some(format!(
        "Source directory: {}\nFile listing ({} entries):\n{}",
        source_path,
        entries.len(),
        summary
    ))
}

/// Walk a directory up to max_depth, returning relative path strings.
fn walkdir(root: &Path, max_depth: usize) -> Result<Vec<String>> {
    let mut results = Vec::new();
    walk_recursive(root, root, 0, max_depth, &mut results)?;
    Ok(results)
}

fn walk_recursive(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    results: &mut Vec<String>,
) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }

    let entries = std::fs::read_dir(current)
        .map_err(|e| anyhow!("failed to read directory {}: {}", current.display(), e))?;

    let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    sorted.sort_by_key(|e| e.file_name());

    for entry in sorted {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files/dirs
        if name.starts_with('.') {
            continue;
        }
        // Skip common non-source dirs
        if path.is_dir()
            && matches!(
                name.as_str(),
                "node_modules" | "target" | ".git" | "__pycache__" | "dist" | "build"
            )
        {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if path.is_dir() {
            results.push(format!("{}/", rel));
            walk_recursive(root, &path, depth + 1, max_depth, results)?;
        } else {
            results.push(rel);
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::question_compiler;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_leaf(question: &str) -> QuestionNode {
        QuestionNode {
            id: String::new(),
            question: question.to_string(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: "Focus on this aspect.".to_string(),
            children: vec![],
            is_leaf: true,
        }
    }

    fn make_branch(question: &str, children: Vec<QuestionNode>) -> QuestionNode {
        QuestionNode {
            id: String::new(),
            question: question.to_string(),
            about: "all L1 nodes at once".to_string(),
            creates: "L1 nodes".to_string(),
            prompt_hint: "Synthesize children.".to_string(),
            children,
            is_leaf: false,
        }
    }

    fn make_tree(apex_children: Vec<QuestionNode>) -> QuestionTree {
        QuestionTree {
            apex: QuestionNode {
                id: String::new(),
                question: "What should I know about this codebase?".to_string(),
                about: "all top-level nodes at once".to_string(),
                creates: "apex".to_string(),
                prompt_hint: "Comprehensive overview.".to_string(),
                children: apex_children,
                is_leaf: false,
            },
            content_type: "code".to_string(),
            config: DecompositionConfig {
                apex_question: "What should I know about this codebase?".to_string(),
                content_type: "code".to_string(),
                granularity: 3,
                max_depth: 3,
                folder_map: None,
                chains_dir: None,
                audience: None,
                model_tier: default_decomposition_model_tier(),
                temperature: default_decomposition_temperature(),
                max_tokens: default_decomposition_max_tokens(),
                sibling_review_max_tokens: default_sibling_review_max_tokens(),
            },
            audience: None,
        }
    }

    fn setup_prompt_dir() -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "question_decomposition_test_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);

        let code_dir = dir.join("prompts").join("code");
        fs::create_dir_all(&code_dir).unwrap();
        fs::write(
            code_dir.join("code_extract.md"),
            "CODE EXTRACT BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(
            code_dir.join("code_extract_frontend.md"),
            "FRONTEND EXTRACT BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(
            code_dir.join("config_extract.md"),
            "CONFIG EXTRACT BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(
            code_dir.join("code_cluster.md"),
            "CODE CLUSTER BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(
            code_dir.join("code_thread.md"),
            "CODE THREAD BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(code_dir.join("code_web.md"), "CODE WEB BASE\n\n/no_think\n").unwrap();
        fs::write(
            code_dir.join("code_recluster.md"),
            "CODE RECLUSTER BASE\n\n/no_think\n",
        )
        .unwrap();
        fs::write(
            code_dir.join("code_distill.md"),
            "CODE DISTILL BASE\n\n/no_think\n",
        )
        .unwrap();

        dir
    }

    // ── Parse tests ───────────────────────────────────────────────────────

    #[test]
    fn parse_valid_decomposition_response() {
        let response = r#"[
            {"question": "What is the architecture?", "prompt_hint": "Focus on high-level design", "is_leaf": false},
            {"question": "What database is used?", "prompt_hint": "Focus on schema", "is_leaf": true}
        ]"#;

        let result = parse_decomposition_response(response).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].question, "What is the architecture?");
        assert!(!result[0].is_leaf);
        assert_eq!(result[1].question, "What database is used?");
        assert!(result[1].is_leaf);
    }

    #[test]
    fn parse_response_with_markdown_fences() {
        let response = r#"```json
[
    {"question": "How does auth work?", "prompt_hint": "auth flow", "is_leaf": true}
]
```"#;

        let result = parse_decomposition_response(response).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].question, "How does auth work?");
    }

    #[test]
    fn parse_response_with_surrounding_text() {
        let response = r#"Here are the sub-questions:
[
    {"question": "What is the frontend?", "prompt_hint": "UI components", "is_leaf": true}
]
That should cover it."#;

        let result = parse_decomposition_response(response).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_response_missing_prompt_hint_defaults() {
        let response = r#"[{"question": "Test?", "is_leaf": true}]"#;
        let result = parse_decomposition_response(response).unwrap();
        assert_eq!(result[0].prompt_hint, "");
    }

    #[test]
    fn parse_response_missing_is_leaf_defaults_to_true() {
        let response = r#"[{"question": "Test?", "prompt_hint": "hint"}]"#;
        let result = parse_decomposition_response(response).unwrap();
        assert!(result[0].is_leaf);
    }

    #[test]
    fn parse_frontier_response_preserves_multiple_parent_ids() {
        let allowed = HashSet::from(["q-parent-a".to_string(), "q-parent-b".to_string()]);
        let index_map = HashMap::from([
            (0usize, "q-parent-a".to_string()),
            (1usize, "q-parent-b".to_string()),
        ]);
        let response = r#"{
            "children": [
                {
                    "question": "What shared constraint affects both branches?",
                    "prompt_hint": "Find the common constraint.",
                    "is_leaf": true,
                    "parent_ids": ["q-parent-a", "q-parent-b"]
                },
                {
                    "question": "What is parent B's distinct risk?",
                    "prompt_hint": "Focus on branch B.",
                    "is_leaf": false,
                    "parent_indices": [1]
                }
            ]
        }"#;

        let parsed = parse_frontier_decomposition_response(response, &allowed, &index_map).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].parent_ids,
            vec!["q-parent-a".to_string(), "q-parent-b".to_string()]
        );
        assert_eq!(parsed[1].parent_ids, vec!["q-parent-b".to_string()]);
        assert!(!parsed[1].raw.is_leaf);
    }

    #[test]
    fn parse_invalid_json_fails() {
        let response = "This is not JSON at all";
        assert!(parse_decomposition_response(response).is_err());
    }

    #[test]
    fn parse_missing_question_field_fails() {
        let response = r#"[{"prompt_hint": "no question field", "is_leaf": true}]"#;
        assert!(parse_decomposition_response(response).is_err());
    }

    // ── Tree counting tests ───────────────────────────────────────────────

    #[test]
    fn count_nodes_leaf_only() {
        let node = make_leaf("test");
        let (total, leaves) = count_nodes(&node);
        assert_eq!(total, 1);
        assert_eq!(leaves, 1);
    }

    #[test]
    fn count_nodes_simple_tree() {
        let tree = make_tree(vec![make_leaf("A"), make_leaf("B"), make_leaf("C")]);
        let (total, leaves) = count_nodes(&tree.apex);
        assert_eq!(total, 4); // apex + 3 leaves
        assert_eq!(leaves, 3);
    }

    #[test]
    fn count_nodes_nested_tree() {
        let tree = make_tree(vec![
            make_branch("Group A", vec![make_leaf("A1"), make_leaf("A2")]),
            make_branch(
                "Group B",
                vec![make_leaf("B1"), make_leaf("B2"), make_leaf("B3")],
            ),
        ]);
        let (total, leaves) = count_nodes(&tree.apex);
        assert_eq!(total, 8); // apex + 2 branches + 5 leaves
        assert_eq!(leaves, 5);
    }

    #[test]
    fn extract_layer_questions_dedupes_shared_dag_nodes() {
        let shared = make_leaf("Which evidence matters to both branches?");
        let mut tree = make_tree(vec![
            make_branch("Branch A", vec![shared.clone()]),
            make_branch("Branch B", vec![shared]),
        ]);
        assign_question_ids(&mut tree);

        let layers = extract_layer_questions(&tree);
        let all_question_ids = layers
            .values()
            .flat_map(|questions| questions.iter().map(|q| q.question_id.clone()))
            .collect::<Vec<_>>();
        let unique_question_ids = all_question_ids.iter().collect::<HashSet<_>>();

        assert_eq!(all_question_ids.len(), unique_question_ids.len());
    }

    #[test]
    fn assign_question_ids_uses_layer_handle_ids_not_hashes() {
        let shared = make_leaf("Which evidence matters to both branches?");
        let mut tree = make_tree(vec![
            make_branch("Branch A", vec![shared.clone()]),
            make_branch("Branch B", vec![shared]),
        ]);
        assign_question_ids(&mut tree);

        assert_eq!(tree.apex.id, "Q-L3-000");
        assert_eq!(tree.apex.children[0].id, "Q-L2-000");
        assert_eq!(tree.apex.children[1].id, "Q-L2-001");
        assert_eq!(tree.apex.children[0].children[0].id, "Q-L1-000");
        assert_eq!(
            tree.apex.children[0].children[0].id,
            tree.apex.children[1].children[0].id
        );
        assert!(!tree.apex.children[0].children[0].id.starts_with("q-"));
    }

    // ── Tree depth tests ──────────────────────────────────────────────────

    #[test]
    fn tree_depth_flat() {
        let tree = make_tree(vec![make_leaf("A"), make_leaf("B")]);
        assert_eq!(tree_depth(&tree.apex), 2); // apex -> leaf
    }

    #[test]
    fn tree_depth_nested() {
        let tree = make_tree(vec![make_branch(
            "Group",
            vec![make_leaf("A"), make_leaf("B")],
        )]);
        assert_eq!(tree_depth(&tree.apex), 3); // apex -> branch -> leaf
    }

    // ── Preview tests ─────────────────────────────────────────────────────

    #[test]
    fn preview_returns_correct_counts() {
        let tree = make_tree(vec![make_leaf("A"), make_leaf("B"), make_leaf("C")]);
        let preview = preview_decomposition(&tree);
        assert_eq!(preview.total_nodes, 4);
        assert_eq!(preview.leaf_nodes, 3);
        assert!(preview.tree_summary.contains("What should I know"));
        assert!(preview.tree_summary.contains("A"));
    }

    #[test]
    fn preview_with_deep_tree() {
        let tree = make_tree(vec![
            make_branch("Group A", vec![make_leaf("A1"), make_leaf("A2")]),
            make_branch("Group B", vec![make_leaf("B1")]),
            make_leaf("C"),
        ]);
        let preview = preview_decomposition(&tree);
        assert_eq!(preview.total_nodes, 7);
        assert_eq!(preview.leaf_nodes, 4);
        assert!(preview.decomposition_llm_calls >= 1);
    }

    // ── QuestionTree → QuestionSet conversion tests ───────────────────────

    #[test]
    fn tree_to_question_set_basic() {
        let tree = make_tree(vec![
            make_leaf("What is the frontend?"),
            make_leaf("What is the backend?"),
            make_leaf("What is the database?"),
        ]);

        let temp_dir = std::env::temp_dir();
        let qs = question_tree_to_question_set(&tree, "code", &temp_dir).unwrap();

        assert_eq!(qs.r#type, "code");
        assert_eq!(qs.version, "3.0");

        // Should have: L0 extract, clustering, L1 synthesis, L1 webbing, apex
        // (no L2 because only 3 children)
        assert_eq!(qs.questions.len(), 5);

        // Check L0 extraction
        assert_eq!(qs.questions[0].creates, "L0 nodes");
        assert_eq!(qs.questions[0].about, "each file individually");

        // Check clustering
        assert_eq!(qs.questions[1].creates, "L1 thread assignments");
        assert_eq!(qs.questions[1].about, "all L0 topics at once");

        // Check L1 synthesis
        assert_eq!(qs.questions[2].creates, "L1 nodes");

        // Check L1 webbing
        assert_eq!(qs.questions[3].creates, "web edges between L1 nodes");

        // Check apex
        assert_eq!(qs.questions[4].creates, "apex");
    }

    #[test]
    fn tree_to_question_set_with_l2() {
        // 5+ children triggers L2 layer
        let tree = make_tree(vec![
            make_leaf("A"),
            make_leaf("B"),
            make_leaf("C"),
            make_leaf("D"),
            make_leaf("E"),
        ]);

        let temp_dir = std::env::temp_dir();
        let qs = question_tree_to_question_set(&tree, "code", &temp_dir).unwrap();

        // Should have: L0, clustering, L1 synthesis, L1 webbing, L2, apex
        assert_eq!(qs.questions.len(), 6);
        assert_eq!(qs.questions[4].creates, "L2 nodes");
        assert_eq!(qs.questions[5].creates, "apex");
        assert_eq!(qs.questions[5].about, "all top-level nodes at once");
        assert!(qs.questions[4].cluster_prompt.is_some());
    }

    #[test]
    fn tree_to_question_set_empty_tree_fails() {
        let tree = QuestionTree {
            apex: QuestionNode {
                id: String::new(),
                question: "Empty apex".to_string(),
                about: "all top-level nodes at once".to_string(),
                creates: "apex".to_string(),
                prompt_hint: "".to_string(),
                children: vec![],
                is_leaf: false,
            },
            content_type: "code".to_string(),
            config: DecompositionConfig::default(),
            audience: None,
        };

        let temp_dir = std::env::temp_dir();
        let result = question_tree_to_question_set(&tree, "code", &temp_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no leaf nodes"));
    }

    #[test]
    fn tree_to_question_set_uses_real_code_prompts_and_preserves_no_think() {
        let tree = make_tree(vec![
            make_leaf("What is the frontend?"),
            make_leaf("What is auth?"),
        ]);
        let temp_dir = setup_prompt_dir();

        let qs = question_tree_to_question_set(&tree, "code", &temp_dir).unwrap();
        let l0 = &qs.questions[0];

        assert!(l0.prompt.contains("CODE EXTRACT BASE"));
        assert!(l0
            .prompt
            .contains("Additional extraction targets from question decomposition"));
        assert!(l0.prompt.trim_end().ends_with("/no_think"));
        assert!(
            l0.prompt
                .find("Additional extraction targets from question decomposition")
                < l0.prompt.find("/no_think")
        );

        let variants = l0
            .variants
            .as_ref()
            .expect("expected code extract variants");
        assert!(variants["config files"].contains("CONFIG EXTRACT BASE"));
        assert!(variants["frontend (.tsx, .jsx)"].contains("FRONTEND EXTRACT BASE"));
        assert!(variants["config files"].trim_end().ends_with("/no_think"));
        assert!(variants["frontend (.tsx, .jsx)"]
            .trim_end()
            .ends_with("/no_think"));

        let cluster = &qs.questions[1].prompt;
        assert!(cluster.contains("CODE CLUSTER BASE"));
        assert!(cluster.trim_end().ends_with("/no_think"));

        let l2 = &question_tree_to_question_set(
            &make_tree(vec![
                make_leaf("What is the frontend?"),
                make_leaf("What is auth?"),
                make_leaf("What is storage?"),
                make_leaf("What is ingestion?"),
                make_leaf("What is sync?"),
            ]),
            "code",
            &temp_dir,
        )
        .unwrap()
        .questions[4];
        assert!(l2.prompt.contains("CODE DISTILL BASE"));
        assert!(l2
            .cluster_prompt
            .as_deref()
            .expect("expected cluster prompt")
            .contains("CODE RECLUSTER BASE"));
        assert!(l2.prompt.trim_end().ends_with("/no_think"));
        assert!(l2
            .cluster_prompt
            .as_deref()
            .expect("expected cluster prompt")
            .trim_end()
            .ends_with("/no_think"));

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn decomposed_tree_compiles_clustering_with_compact_inputs() {
        let tree = make_tree(vec![
            make_leaf("What is the frontend?"),
            make_leaf("What is auth?"),
            make_leaf("What is storage?"),
        ]);
        let temp_dir = std::env::temp_dir();

        let qs = question_tree_to_question_set(&tree, "code", &temp_dir).unwrap();
        let plan = question_compiler::compile_question_set(&qs, &temp_dir).unwrap();
        let clustering = plan
            .steps
            .iter()
            .find(|step| step.id == "clustering")
            .expect("expected clustering step");

        assert!(clustering.compact_inputs);
        assert_eq!(
            clustering.input,
            serde_json::json!({ "topics": "$l0_extract" })
        );
    }

    fn question_row(
        id: &str,
        depth: u32,
        is_leaf: bool,
        children: Option<Vec<&str>>,
    ) -> db::QuestionNodeRow {
        db::QuestionNodeRow {
            question_id: id.to_string(),
            parent_id: None,
            depth,
            question: format!("Question {id}"),
            about: if is_leaf {
                "each file individually".to_string()
            } else {
                "all top-level nodes at once".to_string()
            },
            creates: if is_leaf {
                "L0 nodes".to_string()
            } else {
                "L1 nodes".to_string()
            },
            prompt_hint: format!("Hint {id}"),
            is_leaf,
            children_json: children.map(|ids| {
                serde_json::to_string(
                    &ids.into_iter()
                        .map(|child| child.to_string())
                        .collect::<Vec<_>>(),
                )
                .unwrap()
            }),
            build_id: None,
            created_at: None,
        }
    }

    fn question_edge(parent: &str, child: &str, ordinal: i64) -> db::QuestionEdgeRow {
        db::QuestionEdgeRow {
            parent_question_id: parent.to_string(),
            child_question_id: child.to_string(),
            ordinal,
            edge_kind: "decomposes".to_string(),
            build_id: None,
        }
    }

    #[test]
    fn question_dag_resume_frontier_uses_canonical_edges() {
        let rows = vec![
            question_row("Q-L3-000", 0, false, None),
            question_row("Q-L2-000", 1, false, None),
            question_row("Q-L2-001", 1, false, None),
        ];
        let edges = vec![
            question_edge("Q-L3-000", "Q-L2-000", 0),
            question_edge("Q-L3-000", "Q-L2-001", 1),
        ];
        let dag = question_dag_from_rows(rows, edges).unwrap();

        assert_eq!(dag.apex_id, "Q-L3-000");
        assert_eq!(
            undecomposed_frontier_ids(&dag, 3),
            vec!["Q-L2-000".to_string(), "Q-L2-001".to_string()]
        );
    }

    #[test]
    fn question_dag_from_rows_preserves_multi_parent_child() {
        let rows = vec![
            question_row("Q-L3-000", 0, false, None),
            question_row("Q-L2-000", 1, false, None),
            question_row("Q-L2-001", 1, false, None),
            question_row("Q-L1-000", 2, true, None),
        ];
        let edges = vec![
            question_edge("Q-L3-000", "Q-L2-000", 0),
            question_edge("Q-L3-000", "Q-L2-001", 1),
            question_edge("Q-L2-000", "Q-L1-000", 0),
            question_edge("Q-L2-001", "Q-L1-000", 0),
        ];
        let dag = question_dag_from_rows(rows, edges).unwrap();

        assert_eq!(
            dag.parents_by_child.get("Q-L1-000").cloned().unwrap(),
            vec!["Q-L2-000".to_string(), "Q-L2-001".to_string()]
        );
        assert!(undecomposed_frontier_ids(&dag, 3).is_empty());
    }

    // ── Granularity tests ─────────────────────────────────────────────────

    #[test]
    fn granularity_affects_range() {
        let tier2 = crate::pyramid::Tier2Config::default();
        let (min1, max1) = granularity_to_range(1, &tier2);
        let (min5, max5) = granularity_to_range(5, &tier2);
        assert!(min5 > min1);
        assert!(max5 > max1);
    }

    // ── Max depth tests ───────────────────────────────────────────────────

    #[test]
    fn max_depth_limits_recursion() {
        // With max_depth=1, everything should be leaf after one decomposition
        let _config = DecompositionConfig {
            apex_question: "Test".to_string(),
            content_type: "code".to_string(),
            granularity: 3,
            max_depth: 1,
            folder_map: None,
            chains_dir: None,
            audience: None,
            model_tier: default_decomposition_model_tier(),
            temperature: default_decomposition_temperature(),
            max_tokens: default_decomposition_max_tokens(),
            sibling_review_max_tokens: default_sibling_review_max_tokens(),
        };
        // Can't test the async decompose directly without LLM, but we can test
        // that build_subtree respects depth limits via the terminal condition.
        let raw = RawDecomposedQuestion {
            question: "Sub question".to_string(),
            prompt_hint: "hint".to_string(),
            is_leaf: false, // NOT marked as leaf, but depth should force it
        };

        // At current_depth >= max_depth (1 >= 1), should become leaf
        let rt = tokio::runtime::Runtime::new().unwrap();
        let llm_config = LlmConfig::default();
        // build_subtree is async but will hit the terminal condition before any LLM call
        let tier1 = crate::pyramid::Tier1Config::default();
        let tier2 = crate::pyramid::Tier2Config::default();
        let result = rt.block_on(build_subtree(
            &raw,
            "code",
            None,
            3,
            1, // max_depth
            1, // current_depth == max_depth
            &llm_config,
            None,
            None,
            "max",
            tier1.decomposition_temperature,
            tier1.decomposition_max_tokens,
            tier1.synthesis_prompts_max_tokens,
            &tier2,
            None,
        ));
        let node = result.unwrap();
        assert!(node.is_leaf);
        assert!(node.children.is_empty());
    }

    // ── Empty folder map handled gracefully ───────────────────────────────

    #[test]
    fn build_folder_map_nonexistent_path_returns_none() {
        let result = build_folder_map("/nonexistent/path/that/should/not/exist");
        assert!(result.is_none());
    }

    // ── Scope for depth ───────────────────────────────────────────────────

    #[test]
    fn scope_for_depth_l1() {
        let (about, creates) = scope_for_depth(1);
        assert_eq!(creates, "L1 nodes");
        assert!(about.contains("L1 thread"));
    }

    #[test]
    fn scope_for_depth_l2() {
        let (about, creates) = scope_for_depth(2);
        assert_eq!(creates, "L2 nodes");
        assert_eq!(about, "all L1 nodes");
    }

    #[test]
    fn scope_for_depth_l3() {
        let (about, creates) = scope_for_depth(3);
        assert_eq!(creates, "L3 nodes");
        assert_eq!(about, "all L2 nodes");
    }

    #[test]
    fn scope_for_depth_l4() {
        let (about, creates) = scope_for_depth(4);
        assert_eq!(creates, "L4 nodes");
        assert_eq!(about, "all L3 nodes");
    }

    // ── Tree summary formatting ───────────────────────────────────────────

    #[test]
    fn format_tree_summary_includes_all_nodes() {
        let tree = make_tree(vec![
            make_branch(
                "Architecture",
                vec![make_leaf("Frontend"), make_leaf("Backend")],
            ),
            make_leaf("Database"),
        ]);
        let summary = format_tree_summary(&tree.apex, 0);
        assert!(summary.contains("Architecture"));
        assert!(summary.contains("Frontend"));
        assert!(summary.contains("Backend"));
        assert!(summary.contains("Database"));
        assert!(summary.contains("[leaf]"));
    }
}
