// partner/warm.rs — Warm pass: progressive crystallization from conversation
//
// Two tiers:
//   Tier 1: Zero-cost regex extraction on every message (entities, corrections, decisions)
//   Tier 2: Periodic warm pass — distills new messages into deltas against the pyramid
//
// The warm pass runs in the background after every WARM_PASS_THRESHOLD new messages.
// It does NOT block the partner response.

use std::sync::Arc;
use tokio::sync::Mutex;
use rusqlite::Connection;
use tracing::info;

use crate::partner::{Session, Message, SessionTopic};
use crate::partner::crystal;
use crate::pyramid::delta;
use tracing::warn;

/// Threshold: run warm pass after this many new messages since last pass.
const WARM_PASS_THRESHOLD: usize = 10;

// ── Tier 1: Zero-cost extraction ────────────────────────────────────

/// Result of Tier 1 regex extraction on a single message.
#[derive(Debug, Clone)]
pub struct Tier1Extraction {
    pub entities: Vec<String>,
    pub corrections: Vec<String>,
    pub decisions: Vec<String>,
}

/// Tier 1: Zero-cost regex extraction on every message.
/// Returns extracted entities, corrections, decisions.
pub fn tier1_extract(message: &str) -> Tier1Extraction {
    let mut entities = Vec::new();
    let mut corrections = Vec::new();
    let mut decisions = Vec::new();

    // Entity extraction: @mentions, file paths, capitalized words
    for word in message.split_whitespace() {
        // @mentions
        if word.starts_with('@') && word.len() > 1 {
            entities.push(word.to_string());
        }
        // File paths
        if word.contains('/') && word.contains('.') && !word.starts_with("http") {
            entities.push(word.to_string());
        }
        // Capitalized multi-word names (simple heuristic)
        if word.len() > 1
            && word.chars().next().map_or(false, |c| c.is_uppercase())
            && !word.chars().all(|c| c.is_uppercase()) // not all-caps
            && word.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            entities.push(word.to_string());
        }
    }

    // Correction patterns
    let lower = message.to_lowercase();
    if lower.contains("no, it's")
        || lower.contains("no it's")
        || lower.contains("actually,")
        || lower.contains("correction:")
        || lower.contains("not that, ")
    {
        corrections.push(message.to_string());
    }

    // Decision patterns
    if lower.contains("let's go with")
        || lower.contains("we decided")
        || lower.contains("the answer is")
        || lower.contains("let's use")
    {
        decisions.push(message.to_string());
    }

    Tier1Extraction {
        entities,
        corrections,
        decisions,
    }
}

// ── Tier 2: Warm pass ───────────────────────────────────────────────

/// Result of a warm pass.
#[derive(Debug)]
pub struct WarmPassResult {
    pub deltas_created: usize,
    pub new_topics: Vec<SessionTopic>,
    pub messages_processed: usize,
}

/// Check if warm pass should run based on message count since last pass.
pub fn should_run_warm_pass(session: &Session) -> bool {
    let new_messages = session
        .conversation_buffer
        .len()
        .saturating_sub(session.warm_cursor);
    new_messages >= WARM_PASS_THRESHOLD
}

/// Run the warm pass -- distill new messages into deltas against the pyramid.
///
/// CONCURRENCY: This takes owned copies, not references to shared state.
/// The caller must clone the session data, run this, then merge results back.
pub async fn warm_pass(
    conversation_chunk: Vec<Message>,
    slug: &str,
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    api_key: &str,
    model: &str,
    collapse_model: &str,
) -> anyhow::Result<WarmPassResult> {
    if conversation_chunk.is_empty() {
        return Ok(WarmPassResult {
            deltas_created: 0,
            new_topics: vec![],
            messages_processed: 0,
        });
    }

    // Combine messages into a single content block
    let combined: String = conversation_chunk
        .iter()
        .map(|m| format!("[{}] {}", m.role.as_str(), m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let messages_processed = conversation_chunk.len();

    // Generate a provisional node ID for the warm pass content
    let provisional_node_id = format!("warm-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000"));

    // Match to a thread
    let thread_id =
        delta::match_or_create_thread(reader, writer, slug, &combined, &provisional_node_id, api_key, model).await?;

    // Create a delta
    let delta =
        delta::create_delta(reader, writer, slug, &thread_id, &combined, None, api_key, model)
            .await?;

    info!(
        "[warm] Created delta for thread {} (relevance: {})",
        thread_id,
        delta.relevance.as_str()
    );

    // Create a session topic summary
    let topic_summary = if delta.content.len() > 200 {
        format!(
            "{}...",
            crate::utils::safe_slice_end(&delta.content, 200)
        )
    } else {
        delta.content.clone()
    };

    let topic = SessionTopic {
        summary: topic_summary,
        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };

    // Fire-and-forget: run crystallization pass after warm pass completes
    {
        let reader = reader.clone();
        let writer = writer.clone();
        let slug = slug.to_string();
        let api_key = api_key.to_string();
        let collapse_model = collapse_model.to_string();
        tokio::spawn(async move {
            match crystal::crystallize(&reader, &writer, &slug, &api_key, &collapse_model).await {
                Ok(result) => {
                    if result.collapses > 0 {
                        info!("[warm] crystallization pass collapsed {} threads", result.collapses);
                    }
                }
                Err(e) => {
                    warn!("[warm] crystallization pass failed: {}", e);
                }
            }
        });
    }

    Ok(WarmPassResult {
        deltas_created: 1,
        new_topics: vec![topic],
        messages_processed,
    })
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partner::{Message, MessageRole, Session, DennisState};

    #[test]
    fn test_tier1_extract_entities() {
        let extraction = tier1_extract("Talk to @alice about src/main.rs please");
        assert!(extraction.entities.contains(&"@alice".to_string()));
        assert!(extraction.entities.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_tier1_extract_corrections() {
        let extraction = tier1_extract("No, it's actually called the delta engine");
        assert!(!extraction.corrections.is_empty());
    }

    #[test]
    fn test_tier1_extract_decisions() {
        let extraction = tier1_extract("Let's go with the recursive approach");
        assert!(!extraction.decisions.is_empty());
    }

    #[test]
    fn test_tier1_extract_empty() {
        let extraction = tier1_extract("hello world");
        assert!(extraction.corrections.is_empty());
        assert!(extraction.decisions.is_empty());
    }

    #[test]
    fn test_should_run_warm_pass() {
        let mut session = Session {
            id: "test".into(),
            slug: Some("test-slug".into()),
            is_lobby: false,
            conversation_buffer: Vec::new(),
            session_topics: Vec::new(),
            hydrated_node_ids: Vec::new(),
            lifted_results: Vec::new(),
            dennis_state: DennisState::Idle,
            warm_cursor: 0,
            created_at: String::new(),
            last_active_at: String::new(),
        };

        // Empty buffer: should not run
        assert!(!should_run_warm_pass(&session));

        // Add 9 messages: still should not run
        for i in 0..9 {
            session.conversation_buffer.push(Message {
                role: MessageRole::User,
                content: format!("msg {}", i),
                timestamp: String::new(),
                token_estimate: 10,
            });
        }
        assert!(!should_run_warm_pass(&session));

        // Add 10th message: should run
        session.conversation_buffer.push(Message {
            role: MessageRole::User,
            content: "msg 9".into(),
            timestamp: String::new(),
            token_estimate: 10,
        });
        assert!(should_run_warm_pass(&session));

        // Advance cursor: should not run again
        session.warm_cursor = 10;
        assert!(!should_run_warm_pass(&session));
    }
}
