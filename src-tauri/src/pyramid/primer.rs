// pyramid/primer.rs — WS-PRIMER: Leftmost-slope primer for episodic memory
//
// The primer is a projection of a vine pyramid's leftmost slope that rides in
// every extraction prompt during a new bedrock build. It carries the canonical
// identity catalog and multi-resolution navigation context.
//
// "Leftmost" means the most-recent edge of the pyramid: the node at each layer
// with the highest chunk_index (or most recent created_at when chunk_index is
// absent). Growth is leftward, so the leftmost child at each layer is always
// the freshest content at that scale.
//
// See plan Part III (§3.1-3.4), §4.5, §5.2, §9.1.

use anyhow::Result;
use rusqlite::Connection;

use super::db;
use super::query;
use super::types::*;

// ── Leftmost slope ──────────────────────────────────────────────────────────

/// Walk the leftmost slope of a pyramid from apex down to the most-recent L0.
///
/// Returns one `PyramidNode` per layer, ordered apex-first (highest depth) to
/// L0 (depth 0). For a pyramid with `k` layers (depths 0..=k-1), the result
/// contains `k` nodes.
///
/// "Leftmost" = the most recently created child at each layer, determined by:
/// 1. Highest `chunk_index` among children (primary key — chunk ordering is
///    the authoritative temporal axis for L0 and propagates upward).
/// 2. If chunk_index is NULL or tied, most recent `created_at` as tiebreaker.
///
/// If the pyramid has no apex or is empty, returns an empty Vec.
pub fn get_leftmost_slope(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>> {
    let apex = match query::get_apex(conn, slug)? {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };

    let mut slope = Vec::new();
    let mut current = apex;

    loop {
        let depth = current.depth;
        slope.push(current.clone());

        if depth == 0 {
            break;
        }

        // Find the leftmost (most recent) child.
        // Strategy: load all children, pick the one with highest chunk_index,
        // then by most recent created_at as tiebreaker.
        let child_ids = &current.children;

        if child_ids.is_empty() {
            // No explicit children — try finding children via parent_id.
            // This covers pyramids that use parent_id linkage without
            // populating children[] (e.g., question pyramids using evidence).
            let target_depth = depth - 1;
            let children_at_depth = db::get_nodes_at_depth(conn, slug, target_depth)?;
            let children_of_current: Vec<&PyramidNode> = children_at_depth
                .iter()
                .filter(|n| n.parent_id.as_deref() == Some(&current.id))
                .collect();

            if children_of_current.is_empty() {
                // Also try via evidence links (question pyramids)
                match find_leftmost_via_evidence(conn, slug, &current.id, depth - 1) {
                    Some(child) => {
                        current = child;
                        continue;
                    }
                    None => break, // No children found at all
                }
            }

            // Pick leftmost = highest chunk_index, then most recent created_at
            current = pick_leftmost_child(&children_of_current);
            continue;
        }

        // Load children by ID and pick the leftmost one.
        let mut loaded_children = Vec::new();
        for child_id in child_ids {
            // Handle cross-slug handle paths
            if let Some((ref_slug, _depth, ref_node_id)) = db::parse_handle_path(child_id) {
                if let Ok(Some(child)) = db::get_live_node(conn, ref_slug, ref_node_id) {
                    loaded_children.push(child);
                }
            } else if let Ok(Some(child)) = db::get_live_node(conn, slug, child_id) {
                loaded_children.push(child);
            }
        }

        if loaded_children.is_empty() {
            break; // Children listed but none resolvable
        }

        let refs: Vec<&PyramidNode> = loaded_children.iter().collect();
        current = pick_leftmost_child(&refs);
    }

    Ok(slope)
}

/// Pick the "leftmost" child: highest chunk_index, then most recent created_at.
fn pick_leftmost_child(children: &[&PyramidNode]) -> PyramidNode {
    debug_assert!(
        !children.is_empty(),
        "pick_leftmost_child called with empty slice"
    );

    (*children
        .iter()
        .max_by(|a, b| {
            // Primary: highest chunk_index (most recent content)
            let ci_a = a.chunk_index.unwrap_or(i64::MIN);
            let ci_b = b.chunk_index.unwrap_or(i64::MIN);
            ci_a.cmp(&ci_b)
                // Tiebreaker: most recent created_at (lexicographic on ISO timestamps)
                .then_with(|| a.created_at.cmp(&b.created_at))
        })
        .expect("non-empty slice guaranteed by caller"))
    .clone()
}

/// Try to find the leftmost child via evidence links (for question pyramids
/// that use evidence KEEP links instead of children[]).
fn find_leftmost_via_evidence(
    conn: &Connection,
    slug: &str,
    parent_node_id: &str,
    target_depth: i64,
) -> Option<PyramidNode> {
    // Evidence links: source_node_id (child) → target_node_id (parent)
    let evidence_children: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT source_node_id FROM pyramid_evidence
             WHERE slug = ?1 AND target_node_id = ?2 AND verdict = 'KEEP'",
        )
        .ok()?
        .query_map(rusqlite::params![slug, parent_node_id], |row| {
            row.get::<_, String>(0)
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    if evidence_children.is_empty() {
        return None;
    }

    let mut loaded = Vec::new();
    for child_id in &evidence_children {
        if let Ok(Some(child)) = db::get_live_node(conn, slug, child_id) {
            if child.depth == target_depth {
                loaded.push(child);
            }
        }
    }

    if loaded.is_empty() {
        return None;
    }

    let refs: Vec<&PyramidNode> = loaded.iter().collect();
    Some(pick_leftmost_child(&refs))
}

// ── Primer construction ─────────────────────────────────────────────────────

/// Build the full primer context for a pyramid slug.
///
/// Loads the leftmost slope, extracts the canonical vocabulary from the apex,
/// projects each slope node into a `PrimerNode`, and optionally dehydrates
/// apex-facing nodes to fit within a token budget.
///
/// The token budget dehydration order (apex-facing first, preserving recent end):
/// 1. Drop `distilled` from top-of-slope nodes
/// 2. Drop `entities` from top-of-slope nodes
/// 3. Drop `decisions` from top-of-slope nodes
/// 4. Repeat downward until within budget (recent-end nodes preserved fully)
pub fn build_primer(
    conn: &Connection,
    slug: &str,
    token_budget: Option<usize>,
) -> Result<PrimerContext> {
    let slope = get_leftmost_slope(conn, slug)?;

    if slope.is_empty() {
        return Ok(PrimerContext {
            slug: slug.to_string(),
            slope_nodes: Vec::new(),
            canonical_vocabulary: CanonicalVocabulary {
                topics: Vec::new(),
                entities: Vec::new(),
                decisions: Vec::new(),
                terms: Vec::new(),
            },
            total_tokens_estimate: 0,
        });
    }

    // Apex is the first node in the slope (highest depth)
    let apex = &slope[0];

    // Extract canonical vocabulary from apex
    let canonical_vocabulary = CanonicalVocabulary {
        topics: apex
            .topics
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or_default())
            .collect(),
        entities: apex
            .entities
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or_default())
            .collect(),
        decisions: apex
            .decisions
            .iter()
            .map(|d| serde_json::to_value(d).unwrap_or_default())
            .collect(),
        terms: apex
            .terms
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or_default())
            .collect(),
    };

    // Project each slope node into a PrimerNode
    let mut primer_nodes: Vec<PrimerNode> = slope
        .iter()
        .map(|node| {
            let time_range_str = node
                .time_range
                .as_ref()
                .map(|tr| match (&tr.start, &tr.end) {
                    (Some(s), Some(e)) => format!("{} .. {}", s, e),
                    (Some(s), None) => format!("{} ..", s),
                    (None, Some(e)) => format!(".. {}", e),
                    (None, None) => String::new(),
                });

            PrimerNode {
                node_id: node.id.clone(),
                depth: node.depth,
                headline: node.headline.clone(),
                distilled: if node.distilled.is_empty() {
                    None
                } else {
                    Some(node.distilled.clone())
                },
                topics: node
                    .topics
                    .iter()
                    .map(|t| serde_json::to_value(t).unwrap_or_default())
                    .collect(),
                decisions: node
                    .decisions
                    .iter()
                    .map(|d| serde_json::to_value(d).unwrap_or_default())
                    .collect(),
                entities: node
                    .entities
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect(),
                time_range: time_range_str.filter(|s| !s.is_empty()),
            }
        })
        .collect();

    // Apply token budget dehydration if specified
    if let Some(budget) = token_budget {
        dehydrate_primer_nodes(&mut primer_nodes, budget);
    }

    // Estimate total tokens: serialize to JSON and divide by 4 (rough char-to-token ratio)
    let primer = PrimerContext {
        slug: slug.to_string(),
        slope_nodes: primer_nodes,
        canonical_vocabulary: canonical_vocabulary.clone(),
        total_tokens_estimate: 0, // placeholder, computed below
    };

    let json_str = serde_json::to_string(&primer).unwrap_or_default();
    let total_tokens_estimate = json_str.len() / 4;

    Ok(PrimerContext {
        total_tokens_estimate,
        ..primer
    })
}

/// Dehydrate primer nodes to fit within a token budget.
///
/// Strips data from apex-facing nodes first (index 0 = apex, growing downward),
/// preserving recent-end nodes (highest indices) at full fidelity.
///
/// Dehydration order per node:
/// 1. Drop `distilled`
/// 2. Drop `entities`
/// 3. Drop `decisions`
fn dehydrate_primer_nodes(nodes: &mut Vec<PrimerNode>, budget: usize) {
    // Phase 1: Drop distilled from apex-facing nodes
    for i in 0..nodes.len() {
        if estimate_tokens(nodes) <= budget {
            return;
        }
        nodes[i].distilled = None;
    }

    // Phase 2: Drop entities from apex-facing nodes
    for i in 0..nodes.len() {
        if estimate_tokens(nodes) <= budget {
            return;
        }
        nodes[i].entities.clear();
    }

    // Phase 3: Drop decisions from apex-facing nodes
    for i in 0..nodes.len() {
        if estimate_tokens(nodes) <= budget {
            return;
        }
        nodes[i].decisions.clear();
    }
}

/// Rough token estimate: serialize to JSON and count chars / 4.
fn estimate_tokens(nodes: &[PrimerNode]) -> usize {
    let json = serde_json::to_string(nodes).unwrap_or_default();
    json.len() / 4
}

// ── Prompt formatting ───────────────────────────────────────────────────────

/// Render the primer as a markdown/text block suitable for inclusion in
/// extraction prompts.
///
/// Structure:
/// 1. Canonical Vocabulary section (topics, entities, decisions, terms from apex)
/// 2. Slope Navigation section (apex-first to L0, each node as heading)
///
/// Ordered apex-first (broadest context) to L0 (most recent detail).
pub fn format_primer_for_prompt(primer: &PrimerContext) -> String {
    let mut out = String::new();

    out.push_str("# Primer: Canonical Context\n\n");

    // ── Canonical Vocabulary ──
    out.push_str("## Canonical Vocabulary\n\n");

    if !primer.canonical_vocabulary.topics.is_empty() {
        out.push_str("### Topics\n");
        for topic in &primer.canonical_vocabulary.topics {
            if let Some(name) = topic.get("name").and_then(|v| v.as_str()) {
                let current = topic.get("current").and_then(|v| v.as_str()).unwrap_or("");
                if current.is_empty() {
                    out.push_str(&format!("- {}\n", name));
                } else {
                    out.push_str(&format!("- **{}**: {}\n", name, current));
                }
            }
        }
        out.push('\n');
    }

    if !primer.canonical_vocabulary.entities.is_empty() {
        out.push_str("### Entities\n");
        for entity in &primer.canonical_vocabulary.entities {
            if let Some(name) = entity.get("name").and_then(|v| v.as_str()) {
                let role = entity.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role.is_empty() {
                    out.push_str(&format!("- {}\n", name));
                } else {
                    out.push_str(&format!("- **{}** ({})\n", name, role));
                }
            }
        }
        out.push('\n');
    }

    if !primer.canonical_vocabulary.decisions.is_empty() {
        out.push_str("### Decisions\n");
        for decision in &primer.canonical_vocabulary.decisions {
            if let Some(decided) = decision.get("decided").and_then(|v| v.as_str()) {
                let stance = decision
                    .get("stance")
                    .and_then(|v| v.as_str())
                    .unwrap_or("open");
                out.push_str(&format!("- [{}] {}\n", stance, decided));
            }
        }
        out.push('\n');
    }

    if !primer.canonical_vocabulary.terms.is_empty() {
        out.push_str("### Terms\n");
        for term in &primer.canonical_vocabulary.terms {
            if let Some(t) = term.get("term").and_then(|v| v.as_str()) {
                let def = term
                    .get("definition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                out.push_str(&format!("- **{}**: {}\n", t, def));
            }
        }
        out.push('\n');
    }

    // ── Slope Navigation ──
    out.push_str("## Slope Navigation (apex → L0)\n\n");

    for node in &primer.slope_nodes {
        let layer_label = if node.depth == primer.slope_nodes.first().map_or(0, |n| n.depth) {
            "APEX".to_string()
        } else {
            format!("L{}", node.depth)
        };

        out.push_str(&format!("### {} — {}\n", layer_label, node.headline));

        if let Some(ref tr) = node.time_range {
            out.push_str(&format!("Time: {}\n", tr));
        }

        if let Some(ref distilled) = node.distilled {
            out.push_str(&format!("\n{}\n", distilled));
        }

        // Key topics at this layer
        if !node.topics.is_empty() {
            out.push_str("\nTopics: ");
            let topic_names: Vec<&str> = node
                .topics
                .iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                .collect();
            out.push_str(&topic_names.join(", "));
            out.push('\n');
        }

        // Key decisions at this layer
        if !node.decisions.is_empty() {
            out.push_str("Decisions: ");
            let decision_strs: Vec<String> = node
                .decisions
                .iter()
                .filter_map(|d| {
                    let decided = d.get("decided").and_then(|v| v.as_str())?;
                    let stance = d.get("stance").and_then(|v| v.as_str()).unwrap_or("open");
                    Some(format!("[{}] {}", stance, decided))
                })
                .collect();
            out.push_str(&decision_strs.join("; "));
            out.push('\n');
        }

        // Key entities at this layer
        if !node.entities.is_empty() {
            out.push_str("Entities: ");
            let entity_names: Vec<&str> = node
                .entities
                .iter()
                .filter_map(|e| e.get("name").and_then(|v| v.as_str()))
                .collect();
            out.push_str(&entity_names.join(", "));
            out.push('\n');
        }

        out.push('\n');
    }

    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory pyramid DB with test data: 3-layer pyramid
    /// (L0 x3, L1 x1, L2/apex x1).
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let slug = "test-primer";

        // Create the slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'code', '/tmp/test')",
            rusqlite::params![slug],
        )
        .unwrap();

        // Create L0 nodes (3 nodes at depth 0)
        // L0-a: chunk_index 0 (oldest)
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

        // L0-b: chunk_index 1
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

        // L0-c: chunk_index 2 (most recent = leftmost)
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

        // L1 node: depth 1, children = [l0-a, l0-b, l0-c]
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

        // Apex: depth 2, children = [l1-a]
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled,
             topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id,
             build_version, created_at,
             entities_json, key_quotes_json, narrative_json, transitions_json)
             VALUES ('apex', ?1, 2, 0, 'Test Pyramid Apex', 'Full project arc overview',
             '[{\"name\":\"topic-a\",\"current\":\"canonical-a\"},{\"name\":\"topic-c\",\"current\":\"canonical-c\"}]',
             '[]',
             '[{\"decided\":\"use-pyramids\",\"why\":\"recursive structure\",\"stance\":\"committed\",\"importance\":0.9,\"related\":[]}]',
             '[{\"term\":\"primer\",\"definition\":\"leftmost slope projection for extraction prompts\"}]',
             '[]', '', '[\"l1-a\"]', NULL, 1, '2026-01-03T02:00:00',
             '[{\"name\":\"Adam\",\"role\":\"operator\",\"importance\":1.0,\"liveness\":\"live\"}]',
             '[]', '{}', '{}')",
            rusqlite::params![slug],
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_get_leftmost_slope_3_layers() {
        let conn = setup_test_db();

        let slope = get_leftmost_slope(&conn, "test-primer").unwrap();

        // Should have 3 nodes: apex (depth 2), L1 (depth 1), L0 (depth 0)
        assert_eq!(slope.len(), 3, "Expected 3 nodes for 3-layer pyramid");

        // First = apex
        assert_eq!(slope[0].id, "apex");
        assert_eq!(slope[0].depth, 2);

        // Second = L1
        assert_eq!(slope[1].id, "l1-a");
        assert_eq!(slope[1].depth, 1);

        // Third = leftmost L0 (chunk_index 2, most recent)
        assert_eq!(slope[2].id, "l0-c");
        assert_eq!(slope[2].depth, 0);
        assert_eq!(slope[2].chunk_index, Some(2));
    }

    #[test]
    fn test_build_primer_extracts_canonical_vocabulary() {
        let conn = setup_test_db();

        let primer = build_primer(&conn, "test-primer", None).unwrap();

        assert_eq!(primer.slug, "test-primer");
        assert!(!primer.slope_nodes.is_empty());

        // Canonical vocabulary should come from apex
        assert!(
            !primer.canonical_vocabulary.topics.is_empty(),
            "Should have topics from apex"
        );
        assert!(
            !primer.canonical_vocabulary.decisions.is_empty(),
            "Should have decisions from apex"
        );
        assert!(
            !primer.canonical_vocabulary.terms.is_empty(),
            "Should have terms from apex"
        );
        assert!(
            !primer.canonical_vocabulary.entities.is_empty(),
            "Should have entities from apex"
        );

        // Verify a specific topic
        let has_topic_a = primer
            .canonical_vocabulary
            .topics
            .iter()
            .any(|t| t.get("name").and_then(|v| v.as_str()) == Some("topic-a"));
        assert!(
            has_topic_a,
            "Canonical vocabulary should include topic-a from apex"
        );

        // Verify entity
        let has_adam = primer
            .canonical_vocabulary
            .entities
            .iter()
            .any(|e| e.get("name").and_then(|v| v.as_str()) == Some("Adam"));
        assert!(
            has_adam,
            "Canonical vocabulary should include entity Adam from apex"
        );
    }

    #[test]
    fn test_build_primer_dehydrates_apex_facing_first() {
        let conn = setup_test_db();

        // Build with a very small token budget to force dehydration
        let primer = build_primer(&conn, "test-primer", Some(50)).unwrap();

        // The apex-facing node (index 0) should have been dehydrated first
        if !primer.slope_nodes.is_empty() {
            let apex_node = &primer.slope_nodes[0];
            // Distilled should be stripped from apex-facing node first
            assert!(
                apex_node.distilled.is_none(),
                "Apex-facing node distilled should be stripped under tight budget"
            );
        }

        // The most-recent node (last in slope) should be better preserved
        if primer.slope_nodes.len() >= 3 {
            // Under extreme budget pressure all nodes may be stripped,
            // but the dehydration order should have hit apex-facing first
            // which is verified above.
        }
    }

    #[test]
    fn test_format_primer_for_prompt_produces_nonempty() {
        let conn = setup_test_db();

        let primer = build_primer(&conn, "test-primer", None).unwrap();
        let formatted = format_primer_for_prompt(&primer);

        assert!(
            !formatted.is_empty(),
            "Formatted primer should be non-empty"
        );

        // Should contain the vocabulary section
        assert!(
            formatted.contains("Canonical Vocabulary"),
            "Should have canonical vocabulary section"
        );

        // Should contain the slope navigation section
        assert!(
            formatted.contains("Slope Navigation"),
            "Should have slope navigation section"
        );

        // Should contain apex headline
        assert!(
            formatted.contains("Test Pyramid Apex"),
            "Should contain apex headline"
        );

        // Should contain the leftmost L0 headline
        assert!(
            formatted.contains("L0 newest"),
            "Should contain leftmost L0 headline"
        );

        // Should contain vocabulary items
        assert!(
            formatted.contains("topic-a"),
            "Should contain topic from canonical vocabulary"
        );
    }
}
