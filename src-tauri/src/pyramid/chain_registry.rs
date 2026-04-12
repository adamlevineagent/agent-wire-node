use anyhow::Result;
use rusqlite::Connection;
use tracing::{info, warn};

use super::db::ChainDefaultMapping;

// ── Table initialization ─────────────────────────────────────────────────────

/// Initialize chain tables. Call during init_pyramid_db().
///
/// Two operational tables:
///   * `pyramid_chain_assignments` — per-slug overrides (tier 1), synced from
///     `chain_assignment` contributions.
///   * `pyramid_chain_defaults` — content-type → chain_id mapping (tier 2),
///     synced from the active `chain_defaults` contribution (ships bundled,
///     updatable via Wire, supersedable locally).
///
/// Both tables are operational caches. The source of truth is always the
/// contribution in `pyramid_config_contributions`. External access goes
/// through contribution CRUD; these helpers are called by the sync dispatcher.
pub fn init_chain_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Per-slug chain override (tier 1).
        -- The old schema had an extra `chain_file TEXT` column that no call
        -- site ever read. We leave it in place on existing installs (harmless)
        -- rather than DROP+CREATE, which would destroy any user-set per-slug
        -- assignments on every boot. New installs get the clean schema.
        CREATE TABLE IF NOT EXISTS pyramid_chain_assignments (
            slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            chain_id TEXT NOT NULL,
            assigned_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Content-type default mapping (tier 2).
        -- Populated from the active chain_defaults contribution. The wildcard
        -- content_type '*' serves as the global fallback within the table;
        -- evidence_mode '*' matches any mode.
        CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
            content_type TEXT NOT NULL,
            evidence_mode TEXT NOT NULL DEFAULT '*',
            chain_id TEXT NOT NULL,
            contribution_id TEXT NOT NULL,
            PRIMARY KEY (content_type, evidence_mode)
        );
        ",
    )?;
    Ok(())
}

// ── Tier 1: per-slug assignment (operational helpers) ────────────────────────

/// Assign a chain to a pyramid slug. Called by the `chain_assignment`
/// sync dispatcher branch.
pub fn assign_chain(conn: &Connection, slug: &str, chain_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_assignments (slug, chain_id)
         VALUES (?1, ?2)
         ON CONFLICT(slug) DO UPDATE SET chain_id = excluded.chain_id,
                                         assigned_at = datetime('now')",
        rusqlite::params![slug, chain_id],
    )?;
    Ok(())
}

/// Get the chain assignment for a slug. Returns chain_id or None.
pub fn get_assignment(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT chain_id FROM pyramid_chain_assignments WHERE slug = ?1")?;
    let result = stmt.query_row(rusqlite::params![slug], |row| row.get::<_, String>(0));
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Remove chain assignment for a slug (falls back to tier 2 defaults).
pub fn remove_assignment(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_chain_assignments WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// List all per-slug assignments. Returns Vec of (slug, chain_id).
pub fn list_assignments(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT slug, chain_id FROM pyramid_chain_assignments ORDER BY slug",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

// ── Tier 2: content-type defaults (operational helpers) ──────────────────────

/// Replace the entire `pyramid_chain_defaults` table with the mappings from a
/// `chain_defaults` contribution. Atomic: deletes all existing rows then
/// inserts the new set.
pub fn upsert_chain_defaults(
    conn: &Connection,
    mappings: &[ChainDefaultMapping],
    contribution_id: &str,
) -> Result<()> {
    conn.execute("DELETE FROM pyramid_chain_defaults", [])?;
    let mut stmt = conn.prepare(
        "INSERT INTO pyramid_chain_defaults (content_type, evidence_mode, chain_id, contribution_id)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for m in mappings {
        let evidence_mode = if m.evidence_mode.is_empty() { "*" } else { &m.evidence_mode };
        stmt.execute(rusqlite::params![m.content_type, evidence_mode, m.chain_id, contribution_id])?;
    }
    Ok(())
}

/// Look up the best-matching chain default for a (content_type, evidence_mode)
/// pair. Specificity ordering: exact match > content-type-only > global wildcard.
fn get_chain_default(
    conn: &Connection,
    content_type: &str,
    evidence_mode: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT chain_id FROM pyramid_chain_defaults
         WHERE content_type IN (?1, '*')
           AND evidence_mode IN (?2, '*')
         ORDER BY (content_type != '*') DESC,
                  (evidence_mode != '*') DESC
         LIMIT 1",
    )?;
    let result = stmt.query_row(rusqlite::params![content_type, evidence_mode], |row| {
        row.get::<_, String>(0)
    });
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── Consolidated resolver ────────────────────────────────────────────────────

/// Three-tier chain resolution. All tiers are contribution-driven:
///
///   1. Per-slug override (`pyramid_chain_assignments`, from `chain_assignment` contribution)
///   2. Content-type default (`pyramid_chain_defaults`, from `chain_defaults` contribution)
///   3. Compile-time safety net (`"question-pipeline"`, should never be reached
///      once the bundled `chain_defaults` contribution has bootstrapped)
///
/// This is the **only** function build paths should call. It replaces the
/// prior pattern of `get_assignment()` + `default_chain_id()` scattered
/// across 4 call sites (one of which — `run_decomposed_build` — was missing
/// the tier 1 check entirely).
pub fn resolve_chain_for_slug(
    conn: &Connection,
    slug: &str,
    content_type: &str,
    evidence_mode: &str,
) -> Result<String> {
    // Tier 1: per-slug override
    if let Some(chain_id) = get_assignment(conn, slug)? {
        info!(
            slug,
            chain_id = %chain_id,
            tier = "per-slug override",
            "chain resolved"
        );
        return Ok(chain_id);
    }

    // Tier 2: content-type + evidence_mode default (from chain_defaults contribution)
    if let Some(chain_id) = get_chain_default(conn, content_type, evidence_mode)? {
        info!(
            slug,
            chain_id = %chain_id,
            content_type,
            evidence_mode,
            tier = "content-type default",
            "chain resolved"
        );
        return Ok(chain_id);
    }

    // Tier 3: compile-time safety net. If we reach here, the bundled
    // chain_defaults contribution hasn't been bootstrapped yet (first
    // run before migration completes, or corrupted DB). Use the hardcoded
    // mapping as a last resort.
    let fallback = hardcoded_fallback(content_type, evidence_mode);
    warn!(
        slug,
        chain_id = fallback,
        content_type,
        evidence_mode,
        "chain resolved via compile-time fallback — chain_defaults contribution may not be bootstrapped"
    );
    Ok(fallback.to_string())
}

/// Compile-time fallback mapping. Only reached when the `chain_defaults`
/// contribution hasn't been bootstrapped yet. Mirrors the bundled
/// `chain_defaults` contribution body so behavior is identical.
///
/// This function is intentionally NOT public — all external callers should
/// use `resolve_chain_for_slug`. The old public functions `default_chain_id`
/// and `default_chain_id_for_mode` are removed; any call site that was using
/// them should be updated to call `resolve_chain_for_slug` instead.
fn hardcoded_fallback(content_type: &str, evidence_mode: &str) -> &'static str {
    match (content_type, evidence_mode) {
        ("conversation", "fast") => "conversation-episodic-fast",
        ("conversation", _) => "conversation-episodic",
        ("vine", _) => "topical-vine",
        _ => "question-pipeline",
    }
}
