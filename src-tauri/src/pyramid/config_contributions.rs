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

use crate::pyramid::db;
use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};

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
/// Caller is responsible for picking the right `status`: the standard
/// path is `'active'` for direct user-created configs and `'proposed'`
/// for agent proposals. `source` is one of the canonical vocabulary
/// values (`local`, `agent`, `wire`, `bundled`, `migration`).
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
    let contribution_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at
         ) VALUES (
            ?1, ?2, ?3, ?4,
            '{}', '{}',
            NULL, NULL, ?5,
            ?6, ?7, NULL, ?8,
            CASE WHEN ?6 = 'active' THEN datetime('now') ELSE NULL END
         )",
        rusqlite::params![
            contribution_id,
            slug,
            schema_type,
            yaml_content,
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

    // Load the prior contribution to inherit schema_type + slug.
    let prior: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT schema_type, slug, status FROM pyramid_config_contributions
             WHERE contribution_id = ?1",
            rusqlite::params![prior_contribution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let (schema_type, slug, prior_status) = prior
        .ok_or_else(|| anyhow::anyhow!("prior contribution {prior_contribution_id} not found"))?;

    if prior_status == "superseded" {
        anyhow::bail!(
            "prior contribution {prior_contribution_id} is already superseded — cannot supersede a non-active version"
        );
    }

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
            '{}', '{}',
            ?5, NULL, ?6,
            'active', ?7, NULL, ?8, datetime('now')
         )",
        rusqlite::params![
            new_id,
            slug,
            schema_type,
            new_yaml_content,
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
pub fn sync_config_to_operational(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
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
            // Phase 9: superseding a schema flags downstream configs
            // for LLM-assisted migration.
            flag_configs_for_migration(conn, &contribution.schema_type)?;
            invalidate_schema_registry_cache();
        }
        "schema_annotation" => {
            // Phase 8: YAML-to-UI renderer cache invalidation.
            invalidate_schema_annotation_cache();
        }
        "wire_discovery_weights" => {
            // Phase 14: Wire discovery ranking cache invalidation.
            invalidate_wire_discovery_cache();
        }
        "wire_auto_update_settings" => {
            // Phase 14: per-schema_type auto-update scheduler
            // reconfiguration.
            reconfigure_wire_update_scheduler(conn)?;
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

/// Phase 6: cache-invalidate the prompt composition layer so the next
/// LLM call re-reads prompts from pyramid_config_contributions.
fn invalidate_prompt_cache() {
    debug!("invalidate_prompt_cache: Phase 4 stub (Phase 6 wires this up)");
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
/// config for LLM-assisted migration.
fn flag_configs_for_migration(_conn: &Connection, target_schema_type: &str) -> Result<()> {
    debug!(
        target_schema_type,
        "flag_configs_for_migration: Phase 4 stub (Phase 9 wires this up)"
    );
    Ok(())
}

/// Phase 9: invalidate the cached schema registry so the next
/// validation call re-reads from pyramid_config_contributions.
fn invalidate_schema_registry_cache() {
    debug!("invalidate_schema_registry_cache: Phase 4 stub (Phase 9 wires this up)");
}

/// Phase 8: invalidate the YAML-to-UI renderer cache.
fn invalidate_schema_annotation_cache() {
    debug!("invalidate_schema_annotation_cache: Phase 4 stub (Phase 8 wires this up)");
}

/// Phase 14: invalidate the Wire discovery ranking cache.
fn invalidate_wire_discovery_cache() {
    debug!("invalidate_wire_discovery_cache: Phase 4 stub (Phase 14 wires this up)");
}

/// Phase 14: reconfigure the Wire update scheduler after
/// wire_auto_update_settings changes.
fn reconfigure_wire_update_scheduler(_conn: &Connection) -> Result<()> {
    debug!("reconfigure_wire_update_scheduler: Phase 4 stub (Phase 14 wires this up)");
    Ok(())
}

/// Phase 1 / Phase 11: after a DADBEAR policy updates, trigger the
/// DADBEAR tick loop to re-read its config on the next cycle. Today's
/// DADBEAR tick already re-reads per tick, so this is a no-op for now.
fn trigger_dadbear_reload(_bus: &Arc<BuildEventBus>, slug: Option<&str>) {
    debug!(
        slug = ?slug,
        "trigger_dadbear_reload: Phase 4 no-op (DADBEAR already re-reads per tick)"
    );
}

/// Phase 11: re-evaluate deferred questions after an evidence_policy
/// contribution lands. See `evidence-triage-and-dadbear.md`.
fn reevaluate_deferred_questions(_conn: &Connection, slug: Option<&str>) -> Result<()> {
    debug!(
        slug = ?slug,
        "reevaluate_deferred_questions: Phase 4 stub (Phase 11 wires this up)"
    );
    Ok(())
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
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
        assert_eq!(loaded.wire_native_metadata_json, "{}");
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
            "tiers: []\n",
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
}
