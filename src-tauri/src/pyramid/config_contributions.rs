// pyramid/config_contributions.rs — Phase 4: Config Contribution Foundation.
//
// Per `docs/specs/config-contribution-and-wire-sharing.md`. Every
// behavioral configuration in Wire Node flows through
// `pyramid_config_contributions` as its source of truth: initial
// creation, supersession (with a required note), agent proposals,
// accept/reject, and rollback. Operational tables
// (`pyramid_dadbear_config`, `pyramid_tier_routing`,
// `pyramid_evidence_policy`, etc.) remain as runtime caches — fast
// lookup for the executor's hot path, populated by
// `sync_config_to_operational()` whenever a contribution activates.
//
// Phase 4 scope: schema, CRUD, dispatcher (with stubs for future
// phases), migration (in `db.rs::migrate_legacy_dadbear_to_contributions`),
// and IPC endpoints (registered in `main.rs`). JSON Schema validation
// is stubbed — Phase 9 provides schema definitions. `WireNativeMetadata`
// canonical validation is stubbed — Phase 5 introduces the struct.
//
// Architectural lens: every config change is a contribution, so when a
// future phase wants to share a config to the Wire, change DADBEAR
// policy, or let an agent propose a build strategy, the underlying
// mechanism is the same row write against this table.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::pyramid::chain_registry;
use crate::pyramid::db;
use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use crate::pyramid::provider::ProviderRegistry;
use crate::pyramid::schema_registry::{flag_configs_needing_migration, SchemaRegistry};
use crate::pyramid::wire_native_metadata::{default_wire_native_metadata, WireNativeMetadata};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single config contribution row. Mirrors the schema defined in
/// `db.rs::init_pyramid_db`. Used by CRUD helpers and the dispatcher.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigContribution {
    pub id: i64,
    pub contribution_id: String,
    pub slug: Option<String>,
    pub schema_type: String,
    pub yaml_content: String,
    pub wire_native_metadata_json: String,
    pub wire_publication_state_json: String,
    pub supersedes_id: Option<String>,
    pub superseded_by_id: Option<String>,
    pub triggering_note: Option<String>,
    /// One of "active", "proposed", "rejected", "superseded".
    pub status: String,
    /// One of "local", "wire", "agent", "bundled", "migration".
    pub source: String,
    pub wire_contribution_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub accepted_at: Option<String>,
}

/// Error returned by `sync_config_to_operational()` and its callees.
/// Each variant maps to a specific dispatcher failure mode.
#[derive(Debug, thiserror::Error)]
pub enum ConfigSyncError {
    /// JSON Schema validation failure (Phase 9 provides the schemas;
    /// Phase 4 stubs validation to `Ok(())`).
    #[error("validation failed: {0}")]
    ValidationFailed(String),
    /// `schema_type` isn't one of the known vocabulary entries. Per
    /// the spec: "Unknown types are a bug — schema registry should
    /// only emit known types. Fail loudly rather than silently
    /// skipping sync."
    #[error("unknown schema type: {0}")]
    UnknownSchemaType(String),
    /// YAML deserialization failure inside a specific dispatcher
    /// branch.
    #[error("yaml deserialize error: {0}")]
    SerdeError(#[from] serde_yaml::Error),
    /// Underlying SQLite error from a CRUD helper or the upsert
    /// helpers in `db.rs`.
    #[error("db error: {0}")]
    DbError(#[from] rusqlite::Error),
    /// Catch-all for anyhow errors bubbling up from helper layers.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result of a note validation — the IPC layer rejects empty/whitespace
/// notes before any DB work.
pub fn validate_note(note: &str) -> Result<(), String> {
    if note.trim().is_empty() {
        return Err("note must not be empty or whitespace-only".to_string());
    }
    Ok(())
}

/// Phase 5 (Config History + Rollback): lightweight history entry
/// returned by `load_config_history`. Contains only the fields the
/// frontend timeline needs — no wire_native_metadata, no publication
/// state. Avoids deserializing the full `ConfigContribution` for each
/// row in the history list.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigHistoryEntry {
    pub contribution_id: String,
    pub yaml_content: String,
    pub triggering_note: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub superseded_by_id: Option<String>,
    pub is_active: bool,
}

// ── CRUD helpers ──────────────────────────────────────────────────────────────

/// Parse a row from `pyramid_config_contributions` into a
/// `ConfigContribution`. Columns must match the SELECT list used by
/// the CRUD queries below.
fn contribution_from_row(row: &rusqlite::Row) -> rusqlite::Result<ConfigContribution> {
    Ok(ConfigContribution {
        id: row.get("id")?,
        contribution_id: row.get("contribution_id")?,
        slug: row.get("slug")?,
        schema_type: row.get("schema_type")?,
        yaml_content: row.get("yaml_content")?,
        wire_native_metadata_json: row.get("wire_native_metadata_json")?,
        wire_publication_state_json: row.get("wire_publication_state_json")?,
        supersedes_id: row.get("supersedes_id")?,
        superseded_by_id: row.get("superseded_by_id")?,
        triggering_note: row.get("triggering_note")?,
        status: row.get("status")?,
        source: row.get("source")?,
        wire_contribution_id: row.get("wire_contribution_id")?,
        created_by: row.get("created_by")?,
        created_at: row.get("created_at")?,
        accepted_at: row.get("accepted_at")?,
    })
}

const CONTRIBUTION_SELECT: &str =
    "SELECT id, contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by,
            created_at, accepted_at
     FROM pyramid_config_contributions";

/// Create a new config contribution row. Returns the generated
/// contribution_id (UUID v4).
///
/// **Phase 5 behavior**: the `wire_native_metadata_json` column is
/// initialized from `default_wire_native_metadata(schema_type, slug)`
/// instead of `'{}'`. Every row therefore lands with a canonical,
/// schema-type-appropriate metadata stub (draft maturity, unscoped
/// scope, review sync mode, topic tags from the mapping table) that
/// publish IPC and ToolsMode can render immediately.
///
/// Caller is responsible for picking the right `status`: the standard
/// path is `'active'` for direct user-created configs and `'proposed'`
/// for agent proposals. `source` is one of the canonical vocabulary
/// values (`local`, `agent`, `wire`, `bundled`, `migration`).
///
/// For callers that need to supply explicit metadata (migration from
/// disk, bundled seeds, Wire pulls), use
/// `create_config_contribution_with_metadata()` directly.
pub fn create_config_contribution(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
    yaml_content: &str,
    triggering_note: Option<&str>,
    source: &str,
    created_by: Option<&str>,
    status: &str,
) -> Result<String> {
    let metadata = default_wire_native_metadata(schema_type, slug);
    create_config_contribution_with_metadata(
        conn,
        schema_type,
        slug,
        yaml_content,
        triggering_note,
        source,
        created_by,
        status,
        &metadata,
    )
}

/// Create a new config contribution row with explicit canonical
/// metadata. Used by the migration path, bundled seeds, and Wire
/// pulls where the caller has richer metadata than the default
/// mapping produces. Returns the generated contribution_id.
///
/// Callers that don't care about metadata should use
/// `create_config_contribution()` which applies the Phase 5 default
/// automatically.
pub fn create_config_contribution_with_metadata(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
    yaml_content: &str,
    triggering_note: Option<&str>,
    source: &str,
    created_by: Option<&str>,
    status: &str,
    metadata: &WireNativeMetadata,
) -> Result<String> {
    let contribution_id = uuid::Uuid::new_v4().to_string();
    let metadata_json = metadata
        .to_json()
        .map_err(|e| anyhow::anyhow!("failed to serialize wire_native_metadata: {e}"))?;
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at
         ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, '{}',
            NULL, NULL, ?6,
            ?7, ?8, NULL, ?9,
            CASE WHEN ?7 = 'active' THEN datetime('now') ELSE NULL END
         )",
        rusqlite::params![
            contribution_id,
            slug,
            schema_type,
            yaml_content,
            metadata_json,
            triggering_note,
            status,
            source,
            created_by,
        ],
    )?;
    Ok(contribution_id)
}

/// Supersede a prior active contribution: mark the prior as
/// `superseded` and insert a new `active` contribution that
/// `supersedes_id` → prior. Atomic via a transaction. Returns the new
/// contribution_id.
///
/// Preconditions:
/// - `triggering_note` must be non-empty and non-whitespace. The IPC
///   layer validates this before calling; the function re-validates
///   defensively.
/// - The prior contribution must exist. Its schema_type + slug are
///   inherited by the new contribution (supersession can only replace
///   like-with-like).
pub fn supersede_config_contribution(
    conn: &mut Connection,
    prior_contribution_id: &str,
    new_yaml_content: &str,
    triggering_note: &str,
    source: &str,
    created_by: Option<&str>,
) -> Result<String> {
    if triggering_note.trim().is_empty() {
        anyhow::bail!("triggering_note must not be empty or whitespace-only");
    }

    let tx = conn.transaction()?;

    // Load the prior contribution to inherit schema_type + slug +
    // canonical metadata + publication state. Phase 5: metadata is
    // carried forward with `maturity` reset to Draft per the spec's
    // "Auto-population on refinement" rules, and `supersedes` is set
    // to the prior version's handle-path if it was Wire-published.
    let prior: Option<(String, Option<String>, String, String, String)> = tx
        .query_row(
            "SELECT schema_type, slug, status, wire_native_metadata_json, wire_publication_state_json
             FROM pyramid_config_contributions
             WHERE contribution_id = ?1",
            rusqlite::params![prior_contribution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;

    let (schema_type, slug, prior_status, prior_metadata_json, prior_pub_state_json) = prior
        .ok_or_else(|| anyhow::anyhow!("prior contribution {prior_contribution_id} not found"))?;

    if prior_status == "superseded" {
        anyhow::bail!(
            "prior contribution {prior_contribution_id} is already superseded — cannot supersede a non-active version"
        );
    }

    // Carry forward the prior's canonical metadata with maturity reset
    // to Draft (re-review needed) and `supersedes` set to the prior
    // version's handle-path if it was Wire-published. Falls back to
    // the default metadata if the prior row has an empty/invalid JSON
    // blob (Phase 4 stored `'{}'`).
    let mut new_metadata = WireNativeMetadata::from_json(&prior_metadata_json).unwrap_or_else(|_| {
        default_wire_native_metadata(&schema_type, slug.as_deref())
    });
    new_metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Draft;

    // If the prior was Wire-published, point `supersedes` at its
    // handle-path so publishing the new version creates the next
    // entry in the Wire supersession chain. Publication state is
    // stored separately from metadata per the spec.
    if let Ok(prior_pub_state) =
        serde_json::from_str::<crate::pyramid::wire_native_metadata::WirePublicationState>(
            &prior_pub_state_json,
        )
    {
        if let Some(handle_path) = prior_pub_state.handle_path {
            new_metadata.supersedes = Some(handle_path);
        }
    }

    let metadata_json = new_metadata
        .to_json()
        .map_err(|e| anyhow::anyhow!("failed to serialize wire_native_metadata: {e}"))?;

    // Insert the new active contribution first (so we have its
    // contribution_id to write back into the prior row).
    let new_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at
         ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, '{}',
            ?6, NULL, ?7,
            'active', ?8, NULL, ?9, datetime('now')
         )",
        rusqlite::params![
            new_id,
            slug,
            schema_type,
            new_yaml_content,
            metadata_json,
            prior_contribution_id,
            triggering_note,
            source,
            created_by,
        ],
    )?;

    // Mark the prior row as superseded and link forward.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'superseded', superseded_by_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![new_id, prior_contribution_id],
    )?;

    tx.commit()?;
    Ok(new_id)
}

/// Load the active config contribution for a given (schema_type, slug).
/// Returns `None` if no active contribution exists yet.
///
/// For global configs (e.g. `tier_routing`), pass `slug = None`.
pub fn load_active_config_contribution(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Option<ConfigContribution>> {
    // SQLite's `=` operator returns NULL when either side is NULL, so
    // we branch on the slug to use the right comparison operator.
    let row = if let Some(slug_val) = slug {
        let sql = format!(
            "{CONTRIBUTION_SELECT}
             WHERE slug = ?1 AND schema_type = ?2
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1"
        );
        conn.query_row(
            &sql,
            rusqlite::params![slug_val, schema_type],
            contribution_from_row,
        )
        .optional()?
    } else {
        let sql = format!(
            "{CONTRIBUTION_SELECT}
             WHERE slug IS NULL AND schema_type = ?1
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1"
        );
        conn.query_row(
            &sql,
            rusqlite::params![schema_type],
            contribution_from_row,
        )
        .optional()?
    };
    Ok(row)
}

/// Load the full version history for a given (schema_type, slug),
/// walking the supersedes chain from the active version backward.
/// Returns oldest-to-newest. Includes the active row and every
/// superseded ancestor.
pub fn load_config_version_history(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Vec<ConfigContribution>> {
    // Walk via the `supersedes_id` chain starting from the active
    // version. We can't use a recursive CTE with the slug branch so
    // we do the walk in Rust.
    let mut chain: Vec<ConfigContribution> = Vec::new();
    let Some(active) = load_active_config_contribution(conn, schema_type, slug)? else {
        return Ok(chain);
    };

    // Active comes last (newest) — push in order we walk backward, then
    // reverse.
    let mut current = active;
    loop {
        let predecessor_id = current.supersedes_id.clone();
        chain.push(current);
        let Some(predecessor_id) = predecessor_id else {
            break;
        };
        let predecessor = load_contribution_by_id(conn, &predecessor_id)?;
        let Some(predecessor) = predecessor else {
            warn!(
                contribution_id = %predecessor_id,
                "supersedes_id chain pointed at a missing contribution — breaking walk"
            );
            break;
        };
        current = predecessor;
    }

    chain.reverse();
    Ok(chain)
}

/// Phase 5 (Config History + Rollback): load config history via a
/// single SQL query. Returns most-recent-first, capped by `limit`.
///
/// Unlike `load_config_version_history` (which walks the supersedes
/// chain via O(N) individual queries), this is a single indexed
/// query — O(1) regardless of chain length. Used by the
/// `pyramid_get_config_history` IPC.
///
/// Only returns global configs (slug IS NULL). Per-slug history
/// (e.g. per-pyramid dadbear_policy) would need a separate call
/// with a slug parameter; the Phase 5 UI only surfaces global
/// config history (tier_routing, build_strategy).
pub fn load_config_history(
    conn: &Connection,
    schema_type: &str,
    limit: usize,
) -> Result<Vec<ConfigHistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT contribution_id, yaml_content, triggering_note,
                created_by, created_at, superseded_by_id, status
         FROM pyramid_config_contributions
         WHERE schema_type = ?1 AND slug IS NULL
           AND status IN ('active', 'superseded')
         ORDER BY created_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![schema_type, limit as i64], |row| {
        let status: String = row.get(6)?;
        Ok(ConfigHistoryEntry {
            contribution_id: row.get(0)?,
            yaml_content: row.get(1)?,
            triggering_note: row.get(2)?,
            created_by: row.get(3)?,
            created_at: row.get(4)?,
            superseded_by_id: row.get(5)?,
            is_active: status == "active",
        })
    })?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Look up a single contribution by its contribution_id UUID.
pub fn load_contribution_by_id(
    conn: &Connection,
    contribution_id: &str,
) -> Result<Option<ConfigContribution>> {
    let sql = format!("{CONTRIBUTION_SELECT} WHERE contribution_id = ?1");
    let row = conn
        .query_row(
            &sql,
            rusqlite::params![contribution_id],
            contribution_from_row,
        )
        .optional()?;
    Ok(row)
}

/// List contributions in `status = 'proposed'` state, optionally
/// filtered by slug. Used by the agent-proposal review UI.
pub fn list_pending_proposals(
    conn: &Connection,
    slug: Option<&str>,
) -> Result<Vec<ConfigContribution>> {
    let rows = if let Some(slug_val) = slug {
        let sql = format!(
            "{CONTRIBUTION_SELECT}
             WHERE slug = ?1 AND status = 'proposed'
             ORDER BY created_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let iter = stmt.query_map(rusqlite::params![slug_val], contribution_from_row)?;
        let mut out = Vec::new();
        for row in iter {
            out.push(row?);
        }
        out
    } else {
        let sql = format!(
            "{CONTRIBUTION_SELECT}
             WHERE status = 'proposed'
             ORDER BY created_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let iter = stmt.query_map([], contribution_from_row)?;
        let mut out = Vec::new();
        for row in iter {
            out.push(row?);
        }
        out
    };
    Ok(rows)
}

/// Accept a proposed contribution: transition it to `active` and
/// supersede any prior active contribution for the same
/// (slug, schema_type). Atomic via a transaction.
pub fn accept_proposal(conn: &mut Connection, contribution_id: &str) -> Result<()> {
    let tx = conn.transaction()?;

    // Load the proposal.
    let proposal: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT schema_type, slug, status FROM pyramid_config_contributions
             WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let (schema_type, slug, status) = proposal
        .ok_or_else(|| anyhow::anyhow!("contribution {contribution_id} not found"))?;

    if status != "proposed" {
        anyhow::bail!(
            "contribution {contribution_id} is in status `{status}`, not `proposed`"
        );
    }

    // Find the prior active contribution (if any) and supersede it.
    let prior_id: Option<String> = if let Some(ref slug_val) = slug {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug = ?1 AND schema_type = ?2
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![slug_val, schema_type],
            |row| row.get(0),
        )
        .optional()?
    } else {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug IS NULL AND schema_type = ?1
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![schema_type],
            |row| row.get(0),
        )
        .optional()?
    };

    // Promote the proposal to active.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'active',
             accepted_at = datetime('now'),
             supersedes_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![prior_id, contribution_id],
    )?;

    // If there was a prior active contribution, mark it superseded.
    if let Some(prior) = prior_id {
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded', superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![contribution_id, prior],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Reject a proposed contribution: transition it to `rejected` and
/// optionally record a reason in `triggering_note` (appended if one
/// already exists).
pub fn reject_proposal(
    conn: &Connection,
    contribution_id: &str,
    reason: Option<&str>,
) -> Result<()> {
    // Check the current status first so we fail loudly on invalid
    // transitions rather than silently no-oping.
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |row| row.get(0),
        )
        .optional()?;

    let status =
        status.ok_or_else(|| anyhow::anyhow!("contribution {contribution_id} not found"))?;
    if status != "proposed" {
        anyhow::bail!(
            "contribution {contribution_id} is in status `{status}`, not `proposed` — cannot reject"
        );
    }

    if let Some(reason) = reason {
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'rejected',
                 triggering_note = COALESCE(triggering_note || ' | rejection reason: ', 'rejection reason: ') || ?1
             WHERE contribution_id = ?2",
            rusqlite::params![reason, contribution_id],
        )?;
    } else {
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'rejected'
             WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
        )?;
    }
    Ok(())
}

// ── Dispatcher: sync_config_to_operational ────────────────────────────────────

/// The Phase 4 unified dispatcher. Routes a freshly-activated
/// contribution to its operational table + triggers any downstream
/// reload hooks. Emits `TaggedKind::ConfigSynced` on success.
///
/// Per the spec's 14-branch match statement. Phase 4 implements real
/// upserts for the schema types that have operational tables today;
/// the rest call stub helpers that log a TODO and return `Ok(())`,
/// with the phase number wiring each one up inline.
///
/// Step 1 (JSON Schema validation) is stubbed in Phase 4 — Phase 9
/// provides the schema definitions. Today's validation helper just
/// returns `Ok(())`.
///
/// **Phase 9 note:** the legacy entry point `sync_config_to_operational`
/// delegates to `sync_config_to_operational_with_registry` with
/// `schema_registry = None`. Call sites that want the Phase 9 stubs
/// wired up (invalidate_schema_registry_cache +
/// flag_configs_for_migration) should use the `_with_registry`
/// variant and thread the registry Arc through from
/// `PyramidState::schema_registry`.
pub fn sync_config_to_operational(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
) -> Result<(), ConfigSyncError> {
    sync_config_to_operational_with_registry(conn, bus, contribution, None)
}

/// Phase 9 variant of `sync_config_to_operational` that accepts an
/// optional schema registry reference. When provided, the
/// `schema_definition` branch calls the registry's `invalidate`
/// method and the `flag_configs_for_migration` helper (both are
/// Phase 4 stubs that Phase 9 wires up). When `None`, behavior is
/// identical to the legacy entry point — used by tests that don't
/// need the registry side effects.
pub fn sync_config_to_operational_with_registry(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
    schema_registry: Option<&Arc<SchemaRegistry>>,
) -> Result<(), ConfigSyncError> {
    // Step 1: validate against the active schema_definition for this
    // schema_type. Phase 4 stubs this — Phase 9 wires it up.
    validate_yaml_against_schema(&contribution.yaml_content, &contribution.schema_type)?;

    // Resolve the prior active contribution_id for the event payload
    // (best-effort; used purely as diagnostic metadata).
    let prior_id: Option<String> = match load_active_config_contribution(
        conn,
        &contribution.schema_type,
        contribution.slug.as_deref(),
    ) {
        Ok(Some(row)) if row.contribution_id != contribution.contribution_id => {
            Some(row.contribution_id)
        }
        _ => None,
    };

    // Step 2: dispatch by schema_type.
    let slug_opt = contribution.slug.clone();
    match contribution.schema_type.as_str() {
        "dadbear_policy" => {
            let yaml: db::DadbearPolicyYaml = serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_dadbear_policy(conn, &slug_opt, &yaml, &contribution.contribution_id)?;
            trigger_dadbear_reload(bus, contribution.slug.as_deref());
        }
        "evidence_policy" => {
            let yaml: db::EvidencePolicyYaml = serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_evidence_policy(conn, &slug_opt, &yaml, &contribution.contribution_id)?;
            // Phase 11: reevaluate_deferred_questions runs here per
            // evidence-triage-and-dadbear.md. Stub for Phase 4.
            reevaluate_deferred_questions(conn, contribution.slug.as_deref())?;
        }
        "build_strategy" => {
            let yaml: db::BuildStrategyYaml = serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_build_strategy(conn, &slug_opt, &yaml, &contribution.contribution_id)?;
            // No reload hook — read on next build start.
        }
        "tier_routing" => {
            let yaml: db::TierRoutingYaml = serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_tier_routing_from_contribution(conn, &yaml, &contribution.contribution_id)?;
            invalidate_provider_resolver_cache();
        }
        "custom_prompts" => {
            let yaml: db::CustomPromptsYaml = serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_custom_prompts(conn, &slug_opt, &yaml, &contribution.contribution_id)?;
            invalidate_prompt_cache();
        }
        "step_overrides" => {
            let bundle: db::StepOverridesBundleYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            db::replace_step_overrides_bundle(conn, &bundle, &contribution.contribution_id)?;
            invalidate_provider_resolver_cache();
        }
        "folder_ingestion_heuristics" => {
            let yaml: db::FolderIngestionHeuristicsYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_folder_ingestion_heuristics(
                conn,
                &slug_opt,
                &yaml,
                &contribution.contribution_id,
            )?;
            // No reload hook — read on next folder scan.
        }
        "auto_update_policy" => {
            // Ghost-engine fix: per-pyramid stale engine policy.
            // This is NOT `wire_auto_update_settings` (which controls
            // Wire discovery polling) — this governs the local stale
            // engine's per-pyramid file-watching behavior.
            let yaml: db::AutoUpdatePolicyYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            db::upsert_auto_update_policy(conn, &slug_opt, &yaml, &contribution.contribution_id)?;
            // Note: debounce_minutes is baked at engine construction
            // time. Changes take effect on next engine restart (toggle
            // auto_update off/on, or app restart). All other fields
            // (runaway_threshold, min_changed_files, auto_update) are
            // re-read per drain cycle.
        }
        "dispatch_policy" => {
            // LLM dispatch policy: provider pools, routing rules, escalation,
            // build coordination. The operational table stores the YAML; the
            // runtime ProviderPools + DispatchPolicy are rebuilt from it via
            // a ConfigSynced event listener in server.rs.
            db::upsert_dispatch_policy(conn, &slug_opt, &contribution.yaml_content, &contribution.contribution_id)?;
        }
        "chain_assignment" => {
            // Per-pyramid chain override. Syncs to the
            // `pyramid_chain_assignments` operational table via
            // `chain_registry::assign_chain`. The special value
            // `chain_id: "default"` removes the override, causing the
            // build runner to fall through to the tier 2 content-type
            // defaults.
            let yaml: db::ChainAssignmentYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            let slug = contribution.slug.as_deref().ok_or_else(|| {
                ConfigSyncError::ValidationFailed(
                    "chain_assignment requires a pyramid slug".into(),
                )
            })?;
            if yaml.chain_id == "default" {
                chain_registry::remove_assignment(conn, slug)?;
            } else {
                chain_registry::assign_chain(conn, slug, &yaml.chain_id)?;
            }
        }
        "chain_defaults" => {
            // Global content-type → chain_id mapping. Syncs to the
            // `pyramid_chain_defaults` operational table. Ships bundled,
            // updatable via Wire, supersedable locally. Replaces the
            // former hardcoded `default_chain_id()` match statement.
            let yaml: db::ChainDefaultsYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            chain_registry::upsert_chain_defaults(
                conn,
                &yaml.mappings,
                &contribution.contribution_id,
            )?;
        }
        // ── Stubbed branches ─────────────────────────────────────────
        //
        // These schema types don't have operational tables today. The
        // dispatcher recognizes the schema_type so unknown-type errors
        // only fire for truly unknown entries, and calls through to a
        // stub helper that logs a TODO pointing at the phase that wires
        // it up for real.
        "custom_chains" => {
            // Phase 9: sync_custom_chain_to_disk writes the chain YAML
            // + prompt files to disk and registers with the chain
            // registry.
            sync_custom_chain_to_disk(conn, &contribution.contribution_id)?;
            register_chain_with_registry(conn, &contribution.contribution_id)?;
            invalidate_prompt_cache();
        }
        "skill" => {
            // Phase 6: the prompt cache reads skill bodies directly
            // from pyramid_config_contributions.yaml_content. The only
            // sync action is a cache invalidation.
            invalidate_prompt_cache();
        }
        "schema_definition" => {
            // Phase 9: superseding a schema_definition flags downstream
            // configs of the target schema_type for LLM-assisted
            // migration, then invalidates the schema registry so the
            // next resolver call re-reads from the contribution store.
            //
            // The target is the contribution's `slug` field (per the
            // Phase 9 convention: schema_definition rows use `slug =
            // <target_schema_type>`). Falls back to a no-op when the
            // slug is missing.
            let target_type = contribution
                .slug
                .as_deref()
                .unwrap_or(&contribution.schema_type);
            flag_configs_for_migration(conn, target_type)?;
            if let Some(registry) = schema_registry {
                invalidate_schema_registry_cache(conn, registry);
            } else {
                debug!(
                    "schema_definition supersession: no registry passed, skipping invalidate"
                );
            }
        }
        "schema_annotation" => {
            // Phase 8: YAML-to-UI renderer cache invalidation.
            invalidate_schema_annotation_cache();
        }
        "wire_discovery_weights" => {
            // Phase 14: Wire discovery ranking cache invalidation.
            // No operational table — the ranking engine reads the
            // contribution on demand via `load_ranking_weights` which
            // maintains its own 5-minute TTL cache. Clearing the
            // cache on supersession is the only side effect.
            invalidate_wire_discovery_cache();
        }
        "wire_auto_update_settings" => {
            // Phase 14: per-schema_type auto-update toggles. No
            // operational table — the background update poller reads
            // the active contribution on every run via
            // `load_auto_update_settings`. No reload hook needed
            // (the poller picks up new values on its next cycle).
            reconfigure_wire_update_scheduler(conn)?;
        }
        "wire_update_polling" => {
            // Phase 14: polling interval contribution. No operational
            // table — the background update poller reads the active
            // contribution via `load_update_polling_interval` on every
            // iteration of its loop, so a supersession takes effect
            // on the next cycle automatically.
            debug!("wire_update_polling synced; poller will pick up new interval on next cycle");
        }
        "experimental_territory" => {
            // Phase 6 daemon control plane (AD-6): per-dimension
            // lock/experimental territory markers for the future steward.
            // No operational table needed — metadata for future steward.
            // The contribution persists in pyramid_config_contributions
            // and is readable via standard contribution queries.
            debug!("experimental_territory synced; no operational table — metadata only");
        }
        "compute_participation_policy" => {
            // Fleet MPS Phase 1: durable operator intent for how this node
            // participates in private compute. No operational table yet —
            // the desktop app reads this contribution directly via IPC and
            // later phases derive dispatch behavior from it.
            debug!("compute_participation_policy synced; no operational table — read from contribution store");
        }
        "pyramid_viz_config" => {
            // Pyramid visualization engine configuration.
            // No operational table needed — the viz engine reads the
            // active contribution directly via viz_config::get_pyramid_viz_config.
            debug!("pyramid_viz_config synced; no operational table — read from contribution store");
        }
        "reconciliation_result" => {
            // Post-evidence-loop reconciliation summary (orphans, central
            // nodes, weight map, gaps). Persisted per-build by the chain
            // executor. No operational table — queryable from the
            // contribution store directly.
            debug!("reconciliation_result synced; no operational table — queryable from contribution store");
        }
        // ── DADBEAR Canonical Architecture: split contribution types ───��─
        //
        // Phase 0 of `docs/plans/dadbear-canonical-state-model.md` splits
        // `dadbear_policy` into `watch_root` + `dadbear_norms`. These new
        // dispatcher branches coexist with the old `dadbear_policy` branch
        // (kept for rollback safety — removed in Phase 7).
        "watch_root" => {
            let yaml: db::WatchRootYaml =
                serde_yaml::from_str(&contribution.yaml_content)?;
            // Resolve norms for this slug via the layered resolver.
            // resolve_dadbear_norms returns db::DadbearNormsYaml directly.
            let resolved_norms = resolve_dadbear_norms(conn, contribution.slug.as_deref())
                .unwrap_or_default();
            db::upsert_watch_root(
                conn,
                &slug_opt,
                &yaml,
                &resolved_norms,
                &contribution.contribution_id,
            )?;
            trigger_dadbear_config_changed(bus, contribution.slug.as_deref());
        }
        "dadbear_norms" => {
            // When a norms contribution lands, rebuild the operational
            // norms for all affected slugs. For a global norms (slug=None):
            // rebuild all slugs. For a per-slug norms: just that slug.
            if contribution.slug.is_none() {
                // Global norms changed — all slugs affected.
                rebuild_all_dadbear_norms_cache(conn, bus)?;
            } else if let Some(slug_str) = contribution.slug.as_deref() {
                // Per-slug norms changed — rebuild just this slug's rows.
                let norms = resolve_dadbear_norms(conn, Some(slug_str))
                    .unwrap_or_default();
                // Update all dadbear_config rows for this slug with new norms.
                conn.execute(
                    "UPDATE pyramid_dadbear_config SET
                        scan_interval_secs = ?1,
                        debounce_secs = ?2,
                        session_timeout_secs = ?3,
                        batch_size = ?4,
                        updated_at = datetime('now')
                     WHERE slug = ?5",
                    rusqlite::params![
                        norms.scan_interval_secs,
                        norms.debounce_secs,
                        norms.session_timeout_secs,
                        norms.batch_size,
                        slug_str,
                    ],
                )?;
            }
            trigger_dadbear_config_changed(bus, contribution.slug.as_deref());
        }
        other => {
            // Per the spec: unknown types are a bug — fail loudly
            // rather than silently skipping sync.
            return Err(ConfigSyncError::UnknownSchemaType(other.to_string()));
        }
    }

    // Step 3: emit ConfigSynced event. Use empty-string slug on the
    // outer envelope for global configs — the broadcast bus envelope
    // requires a concrete String.
    let envelope_slug = contribution.slug.clone().unwrap_or_default();
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: envelope_slug,
        kind: TaggedKind::ConfigSynced {
            slug: contribution.slug.clone(),
            schema_type: contribution.schema_type.clone(),
            contribution_id: contribution.contribution_id.clone(),
            prior_contribution_id: prior_id,
        },
    });

    Ok(())
}

// ── Validation stub ───────────────────────────────────────────────────────────

/// Phase 4: stubbed JSON Schema validation. Phase 9 provides the
/// schema definitions via `schema_definition` contributions; this
/// helper will look them up and delegate to a real validator.
///
/// TODO(Phase 9): implement JSON Schema validation against the active
/// `schema_definition` contribution for this `schema_type`.
fn validate_yaml_against_schema(
    _yaml_content: &str,
    schema_type: &str,
) -> Result<(), ConfigSyncError> {
    debug!(
        schema_type,
        "validate_yaml_against_schema: Phase 4 stub (Phase 9 will implement)"
    );
    Ok(())
}

// ── Reload / invalidation stubs ───────────────────────────────────────────────
//
// Phase 4 stubs. Each stub logs a TODO at debug level and returns
// Ok(()). Future phases replace the stub body with the real
// implementation.

/// Phase 5 wiring: invalidate the global prompt cache so the next
/// LLM call re-reads prompts from pyramid_config_contributions. The
/// cache is backed by `pyramid::prompt_cache::PromptCache` which was
/// introduced in Phase 5 alongside the on-disk → contributions
/// migration. Coarse-grained: the entire map is cleared; next read
/// re-faults on demand.
fn invalidate_prompt_cache() {
    debug!("invalidate_prompt_cache: clearing global prompt cache (Phase 5)");
    crate::pyramid::prompt_cache::invalidate_global_prompt_cache();
}

/// Phase 3 already has a provider registry. The cache invalidation
/// hook isn't wired yet — Phase 6's LLM cache layer re-reads the
/// registry on the next call, which serves the same purpose. Phase 9
/// may add a push-based invalidation signal if needed.
fn invalidate_provider_resolver_cache() {
    debug!(
        "invalidate_provider_resolver_cache: Phase 4 stub (Phase 6/9 may add push invalidation)"
    );
}

/// Phase 9: schema definition supersession flags every downstream
/// config of the target schema_type for LLM-assisted migration.
///
/// Delegates to `schema_registry::flag_configs_needing_migration`,
/// which sets `needs_migration = 1` on every active contribution
/// whose `schema_type` matches the superseded schema_definition's
/// target. ToolsMode reads this flag to surface a "Migrate" button
/// (Phase 10 wires the actual LLM-assisted migration flow).
fn flag_configs_for_migration(conn: &Connection, target_schema_type: &str) -> Result<()> {
    let flagged = flag_configs_needing_migration(conn, target_schema_type)
        .map_err(|e| anyhow::anyhow!("flag_configs_for_migration failed: {e}"))?;
    debug!(
        target_schema_type,
        rows_flagged = flagged,
        "flag_configs_for_migration: marked downstream configs needing migration"
    );
    Ok(())
}

/// Phase 9: invalidate the cached schema registry so the next
/// resolver call re-reads from pyramid_config_contributions. Called
/// from the dispatcher's `schema_definition` branch after a
/// supersession lands.
fn invalidate_schema_registry_cache(conn: &Connection, registry: &Arc<SchemaRegistry>) {
    if let Err(e) = registry.invalidate(conn) {
        warn!(
            error = %e,
            "invalidate_schema_registry_cache: registry re-hydration failed"
        );
    } else {
        debug!("invalidate_schema_registry_cache: registry re-hydrated");
    }
}

/// Phase 8: invalidate the YAML-to-UI renderer cache.
fn invalidate_schema_annotation_cache() {
    debug!("invalidate_schema_annotation_cache: Phase 4 stub (Phase 8 wires this up)");
}

/// Phase 14: invalidate the Wire discovery ranking cache.
///
/// Clears the in-memory 5-minute TTL cache held by
/// `wire_discovery::WEIGHTS_CACHE`. The next discovery call re-reads
/// the active `wire_discovery_weights` contribution from the DB and
/// re-populates the cache with the updated weights.
fn invalidate_wire_discovery_cache() {
    crate::pyramid::wire_discovery::invalidate_weights_cache();
    debug!("invalidate_wire_discovery_cache: weights cache cleared");
}

/// Phase 14: reconfigure the Wire update scheduler after
/// `wire_auto_update_settings` changes.
///
/// The Wire update poller re-reads the auto-update settings
/// contribution on every cycle via `load_auto_update_settings`, so a
/// supersession automatically takes effect on the next cycle. No
/// explicit reload is required — this function exists as a hook point
/// for future phases that may add a push-based reconfig signal.
fn reconfigure_wire_update_scheduler(_conn: &Connection) -> Result<()> {
    debug!(
        "reconfigure_wire_update_scheduler: poller will re-read settings on next cycle"
    );
    Ok(())
}

/// Phase 0: after a DADBEAR config contribution updates, emit a
/// `DadbearConfigChanged` event so the tick loop forces an immediate
/// reload on the next cycle. Replaces the former no-op stub.
fn trigger_dadbear_reload(bus: &Arc<BuildEventBus>, slug: Option<&str>) {
    debug!(
        slug = ?slug,
        "trigger_dadbear_reload: emitting DadbearConfigChanged event"
    );
    let envelope_slug = slug.unwrap_or("").to_string();
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: envelope_slug,
        kind: TaggedKind::DadbearConfigChanged {
            slug: slug.map(|s| s.to_string()),
            schema_type: "dadbear_policy".to_string(),
            contribution_id: String::new(),
        },
    });
}

// ── DADBEAR Canonical Architecture: Phase 0 helpers ─────────────────────────

/// Emit a `DadbearConfigChanged` event for the new split contribution
/// types (`watch_root`, `dadbear_norms`). Same pattern as
/// `trigger_dadbear_reload` but with the correct schema_type.
fn trigger_dadbear_config_changed(bus: &Arc<BuildEventBus>, slug: Option<&str>) {
    debug!(
        slug = ?slug,
        "trigger_dadbear_config_changed: emitting DadbearConfigChanged event"
    );
    let envelope_slug = slug.unwrap_or("").to_string();
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: envelope_slug,
        kind: TaggedKind::DadbearConfigChanged {
            slug: slug.map(|s| s.to_string()),
            schema_type: "dadbear_norms".to_string(),
            contribution_id: String::new(),
        },
    });
}

/// Phase 12: re-evaluate deferred questions after an evidence_policy
/// contribution lands. See `evidence-triage-and-dadbear.md` Part 2
/// §Re-evaluation on Policy Change.
///
/// For each deferred question whose `slug` matches (or any slug if
/// `slug` is `None`), re-run the triage DSL against the new policy:
///  * Answer  → remove_deferred + log (caller doesn't get a pending
///    marker in Phase 12 — the next build picks it up naturally).
///  * Defer   → update_deferred_next_check with the new interval.
///  * Skip    → remove_deferred.
///
/// The full flow is synchronous because it's called inside the
/// `sync_config_to_operational` DB write window. LLM classification
/// is deliberately NOT run here — triage rules that depend on
/// `evidence_question_trivial` / `evidence_question_high_value` will
/// evaluate those as false, which is the safe fallback (rules that
/// match `high_value` as true will not match; rules matching
/// `trivial` as true will not match either; default-answer fallback
/// applies).
fn reevaluate_deferred_questions(conn: &Connection, slug: Option<&str>) -> Result<()> {
    // Phase 12 wanderer fix: a global evidence_policy supersession
    // (contribution with `slug = NULL`) previously fell through
    // `list_all_deferred(conn, "")` and never matched any rows — the
    // global-policy re-evaluation path was silently dead. Walk every
    // distinct slug with deferred questions in that case, and
    // re-evaluate per-slug so the global policy actually lands.
    if slug.is_none() {
        match db::list_slugs_with_deferred_questions(conn) {
            Ok(slugs) => {
                for s in slugs {
                    // Recurse per-slug. Any individual slug error
                    // gets logged inside and doesn't abort the outer
                    // supersession handler.
                    let _ = reevaluate_deferred_questions_for_slug(conn, &s);
                }
                return Ok(());
            }
            Err(e) => {
                debug!(
                    error = %e,
                    "reevaluate_deferred_questions: failed to list slugs with deferred rows (global policy path)"
                );
                return Ok(());
            }
        }
    }
    reevaluate_deferred_questions_for_slug(conn, slug.unwrap())
}

/// Per-slug worker for `reevaluate_deferred_questions`. Loads the
/// active policy for the slug, lists its deferred rows, and
/// re-triages each against the new policy. Answer → remove, Defer →
/// update next_check_at, Skip → remove.
fn reevaluate_deferred_questions_for_slug(
    conn: &Connection,
    slug: &str,
) -> Result<()> {
    use crate::pyramid::triage::{resolve_decision, TriageDecision, TriageFacts};
    use crate::pyramid::types::LayerQuestion;

    let policy = match db::load_active_evidence_policy(conn, Some(slug)) {
        Ok(p) => p,
        Err(e) => {
            debug!(
                error = %e,
                slug,
                "reevaluate_deferred_questions_for_slug: failed to load policy, skipping"
            );
            return Ok(());
        }
    };

    let deferred = match db::list_all_deferred(conn, slug) {
        Ok(v) => v,
        Err(e) => {
            debug!(
                error = %e,
                slug,
                "reevaluate_deferred_questions_for_slug: failed to list deferred, skipping"
            );
            return Ok(());
        }
    };

    // Phase 12 wanderer fix: evaluate has_demand_signals at slug
    // granularity. Per-node aggregation by question.question_id
    // never matched because question_id is a q-{sha256} hash while
    // demand signals land on L{layer}-{seq} node ids. See
    // evidence_answering::run_triage_gate for the matching fix
    // and rationale.
    let slug_has_demand_signals = policy.demand_signals.iter().any(|rule| {
        let window = normalize_window_modifier(&rule.window);
        let sum = db::sum_slug_demand_weight(conn, slug, &rule.r#type, &window).unwrap_or(0.0);
        sum >= rule.threshold
    });

    let mut evaluated = 0usize;
    let mut activated = 0usize;
    let mut still_deferred = 0usize;
    let mut skipped = 0usize;

    for row in deferred {
        evaluated += 1;
        let question: LayerQuestion = match serde_json::from_str(&row.question_json) {
            Ok(q) => q,
            Err(_) => continue,
        };

        let facts = TriageFacts {
            question: &question,
            target_node_distilled: None,
            target_node_depth: Some(question.layer),
            is_first_build: false,
            is_stale_check: true, // re-evaluation is maintenance
            has_demand_signals: slug_has_demand_signals,
            evidence_question_trivial: None,
            evidence_question_high_value: None,
        };

        let decision = match resolve_decision(&policy, &facts) {
            Ok(d) => d,
            Err(_) => continue,
        };

        match decision {
            TriageDecision::Answer { .. } => {
                if db::remove_deferred(conn, &row.slug, &question.question_id).is_ok() {
                    activated += 1;
                }
            }
            TriageDecision::Defer { check_interval, .. } => {
                let _ = db::update_deferred_next_check(
                    conn,
                    &row.slug,
                    &question.question_id,
                    &check_interval,
                    policy.contribution_id.as_deref(),
                );
                still_deferred += 1;
            }
            TriageDecision::Skip { .. } => {
                if db::remove_deferred(conn, &row.slug, &question.question_id).is_ok() {
                    skipped += 1;
                }
            }
        }
    }

    debug!(
        slug,
        evaluated,
        activated,
        still_deferred,
        skipped,
        "reevaluate_deferred_questions_for_slug: Phase 12 complete"
    );

    Ok(())
}

/// Convert a short-form window ("7d", "14d", "1h") or already-formatted
/// SQLite modifier ("-7 days") into a valid SQLite datetime modifier.
fn normalize_window_modifier(window: &str) -> String {
    let w = window.trim();
    if w.starts_with('-') || w.contains(' ') {
        return w.to_string();
    }
    let (num_part, unit_part): (String, String) = w
        .chars()
        .partition(|c| c.is_ascii_digit());
    let n: i64 = num_part.parse().unwrap_or(14);
    let unit = match unit_part.as_str() {
        "d" => "days",
        "h" => "hours",
        "w" => "days",
        "m" => "minutes",
        _ => "days",
    };
    let n = if unit_part == "w" { n * 7 } else { n };
    format!("-{} {}", n, unit)
}

/// Phase 9: write the custom chain bundle (chain YAML + prompt files)
/// to disk under `chains/custom/` and `chains/prompts/`.
fn sync_custom_chain_to_disk(
    _conn: &Connection,
    contribution_id: &str,
) -> Result<(), ConfigSyncError> {
    debug!(
        contribution_id,
        "sync_custom_chain_to_disk: Phase 4 stub (Phase 9 wires this up)"
    );
    Ok(())
}

/// Phase 9: register the chain with `pyramid_chain_registry` after
/// disk sync.
fn register_chain_with_registry(
    _conn: &Connection,
    contribution_id: &str,
) -> Result<(), ConfigSyncError> {
    debug!(
        contribution_id,
        "register_chain_with_registry: Phase 4 stub (Phase 9 wires this up)"
    );
    Ok(())
}

// ── Phase 5: Rollback ────────────────────────────────────────────────────────

/// Phase 5 (Config History + Rollback): roll back to a previous config
/// contribution. Creates a new superseding contribution with the
/// target's yaml_content and `triggering_note: "manual rollback to
/// {contribution_id}"`.
///
/// Guards:
/// - **Local mode:** if local mode is enabled, refuse rollback of
///   `tier_routing` or `build_strategy` to prevent state splits.
/// - **Schema validation:** parse the target's yaml_content against its
///   schema_type to catch schema evolution breakage before committing.
///
/// After the supersession, syncs the new contribution to the operational
/// table and refreshes the provider registry so `call_model_unified`
/// picks up the rolled-back tier routing immediately.
pub fn rollback_config(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    contribution_id: &str,
) -> Result<()> {
    // 1. Load the target contribution.
    let target = load_contribution_by_id(conn, contribution_id)?
        .ok_or_else(|| anyhow::anyhow!("contribution {contribution_id} not found"))?;

    // 2. Local mode guard: refuse rollback of tier_routing or
    //    build_strategy while local mode is enabled to prevent
    //    state splits (AD-7).
    if target.schema_type == "tier_routing" || target.schema_type == "build_strategy" {
        let local_state = db::load_local_mode_state(conn)?;
        if local_state.enabled {
            anyhow::bail!(
                "Disable local mode before rolling back tier routing configuration."
            );
        }
    }

    // 3. Schema validation: parse the target YAML against its
    //    schema_type to catch schema evolution breakage. If the
    //    schema has changed since this version, the parse will fail
    //    and we refuse the rollback.
    validate_rollback_yaml(&target.yaml_content, &target.schema_type)?;

    // 4. Find the current active contribution for this schema_type.
    let current_active = load_active_config_contribution(
        conn,
        &target.schema_type,
        target.slug.as_deref(),
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no active {} contribution to roll back from",
            target.schema_type
        )
    })?;

    // 5. Create a new contribution superseding the active one with
    //    the target's yaml_content.
    let triggering_note = format!("manual rollback to {contribution_id}");
    let new_id = supersede_config_contribution(
        conn,
        &current_active.contribution_id,
        &target.yaml_content,
        &triggering_note,
        "local",
        Some("user"),
    )?;

    // 6. Sync the new contribution to operational.
    let new_contribution = load_contribution_by_id(conn, &new_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("rollback contribution disappeared immediately after create")
        })?;
    sync_config_to_operational(conn, bus, &new_contribution)?;

    // 7. Refresh the provider registry so downstream resolvers
    //    (call_model_unified, tier cascade) pick up the change.
    registry.load_from_db(conn)?;

    Ok(())
}

/// Validate that a YAML string parses correctly for its schema_type.
/// Used by rollback to catch schema evolution breakage before
/// committing the supersession.
fn validate_rollback_yaml(yaml_content: &str, schema_type: &str) -> Result<()> {
    match schema_type {
        "tier_routing" => {
            serde_yaml::from_str::<db::TierRoutingYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "build_strategy" => {
            serde_yaml::from_str::<db::BuildStrategyYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "dadbear_policy" => {
            serde_yaml::from_str::<db::DadbearPolicyYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "evidence_policy" => {
            serde_yaml::from_str::<db::EvidencePolicyYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "custom_prompts" => {
            serde_yaml::from_str::<db::CustomPromptsYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "step_overrides" => {
            serde_yaml::from_str::<db::StepOverridesBundleYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "folder_ingestion_heuristics" => {
            serde_yaml::from_str::<db::FolderIngestionHeuristicsYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "auto_update_policy" => {
            serde_yaml::from_str::<db::AutoUpdatePolicyYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "watch_root" => {
            serde_yaml::from_str::<db::WatchRootYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        "dadbear_norms" => {
            serde_yaml::from_str::<db::DadbearNormsYaml>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — configuration schema has changed since this version: {e}"
                ))?;
        }
        _ => {
            // For schema types without a dedicated struct (stubs,
            // future types), basic YAML validity check is sufficient.
            serde_yaml::from_str::<serde_yaml::Value>(yaml_content)
                .map_err(|e| anyhow::anyhow!(
                    "Cannot roll back — YAML is malformed: {e}"
                ))?;
        }
    }
    Ok(())
}

// ── Phase 0: Layered DADBEAR norms resolver ──────────────────────────────────
//
// Per the canonical state model: `dadbear_norms` contributions support
// global defaults (slug=NULL) with per-slug overrides. The resolver
// merges per-slug fields over the global base — missing per-slug fields
// fall through to the global defaults.
//
// Uses `db::DadbearNormsYaml` as the resolved type (defined by WS-C in
// db.rs). This avoids a redundant struct — the YAML schema and the
// resolved output share the same fields and serde defaults.

/// Layered resolver: merges global (slug=NULL) `dadbear_norms` with a
/// per-slug override. Per-slug fields override global; missing fields
/// fall through to global defaults. If neither exists, returns the
/// struct default.
///
/// Uses `serde_yaml::Value` merge so any new fields added to the schema
/// automatically participate in the layered merge without code changes.
///
/// When `slug` is `None`, returns the global defaults contribution
/// directly (or struct defaults if no global contribution exists).
pub fn resolve_dadbear_norms(
    conn: &Connection,
    slug: Option<&str>,
) -> Result<db::DadbearNormsYaml> {
    // 1. Load global default (slug IS NULL, schema_type='dadbear_norms', status='active')
    let global = load_active_config_contribution(conn, "dadbear_norms", None)?;

    // 2. For global-only resolution (slug=None), return global or defaults
    let Some(slug_str) = slug else {
        return match global {
            Some(g) => {
                let norms: db::DadbearNormsYaml = serde_yaml::from_str(&g.yaml_content)
                    .unwrap_or_default();
                Ok(norms)
            }
            None => Ok(db::DadbearNormsYaml::default()),
        };
    };

    // 3. Load per-slug override (slug = slug_str, schema_type='dadbear_norms', status='active')
    let per_slug = load_active_config_contribution(conn, "dadbear_norms", Some(slug_str))?;

    // 4. Merge: per-slug values override global, missing values fall through
    let merged_value: serde_yaml::Value = match (&global, &per_slug) {
        (None, None) => {
            // No contributions at all — return struct defaults
            return Ok(db::DadbearNormsYaml::default());
        }
        (Some(g), None) => {
            // Global only — parse as-is
            serde_yaml::from_str(&g.yaml_content)?
        }
        (None, Some(p)) => {
            // Per-slug only — parse as-is
            serde_yaml::from_str(&p.yaml_content)?
        }
        (Some(g), Some(p)) => {
            // Both exist — merge per-slug over global
            let mut base: serde_yaml::Value = serde_yaml::from_str(&g.yaml_content)?;
            let overlay: serde_yaml::Value = serde_yaml::from_str(&p.yaml_content)?;
            merge_yaml_values(&mut base, overlay);
            base
        }
    };

    // Deserialize the merged YAML into the typed struct.
    // serde defaults fill any fields missing from both layers.
    let resolved: db::DadbearNormsYaml = serde_yaml::from_value(merged_value)
        .unwrap_or_else(|e| {
            warn!(
                slug = ?slug,
                error = %e,
                "resolve_dadbear_norms: failed to deserialize merged norms, using defaults"
            );
            db::DadbearNormsYaml::default()
        });
    Ok(resolved)
}

/// Recursive YAML value merge: overlay's keys overwrite base's keys.
/// For Mapping values, merges recursively. For all other types
/// (scalars, sequences), overlay replaces base entirely.
fn merge_yaml_values(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                if let Some(base_entry) = base_map.get_mut(&key) {
                    merge_yaml_values(base_entry, value);
                } else {
                    base_map.insert(key, value);
                }
            }
        }
        (base, overlay) => {
            *base = overlay;
        }
    }
}

/// Rebuild the resolved dadbear_norms cache for all slugs that have
/// active `watch_root` or `dadbear_policy` contributions. Iterates
/// every distinct slug with an active DADBEAR-related contribution,
/// resolves norms via the layered merge, and upserts the resolved
/// values into `pyramid_dadbear_config` rows.
///
/// Called by the dispatcher when a global `dadbear_norms` contribution
/// syncs — a global change potentially affects every slug's resolved
/// norms.
pub fn rebuild_all_dadbear_norms_cache(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
) -> Result<()> {
    // Collect all distinct slugs that have active DADBEAR-related
    // contributions (dadbear_policy or dadbear_norms with a non-NULL slug).
    let slugs: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT slug FROM pyramid_config_contributions
             WHERE slug IS NOT NULL
               AND schema_type IN ('dadbear_policy', 'dadbear_norms', 'watch_root')
               AND status = 'active'"
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    debug!(
        slug_count = slugs.len(),
        "rebuild_all_dadbear_norms_cache: resolving norms for all active slugs"
    );

    for slug in &slugs {
        match resolve_dadbear_norms(conn, Some(slug.as_str())) {
            Ok(norms) => {
                // Upsert the resolved norms into pyramid_dadbear_config.
                // Only update the norms-related columns; leave source_path,
                // content_type, and enabled untouched (those come from
                // watch_root / dadbear_policy contributions).
                let result = conn.execute(
                    "UPDATE pyramid_dadbear_config SET
                        scan_interval_secs = ?1,
                        debounce_secs = ?2,
                        session_timeout_secs = ?3,
                        batch_size = ?4,
                        updated_at = datetime('now')
                     WHERE slug = ?5",
                    rusqlite::params![
                        norms.scan_interval_secs,
                        norms.debounce_secs,
                        norms.session_timeout_secs,
                        norms.batch_size,
                        slug,
                    ],
                );

                match result {
                    Ok(updated) => {
                        debug!(
                            slug = %slug,
                            rows_updated = updated,
                            scan_interval = norms.scan_interval_secs,
                            debounce = norms.debounce_secs,
                            "rebuild_all_dadbear_norms_cache: updated operational cache"
                        );
                    }
                    Err(e) => {
                        warn!(
                            slug = %slug,
                            error = %e,
                            "rebuild_all_dadbear_norms_cache: failed to update operational cache"
                        );
                    }
                }

                // Emit DadbearConfigChanged so the tick loop picks up
                // the new norms immediately.
                let _ = bus.tx.send(TaggedBuildEvent {
                    slug: slug.clone(),
                    kind: TaggedKind::DadbearConfigChanged {
                        slug: Some(slug.clone()),
                        schema_type: "dadbear_norms".to_string(),
                        contribution_id: String::new(),
                    },
                });
            }
            Err(e) => {
                warn!(
                    slug = %slug,
                    error = %e,
                    "rebuild_all_dadbear_norms_cache: failed to resolve norms, skipping"
                );
            }
        }
    }

    Ok(())
}

// ── Phase 0: Data migrations — contribution split ────────────────────────────

/// Migrate active `dadbear_policy` contributions into the split
/// `watch_root` + `dadbear_norms` contribution types. Idempotent via
/// `_migration_marker` with `created_by = 'dadbear_split_bootstrap'`.
///
/// See module-level doc and `docs/plans/dadbear-canonical-state-model.md`
/// Phase 0 for full details.
pub fn migrate_dadbear_policy_to_split(conn: &Connection) -> Result<()> {
    let marker_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_config_contributions
         WHERE schema_type = '_migration_marker'
           AND source = 'migration'
           AND created_by = 'dadbear_split_bootstrap'",
        [],
        |row| row.get(0),
    )?;
    if marker_exists > 0 {
        return Ok(());
    }

    let policies: Vec<(String, Option<String>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT contribution_id, slug, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'dadbear_policy'
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // DECOMMISSIONED: pyramid_auto_update_config table dropped.
    // Contribution existence in pyramid_dadbear_config is the enable gate.
    // All slugs with a dadbear config are considered enabled.
    let disabled_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut slug_policies: std::collections::HashMap<
        String,
        Vec<(String, db::DadbearPolicyYaml)>,
    > = std::collections::HashMap::new();

    for (contribution_id, slug_opt, yaml_content) in &policies {
        let Some(slug) = slug_opt.as_deref() else {
            continue;
        };
        let yaml: db::DadbearPolicyYaml = match serde_yaml::from_str(yaml_content) {
            Ok(y) => y,
            Err(e) => {
                warn!(
                    contribution_id,
                    error = %e,
                    "migrate_dadbear_policy_to_split: skipping unparseable dadbear_policy"
                );
                continue;
            }
        };
        slug_policies
            .entry(slug.to_string())
            .or_default()
            .push((contribution_id.clone(), yaml));
    }

    for (slug, entries) in &slug_policies {
        let is_disabled = disabled_slugs.contains(slug);

        if !is_disabled {
            for (_, policy_yaml) in entries {
                let watch_root = db::WatchRootYaml {
                    source_path: policy_yaml.source_path.clone(),
                    content_type: policy_yaml.content_type.clone(),
                };
                let yaml_str = serde_yaml::to_string(&watch_root)
                    .unwrap_or_else(|_| format!(
                        "source_path: {:?}\ncontent_type: {:?}\n",
                        watch_root.source_path, watch_root.content_type,
                    ));
                let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
                    "watch_root",
                    Some(slug),
                );
                metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
                let metadata_json = metadata.to_json().unwrap_or_else(|_| "{}".to_string());

                let wr_id = uuid::Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO pyramid_config_contributions (
                        contribution_id, slug, schema_type, yaml_content,
                        wire_native_metadata_json, wire_publication_state_json,
                        supersedes_id, superseded_by_id, triggering_note,
                        status, source, wire_contribution_id, created_by, accepted_at
                     ) VALUES (
                        ?1, ?2, 'watch_root', ?3,
                        ?4, '{}',
                        NULL, NULL, 'Split from dadbear_policy contribution',
                        'active', 'migration', NULL, 'dadbear_split_bootstrap', datetime('now')
                     )",
                    rusqlite::params![wr_id, slug, yaml_str, metadata_json],
                )?;
                // Sync the operational table's FK to point at the new
                // watch_root contribution instead of the now-superseded
                // dadbear_policy.
                conn.execute(
                    "UPDATE pyramid_dadbear_config SET contribution_id = ?1
                     WHERE slug = ?2 AND source_path = ?3",
                    rusqlite::params![wr_id, slug, policy_yaml.source_path],
                )?;
            }
        }

        let mut min_scan = i64::MAX;
        let mut max_debounce = i64::MIN;
        let mut max_session_timeout = i64::MIN;
        let mut max_batch_size = i64::MIN;
        for (_, policy_yaml) in entries {
            min_scan = min_scan.min(policy_yaml.scan_interval_secs);
            max_debounce = max_debounce.max(policy_yaml.debounce_secs);
            max_session_timeout = max_session_timeout.max(policy_yaml.session_timeout_secs);
            max_batch_size = max_batch_size.max(policy_yaml.batch_size);
        }
        if min_scan == i64::MAX { min_scan = 10; }
        if max_debounce == i64::MIN { max_debounce = 30; }
        if max_session_timeout == i64::MIN { max_session_timeout = 1800; }
        if max_batch_size == i64::MIN { max_batch_size = 1; }

        let norms = db::DadbearNormsYaml {
            scan_interval_secs: min_scan,
            debounce_secs: max_debounce,
            session_timeout_secs: max_session_timeout,
            batch_size: max_batch_size,
            min_changed_files: 1,
            runaway_threshold: 0.5,
            retention_window_days: 30,
        };
        let norms_yaml_str = serde_yaml::to_string(&norms)
            .unwrap_or_else(|_| "scan_interval_secs: 10\ndebounce_secs: 30\n".to_string());
        let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
            "dadbear_norms",
            Some(slug),
        );
        metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
        let metadata_json = metadata.to_json().unwrap_or_else(|_| "{}".to_string());

        let norms_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'dadbear_norms', ?3,
                ?4, '{}',
                NULL, NULL, 'Split from dadbear_policy contribution',
                'active', 'migration', NULL, 'dadbear_split_bootstrap', datetime('now')
             )",
            rusqlite::params![norms_id, slug, norms_yaml_str, metadata_json],
        )?;

        for (contribution_id, _) in entries {
            conn.execute(
                "UPDATE pyramid_config_contributions
                 SET status = 'superseded',
                     superseded_by_id = ?1
                 WHERE contribution_id = ?2
                   AND status = 'active'",
                rusqlite::params![norms_id, contribution_id],
            )?;
        }
    }

    let marker_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            status, source, created_by, accepted_at
         ) VALUES (
            ?1, NULL, '_migration_marker', '',
            'active', 'migration', 'dadbear_split_bootstrap', datetime('now')
         )",
        rusqlite::params![marker_id],
    )?;

    debug!("migrate_dadbear_policy_to_split: completed");
    Ok(())
}

/// Migrate `auto_update_policy` fields into the slug's existing
/// `dadbear_norms` contribution. Idempotent via `_migration_marker`
/// with `created_by = 'auto_update_norms_merge'`.
pub fn migrate_auto_update_into_norms(conn: &Connection) -> Result<()> {
    let marker_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_config_contributions
         WHERE schema_type = '_migration_marker'
           AND source = 'migration'
           AND created_by = 'auto_update_norms_merge'",
        [],
        |row| row.get(0),
    )?;
    if marker_exists > 0 {
        return Ok(());
    }

    let policies: Vec<(String, Option<String>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT contribution_id, slug, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'auto_update_policy'
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (contribution_id, slug_opt, yaml_content) in &policies {
        let Some(slug) = slug_opt.as_deref() else {
            continue;
        };

        let auto_yaml: db::AutoUpdatePolicyYaml = match serde_yaml::from_str(yaml_content) {
            Ok(y) => y,
            Err(e) => {
                warn!(
                    contribution_id,
                    error = %e,
                    "migrate_auto_update_into_norms: skipping unparseable auto_update_policy"
                );
                continue;
            }
        };

        let existing_norms = load_active_config_contribution(conn, "dadbear_norms", Some(slug))?;

        if let Some(norms_row) = existing_norms {
            let mut norms: db::DadbearNormsYaml =
                serde_yaml::from_str(&norms_row.yaml_content).unwrap_or_default();

            norms.min_changed_files = auto_yaml.min_changed_files;
            norms.runaway_threshold = auto_yaml.runaway_threshold;
            let debounce_from_auto = auto_yaml.debounce_minutes * 60;
            if debounce_from_auto > norms.debounce_secs {
                norms.debounce_secs = debounce_from_auto;
            }

            let norms_yaml_str = serde_yaml::to_string(&norms)
                .unwrap_or_else(|_| norms_row.yaml_content.clone());

            let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
                "dadbear_norms",
                Some(slug),
            );
            metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
            let metadata_json = metadata.to_json().unwrap_or_else(|_| "{}".to_string());

            let new_norms_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO pyramid_config_contributions (
                    contribution_id, slug, schema_type, yaml_content,
                    wire_native_metadata_json, wire_publication_state_json,
                    supersedes_id, superseded_by_id, triggering_note,
                    status, source, wire_contribution_id, created_by, accepted_at
                 ) VALUES (
                    ?1, ?2, 'dadbear_norms', ?3,
                    ?4, '{}',
                    ?5, NULL, 'Merged auto_update_policy fields into dadbear_norms',
                    'active', 'migration', NULL, 'auto_update_norms_merge', datetime('now')
                 )",
                rusqlite::params![
                    new_norms_id,
                    slug,
                    norms_yaml_str,
                    metadata_json,
                    norms_row.contribution_id,
                ],
            )?;

            conn.execute(
                "UPDATE pyramid_config_contributions
                 SET status = 'superseded',
                     superseded_by_id = ?1
                 WHERE contribution_id = ?2
                   AND status = 'active'",
                rusqlite::params![new_norms_id, norms_row.contribution_id],
            )?;
        } else {
            let norms = db::DadbearNormsYaml {
                min_changed_files: auto_yaml.min_changed_files,
                runaway_threshold: auto_yaml.runaway_threshold,
                debounce_secs: auto_yaml.debounce_minutes * 60,
                ..db::DadbearNormsYaml::default()
            };
            let norms_yaml_str = serde_yaml::to_string(&norms)
                .unwrap_or_else(|_| "min_changed_files: 1\nrunaway_threshold: 0.5\n".to_string());

            let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
                "dadbear_norms",
                Some(slug),
            );
            metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
            let metadata_json = metadata.to_json().unwrap_or_else(|_| "{}".to_string());

            let norms_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO pyramid_config_contributions (
                    contribution_id, slug, schema_type, yaml_content,
                    wire_native_metadata_json, wire_publication_state_json,
                    supersedes_id, superseded_by_id, triggering_note,
                    status, source, wire_contribution_id, created_by, accepted_at
                 ) VALUES (
                    ?1, ?2, 'dadbear_norms', ?3,
                    ?4, '{}',
                    NULL, NULL, 'Created from auto_update_policy migration',
                    'active', 'migration', NULL, 'auto_update_norms_merge', datetime('now')
                 )",
                rusqlite::params![norms_id, slug, norms_yaml_str, metadata_json],
            )?;
        }

        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1
               AND status = 'active'",
            rusqlite::params![contribution_id],
        )?;
    }

    let marker_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            status, source, created_by, accepted_at
         ) VALUES (
            ?1, NULL, '_migration_marker', '',
            'active', 'migration', 'auto_update_norms_merge', datetime('now')
         )",
        rusqlite::params![marker_id],
    )?;

    debug!("migrate_auto_update_into_norms: completed");
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::schema_registry::SchemaRegistry;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn mem_bus() -> Arc<BuildEventBus> {
        Arc::new(BuildEventBus::new())
    }

    fn sample_dadbear_yaml(slug: &str) -> String {
        format!(
            "source_path: \"/tmp/{slug}\"\n\
             content_type: \"conversation\"\n\
             scan_interval_secs: 10\n\
             debounce_secs: 30\n\
             session_timeout_secs: 1800\n\
             batch_size: 1\n\
             enabled: true\n"
        )
    }

    fn sample_evidence_yaml() -> String {
        "triage_rules: []\ndemand_signals: []\nbudget: {}\n".to_string()
    }

    #[test]
    fn test_create_and_load_active_contribution() {
        let conn = mem_conn();
        let id = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("initial intent"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        let loaded = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(loaded.contribution_id, id);
        assert_eq!(loaded.schema_type, "dadbear_policy");
        assert_eq!(loaded.slug.as_deref(), Some("my-slug"));
        assert_eq!(loaded.status, "active");
        assert_eq!(loaded.source, "local");
        assert_eq!(loaded.triggering_note.as_deref(), Some("initial intent"));
        assert!(loaded.accepted_at.is_some());
        // Phase 5: wire_native_metadata_json is now populated with the
        // canonical default for the schema_type, not the `'{}'` stub.
        // Verify that deserializing it yields the expected mapping-
        // table defaults rather than comparing against a raw string.
        let meta = WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();
        assert_eq!(
            meta.contribution_type,
            crate::pyramid::wire_native_metadata::WireContributionType::Template
        );
        assert_eq!(
            meta.maturity,
            crate::pyramid::wire_native_metadata::WireMaturity::Draft
        );
        assert!(meta.topics.iter().any(|t| t == "dadbear_policy"));
        assert!(meta.topics.iter().any(|t| t == "my-slug"));
        // Wire publication state stays empty until first publish.
        assert_eq!(loaded.wire_publication_state_json, "{}");

        let active = load_active_config_contribution(&conn, "dadbear_policy", Some("my-slug"))
            .unwrap()
            .unwrap();
        assert_eq!(active.contribution_id, id);
    }

    #[test]
    fn test_supersede_creates_chain() {
        let mut conn = mem_conn();
        let v1 = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("intent v1"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let v2 = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("my-slug"),
            "tightened scan",
            "local",
            Some("user"),
        )
        .unwrap();
        let v3 = supersede_config_contribution(
            &mut conn,
            &v2,
            &sample_dadbear_yaml("my-slug"),
            "agent suggestion",
            "local",
            Some("user"),
        )
        .unwrap();

        let prior = load_contribution_by_id(&conn, &v1).unwrap().unwrap();
        let mid = load_contribution_by_id(&conn, &v2).unwrap().unwrap();
        let latest = load_contribution_by_id(&conn, &v3).unwrap().unwrap();

        assert_eq!(prior.status, "superseded");
        assert_eq!(prior.superseded_by_id.as_deref(), Some(v2.as_str()));
        assert_eq!(mid.status, "superseded");
        assert_eq!(mid.supersedes_id.as_deref(), Some(v1.as_str()));
        assert_eq!(mid.superseded_by_id.as_deref(), Some(v3.as_str()));
        assert_eq!(latest.status, "active");
        assert_eq!(latest.supersedes_id.as_deref(), Some(v2.as_str()));
        assert!(latest.superseded_by_id.is_none());
    }

    #[test]
    fn test_supersede_requires_note() {
        let mut conn = mem_conn();
        let v1 = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("intent v1"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        let err = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("my-slug"),
            "",
            "local",
            Some("user"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));

        let err = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("my-slug"),
            "   \t\n  ",
            "local",
            Some("user"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_load_version_history_ordering() {
        let mut conn = mem_conn();
        let v1 = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("intent v1"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let v2 = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("my-slug"),
            "refinement 1",
            "local",
            Some("user"),
        )
        .unwrap();
        let v3 = supersede_config_contribution(
            &mut conn,
            &v2,
            &sample_dadbear_yaml("my-slug"),
            "refinement 2",
            "local",
            Some("user"),
        )
        .unwrap();

        let history =
            load_config_version_history(&conn, "dadbear_policy", Some("my-slug")).unwrap();
        let ids: Vec<String> = history.iter().map(|c| c.contribution_id.clone()).collect();
        assert_eq!(ids, vec![v1.clone(), v2.clone(), v3.clone()]);
        assert_eq!(history[0].status, "superseded");
        assert_eq!(history[1].status, "superseded");
        assert_eq!(history[2].status, "active");
    }

    #[test]
    fn test_propose_and_accept() {
        let mut conn = mem_conn();
        let active_id = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let proposal_id = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("agent found smaller batch"),
            "agent",
            Some("build-optimizer"),
            "proposed",
        )
        .unwrap();

        let proposals = list_pending_proposals(&conn, Some("my-slug")).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].contribution_id, proposal_id);
        assert_eq!(proposals[0].source, "agent");

        accept_proposal(&mut conn, &proposal_id).unwrap();

        let prior = load_contribution_by_id(&conn, &active_id)
            .unwrap()
            .unwrap();
        assert_eq!(prior.status, "superseded");
        assert_eq!(prior.superseded_by_id.as_deref(), Some(proposal_id.as_str()));

        let accepted = load_contribution_by_id(&conn, &proposal_id)
            .unwrap()
            .unwrap();
        assert_eq!(accepted.status, "active");
        assert_eq!(accepted.supersedes_id.as_deref(), Some(active_id.as_str()));
        assert!(accepted.accepted_at.is_some());

        let active = load_active_config_contribution(&conn, "dadbear_policy", Some("my-slug"))
            .unwrap()
            .unwrap();
        assert_eq!(active.contribution_id, proposal_id);
    }

    #[test]
    fn test_propose_and_reject() {
        let conn = mem_conn();
        let _active = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let proposal = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("agent idea"),
            "agent",
            Some("build-optimizer"),
            "proposed",
        )
        .unwrap();

        reject_proposal(&conn, &proposal, Some("not aligned with budget")).unwrap();

        let loaded = load_contribution_by_id(&conn, &proposal).unwrap().unwrap();
        assert_eq!(loaded.status, "rejected");
        let note = loaded.triggering_note.as_deref().unwrap_or("");
        assert!(
            note.contains("rejection reason"),
            "expected rejection reason in triggering_note, got: {note:?}"
        );

        // Rejected proposal does NOT become active.
        let active = load_active_config_contribution(&conn, "dadbear_policy", Some("my-slug"))
            .unwrap()
            .unwrap();
        assert_ne!(active.contribution_id, proposal);

        // Double-reject fails loudly.
        let err = reject_proposal(&conn, &proposal, None).unwrap_err();
        assert!(err.to_string().contains("not `proposed`"));
    }

    #[test]
    fn test_sync_dadbear_policy_end_to_end() {
        let conn = mem_conn();
        let bus = mem_bus();
        let yaml = "source_path: \"/tmp/sync-dadbear\"\n\
                    content_type: \"conversation\"\n\
                    scan_interval_secs: 15\n\
                    debounce_secs: 45\n\
                    session_timeout_secs: 900\n\
                    batch_size: 2\n\
                    enabled: true\n";
        let id = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("sync-dadbear-slug"),
            yaml,
            Some("sync me"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();

        // Ensure the slug exists in pyramid_slugs (the DADBEAR CRUD
        // doesn't enforce FK to it, but the upsert pattern is aligned
        // with tables that do — we verify behavior in isolation).
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        // Verify the operational row was written with the correct
        // contribution_id.
        let dadbear_configs =
            crate::pyramid::db::get_dadbear_configs(&conn, "sync-dadbear-slug").unwrap();
        assert!(
            dadbear_configs
                .iter()
                .any(|c| c.source_path == "/tmp/sync-dadbear"),
            "expected dadbear row to be upserted"
        );

        // The UPSERT recorded the contribution_id on the row — verify
        // via direct query since DadbearWatchConfig doesn't expose it
        // yet.
        let row_contribution_id: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_dadbear_config
                 WHERE slug = ?1 AND source_path = ?2",
                rusqlite::params!["sync-dadbear-slug", "/tmp/sync-dadbear"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row_contribution_id, id);
    }

    #[test]
    fn test_sync_evidence_policy_end_to_end() {
        let conn = mem_conn();
        let bus = mem_bus();
        let id = create_config_contribution(
            &conn,
            "evidence_policy",
            Some("sync-evidence-slug"),
            &sample_evidence_yaml(),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();

        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        let row: (String, String) = conn
            .query_row(
                "SELECT contribution_id, triage_rules_json FROM pyramid_evidence_policy
                 WHERE slug = ?1",
                rusqlite::params!["sync-evidence-slug"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, id);
        // Empty triage_rules deserializes to `None` → `"null"` when
        // serialized via serde_json. The upsert helper falls back to
        // "[]" only on serialization failure, so `null` is the
        // expected value for the minimal YAML above.
        assert!(row.1 == "null" || row.1 == "[]");
    }

    #[test]
    fn test_bootstrap_migration_idempotent() {
        // Build a fresh DB, add a legacy DADBEAR row directly,
        // re-run init which exercises the migration, verify one
        // contribution landed, then re-run init again and verify no
        // duplicates.
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        // Insert a legacy DADBEAR row directly — bypassing the
        // migration path so we simulate an install that predates
        // Phase 4.
        conn.execute("DELETE FROM pyramid_config_contributions", [])
            .unwrap();
        conn.execute("UPDATE pyramid_dadbear_config SET contribution_id = NULL", [])
            .unwrap();
        conn.execute(
            "INSERT INTO pyramid_dadbear_config
                (slug, source_path, content_type, scan_interval_secs, debounce_secs,
                 session_timeout_secs, batch_size, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "legacy-slug",
                "/tmp/legacy-source",
                "conversation",
                10,
                30,
                1800,
                1,
                1,
            ],
        )
        .unwrap();

        // First migration pass.
        crate::pyramid::db::migrate_legacy_dadbear_to_contributions(&conn).unwrap();

        let contrib_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'dadbear_policy' AND source = 'migration'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(contrib_count, 1, "first pass should insert one row");

        let marker_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = '_migration_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 1, "migration marker should exist after first pass");

        // Second migration pass.
        crate::pyramid::db::migrate_legacy_dadbear_to_contributions(&conn).unwrap();

        let contrib_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'dadbear_policy' AND source = 'migration'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            contrib_count_after, 1,
            "second pass must not duplicate the migration row"
        );

        let marker_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = '_migration_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            marker_count_after, 1,
            "marker must not duplicate on re-run"
        );

        // The legacy DADBEAR row should have gained its contribution_id.
        let legacy_contribution_id: Option<String> = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_dadbear_config
                 WHERE slug = 'legacy-slug' AND source_path = '/tmp/legacy-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            legacy_contribution_id.is_some(),
            "migration should populate contribution_id on legacy DADBEAR rows"
        );
    }

    #[test]
    fn test_init_pyramid_db_idempotent_with_contributions() {
        // Second idempotency guarantee per the Phase 4 brief: calling
        // `init_pyramid_db` twice on the same connection must not
        // duplicate any Phase 4 rows (including the bootstrap migration
        // path). This complements `test_bootstrap_migration_idempotent`
        // which exercises the migration helper directly; this test
        // exercises the full init path twice.
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        // Seed a legacy DADBEAR row that DOES NOT have a contribution_id,
        // so the next init pass's migration helper must pick it up.
        conn.execute(
            "INSERT INTO pyramid_dadbear_config
                (slug, source_path, content_type, scan_interval_secs, debounce_secs,
                 session_timeout_secs, batch_size, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "init-slug",
                "/tmp/init-source",
                "conversation",
                10,
                30,
                1800,
                1,
                1,
            ],
        )
        .unwrap();

        // First re-init — the seeded row is still unmigrated, but the
        // marker already exists from the first init pass, so the
        // migration helper short-circuits and leaves the row alone.
        init_pyramid_db(&conn).unwrap();

        let first_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let first_marker: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = '_migration_marker'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Second re-init — still no new rows.
        init_pyramid_db(&conn).unwrap();

        let second_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let second_marker: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = '_migration_marker'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            first_count, second_count,
            "re-running init_pyramid_db duplicated pyramid_config_contributions rows"
        );
        assert_eq!(
            first_marker, second_marker,
            "re-running init_pyramid_db duplicated the _migration_marker sentinel"
        );

        // The four new operational tables must exist (CREATE TABLE IF
        // NOT EXISTS idempotent) — sanity check a SELECT doesn't error.
        for tbl in [
            "pyramid_evidence_policy",
            "pyramid_build_strategy",
            "pyramid_custom_prompts",
            "pyramid_folder_ingestion_heuristics",
            "pyramid_config_contributions",
        ] {
            let _: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {tbl}"), [], |r| r.get(0))
                .unwrap_or_else(|e| panic!("{tbl} table missing or unreadable: {e}"));
        }

        // The contribution_id column must exist on pyramid_dadbear_config
        // after both init passes.
        let _: Option<String> = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_dadbear_config
                 WHERE slug = 'init-slug' AND source_path = '/tmp/init-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    }

    #[test]
    fn test_unknown_schema_type_fails_loudly() {
        let conn = mem_conn();
        let bus = mem_bus();
        let id = create_config_contribution(
            &conn,
            "not_a_real_schema_type",
            Some("any-slug"),
            "noop: true\n",
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        let err = sync_config_to_operational(&conn, &bus, &contribution).unwrap_err();
        match err {
            ConfigSyncError::UnknownSchemaType(t) => {
                assert_eq!(t, "not_a_real_schema_type");
            }
            other => panic!("expected UnknownSchemaType, got {other:?}"),
        }
    }

    #[test]
    fn test_global_config_with_null_slug() {
        let conn = mem_conn();
        let id = create_config_contribution(
            &conn,
            "tier_routing",
            None,
            // Phase 18a: canonical field is `entries:` per the bundled
            // tier_routing JSON Schema. Legacy `tiers:` is still
            // accepted as an alias on the struct.
            "entries: []\n",
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        let loaded = load_active_config_contribution(&conn, "tier_routing", None)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.contribution_id, id);
        assert!(loaded.slug.is_none());
    }

    /// Phase 17: the folder_ingestion_heuristics sync branch must round-trip
    /// the extended fields (code/document extensions, Claude Code knobs,
    /// default scan interval) from YAML into the operational table.
    #[test]
    fn test_sync_folder_ingestion_heuristics_with_new_fields() {
        let conn = mem_conn();
        let bus = mem_bus();
        let yaml = "schema_type: folder_ingestion_heuristics\n\
                    min_files_for_pyramid: 4\n\
                    max_recursion_depth: 6\n\
                    max_file_size_bytes: 5000000\n\
                    default_scan_interval_secs: 60\n\
                    code_extensions: [\".rs\", \".ts\"]\n\
                    document_extensions: [\".md\"]\n\
                    ignore_patterns:\n  - node_modules/\n  - target/\n\
                    claude_code_auto_include: false\n\
                    claude_code_conversation_path: /tmp/my-cc\n";
        let id = create_config_contribution(
            &conn,
            "folder_ingestion_heuristics",
            None,
            yaml,
            Some("phase 17 extended sync"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        let config = crate::pyramid::db::load_active_folder_ingestion_heuristics(&conn).unwrap();
        assert_eq!(config.min_files_for_pyramid, 4);
        assert_eq!(config.max_recursion_depth, 6);
        assert_eq!(config.max_file_size_bytes, 5_000_000);
        assert_eq!(config.default_scan_interval_secs, 60);
        assert_eq!(config.code_extensions, vec![".rs".to_string(), ".ts".to_string()]);
        assert_eq!(config.document_extensions, vec![".md".to_string()]);
        assert!(!config.claude_code_auto_include);
        assert_eq!(config.claude_code_conversation_path, "/tmp/my-cc");
        assert!(config
            .ignore_patterns
            .iter()
            .any(|p| p == "node_modules/"));
    }

    #[test]
    fn test_double_accept_errors() {
        let mut conn = mem_conn();
        let id = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("my-slug"),
            &sample_dadbear_yaml("my-slug"),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let err = accept_proposal(&mut conn, &id).unwrap_err();
        assert!(err.to_string().contains("not `proposed`"));
    }

    // ── Phase 5: creation-time capture tests ──────────────────────────────

    /// Every schema_type in the Phase 5 mapping table must produce a
    /// non-empty, canonical `wire_native_metadata_json` on creation
    /// (not the `'{}'` stub Phase 4 shipped with). This is the
    /// "Creation-Time Capture" spec requirement from
    /// `docs/specs/wire-contribution-mapping.md`.
    #[test]
    fn phase5_create_populates_canonical_metadata_for_all_14_schema_types() {
        use crate::pyramid::wire_native_metadata::{
            WireContributionType, WireMaturity, WireNativeMetadata, WireScope,
        };

        let conn = mem_conn();

        // The Phase 5 mapping table's 14 schema types — 9 template
        // types + 1 skill + 1 action + 3 config-template subtypes.
        let cases: &[(&str, WireContributionType)] = &[
            ("skill", WireContributionType::Skill),
            ("schema_definition", WireContributionType::Template),
            ("schema_annotation", WireContributionType::Template),
            ("evidence_policy", WireContributionType::Template),
            ("build_strategy", WireContributionType::Template),
            ("dadbear_policy", WireContributionType::Template),
            ("tier_routing", WireContributionType::Template),
            ("step_overrides", WireContributionType::Template),
            ("custom_prompts", WireContributionType::Template),
            ("folder_ingestion_heuristics", WireContributionType::Template),
            ("custom_chain", WireContributionType::Action),
            ("custom_chains", WireContributionType::Action),
            ("wire_discovery_weights", WireContributionType::Template),
            ("wire_auto_update_settings", WireContributionType::Template),
        ];

        for (schema_type, expected_type) in cases {
            let slug = format!("test-slug-{schema_type}");
            let id = create_config_contribution(
                &conn,
                schema_type,
                Some(&slug),
                "noop: true\n",
                Some("phase 5 creation-time capture test"),
                "local",
                Some("user"),
                "active",
            )
            .unwrap();

            let loaded = load_contribution_by_id(&conn, &id).unwrap().unwrap();

            // Phase 5 assertion: no `'{}'` stubs allowed.
            assert_ne!(
                loaded.wire_native_metadata_json, "{}",
                "schema_type {schema_type}: wire_native_metadata_json must not be the '{{}}' stub"
            );

            // The serialized metadata must deserialize to the correct
            // contribution_type, draft maturity, unscoped scope, and
            // review sync_mode per the spec's Creation-Time Capture
            // table.
            let meta = WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();
            assert_eq!(
                meta.contribution_type, *expected_type,
                "schema_type {schema_type}: expected {expected_type:?}, got {:?}",
                meta.contribution_type
            );
            assert_eq!(
                meta.maturity,
                WireMaturity::Draft,
                "schema_type {schema_type}: default maturity must be Draft"
            );
            assert!(
                matches!(meta.scope, WireScope::Unscoped),
                "schema_type {schema_type}: default scope must be Unscoped"
            );
            assert!(
                !meta.topics.is_empty(),
                "schema_type {schema_type}: default topics must not be empty"
            );
            // Slug should appear in the topic list for discovery.
            assert!(
                meta.topics.iter().any(|t| t == &slug),
                "schema_type {schema_type}: slug must appear in topics, got {:?}",
                meta.topics
            );

            // Publication state stays empty until first publish.
            assert_eq!(loaded.wire_publication_state_json, "{}");
        }
    }

    /// Supersession must carry forward canonical metadata with
    /// maturity reset to Draft — per the spec's "Auto-population on
    /// refinement" rules.
    #[test]
    fn phase5_supersede_carries_metadata_with_draft_reset() {
        use crate::pyramid::wire_native_metadata::{WireMaturity, WireNativeMetadata};

        let mut conn = mem_conn();
        let v1 = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("carry-slug"),
            &sample_dadbear_yaml("carry-slug"),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        // Promote v1 metadata to Canon so we can verify the reset.
        {
            let loaded = load_contribution_by_id(&conn, &v1).unwrap().unwrap();
            let mut meta =
                WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();
            meta.maturity = WireMaturity::Canon;
            meta.topics.push("custom-tag".to_string());
            let meta_json = meta.to_json().unwrap();
            conn.execute(
                "UPDATE pyramid_config_contributions
                 SET wire_native_metadata_json = ?1
                 WHERE contribution_id = ?2",
                rusqlite::params![meta_json, v1],
            )
            .unwrap();
        }

        let v2 = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("carry-slug"),
            "refinement",
            "local",
            Some("user"),
        )
        .unwrap();

        let loaded = load_contribution_by_id(&conn, &v2).unwrap().unwrap();
        let meta = WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();

        // Maturity must be reset to Draft.
        assert_eq!(meta.maturity, WireMaturity::Draft);
        // Custom topic from v1 must carry forward.
        assert!(meta.topics.iter().any(|t| t == "custom-tag"));
        // supersedes should still be None because v1 was not
        // Wire-published (no handle_path in wire_publication_state).
        assert!(meta.supersedes.is_none());
    }

    /// Supersession with a Wire-published prior version should set
    /// `supersedes` to the prior's handle-path.
    #[test]
    fn phase5_supersede_sets_supersedes_when_prior_is_wire_published() {
        use crate::pyramid::wire_native_metadata::{WireNativeMetadata, WirePublicationState};

        let mut conn = mem_conn();
        let v1 = create_config_contribution(
            &conn,
            "dadbear_policy",
            Some("pub-slug"),
            &sample_dadbear_yaml("pub-slug"),
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        // Simulate a Wire publish by writing a publication state.
        let pub_state = WirePublicationState {
            wire_contribution_id: Some("wire-uuid-1".to_string()),
            handle_path: Some("playful/77/3".to_string()),
            chain_root: None,
            chain_head: None,
            published_at: Some("2026-04-10T00:00:00Z".to_string()),
            last_resolved_derived_from: Vec::new(),
        };
        let pub_state_json = serde_json::to_string(&pub_state).unwrap();
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET wire_publication_state_json = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![pub_state_json, v1],
        )
        .unwrap();

        let v2 = supersede_config_contribution(
            &mut conn,
            &v1,
            &sample_dadbear_yaml("pub-slug"),
            "refinement after publish",
            "local",
            Some("user"),
        )
        .unwrap();

        let loaded = load_contribution_by_id(&conn, &v2).unwrap().unwrap();
        let meta = WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();

        // supersedes should be set to the prior's handle-path.
        assert_eq!(meta.supersedes.as_deref(), Some("playful/77/3"));
    }

    /// `create_config_contribution_with_metadata` must honor the
    /// caller-supplied metadata (used by the bundled seed +
    /// migration paths).
    #[test]
    fn phase5_create_with_metadata_honors_caller_values() {
        use crate::pyramid::wire_native_metadata::{
            default_wire_native_metadata, WireMaturity, WireNativeMetadata,
        };

        let conn = mem_conn();
        let mut meta = default_wire_native_metadata("skill", Some("my-seed"));
        meta.maturity = WireMaturity::Canon;
        meta.price = Some(2);

        let id = create_config_contribution_with_metadata(
            &conn,
            "skill",
            Some("my-seed"),
            "# Seed body",
            Some("bundled seed"),
            "bundled",
            Some("phase5_bootstrap"),
            "active",
            &meta,
        )
        .unwrap();

        let loaded = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        let loaded_meta =
            WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap();
        assert_eq!(loaded_meta.maturity, WireMaturity::Canon);
        assert_eq!(loaded_meta.price, Some(2));
    }

    /// `invalidate_prompt_cache` must actually clear the global cache
    /// when a skill contribution lands via the dispatcher.
    #[test]
    fn phase5_dispatcher_invalidates_prompt_cache_on_skill_sync() {
        use crate::pyramid::prompt_cache::{global_prompt_cache, PromptCache};

        let conn = mem_conn();
        let bus = mem_bus();

        // Prime the global cache by inserting a skill directly and
        // pulling it through the cache.
        insert_seed_skill(&conn, "prime/test.md", "primed body");
        let _ = global_prompt_cache().get(&conn, "$prompts/prime/test.md");
        // Small sanity: global_prompt_cache is populated.
        assert!(global_prompt_cache().contains("prime/test.md"));

        // Create a *different* skill via the dispatcher. The
        // dispatcher should call `invalidate_prompt_cache`, which
        // clears the global cache. The primed entry should disappear.
        let id = create_config_contribution(
            &conn,
            "skill",
            Some("other/skill.md"),
            "# Other body",
            Some("test"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        // Global cache should have been cleared.
        assert!(!global_prompt_cache().contains("prime/test.md"));

        // Local cache behavior sanity check (verifies the invalidation
        // is not a global-scope bug).
        let local = PromptCache::new();
        let _ = local.get(&conn, "$prompts/prime/test.md");
        assert!(local.contains("prime/test.md"));
    }

    fn insert_seed_skill(conn: &Connection, slug: &str, body: &str) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                status, source, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'skill', ?3,
                '{}', '{}',
                'active', 'bundled', 'test', datetime('now')
             )",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), slug, body],
        )
        .unwrap();
    }

    /// Dry-run publish must refuse a draft-maturity contribution and
    /// surface credential-leak warnings when the body contains
    /// `${VAR_NAME}` references.
    #[test]
    fn phase5_dry_run_publish_surfaces_warnings_for_draft_with_credentials() {
        use crate::pyramid::wire_native_metadata::WireNativeMetadata;
        use crate::pyramid::wire_publish::PyramidPublisher;

        let conn = mem_conn();
        let id = create_config_contribution(
            &conn,
            "custom_prompts",
            Some("leaky-slug"),
            "header: ${OPENROUTER_API_KEY}\n",
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        let metadata =
            WireNativeMetadata::from_json(&contribution.wire_native_metadata_json).unwrap();

        let publisher = PyramidPublisher::new("https://x.invalid".to_string(), String::new());
        let report = publisher
            .dry_run_publish(
                &contribution.contribution_id,
                &contribution.schema_type,
                &contribution.yaml_content,
                &metadata,
            )
            .unwrap();

        // Warnings should mention credential references.
        let joined = report.warnings.join(" | ");
        assert!(
            joined.contains("credential"),
            "expected credential warning, got: {joined}"
        );

        // Visibility should serialize the unscoped scope.
        assert_eq!(report.visibility, "unscoped");
        // Wire type should match the mapping table.
        assert_eq!(report.wire_type, "template");
    }

    /// Dry-run publish should compute a 28-slot allocation from the
    /// metadata's derived_from weights.
    #[test]
    fn phase5_dry_run_publish_allocates_28_slots_from_derived_from() {
        use crate::pyramid::wire_native_metadata::{
            WireNativeMetadata, WireRef,
        };
        use crate::pyramid::wire_publish::PyramidPublisher;

        let conn = mem_conn();
        let id = create_config_contribution(
            &conn,
            "skill",
            Some("with-sources"),
            "# Skill body",
            Some("initial"),
            "local",
            Some("user"),
            "active",
        )
        .unwrap();

        // Inject derived_from into the metadata column.
        let mut metadata = {
            let loaded = load_contribution_by_id(&conn, &id).unwrap().unwrap();
            WireNativeMetadata::from_json(&loaded.wire_native_metadata_json).unwrap()
        };
        metadata.derived_from = vec![
            WireRef {
                ref_: Some("author/1/1".to_string()),
                doc: None,
                corpus: None,
                weight: 0.5,
                justification: "primary".to_string(),
            },
            WireRef {
                ref_: None,
                doc: Some("wire-actions.md".to_string()),
                corpus: None,
                weight: 0.3,
                justification: "secondary".to_string(),
            },
            WireRef {
                ref_: None,
                doc: None,
                corpus: Some("corpus-name/x.md".to_string()),
                weight: 0.2,
                justification: "tertiary".to_string(),
            },
        ];
        let meta_json = metadata.to_json().unwrap();
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET wire_native_metadata_json = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![meta_json, id],
        )
        .unwrap();

        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        let metadata =
            WireNativeMetadata::from_json(&contribution.wire_native_metadata_json).unwrap();

        let publisher = PyramidPublisher::new("https://x.invalid".to_string(), String::new());
        let report = publisher
            .dry_run_publish(
                &contribution.contribution_id,
                &contribution.schema_type,
                &contribution.yaml_content,
                &metadata,
            )
            .unwrap();

        assert_eq!(report.resolved_derived_from.len(), 3);
        let total_slots: u32 = report
            .resolved_derived_from
            .iter()
            .map(|e| e.allocated_slots)
            .sum();
        assert_eq!(total_slots, 28, "slot allocation must sum to 28");
        // Every source should receive at least 1 slot.
        for entry in &report.resolved_derived_from {
            assert!(entry.allocated_slots >= 1);
        }
        // Phase 5: references are unresolved until Phase 10's live
        // path→UUID map ships.
        for entry in &report.resolved_derived_from {
            assert!(!entry.resolved);
        }
    }

    // ── Phase 9 dispatcher wiring tests ────────────────────────────

    #[test]
    fn test_phase9_schema_definition_dispatcher_flags_and_invalidates() {
        // End-to-end wiring test: create a schema_definition
        // contribution, run the dispatcher via
        // sync_config_to_operational_with_registry, verify the
        // schema_registry cache got invalidated AND downstream
        // configs of the target schema_type got flagged for migration.
        use crate::pyramid::wire_migration::walk_bundled_contributions_manifest;

        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = mem_bus();

        // Before the dispatcher runs: the bundled evidence_policy
        // default should exist and have needs_migration = 0.
        let before: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-evidence_policy-default-v1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0);

        // Create a new schema_definition contribution for
        // evidence_policy and run the dispatcher.
        let metadata = default_wire_native_metadata("schema_definition", Some("evidence_policy"));
        let id = create_config_contribution_with_metadata(
            &conn,
            "schema_definition",
            Some("evidence_policy"),
            "{\"type\":\"object\"}",
            Some("new v2 schema"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();

        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational_with_registry(&conn, &bus, &contribution, Some(&registry))
            .unwrap();

        // After: the bundled evidence_policy row should have
        // needs_migration = 1 (flag_configs_for_migration wired up).
        let after: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-evidence_policy-default-v1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1, "flag_configs_for_migration should have set the flag");
    }

    // ── Phase 14: new schema_type dispatcher branches ──────────────────

    #[test]
    fn test_sync_wire_discovery_weights_no_operational_table() {
        let conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("wire_discovery_weights", None);
        let id = create_config_contribution_with_metadata(
            &conn,
            "wire_discovery_weights",
            None,
            "schema_type: wire_discovery_weights\nfields:\n  w_rating: 0.3\n",
            Some("tune ranking"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        // Should succeed with no operational table write — the
        // ranking engine reads the contribution on demand via its
        // in-memory TTL cache.
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();
    }

    #[test]
    fn test_sync_wire_auto_update_settings() {
        let conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("wire_auto_update_settings", None);
        let id = create_config_contribution_with_metadata(
            &conn,
            "wire_auto_update_settings",
            None,
            "schema_type: wire_auto_update_settings\nauto_update:\n  custom_prompts: true\n",
            Some("enable auto-update for custom prompts"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();
    }

    #[test]
    fn test_sync_wire_update_polling() {
        let conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("wire_update_polling", None);
        let id = create_config_contribution_with_metadata(
            &conn,
            "wire_update_polling",
            None,
            "schema_type: wire_update_polling\ninterval_secs: 3600\n",
            Some("poll hourly"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();
        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();
    }
}
