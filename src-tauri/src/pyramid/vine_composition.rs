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

        // Canonical write: observation event (old WAL INSERT removed)
        let _ = super::observation_events::write_observation_event(
            conn,
            vine_slug,
            "vine",
            "vine_stale",
            None,
            None,
            None,
            None,
            Some(&vine_node_id),
            Some(depth),
            Some(&detail),
        );

        inserted += 1;
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

    #[test]
    fn test_phase16_enqueue_mutations_scopes_to_vine_and_kept_evidence() {
        // Validates `enqueue_vine_manifest_mutations` — the per-level
        // synchronous DB work that `notify_vine_of_child_completion` runs
        // at each parent vine it notifies. This covers the second half of
        // the walk's per-hop behavior (update_child_apex is exercised by
        // test_update_child_apex_works_for_vine_children in db.rs).
        //
        // Setup: a vine `parent-v` composes a bedrock `leaf-b`. The vine
        // has two vine-layer nodes. One is backed by KEEP evidence
        // pointing at the bedrock apex and must be enqueued. The other is
        // backed by a DISCONNECT verdict and must be skipped — the
        // enqueue helper filters on `verdict = 'KEEP'` so only live
        // cross-slug links trigger pending mutations.
        let conn = test_db();

        // Create the parent vine slug and register the composition so the
        // downstream helpers can resolve evidence rows under the vine's
        // scope.
        db::create_slug(
            &conn,
            "parent-v",
            &crate::pyramid::types::ContentType::Vine,
            "",
        )
        .unwrap();
        db::insert_vine_composition(&conn, "parent-v", "leaf-b", 0, "bedrock").unwrap();

        // Save two vine nodes at depth=2. Depth must be >1 because
        // save_node enforces bedrock immutability at depth <= 1 when the
        // row already exists; fresh rows at any depth are fine, but
        // picking depth=2 keeps this test robust if the enqueue helper
        // ever revisits the row.
        let keep_node = crate::pyramid::types::PyramidNode {
            id: "v-node-keep".to_string(),
            slug: "parent-v".to_string(),
            depth: 2,
            chunk_index: None,
            headline: "keep".to_string(),
            distilled: "keeps bedrock apex".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        let disconnect_node = crate::pyramid::types::PyramidNode {
            id: "v-node-disconnect".to_string(),
            slug: "parent-v".to_string(),
            depth: 2,
            chunk_index: None,
            headline: "disconnect".to_string(),
            distilled: "disconnected from bedrock apex".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        db::save_node(&conn, &keep_node, None).unwrap();
        db::save_node(&conn, &disconnect_node, None).unwrap();

        // Insert KEEP and DISCONNECT evidence rows that both reference the
        // bedrock apex. Only the KEEP row should trigger an enqueue —
        // enqueue_vine_manifest_mutations filters on `verdict = 'KEEP'`.
        conn.execute(
            "INSERT INTO pyramid_evidence
                (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
             VALUES
                (?1, ?2, ?3, ?4, 'KEEP', 1.0, 'keeps bedrock apex')",
            rusqlite::params!["parent-v", "b1", "apex-abc", "v-node-keep"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_evidence
                (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
             VALUES
                (?1, ?2, ?3, ?4, 'DISCONNECT', 1.0, 'disconnected bedrock apex')",
            rusqlite::params!["parent-v", "b1", "apex-abc", "v-node-disconnect"],
        )
        .unwrap();

        // Run the enqueue helper as the async walk would at each level.
        let enqueued = enqueue_vine_manifest_mutations(
            &conn,
            "parent-v",
            "leaf-b",
            "apex-abc",
            "bedrock-build-1",
        )
        .unwrap();

        // Exactly one pending mutation should land: the KEEP row's target.
        assert_eq!(enqueued, 1, "should enqueue one mutation for the KEEP row only");

        // Verify the observation event row is scoped to the parent vine and
        // to the KEEP target node at the node's depth.
        // (Replaces old pyramid_pending_mutations query — table dropped.)
        let mut stmt = conn
            .prepare(
                "SELECT slug, layer, event_type, target_node_id
                 FROM dadbear_observation_events
                 WHERE slug = ?1",
            )
            .unwrap();
        let rows: Vec<(String, i64, String, String)> = stmt
            .query_map(rusqlite::params!["parent-v"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, Option<String>>(3)?.unwrap_or_default()))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "parent-v");
        assert_eq!(rows[0].1, 2, "observation event should land at the vine node's layer");
        assert_eq!(rows[0].2, "vine_stale");
        assert_eq!(rows[0].3, "v-node-keep");

        // Running the enqueue a second time is additive — the stale engine
        // de-duplicates on its own when it processes the queue. We only
        // verify we didn't accidentally target the DISCONNECT row after
        // re-entry.
        let enqueued_again = enqueue_vine_manifest_mutations(
            &conn,
            "parent-v",
            "leaf-b",
            "apex-abc",
            "bedrock-build-2",
        )
        .unwrap();
        assert_eq!(enqueued_again, 1);

        // Confirm the DISCONNECT target still hasn't been touched.
        // (Replaces old pyramid_pending_mutations query — table dropped.)
        let disconnect_present: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dadbear_observation_events
                               WHERE slug = 'parent-v' AND target_node_id = 'v-node-disconnect')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !disconnect_present,
            "DISCONNECT evidence must never be promoted to an observation event"
        );
    }

    // ── Wanderer fix: end-to-end propagation test ──────────────────────
    //
    // Phase 16 verifier caught that `notify_vine_of_child_completion` had
    // no production caller. Phase 16 wanderer pass wired it into
    // `build_runner::run_build_from_with_evidence_mode` post-build hook.
    //
    // These tests exercise the async function end-to-end with a real
    // PyramidState so the next regression can catch a wire cut without
    // having to actually run a full chain build.

    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// Build a minimal PyramidState with an in-memory-ish on-disk DB
    /// suitable for exercising `notify_vine_of_child_completion`. The
    /// only subsystems that need to work are the reader/writer sqlite
    /// connections, the build event bus, and the lock manager.
    fn make_propagation_test_state() -> (Arc<PyramidState>, tempfile::TempDir) {
        use crate::pyramid::event_bus::BuildEventBus;

        let dir = tempfile::TempDir::new().unwrap();
        let data_dir = dir.path().to_path_buf();
        let db_path = data_dir.join("pyramid.db");
        let writer_conn = db::open_pyramid_db(&db_path).unwrap();
        let reader_conn = db::open_pyramid_connection(&db_path).unwrap();

        let llm_config = crate::pyramid::llm::LlmConfig::default();
        let credential_store = Arc::new(
            crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
        );

        let state = Arc::new(PyramidState {
            reader: Arc::new(TokioMutex::new(reader_conn)),
            writer: Arc::new(TokioMutex::new(writer_conn)),
            config: Arc::new(tokio::sync::RwLock::new(llm_config)),
            active_build: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            data_dir: Some(data_dir.clone()),
            stale_engines: Arc::new(TokioMutex::new(HashMap::new())),
            file_watchers: Arc::new(TokioMutex::new(HashMap::new())),
            vine_builds: Arc::new(TokioMutex::new(HashMap::new())),
            use_chain_engine: AtomicBool::new(false),
            use_ir_executor: AtomicBool::new(false),
            event_bus: Arc::new(crate::pyramid::event_chain::LocalEventBus::new()),
            operational: Arc::new(crate::pyramid::OperationalConfig::default()),
            chains_dir: data_dir.join("chains"),
            remote_query_rate_limiter: Arc::new(TokioMutex::new(HashMap::new())),
            absorption_gate: Arc::new(TokioMutex::new(
                crate::pyramid::AbsorptionGate::new(),
            )),
            build_event_bus: Arc::new(BuildEventBus::new()),
            supabase_url: None,
            supabase_anon_key: None,
            csrf_secret: [0u8; 32],
            dadbear_handle: Arc::new(TokioMutex::new(None)),
            dadbear_supervisor_handle: Arc::new(TokioMutex::new(None)),
            dadbear_in_flight: Arc::new(std::sync::Mutex::new(HashMap::new())),
            provider_registry: Arc::new(
                crate::pyramid::provider::ProviderRegistry::new(credential_store.clone()),
            ),
            credential_store,
            schema_registry: Arc::new(
                crate::pyramid::schema_registry::SchemaRegistry::new(),
            ),
            cross_pyramid_router: Arc::new(
                crate::pyramid::cross_pyramid_router::CrossPyramidEventRouter::new(),
            ),
            ollama_pull_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ollama_pull_in_progress: Arc::new(tokio::sync::Mutex::new(None)),
        });
        (state, dir)
    }

    /// Install a vine slug + a single apex node so the propagation walk
    /// has a live apex it can lift out of `pyramid_nodes` when it
    /// re-enters the function for the next hop. Async-safe: acquires
    /// the tokio writer lock the normal way.
    async fn install_vine_with_apex(
        state: &PyramidState,
        slug: &str,
        apex_id: &str,
        depth: i64,
    ) {
        use crate::pyramid::types::{ContentType, PyramidNode};

        let writer = state.writer.lock().await;
        db::create_slug(&writer, slug, &ContentType::Vine, "").unwrap();

        let node = PyramidNode {
            id: apex_id.to_string(),
            slug: slug.to_string(),
            depth,
            chunk_index: None,
            headline: format!("{slug} apex"),
            distilled: format!("apex of {slug}"),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        db::save_node(&writer, &node, None).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_phase16_notify_vine_of_child_completion_walks_two_levels() {
        // Setup:
        //   bedrock L0  →  v-mid  (vine child of v-top)  →  v-top
        //
        // Triggering notify on the bedrock's apex should walk one level to
        // v-mid and a second level to v-top. We verify by peeking at the
        // direct-parent return list AND at a second-hop probe. The walk's
        // recursion is a BFS; the bug Phase 16 verifier left behind was
        // that no production code called this at all, so an end-to-end
        // test anchors the wire.
        let (state, _guard) = make_propagation_test_state();

        // Register v-top + v-mid as built vines with their own apexes so
        // the walk can lift the parent apex on each hop.
        install_vine_with_apex(&state, "v-mid", "v-mid-apex", 2).await;
        install_vine_with_apex(&state, "v-top", "v-top-apex", 3).await;

        // Wire the composition: v-mid composes the bedrock, v-top
        // composes v-mid.
        {
            let conn = state.writer.lock().await;
            db::insert_vine_composition(&conn, "v-mid", "b-leaf", 0, "bedrock").unwrap();
            db::insert_vine_composition(&conn, "v-top", "v-mid", 0, "vine").unwrap();
        }

        // Subscribe to the build event bus so we can assert the walk
        // actually emitted DeltaLanded for each parent vine.
        let mut rx = state.build_event_bus.tx.subscribe();

        // Run the notify — this is the call the build completion hook
        // makes in production (wanderer fix in build_runner.rs).
        let notified = notify_vine_of_child_completion(
            &state,
            "b-leaf",
            "bedrock-build-1",
            "bedrock-apex",
        )
        .await
        .expect("notify_vine_of_child_completion should succeed");

        // Direct-parent return list: only v-mid (the direct parent of
        // b-leaf). v-top is a second-hop ancestor and is NOT included in
        // the return list — the function returns the first-hop vines
        // per the Phase 2 contract.
        assert_eq!(notified, vec!["v-mid".to_string()]);

        // Composition rows: v-mid's apex reference for b-leaf now
        // points at bedrock-apex. v-top's apex reference for v-mid
        // points at v-mid's own apex (v-mid-apex).
        let comps_mid = {
            let conn = state.reader.lock().await;
            db::list_vine_compositions(&conn, "v-mid").unwrap()
        };
        assert_eq!(comps_mid.len(), 1);
        assert_eq!(comps_mid[0].bedrock_slug, "b-leaf");
        assert_eq!(
            comps_mid[0].bedrock_apex_node_id.as_deref(),
            Some("bedrock-apex"),
            "v-mid should have b-leaf's apex set after the walk"
        );

        let comps_top = {
            let conn = state.reader.lock().await;
            db::list_vine_compositions(&conn, "v-top").unwrap()
        };
        assert_eq!(comps_top.len(), 1);
        assert_eq!(comps_top[0].bedrock_slug, "v-mid");
        assert_eq!(
            comps_top[0].bedrock_apex_node_id.as_deref(),
            Some("v-mid-apex"),
            "v-top should have v-mid's apex set after the walk — \
             this is the recursive hop the Phase 16 verifier left unverified"
        );

        // Drain the event bus and look for DeltaLanded events on both
        // parent vines.
        let mut delta_slugs: Vec<String> = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            if matches!(
                evt.kind,
                crate::pyramid::event_bus::TaggedKind::DeltaLanded { .. }
            ) {
                delta_slugs.push(evt.slug);
            }
        }
        assert!(
            delta_slugs.contains(&"v-mid".to_string()),
            "expected DeltaLanded for v-mid, got: {:?}",
            delta_slugs
        );
        assert!(
            delta_slugs.contains(&"v-top".to_string()),
            "expected DeltaLanded for v-top (second hop) — \
             got: {:?}. If v-top is missing the recursive walk did not \
             reach the grandparent, which is the specific Phase 16 failure \
             mode the wanderer fix is meant to catch.",
            delta_slugs
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_phase16_notify_vine_of_child_completion_handles_three_level_cycle() {
        // Wanderer stress test for Q5: the BFS walk must terminate on a
        // 3-node cycle v-a → v-b → v-c → v-a.
        let (state, _guard) = make_propagation_test_state();
        install_vine_with_apex(&state, "v-a", "a-apex", 2).await;
        install_vine_with_apex(&state, "v-b", "b-apex", 2).await;
        install_vine_with_apex(&state, "v-c", "c-apex", 2).await;

        {
            let conn = state.writer.lock().await;
            // v-b lists v-a as a child (so v-b is a parent of v-a)
            db::insert_vine_composition(&conn, "v-b", "v-a", 0, "vine").unwrap();
            // v-c lists v-b as a child (so v-c is a parent of v-b)
            db::insert_vine_composition(&conn, "v-c", "v-b", 0, "vine").unwrap();
            // v-a lists v-c as a child (so v-a is a parent of v-c) — cycle
            db::insert_vine_composition(&conn, "v-a", "v-c", 0, "vine").unwrap();
        }

        // Starting from v-a: the cycle guard should allow visiting v-b
        // and v-c (via the recursive walk) but refuse to re-enter v-a.
        let notified = notify_vine_of_child_completion(
            &state,
            "v-a",
            "build-1",
            "a-apex",
        )
        .await
        .expect("cycle walk must terminate, not hang");

        // Direct parents of v-a: only v-b.
        assert_eq!(notified, vec!["v-b".to_string()]);

        // All comp rows should be intact — the cycle guard prevents
        // duplicate writes during the walk but does not corrupt any
        // existing state.
        let conn = state.reader.lock().await;
        assert_eq!(db::list_vine_compositions(&conn, "v-a").unwrap().len(), 1);
        assert_eq!(db::list_vine_compositions(&conn, "v-b").unwrap().len(), 1);
        assert_eq!(db::list_vine_compositions(&conn, "v-c").unwrap().len(), 1);
    }
}
