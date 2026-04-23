// pyramid/migration_config.rs — Phase 18d: Schema Migration UI backend.
//
// Canonical reference:
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/generative-config-pattern.md
//     — "Schema Definitions Are Contributions" section (~line 99)
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/config-contribution-and-wire-sharing.md
//     — supersession + sync semantics
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/plans/phase-18d-workstream-prompt.md
//     — workstream brief
//
// Phase 9 shipped two primitives:
//   1. `flag_configs_needing_migration` in schema_registry.rs — sets
//      `needs_migration = 1` on every active config row whose schema_type
//      target was just superseded by a new schema_definition contribution.
//   2. The `needs_migration INTEGER NOT NULL DEFAULT 0` column on
//      `pyramid_config_contributions` itself.
//
// Phase 4's dispatcher already calls (1) inside the `schema_definition`
// branch when a new schema_definition lands. Nothing surfaces the flag,
// nothing acts on it. Phase 18d (this module) is the user-facing surface
// AND the migration execution path:
//
//   * `list_configs_needing_migration` — query the flagged rows and
//     resolve the prior + current schema definitions for each one.
//   * `propose_config_migration` — load the flagged contribution + the
//     two schema bodies, call the LLM (via the bundled migrate_config
//     skill), persist the result as a draft contribution. Mirrors Phase
//     9's `generate_config_from_intent` 3-phase shape exactly.
//   * `accept_config_migration` — promote the draft to active via
//     supersession, run sync_config_to_operational so the operational
//     table picks up the migrated YAML, clear the `needs_migration`
//     flag.
//   * `reject_config_migration` — delete the draft row, leaving the
//     original flagged contribution intact so the user can retry later
//     or migrate manually via the existing edit flow.
//
// User review is mandatory — there is no auto-apply path. The migration
// flow always goes draft -> review -> accept, matching Phase 9's
// generation flow.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::pyramid::config_contributions::{
    load_contribution_by_id, sync_config_to_operational_with_registry, ConfigContribution,
};
use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use crate::pyramid::llm::{call_model_unified_with_options_and_ctx, LlmCallOptions, LlmConfig};
use crate::pyramid::provider::ProviderRegistry;
use crate::pyramid::schema_registry::SchemaRegistry;
use crate::pyramid::step_context::{compute_prompt_hash, StepContext};
use crate::pyramid::wire_native_metadata::{default_wire_native_metadata, WireMaturity};

// ── Constants ────────────────────────────────────────────────────────

/// Slug convention used by the bundled migration prompt skill. The same
/// pattern Phase 9 uses for generation skills (`generation/<type>.md`),
/// adapted for migration. Looked up at proposal time via the same
/// slug-first / topic-tag-fallback strategy as the schema registry's
/// generation-skill resolver.
const MIGRATION_SKILL_SLUG: &str = "migration/migrate_config.md";

// ── Response types ───────────────────────────────────────────────────

/// One row in the response from `pyramid_list_configs_needing_migration`.
/// Carries everything the UI needs to render a flagged config card and
/// open the review modal: identity (contribution_id, schema_type, slug),
/// the YAML body the user wrote against the prior schema, and the two
/// schema_definition contribution_ids that bracket the migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedsMigrationEntry {
    pub contribution_id: String,
    pub schema_type: String,
    pub slug: Option<String>,
    pub current_yaml: String,
    /// contribution_id of the active `schema_definition` whose target
    /// is this row's schema_type. The migration produces YAML valid
    /// against THIS schema.
    pub current_schema_contribution_id: String,
    /// contribution_id of the prior `schema_definition` that the
    /// flagged row's YAML was originally written against. Found by
    /// walking the schema_definition supersession chain backward by
    /// `created_at` (option B in the workstream prompt). May be `None`
    /// if no prior version exists (the YAML predates any schema
    /// supersession).
    pub prior_schema_contribution_id: Option<String>,
    /// When the row was flagged. Today the column doesn't carry a
    /// dedicated `flagged_at` timestamp, so we surface the contribution's
    /// `created_at` as a stable ordering signal — every flagged row
    /// renders with a "since X" hint.
    pub flagged_at: String,
    /// The triggering_note from the schema_definition contribution that
    /// caused the flag, when resolvable. This is the supersession
    /// rationale the user reads to decide whether to migrate.
    pub supersession_note: Option<String>,
}

/// Response from `pyramid_propose_config_migration`. Contains the LLM's
/// proposed migration as a draft contribution + a structured payload the
/// review modal renders side-by-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationProposal {
    /// contribution_id of the freshly-created draft row holding the
    /// migrated YAML. The accept path looks this up by id.
    pub draft_id: String,
    /// The original YAML (against the prior schema). Echoed back so the
    /// frontend can render the diff without a follow-up load.
    pub old_yaml: String,
    /// The LLM's migrated YAML (against the new schema). This is what
    /// gets stored on the draft contribution.
    pub new_yaml: String,
    /// schema_type the migration targets. Always equals the flagged
    /// contribution's schema_type.
    pub schema_type: String,
    /// JSON schema body of the prior schema_definition. Returned so the
    /// review modal can show "what the old YAML was validated against"
    /// without an additional fetch.
    pub schema_from: String,
    /// JSON schema body of the new schema_definition. Returned so the
    /// review modal can show what the new YAML is validated against.
    pub schema_to: String,
}

/// Response from `pyramid_accept_config_migration`. The accept path is
/// transactional: the draft row gets promoted to active, the prior
/// active is superseded, the operational table picks up the new YAML
/// via `sync_config_to_operational_with_registry`, and the
/// `needs_migration` flag is cleared on the new row (it's freshly
/// valid against the new schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptMigrationOutcome {
    pub new_contribution_id: String,
    pub schema_type: String,
    pub slug: Option<String>,
    /// `true` once `sync_config_to_operational_with_registry` returns
    /// without error. The frontend uses this to confirm the executor
    /// will see the migrated YAML on its next read.
    pub sync_succeeded: bool,
}

/// Response from `pyramid_reject_config_migration`. Confirms the draft
/// was deleted; the original flagged contribution is untouched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectMigrationOutcome {
    pub deleted_draft_id: String,
    /// contribution_id of the original flagged row, untouched. The user
    /// can re-propose later or edit it manually.
    pub original_contribution_id: String,
}

// ── Helpers: schema chain walk + skill lookup ────────────────────────

/// Find the prior `schema_definition` contribution for a given target
/// schema_type, given the flagged config's `created_at` timestamp.
///
/// Strategy (option B from the workstream prompt — the chain walk):
///   1. Look only at superseded schema_definition rows (`superseded_by_id IS
///      NOT NULL`). The "prior schema" by definition MUST have been
///      superseded — otherwise the config wouldn't be flagged for migration.
///      Restricting to superseded rows also avoids accidentally returning
///      the new active schema (which is the destination, not the origin).
///   2. Among the superseded rows, return the most recent one whose
///      `created_at <= the_config_created_at`. SQLite's `datetime('now')`
///      writes second precision, so the comparison must be inclusive: a
///      schema_definition inserted at T and a config seeded at T (same
///      second) means the config WAS valid against that schema even
///      though their text timestamps are equal.
///   3. Order by `created_at DESC, id DESC` so newer prior schemas win
///      on ties and the result is deterministic.
///
/// Returns `None` when no superseded schema_definition predates the
/// flagged contribution (the YAML was written before any schema
/// supersession, which is the bundled-default first-write case where
/// no migration history exists yet).
fn find_prior_schema_definition_id(
    conn: &Connection,
    target_schema_type: &str,
    config_created_at: &str,
) -> Result<Option<String>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = 'schema_definition'
               AND slug = ?1
               AND superseded_by_id IS NOT NULL
               AND created_at <= ?2
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![target_schema_type, config_created_at],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Find the active migration prompt skill contribution. Slug-first
/// (matches the bundled manifest's `migration/migrate_config.md` slug),
/// then a topic-tag fallback for any future user-contributed migration
/// skills that use a different slug convention.
///
/// Returns the skill body string if a skill is found, or an error
/// message describing what was missing. Phase 18d only ships ONE
/// migration skill (the bundled one), but the lookup is generic so
/// users can refine the migration prompt itself via the generative
/// config flow without code changes.
fn load_migration_skill_body(conn: &Connection) -> Result<String> {
    // Slug-convention lookup first.
    let direct: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE schema_type = 'skill'
               AND status = 'active'
               AND superseded_by_id IS NULL
               AND slug = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![MIGRATION_SKILL_SLUG],
            |row| row.get(0),
        )
        .ok();
    if let Some(body) = direct {
        return Ok(body);
    }

    // Topic-tag fallback: scan every active skill and parse its
    // metadata JSON for `topics` containing both "migration" and
    // "schema_migration".
    let mut stmt = conn.prepare(
        "SELECT yaml_content, wire_native_metadata_json
         FROM pyramid_config_contributions
         WHERE schema_type = 'skill'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (body, metadata_json) = row?;
        if metadata_has_both_topics(&metadata_json, "migration", "schema_migration") {
            return Ok(body);
        }
    }

    Err(anyhow!(
        "no active migration skill contribution found (looked for slug '{MIGRATION_SKILL_SLUG}' \
         and topic tags ['migration', 'schema_migration'])"
    ))
}

/// Check whether a WireNativeMetadata JSON blob has both required topic
/// tags. Mirrors the helper in schema_registry.rs — kept private here
/// to avoid an export-just-for-this dependency.
fn metadata_has_both_topics(json: &str, topic_a: &str, topic_b: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Some(topics) = value.get("topics").and_then(|v| v.as_array()) else {
        return false;
    };
    let has_a = topics.iter().any(|t| t.as_str() == Some(topic_a));
    let has_b = topics.iter().any(|t| t.as_str() == Some(topic_b));
    has_a && has_b
}

/// Substitute the `{old_schema}`, `{new_schema}`, `{old_yaml}`,
/// `{user_note}` placeholders in the migration skill body. Also handles
/// the `{if user_note}...{end}` conditional block. Mirrors the
/// substitution logic in `generative_config::substitute_prompt` —
/// duplicated here so the migration flow doesn't pull in the generative
/// module's private helpers.
fn substitute_migration_prompt(
    template: &str,
    old_schema: &str,
    new_schema: &str,
    old_yaml: &str,
    user_note: Option<&str>,
) -> String {
    let mut out = template.to_string();
    out = process_conditional_block(&out, "{if user_note}", "{end}", user_note.is_some());
    out = out.replace("{old_schema}", old_schema);
    out = out.replace("{new_schema}", new_schema);
    out = out.replace("{old_yaml}", old_yaml);
    out = out.replace("{user_note}", user_note.unwrap_or(""));
    out
}

/// Process a `{if X}...{end}` block. When `keep == true` the markers
/// are stripped and the content between them is retained. When
/// `keep == false` the entire block is removed. Operates on the first
/// occurrence only (Phase 18d's prompt uses each conditional once).
fn process_conditional_block(
    input: &str,
    open_marker: &str,
    close_marker: &str,
    keep: bool,
) -> String {
    let Some(start) = input.find(open_marker) else {
        return input.to_string();
    };
    let after_open = start + open_marker.len();
    let Some(rel_end) = input[after_open..].find(close_marker) else {
        return input.to_string();
    };
    let end = after_open + rel_end;
    let after_close = end + close_marker.len();

    let mut out = String::with_capacity(input.len());
    out.push_str(&input[..start]);
    if keep {
        let inner = &input[after_open..end];
        out.push_str(inner.trim_start_matches('\n'));
    }
    out.push_str(&input[after_close..]);
    out
}

/// Extract a YAML body from an LLM response. Same best-effort logic as
/// `generative_config::extract_yaml_body` — strips fenced code blocks
/// and prose prefixes. Duplicated here so migration_config doesn't
/// depend on the generative_config module's private helpers.
fn extract_yaml_body(raw: &str) -> String {
    let trimmed = raw.trim();

    if let Some(body) = extract_fenced_block(trimmed) {
        return body;
    }

    if !trimmed.starts_with("schema_type") && !trimmed.starts_with("---") {
        if let Some(idx) = trimmed.find("schema_type:") {
            return trimmed[idx..].trim().to_string();
        }
    }

    trimmed.to_string()
}

fn extract_fenced_block(input: &str) -> Option<String> {
    let fence_start = input.find("```")?;
    let after_open = &input[fence_start + 3..];
    let after_open_line = after_open
        .find('\n')
        .map(|idx| &after_open[idx + 1..])
        .unwrap_or(after_open);
    let fence_end = after_open_line.find("```")?;
    Some(after_open_line[..fence_end].trim().to_string())
}

// ── 1. List configs needing migration ────────────────────────────────

/// Query every active contribution with `needs_migration = 1` and
/// resolve enough metadata for the UI to render a flagged-config card +
/// open the review modal. Walks the schema_definition supersession
/// chain backward to identify the prior schema each YAML was written
/// against (option B from the workstream prompt).
///
/// Returns rows ordered by the flagged contribution's `created_at`
/// descending — newest flags first, which matches the Phase 9 UI
/// pattern of "show the most recent at top of the list."
pub fn list_configs_needing_migration(conn: &Connection) -> Result<Vec<NeedsMigrationEntry>> {
    let mut stmt = conn.prepare(
        "SELECT contribution_id, schema_type, slug, yaml_content, created_at
         FROM pyramid_config_contributions
         WHERE needs_migration = 1
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut entries = Vec::with_capacity(rows.len());
    for (contribution_id, schema_type, slug, current_yaml, created_at) in rows {
        // Resolve the active schema_definition for this schema_type.
        // Phase 9 stores schema_definition rows with `slug =
        // <target_schema_type>`, so the lookup is a direct slug match.
        let current_schema_id: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT contribution_id, triggering_note
                 FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_definition'
                   AND slug = ?1
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                rusqlite::params![schema_type],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .ok();

        let (current_schema_contribution_id, supersession_note) = match current_schema_id {
            Some((id, note)) => (id, note),
            None => {
                // No active schema_definition for this schema_type — the
                // flag is stale (the schema was deleted or never
                // existed). Skip this row rather than fail the whole
                // list — the user sees one less entry but the list
                // still works.
                warn!(
                    contribution_id,
                    schema_type,
                    "list_configs_needing_migration: flagged row has no active schema_definition; skipping"
                );
                continue;
            }
        };

        // Walk the supersession chain backward to find the prior schema
        // definition the YAML was written against. May be None if the
        // contribution was created before any schema supersession.
        let prior_schema_contribution_id =
            find_prior_schema_definition_id(conn, &schema_type, &created_at)?;

        entries.push(NeedsMigrationEntry {
            contribution_id,
            schema_type,
            slug,
            current_yaml,
            current_schema_contribution_id,
            prior_schema_contribution_id,
            flagged_at: created_at,
            supersession_note,
        });
    }

    debug!(
        count = entries.len(),
        "list_configs_needing_migration: returning entries"
    );
    Ok(entries)
}

// ── 2. Propose a config migration (3-phase form) ─────────────────────

/// Inputs loaded from the DB for a migration proposal. Mirrors Phase
/// 9's `GenerationInputs` shape — separated from the LLM call so the
/// IPC handler can drop the DB lock before the await point (rusqlite
/// connections aren't Send across awaits).
#[derive(Debug, Clone)]
pub struct MigrationInputs {
    pub flagged_contribution: ConfigContribution,
    pub skill_body: String,
    pub old_schema_body: Option<String>,
    pub new_schema_body: String,
    pub user_note: Option<String>,
}

/// Load the inputs required for a migration proposal. Runs
/// synchronously inside the DB lock; callers drop the lock before
/// invoking `run_migration_llm_call`.
pub fn load_migration_inputs(
    conn: &Connection,
    contribution_id: &str,
    user_note: Option<&str>,
) -> Result<MigrationInputs> {
    // 1. Load the flagged contribution.
    let flagged = load_contribution_by_id(conn, contribution_id)?
        .ok_or_else(|| anyhow!("contribution {contribution_id} not found"))?;

    if flagged.status != "active" {
        return Err(anyhow!(
            "contribution {contribution_id} has status '{}', not 'active' — cannot migrate",
            flagged.status
        ));
    }

    // 2. Resolve the active schema_definition for this schema_type.
    let new_schema_body: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE schema_type = 'schema_definition'
               AND slug = ?1
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![flagged.schema_type],
            |row| row.get(0),
        )
        .map_err(|_| {
            anyhow!(
                "no active schema_definition contribution for schema_type '{}'",
                flagged.schema_type
            )
        })?;

    // 3. Walk the schema_definition chain backward to find the prior
    //    one the YAML was written against. None is acceptable — the
    //    LLM can still produce a sensible migration when only the new
    //    schema is known (it just gets less guidance about what
    //    changed).
    let prior_schema_id =
        find_prior_schema_definition_id(conn, &flagged.schema_type, &flagged.created_at)?;
    let old_schema_body: Option<String> = if let Some(prior_id) = prior_schema_id {
        conn.query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE contribution_id = ?1",
            rusqlite::params![prior_id],
            |row| row.get(0),
        )
        .ok()
    } else {
        None
    };

    // 4. Load the migration skill body.
    let skill_body = load_migration_skill_body(conn)?;

    Ok(MigrationInputs {
        flagged_contribution: flagged,
        skill_body,
        old_schema_body,
        new_schema_body,
        user_note: user_note.map(|s| s.to_string()),
    })
}

/// Run the LLM call for a migration proposal using loaded inputs. The
/// DB lock is NOT held while this runs (matches the Phase 9 3-phase
/// form). Returns the raw LLM output; the caller parses + persists.
pub async fn run_migration_llm_call(
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    inputs: &MigrationInputs,
) -> Result<String> {
    // The migration prompt expects an `old_schema` placeholder; if no
    // prior schema is known, fall back to a clear message so the LLM
    // knows it's working blind on the upgrade direction.
    let old_schema_for_prompt = inputs
        .old_schema_body
        .as_deref()
        .unwrap_or("(no prior schema available — assume the user's YAML may have been written against any earlier version)");

    let prompt_body = substitute_migration_prompt(
        &inputs.skill_body,
        old_schema_for_prompt,
        &inputs.new_schema_body,
        &inputs.flagged_contribution.yaml_content,
        inputs.user_note.as_deref(),
    );

    // Resolve the synthesis tier — migration is a synthesis task with
    // tight constraints (preserve the user's intent), so we use the
    // same `synth_heavy` tier Phase 9's generation flow uses. Users can
    // refine the migration skill itself to change tiers inline.
    let tier = "synth_heavy";
    let resolved = provider_registry.resolve_tier(tier, None, None, None).ok();
    let (model_id, provider_id) = match resolved {
        Some(entry) => (entry.tier.model_id.clone(), entry.provider.id.clone()),
        None => {
            // TODO(W3/Phase 1): walker v3 — primary_model retires in W3.
            // This fallback branch should resolve the "synth_heavy" tier
            // via a synthetic `DispatchDecision::synthetic_for_preview`
            // against the walker scope chain. Requires threading a
            // `&Connection` (or an ArcSwap<ScopeCache> handle) into this
            // helper — a W3 task since it coincides with field deletion.
            warn!(
                tier,
                "run_migration_llm_call: tier not resolved via registry; falling back to llm_config.primary_model"
            );
            (llm_config.primary_model.clone(), "openrouter".to_string())
        }
    };

    let build_id = format!("migrate-{}", uuid::Uuid::new_v4());
    let prompt_hash = compute_prompt_hash(&inputs.skill_body);
    let ctx = StepContext::new(
        inputs
            .flagged_contribution
            .slug
            .as_deref()
            .unwrap_or("global"),
        build_id.clone(),
        "migrate_config",
        "config_migration",
        0,
        None,
        db_path,
    )
    .with_model_resolution(tier, model_id)
    .with_provider(provider_id)
    .with_prompt_hash(prompt_hash)
    .with_bus(bus.clone());

    debug!(
        contribution_id = %inputs.flagged_contribution.contribution_id,
        schema_type = %inputs.flagged_contribution.schema_type,
        build_id = %build_id,
        "run_migration_llm_call: calling LLM via cache-aware path"
    );

    let response = call_model_unified_with_options_and_ctx(
        llm_config,
        Some(&ctx),
        "You are a configuration migrator for Wire Node.",
        &prompt_body,
        0.2,
        4096,
        None,
        LlmCallOptions::default(),
    )
    .await?;

    Ok(response.content)
}

/// Persist a migration proposal as a draft contribution. Runs inside
/// the writer DB lock after the LLM call completes.
///
/// The draft row uses:
///   * `status = 'draft'` — never auto-applied; user must accept
///   * `source = 'migration'` — distinct from `local` so the audit log
///     can identify migration drafts
///   * `supersedes_id = <flagged_contribution_id>` — chains the draft
///     to the row it would replace on accept
///   * `triggering_note` carries the user_note (or a default if absent)
pub fn persist_migration_proposal(
    conn: &mut Connection,
    inputs: &MigrationInputs,
    llm_output: &str,
    bus: &Arc<BuildEventBus>,
) -> Result<MigrationProposal> {
    let new_yaml = extract_yaml_body(llm_output);

    // Best-effort YAML parse — same safety net Phase 9 uses. Full
    // JSON Schema validation against the new schema would be ideal but
    // adds a `jsonschema` crate dependency that's not in the workspace.
    let _: serde_yaml::Value = serde_yaml::from_str(&new_yaml)
        .map_err(|e| anyhow!("migrated YAML is not parseable: {e}; body: {new_yaml}"))?;

    let triggering_note = inputs
        .user_note
        .clone()
        .unwrap_or_else(|| {
            format!(
                "LLM-assisted migration of {} from prior schema",
                inputs.flagged_contribution.schema_type
            )
        });

    if triggering_note.trim().is_empty() {
        return Err(anyhow!("triggering_note must not be empty"));
    }

    // Carry forward the flagged contribution's canonical metadata with
    // maturity reset to Draft. Falls back to the schema-type default if
    // the prior metadata is missing or unparseable.
    let mut new_metadata =
        crate::pyramid::wire_native_metadata::WireNativeMetadata::from_json(
            &inputs.flagged_contribution.wire_native_metadata_json,
        )
        .unwrap_or_else(|_| {
            default_wire_native_metadata(
                &inputs.flagged_contribution.schema_type,
                inputs.flagged_contribution.slug.as_deref(),
            )
        });
    new_metadata.maturity = WireMaturity::Draft;

    let metadata_json = new_metadata
        .to_json()
        .map_err(|e| anyhow!("failed to serialize wire_native_metadata: {e}"))?;

    let draft_id = uuid::Uuid::new_v4().to_string();

    // Insert the draft row inside a transaction so the contribution
    // lands atomically. We do NOT flip the flagged row's status here —
    // the original stays active until the user accepts the migration,
    // matching Phase 9's draft-during-refinement semantics so background
    // loops still see the prior policy.
    //
    // Phase 18d wanderer fix: if a prior migration draft already exists
    // for this flagged contribution (the user clicked "Re-propose with
    // guidance" one or more times), delete it before inserting the new
    // one. Without this, stale drafts accumulate: each retry inserts a
    // fresh row but the frontend only tracks the latest draft_id, so
    // the older rows sit in pyramid_config_contributions forever with
    // status='draft', source='migration'. Guarded by the same filter
    // `reject_config_migration` uses (status='draft' AND
    // source='migration') so we can only delete our own drafts.
    //
    // Phase 0a-1 commit 5 / §2.16.1: BEGIN IMMEDIATE so migration
    // draft persistence serializes on write intent against concurrent
    // supersessions.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let stale_drafts_deleted = tx.execute(
        "DELETE FROM pyramid_config_contributions
         WHERE supersedes_id = ?1
           AND status = 'draft'
           AND source = 'migration'",
        rusqlite::params![inputs.flagged_contribution.contribution_id],
    )?;
    if stale_drafts_deleted > 0 {
        debug!(
            flagged_contribution_id = %inputs.flagged_contribution.contribution_id,
            stale_drafts_deleted,
            "persist_migration_proposal: replaced prior migration drafts"
        );
    }
    crate::pyramid::config_contributions::write_contribution_envelope(
        &tx,
        crate::pyramid::config_contributions::ContributionEnvelopeInput {
            contribution_id: draft_id.clone(),
            slug: inputs.flagged_contribution.slug.clone(),
            schema_type: inputs.flagged_contribution.schema_type.clone(),
            body: new_yaml.clone(),
            wire_native_metadata_json: Some(metadata_json),
            supersedes_id: Some(inputs.flagged_contribution.contribution_id.clone()),
            triggering_note: Some(triggering_note.to_string()),
            status: "draft".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: Some("migration_proposal".to_string()),
            accepted_at: crate::pyramid::config_contributions::AcceptedAt::Null,
            needs_migration: Some(0),
            write_mode: crate::pyramid::config_contributions::WriteMode::default(),
        },
        crate::pyramid::config_contributions::TransactionMode::JoinAmbient,
    )?;
    tx.commit()?;

    // Emit a ConfigMigrationProposed event for the DADBEAR Oversight
    // page and any UI listening. Phase 18d adds the variant.
    let envelope_slug = inputs
        .flagged_contribution
        .slug
        .clone()
        .unwrap_or_default();
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: envelope_slug,
        kind: TaggedKind::ConfigMigrationProposed {
            slug: inputs.flagged_contribution.slug.clone(),
            schema_type: inputs.flagged_contribution.schema_type.clone(),
            flagged_contribution_id: inputs.flagged_contribution.contribution_id.clone(),
            draft_contribution_id: draft_id.clone(),
        },
    });

    info!(
        flagged_contribution_id = %inputs.flagged_contribution.contribution_id,
        draft_id = %draft_id,
        schema_type = %inputs.flagged_contribution.schema_type,
        "persist_migration_proposal: created migration draft"
    );

    let old_schema_body = inputs
        .old_schema_body
        .clone()
        .unwrap_or_else(|| "(no prior schema available)".to_string());

    Ok(MigrationProposal {
        draft_id,
        old_yaml: inputs.flagged_contribution.yaml_content.clone(),
        new_yaml,
        schema_type: inputs.flagged_contribution.schema_type.clone(),
        schema_from: old_schema_body,
        schema_to: inputs.new_schema_body.clone(),
    })
}

// ── 3. Accept a migration draft ──────────────────────────────────────

/// Accept a migration draft. Promotes the draft to active, supersedes
/// the original flagged contribution, runs sync_config_to_operational
/// so the operational table picks up the migrated YAML, and clears the
/// `needs_migration` flag on the new active row (it's freshly valid
/// against the new schema).
///
/// The supersession + flag clear run inside a single transaction so
/// a mid-flight failure can't leave the contribution table in a
/// half-updated state. sync_config_to_operational runs AFTER that
/// transaction commits — intentionally: the dispatcher opens its own
/// inner transactions (e.g. step_overrides DELETE+INSERT bundles), and
/// we want sync failures to log a warning rather than roll the accept
/// back. Rolling the accept back on a sync failure is the wrong move:
/// the user has already approved the migrated YAML, and an out-of-date
/// operational table can be recovered by a re-sync, while losing the
/// accept forces the user through the LLM round-trip again for no
/// benefit.
pub fn accept_config_migration(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    schema_registry: &Arc<SchemaRegistry>,
    draft_id: &str,
    accept_note: Option<&str>,
) -> Result<AcceptMigrationOutcome> {
    // Load the draft row first so we can validate it before opening
    // the transaction. (We can't keep a borrow on `conn` while opening
    // a transaction.)
    let draft = load_contribution_by_id(conn, draft_id)?
        .ok_or_else(|| anyhow!("migration draft {draft_id} not found"))?;

    if draft.status != "draft" {
        return Err(anyhow!(
            "contribution {draft_id} has status '{}', not 'draft' — cannot accept migration",
            draft.status
        ));
    }
    if draft.source != "migration" {
        return Err(anyhow!(
            "contribution {draft_id} has source '{}', not 'migration' — refuse to accept via the migration path",
            draft.source
        ));
    }

    let prior_id = draft
        .supersedes_id
        .clone()
        .ok_or_else(|| anyhow!("migration draft {draft_id} has no supersedes_id (corrupt draft)"))?;

    // Resolve the note now so we can fail-fast on empty notes before
    // opening the transaction.
    let note = accept_note
        .map(|s| s.to_string())
        .or_else(|| draft.triggering_note.clone())
        .unwrap_or_else(|| format!("Accepted migration of {}", draft.schema_type));
    if note.trim().is_empty() {
        return Err(anyhow!("accept_note must not be empty"));
    }

    // Open a transaction for the supersession + flag clear.
    //
    // Phase 0a-1 commit 5 / §2.16.1: BEGIN IMMEDIATE so
    // accept-migration serializes on write intent against concurrent
    // supersessions.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Confirm the prior is still the active row for this (schema_type,
    // slug). If something else superseded it between propose and
    // accept, we abort — the user needs to re-propose against the new
    // current.
    let prior_active_check: Option<String> = if let Some(slug_val) = draft.slug.as_deref() {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug = ?1 AND schema_type = ?2
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![slug_val, draft.schema_type],
            |row| row.get(0),
        )
        .ok()
    } else {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug IS NULL AND schema_type = ?1
               AND status = 'active' AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![draft.schema_type],
            |row| row.get(0),
        )
        .ok()
    };

    if prior_active_check.as_deref() != Some(prior_id.as_str()) {
        return Err(anyhow!(
            "the configuration was modified after the migration was proposed (current active is {:?}, draft expected {}); please re-propose the migration",
            prior_active_check,
            prior_id
        ));
    }

    // Phase 0a-1 commit 5: mark prior superseded BEFORE promoting the
    // draft so `uq_config_contrib_active` never sees two active rows
    // for the same (schema_type, slug). The back-link
    // (`superseded_by_id = draft_id`) is written in this same UPDATE.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'superseded',
             superseded_by_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![draft_id, prior_id],
    )?;

    // Promote the draft to active. Carry the accept note in
    // triggering_note so the supersession chain records the user's
    // accept rationale.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'active',
             accepted_at = datetime('now'),
             needs_migration = 0,
             triggering_note = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![note, draft_id],
    )?;

    tx.commit()?;

    // Re-load the now-active contribution to feed into
    // sync_config_to_operational. This needs the writer outside any
    // transaction so the dispatcher can run its own internal
    // transactions (e.g. step_overrides DELETE+INSERT bundle).
    let promoted = load_contribution_by_id(conn, draft_id)?
        .ok_or_else(|| anyhow!("migration draft {draft_id} disappeared after promotion"))?;

    let sync_succeeded =
        match sync_config_to_operational_with_registry(conn, bus, &promoted, Some(schema_registry))
        {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    contribution_id = draft_id,
                    error = %e,
                    "accept_config_migration: sync_config_to_operational_with_registry failed; the contribution is active in the contribution table but the operational sink may be out of date"
                );
                false
            }
        };

    // Emit a ConfigMigrationAccepted event for the oversight page.
    let envelope_slug = draft.slug.clone().unwrap_or_default();
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: envelope_slug,
        kind: TaggedKind::ConfigMigrationAccepted {
            slug: draft.slug.clone(),
            schema_type: draft.schema_type.clone(),
            new_contribution_id: draft_id.to_string(),
            superseded_contribution_id: prior_id,
        },
    });

    info!(
        contribution_id = draft_id,
        schema_type = %draft.schema_type,
        sync_succeeded,
        "accept_config_migration: migration accepted and synced"
    );

    Ok(AcceptMigrationOutcome {
        new_contribution_id: draft_id.to_string(),
        schema_type: draft.schema_type,
        slug: draft.slug,
        sync_succeeded,
    })
}

// ── 4. Reject a migration draft ──────────────────────────────────────

/// Delete a migration draft. The original flagged contribution is left
/// untouched (still active, still flagged) so the user can re-propose
/// later or migrate manually via the existing edit flow.
///
/// Refuses to delete anything that isn't a `status = 'draft'` AND
/// `source = 'migration'` row, so a buggy frontend can't use this IPC
/// to delete random contributions.
pub fn reject_config_migration(
    conn: &Connection,
    draft_id: &str,
) -> Result<RejectMigrationOutcome> {
    let draft = load_contribution_by_id(conn, draft_id)?
        .ok_or_else(|| anyhow!("migration draft {draft_id} not found"))?;

    if draft.status != "draft" {
        return Err(anyhow!(
            "contribution {draft_id} has status '{}', not 'draft' — refuse to delete",
            draft.status
        ));
    }
    if draft.source != "migration" {
        return Err(anyhow!(
            "contribution {draft_id} has source '{}', not 'migration' — refuse to delete via the migration reject path",
            draft.source
        ));
    }

    let original_id = draft
        .supersedes_id
        .clone()
        .ok_or_else(|| anyhow!("migration draft {draft_id} has no supersedes_id (corrupt draft)"))?;

    conn.execute(
        "DELETE FROM pyramid_config_contributions WHERE contribution_id = ?1",
        rusqlite::params![draft_id],
    )?;

    info!(
        draft_id,
        original_id,
        "reject_config_migration: draft deleted, original left untouched"
    );

    Ok(RejectMigrationOutcome {
        deleted_draft_id: draft_id.to_string(),
        original_contribution_id: original_id,
    })
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::config_contributions::create_config_contribution_with_metadata;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::schema_registry::flag_configs_needing_migration;
    use crate::pyramid::wire_migration::walk_bundled_contributions_manifest;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Bundled manifest gives us the migration prompt skill +
        // schema_definition rows for evidence_policy etc.
        walk_bundled_contributions_manifest(&conn).unwrap();
        conn
    }

    fn seed_user_evidence_policy(conn: &Connection) -> String {
        let mut metadata =
            crate::pyramid::wire_native_metadata::default_wire_native_metadata(
                "evidence_policy",
                Some("test-pyramid"),
            );
        metadata.maturity = WireMaturity::Canon;
        create_config_contribution_with_metadata(
            conn,
            "evidence_policy",
            Some("test-pyramid"),
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget: {}\n",
            Some("user wrote this against the bundled schema"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap()
    }

    /// Helper: replace the active schema_definition for a target
    /// schema_type with a new contribution row, marking the old one
    /// superseded the same way Phase 4's dispatcher does. Used to
    /// simulate a schema_definition supersession in the chain walk
    /// tests without going through the full pyramid_supersede_config
    /// IPC path.
    fn supersede_schema_definition(
        conn: &Connection,
        target_schema_type: &str,
        new_body: &str,
    ) -> String {
        // Find the current active schema_definition for this target.
        let prior_id: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_definition'
                   AND slug = ?1
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                rusqlite::params![target_schema_type],
                |row| row.get(0),
            )
            .unwrap();

        let mut metadata = default_wire_native_metadata("schema_definition", Some(target_schema_type));
        metadata.maturity = WireMaturity::Canon;

        // Sleep a single millisecond so the new row's `created_at`
        // (datetime('now') has second precision) sorts strictly after
        // the prior. Without this the schema chain walk's
        // `created_at < ?` comparison can be ambiguous on fast
        // execution paths.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Phase 0a-1 commit 5 test fixture: flip prior to superseded
        // BEFORE creating the new active row so
        // `uq_config_contrib_active` does not reject the INSERT. The
        // `superseded_by_id` back-link is patched in after.
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1",
            rusqlite::params![prior_id],
        )
        .unwrap();

        let new_id =
            create_config_contribution_with_metadata(
                conn,
                "schema_definition",
                Some(target_schema_type),
                new_body,
                Some("test supersession"),
                "local",
                Some("test"),
                "active",
                &metadata,
            )
            .unwrap();

        conn.execute(
            "UPDATE pyramid_config_contributions
             SET superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior_id],
        )
        .unwrap();

        new_id
    }

    #[test]
    fn test_list_returns_only_flagged_active_rows() {
        let conn = mem_conn();
        // Bundled manifest seeded one active evidence_policy with
        // needs_migration = 0. Flagging downstream configs of
        // 'evidence_policy' should leave the bundled default in
        // the list (it's the only active row).
        let _ = seed_user_evidence_policy(&conn);

        // Pre-flag: list should be empty.
        let entries = list_configs_needing_migration(&conn).unwrap();
        assert_eq!(entries.len(), 0);

        // Flag everything for evidence_policy.
        let flagged = flag_configs_needing_migration(&conn, "evidence_policy").unwrap();
        assert!(flagged >= 1, "should flag at least one row");

        // Now the list should include both the bundled default AND the
        // user-seeded row, in newest-first order.
        let entries = list_configs_needing_migration(&conn).unwrap();
        assert!(entries.len() >= 2);
        assert!(entries
            .iter()
            .all(|e| e.schema_type == "evidence_policy"));
    }

    #[test]
    fn test_list_skips_drafts_and_superseded_rows() {
        let conn = mem_conn();
        let _ = seed_user_evidence_policy(&conn);

        // Insert a draft row directly to verify it's skipped.
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET needs_migration = 1
             WHERE schema_type = 'evidence_policy' AND status = 'active'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at,
                needs_migration
             ) VALUES (
                'draft-row', 'test-pyramid', 'evidence_policy',
                'schema_type: evidence_policy\n',
                '{}', '{}', NULL, NULL, 'draft for test',
                'draft', 'local', NULL, 'test', NULL, 1
             )",
            [],
        )
        .unwrap();

        let entries = list_configs_needing_migration(&conn).unwrap();
        assert!(entries.iter().all(|e| e.contribution_id != "draft-row"));
    }

    #[test]
    fn test_propose_creates_draft_with_correct_lineage() {
        let mut conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);

        // Supersede the bundled schema_definition to create a chain.
        let _new_schema_id = supersede_schema_definition(
            &conn,
            "evidence_policy",
            "{\"type\":\"object\",\"required\":[\"schema_type\",\"triage_rules\"],\"properties\":{\"schema_type\":{\"const\":\"evidence_policy\"},\"triage_rules\":{\"type\":\"array\"}}}",
        );

        // Flag the user's row.
        flag_configs_needing_migration(&conn, "evidence_policy").unwrap();

        // Load inputs (this is what the IPC handler calls before the
        // LLM round-trip).
        let inputs = load_migration_inputs(&conn, &user_id, Some("just a test")).unwrap();
        assert_eq!(inputs.flagged_contribution.contribution_id, user_id);
        assert!(inputs.old_schema_body.is_some(),
            "chain walk should find the bundled (now-superseded) schema");
        assert!(!inputs.new_schema_body.is_empty());
        assert!(inputs.skill_body.contains("migrating"));

        // Persist the proposal directly with a stub LLM output.
        let bus = Arc::new(BuildEventBus::new());
        let stub_output =
            "schema_type: evidence_policy\ntriage_rules:\n  - condition: \"first_build\"\n    action: answer\n";
        let proposal = persist_migration_proposal(&mut conn, &inputs, stub_output, &bus).unwrap();

        // Verify the draft row.
        let draft = load_contribution_by_id(&conn, &proposal.draft_id)
            .unwrap()
            .unwrap();
        assert_eq!(draft.status, "draft");
        assert_eq!(draft.source, "migration");
        assert_eq!(draft.supersedes_id.as_deref(), Some(user_id.as_str()));
        assert_eq!(draft.schema_type, "evidence_policy");

        // The original row stays active and stays flagged until accept.
        let original = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();
        assert_eq!(original.status, "active");
    }

    #[test]
    fn test_propose_rejects_non_active_contribution() {
        let conn = mem_conn();
        // Create a draft row that should be refused.
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at,
                needs_migration
             ) VALUES (
                'a-draft', 'foo', 'evidence_policy',
                'schema_type: evidence_policy\n',
                '{}', '{}', NULL, NULL, 'a draft',
                'draft', 'local', NULL, 'test', NULL, 0
             )",
            [],
        )
        .unwrap();

        let err = load_migration_inputs(&conn, "a-draft", None).unwrap_err();
        assert!(err.to_string().contains("not 'active'"));
    }

    #[test]
    fn test_accept_supersedes_and_clears_flag() {
        let mut conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);
        flag_configs_needing_migration(&conn, "evidence_policy").unwrap();

        // Build a synthetic draft row that supersedes the user's row.
        let inputs = load_migration_inputs(&conn, &user_id, None).unwrap();
        let bus = Arc::new(BuildEventBus::new());
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let stub_output =
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget: {}\n";
        let proposal = persist_migration_proposal(&mut conn, &inputs, stub_output, &bus).unwrap();

        let outcome = accept_config_migration(
            &mut conn,
            &bus,
            &registry,
            &proposal.draft_id,
            Some("looks good, ship it"),
        )
        .unwrap();

        assert_eq!(outcome.new_contribution_id, proposal.draft_id);
        assert!(outcome.sync_succeeded);

        // The new row is active with needs_migration = 0.
        let new_row = load_contribution_by_id(&conn, &proposal.draft_id)
            .unwrap()
            .unwrap();
        assert_eq!(new_row.status, "active");
        let needs_migration: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![&proposal.draft_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(needs_migration, 0);

        // The original is now superseded.
        let original = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();
        assert_eq!(original.status, "superseded");
        assert_eq!(
            original.superseded_by_id.as_deref(),
            Some(proposal.draft_id.as_str())
        );
    }

    #[test]
    fn test_reject_deletes_draft_and_leaves_original() {
        let mut conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);
        flag_configs_needing_migration(&conn, "evidence_policy").unwrap();

        let inputs = load_migration_inputs(&conn, &user_id, None).unwrap();
        let bus = Arc::new(BuildEventBus::new());
        let stub_output =
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget: {}\n";
        let proposal = persist_migration_proposal(&mut conn, &inputs, stub_output, &bus).unwrap();

        let outcome = reject_config_migration(&conn, &proposal.draft_id).unwrap();
        assert_eq!(outcome.deleted_draft_id, proposal.draft_id);
        assert_eq!(outcome.original_contribution_id, user_id);

        // The draft row is gone.
        let absent = load_contribution_by_id(&conn, &proposal.draft_id).unwrap();
        assert!(absent.is_none());

        // The original is still active and still flagged.
        let original = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();
        assert_eq!(original.status, "active");
        let needs_migration: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![&user_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(needs_migration, 1);
    }

    #[test]
    fn test_reject_refuses_non_migration_drafts() {
        let conn = mem_conn();
        // Use the bundled evidence_policy default as a real parent so
        // the FK constraint is satisfied.
        let parent_id =
            crate::pyramid::config_contributions::load_active_config_contribution(
                &conn,
                "evidence_policy",
                None,
            )
            .unwrap()
            .unwrap()
            .contribution_id;

        // A draft row with source = local should be refused.
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at,
                needs_migration
             ) VALUES (
                'local-draft', 'foo', 'evidence_policy',
                'schema_type: evidence_policy\n',
                '{}', '{}', ?1, NULL, 'a local draft',
                'draft', 'local', NULL, 'test', NULL, 0
             )",
            rusqlite::params![parent_id],
        )
        .unwrap();

        let err = reject_config_migration(&conn, "local-draft").unwrap_err();
        assert!(err.to_string().contains("source 'local'"));
    }

    #[test]
    fn test_chain_walk_finds_prior_schema() {
        let conn = mem_conn();
        // Bundled evidence_policy schema_definition exists. The user's
        // contribution was written against it. After supersession,
        // chain walk should resolve back to the bundled (now superseded)
        // row.
        let user_id = seed_user_evidence_policy(&conn);
        let _new_id = supersede_schema_definition(
            &conn,
            "evidence_policy",
            "{\"type\":\"object\",\"required\":[\"schema_type\",\"triage_rules\"]}",
        );

        let user_row = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();
        let prior =
            find_prior_schema_definition_id(&conn, "evidence_policy", &user_row.created_at)
                .unwrap();
        assert!(
            prior.is_some(),
            "chain walk should find the original bundled schema_definition"
        );
    }

    /// Phase 18d wanderer fix: cover the "no prior schema" case that
    /// test_chain_walk_finds_prior_schema didn't exercise. When the
    /// config was created against the very first schema_definition and
    /// no supersession has ever happened, find_prior_schema_definition_id
    /// must return None gracefully instead of erroring.
    #[test]
    fn test_chain_walk_returns_none_when_no_prior_exists() {
        let conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);
        // Intentionally do NOT supersede the schema — the bundled default
        // is still active, so there is no superseded row to find.
        let user_row = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();
        let prior =
            find_prior_schema_definition_id(&conn, "evidence_policy", &user_row.created_at)
                .unwrap();
        assert!(
            prior.is_none(),
            "chain walk must return None when no superseded schema predates the config"
        );
    }

    /// Phase 18d wanderer fix: confirm the chain walk is deterministic
    /// under timestamp collision. SQLite's `datetime('now')` writes
    /// second precision. If two schema_definitions land in the same
    /// second, the `ORDER BY created_at DESC, id DESC` tiebreaker must
    /// still pick a single row (not error, not swap across runs). We
    /// seed two superseded schemas with the same created_at and assert
    /// that find_prior_schema_definition_id returns one of them.
    #[test]
    fn test_chain_walk_tiebreaker_under_timestamp_collision() {
        let conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);
        let user_row = load_contribution_by_id(&conn, &user_id).unwrap().unwrap();

        // Insert two synthetic superseded schema_definition rows with
        // identical created_at timestamps (matching the user's row's
        // timestamp so the `<=` predicate includes them). These simulate
        // two back-to-back supersessions within the same SQLite second.
        let colliding_ts = user_row.created_at.clone();
        let ids = ["collision-a", "collision-b"];
        for id in &ids {
            conn.execute(
                "INSERT INTO pyramid_config_contributions (
                    contribution_id, slug, schema_type, yaml_content,
                    wire_native_metadata_json, wire_publication_state_json,
                    supersedes_id, superseded_by_id, triggering_note,
                    status, source, wire_contribution_id, created_by,
                    accepted_at, needs_migration, created_at
                 ) VALUES (
                    ?1, 'evidence_policy', 'schema_definition',
                    '{\"type\":\"object\"}',
                    '{}', '{}', NULL, 'some-later-superseder', 'collision test',
                    'superseded', 'local', NULL, 'test', NULL, 0, ?2
                 )",
                rusqlite::params![id, colliding_ts],
            )
            .unwrap();
        }

        // The walk must pick exactly one row deterministically. We don't
        // care which — only that it resolves without erroring and
        // returns a value stable across runs (SQLite's id ordering is
        // deterministic for a given dataset).
        let first = find_prior_schema_definition_id(&conn, "evidence_policy", &colliding_ts)
            .unwrap();
        let second = find_prior_schema_definition_id(&conn, "evidence_policy", &colliding_ts)
            .unwrap();
        assert!(first.is_some(), "walk must succeed under collision");
        assert_eq!(first, second, "walk must be deterministic");
    }

    /// Phase 18d wanderer fix: repeated proposes (the "Re-propose with
    /// guidance" flow) must REPLACE the prior draft, not accumulate.
    /// Without this, every retry stranded another draft row in the
    /// contribution table because the frontend only tracks the latest
    /// draft_id.
    #[test]
    fn test_repeated_propose_replaces_prior_draft() {
        let mut conn = mem_conn();
        let user_id = seed_user_evidence_policy(&conn);
        flag_configs_needing_migration(&conn, "evidence_policy").unwrap();
        let bus = Arc::new(BuildEventBus::new());

        let inputs_first = load_migration_inputs(&conn, &user_id, Some("first note")).unwrap();
        let first_proposal = persist_migration_proposal(
            &mut conn,
            &inputs_first,
            "schema_type: evidence_policy\ntriage_rules: []\ndemand_signals: []\nbudget: {}\n",
            &bus,
        )
        .unwrap();

        // A second propose for the same flagged contribution (simulating
        // "Re-propose with guidance") should DELETE the prior draft and
        // insert a new one.
        let inputs_second =
            load_migration_inputs(&conn, &user_id, Some("refined note")).unwrap();
        let second_proposal = persist_migration_proposal(
            &mut conn,
            &inputs_second,
            "schema_type: evidence_policy\ntriage_rules:\n  - condition: x\n",
            &bus,
        )
        .unwrap();

        assert_ne!(first_proposal.draft_id, second_proposal.draft_id,
            "each propose must mint a fresh draft_id");

        // Only ONE draft row for this flagged contribution should remain
        // after the second propose, and it must be the second one.
        let drafts: Vec<String> = conn
            .prepare(
                "SELECT contribution_id FROM pyramid_config_contributions
                 WHERE supersedes_id = ?1
                   AND status = 'draft'
                   AND source = 'migration'",
            )
            .unwrap()
            .query_map(rusqlite::params![user_id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(drafts.len(), 1, "stale draft must be deleted on re-propose");
        assert_eq!(drafts[0], second_proposal.draft_id);

        // The first draft row should be gone.
        let first_exists = load_contribution_by_id(&conn, &first_proposal.draft_id).unwrap();
        assert!(first_exists.is_none(),
            "first propose draft must be deleted by the second");
    }

    #[test]
    fn test_load_migration_skill_finds_bundled_prompt() {
        let conn = mem_conn();
        let body = load_migration_skill_body(&conn).unwrap();
        assert!(body.contains("migrating"));
        assert!(body.contains("{old_schema}"));
        assert!(body.contains("{new_schema}"));
        assert!(body.contains("{old_yaml}"));
    }

    #[test]
    fn test_substitute_migration_prompt_with_note() {
        let template = "OLD: {old_schema}\nNEW: {new_schema}\nYAML: {old_yaml}\n{if user_note}NOTE: {user_note}{end}";
        let out = substitute_migration_prompt(
            template,
            "old-s",
            "new-s",
            "y: 1",
            Some("preserve x"),
        );
        assert!(out.contains("OLD: old-s"));
        assert!(out.contains("NEW: new-s"));
        assert!(out.contains("YAML: y: 1"));
        assert!(out.contains("NOTE: preserve x"));
    }

    #[test]
    fn test_substitute_migration_prompt_without_note() {
        let template = "OLD: {old_schema}\n{if user_note}NOTE: {user_note}{end}\nEND";
        let out = substitute_migration_prompt(template, "old-s", "new-s", "y: 1", None);
        assert!(!out.contains("NOTE"));
        assert!(out.contains("END"));
    }
}
