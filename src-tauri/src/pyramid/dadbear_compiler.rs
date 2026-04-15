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
fn map_event_to_primitive(event_type: &str) -> Option<(&'static str, &'static str, &'static str)> {
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
        _ => None,
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

    let new_cursor = events.iter().map(|e| e.id).max().unwrap_or(last_compiled_observation_id);
    let ep_short = epoch_short(epoch_id);
    let bid = batch_id(slug, &ep_short, new_cursor);
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut items_compiled = 0usize;
    let mut deps_created = 0usize;
    let mut deduped = 0usize;

    // ── (b-g) Process each observation event ─────────────────────────────
    for event in &events {
        // (b) Map event_type to primitive
        let (primitive, step_name, model_tier) = match map_event_to_primitive(&event.event_type) {
            Some(mapping) => mapping,
            None => {
                warn!(
                    slug = %slug,
                    event_type = %event.event_type,
                    event_id = event.id,
                    "Unknown observation event type, skipping"
                );
                continue;
            }
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
            continue;
        }

        items_compiled += 1;

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

    // ── Update compilation cursor ────────────────────────────────────────
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
}
