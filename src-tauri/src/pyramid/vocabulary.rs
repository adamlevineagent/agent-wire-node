// pyramid/vocabulary.rs — Vocabulary catalog extraction, persistence, and query semantics
//
// WS-VOCAB (Phase 3): Implements the vocabulary pyramid's mechanical substrate.
// Extracts canonical identities from a pyramid's apex node, persists them in
// `pyramid_vocabulary_catalog`, and provides the four query semantics from §5.3:
//   - Recognition: does this term match a known identity?
//   - Drill: given a category, find specific identities
//   - Reverse: given an identity, find its category and neighbors
//   - Diff: what's new since a given timestamp?

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::query;
use super::types::{VocabEntry, VocabReverseResult, VocabularyCatalog};

// ── Extraction ──────────────────────────────────────────────────────────────

/// Extract the vocabulary catalog from a pyramid's apex node.
///
/// Reads the apex node and extracts all canonical identities: topics, entities,
/// decisions, terms. Practices are extracted from topics whose `extra` map
/// contains a `"practice"` key (LLM-driven; the Topic's `extra` field is a
/// pass-through for arbitrary LLM output).
pub fn extract_vocabulary_catalog(conn: &Connection, slug: &str) -> Result<VocabularyCatalog> {
    let apex = query::get_apex(conn, slug)?
        .ok_or_else(|| anyhow::anyhow!("No apex node found for slug '{}'", slug))?;

    let now = chrono::Utc::now().to_rfc3339();

    // Extract topics
    let topics: Vec<VocabEntry> = apex
        .topics
        .iter()
        .map(|t| {
            let importance = t.extra.get("importance").and_then(|v| v.as_f64());
            let liveness = t
                .extra
                .get("liveness")
                .and_then(|v| v.as_str())
                .unwrap_or("live")
                .to_string();
            let category = t
                .extra
                .get("category")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            VocabEntry {
                name: t.name.clone(),
                category,
                importance,
                liveness,
                detail: serde_json::to_value(t).unwrap_or_default(),
            }
        })
        .collect();

    // Extract entities
    let entities: Vec<VocabEntry> = apex
        .entities
        .iter()
        .map(|e| {
            let liveness = if e.liveness.is_empty() {
                "live".to_string()
            } else {
                e.liveness.clone()
            };
            let importance = if e.importance > 0.0 {
                Some(e.importance)
            } else {
                None
            };
            VocabEntry {
                name: e.name.clone(),
                category: if e.role.is_empty() {
                    None
                } else {
                    Some(e.role.clone())
                },
                importance,
                liveness,
                detail: serde_json::to_value(e).unwrap_or_default(),
            }
        })
        .collect();

    // Extract decisions
    let decisions: Vec<VocabEntry> = apex
        .decisions
        .iter()
        .map(|d| {
            let importance = if d.importance > 0.0 {
                Some(d.importance)
            } else {
                None
            };
            // Decisions don't have liveness per se; infer from stance:
            // "ruled_out" and "superseded" → mooted, everything else → live
            let liveness = match d.stance.as_str() {
                "ruled_out" | "superseded" => "mooted".to_string(),
                _ => "live".to_string(),
            };
            VocabEntry {
                name: d.decided.clone(),
                category: Some(d.stance.clone()),
                importance,
                liveness,
                detail: serde_json::to_value(d).unwrap_or_default(),
            }
        })
        .collect();

    // Extract terms
    let terms: Vec<VocabEntry> = apex
        .terms
        .iter()
        .map(|t| VocabEntry {
            name: t.term.clone(),
            category: None,
            importance: None,
            liveness: "live".to_string(),
            detail: serde_json::to_value(t).unwrap_or_default(),
        })
        .collect();

    // Extract practices: topics whose `extra` contains a "practice" key
    let practices: Vec<VocabEntry> = apex
        .topics
        .iter()
        .filter(|t| t.extra.contains_key("practice"))
        .map(|t| {
            let importance = t.extra.get("importance").and_then(|v| v.as_f64());
            let liveness = t
                .extra
                .get("liveness")
                .and_then(|v| v.as_str())
                .unwrap_or("live")
                .to_string();
            VocabEntry {
                name: t.name.clone(),
                category: Some("practice".to_string()),
                importance,
                liveness,
                detail: serde_json::to_value(t).unwrap_or_default(),
            }
        })
        .collect();

    let total = topics.len() + entities.len() + decisions.len() + terms.len() + practices.len();

    Ok(VocabularyCatalog {
        slug: slug.to_string(),
        topics,
        entities,
        decisions,
        terms,
        practices,
        total_entries: total,
        extracted_at: now,
    })
}

// ── Persistence ─────────────────────────────────────────────────────────────

/// Persist a vocabulary catalog to the `pyramid_vocabulary_catalog` table.
///
/// Uses INSERT OR REPLACE (UPSERT on the UNIQUE constraint) so re-extraction
/// updates existing entries without duplicating them.
pub fn persist_vocabulary_catalog(
    conn: &Connection,
    catalog: &VocabularyCatalog,
    source_node_id: Option<&str>,
) -> Result<usize> {
    let mut count = 0usize;
    let sql = "INSERT INTO pyramid_vocabulary_catalog
        (slug, entry_name, entry_type, category, importance, liveness, detail, source_node_id, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
        ON CONFLICT(slug, entry_name, entry_type) DO UPDATE SET
            category = excluded.category,
            importance = excluded.importance,
            liveness = excluded.liveness,
            detail = excluded.detail,
            source_node_id = excluded.source_node_id,
            updated_at = datetime('now')";

    let mut stmt = conn.prepare(sql)?;

    let persist_entries = |entries: &[VocabEntry],
                           entry_type: &str,
                           stmt: &mut rusqlite::Statement,
                           count: &mut usize|
     -> Result<()> {
        for entry in entries {
            let detail_json = serde_json::to_string(&entry.detail).unwrap_or_default();
            stmt.execute(rusqlite::params![
                &catalog.slug,
                &entry.name,
                entry_type,
                &entry.category,
                &entry.importance,
                &entry.liveness,
                &detail_json,
                &source_node_id,
            ])?;
            *count += 1;
        }
        Ok(())
    };

    persist_entries(&catalog.topics, "topic", &mut stmt, &mut count)?;
    persist_entries(&catalog.entities, "entity", &mut stmt, &mut count)?;
    persist_entries(&catalog.decisions, "decision", &mut stmt, &mut count)?;
    persist_entries(&catalog.terms, "term", &mut stmt, &mut count)?;
    persist_entries(&catalog.practices, "practice", &mut stmt, &mut count)?;

    Ok(count)
}

/// Load the full vocabulary catalog from the persistence table for a given slug.
pub fn load_vocabulary_catalog(conn: &Connection, slug: &str) -> Result<VocabularyCatalog> {
    let mut topics = Vec::new();
    let mut entities = Vec::new();
    let mut decisions = Vec::new();
    let mut terms = Vec::new();
    let mut practices = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT entry_name, entry_type, category, importance, liveness, detail, updated_at
         FROM pyramid_vocabulary_catalog
         WHERE slug = ?1
         ORDER BY entry_type, importance DESC NULLS LAST, entry_name",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let entry_name: String = row.get(0)?;
        let entry_type: String = row.get(1)?;
        let category: Option<String> = row.get(2)?;
        let importance: Option<f64> = row.get(3)?;
        let liveness: String = row.get(4)?;
        let detail_json: Option<String> = row.get(5)?;
        let detail: serde_json::Value = detail_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Ok((
            entry_type,
            VocabEntry {
                name: entry_name,
                category,
                importance,
                liveness,
                detail,
            },
        ))
    })?;

    let mut total = 0usize;
    for row in rows {
        let (entry_type, entry) = row.context("Failed to read vocabulary catalog row")?;
        match entry_type.as_str() {
            "topic" => topics.push(entry),
            "entity" => entities.push(entry),
            "decision" => decisions.push(entry),
            "term" => terms.push(entry),
            "practice" => practices.push(entry),
            _ => {} // Unknown type — skip
        }
        total += 1;
    }

    let now = chrono::Utc::now().to_rfc3339();

    Ok(VocabularyCatalog {
        slug: slug.to_string(),
        topics,
        entities,
        decisions,
        terms,
        practices,
        total_entries: total,
        extracted_at: now,
    })
}

// ── Query Semantics (§5.3) ──────────────────────────────────────────────────

/// Recognition query: does this term match a known canonical identity?
///
/// Performs case-insensitive substring matching against entry names in the
/// vocabulary catalog. Returns all matching entries across all types.
pub fn vocab_recognition_query(
    conn: &Connection,
    slug: &str,
    term: &str,
) -> Result<Vec<VocabEntry>> {
    let pattern = format!("%{}%", term.to_lowercase());
    let mut stmt = conn.prepare(
        "SELECT entry_name, entry_type, category, importance, liveness, detail
         FROM pyramid_vocabulary_catalog
         WHERE slug = ?1 AND LOWER(entry_name) LIKE ?2
         ORDER BY importance DESC NULLS LAST, entry_name",
    )?;

    let results = stmt
        .query_map(rusqlite::params![slug, pattern], |row| {
            let detail_json: Option<String> = row.get(5)?;
            let detail: serde_json::Value = detail_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(VocabEntry {
                name: row.get(0)?,
                category: row.get(2)?,
                importance: row.get(3)?,
                liveness: row.get(4)?,
                detail,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to execute recognition query")?;

    Ok(results)
}

/// Drill query: given a category, find all identities that belong to it.
///
/// Matches against the `category` column (exact match, case-insensitive).
/// For entities, category is the role. For decisions, category is the stance.
pub fn vocab_drill_query(conn: &Connection, slug: &str, category: &str) -> Result<Vec<VocabEntry>> {
    let mut stmt = conn.prepare(
        "SELECT entry_name, entry_type, category, importance, liveness, detail
         FROM pyramid_vocabulary_catalog
         WHERE slug = ?1 AND LOWER(category) = LOWER(?2)
         ORDER BY importance DESC NULLS LAST, entry_name",
    )?;

    let results = stmt
        .query_map(rusqlite::params![slug, category], |row| {
            let detail_json: Option<String> = row.get(5)?;
            let detail: serde_json::Value = detail_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(VocabEntry {
                name: row.get(0)?,
                category: row.get(2)?,
                importance: row.get(3)?,
                liveness: row.get(4)?,
                detail,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to execute drill query")?;

    Ok(results)
}

/// Reverse query: given an identity name, find its category and neighbors.
///
/// Looks up the named identity, then finds all other entries in the same
/// category (same category + same slug). Returns the entry, its category,
/// and its neighbor entries.
pub fn vocab_reverse_query(
    conn: &Connection,
    slug: &str,
    identity: &str,
) -> Result<VocabReverseResult> {
    // Find the identity itself
    let entry_row = conn
        .prepare(
            "SELECT entry_name, entry_type, category, importance, liveness, detail
             FROM pyramid_vocabulary_catalog
             WHERE slug = ?1 AND LOWER(entry_name) = LOWER(?2)
             LIMIT 1",
        )?
        .query_row(rusqlite::params![slug, identity], |row| {
            let detail_json: Option<String> = row.get(5)?;
            let detail: serde_json::Value = detail_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(VocabEntry {
                name: row.get(0)?,
                category: row.get(2)?,
                importance: row.get(3)?,
                liveness: row.get(4)?,
                detail,
            })
        })
        .optional()
        .context("Failed to execute reverse query")?;

    let entry = entry_row.ok_or_else(|| {
        anyhow::anyhow!(
            "Identity '{}' not found in vocabulary for slug '{}'",
            identity,
            slug
        )
    })?;

    let category = entry.category.clone();

    // Find neighbors in the same category
    let neighbors = if let Some(ref cat) = category {
        let mut stmt = conn.prepare(
            "SELECT entry_name, entry_type, category, importance, liveness, detail
             FROM pyramid_vocabulary_catalog
             WHERE slug = ?1 AND LOWER(category) = LOWER(?2) AND LOWER(entry_name) != LOWER(?3)
             ORDER BY importance DESC NULLS LAST, entry_name",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug, cat, identity], |row| {
            let detail_json: Option<String> = row.get(5)?;
            let detail: serde_json::Value = detail_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(VocabEntry {
                name: row.get(0)?,
                category: row.get(2)?,
                importance: row.get(3)?,
                liveness: row.get(4)?,
                detail,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to query neighbors")?
    } else {
        Vec::new()
    };

    Ok(VocabReverseResult {
        entry,
        category,
        neighbors,
    })
}

/// Diff query: what's new or changed since a given ISO timestamp?
///
/// Returns entries whose `updated_at` is after the given timestamp.
pub fn vocab_diff_query(conn: &Connection, slug: &str, since: &str) -> Result<Vec<VocabEntry>> {
    let mut stmt = conn.prepare(
        "SELECT entry_name, entry_type, category, importance, liveness, detail
         FROM pyramid_vocabulary_catalog
         WHERE slug = ?1 AND updated_at > ?2
         ORDER BY updated_at DESC, entry_name",
    )?;

    let results = stmt
        .query_map(rusqlite::params![slug, since], |row| {
            let detail_json: Option<String> = row.get(5)?;
            let detail: serde_json::Value = detail_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(VocabEntry {
                name: row.get(0)?,
                category: row.get(2)?,
                importance: row.get(3)?,
                liveness: row.get(4)?,
                detail,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to execute diff query")?;

    Ok(results)
}

// ── Promotion Check ─────────────────────────────────────────────────────────

/// Check whether the vocabulary catalog should be promoted to its own pyramid.
///
/// Returns true when the catalog's total entries exceed the given threshold.
/// This is a chain-config decision point — the function just evaluates the
/// condition. The actual promotion (creating a new pyramid, populating it
/// with vocabulary entries as L0 nodes) is handled by chain configuration.
pub fn should_promote_vocabulary(catalog: &VocabularyCatalog, threshold: usize) -> bool {
    catalog.total_entries > threshold
}

// ── Refresh ─────────────────────────────────────────────────────────────────

/// Re-extract vocabulary from the current apex and persist it.
///
/// Returns the refreshed catalog and the number of entries persisted.
pub fn refresh_vocabulary(conn: &Connection, slug: &str) -> Result<(VocabularyCatalog, usize)> {
    let catalog = extract_vocabulary_catalog(conn, slug)?;

    // Get the apex node id for source tracking
    let apex_node_id = query::get_apex(conn, slug)?.map(|n| n.id);

    let count = persist_vocabulary_catalog(conn, &catalog, apex_node_id.as_deref())?;
    Ok((catalog, count))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;
    use crate::pyramid::types::*;
    use rusqlite::Connection;

    /// Create an in-memory test database with schema initialized, a slug,
    /// and an apex node populated with topics, entities, decisions, and terms.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        // Create a test slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES ('test-slug', 'code', '/tmp/test')",
            [],
        )
        .unwrap();

        // Build topic data with extra fields for importance and liveness
        let topics = vec![
            Topic {
                name: "Wire Protocol".to_string(),
                current: "Active development".to_string(),
                entities: vec!["Wire".to_string()],
                corrections: vec![],
                decisions: vec![],
                extra: {
                    let mut m = serde_json::Map::new();
                    m.insert("importance".to_string(), serde_json::json!(0.9));
                    m.insert("liveness".to_string(), serde_json::json!("live"));
                    m.insert("category".to_string(), serde_json::json!("protocols"));
                    m
                },
            },
            Topic {
                name: "Old Format".to_string(),
                current: "Deprecated".to_string(),
                entities: vec![],
                corrections: vec![],
                decisions: vec![],
                extra: {
                    let mut m = serde_json::Map::new();
                    m.insert("importance".to_string(), serde_json::json!(0.3));
                    m.insert("liveness".to_string(), serde_json::json!("mooted"));
                    m.insert("category".to_string(), serde_json::json!("formats"));
                    m
                },
            },
            Topic {
                name: "Daily Standups".to_string(),
                current: "Team practice".to_string(),
                entities: vec![],
                corrections: vec![],
                decisions: vec![],
                extra: {
                    let mut m = serde_json::Map::new();
                    m.insert("importance".to_string(), serde_json::json!(0.5));
                    m.insert("liveness".to_string(), serde_json::json!("live"));
                    m.insert("practice".to_string(), serde_json::json!(true));
                    m.insert("category".to_string(), serde_json::json!("processes"));
                    m
                },
            },
        ];
        let topics_json = serde_json::to_string(&topics).unwrap();

        let entities = vec![
            Entity {
                name: "Adam".to_string(),
                role: "operator".to_string(),
                importance: 0.95,
                liveness: "live".to_string(),
            },
            Entity {
                name: "Partner".to_string(),
                role: "agent".to_string(),
                importance: 0.9,
                liveness: "live".to_string(),
            },
        ];
        let entities_json = serde_json::to_string(&entities).unwrap();

        let decisions = vec![
            Decision {
                decided: "Use YAML for chain config".to_string(),
                why: "Iteration speed".to_string(),
                rejected: "JSON".to_string(),
                stance: "committed".to_string(),
                importance: 0.8,
                related: vec![],
            },
            Decision {
                decided: "Drop XML support".to_string(),
                why: "No users".to_string(),
                rejected: String::new(),
                stance: "ruled_out".to_string(),
                importance: 0.4,
                related: vec![],
            },
        ];
        let decisions_json = serde_json::to_string(&decisions).unwrap();

        let terms = vec![
            Term {
                term: "Pyramid".to_string(),
                definition: "Recursive memory artifact".to_string(),
            },
            Term {
                term: "Vine".to_string(),
                definition: "Composing pyramid over bedrocks".to_string(),
            },
        ];
        let terms_json = serde_json::to_string(&terms).unwrap();

        // Insert an apex node at depth 3
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, terms, dead_ends, self_prompt, children, parent_id, build_version, entities_json)
             VALUES ('apex-1', 'test-slug', 3, NULL, 'Test Apex', 'Test distilled', ?1, '[]', ?2, ?3, '[]', '', '[]', NULL, 1, ?4)",
            rusqlite::params![topics_json, decisions_json, terms_json, entities_json],
        )
        .unwrap();

        // Update slug stats
        conn.execute(
            "UPDATE pyramid_slugs SET node_count = 1, max_depth = 3 WHERE slug = 'test-slug'",
            [],
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_extract_vocabulary_from_apex() {
        let conn = setup_test_db();
        let catalog = extract_vocabulary_catalog(&conn, "test-slug").unwrap();

        assert_eq!(catalog.slug, "test-slug");
        assert_eq!(catalog.topics.len(), 3, "Expected 3 topics");
        assert_eq!(catalog.entities.len(), 2, "Expected 2 entities");
        assert_eq!(catalog.decisions.len(), 2, "Expected 2 decisions");
        assert_eq!(catalog.terms.len(), 2, "Expected 2 terms");
        assert_eq!(
            catalog.practices.len(),
            1,
            "Expected 1 practice (Daily Standups)"
        );
        assert_eq!(
            catalog.total_entries,
            3 + 2 + 2 + 2 + 1,
            "Total should be sum of all entry types"
        );

        // Verify topic details
        let wire_topic = catalog
            .topics
            .iter()
            .find(|t| t.name == "Wire Protocol")
            .unwrap();
        assert_eq!(wire_topic.importance, Some(0.9));
        assert_eq!(wire_topic.liveness, "live");
        assert_eq!(wire_topic.category.as_deref(), Some("protocols"));

        // Verify entity details
        let adam = catalog.entities.iter().find(|e| e.name == "Adam").unwrap();
        assert_eq!(adam.importance, Some(0.95));
        assert_eq!(adam.category.as_deref(), Some("operator"));

        // Verify decision liveness inference
        let yaml_decision = catalog
            .decisions
            .iter()
            .find(|d| d.name == "Use YAML for chain config")
            .unwrap();
        assert_eq!(yaml_decision.liveness, "live");

        let xml_decision = catalog
            .decisions
            .iter()
            .find(|d| d.name == "Drop XML support")
            .unwrap();
        assert_eq!(xml_decision.liveness, "mooted");
    }

    #[test]
    fn test_recognition_query_matches_known_term() {
        let conn = setup_test_db();

        // Persist first so we can query the table
        let catalog = extract_vocabulary_catalog(&conn, "test-slug").unwrap();
        persist_vocabulary_catalog(&conn, &catalog, Some("apex-1")).unwrap();

        // Exact match
        let results = vocab_recognition_query(&conn, "test-slug", "Wire Protocol").unwrap();
        assert!(
            !results.is_empty(),
            "Should find 'Wire Protocol' in vocabulary"
        );
        assert_eq!(results[0].name, "Wire Protocol");

        // Partial match (case-insensitive)
        let results = vocab_recognition_query(&conn, "test-slug", "wire").unwrap();
        assert!(!results.is_empty(), "Should find partial match for 'wire'");

        // No match
        let results = vocab_recognition_query(&conn, "test-slug", "nonexistent-term-xyz").unwrap();
        assert!(results.is_empty(), "Should not find nonexistent term");
    }

    #[test]
    fn test_diff_query_returns_entries_newer_than_timestamp() {
        let conn = setup_test_db();

        // Record a timestamp before insertion
        let before_ts = "2020-01-01T00:00:00";

        // Persist the catalog
        let catalog = extract_vocabulary_catalog(&conn, "test-slug").unwrap();
        persist_vocabulary_catalog(&conn, &catalog, Some("apex-1")).unwrap();

        // Diff since the old timestamp should return all entries
        let diff = vocab_diff_query(&conn, "test-slug", before_ts).unwrap();
        assert_eq!(
            diff.len(),
            catalog.total_entries,
            "All entries should be newer than 2020"
        );

        // Diff since a future timestamp should return nothing
        let future_ts = "2099-01-01T00:00:00";
        let diff_future = vocab_diff_query(&conn, "test-slug", future_ts).unwrap();
        assert!(
            diff_future.is_empty(),
            "No entries should be newer than 2099"
        );
    }

    #[test]
    fn test_promotion_check_threshold() {
        let conn = setup_test_db();
        let catalog = extract_vocabulary_catalog(&conn, "test-slug").unwrap();

        // total_entries = 10 (3 topics + 2 entities + 2 decisions + 2 terms + 1 practice)
        assert!(
            should_promote_vocabulary(&catalog, 5),
            "Should promote when threshold (5) < total entries (10)"
        );
        assert!(
            !should_promote_vocabulary(&catalog, 100),
            "Should NOT promote when threshold (100) > total entries (10)"
        );
        assert!(
            !should_promote_vocabulary(&catalog, 10),
            "Should NOT promote when threshold (10) == total entries (10) — must exceed, not equal"
        );
    }
}
