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
use crate::pyramid::compute_chronicle::{
    EVENT_BUNDLED_CONTRIBUTION_VALIDATION_FAILED, EVENT_CONFIG_RETRACTED,
    EVENT_CONFIG_RETRACTED_TO_BUNDLED, EVENT_CONFIG_SUPERSESSION_CONFLICT,
    EVENT_RETRACTION_WALKED_DEEP,
};
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

// ── Envelope writer (Phase 0a-1 commit 5: activated body) ───────────────────
//
// Single choke point for every production-side `INSERT INTO
// pyramid_config_contributions` — the writer's INSERT is the ONLY raw
// INSERT in production code. `scripts/check-insert-sites.sh` enforces
// this invariant at CI time.
//
// Commit 5 activates three behaviors atomically with the
// `uq_config_contrib_active` partial unique index migration:
//
//   1. Normalize-then-validate via `schema_annotation` shape. When an
//      active `schema_annotation` declares a per-parameter shape for
//      the writer's target `schema_type`, the body is normalized (e.g.
//      string shorthand `"time_secs:300"` → `{kind: time_secs, value:
//      300}`) and validated against scalar / list / tagged_union /
//      tiered_map constraints (§2.11). When no annotation exists OR
//      the annotation declares no shape rules, the body is passed
//      through unchanged — this preserves commit-4 parity for every
//      schema that has not yet grown walker-v3 shape declarations.
//
//   2. `mode: TransactionMode` tells the writer whether the CALLER
//      has already opened a transaction. `OwnTransaction` is the
//      default for runtime writes — the caller wraps INSERT+UPDATE
//      supersede pairs in `conn.transaction_with_behavior(Immediate)`
//      so SQLite serializes on write intent and the unique-index
//      contention path emits `config_supersession_conflict`.
//      `JoinAmbient` is used by the §5.3 migration path and by
//      boot-time bundled loads where a top-level transaction is
//      already open. The writer itself does not open a transaction
//      in either mode — it just performs the INSERT inside whichever
//      scope the caller established.
//
//   3. `write_mode: WriteMode` selects the validation-failure policy.
//      `Strict` returns `ContributionWriterError::ValidationFailed`.
//      `BundledBootSkipOnFail` emits
//      `EVENT_BUNDLED_CONTRIBUTION_VALIDATION_FAILED` chronicle event
//      and returns `ContributionWriterError::BundledValidationSkipped`
//      so the bundled manifest loader can continue with other rows
//      instead of bricking the install (§2.11 / Root 22 / A-C4).
//
// SQLITE_CONSTRAINT on the `uq_config_contrib_active` partial index
// maps to `ContributionWriterError::SupersessionConflict`. Callers
// surface the typed error (no automatic retry). The bundled manifest
// loader performs a user-active pre-check (see
// `wire_migration::insert_bundled_contribution`) so the common
// "user refined a bundled default" case does not raise a spurious
// conflict.
//
// See `docs/plans/walker-provider-configs-and-slot-policy-v3.md`
// §2.11, §2.16.1, and §5.3 step 7 for the full contract.

/// How the envelope writer should couple with surrounding SQL state.
///
/// `OwnTransaction` is the default for runtime writes: the CALLER opens
/// `conn.transaction_with_behavior(TransactionBehavior::Immediate)`
/// around every INSERT+UPDATE supersede pair. `JoinAmbient` says the
/// writer is being invoked INSIDE a caller's already-open transaction
/// (§5.3 migration, boot-time bundled load, draft/accept paths). The
/// writer itself does not open a transaction in either mode; the
/// variants exist so plan-integrity Check 13 can audit every call
/// site against the single-writer invariant in §2.16.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    /// Caller opens a BEGIN IMMEDIATE transaction around the INSERT
    /// (plus any follow-up UPDATE on the prior row). Default for every
    /// runtime-path supersession.
    OwnTransaction,
    /// Caller is already inside a transaction — writer runs bare and
    /// relies on the outer transaction for atomicity. Used by the
    /// §5.3 migration walk, boot-time bundled loads, and
    /// supersede-then-update pairs (`supersede_config_contribution`,
    /// `commit_pulled_active`, `accept_config_draft`,
    /// `create_draft_supersession`, `persist_migration_proposal`).
    JoinAmbient,
}

/// Validation-failure policy for the envelope writer. Commit 5
/// separates runtime writes (which must fail loud on malformed
/// contributions) from boot-time bundled loads (which must skip
/// malformed rows and log, so other bundled contributions still
/// land — per §2.11 Root 22 A-C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Runtime-path default. Validation failure returns
    /// `ContributionWriterError::ValidationFailed`. Caller is
    /// expected to surface the error to the operator (Settings save,
    /// HTTP 4xx, proposal reject, etc).
    Strict,
    /// Bundled-manifest-loader mode. Validation failure emits
    /// `EVENT_BUNDLED_CONTRIBUTION_VALIDATION_FAILED` on the
    /// event bus (or, when no bus is available at boot, logs via
    /// `tracing::warn!`) and returns
    /// `ContributionWriterError::BundledValidationSkipped` so the
    /// loader can continue to the next manifest entry.
    BundledBootSkipOnFail,
}

impl Default for WriteMode {
    fn default() -> Self {
        Self::Strict
    }
}

/// Input to `write_contribution_envelope`. The writer populates every
/// column of `pyramid_config_contributions`; callers pass `None` / default
/// for columns they don't care about and the shim substitutes the same
/// defaults each INSERT site used before the refactor:
///   * `contribution_id`: required — caller generates the UUID so it
///     can reference the row (e.g. in a follow-up UPDATE on the prior).
///   * `wire_native_metadata_json`: `None` → `'{}'` (matches the pre-
///     Phase-5 literal that several test-side sites and the
///     `_migration_marker` rows use). Producers that care supply
///     the serialized `WireNativeMetadata`.
///   * `wire_publication_state_json`: always `'{}'` at insert time — no
///     existing site overrides this.
///   * `superseded_by_id`: always `NULL` at insert time — no existing
///     site sets this on the INSERT (always on a follow-up UPDATE).
///   * `accepted_at`: see `AcceptedAt` for the three policies the
///     existing sites use.
///   * `needs_migration`: `None` → column default (0). Only
///     `migration_config::persist_migration_proposal` passes an explicit
///     value and it's 0 (kept for future phases that may want 1).
#[derive(Debug, Clone)]
pub struct ContributionEnvelopeInput {
    /// UUID v4 generated by the caller. The writer does not mint IDs so
    /// supersede-style callers can reference the new row before the
    /// INSERT commits.
    pub contribution_id: String,
    /// `None` = global-scope config (e.g. `tier_routing`,
    /// `_migration_marker`).
    pub slug: Option<String>,
    pub schema_type: String,
    /// YAML or JSON body; the shim does not interpret it. Commit 5 adds
    /// schema-type-aware normalization+validation here.
    pub body: String,
    /// Serialized `WireNativeMetadata`; `None` → `'{}'` literal.
    pub wire_native_metadata_json: Option<String>,
    pub supersedes_id: Option<String>,
    pub triggering_note: Option<String>,
    /// One of "active", "draft", "proposed", "superseded", "rejected".
    pub status: String,
    /// One of "local", "wire", "agent", "bundled", "migration".
    pub source: String,
    pub wire_contribution_id: Option<String>,
    pub created_by: Option<String>,
    /// How to populate the `accepted_at` column. See the variants.
    pub accepted_at: AcceptedAt,
    /// `None` → column default (0). The `needs_migration` column is
    /// written explicitly only by `persist_migration_proposal`.
    pub needs_migration: Option<i64>,
    /// Validation-failure policy. Defaults to `Strict`. The bundled
    /// manifest loader (`insert_bundled_contribution` in
    /// `wire_migration.rs`) overrides to `BundledBootSkipOnFail` so a
    /// malformed bundled row cannot brick boot.
    pub write_mode: WriteMode,
}

/// `accepted_at` column policy. Mirrors the three SQL patterns the
/// existing 17 production INSERT sites use verbatim:
///   * `Now` → `datetime('now')` literal (most sites).
///   * `Null` → `NULL` literal (draft-status rows whose acceptance
///     timestamp will be stamped later by the promote path).
///   * `ActiveOnly` → `CASE WHEN status = 'active' THEN datetime('now')
///     ELSE NULL END` (used by `create_config_contribution_with_metadata`
///     where the caller picks status and the stamp follows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptedAt {
    Now,
    Null,
    ActiveOnly,
}

/// Error type returned by `write_contribution_envelope`. Commit 5
/// adds `SupersessionConflict` (SQLITE_CONSTRAINT on the
/// `uq_config_contrib_active` partial unique index),
/// `ValidationFailed` (schema_annotation shape mismatch in Strict
/// mode), and `BundledValidationSkipped` (shape mismatch in
/// BundledBootSkipOnFail mode — not a hard error; callers treat this
/// as a skip-and-continue).
#[derive(Debug, thiserror::Error)]
pub enum ContributionWriterError {
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    /// SQLITE_CONSTRAINT on `uq_config_contrib_active`: a concurrent
    /// writer landed another `status='active'` row for the same
    /// `(COALESCE(slug, '__global__'), schema_type)` before this
    /// writer's INSERT committed. Surfaced, not retried — the operator
    /// / caller decides.
    #[error("supersession conflict for schema_type={schema_type} slug={slug:?}: another active row exists")]
    SupersessionConflict {
        schema_type: String,
        slug: Option<String>,
    },
    /// Strict-mode shape validation failure. `details` names the
    /// parameter and the violation ("field `breaker_reset` expected
    /// tagged union variant of {per_build, probe_based, time_secs},
    /// got `...`").
    #[error("validation failed for schema_type={schema_type}: {details}")]
    ValidationFailed {
        schema_type: String,
        details: String,
    },
    /// BundledBootSkipOnFail-mode validation failure. Not a hard
    /// error — chronicle event emitted, caller skips the row and
    /// continues loading the rest of the manifest.
    #[error("bundled contribution {contribution_id} (schema_type={schema_type}) skipped: validation failed")]
    BundledValidationSkipped {
        contribution_id: String,
        schema_type: String,
    },
    /// Target contribution_id not present in pyramid_config_contributions.
    #[error("contribution {contribution_id} not found")]
    ContributionNotFound { contribution_id: String },
    /// §5.4.4 — refused to retract the bundled floor (nothing to revert to).
    #[error("retraction refused for {contribution_id}: {reason}")]
    RetractionRefused {
        contribution_id: String,
        reason: String,
    },
    /// §5.5.3 — supersession chain walk hit a cycle or exceeded the depth
    /// ceiling (16 hops). Loud fail; operator must investigate the chain.
    #[error("retraction chain corrupt at {contribution_id}: {reason}")]
    RetractionChainCorrupt {
        contribution_id: String,
        reason: String,
    },
}

/// Outcome of `retract_config_contribution`. Callers can use this to
/// decide whether to notify UI, trigger ScopeCache rebuild, or refresh
/// downstream state.
#[derive(Debug, Clone)]
pub enum RetractionOutcome {
    /// Walked the supersession chain backwards and reactivated the
    /// first non-retracted ancestor. `walked_hops == 1` means the
    /// immediate parent; `> 1` means intervening retracted ancestors
    /// were skipped (emits `retraction_walked_deep`).
    ReactivatedAncestor {
        retracted_id: String,
        reactivated_id: String,
        walked_hops: u32,
    },
    /// Walked off the end of the supersession chain; every ancestor
    /// retracted. Reactivated the bundled floor. Emits
    /// `config_retracted_to_bundled`.
    ReactivatedBundledFloor {
        retracted_id: String,
        reactivated_id: String,
    },
}

/// Returned from `write_contribution_envelope` — just the UUID the caller
/// supplied, re-emitted so callers can chain without re-threading the id.
pub type ContributionId = String;

/// Phase 0a-1 commit 5 activated writer. Normalize-then-validate via
/// active `schema_annotation`, then INSERT inside the caller's
/// transaction scope. Maps SQLITE_CONSTRAINT on
/// `uq_config_contrib_active` to `SupersessionConflict`. See the
/// module-level comment for the full contract.
///
/// `mode` is informational for future plan-integrity audit passes —
/// the writer itself does not branch on it for transaction management
/// (callers handle BEGIN IMMEDIATE per §2.16.1). `write_mode` selects
/// the validation-failure policy.
pub fn write_contribution_envelope(
    conn: &Connection,
    input: ContributionEnvelopeInput,
    _mode: TransactionMode,
) -> Result<ContributionId, ContributionWriterError> {
    // ── Normalize + validate against active schema_annotation ──────
    //
    // Best-effort load: if the annotation is missing, the schema_type
    // has no shape rules yet, or the load fails, the body is passed
    // through unchanged (commit-4 parity). Walker-v3 annotations land
    // in Phase 0b; until then the validator is infrastructure only.
    let normalized_body = match normalize_and_validate_body(
        conn,
        &input.schema_type,
        &input.body,
    ) {
        Ok(body) => body,
        Err(details) => match input.write_mode {
            WriteMode::Strict => {
                return Err(ContributionWriterError::ValidationFailed {
                    schema_type: input.schema_type.clone(),
                    details,
                });
            }
            WriteMode::BundledBootSkipOnFail => {
                // Chronicle emission at boot runs without a build bus;
                // log via tracing instead. The const is defined in
                // compute_chronicle so grep-verification of emission
                // sites finds this reference.
                warn!(
                    event = EVENT_BUNDLED_CONTRIBUTION_VALIDATION_FAILED,
                    contribution_id = %input.contribution_id,
                    schema_type = %input.schema_type,
                    validation_error = %details,
                    "bundled contribution skipped: shape validation failed"
                );
                return Err(ContributionWriterError::BundledValidationSkipped {
                    contribution_id: input.contribution_id,
                    schema_type: input.schema_type,
                });
            }
        },
    };

    // `wire_publication_state_json` is always `'{}'` at insert; no
    // existing site overrides it.
    let metadata_json = input
        .wire_native_metadata_json
        .unwrap_or_else(|| "{}".to_string());

    // Bind order (sqlite positional params, 1-indexed):
    //   ?1  contribution_id
    //   ?2  slug
    //   ?3  schema_type
    //   ?4  body
    //   ?5  wire_native_metadata_json
    //   ?6  supersedes_id
    //   ?7  triggering_note
    //   ?8  status           (also referenced by the ActiveOnly accepted_at branch)
    //   ?9  source
    //   ?10 wire_contribution_id
    //   ?11 created_by
    //   ?12 needs_migration  (bound only when `Some(_)`)
    let accepted_at_sql = match input.accepted_at {
        AcceptedAt::Now => "datetime('now')",
        AcceptedAt::Null => "NULL",
        AcceptedAt::ActiveOnly => {
            "CASE WHEN ?8 = 'active' THEN datetime('now') ELSE NULL END"
        }
    };

    // The `needs_migration` column is omitted from the INSERT when the
    // caller leaves it as `None` (column default = 0 applies). This
    // mirrors the 16 production sites that never write the column.
    let (needs_migration_col, needs_migration_val_sql) = match input.needs_migration {
        None => ("", ""),
        Some(_) => (", needs_migration", ", ?12"),
    };

    let sql = format!(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at{needs_migration_col}
         ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, '{{}}',
            ?6, NULL, ?7,
            ?8, ?9, ?10, ?11, {accepted_at_sql}{needs_migration_val_sql}
         )",
    );

    let exec = match input.needs_migration {
        None => conn.execute(
            &sql,
            rusqlite::params![
                input.contribution_id,
                input.slug,
                input.schema_type,
                normalized_body,
                metadata_json,
                input.supersedes_id,
                input.triggering_note,
                input.status,
                input.source,
                input.wire_contribution_id,
                input.created_by,
            ],
        ),
        Some(needs_migration) => conn.execute(
            &sql,
            rusqlite::params![
                input.contribution_id,
                input.slug,
                input.schema_type,
                normalized_body,
                metadata_json,
                input.supersedes_id,
                input.triggering_note,
                input.status,
                input.source,
                input.wire_contribution_id,
                input.created_by,
                needs_migration,
            ],
        ),
    };

    match exec {
        Ok(_) => Ok(input.contribution_id),
        Err(e) => Err(map_insert_error(e, &input.schema_type, &input.slug)),
    }
}

/// Translate a rusqlite error into the typed ContributionWriterError.
/// SQLITE_CONSTRAINT hits with a message mentioning
/// `uq_config_contrib_active` become `SupersessionConflict`; everything
/// else stays `Db(e)`. Emits the `config_supersession_conflict`
/// chronicle event via tracing at the conflict site so plan-integrity
/// Check 2 finds an emission point.
fn map_insert_error(
    err: rusqlite::Error,
    schema_type: &str,
    slug: &Option<String>,
) -> ContributionWriterError {
    if let rusqlite::Error::SqliteFailure(ref ffi_err, ref msg) = err {
        if ffi_err.code == rusqlite::ErrorCode::ConstraintViolation {
            // SQLite surfaces "UNIQUE constraint failed: index
            // 'uq_config_contrib_active'" OR "UNIQUE constraint failed:
            // pyramid_config_contributions.<col>". The index-named
            // form is the one we care about.
            let msg_str = msg.as_deref().unwrap_or("");
            if msg_str.contains("uq_config_contrib_active") {
                // Emit chronicle event stub (no bus available in the
                // writer's scope; log via tracing so Check 2 greps the
                // const to an emission site).
                warn!(
                    event = EVENT_CONFIG_SUPERSESSION_CONFLICT,
                    schema_type = %schema_type,
                    slug = ?slug,
                    sqlite_msg = %msg_str,
                    "supersession conflict: another active contribution exists for the same (schema_type, slug)"
                );
                return ContributionWriterError::SupersessionConflict {
                    schema_type: schema_type.to_string(),
                    slug: slug.clone(),
                };
            }
        }
    }
    ContributionWriterError::Db(err)
}

// ── Shape validator (Phase 0a-1 commit 5) ───────────────────────────
//
// Loads the active `schema_annotation` for `schema_type` and, when
// one exists with per-parameter shape rules, walks the YAML body
// applying normalization and validation.
//
// Commit-5 scope is intentionally minimal: `scalar`, `list`,
// `tagged_union`, `tiered_map` per §2.11. No JSON-Schema engine.
// Walker-v3 annotations in Phase 0b will exercise every branch; for
// every schema that has no annotation or whose annotation declares
// no `parameters` map, the body is returned as-is (commit-4 parity).
//
// Empty bodies are accepted unconditionally — `_migration_marker`
// rows carry `body: ""` (wanderer note 5) and the annotation system
// does not define them.

/// Normalize-and-validate entry point. Returns the (possibly rewritten)
/// body on success, or a human-readable description of the first
/// violation on failure.
fn normalize_and_validate_body(
    conn: &Connection,
    schema_type: &str,
    body: &str,
) -> Result<String, String> {
    // Empty body: always pass through. Empty bodies are used by
    // `_migration_marker` and the test-side
    // `annotation_allows_empty_body` case.
    if body.trim().is_empty() {
        return Ok(body.to_string());
    }

    // Look up the annotation body inline — a direct SQL query avoids
    // threading an Arc<SchemaRegistry> through every caller and
    // matches the approach used by `yaml_renderer::load_schema_annotation_for`.
    //
    // Lookup precedence mirrors `schema_registry::find_active_annotation_id`:
    // (a) direct-slug match (the common case — annotations are keyed
    // on `applies_to`), (b) scan fallback — walk all active annotations
    // and match on `applies_to:` / `schema_type:` line.
    let annotation_body = match load_annotation_for_schema_type(conn, schema_type) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(body.to_string()),
        Err(_) => return Ok(body.to_string()), // best-effort
    };

    // Parse both documents. Any YAML parse error on the annotation
    // side is a no-op (same best-effort policy). A YAML parse error
    // on the body side, WHEN an annotation exists, is itself a shape
    // violation (malformed YAML).
    let annotation_value: serde_yaml::Value = match serde_yaml::from_str(&annotation_body) {
        Ok(v) => v,
        Err(_) => return Ok(body.to_string()),
    };
    let parameters = match annotation_value
        .get("parameters")
        .and_then(|p| p.as_mapping())
    {
        Some(m) => m,
        None => return Ok(body.to_string()), // annotation declares no shape rules
    };

    let mut body_value: serde_yaml::Value = serde_yaml::from_str(body)
        .map_err(|e| format!("body is not well-formed YAML: {e}"))?;

    // Walk the declared parameters. Normalization can mutate the body
    // in-place; validation errors short-circuit with a path-prefixed
    // message.
    for (param_name, shape_decl) in parameters {
        let Some(param_name_str) = param_name.as_str() else {
            continue;
        };
        normalize_and_validate_parameter(
            &mut body_value,
            param_name_str,
            shape_decl,
        )
        .map_err(|e| format!("parameter `{param_name_str}`: {e}"))?;
    }

    // Re-serialize. If the normalization walk mutated the body,
    // re-serialization produces canonical YAML; otherwise the round
    // trip is a no-op aside from whitespace normalization.
    serde_yaml::to_string(&body_value)
        .map_err(|e| format!("failed to serialize normalized body: {e}"))
}

/// Look up the active schema_annotation body targeting `schema_type`.
/// Returns `Ok(None)` when no annotation is registered for this
/// schema_type (common case — commit-4 parity).
fn load_annotation_for_schema_type(
    conn: &Connection,
    schema_type: &str,
) -> rusqlite::Result<Option<String>> {
    // (a) direct-slug match — annotations keyed on applies_to via slug.
    let direct: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE schema_type = 'schema_annotation'
               AND status = 'active'
               AND superseded_by_id IS NULL
               AND slug = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![schema_type],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(body) = direct {
        return Ok(Some(body));
    }

    // (b) scan fallback — walk all active schema_annotations and
    // match on a top-level `applies_to:` / `schema_type:` line. Cheap:
    // annotation count is O(number of config types).
    let mut stmt = conn.prepare(
        "SELECT yaml_content FROM pyramid_config_contributions
         WHERE schema_type = 'schema_annotation'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let body = row?;
        if annotation_targets(&body, schema_type) {
            return Ok(Some(body));
        }
    }
    Ok(None)
}

/// Cheap line-scan check: does the YAML body target `schema_type`
/// via a top-level `applies_to:` or `schema_type:` line? Avoids a
/// full YAML parse.
fn annotation_targets(yaml: &str, target: &str) -> bool {
    for line in yaml.lines() {
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let trimmed = line.trim_start();
        for key in ["applies_to:", "schema_type:"] {
            if let Some(rest) = trimmed.strip_prefix(key) {
                let value = rest.trim().trim_matches(|c: char| c == '"' || c == '\'');
                if value == target {
                    return true;
                }
            }
        }
    }
    false
}

/// Normalize + validate a single parameter in the body against its
/// declared shape. Mutates `body_value` in place (e.g. collapses
/// `"time_secs:300"` to a struct). Returns a description of the first
/// violation on failure.
fn normalize_and_validate_parameter(
    body_value: &mut serde_yaml::Value,
    param_name: &str,
    shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    // Body must be a mapping for per-key parameter validation to apply.
    // Non-mapping bodies (rare; some schemas use a top-level scalar)
    // fall through to pass-through.
    let Some(body_map) = body_value.as_mapping_mut() else {
        return Ok(());
    };
    let key = serde_yaml::Value::String(param_name.to_string());
    let Some(param_value) = body_map.get_mut(&key) else {
        return Ok(()); // absent field is fine — shape rules gate presence only when required
    };

    let shape = shape_decl
        .get("shape")
        .and_then(|s| s.as_str())
        .unwrap_or("");

    match shape {
        "" => Ok(()), // no shape declared for this parameter — passthrough
        "scalar" => validate_scalar(param_value, shape_decl),
        "list" => validate_list(param_value, shape_decl),
        "tagged_union" => normalize_and_validate_tagged_union(param_value, shape_decl),
        "tiered_map" => validate_tiered_map(param_value, shape_decl),
        other => {
            // Unknown shapes are treated as passthrough so an
            // annotation author can introduce new shape kinds without
            // breaking old writers. Plan-integrity Check 10 catches
            // unused enum variants elsewhere.
            debug!(shape = %other, "unknown shape in annotation — passthrough");
            Ok(())
        }
    }
}

fn validate_scalar(
    value: &serde_yaml::Value,
    shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    let expected = shape_decl
        .get("type")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let ok = match expected {
        "string" => value.is_string(),
        "bool" => value.is_bool(),
        "u64" | "i64" | "u32" | "int" => value.is_i64() || value.is_u64(),
        "f64" | "float" => value.is_f64() || value.is_i64() || value.is_u64(),
        "" => true, // untyped scalar — any leaf passes
        _ => true,
    };
    if !ok {
        return Err(format!("expected scalar of type `{expected}`"));
    }
    Ok(())
}

fn validate_list(
    value: &serde_yaml::Value,
    _shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    // §2.11 normalization: "empty list → None" is expressed at read
    // time via typed accessor; at validation time we just require the
    // value to be a sequence (or null, which the typed accessor treats
    // as an empty list).
    if value.is_null() || value.is_sequence() {
        return Ok(());
    }
    Err("expected list".to_string())
}

fn normalize_and_validate_tagged_union(
    value: &mut serde_yaml::Value,
    shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    // Accept the string-shorthand form via `accepts_string_shorthand`.
    // §2.11 worked example: `"time_secs:300"` → `{kind: "time_secs",
    // value: 300}`.
    if let Some(s) = value.as_str() {
        let shorthands = shape_decl
            .get("accepts_string_shorthand")
            .and_then(|v| v.as_sequence());
        if let Some(patterns) = shorthands {
            for pattern_entry in patterns {
                if let Some(map) = pattern_entry.as_mapping() {
                    let pattern = map
                        .get(&serde_yaml::Value::String("pattern".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Commit-5 minimal shorthand: exact-prefix match
                    // on `name` or `name:<int>` forms. Regex support
                    // arrives with walker_* annotations (Phase 0b).
                    if pattern.starts_with('^') && pattern.ends_with('$') {
                        // Crude literal match of alternation or integer
                        // suffix — see tests for the two supported
                        // forms. Callers with richer patterns can
                        // post-normalize in Phase 0b.
                        let stripped =
                            &pattern[1..pattern.len() - 1];
                        if let Some(literals) = stripped.strip_prefix("(") {
                            // alternation of bare names, e.g.
                            // ^(per_build|probe_based)$
                            let literals = literals.trim_end_matches(')');
                            if literals.split('|').any(|lit| lit == s) {
                                *value = serde_yaml::from_str(&format!(
                                    "kind: {s}\n"
                                ))
                                .map_err(|e| format!("normalize tagged union: {e}"))?;
                                return validate_tagged_union_struct(value, shape_decl);
                            }
                        } else if let Some((name, rest)) = stripped.split_once(":(") {
                            // integer-suffix form, e.g.
                            // ^time_secs:(\d+)$
                            let _ = rest;
                            if let Some(suffix) = s.strip_prefix(&format!("{name}:")) {
                                if let Ok(n) = suffix.parse::<u64>() {
                                    *value = serde_yaml::from_str(&format!(
                                        "kind: {name}\nvalue: {n}\n"
                                    ))
                                    .map_err(|e| format!("normalize tagged union: {e}"))?;
                                    return validate_tagged_union_struct(value, shape_decl);
                                }
                            }
                        }
                    }
                }
            }
        }
        return Err(format!(
            "string `{s}` does not match any declared shorthand for tagged_union"
        ));
    }
    validate_tagged_union_struct(value, shape_decl)
}

fn validate_tagged_union_struct(
    value: &serde_yaml::Value,
    shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    let map = value
        .as_mapping()
        .ok_or_else(|| "expected mapping for tagged_union".to_string())?;
    let kind = map
        .get(&serde_yaml::Value::String("kind".to_string()))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "tagged_union missing `kind`".to_string())?;
    let variants = shape_decl
        .get("variants")
        .and_then(|v| v.as_mapping());
    if let Some(variants) = variants {
        if !variants.contains_key(&serde_yaml::Value::String(kind.to_string())) {
            let declared: Vec<String> = variants
                .keys()
                .filter_map(|k| k.as_str().map(String::from))
                .collect();
            return Err(format!(
                "kind `{kind}` not in declared variants {declared:?}"
            ));
        }
    }
    Ok(())
}

fn validate_tiered_map(
    value: &serde_yaml::Value,
    _shape_decl: &serde_yaml::Value,
) -> Result<(), String> {
    // §2.11 scope_behavior: at scopes 3-4 a tiered_map is
    // `{tier: [values]}`; at scopes 1-2 it's a flat list. Commit-5
    // validator accepts EITHER shape (mapping of lists OR a flat list);
    // path-rule dispatch (§5.5.5) ships with walker_slot_policy in
    // Phase 0b.
    if value.is_null() {
        return Ok(());
    }
    if value.is_sequence() {
        return Ok(());
    }
    if let Some(map) = value.as_mapping() {
        for (_tier, tier_val) in map {
            if !tier_val.is_sequence() && !tier_val.is_null() {
                return Err("tiered_map values must be lists".to_string());
            }
        }
        return Ok(());
    }
    Err("tiered_map must be mapping-of-lists or flat list".to_string())
}

// ── One-time migration: pre-index dedup + unique index ──────────────

/// §5.3 step 7 + §2.16.1 Phase 0a migration. Idempotent; safe to run
/// on every boot. Short-circuits via `sqlite_master` check when the
/// `uq_config_contrib_active` index already exists.
///
/// Runs as a single SQL transaction:
///   1. Snapshot pre-dedup duplicate-active rows into
///      `_pre_v3_dedup_snapshot` (retained 30 days per §5.5.9).
///   2. Mark all-but-newest-id as `status='superseded'` per duplicate
///      key, where the key is `(COALESCE(slug, '__global__'), schema_type)`.
///   3. `CREATE UNIQUE INDEX uq_config_contrib_active` over the same
///      normalized expression, filtered on `status='active'`.
///
/// Called from `db::init_pyramid_db` after the legacy-migration
/// helpers run so existing `_migration_marker` rows are already
/// present.
pub fn ensure_config_contrib_active_unique_index(conn: &Connection) -> Result<()> {
    // Idempotency check via sqlite_master.
    let index_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'uq_config_contrib_active'",
        [],
        |row| row.get(0),
    )?;
    if index_exists > 0 {
        return Ok(());
    }

    // Single transaction wrapping dedup + index creation. If any step
    // fails, the whole migration rolls back and the next boot retries.
    conn.execute_batch(
        "BEGIN IMMEDIATE TRANSACTION;

         CREATE TABLE IF NOT EXISTS _pre_v3_dedup_snapshot (
           snapshot_at TEXT NOT NULL,
           contribution_id TEXT NOT NULL,
           slug TEXT,
           schema_type TEXT NOT NULL,
           status TEXT NOT NULL,
           deactivated_by_dedup INTEGER NOT NULL DEFAULT 0
         );

         INSERT INTO _pre_v3_dedup_snapshot
           (snapshot_at, contribution_id, slug, schema_type, status, deactivated_by_dedup)
           SELECT datetime('now'), contribution_id, slug, schema_type, status, 0
           FROM pyramid_config_contributions
           WHERE status = 'active'
             AND (COALESCE(slug, '__global__'), schema_type) IN (
               SELECT COALESCE(slug, '__global__'), schema_type
               FROM pyramid_config_contributions
               WHERE status = 'active'
               GROUP BY COALESCE(slug, '__global__'), schema_type
               HAVING COUNT(*) > 1
             );

         UPDATE pyramid_config_contributions
           SET status = 'superseded'
           WHERE status = 'active'
             AND id NOT IN (
               SELECT MAX(id)
               FROM pyramid_config_contributions
               WHERE status = 'active'
               GROUP BY COALESCE(slug, '__global__'), schema_type
             );

         UPDATE _pre_v3_dedup_snapshot
           SET deactivated_by_dedup = 1
           WHERE contribution_id IN (
             SELECT contribution_id FROM pyramid_config_contributions
             WHERE status = 'superseded'
               AND contribution_id IN (SELECT contribution_id FROM _pre_v3_dedup_snapshot)
           );

         CREATE UNIQUE INDEX uq_config_contrib_active
           ON pyramid_config_contributions(COALESCE(slug, '__global__'), schema_type)
           WHERE status = 'active';

         COMMIT;",
    )?;
    debug!("ensure_config_contrib_active_unique_index: migration applied");
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
    write_contribution_envelope(
        conn,
        ContributionEnvelopeInput {
            contribution_id: contribution_id.clone(),
            slug: slug.map(|s| s.to_string()),
            schema_type: schema_type.to_string(),
            body: yaml_content.to_string(),
            wire_native_metadata_json: Some(metadata_json),
            supersedes_id: None,
            triggering_note: triggering_note.map(|s| s.to_string()),
            status: status.to_string(),
            source: source.to_string(),
            wire_contribution_id: None,
            created_by: created_by.map(|s| s.to_string()),
            accepted_at: AcceptedAt::ActiveOnly,
            needs_migration: None,
            write_mode: WriteMode::default(),
        },
        TransactionMode::OwnTransaction,
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

    // Phase 0a-1 commit 5 / §2.16.1: BEGIN IMMEDIATE so concurrent
    // supersessions serialize on write intent. Second concurrent
    // supersession fails loud at the unique-index guard
    // (ContributionWriterError::SupersessionConflict).
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

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

    // Phase 0a-1 commit 5: UPDATE prior → 'superseded' BEFORE the
    // INSERT so the unique index `uq_config_contrib_active` never
    // sees two active rows for the same (schema_type, slug) at the
    // same instant. The `superseded_by_id` back-link is patched in
    // after the INSERT (second UPDATE).
    let new_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'superseded'
         WHERE contribution_id = ?1",
        rusqlite::params![prior_contribution_id],
    )?;

    write_contribution_envelope(
        &tx,
        ContributionEnvelopeInput {
            contribution_id: new_id.clone(),
            slug: slug.clone(),
            schema_type: schema_type.clone(),
            body: new_yaml_content.to_string(),
            wire_native_metadata_json: Some(metadata_json),
            supersedes_id: Some(prior_contribution_id.to_string()),
            triggering_note: Some(triggering_note.to_string()),
            status: "active".to_string(),
            source: source.to_string(),
            wire_contribution_id: None,
            created_by: created_by.map(|s| s.to_string()),
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::default(),
        },
        TransactionMode::JoinAmbient,
    )?;

    // Back-fill the prior row's forward pointer.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET superseded_by_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![new_id, prior_contribution_id],
    )?;

    tx.commit()?;
    Ok(new_id)
}

/// Retract a config contribution and walk backwards through the
/// supersession chain to reactivate the nearest non-retracted ancestor.
/// Plan rev 1.0.2 §5.4.4 + §5.5.3.
///
/// Semantics:
/// - Marks the target row `status='retracted'`.
/// - Walks `supersedes_id` backwards, skipping retracted ancestors, with
///   depth ceiling 16 and visited-set cycle detection.
/// - First non-retracted ancestor found → reactivate (status='active',
///   clear superseded_by_id). Emits `config_retracted`. If the walk
///   traversed more than 1 hop, additionally emits `retraction_walked_deep`.
/// - If all ancestors retracted and a bundled floor is in the chain,
///   reactivate the bundled floor with `config_retracted_to_bundled`.
/// - Refuses to retract a bundled-floor row (source='bundled' AND
///   supersedes_id IS NULL — nothing to revert to).
/// - On cycle or depth exhaustion → RetractionChainCorrupt.
///
/// Entire operation runs inside a single BEGIN IMMEDIATE transaction so
/// concurrent writers serialize; the target's retraction, the ancestor's
/// reactivation, and the chronicle emission are atomic. Wire-originating
/// retractions (pulled via sync) hit the same code path.
///
/// The caller is responsible for triggering a ScopeCache rebuild after
/// a successful retraction. For now, emit the chronicle event and let
/// downstream listeners pick up the state change. Phase 0a-2 WS5's boot
/// sequence wires an ArcSwap reload trigger; future refactor can take
/// an optional rebuild-notify channel here.
pub fn retract_config_contribution(
    conn: &mut Connection,
    contribution_id: &str,
    triggering_note: &str,
) -> std::result::Result<RetractionOutcome, ContributionWriterError> {
    const DEPTH_CEILING: u32 = 16;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Load target.
    let target = load_contribution_for_retract(&tx, contribution_id)?;

    // Refusal: target is the bundled floor (no ancestor to revert to).
    if target.source == "bundled" && target.supersedes_id.is_none() {
        return Err(ContributionWriterError::RetractionRefused {
            contribution_id: contribution_id.to_string(),
            reason: "bundled floor — no ancestor to revert to".to_string(),
        });
    }

    // Mark target retracted. The superseded_by_id forward pointer on
    // target's prior (if any) is preserved — retraction is distinct
    // from supersession; downstream readers use the status field to
    // distinguish. (load_active_config_contribution filters on
    // status='active' so retracted rows naturally drop out of reads.)
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'retracted', triggering_note = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![triggering_note, contribution_id],
    )?;

    // Walk ancestors.
    let mut candidate_id = target.supersedes_id.clone();
    let mut hops: u32 = 0;
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(contribution_id.to_string());
    let mut last_bundled_floor_id: Option<String> = None;

    while let Some(cid) = candidate_id {
        hops += 1;
        if hops > DEPTH_CEILING {
            return Err(ContributionWriterError::RetractionChainCorrupt {
                contribution_id: contribution_id.to_string(),
                reason: format!("depth ceiling {DEPTH_CEILING} exceeded"),
            });
        }
        if !visited.insert(cid.clone()) {
            return Err(ContributionWriterError::RetractionChainCorrupt {
                contribution_id: contribution_id.to_string(),
                reason: format!("cycle detected at {cid}"),
            });
        }

        let candidate = match load_contribution_for_retract(&tx, &cid) {
            Ok(c) => c,
            Err(ContributionWriterError::ContributionNotFound { .. }) => {
                return Err(ContributionWriterError::RetractionChainCorrupt {
                    contribution_id: contribution_id.to_string(),
                    reason: format!("dangling supersedes_id pointer to {cid}"),
                });
            }
            Err(e) => return Err(e),
        };

        // Track any bundled floor we walk past; used as the fallback if
        // every ancestor is retracted.
        if candidate.source == "bundled" && candidate.supersedes_id.is_none() {
            last_bundled_floor_id = Some(candidate.contribution_id.clone());
        }

        if candidate.status != "retracted" {
            // Alive ancestor — reactivate and return.
            tx.execute(
                "UPDATE pyramid_config_contributions
                 SET status = 'active', superseded_by_id = NULL
                 WHERE contribution_id = ?1",
                rusqlite::params![candidate.contribution_id],
            )?;
            tx.commit()?;

            warn!(
                event = EVENT_CONFIG_RETRACTED,
                retracted_id = contribution_id,
                reactivated_id = %candidate.contribution_id,
                hops,
                "config retracted; ancestor reactivated"
            );
            if hops > 1 {
                warn!(
                    event = EVENT_RETRACTION_WALKED_DEEP,
                    retracted_id = contribution_id,
                    reactivated_id = %candidate.contribution_id,
                    hops,
                    "retraction walked past {} retracted ancestors", hops - 1
                );
            }

            return Ok(RetractionOutcome::ReactivatedAncestor {
                retracted_id: contribution_id.to_string(),
                reactivated_id: candidate.contribution_id,
                walked_hops: hops,
            });
        }

        candidate_id = candidate.supersedes_id;
    }

    // Walked off the end; every ancestor was retracted. If we passed
    // through a bundled floor, resurrect it.
    if let Some(floor_id) = last_bundled_floor_id {
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'active', superseded_by_id = NULL
             WHERE contribution_id = ?1",
            rusqlite::params![floor_id],
        )?;
        tx.commit()?;

        warn!(
            event = EVENT_CONFIG_RETRACTED_TO_BUNDLED,
            retracted_id = contribution_id,
            reactivated_id = %floor_id,
            hops,
            "retraction walked chain to exhaustion; bundled floor reactivated"
        );
        return Ok(RetractionOutcome::ReactivatedBundledFloor {
            retracted_id: contribution_id.to_string(),
            reactivated_id: floor_id,
        });
    }

    // No bundled floor in the chain. Chain is dead-ended with no revert
    // target. Fail loud — surfaces operator-authored roots that were
    // never anchored to a bundled default.
    Err(ContributionWriterError::RetractionRefused {
        contribution_id: contribution_id.to_string(),
        reason: "all ancestors retracted and no bundled floor found in chain".to_string(),
    })
}

/// Shim read for retract: loads a minimal projection (status, source,
/// supersedes_id) and returns `ContributionNotFound` instead of None
/// so the caller can short-circuit cleanly via `?`.
fn load_contribution_for_retract(
    conn: &rusqlite::Connection,
    contribution_id: &str,
) -> std::result::Result<RetractRow, ContributionWriterError> {
    let row: Option<RetractRow> = conn
        .query_row(
            "SELECT contribution_id, status, source, supersedes_id
             FROM pyramid_config_contributions
             WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |r| {
                Ok(RetractRow {
                    contribution_id: r.get(0)?,
                    status: r.get(1)?,
                    source: r.get(2)?,
                    supersedes_id: r.get(3)?,
                })
            },
        )
        .optional()?;
    row.ok_or_else(|| ContributionWriterError::ContributionNotFound {
        contribution_id: contribution_id.to_string(),
    })
}

#[derive(Debug, Clone)]
struct RetractRow {
    contribution_id: String,
    status: String,
    source: String,
    supersedes_id: Option<String>,
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
    // Phase 0a-1 commit 5 / §2.16.1: BEGIN IMMEDIATE around
    // accept-promote so proposal promotion serializes on write intent
    // against concurrent supersessions.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

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

    // Phase 0a-1 commit 5: mark prior superseded BEFORE promoting the
    // proposal so the `uq_config_contrib_active` unique index never
    // sees two active rows for the same (schema_type, slug) at once.
    if let Some(ref prior) = prior_id {
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded', superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![contribution_id, prior],
        )?;
    }

    // Promote the proposal to active.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'active',
             accepted_at = datetime('now'),
             supersedes_id = ?1
         WHERE contribution_id = ?2",
        rusqlite::params![prior_id, contribution_id],
    )?;

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
        "fleet_delivery_policy" => {
            // Async fleet dispatch operational policy: ACK/callback timeouts,
            // sweep cadences, retention windows, admission caps, peer
            // staleness. Node-scoped (`slug_opt` ignored — matches
            // `dispatch_policy`'s slug-ignoring pattern). The operational
            // table stores the YAML verbatim so operators see the
            // source-of-truth text round-trip through contribution sync;
            // parsing happens at read time via
            // `FleetDeliveryPolicy::from_yaml`, which applies
            // `deny_unknown_fields` to catch operator typos. The runtime
            // `Arc<RwLock<FleetDeliveryPolicy>>` inside
            // `FleetDispatchContext` is refreshed via a ConfigSynced event
            // listener wired in `main.rs` (Init Ordering step 7).
            crate::pyramid::fleet_delivery_policy::upsert_fleet_delivery_policy_yaml(
                conn,
                &contribution.yaml_content,
                Some(&contribution.contribution_id),
            )?;
        }
        "market_delivery_policy" => {
            // Compute market dispatch operational policy per architecture
            // §VIII.6 DD-E / DD-Q. Shape-parallel to `fleet_delivery_policy`
            // but with market-specific economic-gate fees
            // (match_search_fee, offer_creation_fee, queue_push_fee,
            // queue_mirror_debounce_ms) absorbed from what were previously
            // standalone economic_parameter contributions. Same slug-
            // ignoring, raw-YAML-stored, parse-at-read pattern as the fleet
            // sibling. The Phase 2 WS1+ `MarketDispatchContext` will hold
            // the runtime `Arc<RwLock<MarketDeliveryPolicy>>` and refresh
            // on a ConfigSynced event listener that will be wired into
            // `main.rs` when that context is constructed.
            crate::pyramid::market_delivery_policy::upsert_market_delivery_policy_yaml(
                conn,
                &contribution.yaml_content,
                Some(&contribution.contribution_id),
            )?;
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
                write_contribution_envelope(
                    conn,
                    ContributionEnvelopeInput {
                        contribution_id: wr_id.clone(),
                        slug: Some(slug.clone()),
                        schema_type: "watch_root".to_string(),
                        body: yaml_str,
                        wire_native_metadata_json: Some(metadata_json),
                        supersedes_id: None,
                        triggering_note: Some(
                            "Split from dadbear_policy contribution".to_string(),
                        ),
                        status: "active".to_string(),
                        source: "migration".to_string(),
                        wire_contribution_id: None,
                        created_by: Some("dadbear_split_bootstrap".to_string()),
                        accepted_at: AcceptedAt::Now,
                        needs_migration: None,
                        write_mode: WriteMode::default(),
                    },
                    TransactionMode::OwnTransaction,
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
        write_contribution_envelope(
            conn,
            ContributionEnvelopeInput {
                contribution_id: norms_id.clone(),
                slug: Some(slug.clone()),
                schema_type: "dadbear_norms".to_string(),
                body: norms_yaml_str,
                wire_native_metadata_json: Some(metadata_json),
                supersedes_id: None,
                triggering_note: Some(
                    "Split from dadbear_policy contribution".to_string(),
                ),
                status: "active".to_string(),
                source: "migration".to_string(),
                wire_contribution_id: None,
                created_by: Some("dadbear_split_bootstrap".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
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
    write_contribution_envelope(
        conn,
        ContributionEnvelopeInput {
            contribution_id: marker_id,
            slug: None,
            schema_type: "_migration_marker".to_string(),
            body: String::new(),
            wire_native_metadata_json: None,
            supersedes_id: None,
            triggering_note: None,
            status: "active".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: Some("dadbear_split_bootstrap".to_string()),
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::default(),
        },
        TransactionMode::OwnTransaction,
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
            write_contribution_envelope(
                conn,
                ContributionEnvelopeInput {
                    contribution_id: new_norms_id.clone(),
                    slug: Some(slug.to_string()),
                    schema_type: "dadbear_norms".to_string(),
                    body: norms_yaml_str,
                    wire_native_metadata_json: Some(metadata_json),
                    supersedes_id: Some(norms_row.contribution_id.clone()),
                    triggering_note: Some(
                        "Merged auto_update_policy fields into dadbear_norms".to_string(),
                    ),
                    status: "active".to_string(),
                    source: "migration".to_string(),
                    wire_contribution_id: None,
                    created_by: Some("auto_update_norms_merge".to_string()),
                    accepted_at: AcceptedAt::Now,
                    needs_migration: None,
                    write_mode: WriteMode::default(),
                },
                TransactionMode::OwnTransaction,
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
            write_contribution_envelope(
                conn,
                ContributionEnvelopeInput {
                    contribution_id: norms_id,
                    slug: Some(slug.to_string()),
                    schema_type: "dadbear_norms".to_string(),
                    body: norms_yaml_str,
                    wire_native_metadata_json: Some(metadata_json),
                    supersedes_id: None,
                    triggering_note: Some(
                        "Created from auto_update_policy migration".to_string(),
                    ),
                    status: "active".to_string(),
                    source: "migration".to_string(),
                    wire_contribution_id: None,
                    created_by: Some("auto_update_norms_merge".to_string()),
                    accepted_at: AcceptedAt::Now,
                    needs_migration: None,
                    write_mode: WriteMode::default(),
                },
                TransactionMode::OwnTransaction,
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
    write_contribution_envelope(
        conn,
        ContributionEnvelopeInput {
            contribution_id: marker_id,
            slug: None,
            schema_type: "_migration_marker".to_string(),
            body: String::new(),
            wire_native_metadata_json: None,
            supersedes_id: None,
            triggering_note: None,
            status: "active".to_string(),
            source: "migration".to_string(),
            wire_contribution_id: None,
            created_by: Some("auto_update_norms_merge".to_string()),
            accepted_at: AcceptedAt::Now,
            needs_migration: None,
            write_mode: WriteMode::default(),
        },
        TransactionMode::OwnTransaction,
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

        let mut conn = mem_conn();
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

        // Phase 0a-1 commit 5: a bundled schema_definition for
        // evidence_policy already exists; land the v2 via supersede
        // so `uq_config_contrib_active` is respected.
        let prior_schema_def: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_definition'
                   AND slug = 'evidence_policy'
                   AND status = 'active'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let id = supersede_config_contribution(
            &mut conn,
            &prior_schema_def,
            "{\"type\":\"object\"}",
            "new v2 schema",
            "local",
            Some("user"),
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

    // ── Async fleet dispatch: fleet_delivery_policy dispatcher branch ──
    //
    // Exercises the match arm added for the async fleet dispatch
    // feature. The arm routes `schema_type = "fleet_delivery_policy"`
    // contributions into the dedicated singleton operational table via
    // `upsert_fleet_delivery_policy_yaml`, mirroring the `dispatch_policy`
    // raw-YAML storage pattern exactly.

    const FLEET_DELIVERY_POLICY_SEED_YAML: &str =
        include_str!("../../../docs/seeds/fleet_delivery_policy.yaml");

    #[test]
    fn test_sync_fleet_delivery_policy_writes_operational_table() {
        use crate::pyramid::fleet_delivery_policy::read_fleet_delivery_policy;

        let conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("fleet_delivery_policy", None);
        let id = create_config_contribution_with_metadata(
            &conn,
            "fleet_delivery_policy",
            None,
            FLEET_DELIVERY_POLICY_SEED_YAML,
            Some("seed fleet delivery policy"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();

        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        // The operational read helper parses the stored YAML via
        // `FleetDeliveryPolicy::from_yaml` and returns the populated
        // struct. Field-level assertions confirm the seed values landed.
        let policy = read_fleet_delivery_policy(&conn)
            .unwrap()
            .expect("policy row must exist after sync");

        assert_eq!(policy.dispatch_ack_timeout_secs, 10);
        assert_eq!(policy.outbox_sweep_interval_secs, 15);
        assert_eq!(policy.max_inflight_jobs, 32);
        assert_eq!(policy.peer_staleness_secs, 120);
    }

    #[test]
    fn test_sync_fleet_delivery_policy_overwrites_on_resync() {
        use crate::pyramid::fleet_delivery_policy::read_fleet_delivery_policy;

        let mut conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("fleet_delivery_policy", None);

        // First contribution: seed values.
        let id1 = create_config_contribution_with_metadata(
            &conn,
            "fleet_delivery_policy",
            None,
            FLEET_DELIVERY_POLICY_SEED_YAML,
            Some("initial"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();
        let c1 = load_contribution_by_id(&conn, &id1).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &c1).unwrap();

        let initial = read_fleet_delivery_policy(&conn).unwrap().unwrap();
        assert_eq!(initial.dispatch_ack_timeout_secs, 10);
        assert_eq!(initial.max_inflight_jobs, 32);

        // Second contribution: operator tuned a couple of knobs. Same
        // schema_type, no slug (node-scoped). The singleton row (id=1)
        // must be overwritten in place and the stored `contribution_id`
        // must track the new contribution.
        let tuned_yaml = "schema_type: fleet_delivery_policy\n\
                          version: 1\n\
                          dispatch_ack_timeout_secs: 20\n\
                          timeout_grace_secs: 5\n\
                          orphan_sweep_interval_secs: 60\n\
                          orphan_sweep_multiplier: 3\n\
                          callback_post_timeout_secs: 45\n\
                          outbox_sweep_interval_secs: 30\n\
                          worker_heartbeat_interval_secs: 15\n\
                          worker_heartbeat_tolerance_secs: 45\n\
                          backoff_base_secs: 2\n\
                          backoff_cap_secs: 128\n\
                          max_delivery_attempts: 40\n\
                          ready_retention_secs: 3600\n\
                          delivered_retention_secs: 7200\n\
                          failed_retention_secs: 1209600\n\
                          max_inflight_jobs: 64\n\
                          admission_retry_after_secs: 60\n\
                          peer_staleness_secs: 240\n";
        // Phase 0a-1 commit 5: `uq_config_contrib_active` forbids two
        // active rows for (fleet_delivery_policy, NULL slug) at once.
        // The on-disk supersede path is the supported way to land a
        // new active; route the tuned YAML through it.
        let id2 = supersede_config_contribution(
            &mut conn,
            &id1,
            tuned_yaml,
            "tune for slow rules",
            "local",
            Some("user"),
        )
        .unwrap();
        let c2 = load_contribution_by_id(&conn, &id2).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &c2).unwrap();

        let tuned = read_fleet_delivery_policy(&conn).unwrap().unwrap();
        assert_eq!(tuned.dispatch_ack_timeout_secs, 20);
        assert_eq!(tuned.max_inflight_jobs, 64);
        assert_eq!(tuned.peer_staleness_secs, 240);

        // Verify the stored contribution_id was overwritten to the new
        // contribution. Confirms the singleton upsert — not a second row
        // — was exercised.
        let stored_cid: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_fleet_delivery_policy WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_cid, id2);
    }

    // NOTE: The spec's suggested "malformed YAML rejected at sync time"
    // test is deliberately omitted. The match arm mirrors
    // `dispatch_policy` exactly — raw YAML goes into the operational
    // table verbatim, parsing happens at read time via
    // `FleetDeliveryPolicy::from_yaml`. Malformed YAML is therefore
    // accepted at sync time (matching `dispatch_policy` behavior) and
    // surfaces as a parse error at read time; the fleet_delivery_policy
    // module's own test suite (`from_yaml_rejects_malformed_yaml`,
    // `from_yaml_rejects_unknown_fields`) already covers that read-time
    // path. The directive explicitly pinned this to dispatch_policy's
    // shape: "If it just stores raw YAML, fleet_delivery_policy does
    // the same."

    // ── market_delivery_policy routing (shape-parallel to fleet) ──
    //
    // Same raw-YAML storage pattern; arm routes `schema_type =
    // "market_delivery_policy"` contributions into
    // `pyramid_market_delivery_policy` singleton via
    // `upsert_market_delivery_policy_yaml`. Per architecture §VIII.6
    // DD-E / DD-Q.

    const MARKET_DELIVERY_POLICY_SEED_YAML: &str =
        include_str!("../../../docs/seeds/market_delivery_policy.yaml");

    #[test]
    fn test_sync_market_delivery_policy_writes_operational_table() {
        use crate::pyramid::market_delivery_policy::read_market_delivery_policy;

        let conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("market_delivery_policy", None);
        let id = create_config_contribution_with_metadata(
            &conn,
            "market_delivery_policy",
            None,
            MARKET_DELIVERY_POLICY_SEED_YAML,
            Some("seed market delivery policy"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();

        let contribution = load_contribution_by_id(&conn, &id).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &contribution).unwrap();

        let policy = read_market_delivery_policy(&conn)
            .unwrap()
            .expect("policy row must exist after sync");

        // Spot-check the four economic-gate fees absorbed from standalone
        // economic_parameters per DD-E, plus a few cross-section knobs.
        assert_eq!(policy.callback_post_timeout_secs, 30);
        assert_eq!(policy.max_inflight_jobs, 32);
        assert_eq!(policy.match_search_fee, 1);
        assert_eq!(policy.offer_creation_fee, 1);
        assert_eq!(policy.queue_push_fee, 1);
        assert_eq!(policy.queue_mirror_debounce_ms, 500);
    }

    #[test]
    fn test_sync_market_delivery_policy_overwrites_on_resync() {
        use crate::pyramid::market_delivery_policy::read_market_delivery_policy;

        let mut conn = mem_conn();
        let bus = mem_bus();
        let metadata = default_wire_native_metadata("market_delivery_policy", None);

        // First contribution: seed values.
        let id1 = create_config_contribution_with_metadata(
            &conn,
            "market_delivery_policy",
            None,
            MARKET_DELIVERY_POLICY_SEED_YAML,
            Some("initial"),
            "local",
            Some("user"),
            "active",
            &metadata,
        )
        .unwrap();
        let c1 = load_contribution_by_id(&conn, &id1).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &c1).unwrap();

        let initial = read_market_delivery_policy(&conn).unwrap().unwrap();
        assert_eq!(initial.max_inflight_jobs, 32);
        assert_eq!(initial.match_search_fee, 1);

        // Operator tunes the economic-gate fees and the inflight cap.
        // Singleton row (id=1) must be overwritten; contribution_id
        // must track the new contribution.
        let tuned_yaml = "schema_type: market_delivery_policy\n\
                          version: 1\n\
                          callback_post_timeout_secs: 45\n\
                          outbox_sweep_interval_secs: 30\n\
                          worker_heartbeat_interval_secs: 15\n\
                          worker_heartbeat_tolerance_secs: 45\n\
                          backoff_base_secs: 2\n\
                          backoff_cap_secs: 128\n\
                          max_delivery_attempts: 40\n\
                          ready_retention_secs: 3600\n\
                          delivered_retention_secs: 7200\n\
                          failed_retention_secs: 1209600\n\
                          max_inflight_jobs: 64\n\
                          admission_retry_after_secs: 60\n\
                          match_search_fee: 5\n\
                          offer_creation_fee: 3\n\
                          queue_push_fee: 2\n\
                          queue_mirror_debounce_ms: 1000\n\
                          lease_grace_secs: 10\n\
                          max_concurrent_deliveries: 8\n\
                          max_error_message_chars: 2048\n";
        // Phase 0a-1 commit 5: `uq_config_contrib_active` forbids two
        // active rows for (market_delivery_policy, NULL slug) at once.
        // Route the tuned YAML through supersede_config_contribution.
        let id2 = supersede_config_contribution(
            &mut conn,
            &id1,
            tuned_yaml,
            "tune economic gates",
            "local",
            Some("user"),
        )
        .unwrap();
        let c2 = load_contribution_by_id(&conn, &id2).unwrap().unwrap();
        sync_config_to_operational(&conn, &bus, &c2).unwrap();

        let tuned = read_market_delivery_policy(&conn).unwrap().unwrap();
        assert_eq!(tuned.max_inflight_jobs, 64);
        assert_eq!(tuned.match_search_fee, 5);
        assert_eq!(tuned.offer_creation_fee, 3);
        assert_eq!(tuned.queue_push_fee, 2);
        assert_eq!(tuned.queue_mirror_debounce_ms, 1000);

        // Confirm singleton (not a second row) — stored contribution_id
        // must track id2.
        let stored_cid: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_market_delivery_policy WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_cid, id2);
    }

    // ── Phase 0a-1 commit 5 tests ──────────────────────────────────

    /// Helper: seed a minimal `schema_annotation` contribution for a
    /// target schema_type. Writes directly via the envelope (the test
    /// is itself exercising the writer).
    fn seed_annotation(conn: &Connection, target_schema_type: &str, parameters_yaml: &str) {
        let yaml = format!(
            "applies_to: {target_schema_type}\nparameters:\n{parameters_yaml}"
        );
        write_contribution_envelope(
            conn,
            ContributionEnvelopeInput {
                contribution_id: uuid::Uuid::new_v4().to_string(),
                slug: Some(target_schema_type.to_string()),
                schema_type: "schema_annotation".to_string(),
                body: yaml,
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("test annotation".to_string()),
                status: "active".to_string(),
                source: "bundled".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        )
        .expect("seed annotation");
    }

    #[test]
    fn test_unique_index_rejects_duplicate_active_inserts() {
        let mut conn = mem_conn();
        // Insert two active rows with the same (schema_type, slug).
        let id1 = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: uuid::Uuid::new_v4().to_string(),
                slug: Some("slug-x".to_string()),
                schema_type: "walker_test_schema".to_string(),
                body: "k: v\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("first".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        )
        .expect("first insert should succeed");
        assert!(!id1.is_empty());

        let second = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: uuid::Uuid::new_v4().to_string(),
                slug: Some("slug-x".to_string()),
                schema_type: "walker_test_schema".to_string(),
                body: "k: v2\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("second".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        );
        assert!(
            matches!(
                second,
                Err(ContributionWriterError::SupersessionConflict { .. })
            ),
            "expected SupersessionConflict, got {:?}",
            second
        );
        // Silence unused-mut warning.
        let _ = &mut conn;
    }

    #[test]
    fn test_pre_index_dedup_keeps_newest_id() {
        // Drop the index so the writer can seed 3 duplicate-active
        // rows for the same (schema_type, slug), mirroring a pre-v3
        // dev DB. The dedup helper under test then re-creates the
        // index after superseding all but the id DESC winner.
        let conn = mem_conn();
        conn.execute("DROP INDEX IF EXISTS uq_config_contrib_active", [])
            .unwrap();
        for i in 0..3 {
            write_contribution_envelope(
                &conn,
                ContributionEnvelopeInput {
                    contribution_id: format!("row-{i}"),
                    slug: Some("dup-slug".to_string()),
                    schema_type: "dup_schema".to_string(),
                    body: format!("v: {i}\n"),
                    wire_native_metadata_json: None,
                    supersedes_id: None,
                    triggering_note: Some("seed".to_string()),
                    status: "active".to_string(),
                    source: "local".to_string(),
                    wire_contribution_id: None,
                    created_by: Some("test".to_string()),
                    accepted_at: AcceptedAt::Now,
                    needs_migration: None,
                    write_mode: WriteMode::default(),
                },
                TransactionMode::OwnTransaction,
            )
            .unwrap();
        }
        // Now re-run the migration; it should superseded 2, keep the id DESC winner.
        ensure_config_contrib_active_unique_index(&conn).unwrap();

        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'dup_schema' AND slug = 'dup-slug'
                   AND status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 1, "exactly one active row should remain");

        let superseded_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'dup_schema' AND slug = 'dup-slug'
                   AND status = 'superseded'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(superseded_count, 2);

        let snapshot_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _pre_v3_dedup_snapshot
                 WHERE schema_type = 'dup_schema' AND slug = 'dup-slug'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(snapshot_count, 3);

        // Winner is id DESC — the row whose contribution_id ends in '-2'.
        let winner_cid: String = conn
            .query_row(
                "SELECT contribution_id FROM pyramid_config_contributions
                 WHERE schema_type = 'dup_schema' AND slug = 'dup-slug'
                   AND status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(winner_cid, "row-2");
    }

    #[test]
    fn test_migration_idempotent() {
        let conn = mem_conn();
        // First call already happened inside init_pyramid_db. Running
        // again must be a no-op.
        ensure_config_contrib_active_unique_index(&conn).unwrap();
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'uq_config_contrib_active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }

    #[test]
    fn test_bundled_skip_on_fail_with_malformed_body() {
        let conn = mem_conn();
        // Seed an annotation requiring `k` to be a tagged_union with
        // variants {per_build, probe_based}. A body with `k: "nope"`
        // will fail validation.
        seed_annotation(
            &conn,
            "walker_validated_schema",
            "  k:\n    shape: tagged_union\n    variants:\n      per_build: {}\n      probe_based: {}\n    accepts_string_shorthand:\n      - pattern: \"^(per_build|probe_based)$\"\n",
        );

        let result = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "bundled-malformed-1".to_string(),
                slug: Some("some-slug".to_string()),
                schema_type: "walker_validated_schema".to_string(),
                body: "k: nope\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("malformed bundled".to_string()),
                status: "active".to_string(),
                source: "bundled".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::BundledBootSkipOnFail,
            },
            TransactionMode::OwnTransaction,
        );
        assert!(
            matches!(
                result,
                Err(ContributionWriterError::BundledValidationSkipped { .. })
            ),
            "expected BundledValidationSkipped, got {:?}",
            result
        );
        // Confirm the row was NOT inserted.
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE contribution_id = 'bundled-malformed-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 0);
    }

    #[test]
    fn test_strict_mode_rejects_malformed_body() {
        let conn = mem_conn();
        seed_annotation(
            &conn,
            "walker_validated_schema",
            "  k:\n    shape: tagged_union\n    variants:\n      per_build: {}\n      probe_based: {}\n    accepts_string_shorthand:\n      - pattern: \"^(per_build|probe_based)$\"\n",
        );

        let result = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "strict-malformed-1".to_string(),
                slug: Some("other-slug".to_string()),
                schema_type: "walker_validated_schema".to_string(),
                body: "k: nope\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("bad user".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::Strict,
            },
            TransactionMode::OwnTransaction,
        );
        assert!(
            matches!(
                result,
                Err(ContributionWriterError::ValidationFailed { .. })
            ),
            "expected ValidationFailed, got {:?}",
            result
        );
    }

    #[test]
    fn test_normalize_string_shorthand_tagged_union() {
        let conn = mem_conn();
        seed_annotation(
            &conn,
            "walker_validated_schema",
            "  breaker_reset:\n    shape: tagged_union\n    variants:\n      per_build: {}\n      probe_based: {}\n      time_secs: {}\n    accepts_string_shorthand:\n      - pattern: \"^(per_build|probe_based)$\"\n      - pattern: \"^time_secs:(\\\\d+)$\"\n",
        );

        // Accept a `time_secs:300` shorthand. Writer should normalize
        // to `{kind: time_secs, value: 300}`.
        let id = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "norm-1".to_string(),
                slug: Some("norm-slug".to_string()),
                schema_type: "walker_validated_schema".to_string(),
                body: "breaker_reset: \"time_secs:300\"\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("shorthand".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::Strict,
            },
            TransactionMode::OwnTransaction,
        )
        .expect("normalize should succeed");
        assert_eq!(id, "norm-1");

        let stored_body: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions WHERE contribution_id = 'norm-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Normalized YAML should have `kind:` + `value:` keys.
        assert!(
            stored_body.contains("kind: time_secs"),
            "expected normalized kind field in: {stored_body}"
        );
        assert!(
            stored_body.contains("value: 300"),
            "expected normalized value field in: {stored_body}"
        );
    }

    #[test]
    fn test_migration_marker_empty_body_passes_validation() {
        // `_migration_marker` rows have `body: ""` (wanderer note 5).
        // Validator must accept empty bodies even if a (hypothetical)
        // annotation targeted the schema_type.
        //
        // Use a proposed-status row (not active) so the
        // `uq_config_contrib_active` index does not collide with the
        // bootstrap markers already inserted by `init_pyramid_db`.
        // Validation runs regardless of status.
        let conn = mem_conn();
        // Seed an annotation declaring a required scalar field, then
        // verify an empty body STILL passes (the empty-body short-circuit
        // fires before the shape walk).
        seed_annotation(
            &conn,
            "marker_shape_test",
            "  required_field:\n    shape: scalar\n    type: string\n",
        );
        let result = write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "marker-empty-body".to_string(),
                slug: Some("fresh-marker-slug".to_string()),
                schema_type: "marker_shape_test".to_string(),
                body: String::new(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: None,
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test_marker".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        );
        assert!(result.is_ok(), "empty body must pass validation: {:?}", result);
    }

    #[test]
    fn test_passthrough_when_no_annotation_declares_shape() {
        // Commit-4 parity: when no schema_annotation exists for the
        // target schema_type, the body is written unchanged.
        let conn = mem_conn();
        let body = "arbitrary: payload\nnested:\n  - a\n  - b\n".to_string();
        write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "pt-1".to_string(),
                slug: Some("pt-slug".to_string()),
                schema_type: "schema_without_annotation".to_string(),
                body: body.clone(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("passthrough".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("test".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        )
        .unwrap();

        let stored: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions WHERE contribution_id = 'pt-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, body, "body must round-trip unchanged when no annotation applies");
    }

    #[test]
    fn test_bundled_skips_when_user_active_exists() {
        use crate::pyramid::wire_native_metadata::default_wire_native_metadata;
        let conn = mem_conn();

        // Seed a user-authored active row for (some_schema, some-slug).
        write_contribution_envelope(
            &conn,
            ContributionEnvelopeInput {
                contribution_id: "user-active".to_string(),
                slug: Some("some-slug".to_string()),
                schema_type: "walker_bundled_test".to_string(),
                body: "user: yes\n".to_string(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: Some("user refinement".to_string()),
                status: "active".to_string(),
                source: "local".to_string(),
                wire_contribution_id: None,
                created_by: Some("user".to_string()),
                accepted_at: AcceptedAt::Now,
                needs_migration: None,
                write_mode: WriteMode::default(),
            },
            TransactionMode::OwnTransaction,
        )
        .unwrap();

        // Attempt to insert a bundled row for the same (schema, slug)
        // via the insert_bundled_contribution path.
        use crate::pyramid::wire_migration::BundledContributionEntry;
        let entry = BundledContributionEntry {
            contribution_id: "bundled-would-lose".to_string(),
            slug: Some("some-slug".to_string()),
            schema_type: "walker_bundled_test".to_string(),
            yaml_content: "bundled: default\n".to_string(),
            triggering_note: "bundled default".to_string(),
            topics_extra: Vec::new(),
            applies_to: None,
        };
        let metadata = default_wire_native_metadata(&entry.schema_type, entry.slug.as_deref());
        let inserted = crate::pyramid::wire_migration::insert_bundled_contribution_for_test(
            &conn, &entry, &metadata,
        )
        .expect("pre-check should succeed with Ok(false)");
        assert!(!inserted, "bundled row must be skipped when user-active exists");
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions WHERE contribution_id = 'bundled-would-lose'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 0);
    }

    // ── Phase 0a-2 WS4: retract_config_contribution tests ──────────────

    /// Seed a minimal row via raw SQL (bypasses the envelope writer; tests
    /// are allow-listed by scripts/check-insert-sites.sh). Caller supplies
    /// the full chain topology.
    fn seed_retract_row(
        conn: &Connection,
        contribution_id: &str,
        status: &str,
        source: &str,
        supersedes_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content, wire_native_metadata_json,
                wire_publication_state_json, supersedes_id, superseded_by_id,
                triggering_note, status, source, wire_contribution_id,
                created_at, accepted_at, created_by
             ) VALUES (?1, 'retract-test', 'dadbear_policy', '', '{}', '{}',
                      ?2, NULL, 'seed', ?3, ?4, NULL,
                      datetime('now'), datetime('now'), NULL)",
            rusqlite::params![contribution_id, supersedes_id, status, source],
        )
        .unwrap();
    }

    fn row_status(conn: &Connection, contribution_id: &str) -> String {
        conn.query_row(
            "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    }

    #[test]
    fn retract_reactivates_immediate_parent() {
        let mut conn = mem_conn();
        seed_retract_row(&conn, "floor", "superseded", "bundled", None);
        seed_retract_row(&conn, "active", "active", "operator_authored", Some("floor"));

        let outcome = retract_config_contribution(&mut conn, "active", "operator retract").unwrap();
        match outcome {
            RetractionOutcome::ReactivatedAncestor {
                retracted_id,
                reactivated_id,
                walked_hops,
            } => {
                assert_eq!(retracted_id, "active");
                assert_eq!(reactivated_id, "floor");
                assert_eq!(walked_hops, 1);
            }
            other => panic!("expected ReactivatedAncestor, got {other:?}"),
        }
        assert_eq!(row_status(&conn, "active"), "retracted");
        assert_eq!(row_status(&conn, "floor"), "active");
    }

    #[test]
    fn retract_walks_past_retracted_ancestors() {
        let mut conn = mem_conn();
        // chain: floor (bundled, superseded) <- mid (retracted) <- top (active)
        seed_retract_row(&conn, "floor", "superseded", "bundled", None);
        seed_retract_row(&conn, "mid", "retracted", "operator_authored", Some("floor"));
        seed_retract_row(&conn, "top", "active", "operator_authored", Some("mid"));

        let outcome = retract_config_contribution(&mut conn, "top", "walk deep").unwrap();
        match outcome {
            RetractionOutcome::ReactivatedAncestor {
                reactivated_id,
                walked_hops,
                ..
            } => {
                assert_eq!(reactivated_id, "floor");
                assert_eq!(walked_hops, 2, "should walk past retracted mid to find floor");
            }
            other => panic!("expected ReactivatedAncestor, got {other:?}"),
        }
        assert_eq!(row_status(&conn, "top"), "retracted");
        assert_eq!(row_status(&conn, "mid"), "retracted"); // untouched
        assert_eq!(row_status(&conn, "floor"), "active");
    }

    #[test]
    fn retract_reactivates_bundled_floor_when_all_ancestors_retracted() {
        let mut conn = mem_conn();
        // chain: floor (bundled, retracted) <- mid (retracted) <- top (active)
        seed_retract_row(&conn, "floor", "retracted", "bundled", None);
        seed_retract_row(&conn, "mid", "retracted", "operator_authored", Some("floor"));
        seed_retract_row(&conn, "top", "active", "operator_authored", Some("mid"));

        let outcome = retract_config_contribution(&mut conn, "top", "full retract cascade").unwrap();
        match outcome {
            RetractionOutcome::ReactivatedBundledFloor {
                retracted_id,
                reactivated_id,
            } => {
                assert_eq!(retracted_id, "top");
                assert_eq!(reactivated_id, "floor");
            }
            other => panic!("expected ReactivatedBundledFloor, got {other:?}"),
        }
        assert_eq!(row_status(&conn, "top"), "retracted");
        assert_eq!(row_status(&conn, "mid"), "retracted");
        assert_eq!(row_status(&conn, "floor"), "active");
    }

    #[test]
    fn retract_refused_on_bundled_floor() {
        let mut conn = mem_conn();
        seed_retract_row(&conn, "floor", "active", "bundled", None);

        let err = retract_config_contribution(&mut conn, "floor", "try retract floor").unwrap_err();
        assert!(
            matches!(err, ContributionWriterError::RetractionRefused { .. }),
            "got {err:?}"
        );
        // Floor still active — transaction rolled back on refusal.
        assert_eq!(row_status(&conn, "floor"), "active");
    }

    #[test]
    fn retract_detects_cycle() {
        let mut conn = mem_conn();
        // FK constraint prevents forward-referencing a row that doesn't
        // exist yet, so seed both with supersedes_id=NULL, then patch
        // them into a cycle via UPDATE.
        seed_retract_row(&conn, "a", "active", "operator_authored", None);
        seed_retract_row(&conn, "b", "retracted", "operator_authored", None);
        conn.execute(
            "UPDATE pyramid_config_contributions SET supersedes_id = 'b' WHERE contribution_id = 'a'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE pyramid_config_contributions SET supersedes_id = 'a' WHERE contribution_id = 'b'",
            [],
        )
        .unwrap();

        let err = retract_config_contribution(&mut conn, "a", "cycle probe").unwrap_err();
        assert!(
            matches!(
                err,
                ContributionWriterError::RetractionChainCorrupt { ref reason, .. }
                    if reason.contains("cycle")
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn retract_detects_depth_exhaustion() {
        let mut conn = mem_conn();
        // FK-forward-reference safe: insert root first (supersedes_id=NULL),
        // then each child whose parent already exists. Chain is
        // retracted_19 (root, no parent) <- retracted_18 <- ... <- retracted_0 <- top (active).
        // No bundled floor; every ancestor retracted; walk hits depth ceiling.
        seed_retract_row(&conn, "retracted_19", "retracted", "operator_authored", None);
        for i in (0..19u32).rev() {
            let id = format!("retracted_{i}");
            let parent = format!("retracted_{}", i + 1);
            seed_retract_row(&conn, &id, "retracted", "operator_authored", Some(&parent));
        }
        seed_retract_row(
            &conn,
            "top",
            "active",
            "operator_authored",
            Some("retracted_0"),
        );

        let err = retract_config_contribution(&mut conn, "top", "depth probe").unwrap_err();
        assert!(
            matches!(
                err,
                ContributionWriterError::RetractionChainCorrupt { ref reason, .. }
                    if reason.contains("depth ceiling")
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn retract_refused_when_chain_dead_ends_without_bundled_floor() {
        let mut conn = mem_conn();
        // Every ancestor retracted AND root is operator-authored, not bundled.
        seed_retract_row(&conn, "root", "retracted", "operator_authored", None);
        seed_retract_row(
            &conn,
            "top",
            "active",
            "operator_authored",
            Some("root"),
        );

        let err = retract_config_contribution(&mut conn, "top", "dead end").unwrap_err();
        assert!(
            matches!(err, ContributionWriterError::RetractionRefused { .. }),
            "got {err:?}"
        );
        // Both rows stay put — tx rolled back.
        assert_eq!(row_status(&conn, "top"), "active");
        assert_eq!(row_status(&conn, "root"), "retracted");
    }

    #[test]
    fn retract_nonexistent_returns_not_found() {
        let mut conn = mem_conn();
        let err =
            retract_config_contribution(&mut conn, "does-not-exist", "probe").unwrap_err();
        assert!(
            matches!(err, ContributionWriterError::ContributionNotFound { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn retract_empty_triggering_note_allowed() {
        // §5.4.4 does not require a triggering_note to be non-empty for
        // retraction (unlike supersede, where intent is explicit). A
        // chronicle-event trail carries the retraction context.
        let mut conn = mem_conn();
        seed_retract_row(&conn, "floor", "superseded", "bundled", None);
        seed_retract_row(&conn, "top", "active", "operator_authored", Some("floor"));

        let outcome = retract_config_contribution(&mut conn, "top", "").unwrap();
        assert!(matches!(outcome, RetractionOutcome::ReactivatedAncestor { .. }));
    }
}
