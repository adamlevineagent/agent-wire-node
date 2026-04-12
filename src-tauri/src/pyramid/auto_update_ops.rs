// pyramid/auto_update_ops.rs — Operational state transitions for auto-update.
//
// NORM: All frozen/breaker state mutations on `pyramid_auto_update_config`
// go through this module. Do NOT write
//   UPDATE pyramid_auto_update_config SET frozen = ... / breaker_tripped = ...
// anywhere else in the codebase. This module owns the DB write + event
// emission atomically, ensuring every state change is both persisted and
// broadcast to the UI.
//
// The stale engine methods (freeze/unfreeze/trip_breaker/resume_breaker)
// handle in-memory state (cancel timers, set flags) and then delegate
// the DB write + event to functions in this module.
//
// Any new subsystem that participates in "keeping a pyramid updated" MUST
// respect the master enable gate on `pyramid_auto_update_config`:
//   auto_update = 1 AND frozen = 0 AND breaker_tripped = 0
// See `db::get_enabled_dadbear_configs()` for the canonical enforcement.

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::pyramid::db;
use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};

// ── Single-slug operations ──────────────────────────────────────────────────

/// Freeze a pyramid's auto-update. Persists to DB + emits event.
pub fn freeze(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "UPDATE pyramid_auto_update_config
         SET frozen = 1, frozen_at = ?1
         WHERE slug = ?2",
        rusqlite::params![now, slug],
    )?;
    info!(slug = %slug, "auto_update_ops::freeze persisted");
    emit_state_changed(conn, bus, slug);
    Ok(())
}

/// Unfreeze a pyramid's auto-update. Persists to DB + emits event.
pub fn unfreeze(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_auto_update_config
         SET frozen = 0, frozen_at = NULL
         WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    info!(slug = %slug, "auto_update_ops::unfreeze persisted");
    emit_state_changed(conn, bus, slug);
    Ok(())
}

/// Trip the circuit breaker. Persists to DB + emits event.
pub fn trip_breaker(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "UPDATE pyramid_auto_update_config
         SET breaker_tripped = 1, breaker_tripped_at = ?1
         WHERE slug = ?2",
        rusqlite::params![now, slug],
    )?;
    warn!(slug = %slug, "auto_update_ops::trip_breaker persisted");
    emit_state_changed(conn, bus, slug);
    Ok(())
}

/// Resume from a breaker trip. Persists to DB + emits event.
pub fn resume_breaker(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_auto_update_config
         SET breaker_tripped = 0, breaker_tripped_at = NULL
         WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    info!(slug = %slug, "auto_update_ops::resume_breaker persisted");
    emit_state_changed(conn, bus, slug);
    Ok(())
}

// ── Bulk operations ─────────────────────────────────────────────────────────

/// Freeze all pyramids matching the given scope. Returns the slugs that were
/// actually changed (previously unfrozen, now frozen). Callers use `.len()`
/// for the count and the slug list to freeze in-memory stale engines.
///
/// Scopes:
/// - "all": freeze every pyramid with `frozen = 0`
/// - "slug": freeze a single pyramid (scope_value = slug)
/// - "folder": freeze all pyramids that have DADBEAR watch configs under
///   the given folder path. Resolves slugs via `pyramid_dadbear_config`.
pub fn freeze_all(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    scope: &str,
    scope_value: Option<&str>,
) -> Result<Vec<String>> {
    let slugs = resolve_scope_slugs(conn, scope, scope_value, false)?;
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut affected = Vec::new();
    for slug in &slugs {
        let changed = conn.execute(
            "UPDATE pyramid_auto_update_config
             SET frozen = 1, frozen_at = ?1
             WHERE slug = ?2 AND frozen = 0",
            rusqlite::params![now, slug],
        )?;
        if changed > 0 {
            affected.push(slug.clone());
            emit_state_changed(conn, bus, slug);
        }
    }
    Ok(affected)
}

/// Unfreeze all pyramids matching the given scope. Returns the slugs that were
/// actually changed (previously frozen, now unfrozen). Callers use `.len()`
/// for the count and the slug list to unfreeze in-memory stale engines.
pub fn unfreeze_all(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    scope: &str,
    scope_value: Option<&str>,
) -> Result<Vec<String>> {
    let slugs = resolve_scope_slugs(conn, scope, scope_value, true)?;
    let mut affected = Vec::new();
    for slug in &slugs {
        let changed = conn.execute(
            "UPDATE pyramid_auto_update_config
             SET frozen = 0, frozen_at = NULL
             WHERE slug = ?1 AND frozen = 1",
            rusqlite::params![slug],
        )?;
        if changed > 0 {
            affected.push(slug.clone());
            emit_state_changed(conn, bus, slug);
        }
    }
    Ok(affected)
}

/// Count how many pyramids would be affected by a freeze/unfreeze operation.
/// Used by the scope picker modal for count preview.
///
/// `target_frozen`: true = count currently unfrozen (would be frozen),
///                  false = count currently frozen (would be unfrozen).
pub fn count_freeze_scope(
    conn: &Connection,
    scope: &str,
    scope_value: Option<&str>,
    target_frozen: bool,
) -> Result<usize> {
    let slugs = resolve_scope_slugs(conn, scope, scope_value, target_frozen)?;
    Ok(slugs.len())
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Read the current state and emit an AutoUpdateStateChanged event.
fn emit_state_changed(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) {
    let (frozen, breaker_tripped, auto_update) = match conn.query_row(
        "SELECT frozen, breaker_tripped, auto_update FROM pyramid_auto_update_config WHERE slug = ?1",
        rusqlite::params![slug],
        |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)),
    ) {
        Ok(state) => state,
        Err(e) => {
            warn!(slug = %slug, "auto_update_ops: failed to read state for event: {e}");
            return;
        }
    };

    let _ = bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::AutoUpdateStateChanged {
            frozen,
            breaker_tripped,
            auto_update,
        },
    });
}

/// Resolve which slugs are affected by a scope + scope_value combination.
///
/// `currently_frozen`: when true, returns only frozen slugs (for unfreeze);
///                     when false, returns only unfrozen slugs (for freeze).
fn resolve_scope_slugs(
    conn: &Connection,
    scope: &str,
    scope_value: Option<&str>,
    currently_frozen: bool,
) -> Result<Vec<String>> {
    let frozen_val = if currently_frozen { 1 } else { 0 };
    match scope {
        "all" => {
            let mut stmt = conn.prepare(
                "SELECT slug FROM pyramid_auto_update_config WHERE frozen = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![frozen_val], |row| {
                row.get::<_, String>(0)
            })?;
            let mut slugs = Vec::new();
            for row in rows {
                slugs.push(row?);
            }
            Ok(slugs)
        }
        "slug" => {
            let slug = scope_value
                .ok_or_else(|| anyhow::anyhow!("scope='slug' requires scope_value"))?;
            // Verify the slug exists and matches the frozen state
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pyramid_auto_update_config WHERE slug = ?1 AND frozen = ?2",
                rusqlite::params![slug, frozen_val],
                |row| row.get(0),
            )?;
            if count > 0 {
                Ok(vec![slug.to_string()])
            } else {
                Ok(vec![])
            }
        }
        "folder" => {
            let folder = scope_value
                .ok_or_else(|| anyhow::anyhow!("scope='folder' requires scope_value"))?;
            // Find slugs that have DADBEAR watch configs under this folder.
            // Use pyramid_dadbear_config (individual source_path values) to avoid
            // JSON parsing of pyramid_slugs.source_path.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT d.slug
                 FROM pyramid_dadbear_config d
                 JOIN pyramid_auto_update_config a ON d.slug = a.slug
                 WHERE (d.source_path = ?1 OR d.source_path LIKE ?2)
                   AND a.frozen = ?3",
            )?;
            let like_pattern = format!("{}/%", folder);
            let rows = stmt.query_map(
                rusqlite::params![folder, like_pattern, frozen_val],
                |row| row.get::<_, String>(0),
            )?;
            let mut slugs = Vec::new();
            for row in rows {
                slugs.push(row?);
            }
            Ok(slugs)
        }
        other => Err(anyhow::anyhow!("Unknown freeze scope: {}", other)),
    }
}
