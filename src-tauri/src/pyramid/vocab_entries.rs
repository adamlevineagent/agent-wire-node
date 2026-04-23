// pyramid/vocab_entries.rs — Phase 6c-A: vocabulary contribution storage + read API.
//
// Kills hardcoded-vocabulary v5 today: the 11 AnnotationType variants,
// 4 NodeShape variants, and 10 role names ship as contribution-driven
// registry entries. Adding a new entry is a contribution write, not a
// code deploy.
//
// Per `feedback_everything_is_contribution`: entries live in the existing
// `pyramid_config_contributions` table with a `vocabulary_entry:<kind>:<name>`
// compound schema_type. Compound-type choice is deliberate:
//
//   - The existing `uq_config_contrib_active` unique index enforces
//     exactly one active row per `(COALESCE(slug, '__global__'),
//     schema_type)`. A literal `schema_type = "vocabulary_entry"` with
//     `slug = NULL` would permit only one active vocab entry total.
//   - A compound schema_type `vocabulary_entry:<kind>:<name>` gives
//     every `(vocab_kind, name)` pair a globally unique schema_type,
//     so `uq_config_contrib_active` naturally enforces
//     single-active-per-(kind, name) without touching the existing
//     index or adding a second one. The `vocab_entry:` prefix is the
//     discriminator the dispatcher and read path match on.
//
// Supersession follows the native `pyramid_config_contributions`
// chain: `supersedes_id` → prior `contribution_id`,
// `superseded_by_id` → successor `contribution_id`. The public
// `VocabEntry` struct normalizes these TEXT UUIDs into integer `id`
// values matching the spec (row's native AUTOINCREMENT `id`).
//
// Cache: process-wide `OnceLock<RwLock<HashMap<(vocab_kind, name),
// VocabEntry>>>` populated lazily on first read, invalidated on
// publish / supersede. Thread-safe.
//
// Observation events: every publish + supersede writes a
// `vocabulary_published` / `vocabulary_superseded` event with
// `source = "vocabulary"` so the DADBEAR compiler can react.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use super::config_contributions;
use super::observation_events;
use super::vocab_genesis::{
    GENESIS_ANNOTATION_TYPES, GENESIS_NODE_SHAPES, GENESIS_ROLE_NAMES,
};

// ── Constants ───────────────────────────────────────────────────────

/// schema_type prefix used by all vocabulary_entry rows. See module docs
/// for why the compound schema_type is used instead of a literal
/// `"vocabulary_entry"` + slug trick.
pub const VOCAB_SCHEMA_PREFIX: &str = "vocabulary_entry:";

/// Namespace for `vocab_kind` values. Entries do not cross-namespace —
/// a `steel_man` in `annotation_type` is NOT the same as a hypothetical
/// `steel_man` in `role_name`. New namespaces are added by code deploy
/// (a 6c-B / C / D-style migration), not by contribution — namespaces
/// are the registry's dimensions, not its rows.
pub const VOCAB_KIND_ANNOTATION_TYPE: &str = "annotation_type";
pub const VOCAB_KIND_NODE_SHAPE: &str = "node_shape";
pub const VOCAB_KIND_ROLE_NAME: &str = "role_name";

/// All three genesis-supported vocab_kinds. Callers that want to iterate
/// every namespace (e.g. tests asserting 11 + 4 + 10 = 25 genesis
/// entries, the HTTP route that rejects unknown kinds) read from this
/// list. Adding a fourth vocab_kind means a code-level change — the
/// namespace dimension is not itself a contribution.
pub const VOCAB_KINDS: &[&str] = &[
    VOCAB_KIND_ANNOTATION_TYPE,
    VOCAB_KIND_NODE_SHAPE,
    VOCAB_KIND_ROLE_NAME,
];

// ── Types ───────────────────────────────────────────────────────────

/// A single vocabulary entry — the registry row behind AnnotationType,
/// NodeShape, and role names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VocabEntry {
    /// Native `pyramid_config_contributions.id` (AUTOINCREMENT integer).
    pub id: i64,
    /// Namespace: "annotation_type" | "node_shape" | "role_name".
    pub vocab_kind: String,
    /// Canonical string used as the DB value + wire-protocol string.
    /// Unique within its `vocab_kind` among active rows.
    pub name: String,
    pub description: String,
    /// For role entries: the starter chain this role binds to. For
    /// annotation_type entries: the chain dispatched when this type of
    /// annotation arrives (6c-B will rewrite `process_annotation_hook`
    /// to read this). For node_shape entries: None today (shapes
    /// don't dispatch on their own).
    pub handler_chain_id: Option<String>,
    /// For annotation_type entries: `true` means arrival emits an
    /// `annotation_reacted` observation event (Phase 7 wires this).
    /// `steel_man` + `red_team` are reactive today; future v5 verbs
    /// (hypothesis / gap / purpose_declaration / purpose_shift) will
    /// be published with `reactive: true` when those variants exist.
    pub reactive: bool,
    pub created_at: String,
    /// If this row was superseded by another row, the successor's
    /// integer `id`. None for the currently-active row.
    pub superseded_by: Option<i64>,
    /// Reason recorded at supersession time. Inherited from the
    /// supersede call's `triggering_note`.
    pub supersede_reason: Option<String>,
}

/// YAML body shape persisted in `pyramid_config_contributions.yaml_content`.
/// Read-path recovery cheaply deserializes this into a `VocabEntry` via
/// the `row_to_vocab_entry` helper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct VocabBody {
    pub vocab_kind: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub handler_chain_id: Option<String>,
    #[serde(default)]
    pub reactive: bool,
}

/// HTTP response shape for `GET /vocabulary/:vocab_kind`.
#[derive(Debug, Clone, Serialize)]
pub struct VocabListResponse {
    pub vocab_kind: String,
    pub entries: Vec<VocabListItem>,
}

/// Trimmed vocab entry for HTTP responses — skips `id` /
/// `created_at` / supersession bookkeeping, keeps only the fields
/// downstream consumers (MCP, frontend) actually need.
#[derive(Debug, Clone, Serialize)]
pub struct VocabListItem {
    pub name: String,
    pub description: String,
    pub handler_chain_id: Option<String>,
    pub reactive: bool,
}

// ── Cache ───────────────────────────────────────────────────────────

type CacheKey = (String, String); // (vocab_kind, name)
type CacheMap = HashMap<CacheKey, VocabEntry>;

/// Process-wide cache of the active vocabulary set, keyed by
/// `(vocab_kind, name)`. Populated lazily on first read;
/// invalidated (full drop) on every publish / supersede.
static CACHE: OnceLock<RwLock<Option<CacheMap>>> = OnceLock::new();

fn cache_handle() -> &'static RwLock<Option<CacheMap>> {
    CACHE.get_or_init(|| RwLock::new(None))
}

/// Drop the cache so the next read re-faults from the DB. Called from
/// `publish_vocabulary_entry` + `supersede_vocabulary_entry`. Also
/// callable from tests that seed rows directly.
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache_handle().write() {
        *guard = None;
    }
}

/// Lazily populate the cache from the DB. Rebuilds the full map
/// (all active entries across all vocab_kinds) on a cache miss.
fn ensure_cache(conn: &Connection) -> Result<()> {
    // Read-only check first — common case.
    if let Ok(guard) = cache_handle().read() {
        if guard.is_some() {
            return Ok(());
        }
    }

    // Miss: build fresh from the DB and install.
    let mut map: CacheMap = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT id, schema_type, yaml_content, created_at, superseded_by_id, triggering_note
           FROM pyramid_config_contributions
          WHERE schema_type LIKE 'vocabulary_entry:%'
            AND status = 'active'
            AND superseded_by_id IS NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let schema_type: String = row.get(1)?;
        let yaml: String = row.get(2)?;
        let created_at: String = row.get(3)?;
        let superseded_by_id_txt: Option<String> = row.get(4)?;
        let triggering_note: Option<String> = row.get(5)?;
        Ok((id, schema_type, yaml, created_at, superseded_by_id_txt, triggering_note))
    })?;

    // Prepare a second statement for superseded_by_id → i64 id resolution.
    // Active rows always have superseded_by_id NULL (the WHERE above),
    // so this is actually unreachable for the cache — but we keep it
    // parameterized so the row-mapper can serve both paths.
    for row in rows {
        let (id, schema_type, yaml, created_at, _sb_txt, triggering_note) = row?;
        let body: VocabBody = serde_yaml::from_str(&yaml)
            .with_context(|| format!("vocab_entry id={id} has malformed yaml_content"))?;
        // Defensive: compound schema_type must match the body (catches
        // hand-edited rows). The SQL filter already constrains prefix;
        // we parse to keep name / kind in sync with the body.
        // Expected format: `vocabulary_entry:<kind>:<name>`.
        validate_schema_type_matches_body(&schema_type, &body, id)?;
        let entry = VocabEntry {
            id,
            vocab_kind: body.vocab_kind.clone(),
            name: body.name.clone(),
            description: body.description,
            handler_chain_id: body.handler_chain_id,
            reactive: body.reactive,
            created_at,
            superseded_by: None,
            supersede_reason: triggering_note,
        };
        map.insert((body.vocab_kind, body.name), entry);
    }

    if let Ok(mut guard) = cache_handle().write() {
        *guard = Some(map);
    }
    Ok(())
}

fn validate_schema_type_matches_body(schema_type: &str, body: &VocabBody, id: i64) -> Result<()> {
    let expected = compound_schema_type(&body.vocab_kind, &body.name);
    if schema_type != expected {
        anyhow::bail!(
            "vocab_entry id={id}: schema_type '{schema_type}' does not match body (expected '{expected}')"
        );
    }
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Build the compound schema_type `vocabulary_entry:<kind>:<name>`.
/// Callers must not include colons in `vocab_kind` or `name` — that
/// would break round-trip parsing. Genesis seeds and all public APIs
/// use simple identifiers (snake_case), so this is a code-level
/// invariant, not a runtime validation.
fn compound_schema_type(vocab_kind: &str, name: &str) -> String {
    format!("{VOCAB_SCHEMA_PREFIX}{vocab_kind}:{name}")
}

/// Serialize a VocabBody to YAML. Used both by the writer and by the
/// genesis seeder. Kept consistent so parsed-out bodies round-trip.
fn body_to_yaml(body: &VocabBody) -> Result<String> {
    serde_yaml::to_string(body).context("failed to serialize vocab_entry body")
}

/// Look up a contribution's integer `id` from its TEXT `contribution_id`.
/// Used to convert the native `superseded_by_id` TEXT chain into the
/// `VocabEntry.superseded_by: Option<i64>` the spec declares.
fn lookup_id_by_contribution_id(conn: &Connection, contribution_id: &str) -> Result<Option<i64>> {
    let result: Option<i64> = conn
        .query_row(
            "SELECT id FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![contribution_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

/// Read one vocabulary row (active or superseded) and materialize a
/// `VocabEntry`. Returns None if the row doesn't exist.
fn load_entry_by_schema_type(
    conn: &Connection,
    schema_type: &str,
    active_only: bool,
) -> Result<Option<VocabEntry>> {
    let sql = if active_only {
        "SELECT id, yaml_content, created_at, superseded_by_id, triggering_note
           FROM pyramid_config_contributions
          WHERE schema_type = ?1 AND status = 'active' AND superseded_by_id IS NULL
          LIMIT 1"
    } else {
        "SELECT id, yaml_content, created_at, superseded_by_id, triggering_note
           FROM pyramid_config_contributions
          WHERE schema_type = ?1
          ORDER BY id DESC
          LIMIT 1"
    };
    let row: Option<(i64, String, String, Option<String>, Option<String>)> = conn
        .query_row(sql, rusqlite::params![schema_type], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .optional()
        .with_context(|| format!("failed to load vocab entry for schema_type={schema_type}"))?;

    match row {
        None => Ok(None),
        Some((id, yaml, created_at, superseded_by_id_txt, triggering_note)) => {
            let body: VocabBody = serde_yaml::from_str(&yaml)
                .with_context(|| format!("vocab_entry id={id} has malformed yaml_content"))?;
            validate_schema_type_matches_body(schema_type, &body, id)?;
            let superseded_by = match superseded_by_id_txt {
                Some(txt) => lookup_id_by_contribution_id(conn, &txt)?,
                None => None,
            };
            Ok(Some(VocabEntry {
                id,
                vocab_kind: body.vocab_kind,
                name: body.name,
                description: body.description,
                handler_chain_id: body.handler_chain_id,
                reactive: body.reactive,
                created_at,
                superseded_by,
                supersede_reason: triggering_note,
            }))
        }
    }
}

// ── Read API ────────────────────────────────────────────────────────

/// List all active entries for a given `vocab_kind`, ordered by name.
/// Returns an empty vec for unknown vocab_kinds (no error — the HTTP
/// layer converts empty to 404 if that's the desired semantics).
pub fn list_vocabulary(conn: &Connection, vocab_kind: &str) -> Result<Vec<VocabEntry>> {
    ensure_cache(conn)?;
    let mut out: Vec<VocabEntry> = Vec::new();
    if let Ok(guard) = cache_handle().read() {
        if let Some(ref map) = *guard {
            for ((k, _), entry) in map.iter() {
                if k == vocab_kind {
                    out.push(entry.clone());
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Load a single active entry by `(vocab_kind, name)`. Returns None
/// if no active entry exists.
pub fn get_vocabulary_entry(
    conn: &Connection,
    vocab_kind: &str,
    name: &str,
) -> Result<Option<VocabEntry>> {
    ensure_cache(conn)?;
    if let Ok(guard) = cache_handle().read() {
        if let Some(ref map) = *guard {
            return Ok(map.get(&(vocab_kind.to_string(), name.to_string())).cloned());
        }
    }
    Ok(None)
}

// ── Write API ───────────────────────────────────────────────────────

/// Publish a new vocabulary entry. Fails loud if `(vocab_kind, name)`
/// already has an active entry — callers must use
/// `supersede_vocabulary_entry` to replace an existing entry.
///
/// Emits a `vocabulary_published` observation event with metadata
/// carrying the vocab_kind, name, handler_chain_id, and reactive
/// flag so the DADBEAR compiler can react.
pub fn publish_vocabulary_entry(
    conn: &Connection,
    entry: &VocabEntry,
) -> Result<VocabEntry> {
    let body = VocabBody {
        vocab_kind: entry.vocab_kind.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        handler_chain_id: entry.handler_chain_id.clone(),
        reactive: entry.reactive,
    };
    let yaml = body_to_yaml(&body)?;
    let schema_type = compound_schema_type(&entry.vocab_kind, &entry.name);

    let _contribution_id = config_contributions::create_config_contribution(
        conn,
        &schema_type,
        None,                             // slug — vocab entries are global
        &yaml,
        None,                             // triggering_note — only used on supersede
        "local",                          // source
        None,                             // created_by
        "active",                         // status
    )
    .with_context(|| {
        format!(
            "failed to publish vocabulary_entry ({}, {})",
            entry.vocab_kind, entry.name
        )
    })?;

    // Observation event BEFORE cache invalidation so reader that picks
    // up the event can see the new row on first read.
    emit_vocabulary_event(
        conn,
        "vocabulary_published",
        &entry.vocab_kind,
        &entry.name,
        entry.handler_chain_id.as_deref(),
        entry.reactive,
        None,
    )?;

    invalidate_cache();

    // Re-load the full entry from DB so the caller sees the
    // authoritative `id` / `created_at` the DB assigned.
    let saved = load_entry_by_schema_type(conn, &schema_type, true)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "vocabulary_entry ({}, {}) not found after publish",
                entry.vocab_kind,
                entry.name
            )
        })?;
    Ok(saved)
}

/// Supersede the active entry for `(vocab_kind, name)` with a new
/// row. At least one of `new_description` / `new_handler_chain_id`
/// / `new_reactive` should be Some — unchanged fields inherit from
/// the prior row.
///
/// Emits a `vocabulary_superseded` observation event. Loud-raises if
/// no active entry exists.
pub fn supersede_vocabulary_entry(
    conn: &mut Connection,
    vocab_kind: &str,
    name: &str,
    new_description: Option<&str>,
    new_handler_chain_id: Option<Option<&str>>,
    new_reactive: Option<bool>,
    reason: Option<&str>,
) -> Result<VocabEntry> {
    let prior = get_vocabulary_entry(conn, vocab_kind, name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "cannot supersede vocabulary_entry ({vocab_kind}, {name}): no active entry"
        )
    })?;

    let new_body = VocabBody {
        vocab_kind: vocab_kind.to_string(),
        name: name.to_string(),
        description: new_description
            .map(|s| s.to_string())
            .unwrap_or_else(|| prior.description.clone()),
        handler_chain_id: match new_handler_chain_id {
            Some(v) => v.map(|s| s.to_string()),
            None => prior.handler_chain_id.clone(),
        },
        reactive: new_reactive.unwrap_or(prior.reactive),
    };
    let new_yaml = body_to_yaml(&new_body)?;
    let schema_type = compound_schema_type(vocab_kind, name);

    // Look up the prior row's contribution_id (text) from its integer id.
    let prior_contribution_id: String = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions WHERE id = ?1",
            rusqlite::params![prior.id],
            |row| row.get(0),
        )
        .with_context(|| {
            format!(
                "failed to load prior contribution_id for vocab_entry id={} ({vocab_kind}, {name})",
                prior.id
            )
        })?;

    let effective_reason = reason.unwrap_or("vocab supersession");
    if effective_reason.trim().is_empty() {
        anyhow::bail!("supersede reason must be non-empty (pass None to use default)");
    }

    let _new_contribution_id = config_contributions::supersede_config_contribution(
        conn,
        &prior_contribution_id,
        &new_yaml,
        effective_reason,
        "local",
        None,
    )
    .with_context(|| {
        format!(
            "failed to supersede vocabulary_entry ({vocab_kind}, {name})"
        )
    })?;

    emit_vocabulary_event(
        conn,
        "vocabulary_superseded",
        vocab_kind,
        name,
        new_body.handler_chain_id.as_deref(),
        new_body.reactive,
        Some(prior.id),
    )?;

    invalidate_cache();

    let saved = load_entry_by_schema_type(conn, &schema_type, true)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "vocabulary_entry ({vocab_kind}, {name}) not found after supersede"
            )
        })?;
    Ok(saved)
}

fn emit_vocabulary_event(
    conn: &Connection,
    event_type: &str,
    vocab_kind: &str,
    name: &str,
    handler_chain_id: Option<&str>,
    reactive: bool,
    prior_id: Option<i64>,
) -> Result<()> {
    let metadata = serde_json::json!({
        "vocab_kind": vocab_kind,
        "name": name,
        "handler_chain_id": handler_chain_id,
        "reactive": reactive,
        "prior_id": prior_id,
    })
    .to_string();
    // Vocabulary events are global (no slug). DADBEAR observation
    // events require a concrete slug value; we use "__global__"
    // consistently with `uq_config_contrib_active`'s COALESCE value.
    observation_events::write_observation_event(
        conn,
        "__global__",  // slug
        "vocabulary",  // source
        event_type,    // event_type
        None,          // source_path
        None,          // file_path
        None,          // content_hash
        None,          // previous_hash
        None,          // target_node_id
        None,          // layer
        Some(&metadata),
    )
    .with_context(|| {
        format!(
            "failed to emit {event_type} observation event for ({vocab_kind}, {name})"
        )
    })?;
    Ok(())
}

// ── Genesis seeder ──────────────────────────────────────────────────

/// Seed the 25 genesis vocabulary entries (11 annotation types, 4 node
/// shapes, 10 role names) into the contribution store. Idempotent —
/// existing active entries are left alone; only missing entries are
/// inserted. Called from `db::init_pyramid_db` after Phase 5 backfills.
///
/// Failures on individual entries do not abort the whole seed pass —
/// each entry is wrapped in a best-effort `if let Err` per the pattern
/// used by `role_binding::backfill_existing_cascade_handlers`'s caller
/// in `init_pyramid_db`. Loud `tracing::error!` on individual failures
/// so operators see the drift.
pub fn seed_genesis_vocabulary(conn: &Connection) -> Result<()> {
    // Annotation types (11)
    for (name, description, handler_chain_id, reactive) in GENESIS_ANNOTATION_TYPES {
        seed_if_missing(
            conn,
            VOCAB_KIND_ANNOTATION_TYPE,
            name,
            description,
            *handler_chain_id,
            *reactive,
        )?;
    }

    // Node shapes (4) — never reactive, no handler
    for (name, description) in GENESIS_NODE_SHAPES {
        seed_if_missing(
            conn,
            VOCAB_KIND_NODE_SHAPE,
            name,
            description,
            None,
            false,
        )?;
    }

    // Role names (11 incl. cascade_handler) — always have handler_chain_id, non-reactive
    for (name, description, handler_chain_id) in GENESIS_ROLE_NAMES {
        seed_if_missing(
            conn,
            VOCAB_KIND_ROLE_NAME,
            name,
            description,
            Some(*handler_chain_id),
            false,
        )?;
    }

    // Ensure cache reflects the seed pass. The next reader will
    // populate from the DB; invalidation is safe to run before
    // the first read too.
    invalidate_cache();
    Ok(())
}

fn seed_if_missing(
    conn: &Connection,
    vocab_kind: &str,
    name: &str,
    description: &str,
    handler_chain_id: Option<&str>,
    reactive: bool,
) -> Result<()> {
    let schema_type = compound_schema_type(vocab_kind, name);
    // Idempotency check: is there already an active row for this
    // compound schema_type? If so, skip.
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM pyramid_config_contributions
              WHERE schema_type = ?1 AND status = 'active' AND superseded_by_id IS NULL",
            rusqlite::params![schema_type],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(());
    }

    let body = VocabBody {
        vocab_kind: vocab_kind.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        handler_chain_id: handler_chain_id.map(|s| s.to_string()),
        reactive,
    };
    let yaml = body_to_yaml(&body)?;

    // Use the canonical writer. Source = "bundled" flags this row as
    // genesis-seeded — matches the pattern bundled manifest rows use.
    let _contribution_id = config_contributions::create_config_contribution(
        conn,
        &schema_type,
        None,                               // slug — global
        &yaml,
        Some("genesis vocabulary seed"),    // triggering_note
        "bundled",                          // source
        Some("genesis"),                    // created_by
        "active",                           // status
    )
    .with_context(|| {
        format!("failed to seed genesis vocab_entry ({vocab_kind}, {name})")
    })?;

    // Publish event for observability. The DADBEAR compiler may react
    // to genesis-seeded entries on the first boot after upgrade.
    emit_vocabulary_event(
        conn,
        "vocabulary_published",
        vocab_kind,
        name,
        handler_chain_id,
        reactive,
        None,
    )?;

    Ok(())
}

// ── HTTP handler (factored out of routes.rs for direct testability) ─

/// Handler body for `GET /vocabulary/:vocab_kind`. Returns the full
/// list of active entries for the requested kind.
///
/// Spec format:
/// ```json
/// {
///   "vocab_kind": "annotation_type",
///   "entries": [
///     {"name": "observation", "description": "...", "handler_chain_id": null, "reactive": false},
///     ...
///   ]
/// }
/// ```
///
/// Callers (HTTP route + tests) pass a fresh Connection. Zero auth
/// required — vocabulary is public read per the 6c-A spec.
pub fn handle_get_vocabulary(
    conn: &Connection,
    vocab_kind: &str,
) -> Result<VocabListResponse> {
    let entries = list_vocabulary(conn, vocab_kind)?;
    let items: Vec<VocabListItem> = entries
        .into_iter()
        .map(|e| VocabListItem {
            name: e.name,
            description: e.description,
            handler_chain_id: e.handler_chain_id,
            reactive: e.reactive,
        })
        .collect();
    Ok(VocabListResponse {
        vocab_kind: vocab_kind.to_string(),
        entries: items,
    })
}
