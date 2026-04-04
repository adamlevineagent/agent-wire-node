// pyramid/evidence_answering.rs — Evidence-Weighted Answering (Steps 3.1–3.2)
//
// Two-phase approach to building upper-layer pyramid nodes:
//   1. pre_map_layer()   — Horizontal pre-mapping: one LLM call maps ALL questions
//                          to candidate evidence nodes from the layer below.
//   2. answer_questions() — Vertical answering: parallel per-question LLM calls that
//                          evaluate candidates, produce KEEP/DISCONNECT/MISSING verdicts,
//                          and synthesize answers into new pyramid nodes.
//
// This replaces the old clustering/synthesis approach with question-driven evidence
// answering. Each upper-layer node is the answer to a specific question, grounded
// in weighted evidence links to lower-layer nodes.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use std::path::PathBuf;

use rusqlite;

use super::db;
use super::llm::{self, LlmConfig};
use super::question_decomposition::render_prompt_template;
use super::types::{
    AnswerBatchResult, AnsweredNode, CandidateMap, EvidenceLink, EvidenceSet, EvidenceVerdict,
    FailedQuestion, LayerQuestion, PyramidNode,
};
use super::OperationalConfig;

/// Check if an L0 node ID is a targeted re-examination (L0-{uuid} format).
/// Canonical L0 nodes use patterns like C-L0-001, D-L0-042, or short sequential IDs.
/// Targeted evidence nodes use L0-{uuid} where the UUID part is 36 chars.
fn is_targeted_l0_id(id: &str) -> bool {
    // Targeted: "L0-" followed by a UUID (36 chars with hyphens, e.g., L0-491a10ef-4b59-...)
    if let Some(suffix) = id.strip_prefix("L0-") {
        suffix.len() >= 36 && suffix.chars().nth(8) == Some('-')
    } else {
        false
    }
}

// ── L0 Summary Helper ────────────────────────────────────────────────────────

/// Build a summary string from L0 nodes for use in synthesis prompt generation.
///
/// Concatenates each node's headline + distilled text (truncated to ~200 chars
/// per node). Total budget: ~100K chars. If it exceeds that, truncates from the end.
pub fn build_l0_summary(nodes: &[PyramidNode], ops: &OperationalConfig) -> String {
    let budget = ops.tier2.l0_summary_budget;
    let mut summary = String::new();
    for node in nodes {
        let distilled_trunc: String = node.distilled.chars().take(200).collect();
        let entry = format!("- {}: {}\n", node.headline, distilled_trunc);
        if summary.len() + entry.len() > budget {
            summary.push_str("... (truncated)\n");
            break;
        }
        summary.push_str(&entry);
    }
    summary
}

// ── Step 3.1: Horizontal Pre-Mapping ─────────────────────────────────────────

/// Map all questions for a layer to candidate evidence nodes from the layer below.
///
/// One LLM call reads ALL questions + ALL node headlines/distilled from the lower
/// layer. Returns a CandidateMap (question_id → [candidate_node_ids]).
///
/// Intentionally OVER-INCLUDES candidates — better a false positive than a miss.
/// The answering step (3.2) will prune irrelevant candidates via verdicts.
///
/// Uses mercury-2 (fast model) since this is classification, not synthesis.
pub async fn pre_map_layer(
    questions: &[LayerQuestion],
    lower_layer_nodes: &[PyramidNode],
    llm_config: &LlmConfig,
    ops: &OperationalConfig,
    audience: Option<&str>,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
    evidence_sets: Option<&[EvidenceSet]>, // loaded by caller, None for single-pass
) -> Result<CandidateMap> {
    if questions.is_empty() {
        return Ok(CandidateMap {
            mappings: HashMap::new(),
        });
    }
    if lower_layer_nodes.is_empty() {
        // No evidence to map — return empty candidates for each question
        let mappings = questions
            .iter()
            .map(|q| (q.question_id.clone(), Vec::new()))
            .collect();
        return Ok(CandidateMap { mappings });
    }

    // ── Build question listing ──────────────────────────────────────────
    let questions_text = questions
        .iter()
        .map(|q| {
            format!(
                "  - id: \"{}\"\n    question: \"{}\"\n    about: \"{}\"\n    creates: \"{}\"",
                q.question_id, q.question_text, q.about, q.creates
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ── Build node listing with token budget guard ────────────────────
    let mut nodes_text = lower_layer_nodes
        .iter()
        .map(|n| {
            format!(
                "  - id: \"{}\"\n    headline: \"{}\"\n    distilled: \"{}\"",
                n.id,
                n.headline.chars().take(200).collect::<String>(),
                n.distilled.chars().take(300).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Token budget guard: if combined prompt exceeds budget, try two-stage first, then headlines-only
    let combined_len = questions_text.len() + nodes_text.len();
    if combined_len > ops.tier2.pre_map_prompt_budget {
        // WS-3B: Two-stage pre-mapping via evidence set indexes
        if let Some(sets) = evidence_sets {
            if !sets.is_empty() {
                return pre_map_layer_two_stage(
                    questions,
                    sets,
                    lower_layer_nodes,
                    llm_config,
                    ops,
                    audience,
                    chains_dir,
                    source_content_type,
                )
                .await;
            }
        }
        // else: fall through to existing headlines-only path
        warn!(
            combined_len,
            budget = ops.tier2.pre_map_prompt_budget,
            "Pre-mapping prompt exceeds budget, switching to headlines-only mode"
        );
        nodes_text = lower_layer_nodes
            .iter()
            .map(|n| {
                format!(
                    "  - id: \"{}\"\n    headline: \"{}\"",
                    n.id,
                    n.headline.chars().take(200).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        // If STILL too large after headlines-only, truncate node list
        if questions_text.len() + nodes_text.len() > ops.tier2.pre_map_prompt_budget {
            let max_nodes = ops
                .tier2
                .pre_map_prompt_budget
                .saturating_sub(questions_text.len())
                / 220; // ~220 chars per headline entry
            warn!(
                total_nodes = lower_layer_nodes.len(),
                max_nodes, "Pre-mapping still too large, truncating node list"
            );
            nodes_text = lower_layer_nodes
                .iter()
                .take(max_nodes)
                .map(|n| {
                    format!(
                        "  - id: \"{}\"\n    headline: \"{}\"",
                        n.id,
                        n.headline.chars().take(200).collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }

    // ── Prompts ─────────────────────────────────────────────────────────
    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "The questioner is {aud}. ALL evidence is potentially relevant regardless of vocabulary — the answering step handles translation.\n"
        ),
        _ => String::new(),
    };

    let content_type_block = match source_content_type {
        Some(ct) if !ct.is_empty() => format!("The source material is \"{ct}\" content.\n"),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/pre_map.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("pre_map.md not found — using inline fallback");
            format!(
                r#"You are mapping questions to candidate evidence nodes. Your job is to determine which nodes from the layer below MIGHT contain relevant evidence for each question.

{audience_block}IMPORTANT: Over-include rather than miss. If a node MIGHT be relevant, include it. The next step will prune irrelevant candidates — a false positive here costs little, but a miss loses evidence permanently.

ALL evidence is potentially relevant regardless of how technical or internal it appears — the answering step handles translation for the audience.

{content_type_block}
Respond with ONLY a JSON object in this exact format:
{{{{
  "mappings": {{{{
    "question_id_1": ["node_id_a", "node_id_b"],
    "question_id_2": ["node_id_c"],
    ...
  }}}}
}}}}

Every question_id from the input MUST appear as a key in the mappings, even if its candidate list is empty."#
            )
        }
    };

    let user_prompt = format!(
        "QUESTIONS for this layer:\n{}\n\nNODES from the layer below (candidate evidence):\n{}\n\nFor each question, identify which nodes likely contain relevant evidence. Include uncertain matches.",
        questions_text, nodes_text
    );

    // ── LLM call (primary model — fast classification) ──────
    let response = llm::call_model_unified(
        llm_config,
        &system_prompt,
        &user_prompt,
        ops.tier1.pre_map_temperature,
        ops.tier1.pre_map_max_tokens,
        None,
    )
    .await?;

    info!(
        questions = questions.len(),
        nodes = lower_layer_nodes.len(),
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        "pre-mapping LLM call complete"
    );

    // ── Parse response ──────────────────────────────────────────────────
    let json_value = llm::extract_json(&response.content)?;

    let raw: PreMapResponse = serde_json::from_value(json_value).map_err(|e| {
        anyhow!(
            "Failed to parse pre-mapping response: {} — raw: {}",
            e,
            &response.content[..response.content.len().min(400)]
        )
    })?;

    // Validate: every question should have an entry; add empty vec for any missing
    let mut mappings = raw.mappings;
    for q in questions {
        mappings.entry(q.question_id.clone()).or_default();
    }

    // Filter out any node IDs that don't actually exist in the lower layer
    let valid_ids: std::collections::HashSet<&str> =
        lower_layer_nodes.iter().map(|n| n.id.as_str()).collect();
    for candidates in mappings.values_mut() {
        candidates.retain(|id| valid_ids.contains(id.as_str()));
    }

    let total_candidates: usize = mappings.values().map(|v| v.len()).sum();
    info!(
        total_candidates,
        questions = questions.len(),
        "pre-mapping complete"
    );

    Ok(CandidateMap { mappings })
}

// ── WS-3B: Two-Stage Pre-Mapping ─────────────────────────────────────────────

/// Stage 1 asks the LLM which evidence sets are relevant per question (using
/// lightweight index headlines). Stage 2 filters the full node list to only
/// members of the relevant sets, then delegates to the existing single-pass
/// pre-mapping logic on the smaller subset.
async fn pre_map_layer_two_stage(
    questions: &[LayerQuestion],
    evidence_sets: &[EvidenceSet],
    all_l0_nodes: &[PyramidNode],
    llm_config: &LlmConfig,
    ops: &OperationalConfig,
    audience: Option<&str>,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
) -> Result<CandidateMap> {
    info!(
        questions = questions.len(),
        evidence_sets = evidence_sets.len(),
        total_l0 = all_l0_nodes.len(),
        "starting two-stage pre-mapping"
    );

    // ── Stage 1: Map questions to evidence sets ──────────────────────────

    // Build evidence set listing
    let sets_text = evidence_sets
        .iter()
        .map(|s| {
            let headline = s
                .index_headline
                .as_deref()
                .unwrap_or("(no index headline)");
            format!(
                "  - self_prompt: \"{}\"\n    index_headline: \"{}\"\n    member_count: {}",
                s.self_prompt, headline, s.member_count
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Build question listing (same format as single-pass)
    let questions_text = questions
        .iter()
        .map(|q| {
            format!(
                "  - id: \"{}\"\n    question: \"{}\"\n    about: \"{}\"\n    creates: \"{}\"",
                q.question_id, q.question_text, q.about, q.creates
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Load stage1 prompt
    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "The questioner is {aud}. ALL evidence sets are potentially relevant regardless of vocabulary.\n"
        ),
        _ => String::new(),
    };

    let content_type_block = match source_content_type {
        Some(ct) if !ct.is_empty() => format!("The source material is \"{ct}\" content.\n"),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/pre_map_stage1.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => super::question_decomposition::render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("pre_map_stage1.md not found — using inline fallback");
            format!(
                r#"You are mapping questions to EVIDENCE SETS. Each evidence set is a group of related L0 nodes identified by a self_prompt. Your job is to determine which evidence sets MIGHT contain relevant evidence for each question.

{audience_block}IMPORTANT: Over-include rather than miss. If an evidence set MIGHT be relevant, include it. The next stage will do fine-grained mapping within the selected sets.

{content_type_block}
Respond with ONLY a JSON object in this exact format:
{{{{
  "set_mappings": {{{{
    "question_id_1": ["self_prompt_a", "self_prompt_b"],
    "question_id_2": ["self_prompt_c"],
    ...
  }}}}
}}}}

Every question_id from the input MUST appear as a key in set_mappings, even if its list is empty."#
            )
        }
    };

    let user_prompt = format!(
        "QUESTIONS for this layer:\n{}\n\nEVIDENCE SETS available:\n{}\n\nFor each question, identify which evidence sets likely contain relevant evidence. Include uncertain matches.",
        questions_text, sets_text
    );

    // LLM call — stage 1 (fast classification)
    let response = llm::call_model_unified(
        llm_config,
        &system_prompt,
        &user_prompt,
        ops.tier1.pre_map_temperature,
        ops.tier1.pre_map_max_tokens,
        None,
    )
    .await?;

    info!(
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        "two-stage pre-mapping stage 1 LLM call complete"
    );

    // Parse stage 1 response
    let json_value = llm::extract_json(&response.content)?;
    let mut stage1: Stage1Response = serde_json::from_value(json_value).map_err(|e| {
        anyhow!(
            "Failed to parse stage 1 set_mappings response: {} — raw: {}",
            e,
            &response.content[..response.content.len().min(400)]
        )
    })?;

    // Validate stage 1: filter to only valid self_prompts, fill missing questions
    let valid_prompts: std::collections::HashSet<&str> =
        evidence_sets.iter().map(|s| s.self_prompt.as_str()).collect();

    for q in questions {
        let entry = stage1.set_mappings.entry(q.question_id.clone()).or_insert_with(Vec::new);
        let before = entry.len();
        entry.retain(|sp| valid_prompts.contains(sp.as_str()));
        let removed = before - entry.len();
        if removed > 0 {
            warn!(
                question_id = %q.question_id,
                removed,
                "stage 1 validation: filtered out hallucinated self_prompt strings"
            );
        }
    }

    // Collect all relevant set self_prompts across all questions
    let relevant_self_prompts: std::collections::HashSet<&str> = stage1
        .set_mappings
        .values()
        .flat_map(|v| v.iter().map(|s| s.as_str()))
        .collect();

    info!(
        relevant_sets = relevant_self_prompts.len(),
        total_sets = evidence_sets.len(),
        "stage 1 selected evidence sets"
    );

    // ── Stage 2: Filter nodes to relevant sets + canonical, then single-pass ─

    // Filtered node list: canonical L0 (empty self_prompt) + members of relevant sets
    // Exclude evidence set index nodes (id starts with "ES-")
    let filtered_nodes: Vec<&PyramidNode> = all_l0_nodes
        .iter()
        .filter(|n| {
            if n.id.starts_with("ES-") {
                return false;
            }
            if n.self_prompt.is_empty() {
                return true; // canonical L0
            }
            relevant_self_prompts.contains(n.self_prompt.as_str())
        })
        .collect();

    info!(
        total_l0 = all_l0_nodes.len(),
        filtered = filtered_nodes.len(),
        "stage 2 filtered node list"
    );

    // Clone filtered nodes into an owned vec for the single-pass call
    let filtered_owned: Vec<PyramidNode> = filtered_nodes.into_iter().cloned().collect();

    // Delegate to existing single-pass pre-mapping on the filtered subset.
    // Pass evidence_sets=None so it won't recurse back into two-stage.
    // Box::pin required because pre_map_layer → two_stage → pre_map_layer is recursive async.
    Box::pin(pre_map_layer(
        questions,
        &filtered_owned,
        llm_config,
        ops,
        audience,
        chains_dir,
        source_content_type,
        None, // single-pass — no evidence sets
    ))
    .await
}

/// Internal deserialization target for stage 1 of two-stage pre-mapping.
#[derive(Deserialize)]
struct Stage1Response {
    set_mappings: HashMap<String, Vec<String>>,
}

/// Internal deserialization target for the pre-mapping LLM response.
#[derive(Deserialize)]
struct PreMapResponse {
    mappings: HashMap<String, Vec<String>>,
}

// ── Step 3.2: Vertical Answering ─────────────────────────────────────────────

/// Answer all questions in parallel using their candidate evidence.
///
/// For each question:
///   1. Look up candidates from the CandidateMap
///   2. Fetch full node content for each candidate
///   3. LLM call to evaluate evidence and synthesize an answer
///   4. Parse KEEP/DISCONNECT/MISSING verdicts
///   5. Save evidence links to pyramid_evidence table
///   6. Save the answered node to pyramid_nodes
///
/// Returns the answered nodes with their evidence links and any MISSING reports.
/// The caller is responsible for persisting results to the database (e.g. via
/// spawn_blocking), which solves the `&Connection` / `!Send` problem.
///
/// Parallel, 5x concurrency via tokio::sync::Semaphore.
pub async fn answer_questions(
    questions: &[LayerQuestion],
    candidate_map: &CandidateMap,
    all_nodes: &[PyramidNode],
    synthesis_prompt: Option<&str>,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    slug: &str,
    answer_slug: &str,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
    ops: &OperationalConfig,
) -> Result<AnswerBatchResult> {
    if questions.is_empty() {
        return Ok(AnswerBatchResult {
            answered: Vec::new(),
            failed: Vec::new(),
        });
    }

    // Build a lookup map for all nodes by ORIGINAL ID
    let node_map: HashMap<&str, &PyramidNode> =
        all_nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let semaphore = Arc::new(Semaphore::new(ops.tier1.answer_concurrency));
    let llm_config = Arc::new(llm_config.clone());
    let slug = slug.to_string();
    let synthesis_prompt = synthesis_prompt.map(|s| s.to_string());
    let audience = audience.map(|s| s.to_string());
    let answer_temperature = ops.tier1.answer_temperature;
    let answer_max_tokens = ops.tier1.answer_max_tokens;

    // Prepare per-question work items, rewriting cross-slug node IDs to handle-paths
    let answer_slug_owned = answer_slug.to_string();
    let work_items: Vec<AnswerWorkItem> = questions
        .iter()
        .map(|q| {
            let candidate_ids = candidate_map
                .mappings
                .get(&q.question_id)
                .cloned()
                .unwrap_or_default();

            // Resolve candidate IDs to full node data, rewriting IDs for cross-slug candidates
            let candidate_nodes: Vec<PyramidNode> = candidate_ids
                .iter()
                .filter_map(|id| {
                    node_map.get(id.as_str()).map(|n| {
                        let mut node = (*n).clone();
                        // If candidate comes from a different slug, rewrite its ID to handle-path
                        if node.slug != answer_slug_owned {
                            node.id = db::format_handle_path(&node.slug, node.depth, &node.id);
                        }
                        node
                    })
                })
                .collect();

            AnswerWorkItem {
                question: q.clone(),
                candidate_nodes,
            }
        })
        .collect();

    // Spawn parallel tasks — each returns (question_meta, Result<AnsweredNode>)
    let mut handles = Vec::new();
    for work in work_items {
        let semaphore = semaphore.clone();
        let llm_config = llm_config.clone();
        let slug = slug.clone();
        let answer_slug = answer_slug_owned.clone();
        let synthesis_prompt = synthesis_prompt.clone();
        let audience = audience.clone();
        let chains_dir_owned = chains_dir.cloned();
        let source_ct = source_content_type.map(|s| s.to_string());
        let q_id = work.question.question_id.clone();
        let q_text = work.question.question_text.clone();
        let q_layer = work.question.layer;

        let handle = tokio::spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("answer semaphore should remain open");

            let result = answer_single_question(
                &work.question,
                &work.candidate_nodes,
                synthesis_prompt.as_deref(),
                audience.as_deref(),
                &llm_config,
                &slug,
                &answer_slug,
                answer_temperature,
                answer_max_tokens,
                chains_dir_owned.as_ref(),
                source_ct.as_deref(),
            )
            .await;
            (q_id, q_text, q_layer, result)
        });

        handles.push(handle);
    }

    // Collect results — NO DB writes here. The caller persists via spawn_blocking.
    let mut answered_nodes = Vec::new();
    let mut failed_questions = Vec::new();
    let mut total_evidence = 0usize;
    let mut total_missing = 0usize;

    for handle in handles {
        let (q_id, q_text, q_layer, result) = handle
            .await
            .map_err(|e| anyhow!("Answer task panicked: {}", e))?;
        match result {
            Ok(answered) => {
                total_evidence += answered.evidence.len();
                total_missing += answered.missing.len();
                answered_nodes.push(answered);
            }
            Err(e) => {
                warn!(question_id = %q_id, error = %e, "Failed to answer question — recording as gap report");
                failed_questions.push(FailedQuestion {
                    question_id: q_id,
                    question_text: q_text,
                    layer: q_layer,
                    error: e.to_string(),
                });
            }
        }
    }

    info!(
        answered = answered_nodes.len(),
        failed = failed_questions.len(),
        total_evidence,
        total_missing,
        "answering complete"
    );

    Ok(AnswerBatchResult {
        answered: answered_nodes,
        failed: failed_questions,
    })
}

// ── Internal Types ───────────────────────────────────────────────────────────

struct AnswerWorkItem {
    question: LayerQuestion,
    candidate_nodes: Vec<PyramidNode>,
}

// ── Per-Question Answering ───────────────────────────────────────────────────

/// Answer a single question using its candidate evidence nodes.
///
/// Returns an AnsweredNode containing the synthesized node, evidence links, and
/// any MISSING evidence reports.
async fn answer_single_question(
    question: &LayerQuestion,
    candidate_nodes: &[PyramidNode],
    synthesis_prompt: Option<&str>,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    slug: &str,
    answer_slug: &str,
    answer_temperature: f32,
    answer_max_tokens: usize,
    chains_dir: Option<&PathBuf>,
    source_content_type: Option<&str>,
) -> Result<AnsweredNode> {
    let node_id = format!("L{}-{}", question.layer, Uuid::new_v4());

    // Build candidate_map keyed by the IDs shown to the LLM (handle-paths for cross-slug, bare for same-slug).
    // Node IDs have already been rewritten by answer_questions before reaching here.
    let candidate_map: HashMap<String, &PyramidNode> =
        candidate_nodes.iter().map(|n| (n.id.clone(), n)).collect();

    // ── Build candidate evidence context ────────────────────────────────
    let evidence_context = if candidate_nodes.is_empty() {
        "(no candidate evidence nodes were mapped to this question)".to_string()
    } else {
        candidate_nodes
            .iter()
            .map(|n| {
                format!(
                    "--- NODE {} ---\nHeadline: {}\nDistilled: {}\nTopics: {}\n",
                    n.id,
                    n.headline,
                    n.distilled,
                    n.topics
                        .iter()
                        .map(|t| format!("{}: {}", t.name, t.current))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // ── Prompts ─────────────────────────────────────────────────────────
    let synthesis_guidance = synthesis_prompt.unwrap_or("");

    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "You are writing for {aud}. ALL technical terms from the evidence MUST be translated to plain language.\nThe reader should NEVER encounter framework names, file names, function names, API terms, or programming concepts unless they specifically asked about development.\nExtract the USER-FACING MEANING from technical evidence and express THAT.\n\n"
        ),
        _ => String::new(),
    };

    let content_type_block = match source_content_type {
        Some(ct) if !ct.is_empty() => format!("The source material is \"{ct}\" content.\n"),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/answer.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("synthesis_prompt", synthesis_guidance),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("answer.md not found — using inline fallback");
            format!(
                r#"{audience_block}You are answering a knowledge pyramid question using candidate evidence from the layer below.

For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is relevant. Weight 0.0-1.0 indicates how central it is.
- DISCONNECT(reason) — this evidence was a false positive from pre-mapping, not actually relevant.
- MISSING(description) — describe evidence you wish you had but don't.

Then synthesize your answer to the question using ONLY the KEEP evidence.

Focus your synthesis on your STRONGEST evidence — the nodes that most directly answer the question.
You do not need to mention every KEEP node. A focused answer drawing from your best sources is better than a sprawling answer trying to mention everything.

{synthesis_guidance}

{content_type_block}

Respond with ONLY a JSON object:
{{{{
  "headline": "short headline for this answer (max 120 chars)",
  "distilled": "2-4 sentence synthesis answering the question",
  "topics": [
    {{{{"name": "topic_name", "current": "what we know about this topic"}}}}
  ],
  "verdicts": [
    {{{{"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."}}}},
    {{{{"node_id": "...", "verdict": "DISCONNECT", "reason": "..."}}}},
    {{{{"node_id": "...", "verdict": "KEEP", "weight": 0.3, "reason": "..."}}}}
  ],
  "missing": [
    "description of evidence we wish we had"
  ],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}}}}"#
            )
        }
    };

    let user_prompt = format!(
        "QUESTION (id: {}):\n{}\n\nAbout: {}\nCreates: {}\n\nCANDIDATE EVIDENCE:\n{}\n\nEvaluate each candidate, produce verdicts, and synthesize your answer.",
        question.question_id,
        question.question_text,
        question.about,
        question.creates,
        evidence_context
    );

    // ── LLM call ────────────────────────────────────────────────────────
    let response = llm::call_model_unified(
        llm_config,
        &system_prompt,
        &user_prompt,
        answer_temperature,
        answer_max_tokens,
        None,
    )
    .await?;

    info!(
        question_id = %question.question_id,
        candidates = candidate_nodes.len(),
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        "question answering LLM call complete"
    );

    // ── Parse response ──────────────────────────────────────────────────
    let json_value = llm::extract_json(&response.content)?;
    let raw: RawAnswerResponse = serde_json::from_value(json_value).map_err(|e| {
        anyhow!(
            "Failed to parse answer response for {}: {} — raw: {}",
            question.question_id,
            e,
            &response.content[..response.content.len().min(400)]
        )
    })?;

    // ── Build PyramidNode ───────────────────────────────────────────────
    let topics = raw
        .topics
        .into_iter()
        .map(|t| super::types::Topic {
            name: t.name,
            current: t.current,
            entities: Vec::new(),
            corrections: Vec::new(),
            decisions: Vec::new(),
            extra: serde_json::Map::new(),
        })
        .collect();

    let corrections = raw
        .corrections
        .unwrap_or_default()
        .into_iter()
        .map(|c| super::types::Correction {
            wrong: c.wrong,
            right: c.right,
            who: c.who.unwrap_or_default(),
        })
        .collect();

    let decisions = raw
        .decisions
        .unwrap_or_default()
        .into_iter()
        .map(|d| super::types::Decision {
            decided: d.decided,
            why: d.why,
            rejected: d.rejected.unwrap_or_default(),
        })
        .collect();

    let terms = raw
        .terms
        .unwrap_or_default()
        .into_iter()
        .map(|t| super::types::Term {
            term: t.term,
            definition: t.definition,
        })
        .collect();

    // ── Build EvidenceLinks (KEEP and DISCONNECT only) ────────────────
    // MISSING verdicts are NOT evidence links — they have fabricated source_node_ids.
    // Missing evidence is captured via raw.missing and saved as gap reports by the caller.
    //
    // Resolve verdict node_ids against the candidate_map. The LLM sees handle-path IDs
    // for cross-slug candidates, so it should return them. If a verdict references an
    // unknown ID, skip it with a warning.
    let mut evidence: Vec<EvidenceLink> = Vec::new();
    let mut children: Vec<String> = Vec::new();

    for v in &raw.verdicts {
        let verdict = match v.verdict.to_uppercase().as_str() {
            "KEEP" => EvidenceVerdict::Keep,
            "DISCONNECT" => EvidenceVerdict::Disconnect,
            "MISSING" => continue, // Skip — tracked via raw.missing gap reports
            other => {
                warn!(verdict = other, "Unknown verdict, defaulting to Keep");
                EvidenceVerdict::Keep
            }
        };

        // Resolve against candidate_map — ensures we only accept IDs we showed the LLM
        if !candidate_map.contains_key(&v.node_id) {
            warn!(
                node_id = %v.node_id,
                question_id = %question.question_id,
                "LLM returned unknown node_id, skipping"
            );
            continue;
        }

        // Use the candidate_map key as source_node_id (handle-path for cross-slug, bare for same-slug)
        let source_node_id = v.node_id.clone();

        if verdict == EvidenceVerdict::Keep {
            children.push(source_node_id.clone());
        }

        let weight = if verdict == EvidenceVerdict::Keep {
            Some(v.weight.unwrap_or(0.5).clamp(0.0, 1.0))
        } else {
            None
        };

        evidence.push(EvidenceLink {
            slug: answer_slug.to_string(),
            source_node_id,
            target_node_id: node_id.clone(),
            verdict,
            weight,
            reason: v.reason.clone(),
            build_id: None,
            live: Some(true),
        });
    }

    let node = PyramidNode {
        id: node_id.clone(),
        slug: answer_slug.to_string(),
        depth: question.layer,
        chunk_index: None,
        headline: raw.headline,
        distilled: raw.distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends: raw.dead_ends.unwrap_or_default(),
        self_prompt: question.question_text.clone(),
        children,
        parent_id: None,
        superseded_by: None,
        build_id: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let missing = raw.missing.unwrap_or_default();

    Ok(AnsweredNode {
        node,
        evidence,
        missing,
    })
}

// ── Raw LLM Response Types (internal) ────────────────────────────────────────

#[derive(Deserialize)]
struct RawAnswerResponse {
    headline: String,
    distilled: String,
    #[serde(default)]
    topics: Vec<RawTopic>,
    verdicts: Vec<RawVerdict>,
    #[serde(default)]
    missing: Option<Vec<String>>,
    #[serde(default)]
    corrections: Option<Vec<RawCorrection>>,
    #[serde(default)]
    decisions: Option<Vec<RawDecision>>,
    #[serde(default)]
    terms: Option<Vec<RawTerm>>,
    #[serde(default)]
    dead_ends: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawTopic {
    name: String,
    current: String,
}

#[derive(Deserialize)]
struct RawVerdict {
    node_id: String,
    verdict: String,
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct RawCorrection {
    wrong: String,
    right: String,
    #[serde(default)]
    who: Option<String>,
}

#[derive(Deserialize)]
struct RawDecision {
    decided: String,
    why: String,
    #[serde(default)]
    rejected: Option<String>,
}

#[derive(Deserialize)]
struct RawTerm {
    term: String,
    definition: String,
}

// ── Targeted Re-examination (WS-2B) ────────────────────────────────────────

/// Raw LLM response for targeted extraction.
#[derive(Deserialize)]
struct RawTargetedExtraction {
    #[serde(default)]
    extractions: Vec<RawTargetedEntry>,
}

#[derive(Deserialize)]
struct RawTargetedEntry {
    headline: String,
    distilled: String,
    #[serde(default)]
    topics: Vec<RawTopic>,
}

/// Re-examine source files through the lens of a specific gap.
///
/// Loads the targeted_extract.md prompt and calls the LLM for each source file
/// to extract evidence specifically relevant to the question and gap. Returns
/// new L0 PyramidNodes with non-empty self_prompt (targeted evidence).
pub async fn targeted_reexamination(
    question_text: &str,
    gap_description: &str,
    source_candidates: &[(String, String)], // (file_path, content)
    llm_config: &LlmConfig,
    target_slug: &str,
    build_id: &str,
    audience: Option<&str>,
    chains_dir: Option<&PathBuf>,
    ops: &OperationalConfig,
) -> Result<Vec<PyramidNode>> {
    if source_candidates.is_empty() {
        return Ok(Vec::new());
    }

    // ── Build template variables ────────────────────────────────────────
    let audience_block = match audience {
        Some(aud) if !aud.is_empty() => format!(
            "You are writing for {aud}. Translate technical evidence into plain language.\n\n"
        ),
        _ => String::new(),
    };

    let content_type_block = String::new(); // targeted extraction is content-type agnostic

    // ── Load prompt template ────────────────────────────────────────────
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/targeted_extract.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[
                ("audience_block", &audience_block),
                ("question_text", question_text),
                ("gap_description", gap_description),
                ("content_type_block", &content_type_block),
            ],
        ),
        None => {
            warn!("targeted_extract.md not found — using inline fallback");
            format!(
                r#"{audience_block}You are performing a TARGETED re-examination of a source file. This file was already extracted generically, but a specific question needed evidence that the generic extraction didn't capture.

THE QUESTION: {question_text}

WHAT WAS MISSING: {gap_description}

Your job: read this source file through the lens of the question above. Extract ONLY information relevant to answering that question. Do not repeat what a generic extraction would capture — focus on the specific evidence the question needs.

Be precise and specific. Names, values, relationships, mechanisms. Not summaries or overviews.

Respond with ONLY a JSON object:
{{{{
  "extractions": [
    {{{{
      "headline": "short headline describing this piece of evidence",
      "distilled": "detailed extraction — the specific evidence relevant to the question",
      "topics": [
        {{{{"name": "topic_name", "current": "what this extraction reveals about this topic"}}}}
      ]
    }}}}
  ]
}}}}"#
            )
        }
    };

    // ── Process each source file ────────────────────────────────────────
    let mut all_nodes = Vec::new();

    for (file_path, content) in source_candidates {
        let user_prompt = format!(
            "SOURCE FILE: {}\n\n{}",
            file_path,
            content
        );

        let response = match llm::call_model_unified(
            llm_config,
            &system_prompt,
            &user_prompt,
            ops.tier1.answer_temperature,
            ops.tier1.answer_max_tokens,
            None,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    file_path = %file_path,
                    error = %e,
                    "targeted extraction LLM call failed, skipping file"
                );
                continue;
            }
        };

        info!(
            file_path = %file_path,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "targeted extraction LLM call complete"
        );

        // ── Parse response ──────────────────────────────────────────────
        let json_value = match llm::extract_json(&response.content) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    file_path = %file_path,
                    error = %e,
                    "targeted extraction JSON parse failed, skipping file"
                );
                continue;
            }
        };

        let raw: RawTargetedExtraction = match serde_json::from_value(json_value) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    file_path = %file_path,
                    error = %e,
                    "targeted extraction deserialization failed, skipping file"
                );
                continue;
            }
        };

        // ── Create PyramidNodes for each extraction ─────────────────────
        for entry in raw.extractions {
            let topics = entry
                .topics
                .into_iter()
                .map(|t| super::types::Topic {
                    name: t.name,
                    current: t.current,
                    entities: Vec::new(),
                    corrections: Vec::new(),
                    decisions: Vec::new(),
                    extra: serde_json::Map::new(),
                })
                .collect();

            let node = PyramidNode {
                id: format!("L0-{}", Uuid::new_v4()),
                slug: target_slug.to_string(),
                depth: 0,
                chunk_index: None,
                headline: entry.headline,
                distilled: entry.distilled,
                topics,
                corrections: Vec::new(),
                decisions: Vec::new(),
                terms: Vec::new(),
                dead_ends: Vec::new(),
                self_prompt: question_text.to_string(), // MUST be non-empty
                children: Vec::new(),
                parent_id: None,
                superseded_by: None,
                build_id: Some(build_id.to_string()),
                created_at: chrono::Utc::now().to_rfc3339(),
            };

            all_nodes.push(node);
        }
    }

    info!(
        question = %question_text,
        gap = %gap_description,
        source_files = source_candidates.len(),
        new_nodes = all_nodes.len(),
        "targeted re-examination complete"
    );

    Ok(all_nodes)
}

// ── Gap File Resolution (WS-2B) ────────────────────────────────────────────

/// Resolve source files that might contain evidence for a gap.
///
/// Rule-based (NO LLM): tokenizes the gap description into keywords, scores
/// canonical L0 nodes by keyword overlap, then looks up the top-scoring nodes'
/// source file paths via pyramid_file_hashes.
///
/// Returns (base_slug, file_path, content) triples.
pub fn resolve_files_for_gap(
    conn: &rusqlite::Connection,
    base_slugs: &[String],
    gap_description: &str,
    _existing_l0_nodes: &[PyramidNode],
    max_files: usize,
) -> Result<Vec<(String, String, String)>> {
    // ── 1. Tokenize gap description into keywords ───────────────────────
    let keywords: Vec<String> = gap_description
        .split_whitespace()
        .map(|w| w.to_lowercase().replace(|c: char| !c.is_alphanumeric(), ""))
        .filter(|w| w.len() >= 3)
        .collect();

    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored_nodes: Vec<(String, String, usize)> = Vec::new(); // (slug, node_id, score)

    // ── 2. For each base slug, get canonical L0 nodes ──
    // Canonical L0 nodes are from the original extraction (C-L0-*, D-L0-*, or short index IDs).
    // Targeted evidence L0 nodes (from gap re-examination) use L0-{uuid} format (long UUID).
    // We include ALL L0 that are NOT targeted evidence — self_prompt is NOT a reliable
    // discriminator because canonical nodes also have self_prompt populated (orientation text).
    for base_slug in base_slugs {
        let all_l0 = db::get_nodes_at_depth(conn, base_slug, 0)?;
        let canonical: Vec<&PyramidNode> = all_l0
            .iter()
            .filter(|n| !n.id.starts_with("ES-") && !is_targeted_l0_id(&n.id))
            .collect();

        // ── 3. Score each by keyword overlap ────────────────────────────
        for node in &canonical {
            let text = format!("{} {}", node.headline, node.distilled).to_lowercase();
            let score = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            if score > 0 {
                scored_nodes.push((base_slug.clone(), node.id.clone(), score));
            }
        }
    }

    // ── 4. Sort by score descending, take top N ────────────────────────
    scored_nodes.sort_by(|a, b| b.2.cmp(&a.2));
    scored_nodes.truncate(max_files);

    // ── 5. Look up file paths and read content ─────────────────────────
    let mut results = Vec::new();

    for (slug, node_id, _score) in &scored_nodes {
        // Find file_path from pyramid_file_hashes where node_ids contains this node
        let mut stmt = conn.prepare(
            "SELECT file_path FROM pyramid_file_hashes
             WHERE slug = ?1 AND EXISTS (SELECT 1 FROM json_each(node_ids) WHERE value = ?2)
             LIMIT 1",
        )?;
        let file_path: Option<String> = stmt
            .query_row(rusqlite::params![slug, node_id], |row| row.get(0))
            .ok();

        if let Some(path) = file_path {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    results.push((slug.clone(), path, content));
                }
                Err(e) => {
                    warn!(
                        file_path = %path,
                        error = %e,
                        "failed to read source file for gap resolution, skipping"
                    );
                }
            }
        }
    }

    info!(
        gap = %gap_description,
        keywords = keywords.len(),
        candidates_scored = scored_nodes.len(),
        files_resolved = results.len(),
        "gap file resolution complete"
    );

    Ok(results)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_answer_response() {
        let json = r#"{
            "headline": "Auth flow overview",
            "distilled": "The system uses JWT tokens with refresh rotation.",
            "topics": [
                {"name": "auth", "current": "JWT-based with refresh tokens"}
            ],
            "verdicts": [
                {"node_id": "node-1", "verdict": "KEEP", "weight": 0.9, "reason": "Core auth implementation"},
                {"node_id": "node-2", "verdict": "DISCONNECT", "reason": "Unrelated to auth"},
                {"node_id": "node-3", "verdict": "KEEP", "weight": 0.4, "reason": "Tangential error handling"}
            ],
            "missing": ["OAuth2 provider configuration details"],
            "corrections": [],
            "decisions": [{"decided": "Use JWT", "why": "Stateless", "rejected": "Sessions"}],
            "terms": [{"term": "JWT", "definition": "JSON Web Token"}],
            "dead_ends": []
        }"#;

        let raw: RawAnswerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.headline, "Auth flow overview");
        assert_eq!(raw.verdicts.len(), 3);
        assert_eq!(raw.verdicts[0].verdict, "KEEP");
        assert_eq!(raw.verdicts[0].weight, Some(0.9));
        assert_eq!(raw.verdicts[1].verdict, "DISCONNECT");
        assert_eq!(raw.missing.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parse_raw_answer_response_minimal() {
        let json = r#"{
            "headline": "Minimal answer",
            "distilled": "Not much evidence available.",
            "topics": [],
            "verdicts": [],
            "missing": ["Everything"]
        }"#;

        let raw: RawAnswerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.headline, "Minimal answer");
        assert!(raw.verdicts.is_empty());
    }

    #[test]
    fn parse_pre_map_response() {
        let json = r#"{"mappings": {"q1": ["n1", "n2"], "q2": ["n3"]}}"#;
        let raw: PreMapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.mappings.len(), 2);
        assert_eq!(raw.mappings["q1"].len(), 2);
    }

    #[test]
    fn verdict_weight_clamping() {
        // Verify weight clamping logic
        let weight = Some(1.5_f64);
        let clamped = weight.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped, 1.0);

        let weight_neg = Some(-0.3_f64);
        let clamped_neg = weight_neg.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped_neg, 0.0);

        let weight_none: Option<f64> = None;
        let clamped_none = weight_none.unwrap_or(0.5).clamp(0.0, 1.0);
        assert_eq!(clamped_none, 0.5);
    }
}
