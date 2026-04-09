// pyramid/collapse.rs — Enhanced delta chain collapse (WS-COLLAPSE-EXTEND)
//
// Extends the basic recovery_collapse_delta_chain from recovery.rs with:
//   - Proper collapse preserving or pruning version history
//   - Bulk collapse for nodes above a version threshold
//   - Auto-collapse candidate detection (version count + idle time)
//   - Collapse logging to pyramid_collapse_log
//
// The collapsed version IS the current live content — it's already the latest.
// Collapse compacts the version history chain, not the content.

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use tracing::info;

use super::db;
use super::types::CollapseResult;

// ── Single-node collapse ────────────────────────────────────────────────────

/// Collapse the delta chain for a single node.
///
/// 1. Load all versions from pyramid_node_versions
/// 2. The collapsed version IS the current live content (already latest)
/// 3. If preserve_history=true: keep version records but mark as "collapsed"
/// 4. If preserve_history=false: delete version records (recovery mode)
/// 5. Reset current_version to 1
/// 6. Log the collapse to pyramid_collapse_log
pub fn collapse_node_delta_chain(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    preserve_history: bool,
) -> Result<CollapseResult> {
    // Verify the node exists
    let node = db::get_node(conn, slug, node_id)?
        .ok_or_else(|| anyhow!("Node '{}' not found in slug '{}'", node_id, slug))?;

    let old_version = node.current_version as i32;

    if old_version <= 1 {
        // Nothing to collapse — already at version 1 with no chain
        let version_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = ?1 AND node_id = ?2",
                rusqlite::params![slug, node_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if version_count == 0 {
            info!(
                slug = slug,
                node_id = node_id,
                "Collapse: node already at version 1 with no history, nothing to collapse"
            );
            return Ok(CollapseResult {
                node_id: node_id.to_string(),
                versions_before: 1,
                versions_after: 1,
                preserved: preserve_history,
            });
        }
    }

    // Count versions being collapsed
    let version_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = ?1 AND node_id = ?2",
            rusqlite::params![slug, node_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Total versions = version history entries + 1 (the live row)
    let versions_before = (version_count as i32) + 1;

    // Use a savepoint so this is atomic
    conn.execute_batch("SAVEPOINT collapse_delta_chain;")?;

    let result: Result<()> = (|| {
        if preserve_history {
            // Mark all version records as collapsed (add a supersession_reason marker)
            conn.execute(
                "UPDATE pyramid_node_versions
                 SET supersession_reason = COALESCE(supersession_reason, '') || ' [collapsed]'
                 WHERE slug = ?1 AND node_id = ?2
                   AND (supersession_reason IS NULL OR supersession_reason NOT LIKE '%[collapsed]%')",
                rusqlite::params![slug, node_id],
            )?;
        } else {
            // Delete all version history for this node
            conn.execute(
                "DELETE FROM pyramid_node_versions WHERE slug = ?1 AND node_id = ?2",
                rusqlite::params![slug, node_id],
            )?;
        }

        // Reset the live row's current_version to 1
        conn.execute(
            "UPDATE pyramid_nodes SET current_version = 1, build_version = 1
             WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id],
        )?;

        // Log the collapse
        conn.execute(
            "INSERT INTO pyramid_collapse_log (slug, node_id, versions_before, versions_after, preserved)
             VALUES (?1, ?2, ?3, 1, ?4)",
            rusqlite::params![slug, node_id, versions_before, preserve_history],
        )?;

        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE SAVEPOINT collapse_delta_chain;")?;
            info!(
                slug = slug,
                node_id = node_id,
                versions_before = versions_before,
                preserve_history = preserve_history,
                "Collapse: delta chain collapsed to version 1"
            );
            Ok(CollapseResult {
                node_id: node_id.to_string(),
                versions_before,
                versions_after: 1,
                preserved: preserve_history,
            })
        }
        Err(e) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO SAVEPOINT collapse_delta_chain; RELEASE SAVEPOINT collapse_delta_chain;",
            );
            Err(e)
        }
    }
}

// ── Bulk collapse ───────────────────────────────────────────────────────────

/// Collapse all nodes in a slug that have version count >= min_versions.
///
/// Finds eligible nodes by counting entries in pyramid_node_versions, then
/// collapses each one with preserve_history=true (bulk collapses are
/// non-destructive by default — use single-node collapse for recovery mode).
pub fn collapse_stale_delta_chains(
    conn: &Connection,
    slug: &str,
    min_versions: i32,
) -> Result<Vec<CollapseResult>> {
    // Find all nodes with version count >= threshold.
    // The version count in pyramid_node_versions is the number of *prior* versions;
    // the live row adds 1 more. We query nodes where the history count alone
    // reaches (min_versions - 1) so total versions >= min_versions.
    let threshold = (min_versions - 1).max(0) as i64;

    let mut stmt = conn.prepare(
        "SELECT node_id, COUNT(*) as ver_count
         FROM pyramid_node_versions
         WHERE slug = ?1
         GROUP BY node_id
         HAVING ver_count >= ?2",
    )?;

    let candidates: Vec<String> = stmt
        .query_map(rusqlite::params![slug, threshold], |row| {
            row.get::<_, String>(0)
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut results = Vec::new();

    for node_id in candidates {
        match collapse_node_delta_chain(conn, slug, &node_id, true) {
            Ok(result) => results.push(result),
            Err(e) => {
                // Log but continue — don't let one node's failure stop the batch
                tracing::warn!(
                    slug = slug,
                    node_id = %node_id,
                    error = %e,
                    "Collapse: failed to collapse node in bulk operation, skipping"
                );
            }
        }
    }

    info!(
        slug = slug,
        min_versions = min_versions,
        collapsed_count = results.len(),
        "Collapse: bulk collapse complete"
    );

    Ok(results)
}

// ── Auto-collapse scheduling ────────────────────────────────────────────────

/// Returns node_ids that should be auto-collapsed based on:
/// - Version count > configured threshold (default: 10)
/// - No recent writes (last version created > configured idle time, default: 1 hour)
///
/// Both thresholds are configurable via Tier3Config.collapse_threshold,
/// but the caller can also pass explicit values.
pub fn should_auto_collapse(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<String>> {
    // Use Tier3Config defaults for thresholds
    let version_threshold = super::Tier3Config::default().collapse_threshold;
    let idle_minutes = 60; // 1 hour default idle time

    should_auto_collapse_with_config(conn, slug, version_threshold, idle_minutes)
}

/// Configurable version of should_auto_collapse for testing and explicit control.
pub fn should_auto_collapse_with_config(
    conn: &Connection,
    slug: &str,
    version_threshold: i64,
    idle_minutes: i64,
) -> Result<Vec<String>> {
    // History count threshold: we want total versions >= version_threshold,
    // and history count = total - 1 (the live row isn't in the history table).
    let history_threshold = (version_threshold - 1).max(0);

    // Find nodes with:
    // 1. version history count >= history_threshold
    // 2. most recent version entry created more than idle_minutes ago
    let mut stmt = conn.prepare(
        "SELECT v.node_id
         FROM pyramid_node_versions v
         WHERE v.slug = ?1
         GROUP BY v.node_id
         HAVING COUNT(*) >= ?2
            AND MAX(v.created_at) <= datetime('now', ?3)",
    )?;

    let idle_offset = format!("-{} minutes", idle_minutes);

    let candidates: Vec<String> = stmt
        .query_map(rusqlite::params![slug, history_threshold, idle_offset], |row| {
            row.get::<_, String>(0)
        })?
        .filter_map(|r| r.ok())
        .collect();

    info!(
        slug = slug,
        version_threshold = version_threshold,
        idle_minutes = idle_minutes,
        candidate_count = candidates.len(),
        "Collapse: auto-collapse candidate scan complete"
    );

    Ok(candidates)
}

// ── Collapse log queries ────────────────────────────────────────────────────

/// Query the collapse log for a slug. Returns recent collapse events.
pub fn get_collapse_log(conn: &Connection, slug: &str, limit: i64) -> Result<Vec<CollapseLogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, node_id, versions_before, versions_after, preserved, collapsed_at
         FROM pyramid_collapse_log
         WHERE slug = ?1
         ORDER BY collapsed_at DESC
         LIMIT ?2",
    )?;

    let entries = stmt
        .query_map(rusqlite::params![slug, limit], |row| {
            Ok(CollapseLogEntry {
                id: row.get(0)?,
                slug: row.get(1)?,
                node_id: row.get(2)?,
                versions_before: row.get(3)?,
                versions_after: row.get(4)?,
                preserved: row.get(5)?,
                collapsed_at: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(entries)
}

/// A row from the pyramid_collapse_log table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CollapseLogEntry {
    pub id: i64,
    pub slug: String,
    pub node_id: String,
    pub versions_before: i32,
    pub versions_after: i32,
    pub preserved: bool,
    pub collapsed_at: String,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    /// Helper: create an in-memory DB and init schema.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        conn
    }

    /// Helper: create a slug with the given content type.
    fn create_slug(conn: &Connection, slug: &str, content_type: &str) {
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, ?2, '')",
            rusqlite::params![slug, content_type],
        )
        .unwrap();
    }

    /// Helper: insert a node with a specific version.
    fn insert_node(conn: &Connection, slug: &str, node_id: &str, depth: i64, version: i64) {
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, current_version, build_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                node_id,
                slug,
                depth,
                format!("Node {}", node_id),
                "test content",
                version,
            ],
        )
        .unwrap();
    }

    /// Helper: insert a version history row.
    fn insert_version(conn: &Connection, slug: &str, node_id: &str, version: i64) {
        conn.execute(
            "INSERT INTO pyramid_node_versions (slug, node_id, version, headline, distilled, supersession_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'delta')",
            rusqlite::params![
                slug,
                node_id,
                version,
                format!("v{} headline", version),
                format!("v{} content", version),
            ],
        )
        .unwrap();
    }

    /// Helper: insert a version history row with a specific created_at time.
    fn insert_version_at(
        conn: &Connection,
        slug: &str,
        node_id: &str,
        version: i64,
        created_at: &str,
    ) {
        conn.execute(
            "INSERT INTO pyramid_node_versions (slug, node_id, version, headline, distilled, supersession_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'delta', ?6)",
            rusqlite::params![
                slug,
                node_id,
                version,
                format!("v{} headline", version),
                format!("v{} content", version),
                created_at,
            ],
        )
        .unwrap();
    }

    // ── Test 1: collapse node with 5 versions → versions_before=5, versions_after=1 ──

    #[test]
    fn test_collapse_node_with_versions() {
        let conn = test_db();
        create_slug(&conn, "test-collapse", "code");

        // Insert a node at version 5 (depth 2 so it's mutable)
        insert_node(&conn, "test-collapse", "n-1", 2, 5);

        // Insert 4 version history entries (versions 1-4; live row is version 5)
        for v in 1..=4 {
            insert_version(&conn, "test-collapse", "n-1", v);
        }

        // Collapse with preserve_history=false
        let result =
            collapse_node_delta_chain(&conn, "test-collapse", "n-1", false).unwrap();

        assert_eq!(result.versions_before, 5, "Should report 5 total versions before collapse");
        assert_eq!(result.versions_after, 1, "Should report 1 version after collapse");
        assert!(!result.preserved, "Should not preserve history");

        // Verify: all version rows deleted
        let version_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = 'test-collapse' AND node_id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version_count, 0, "All version history should be deleted");

        // Verify: live row is at version 1
        let live_version: i64 = conn
            .query_row(
                "SELECT current_version FROM pyramid_nodes WHERE slug = 'test-collapse' AND id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live_version, 1, "Live row should be at version 1");

        // Verify: collapse was logged
        let log_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_collapse_log WHERE slug = 'test-collapse' AND node_id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(log_count, 1, "Should have 1 collapse log entry");
    }

    // ── Test 2: bulk collapse only affects nodes above threshold ──

    #[test]
    fn test_bulk_collapse_threshold() {
        let conn = test_db();
        create_slug(&conn, "bulk-test", "code");

        // Node A: 6 total versions (5 in history + 1 live) — above threshold of 5
        insert_node(&conn, "bulk-test", "node-a", 2, 6);
        for v in 1..=5 {
            insert_version(&conn, "bulk-test", "node-a", v);
        }

        // Node B: 3 total versions (2 in history + 1 live) — below threshold of 5
        insert_node(&conn, "bulk-test", "node-b", 2, 3);
        for v in 1..=2 {
            insert_version(&conn, "bulk-test", "node-b", v);
        }

        // Node C: 5 total versions (4 in history + 1 live) — exactly at threshold of 5
        insert_node(&conn, "bulk-test", "node-c", 2, 5);
        for v in 1..=4 {
            insert_version(&conn, "bulk-test", "node-c", v);
        }

        // Bulk collapse with min_versions=5
        let results = collapse_stale_delta_chains(&conn, "bulk-test", 5).unwrap();

        // Should collapse node-a (6 versions) and node-c (5 versions), skip node-b (3)
        assert_eq!(results.len(), 2, "Should collapse 2 nodes");

        let collapsed_ids: Vec<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
        assert!(collapsed_ids.contains(&"node-a"), "Should collapse node-a");
        assert!(collapsed_ids.contains(&"node-c"), "Should collapse node-c");

        // Node B should be untouched
        let b_version: i64 = conn
            .query_row(
                "SELECT current_version FROM pyramid_nodes WHERE slug = 'bulk-test' AND id = 'node-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_version, 3, "Node B should still be at version 3");
    }

    // ── Test 3: should_auto_collapse returns nodes with many versions ──

    #[test]
    fn test_should_auto_collapse() {
        let conn = test_db();
        create_slug(&conn, "auto-test", "code");

        // Node with 12 total versions, old timestamps (more than 1 hour ago)
        insert_node(&conn, "auto-test", "old-node", 2, 12);
        for v in 1..=11 {
            insert_version_at(
                &conn,
                "auto-test",
                "old-node",
                v,
                "2025-01-01 00:00:00",
            );
        }

        // Node with 12 total versions, recent timestamps (should NOT be returned)
        insert_node(&conn, "auto-test", "recent-node", 2, 12);
        for v in 1..=11 {
            // Use a far-future timestamp to guarantee "recent"
            insert_version_at(
                &conn,
                "auto-test",
                "recent-node",
                v,
                "2099-12-31 23:59:59",
            );
        }

        // Node with only 2 total versions (below threshold)
        insert_node(&conn, "auto-test", "small-node", 2, 2);
        insert_version_at(
            &conn,
            "auto-test",
            "small-node",
            1,
            "2025-01-01 00:00:00",
        );

        // Use explicit config: threshold=10, idle=60 minutes
        let candidates =
            should_auto_collapse_with_config(&conn, "auto-test", 10, 60).unwrap();

        assert!(
            candidates.contains(&"old-node".to_string()),
            "Should include old-node (12 versions, old timestamps)"
        );
        assert!(
            !candidates.contains(&"recent-node".to_string()),
            "Should NOT include recent-node (recent timestamps)"
        );
        assert!(
            !candidates.contains(&"small-node".to_string()),
            "Should NOT include small-node (only 2 versions)"
        );
    }

    // ── Test 4: collapse log records the operation ──

    #[test]
    fn test_collapse_log_records() {
        let conn = test_db();
        create_slug(&conn, "log-test", "code");

        // Create a node with history
        insert_node(&conn, "log-test", "logged-node", 2, 4);
        for v in 1..=3 {
            insert_version(&conn, "log-test", "logged-node", v);
        }

        // Collapse it
        collapse_node_delta_chain(&conn, "log-test", "logged-node", true).unwrap();

        // Query the log
        let log = get_collapse_log(&conn, "log-test", 10).unwrap();
        assert_eq!(log.len(), 1, "Should have 1 log entry");

        let entry = &log[0];
        assert_eq!(entry.slug, "log-test");
        assert_eq!(entry.node_id, "logged-node");
        assert_eq!(entry.versions_before, 4, "4 total versions (3 history + 1 live)");
        assert_eq!(entry.versions_after, 1);
        assert!(entry.preserved, "Should be preserved");
        assert!(!entry.collapsed_at.is_empty(), "Should have a timestamp");
    }
}
