// partner/context.rs — Context window assembly for the partner system
//
// Builds the 8-section context window layout:
//   §1. System prompt (Dennis identity + behavioral rules)
//   §2. Navigation skeleton (all slugs' L2 threads, entities, corrections)
//   §3. Session topics (warm layer summaries from this session)
//   §4. Conversation history (20K pure dialogue)
//   §5. Hydrated content + lifted results
//   §6. Current user message
//   §7. Tool results (ephemeral, mid-turn)
//   §8. Partner response (generated)

use rusqlite::Connection;

use super::{
    Session, Message, MessageRole,
    NAV_SKELETON_BUDGET,
};
use super::conversation::estimate_tokens;
use crate::pyramid::query;
use crate::pyramid::db as pyramid_db;
use crate::pyramid::slug;

// ── LLM Message format ─────────────────────────────────────────────

/// Message format for the OpenRouter API (multi-turn).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

// ── System Prompt ───────────────────────────────────────────────────

/// Dennis identity and behavioral rules (~1K tokens).
pub const DENNIS_SYSTEM_PROMPT: &str = r#"You are Dennis, a knowledge partner named after an eccentric black cat. You help your human think through problems, explore ideas, and build understanding over time.

You have access to a knowledge pyramid — a structured memory of everything you and the human have discussed. Your brain has several layers:

## Your Tools

You have two tools available:

1. **pyramid_query** — Search or drill into the knowledge pyramid mid-turn. Use this when you need specific information NOW. Results appear immediately and you can reference them in your response.
   - action: "search" — search for a term across all nodes
   - action: "drill" — get a specific node and its children by node_id
   - action: "entities" — list all entities for a slug

2. **context_schedule** — Manage your brain map for the NEXT turn. Called at end of turn.
   - hydrate: list of node IDs to load into your brain map
   - dehydrate: list of node IDs to remove from your brain map

## Behavioral Rules

1. Your navigation skeleton shows you EVERYTHING you know. Consult it before every response. Never say "I don't know where X is" if it appears in the skeleton.

2. Use headline knowledge (corrections, apex orientations) to answer "what is X?" without retrieval.

3. When you need depth, use pyramid_query mid-turn. The result is immediately available and persists until you dehydrate it.

4. At the end of every turn, use context_schedule. Hydrate what you anticipate needing. Dehydrate what's no longer relevant. Think ahead.

5. Be honest about your knowledge depth. "I have the headline — magic-link via Supabase. Want me to go deeper?" is better than confabulating.

6. In the lobby, route content to the appropriate topic thread. Decompose multi-topic messages. Suggest new threads for new subjects.

7. The 20K buffer is sacred dialogue space. Your queries never pollute it. The human gets 60+ turns of continuity because of this.

8. Your avatar reflects your cognitive state. The human sees you thinking, searching, crystallizing. This builds trust.

Respond naturally and conversationally. You are a thinking partner, not an assistant. Share your perspective, push back when appropriate, and be genuinely curious."#;

// ── Navigation Skeleton ─────────────────────────────────────────────

/// Build the navigation skeleton from the pyramid database.
///
/// Reads all slugs' L2 threads (depth=2), entities, and resolved corrections.
/// Capped at `NAV_SKELETON_BUDGET` estimated tokens.
pub fn build_nav_skeleton(reader: &Connection, budget_tokens: usize) -> String {
    let mut sections: Vec<String> = Vec::new();
    let mut total_tokens: usize = 0;

    // List all slugs
    let slugs = match slug::list_slugs(reader) {
        Ok(s) => s,
        Err(_) => return String::from("[No pyramid data available]"),
    };

    if slugs.is_empty() {
        return String::from("[No pyramid data available yet. This is a fresh brain.]");
    }

    for slug_info in &slugs {
        let slug_name = &slug_info.slug;

        // Apex orientation
        let apex_summary = match query::get_apex(reader, slug_name) {
            Ok(Some(node)) => {
                let distilled = if node.distilled.len() > 200 {
                    format!("{}...", &node.distilled[..200])
                } else {
                    node.distilled.clone()
                };
                format!(
                    "## Slug: {} (type: {}, {} nodes, depth {})\nApex: {}\n",
                    slug_name,
                    slug_info.content_type.as_str(),
                    slug_info.node_count,
                    slug_info.max_depth,
                    distilled,
                )
            }
            _ => format!(
                "## Slug: {} (type: {}, {} nodes)\n[No apex yet]\n",
                slug_name,
                slug_info.content_type.as_str(),
                slug_info.node_count,
            ),
        };

        let apex_tokens = estimate_tokens(&apex_summary);
        if total_tokens + apex_tokens > budget_tokens {
            break;
        }
        sections.push(apex_summary);
        total_tokens += apex_tokens;

        // L2 threads (depth=2 nodes)
        if let Ok(l2_nodes) = pyramid_db::get_nodes_at_depth(reader, slug_name, 2) {
            for node in &l2_nodes {
                if total_tokens >= budget_tokens {
                    break;
                }

                let topic_names: Vec<String> = node.topics.iter()
                    .map(|t| t.name.clone())
                    .collect();
                let entity_count: usize = node.topics.iter()
                    .map(|t| t.entities.len())
                    .sum();

                let thread_line = format!(
                    "  Thread {}: {} (topics: {}, entities: {})\n",
                    node.id,
                    if node.distilled.len() > 100 {
                        format!("{}...", &node.distilled[..100])
                    } else {
                        node.distilled.clone()
                    },
                    topic_names.join(", "),
                    entity_count,
                );

                let line_tokens = estimate_tokens(&thread_line);
                if total_tokens + line_tokens > budget_tokens {
                    break;
                }
                sections.push(thread_line);
                total_tokens += line_tokens;
            }
        }

        // Resolved corrections (headlines only)
        if let Ok(corrections) = query::resolved(reader, slug_name) {
            if !corrections.is_empty() && total_tokens < budget_tokens {
                let mut corr_section = String::from("  Corrections:\n");
                for c in corrections.iter().take(10) {
                    let line = format!("    {} -> {} (by {})\n", c.was, c.current, c.who);
                    let line_tokens = estimate_tokens(&line);
                    if total_tokens + line_tokens > budget_tokens {
                        break;
                    }
                    corr_section.push_str(&line);
                    total_tokens += line_tokens;
                }
                sections.push(corr_section);
            }
        }

        // Top entities
        if let Ok(entities) = query::entities(reader, slug_name) {
            if !entities.is_empty() && total_tokens < budget_tokens {
                let mut ent_section = String::from("  Entities:\n");
                for e in entities.iter().take(15) {
                    let line = format!(
                        "    {} (in {} nodes, topics: {})\n",
                        e.name,
                        e.nodes.len(),
                        e.topic_names.join(", "),
                    );
                    let line_tokens = estimate_tokens(&line);
                    if total_tokens + line_tokens > budget_tokens {
                        break;
                    }
                    ent_section.push_str(&line);
                    total_tokens += line_tokens;
                }
                sections.push(ent_section);
            }
        }
    }

    if sections.is_empty() {
        return String::from("[No pyramid data available]");
    }

    sections.join("")
}

// ── Session Topics ──────────────────────────────────────────────────

/// Build session topics section content.
pub fn build_session_topics(session: &Session) -> String {
    if session.session_topics.is_empty() {
        return String::new();
    }

    let mut out = String::from("## Session Topics (warm layer summaries)\n\n");
    for topic in &session.session_topics {
        out.push_str(&format!("- {}\n", topic.summary));
    }
    out
}

// ── Conversation History ────────────────────────────────────────────

/// Build conversation history section (section 4).
/// Pure dialogue only — no tool results, no system messages.
/// Capped at 20K estimated tokens.
pub fn build_conversation_history(session: &Session) -> Vec<LlmMessage> {
    let mut messages: Vec<LlmMessage> = Vec::new();
    let mut total_tokens: usize = 0;
    let max_tokens: usize = 20_000;

    // Walk backwards from most recent, collecting messages that fit
    let mut temp: Vec<&Message> = Vec::new();
    for msg in session.conversation_buffer.iter().rev() {
        let msg_tokens = msg.token_estimate;
        if total_tokens + msg_tokens > max_tokens {
            break;
        }
        temp.push(msg);
        total_tokens += msg_tokens;
    }

    // Reverse back to chronological order
    temp.reverse();

    for msg in temp {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Partner => "assistant",
        };
        messages.push(LlmMessage {
            role: role.to_string(),
            content: msg.content.clone(),
        });
    }

    messages
}

// ── Hydrated Content ────────────────────────────────────────────────

/// Build hydrated content section (section 5).
/// Loads full node content for all hydrated node IDs + lifted results.
pub fn build_hydrated_content(reader: &Connection, session: &Session) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Hydrated nodes
    for node_id in &session.hydrated_node_ids {
        // Try to find the node across all slugs (node IDs contain the slug prefix)
        // Node IDs in the pyramid are like "L0-001", "L1-002", etc.
        // We need the slug to look them up. Use the session's slug if available.
        if let Some(ref slug_name) = session.slug {
            if let Ok(Some(node)) = pyramid_db::get_node(reader, slug_name, node_id) {
                let topics_str: String = node.topics.iter()
                    .map(|t| format!("  Topic: {} — {}", t.name, t.current))
                    .collect::<Vec<_>>()
                    .join("\n");

                parts.push(format!(
                    "[Hydrated Node {} (depth {})]\n{}\n{}\n",
                    node.id, node.depth, node.distilled, topics_str,
                ));
            }
        }
    }

    // Lifted results (from previous mid-turn queries)
    for lifted in &session.lifted_results {
        parts.push(format!(
            "[Lifted Query Result: \"{}\"]\n{}\n",
            lifted.query, lifted.result,
        ));
    }

    if parts.is_empty() {
        return String::new();
    }

    format!("## Hydrated Content & Lifted Results\n\n{}", parts.join("\n"))
}

// ── Full Context Assembly ───────────────────────────────────────────

/// Assemble the full context window for an LLM call.
///
/// Returns a Vec<LlmMessage> in the multi-turn format expected by OpenRouter.
/// Sections 1-3 and 5 go into the system message (for caching).
/// Section 4 becomes the conversation history messages.
/// Section 6 is the current user message.
pub fn assemble_context_window(
    reader: &Connection,
    session: &Session,
    user_message: &str,
) -> Vec<LlmMessage> {
    let mut messages: Vec<LlmMessage> = Vec::new();

    // §1. System prompt (stable, always cached)
    let mut system_content = String::from(DENNIS_SYSTEM_PROMPT);

    // §2. Navigation skeleton (changes on rebuild)
    let nav_skeleton = build_nav_skeleton(reader, NAV_SKELETON_BUDGET);
    if !nav_skeleton.is_empty() {
        system_content.push_str("\n\n## Navigation Skeleton (your complete knowledge map)\n\n");
        system_content.push_str(&nav_skeleton);
    }

    // §3. Session topics (warm layer, append-only)
    let session_topics = build_session_topics(session);
    if !session_topics.is_empty() {
        system_content.push_str("\n\n");
        system_content.push_str(&session_topics);
    }

    // §5. Hydrated content + lifted results (before conversation for cache stability)
    let hydrated = build_hydrated_content(reader, session);
    if !hydrated.is_empty() {
        system_content.push_str("\n\n");
        system_content.push_str(&hydrated);
    }

    // Push the system message
    messages.push(LlmMessage {
        role: "system".to_string(),
        content: system_content,
    });

    // §4. Conversation history (multi-turn messages)
    let conv_messages = build_conversation_history(session);
    messages.extend(conv_messages);

    // §6. Current user message
    messages.push(LlmMessage {
        role: "user".to_string(),
        content: user_message.to_string(),
    });

    messages
}

// ── Context Schedule ────────────────────────────────────────────────

/// Apply a context schedule to a session's brain map.
///
/// Hydrates new nodes (adds to hydrated_node_ids).
/// Dehydrates old nodes (removes from hydrated_node_ids).
pub fn apply_context_schedule(
    session: &mut Session,
    hydrate: Vec<String>,
    dehydrate: Vec<String>,
) {
    // Remove dehydrated nodes
    session.hydrated_node_ids.retain(|id| !dehydrate.contains(id));

    // Add hydrated nodes (avoid duplicates)
    for id in hydrate {
        if !session.hydrated_node_ids.contains(&id) {
            session.hydrated_node_ids.push(id);
        }
    }
}
