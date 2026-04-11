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
/// Phase 16: this is now a thin alias for `notify_vine_of_child_completion`.
/// The recursive walk logic lives in the unified function so bedrock and
/// sub-vine completions share one propagation path. Existing Phase 2/13
/// callers continue to work unchanged.
///
/// Callers: DADBEAR on build completion, or build_runner at the tail of a
/// successful build.
///
/// Returns the list of vine slugs that were notified AT THE DIRECT PARENT
/// LEVEL. The function also recursively propagates to grandparent vines,
/// grand-grandparent vines, etc. — but the direct-parent list is returned
/// so existing callers that log how many vines were touched remain accurate.
pub async fn notify_vine_of_bedrock_completion(
    state: &PyramidState,
    bedrock_slug: &str,
    bedrock_build_id: &str,
    apex_node_id: &str,
) -> Result<Vec<String>> {
    notify_vine_of_child_completion(state, bedrock_slug, bedrock_build_id, apex_node_id)
        .await
}

/// Phase 16: called when any child (bedrock OR sub-vine) build completes.
/// Walks up the composition hierarchy: bedrock → parent vine → grandparent
/// vine → … until either no parents remain or the cycle guard / depth cap
/// is hit.
///
/// At each level the function:
/// 1. Looks up the direct parent vines via `get_vines_for_child`.
/// 2. For each parent, acquires the child-then-parent lock, updates the
///    child's apex reference in the composition table, and enqueues
///    change-manifest pending mutations for the stale engine.
/// 3. Emits `DeltaLanded` + `SlopeChanged` events on the parent vine.
/// 4. **Recurses upward**: for each notified parent, treats the parent vine
///    as a newly-updated child and notifies its own parents. The parent
///    vine's apex is its own apex (resolved from `pyramid_vine_compositions`
///    or `pyramid_nodes`). If a parent vine has no apex yet (e.g., it was
///    just registered and never built), the recursion skips that branch —
///    the parent will pick up the update on its own next build.
///
/// **Cycle guard**: a `HashSet<String>` of visited slugs prevents infinite
/// loops on cyclic vine-of-vine references. Max walk depth is bounded by
/// `MAX_VINE_PROPAGATION_DEPTH` (32) as a defensive safety net even with the
/// cycle guard in place.
///
/// **Fire-and-forget at each level**: SYNCHRONOUS DB writes (composition
/// table + pending mutation enqueues) happen inline so the stale engine has
/// the up-to-date state on its next tick. CHAIN-EXECUTOR rebuilds are
/// ASYNCHRONOUS — enqueuing pending mutations hands the work to the stale
/// engine, which dispatches the rebuild on its own schedule. This keeps the
/// DADBEAR tick loop from blocking on a full recursive rebuild chain.
///
/// Returns the list of vine slugs notified AT THE DIRECT PARENT LEVEL
/// (first hop only), matching the Phase 2 return contract.
pub async fn notify_vine_of_child_completion(
    state: &PyramidState,
    child_slug: &str,
    child_build_id: &str,
    apex_node_id: &str,
) -> Result<Vec<String>> {
    const MAX_VINE_PROPAGATION_DEPTH: usize = 32;

    // Visited set guards against cycles. Includes the starting child so a
    // vine that references itself at any distance cannot trigger re-entry.
    let mut visited: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    visited.insert(child_slug.to_string());

    // BFS frontier — (child_slug, child_apex_node_id, child_build_id).
    // child_build_id is informational: it feeds the change-manifest detail
    // string. For recursive levels we generate a synthetic build id because
    // the parent vine hasn't produced a "real" build yet — the stale engine
    // does that asynchronously.
    let mut frontier: std::collections::VecDeque<(String, String, String)> =
        std::collections::VecDeque::new();
    frontier.push_back((
        child_slug.to_string(),
        apex_node_id.to_string(),
        child_build_id.to_string(),
    ));

    let mut direct_parent_notifications: Vec<String> = Vec::new();
    let mut total_notified = 0usize;
    let mut depth = 0usize;

    while let Some((current_child, current_apex, current_build_id)) = frontier.pop_front() {
        if depth > MAX_VINE_PROPAGATION_DEPTH {
            warn!(
                child_slug,
                visited = visited.len(),
                "notify_vine_of_child_completion: hit max depth cap, stopping recursive walk"
            );
            break;
        }

        // Look up direct parent vines for this level's child.
        let vine_slugs = {
            let conn = state.reader.lock().await;
            db::get_vines_for_child(&conn, &current_child)?
        };

        if vine_slugs.is_empty() {
            // Nothing to propagate at this level.
            continue;
        }

        info!(
            child = %current_child,
            build_id = %current_build_id,
            apex = %current_apex,
            vine_count = vine_slugs.len(),
            depth,
            "vine propagation: notifying {} parent vine(s) at depth {}",
            vine_slugs.len(),
            depth
        );

        for vine_slug in &vine_slugs {
            // Cycle guard: skip already-visited slugs. This catches both
            // direct self-reference (vine V includes V as a child) and
            // transitive cycles (V1 → V2 → V1).
            if !visited.insert(vine_slug.clone()) {
                warn!(
                    vine = %vine_slug,
                    child = %current_child,
                    "vine propagation: cycle detected, skipping already-visited parent"
                );
                continue;
            }

            // Acquire child-then-parent lock to stay deadlock-free.
            let (_child_guard, _vine_guard) = LockManager::global()
                .write_child_then_parent(&current_child, vine_slug)
                .await;

            // Update child apex in the composition table and enqueue
            // change-manifest mutations in a single writer lock scope.
            let enqueue_result = {
                let conn = state.writer.lock().await;
                if let Err(e) =
                    db::update_child_apex(&conn, vine_slug, &current_child, &current_apex)
                {
                    warn!(
                        vine = %vine_slug,
                        child = %current_child,
                        error = %e,
                        "vine propagation: failed to update child apex"
                    );
                    continue;
                }

                enqueue_vine_manifest_mutations(
                    &conn,
                    vine_slug,
                    &current_child,
                    &current_apex,
                    &current_build_id,
                )
            };

            match enqueue_result {
                Ok(count) if count > 0 => {
                    info!(
                        vine = %vine_slug,
                        child = %current_child,
                        apex = %current_apex,
                        enqueued = count,
                        "Enqueued {} vine-level change-manifest mutations for stale engine",
                        count
                    );
                }
                Ok(_) => {
                    // No affected vine nodes. Not an error — the vine may
                    // not have upper layers yet, or its evidence links
                    // haven't been wired.
                }
                Err(e) => {
                    warn!(
                        vine = %vine_slug,
                        child = %current_child,
                        error = %e,
                        "enqueue_vine_manifest_mutations failed (continuing with event emission)"
                    );
                }
            }

            // Emit DeltaLanded + SlopeChanged for downstream consumers.
            let event = TaggedBuildEvent {
                slug: vine_slug.clone(),
                kind: TaggedKind::DeltaLanded {
                    depth: 0,
                    node_id: current_apex.clone(),
                },
            };
            let _ = state.build_event_bus.tx.send(event);

            let slope_event = TaggedBuildEvent {
                slug: vine_slug.clone(),
                kind: TaggedKind::SlopeChanged {
                    affected_layers: vec![0],
                },
            };
            let _ = state.build_event_bus.tx.send(slope_event);

            info!(
                vine = %vine_slug,
                child = %current_child,
                apex = %current_apex,
                depth,
                "vine propagation: parent vine notified"
            );

            total_notified += 1;
            if depth == 0 {
                direct_parent_notifications.push(vine_slug.clone());
            }

            // Queue the parent vine for further propagation upward. Use the
            // parent vine's own apex (if it has one) as the apex to pass
            // up. If the parent vine has never been built, its apex lookup
            // returns None and we skip recursive propagation for this
            // branch — the parent will pick up the update when its own
            // build finishes.
            let parent_apex: Option<String> = {
                let conn = state.reader.lock().await;
                // Prefer the highest-depth live node from the parent vine's
                // own nodes table. If nothing is there, recursion for this
                // branch is a no-op.
                match db::get_all_live_nodes(&conn, vine_slug) {
                    Ok(nodes) => nodes
                        .iter()
                        .max_by_key(|n| n.depth)
                        .map(|n| n.id.clone()),
                    Err(_) => None,
                }
            };

            if let Some(pa) = parent_apex {
                // Synthetic build_id so the manifest detail string carries a
                // trace of the propagation hop. The actual build id of the
                // parent vine is established by the async rebuild the stale
                // engine will eventually dispatch.
                let propagated_build_id = format!("vine-prop-{}-{}", vine_slug, child_build_id);
                frontier.push_back((vine_slug.clone(), pa, propagated_build_id));
            }
        }

        depth = depth.saturating_add(1);
    }

    info!(
        starting_child = child_slug,
        total_vines_notified = total_notified,
        direct_parents = direct_parent_notifications.len(),
        max_depth_reached = depth,
        "vine propagation: recursive walk complete"
    );

    Ok(direct_parent_notifications)
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

    // ── Phase 16: vine-of-vines composition graph tests ──────────────────

    #[test]
    fn test_phase16_multi_level_vine_graph_is_walkable() {
        // Validates the composition graph shape that
        // notify_vine_of_child_completion walks. Builds:
        //   v-top composes v-mid (as vine) + b-sibling (as bedrock)
        //   v-mid composes b-leaf (as bedrock)
        // and verifies get_parent_vines_recursive returns the full chain
        // starting from the leaf.
        let conn = test_db();

        db::insert_vine_composition(&conn, "v-mid", "b-leaf", 0, "bedrock").unwrap();
        db::insert_vine_composition(&conn, "v-top", "v-mid", 0, "vine").unwrap();
        db::insert_vine_composition(&conn, "v-top", "b-sibling", 1, "bedrock").unwrap();

        // b-leaf's ancestors are v-mid (direct) and v-top (grandparent).
        let ancestors = db::get_parent_vines_recursive(&conn, "b-leaf").unwrap();
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], "v-mid");
        assert_eq!(ancestors[1], "v-top");

        // b-sibling's ancestors are just v-top.
        let sibling_ancestors = db::get_parent_vines_recursive(&conn, "b-sibling").unwrap();
        assert_eq!(sibling_ancestors.len(), 1);
        assert_eq!(sibling_ancestors[0], "v-top");

        // v-mid is itself a child — v-top is its only ancestor.
        let mid_ancestors = db::get_parent_vines_recursive(&conn, "v-mid").unwrap();
        assert_eq!(mid_ancestors.len(), 1);
        assert_eq!(mid_ancestors[0], "v-top");
    }

    #[test]
    fn test_phase16_notification_skips_vines_with_no_apex() {
        // Validates the composition state that
        // notify_vine_of_child_completion checks before recursing upward.
        // A vine with no apex node in pyramid_nodes (never built) should
        // not cause the walk to explode; the recursive propagation
        // simply skips that branch.
        //
        // This test validates the invariants the async function relies on:
        //   1. A composition row can exist without the child ever having
        //      a stored apex in the composition table.
        //   2. get_all_live_nodes returns an empty vec for never-built
        //      slugs, which the async notify function uses as the
        //      "skip this branch" signal.
        let conn = test_db();

        db::insert_vine_composition(&conn, "v-empty-parent", "never-built-child", 0, "vine")
            .unwrap();

        // list_vine_compositions still returns the composition row.
        let comps = db::list_vine_compositions(&conn, "v-empty-parent").unwrap();
        assert_eq!(comps.len(), 1);
        assert!(comps[0].bedrock_apex_node_id.is_none());

        // get_all_live_nodes for a never-built slug returns empty.
        let nodes = db::get_all_live_nodes(&conn, "never-built-child").unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_phase16_cycle_guard_prevents_runaway_walk() {
        // Validates that the DB-layer cycle guard terminates on a cyclic
        // vine-of-vine graph — the same invariant
        // notify_vine_of_child_completion relies on in its visited set.
        let conn = test_db();

        // Indirect cycle: v-alpha → v-beta → v-gamma → v-alpha.
        db::insert_vine_composition(&conn, "v-beta", "v-alpha", 0, "vine").unwrap();
        db::insert_vine_composition(&conn, "v-gamma", "v-beta", 0, "vine").unwrap();
        db::insert_vine_composition(&conn, "v-alpha", "v-gamma", 0, "vine").unwrap();

        // The walk must terminate and return a bounded answer.
        let ancestors = db::get_parent_vines_recursive(&conn, "v-alpha").unwrap();
        // The walk discovers v-beta (direct parent of v-alpha), then
        // v-gamma (parent of v-beta), then tries v-alpha (parent of
        // v-gamma) which is already in visited — walk terminates.
        assert_eq!(ancestors.len(), 2);
        assert!(ancestors.contains(&"v-beta".to_string()));
        assert!(ancestors.contains(&"v-gamma".to_string()));
    }
}
