// pyramid/auto_update_ops.rs — Operational state transitions for auto-update.
//
// NORM: All frozen/breaker state mutations go through this module.
// Do NOT write bare UPDATEs to hold state anywhere else in the codebase.
//
// == DADBEAR Canonical Architecture (Phase 7 — legacy cleanup complete) ==
//
// Every hold mutation writes TWO things atomically:
//   1. Append-only hold event  → `dadbear_hold_events`
//   2. Materialized projection → `dadbear_holds_projection`
//
// The holds projection is the sole authority for frozen/breaker state.
// The old `pyramid_auto_update_config.frozen` / `breaker_tripped` columns
// are no longer written (dual-write removed in Phase 7).
//
// After each mutation, `DadbearHoldsChanged` is emitted on the event bus.
//
// The master gate query (`db::get_enabled_dadbear_configs`) uses the holds
// projection anti-join — contribution existence is the enable gate.
//
// The stale engine methods (freeze/unfreeze/trip_breaker/resume_breaker)
// handle in-memory state (cancel timers, set flags) and then delegate
// the DB write + event to functions in this module.

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use std::sync::Arc;
use tracing::{info, warn};

use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};

/// A single active hold on a pyramid slug.
#[derive(Debug, Clone)]
pub struct Hold {
    pub slug: String,
    pub hold: String,
    pub held_since: String,
    pub reason: Option<String>,
}

// ── Single-slug operations ──────────────────────────────────────────────────

/// Freeze a pyramid's auto-update. Writes hold event + projection.
pub fn freeze(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, 'frozen', 'placed', NULL, ?2)",
        rusqlite::params![slug, now],
    )?;

    // 2. Upsert projection
    conn.execute(
        "INSERT OR REPLACE INTO dadbear_holds_projection (slug, hold, held_since, reason)
         VALUES (?1, 'frozen', ?2, NULL)",
        rusqlite::params![slug, now],
    )?;

    info!(slug = %slug, "auto_update_ops::freeze persisted");
    emit_holds_changed(bus, slug, "frozen", "placed");
    Ok(())
}

/// Unfreeze a pyramid's auto-update. Writes hold event + clears projection.
pub fn unfreeze(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, 'frozen', 'cleared', NULL, ?2)",
        rusqlite::params![slug, now],
    )?;

    // 2. Remove from projection
    conn.execute(
        "DELETE FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen'",
        rusqlite::params![slug],
    )?;

    info!(slug = %slug, "auto_update_ops::unfreeze persisted");
    emit_holds_changed(bus, slug, "frozen", "cleared");
    Ok(())
}

/// Trip the circuit breaker. Writes hold event + projection.
pub fn trip_breaker(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, 'breaker', 'placed', NULL, ?2)",
        rusqlite::params![slug, now],
    )?;

    // 2. Upsert projection
    conn.execute(
        "INSERT OR REPLACE INTO dadbear_holds_projection (slug, hold, held_since, reason)
         VALUES (?1, 'breaker', ?2, NULL)",
        rusqlite::params![slug, now],
    )?;

    warn!(slug = %slug, "auto_update_ops::trip_breaker persisted");
    emit_holds_changed(bus, slug, "breaker", "placed");
    Ok(())
}

/// Resume from a breaker trip. Writes hold event + clears projection.
pub fn resume_breaker(conn: &Connection, bus: &Arc<BuildEventBus>, slug: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, 'breaker', 'cleared', NULL, ?2)",
        rusqlite::params![slug, now],
    )?;

    // 2. Remove from projection
    conn.execute(
        "DELETE FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'breaker'",
        rusqlite::params![slug],
    )?;

    info!(slug = %slug, "auto_update_ops::resume_breaker persisted");
    emit_holds_changed(bus, slug, "breaker", "cleared");
    Ok(())
}

// ── Generic hold operations ────────────────────────────────────────────────

/// Place an arbitrary hold on a pyramid slug.
///
/// This is the generic form of freeze/trip_breaker. Hold names are open-ended
/// (e.g. "frozen", "breaker", "cost_limit") — any string is valid. The hold
/// event + projection pattern is identical regardless of hold name.
pub fn place_hold(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    slug: &str,
    hold: &str,
    reason: Option<&str>,
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, ?2, 'placed', ?3, ?4)",
        rusqlite::params![slug, hold, reason, now],
    )?;

    // 2. Upsert projection
    conn.execute(
        "INSERT OR REPLACE INTO dadbear_holds_projection (slug, hold, held_since, reason)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![slug, hold, now, reason],
    )?;

    info!(slug = %slug, hold = %hold, "auto_update_ops::place_hold persisted");
    emit_holds_changed(bus, slug, hold, "placed");
    Ok(())
}

/// Clear an arbitrary hold on a pyramid slug.
///
/// Generic counterpart to `place_hold`. For "frozen" and "breaker" holds,
/// prefer the dedicated unfreeze/resume_breaker functions.
pub fn clear_hold(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    slug: &str,
    hold: &str,
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 1. Append hold event
    conn.execute(
        "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
         VALUES (?1, ?2, 'cleared', NULL, ?3)",
        rusqlite::params![slug, hold, now],
    )?;

    // 2. Remove from projection
    conn.execute(
        "DELETE FROM dadbear_holds_projection WHERE slug = ?1 AND hold = ?2",
        rusqlite::params![slug, hold],
    )?;

    info!(slug = %slug, hold = %hold, "auto_update_ops::clear_hold persisted");
    emit_holds_changed(bus, slug, hold, "cleared");
    Ok(())
}

/// Check whether a slug has a specific active hold.
pub fn has_hold(conn: &Connection, slug: &str, hold: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = ?2)",
        rusqlite::params![slug, hold],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

// ── Utility functions ──────────────────────────────────────────────────────

/// Check whether a slug has ANY active hold (frozen, breaker, or future types).
pub fn is_held(conn: &Connection, slug: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1)",
        rusqlite::params![slug],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Get all active holds for a slug.
pub fn get_holds(conn: &Connection, slug: &str) -> Vec<Hold> {
    let result: Result<Vec<Hold>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare(
            "SELECT slug, hold, held_since, reason FROM dadbear_holds_projection WHERE slug = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug], |row| {
            Ok(Hold {
                slug: row.get(0)?,
                hold: row.get(1)?,
                held_since: row.get(2)?,
                reason: row.get(3)?,
            })
        })?;
        let mut holds = Vec::new();
        for row in rows {
            holds.push(row?);
        }
        Ok(holds)
    })();
    result.unwrap_or_default()
}

// ── Bulk operations ─────────────────────────────────────────────────────────

/// Freeze all pyramids matching the given scope. Returns the slugs that were
/// actually changed (previously unfrozen, now frozen). Callers use `.len()`
/// for the count and the slug list to freeze in-memory stale engines.
///
/// Scopes:
/// - "all": freeze every pyramid without a 'frozen' hold
/// - "slug": freeze a single pyramid (scope_value = slug)
/// - "folder": freeze all pyramids that have DADBEAR watch configs under
///   the given folder path. Resolves slugs via `pyramid_dadbear_config`.
pub fn freeze_all(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    scope: &str,
    scope_value: Option<&str>,
) -> Result<Vec<String>> {
    // resolve_scope_slugs(currently_frozen=false) returns only unfrozen slugs,
    // so every slug in this list is genuinely transitioning to frozen.
    let slugs = resolve_scope_slugs(conn, scope, scope_value, false)?;
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut affected = Vec::new();
    for slug in &slugs {
        // 1. Append hold event
        conn.execute(
            "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
             VALUES (?1, 'frozen', 'placed', NULL, ?2)",
            rusqlite::params![slug, now],
        )?;

        // 2. Upsert projection (INSERT OR REPLACE handles idempotency)
        conn.execute(
            "INSERT OR REPLACE INTO dadbear_holds_projection (slug, hold, held_since, reason)
             VALUES (?1, 'frozen', ?2, NULL)",
            rusqlite::params![slug, now],
        )?;

        affected.push(slug.clone());
        emit_holds_changed(bus, slug, "frozen", "placed");
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
    // resolve_scope_slugs(currently_frozen=true) returns only frozen slugs,
    // so every slug in this list is genuinely transitioning to unfrozen.
    let slugs = resolve_scope_slugs(conn, scope, scope_value, true)?;
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut affected = Vec::new();
    for slug in &slugs {
        // 1. Append hold event
        conn.execute(
            "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
             VALUES (?1, 'frozen', 'cleared', NULL, ?2)",
            rusqlite::params![slug, now],
        )?;

        // 2. Remove from projection
        conn.execute(
            "DELETE FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen'",
            rusqlite::params![slug],
        )?;

        affected.push(slug.clone());
        emit_holds_changed(bus, slug, "frozen", "cleared");
    }
    Ok(affected)
}

/// Count how many pyramids would be affected by a freeze/unfreeze operation.
/// Used by the scope picker modal for count preview.
///
/// `target_frozen`: true = count currently unfrozen (would be frozen),
///                  false = count currently frozen (would be unfrozen).
///
/// Now reads from the holds projection instead of the old table.
pub fn count_freeze_scope(
    conn: &Connection,
    scope: &str,
    scope_value: Option<&str>,
    target_frozen: bool,
) -> Result<usize> {
    // target_frozen=true means "user wants to freeze" → needs currently
    // UNFROZEN slugs → currently_frozen=false.  Mirrors freeze_all (false)
    // and unfreeze_all (true).
    let slugs = resolve_scope_slugs(conn, scope, scope_value, !target_frozen)?;
    Ok(slugs.len())
}

// ── Internals ───────────────────────────────────────────────────────────────

/// Emit the new canonical DadbearHoldsChanged event.
fn emit_holds_changed(bus: &Arc<BuildEventBus>, slug: &str, hold: &str, action: &str) {
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::DadbearHoldsChanged {
            slug: slug.to_string(),
            hold: hold.to_string(),
            action: action.to_string(),
        },
    });
}

/// Resolve which slugs are affected by a scope + scope_value combination.
///
/// `currently_frozen`: when true, returns only slugs WITH a 'frozen' hold
///   in the projection (for unfreeze); when false, returns only slugs
///   WITHOUT a 'frozen' hold (for freeze).
///
/// Now reads from `dadbear_holds_projection` instead of
/// `pyramid_auto_update_config.frozen`.
fn resolve_scope_slugs(
    conn: &Connection,
    scope: &str,
    scope_value: Option<&str>,
    currently_frozen: bool,
) -> Result<Vec<String>> {
    match scope {
        "all" => {
            if currently_frozen {
                // Return slugs that HAVE a 'frozen' hold
                let mut stmt = conn
                    .prepare("SELECT slug FROM dadbear_holds_projection WHERE hold = 'frozen'")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                let mut slugs = Vec::new();
                for row in rows {
                    slugs.push(row?);
                }
                Ok(slugs)
            } else {
                // Return slugs that do NOT have a 'frozen' hold.
                // Use pyramid_slugs as the slug universe (ALL pyramids),
                // not pyramid_dadbear_config (only watch-configured ones).
                let mut stmt = conn.prepare(
                    "SELECT s.slug FROM pyramid_slugs s
                     WHERE s.archived_at IS NULL
                       AND s.slug NOT LIKE '%--bunch-%'
                       AND NOT EXISTS (
                         SELECT 1 FROM dadbear_holds_projection h
                         WHERE h.slug = s.slug AND h.hold = 'frozen'
                     )",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                let mut slugs = Vec::new();
                for row in rows {
                    slugs.push(row?);
                }
                Ok(slugs)
            }
        }
        "slug" => {
            let slug =
                scope_value.ok_or_else(|| anyhow::anyhow!("scope='slug' requires scope_value"))?;
            if currently_frozen {
                // Check if this slug has a 'frozen' hold
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen'",
                    rusqlite::params![slug],
                    |row| row.get(0),
                )?;
                if count > 0 {
                    Ok(vec![slug.to_string()])
                } else {
                    Ok(vec![])
                }
            } else {
                // Check if this slug does NOT have a 'frozen' hold
                let has_frozen: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen')",
                    rusqlite::params![slug],
                    |row| row.get(0),
                )?;
                if !has_frozen {
                    // Verify slug exists in pyramid_slugs (not archived)
                    let exists: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM pyramid_slugs WHERE slug = ?1 AND archived_at IS NULL)",
                        rusqlite::params![slug],
                        |row| row.get(0),
                    )?;
                    if exists {
                        Ok(vec![slug.to_string()])
                    } else {
                        Ok(vec![])
                    }
                } else {
                    Ok(vec![])
                }
            }
        }
        "folder" => {
            let folder = scope_value
                .ok_or_else(|| anyhow::anyhow!("scope='folder' requires scope_value"))?;
            let like_pattern = format!("{}/%", folder);
            if currently_frozen {
                // Slugs under this folder WITH a 'frozen' hold.
                // Use pyramid_slugs as universe, filter by source_path.
                let mut stmt = conn.prepare(
                    "SELECT s.slug
                     FROM pyramid_slugs s
                     WHERE s.archived_at IS NULL
                       AND (s.source_path = ?1 OR s.source_path LIKE ?2)
                       AND EXISTS (
                           SELECT 1 FROM dadbear_holds_projection h
                           WHERE h.slug = s.slug AND h.hold = 'frozen'
                       )",
                )?;
                let rows = stmt.query_map(rusqlite::params![folder, like_pattern], |row| {
                    row.get::<_, String>(0)
                })?;
                let mut slugs = Vec::new();
                for row in rows {
                    slugs.push(row?);
                }
                Ok(slugs)
            } else {
                // Slugs under this folder WITHOUT a 'frozen' hold
                let mut stmt = conn.prepare(
                    "SELECT s.slug
                     FROM pyramid_slugs s
                     WHERE s.archived_at IS NULL
                       AND (s.source_path = ?1 OR s.source_path LIKE ?2)
                       AND NOT EXISTS (
                           SELECT 1 FROM dadbear_holds_projection h
                           WHERE h.slug = s.slug AND h.hold = 'frozen'
                       )",
                )?;
                let rows = stmt.query_map(rusqlite::params![folder, like_pattern], |row| {
                    row.get::<_, String>(0)
                })?;
                let mut slugs = Vec::new();
                for row in rows {
                    slugs.push(row?);
                }
                Ok(slugs)
            }
        }
        other => Err(anyhow::anyhow!("Unknown freeze scope: {}", other)),
    }
}
