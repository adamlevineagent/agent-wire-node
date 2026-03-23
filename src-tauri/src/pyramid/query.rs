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

// ── Public query API ─────────────────────────────────────────────────

/// Get the apex node (highest depth, single node at that depth).
/// If multiple nodes exist at the max depth (e.g. cancelled build), logs a warning
/// and falls back one level to find the completed apex.
pub fn get_apex(conn: &Connection, slug: &str) -> Result<Option<PyramidNode>> {
    // Find max depth
    let max_depth: Option<i64> = conn
        .prepare("SELECT MAX(depth) FROM live_pyramid_nodes WHERE slug = ?")?
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
        .prepare("SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ? AND depth = ?")?
        .query_row(rusqlite::params![slug, max_depth], |row| row.get(0))?;

    if count == 1 {
        let node = conn
            .prepare("SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth = ?")?
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
                .prepare("SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ? AND depth = ?")?
                .query_row(rusqlite::params![slug, d], |row| row.get(0))?;

            if d_count == 1 {
                warn!(
                    "Multiple nodes ({}) at max depth {} for slug '{}' (likely cancelled build). Using single node at depth {} as apex.",
                    count, max_depth, slug, d
                );
                let node = conn
                    .prepare("SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth = ?")?
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

/// Get the full tree structure from the apex down.
///
/// Loads all nodes for the slug into memory, finds the apex (highest depth),
/// then recursively builds the tree by following `children` arrays.
pub fn get_tree(conn: &Connection, slug: &str) -> Result<Vec<TreeNode>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? ORDER BY id",
    )?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
        .collect();

    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    // Index by ID for O(1) lookup
    let node_map: HashMap<&str, &PyramidNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Find max depth
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);

    // Apex nodes are all nodes at max depth
    let apex_nodes: Vec<&PyramidNode> = nodes.iter().filter(|n| n.depth == max_depth).collect();

    fn build_tree_node(
        node: &PyramidNode,
        node_map: &HashMap<&str, &PyramidNode>,
    ) -> TreeNode {
        let children = node
            .children
            .iter()
            .filter_map(|child_id| {
                node_map
                    .get(child_id.as_str())
                    .map(|child| build_tree_node(child, node_map))
            })
            .collect();

        TreeNode {
            id: node.id.clone(),
            depth: node.depth,
            distilled: node.distilled.clone(),
            children,
        }
    }

    let trees = apex_nodes
        .into_iter()
        .map(|apex| build_tree_node(apex, &node_map))
        .collect();

    Ok(trees)
}

/// Drill into a node — returns the node plus its direct children.
pub fn drill(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<DrillResult>> {
    let parent = match db::get_node(conn, slug, node_id)? {
        Some(n) => n,
        None => return Ok(None),
    };

    let mut children = Vec::new();
    for child_id in &parent.children {
        if let Some(child) = db::get_node(conn, slug, child_id)? {
            children.push(child);
        }
    }

    Ok(Some(DrillResult {
        node: parent,
        children,
    }))
}

/// Search across all nodes for a term (case-insensitive).
///
/// Searches in distilled text, topics JSON, and corrections JSON.
/// Returns results ordered by depth descending (most synthesized first).
pub fn search(conn: &Connection, slug: &str, term: &str) -> Result<Vec<SearchHit>> {
    let pattern = format!("%{}%", term.to_lowercase());

    let mut stmt = conn.prepare(
        "SELECT id, depth, distilled, topics, corrections FROM live_pyramid_nodes \
         WHERE slug = ? AND (\
             LOWER(distilled) LIKE ? \
             OR LOWER(topics) LIKE ? \
             OR LOWER(corrections) LIKE ? \
             OR LOWER(terms) LIKE ?\
         ) ORDER BY depth DESC",
    )?;

    let hits: Vec<SearchHit> = stmt
        .query_map(rusqlite::params![slug, &pattern, &pattern, &pattern, &pattern], |row| {
            let id: String = row.get("id")?;
            let depth: i64 = row.get("depth")?;
            let distilled: String = row.get("distilled")?;

            // Find snippet around the match
            let term_lower = term.to_lowercase();
            let distilled_lower = distilled.to_lowercase();
            let snippet = if let Some(idx) = distilled_lower.find(&term_lower) {
                let mut start = idx.saturating_sub(40);
                while start > 0 && !distilled.is_char_boundary(start) { start -= 1; }
                let mut end = (idx + term.len() + 40).min(distilled.len());
                while end < distilled.len() && !distilled.is_char_boundary(end) { end += 1; }
                format!("...{}...", &distilled[start..end])
            } else {
                let mut end = distilled.len().min(80);
                while end < distilled.len() && !distilled.is_char_boundary(end) { end += 1; }
                distilled[..end].to_string()
            };

            // Simple relevance: higher depth = more synthesized = higher score
            let score = depth as f64 + 1.0;

            Ok(SearchHit {
                node_id: id,
                depth,
                snippet,
                score,
            })
        })?
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
        .collect();

    Ok(hits)
}

/// Entity index — all entities that appear in 2+ nodes, with their locations.
///
/// Scans all nodes' topics for entity strings, builds an inverted index
/// (entity -> list of nodes), and returns only entities appearing in 2+ distinct nodes.
/// Sorted by node count descending.
pub fn entities(conn: &Connection, slug: &str) -> Result<Vec<EntityEntry>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? ORDER BY depth, id",
    )?;

    let nodes: Vec<PyramidNode> = stmt
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
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
            entry.locations.push((node.id.clone(), node.depth, topic_name));
        }
    }

    // Filter to entities in 2+ distinct nodes, sort by count descending
    let mut results: Vec<EntityEntry> = index
        .into_values()
        .filter_map(|data| {
            let unique_nodes: HashSet<&str> = data.locations.iter().map(|(n, _, _)| n.as_str()).collect();
            if unique_nodes.len() < 2 {
                return None;
            }
            let mut nodes_sorted: Vec<String> = unique_nodes.into_iter().map(|s| s.to_string()).collect();
            nodes_sorted.sort();
            let mut depths: Vec<i64> = data.locations.iter().map(|(_, d, _)| *d).collect::<HashSet<_>>().into_iter().collect();
            depths.sort();
            let mut topics: Vec<String> = data.locations.iter().map(|(_, _, t)| t.clone()).collect::<HashSet<_>>().into_iter().collect();
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
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
        .collect();

    // Gather upper corrections (depth > 0)
    let mut stmt_upper = conn.prepare(
        "SELECT * FROM live_pyramid_nodes WHERE slug = ? AND depth > 0 \
         ORDER BY depth, id",
    )?;
    let upper_nodes: Vec<PyramidNode> = stmt_upper
        .query_map(rusqlite::params![slug], row_to_node)?
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
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
                    let entry_right_norm: String = entry.right.to_lowercase().chars().take(80).collect();
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
                chains.push((right_norm, vec![ChainEntry {
                    wrong,
                    right,
                    who: c.who.clone(),
                    source: node.id.clone(),
                    chunk_index: ci,
                }]));
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
                chains.push((right_norm, vec![ChainEntry {
                    wrong,
                    right,
                    who: c.who.clone(),
                    source: node.id.clone(),
                    chunk_index: -1,
                }]));
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
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
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
    entries.sort_by(|a, b| b.depth.cmp(&a.depth).then_with(|| a.node_id.cmp(&b.node_id)));

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
        .filter_map(|r| match r { Ok(v) => Some(v), Err(e) => { warn!("Skipping row: {e}"); None } })
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
    entries.sort_by(|a, b| b.depth.cmp(&a.depth).then_with(|| a.node_id.cmp(&b.node_id)));

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
