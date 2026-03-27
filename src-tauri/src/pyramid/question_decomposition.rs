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

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::db;
use super::llm::{self, LlmConfig};
use super::question_yaml::{Question, QuestionDefaults, QuestionSet};

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
}

impl Default for DecompositionConfig {
    fn default() -> Self {
        Self {
            apex_question: String::new(),
            content_type: "code".to_string(),
            granularity: 3,
            max_depth: 3,
            folder_map: None,
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
    /// Stable deterministic ID: `q-{sha256_hex_first_12}`.
    /// NOT produced by the LLM — assigned after deserialization via `assign_question_ids`.
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
/// ID format: `q-{sha256_hex_first_12}` where the hash input is
/// `"{question}|{about}|{depth}"`. Depth is 0 for leaves, increasing toward
/// the root (i.e. the root/apex gets the highest depth value).
///
/// Two passes: first compute max_depth, then assign IDs with correct depths.
pub fn assign_question_ids(tree: &mut QuestionTree) {
    let max_depth = compute_max_depth(&tree.apex);
    assign_ids_recursive(&mut tree.apex, max_depth, 0);
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

/// Recursively assign IDs. `max_depth` is the tree height, `current_level` is
/// how far from the root (0 = root). The node's depth = max_depth - current_level
/// (so leaves = 0, root = max_depth).
fn assign_ids_recursive(node: &mut QuestionNode, max_depth: u32, current_level: u32) {
    let depth = max_depth.saturating_sub(current_level);
    node.id = make_question_id(&node.question, &node.about, depth);
    for child in &mut node.children {
        assign_ids_recursive(child, max_depth, current_level + 1);
    }
}

/// Build a deterministic question ID from question text, about, and depth.
fn make_question_id(question: &str, about: &str, depth: u32) -> String {
    use sha2::{Sha256, Digest};
    let input = format!("{}|{}|{}", question, about, depth);
    let hash = Sha256::digest(input.as_bytes());
    let hex_str = hex::encode(hash);
    format!("q-{}", &hex_str[..12])
}

// ── Layer Question Extraction ─────────────────────────────────────────────────

/// Extract per-layer question sets from a question tree.
///
/// Leaves are layer 0 (L0). Their parents are layer 1. Root/apex is the highest
/// layer. Returns a HashMap<layer, Vec<LayerQuestion>>.
///
/// Requires `assign_question_ids` to have been called first (IDs must be populated).
pub fn extract_layer_questions(
    tree: &QuestionTree,
) -> std::collections::HashMap<i64, Vec<super::types::LayerQuestion>> {
    let max_depth = compute_max_depth(&tree.apex);
    let mut result: std::collections::HashMap<i64, Vec<super::types::LayerQuestion>> =
        std::collections::HashMap::new();
    collect_layer_questions(&tree.apex, max_depth, 0, &mut result);
    result
}

fn collect_layer_questions(
    node: &QuestionNode,
    max_depth: u32,
    current_level: u32,
    result: &mut std::collections::HashMap<i64, Vec<super::types::LayerQuestion>>,
) {
    let depth = max_depth.saturating_sub(current_level) as i64;

    result.entry(depth).or_default().push(super::types::LayerQuestion {
        question_id: node.id.clone(),
        question_text: node.question.clone(),
        layer: depth,
        about: node.about.clone(),
        creates: node.creates.clone(),
    });

    for child in &node.children {
        collect_layer_questions(child, max_depth, current_level + 1, result);
    }
}

// ── Decomposition ─────────────────────────────────────────────────────────────

/// Decompose an apex question into a question tree via LLM calls.
///
/// Uses the "High Intelligence" tier (mapped to `max` in the tier vocabulary)
/// because decomposition is judgment work — it determines the entire pyramid topology.
///
/// The bounded unroll pattern limits recursion to `config.max_depth` levels.
/// Each level is a single LLM call that decomposes ALL questions at that level
/// simultaneously (so they can see each other and avoid overlap).
pub async fn decompose_question(
    config: &DecompositionConfig,
    llm_config: &LlmConfig,
) -> Result<QuestionTree> {
    if config.apex_question.trim().is_empty() {
        return Err(anyhow!("apex_question cannot be empty"));
    }
    if config.max_depth == 0 {
        return Err(anyhow!("max_depth must be at least 1"));
    }

    let granularity = config.granularity.clamp(1, 5);
    let (min_subs, max_subs) = granularity_to_range(granularity);

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
) -> Result<QuestionTree> {
    if config.apex_question.trim().is_empty() {
        return Err(anyhow!("apex_question cannot be empty"));
    }
    if config.max_depth == 0 {
        return Err(anyhow!("max_depth must be at least 1"));
    }

    let granularity = config.granularity.clamp(1, 5);
    let (min_subs, max_subs) = granularity_to_range(granularity);

    // Check for existing partial tree
    let existing_count = {
        let conn = writer.lock().await;
        db::count_question_nodes(&conn, slug)?
    };

    if existing_count > 0 {
        // Resume path: check if there are undecomposed branch nodes
        let undecomposed = {
            let conn = writer.lock().await;
            db::get_undecomposed_nodes(&conn, slug)?
        };

        if undecomposed.is_empty() {
            // Tree is complete — reconstruct and return
            info!(
                slug = slug,
                existing_nodes = existing_count,
                "question tree already fully decomposed, reconstructing"
            );
            let rows = {
                let conn = writer.lock().await;
                db::load_question_nodes_as_tree(&conn, slug)?
                    .ok_or_else(|| anyhow!("no nodes found despite count > 0"))?
            };
            return db::reconstruct_question_tree(&rows, config);
        }

        info!(
            slug = slug,
            existing_nodes = existing_count,
            undecomposed = undecomposed.len(),
            "resuming decomposition from existing partial tree"
        );

        // Decompose each undecomposed branch node
        for node_row in &undecomposed {
            let raw = RawDecomposedQuestion {
                question: node_row.question.clone(),
                prompt_hint: node_row.prompt_hint.clone(),
                is_leaf: false,
            };

            // Determine current_depth from the node's depth in the tree
            // node_row.depth is the tree depth (root=max, leaves=0), we need
            // the distance from root for decomposition depth
            let current_depth = node_row.depth;

            info!(
                slug = slug,
                question_id = %node_row.question_id,
                depth = current_depth,
                question = %node_row.question,
                "resuming decomposition for unfinished node"
            );

            build_subtree_incremental(
                &raw,
                &config.content_type,
                config.folder_map.as_deref(),
                granularity,
                config.max_depth,
                current_depth,
                llm_config,
                writer.clone(),
                slug,
                Some(&node_row.question_id),
            )
            .await?;
        }

        // Reconstruct the full tree
        let rows = {
            let conn = writer.lock().await;
            db::load_question_nodes_as_tree(&conn, slug)?
                .ok_or_else(|| anyhow!("no nodes found after resume decomposition"))?
        };
        return db::reconstruct_question_tree(&rows, config);
    }

    // Fresh decomposition — no existing nodes
    info!(
        apex = %config.apex_question,
        content_type = %config.content_type,
        granularity = granularity,
        max_depth = config.max_depth,
        slug = slug,
        "starting incremental question decomposition"
    );

    // First call: apex → L1 sub-questions
    let sub_questions = call_decomposition_llm(
        &config.apex_question,
        &config.content_type,
        config.folder_map.as_deref(),
        min_subs,
        max_subs,
        1,
        llm_config,
    )
    .await?;

    // Build apex node (we'll save it after we know its children)
    let mut apex_children = Vec::new();

    for (i, sq) in sub_questions.iter().enumerate() {
        info!(
            branch = i + 1,
            total = sub_questions.len(),
            question = %sq.question,
            is_leaf = sq.is_leaf,
            slug = slug,
            "decomposing L1 branch (incremental)"
        );
        let child = build_subtree_incremental(
            sq,
            &config.content_type,
            config.folder_map.as_deref(),
            granularity,
            config.max_depth,
            2,
            llm_config,
            writer.clone(),
            slug,
            None, // parent_id set after apex node gets its ID
        )
        .await?;
        let node_count = count_tree_nodes(&child);
        info!(
            branch = i + 1,
            question = %sq.question,
            nodes = node_count,
            slug = slug,
            "L1 branch complete (incremental)"
        );
        apex_children.push(child);
    }

    // ── Horizontal review: merge overlaps + depth-check L1 siblings ────
    let (merged, leafed) = horizontal_review_siblings(&mut apex_children, llm_config).await?;
    if merged > 0 || leafed > 0 {
        info!(
            merged = merged,
            marked_as_leaf = leafed,
            remaining = apex_children.len(),
            slug = slug,
            "horizontal review: merged overlaps and checked depth"
        );
    }

    let apex_node = QuestionNode {
        id: String::new(),
        question: config.apex_question.clone(),
        about: "all top-level nodes at once".to_string(),
        creates: "apex".to_string(),
        prompt_hint: "Synthesize all sub-answers into a comprehensive answer to the apex question."
            .to_string(),
        children: apex_children,
        is_leaf: false,
    };

    let mut tree = QuestionTree {
        apex: apex_node,
        content_type: config.content_type.clone(),
        config: config.clone(),
        audience: None,
    };

    // Assign stable deterministic IDs to all nodes
    assign_question_ids(&mut tree);

    // Now persist ALL nodes to the DB with correct IDs
    save_tree_nodes_to_db(&tree.apex, None, 0, slug, &writer).await?;

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

/// Recursively save all nodes in a tree to the DB.
async fn save_tree_nodes_to_db(
    node: &QuestionNode,
    parent_id: Option<&str>,
    depth: u32,
    slug: &str,
    writer: &Arc<Mutex<Connection>>,
) -> Result<()> {
    {
        let conn = writer.lock().await;
        db::save_question_node(&conn, slug, node, parent_id, depth)?;
    }

    let node_id = node.id.clone();
    for child in &node.children {
        Box::pin(save_tree_nodes_to_db(
            child,
            Some(&node_id),
            depth + 1,
            slug,
            writer,
        ))
        .await?;
    }

    Ok(())
}

/// Incrementally build a subtree, persisting each node as it's created.
///
/// Similar to `build_subtree` but saves nodes to DB after each LLM call.
/// For resumed builds, `existing_parent_id` lets us update the parent's
/// children_json once we know the child IDs.
async fn build_subtree_incremental(
    raw: &RawDecomposedQuestion,
    content_type: &str,
    folder_map: Option<&str>,
    granularity: u32,
    max_depth: u32,
    current_depth: u32,
    llm_config: &LlmConfig,
    writer: Arc<Mutex<Connection>>,
    slug: &str,
    _existing_parent_id: Option<&str>,
) -> Result<QuestionNode> {
    // Terminal conditions: marked as leaf, or depth exceeded
    if raw.is_leaf || current_depth >= max_depth {
        let node = QuestionNode {
            id: String::new(), // Will be assigned later by assign_question_ids
            question: raw.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: raw.prompt_hint.clone(),
            children: vec![],
            is_leaf: true,
        };
        return Ok(node);
    }

    // The LLM decides whether to decompose further — it can return an empty
    // array if the question is already specific enough. No forced minimums.

    let (min_subs, max_subs) = granularity_to_range(granularity);

    info!(
        depth = current_depth,
        question = %raw.question,
        slug = slug,
        "decomposing sub-question (incremental)"
    );
    let sub_questions = call_decomposition_llm(
        &raw.question,
        content_type,
        folder_map,
        min_subs,
        max_subs,
        current_depth,
        llm_config,
    )
    .await?;

    info!(
        depth = current_depth,
        question = %raw.question,
        sub_count = sub_questions.len(),
        slug = slug,
        "sub-questions returned (incremental)"
    );

    if sub_questions.is_empty() {
        let node = QuestionNode {
            id: String::new(),
            question: raw.question.clone(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: raw.prompt_hint.clone(),
            children: vec![],
            is_leaf: true,
        };
        return Ok(node);
    }

    let mut children = Vec::new();
    for sq in sub_questions {
        let child = Box::pin(build_subtree_incremental(
            &sq,
            content_type,
            folder_map,
            granularity,
            max_depth,
            current_depth + 1,
            llm_config,
            writer.clone(),
            slug,
            None,
        ))
        .await?;
        children.push(child);
    }

    let (about, creates) = scope_for_depth(current_depth);

    let node = QuestionNode {
        id: String::new(),
        question: raw.question.clone(),
        about,
        creates,
        prompt_hint: raw.prompt_hint.clone(),
        children,
        is_leaf: false,
    };

    info!(
        slug = slug,
        depth = current_depth,
        children_count = node.children.len(),
        question = %raw.question,
        "decomposed question node (incremental)"
    );

    Ok(node)
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

    let (min_subs, max_subs) = granularity_to_range(granularity);

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
        ))
        .await?;
        children.push(child);
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
/// Uses the "max" tier (High Intelligence) because decomposition is judgment work.
async fn call_decomposition_llm(
    parent_question: &str,
    content_type: &str,
    folder_map: Option<&str>,
    _min_subs: u32,
    _max_subs: u32,
    depth: u32,
    llm_config: &LlmConfig,
) -> Result<Vec<RawDecomposedQuestion>> {
    let folder_context = folder_map.unwrap_or("(no folder map provided)");

    let system_prompt = format!(
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

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (what to focus on when answering),
  "is_leaf": boolean

Return ONLY the JSON array. An empty array [] is valid if the parent question needs no decomposition."#,
    );

    let user_prompt = format!(
        r#"Parent question: "{parent_question}"

Source material:
{folder_context}

What are the genuinely distinct sub-questions needed to answer this? Only create sub-questions that cover unique territory."#,
    );

    // Use the "max" tier model for decomposition (needs reasoning quality)
    let mut decomp_config = llm_config.clone();
    decomp_config.primary_model = llm_config.fallback_model_2.clone();

    let temperature = 0.3;
    let max_tokens: usize = 4096;

    // Try up to 2 times on parse failure
    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        let response = llm::call_model_unified(
            &decomp_config,
            &system_prompt,
            &user_prompt,
            temp,
            max_tokens,
            None,
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
            cluster_model: Some("qwen/qwen3.5-flash-02-23".to_string()),
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
    if node.is_leaf {
        return vec![node];
    }
    let mut leaves = Vec::new();
    for child in &node.children {
        leaves.extend(collect_leaves(child));
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
async fn horizontal_review_siblings(
    siblings: &mut Vec<QuestionNode>,
    llm_config: &LlmConfig,
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

    let system_prompt = r#"You are reviewing a set of sibling questions that together answer a parent question. You have two jobs:

JOB 1 — MERGE OVERLAPS:
Identify pairs of questions that cover essentially the same territory. For each merge:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

JOB 2 — DEPTH CHECK:
For each remaining question currently marked [BRANCH], decide: is this question specific enough to be answered directly from source material? If YES, mark it as a leaf (stopping further decomposition).

Think about it this way: further decomposition is only valuable if the question is genuinely too broad to answer from source files. If a skilled reader could answer it by looking at the relevant files, it's a leaf.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": [N, N, ...]
}

Both arrays can be empty. Be conservative with merges but aggressive with marking leaves — prefer fewer, deeper questions over a sprawling shallow tree.

Return ONLY the JSON object."#;

    let user_prompt = format!(
        "Review these sibling sub-questions:\n\n{questions_text}\n\n\
         Which should be merged? Which branches are specific enough to be leaves?"
    );

    let mut review_config = llm_config.clone();
    review_config.primary_model = llm_config.fallback_model_2.clone();

    let response = llm::call_model_unified(
        &review_config,
        system_prompt,
        &user_prompt,
        0.1,
        2048,
        None,
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

fn granularity_to_range(granularity: u32) -> (u32, u32) {
    // These are hints passed to the LLM but the prompt no longer forces them.
    // The LLM decides how many sub-questions are genuinely needed.
    match granularity {
        1 => (2, 3),
        2 => (3, 4),
        3 => (3, 4),
        4 => (4, 5),
        5 => (5, 6),
        _ => (3, 4),
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
        _ => ("all L1 nodes at once".to_string(), "L2 nodes".to_string()),
    }
}

/// Build a folder map string from a source path by listing files.
///
/// Used when the caller doesn't provide a folder_map but we have a source_path.
/// Returns a summary of file names, extensions, and directory structure.
pub fn build_folder_map(source_path: &str) -> Option<String> {
    let path = std::path::Path::new(source_path);
    if !path.exists() || !path.is_dir() {
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

    // ── Granularity tests ─────────────────────────────────────────────────

    #[test]
    fn granularity_affects_range() {
        let (min1, max1) = granularity_to_range(1);
        let (min5, max5) = granularity_to_range(5);
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
        let result = rt.block_on(build_subtree(
            &raw,
            "code",
            None,
            3,
            1, // max_depth
            1, // current_depth == max_depth
            &llm_config,
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
        let (_about, creates) = scope_for_depth(2);
        assert_eq!(creates, "L2 nodes");
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
