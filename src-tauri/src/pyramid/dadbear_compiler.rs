// pyramid/dadbear_compiler.rs — WS-F: DADBEAR Compiler
//
// The compiler reads observation events, applies staleness logic, and emits
// durable work items with dependency edges and semantic path IDs.
//
// This replaces the `drain_and_dispatch` pattern with a DAG-aware,
// epoch-versioned compilation pipeline. The compiler does NOT make LLM calls
// — it only creates work item records in 'compiled' state. Execution is
// Phase 5 (supervisor dispatch).
//
// Key design points:
//   - Observations are grouped by event_type and mapped to primitives
//   - Dedup check: skip if non-terminal work item already targets same (slug, target_id, step_name, layer)
//   - Semantic path IDs: {slug}:{epoch_short}:{primitive}:{layer}:{target_id}
//     where epoch_short is the recipe_short segment of the epoch_id
//   - Cross-layer deps: L1+ items depend on their L0 prerequisites being applied
//   - Epoch versioning: recipe/norms change → new epoch, old compiled/blocked items → stale
//   - Prompt materialization is deferred to Phase 5 dispatch; work items carry
//     placeholder prompts describing what the item represents

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ── Semantic path ID constructors ─────────────────────────────────────────────

/// Construct a work item ID.
/// Format: `{slug}:{epoch_short}:{primitive}:{layer}:{target_id}`
/// where epoch_short is the first 8 chars of the epoch_id.
///
/// Consumer parsing contract: `splitn(5, ':')` — field 5 is the complete
/// target_id which may contain internal `/` separators.
pub fn work_item_id(slug: &str, epoch_short: &str, primitive: &str, layer: i64, target_id: &str) -> String {
    format!("{slug}:{epoch_short}:{primitive}:{layer}:{target_id}")
}

/// Construct a batch ID.
/// Format: `{slug}:{epoch_short}:batch-{cursor_position}`
pub fn batch_id(slug: &str, epoch_short: &str, cursor_position: i64) -> String {
    format!("{slug}:{epoch_short}:batch-{cursor_position}")
}

/// Construct an attempt ID.
/// Format: `{work_item_id}:a{attempt_number}`
pub fn attempt_id(work_item_id: &str, attempt_number: i64) -> String {
    format!("{work_item_id}:a{attempt_number}")
}

/// Extract epoch_short from an epoch_id for use in semantic path IDs.
/// Epoch ID format: `{slug}:{recipe_short}:{norms_short}:{timestamp}`
/// We extract the recipe_short segment (first 8 chars of parts[1])
/// because the slug prefix is redundant in the work item ID.
fn epoch_short(epoch_id: &str) -> String {
    // Use the recipe_short segment (second colon-delimited part) as the
    // epoch discriminator. The full epoch_id is stored on the work item
    // row for exact uniqueness; this short form is for human readability.
    let parts: Vec<&str> = epoch_id.splitn(4, ':').collect();
    if parts.len() >= 2 {
        // Use recipe_short segment (second part after slug)
        parts[1].chars().take(8).collect()
    } else {
        epoch_id.chars().take(8).collect()
    }
}

// ── Observation event row ─────────────────────────────────────────────────────

/// A single observation event read from `dadbear_observation_events`.
#[derive(Debug, Clone)]
struct ObservationEvent {
    id: i64,
    slug: String,
    source: String,
    event_type: String,
    source_path: Option<String>,
    file_path: Option<String>,
    content_hash: Option<String>,
    previous_hash: Option<String>,
    target_node_id: Option<String>,
    layer: Option<i64>,
    detected_at: String,
    metadata_json: Option<String>,
}

// ── Compilation result ────────────────────────────────────────────────────────

/// Result of a single compilation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationResult {
    /// New cursor position (highest observation event ID processed).
    pub new_cursor: i64,
    /// Number of work items created in this pass.
    pub items_compiled: usize,
    /// Number of dependency edges created.
    pub deps_created: usize,
    /// Number of observations skipped due to dedup (non-terminal item already exists).
    pub deduped: usize,
}

// ── Primitive mapping ─────────────────────────────────────────────────────────

/// Map an observation event_type to a (primitive, step_name, model_tier) tuple.
///
/// This is the observation → primitive mapping from the plan:
///   file_created → extract
///   file_modified → stale_check
///   file_deleted → tombstone
///   file_renamed → rename_candidate
///   cascade_stale → stale_check at target layer
///   edge_stale → edge_check
///   targeted_stale → stale_check at L0
///   evidence_growth → stale_check at L0
///   vine_stale → stale_check for vine nodes
///   connection_check → connection_check
///   node_stale → node_stale_check
///   faq_category_stale → faq_redistill
///   full_sweep → extract for all source files
///   annotation_written → re_distill (ancestor re-summary triggered by new annotation)
///   annotation_superseded → re_distill (same primitive; supersession is a stronger signal
///     preserved in metadata_json but coalesces with additive writes so a single parent
///     re-distill runs even if multiple annotations fire within the dedup window)
pub(crate) fn map_event_to_primitive(event_type: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // Returns (primitive, step_name, model_tier)
    match event_type {
        "file_created" => Some(("extract", "l0_extract", "stale_remote")),
        "file_modified" => Some(("stale_check", "l0_stale_check", "stale_remote")),
        "file_deleted" => Some(("tombstone", "l0_tombstone", "stale_remote")),
        "file_renamed" => Some(("rename_candidate", "l0_rename_eval", "stale_remote")),
        "cascade_stale" => Some(("stale_check", "cascade_stale_check", "stale_remote")),
        "edge_stale" => Some(("edge_check", "edge_stale_check", "stale_remote")),
        "targeted_stale" => Some(("stale_check", "targeted_l0_stale_check", "stale_remote")),
        "evidence_growth" => Some(("stale_check", "evidence_stale_check", "stale_remote")),
        "vine_stale" => Some(("stale_check", "vine_stale_check", "stale_remote")),
        "connection_check" => Some(("connection_check", "connection_reeval", "stale_remote")),
        "node_stale" => Some(("node_stale_check", "node_stale_check", "stale_remote")),
        "faq_category_stale" => Some(("faq_redistill", "faq_redistill", "stale_remote")),
        "full_sweep" => Some(("extract", "full_sweep_extract", "stale_remote")),
        "annotation_written" | "annotation_superseded" => {
            // Phase 8-1 flip: route through the `cascade_handler` role. The
            // bound chain (starter-cascade-judge-gated for new slugs,
            // starter-cascade-immediate-redistill for legacy slugs) runs,
            // and its terminal mechanical `queue_re_distill_for_target`
            // enqueues a re_distill work item against the target ancestor.
            // The supervisor's Phase 8-2 re_distill arm (apply_mechanical)
            // then actually re-distills the node via execute_supersession,
            // updating pyramid_nodes.distilled/headline/topics/build_version.
            //
            // Pre-Phase-8 mapping was `re_distill` + `annotation_redistill`
            // which silently no-op'd in the supervisor default arm. That
            // was THE original DADBEAR non-firing bug.
            Some(("role_bound", "annotation_cascade", "stale_remote"))
        }
        // ── Post-build accretion v5 event types ─────────────────────────────
        // Per v5 R8: per-event-type step_names so dedup in has_active_work_item
        // (keyed on target_id + step_name + layer) doesn't collapse
        // semantically distinct events onto the same work_item row.
        "annotation_reacted" => Some(("role_bound", "cascade_reacted", "stale_remote")),
        "debate_spawned" => Some(("role_bound", "debate_spawn", "stale_remote")),
        "debate_collapsed" => Some(("role_bound", "debate_collapse", "stale_remote")),
        // v5 audit P3: gap_detected is observability-only. The actual
        // dispatch already fired via annotation_reacted → handler_chain_id
        // (6c-B flip), so compiling a second work item for gap_detected
        // was a wasted compile+dispatch+no_op cycle. Now log_only —
        // chronicle entry stays, no work item.
        "gap_detected" => Some(("log_only", "gap_detected_log", "stale_remote")),
        "gap_resolved" => Some(("role_bound", "oracle_gap_resolved", "stale_remote")),
        "purpose_shifted" => Some(("role_bound", "oracle_purpose_shift", "stale_remote")),
        "meta_layer_crystallized" => Some(("role_bound", "synthesize_meta_layer", "stale_remote")),
        // Chronicle-only events (log_only dispatches nothing; observability hook).
        "binding_unresolved" => Some(("log_only", "binding_unresolved_log", "stale_remote")),
        "cascade_handler_invoked" => Some(("log_only", "cascade_invoked_log", "stale_remote")),
        // v5 Phase 7a: emitted by emit_debate_steward_invoked at the head of
        // the debate_steward chain; pure observability, no downstream action.
        "debate_steward_invoked" => {
            Some(("log_only", "debate_steward_invoked_log", "stale_remote"))
        }
        // v5 Phase 7 wanderer fix: every Phase 7 starter chain emits a
        // `*_invoked` / `*_skipped` / `*_written` observation event for
        // chronicle visibility. Those events land in dadbear_observation_events
        // and are read back by the next compile tick. Without an explicit
        // mapping, the compiler's unknown-event loud-hold arm (feedback_loud_deferrals)
        // pins the cursor FOREVER on the first chain execution — stalling
        // all subsequent compilation for that slug. All of these are pure
        // observability and carry no downstream primitive; log_only keeps
        // the chronicle row without spawning a work item.
        "meta_layer_oracle_invoked" => {
            Some(("log_only", "oracle_invoked_log", "stale_remote"))
        }
        "meta_layer_oracle_skipped" => {
            Some(("log_only", "oracle_skipped_log", "stale_remote"))
        }
        "synthesizer_invoked" => {
            Some(("log_only", "synthesizer_invoked_log", "stale_remote"))
        }
        "gap_dispatcher_invoked" => {
            Some(("log_only", "gap_dispatcher_invoked_log", "stale_remote"))
        }
        "gap_dispatcher_skipped" => {
            Some(("log_only", "gap_dispatcher_skipped_log", "stale_remote"))
        }
        "judge_invoked" => {
            Some(("log_only", "judge_invoked_log", "stale_remote"))
        }
        "authorize_question_invoked" => {
            Some(("log_only", "authorize_invoked_log", "stale_remote"))
        }
        "accretion_invoked" => {
            Some(("log_only", "accretion_invoked_log", "stale_remote"))
        }
        "accretion_written" => {
            Some(("log_only", "accretion_written_log", "stale_remote"))
        }
        "sweep_invoked" => {
            Some(("log_only", "sweep_invoked_log", "stale_remote"))
        }
        "sweep_stale_failed_counted" => {
            Some(("log_only", "sweep_stale_counted_log", "stale_remote"))
        }
        "sweep_vocab_reindexed" => {
            Some(("log_only", "sweep_vocab_reindexed_log", "stale_remote"))
        }
        // v5 Phase 8-2: supervisor emits node_re_distilled after
        // execute_supersession successfully updates pyramid_nodes for a
        // re_distill work item. Pure observability — the mutation has
        // already happened on the node row; the event is the chronicle
        // breadcrumb that closes the annotation → cascade → re_distill
        // loop for operators. No downstream work.
        "node_re_distilled" => {
            Some(("log_only", "node_redistilled_log", "stale_remote"))
        }
        // Phase 9b-1: pyramid_scheduler tick events. Each is role_bound
        // so the slug's configured role binding (default: accretion_handler
        // → starter-accretion-handler, sweep → starter-sweep) dispatches.
        // Operators can swap the chain via role_binding supersession per
        // slug without touching the scheduler.
        "accretion_tick" => Some(("role_bound", "accretion_tick_dispatch", "stale_remote")),
        "sweep_tick" => Some(("role_bound", "sweep_tick_dispatch", "stale_remote")),
        // Phase 9b-2: annotation-volume threshold crossing. Immediate
        // dispatch path — same role as `accretion_tick` but with richer
        // metadata (annotation_id, count_since_cursor) so the handler
        // chain can key off the trigger if desired. Distinct step_name
        // so has_active_work_item dedup doesn't collapse a threshold-hit
        // onto a coincident tick-initiated row.
        "accretion_threshold_hit" => {
            Some(("role_bound", "accretion_threshold_dispatch", "stale_remote"))
        }
        unknown => {
            // Per v5 R5 loud-raise discipline: opt-in strict mode for
            // production. Default warn-and-skip preserves backward compat
            // for any pre-existing emitter outside the known vocabulary.
            if std::env::var("STRICT_UNKNOWN_EVENTS").is_ok() {
                panic!(
                    "map_event_to_primitive: unknown event_type '{}' — \
                     must be added to the map before emission. Unset \
                     STRICT_UNKNOWN_EVENTS to warn-skip instead.",
                    unknown
                );
            }
            tracing::warn!(
                event_type = %unknown,
                "map_event_to_primitive: unknown event_type — skipping. \
                 Set STRICT_UNKNOWN_EVENTS=1 to raise instead."
            );
            None
        }
    }
}

/// Derive the target_id for a work item from an observation event.
///
/// For file-based events, target_id is the file_path.
/// For cascade/internal events, target_id is the target_node_id.
/// For edge events, target_id uses the edge/{from}/{to} composite format.
/// For rename events, target_id uses rename/{old}/{new} composite format.
fn derive_target_id(event: &ObservationEvent) -> String {
    match event.event_type.as_str() {
        "file_created" | "file_modified" | "file_deleted" | "full_sweep" => {
            event.file_path.clone().unwrap_or_else(|| "unknown".to_string())
        }
        "file_renamed" => {
            // metadata_json should contain the rename pair info
            // Format: rename/{old_path}/{new_path}
            if let Some(ref meta) = event.metadata_json {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) {
                    let old = v.get("old_path")
                        .and_then(|v| v.as_str())
                        .or_else(|| v.get("source_path").and_then(|v| v.as_str()))
                        .unwrap_or("unknown");
                    let new = v.get("new_path")
                        .and_then(|v| v.as_str())
                        .or_else(|| v.get("file_path").and_then(|v| v.as_str()))
                        .unwrap_or("unknown");
                    return format!("rename/{old}/{new}");
                }
            }
            // Fallback: use file_path
            format!("rename/{}", event.file_path.clone().unwrap_or_else(|| "unknown".to_string()))
        }
        "edge_stale" => {
            // target_node_id for edge events encodes both endpoints
            if let Some(ref target) = event.target_node_id {
                if target.starts_with("edge/") {
                    target.clone()
                } else {
                    format!("edge/{target}")
                }
            } else {
                "edge/unknown".to_string()
            }
        }
        "connection_check" => {
            if let Some(ref target) = event.target_node_id {
                if target.starts_with("conn/") {
                    target.clone()
                } else {
                    format!("conn/{target}")
                }
            } else {
                "conn/unknown".to_string()
            }
        }
        // cascade_stale, targeted_stale, evidence_growth, vine_stale, node_stale, faq_category_stale
        _ => {
            event.target_node_id.clone().unwrap_or_else(|| {
                event.file_path.clone().unwrap_or_else(|| "unknown".to_string())
            })
        }
    }
}

/// Derive the layer for a work item from an observation event.
///
/// File-based events (created/modified/deleted/renamed) are always L0.
/// Cascade events use the event's layer field.
/// Other internal events default to L0 unless a layer is specified.
fn derive_layer(event: &ObservationEvent) -> i64 {
    match event.event_type.as_str() {
        "file_created" | "file_modified" | "file_deleted" | "file_renamed" | "full_sweep" => 0,
        "targeted_stale" | "evidence_growth" => 0,
        "cascade_stale" | "edge_stale" | "vine_stale" | "node_stale" | "faq_category_stale" | "connection_check" => {
            event.layer.unwrap_or(0)
        }
        "annotation_written" | "annotation_superseded" => event.layer.unwrap_or(0),
        _ => event.layer.unwrap_or(0),
    }
}

// ── Epoch management ──────────────────────────────────────────────────────────

/// Get or create an epoch for the given slug. If the recipe/norms match the
/// current epoch, returns the existing cursor. If they changed, creates a new
/// epoch (marking old compiled/blocked items as 'stale') and returns cursor 0.
///
/// Returns (epoch_id, last_compiled_observation_id).
pub fn get_or_create_epoch(
    conn: &Connection,
    slug: &str,
    recipe_contribution_id: Option<&str>,
    norms_contribution_id: Option<&str>,
) -> Result<(String, i64)> {
    // Check for existing epoch
    let existing: Option<(String, Option<String>, Option<String>, i64)> = conn
        .query_row(
            "SELECT epoch_id, recipe_contribution_id, norms_contribution_id, last_compiled_observation_id
             FROM dadbear_compilation_state WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok();

    if let Some((epoch_id, existing_recipe, existing_norms, cursor)) = existing {
        // Check if recipe/norms match
        let recipe_match = match (&existing_recipe, recipe_contribution_id) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        let norms_match = match (&existing_norms, norms_contribution_id) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };

        if recipe_match && norms_match {
            return Ok((epoch_id, cursor));
        }

        // Recipe or norms changed — rotate epoch
        info!(
            slug = %slug,
            old_epoch = %epoch_id,
            "Epoch rotation: recipe or norms changed, creating new epoch"
        );

        // Mark old compiled/blocked items as 'stale'
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "UPDATE dadbear_work_items SET state = 'stale', state_changed_at = ?1
             WHERE slug = ?2 AND epoch_id = ?3 AND state IN ('compiled', 'blocked')",
            params![now, slug, epoch_id],
        )?;

        debug!(
            slug = %slug,
            old_epoch = %epoch_id,
            "Marked old epoch work items as stale"
        );
    }

    // Create new epoch
    let recipe_short = contribution_short(recipe_contribution_id);
    let norms_short = contribution_short(norms_contribution_id);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let new_epoch_id = format!("{slug}:{recipe_short}:{norms_short}:{timestamp}");
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    conn.execute(
        "INSERT OR REPLACE INTO dadbear_compilation_state
         (slug, epoch_id, recipe_contribution_id, norms_contribution_id,
          last_compiled_observation_id, epoch_start_observation_id, epoch_started_at)
         VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
        params![slug, new_epoch_id, recipe_contribution_id, norms_contribution_id, now],
    )
    .with_context(|| format!("Failed to create new epoch for slug '{slug}'"))?;

    info!(
        slug = %slug,
        epoch_id = %new_epoch_id,
        "Created new compilation epoch"
    );

    Ok((new_epoch_id, 0))
}

/// Extract the first 8 hex chars of a contribution UUID (hyphens removed).
/// Returns "00000000" for None.
fn contribution_short(contribution_id: Option<&str>) -> String {
    match contribution_id {
        Some(id) => {
            let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            hex.chars().take(8).collect::<String>()
        }
        None => "00000000".to_string(),
    }
}

// ── Core compilation ──────────────────────────────────────────────────────────

/// Read observation events since last_compiled_observation_id and compile them
/// into durable work items with dependency edges.
///
/// This function:
///   a) Reads observation events WHERE slug = ? AND id > last_compiled_observation_id
///   b) Maps event_type to primitives
///   c) Dedup check: skips if non-terminal work item already targets same (slug, target_id, step_name, layer)
///   d) Constructs semantic path IDs
///   e) Stores placeholder prompts (actual materialization at Phase 5 dispatch)
///   f) Creates work item rows in 'compiled' state
///   g) Creates dependency edges for cross-layer items
///   h) Returns the new cursor position and compilation stats
pub fn compile_observations(
    conn: &Connection,
    slug: &str,
    epoch_id: &str,
    recipe_contribution_id: Option<&str>,
    last_compiled_observation_id: i64,
) -> Result<CompilationResult> {
    // ── (a) Read new observation events ──────────────────────────────────
    let events = read_observation_events(conn, slug, last_compiled_observation_id)?;

    if events.is_empty() {
        return Ok(CompilationResult {
            new_cursor: last_compiled_observation_id,
            items_compiled: 0,
            deps_created: 0,
            deduped: 0,
        });
    }

    // v5 R4 cursor-gating fix: max of all-read events is dangerous because
    // a failed role-binding resolution (role_bound events) can leave the
    // event permanently skipped if cursor jumps past it. Compute cursor from
    // successfully-processed event ids only.
    let max_event_id = events.iter().map(|e| e.id).max().unwrap_or(last_compiled_observation_id);
    let ep_short = epoch_short(epoch_id);
    let bid = batch_id(slug, &ep_short, max_event_id);
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut items_compiled = 0usize;
    let mut deps_created = 0usize;
    let mut deduped = 0usize;
    let mut compiled_event_ids: Vec<i64> = Vec::new();

    // ── (b-g) Process each observation event ─────────────────────────────
    for event in &events {
        // (b) Map event_type to primitive
        let (primitive, step_name, model_tier) = match map_event_to_primitive(&event.event_type) {
            Some(mapping) => mapping,
            None => {
                // Phase 3 verifier fix: previously this arm advanced the
                // cursor (silent-skip-and-advance). Per feedback_loud_deferrals
                // unknown event types are now held in the read-pool: we warn
                // every tick until an operator either teaches the compiler
                // the new event_type or supersedes/deletes the rogue row.
                // Silent-advance used to lose events permanently.
                //
                // STRICT_UNKNOWN_EVENTS=1 still escalates to a hard panic
                // inside map_event_to_primitive, for operators who prefer
                // the loop die over spam logs. The default now loud-holds
                // instead of loud-losing.
                warn!(
                    slug = %slug,
                    event_type = %event.event_type,
                    event_id = event.id,
                    "Unknown observation event type — holding cursor so event is not lost; \
                     update map_event_to_primitive or supersede the row"
                );
                continue;
            }
        };

        // v5 Phase 3: for role_bound events, resolve the binding up front
        // and capture the handler chain id. On resolution failure, we do
        // NOT advance the cursor past this event so the next tick retries.
        //
        // v5 Phase 7a: annotation_reacted is routed via the TRIGGERING
        // VOCAB ENTRY's handler_chain_id (stamped into the event's
        // metadata_json by `process_annotation_hook` in 6c-B) rather than
        // via role_for_event → resolve_binding.
        //
        // v5 audit 7a-gen: generalized — ANY role_bound event whose
        // metadata carries a non-empty `handler_chain_id` takes the
        // override path (the vocab-supplied handler wins over
        // role_for_event). Before this audit the condition was hardcoded
        // to `event.event_type == "annotation_reacted"`, which forced a
        // code deploy every time a new role_bound event type wanted
        // vocab-driven dispatch. Post-audit, new role_bound emitters
        // just stamp `handler_chain_id` into their metadata and they
        // route via the override without touching the compiler.
        //
        // Safety audit (per feedback_architectural_lens): grepped every
        // `handler_chain_id` write site in observation-event metadata —
        // (1) process_annotation_hook emits annotation_reacted
        //     (intentional; this IS the generalized path);
        // (2) emit_vocabulary_event_with_reason emits vocabulary_published
        //     and vocabulary_superseded to the `__global__` slug. Those
        //     event types are NOT in map_event_to_primitive, so they
        //     never reach this role_bound branch regardless of their
        //     metadata shape;
        // (3) two test helpers in db.rs that mirror (1) for test driving.
        // No accidental `handler_chain_id` metadata fields exist today,
        // so the generalization is safe.
        //
        // Missing metadata on an event type that explicitly relies on
        // the override path (today: annotation_reacted) is a loud raise
        // per feedback_loud_deferrals: process_annotation_hook
        // unconditionally stamps `handler_chain_id` on every
        // annotation_reacted event, so an event without it is either a
        // direct-DB write bypassing the hook or a downgrade bug. Cursor
        // holds on the event so the operator sees the stuck row.
        //
        // For event types that DON'T depend on the override (e.g.
        // debate_spawned, gap_resolved), missing handler_chain_id is
        // expected — they fall through to role_for_event → resolve_binding.
        let resolved_chain_id: Option<String> = if primitive == "role_bound" {
            let meta_handler: Option<String> = event
                .metadata_json
                .as_deref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                .and_then(|v| {
                    v.get("handler_chain_id")
                        .and_then(|h| h.as_str())
                        .filter(|h| !h.is_empty())
                        .map(String::from)
                });

            // Event types that MUST have handler_chain_id (vocab-driven
            // dispatch is load-bearing): annotation_reacted today.
            // Missing metadata on these is a loud-hold.
            const METADATA_HANDLER_REQUIRED: &[&str] = &["annotation_reacted"];

            if let Some(handler) = meta_handler {
                Some(handler)
            } else if METADATA_HANDLER_REQUIRED.contains(&event.event_type.as_str()) {
                warn!(
                    slug = %slug,
                    event_type = %event.event_type,
                    event_id = event.id,
                    "{} missing handler_chain_id in metadata — \
                     process_annotation_hook should have stamped it. Cursor held; \
                     fix the event row or republish the annotation's vocab entry.",
                    event.event_type,
                );
                // Surface once into the chronicle, same loud-hold
                // posture the role-binding arm below uses.
                let source_id_fragment = format!(r#""source_event_id":{}"#, event.id);
                let already_emitted: bool = conn
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM dadbear_observation_events
                             WHERE slug = ?1
                               AND event_type = 'binding_unresolved'
                               AND metadata_json LIKE ?2
                         )",
                        params![slug, format!("%{source_id_fragment}%")],
                        |row| row.get::<_, bool>(0),
                    )
                    .unwrap_or(false);
                if !already_emitted {
                    let _ = crate::pyramid::observation_events::write_observation_event(
                        conn,
                        slug,
                        "dadbear",
                        "binding_unresolved",
                        None, None, None, None, None, None,
                        Some(&format!(
                            r#"{{"reason":"{}_missing_handler_chain_id","event_type":"{}","source_event_id":{}}}"#,
                            event.event_type, event.event_type, event.id
                        )),
                    );
                }
                // Cursor holds.
                continue;
            } else {
            let role_name = match role_for_event(&event.event_type) {
                Some(r) => r,
                None => {
                    warn!(
                        slug = %slug,
                        event_type = %event.event_type,
                        event_id = event.id,
                        "role_bound event has no role mapping — misconfigured event vocabulary"
                    );
                    // Cursor does NOT advance — retry next tick after fix.
                    continue;
                }
            };
            match crate::pyramid::role_binding::resolve_binding(conn, slug, role_name) {
                Ok(binding) => Some(binding.handler_chain_id),
                Err(e) => {
                    warn!(
                        slug = %slug,
                        event_type = %event.event_type,
                        role = %role_name,
                        event_id = event.id,
                        error = %e,
                        "role_bound resolution failed — cursor held, retry next tick"
                    );
                    // Emit chronicle entry for observability — but at most
                    // once per (source_event_id) pair. Without this guard,
                    // a stuck cursor (operator hasn't fixed the missing
                    // role binding) would re-emit `binding_unresolved` for
                    // the same source event on every compile tick.
                    //
                    // At 5s tick interval that's 17,280 rows/day per stuck
                    // role — retention only prunes below the min cursor,
                    // which is itself held by the same unresolved binding,
                    // so the rows accumulate indefinitely in
                    // `dadbear_observation_events`. One row per unresolved
                    // (source_event_id) is enough for an operator to see
                    // the drift in the chronicle; subsequent ticks still
                    // log a `warn!` line so the issue is visible in the
                    // running logs. Wanderer fix Phase 3.
                    let source_id_fragment =
                        format!(r#""source_event_id":{}"#, event.id);
                    let already_emitted: bool = conn
                        .query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM dadbear_observation_events
                                 WHERE slug = ?1
                                   AND event_type = 'binding_unresolved'
                                   AND metadata_json LIKE ?2
                             )",
                            params![slug, format!("%{source_id_fragment}%")],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false);
                    if !already_emitted {
                        let _ = crate::pyramid::observation_events::write_observation_event(
                            conn,
                            slug,
                            "dadbear",
                            "binding_unresolved",
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(&format!(
                                r#"{{"role":"{}","event_type":"{}","source_event_id":{}}}"#,
                                role_name, event.event_type, event.id
                            )),
                        );
                    }
                    // Cursor does NOT advance — retry next tick.
                    continue;
                }
            }
            }
        } else {
            None
        };

        let target_id = derive_target_id(event);
        let layer = derive_layer(event);

        // (c) Dedup check: skip if non-terminal work item already exists for same target
        if has_active_work_item(conn, slug, &target_id, step_name, layer)? {
            deduped += 1;
            debug!(
                slug = %slug,
                target_id = %target_id,
                step_name = %step_name,
                layer = layer,
                "Dedup: active work item already exists, skipping"
            );
            // Dedup-skipped events should still advance the cursor —
            // they're semantic duplicates that an earlier tick handled.
            compiled_event_ids.push(event.id);
            continue;
        }

        // (d) Construct semantic path ID
        let wi_id = work_item_id(slug, &ep_short, primitive, layer, &target_id);

        // (e) Placeholder prompts — actual materialization at dispatch time (Phase 5)
        let system_prompt = format!(
            "[DADBEAR compiler placeholder — materialized at dispatch]\n\
             Primitive: {primitive}\n\
             Step: {step_name}\n\
             Slug: {slug}\n\
             Layer: L{layer}\n\
             Target: {target_id}"
        );
        let user_prompt = format!(
            "[Pending materialization]\n\
             Event type: {event_type}\n\
             Source: {source}\n\
             File: {file_path}\n\
             Target node: {target_node}\n\
             Content hash: {content_hash}",
            event_type = event.event_type,
            source = event.source,
            file_path = event.file_path.as_deref().unwrap_or("N/A"),
            target_node = event.target_node_id.as_deref().unwrap_or("N/A"),
            content_hash = event.content_hash.as_deref().unwrap_or("N/A"),
        );

        // (f) Create work item row in 'compiled' state
        let observation_event_ids = serde_json::to_string(&[event.id])
            .unwrap_or_else(|_| format!("[{}]", event.id));

        let inserted = insert_work_item(
            conn,
            &wi_id,
            slug,
            &bid,
            epoch_id,
            recipe_contribution_id,
            step_name,
            primitive,
            layer,
            &target_id,
            &system_prompt,
            &user_prompt,
            model_tier,
            &observation_event_ids,
            &now,
        )?;

        if !inserted {
            // Work item ID already exists (idempotent — semantic path collision)
            deduped += 1;
            // Dedup via semantic path collision also counts as processed.
            compiled_event_ids.push(event.id);
            continue;
        }

        items_compiled += 1;
        compiled_event_ids.push(event.id);

        // v5 Phase 3: for role_bound dispatch, stamp the resolved chain id
        // onto the work_item row so the supervisor's dispatch arm can load
        // and invoke the chain without re-resolving the binding.
        if let Some(chain_id) = &resolved_chain_id {
            conn.execute(
                "UPDATE dadbear_work_items SET resolved_chain_id = ?1 WHERE id = ?2",
                params![chain_id, &wi_id],
            )
            .with_context(|| format!(
                "Failed to stamp resolved_chain_id on work_item '{wi_id}'"
            ))?;
        }

        // (g) Create dependency edges for cross-layer items.
        // The cascade observation's metadata_json may carry a triggering_work_item_id
        // (set by Phase 5 supervisor's result-application feedback loop). If present,
        // create a precise dep to that specific item. If absent (legacy cascade path),
        // create no dep — the cascade observation's existence proves L0 completed.
        if layer > 0 {
            let trigger_id = event.metadata_json.as_ref().and_then(|meta| {
                serde_json::from_str::<serde_json::Value>(meta)
                    .ok()
                    .and_then(|v| v.get("triggering_work_item_id")?.as_str().map(String::from))
            });
            let created = create_cross_layer_deps(
                conn, slug, &wi_id, &target_id, layer, &ep_short,
                trigger_id.as_deref(),
            )?;
            deps_created += created;
        }
    }

    // ── v5 R4 cursor-gating (Phase 3 verifier refinement): the cursor must
    // advance only to the largest contiguous-prefix id. `max(compiled_ids)`
    // alone was insufficient — it silently skipped over a held event when
    // subsequent events still succeeded, because the held id was absent
    // from `compiled_event_ids` while larger successful ids were present.
    // We compute the smallest held id, then take the largest compiled id
    // strictly less than it. Held events (and everything after them) stay
    // in the read-pool for next-tick retry — preserving the Wave 3 R4
    // contract that a failed resolution never loses in-flight events.
    let compiled_set: std::collections::HashSet<i64> =
        compiled_event_ids.iter().copied().collect();
    let min_held_id: Option<i64> = events
        .iter()
        .map(|e| e.id)
        .filter(|id| !compiled_set.contains(id))
        .min();
    let new_cursor = if let Some(first_held) = min_held_id {
        compiled_event_ids
            .iter()
            .copied()
            .filter(|id| *id < first_held)
            .max()
            .unwrap_or(last_compiled_observation_id)
    } else {
        compiled_event_ids
            .iter()
            .copied()
            .max()
            .unwrap_or(last_compiled_observation_id)
    };

    conn.execute(
        "UPDATE dadbear_compilation_state SET last_compiled_observation_id = ?1 WHERE slug = ?2",
        params![new_cursor, slug],
    )
    .with_context(|| format!("Failed to advance compilation cursor for slug '{slug}'"))?;

    if items_compiled > 0 || deduped > 0 {
        info!(
            slug = %slug,
            epoch_id = %epoch_id,
            items_compiled = items_compiled,
            deps_created = deps_created,
            deduped = deduped,
            cursor = new_cursor,
            skipped_for_retry = events.len() - compiled_event_ids.len(),
            "Compilation pass complete"
        );
    }

    Ok(CompilationResult {
        new_cursor,
        items_compiled,
        deps_created,
        deduped,
    })
}

/// v5 Phase 3: map event_type to the role that handles role_bound dispatch.
///
/// Returns None if the event_type is not a role_bound event (caller should
/// handle via map_event_to_primitive's primitive string instead).
///
/// Post-build accretion v5 audit (P3) decision on `gap_detected`:
///   The `gap` annotation type's vocab entry (Phase 7c addition) nominates
///   `starter-gap-dispatcher` as its handler_chain_id, so the FIRST
///   dispatch on a `gap` annotation already routes via the
///   annotation_reacted → handler_chain_id path (6c-B flip) — not via
///   this map. The `gap_detected` event is emitted BY the gap_dispatcher
///   chain AFTER the Gap node is materialized, purely as an
///   observability/chronicle marker.
///
///   Earlier Phase 7c kept `gap_detected → gap_dispatcher` for
///   "event-map symmetry". The audit found that this creates a wasted
///   compile+dispatch+no_op cycle on every gap-shape upgrade: the second
///   dispatch finds the target already Gap-shaped AND no annotation_id
///   in the input, so `materialize_gap_node` emits
///   `gap_dispatcher_skipped` and the work item CAS-completes. Result:
///   one spurious chronicle event + one wasted dispatch per gap, and an
///   annotation_reacted → gap_detected → gap_dispatcher cycle where the
///   second step is entirely redundant with the first.
///
///   Fix: map `gap_detected` to None (log_only). The chronicle event
///   still fires from the originating chain (audit trail preserved);
///   the second dispatch is removed entirely. If a caller directly
///   invokes the gap_dispatcher chain outside the annotation_reacted
///   path, `gap_dispatcher_skipped` can still fire — but it shouldn't
///   be the norm, and if it becomes one the loud deferral is visible.
///   Phase 8's LLM-driven gap enrichment (should it want reentry on
///   gap_detected) will route via a chain-level subscription, not via
///   this event-to-role map — the map is for role_bound dispatch, not
///   for every event that needs an effect.
pub(crate) fn role_for_event(event_type: &str) -> Option<&'static str> {
    match event_type {
        // Phase 8-1: annotation_written + annotation_superseded now route
        // via the cascade_handler role (previously mapped to `re_distill`
        // primitive which silently no-op'd in the supervisor — the
        // original DADBEAR non-firing bug). The bound chain runs; its
        // terminal queue_re_distill_for_target step enqueues a real
        // re_distill work item the Phase 8-2 supervisor arm applies.
        "annotation_written" | "annotation_superseded" => Some("cascade_handler"),
        "annotation_reacted" => Some("cascade_handler"),
        "debate_spawned" | "debate_collapsed" => Some("debate_steward"),
        // v5 audit P3: gap_detected is observability-only — the actual
        // dispatch already fired via annotation_reacted → handler_chain_id.
        "gap_detected" => None,
        "gap_resolved" | "purpose_shifted" => Some("meta_layer_oracle"),
        "meta_layer_crystallized" => Some("synthesizer"),
        // Phase 9b-1/9b-2: scheduler tick + volume-threshold trigger
        // fan out to the slug's `accretion_handler` / `sweep` role
        // bindings. The slug can supersede the binding to swap
        // chains without touching this map.
        "accretion_tick" | "accretion_threshold_hit" => Some("accretion_handler"),
        "sweep_tick" => Some("sweep"),
        _ => None,
    }
}

// ── DB helpers ────────────────────────────────────────────────────────────────

/// Read observation events for a slug since the given cursor.
fn read_observation_events(
    conn: &Connection,
    slug: &str,
    since_id: i64,
) -> Result<Vec<ObservationEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, source, event_type, source_path, file_path, content_hash,
                previous_hash, target_node_id, layer, detected_at, metadata_json
         FROM dadbear_observation_events
         WHERE slug = ?1 AND id > ?2
         ORDER BY id ASC",
    )?;

    let rows = stmt.query_map(params![slug, since_id], |row| {
        Ok(ObservationEvent {
            id: row.get(0)?,
            slug: row.get(1)?,
            source: row.get(2)?,
            event_type: row.get(3)?,
            source_path: row.get(4)?,
            file_path: row.get(5)?,
            content_hash: row.get(6)?,
            previous_hash: row.get(7)?,
            target_node_id: row.get(8)?,
            layer: row.get(9)?,
            detected_at: row.get(10)?,
            metadata_json: row.get(11)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row.with_context(|| "Failed to read observation event row")?);
    }
    Ok(events)
}

/// Check if a non-terminal work item already exists for the same
/// (slug, target_id, step_name, layer). Non-terminal states are:
/// compiled, previewed, dispatched, blocked.
fn has_active_work_item(
    conn: &Connection,
    slug: &str,
    target_id: &str,
    step_name: &str,
    layer: i64,
) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM dadbear_work_items
            WHERE slug = ?1 AND target_id = ?2 AND step_name = ?3 AND layer = ?4
              AND state IN ('compiled', 'previewed', 'dispatched', 'blocked')
        )",
        params![slug, target_id, step_name, layer],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Insert a work item row. Returns false if the ID already exists (ON CONFLICT IGNORE).
fn insert_work_item(
    conn: &Connection,
    id: &str,
    slug: &str,
    batch_id: &str,
    epoch_id: &str,
    recipe_contribution_id: Option<&str>,
    step_name: &str,
    primitive: &str,
    layer: i64,
    target_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    model_tier: &str,
    observation_event_ids: &str,
    now: &str,
) -> Result<bool> {
    let rows_affected = conn.execute(
        "INSERT OR IGNORE INTO dadbear_work_items
         (id, slug, batch_id, epoch_id, recipe_contribution_id, step_name, primitive,
          layer, target_id, system_prompt, user_prompt, model_tier,
          observation_event_ids, compiled_at, state, state_changed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'compiled', ?14)",
        params![
            id,
            slug,
            batch_id,
            epoch_id,
            recipe_contribution_id,
            step_name,
            primitive,
            layer,
            target_id,
            system_prompt,
            user_prompt,
            model_tier,
            observation_event_ids,
            now,
        ],
    )?;
    Ok(rows_affected > 0)
}

/// Create dependency edges for cross-layer work items.
///
/// For a work item at layer N targeting a given node, look for applied or
/// in-flight work items at layer N-1 that could be prerequisites. The DAG
/// Create cross-layer dependency edges for L1+ work items.
///
/// For cascade-triggered items: the cascade observation carries a
/// `triggering_work_item_id` in metadata_json (set by the Phase 5
/// supervisor's result-application feedback loop). If present, create
/// a dep to that specific item. If absent (legacy cascade path before
/// Phase 5), create NO dep — the cascade observation's existence proves
/// the prerequisite L0 work already completed and was applied.
///
/// This is the systemic fix: dependencies are precise (one L1 item →
/// one triggering L0 item), not slug-wide. The dep is for traceability
/// and correctness, not blocking — cascade-triggered items' deps are
/// immediately met since the triggering item is already applied.
///
/// Returns the number of dependency edges created.
fn create_cross_layer_deps(
    conn: &Connection,
    _slug: &str,
    work_item_id: &str,
    _target_id: &str,
    _layer: i64,
    _ep_short: &str,
    triggering_work_item_id: Option<&str>,
) -> Result<usize> {
    let Some(trigger_id) = triggering_work_item_id else {
        // No triggering work item ID available (legacy cascade path).
        // The cascade observation's existence proves L0 work completed.
        // No dep needed — this item is independently dispatchable.
        return Ok(0);
    };

    // Verify the triggering item exists before creating the edge
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dadbear_work_items WHERE id = ?1)",
        params![trigger_id],
        |row| row.get(0),
    ).unwrap_or(false);

    if !exists {
        debug!(
            work_item_id = %work_item_id,
            trigger_id = %trigger_id,
            "Triggering work item not found — skipping dep edge"
        );
        return Ok(0);
    }

    let rows = conn.execute(
        "INSERT OR IGNORE INTO dadbear_work_item_deps (work_item_id, depends_on_id)
         VALUES (?1, ?2)",
        params![work_item_id, trigger_id],
    )?;

    if rows > 0 {
        debug!(
            work_item_id = %work_item_id,
            depends_on = %trigger_id,
            "Created targeted cross-layer dependency edge"
        );
    }

    Ok(rows)
}

// ── Public integration point for the tick loop ────────────────────────────────

/// Run a full compilation pass for a single slug. This is the entry point
/// called from `dadbear_extend.rs` in the tick loop.
///
/// Steps:
///   1. Get or create the current epoch (checking recipe/norms)
///   2. Compile new observations since the cursor
///   3. Return the compilation result for logging/telemetry
pub fn run_compilation_for_slug(
    conn: &Connection,
    slug: &str,
    recipe_contribution_id: Option<&str>,
    norms_contribution_id: Option<&str>,
) -> Result<CompilationResult> {
    let (epoch_id, last_cursor) = get_or_create_epoch(
        conn,
        slug,
        recipe_contribution_id,
        norms_contribution_id,
    )?;

    compile_observations(
        conn,
        slug,
        &epoch_id,
        recipe_contribution_id,
        last_cursor,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_item_id_format() {
        let id = work_item_id("opt-025", "a1b2c3d4", "stale_check", 0, "node-abc123");
        assert_eq!(id, "opt-025:a1b2c3d4:stale_check:0:node-abc123");
    }

    #[test]
    fn test_work_item_id_with_composite_target() {
        let id = work_item_id("opt-025", "a1b2c3d4", "edge_check", 2, "edge/L2-003/L2-007");
        assert_eq!(id, "opt-025:a1b2c3d4:edge_check:2:edge/L2-003/L2-007");
        // Verify splitn(5, ':') parsing contract
        let parts: Vec<&str> = id.splitn(5, ':').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "opt-025");
        assert_eq!(parts[1], "a1b2c3d4");
        assert_eq!(parts[2], "edge_check");
        assert_eq!(parts[3], "2");
        assert_eq!(parts[4], "edge/L2-003/L2-007");
    }

    #[test]
    fn test_batch_id_format() {
        let id = batch_id("opt-025", "a1b2c3d4", 42);
        assert_eq!(id, "opt-025:a1b2c3d4:batch-42");
    }

    #[test]
    fn test_attempt_id_format() {
        let wi_id = "opt-025:a1b2c3d4:stale_check:0:node-abc123";
        let id = attempt_id(wi_id, 3);
        assert_eq!(id, "opt-025:a1b2c3d4:stale_check:0:node-abc123:a3");
    }

    #[test]
    fn test_contribution_short() {
        assert_eq!(contribution_short(Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890")), "a1b2c3d4");
        assert_eq!(contribution_short(Some("abcdef01")), "abcdef01");
        assert_eq!(contribution_short(None), "00000000");
    }

    #[test]
    fn test_epoch_short() {
        let eid = "opt-025:a1b2c3d4:e5f6g7h8:20260415T0130";
        assert_eq!(epoch_short(eid), "a1b2c3d4");
    }

    #[test]
    fn test_map_event_to_primitive() {
        assert!(map_event_to_primitive("file_created").is_some());
        assert!(map_event_to_primitive("file_modified").is_some());
        assert!(map_event_to_primitive("bogus_event").is_none());

        let (prim, step, tier) = map_event_to_primitive("file_modified").unwrap();
        assert_eq!(prim, "stale_check");
        assert_eq!(step, "l0_stale_check");
        assert_eq!(tier, "stale_remote");
    }

    #[test]
    fn test_annotation_events_route_to_cascade_handler_role() {
        // Phase 8-1 flip: annotation_written and annotation_superseded now
        // compile to `role_bound` so the cascade_handler chain runs instead
        // of the legacy silent `re_distill` primitive. Both events share
        // the same step_name so they coalesce on has_active_work_item.
        let (prim, step, tier) = map_event_to_primitive("annotation_written").unwrap();
        assert_eq!(prim, "role_bound");
        assert_eq!(step, "annotation_cascade");
        assert_eq!(tier, "stale_remote");

        let (prim2, step2, tier2) = map_event_to_primitive("annotation_superseded").unwrap();
        assert_eq!(prim2, prim);
        assert_eq!(step2, step);
        assert_eq!(tier2, tier);

        // role_for_event must now hand these events to cascade_handler so
        // the supervisor resolves the slug's binding and invokes the chain.
        assert_eq!(role_for_event("annotation_written"), Some("cascade_handler"));
        assert_eq!(role_for_event("annotation_superseded"), Some("cascade_handler"));
    }

    #[test]
    fn test_annotation_events_use_event_layer() {
        let event = ObservationEvent {
            id: 1,
            slug: "s".into(),
            source: "annotation".into(),
            event_type: "annotation_written".into(),
            source_path: None,
            file_path: None,
            content_hash: None,
            previous_hash: None,
            target_node_id: Some("L2-004".into()),
            layer: Some(2),
            detected_at: "2026-04-22 00:00:00".into(),
            metadata_json: None,
        };
        assert_eq!(derive_layer(&event), 2);
        assert_eq!(derive_target_id(&event), "L2-004");
    }
}
