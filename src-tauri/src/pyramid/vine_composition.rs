// pyramid/vine_composition.rs — Vine composition through the chain executor (WS-VINE-UNIFY)
//
// Unifies the vine composition mechanism so that vine pyramids compose bedrock
// pyramids through the chain executor rather than through legacy hardcoded paths.
// When a bedrock finishes building, its apex becomes the leftmost L0 of the vine,
// and a delta propagates upward.
//
// CRITICAL: This module does NOT modify vine.rs per Q2 constraint. All new
// composition dispatch goes through build_runner::run_build_from.

use anyhow::{anyhow, Result};
use std::sync::Arc;
use tracing::{info, warn};

use super::db;
use super::event_bus::{TaggedBuildEvent, TaggedKind};
use super::lock_manager::LockManager;
use super::query;
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

        // 3. Update the bedrock's apex reference in the composition table
        {
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
        }

        // 4. Emit a DeltaLanded event on the vine's build event bus so that
        //    downstream consumers (DADBEAR, primer cache invalidation) know the
        //    vine's L0 has changed. The actual chain-driven delta propagation
        //    through the vine's upper layers is dispatched by DADBEAR-EXTEND
        //    when it observes this event — this module's job is just the
        //    notification and apex-ref bookkeeping.
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
