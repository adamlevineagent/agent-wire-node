// pyramid/observation_events.rs — Write helper for dadbear_observation_events
//
// This is the canonical append-only observation stream consumed by the
// DADBEAR supervisor. The old WAL (pyramid_pending_mutations) has been
// decommissioned and its table dropped. All mutation sites now write
// exclusively to this observation events table.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;

/// Write a single observation event to `dadbear_observation_events`.
/// Returns the autoincrement row ID of the new event.
///
/// Parameters map to the table columns:
/// - `source`: "watcher" | "cascade" | "rescan" | "evidence" | "vine" | "annotation"
/// - `event_type`: "file_modified" | "file_created" | "file_deleted" | "file_renamed"
///                  | "cascade_stale" | "edge_stale" | "evidence_growth" | "vine_stale"
///                  | "targeted_stale" | "full_sweep"
///                  | "annotation_written" | "annotation_superseded"
/// - `source_path`: filesystem path for the observation source (NULL for internal events)
/// - `file_path`: filesystem path of the affected file (NULL for internal events)
/// - `content_hash`: SHA-256 of new content (NULL for deletes/internal)
/// - `previous_hash`: SHA-256 of old content (NULL for creates/internal)
/// - `target_node_id`: for cascade/internal events, the node being affected
/// - `layer`: for cascade events, the target layer
/// - `metadata_json`: rename candidate pair, cascade reason, etc.
pub fn write_observation_event(
    conn: &Connection,
    slug: &str,
    source: &str,
    event_type: &str,
    source_path: Option<&str>,
    file_path: Option<&str>,
    content_hash: Option<&str>,
    previous_hash: Option<&str>,
    target_node_id: Option<&str>,
    layer: Option<i64>,
    metadata_json: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO dadbear_observation_events
         (slug, source, event_type, source_path, file_path, content_hash, previous_hash,
          target_node_id, layer, detected_at, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            slug,
            source,
            event_type,
            source_path,
            file_path,
            content_hash,
            previous_hash,
            target_node_id,
            layer,
            now,
            metadata_json,
        ],
    )
    .with_context(|| {
        format!(
            "Failed to write observation event type='{}' source='{}' for slug='{}'",
            event_type, source, slug
        )
    })?;
    Ok(conn.last_insert_rowid())
}
