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
use rusqlite::Connection;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use super::db;
use super::llm::{self, LlmConfig};
use super::types::{
    AnsweredNode, CandidateMap, EvidenceLink, EvidenceVerdict, GapReport, LayerQuestion,
    PyramidNode,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Max concurrency for parallel question answering.
const ANSWER_CONCURRENCY: usize = 5;

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

    // ── Build node listing (headline + distilled only, not full content) ─
    let nodes_text = lower_layer_nodes
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

    // ── Prompts ─────────────────────────────────────────────────────────
    let system_prompt = r#"You are mapping questions to candidate evidence nodes. Your job is to determine which nodes from the layer below MIGHT contain relevant evidence for each question.

IMPORTANT: Over-include rather than miss. If a node MIGHT be relevant, include it. The next step will prune irrelevant candidates — a false positive here costs little, but a miss loses evidence permanently.

Respond with ONLY a JSON object in this exact format:
{
  "mappings": {
    "question_id_1": ["node_id_a", "node_id_b"],
    "question_id_2": ["node_id_c"],
    ...
  }
}

Every question_id from the input MUST appear as a key in the mappings, even if its candidate list is empty."#;

    let user_prompt = format!(
        "QUESTIONS for this layer:\n{}\n\nNODES from the layer below (candidate evidence):\n{}\n\nFor each question, identify which nodes likely contain relevant evidence. Include uncertain matches.",
        questions_text, nodes_text
    );

    // ── LLM call (mercury-2 / primary model — fast classification) ──────
    let response = llm::call_model_unified(
        llm_config,
        system_prompt,
        &user_prompt,
        0.2, // low temperature for classification
        4096,
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
///
/// Parallel, 5x concurrency via tokio::sync::Semaphore.
pub async fn answer_questions(
    questions: &[LayerQuestion],
    candidate_map: &CandidateMap,
    all_nodes: &[PyramidNode],
    synthesis_prompt: Option<&str>,
    llm_config: &LlmConfig,
    conn: &Connection,
    slug: &str,
) -> Result<Vec<AnsweredNode>> {
    if questions.is_empty() {
        return Ok(Vec::new());
    }

    // Build a lookup map for all nodes by ID
    let node_map: HashMap<&str, &PyramidNode> =
        all_nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let semaphore = Arc::new(Semaphore::new(ANSWER_CONCURRENCY));
    let llm_config = Arc::new(llm_config.clone());
    let slug = slug.to_string();
    let synthesis_prompt = synthesis_prompt.map(|s| s.to_string());

    // Prepare per-question work items
    let work_items: Vec<AnswerWorkItem> = questions
        .iter()
        .map(|q| {
            let candidate_ids = candidate_map
                .mappings
                .get(&q.question_id)
                .cloned()
                .unwrap_or_default();

            // Resolve candidate IDs to full node data
            let candidate_nodes: Vec<PyramidNode> = candidate_ids
                .iter()
                .filter_map(|id| node_map.get(id.as_str()).map(|n| (*n).clone()))
                .collect();

            AnswerWorkItem {
                question: q.clone(),
                candidate_nodes,
            }
        })
        .collect();

    // Spawn parallel tasks
    let mut handles = Vec::new();
    for work in work_items {
        let semaphore = semaphore.clone();
        let llm_config = llm_config.clone();
        let slug = slug.clone();
        let synthesis_prompt = synthesis_prompt.clone();

        let handle = tokio::spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("answer semaphore should remain open");

            answer_single_question(&work.question, &work.candidate_nodes, synthesis_prompt.as_deref(), &llm_config, &slug)
                .await
        });

        handles.push(handle);
    }

    // Collect results — all DB writes happen here sequentially (not in spawned tasks)
    // because rusqlite::Connection is !Send. The parallel LLM work is done; now we
    // persist results synchronously using the borrowed connection.
    let mut answered_nodes = Vec::new();
    let mut total_evidence = 0usize;
    let mut total_missing = 0usize;

    for handle in handles {
        let result = handle.await.map_err(|e| anyhow!("Answer task panicked: {}", e))?;
        match result {
            Ok(answered) => {
                // Save evidence links to DB (synchronous, no mutex needed)
                for link in &answered.evidence {
                    if let Err(e) = db::save_evidence_link(conn, link) {
                        warn!(
                            source = %link.source_node_id,
                            target = %link.target_node_id,
                            "Failed to save evidence link: {}",
                            e
                        );
                    }
                }
                // Save the answered node to pyramid_nodes
                if let Err(e) = db::save_node(conn, &answered.node, None) {
                    warn!(
                        node_id = %answered.node.id,
                        "Failed to save answered node: {}",
                        e
                    );
                }
                // Save MISSING evidence as gap reports
                for desc in &answered.missing {
                    let gap = GapReport {
                        question_id: answered.node.self_prompt.clone(),
                        description: desc.clone(),
                        layer: answered.node.depth,
                    };
                    if let Err(e) = db::save_gap(conn, &slug, &gap) {
                        warn!(
                            node_id = %answered.node.id,
                            "Failed to save gap report: {}",
                            e
                        );
                    }
                }

                total_evidence += answered.evidence.len();
                total_missing += answered.missing.len();
                answered_nodes.push(answered);
            }
            Err(e) => {
                warn!("Failed to answer question: {}", e);
                // Continue with other questions rather than failing the whole batch
            }
        }
    }

    info!(
        answered = answered_nodes.len(),
        total_evidence,
        total_missing,
        "answering complete"
    );

    Ok(answered_nodes)
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
    llm_config: &LlmConfig,
    slug: &str,
) -> Result<AnsweredNode> {
    let node_id = format!("L{}-{}", question.layer, Uuid::new_v4());

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
    let extra_guidance = synthesis_prompt.unwrap_or("");

    let system_prompt = format!(
        r#"You are answering a knowledge pyramid question using candidate evidence from the layer below.

For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is relevant. Weight 0.0-1.0 indicates how central it is.
- DISCONNECT(reason) — this evidence was a false positive from pre-mapping, not actually relevant.
- MISSING(description) — describe evidence you wish you had but don't.

Then synthesize your answer to the question using ONLY the KEEP evidence.

{extra_guidance}

Respond with ONLY a JSON object:
{{
  "headline": "short headline for this answer (max 120 chars)",
  "distilled": "2-4 sentence synthesis answering the question",
  "topics": [
    {{"name": "topic_name", "current": "what we know about this topic"}}
  ],
  "verdicts": [
    {{"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."}},
    {{"node_id": "...", "verdict": "DISCONNECT", "reason": "..."}},
    {{"node_id": "...", "verdict": "KEEP", "weight": 0.3, "reason": "..."}}
  ],
  "missing": [
    "description of evidence we wish we had"
  ],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}}"#
    );

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
        0.3,
        4096,
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

    // Children are the KEEP evidence nodes
    let children: Vec<String> = raw
        .verdicts
        .iter()
        .filter(|v| v.verdict.eq_ignore_ascii_case("KEEP"))
        .map(|v| v.node_id.clone())
        .collect();

    let node = PyramidNode {
        id: node_id.clone(),
        slug: slug.to_string(),
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
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    // ── Build EvidenceLinks (KEEP and DISCONNECT only) ────────────────
    // MISSING verdicts are NOT evidence links — they have fabricated source_node_ids.
    // Missing evidence is captured via raw.missing and saved as gap reports by the caller.
    let mut evidence: Vec<EvidenceLink> = Vec::new();
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

        let weight = if verdict == EvidenceVerdict::Keep {
            Some(v.weight.unwrap_or(0.5).clamp(0.0, 1.0))
        } else {
            None
        };

        evidence.push(EvidenceLink {
            slug: slug.to_string(),
            source_node_id: v.node_id.clone(),
            target_node_id: node_id.clone(),
            verdict,
            weight,
            reason: v.reason.clone(),
        });
    }

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
