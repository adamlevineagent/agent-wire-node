// pyramid/vine_composition.rs — Vine composition through the chain executor (WS-VINE-UNIFY)
//
// Unifies the vine composition mechanism so that vine pyramids compose bedrock
// pyramids through the chain executor rather than through legacy hardcoded paths.
// When a bedrock finishes building, its apex becomes the leftmost L0 of the vine,
// and a delta propagates upward.
//
// Phase 2: adds vine-level change-manifest propagation. When a bedrock apex
// updates, the vine nodes that reference that apex via cross-slug evidence
// links are enqueued as `confirmed_stale` pending mutations. The stale
// engine picks these up and routes them through `execute_supersession`,
// which now generates a targeted change manifest via
// `stale_helpers_upper::generate_change_manifest` and applies it in place on
// the vine node — same id, bumped build_version. See
// `docs/specs/change-manifest-supersession.md` → "Vine-Level Manifests".
//
// CRITICAL: This module does NOT modify vine.rs per Q2 constraint. All new
// composition dispatch goes through build_runner::run_build_from.

use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn};

use super::db;
use super::event_bus::{TaggedBuildEvent, TaggedKind};
use super::lock_manager::LockManager;
use super::PyramidState;

/// Called when a bedrock build completes. Looks up which vines include this
/// bedrock, updates the apex reference, and fires a composition delta event
/// for each affected vine.
///
/// Callers: DADBEAR on build completion, or build_runner at the tail of a
/// successful build.
///
/// Uses `write_child_then_parent` from LockManager to acquire locks in
/// deadlock-free order (bedrock first, then vine).
///
/// Returns the list of vine slugs that were notified.
pub async fn notify_vine_of_bedrock_completion(
    state: &PyramidState,
    bedrock_slug: &str,
    bedrock_build_id: &str,
    apex_node_id: &str,
) -> Result<Vec<String>> {
    // 1. Look up which vines include this bedrock
    let vine_slugs = {
        let conn = state.reader.lock().await;
        db::get_vines_for_bedrock(&conn, bedrock_slug)?
    };

    if vine_slugs.is_empty() {
        return Ok(vec![]);
    }

    info!(
        bedrock = bedrock_slug,
        build_id = bedrock_build_id,
        apex = apex_node_id,
        vine_count = vine_slugs.len(),
        "Bedrock build complete, notifying {} vine(s)",
        vine_slugs.len()
    );

    let mut notified = Vec::new();

    for vine_slug in &vine_slugs {
        // 2. Acquire child-then-parent lock (bedrock → vine)
        let (_bedrock_guard, _vine_guard) = LockManager::global()
            .write_child_then_parent(bedrock_slug, vine_slug)
            .await;

        // 3. Update the bedrock's apex reference in the composition table.
        //    Also enqueue vine-level pending mutations in the same writer
        //    lock scope so the stale engine picks up the change and routes
        //    affected vine L1+ nodes through execute_supersession (which
        //    uses the change-manifest flow per Phase 2).
        let enqueue_result = {
            let conn = state.writer.lock().await;
            if let Err(e) =
                db::update_bedrock_apex(&conn, vine_slug, bedrock_slug, apex_node_id)
            {
                warn!(
                    vine = vine_slug,
                    bedrock = bedrock_slug,
                    error = %e,
                    "Failed to update bedrock apex in vine composition table"
                );
                continue;
            }

            enqueue_vine_manifest_mutations(
                &conn,
                vine_slug,
                bedrock_slug,
                apex_node_id,
                bedrock_build_id,
            )
        };

        match enqueue_result {
            Ok(count) if count > 0 => {
                info!(
                    vine = vine_slug,
                    bedrock = bedrock_slug,
                    apex = apex_node_id,
                    enqueued = count,
                    "Enqueued {} vine-level change-manifest mutations for stale engine",
                    count
                );
            }
            Ok(_) => {
                // No affected vine nodes — the vine may not yet have upper
                // layers built, or the evidence links haven't been wired
                // yet. Not an error.
            }
            Err(e) => {
                warn!(
                    vine = vine_slug,
                    bedrock = bedrock_slug,
                    error = %e,
                    "enqueue_vine_manifest_mutations failed (continuing with event emission)"
                );
            }
        }

        // 4. Emit a DeltaLanded event on the vine's build event bus so that
        //    downstream consumers (DADBEAR, primer cache invalidation) know the
        //    vine's L0 has changed. The actual chain-driven delta propagation
        //    through the vine's upper layers is driven by the stale engine
        //    processing the pending mutations enqueued above.
        let event = TaggedBuildEvent {
            slug: vine_slug.clone(),
            kind: TaggedKind::DeltaLanded {
                depth: 0,
                node_id: apex_node_id.to_string(),
            },
        };
        let _ = state.build_event_bus.tx.send(event);

        // Also emit SlopeChanged since the vine's L0 layer just changed
        let slope_event = TaggedBuildEvent {
            slug: vine_slug.clone(),
            kind: TaggedKind::SlopeChanged {
                affected_layers: vec![0],
            },
        };
        let _ = state.build_event_bus.tx.send(slope_event);

        info!(
            vine = vine_slug,
            bedrock = bedrock_slug,
            apex = apex_node_id,
            "Vine notified of bedrock completion"
        );

        notified.push(vine_slug.clone());
    }

    Ok(notified)
}

/// Enqueue `confirmed_stale` pending mutations on the vine's L1+ nodes that
/// reference the updated bedrock apex via cross-slug evidence links.
///
/// The stale engine processes these mutations by calling
/// `execute_supersession`, which now generates a change manifest via
/// `generate_change_manifest` and applies it in place on the affected vine
/// node (same id, bumped build_version). For vine-level manifests the
/// `children_swapped` entries use the `{bedrock_slug}:{node_id}` prefix
/// format so the manifest audit trail records which bedrock apex changed.
///
/// Returns the number of pending mutation rows enqueued.
fn enqueue_vine_manifest_mutations(
    conn: &rusqlite::Connection,
    vine_slug: &str,
    bedrock_slug: &str,
    apex_node_id: &str,
    bedrock_build_id: &str,
) -> Result<usize> {
    // Cross-slug handle paths in pyramid_evidence use the format
    // `{slug}/{depth}/{node_id}` (see db::parse_handle_path). Look for
    // evidence links in the vine slug whose source_node_id matches the
    // bedrock apex under any of its valid reference formats:
    //   1. bare apex_node_id (same-slug embedding)
    //   2. `{bedrock_slug}/{depth}/{apex_node_id}` handle path
    //   3. `{bedrock_slug}:{apex_node_id}` short form used in some callers
    //
    // We query for any KEEP evidence row in the vine slug that mentions the
    // apex id in any of those shapes.
    let patterns = [
        apex_node_id.to_string(),
        format!("{bedrock_slug}/%/{apex_node_id}"),
        format!("{bedrock_slug}:{apex_node_id}"),
    ];

    // Collect affected target_node_ids — these are the vine's L1+ nodes
    // that built on the bedrock apex. We then write one confirmed_stale row
    // per affected vine node, at the layer of that node.
    let mut affected_vine_nodes: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    let mut stmt = conn.prepare(
        "SELECT target_node_id FROM pyramid_evidence
         WHERE slug = ?1
           AND verdict = 'KEEP'
           AND (source_node_id = ?2 OR source_node_id LIKE ?3 OR source_node_id = ?4)",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![vine_slug, &patterns[0], &patterns[1], &patterns[2]],
        |row| row.get::<_, String>(0),
    )?;

    for row in rows {
        if let Ok(tgt) = row {
            affected_vine_nodes.insert(tgt);
        }
    }
    drop(stmt);

    if affected_vine_nodes.is_empty() {
        return Ok(0);
    }

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut inserted = 0usize;

    for vine_node_id in affected_vine_nodes {
        // Look up the depth of the vine node so the pending mutation lands
        // on the right stale-engine layer queue.
        let depth: i64 = conn
            .query_row(
                "SELECT depth FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![vine_slug, vine_node_id],
                |row| row.get(0),
            )
            .unwrap_or(1);

        let detail = format!(
            "vine-level change manifest required: bedrock {} apex updated to {} (build {})",
            bedrock_slug, apex_node_id, bedrock_build_id
        );

        let rows_changed = conn.execute(
            "INSERT INTO pyramid_pending_mutations
             (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
             VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, 0, ?5, 0)",
            rusqlite::params![vine_slug, depth, vine_node_id, detail, now],
        )?;
        inserted += rows_changed;
    }

    Ok(inserted)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    /// Helper: create an in-memory DB and init schema.
    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_add_bedrocks_to_vine_verify_ordering() {
        let conn = test_db();

        // Add three bedrocks at positions 0, 1, 2
        db::add_bedrock_to_vine(&conn, "project-vine", "session-a", 0).unwrap();
        db::add_bedrock_to_vine(&conn, "project-vine", "session-b", 1).unwrap();
        db::add_bedrock_to_vine(&conn, "project-vine", "session-c", 2).unwrap();

        let bedrocks = db::get_vine_bedrocks(&conn, "project-vine").unwrap();
        assert_eq!(bedrocks.len(), 3);
        assert_eq!(bedrocks[0].bedrock_slug, "session-a");
        assert_eq!(bedrocks[0].position, 0);
        assert_eq!(bedrocks[1].bedrock_slug, "session-b");
        assert_eq!(bedrocks[1].position, 1);
        assert_eq!(bedrocks[2].bedrock_slug, "session-c");
        assert_eq!(bedrocks[2].position, 2);

        // All should be active
        for b in &bedrocks {
            assert_eq!(b.status, "active");
        }
    }

    #[test]
    fn test_get_vines_for_bedrock() {
        let conn = test_db();

        // session-a is in two vines
        db::add_bedrock_to_vine(&conn, "vine-1", "session-a", 0).unwrap();
        db::add_bedrock_to_vine(&conn, "vine-2", "session-a", 0).unwrap();
        db::add_bedrock_to_vine(&conn, "vine-1", "session-b", 1).unwrap();

        let vines = db::get_vines_for_bedrock(&conn, "session-a").unwrap();
        assert_eq!(vines.len(), 2);
        assert!(vines.contains(&"vine-1".to_string()));
        assert!(vines.contains(&"vine-2".to_string()));

        // session-b is only in vine-1
        let vines_b = db::get_vines_for_bedrock(&conn, "session-b").unwrap();
        assert_eq!(vines_b.len(), 1);
        assert_eq!(vines_b[0], "vine-1");

        // session-c is in no vines
        let vines_c = db::get_vines_for_bedrock(&conn, "session-c").unwrap();
        assert!(vines_c.is_empty());
    }

    #[test]
    fn test_update_bedrock_apex() {
        let conn = test_db();

        db::add_bedrock_to_vine(&conn, "vine-1", "session-a", 0).unwrap();

        // Initially no apex
        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert!(bedrocks[0].bedrock_apex_node_id.is_none());

        // Update apex
        db::update_bedrock_apex(&conn, "vine-1", "session-a", "node-abc-123").unwrap();

        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert_eq!(
            bedrocks[0].bedrock_apex_node_id.as_deref(),
            Some("node-abc-123")
        );

        // Update apex again
        db::update_bedrock_apex(&conn, "vine-1", "session-a", "node-def-456").unwrap();
        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert_eq!(
            bedrocks[0].bedrock_apex_node_id.as_deref(),
            Some("node-def-456")
        );

        // Updating a non-existent composition should fail
        let result = db::update_bedrock_apex(&conn, "vine-1", "nonexistent", "node-xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_bedrock_and_reorder() {
        let conn = test_db();

        db::add_bedrock_to_vine(&conn, "vine-1", "session-a", 0).unwrap();
        db::add_bedrock_to_vine(&conn, "vine-1", "session-b", 1).unwrap();
        db::add_bedrock_to_vine(&conn, "vine-1", "session-c", 2).unwrap();

        // Remove the middle one
        db::remove_bedrock_from_vine(&conn, "vine-1", "session-b").unwrap();

        // Should only return 2 active bedrocks
        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert_eq!(bedrocks.len(), 2);

        // Positions are still 0 and 2 (gap)
        assert_eq!(bedrocks[0].position, 0);
        assert_eq!(bedrocks[1].position, 2);

        // Reorder to compact
        db::reorder_vine_bedrocks(&conn, "vine-1").unwrap();

        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert_eq!(bedrocks.len(), 2);
        assert_eq!(bedrocks[0].position, 0);
        assert_eq!(bedrocks[0].bedrock_slug, "session-a");
        assert_eq!(bedrocks[1].position, 1);
        assert_eq!(bedrocks[1].bedrock_slug, "session-c");

        // Removing non-existent should error
        let result = db::remove_bedrock_from_vine(&conn, "vine-1", "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_add_bedrock_reactivates_removed() {
        let conn = test_db();

        db::add_bedrock_to_vine(&conn, "vine-1", "session-a", 0).unwrap();
        db::remove_bedrock_from_vine(&conn, "vine-1", "session-a").unwrap();

        // Should not appear in active list
        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert!(bedrocks.is_empty());

        // Re-add should reactivate
        db::add_bedrock_to_vine(&conn, "vine-1", "session-a", 0).unwrap();
        let bedrocks = db::get_vine_bedrocks(&conn, "vine-1").unwrap();
        assert_eq!(bedrocks.len(), 1);
        assert_eq!(bedrocks[0].status, "active");
    }
}
