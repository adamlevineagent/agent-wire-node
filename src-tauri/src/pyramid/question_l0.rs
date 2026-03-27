// pyramid/question_l0.rs — Question L0 Pass (WS-B)
//
// Generates question-shaped L0 nodes from canonical L0 nodes.
// Reads canonical L0 summaries, applies question + audience framing,
// and produces question-L0 nodes that feed into the evidence loop.
//
// This is an LLM pass that reads canonical L0 distilled text (not raw files)
// and reshapes it for the specific question being asked.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use super::extraction_schema;
use super::llm::{self, LlmConfig};
use super::question_decomposition::QuestionTree;
use super::types::{ExtractionSchema, PyramidNode};

/// Maximum number of concurrent LLM calls for question L0 extraction.
const MAX_CONCURRENT_LLM_CALLS: usize = 8;

// ── Response parsing ─────────────────────────────────────────────────────────

/// Parsed LLM response for a question L0 extraction.
#[derive(Debug, serde::Deserialize)]
struct QuestionL0Response {
    relevant: bool,
    #[serde(default)]
    headline: String,
    #[serde(default)]
    distilled: String,
    #[serde(default)]
    topics: Vec<TopicResponse>,
    #[serde(default)]
    corrections: Vec<CorrectionResponse>,
    #[serde(default)]
    decisions: Vec<DecisionResponse>,
    #[serde(default)]
    terms: Vec<TermResponse>,
    #[serde(default)]
    dead_ends: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TopicResponse {
    name: String,
    #[serde(default)]
    current: String,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    corrections: Vec<CorrectionResponse>,
    #[serde(default)]
    decisions: Vec<DecisionResponse>,
}

#[derive(Debug, serde::Deserialize)]
struct CorrectionResponse {
    #[serde(default)]
    wrong: String,
    #[serde(default)]
    right: String,
    #[serde(default)]
    who: String,
}

#[derive(Debug, serde::Deserialize)]
struct DecisionResponse {
    #[serde(default)]
    decided: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    rejected: String,
}

#[derive(Debug, serde::Deserialize)]
struct TermResponse {
    #[serde(default)]
    term: String,
    #[serde(default)]
    definition: String,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Generate question-shaped L0 nodes from canonical L0 nodes.
///
/// For each canonical L0 node, asks the LLM to reshape the extraction for the
/// specific question tree. Filters out irrelevant nodes and produces question-L0
/// nodes with `L0-{uuid}` IDs.
///
/// Does NOT save to DB — the caller (build_runner) persists the returned nodes.
pub async fn generate_question_l0(
    canonical_nodes: &[PyramidNode],
    question_tree: &QuestionTree,
    extraction_schema: &ExtractionSchema,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    slug: &str,
) -> Result<Vec<PyramidNode>> {
    if canonical_nodes.is_empty() {
        info!(slug = slug, "no canonical L0 nodes — skipping question L0 pass");
        return Ok(vec![]);
    }

    // 1. Collect leaf questions from the question tree
    let leaf_questions = extraction_schema::collect_leaf_questions(question_tree);
    if leaf_questions.is_empty() {
        anyhow::bail!("no leaf questions in question tree — cannot generate question L0");
    }

    // Format leaf questions for the prompt
    let leaf_questions_formatted = leaf_questions
        .iter()
        .enumerate()
        .map(|(i, q)| format!("  {}. {}", i + 1, q.question))
        .collect::<Vec<_>>()
        .join("\n");

    // Collect leaf question text for keyword matching
    let leaf_question_keywords: Vec<Vec<String>> = leaf_questions
        .iter()
        .map(|q| extract_keywords(&q.question))
        .collect();

    // 2. Pre-filter canonical nodes by keyword overlap with leaf questions
    let relevant_indices: Vec<usize> = canonical_nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            has_keyword_overlap(node, &leaf_question_keywords)
        })
        .map(|(i, _)| i)
        .collect();

    let skipped = canonical_nodes.len() - relevant_indices.len();
    if skipped > 0 {
        info!(
            slug = slug,
            total = canonical_nodes.len(),
            relevant = relevant_indices.len(),
            skipped = skipped,
            "pre-filtered canonical L0 nodes by keyword overlap"
        );
    }

    if relevant_indices.is_empty() {
        warn!(
            slug = slug,
            "all canonical L0 nodes filtered out — no keyword overlap with leaf questions"
        );
        return Ok(vec![]);
    }

    // 3. Build the system prompt
    let audience_text = audience.unwrap_or("a general audience");
    let system_prompt = format!(
        r#"You are reshaping a source material extraction for a specific question context.
You are given a comprehensive extraction of a document and a set of questions to answer.

Your job:
1. Identify which parts of this extraction are relevant to answering the questions below.
2. Rewrite the extraction focused on the relevant parts, using language appropriate for: {audience_text}.
3. Cross-check: Do NOT add any claims not supported by the canonical extraction. You are reshaping, not inventing.
4. If the extraction has NO relevance to any of the questions, respond with {{"relevant": false}}.

Questions to answer:
{leaf_questions_formatted}

Extraction schema to follow:
{extraction_prompt}

Respond in JSON with these fields:
{{
  "relevant": true,
  "headline": "A question-shaped headline summarizing the relevant content",
  "distilled": "The reshaped extraction focused on answering the questions above, written for {audience_text}",
  "topics": [
    {{
      "name": "topic_name",
      "current": "current understanding of this topic relevant to the questions",
      "entities": ["entity1", "entity2"],
      "corrections": [{{"wrong": "", "right": "", "who": ""}}],
      "decisions": [{{"decided": "", "why": "", "rejected": ""}}]
    }}
  ],
  "corrections": [{{"wrong": "", "right": "", "who": ""}}],
  "decisions": [{{"decided": "", "why": "", "rejected": ""}}],
  "terms": [{{"term": "", "definition": ""}}],
  "dead_ends": ["dead end description"]
}}

If no part of this extraction is relevant to ANY of the questions, respond with ONLY:
{{"relevant": false}}

Return ONLY the JSON object, no other text."#,
        extraction_prompt = extraction_schema.extraction_prompt,
    );

    // 4. Spawn parallel LLM calls with semaphore
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_LLM_CALLS));
    let system_prompt = Arc::new(system_prompt);
    let llm_config = Arc::new(llm_config.clone());
    let slug = slug.to_string();

    let mut handles = Vec::with_capacity(relevant_indices.len());

    // Find the most relevant leaf question for each node (used for self_prompt)
    for &idx in &relevant_indices {
        let node = canonical_nodes[idx].clone();
        let sem = semaphore.clone();
        let sys_prompt = system_prompt.clone();
        let config = llm_config.clone();
        let slug = slug.clone();
        let leaf_qs = leaf_questions
            .iter()
            .map(|q| q.question.clone())
            .collect::<Vec<_>>();
        let leaf_kws = leaf_question_keywords.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.map_err(|e| {
                anyhow::anyhow!("semaphore acquisition failed: {}", e)
            })?;

            process_single_node(&node, &sys_prompt, &config, &slug, &leaf_qs, &leaf_kws).await
        });

        handles.push(handle);
    }

    // 5. Collect results
    let mut question_l0_nodes = Vec::new();
    let mut errors = 0;

    for handle in handles {
        match handle.await {
            Ok(Ok(Some(node))) => question_l0_nodes.push(node),
            Ok(Ok(None)) => {} // irrelevant, filtered
            Ok(Err(e)) => {
                errors += 1;
                warn!("question L0 extraction failed for a node: {}", e);
            }
            Err(e) => {
                errors += 1;
                warn!("question L0 task panicked: {}", e);
            }
        }
    }

    info!(
        slug = slug,
        produced = question_l0_nodes.len(),
        errors = errors,
        "question L0 pass complete"
    );

    Ok(question_l0_nodes)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Process a single canonical L0 node through the LLM to produce a question-L0 node.
/// Returns None if the LLM determines the node is irrelevant.
async fn process_single_node(
    canonical_node: &PyramidNode,
    system_prompt: &str,
    llm_config: &LlmConfig,
    slug: &str,
    leaf_questions: &[String],
    leaf_question_keywords: &[Vec<String>],
) -> Result<Option<PyramidNode>> {
    // Build user prompt from canonical node's distilled text + topics
    let topics_json = serde_json::to_string(&canonical_node.topics).unwrap_or_default();
    let user_prompt = format!(
        "Canonical extraction:\n\nHeadline: {}\n\n{}\n\nTopics:\n{}",
        canonical_node.headline,
        canonical_node.distilled,
        topics_json,
    );

    // Call LLM with default config (mercury-2)
    let response = llm::call_model_unified(
        llm_config,
        system_prompt,
        &user_prompt,
        0.2,   // low temperature for faithful reshaping
        4096,
        None,
    )
    .await?;

    info!(
        node_id = canonical_node.id,
        tokens_in = response.usage.prompt_tokens,
        tokens_out = response.usage.completion_tokens,
        "question L0 LLM call complete"
    );

    // Parse response
    let parsed: QuestionL0Response = match llm::extract_json(&response.content) {
        Ok(json_value) => serde_json::from_value(json_value)
            .map_err(|e| anyhow::anyhow!("failed to parse question L0 response: {}", e))?,
        Err(e) => {
            warn!(
                node_id = canonical_node.id,
                "failed to extract JSON from question L0 response: {}",
                e
            );
            return Err(e);
        }
    };

    // If not relevant, skip
    if !parsed.relevant {
        info!(
            node_id = canonical_node.id,
            "canonical node marked irrelevant by LLM — skipping"
        );
        return Ok(None);
    }

    // Find the most relevant leaf question for self_prompt
    let self_prompt = find_most_relevant_question(
        &parsed.headline,
        &parsed.distilled,
        leaf_questions,
        leaf_question_keywords,
    );

    // Build the PyramidNode
    let node = PyramidNode {
        id: format!("L0-{}", Uuid::new_v4()),
        slug: slug.to_string(),
        depth: 0,
        chunk_index: None,
        headline: parsed.headline,
        distilled: parsed.distilled,
        topics: parsed
            .topics
            .into_iter()
            .map(|t| super::types::Topic {
                name: t.name,
                current: t.current,
                entities: t.entities,
                corrections: t
                    .corrections
                    .into_iter()
                    .map(|c| super::types::Correction {
                        wrong: c.wrong,
                        right: c.right,
                        who: c.who,
                    })
                    .collect(),
                decisions: t
                    .decisions
                    .into_iter()
                    .map(|d| super::types::Decision {
                        decided: d.decided,
                        why: d.why,
                        rejected: d.rejected,
                    })
                    .collect(),
            })
            .collect(),
        corrections: parsed
            .corrections
            .into_iter()
            .map(|c| super::types::Correction {
                wrong: c.wrong,
                right: c.right,
                who: c.who,
            })
            .collect(),
        decisions: parsed
            .decisions
            .into_iter()
            .map(|d| super::types::Decision {
                decided: d.decided,
                why: d.why,
                rejected: d.rejected,
            })
            .collect(),
        terms: parsed
            .terms
            .into_iter()
            .map(|t| super::types::Term {
                term: t.term,
                definition: t.definition,
            })
            .collect(),
        dead_ends: parsed.dead_ends,
        self_prompt,
        children: vec![],
        parent_id: None,
        superseded_by: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    Ok(Some(node))
}

/// Extract lowercased keywords from a question string.
/// Strips common stop words and punctuation.
fn extract_keywords(text: &str) -> Vec<String> {
    static STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "shall", "can", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "as", "into", "through", "during",
        "before", "after", "above", "below", "between", "under", "again",
        "further", "then", "once", "here", "there", "when", "where", "why",
        "how", "all", "each", "every", "both", "few", "more", "most", "other",
        "some", "such", "no", "nor", "not", "only", "own", "same", "so",
        "than", "too", "very", "just", "because", "but", "and", "or", "if",
        "while", "about", "what", "which", "who", "whom", "this", "that",
        "these", "those", "it", "its", "they", "them", "their",
    ];

    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Check if a canonical node has keyword overlap with any leaf question.
fn has_keyword_overlap(node: &PyramidNode, leaf_question_keywords: &[Vec<String>]) -> bool {
    let node_text = format!(
        "{} {} {}",
        node.headline,
        node.distilled,
        node.topics
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let node_keywords = extract_keywords(&node_text);

    for question_kws in leaf_question_keywords {
        for qkw in question_kws {
            if node_keywords.iter().any(|nkw| nkw.contains(qkw.as_str()) || qkw.contains(nkw.as_str())) {
                return true;
            }
        }
    }

    false
}

/// Find the most relevant leaf question for a given question L0 node.
/// Uses simple keyword overlap scoring to pick the best match.
fn find_most_relevant_question(
    headline: &str,
    distilled: &str,
    leaf_questions: &[String],
    leaf_question_keywords: &[Vec<String>],
) -> String {
    let node_text = format!("{} {}", headline, distilled);
    let node_keywords = extract_keywords(&node_text);

    let mut best_score = 0usize;
    let mut best_idx = 0usize;

    for (i, question_kws) in leaf_question_keywords.iter().enumerate() {
        let score = question_kws
            .iter()
            .filter(|qkw| {
                node_keywords
                    .iter()
                    .any(|nkw| nkw.contains(qkw.as_str()) || qkw.contains(nkw.as_str()))
            })
            .count();

        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    leaf_questions
        .get(best_idx)
        .cloned()
        .unwrap_or_default()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords_filters_stop_words() {
        let kws = extract_keywords("How does the system handle authentication and authorization?");
        assert!(kws.contains(&"system".to_string()));
        assert!(kws.contains(&"handle".to_string()));
        assert!(kws.contains(&"authentication".to_string()));
        assert!(kws.contains(&"authorization".to_string()));
        assert!(!kws.contains(&"how".to_string()));
        assert!(!kws.contains(&"does".to_string()));
        assert!(!kws.contains(&"the".to_string()));
        assert!(!kws.contains(&"and".to_string()));
    }

    #[test]
    fn extract_keywords_handles_underscores() {
        let kws = extract_keywords("stale_engine uses auto_update config");
        assert!(kws.contains(&"stale_engine".to_string()));
        assert!(kws.contains(&"auto_update".to_string()));
        assert!(kws.contains(&"uses".to_string()));
        assert!(kws.contains(&"config".to_string()));
    }

    #[test]
    fn has_keyword_overlap_detects_match() {
        let node = PyramidNode {
            id: "C-L0-001".to_string(),
            slug: "test".to_string(),
            depth: 0,
            chunk_index: None,
            headline: "Authentication module overview".to_string(),
            distilled: "This file implements OAuth2 authentication flows.".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        };

        let leaf_kws = vec![
            extract_keywords("How does authentication work?"),
        ];

        assert!(has_keyword_overlap(&node, &leaf_kws));
    }

    #[test]
    fn has_keyword_overlap_rejects_no_match() {
        let node = PyramidNode {
            id: "C-L0-002".to_string(),
            slug: "test".to_string(),
            depth: 0,
            chunk_index: None,
            headline: "Database migration scripts".to_string(),
            distilled: "Contains SQL migration files for schema updates.".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        };

        let leaf_kws = vec![
            extract_keywords("How does the UI render animations?"),
        ];

        assert!(!has_keyword_overlap(&node, &leaf_kws));
    }

    #[test]
    fn find_most_relevant_question_picks_best_match() {
        let leaf_questions = vec![
            "How does authentication work?".to_string(),
            "How does the database handle migrations?".to_string(),
            "What is the deployment pipeline?".to_string(),
        ];
        let leaf_kws: Vec<Vec<String>> = leaf_questions
            .iter()
            .map(|q| extract_keywords(q))
            .collect();

        let result = find_most_relevant_question(
            "OAuth2 authentication flows",
            "Implements token-based authentication with refresh tokens.",
            &leaf_questions,
            &leaf_kws,
        );

        assert_eq!(result, "How does authentication work?");
    }

    #[test]
    fn question_l0_id_format() {
        let id = format!("L0-{}", Uuid::new_v4());
        assert!(id.starts_with("L0-"));
        assert!(id.len() > 3); // L0- prefix + UUID
    }
}
