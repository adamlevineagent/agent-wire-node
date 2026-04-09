// pyramid/query.rs — Query functions for the Knowledge Pyramid
//
// All queries operate on `live_pyramid_nodes` view (excludes superseded and provisional nodes).
// JSON columns (topics, corrections, decisions, terms, dead_ends, children)
// are parsed with serde_json.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::warn;

use super::db;
use super::types::*;

// ── Query-specific response types ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedCorrection {
    pub current: String,
    pub was: String,
    pub chain: Vec<String>,
    pub who: String,
    pub source_node: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionWithSource {
    pub wrong: String,
    pub right: String,
    pub who: String,
    pub node_id: String,
    pub depth: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermWithSource {
    pub term: String,
    pub definition: String,
    pub node_id: String,
    pub depth: i64,
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Delegate to the canonical node_from_row in db.rs.
use super::db::node_from_row as row_to_node;

/// WS-SCHEMA-V2 (§15.7): fetch a specific historical version of a node from
/// `pyramid_node_versions`. Returns `Ok(None)` if no such version exists.
///
/// To fetch the current canonical state, call `db::get_node` — the versions
/// table never holds the live tip row, only prior snapshots.
pub fn get_node_version(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    version: i64,
) -> Result<Option<PyramidNode>> {
    let sql = "SELECT slug, node_id, version, headline, distilled,
                      topics, corrections, decisions, terms, dead_ends,
                      self_prompt, children, parent_id,
                      time_range_start, time_range_end, weight,
                      narrative_json, entities_json, key_quotes_json, transitions_json,
                      chain_phase, build_id, supersession_reason, created_at
               FROM pyramid_node_versions
               WHERE slug = ?1 AND node_id = ?2 AND version = ?3";
    let mut stmt = conn.prepare(sql)?;
    let result = stmt.query_row(rusqlite::params![slug, node_id, version], |row| {
        let topics_json: String = row
            .get::<_, Option<String>>("topics")?
            .unwrap_or_default();
        let corrections_json: String = row
            .get::<_, Option<String>>("corrections")?
            .unwrap_or_default();
        let decisions_json: String = row
            .get::<_, Option<String>>("decisions")?
            .unwrap_or_default();
        let terms_json: String = row
            .get::<_, Option<String>>("terms")?
            .unwrap_or_default();
        let dead_ends_json: String = row
            .get::<_, Option<String>>("dead_ends")?
            .unwrap_or_default();
        let children_json: String = row
            .get::<_, Option<String>>("children")?
            .unwrap_or_default();

        let narrative: NarrativeMultiZoom = row
            .get::<_, Option<String>>("narrative_json")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let entities: Vec<Entity> = row
            .get::<_, Option<String>>("entities_json")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let key_quotes: Vec<KeyQuote> = row
            .get::<_, Option<String>>("key_quotes_json")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let transitions: Transitions = row
            .get::<_, Option<String>>("transitions_json")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let start = row.get::<_, Option<String>>("time_range_start")?;
        let end = row.get::<_, Option<String>>("time_range_end")?;
        let time_range = if start.is_some() || end.is_some() {
            Some(TimeRange { start, end })
        } else {
            None
        };

        Ok(PyramidNode {
            id: row.get::<_, String>("node_id")?,
            slug: row.get::<_, String>("slug")?,
            depth: 0, // depth isn't snapshotted — callers query the live row
            chunk_index: None,
            headline: row.get::<_, String>("headline").unwrap_or_default(),
            distilled: row.get::<_, String>("distilled").unwrap_or_default(),
            topics: serde_json::from_str(&topics_json).unwrap_or_default(),
            corrections: serde_json::from_str(&corrections_json).unwrap_or_default(),
            decisions: serde_json::from_str(&decisions_json).unwrap_or_default(),
            terms: serde_json::from_str(&terms_json).unwrap_or_default(),
            dead_ends: serde_json::from_str(&dead_ends_json).unwrap_or_default(),
            self_prompt: row
                .get::<_, Option<String>>("self_prompt")?
                .unwrap_or_default(),
            children: serde_json::from_str(&children_json).unwrap_or_default(),
            parent_id: row.get::<_, Option<String>>("parent_id")?,
            superseded_by: None,
            build_id: row.get::<_, Option<String>>("build_id")?,
            created_at: row
                .get::<_, Option<String>>("created_at")?
                .unwrap_or_default(),
            time_range,
            weight: row.get::<_, Option<f64>>("weight")?.unwrap_or(1.0),
            provisional: false,
            promoted_from: None,
            narrative,
            entities,
            key_quotes,
            transitions,
            current_version: row.get::<_, i64>("version")?,
            current_version_chain_phase: row.get::<_, Option<String>>("chain_phase")?,
        })
    });

    match result {
        Ok(n) => Ok(Some(n)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Collect all corrections from a node — from both top-level `corrections`
/// and from `topics[].corrections`.
fn collect_corrections(node: &PyramidNode) -> Vec<&Correction> {
    let mut out: Vec<&Correction> = node.corrections.iter().collect();
    for topic in &node.topics {
        out.extend(topic.corrections.iter());
    }
    out
}

/// Collect all entities from a node — from `topics[].entities` and `terms[].term`.
fn collect_entities(node: &PyramidNode) -> Vec<(String, String)> {
    // Returns (entity_name, topic_or_context_name)
    let mut out = Vec::new();
    for topic in &node.topics {
        let topic_name = &topic.name;
        for entity in &topic.entities {
            out.push((entity.clone(), topic_name.clone()));
        }
    }
    // Also treat terms as entities (for legacy/non-topic nodes)
    for t in &node.terms {
        out.push((t.term.clone(), "terms".to_string()));
    }
    out
}

fn load_connected_web_edges(
    conn: &Connection,
    slug: &str,
    canonical_node_id: &str,
) -> Result<Vec<ConnectedWebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT
            other.current_canonical_id AS connected_to,
            node.headline AS connected_headline,
            edge.relationship,
            edge.relevance
         FROM pyramid_threads AS current
         JOIN pyramid_web_edges AS edge
           ON edge.slug = current.slug
          AND (edge.thread_a_id = current.thread_id OR edge.thread_b_id = current.thread_id)
          AND edge.archived_at IS NULL
         JOIN pyramid_threads AS other
           ON other.slug = current.slug
          AND other.thread_id = CASE
                WHEN edge.thread_a_id = current.thread_id THEN edge.thread_b_id
                ELSE edge.thread_a_id
              END
         JOIN live_pyramid_nodes AS node
           ON node.slug = other.slug
          AND node.id = other.current_canonical_id
         WHERE current.slug = ?1
           AND current.current_canonical_id = ?2
         ORDER BY edge.relevance DESC, connected_to ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, canonical_node_id], |row| {
        Ok(ConnectedWebEdge {
            connected_to: row.get(0)?,
            connected_headline: row.get(1)?,
            relationship: row.get(2)?,
            strength: row.get(3)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// WS-ONLINE-F: Load remote web edges for a node's thread.
///
/// Looks up the thread for the given canonical node ID, then loads all remote
/// web edges for that thread. Returns an empty vec on any error (non-fatal).
fn load_remote_web_edges(
    conn: &Connection,
    slug: &str,
    canonical_node_id: &str,
) -> Vec<ConnectedRemoteWebEdge> {
    // Find the thread for this canonical node
    let thread_id: Option<String> = conn
        .query_row(
            "SELECT thread_id FROM pyramid_threads WHERE slug = ?1 AND current_canonical_id = ?2",
            rusqlite::params![slug, canonical_node_id],
            |row| row.get(0),
        )
        .ok();

    let Some(tid) = thread_id else {
        return Vec::new();
    };

    match db::get_remote_web_edges_for_thread(conn, slug, &tid) {
        Ok(edges) => edges
            .into_iter()
            .map(|e| {
                let remote_slug = HandlePath::parse(&e.remote_handle_path)
                    .map(|h| h.slug)
                    .unwrap_or_else(|| e.remote_handle_path.clone());
                ConnectedRemoteWebEdge {
                    remote_handle_path: e.remote_handle_path,
                    remote_slug,
                    relationship: e.relationship,
                    relevance: e.relevance,
                    build_id: e.build_id,
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub fn get_node_with_edges(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<NodeWithWebEdges>> {
    let Some(node) = db::get_live_node(conn, slug, node_id)? else {
        return Ok(None);
    };
    let web_edges = load_connected_web_edges(conn, slug, &node.id)?;
    Ok(Some(NodeWithWebEdges { node, web_edges }))
}

// ── Public query API ─────────────────────────────────────────────────

/// Get the mechanical apex node (highest depth, build_id IS NULL or NOT a question overlay).
/// If multiple nodes exist at the max depth (e.g. cancelled build), logs a warning
/// and falls back one level to find the completed apex.
pub fn get_apex(conn: &Connection, slug: &str) -> Result<Option<PyramidNode>> {
    // All pyramids now use the question chain — return the highest-depth live node.
    // No need to filter by build_id since all nodes participate in the same evidence DAG.
    get_apex_filtered(conn, slug, "")
}

/// Get the apex node for a specific build_id (question overlay).
/// Filters to nodes belonging to that build_id and returns the highest-depth one.
pub fn get_apex_for_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
) -> Result<Option<PyramidNode>> {
    let result = conn
        .prepare(
            "SELECT * FROM live_pyramid_nodes WHERE slug = ?1 AND build_id = ?2
             ORDER BY depth DESC LIMIT 1",
        )?
        .query_row(rusqlite::params![slug, build_id], row_to_node)
        .optional()
        .context("Failed to query overlay apex node")?;
    Ok(result)
}

/// Internal: get apex with an additional SQL filter clause.
fn get_apex_filtered(conn: &Connection, slug: &str, filter: &str) -> Result<Option<PyramidNode>> {
    // Find max depth with filter
    let max_depth: Option<i64> = conn
        .prepare(&format!(
            "SELECT MAX(depth) FROM live_pyramid_nodes WHERE slug = ? {filter}"
        ))?
        .query_row(rusqlite::params![slug], |row| row.get(0))
        .optional()
        .context("Failed to query max depth")?
        .flatten();

    let max_depth = match max_depth {
        Some(d) => d,
        None => return Ok(None),
    };

    // Count nodes at max depth
    let count: i64 = conn
        .prepare(&format!(
            "SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ? AND depth = ? {filter}"
        ))?
        .query_row(rusqlite::params![slug, max_depth], |row| row.get(0))?;

    if count == 1 {
        let node = conn
            .prepare(&format!(
                "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth = ? {filter}"
            ))?
            .query_row(rusqlite::params![slug, max_depth], row_to_node)
            .optional()
            .context("Failed to query apex node")?;
        return Ok(node);
    }

    // Multiple nodes at max depth (likely cancelled mid-layer build).
    // Scan downward to find the highest depth with exactly ONE node.
    if count > 1 {
        for d in (0..max_depth).rev() {
            let d_count: i64 = conn
                .prepare(&format!(
                    "SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ? AND depth = ? {filter}"
                ))?
                .query_row(rusqlite::params![slug, d], |row| row.get(0))?;

            if d_count == 1 {
                warn!(
                    "Multiple nodes ({}) at max depth {} for slug '{}' (likely cancelled build). Using single node at depth {} as apex.",
                    count, max_depth, slug, d
                );
                let node = conn
                    .prepare(&format!(
                        "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth = ? {filter}"
                    ))?
                    .query_row(rusqlite::params![slug, d], row_to_node)
                    .optional()
                    .context("Failed to query apex node at lower depth")?;
                return Ok(node);
            }
        }

        // No single-node depth exists — return an error
        anyhow::bail!(
            "No valid apex for slug '{}': multiple nodes at every depth (max depth {}, {} nodes). Build may have been cancelled before completing any layer.",
            slug, max_depth, count
        );
    }

    Ok(None)
}

pub fn get_apex_with_edges(conn: &Connection, slug: &str) -> Result<Option<NodeWithWebEdges>> {
    let Some(node) = get_apex(conn, slug)? else {
        return Ok(None);
    };
    let web_edges = load_connected_web_edges(conn, slug, &node.id)?;
    Ok(Some(NodeWithWebEdges { node, web_edges }))
}

/// Get the full tree structure from the apex down.
///
/// Loads all nodes for the slug into memory, finds the apex (highest depth),
/// then recursively builds the tree by following `children` arrays.
pub fn get_tree(conn: &Connection, slug: &str) -> Result<Vec<TreeNode>> {
    let mut stmt = conn.prepare("SELECT * FROM live_pyramid_nodes WHERE slug = ? ORDER BY id")?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    let mut source_path_by_node_id: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT file_path, node_ids FROM pyramid_file_hashes WHERE slug = ?1 ORDER BY file_path",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let Ok((file_path, node_ids_json)) = row else {
                continue;
            };
            let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
            for node_id in node_ids {
                source_path_by_node_id
                    .entry(node_id)
                    .or_insert_with(|| file_path.clone());
            }
        }
    }

    let mut thread_id_by_canonical_id: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT thread_id, current_canonical_id FROM pyramid_threads WHERE slug = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let Ok((thread_id, canonical_id)) = row else {
                continue;
            };
            thread_id_by_canonical_id.insert(canonical_id, thread_id);
        }
    }

    // Index by ID for O(1) lookup
    let node_map: HashMap<&str, &PyramidNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Find max depth
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);

    // Apex nodes are all nodes at max depth
    let apex_nodes: Vec<&PyramidNode> = nodes.iter().filter(|n| n.depth == max_depth).collect();

    // All pyramids now use the question chain internally — build tree via evidence
    // links. Fall back to mechanical children[] only if no evidence links exist.
    let has_evidence: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_evidence WHERE slug = ?1 AND verdict = 'KEEP' LIMIT 1",
            rusqlite::params![slug],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if has_evidence {
        // Load KEEP evidence links: source (child layer) → target (parent layer)
        let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT source_node_id, target_node_id FROM pyramid_evidence WHERE slug = ?1 AND verdict = 'KEEP'",
            )?;
            let rows = stmt.query_map(rusqlite::params![slug], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let Ok((src_id, tgt_id)) = row else { continue };
                children_by_parent.entry(tgt_id).or_default().push(src_id);
            }
        }

        fn build_question_tree_node(
            node: &PyramidNode,
            node_map: &HashMap<&str, &PyramidNode>,
            children_by_parent: &HashMap<String, Vec<String>>,
            conn: &Connection,
            thread_id_by_canonical_id: &HashMap<String, String>,
            source_path_by_node_id: &HashMap<String, String>,
        ) -> TreeNode {
            let child_ids = children_by_parent.get(&node.id).cloned().unwrap_or_default();
            let children: Vec<TreeNode> = child_ids
                .iter()
                .filter_map(|child_id| {
                    if let Some((ref_slug, _depth, ref_node_id)) = db::parse_handle_path(child_id) {
                        // Cross-slug: load as leaf
                        match db::get_node_summary(conn, ref_slug, ref_node_id) {
                            Ok(Some(s)) => Some(TreeNode {
                                id: s.id.clone(),
                                depth: s.depth,
                                headline: s.headline.clone(),
                                distilled: s.distilled.clone(),
                                self_prompt: None,
                                thread_id: None,
                                source_path: None,
                                source_slug: Some(ref_slug.to_string()),
                                children: vec![],
                            }),
                            _ => None,
                        }
                    } else {
                        // Same-slug: recurse
                        node_map.get(child_id.as_str()).map(|child| {
                            build_question_tree_node(
                                child,
                                node_map,
                                children_by_parent,
                                conn,
                                thread_id_by_canonical_id,
                                source_path_by_node_id,
                            )
                        })
                    }
                })
                .collect();

            TreeNode {
                id: node.id.clone(),
                depth: node.depth,
                headline: node.headline.clone(),
                distilled: node.distilled.clone(),
                self_prompt: if node.self_prompt.is_empty() { None } else { Some(node.self_prompt.clone()) },
                thread_id: thread_id_by_canonical_id.get(&node.id).cloned(),
                source_path: source_path_by_node_id.get(&node.id).cloned(),
                source_slug: None,
                children,
            }
        }

        let result: Vec<TreeNode> = apex_nodes
            .iter()
            .map(|apex| {
                build_question_tree_node(
                    apex,
                    &node_map,
                    &children_by_parent,
                    conn,
                    &thread_id_by_canonical_id,
                    &source_path_by_node_id,
                )
            })
            .collect();
        return Ok(result);
    }

    fn build_tree_node(
        node: &PyramidNode,
        source_slug: Option<String>,
        node_map: &HashMap<&str, &PyramidNode>,
        conn: &Connection,
        thread_id_by_canonical_id: &HashMap<String, String>,
        source_path_by_node_id: &HashMap<String, String>,
    ) -> TreeNode {
        let children = node
            .children
            .iter()
            .filter_map(|child_id| {
                if let Some((ref_slug, _depth, ref_node_id)) = db::parse_handle_path(child_id) {
                    // Cross-slug child: load summary from the referenced slug
                    match db::get_node_summary(conn, ref_slug, ref_node_id) {
                        Ok(Some(child_node)) => Some(TreeNode {
                            id: child_node.id.clone(),
                            depth: child_node.depth,
                            headline: child_node.headline.clone(),
                            distilled: child_node.distilled.clone(),
                            self_prompt: None,
                            thread_id: None,
                            source_path: None,
                            source_slug: Some(ref_slug.to_string()),
                            children: vec![], // Don't recurse into foreign slugs
                        }),
                        _ => None,
                    }
                } else {
                    // Same-slug child: existing lookup
                    node_map.get(child_id.as_str()).map(|child| {
                        build_tree_node(
                            child,
                            None,
                            node_map,
                            conn,
                            thread_id_by_canonical_id,
                            source_path_by_node_id,
                        )
                    })
                }
            })
            .collect();

        TreeNode {
            id: node.id.clone(),
            depth: node.depth,
            headline: node.headline.clone(),
            distilled: node.distilled.clone(),
            self_prompt: None,
            thread_id: thread_id_by_canonical_id.get(&node.id).cloned(),
            source_path: source_path_by_node_id.get(&node.id).cloned(),
            source_slug: source_slug,
            children,
        }
    }

    let trees = apex_nodes
        .into_iter()
        .map(|apex| {
            build_tree_node(
                apex,
                None,
                &node_map,
                conn,
                &thread_id_by_canonical_id,
                &source_path_by_node_id,
            )
        })
        .collect();

    Ok(trees)
}

/// Drill into a node — returns the node plus its direct children,
/// evidence links, gaps, and question tree context.
///
/// Cross-slug support: children that are handle-paths (e.g. "other-slug/0/node-id")
/// are loaded from the referenced slug via `db::get_node_summary`.
pub fn drill(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<DrillResult>> {
    let parent = match db::get_live_node(conn, slug, node_id)? {
        Some(n) => n,
        None => return Ok(None),
    };

    // Children use raw get_node_summary (not live view) so the UI can badge
    // superseded children and show links to their successors.
    // Cross-slug handle-path children are resolved to their source slug.
    let mut children = Vec::new();
    for child_id in &parent.children {
        if let Some((ref_slug, _depth, ref_node_id)) = db::parse_handle_path(child_id) {
            // Cross-slug child: load from the referenced slug
            if let Some(child) = db::get_node_summary(conn, ref_slug, ref_node_id)? {
                children.push(child);
            }
        } else {
            // Same-slug child: existing behavior
            if let Some(child) = db::get_node_summary(conn, slug, child_id)? {
                children.push(child);
            }
        }
    }

    // For question pyramid nodes: if children[] is empty, use evidence KEEP sources as children.
    // Gate on content_type so mechanical pyramids with empty L0 leaves don't query evidence.
    let is_question_slug = conn
        .query_row(
            "SELECT content_type FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap_or(None)
        .map_or(false, |ct| ct == "question");
    if children.is_empty() && is_question_slug {
        let keep_links = db::get_keep_evidence_for_target_cross(conn, slug, node_id)?;
        for link in &keep_links {
            let child = if let Some((ref_slug, _depth, ref_node_id)) = db::parse_handle_path(&link.source_node_id) {
                db::get_node_summary(conn, ref_slug, ref_node_id)?
            } else {
                db::get_node_summary(conn, slug, &link.source_node_id)?
            };
            if let Some(child_node) = child {
                children.push(child_node);
            }
        }
    }

    // Use cross-slug evidence loader (returns evidence with live flags)
    let evidence = db::get_evidence_for_target_cross(conn, slug, node_id)?;

    let gaps = db::get_gaps_for_question(conn, slug, node_id)?;

    let question_context = if !parent.self_prompt.is_empty() {
        db::get_question_tree(conn, slug)?
            .and_then(|tree_json| find_question_context(&tree_json, &parent.self_prompt))
    } else {
        None
    };

    // WS-ONLINE-F: Load remote web edges for this node's thread
    let remote_web_edges = load_remote_web_edges(conn, slug, node_id);

    Ok(Some(DrillResult {
        node: parent,
        children,
        web_edges: load_connected_web_edges(conn, slug, node_id)?,
        remote_web_edges,
        evidence,
        gaps,
        question_context,
    }))
}

/// Walk a question tree JSON to find a node by question text and return its parent question + siblings.
fn find_question_context(tree_json: &serde_json::Value, question_text: &str) -> Option<QuestionContext> {
    find_in_tree(&tree_json["apex"], question_text, None)
}

/// Recursive helper: searches the question tree for `target_question`.
/// `parent_question` is the question text of the caller's node (None at root).
fn find_in_tree(
    node: &serde_json::Value,
    target_question: &str,
    parent_question: Option<&str>,
) -> Option<QuestionContext> {
    let current_question = node["question"].as_str().unwrap_or("");

    // If the current node IS the target, return context only if there's a parent.
    // Apex nodes (no parent, no siblings) return None so the frontend skips the section.
    if current_question == target_question {
        return match parent_question {
            Some(pq) => Some(QuestionContext {
                parent_question: Some(pq.to_string()),
                sibling_questions: Vec::new(),
            }),
            None => None,
        };
    }

    if let Some(kids) = node["children"].as_array() {
        // Check if target is a direct child — then we can compute siblings.
        for child in kids {
            if child["question"].as_str() == Some(target_question) {
                let siblings: Vec<String> = kids
                    .iter()
                    .filter(|k| k["question"].as_str() != Some(target_question))
                    .filter_map(|k| k["question"].as_str().map(String::from))
                    .collect();
                return Some(QuestionContext {
                    parent_question: Some(current_question.to_string()),
                    sibling_questions: siblings,
                });
            }
        }

        // Recurse into children.
        for child in kids {
            if let Some(ctx) = find_in_tree(child, target_question, Some(current_question)) {
                return Some(ctx);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    fn seed_slug(conn: &Connection, slug: &str) {
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '')",
            rusqlite::params![slug],
        )
        .unwrap();
    }

    fn test_node(slug: &str, id: &str, depth: i64, headline: &str) -> PyramidNode {
        PyramidNode {
            id: id.to_string(),
            slug: slug.to_string(),
            depth,
            chunk_index: None,
            headline: headline.to_string(),
            distilled: format!("{headline} distilled"),
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
            created_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn test_thread(slug: &str, id: &str, headline: &str, depth: i64) -> PyramidThread {
        PyramidThread {
            slug: slug.to_string(),
            thread_id: id.to_string(),
            thread_name: headline.to_string(),
            current_canonical_id: id.to_string(),
            depth,
            delta_count: 0,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_get_node_with_edges_returns_connected_edge_payload() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        seed_slug(&conn, "s");

        let node_a = test_node("s", "L1-000", 1, "Alpha Thread");
        let node_b = test_node("s", "L1-001", 1, "Beta Thread");
        db::save_node(&conn, &node_a, None).unwrap();
        db::save_node(&conn, &node_b, None).unwrap();
        db::save_thread(&conn, &test_thread("s", "L1-000", "Alpha Thread", 1)).unwrap();
        db::save_thread(&conn, &test_thread("s", "L1-001", "Beta Thread", 1)).unwrap();
        db::save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".to_string(),
                thread_a_id: "L1-000".to_string(),
                thread_b_id: "L1-001".to_string(),
                relationship: "Both read pyramid_nodes".to_string(),
                relevance: 0.82,
                delta_count: 0,
                build_id: None,
                created_at: "2025-01-01T00:00:00Z".to_string(),
                updated_at: "2025-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();

        let result = get_node_with_edges(&conn, "s", "L1-000").unwrap().unwrap();
        assert_eq!(result.node.id, "L1-000");
        assert_eq!(result.web_edges.len(), 1);
        assert_eq!(result.web_edges[0].connected_to, "L1-001");
        assert_eq!(result.web_edges[0].connected_headline, "Beta Thread");
        assert_eq!(result.web_edges[0].relationship, "Both read pyramid_nodes");
        assert!((result.web_edges[0].strength - 0.82).abs() < f64::EPSILON);
    }

    #[test]
    fn test_drill_includes_web_edges_and_empty_when_no_thread() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        seed_slug(&conn, "s");

        let parent = PyramidNode {
            children: vec!["L0-000".to_string()],
            ..test_node("s", "L1-000", 1, "Parent")
        };
        let child = test_node("s", "L0-000", 0, "Leaf");
        let sibling = test_node("s", "L1-001", 1, "Sibling");
        db::save_node(&conn, &parent, None).unwrap();
        db::save_node(&conn, &child, None).unwrap();
        db::save_node(&conn, &sibling, None).unwrap();
        db::save_thread(&conn, &test_thread("s", "L1-000", "Parent", 1)).unwrap();
        db::save_thread(&conn, &test_thread("s", "L1-001", "Sibling", 1)).unwrap();
        db::save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".to_string(),
                thread_a_id: "L1-000".to_string(),
                thread_b_id: "L1-001".to_string(),
                relationship: "Shared API".to_string(),
                relevance: 0.7,
                delta_count: 0,
                build_id: None,
                created_at: "2025-01-01T00:00:00Z".to_string(),
                updated_at: "2025-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();

        let drilled = drill(&conn, "s", "L1-000").unwrap().unwrap();
        assert_eq!(drilled.children.len(), 1);
        assert_eq!(drilled.web_edges.len(), 1);

        let leaf = get_node_with_edges(&conn, "s", "L0-000").unwrap().unwrap();
        assert!(leaf.web_edges.is_empty());
    }
}

/// Search across all nodes for a term (case-insensitive).
///
/// Searches in distilled text, topics JSON, and corrections JSON.
/// Returns results ordered by depth descending (most synthesized first).
///
/// Cross-slug support: when the slug has references, searches across all
/// referenced slugs + self in a single query. Results are tagged with source_slug.
pub fn search(conn: &Connection, slug: &str, term: &str) -> Result<Vec<SearchHit>> {
    // Tokenize, lowercase, drop short noise + stop words. The remaining
    // tokens drive an OR-match across searchable fields; the score is
    // computed below from the count of matched words and the depth.
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "how",
        "i", "in", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was",
        "what", "when", "where", "which", "who", "why", "will", "with", "you", "your", "do",
        "does", "did", "can", "could", "would", "should", "may", "might", "must", "shall",
        "but", "if", "then", "than", "so", "no", "not", "any", "all", "some", "such",
    ];
    let mut words: Vec<String> = term
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .filter(|w| !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect();

    if words.is_empty() {
        // Fallback: every word was a stop word or too short — search the
        // raw trimmed term as a single token so the user still gets hits.
        let raw = term.trim().to_lowercase();
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        words = vec![raw];
    }

    // Gather all slugs to search: self + referenced slugs
    let referenced = db::get_slug_references(conn, slug)?;
    let all_slugs: Vec<String> = {
        let mut s = vec![slug.to_string()];
        s.extend(referenced);
        s
    };

    // Build dynamic WHERE clause: slug IN (...) + each word must match at least one field
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // Build slug IN clause
    let slug_placeholders: Vec<String> = all_slugs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            params.push(Box::new(s.clone()));
            format!("?{}", i + 1)
        })
        .collect();
    let slug_in_clause = format!("slug IN ({})", slug_placeholders.join(", "));

    let mut conditions = Vec::new();
    for word in &words {
        let pattern = format!("%{}%", word);
        let idx = params.len();
        // 11-Y: Search headline, distilled, topics (which includes entity names), corrections, and terms
        conditions.push(format!(
            "(LOWER(headline) LIKE ?{p1} OR LOWER(distilled) LIKE ?{p2} OR LOWER(topics) LIKE ?{p3} OR LOWER(corrections) LIKE ?{p4} OR LOWER(terms) LIKE ?{p5})",
            p1 = idx + 1, p2 = idx + 2, p3 = idx + 3, p4 = idx + 4, p5 = idx + 5
        ));
        for _ in 0..5 {
            params.push(Box::new(pattern.clone()));
        }
    }

    let sql = format!(
        "SELECT id, slug, depth, headline, distilled, topics, corrections, terms FROM live_pyramid_nodes \
         WHERE {} AND ({}) ORDER BY depth DESC",
        slug_in_clause,
        conditions.join(" OR ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    let hits: Vec<SearchHit> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let id: String = row.get("id")?;
            let hit_slug: String = row.get("slug")?;
            let depth: i64 = row.get("depth")?;
            let headline: String = row.get::<_, String>("headline").unwrap_or_default();
            let distilled: String = row.get("distilled")?;
            let topics: String = row.get::<_, String>("topics").unwrap_or_default();
            let terms_str: String = row.get::<_, String>("terms").unwrap_or_default();

            // Combine all searchable text for snippet extraction and scoring
            // (topics JSON already contains entity names, so they're searched implicitly)
            let all_text = format!("{} {} {} {}", distilled, topics, terms_str, headline);
            let all_lower = all_text.to_lowercase();

            // Count how many query words appear and find first match for snippet
            let mut word_hits = 0;
            let mut first_match_idx: Option<usize> = None;
            for word in &words {
                if all_lower.contains(word.as_str()) {
                    word_hits += 1;
                    if first_match_idx.is_none() {
                        if let Some(idx) = all_lower.find(word.as_str()) {
                            first_match_idx = Some(idx);
                        }
                    }
                }
            }

            // Build snippet around first match
            let snippet = if let Some(_idx) = first_match_idx {
                let source = &distilled; // prefer distilled for snippet
                let source_lower = source.to_lowercase();
                if let Some(sidx) = source_lower.find(&words[0]) {
                    let mut start = sidx.saturating_sub(50);
                    while start > 0 && !source.is_char_boundary(start) {
                        start -= 1;
                    }
                    let mut end = (sidx + words[0].len() + 80).min(source.len());
                    while end < source.len() && !source.is_char_boundary(end) {
                        end += 1;
                    }
                    format!("...{}...", &source[start..end])
                } else {
                    let mut end = distilled.len().min(120);
                    while end < distilled.len() && !distilled.is_char_boundary(end) {
                        end += 1;
                    }
                    distilled[..end].to_string()
                }
            } else {
                let mut end = distilled.len().min(120);
                while end < distilled.len() && !distilled.is_char_boundary(end) {
                    end += 1;
                }
                distilled[..end].to_string()
            };

            // Score: word coverage * depth bonus
            // More words matched = higher score. Higher depth = more synthesized.
            let coverage = word_hits as f64 / words.len() as f64;
            let score = coverage * (depth as f64 + 1.0) * 10.0;

            // Tag with source_slug if it came from a different slug
            let source_slug_tag = if hit_slug != slug {
                Some(hit_slug)
            } else {
                None
            };

            Ok(SearchHit {
                node_id: id,
                depth,
                headline,
                snippet,
                score,
                source_slug: source_slug_tag,
                child_count: 0,
                annotation_count: 0,
                has_web_edges: false,
            })
        })?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // Sort by score descending (best matches first)
    let mut sorted = hits;
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ── Enrichment pass: fill child_count, annotation_count, has_web_edges ──
    if !sorted.is_empty() {
        // Build node ID list for batch queries
        let node_ids: Vec<String> = sorted.iter().map(|h| h.node_id.clone()).collect();

        // 1. Child counts from live_pyramid_nodes.children (JSON array)
        {
            let placeholders: String = node_ids.iter().enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT id, json_array_length(children) FROM live_pyramid_nodes WHERE id IN ({})",
                placeholders
            );
            let params: Vec<&dyn rusqlite::types::ToSql> = node_ids.iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let mut child_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
                if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1).unwrap_or(0)))
                }) {
                    for row in rows.flatten() {
                        child_map.insert(row.0, row.1);
                    }
                }
                for hit in sorted.iter_mut() {
                    if let Some(&count) = child_map.get(&hit.node_id) {
                        hit.child_count = count;
                    }
                }
            }
        }

        // 2. Annotation counts from pyramid_annotations
        {
            let placeholders: String = node_ids.iter().enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT node_id, COUNT(*) FROM pyramid_annotations WHERE node_id IN ({}) GROUP BY node_id",
                placeholders
            );
            let params: Vec<&dyn rusqlite::types::ToSql> = node_ids.iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let mut annot_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
                if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1).unwrap_or(0)))
                }) {
                    for row in rows.flatten() {
                        annot_map.insert(row.0, row.1);
                    }
                }
                for hit in sorted.iter_mut() {
                    if let Some(&count) = annot_map.get(&hit.node_id) {
                        hit.annotation_count = count;
                    }
                }
            }
        }

        // 3. Web edges: check if node's thread has any edges
        // Web edges use thread_id, not node_id. We need to go through pyramid_threads.
        {
            let placeholders: String = node_ids.iter().enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT DISTINCT t.current_canonical_id FROM pyramid_threads t \
                 JOIN pyramid_web_edges e ON t.slug = e.slug AND (t.thread_id = e.thread_a_id OR t.thread_id = e.thread_b_id) \
                 WHERE t.current_canonical_id IN ({})",
                placeholders
            );
            let params: Vec<&dyn rusqlite::types::ToSql> = node_ids.iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let mut edge_set: std::collections::HashSet<String> = std::collections::HashSet::new();
                if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                    row.get::<_, String>(0)
                }) {
                    for row in rows.flatten() {
                        edge_set.insert(row);
                    }
                }
                for hit in sorted.iter_mut() {
                    hit.has_web_edges = edge_set.contains(&hit.node_id);
                }
            }
        }
    }

    Ok(sorted)
}

/// Composed view: load all live nodes from the slug + all referenced slugs,
/// build edges from evidence links and children arrays.
/// Returns the full cross-slug graph for visualization.
pub fn get_composed_view(conn: &Connection, slug: &str) -> Result<ComposedView> {
    // Gather all slugs: self + referenced
    let referenced = db::get_slug_references(conn, slug)?;
    let all_slugs: Vec<String> = {
        let mut s = vec![slug.to_string()];
        s.extend(referenced);
        s
    };

    let mut composed_nodes: Vec<ComposedNode> = Vec::new();
    let mut composed_edges: Vec<ComposedEdge> = Vec::new();
    let mut node_ids_seen: HashSet<String> = HashSet::new();

    for s in &all_slugs {
        // Load all live nodes for this slug
        let mut stmt = conn.prepare(
            "SELECT id, slug, depth, headline, distilled, build_id, self_prompt FROM live_pyramid_nodes WHERE slug = ?1 ORDER BY depth, id",
        )?;
        let rows = stmt.query_map(rusqlite::params![s], |row| {
            let id: String = row.get(0)?;
            let node_slug: String = row.get(1)?;
            let depth: i64 = row.get(2)?;
            let headline: String = row.get(3)?;
            let distilled: String = row.get(4)?;
            let build_id: Option<String> = row.get(5)?;
            let self_prompt: Option<String> = row.get(6)?;
            let node_type = if build_id.as_deref().map_or(false, |b| b.starts_with("qb-")) {
                "answer".to_string()
            } else {
                "mechanical".to_string()
            };
            Ok(ComposedNode {
                id,
                slug: node_slug,
                depth,
                headline,
                distilled,
                self_prompt,
                node_type,
            })
        })?;

        for row in rows {
            if let Ok(node) = row {
                node_ids_seen.insert(node.id.clone());
                composed_nodes.push(node);
            }
        }

        // Load children arrays to build child edges
        let mut children_stmt =
            conn.prepare("SELECT id, children FROM live_pyramid_nodes WHERE slug = ?1")?;
        let child_rows = children_stmt.query_map(rusqlite::params![s], |row| {
            let id: String = row.get(0)?;
            let children_json: String = row.get(1)?;
            Ok((id, children_json))
        })?;

        for row in child_rows {
            if let Ok((parent_id, children_json)) = row {
                let children: Vec<String> =
                    serde_json::from_str(&children_json).unwrap_or_default();
                for child_id in children {
                    composed_edges.push(ComposedEdge {
                        source_id: parent_id.clone(),
                        target_id: child_id,
                        weight: 1.0,
                        edge_type: "child".to_string(),
                        live: true,
                    });
                }
            }
        }

        // Load evidence edges
        let mut ev_stmt = conn.prepare(
            "SELECT source_node_id, target_node_id, weight, verdict FROM pyramid_evidence WHERE slug = ?1",
        )?;
        let ev_rows = ev_stmt.query_map(rusqlite::params![s], |row| {
            let source_id: String = row.get(0)?;
            let target_id: String = row.get(1)?;
            let weight: Option<f64> = row.get(2)?;
            let verdict: String = row.get(3)?;
            Ok((source_id, target_id, weight, verdict))
        })?;

        for row in ev_rows {
            if let Ok((source_id, target_id, weight, verdict)) = row {
                if verdict == "DISCONNECT" {
                    continue;
                }
                // Determine liveness: check if the source node is superseded
                let live = if let Some((_ref_slug, _depth, ref_node_id)) =
                    db::parse_handle_path(&source_id)
                {
                    // Cross-slug source: check if the node is still live
                    node_ids_seen.contains(ref_node_id)
                } else {
                    node_ids_seen.contains(&source_id)
                };
                composed_edges.push(ComposedEdge {
                    source_id,
                    target_id,
                    weight: weight.unwrap_or(0.5),
                    edge_type: "evidence".to_string(),
                    live,
                });
            }
        }

        // Load web edges
        let mut web_stmt = conn.prepare(
            "SELECT e.thread_a_id, e.thread_b_id, e.relevance \
             FROM pyramid_web_edges e WHERE e.slug = ?1",
        )?;
        let web_rows = web_stmt.query_map(rusqlite::params![s], |row| {
            let thread_a: String = row.get(0)?;
            let thread_b: String = row.get(1)?;
            let relevance: f64 = row.get(2)?;
            Ok((thread_a, thread_b, relevance))
        })?;

        // Resolve thread IDs to canonical node IDs for web edges
        let mut thread_to_canonical: HashMap<String, String> = HashMap::new();
        let mut t_stmt = conn.prepare(
            "SELECT thread_id, current_canonical_id FROM pyramid_threads WHERE slug = ?1",
        )?;
        let t_rows = t_stmt.query_map(rusqlite::params![s], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in t_rows {
            if let Ok((tid, cid)) = row {
                thread_to_canonical.insert(tid, cid);
            }
        }

        for row in web_rows {
            if let Ok((thread_a, thread_b, relevance)) = row {
                if let (Some(node_a), Some(node_b)) = (
                    thread_to_canonical.get(&thread_a),
                    thread_to_canonical.get(&thread_b),
                ) {
                    composed_edges.push(ComposedEdge {
                        source_id: node_a.clone(),
                        target_id: node_b.clone(),
                        weight: relevance,
                        edge_type: "web".to_string(),
                        live: true,
                    });
                }
            }
        }
    }

    Ok(ComposedView {
        nodes: composed_nodes,
        edges: composed_edges,
        slugs: all_slugs,
    })
}

/// Entity index — all entities that appear in 2+ nodes, with their locations.
///
/// Scans all nodes' topics for entity strings, builds an inverted index
/// (entity -> list of nodes), and returns only entities appearing in 2+ distinct nodes.
/// Sorted by node count descending.
pub fn entities(conn: &Connection, slug: &str) -> Result<Vec<EntityEntry>> {
    let mut stmt =
        conn.prepare("SELECT * FROM live_pyramid_nodes WHERE slug = ? ORDER BY depth, id")?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // entity_normalized -> { name, locations: [(node_id, depth, topic_name)] }
    struct EntityData {
        name: String,
        locations: Vec<(String, i64, String)>,
    }

    let mut index: HashMap<String, EntityData> = HashMap::new();

    for node in &nodes {
        let ents = collect_entities(node);
        for (entity, topic_name) in ents {
            let key = entity.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let entry = index.entry(key).or_insert_with(|| EntityData {
                name: entity.clone(),
                locations: Vec::new(),
            });
            entry
                .locations
                .push((node.id.clone(), node.depth, topic_name));
        }
    }

    // Filter to entities in 2+ distinct nodes, sort by count descending
    let mut results: Vec<EntityEntry> = index
        .into_values()
        .filter_map(|data| {
            let unique_nodes: HashSet<&str> =
                data.locations.iter().map(|(n, _, _)| n.as_str()).collect();
            if unique_nodes.len() < 2 {
                return None;
            }
            let mut nodes_sorted: Vec<String> =
                unique_nodes.into_iter().map(|s| s.to_string()).collect();
            nodes_sorted.sort();
            let mut depths: Vec<i64> = data
                .locations
                .iter()
                .map(|(_, d, _)| *d)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            depths.sort();
            let mut topics: Vec<String> = data
                .locations
                .iter()
                .map(|(_, _, t)| t.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            topics.sort();

            Some(EntityEntry {
                name: data.name,
                nodes: nodes_sorted,
                depths,
                topic_names: topics,
            })
        })
        .collect();

    results.sort_by(|a, b| b.nodes.len().cmp(&a.nodes.len()));

    Ok(results)
}

/// Resolved corrections — terminal values of correction chains.
///
/// Scans L0 nodes first for base corrections, builds chains (A->B, B->C),
/// then adds upper-level corrections that aren't already covered.
pub fn resolved(conn: &Connection, slug: &str) -> Result<Vec<ResolvedCorrection>> {
    // Gather L0 corrections (depth == 0)
    let mut stmt_l0 = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth = 0 \
         ORDER BY chunk_index",
    )?;
    let l0_nodes: Vec<PyramidNode> = stmt_l0
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // Gather upper corrections (depth > 0)
    let mut stmt_upper = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth > 0 \
         ORDER BY depth, id",
    )?;
    let upper_nodes: Vec<PyramidNode> = stmt_upper
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // Build correction chains from L0 nodes.
    // Key: normalized right value -> chain of (wrong, right, who, source_node, chunk_index)
    struct ChainEntry {
        wrong: String,
        right: String,
        who: String,
        source: String,
        chunk_index: i64,
    }

    let mut chains: Vec<(String, Vec<ChainEntry>)> = Vec::new(); // (key, entries)

    for node in &l0_nodes {
        let all_corrections = collect_corrections(node);
        let ci = node.chunk_index.unwrap_or(-1);

        for c in all_corrections {
            let right = c.right.trim().to_string();
            let wrong = c.wrong.trim().to_string();
            let right_norm = right.to_lowercase().chars().take(80).collect::<String>();
            let wrong_norm = wrong.to_lowercase().chars().take(80).collect::<String>();

            // Try to find an existing chain where this correction's "wrong" matches
            // a previous entry's "right" (chaining: A->B, B->C)
            let mut found_chain_idx: Option<usize> = None;
            for (idx, (_, chain)) in chains.iter().enumerate() {
                for entry in chain {
                    let entry_right_norm: String =
                        entry.right.to_lowercase().chars().take(80).collect();
                    if entry_right_norm == wrong_norm {
                        found_chain_idx = Some(idx);
                        break;
                    }
                }
                if found_chain_idx.is_some() {
                    break;
                }
            }

            if let Some(idx) = found_chain_idx {
                chains[idx].1.push(ChainEntry {
                    wrong,
                    right,
                    who: c.who.clone(),
                    source: node.id.clone(),
                    chunk_index: ci,
                });
            } else {
                // New chain
                chains.push((
                    right_norm,
                    vec![ChainEntry {
                        wrong,
                        right,
                        who: c.who.clone(),
                        source: node.id.clone(),
                        chunk_index: ci,
                    }],
                ));
            }
        }
    }

    // Add upper-node corrections that aren't already covered
    for node in &upper_nodes {
        let all_corrections = collect_corrections(node);
        for c in all_corrections {
            let right = c.right.trim().to_string();
            let wrong = c.wrong.trim().to_string();
            let right_norm: String = right.to_lowercase().chars().take(80).collect();
            let wrong_norm: String = wrong.to_lowercase().chars().take(80).collect();

            // Check if already covered
            let already_covered = chains.iter().any(|(_, chain)| {
                chain.iter().any(|entry| {
                    let er: String = entry.right.to_lowercase().chars().take(80).collect();
                    let ew: String = entry.wrong.to_lowercase().chars().take(80).collect();
                    er == right_norm && ew == wrong_norm
                })
            });

            if !already_covered {
                chains.push((
                    right_norm,
                    vec![ChainEntry {
                        wrong,
                        right,
                        who: c.who.clone(),
                        source: node.id.clone(),
                        chunk_index: -1,
                    }],
                ));
            }
        }
    }

    // Sort by most recent chunk_index first
    chains.sort_by(|a, b| {
        let max_a = a.1.iter().map(|e| e.chunk_index).max().unwrap_or(-1);
        let max_b = b.1.iter().map(|e| e.chunk_index).max().unwrap_or(-1);
        max_b.cmp(&max_a)
    });

    // Convert to ResolvedCorrection
    let results = chains
        .into_iter()
        .map(|(_, chain)| {
            let terminal = chain.last().unwrap();
            let current = terminal.right.clone();
            let was = chain.first().unwrap().wrong.clone();
            let who = terminal.who.clone();
            let source_node = terminal.source.clone();

            // Build the evolution chain
            let mut evolution: Vec<String> = chain.iter().map(|e| e.wrong.clone()).collect();
            evolution.push(terminal.right.clone());

            ResolvedCorrection {
                current,
                was,
                chain: evolution,
                who,
                source_node,
            }
        })
        .collect();

    Ok(results)
}

/// Raw corrections list (deduped, highest depth).
///
/// Deduplicates by normalized (wrong.lower(), right.lower()) pair,
/// keeping the version from the highest depth node.
pub fn corrections(conn: &Connection, slug: &str) -> Result<Vec<CorrectionWithSource>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? \
         ORDER BY depth DESC, id",
    )?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // Dedup: (wrong_norm, right_norm) -> CorrectionWithSource
    // Since nodes are ordered depth DESC, first occurrence wins (highest depth).
    let mut seen: HashMap<(String, String), CorrectionWithSource> = HashMap::new();

    for node in &nodes {
        let all_corrections = collect_corrections(node);
        for c in all_corrections {
            let wrong_norm: String = c.wrong.trim().to_lowercase().chars().take(80).collect();
            let right_norm: String = c.right.trim().to_lowercase().chars().take(80).collect();
            let key = (wrong_norm, right_norm);

            seen.entry(key).or_insert_with(|| CorrectionWithSource {
                wrong: c.wrong.clone(),
                right: c.right.clone(),
                who: c.who.clone(),
                node_id: node.id.clone(),
                depth: node.depth,
            });
        }
    }

    // Sort by depth descending, then node_id
    let mut entries: Vec<CorrectionWithSource> = seen.into_values().collect();
    entries.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    Ok(entries)
}

/// Terms list (deduped by term name, highest-depth wins).
pub fn terms(conn: &Connection, slug: &str) -> Result<Vec<TermWithSource>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND terms != '[]' \
         ORDER BY depth DESC, id",
    )?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("Skipping row: {e}");
                None
            }
        })
        .collect();

    // Dedup by normalized term name — first occurrence wins (highest depth)
    let mut seen: HashMap<String, TermWithSource> = HashMap::new();

    for node in &nodes {
        for t in &node.terms {
            let key = t.term.trim().to_lowercase();
            seen.entry(key).or_insert_with(|| TermWithSource {
                term: t.term.clone(),
                definition: t.definition.clone(),
                node_id: node.id.clone(),
                depth: node.depth,
            });
        }
    }

    let mut entries: Vec<TermWithSource> = seen.into_values().collect();
    entries.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    Ok(entries)
}

// ── rusqlite optional helper ─────────────────────────────────────────

/// Extension trait to add `.optional()` to rusqlite query results,
/// converting "no rows" into `Ok(None)`.
trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
