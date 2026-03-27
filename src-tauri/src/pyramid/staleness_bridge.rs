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
use tracing::{info, warn};

use super::staleness::{
    self, ChangedFile, ChangeType, StalenessReport, DEFAULT_STALENESS_THRESHOLD,
};
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
    /// Items dequeued for re-answering (up to 50).
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
pub fn run_staleness_check(
    conn: &Connection,
    slug: &str,
    changed_files: &[ChangedFile],
    threshold: f64,
) -> Result<(StalenessReport, Vec<StalenessItem>)> {
    if changed_files.is_empty() {
        info!(slug, "No changed files provided, returning empty staleness report");
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

    // Step 3: Dequeue items for re-answering (cap at 50 for the response)
    let queued_items = staleness::process_staleness_queue(conn, slug, 50)?;

    info!(
        slug,
        affected = report.affected_questions.len(),
        queued = queued_items.len(),
        "Staleness check complete"
    );

    Ok((report, queued_items))
}

// ── Auto-detect from Pending Mutations ───────────────────────────────────────

/// Read pending mutations from `pyramid_pending_mutations` (DADBEAR's table)
/// and convert them to `ChangedFile` entries for the staleness pipeline.
///
/// This bridges DADBEAR's mutation format into the crystallization format.
pub fn auto_detect_changed_files(conn: &Connection, slug: &str) -> Result<Vec<ChangedFile>> {
    // pyramid_pending_mutations columns: id, slug, layer, mutation_type, target_ref, detail,
    // cascade_depth, detected_at, processed, batch_id.
    // target_ref holds the file path for file-level mutations. Only process unprocessed entries.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT target_ref, mutation_type
         FROM pyramid_pending_mutations
         WHERE slug = ?1 AND target_ref IS NOT NULL AND target_ref != '' AND processed = 0
         ORDER BY detected_at DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let file_path: String = row.get(0)?;
        let mutation_type: String = row.get(1)?;
        Ok((file_path, mutation_type))
    })?;

    let mut changed_files = Vec::new();
    for row in rows {
        let (file_path, mutation_type) = row?;
        let change_type = match mutation_type.as_str() {
            "added" | "add" => ChangeType::Addition,
            "deleted" | "delete" | "removed" | "remove" => ChangeType::Deletion,
            _ => ChangeType::Modification, // "modified", "changed", etc.
        };
        changed_files.push(ChangedFile {
            path: file_path,
            change_type,
        });
    }

    // Mark consumed mutations as processed to prevent re-processing
    if !changed_files.is_empty() {
        conn.execute(
            "UPDATE pyramid_pending_mutations SET processed = 1 WHERE slug = ?1 AND processed = 0",
            rusqlite::params![slug],
        )?;
    }

    info!(
        slug,
        count = changed_files.len(),
        "Auto-detected changed files from pending mutations (marked as processed)"
    );

    Ok(changed_files)
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
        let (report, items) = run_staleness_check(&conn, "test", &[], 0.3).unwrap();
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
