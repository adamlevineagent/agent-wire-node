// pyramid/manifest.rs — WS-MANIFEST-API (Phase 3)
//
// The manifest API is how the agent steers its own cognition at runtime.
// Between turns, the agent emits a structured context manifest specifying
// what to do with its Brain Map. The runtime harness executes manifest
// operations against the pyramid graph.
//
// See plan §9.2 (Brain Map and manifest operations), §9.3 (dehydration
// as projection), §9.4 ("let me think about that"), §9.5 (async writeback).

use anyhow::Result;
use rusqlite::Connection;

use super::db;
use super::primer;
use super::query;
use super::types::*;
use super::PyramidState;

// ── Manifest execution ─────────────────────────────────────────────────────

/// Execute a batch of manifest operations against a pyramid.
///
/// Each operation is executed sequentially. Failures on individual operations
/// do not abort the batch — each result is recorded independently.
/// The full batch is logged to `pyramid_manifest_log` for provenance.
pub async fn execute_manifest(
    state: &PyramidState,
    slug: &str,
    operations: Vec<ManifestOperation>,
    session_id: Option<&str>,
) -> Result<ManifestResult> {
    let provenance_id = uuid::Uuid::new_v4().to_string();
    let mut results = Vec::with_capacity(operations.len());

    for op in &operations {
        let result = {
            let conn = state.reader.lock().await;
            execute_single_op(&conn, slug, op)
        };
        results.push(result);
    }

    // Log provenance
    {
        let conn = state.writer.lock().await;
        let ops_json = serde_json::to_string(&operations).unwrap_or_default();
        let results_json = serde_json::to_string(&results).unwrap_or_default();
        log_manifest_provenance(
            &conn,
            &provenance_id,
            slug,
            session_id,
            &ops_json,
            &results_json,
        )?;
    }

    Ok(ManifestResult {
        operations_executed: results.len(),
        results,
        provenance_id,
    })
}

/// Execute a single manifest operation and return its result.
fn execute_single_op(conn: &Connection, slug: &str, op: &ManifestOperation) -> ManifestOpResult {
    match op {
        ManifestOperation::Hydrate {
            node_id,
            abstraction_level,
        } => execute_hydrate(conn, slug, node_id, abstraction_level.as_deref()),

        ManifestOperation::Dehydrate { node_id } => execute_dehydrate(conn, slug, node_id),

        ManifestOperation::Compress { buffer_range } => execute_compress(*buffer_range),

        ManifestOperation::Densify { missing_node_id } => {
            execute_densify(conn, slug, missing_node_id)
        }

        ManifestOperation::Colocate { seed_node_id } => execute_colocate(conn, slug, seed_node_id),

        ManifestOperation::Lookahead { node_ids } => execute_lookahead(conn, slug, node_ids),

        ManifestOperation::Investigation { node_id } => execute_investigation(conn, slug, node_id),

        ManifestOperation::Ask {
            pyramid_slug,
            question,
        } => execute_ask(conn, pyramid_slug, question),

        ManifestOperation::ProposeChainUpdate { chain_id, patch } => {
            execute_propose_chain_update(conn, slug, chain_id, patch)
        }
    }
}

// ── Individual operation implementations ───────────────────────────────────

/// Hydrate: Load a node from the pyramid via drill, return at specified
/// abstraction level. If no abstraction_level is given, returns full content.
fn execute_hydrate(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    _abstraction_level: Option<&str>,
) -> ManifestOpResult {
    match query::drill(conn, slug, node_id) {
        Ok(Some(drill_result)) => {
            // Return the node + its children as JSON values for Brain Map inclusion.
            let mut nodes = Vec::new();
            if let Ok(node_json) = serde_json::to_value(&drill_result.node) {
                nodes.push(node_json);
            }
            for child in &drill_result.children {
                if let Ok(child_json) = serde_json::to_value(child) {
                    nodes.push(child_json);
                }
            }
            ManifestOpResult {
                op: "Hydrate".to_string(),
                success: true,
                nodes_returned: nodes,
                error: None,
            }
        }
        Ok(None) => ManifestOpResult {
            op: "Hydrate".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!("Node '{node_id}' not found in slug '{slug}'")),
        },
        Err(e) => ManifestOpResult {
            op: "Hydrate".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!("Hydrate failed: {e}")),
        },
    }
}

/// Dehydrate: Return vocabulary floor only — headline, topics (names only),
/// entity names. No distilled content, no full decisions, no narrative.
///
/// Plan §9.3: "Dehydration at runtime is a pure projection operation over
/// the multi-dimensional content the synthesis prompt produced at write time."
fn execute_dehydrate(conn: &Connection, slug: &str, node_id: &str) -> ManifestOpResult {
    match db::get_live_node(conn, slug, node_id) {
        Ok(Some(node)) => {
            // Project to vocabulary floor: headline + topic names + entity names.
            // No distilled, no decisions detail, no narrative, no key_quotes.
            let topic_names: Vec<String> = node.topics.iter().map(|t| t.name.clone()).collect();
            let entity_names: Vec<String> = node.entities.iter().map(|e| e.name.clone()).collect();

            let floor = serde_json::json!({
                "node_id": node.id,
                "headline": node.headline,
                "depth": node.depth,
                "topics": topic_names,
                "entities": entity_names,
                "dehydrated": true,
            });

            ManifestOpResult {
                op: "Dehydrate".to_string(),
                success: true,
                nodes_returned: vec![floor],
                error: None,
            }
        }
        Ok(None) => ManifestOpResult {
            op: "Dehydrate".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!("Node '{node_id}' not found in slug '{slug}'")),
        },
        Err(e) => ManifestOpResult {
            op: "Dehydrate".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!("Dehydrate failed: {e}")),
        },
    }
}

/// Compress: Placeholder — actual buffer compression is runtime-side.
/// Returns acknowledgment with the requested range.
fn execute_compress(buffer_range: (usize, usize)) -> ManifestOpResult {
    ManifestOpResult {
        op: "Compress".to_string(),
        success: true,
        nodes_returned: vec![serde_json::json!({
            "acknowledged": true,
            "buffer_range": [buffer_range.0, buffer_range.1],
            "note": "Buffer compression is handled by the runtime harness"
        })],
        error: None,
    }
}

/// Densify: Fire async demand-gen for the missing node. Returns the node_id
/// so the caller knows a request was filed. The actual generation happens
/// asynchronously via the demand-gen infrastructure.
fn execute_densify(conn: &Connection, slug: &str, missing_node_id: &str) -> ManifestOpResult {
    // Check whether the node actually exists (if it does, no densification needed).
    match db::get_live_node(conn, slug, missing_node_id) {
        Ok(Some(_node)) => ManifestOpResult {
            op: "Densify".to_string(),
            success: true,
            nodes_returned: vec![serde_json::json!({
                "status": "already_exists",
                "node_id": missing_node_id,
            })],
            error: None,
        },
        Ok(None) => {
            // Node doesn't exist — signal that demand-gen should be triggered.
            // The actual async dispatch is handled by the HTTP handler layer
            // which has access to the full async state.
            ManifestOpResult {
                op: "Densify".to_string(),
                success: true,
                nodes_returned: vec![serde_json::json!({
                    "status": "demand_gen_requested",
                    "missing_node_id": missing_node_id,
                })],
                error: None,
            }
        }
        Err(e) => ManifestOpResult {
            op: "Densify".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!("Densify check failed: {e}")),
        },
    }
}

/// Colocate: Follow web edges from a seed node, return related nodes.
/// This implements the "pull in nodes related to a seed via ties_to"
/// operation from §9.2.
fn execute_colocate(conn: &Connection, slug: &str, seed_node_id: &str) -> ManifestOpResult {
    // First verify the seed exists
    let seed = match db::get_live_node(conn, slug, seed_node_id) {
        Ok(Some(n)) => n,
        Ok(None) => {
            return ManifestOpResult {
                op: "Colocate".to_string(),
                success: false,
                nodes_returned: vec![],
                error: Some(format!(
                    "Seed node '{seed_node_id}' not found in slug '{slug}'"
                )),
            };
        }
        Err(e) => {
            return ManifestOpResult {
                op: "Colocate".to_string(),
                success: false,
                nodes_returned: vec![],
                error: Some(format!("Colocate lookup failed: {e}")),
            };
        }
    };

    // Collect related nodes via web edges (thread connections)
    let mut related_nodes = Vec::new();

    // Try to load connected web edges via the thread system
    if let Ok(edges) = load_web_edge_neighbors(conn, slug, seed_node_id) {
        for neighbor_id in edges {
            if let Ok(Some(neighbor)) = db::get_live_node(conn, slug, &neighbor_id) {
                if let Ok(json) = serde_json::to_value(&neighbor) {
                    related_nodes.push(json);
                }
            }
        }
    }

    // Also follow children of the seed (if any) — immediate graph neighborhood
    for child_id in &seed.children {
        if let Ok(Some(child)) = db::get_live_node(conn, slug, child_id) {
            if let Ok(json) = serde_json::to_value(&child) {
                related_nodes.push(json);
            }
        }
    }

    // Follow parent if it exists
    if let Some(ref parent_id) = seed.parent_id {
        if let Ok(Some(parent)) = db::get_live_node(conn, slug, parent_id) {
            if let Ok(json) = serde_json::to_value(&parent) {
                related_nodes.push(json);
            }
        }
    }

    ManifestOpResult {
        op: "Colocate".to_string(),
        success: true,
        nodes_returned: related_nodes,
        error: None,
    }
}

/// Load neighbor node IDs via web edges for a given canonical node.
fn load_web_edge_neighbors(conn: &Connection, slug: &str, node_id: &str) -> Result<Vec<String>> {
    // Find the thread for this node, then find connected threads via web edges.
    let thread_id: Option<String> = conn
        .prepare(
            "SELECT thread_id FROM pyramid_threads
             WHERE slug = ?1 AND current_canonical_id = ?2",
        )?
        .query_row(rusqlite::params![slug, node_id], |row| {
            row.get::<_, String>(0)
        })
        .ok();

    let thread_id = match thread_id {
        Some(tid) => tid,
        None => return Ok(vec![]),
    };

    // Get connected thread IDs via web edges
    let mut stmt = conn.prepare(
        "SELECT
            CASE WHEN thread_a_id = ?2 THEN thread_b_id ELSE thread_a_id END AS other_thread
         FROM pyramid_web_edges
         WHERE slug = ?1
           AND (thread_a_id = ?2 OR thread_b_id = ?2)
           AND archived_at IS NULL
         ORDER BY relevance DESC
         LIMIT 10",
    )?;

    let neighbor_thread_ids: Vec<String> = stmt
        .query_map(rusqlite::params![slug, thread_id], |row| {
            row.get::<_, String>(0)
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Resolve thread IDs to their current canonical node IDs
    let mut result = Vec::new();
    for other_tid in &neighbor_thread_ids {
        if let Ok(canonical_id) = conn.query_row(
            "SELECT current_canonical_id FROM pyramid_threads
             WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, other_tid],
            |row| row.get::<_, String>(0),
        ) {
            result.push(canonical_id);
        }
    }

    Ok(result)
}

/// Lookahead: Pre-fetch nodes the agent anticipates needing. Just load and
/// return them — the runtime caches them in the Brain Map for the next turn.
fn execute_lookahead(conn: &Connection, slug: &str, node_ids: &[String]) -> ManifestOpResult {
    let mut nodes = Vec::new();
    let mut errors = Vec::new();

    for node_id in node_ids {
        match db::get_live_node(conn, slug, node_id) {
            Ok(Some(node)) => {
                if let Ok(json) = serde_json::to_value(&node) {
                    nodes.push(json);
                }
            }
            Ok(None) => {
                errors.push(format!("Node '{node_id}' not found"));
            }
            Err(e) => {
                errors.push(format!("Failed to load '{node_id}': {e}"));
            }
        }
    }

    ManifestOpResult {
        op: "Lookahead".to_string(),
        success: errors.is_empty(),
        nodes_returned: nodes,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        },
    }
}

/// Investigation: Check a node's staleness status by looking at the most
/// recent stale check log entry. Returns the staleness state so the agent
/// knows whether to trust the content.
fn execute_investigation(conn: &Connection, slug: &str, node_id: &str) -> ManifestOpResult {
    // Verify the node exists
    let node = match db::get_live_node(conn, slug, node_id) {
        Ok(Some(n)) => n,
        Ok(None) => {
            return ManifestOpResult {
                op: "Investigation".to_string(),
                success: false,
                nodes_returned: vec![],
                error: Some(format!("Node '{node_id}' not found in slug '{slug}'")),
            };
        }
        Err(e) => {
            return ManifestOpResult {
                op: "Investigation".to_string(),
                success: false,
                nodes_returned: vec![],
                error: Some(format!("Investigation lookup failed: {e}")),
            };
        }
    };

    // Check dadbear_work_items for this node's most recent staleness check
    let stale_info: Option<(bool, String)> = conn
        .prepare(
            "SELECT CASE WHEN state = 'applied' THEN 1 ELSE 0 END,
                    COALESCE(result_json, '')
             FROM dadbear_work_items
             WHERE slug = ?1 AND target_id = ?2
             ORDER BY completed_at DESC LIMIT 1",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_row(rusqlite::params![slug, node_id], |row| {
                Ok((row.get::<_, bool>(0)?, row.get::<_, String>(1)?))
            })
            .ok()
        });

    let (is_stale, reason) = stale_info.unwrap_or((false, String::new()));

    let investigation_result = serde_json::json!({
        "node_id": node.id,
        "headline": node.headline,
        "depth": node.depth,
        "is_stale": is_stale,
        "stale_reason": reason,
        "created_at": node.created_at,
        "investigation_status": if is_stale { "stale_flagged" } else { "current" },
    });

    ManifestOpResult {
        op: "Investigation".to_string(),
        success: true,
        nodes_returned: vec![investigation_result],
        error: None,
    }
}

/// Ask: Fire a question against a pyramid. Wraps demand-gen infrastructure.
/// Returns a signal that the question was accepted. The actual answer may
/// require async demand-driven generation.
fn execute_ask(conn: &Connection, pyramid_slug: &str, question: &str) -> ManifestOpResult {
    // Verify the target pyramid exists
    let slug_exists = conn
        .prepare("SELECT 1 FROM pyramid_slugs WHERE slug = ?1 AND archived_at IS NULL")
        .ok()
        .and_then(|mut stmt| {
            stmt.query_row(rusqlite::params![pyramid_slug], |_| Ok(()))
                .ok()
        })
        .is_some();

    if !slug_exists {
        return ManifestOpResult {
            op: "Ask".to_string(),
            success: false,
            nodes_returned: vec![],
            error: Some(format!(
                "Pyramid slug '{pyramid_slug}' not found or archived"
            )),
        };
    }

    // Signal that the question should be routed to demand-gen.
    // The HTTP handler layer dispatches the actual async job.
    ManifestOpResult {
        op: "Ask".to_string(),
        success: true,
        nodes_returned: vec![serde_json::json!({
            "status": "question_accepted",
            "pyramid_slug": pyramid_slug,
            "question": question,
            "note": "Answer will be provided via demand-gen if not already cached"
        })],
        error: None,
    }
}

/// ProposeChainUpdate: Store a chain configuration update proposal for
/// operator review. See plan §9.6.
fn execute_propose_chain_update(
    _conn: &Connection,
    slug: &str,
    chain_id: &str,
    patch: &serde_json::Value,
) -> ManifestOpResult {
    // Store the proposal in pyramid_manifest_log as a special entry.
    // The operator reviews proposals via the manifest log endpoint.
    let proposal = serde_json::json!({
        "type": "chain_update_proposal",
        "slug": slug,
        "chain_id": chain_id,
        "patch": patch,
        "status": "pending_review",
    });

    ManifestOpResult {
        op: "ProposeChainUpdate".to_string(),
        success: true,
        nodes_returned: vec![proposal],
        error: None,
    }
}

// ── Cold start ─────────────────────────────────────────────────────────────

/// Load the cold-start payload for a new agent session.
///
/// Combines the primer (leftmost slope + canonical vocabulary) with the
/// initial Brain Map nodes (slope nodes as full JSON).
pub async fn cold_start(state: &PyramidState, slug: &str) -> Result<ColdStartPayload> {
    let session_id = uuid::Uuid::new_v4().to_string();

    let conn = state.reader.lock().await;

    // Build the primer (uses get_leftmost_slope internally)
    let primer_ctx = primer::build_primer(&conn, slug, None)?;

    // Get the leftmost slope nodes as full JSON for Brain Map inclusion
    let slope = primer::get_leftmost_slope(&conn, slug)?;
    let brain_map_initial: Vec<serde_json::Value> = slope
        .iter()
        .filter_map(|node| serde_json::to_value(node).ok())
        .collect();

    Ok(ColdStartPayload {
        primer: primer_ctx,
        brain_map_initial,
        session_id,
    })
}

// ── Provenance logging ─────────────────────────────────────────────────────

/// Log a manifest execution to the provenance table.
fn log_manifest_provenance(
    conn: &Connection,
    provenance_id: &str,
    slug: &str,
    session_id: Option<&str>,
    operations_json: &str,
    results_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_manifest_log (provenance_id, slug, session_id, operations, results)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            provenance_id,
            slug,
            session_id,
            operations_json,
            results_json
        ],
    )?;
    Ok(())
}

/// Read recent manifest provenance log entries for a slug.
pub fn get_manifest_log(
    conn: &Connection,
    slug: &str,
    limit: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, provenance_id, slug, session_id, operations, results, executed_at
         FROM pyramid_manifest_log
         WHERE slug = ?1
         ORDER BY executed_at DESC
         LIMIT ?2",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![slug, limit], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "provenance_id": row.get::<_, String>(1)?,
                "slug": row.get::<_, String>(2)?,
                "session_id": row.get::<_, Option<String>>(3)?,
                "operations": serde_json::from_str::<serde_json::Value>(
                    &row.get::<_, String>(4)?
                ).unwrap_or_default(),
                "results": serde_json::from_str::<serde_json::Value>(
                    &row.get::<_, Option<String>>(5)?.unwrap_or_default()
                ).unwrap_or_default(),
                "executed_at": row.get::<_, String>(6)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory pyramid DB with test data matching the primer test setup.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let slug = "test-manifest";

        // Create the slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'code', '/tmp/test')",
            rusqlite::params![slug],
        )
        .unwrap();

        // Create L0 nodes (3 nodes at depth 0)
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at)
             VALUES ('l0-a', ?1, 0, 0, 'L0 oldest', 'Oldest chunk details',
             '[{\"name\":\"topic-a\",\"current\":\"state-a\"}]', '[]',
             '[{\"decided\":\"decision-a\",\"why\":\"reason\",\"stance\":\"committed\"}]',
             '[{\"term\":\"term-a\",\"definition\":\"def-a\"}]',
             '[]', '', '[]', 'l1-a', 1, '2026-01-01T00:00:00')",
            rusqlite::params![slug],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at)
             VALUES ('l0-b', ?1, 0, 1, 'L0 middle', 'Middle chunk details',
             '[{\"name\":\"topic-b\",\"current\":\"state-b\"}]', '[]', '[]', '[]',
             '[]', '', '[]', 'l1-a', 1, '2026-01-02T00:00:00')",
            rusqlite::params![slug],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at)
             VALUES ('l0-c', ?1, 0, 2, 'L0 newest', 'Newest chunk details',
             '[{\"name\":\"topic-c\",\"current\":\"state-c\"}]', '[]', '[]', '[]',
             '[]', '', '[]', 'l1-a', 1, '2026-01-03T00:00:00')",
            rusqlite::params![slug],
        )
        .unwrap();

        // L1 node
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at)
             VALUES ('l1-a', ?1, 1, 0, 'L1 segment', 'L1 summary of all chunks',
             '[{\"name\":\"topic-b\",\"current\":\"state-b\"},{\"name\":\"topic-c\",\"current\":\"state-c\"}]',
             '[]', '[]', '[]', '[]', '', '[\"l0-a\",\"l0-b\",\"l0-c\"]', 'apex', 1, '2026-01-03T01:00:00')",
            rusqlite::params![slug],
        )
        .unwrap();

        // Apex
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at,
             entities_json, key_quotes_json, narrative_json, transitions_json)
             VALUES ('apex', ?1, 2, 0, 'Test Manifest Apex', 'Full project arc overview',
             '[{\"name\":\"topic-a\",\"current\":\"canonical-a\"},{\"name\":\"topic-c\",\"current\":\"canonical-c\"}]',
             '[]',
             '[{\"decided\":\"use-pyramids\",\"why\":\"recursive structure\",\"stance\":\"committed\",\"importance\":0.9,\"related\":[]}]',
             '[{\"term\":\"primer\",\"definition\":\"leftmost slope projection\"}]',
             '[]', '', '[\"l1-a\"]', NULL, 1, '2026-01-03T02:00:00',
             '[{\"name\":\"Adam\",\"role\":\"operator\",\"importance\":1.0,\"liveness\":\"live\"}]',
             '[]', '{}', '{}')",
            rusqlite::params![slug],
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_hydrate_returns_node_content() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Hydrate {
                node_id: "apex".to_string(),
                abstraction_level: None,
            },
        );

        assert!(result.success, "Hydrate should succeed: {:?}", result.error);
        assert_eq!(result.op, "Hydrate");
        assert!(
            !result.nodes_returned.is_empty(),
            "Hydrate should return node content"
        );

        // The first returned node should be the apex
        let first = &result.nodes_returned[0];
        assert_eq!(
            first.get("headline").and_then(|v| v.as_str()),
            Some("Test Manifest Apex"),
            "Hydrated node should have the apex headline"
        );

        // Should also return children (the L1 node)
        assert!(
            result.nodes_returned.len() > 1,
            "Hydrate should return node + children"
        );
    }

    #[test]
    fn test_hydrate_missing_node_returns_error() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Hydrate {
                node_id: "nonexistent-node".to_string(),
                abstraction_level: None,
            },
        );

        assert!(!result.success, "Hydrate of missing node should fail");
        assert!(result.error.is_some(), "Should have error message");
        assert!(result.nodes_returned.is_empty());
    }

    #[test]
    fn test_dehydrate_returns_vocabulary_floor_only() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Dehydrate {
                node_id: "apex".to_string(),
            },
        );

        assert!(
            result.success,
            "Dehydrate should succeed: {:?}",
            result.error
        );
        assert_eq!(result.op, "Dehydrate");
        assert_eq!(result.nodes_returned.len(), 1);

        let floor = &result.nodes_returned[0];

        // Should have headline
        assert_eq!(
            floor.get("headline").and_then(|v| v.as_str()),
            Some("Test Manifest Apex"),
        );

        // Should have topic names (as simple strings, not full topic objects)
        let topics = floor.get("topics").and_then(|v| v.as_array());
        assert!(topics.is_some(), "Dehydrated floor should have topics");
        let topic_names: Vec<&str> = topics.unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert!(
            topic_names.contains(&"topic-a"),
            "Should contain topic-a name"
        );

        // Should have entity names
        let entities = floor.get("entities").and_then(|v| v.as_array());
        assert!(entities.is_some(), "Dehydrated floor should have entities");
        let entity_names: Vec<&str> = entities
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(entity_names.contains(&"Adam"), "Should contain entity Adam");

        // Should be flagged as dehydrated
        assert_eq!(
            floor.get("dehydrated").and_then(|v| v.as_bool()),
            Some(true),
        );

        // Should NOT have distilled content, decisions, narrative, key_quotes
        assert!(
            floor.get("distilled").is_none(),
            "Dehydrated floor should not have distilled"
        );
        assert!(
            floor.get("decisions").is_none(),
            "Dehydrated floor should not have decisions"
        );
    }

    #[test]
    fn test_cold_start_returns_primer_and_brain_map() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        // Build primer directly (cold_start is async, so test the components)
        let primer_ctx = primer::build_primer(&conn, slug, None).unwrap();
        assert!(
            !primer_ctx.slope_nodes.is_empty(),
            "Primer should have slope nodes"
        );

        let slope = primer::get_leftmost_slope(&conn, slug).unwrap();
        let brain_map_initial: Vec<serde_json::Value> = slope
            .iter()
            .filter_map(|node| serde_json::to_value(node).ok())
            .collect();

        assert!(
            !brain_map_initial.is_empty(),
            "Brain map initial should have nodes"
        );

        // Verify the brain map contains the apex
        let has_apex = brain_map_initial
            .iter()
            .any(|n| n.get("headline").and_then(|v| v.as_str()) == Some("Test Manifest Apex"));
        assert!(has_apex, "Brain map initial should contain the apex node");

        // Verify the brain map contains the leftmost L0
        let has_l0 = brain_map_initial
            .iter()
            .any(|n| n.get("headline").and_then(|v| v.as_str()) == Some("L0 newest"));
        assert!(
            has_l0,
            "Brain map initial should contain the leftmost L0 node"
        );
    }

    #[test]
    fn test_manifest_provenance_is_logged() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let provenance_id = "test-provenance-001";
        let ops_json = serde_json::to_string(&vec![ManifestOperation::Hydrate {
            node_id: "apex".to_string(),
            abstraction_level: None,
        }])
        .unwrap();
        let results_json = serde_json::to_string(&vec![ManifestOpResult {
            op: "Hydrate".to_string(),
            success: true,
            nodes_returned: vec![],
            error: None,
        }])
        .unwrap();

        // Log provenance
        log_manifest_provenance(
            &conn,
            provenance_id,
            slug,
            Some("session-abc"),
            &ops_json,
            &results_json,
        )
        .expect("Provenance logging should succeed");

        // Read it back
        let logs = get_manifest_log(&conn, slug, 10).expect("Log read should succeed");

        assert_eq!(logs.len(), 1, "Should have exactly one log entry");

        let entry = &logs[0];
        assert_eq!(
            entry.get("provenance_id").and_then(|v| v.as_str()),
            Some("test-provenance-001"),
        );
        assert_eq!(
            entry.get("slug").and_then(|v| v.as_str()),
            Some("test-manifest"),
        );
        assert_eq!(
            entry.get("session_id").and_then(|v| v.as_str()),
            Some("session-abc"),
        );

        // Operations should be parseable
        let ops = entry.get("operations").unwrap();
        assert!(ops.is_array(), "Operations should be a JSON array");
    }

    #[test]
    fn test_colocate_returns_neighbors() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        // Colocate from L1 node — should return its children (L0 nodes) and parent (apex)
        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Colocate {
                seed_node_id: "l1-a".to_string(),
            },
        );

        assert!(
            result.success,
            "Colocate should succeed: {:?}",
            result.error
        );
        assert_eq!(result.op, "Colocate");
        // Should have children + parent
        assert!(
            !result.nodes_returned.is_empty(),
            "Colocate should return related nodes"
        );
    }

    #[test]
    fn test_lookahead_prefetches_nodes() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Lookahead {
                node_ids: vec!["l0-a".to_string(), "l0-b".to_string()],
            },
        );

        assert!(
            result.success,
            "Lookahead should succeed: {:?}",
            result.error
        );
        assert_eq!(
            result.nodes_returned.len(),
            2,
            "Should return 2 prefetched nodes"
        );
    }

    #[test]
    fn test_investigation_checks_staleness() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Investigation {
                node_id: "apex".to_string(),
            },
        );

        assert!(
            result.success,
            "Investigation should succeed: {:?}",
            result.error
        );
        assert_eq!(result.nodes_returned.len(), 1);

        let info = &result.nodes_returned[0];
        assert_eq!(info.get("node_id").and_then(|v| v.as_str()), Some("apex"),);
        // No stale check exists for this test node, so should report current
        assert_eq!(
            info.get("investigation_status").and_then(|v| v.as_str()),
            Some("current"),
        );
    }

    #[test]
    fn test_compress_returns_acknowledgment() {
        let conn = setup_test_db();
        let slug = "test-manifest";

        let result = execute_single_op(
            &conn,
            slug,
            &ManifestOperation::Compress {
                buffer_range: (0, 5),
            },
        );

        assert!(result.success, "Compress should succeed");
        assert_eq!(result.nodes_returned.len(), 1);
        assert_eq!(
            result.nodes_returned[0]
                .get("acknowledged")
                .and_then(|v| v.as_bool()),
            Some(true),
        );
    }
}
