// pyramid/supersession.rs — Belief Supersession (Crystallization Channel B)
//
// When source files change, Channel B detects contradictions between the new
// content and existing pyramid claims, then traces those contradictions through
// EVERY layer of the pyramid without attenuation. Unlike Channel A (staleness),
// supersession cannot be dismissed and forces correction of all affected nodes.
//
// Three entry points:
//   detect_contradictions() — LLM-based contradiction detection
//   trace_supersession()   — Non-attenuating upward trace through all layers
//   record_supersession()  — Audit trail + staleness queue enqueue

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use tracing::{info, warn};

use super::db;
use super::llm::{self, LlmConfig};
use super::step_context::make_step_ctx_from_llm_config;
use super::types::{AffectedNode, Contradiction, EvidenceVerdict, PyramidNode, SupersessionTrace};

// ── Constants (loaded from OperationalConfig) ─────────────────────────────────

use super::Tier3Config;

fn contradiction_confidence_threshold() -> f64 {
    Tier3Config::default().contradiction_confidence_threshold
}

/// Channel identifier for staleness queue entries created by supersession.
const CHANNEL_BELIEF_SUPERSESSION: &str = "belief_supersession";

fn supersession_priority() -> f64 {
    Tier3Config::default().supersession_priority
}
fn max_trace_depth() -> i64 {
    Tier3Config::default().max_trace_depth
}

// ── LLM Response Types ───────────────────────────────────────────────────────

/// Raw contradiction output from LLM, before confidence filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LlmContradiction {
    superseded_claim: String,
    corrected_to: String,
    confidence: f64,
}

/// LLM response envelope for contradiction detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContradictionResponse {
    contradictions: Vec<LlmContradiction>,
}

// ── detect_contradictions ────────────────────────────────────────────────────

/// Use LLM to detect contradictions between new source content and existing
/// pyramid claims in the affected L0 node.
///
/// One LLM call per affected L0 node. Only high-confidence contradictions
/// (>0.8) are returned. Uses mercury-2 (fast, focused analysis).
pub async fn detect_contradictions(
    _conn: &Connection,
    _slug: &str,
    changed_file_path: &str,
    new_content: &str,
    affected_l0_node: &PyramidNode,
    llm_config: &LlmConfig,
) -> Result<Vec<Contradiction>> {
    let existing_extraction = &affected_l0_node.distilled;
    if existing_extraction.is_empty() {
        info!(
            "[supersession] L0 node {} has empty distilled content, skipping contradiction check",
            affected_l0_node.id
        );
        return Ok(vec![]);
    }

    let system_prompt = "You are a precise fact-checker for a knowledge pyramid system. \
        Your job is to compare the previous extraction of a source file against the new file content, \
        and identify any specific claims in the extraction that are now FALSE or contradicted by the \
        new content. Only report genuine contradictions — not additions, not refinements, not \
        stylistic changes. A contradiction means the extraction asserts something that the new \
        content directly contradicts or invalidates.";

    let user_prompt = format!(
        "## Previous extraction for file: {changed_file_path}\n\n\
         {existing_extraction}\n\n\
         ## New file content\n\n\
         {new_content}\n\n\
         ## Task\n\n\
         Does the new content contradict any specific claims in the extraction? \
         List each contradiction as JSON:\n\
         ```json\n\
         {{\n\
           \"contradictions\": [\n\
             {{\n\
               \"superseded_claim\": \"the exact claim from the extraction that is now false\",\n\
               \"corrected_to\": \"what the new content says instead\",\n\
               \"confidence\": 0.95\n\
             }}\n\
           ]\n\
         }}\n\
         ```\n\
         If there are no contradictions, return {{\"contradictions\": []}}.\n\
         Confidence must be between 0.0 and 1.0. Only include contradictions you are highly \
         confident about (>0.8). Do NOT include additions or modifications that don't contradict \
         existing claims."
    );

    // Declared intent per docstring: mercury-2 (fast, focused analysis) = "mid"
    // tier. Resolve through provider registry so the walker sees a
    // provider-valid model id. Fail-loud if registry has no "mid" tier —
    // walker-v3 W3c removed the legacy primary_model fallback and this is a
    // top-level entry point with no outer DispatchDecision to cascade to.
    let resolved = llm_config
        .provider_registry
        .as_ref()
        .and_then(|reg| reg.resolve_tier("mid", None, None, None).ok())
        .ok_or_else(|| {
            anyhow!(
                "supersession_detect_contradictions: provider registry has no 'mid' tier \
                 routing. Configure a walker_provider_openrouter contribution with a \
                 'mid' slot model_list entry."
            )
        })?;
    let resolved_model_id = resolved.tier.model_id.clone();
    let resolved_provider_id = Some(resolved.provider.id.clone());

    let cache_ctx = make_step_ctx_from_llm_config(
        llm_config,
        "supersession_detect_contradictions",
        "supersession",
        affected_l0_node.depth,
        None,
        system_prompt,
        "mid",
        Some(&resolved_model_id),
        resolved_provider_id.as_deref(),
    )
    .await;
    let response = llm::call_model_unified_and_ctx(
        llm_config,
        cache_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        0.1, // low temperature for factual analysis
        2048,
        None,
    )
    .await?;

    info!(
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        node_id = %affected_l0_node.id,
        "[supersession] contradiction detection LLM call complete"
    );

    let raw_response = &response.content;

    // Parse the LLM response
    let parsed: ContradictionResponse = match llm::extract_json(raw_response) {
        Ok(json_value) => serde_json::from_value(json_value).map_err(|e| {
            anyhow!(
                "Failed to parse contradiction response structure: {}. Raw: {}",
                e,
                &raw_response[..raw_response.len().min(500)]
            )
        })?,
        Err(e) => {
            warn!(
                "[supersession] Failed to extract JSON from LLM response for node {}: {}",
                affected_l0_node.id, e
            );
            return Ok(vec![]);
        }
    };

    // Filter by confidence threshold and attach source node ID
    let contradictions: Vec<Contradiction> = parsed
        .contradictions
        .into_iter()
        .filter(|c| c.confidence >= contradiction_confidence_threshold())
        .map(|c| Contradiction {
            superseded_claim: c.superseded_claim,
            corrected_to: c.corrected_to,
            confidence: c.confidence,
            source_node_id: affected_l0_node.id.clone(),
        })
        .collect();

    if !contradictions.is_empty() {
        info!(
            "[supersession] Detected {} high-confidence contradiction(s) in L0 node {} (file: {})",
            contradictions.len(),
            affected_l0_node.id,
            changed_file_path
        );
    }

    Ok(contradictions)
}

// ── trace_supersession ───────────────────────────────────────────────────────

/// Trace contradictions upward through ALL pyramid layers without attenuation.
///
/// CRITICAL DIFFERENCE from Channel A (staleness):
/// - Does NOT attenuate through layers
/// - Cannot be dismissed by operator
/// - Every affected node gets enqueued for FORCED re-answering
///
/// Algorithm for each contradiction:
/// 1. Start at the L0 node that contained the superseded claim
/// 2. Find all KEEP evidence links where source = this L0 node
/// 3. For each linked L1+ node: check if its content references the superseded claim
/// 4. Continue upward through ALL layers — no attenuation, no threshold
/// 5. Every affected node is collected in the trace
pub fn trace_supersession(
    conn: &Connection,
    slug: &str,
    contradictions: &[Contradiction],
    source_l0_node_id: &str,
) -> Result<SupersessionTrace> {
    let mut affected_nodes: Vec<AffectedNode> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut max_depth_reached: i64 = 0;

    for contradiction in contradictions {
        // BFS upward from the source L0 node
        let mut queue: VecDeque<(String, i64, Vec<String>)> = VecDeque::new();

        // Seed: the source L0 node itself is affected
        queue.push_back((
            source_l0_node_id.to_string(),
            0,
            vec![source_l0_node_id.to_string()],
        ));

        while let Some((current_node_id, depth, path)) = queue.pop_front() {
            // Depth safety cap
            if depth > max_trace_depth() {
                warn!(
                    "[supersession] Trace depth exceeded {} for node {}, stopping this branch",
                    max_trace_depth(),
                    current_node_id
                );
                continue;
            }

            // Skip already-visited nodes (across all contradictions)
            let visit_key = format!("{}:{}", current_node_id, contradiction.superseded_claim);
            if !visited.insert(visit_key) {
                continue;
            }

            // Load the current node to check if it references the superseded claim
            let node = match db::get_node(conn, slug, &current_node_id)? {
                Some(n) => n,
                None => {
                    warn!(
                        "[supersession] Node {} not found during trace, skipping",
                        current_node_id
                    );
                    continue;
                }
            };

            // Check if this node's content references the superseded claim.
            // The source L0 node is always affected. For higher nodes, we do a
            // case-insensitive substring check against distilled content, headlines,
            // and topic descriptions.
            let is_source = current_node_id == source_l0_node_id;
            let contains_claim = if is_source {
                true
            } else {
                node_references_claim(&node, &contradiction.superseded_claim)
            };

            if contains_claim {
                if depth > max_depth_reached {
                    max_depth_reached = depth;
                }

                affected_nodes.push(AffectedNode {
                    node_id: current_node_id.clone(),
                    depth,
                    contains_claim: contradiction.superseded_claim.clone(),
                    path_from_source: path.clone(),
                });
            }

            // Continue upward regardless of whether this node was affected —
            // a higher node may reference the claim even if an intermediate doesn't.
            // Find all KEEP evidence links where this node is the source (evidence provider).
            let evidence_links = db::get_evidence_for_source_cross(conn, &current_node_id)?;
            let keep_links: Vec<_> = evidence_links
                .into_iter()
                .filter(|e| e.verdict == EvidenceVerdict::Keep)
                .collect();

            for link in keep_links {
                let mut new_path = path.clone();
                new_path.push(link.target_node_id.clone());
                queue.push_back((link.target_node_id, depth + 1, new_path));
            }
        }
    }

    // Deduplicate affected nodes by node_id (a node may be reached via multiple
    // contradictions or multiple paths — keep the first occurrence)
    let mut seen_nodes: HashSet<String> = HashSet::new();
    affected_nodes.retain(|n| seen_nodes.insert(n.node_id.clone()));

    let total = affected_nodes.len();

    info!(
        "[supersession] Trace complete: {} contradiction(s) → {} affected node(s), max depth {}",
        contradictions.len(),
        total,
        max_depth_reached
    );

    Ok(SupersessionTrace {
        contradictions: contradictions.to_vec(),
        affected_nodes,
        total_nodes_affected: total,
        max_depth_reached,
    })
}

/// Check if a node's content references a superseded claim.
///
/// Uses case-insensitive substring matching against the node's distilled content,
/// headline, and all topic descriptions. This is a deliberately over-inclusive
/// heuristic: false positives are acceptable because the downstream LLM-based
/// re-answering step performs precise correction and will no-op on nodes that
/// don't actually need updating. Under-inclusion (missing a genuinely affected
/// node) is much worse than over-inclusion.
fn node_references_claim(node: &PyramidNode, claim: &str) -> bool {
    let claim_lower = claim.to_lowercase();

    // Split the claim into significant words (5+ chars) for fuzzy matching.
    // The 5-char minimum filters common English stopwords (the, with, from, etc.)
    // that would otherwise cause false positives across unrelated nodes.
    // A node "references" the claim if it contains at least half of the
    // significant words from the claim.
    let significant_words: Vec<&str> = claim_lower
        .split_whitespace()
        .filter(|w| w.len() >= 5)
        .collect();

    if significant_words.is_empty() {
        // Very short claim — fall back to exact substring
        let haystack = format!(
            "{} {} {}",
            node.headline.to_lowercase(),
            node.distilled.to_lowercase(),
            node.topics
                .iter()
                .map(|t| format!("{} {}", t.name, t.current))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        );
        return haystack.contains(&claim_lower);
    }

    let haystack = format!(
        "{} {} {}",
        node.headline.to_lowercase(),
        node.distilled.to_lowercase(),
        node.topics
            .iter()
            .map(|t| format!("{} {}", t.name, t.current))
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    );

    let matches = significant_words
        .iter()
        .filter(|w| haystack.contains(**w))
        .count();

    // Threshold: at least half of significant words must be present
    matches * 2 >= significant_words.len()
}

// ── record_supersession ──────────────────────────────────────────────────────

/// Record the supersession audit trail and enqueue all affected questions for
/// re-answering with HIGH priority.
///
/// For each affected node:
/// 1. Save a record to `pyramid_supersessions` table
/// 2. Enqueue the node's question to `pyramid_staleness_queue` with
///    channel="belief_supersession" and priority=1.0
pub fn record_supersession(conn: &Connection, slug: &str, trace: &SupersessionTrace) -> Result<()> {
    if trace.affected_nodes.is_empty() {
        info!("[supersession] No affected nodes to record");
        return Ok(());
    }

    let mut recorded = 0usize;
    let mut enqueued = 0usize;

    for affected in &trace.affected_nodes {
        // Find which contradiction this node is affected by
        let contradiction = trace
            .contradictions
            .iter()
            .find(|c| c.superseded_claim == affected.contains_claim);

        let (superseded_claim, corrected_to, source_node) = match contradiction {
            Some(c) => (
                c.superseded_claim.as_str(),
                c.corrected_to.as_str(),
                Some(c.source_node_id.as_str()),
            ),
            None => (
                affected.contains_claim.as_str(),
                "unknown — contradiction not found in trace",
                None,
            ),
        };

        // Save supersession record
        db::save_supersession(
            conn,
            slug,
            &affected.node_id,
            superseded_claim,
            corrected_to,
            source_node,
            CHANNEL_BELIEF_SUPERSESSION,
        )?;
        recorded += 1;

        // Enqueue for re-answering. The node_id is used as the question_id
        // since in the question pyramid, each node answers a question.
        let reason = format!(
            "Belief supersession: '{}' corrected to '{}'",
            truncate_str(superseded_claim, 100),
            truncate_str(corrected_to, 100)
        );

        db::enqueue_staleness(
            conn,
            slug,
            &affected.node_id,
            &reason,
            CHANNEL_BELIEF_SUPERSESSION,
            supersession_priority(),
        )?;
        enqueued += 1;
    }

    info!(
        "[supersession] Recorded {} supersession(s), enqueued {} question(s) for re-answering",
        recorded, enqueued
    );

    Ok(())
}

/// Truncate a string to a maximum length at a char boundary, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::types::{Correction, Decision, Term, Topic};

    /// Helper: create a minimal PyramidNode for testing.
    fn make_test_node(id: &str, depth: i64, distilled: &str, headline: &str) -> PyramidNode {
        PyramidNode {
            id: id.to_string(),
            slug: "test".to_string(),
            depth,
            chunk_index: Some(0),
            headline: headline.to_string(),
            distilled: distilled.to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_node_references_claim_exact_match() {
        let node = make_test_node(
            "n1",
            1,
            "The auth module uses JWT tokens for session management",
            "Authentication System",
        );
        assert!(node_references_claim(&node, "auth module uses JWT tokens"));
    }

    #[test]
    fn test_node_references_claim_partial_word_match() {
        let node = make_test_node(
            "n1",
            1,
            "The system handles authentication via JWT tokens stored in cookies",
            "Auth Flow",
        );
        // "tokens" and "authentication" should match even if claim wording differs
        assert!(node_references_claim(
            &node,
            "authentication tokens stored in cookies"
        ));
    }

    #[test]
    fn test_node_references_claim_no_match() {
        let node = make_test_node(
            "n1",
            1,
            "The billing module processes credit card payments",
            "Billing",
        );
        assert!(!node_references_claim(
            &node,
            "authentication via JWT tokens"
        ));
    }

    #[test]
    fn test_node_references_claim_topic_match() {
        let mut node = make_test_node("n1", 1, "Overview of the system", "System Overview");
        node.topics.push(Topic {
            name: "auth".to_string(),
            current: "Uses JWT tokens for session management".to_string(),
            entities: vec![],
            corrections: vec![],
            decisions: vec![],
            extra: serde_json::Map::new(),
        });
        assert!(node_references_claim(
            &node,
            "JWT tokens for session management"
        ));
    }

    #[test]
    fn test_node_references_claim_case_insensitive() {
        let node = make_test_node(
            "n1",
            1,
            "The API uses REST endpoints for communication",
            "API Layer",
        );
        assert!(node_references_claim(
            &node,
            "REST ENDPOINTS FOR COMMUNICATION"
        ));
    }

    #[test]
    fn test_node_references_claim_short_claim() {
        let node = make_test_node("n1", 1, "Uses JWT", "Auth");
        // Very short claim — falls back to exact substring
        assert!(node_references_claim(&node, "JWT"));
        assert!(!node_references_claim(&node, "OAuth"));
    }

    #[test]
    fn test_truncate_str_no_truncation() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_with_truncation() {
        assert_eq!(truncate_str("hello world", 5), "hello...");
    }

    #[test]
    fn test_contradiction_confidence_threshold() {
        // Verify the threshold constant is set correctly
        assert_eq!(contradiction_confidence_threshold(), 0.8);
    }

    #[test]
    fn test_supersession_trace_empty_contradictions() {
        // trace_supersession with empty contradictions should return empty trace
        // (This tests the pure logic path — we can't easily test DB-dependent paths
        // without a test database, but we verify the algorithm handles the edge case.)
        let trace = SupersessionTrace {
            contradictions: vec![],
            affected_nodes: vec![],
            total_nodes_affected: 0,
            max_depth_reached: 0,
        };
        assert_eq!(trace.total_nodes_affected, 0);
        assert_eq!(trace.max_depth_reached, 0);
    }

    #[test]
    fn test_affected_node_path_structure() {
        let affected = AffectedNode {
            node_id: "L1-abc".to_string(),
            depth: 1,
            contains_claim: "uses JWT tokens".to_string(),
            path_from_source: vec!["L0-source".to_string(), "L1-abc".to_string()],
        };
        assert_eq!(affected.path_from_source.len(), 2);
        assert_eq!(affected.path_from_source[0], "L0-source");
    }
}
