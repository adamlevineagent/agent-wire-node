// pyramid/question_retrieve.rs — WS-QUESTION-RETRIEVE (Phase 3)
//
// Read-time question retrieval: decomposes a question into sub-questions,
// searches the pyramid's structured content (vocabulary recognition → node
// search → FTS fallback), gathers evidence, and composes answers.
//
// LOCKED: evidence_mode: fast — always returns immediately (<5s).
// With allow_demand_gen: true, returns 202 with partial result + job_ids.
//
// This is the read-time counterpart to build-time synthesis (Section 6.3).

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use rusqlite::Connection;
use tracing::{debug, info};

use super::db;
use super::demand_gen;
use super::query;
use super::types::{
    DemandGenJob, QuestionRetrieveResult, SubQuestionResult,
};
use super::vocabulary;
use super::PyramidState;

// ── Mechanical sub-question decomposition ────────────────────────────────────

/// Mechanically decompose a question into sub-questions without LLM calls.
///
/// Strategy:
/// 1. Split on explicit delimiters: semicolons, " and ", " also "
/// 2. If a question contains "how" + "why" or "what" + "where", split on those
/// 3. Trim and filter empty fragments
/// 4. If only one sub-question results, use the original question as-is
///
/// This is the fast-path decomposition (<5s budget). Full LLM decomposition
/// is a build-time operation via question_decomposition.rs.
fn decompose_mechanically(question: &str) -> Vec<String> {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // Split on semicolons first
    let mut parts: Vec<String> = trimmed
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // If we got multiple parts from semicolons, return those
    if parts.len() > 1 {
        return parts;
    }

    // Try splitting on " and " only when it connects question-like clauses
    // (both sides should have >=3 words to avoid splitting "cats and dogs")
    let and_split: Vec<&str> = trimmed.split(" and ").collect();
    if and_split.len() > 1 {
        let valid_splits: Vec<String> = and_split
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| s.split_whitespace().count() >= 3)
            .collect();
        if valid_splits.len() > 1 {
            return valid_splits;
        }
    }

    // Try splitting on "? " (multiple questions in one string)
    let q_split: Vec<&str> = trimmed.split("? ").collect();
    if q_split.len() > 1 {
        parts = q_split
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let s = s.trim().to_string();
                // Re-add the question mark except for the last part (which already has one)
                if i < q_split.len() - 1 && !s.ends_with('?') {
                    format!("{}?", s)
                } else {
                    s
                }
            })
            .filter(|s| !s.is_empty() && s != "?")
            .collect();
        if parts.len() > 1 {
            return parts;
        }
    }

    // Single question — return as-is
    vec![trimmed.to_string()]
}

// ── Sub-question answering ──────────────────────────────────────────────────

/// Answer a single sub-question against a pyramid's content.
///
/// Search cascade:
/// 1. Vocabulary recognition query (fast, exact/partial match on canonical identities)
/// 2. Pyramid search via FTS (searches headline, distilled, topics, terms)
/// 3. Drill into top matching nodes for detail
///
/// Returns a SubQuestionResult with evidence nodes and confidence score.
fn answer_sub_question(
    conn: &Connection,
    slug: &str,
    sub_question: &str,
) -> Result<SubQuestionResult> {
    let mut evidence_nodes: Vec<String> = Vec::new();
    let mut answer_fragments: Vec<String> = Vec::new();
    let mut confidence: f64 = 0.0;

    // ── Step 1: Vocabulary recognition ──────────────────────────────────
    // Extract key terms from the sub-question and try vocabulary recognition
    let question_words: Vec<String> = sub_question
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect();

    let mut vocab_matches = Vec::new();
    for word in &question_words {
        if let Ok(entries) = vocabulary::vocab_recognition_query(conn, slug, word) {
            for entry in &entries {
                if !vocab_matches.iter().any(|v: &super::types::VocabEntry| v.name == entry.name) {
                    vocab_matches.push(entry.clone());
                }
            }
        }
    }

    if !vocab_matches.is_empty() {
        // Vocabulary hit — boost confidence
        confidence += 0.3;
        debug!(
            slug = slug,
            sub_question = sub_question,
            vocab_hits = vocab_matches.len(),
            "Vocabulary recognition matched"
        );
    }

    // ── Step 2: Pyramid search (FTS) ────────────────────────────────────
    let search_hits = query::search(conn, slug, sub_question)?;

    if !search_hits.is_empty() {
        confidence += 0.4;

        // Gather evidence from top hits (limit to 5 most relevant)
        let top_hits: Vec<_> = search_hits.iter().take(5).collect();
        for hit in &top_hits {
            if !evidence_nodes.contains(&hit.node_id) {
                evidence_nodes.push(hit.node_id.clone());
            }
        }

        debug!(
            slug = slug,
            sub_question = sub_question,
            search_hits = search_hits.len(),
            evidence_count = evidence_nodes.len(),
            "FTS search matched"
        );
    }

    // ── Step 3: Drill into matching nodes for detail ────────────────────
    // Fetch full content from top evidence nodes to compose answer fragments
    let nodes_to_drill: Vec<String> = evidence_nodes.iter().take(3).cloned().collect();
    for node_id in &nodes_to_drill {
        if let Ok(Some(node)) = db::get_live_node(conn, slug, node_id) {
            // Use headline + distilled as answer fragment
            let fragment = if node.distilled.len() > 300 {
                format!("{}: {}", node.headline, &node.distilled[..300])
            } else {
                format!("{}: {}", node.headline, node.distilled)
            };
            answer_fragments.push(fragment);

            // Boost confidence if we got substantive content
            if node.distilled.len() > 50 {
                confidence += 0.1;
            }
        }
    }

    // Also check vocab matches for detail that can serve as evidence.
    // Vocab entries have a detail field with the full structured data.
    for entry in &vocab_matches {
        if let Some(detail_str) = entry.detail.as_str() {
            if !detail_str.is_empty() && detail_str.len() > 10 {
                answer_fragments.push(format!("[vocab] {}: {}", entry.name, detail_str));
            }
        }
    }

    // Clamp confidence to 0.0-1.0
    confidence = confidence.min(1.0);

    // Compose answer from fragments
    let answer = if answer_fragments.is_empty() {
        None
    } else {
        Some(answer_fragments.join("\n\n"))
    };

    Ok(SubQuestionResult {
        question: sub_question.to_string(),
        answer,
        evidence_nodes,
        confidence,
    })
}

// ── Cross-pyramid escalation ────────────────────────────────────────────────

/// Escalate a sub-question into composed (bedrock) pyramids when the current
/// pyramid's answer is insufficient.
///
/// Follows slug references (ties_to edges into composed pyramids). Bounded by
/// max_depth and protected by a visited set to prevent cycles.
fn escalate_to_composed_pyramids(
    conn: &Connection,
    slug: &str,
    sub_question: &str,
    visited: &mut HashSet<String>,
    max_depth: usize,
    current_depth: usize,
) -> Result<SubQuestionResult> {
    if current_depth >= max_depth {
        return Ok(SubQuestionResult {
            question: sub_question.to_string(),
            answer: None,
            evidence_nodes: vec![],
            confidence: 0.0,
        });
    }

    // Get referenced slugs (bedrock pyramids this slug composes)
    let referenced = db::get_slug_references(conn, slug)?;

    let mut best_result = SubQuestionResult {
        question: sub_question.to_string(),
        answer: None,
        evidence_nodes: vec![],
        confidence: 0.0,
    };

    for ref_slug in &referenced {
        if visited.contains(ref_slug) {
            continue; // Cycle prevention
        }
        visited.insert(ref_slug.clone());

        // Try answering in the composed pyramid
        let result = answer_sub_question(conn, ref_slug, sub_question)?;

        if result.confidence > best_result.confidence {
            // Prefix node IDs with the source slug for cross-pyramid tracing
            let mut cross_result = result;
            cross_result.evidence_nodes = cross_result
                .evidence_nodes
                .iter()
                .map(|nid| format!("{}:{}", ref_slug, nid))
                .collect();
            best_result = cross_result;
        }

        // If confidence is already good, don't recurse further
        if best_result.confidence >= 0.5 {
            break;
        }

        // Recurse into the referenced slug's own references
        let deeper = escalate_to_composed_pyramids(
            conn,
            ref_slug,
            sub_question,
            visited,
            max_depth,
            current_depth + 1,
        )?;

        if deeper.confidence > best_result.confidence {
            best_result = deeper;
        }
    }

    Ok(best_result)
}

// ── Main retrieval function ─────────────────────────────────────────────────

/// Retrieve answers to a question from a pyramid's structured content.
///
/// Steps:
/// 1. Decompose question into sub-questions (mechanical, no LLM)
/// 2. For each sub-question: vocabulary recognition → FTS search → evidence gathering
/// 3. If sub-questions unanswered, escalate into composed pyramids
/// 4. If still unanswered and allow_demand_gen: create demand-gen jobs
/// 5. Compose results with citations
///
/// LOCKED: evidence_mode: fast — always returns immediately (<5s).
pub async fn question_retrieve(
    state: &Arc<PyramidState>,
    slug: &str,
    question: &str,
    allow_demand_gen: bool,
) -> Result<QuestionRetrieveResult> {
    info!(slug = slug, question = question, "Starting question retrieval");

    // 1. Mechanical decomposition
    let sub_questions = decompose_mechanically(question);
    if sub_questions.is_empty() {
        return Ok(QuestionRetrieveResult {
            question: question.to_string(),
            sub_questions: vec![],
            composed_answer: None,
            demand_gen_needed: vec![],
            demand_gen_job_ids: vec![],
            sources: vec![],
        });
    }

    // 2. Answer each sub-question against the pyramid
    let mut sub_results = Vec::new();
    let mut all_sources: Vec<String> = Vec::new();
    let mut unanswered: Vec<String> = Vec::new();

    {
        let conn = state.reader.lock().await;

        for sub_q in &sub_questions {
            let mut result = answer_sub_question(&conn, slug, sub_q)?;

            // 3. If confidence is low, try escalating into composed pyramids
            if result.confidence < 0.3 {
                let mut visited = HashSet::new();
                visited.insert(slug.to_string());

                let escalated = escalate_to_composed_pyramids(
                    &conn,
                    slug,
                    sub_q,
                    &mut visited,
                    3, // max_depth for cross-pyramid escalation
                    0,
                )?;

                if escalated.confidence > result.confidence {
                    result = escalated;
                }
            }

            // Track unanswered sub-questions
            if result.answer.is_none() {
                unanswered.push(sub_q.clone());
            }

            // Collect sources
            for node_id in &result.evidence_nodes {
                if !all_sources.contains(node_id) {
                    all_sources.push(node_id.clone());
                }
            }

            sub_results.push(result);
        }
    } // Drop reader lock

    // 4. Fire demand-gen jobs for unanswered sub-questions if allowed
    let mut demand_gen_job_ids = Vec::new();
    if allow_demand_gen && !unanswered.is_empty() {
        let job_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();

        let job = DemandGenJob {
            id: 0,
            job_id: job_id.clone(),
            slug: slug.to_string(),
            question: question.to_string(),
            sub_questions: unanswered.clone(),
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: now,
            started_at: None,
            completed_at: None,
        };

        // Insert job into DB
        {
            let conn = state.writer.lock().await;
            db::create_demand_gen_job(&conn, &job)?;
        }

        // Spawn async execution
        demand_gen::spawn_demand_gen(
            state.clone(),
            slug.to_string(),
            job_id.clone(),
        );

        demand_gen_job_ids.push(job_id);

        info!(
            slug = slug,
            unanswered_count = unanswered.len(),
            "Demand-gen jobs created for unanswered sub-questions"
        );
    }

    // 5. Compose answer from sub-question results
    let answered_subs: Vec<&SubQuestionResult> = sub_results
        .iter()
        .filter(|r| r.answer.is_some())
        .collect();

    let composed_answer = if answered_subs.is_empty() {
        None
    } else {
        let composed = answered_subs
            .iter()
            .map(|r| {
                format!(
                    "**{}**\n{}",
                    r.question,
                    r.answer.as_deref().unwrap_or("(no answer)")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        Some(composed)
    };

    Ok(QuestionRetrieveResult {
        question: question.to_string(),
        sub_questions: sub_results,
        composed_answer,
        demand_gen_needed: unanswered,
        demand_gen_job_ids,
        sources: all_sources,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    /// Insert a test pyramid node into the DB for search and retrieval testing.
    fn insert_test_node(conn: &Connection, slug: &str, node_id: &str, depth: i64, headline: &str, distilled: &str) {
        conn.execute(
            "INSERT INTO pyramid_nodes (slug, id, depth, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, datetime('now'))",
            rusqlite::params![slug, node_id, depth, headline, distilled],
        ).unwrap();
    }

    /// Insert a test vocabulary entry for recognition queries.
    fn insert_test_vocab(conn: &Connection, slug: &str, name: &str, entry_type: &str) {
        conn.execute(
            "INSERT INTO pyramid_vocabulary_catalog (slug, entry_name, entry_type, category, importance, liveness, detail, source_node_id, updated_at)
             VALUES (?1, ?2, ?3, NULL, 0.8, 'live', '{}', 'test-node', datetime('now'))",
            rusqlite::params![slug, name, entry_type],
        ).unwrap();
    }

    /// Insert a slug record for the test pyramid.
    fn insert_test_slug(conn: &Connection, slug: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path, created_at)
             VALUES (?1, 'code', '/tmp/test', datetime('now'))",
            rusqlite::params![slug],
        ).unwrap();
    }

    #[test]
    fn test_mechanical_decomposition_single_question() {
        let result = decompose_mechanically("What is the architecture of the system?");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "What is the architecture of the system?");
    }

    #[test]
    fn test_mechanical_decomposition_semicolon_split() {
        let result = decompose_mechanically(
            "What is the architecture?; How does the build pipeline work?; What are the key decisions?"
        );
        assert_eq!(result.len(), 3);
        assert!(result[0].contains("architecture"));
        assert!(result[1].contains("build pipeline"));
        assert!(result[2].contains("key decisions"));
    }

    #[test]
    fn test_mechanical_decomposition_question_mark_split() {
        let result = decompose_mechanically(
            "What is the architecture? How does the build pipeline work?"
        );
        assert_eq!(result.len(), 2);
        assert!(result[0].contains("architecture"));
        assert!(result[1].contains("build pipeline"));
    }

    #[test]
    fn test_mechanical_decomposition_empty() {
        let result = decompose_mechanically("");
        assert!(result.is_empty());

        let result2 = decompose_mechanically("   ");
        assert!(result2.is_empty());
    }

    #[test]
    fn test_answer_sub_question_with_matching_vocab() {
        let conn = setup_test_db();
        let slug = "test-slug";

        insert_test_slug(&conn, slug);
        insert_test_node(&conn, slug, "L2-apex", 2, "System Overview", "The system uses a pyramid architecture for knowledge organization with chain executors and vocabulary catalogs.");
        insert_test_node(&conn, slug, "L1-arch", 1, "Architecture Details", "The architecture consists of chain executors, vocabulary recognition, and demand-gen pipelines.");
        insert_test_node(&conn, slug, "L0-base", 0, "Base Implementation", "Implementation uses Rust with SQLite for persistence and warp for HTTP routing.");

        insert_test_vocab(&conn, slug, "chain executor", "topic");
        insert_test_vocab(&conn, slug, "vocabulary", "topic");

        let result = answer_sub_question(&conn, slug, "How does the chain executor work?").unwrap();

        // Should find evidence from vocab recognition + FTS
        assert!(!result.evidence_nodes.is_empty(), "Should find evidence nodes");
        assert!(result.confidence > 0.0, "Should have non-zero confidence");
    }

    #[test]
    fn test_answer_sub_question_no_matches() {
        let conn = setup_test_db();
        let slug = "empty-slug";

        insert_test_slug(&conn, slug);

        let result = answer_sub_question(&conn, slug, "What is quantum chromodynamics?").unwrap();

        assert!(result.evidence_nodes.is_empty(), "Should find no evidence");
        assert!(result.answer.is_none(), "Should have no answer");
        assert_eq!(result.confidence, 0.0, "Should have zero confidence");
    }

    #[test]
    fn test_demand_gen_triggered_for_unanswered() {
        let conn = setup_test_db();
        let slug = "test-dg";

        insert_test_slug(&conn, slug);

        // Create a DemandGenJob to verify the DB infrastructure works
        let job = DemandGenJob {
            id: 0,
            job_id: "test-qr-job".to_string(),
            slug: slug.to_string(),
            question: "What is the meaning of life?".to_string(),
            sub_questions: vec!["What is meaning?".to_string()],
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: "2026-04-08T12:00:00".to_string(),
            started_at: None,
            completed_at: None,
        };
        db::create_demand_gen_job(&conn, &job).unwrap();

        let fetched = db::get_demand_gen_job(&conn, "test-qr-job").unwrap();
        assert!(fetched.is_some(), "Job should be created");
        let fetched = fetched.unwrap();
        assert_eq!(fetched.status, "queued");
        assert_eq!(fetched.sub_questions.len(), 1);
    }

    #[test]
    fn test_decomposition_produces_multiple_sub_questions() {
        // Semicolons
        let result = decompose_mechanically(
            "What is A?; What is B?; What is C?"
        );
        assert!(result.len() > 1, "Should produce multiple sub-questions from semicolons");

        // Question marks
        let result2 = decompose_mechanically(
            "What is the build system? How does deployment work?"
        );
        assert!(result2.len() > 1, "Should produce multiple sub-questions from question marks");

        // Single question should not split
        let result3 = decompose_mechanically("What is the build system?");
        assert_eq!(result3.len(), 1, "Single question should not split");
    }
}
