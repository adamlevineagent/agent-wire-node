// pyramid/role_binding.rs — Per-pyramid role→handler-chain bindings.
//
// Post-build accretion v5: the substrate's new system-level roles (judge,
// reconciler, debate_steward, meta_layer_oracle, synthesizer, gap_dispatcher,
// sweep, accretion_handler, authorize_question, cascade_handler) are bound to
// specific handler chains per-pyramid via this table. Operators can supersede
// any binding to swap the chain that fires on the role's events.
//
// CRITICAL: this table is ONLY for NEW roles. Existing dispatch in
// `dadbear_compiler` / `stale_engine` / `build_runner` stays hardcoded.
//
// Genesis bindings ship with starter chain IDs. New slugs get them
// synchronously in `db::create_slug` via `initialize_genesis_bindings`.
// Existing slugs are backfilled at startup (Phase 8 WS8-E).
//
// Resolution semantics (v5 binding decision 9): unresolved binding RAISES.
// `resolve_binding` returns a typed error; callers should NOT silently skip.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::types::RoleBinding;

/// Genesis bindings — role name → starter chain id. Inserted on every new
/// slug created via `db::create_slug`. `cascade_handler` is seeded separately
/// by `db::create_slug` itself because its value depends on whether the slug
/// is being created fresh (judge-gated) or backfilled for pre-existing rows
/// (immediate-redistill).
pub const GENESIS_BINDINGS: &[(&str, &str)] = &[
    ("accretion_handler", "starter-accretion-handler"),
    ("reconciler", "starter-reconciler"),
    ("evidence_tester", "starter-evidence-tester"),
    ("judge", "starter-judge"),
    ("debate_steward", "starter-debate-steward"),
    ("meta_layer_oracle", "starter-meta-layer-oracle"),
    ("synthesizer", "starter-synthesizer"),
    ("gap_dispatcher", "starter-gap-dispatcher"),
    ("sweep", "starter-sweep"),
    ("authorize_question", "starter-authorize-question"),
];

/// Cascade handler default for NEW pyramids (binding decision 1).
pub const CASCADE_HANDLER_NEW_DEFAULT: &str = "starter-cascade-judge-gated";

/// Cascade handler default for EXISTING pyramids backfilled at upgrade
/// (binding decision 10). Preserves pre-upgrade cascade intent while the
/// new primitive `annotation_ancestor_redistill` fixes the pre-existing
/// re_distill dead-dispatch bug.
pub const CASCADE_HANDLER_EXISTING_DEFAULT: &str = "starter-cascade-immediate-redistill";

/// Error indicating a role has no active binding for the given slug.
/// Production callers should chronicle + escalate, not silently skip.
#[derive(Debug, thiserror::Error)]
#[error("no active binding for role '{role_name}' on slug '{slug}'")]
pub struct UnresolvedBinding {
    pub slug: String,
    pub role_name: String,
}

/// Load the active binding for (slug, role_name) in the default `pyramid`
/// scope. Raises `UnresolvedBinding` if none exists — callers must treat
/// this as a loud failure per v5 binding decision 9.
pub fn resolve_binding(
    conn: &Connection,
    slug: &str,
    role_name: &str,
) -> Result<RoleBinding> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, role_name, handler_chain_id, scope, created_at, superseded_by
           FROM pyramid_role_bindings
          WHERE slug = ?1 AND role_name = ?2 AND scope = 'pyramid' AND superseded_by IS NULL
          LIMIT 1",
    )?;
    let result = stmt.query_row(rusqlite::params![slug, role_name], |row| {
        Ok(RoleBinding {
            id: row.get(0)?,
            slug: row.get(1)?,
            role_name: row.get(2)?,
            handler_chain_id: row.get(3)?,
            scope: row.get(4)?,
            created_at: row.get(5)?,
            superseded_by: row.get(6)?,
        })
    });
    match result {
        Ok(b) => Ok(b),
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(anyhow::Error::new(UnresolvedBinding {
            slug: slug.to_string(),
            role_name: role_name.to_string(),
        })),
        Err(e) => Err(e).with_context(|| {
            format!("Failed to resolve binding '{role_name}' for slug '{slug}'")
        }),
    }
}

/// Set (or supersede) a binding. If an active binding already exists for
/// (slug, role_name, 'pyramid'), it is superseded by the new one.
///
/// Uses the self-reference-then-fixup supersession dance to work around the
/// partial UNIQUE index `WHERE superseded_by IS NULL` — see `purpose.rs`
/// `supersede_purpose` for the same pattern.
pub fn set_binding(
    conn: &Connection,
    slug: &str,
    role_name: &str,
    handler_chain_id: &str,
) -> Result<RoleBinding> {
    // Find active binding id if present
    let prior_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM pyramid_role_bindings
              WHERE slug = ?1 AND role_name = ?2 AND scope = 'pyramid' AND superseded_by IS NULL",
            rusqlite::params![slug, role_name],
            |row| row.get(0),
        )
        .ok();

    // Step 1: park prior row outside the active partial index via self-ref
    if let Some(pid) = prior_id {
        conn.execute(
            "UPDATE pyramid_role_bindings SET superseded_by = id WHERE id = ?1",
            rusqlite::params![pid],
        )
        .with_context(|| {
            format!("Failed to park prior binding (self-ref) '{role_name}' '{slug}'")
        })?;
    }

    // Step 2: INSERT new active row
    conn.execute(
        "INSERT INTO pyramid_role_bindings
            (slug, role_name, handler_chain_id, scope)
         VALUES (?1, ?2, ?3, 'pyramid')",
        rusqlite::params![slug, role_name, handler_chain_id],
    )
    .with_context(|| format!("Failed to insert binding '{role_name}' for slug '{slug}'"))?;
    let new_id = conn.last_insert_rowid();

    // Step 3: redirect prior row's pointer from self to successor
    if let Some(pid) = prior_id {
        conn.execute(
            "UPDATE pyramid_role_bindings SET superseded_by = ?1 WHERE id = ?2",
            rusqlite::params![new_id, pid],
        )
        .with_context(|| {
            format!("Failed to mark prior binding superseded '{role_name}' '{slug}'")
        })?;
    }

    resolve_binding(conn, slug, role_name)
}

/// Idempotent helper — insert a binding only if no active one exists. Used
/// by genesis initialization and backfill paths where "leave existing
/// operator override intact" is the correct behavior.
pub fn set_binding_ignore_existing(
    conn: &Connection,
    slug: &str,
    role_name: &str,
    handler_chain_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pyramid_role_bindings
            (slug, role_name, handler_chain_id, scope)
         VALUES (?1, ?2, ?3, 'pyramid')",
        rusqlite::params![slug, role_name, handler_chain_id],
    )
    .with_context(|| {
        format!("Failed to ignore-insert binding '{role_name}' for slug '{slug}'")
    })?;
    Ok(())
}

/// List all active bindings for a slug.
pub fn list_bindings(conn: &Connection, slug: &str) -> Result<Vec<RoleBinding>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, role_name, handler_chain_id, scope, created_at, superseded_by
           FROM pyramid_role_bindings
          WHERE slug = ?1 AND superseded_by IS NULL
          ORDER BY role_name",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![slug], |row| {
            Ok(RoleBinding {
                id: row.get(0)?,
                slug: row.get(1)?,
                role_name: row.get(2)?,
                handler_chain_id: row.get(3)?,
                scope: row.get(4)?,
                created_at: row.get(5)?,
                superseded_by: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Populate genesis bindings for a freshly-created slug. Idempotent.
/// Does NOT set `cascade_handler` — `db::create_slug` sets that separately
/// using `CASCADE_HANDLER_NEW_DEFAULT` because the new-vs-backfilled
/// distinction is known at the call site.
pub fn initialize_genesis_bindings(conn: &Connection, slug: &str) -> Result<()> {
    for (role, chain) in GENESIS_BINDINGS {
        set_binding_ignore_existing(conn, slug, role, chain)?;
    }
    Ok(())
}

/// Backfill `cascade_handler` binding for pre-existing slugs at upgrade.
/// Idempotent via `INSERT OR IGNORE`. Every slug without an active cascade
/// binding gets `CASCADE_HANDLER_EXISTING_DEFAULT`. Slugs that already have
/// a binding (either from the new-slug path or a prior backfill) are left
/// alone.
pub fn backfill_existing_cascade_handlers(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT slug FROM pyramid_slugs
          WHERE slug NOT IN (
              SELECT DISTINCT slug FROM pyramid_role_bindings
               WHERE role_name = 'cascade_handler'
                 AND scope = 'pyramid'
                 AND superseded_by IS NULL
          )",
    )?;
    let slugs: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut count = 0usize;
    for s in &slugs {
        set_binding_ignore_existing(conn, s, "cascade_handler", CASCADE_HANDLER_EXISTING_DEFAULT)?;
        count += 1;
    }
    Ok(count)
}

/// Also backfill all genesis bindings for any pre-existing slug missing
/// them. Separate from the cascade backfill so an operator's bespoke
/// cascade choice isn't disturbed — but the other genesis roles do need
/// to exist before their events can dispatch.
pub fn backfill_genesis_bindings(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT slug FROM pyramid_slugs")?;
    let slugs: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut count = 0usize;
    for s in &slugs {
        initialize_genesis_bindings(conn, s)?;
        count += 1;
    }
    Ok(count)
}
