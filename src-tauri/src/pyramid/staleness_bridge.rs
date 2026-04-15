// pyramid/staleness_bridge.rs — Bridge between DADBEAR stale engine and crystallization staleness
//
// DADBEAR detects file changes via watcher.rs → write_mutation → stale engine.
// This module bridges those results into the crystallization staleness pipeline
// (staleness.rs → detect_source_changes → propagate_staleness → queue).
//
// The route handler (`POST /pyramid/:slug/check-staleness`) can be called:
//   1. Manually (e.g., from the frontend or CLI) with explicit changed files
//   2. With no body to auto-detect from pending mutations in pyramid_pending_mutations
//   3. Eventually from DADBEAR's output (not wired yet — just the route)

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::staleness::{self, ChangeType, ChangedFile, StalenessReport};
use super::types::StalenessItem;

// ── Request / Response Types ─────────────────────────────────────────────────

/// A single file change entry in the request body.
#[derive(Debug, Clone, Deserialize)]
pub struct FileChangeEntry {
    pub path: String,
    pub change_type: String, // "addition", "modification", "deletion"
}

/// Request body for `POST /pyramid/:slug/check-staleness`.
/// If `files` is None or empty, auto-detect from pending mutations.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CheckStalenessRequest {
    #[serde(default)]
    pub files: Option<Vec<FileChangeEntry>>,
    /// Override the default staleness threshold (0.3).
    #[serde(default)]
    pub threshold: Option<f64>,
}

/// Full staleness check response.
#[derive(Debug, Clone, Serialize)]
pub struct CheckStalenessResponse {
    /// How the changed files were determined.
    pub source: String,
    /// Number of changed files processed.
    pub files_processed: usize,
    /// The staleness propagation report.
    pub report: StalenessReport,
    /// Items dequeued for re-answering (capped by staleness_queue_dequeue_cap config).
    pub queued_items: Vec<StalenessItem>,
}

// ── Core Bridge Function ─────────────────────────────────────────────────────

/// Run the full staleness check pipeline:
///   1. detect_source_changes — save deltas, get pending set
///   2. propagate_staleness — trace evidence weights upward, enqueue above threshold
///   3. process_staleness_queue — dequeue items for re-answering
///
/// `changed_files` is the list of files that changed. If empty, this is a no-op
/// that returns an empty report (the caller should auto-detect before calling).
///
/// `dequeue_cap` limits how many items are dequeued for re-answering per call.
/// Read from `operational.tier2.staleness_queue_dequeue_cap` in config.
pub fn run_staleness_check(
    conn: &Connection,
    slug: &str,
    changed_files: &[ChangedFile],
    threshold: f64,
    dequeue_cap: usize,
) -> Result<(StalenessReport, Vec<StalenessItem>)> {
    if changed_files.is_empty() {
        info!(
            slug,
            "No changed files provided, returning empty staleness report"
        );
        return Ok((
            StalenessReport {
                affected_questions: vec![],
                max_depth_reached: 0,
                staleness_scores: Default::default(),
            },
            vec![],
        ));
    }

    // Step 1: Record source deltas and get all unprocessed deltas
    let deltas = staleness::detect_source_changes(conn, slug, changed_files)?;

    info!(
        slug,
        delta_count = deltas.len(),
        file_count = changed_files.len(),
        "Source changes detected, propagating staleness"
    );

    // Step 2: Propagate staleness through evidence graph
    let report = staleness::propagate_staleness(conn, slug, &deltas, threshold)?;

    // Step 3: Dequeue items for re-answering (capped by config)
    let queued_items = staleness::process_staleness_queue(conn, slug, dequeue_cap as u32)?;

    info!(
        slug,
        affected = report.affected_questions.len(),
        queued = queued_items.len(),
        "Staleness check complete"
    );

    Ok((report, queued_items))
}

// ── Auto-detect from Observation Events (with WAL fallback) ─────────────────

/// Ensure the `last_bridge_observation_id` column exists on `pyramid_build_metadata`.
/// Uses ALTER TABLE IF NOT EXISTS pattern (idempotent).
fn ensure_bridge_cursor_column(conn: &Connection) {
    // SQLite doesn't have ALTER TABLE ... ADD COLUMN IF NOT EXISTS, so we
    // check pragma table_info first.
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('pyramid_build_metadata') WHERE name = 'last_bridge_observation_id'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    if !has_column {
        let _ = conn.execute_batch(
            "ALTER TABLE pyramid_build_metadata ADD COLUMN last_bridge_observation_id INTEGER DEFAULT 0;"
        );
    }
}

/// Get the current bridge cursor for a slug.
fn get_bridge_cursor(conn: &Connection, slug: &str) -> i64 {
    conn.query_row(
        "SELECT COALESCE(last_bridge_observation_id, 0) FROM pyramid_build_metadata WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Advance the bridge cursor to the given observation event ID.
fn advance_bridge_cursor(conn: &Connection, slug: &str, new_cursor: i64) {
    let _ = conn.execute(
        "INSERT INTO pyramid_build_metadata (slug, last_bridge_observation_id, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(slug) DO UPDATE SET last_bridge_observation_id = ?2, updated_at = datetime('now')",
        rusqlite::params![slug, new_cursor],
    );
}

/// Read observation events from `dadbear_observation_events` using a cursor,
/// converting file-level events to `ChangedFile` entries for the staleness pipeline.
///
/// Read observation events from `dadbear_observation_events` using a cursor,
/// converting file-level events to `ChangedFile` entries for the staleness pipeline.
pub fn auto_detect_changed_files(conn: &Connection, slug: &str) -> Result<Vec<ChangedFile>> {
    // Ensure the cursor column exists (idempotent migration)
    ensure_bridge_cursor_column(conn);

    // Try the new observation events path first
    let cursor = get_bridge_cursor(conn, slug);

    let mut stmt = conn.prepare(
        "SELECT id, event_type, file_path FROM dadbear_observation_events
         WHERE slug = ?1 AND id > ?2
           AND event_type IN ('file_modified', 'file_created', 'file_deleted', 'file_renamed')
           AND file_path IS NOT NULL AND file_path != ''
         ORDER BY id ASC",
    )?;

    let rows: Vec<(i64, String, String)> = stmt
        .query_map(rusqlite::params![slug, cursor], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if !rows.is_empty() {
        // New path: read from observation events and advance cursor
        let max_id = rows.iter().map(|(id, _, _)| *id).max().unwrap_or(cursor);

        let mut seen = std::collections::HashSet::new();
        let mut changed_files = Vec::new();
        for (_id, event_type, file_path) in &rows {
            if !seen.insert((file_path.clone(), event_type.clone())) {
                continue;
            }
            let change_type = match event_type.as_str() {
                "file_created" => ChangeType::Addition,
                "file_deleted" => ChangeType::Deletion,
                "file_modified" => ChangeType::Modification,
                "file_renamed" => ChangeType::Modification,
                _ => ChangeType::Modification,
            };
            changed_files.push(ChangedFile {
                path: file_path.clone(),
                change_type,
            });
        }

        advance_bridge_cursor(conn, slug, max_id);

        info!(
            slug,
            count = changed_files.len(),
            cursor_from = cursor,
            cursor_to = max_id,
            "Auto-detected changed files from observation events (cursor advanced)"
        );

        return Ok(changed_files);
    }

    // No fallback — the old WAL (pyramid_pending_mutations) has been dropped.
    // If observation events are empty, return empty vec.
    Ok(Vec::new())
}

/// Convert request body entries to internal `ChangedFile` format.
pub fn entries_to_changed_files(entries: &[FileChangeEntry]) -> Vec<ChangedFile> {
    entries
        .iter()
        .map(|e| ChangedFile {
            path: e.path.clone(),
            change_type: ChangeType::from_str(&e.change_type),
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entries_to_changed_files() {
        let entries = vec![
            FileChangeEntry {
                path: "src/main.rs".to_string(),
                change_type: "modification".to_string(),
            },
            FileChangeEntry {
                path: "src/new.rs".to_string(),
                change_type: "addition".to_string(),
            },
            FileChangeEntry {
                path: "src/old.rs".to_string(),
                change_type: "deletion".to_string(),
            },
        ];

        let files = entries_to_changed_files(&entries);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].change_type, ChangeType::Modification);
        assert_eq!(files[1].change_type, ChangeType::Addition);
        assert_eq!(files[2].change_type, ChangeType::Deletion);
    }

    #[test]
    fn test_empty_changed_files_returns_empty_report() {
        // Use an in-memory DB (we won't hit it since changed_files is empty)
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let (report, items) = run_staleness_check(&conn, "test", &[], 0.3, 50).unwrap();
        assert!(report.affected_questions.is_empty());
        assert!(items.is_empty());
    }

    #[test]
    fn test_default_request_deserializes() {
        let json = r#"{}"#;
        let req: CheckStalenessRequest = serde_json::from_str(json).unwrap();
        assert!(req.files.is_none());
        assert!(req.threshold.is_none());
    }

    #[test]
    fn test_request_with_files_deserializes() {
        let json = r#"{"files": [{"path": "src/main.rs", "change_type": "modification"}], "threshold": 0.5}"#;
        let req: CheckStalenessRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.files.as_ref().unwrap().len(), 1);
        assert_eq!(req.threshold, Some(0.5));
    }
}
