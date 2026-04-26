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
///             | "purpose"                        // v5: purpose supersession
///             | "dadbear"                        // v5: binding_unresolved from compiler
///             | "chain"                          // v5 Phase 5: emitted from a
///                                                //             role_bound chain step
///                                                //             (starter-cascade-*,
///                                                //             starter-meta-layer-oracle,
///                                                //             etc.) via the
///                                                //             emit_cascade_handler_invoked
///                                                //             mechanical primitive
///             | "vocabulary"                     // v5 Phase 6c-A: vocab entry publish /
///                                                //                 supersede (annotation
///                                                //                 types, node shapes,
///                                                //                 role names — stored as
///                                                //                 contribution subtype)
///             | "scheduler"                      // Phase 9b: pyramid_scheduler periodic
///                                                //           tick + volume-threshold
///                                                //           annotation hook
///             | "operator"                       // v5 Phase 9c-3-3: events emitted in
///                                                //                  response to an
///                                                //                  explicit operator
///                                                //                  HTTP action
///                                                //                  (debate_reopened)
/// - `event_type`: "file_modified" | "file_created" | "file_deleted" | "file_renamed"
///                  | "cascade_stale" | "edge_stale" | "evidence_growth" | "vine_stale"
///                  | "targeted_stale" | "full_sweep"
///                  | "annotation_written" | "annotation_superseded"
///                  | "annotation_reacted"            // v5: vote event
///                  | "debate_spawned"                // v5: silent->named
///                  | "debate_collapsed"              // v5: debate resolved
///                  | "debate_reopened"               // v5 Phase 9c-3-3: operator-driven
///                                                     //                  re-open of a
///                                                     //                  collapsed debate;
///                                                     //                  bypasses the
///                                                     //                  post-collapse
///                                                     //                  cooldown on the
///                                                     //                  next annotation
///                                                     //                  append
///                  | "gap_detected"                  // v5: gap surfaced
///                  | "gap_resolved"                  // v5: gap closed
///                  | "purpose_shifted"               // v5: purpose superseded
///                  | "meta_layer_crystallized"       // v5: meta-layer emerged
///                  | "binding_unresolved"            // v5: observability for RAISE
///                  | "cascade_handler_invoked"       // v5: chronicle cascade trace
///                  | "debate_steward_invoked"        // v5 Phase 7a: debate_steward chain fired
///                  | "meta_layer_oracle_invoked"     // v5 Phase 7b: meta_layer_oracle chain fired
///                  | "meta_layer_oracle_skipped"     // v5 Phase 7b: oracle decided no crystallization
///                  | "synthesizer_invoked"           // v5 Phase 7b: synthesizer chain fired
///                  | "gap_dispatcher_invoked"        // v5 Phase 7c: gap_dispatcher chain fired
///                  | "gap_dispatcher_skipped"        // v5 Phase 7c verifier: skip path
///                                                     //   (target already typed Debate/MetaLayer,
///                                                     //   or retrigger hit existing anchor → no_op).
///                                                     //   Emitted instead of tracing::warn so
///                                                     //   operators see it in the chronicle
///                                                     //   (feedback_loud_deferrals).
///                  | "judge_invoked"                  // v5 Phase 7d: starter-judge chain fired
///                  | "authorize_question_invoked"    // v5 Phase 7d: starter-authorize-question fired
///                  | "accretion_invoked"              // v5 Phase 7d: starter-accretion-handler fired
///                  | "accretion_written"              // v5 Phase 7d: accretion note persisted
///                  | "sweep_invoked"                  // v5 Phase 7d: starter-sweep chain fired
///                  | "sweep_stale_failed_counted"    // v5 Phase 7d: sweep count-only step
///                  | "sweep_vocab_reindexed"          // v5 Phase 7d: sweep vocab cache refresh
///                  | "vocabulary_published"          // v5 6c-A: vocab entry publish
///                  | "vocabulary_superseded"         // v5 6c-A: vocab entry supersede
///                  | "node_re_distilled"             // v5 Phase 8-2: supervisor re_distill arm
///                                                     //                 successfully updated a node's
///                                                     //                 distilled/headline/topics via
///                                                     //                 execute_supersession. Chronicle
///                                                     //                 breadcrumb for the original-bug
///                                                     //                 fix path:
///                                                     //                 annotation → cascade_handler →
///                                                     //                 queue_re_distill → supervisor →
///                                                     //                 execute_supersession → this.
///                  | "accretion_tick"                // Phase 9b-1: pyramid_scheduler periodic
///                                                     //             accretion tick — fans to
///                                                     //             accretion_handler role binding
///                  | "sweep_tick"                    // Phase 9b-1: pyramid_scheduler periodic
///                                                     //             sweep tick — fans to sweep
///                                                     //             role binding
///                  | "accretion_threshold_hit"       // Phase 9b-2: annotation hook volume-
///                                                     //             threshold immediate trigger
///                                                     //             (same role routing as
///                                                     //             accretion_tick; different
///                                                     //             step_name so dedup works)
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

/// v5 Phase 8-3: emit a `debate_collapsed` observation event.
///
/// Deliberately a standalone helper (not inline at a call site) so the
/// trigger logic can live wherever it makes sense — an annotation handler,
/// an operator HTTP route, or the canonical `starter-debate-collapse`
/// chain — without each caller rewriting the metadata shape.
///
/// `reason`: short machine-friendly token (e.g. `"last_position_abandoned"`,
/// `"operator_collapsed"`, `"steel_man_superseded"`).
/// `positions_remaining`: integer count at the moment of collapse (0 for
/// the abandonment case; >0 for an explicit operator collapse that leaves
/// a winner).
/// `collapsed_by`: human- or agent-identifier (author, operator id, chain id).
///
/// Phase 9c-1 update: the v5 feature is now canonical. The triggering path
/// is `debate_collapse` annotation type → `annotation_reacted` with
/// `handler_chain_id=starter-debate-collapse` → `finalize_debate_node`
/// mechanical → this emitter (writes the terminal debate state into the
/// chronicle + transitions the node from debate shape back to scaffolding).
/// Kept as a helper so operator-initiated direct-DB collapse flows stay
/// one-liners without the full chain round-trip.
pub fn emit_debate_collapsed(
    conn: &Connection,
    slug: &str,
    debate_node_id: &str,
    layer: Option<i64>,
    reason: &str,
    positions_remaining: usize,
    collapsed_by: &str,
) -> Result<i64> {
    let metadata = serde_json::json!({
        "debate_node_id": debate_node_id,
        "reason": reason,
        "positions_remaining": positions_remaining,
        "collapsed_by": collapsed_by,
    })
    .to_string();
    write_observation_event(
        conn,
        slug,
        "chain",
        "debate_collapsed",
        None, None, None, None,
        Some(debate_node_id),
        layer,
        Some(&metadata),
    )
}

/// v5 Phase 9c-3-3: emit a `debate_reopened` observation event.
///
/// An operator-driven re-open of a previously-collapsed debate. The
/// post-collapse append-race cooldown in
/// `append_annotation_to_debate_node` (Phase 9c-2-3) blocks the legitimate
/// re-open case because it can't tell the difference between a late-
/// arriving steel_man (race) and an operator's deliberate decision to
/// re-open. This event is the explicit re-open signal: on the next
/// steel_man / red_team annotation append, the cooldown check observes
/// that the most-recent `debate_reopened` event is newer than the most-
/// recent `debate_collapsed` event, and the append proceeds.
///
/// Pure observability otherwise — `map_event_to_primitive` maps this to
/// `log_only` and `role_for_event` returns None so the emitted event
/// does not itself kick off a chain.
///
/// `reason`: short human-readable explanation (e.g.
/// "new evidence surfaced", "collapse was premature").
/// `reopened_by`: operator / agent identifier.
/// `referenced_collapse_event_id`: id of the `debate_collapsed` event
/// this re-open targets, for chronicle traceability. Optional because
/// operator callers may not have the id to hand; the cooldown-bypass
/// check uses timestamp ordering regardless.
pub fn emit_debate_reopened(
    conn: &Connection,
    slug: &str,
    debate_node_id: &str,
    layer: Option<i64>,
    reason: &str,
    reopened_by: &str,
    referenced_collapse_event_id: Option<i64>,
) -> Result<i64> {
    let metadata = serde_json::json!({
        "debate_node_id": debate_node_id,
        "reason": reason,
        "reopened_by": reopened_by,
        "referenced_collapse_event_id": referenced_collapse_event_id,
    })
    .to_string();
    write_observation_event(
        conn,
        slug,
        "operator",
        "debate_reopened",
        None, None, None, None,
        Some(debate_node_id),
        layer,
        Some(&metadata),
    )
}
