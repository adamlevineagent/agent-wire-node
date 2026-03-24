// pyramid/webbing.rs — Cross-thread web edge delta chain management
//
// Functions:
//   process_web_edge_notes — process web edge notes from distillation rewrite
//   collapse_web_edge      — collapse accumulated edge deltas via LLM
//   decay_web_edges         — reduce relevance of stale edges
//   get_active_edges        — return edges above relevance threshold

use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::pyramid::config_helper::config_for_model;
use crate::pyramid::db;
use crate::pyramid::llm;
use crate::pyramid::types::*;

// ── Constants ────────────────────────────────────────────────────────────────

const WEB_EDGE_COLLAPSE_THRESHOLD: i64 = 20;
#[allow(dead_code)]
const MAX_EDGES_PER_THREAD: usize = 10;
const EDGE_DECAY_RATE: f64 = 0.05;
const EDGE_MIN_RELEVANCE: f64 = 0.1;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Generate a timestamp string.
fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── process_web_edge_notes ───────────────────────────────────────────────────

/// Processes web edge notes emitted by `rewrite_distillation`.
///
/// For each note:
/// 1. Normalizes edge direction (thread_a_id < thread_b_id alphabetically)
/// 2. Looks up existing edge between the two threads
/// 3. If edge exists: creates a WebEdgeDelta, increments delta_count, boosts relevance
/// 4. If edge doesn't exist: creates a new WebEdge with the relationship
pub async fn process_web_edge_notes(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    source_thread_id: &str,
    notes: &[WebEdgeNote],
) -> anyhow::Result<()> {
    for note in notes {
        // Skip self-references
        if note.thread_id == source_thread_id {
            continue;
        }

        // 1. Normalize edge direction: ensure thread_a_id < thread_b_id
        let (thread_a, thread_b) = if source_thread_id < note.thread_id.as_str() {
            (source_thread_id.to_string(), note.thread_id.clone())
        } else {
            (note.thread_id.clone(), source_thread_id.to_string())
        };

        // 2. Look up existing edge
        let existing_edge = {
            let conn = reader.lock().await;
            db::get_web_edge_between(&conn, slug, &thread_a, &thread_b)?
        };

        match existing_edge {
            Some(edge) => {
                // 3. Edge exists: create delta, increment count, boost relevance
                let delta = WebEdgeDelta {
                    id: 0,
                    edge_id: edge.id,
                    content: note.relationship.clone(),
                    created_at: now_ts(),
                };

                let new_delta_count = edge.delta_count + 1;
                // Boost relevance toward 1.0 on each new note (diminishing returns)
                let new_relevance = (edge.relevance + 0.1).min(1.0);

                {
                    let conn = writer.lock().await;
                    db::save_web_edge_delta(&conn, &delta)?;
                    db::update_web_edge(
                        &conn,
                        edge.id,
                        &edge.relationship,
                        new_relevance,
                        new_delta_count,
                    )?;
                }

                info!(
                    "[webbing] added delta to edge {} <-> {} (count: {})",
                    thread_a, thread_b, new_delta_count
                );
            }
            None => {
                // 4. No edge exists: create new one
                let edge = WebEdge {
                    id: 0,
                    slug: slug.to_string(),
                    thread_a_id: thread_a.clone(),
                    thread_b_id: thread_b.clone(),
                    relationship: note.relationship.clone(),
                    relevance: 1.0,
                    delta_count: 0,
                    created_at: now_ts(),
                    updated_at: now_ts(),
                };

                {
                    let conn = writer.lock().await;
                    db::save_web_edge(&conn, &edge)?;
                }

                info!(
                    "[webbing] created new edge {} <-> {}: {}",
                    thread_a, thread_b, note.relationship
                );
            }
        }
    }

    Ok(())
}

// ── collapse_web_edge ────────────────────────────────────────────────────────

/// Collapses accumulated deltas on a web edge into a unified relationship description.
///
/// When an edge accumulates 20+ deltas:
/// 1. Loads the edge and all its deltas
/// 2. Calls LLM to produce a new unified relationship description
/// 3. Updates the edge relationship text
/// 4. Deletes absorbed deltas
/// 5. Resets delta_count
pub async fn collapse_web_edge(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    edge_id: i64,
    api_key: &str,
    model: &str,
) -> anyhow::Result<()> {
    // 1. Load edge and deltas
    let (edge, deltas) = {
        let conn = reader.lock().await;
        let edge = db::get_web_edge(&conn, edge_id)?
            .ok_or_else(|| anyhow::anyhow!("Web edge {} not found", edge_id))?;
        let deltas = db::get_web_edge_deltas(&conn, edge_id)?;
        (edge, deltas)
    };

    if (deltas.len() as i64) < WEB_EDGE_COLLAPSE_THRESHOLD {
        info!(
            "[webbing] edge {} has {} deltas, below collapse threshold ({})",
            edge_id,
            deltas.len(),
            WEB_EDGE_COLLAPSE_THRESHOLD
        );
        return Ok(());
    }

    // Resolve thread names for the prompt
    let (thread_a_name, thread_b_name) = {
        let conn = reader.lock().await;
        let a_name = db::get_thread(&conn, slug, &edge.thread_a_id)?
            .map(|t| t.thread_name)
            .unwrap_or_else(|| edge.thread_a_id.clone());
        let b_name = db::get_thread(&conn, slug, &edge.thread_b_id)?
            .map(|t| t.thread_name)
            .unwrap_or_else(|| edge.thread_b_id.clone());
        (a_name, b_name)
    };

    // Build delta contents for the prompt
    let delta_contents = deltas
        .iter()
        .map(|d| format!("- {}", d.content))
        .collect::<Vec<_>>()
        .join("\n");

    // 2. Call LLM
    let system_prompt =
        "You are summarizing how two knowledge threads are connected. Output JSON only.";
    let user_prompt = format!(
        r#"You are summarizing how two knowledge threads are connected.

THREAD A: {thread_a_name}
THREAD B: {thread_b_name}

CURRENT RELATIONSHIP:
{relationship}

ACCUMULATED CHANGES ({n} updates):
{delta_contents}

Produce a concise description of how these threads currently connect.
Focus on: shared entities, cascading decisions, dependency relationships.

Output JSON only:
{{"relationship": "how A and B connect right now"}}"#,
        thread_a_name = thread_a_name,
        thread_b_name = thread_b_name,
        relationship = edge.relationship,
        n = deltas.len(),
        delta_contents = delta_contents,
    );

    let cfg = config_for_model(api_key, model);
    let raw = llm::call_model(&cfg, system_prompt, &user_prompt, 0.2, 500).await?;
    let parsed = llm::extract_json(&raw)?;

    let new_relationship = parsed
        .get("relationship")
        .and_then(|v| v.as_str())
        .unwrap_or(&edge.relationship)
        .to_string();

    // 3-5. Update edge, delete deltas, reset count
    {
        let conn = writer.lock().await;
        db::update_web_edge(&conn, edge_id, &new_relationship, edge.relevance, 0)?;
        db::delete_web_edge_deltas(&conn, edge_id)?;
    }

    info!(
        "[webbing] collapsed edge {} ({} <-> {}): absorbed {} deltas",
        edge_id,
        thread_a_name,
        thread_b_name,
        deltas.len()
    );

    Ok(())
}

// ── check_and_collapse_edges ─────────────────────────────────────────────────

/// Checks all edges for a slug and collapses any that exceed the threshold.
pub async fn check_and_collapse_edges(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    api_key: &str,
    model: &str,
) -> anyhow::Result<usize> {
    let edges = {
        let conn = reader.lock().await;
        db::get_web_edges(&conn, slug)?
    };

    let mut collapsed = 0;
    for edge in &edges {
        if edge.delta_count >= WEB_EDGE_COLLAPSE_THRESHOLD {
            match collapse_web_edge(reader, writer, slug, edge.id, api_key, model).await {
                Ok(()) => collapsed += 1,
                Err(e) => warn!("[webbing] failed to collapse edge {}: {}", edge.id, e),
            }
        }
    }

    // After collapse cycle, decay all edges
    {
        let conn = writer.lock().await;
        let archived = db::decay_web_edges(&conn, slug, EDGE_DECAY_RATE)?;
        if archived > 0 {
            info!(
                "[webbing] archived {} stale edges (relevance < {})",
                archived, EDGE_MIN_RELEVANCE
            );
        }
    }

    Ok(collapsed)
}

// ── decay_web_edges (module-level wrapper) ───────────────────────────────────

/// Reduces relevance of stale edges. Wrapper around db::decay_web_edges.
pub fn decay_web_edges(conn: &Connection, slug: &str, decay_rate: f64) -> anyhow::Result<usize> {
    let archived = db::decay_web_edges(conn, slug, decay_rate)?;
    Ok(archived)
}

// ── get_active_edges (module-level wrapper) ──────────────────────────────────

/// Returns edges above the given relevance threshold.
pub fn get_active_edges(
    conn: &Connection,
    slug: &str,
    min_relevance: f64,
) -> anyhow::Result<Vec<WebEdge>> {
    let edges = db::get_active_edges(conn, slug, min_relevance)?;
    Ok(edges)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_direction_normalization() {
        // Verify alphabetical ordering logic
        let (a, b) = if "thread-alpha" < "thread-beta" {
            ("thread-alpha", "thread-beta")
        } else {
            ("thread-beta", "thread-alpha")
        };
        assert_eq!(a, "thread-alpha");
        assert_eq!(b, "thread-beta");

        // Reverse case
        let (a, b) = if "thread-zeta" < "thread-alpha" {
            ("thread-zeta", "thread-alpha")
        } else {
            ("thread-alpha", "thread-zeta")
        };
        assert_eq!(a, "thread-alpha");
        assert_eq!(b, "thread-zeta");
    }

    #[test]
    fn test_decay_web_edges_in_memory() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        // Insert prerequisite slug + nodes (FK: threads.current_canonical_id → nodes.id)
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES ('test', 'code', '/tmp')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline) VALUES ('node-1', 'test', 1, 'Node 1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline) VALUES ('node-2', 'test', 1, 'Node 2')",
            [],
        ).unwrap();

        // Create two threads first (needed for FK constraints)
        let thread_a = PyramidThread {
            slug: "test".into(),
            thread_id: "thread-aaa".into(),
            thread_name: "Thread A".into(),
            current_canonical_id: "node-1".into(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        let thread_b = PyramidThread {
            slug: "test".into(),
            thread_id: "thread-bbb".into(),
            thread_name: "Thread B".into(),
            current_canonical_id: "node-2".into(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        db::save_thread(&conn, &thread_a).unwrap();
        db::save_thread(&conn, &thread_b).unwrap();

        // Create an edge with low relevance — use 0.20 to avoid IEEE 754 rounding
        // (0.15 - 0.05 = 0.09999999999999998 in float, which is < 0.1)
        let edge = WebEdge {
            id: 0,
            slug: "test".into(),
            thread_a_id: "thread-aaa".into(),
            thread_b_id: "thread-bbb".into(),
            relationship: "test relationship".into(),
            relevance: 0.20,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        db::save_web_edge(&conn, &edge).unwrap();

        // First decay: 0.20 - 0.05 = 0.15, still >= 0.1, not archived
        let archived = decay_web_edges(&conn, "test", EDGE_DECAY_RATE).unwrap();
        assert_eq!(archived, 0);

        // Second decay: 0.15 - 0.05 = 0.10, still >= 0.1 (exact in float), not archived
        let archived = decay_web_edges(&conn, "test", EDGE_DECAY_RATE).unwrap();
        assert_eq!(archived, 0);

        // Third decay: 0.10 - 0.05 = 0.05, below 0.1, should be archived
        let archived = decay_web_edges(&conn, "test", EDGE_DECAY_RATE).unwrap();
        assert_eq!(archived, 1);

        // No edges should remain
        let edges = get_active_edges(&conn, "test", EDGE_MIN_RELEVANCE).unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_web_edge_delta_crud() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        // Insert prerequisite slug + nodes (FK: threads.current_canonical_id → nodes.id)
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES ('test', 'code', '/tmp')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline) VALUES ('node-1', 'test', 1, 'Node 1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline) VALUES ('node-2', 'test', 1, 'Node 2')",
            [],
        ).unwrap();

        // Create threads
        let thread_a = PyramidThread {
            slug: "test".into(),
            thread_id: "thread-aaa".into(),
            thread_name: "Thread A".into(),
            current_canonical_id: "node-1".into(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        let thread_b = PyramidThread {
            slug: "test".into(),
            thread_id: "thread-bbb".into(),
            thread_name: "Thread B".into(),
            current_canonical_id: "node-2".into(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        db::save_thread(&conn, &thread_a).unwrap();
        db::save_thread(&conn, &thread_b).unwrap();

        // Create edge
        let edge = WebEdge {
            id: 0,
            slug: "test".into(),
            thread_a_id: "thread-aaa".into(),
            thread_b_id: "thread-bbb".into(),
            relationship: "initial".into(),
            relevance: 1.0,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        let edge_id = db::save_web_edge(&conn, &edge).unwrap();

        // Save deltas
        let delta1 = WebEdgeDelta {
            id: 0,
            edge_id,
            content: "first change".into(),
            created_at: now_ts(),
        };
        let delta2 = WebEdgeDelta {
            id: 0,
            edge_id,
            content: "second change".into(),
            created_at: now_ts(),
        };
        db::save_web_edge_delta(&conn, &delta1).unwrap();
        db::save_web_edge_delta(&conn, &delta2).unwrap();

        // Read deltas
        let deltas = db::get_web_edge_deltas(&conn, edge_id).unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content, "first change");
        assert_eq!(deltas[1].content, "second change");

        // Delete deltas
        let deleted = db::delete_web_edge_deltas(&conn, edge_id).unwrap();
        assert_eq!(deleted, 2);

        let deltas = db::get_web_edge_deltas(&conn, edge_id).unwrap();
        assert!(deltas.is_empty());
    }
}
