// Walker v3 — Phase A DDL migration + Phase B config-file rewrite
// (plan rev 1.0.2 §5.3).
//
// Absorbs the legacy `pyramid_tier_routing` table, the active
// `dispatch_policy` contribution's `routing_rules.route_to`, and the
// operator's `pyramid_config.json` primary/fallback model fields into
// walker v3's scope-4 contribution graph:
//
//   * `walker_provider_openrouter`   (per-provider carrier; scope 4)
//   * `walker_provider_local`        (per-provider carrier; scope 4)
//   * `walker_provider_fleet`        (per-provider carrier; scope 4)
//   * `walker_provider_market`       (per-provider carrier; scope 4)
//   * `walker_call_order`            (per-call-order carrier; scope 3)
//
// Phase A runs inside ONE SQL transaction. Phase B (config-file rewrite)
// is a separate workstream (W4) — Phase A reads pyramid_config.json but
// does not mutate it.
//
// Plan anchors:
//   §2.11  schema_annotation-shape validation at envelope writer. This
//          module writes bodies that pass that gate; unknown `_`-prefixed
//          keys (`_notes`) are skipped by the validator.
//   §2.14.3 schema evolution — new parameter keys require catalog +
//          accessor. Handled in the walker_resolver.rs changes that
//          land alongside this module (W1a).
//   §2.16.1 single-active-contribution invariant — the ambient SQL tx
//          here uses `TransactionMode::JoinAmbient` for any envelope-
//          writer calls inside.
//   §2.17   boot and init order — this module is the implementation
//          behind boot.rs step 4.
//   §2.17.3 boot aborts to known states — the error types here are
//          what boot.rs translates into operator-visible modals.
//   §5.1    retires table — authoritative column → (walker_provider_*)
//          overrides map.
//   §5.3    ONE SQL transaction; pre-migration snapshot; GROUP BY
//          provider_id; unknown-provider hard-fail; in-progress-build
//          refusal; marker supersession v2 → v3-db-migrated-config-pending.
//   §5.5.9  `_pre_v3_snapshot_*` tables retention semantics (30d
//          auto-prune lives in a separate janitor, not here).
//
// Strict non-goals (by design, other workstreams):
//   * Does NOT rewrite pyramid_config.json (W4 / Phase B).
//   * Does NOT delete legacy `primary_model` / `fallback_model_{1,2}`
//     fields from PyramidConfig (W3).
//   * Does NOT migrate the 203 consumer sites (W2).
//   * Does NOT implement Phase-6 UI modals (unknown-provider
//     acknowledgment / in-flight-build gate).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};

use crate::pyramid::config_contributions::{
    ensure_config_contrib_active_unique_index, AcceptedAt, ContributionEnvelopeInput,
    TransactionMode, WriteMode,
};
use crate::pyramid::walker_resolver::ProviderType;

/// Per-boot successful Phase-A migration report. Surfaces what landed
/// so boot.rs can log contribution_ids + confirm the marker transition.
#[derive(Debug, Clone, Default)]
pub struct V3MigrationReport {
    /// `contribution_id`s of the walker_provider_* rows this pass wrote.
    /// Empty when `pyramid_tier_routing` had no legacy rows.
    pub walker_provider_contributions_written: Vec<String>,
    /// `contribution_id` of the walker_call_order row written from the
    /// legacy dispatch_policy's routing_rules. `None` when the legacy
    /// contribution had no active row — the bundled walker_call_order
    /// default stays active.
    pub walker_call_order_written: Option<String>,
    /// walker_slot_policy starts empty per §5.3 step 5. Always `None`
    /// today; kept in the report shape so future phases can populate.
    pub walker_slot_policy_written: Option<String>,
    /// Count of rows dumped into `_pre_v3_snapshot_pyramid_tier_routing`.
    pub snapshot_rows_dumped: usize,
    /// Body of the prior migration_marker contribution
    /// (e.g. "v2" or "" for missing).
    pub marker_transitioned_from: String,
    /// Always `"v3-db-migrated-config-pending"` on success.
    pub marker_transitioned_to: String,
}

/// Failure modes for `run_v3_phase_a_migration`. Each variant maps
/// directly to a distinct operator-visible modal once Phase-6 UI lands;
/// the boot coordinator (boot.rs step 4) translates these into
/// `BootResult::Aborted` messages in the meantime.
#[derive(Debug, thiserror::Error)]
pub enum V3MigrationError {
    /// `pyramid_tier_routing` rows reference provider_ids the v3
    /// migration doesn't know how to place. Recovery modal listing the
    /// unknown ids is Phase-6 UI work; here we hard-fail the migration
    /// so nothing partial commits.
    #[error("unknown provider_ids in pyramid_tier_routing: {ids:?}")]
    UnknownProviderIds { ids: Vec<String> },
    /// The active `migration_marker` contribution already records a
    /// post-v2 body (e.g. `v3-db-migrated-config-pending` or `v3`). The
    /// caller should skip Phase A; idempotent re-runs return this, not
    /// silently re-migrate.
    #[error(
        "migration_marker body `{body}` — Phase A already ran (expected `v2` or missing)"
    )]
    AlreadyMigrated { body: String },
    /// Active builds block the migration (§2.16.4). Boot coordinator
    /// surfaces the recovery modal; this variant carries the slug set so
    /// the operator can see which builds to resume / mark failed.
    #[error("in-progress builds block migration: {0:?}")]
    InProgressBuildsBlock(Vec<String>),
    /// The `_pre_v3_snapshot_*` setup failed (DDL or INSERT). Treated as
    /// fatal — the migration cannot proceed without a rollback target.
    #[error("snapshot creation failed: {0}")]
    SnapshotFailed(#[source] rusqlite::Error),
    /// SQLite error outside the snapshot path.
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Envelope-writer / anyhow-flavored errors from the contribution
    /// writes inside the transaction.
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Phase A entry point. Runs inside ONE SQL transaction; returns
/// `V3MigrationReport` on success or a typed error the boot coordinator
/// can map to the right operator modal.
///
/// Pre-conditions (all checked by this function):
///   * No `pyramid_builds` row has status `running` or `paused_for_resume`.
///   * The active `migration_marker` body is `v2` or the row is absent.
///   * Every `pyramid_tier_routing.provider_id` value is one of the
///     known strings (`openrouter` / `ollama` / `ollama-local` /
///     `fleet` / `market`). Otherwise: hard-fail with UnknownProviderIds.
///
/// `data_dir` is the path where `pyramid_config.json` lives — usually
/// `AppConfig::data_dir()`. Pass `None` when `pyramid_config.json` is
/// unavailable (tests); the config-fold step silently skips in that
/// case.
pub fn run_v3_phase_a_migration(
    conn: &mut Connection,
    data_dir: Option<&Path>,
) -> std::result::Result<V3MigrationReport, V3MigrationError> {
    // Ensure the unique-active index has been applied BEFORE we open
    // the migration transaction. This is idempotent (Phase 0a-1 commit
    // 5 short-circuits on the sqlite_master existence check) and runs
    // inside its own transaction — running it underneath our
    // `BEGIN IMMEDIATE` below would nest transactions.
    ensure_config_contrib_active_unique_index(conn)
        .map_err(|e| V3MigrationError::Other(anyhow::anyhow!("ensure_unique_index: {e}")))?;

    // Open the single SQL transaction for Phase A.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // ── 0. Idempotency: refuse rerun on already-migrated DB. ─────────
    //
    // Read active `migration_marker`. Body `v2` (or no active row, i.e.
    // fresh first-boot of v3 binary) is the only set that can proceed.
    // Everything else returns AlreadyMigrated so the caller can skip.
    let marker_body: Option<String> = tx
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE schema_type = 'migration_marker'
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let prior_marker_contribution_id: Option<String> = tx
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = 'migration_marker'
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let marker_from_body = marker_body
        .as_deref()
        .map(|b| extract_marker_body_field(b).unwrap_or_else(|| b.to_string()))
        .unwrap_or_default();

    match marker_from_body.as_str() {
        "" | "v2" => { /* proceed */ }
        other @ ("v3" | "v3-db-migrated-config-pending") => {
            return Err(V3MigrationError::AlreadyMigrated {
                body: other.to_string(),
            });
        }
        other => {
            return Err(V3MigrationError::AlreadyMigrated {
                body: other.to_string(),
            });
        }
    }

    // ── 1. In-progress build check. ──────────────────────────────────
    let running_slugs: Vec<String> = {
        // pyramid_builds may or may not exist (tests may use a cut-down
        // schema). If the table is missing we treat it as "no running
        // builds" rather than failing the migration — the boot-time
        // check at §2.17 step 4.b is the strict version.
        let table_exists: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'pyramid_builds'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if table_exists == 0 {
            Vec::new()
        } else {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT slug FROM pyramid_builds \
                 WHERE status IN ('running', 'paused_for_resume')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        }
    };
    if !running_slugs.is_empty() {
        return Err(V3MigrationError::InProgressBuildsBlock(running_slugs));
    }

    // ── 2. Pre-migration snapshot. ───────────────────────────────────
    //
    // Three tables so rollback can find each source independently:
    //   * _pre_v3_snapshot_pyramid_tier_routing: the legacy routing
    //     table.
    //   * _pre_v3_snapshot_dispatch_policy: active dispatch_policy
    //     contribution body.
    //   * _pre_v3_snapshot_config: pyramid_config.json fields we read
    //     (for W4 rollback).
    // `_pre_v3_dedup_snapshot` is already seeded by
    // `ensure_config_contrib_active_unique_index` (above); we do NOT
    // duplicate that snapshot here.
    let snapshot_rows_dumped = snapshot_legacy_state(&tx).map_err(|e| {
        // Deliberately keep the underlying rusqlite::Error for
        // diagnostics. Maps to a loud boot abort.
        V3MigrationError::SnapshotFailed(e)
    })?;

    // ── 3. GROUP BY provider_id — load legacy routing rows. ──────────
    //
    // Each row: (tier_name, provider_id, model_id, context_limit,
    //            max_completion_tokens, pricing_json, supported_parameters_json,
    //            notes).
    // `pricing_json` is NOT NULL in the source shape (defaults to '{}')
    // so we treat `'{}'` as "none" for migration purposes.
    let routing_rows: Vec<TierRoutingRow> = {
        let tier_routing_exists: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'pyramid_tier_routing'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if tier_routing_exists == 0 {
            Vec::new()
        } else {
            let mut stmt = tx.prepare(
                "SELECT tier_name, provider_id, model_id, context_limit, \
                        max_completion_tokens, pricing_json, \
                        supported_parameters_json, notes \
                 FROM pyramid_tier_routing \
                 ORDER BY tier_name",
            )?;
            let mapped = stmt.query_map([], |row| {
                Ok(TierRoutingRow {
                    tier_name: row.get(0)?,
                    provider_id: row.get(1)?,
                    model_id: row.get(2)?,
                    context_limit: row.get(3)?,
                    max_completion_tokens: row.get(4)?,
                    pricing_json: row.get(5)?,
                    supported_parameters_json: row.get(6)?,
                    notes: row.get(7)?,
                })
            })?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r?);
            }
            out
        }
    };

    // Classify provider_ids. Unknowns collect into a hard-fail set.
    let mut unknowns: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<ProviderType, Vec<TierRoutingRow>> = BTreeMap::new();
    for row in &routing_rows {
        match map_provider_id(&row.provider_id) {
            Some(pt) => grouped.entry(pt).or_default().push(row.clone()),
            None => {
                // Collect, don't early-return: we want a complete
                // inventory of unknowns for the modal / log, not the
                // first one we trip over.
                if !unknowns.contains(&row.provider_id) {
                    unknowns.push(row.provider_id.clone());
                }
            }
        }
    }
    if !unknowns.is_empty() {
        return Err(V3MigrationError::UnknownProviderIds { ids: unknowns });
    }

    // ── 4. Fold pyramid_config.json primary_model / fallback_model_*
    //       into walker_provider_openrouter.overrides.model_list[mid]. ─
    //
    // Per §5.3 step 3: fallbacks become list entries; mid is the
    // default workhorse tier per §8. The config file is NOT rewritten
    // here (W4); we only READ.
    //
    // W3c: the typed `PyramidConfig` no longer carries these fields, so
    // we read the raw JSON directly with serde_json::Value. Pre-W3c
    // configs on disk still have the keys; once W4 rewrites the file
    // they'll be absent and this migration becomes a no-op harmlessly.
    let config_fallback_chain: Vec<String> = data_dir
        .and_then(|d| {
            let path = d.join("pyramid_config.json");
            let raw = std::fs::read_to_string(&path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let mut out: Vec<String> = Vec::new();
            for key in ["primary_model", "fallback_model_1", "fallback_model_2"] {
                if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        push_unique(&mut out, s);
                    }
                }
            }
            Some(out)
        })
        .unwrap_or_default();

    // ── 5. Write walker_provider_* contributions. ────────────────────
    let mut written_ids: Vec<String> = Vec::new();
    for (pt, rows) in &grouped {
        let body = build_walker_provider_body(*pt, rows, &config_fallback_chain);
        let id = uuid::Uuid::new_v4().to_string();
        write_envelope_in_tx(
            &tx,
            ContributionEnvelopeInput {
                contribution_id: id.clone(),
                slug: None,
                schema_type: pt.schema_type().to_string(),
                body,
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some(format!(
                    "Walker v3 Phase A migration from pyramid_tier_routing (provider_id={})",
                    pt.as_str()
                )),
                status: "active".to_string(),
                source: "migration".to_string(),
                wire_contribution_id: None,
                created_by: None,
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::Strict,
            },
        )?;
        written_ids.push(id);
    }

    // Openrouter-only: if there's no routing row for openrouter BUT the
    // operator has primary_model / fallbacks set in pyramid_config.json,
    // still emit a walker_provider_openrouter carrier so the fold lands
    // somewhere instead of being discarded.
    if !config_fallback_chain.is_empty()
        && !grouped.contains_key(&ProviderType::OpenRouter)
    {
        let body = build_walker_provider_body(
            ProviderType::OpenRouter,
            &[],
            &config_fallback_chain,
        );
        let id = uuid::Uuid::new_v4().to_string();
        write_envelope_in_tx(
            &tx,
            ContributionEnvelopeInput {
                contribution_id: id.clone(),
                slug: None,
                schema_type: ProviderType::OpenRouter.schema_type().to_string(),
                body,
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some(
                    "Walker v3 Phase A migration — pyramid_config.json primary/fallback fold"
                        .to_string(),
                ),
                status: "active".to_string(),
                source: "migration".to_string(),
                wire_contribution_id: None,
                created_by: None,
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::Strict,
            },
        )?;
        written_ids.push(id);
    }

    // ── 6. walker_call_order from dispatch_policy.routing_rules. ─────
    //
    // Per §5.3 step 4: `order = [rt.provider_type for rt in route_to]`,
    // `overrides_by_provider[pt].max_budget_credits = rt.max_budget_credits`.
    // If no active dispatch_policy contribution: the bundled
    // walker_call_order default stays active (no migration action).
    let walker_call_order_id = migrate_walker_call_order(&tx)?;

    // ── 7. Supersede migration_marker v2 → v3-db-migrated-config-pending.
    //
    // Use a manual supersede pattern (UPDATE prior → superseded, INSERT
    // new active, back-fill forward pointer). Can't call
    // `supersede_config_contribution` because it opens its own BEGIN
    // IMMEDIATE (§2.16.1 transaction-mode parameter was added only for
    // the envelope writer, not the high-level supersede helper).
    supersede_marker_in_tx(&tx, prior_marker_contribution_id.as_deref())?;

    tx.commit()?;

    Ok(V3MigrationReport {
        walker_provider_contributions_written: written_ids,
        walker_call_order_written: walker_call_order_id,
        walker_slot_policy_written: None,
        snapshot_rows_dumped,
        marker_transitioned_from: marker_from_body,
        marker_transitioned_to: "v3-db-migrated-config-pending".to_string(),
    })
}

// ── Helpers ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TierRoutingRow {
    tier_name: String,
    provider_id: String,
    model_id: String,
    context_limit: Option<i64>,
    max_completion_tokens: Option<i64>,
    pricing_json: Option<String>,
    supported_parameters_json: Option<String>,
    notes: Option<String>,
}

/// Map a legacy `pyramid_tier_routing.provider_id` string to the v3
/// `ProviderType` enum. Unknown strings return `None`; the caller
/// collects unknowns and hard-fails the migration (§5.3 step 1).
fn map_provider_id(s: &str) -> Option<ProviderType> {
    match s {
        "openrouter" => Some(ProviderType::OpenRouter),
        "ollama" | "ollama-local" => Some(ProviderType::Local),
        "fleet" => Some(ProviderType::Fleet),
        "market" => Some(ProviderType::Market),
        _ => None,
    }
}

/// Push `v` into `out` preserving insertion order, skipping empty
/// strings and duplicates. Used for the primary/fallback fold.
fn push_unique(out: &mut Vec<String>, v: &str) {
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return;
    }
    if !out.iter().any(|x| x == trimmed) {
        out.push(trimmed.to_string());
    }
}

/// Extract the `body:` scalar from a migration_marker YAML document.
/// Returns `None` if the input isn't well-formed YAML with a top-level
/// `body:` string.
fn extract_marker_body_field(yaml: &str) -> Option<String> {
    let v: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    v.get("body")?.as_str().map(|s| s.trim().to_string())
}

/// Build a walker_provider_* YAML body from a group of legacy
/// `pyramid_tier_routing` rows (all sharing a provider_id) plus an
/// optional openrouter-only fallback chain from `pyramid_config.json`.
///
/// Per §5.1:
///   * model_id → overrides.model_list[tier_name] = [model_id]
///   * context_limit → overrides.context_limit[tier] = u64
///   * max_completion_tokens → overrides.max_completion_tokens[tier] = u64
///   * pricing_json → overrides.pricing_json (per-provider scalar; first
///                    non-trivial wins, with a warning log)
///   * supported_parameters_json → overrides.supported_parameters
///                    (per-provider; first non-trivial wins)
///   * notes → overrides._notes (underscore-prefixed metadata; resolver
///             has no typed accessor, validator skips)
fn build_walker_provider_body(
    pt: ProviderType,
    rows: &[TierRoutingRow],
    openrouter_fallback_chain: &[String],
) -> String {
    // model_list: {tier: [model_id]}. When multiple rows share a
    // provider, each tier gets its own single-entry list.
    let mut model_list: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut context_limit: BTreeMap<String, u64> = BTreeMap::new();
    let mut max_completion_tokens: BTreeMap<String, u64> = BTreeMap::new();
    let mut pricing_json: Option<serde_yaml::Value> = None;
    let mut supported_parameters: Option<Vec<String>> = None;
    let mut notes_parts: Vec<String> = Vec::new();

    for row in rows {
        model_list
            .entry(row.tier_name.clone())
            .or_default()
            .push(row.model_id.clone());

        if let Some(cl) = row.context_limit.filter(|&n| n > 0) {
            context_limit.insert(row.tier_name.clone(), cl as u64);
        }
        if let Some(mct) = row.max_completion_tokens.filter(|&n| n > 0) {
            max_completion_tokens.insert(row.tier_name.clone(), mct as u64);
        }
        if let Some(pj) = row.pricing_json.as_ref() {
            // Treat '{}' (the NOT NULL DEFAULT in the legacy table) as "no
            // pricing set". Non-trivial pricing wins first-write; log the
            // discard when multiple rows differ (known data-quality issue
            // named in §5.3 step 3).
            let trimmed = pj.trim();
            if !trimmed.is_empty() && trimmed != "{}" {
                if let Ok(parsed) = serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
                    if pricing_json.is_none() {
                        pricing_json = Some(parsed);
                    } else {
                        tracing::warn!(
                            provider_type = pt.as_str(),
                            tier = %row.tier_name,
                            "discarding divergent pricing_json on secondary routing row — first-wins"
                        );
                    }
                }
            }
        }
        if let Some(sp) = row.supported_parameters_json.as_ref() {
            let trimmed = sp.trim();
            if !trimmed.is_empty() {
                // Accept either a JSON list of strings or a comma-split
                // fallback; the legacy column was free-form.
                let parsed: Option<Vec<String>> = serde_json::from_str::<Vec<String>>(trimmed)
                    .ok()
                    .or_else(|| {
                        serde_yaml::from_str::<Vec<String>>(trimmed).ok()
                    });
                if let Some(list) = parsed {
                    if supported_parameters.is_none() {
                        supported_parameters = Some(list);
                    }
                }
            }
        }
        if let Some(n) = row.notes.as_ref() {
            let trimmed = n.trim();
            if !trimmed.is_empty() {
                notes_parts.push(format!("{}: {}", row.tier_name, trimmed));
            }
        }
    }

    // Fold openrouter primary/fallback chain as tail entries per §5.3
    // step 3, with dedup preserving order. Assumes `mid` is the default
    // workhorse tier (§8 tester smoke).
    if matches!(pt, ProviderType::OpenRouter) && !openrouter_fallback_chain.is_empty() {
        let mid_list = model_list.entry("mid".to_string()).or_default();
        for m in openrouter_fallback_chain {
            if !mid_list.iter().any(|existing| existing == m) {
                mid_list.push(m.clone());
            }
        }
    }

    // Dedup each model_list entry preserving order — protects against
    // the same model_id appearing twice for the same tier across rows
    // (unlikely with the PK on tier_name but defensive).
    for vals in model_list.values_mut() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        vals.retain(|m| seen.insert(m.clone()));
    }

    // Emit YAML. `serde_yaml::to_string` on an IndexMap-backed mapping
    // wouldn't preserve order; we build the body string with explicit
    // key ordering for readability and round-trip stability.
    let schema_type = pt.schema_type();
    let mut out = String::new();
    out.push_str(&format!("schema_type: {}\n", schema_type));
    out.push_str("version: 1\n");
    out.push_str("overrides:\n");

    // model_list
    if model_list.is_empty() {
        out.push_str("  model_list: {}\n");
    } else {
        out.push_str("  model_list:\n");
        for (tier, ms) in &model_list {
            if ms.len() == 1 {
                out.push_str(&format!("    {}: [\"{}\"]\n", yaml_scalar(tier), ms[0]));
            } else {
                out.push_str(&format!("    {}:\n", yaml_scalar(tier)));
                for m in ms {
                    out.push_str(&format!("      - \"{}\"\n", m));
                }
            }
        }
    }

    // context_limit (optional tiered_map)
    if !context_limit.is_empty() {
        out.push_str("  context_limit:\n");
        for (tier, cl) in &context_limit {
            out.push_str(&format!("    {}: {}\n", yaml_scalar(tier), cl));
        }
    }

    // max_completion_tokens (optional tiered_map)
    if !max_completion_tokens.is_empty() {
        out.push_str("  max_completion_tokens:\n");
        for (tier, mct) in &max_completion_tokens {
            out.push_str(&format!("    {}: {}\n", yaml_scalar(tier), mct));
        }
    }

    // supported_parameters (optional list)
    if let Some(sp) = &supported_parameters {
        out.push_str("  supported_parameters:\n");
        for s in sp {
            out.push_str(&format!("    - \"{}\"\n", s));
        }
    }

    // pricing_json (optional scalar) — serialize via serde_yaml so
    // whatever nested shape the legacy blob had lands verbatim.
    if let Some(pj) = &pricing_json {
        let serialized = serde_yaml::to_string(pj).unwrap_or_default();
        // Indent the serialized block 2 spaces under `pricing_json:`.
        let indented: String = serialized
            .lines()
            .map(|l| format!("    {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str("  pricing_json:\n");
        out.push_str(&indented);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    // _notes — underscore-prefixed, non-resolvable metadata (§5.1).
    // Joined across tiers to preserve each row's note.
    if !notes_parts.is_empty() {
        let joined = notes_parts.join(" | ");
        out.push_str("  _notes: ");
        out.push_str(&yaml_scalar(&joined));
        out.push('\n');
    }

    out
}

/// Emit a YAML-safe scalar — quoted if it contains YAML-significant
/// characters or looks like a bool/number. Keeps our hand-assembled
/// body robust against tier names like `"true"` or `"1"`.
fn yaml_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.chars().any(|c| ":{}[],&*#?|<>=!%@\\\"'\n".contains(c))
        || matches!(s, "true" | "false" | "null" | "yes" | "no")
        || s.parse::<f64>().is_ok();
    if needs_quote {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Write a walker_call_order body from the active `dispatch_policy`
/// contribution's `routing_rules.route_to`. Returns the new
/// contribution_id, or `None` if no active dispatch_policy exists (the
/// bundled walker_call_order default stays active — no migration action).
fn migrate_walker_call_order(
    tx: &rusqlite::Transaction<'_>,
) -> std::result::Result<Option<String>, V3MigrationError> {
    let dispatch_body: Option<String> = tx
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'dispatch_policy' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = dispatch_body else {
        return Ok(None);
    };

    // Parse minimally: we only need `routing_rules[*].route_to[*]`.
    // Going through serde_yaml::Value keeps us decoupled from the full
    // dispatch_policy.rs struct (which has lots of unrelated fields).
    let parsed: serde_yaml::Value = serde_yaml::from_str(&body)
        .map_err(|e| V3MigrationError::Other(anyhow::anyhow!("parse dispatch_policy YAML: {e}")))?;

    // walker_call_order.order = order of distinct provider_types seen in
    // the FIRST route_to list of the FIRST routing_rule. Each subsequent
    // route_to contributes additional provider_types to the tail (dedup
    // preserving order).
    let mut seen: Vec<ProviderType> = Vec::new();
    let mut overrides_by_provider: BTreeMap<ProviderType, Option<i64>> = BTreeMap::new();

    if let Some(rules) = parsed.get("routing_rules").and_then(|v| v.as_sequence()) {
        for rule in rules {
            if let Some(route_to) = rule.get("route_to").and_then(|v| v.as_sequence()) {
                for entry in route_to {
                    // provider_id in legacy → provider_type in v3
                    let pid = entry.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
                    if pid.is_empty() {
                        continue;
                    }
                    let Some(pt) = map_provider_id(pid) else {
                        // Unknown provider_ids in dispatch_policy are
                        // silently skipped here — the tier_routing
                        // path already hard-failed on them. If a
                        // dispatch_policy references a provider with
                        // no tier_routing counterpart, it's operator
                        // error; the walker_call_order body just
                        // omits the unknown provider.
                        tracing::warn!(
                            provider_id = %pid,
                            "skipping unknown provider_id in dispatch_policy.routing_rules.route_to"
                        );
                        continue;
                    };
                    if !seen.contains(&pt) {
                        seen.push(pt);
                    }
                    // max_budget_credits: carry forward when Some.
                    let mbc: Option<i64> = entry.get("max_budget_credits").and_then(|v| v.as_i64());
                    if let Some(cap) = mbc {
                        overrides_by_provider.entry(pt).or_insert(Some(cap));
                    }
                }
            }
        }
    }

    // Empty order → no useful migration action. Fall through to bundled.
    if seen.is_empty() {
        return Ok(None);
    }

    // Build body YAML.
    let mut body_out = String::new();
    body_out.push_str("schema_type: walker_call_order\n");
    body_out.push_str("version: 1\n");
    body_out.push_str("order: [");
    for (i, pt) in seen.iter().enumerate() {
        if i > 0 {
            body_out.push_str(", ");
        }
        body_out.push_str(pt.as_str());
    }
    body_out.push_str("]\n");
    // Only emit overrides_by_provider when at least one entry has a
    // non-None max_budget_credits.
    let any_cap = overrides_by_provider.values().any(|v| v.is_some());
    if any_cap {
        body_out.push_str("overrides_by_provider:\n");
        for (pt, cap) in &overrides_by_provider {
            if let Some(c) = cap {
                body_out.push_str(&format!(
                    "  {}:\n    max_budget_credits: {}\n",
                    pt.as_str(),
                    c
                ));
            }
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    write_envelope_in_tx(
        tx,
        ContributionEnvelopeInput {
            contribution_id: id.clone(),
            slug: None,
            schema_type: "walker_call_order".to_string(),
            body: body_out,
            wire_native_metadata_json: None,
            supersedes_id: None,
            triggering_note: Some(
                "Walker v3 Phase A migration — dispatch_policy.routing_rules.route_to"
                    .to_string(),
            ),
            status: "active".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: None,
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::Strict,
        },
    )?;
    // The bundled walker_call_order row (if loaded at boot) would
    // conflict with the new one on the unique-active index. The bundled
    // loader supersedes itself via the envelope writer's conflict path;
    // but at Phase A time the walker_call_order bundled default might
    // also be active. Defense-in-depth: supersede the bundled row if
    // present (and not the row we just wrote). Rest of the bundled
    // schema-annotation / schema-definition / generation skill rows
    // aren't affected — they're different schema_types.
    supersede_prior_active_if_present(tx, "walker_call_order", &id)?;

    Ok(Some(id))
}

/// If there's an active non-superseded contribution for
/// `(schema_type, slug=NULL)` other than `new_id`, mark it superseded
/// with the new row. Keeps the unique-active invariant (§2.16.1)
/// satisfied when migration writes over a bundled default.
fn supersede_prior_active_if_present(
    tx: &rusqlite::Transaction<'_>,
    schema_type: &str,
    new_id: &str,
) -> std::result::Result<(), V3MigrationError> {
    let prior_id: Option<String> = tx
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions \
             WHERE schema_type = ?1 \
               AND slug IS NULL \
               AND status = 'active' \
               AND contribution_id != ?2 \
               AND superseded_by_id IS NULL",
            rusqlite::params![schema_type, new_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(prior) = prior_id {
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET status = 'superseded', superseded_by_id = ?1 \
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior],
        )?;
        // Forward-link from new row to the prior one via supersedes_id.
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET supersedes_id = ?1 \
             WHERE contribution_id = ?2 AND supersedes_id IS NULL",
            rusqlite::params![prior, new_id],
        )?;
    }
    Ok(())
}

/// Supersede `migration_marker` v2 → v3-db-migrated-config-pending
/// inside the ambient transaction. Creates a fresh active row if no
/// prior marker exists (first-ever boot of v3 binary on a
/// never-had-bundled-seed DB).
fn supersede_marker_in_tx(
    tx: &rusqlite::Transaction<'_>,
    prior_contribution_id: Option<&str>,
) -> std::result::Result<(), V3MigrationError> {
    let new_id = uuid::Uuid::new_v4().to_string();
    let body = "schema_type: migration_marker\nbody: \"v3-db-migrated-config-pending\"\n";

    // UPDATE prior → superseded BEFORE INSERT so the unique-active
    // index never sees two active migration_marker rows.
    if let Some(prior) = prior_contribution_id {
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET status = 'superseded' \
             WHERE contribution_id = ?1",
            rusqlite::params![prior],
        )?;
    }

    write_envelope_in_tx(
        tx,
        ContributionEnvelopeInput {
            contribution_id: new_id.clone(),
            slug: None,
            schema_type: "migration_marker".to_string(),
            body: body.to_string(),
            wire_native_metadata_json: None,
            supersedes_id: prior_contribution_id.map(|s| s.to_string()),
            triggering_note: Some(
                "Walker v3 Phase A — DB migration complete; config-file rewrite pending"
                    .to_string(),
            ),
            status: "active".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: None,
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::Strict,
        },
    )?;

    // Back-fill forward pointer on the prior row.
    if let Some(prior) = prior_contribution_id {
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET superseded_by_id = ?1 \
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior],
        )?;
    }

    Ok(())
}

/// Thin wrapper around `write_contribution_envelope` that converts its
/// error type into our `V3MigrationError` flavor. Every write inside
/// Phase A runs in `TransactionMode::JoinAmbient` — we're already in
/// an outer `BEGIN IMMEDIATE`.
fn write_envelope_in_tx(
    tx: &rusqlite::Transaction<'_>,
    input: ContributionEnvelopeInput,
) -> std::result::Result<String, V3MigrationError> {
    crate::pyramid::config_contributions::write_contribution_envelope(
        tx,
        input,
        TransactionMode::JoinAmbient,
    )
    .map_err(|e| V3MigrationError::Other(anyhow::anyhow!("envelope write: {e}")))
}

/// Snapshot every legacy source row into the three
/// `_pre_v3_snapshot_*` tables. Returns total rows dumped (tier_routing
/// contribution to the count; dispatch_policy + config are single-row
/// dumps that aren't counted here, just the routing rows).
fn snapshot_legacy_state(
    tx: &rusqlite::Transaction<'_>,
) -> std::result::Result<usize, rusqlite::Error> {
    // _pre_v3_snapshot_pyramid_tier_routing — mirrors the legacy column set.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _pre_v3_snapshot_pyramid_tier_routing (
           snapshot_at TEXT NOT NULL,
           tier_name TEXT NOT NULL,
           provider_id TEXT NOT NULL,
           model_id TEXT,
           context_limit INTEGER,
           max_completion_tokens INTEGER,
           pricing_json TEXT,
           supported_parameters_json TEXT,
           notes TEXT,
           source_table TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS _pre_v3_snapshot_dispatch_policy (
           snapshot_at TEXT NOT NULL,
           contribution_id TEXT,
           yaml_content TEXT,
           source_table TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS _pre_v3_snapshot_config (
           snapshot_at TEXT NOT NULL,
           primary_model TEXT,
           fallback_model_1 TEXT,
           fallback_model_2 TEXT,
           source_table TEXT NOT NULL
         );",
    )?;

    // Dump rows.
    let tier_routing_exists: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'table' AND name = 'pyramid_tier_routing'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let mut rows_dumped = 0;
    if tier_routing_exists > 0 {
        tx.execute(
            "INSERT INTO _pre_v3_snapshot_pyramid_tier_routing \
               (snapshot_at, tier_name, provider_id, model_id, context_limit, \
                max_completion_tokens, pricing_json, supported_parameters_json, \
                notes, source_table) \
             SELECT datetime('now'), tier_name, provider_id, model_id, context_limit, \
                    max_completion_tokens, pricing_json, supported_parameters_json, \
                    notes, 'pyramid_tier_routing' \
             FROM pyramid_tier_routing",
            [],
        )?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM _pre_v3_snapshot_pyramid_tier_routing",
            [],
            |row| row.get(0),
        )?;
        rows_dumped = count as usize;
    }

    // dispatch_policy: single-row active contribution.
    let _ = tx.execute(
        "INSERT INTO _pre_v3_snapshot_dispatch_policy \
           (snapshot_at, contribution_id, yaml_content, source_table) \
         SELECT datetime('now'), contribution_id, yaml_content, 'pyramid_config_contributions' \
         FROM pyramid_config_contributions \
         WHERE schema_type = 'dispatch_policy' \
           AND status = 'active' \
           AND superseded_by_id IS NULL",
        [],
    );

    // `_pre_v3_snapshot_config` — populated in a separate code path
    // that has access to the data_dir. Here we just ensure the table
    // exists; W4 (config-file rewrite) is the one that reads
    // pyramid_config.json and can decide whether to mirror the fields
    // into this table. For Phase A we can't reach the config file from
    // inside the SQL tx without threading a second arg, and the
    // snapshot isn't load-bearing for DB rollback.

    Ok(rows_dumped)
}

// ── Convenience surface for boot.rs ──────────────────────────────────

/// Resolve the data_dir where `pyramid_config.json` lives. `boot.rs`
/// calls this so the caller doesn't need to thread a Path through.
/// Returns `None` if platform dirs can't be resolved (tests).
#[allow(dead_code)]
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("wire-node"))
}

/// Read-only probe for boot.rs: does the active migration_marker say
/// we need to run Phase A? Returns `Some(body)` if migration should
/// run (`v2` or missing), `None` otherwise (`v3` or later).
#[allow(dead_code)]
pub fn should_run_phase_a(conn: &Connection) -> anyhow::Result<Option<String>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("read migration_marker")?;
    let current = body
        .as_deref()
        .map(|b| extract_marker_body_field(b).unwrap_or_else(|| b.to_string()))
        .unwrap_or_default();
    match current.as_str() {
        "" | "v2" => Ok(Some(current)),
        _ => Ok(None),
    }
}

// ── Phase B ──────────────────────────────────────────────────────────
//
// Phase B rewrites `pyramid_config.json` to remove the legacy model
// fields and transitions the `migration_marker` contribution body from
// `v3-db-migrated-config-pending` to `v3`. See plan rev 1.0.2 §5.3
// Phase B (steps 8–10) + §2.17 step 4.5 + §2.17.3 + §5.6.1.
//
// Contract with Phase A:
//   * Phase A leaves the marker at `v3-db-migrated-config-pending`
//     after committing the SQL transaction. Phase B is the only code
//     path that moves it to `v3`.
//   * Phase A snapshotted nothing into `_pre_v3_snapshot_config`
//     because the config file lives on disk, not in SQL. Phase B
//     captures the pre-rewrite JSON body in a single-row dump before
//     touching the file.
//   * A crash between A and B leaves the marker at
//     `v3-db-migrated-config-pending` — the next boot's Phase A sees
//     `AlreadyMigrated` and returns; Phase B is called
//     unconditionally by boot.rs and picks up from the pending state.

/// Per-boot Phase-B migration report. Surfaces the transition details
/// boot.rs logs for operator visibility.
#[derive(Debug, Clone)]
pub struct V3PhaseBReport {
    /// Bytes on disk before the rewrite (0 when the file was absent).
    pub bytes_before: usize,
    /// Bytes on disk after the rewrite (0 when no file was written —
    /// i.e. the file was absent at Phase B entry).
    pub bytes_after: usize,
    /// `contribution_id` of the new `migration_marker` (`v3`) row.
    pub snapshot_id: String,
    /// Human-readable summary of the transition, e.g.
    /// `"v3-db-migrated-config-pending -> v3"`.
    pub marker_transition: String,
}

/// Failure modes for `run_v3_phase_b_migration`. Each variant maps to
/// an operator-visible modal once Phase-6 UI lands; boot.rs translates
/// these into `BootResult::Aborted` messages in the meantime.
#[derive(Debug, thiserror::Error)]
pub enum V3PhaseBError {
    /// Active migration_marker already says `v3` — Phase B previously
    /// committed. Idempotent no-op for the caller.
    #[error("migration_marker body `{body}` — Phase B already ran")]
    AlreadyMigrated { body: String },
    /// Active marker says `v2` or is absent — Phase A hasn't run yet.
    /// Boot sequence runs A before B, so this shouldn't happen in
    /// production; surfaced loudly so it does not silently eat state.
    #[error("Phase A has not run (marker body indicates pre-migration state)")]
    PhaseANotRun,
    /// Marker body is a value neither this module nor Phase A
    /// recognizes. Hard-fails rather than guessing.
    #[error("unexpected migration_marker body `{body}`")]
    UnexpectedMarkerBody { body: String },
    /// IO failure reading or renaming the config file. The temp file
    /// may still be on disk for forensics.
    #[error("pyramid_config.json IO error: {0}")]
    ConfigFileIoError(#[source] std::io::Error),
    /// `pyramid_config.json` contents could not be parsed as JSON.
    /// Phase B refuses to rewrite a corrupt file.
    #[error("pyramid_config.json parse error: {0}")]
    ConfigFileParseError(#[source] serde_json::Error),
    /// `_pre_v3_snapshot_config` insert failed.
    #[error("snapshot insert failed: {0}")]
    SnapshotFailed(#[source] rusqlite::Error),
    /// SQLite error outside the snapshot path.
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Anyhow-flavored errors from envelope writes.
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Legacy top-level keys Phase B strips out of `pyramid_config.json`.
/// Anything else in the file (auth_token, openrouter_api_key,
/// operational, etc.) is preserved verbatim.
const PHASE_B_LEGACY_KEYS: &[&str] = &[
    "primary_model",
    "fallback_model_1",
    "fallback_model_2",
    "primary_context_limit",
    "fallback_1_context_limit",
];

/// Phase B entry point. Runs inside ONE SQL transaction (for the
/// snapshot + marker supersession). The file rewrite is a separate
/// atomic rename(2) outside the SQL transaction — per §5.3 Phase B
/// the two stores are independent and the intermediate marker body
/// (`v3-db-migrated-config-pending`) exists precisely to signal a
/// resume point if we crash between them.
///
/// Sequence:
///   1. Idempotency gate — read active migration_marker body.
///   2. Read `pyramid_config.json` as raw JSON (if present).
///   3. Snapshot the pre-rewrite body into `_pre_v3_snapshot_config`.
///   4. Strip the legacy keys from the top-level JSON object.
///   5. Rewrite via temp-file + atomic rename.
///   6. Supersede migration_marker `v3-db-migrated-config-pending` → `v3`.
///
/// The SQL transaction covers steps 3 + 6. Step 5 (file rename) runs
/// between them deliberately: if the rename fails, the marker remains
/// at `v3-db-migrated-config-pending` and the next boot retries. If
/// the rename succeeds but the marker supersession fails (SQLite
/// error), the next boot sees a clean config file + pending marker and
/// the rewrite step is a no-op (file already lacks the legacy keys),
/// so the retry converges.
pub fn run_v3_phase_b_migration(
    conn: &mut Connection,
    data_dir: &Path,
) -> std::result::Result<V3PhaseBReport, V3PhaseBError> {
    // ── 1. Idempotency gate ──────────────────────────────────────────
    let marker_body_yaml: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let prior_marker_contribution_id: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let current_body = marker_body_yaml
        .as_deref()
        .map(|b| extract_marker_body_field(b).unwrap_or_else(|| b.to_string()))
        .unwrap_or_default();

    match current_body.as_str() {
        "v3-db-migrated-config-pending" => { /* proceed */ }
        "v3" => {
            return Err(V3PhaseBError::AlreadyMigrated {
                body: current_body,
            });
        }
        "" | "v2" => {
            return Err(V3PhaseBError::PhaseANotRun);
        }
        _ => {
            return Err(V3PhaseBError::UnexpectedMarkerBody {
                body: current_body,
            });
        }
    }

    // ── 2. Read pyramid_config.json (if present) ─────────────────────
    let config_path = data_dir.join("pyramid_config.json");
    let tmp_path = data_dir.join("pyramid_config.json.walker_v3_tmp");

    let (pre_bytes, parsed_value): (Option<String>, Option<serde_json::Value>) =
        match std::fs::read_to_string(&config_path) {
            Ok(raw) => {
                let v: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(V3PhaseBError::ConfigFileParseError)?;
                (Some(raw), Some(v))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Treat missing config as "already clean" — fresh
                // install where nothing has written the JSON yet.
                (None, None)
            }
            Err(err) => {
                return Err(V3PhaseBError::ConfigFileIoError(err));
            }
        };

    let bytes_before = pre_bytes.as_ref().map(|s| s.len()).unwrap_or(0);

    // ── 3 + 6: SQL-side transaction (snapshot + marker supersession) ─
    //
    // Open the transaction first so we can roll back on any failure
    // before the file rename. Step 5 (file rename) is performed between
    // the snapshot insert and the marker supersession but OUTSIDE the
    // SQL transaction — SQLite cannot rollback a rename(2).
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // ── 3. Snapshot the pre-rewrite body ─────────────────────────────
    //
    // Ensure the table exists (Phase A creates it, but a fresh-test
    // path could hit Phase B without Phase A's snapshot_legacy_state
    // having run). Columns extended beyond Phase A's minimal legacy
    // column list to hold the raw JSON body; old rows (none today —
    // Phase A has never populated this table) use NULL in the new
    // columns.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _pre_v3_snapshot_config (
           snapshot_at TEXT NOT NULL,
           primary_model TEXT,
           fallback_model_1 TEXT,
           fallback_model_2 TEXT,
           source_table TEXT NOT NULL
         );",
    )
    .map_err(V3PhaseBError::SnapshotFailed)?;
    // Add the body + source_file columns if the table predates them.
    // `ALTER TABLE ADD COLUMN` is idempotent via pragma check; simpler
    // to query sqlite_master's column list and emit the ADDs for keys
    // that aren't present yet.
    ensure_snapshot_config_columns(&tx).map_err(V3PhaseBError::SnapshotFailed)?;

    // Only insert a snapshot row when we actually have a pre-rewrite
    // body to capture. If the file is absent, nothing to snapshot.
    if let Some(raw) = &pre_bytes {
        // Pull the three legacy model fields into their dedicated
        // columns so rollback can reconstruct without reparsing.
        let (pm, fb1, fb2) = if let Some(v) = &parsed_value {
            (
                v.get("primary_model").and_then(|x| x.as_str()).map(String::from),
                v.get("fallback_model_1").and_then(|x| x.as_str()).map(String::from),
                v.get("fallback_model_2").and_then(|x| x.as_str()).map(String::from),
            )
        } else {
            (None, None, None)
        };
        tx.execute(
            "INSERT INTO _pre_v3_snapshot_config \
               (snapshot_at, primary_model, fallback_model_1, fallback_model_2, \
                source_table, body, source_file) \
             VALUES (datetime('now'), ?1, ?2, ?3, 'pyramid_config.json', ?4, 'pyramid_config.json')",
            rusqlite::params![pm, fb1, fb2, raw],
        )
        .map_err(V3PhaseBError::SnapshotFailed)?;
    }

    // ── 4. Strip legacy keys from the parsed JSON ────────────────────
    //
    // If the top level isn't a JSON object, log warn and skip the file
    // rewrite (defensive — a corrupt/unexpected shape shouldn't be
    // silently destroyed). The marker supersession still runs so the
    // next boot doesn't infinite-loop on Phase B.
    let (bytes_after, rewrite_skipped) = match parsed_value {
        Some(mut v) if v.is_object() => {
            let mut removed_keys: Vec<&str> = Vec::new();
            if let Some(obj) = v.as_object_mut() {
                for key in PHASE_B_LEGACY_KEYS {
                    if obj.remove(*key).is_some() {
                        removed_keys.push(*key);
                    }
                }
            }
            // Serialize with pretty-print so the file stays
            // human-readable (matches the existing save shape).
            let serialized = serde_json::to_string_pretty(&v)
                .map_err(V3PhaseBError::ConfigFileParseError)?;

            // ── 5. Atomic rewrite: temp-file + rename(2) ─────────────
            //
            // NOTE: this happens between snapshot insert (committed
            // below) and marker supersession. If it fails, we roll
            // back the SQL tx so the snapshot doesn't get half-stored.
            if let Err(e) = write_temp_and_rename(&tmp_path, &config_path, &serialized) {
                tx.rollback().ok();
                return Err(V3PhaseBError::ConfigFileIoError(e));
            }
            tracing::info!(
                event = "v3_phase_b_config_rewritten",
                removed_keys = ?removed_keys,
                bytes_before,
                bytes_after = serialized.len(),
                "pyramid_config.json rewritten (legacy keys stripped)"
            );
            (serialized.len(), false)
        }
        Some(_) => {
            tracing::warn!(
                event = "v3_phase_b_config_shape_unexpected",
                "pyramid_config.json top-level is not an object; skipping rewrite \
                 (marker supersession still proceeds — next boot will not retry)"
            );
            (bytes_before, true)
        }
        None => {
            // No file on disk — nothing to rewrite. Proceed to marker.
            tracing::debug!(
                event = "v3_phase_b_config_absent",
                path = %config_path.display(),
                "pyramid_config.json absent; no rewrite needed"
            );
            (0, true)
        }
    };
    let _ = rewrite_skipped;

    // ── 6. Supersede marker v3-db-migrated-config-pending → v3 ───────
    let new_marker_id =
        supersede_marker_to_v3_in_tx(&tx, prior_marker_contribution_id.as_deref())?;

    tx.commit()?;

    Ok(V3PhaseBReport {
        bytes_before,
        bytes_after,
        snapshot_id: new_marker_id,
        marker_transition: "v3-db-migrated-config-pending -> v3".to_string(),
    })
}

/// Ensure `_pre_v3_snapshot_config` has the `body TEXT` + `source_file
/// TEXT` columns. Phase A creates the table with the legacy-field
/// columns only; Phase B extends it idempotently.
fn ensure_snapshot_config_columns(
    tx: &rusqlite::Transaction<'_>,
) -> std::result::Result<(), rusqlite::Error> {
    let mut has_body = false;
    let mut has_source_file = false;
    let mut stmt = tx.prepare("PRAGMA table_info(_pre_v3_snapshot_config)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for r in rows {
        let name = r?;
        if name == "body" {
            has_body = true;
        } else if name == "source_file" {
            has_source_file = true;
        }
    }
    drop(stmt);
    if !has_body {
        tx.execute(
            "ALTER TABLE _pre_v3_snapshot_config ADD COLUMN body TEXT",
            [],
        )?;
    }
    if !has_source_file {
        tx.execute(
            "ALTER TABLE _pre_v3_snapshot_config ADD COLUMN source_file TEXT",
            [],
        )?;
    }
    Ok(())
}

/// Write `contents` to `tmp_path`, fsync, then rename to `final_path`.
/// Leaves the temp file behind on any error (operator forensics).
fn write_temp_and_rename(
    tmp_path: &Path,
    final_path: &Path,
    contents: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    {
        // Open (truncate if the temp file from a prior aborted pass is
        // still around), write, sync.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp_path)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(tmp_path, final_path)?;
    Ok(())
}

/// Supersede the active `migration_marker` row (expected body
/// `v3-db-migrated-config-pending`) with a new row carrying body `v3`.
/// Mirrors `supersede_marker_in_tx` from Phase A but targets the Phase
/// B body transition.
fn supersede_marker_to_v3_in_tx(
    tx: &rusqlite::Transaction<'_>,
    prior_contribution_id: Option<&str>,
) -> std::result::Result<String, V3PhaseBError> {
    let new_id = uuid::Uuid::new_v4().to_string();
    let body = "schema_type: migration_marker\nbody: \"v3\"\n";

    // UPDATE prior → superseded BEFORE INSERT so the unique-active
    // index never sees two active migration_marker rows.
    if let Some(prior) = prior_contribution_id {
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET status = 'superseded' \
             WHERE contribution_id = ?1",
            rusqlite::params![prior],
        )?;
    }

    write_envelope_in_tx_phase_b(
        tx,
        ContributionEnvelopeInput {
            contribution_id: new_id.clone(),
            slug: None,
            schema_type: "migration_marker".to_string(),
            body: body.to_string(),
            wire_native_metadata_json: None,
            supersedes_id: prior_contribution_id.map(|s| s.to_string()),
            triggering_note: Some(
                "Walker v3 Phase B — config-file rewrite complete; schema_version=v3"
                    .to_string(),
            ),
            status: "active".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: None,
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::Strict,
        },
    )?;

    if let Some(prior) = prior_contribution_id {
        tx.execute(
            "UPDATE pyramid_config_contributions \
             SET superseded_by_id = ?1 \
             WHERE contribution_id = ?2",
            rusqlite::params![new_id, prior],
        )?;
    }

    Ok(new_id)
}

/// Phase B flavor of `write_envelope_in_tx` — identical to Phase A's
/// helper but returns `V3PhaseBError`. Kept local so the two phases
/// stay independently typed.
fn write_envelope_in_tx_phase_b(
    tx: &rusqlite::Transaction<'_>,
    input: ContributionEnvelopeInput,
) -> std::result::Result<String, V3PhaseBError> {
    crate::pyramid::config_contributions::write_contribution_envelope(
        tx,
        input,
        TransactionMode::JoinAmbient,
    )
    .map_err(|e| V3PhaseBError::Other(anyhow::anyhow!("envelope write: {e}")))
}

/// Read-only probe for boot.rs: does the active migration_marker say
/// we need to run Phase B? Returns `Some(body)` if Phase B should run
/// (`v3-db-migrated-config-pending`), `None` otherwise.
#[allow(dead_code)]
pub fn should_run_phase_b(conn: &Connection) -> anyhow::Result<Option<String>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("read migration_marker")?;
    let current = body
        .as_deref()
        .map(|b| extract_marker_body_field(b).unwrap_or_else(|| b.to_string()))
        .unwrap_or_default();
    match current.as_str() {
        "v3-db-migrated-config-pending" => Ok(Some(current)),
        _ => Ok(None),
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    /// Minimal DB schema: pyramid_config_contributions + pyramid_tier_routing
    /// + pyramid_builds. Mirrors the columns each piece of this module
    /// reads/writes. Full init_pyramid_db pulls in a lot of unrelated
    /// surface; keep the test fixture narrow.
    fn make_test_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("v3_migration_test.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE pyramid_config_contributions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                contribution_id TEXT NOT NULL UNIQUE,
                slug TEXT,
                schema_type TEXT NOT NULL,
                yaml_content TEXT NOT NULL,
                wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
                wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
                supersedes_id TEXT,
                superseded_by_id TEXT,
                triggering_note TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                source TEXT NOT NULL DEFAULT 'local',
                wire_contribution_id TEXT,
                created_by TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                accepted_at TEXT
            );

            CREATE TABLE pyramid_tier_routing (
                tier_name TEXT PRIMARY KEY,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                context_limit INTEGER,
                max_completion_tokens INTEGER,
                pricing_json TEXT NOT NULL DEFAULT '{}',
                supported_parameters_json TEXT,
                notes TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE pyramid_builds (
                slug TEXT NOT NULL,
                build_id TEXT NOT NULL,
                question TEXT NOT NULL,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                layers_completed INTEGER DEFAULT 0,
                total_layers INTEGER DEFAULT 0,
                l0_node_count INTEGER DEFAULT 0,
                total_node_count INTEGER DEFAULT 0,
                quality_score REAL,
                error_message TEXT,
                PRIMARY KEY (slug, build_id)
            );
            "#,
        )
        .unwrap();
        (dir, conn)
    }

    fn insert_active_marker(conn: &Connection, body: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let yaml = format!("schema_type: migration_marker\nbody: \"{}\"\n", body);
        conn.execute(
            "INSERT INTO pyramid_config_contributions \
               (contribution_id, schema_type, yaml_content, status, source) \
             VALUES (?1, 'migration_marker', ?2, 'active', 'bundled')",
            rusqlite::params![id, yaml],
        )
        .unwrap();
        id
    }

    fn count_active(conn: &Connection, schema_type: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM pyramid_config_contributions \
             WHERE schema_type = ?1 AND status = 'active' AND superseded_by_id IS NULL",
            rusqlite::params![schema_type],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn get_active_body(conn: &Connection, schema_type: &str) -> Option<String> {
        conn.query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = ?1 AND status = 'active' AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC LIMIT 1",
            rusqlite::params![schema_type],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    }

    #[test]
    fn test_migration_from_empty_legacy_tables() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");

        let report = run_v3_phase_a_migration(&mut conn, None).expect("must succeed");

        assert!(report.walker_provider_contributions_written.is_empty());
        assert!(report.walker_call_order_written.is_none());
        assert_eq!(report.snapshot_rows_dumped, 0);
        assert_eq!(report.marker_transitioned_from, "v2");
        assert_eq!(
            report.marker_transitioned_to,
            "v3-db-migrated-config-pending"
        );

        // Marker transitioned.
        let body = get_active_body(&conn, "migration_marker").unwrap();
        assert!(body.contains("v3-db-migrated-config-pending"));
        assert_eq!(count_active(&conn, "migration_marker"), 1);
    }

    #[test]
    fn test_migration_from_seeded_legacy_routing() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");

        conn.execute(
            "INSERT INTO pyramid_tier_routing \
               (tier_name, provider_id, model_id, context_limit, max_completion_tokens, \
                pricing_json, supported_parameters_json, notes) \
             VALUES ('mid', 'openrouter', 'inception/mercury-2', 200000, NULL, \
                     '{}', NULL, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_tier_routing \
               (tier_name, provider_id, model_id, pricing_json) \
             VALUES ('max', 'openrouter', 'x-ai/grok-4.20-beta', '{}')",
            [],
        )
        .unwrap();

        let report = run_v3_phase_a_migration(&mut conn, None).expect("must succeed");
        assert_eq!(report.snapshot_rows_dumped, 2);
        assert_eq!(report.walker_provider_contributions_written.len(), 1);

        let body = get_active_body(&conn, "walker_provider_openrouter").unwrap();
        assert!(
            body.contains("mid:") && body.contains("inception/mercury-2"),
            "body: {body}"
        );
        assert!(
            body.contains("max:") && body.contains("x-ai/grok-4.20-beta"),
            "body: {body}"
        );
        assert!(
            body.contains("context_limit:") && body.contains("200000"),
            "body: {body}"
        );
    }

    #[test]
    fn test_migration_unknown_provider_id_hard_fails() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");
        conn.execute(
            "INSERT INTO pyramid_tier_routing \
               (tier_name, provider_id, model_id, pricing_json) \
             VALUES ('mid', 'mystery-provider', 'some-model', '{}')",
            [],
        )
        .unwrap();

        let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
        match err {
            V3MigrationError::UnknownProviderIds { ids } => {
                assert_eq!(ids, vec!["mystery-provider".to_string()]);
            }
            other => panic!("expected UnknownProviderIds, got {:?}", other),
        }

        // Nothing committed: marker still v2, no walker_provider_* rows.
        let marker = get_active_body(&conn, "migration_marker").unwrap();
        assert!(marker.contains("\"v2\""), "marker: {marker}");
        assert_eq!(count_active(&conn, "walker_provider_openrouter"), 0);
    }

    #[test]
    fn test_migration_refuses_in_progress_builds() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");
        conn.execute(
            "INSERT INTO pyramid_builds (slug, build_id, question, status) \
             VALUES ('active-slug', 'build-uuid-1', 'hello', 'running')",
            [],
        )
        .unwrap();

        let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
        match err {
            V3MigrationError::InProgressBuildsBlock(slugs) => {
                assert_eq!(slugs, vec!["active-slug".to_string()]);
            }
            other => panic!("expected InProgressBuildsBlock, got {:?}", other),
        }
        // Marker untouched.
        let marker = get_active_body(&conn, "migration_marker").unwrap();
        assert!(marker.contains("\"v2\""));
    }

    #[test]
    fn test_migration_is_idempotent_on_rerun() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");
        // First run: succeeds.
        let _ = run_v3_phase_a_migration(&mut conn, None).unwrap();
        // Second run: AlreadyMigrated.
        let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
        match err {
            V3MigrationError::AlreadyMigrated { body } => {
                assert_eq!(body, "v3-db-migrated-config-pending");
            }
            other => panic!("expected AlreadyMigrated, got {:?}", other),
        }
    }

    #[test]
    fn test_migration_preserves_notes_as_underscore_key() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");
        conn.execute(
            "INSERT INTO pyramid_tier_routing \
               (tier_name, provider_id, model_id, pricing_json, notes) \
             VALUES ('mid', 'openrouter', 'inception/mercury-2', '{}', 'custom pricing for this tier')",
            [],
        )
        .unwrap();

        let _ = run_v3_phase_a_migration(&mut conn, None).unwrap();

        let body = get_active_body(&conn, "walker_provider_openrouter").unwrap();
        // `_notes` present with the operator string.
        assert!(body.contains("_notes:"), "body: {body}");
        assert!(
            body.contains("custom pricing for this tier"),
            "body: {body}"
        );
    }

    #[test]
    fn test_migration_folds_dispatch_policy_route_to_into_walker_call_order() {
        let (_dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v2");

        // Seed a dispatch_policy contribution whose routing_rules.route_to
        // lists four providers with a budget cap on fleet.
        let dp_yaml = r#"
version: 1
routing_rules:
  - name: build
    match_config: {}
    route_to:
      - provider_id: fleet
        max_budget_credits: 500
      - provider_id: market
      - provider_id: ollama-local
      - provider_id: openrouter
"#;
        conn.execute(
            "INSERT INTO pyramid_config_contributions \
               (contribution_id, schema_type, yaml_content, status, source) \
             VALUES (?1, 'dispatch_policy', ?2, 'active', 'bundled')",
            rusqlite::params!["dp-1", dp_yaml],
        )
        .unwrap();

        let report = run_v3_phase_a_migration(&mut conn, None).unwrap();
        assert!(report.walker_call_order_written.is_some());

        let body = get_active_body(&conn, "walker_call_order").unwrap();
        assert!(
            body.contains("order: [fleet, market, local, openrouter]"),
            "body: {body}"
        );
        assert!(body.contains("fleet:"), "body: {body}");
        assert!(body.contains("max_budget_credits: 500"), "body: {body}");
    }

    #[test]
    fn test_build_walker_provider_body_dedups_model_list() {
        // Two openrouter rows for the same tier with the same model_id —
        // defensive dedup via BTreeMap grouping + per-tier seen set.
        let rows = vec![
            TierRoutingRow {
                tier_name: "mid".into(),
                provider_id: "openrouter".into(),
                model_id: "inception/mercury-2".into(),
                context_limit: Some(200_000),
                max_completion_tokens: None,
                pricing_json: Some("{}".into()),
                supported_parameters_json: None,
                notes: None,
            },
        ];
        let body = build_walker_provider_body(
            ProviderType::OpenRouter,
            &rows,
            &["inception/mercury-2".to_string(), "fallback/one".to_string()],
        );
        // Primary already in list; only fallback/one appears as addition.
        let lines: Vec<&str> = body.lines().collect();
        let mid_lines: Vec<&str> = lines
            .iter()
            .filter(|l| l.contains("inception") || l.contains("fallback/one"))
            .copied()
            .collect();
        // Model list should have inception/mercury-2 once plus fallback/one.
        let body_has_primary = body.matches("inception/mercury-2").count();
        assert_eq!(body_has_primary, 1, "body: {body}\nmid lines: {mid_lines:?}");
        assert!(body.contains("fallback/one"));
    }

    // ── Phase B tests ────────────────────────────────────────────────

    /// Seed `pyramid_config.json` with the legacy fields alongside
    /// other keys that must survive the rewrite.
    fn write_seeded_config_json(dir: &Path) {
        let body = serde_json::json!({
            "auth_token": "bearer-abc",
            "openrouter_api_key": "sk-or-v1-xxx",
            "primary_model": "inception/mercury-2",
            "fallback_model_1": "x-ai/grok-4.20-beta",
            "fallback_model_2": "moonshotai/kimi-k2.6",
            "primary_context_limit": 200000,
            "fallback_1_context_limit": 1000000,
            "partner_model": "xiaomi/mimo-v2-pro",
            "collapse_model": "x-ai/grok-4.20-beta",
            "use_chain_engine": true,
            "operational": {},
        });
        std::fs::write(
            dir.join("pyramid_config.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    /// Move the marker from `v3-db-migrated-config-pending` to `v3`
    /// state is what Phase A leaves behind. This test helper mimics
    /// that state without running the full Phase A.
    fn insert_pending_marker(conn: &Connection) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let yaml =
            "schema_type: migration_marker\nbody: \"v3-db-migrated-config-pending\"\n".to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions \
               (contribution_id, schema_type, yaml_content, status, source) \
             VALUES (?1, 'migration_marker', ?2, 'active', 'migration')",
            rusqlite::params![id, yaml],
        )
        .unwrap();
        id
    }

    #[test]
    fn test_phase_b_strips_legacy_keys_from_config_file() {
        let (dir, mut conn) = make_test_db();
        insert_pending_marker(&conn);
        write_seeded_config_json(dir.path());

        let report =
            run_v3_phase_b_migration(&mut conn, dir.path()).expect("Phase B must succeed");
        assert!(report.bytes_after > 0);
        assert!(report.bytes_before > report.bytes_after);

        let rewritten: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("pyramid_config.json")).unwrap(),
        )
        .unwrap();
        assert!(rewritten.get("primary_model").is_none());
        assert!(rewritten.get("fallback_model_1").is_none());
        assert!(rewritten.get("fallback_model_2").is_none());
        assert!(rewritten.get("primary_context_limit").is_none());
        assert!(rewritten.get("fallback_1_context_limit").is_none());
        // Non-legacy fields preserved.
        assert_eq!(
            rewritten.get("auth_token").and_then(|v| v.as_str()),
            Some("bearer-abc")
        );
        assert_eq!(
            rewritten.get("partner_model").and_then(|v| v.as_str()),
            Some("xiaomi/mimo-v2-pro")
        );
    }

    #[test]
    fn test_phase_b_idempotent_on_rerun() {
        let (dir, mut conn) = make_test_db();
        insert_pending_marker(&conn);
        write_seeded_config_json(dir.path());

        let _ = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap();

        let err = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap_err();
        match err {
            V3PhaseBError::AlreadyMigrated { body } => {
                assert_eq!(body, "v3");
            }
            other => panic!("expected AlreadyMigrated, got {:?}", other),
        }
    }

    #[test]
    fn test_phase_b_refuses_when_phase_a_not_run() {
        let (dir, mut conn) = make_test_db();
        // Marker at `v2` → Phase A hasn't run.
        insert_active_marker(&conn, "v2");

        let err = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap_err();
        match err {
            V3PhaseBError::PhaseANotRun => { /* ok */ }
            other => panic!("expected PhaseANotRun, got {:?}", other),
        }
    }

    #[test]
    fn test_phase_b_transitions_marker_to_v3() {
        let (dir, mut conn) = make_test_db();
        let prior_id = insert_pending_marker(&conn);
        write_seeded_config_json(dir.path());

        let _ = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap();

        // Active marker body is now `v3`.
        let body = get_active_body(&conn, "migration_marker").unwrap();
        assert!(body.contains("\"v3\""), "body: {body}");
        assert!(
            !body.contains("v3-db-migrated-config-pending"),
            "body: {body}"
        );
        assert_eq!(count_active(&conn, "migration_marker"), 1);

        // Prior row is superseded with `superseded_by_id` pointing at
        // the new active row.
        let (prior_status, prior_sb): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions \
                 WHERE contribution_id = ?1",
                rusqlite::params![prior_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(prior_status, "superseded");
        assert!(prior_sb.is_some(), "prior row must have superseded_by_id");
    }

    #[test]
    fn test_phase_b_handles_missing_config_file() {
        let (dir, mut conn) = make_test_db();
        insert_pending_marker(&conn);
        // Do not write pyramid_config.json.

        let report = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap();
        assert_eq!(report.bytes_before, 0);
        assert_eq!(report.bytes_after, 0);

        // Marker still transitions.
        let body = get_active_body(&conn, "migration_marker").unwrap();
        assert!(body.contains("\"v3\""));
    }

    #[test]
    fn test_phase_b_snapshot_captures_pre_rewrite_body() {
        let (dir, mut conn) = make_test_db();
        insert_pending_marker(&conn);
        write_seeded_config_json(dir.path());
        let original = std::fs::read_to_string(dir.path().join("pyramid_config.json")).unwrap();

        let _ = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap();

        let (snap_body, pm, fb1, fb2, source_file): (String, Option<String>, Option<String>, Option<String>, String) =
            conn.query_row(
                "SELECT body, primary_model, fallback_model_1, fallback_model_2, source_file \
                 FROM _pre_v3_snapshot_config ORDER BY rowid DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(snap_body, original);
        assert_eq!(pm.as_deref(), Some("inception/mercury-2"));
        assert_eq!(fb1.as_deref(), Some("x-ai/grok-4.20-beta"));
        assert_eq!(fb2.as_deref(), Some("moonshotai/kimi-k2.6"));
        assert_eq!(source_file, "pyramid_config.json");
    }

    #[test]
    fn test_phase_b_rejects_unexpected_marker_body() {
        let (dir, mut conn) = make_test_db();
        insert_active_marker(&conn, "v99-garbage");

        let err = run_v3_phase_b_migration(&mut conn, dir.path()).unwrap_err();
        match err {
            V3PhaseBError::UnexpectedMarkerBody { body } => {
                assert_eq!(body, "v99-garbage");
            }
            other => panic!("expected UnexpectedMarkerBody, got {:?}", other),
        }
    }
}
