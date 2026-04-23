// pyramid/reading_modes.rs — WS-READING-MODES (Phase 4): Six reading mode queries
//
// Implements the six rendering modes from the episodic memory vine canonical
// design (Part VII). All modes project from the existing node storage — no
// additional extraction is required.
//
// Modes:
//   1. Memoir  — apex top-to-bottom, dense prose at whole-arc scale
//   2. Walk    — paginated nodes at a specified layer, chronological order
//   3. Thread  — follow a canonical identity across non-adjacent nodes
//   4. Decisions — aggregated decisions[] across corpus, filterable by stance
//   5. Speaker — filter to one speaker role's key_quotes contributions
//   6. Search  — wraps existing search with ancestor node chain

use anyhow::Result;
use rusqlite::Connection;

use super::db;
use super::query;
use super::types::*;

// ── 1. Memoir ──────────────────────────────────────────────────────────────

/// Load the apex node and return its content formatted as a memoir view.
/// Primary cold-start path: dense prose at whole-arc scale.
pub fn reading_memoir(conn: &Connection, slug: &str) -> Result<MemoirView> {
    let apex = query::get_apex(conn, slug)?
        .ok_or_else(|| anyhow::anyhow!("No apex node found for slug '{}'", slug))?;

    Ok(MemoirView {
        slug: slug.to_string(),
        headline: apex.headline,
        distilled: apex.distilled,
        narrative: apex.narrative,
        topics: apex.topics,
        decisions: apex.decisions,
        terms: apex.terms,
    })
}

// ── 2. Walk ────────────────────────────────────────────────────────────────

/// Paginated walk through nodes at a specified layer.
/// Direction: "newest" = DESC by chunk_index (default), "oldest" = ASC.
pub fn reading_walk(
    conn: &Connection,
    slug: &str,
    layer: i64,
    direction: &str,
    offset: usize,
    limit: usize,
) -> Result<WalkView> {
    let total_count = db::count_nodes_at_depth(conn, slug, layer)?;

    let order = if direction == "oldest" { "ASC" } else { "DESC" };

    let sql = format!(
        "SELECT id, chunk_index, headline, distilled, time_range_start, time_range_end, weight, topics
         FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth = ?2
         ORDER BY chunk_index {order}, id {order}
         LIMIT ?3 OFFSET ?4"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![slug, layer, limit as i64, offset as i64],
        |row| {
            let topics_json: String = row.get::<_, String>(7).unwrap_or_default();
            let topics: Vec<Topic> = serde_json::from_str(&topics_json).unwrap_or_default();
            let topic_names: Vec<String> = topics.iter().map(|t| t.name.clone()).collect();

            let time_range_start: Option<String> = row.get(4).ok().flatten();
            let time_range_end: Option<String> = row.get(5).ok().flatten();
            let time_range = if time_range_start.is_some() || time_range_end.is_some() {
                Some(TimeRange {
                    start: time_range_start,
                    end: time_range_end,
                })
            } else {
                None
            };

            Ok(WalkNode {
                id: row.get(0)?,
                chunk_index: row.get(1)?,
                headline: row.get::<_, String>(2).unwrap_or_default(),
                distilled: row.get::<_, String>(3).unwrap_or_default(),
                time_range,
                weight: row.get::<_, f64>(6).unwrap_or(0.0),
                topic_names,
            })
        },
    )?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }

    Ok(WalkView {
        slug: slug.to_string(),
        layer,
        nodes,
        total_count,
        offset,
    })
}

// ── 3. Thread ──────────────────────────────────────────────────────────────

/// Follow a canonical identity (topic, entity, or decision) across all nodes.
/// Searches all live nodes for mentions in topics[], entities[], or decisions[].
/// Results are ordered chronologically (by chunk_index ASC within each depth).
pub fn reading_thread(conn: &Connection, slug: &str, identity: &str) -> Result<ThreadView> {
    let show_all = identity.is_empty() || identity == "*";
    let identity_lower = identity.to_lowercase();
    let all_nodes = db::get_all_live_nodes(conn, slug)?;

    let mut mentions = Vec::new();

    for node in &all_nodes {
        // Check topics
        for topic in &node.topics {
            if show_all || topic.name.to_lowercase().contains(&identity_lower) {
                let importance = topic
                    .extra
                    .get("importance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                mentions.push(ThreadMention {
                    node_id: node.id.clone(),
                    depth: node.depth,
                    headline: node.headline.clone(),
                    mention_type: "topic".to_string(),
                    matched_text: topic.name.clone(),
                    importance,
                    time_range: node.time_range.clone(),
                });
                break; // One mention per node per type is sufficient
            }
        }

        // Check entities
        for entity in &node.entities {
            if show_all || entity.name.to_lowercase().contains(&identity_lower) {
                mentions.push(ThreadMention {
                    node_id: node.id.clone(),
                    depth: node.depth,
                    headline: node.headline.clone(),
                    mention_type: "entity".to_string(),
                    matched_text: entity.name.clone(),
                    importance: entity.importance,
                    time_range: node.time_range.clone(),
                });
                break;
            }
        }

        // Check decisions
        for decision in &node.decisions {
            if show_all || decision.decided.to_lowercase().contains(&identity_lower) {
                mentions.push(ThreadMention {
                    node_id: node.id.clone(),
                    depth: node.depth,
                    headline: node.headline.clone(),
                    mention_type: "decision".to_string(),
                    matched_text: decision.decided.clone(),
                    importance: decision.importance,
                    time_range: node.time_range.clone(),
                });
                break;
            }
        }
    }

    // Deduplicate: keep highest-importance mention per node_id
    let mut best_per_node: std::collections::HashMap<String, ThreadMention> =
        std::collections::HashMap::new();
    for mention in mentions {
        let existing = best_per_node.get(&mention.node_id);
        if existing.is_none() || existing.unwrap().importance < mention.importance {
            best_per_node.insert(mention.node_id.clone(), mention);
        }
    }

    let mut result: Vec<ThreadMention> = best_per_node.into_values().collect();
    // Sort chronologically: lower depth first, then by node_id as proxy for time
    result.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    Ok(ThreadView {
        slug: slug.to_string(),
        identity: identity.to_string(),
        mentions: result,
    })
}

// ── 4. Decisions Ledger ────────────────────────────────────────────────────

/// Aggregate all decisions[] from all live nodes, filterable by stance.
/// Sorted by importance DESC.
pub fn reading_decisions(
    conn: &Connection,
    slug: &str,
    stance: Option<&str>,
) -> Result<DecisionsView> {
    let all_nodes = db::get_all_live_nodes(conn, slug)?;

    let mut entries = Vec::new();

    for node in &all_nodes {
        for decision in &node.decisions {
            if let Some(filter_stance) = stance {
                if decision.stance != filter_stance {
                    continue;
                }
            }
            entries.push(DecisionEntry {
                decided: decision.decided.clone(),
                why: decision.why.clone(),
                stance: decision.stance.clone(),
                importance: decision.importance,
                related: decision.related.clone(),
                source_node_id: node.id.clone(),
                source_headline: node.headline.clone(),
                source_depth: node.depth,
            });
        }
    }

    // Sort by importance DESC
    entries.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_count = entries.len();

    Ok(DecisionsView {
        slug: slug.to_string(),
        decisions: entries,
        total_count,
    })
}

// ── 5. Speaker ─────────────────────────────────────────────────────────────

/// Filter key_quotes[] across all nodes by speaker_role.
/// Sorted by importance DESC.
pub fn reading_speaker(conn: &Connection, slug: &str, role: &str) -> Result<SpeakerView> {
    let all_nodes = db::get_all_live_nodes(conn, slug)?;

    let mut quotes = Vec::new();

    for node in &all_nodes {
        for quote in &node.key_quotes {
            if quote.speaker_role == role {
                quotes.push(SpeakerQuote {
                    text: quote.text.clone(),
                    speaker_role: quote.speaker_role.clone(),
                    importance: quote.importance,
                    source_node_id: node.id.clone(),
                    source_headline: node.headline.clone(),
                    source_depth: node.depth,
                });
            }
        }
    }

    // Sort by importance DESC
    quotes.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_count = quotes.len();

    Ok(SpeakerView {
        slug: slug.to_string(),
        role: role.to_string(),
        quotes,
        total_count,
    })
}

// ── 6. Search ──────────────────────────────────────────────────────────────

/// Wraps the existing query::search with ancestor node chain.
/// For each search hit, walks up the parent_id chain to build an ancestry
/// trail from the hit node to the apex.
pub fn reading_search(
    conn: &Connection,
    slug: &str,
    query_str: &str,
    limit: usize,
) -> Result<SearchReadingView> {
    let hits = query::search(conn, slug, query_str)?;

    let mut results = Vec::new();
    for hit in hits.iter().take(limit) {
        let ancestors = build_ancestor_chain(conn, slug, &hit.node_id)?;

        results.push(SearchReadingHit {
            node_id: hit.node_id.clone(),
            depth: hit.depth,
            headline: hit.headline.clone(),
            snippet: hit.snippet.clone(),
            score: hit.score,
            ancestors,
        });
    }

    let total_count = results.len();

    Ok(SearchReadingView {
        slug: slug.to_string(),
        query: query_str.to_string(),
        results,
        total_count,
    })
}

/// Walk up the parent_id chain from a node to the apex, collecting ancestor
/// nodes along the way. Returns ancestors ordered from immediate parent to apex.
fn build_ancestor_chain(conn: &Connection, slug: &str, node_id: &str) -> Result<Vec<AncestorNode>> {
    let mut ancestors = Vec::new();
    let mut current_id = node_id.to_string();

    // Safety limit to prevent infinite loops
    for _ in 0..20 {
        let node = match db::get_live_node(conn, slug, &current_id)? {
            Some(n) => n,
            None => break,
        };

        match &node.parent_id {
            Some(parent_id) if !parent_id.is_empty() => {
                // Fetch the parent to get its metadata
                if let Some(parent) = db::get_live_node(conn, slug, parent_id)? {
                    ancestors.push(AncestorNode {
                        node_id: parent.id.clone(),
                        depth: parent.depth,
                        headline: parent.headline.clone(),
                    });
                    current_id = parent.id;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    Ok(ancestors)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    /// Helper: create an in-memory DB with schema and a test slug + nodes.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        db::init_pyramid_db(&conn).expect("init schema");

        // Create a test slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path, node_count, max_depth, created_at)
             VALUES ('test-slug', 'code', '/tmp/test', 0, 2, '2026-04-08T00:00:00Z')",
            [],
        )
        .expect("insert slug");

        // Insert L0 nodes
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, created_at, weight, narrative_json, entities_json, key_quotes_json, transitions_json, current_version)
             VALUES ('L0-0', 'test-slug', 0, 0, 'First chunk', 'The first chunk content', '[]', '[]',
                     '[{\"decided\":\"Use Rust\",\"why\":\"Performance\",\"rejected\":\"\",\"stance\":\"committed\",\"importance\":0.9,\"related\":[]}]',
                     '[{\"term\":\"pyramid\",\"definition\":\"A recursive memory artifact\"}]',
                     '[]', '', '[]', '2026-04-08T00:00:00Z', 0.5,
                     '{\"levels\":[]}', '[]',
                     '[{\"text\":\"We should use Rust\",\"speaker_role\":\"human\",\"importance\":0.8}]',
                     '{\"prior\":\"\",\"next\":\"\"}', 1)",
            [],
        )
        .expect("insert L0-0");

        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, created_at, weight, narrative_json, entities_json, key_quotes_json, transitions_json, current_version)
             VALUES ('L0-1', 'test-slug', 0, 1, 'Second chunk', 'The second chunk about authentication',
                     '[{\"name\":\"authentication\",\"current\":\"Active\",\"entities\":[],\"corrections\":[],\"decisions\":[]}]',
                     '[]',
                     '[{\"decided\":\"Use JWT tokens\",\"why\":\"Standard auth\",\"rejected\":\"sessions\",\"stance\":\"committed\",\"importance\":0.7,\"related\":[\"authentication\"]}]',
                     '[]', '[]', '', '[]', '2026-04-08T00:01:00Z', 0.5,
                     '{\"levels\":[]}',
                     '[{\"name\":\"Auth Service\",\"role\":\"component\",\"importance\":0.6,\"liveness\":\"live\"}]',
                     '[{\"text\":\"JWT is the way to go\",\"speaker_role\":\"agent\",\"importance\":0.5}]',
                     '{\"prior\":\"\",\"next\":\"\"}', 1)",
            [],
        )
        .expect("insert L0-1");

        // Insert L1 node (parent of L0 nodes)
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, created_at, weight, narrative_json, entities_json, key_quotes_json, transitions_json, current_version)
             VALUES ('L1-0', 'test-slug', 1, 0, 'Phase overview', 'Overview of the initial setup phase',
                     '[{\"name\":\"authentication\",\"current\":\"Active\",\"entities\":[],\"corrections\":[],\"decisions\":[]}]',
                     '[]',
                     '[{\"decided\":\"Use Rust\",\"why\":\"Performance\",\"rejected\":\"\",\"stance\":\"committed\",\"importance\":0.9,\"related\":[]},{\"decided\":\"Use JWT tokens\",\"why\":\"Standard auth\",\"rejected\":\"sessions\",\"stance\":\"open\",\"importance\":0.7,\"related\":[\"authentication\"]}]',
                     '[{\"term\":\"pyramid\",\"definition\":\"A recursive memory artifact\"}]',
                     '[]', '', '[\"L0-0\",\"L0-1\"]', '2026-04-08T00:02:00Z', 1.0,
                     '{\"levels\":[{\"zoom\":1,\"text\":\"The project began with technology decisions.\"}]}',
                     '[{\"name\":\"Auth Service\",\"role\":\"component\",\"importance\":0.6,\"liveness\":\"live\"}]',
                     '[{\"text\":\"We should use Rust\",\"speaker_role\":\"human\",\"importance\":0.8},{\"text\":\"JWT is the way to go\",\"speaker_role\":\"agent\",\"importance\":0.5}]',
                     '{\"prior\":\"\",\"next\":\"\"}', 1)",
            [],
        )
        .expect("insert L1-0");

        // Insert apex (L2) node
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, created_at, weight, narrative_json, entities_json, key_quotes_json, transitions_json, current_version)
             VALUES ('APEX', 'test-slug', 2, 0, 'Test Project Apex', 'Full project arc overview covering technology and auth decisions',
                     '[{\"name\":\"authentication\",\"current\":\"Active\",\"entities\":[],\"corrections\":[],\"decisions\":[]}]',
                     '[]',
                     '[{\"decided\":\"Use Rust\",\"why\":\"Performance\",\"rejected\":\"\",\"stance\":\"committed\",\"importance\":0.9,\"related\":[]},{\"decided\":\"Use JWT tokens\",\"why\":\"Standard auth\",\"rejected\":\"sessions\",\"stance\":\"committed\",\"importance\":0.7,\"related\":[\"authentication\"]}]',
                     '[{\"term\":\"pyramid\",\"definition\":\"A recursive memory artifact\"}]',
                     '[]', '', '[\"L1-0\"]', '2026-04-08T00:03:00Z', 1.0,
                     '{\"levels\":[{\"zoom\":1,\"text\":\"This project uses Rust for performance and JWT for authentication.\"}]}',
                     '[{\"name\":\"Auth Service\",\"role\":\"component\",\"importance\":0.6,\"liveness\":\"live\"}]',
                     '[{\"text\":\"We should use Rust\",\"speaker_role\":\"human\",\"importance\":0.8}]',
                     '{\"prior\":\"\",\"next\":\"\"}', 1)",
            [],
        )
        .expect("insert APEX");

        // Set parent_id relationships
        conn.execute(
            "UPDATE pyramid_nodes SET parent_id = 'L1-0' WHERE id IN ('L0-0', 'L0-1') AND slug = 'test-slug'",
            [],
        )
        .expect("set L0 parent_id");

        conn.execute(
            "UPDATE pyramid_nodes SET parent_id = 'APEX' WHERE id = 'L1-0' AND slug = 'test-slug'",
            [],
        )
        .expect("set L1 parent_id");

        // Update slug stats
        conn.execute(
            "UPDATE pyramid_slugs SET node_count = 4, max_depth = 2 WHERE slug = 'test-slug'",
            [],
        )
        .expect("update slug stats");

        conn
    }

    #[test]
    fn test_memoir_returns_apex_content() {
        let conn = setup_test_db();
        let result = reading_memoir(&conn, "test-slug").unwrap();

        assert_eq!(result.slug, "test-slug");
        assert_eq!(result.headline, "Test Project Apex");
        assert!(result.distilled.contains("project arc"));
        assert!(!result.topics.is_empty());
        assert!(!result.decisions.is_empty());
        assert!(!result.terms.is_empty());
        // Verify we got apex-level decisions
        assert!(result.decisions.iter().any(|d| d.decided == "Use Rust"));
    }

    #[test]
    fn test_walk_returns_paginated_nodes() {
        let conn = setup_test_db();

        // Walk L0, newest first
        let result = reading_walk(&conn, "test-slug", 0, "newest", 0, 20).unwrap();
        assert_eq!(result.slug, "test-slug");
        assert_eq!(result.layer, 0);
        assert_eq!(result.total_count, 2);
        assert_eq!(result.nodes.len(), 2);
        // Newest first = higher chunk_index first
        assert_eq!(result.nodes[0].chunk_index, Some(1));
        assert_eq!(result.nodes[1].chunk_index, Some(0));

        // Walk L0, oldest first
        let result = reading_walk(&conn, "test-slug", 0, "oldest", 0, 20).unwrap();
        assert_eq!(result.nodes[0].chunk_index, Some(0));
        assert_eq!(result.nodes[1].chunk_index, Some(1));

        // Pagination: offset=1, limit=1
        let result = reading_walk(&conn, "test-slug", 0, "newest", 1, 1).unwrap();
        assert_eq!(result.total_count, 2);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.offset, 1);
    }

    #[test]
    fn test_decisions_aggregation_flattens() {
        let conn = setup_test_db();

        // All decisions
        let result = reading_decisions(&conn, "test-slug", None).unwrap();
        assert!(result.total_count > 0);
        // Should have decisions from multiple nodes
        let node_ids: std::collections::HashSet<&str> = result
            .decisions
            .iter()
            .map(|d| d.source_node_id.as_str())
            .collect();
        assert!(
            node_ids.len() > 1,
            "Decisions should come from multiple nodes"
        );

        // Sorted by importance DESC
        for window in result.decisions.windows(2) {
            assert!(window[0].importance >= window[1].importance);
        }

        // Filter by stance=committed
        let committed = reading_decisions(&conn, "test-slug", Some("committed")).unwrap();
        for d in &committed.decisions {
            assert_eq!(d.stance, "committed");
        }

        // Filter by stance that doesn't exist
        let none = reading_decisions(&conn, "test-slug", Some("deferred")).unwrap();
        assert_eq!(none.total_count, 0);
    }

    #[test]
    fn test_search_wraps_with_ancestry() {
        let conn = setup_test_db();

        let result = reading_search(&conn, "test-slug", "authentication", 20).unwrap();
        assert_eq!(result.slug, "test-slug");
        assert_eq!(result.query, "authentication");

        // Should find hits containing "authentication"
        if !result.results.is_empty() {
            let hit = &result.results[0];
            // Verify ancestry is populated for non-apex nodes
            if hit.depth < 2 {
                assert!(
                    !hit.ancestors.is_empty(),
                    "Non-apex search hits should have ancestors"
                );
            }
        }
    }
}
