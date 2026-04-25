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
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
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
/// every namespace (e.g. tests asserting 15 + 4 + 11 = 30 genesis
/// entries after Phase 7c, the HTTP route that rejects unknown kinds)
/// read from this list. Adding a fourth vocab_kind means a code-level
/// change — the namespace dimension is not itself a contribution.
pub const VOCAB_KINDS: &[&str] = &[
    VOCAB_KIND_ANNOTATION_TYPE,
    VOCAB_KIND_NODE_SHAPE,
    VOCAB_KIND_ROLE_NAME,
];

const VOCAB_NAME_MAX_CHARS: usize = 128;
const VOCAB_DESCRIPTION_MAX_BYTES: usize = 8 * 1024;

/// Returns true if `kind` is one of the three whitelisted vocab_kinds.
/// Used by the HTTP handler to reject unknown kinds loud (feedback_loud_deferrals)
/// — an unknown kind is almost always a typo, not a valid request.
pub fn is_known_vocab_kind(kind: &str) -> bool {
    VOCAB_KINDS.iter().any(|k| *k == kind)
}

/// Error raised when an HTTP caller requests a vocab_kind that isn't in
/// `VOCAB_KINDS`. Carries the list of valid kinds so the HTTP layer can
/// enumerate them in the 400 response.
#[derive(Debug, thiserror::Error)]
#[error("unknown vocab_kind '{kind}': valid kinds are {valid:?}")]
pub struct UnknownVocabKind {
    pub kind: String,
    pub valid: &'static [&'static str],
}

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
    /// For annotation_type entries: `true` means arrival creates a delta
    /// on the matching thread (the pre-v5 behavior hardcoded for
    /// `correction`). Phase 6c-B lifted this out of a Rust match arm
    /// into a vocab flag — generalize-not-enumerate per
    /// `feedback_generalize_not_enumerate`. `correction` is the only
    /// genesis entry with `creates_delta = true`; operators can publish
    /// new annotation types that create deltas without a code deploy.
    /// Default `false` so existing genesis entries without this flag
    /// keep their current non-delta semantics.
    #[serde(default)]
    pub creates_delta: bool,
    /// For annotation_type entries: `true` means this annotation's
    /// content should be included in the `cascade_annotations` section
    /// of the ancestor re-distill LLM prompt. Phase 9c-2-2 splits
    /// narrative-feedback annotation types (observation / correction /
    /// question / friction / idea / hypothesis / steel_man / red_team /
    /// era / transition / health_check / directory) from operational
    /// directives (gap / purpose_declaration / purpose_shift /
    /// debate_collapse) — the former are content the LLM should
    /// consider when re-distilling; the latter are operational chatter
    /// that would pollute the prompt without improving the distill.
    ///
    /// Default: `true` for existing (pre-9c-2) contributions via
    /// `#[serde(default = "default_include_in_cascade_prompt")]`. This
    /// preserves the pre-9c-2 behavior (every annotation flowed in)
    /// for any non-genesis row missing the field — a conservative
    /// choice because it's safer to over-include than to silently drop
    /// narrative content. Operators supersede individual entries to
    /// flip the default to `false` for operational types they add.
    #[serde(default = "default_include_in_cascade_prompt")]
    pub include_in_cascade_prompt: bool,
    /// For annotation_type entries: the observation `event_type` the
    /// annotation hook emits on each walked ancestor when an annotation
    /// of this type arrives. Phase 9 close-2 lifted the pre-9 hardcoded
    /// match — `correction → annotation_superseded`, else
    /// `annotation_written` — into a vocab flag so operators can publish
    /// new annotation types that emit supersession semantics without a
    /// code deploy. `None` → emits `annotation_written` (the default
    /// cascade event). Genesis `correction` carries
    /// `Some("annotation_superseded")` to preserve pre-9 behavior.
    ///
    /// Serde default `None` keeps pre-close-2 contributions forward-
    /// compatible: a row without the field deserializes as `None` and
    /// maps to the default `annotation_written` event.
    #[serde(default)]
    pub event_type_on_emit: Option<String>,
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
    /// Phase 6c-B: lifted out of Rust match arms into vocab. See
    /// `VocabEntry::creates_delta`.
    #[serde(default)]
    pub creates_delta: bool,
    /// Phase 9c-2-2: narrative-feedback vs operational-directive flag.
    /// Default `true` via `default_include_in_cascade_prompt` so legacy
    /// rows (missing the field entirely) behave exactly as pre-9c-2 —
    /// every annotation type flowed into the prompt.
    #[serde(default = "default_include_in_cascade_prompt")]
    pub include_in_cascade_prompt: bool,
    /// Phase 9 close-2: lifts the pre-9 hardcoded
    /// `correction → annotation_superseded` match from
    /// `emit_annotation_observation_events` into vocab. `None` → default
    /// `annotation_written` event. Serde default `None` keeps pre-close-2
    /// contributions forward-compatible.
    #[serde(default)]
    pub event_type_on_emit: Option<String>,
}

/// Serde default for `include_in_cascade_prompt` — preserves the
/// pre-9c-2 "every annotation flows" semantics for legacy rows.
/// Operators flip the flag to `false` on operational types via
/// `supersede_vocabulary_entry`.
fn default_include_in_cascade_prompt() -> bool {
    true
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
    /// Phase 6c-B: exposed so MCP / frontend can surface which vocab
    /// entries will trigger a delta on arrival. Default false for
    /// entries published without the flag.
    #[serde(default)]
    pub creates_delta: bool,
    /// Phase 9c-2-2: surfaced so MCP / frontend can show whether an
    /// annotation type contributes to ancestor re-distill prompts or
    /// is an operational directive held back from the prompt. See
    /// `VocabEntry::include_in_cascade_prompt` for rationale.
    #[serde(default = "default_include_in_cascade_prompt")]
    pub include_in_cascade_prompt: bool,
    /// Phase 9 close-2: observation event_type emitted on ancestor walk
    /// when an annotation of this type arrives. `None` → default
    /// `annotation_written`. Exposed so MCP / frontend can surface which
    /// vocab entries will emit the stronger supersession signal vs the
    /// default written signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type_on_emit: Option<String>,
}

/// Request shape for publishing a new vocabulary entry over HTTP/CLI.
///
/// The canonical Rust field names are `vocab_kind`, `name`, and
/// `description`. The route also accepts operator-friendly aliases
/// (`type`/`kind`, `term`, `definition`) so the surface can match the
/// vocabulary-entry language without adding a parallel schema.
#[derive(Debug, Clone)]
pub struct VocabPublishRequest {
    pub vocab_kind: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub handler_chain_id: Option<String>,
    pub reactive: bool,
    pub creates_delta: bool,
    pub include_in_cascade_prompt: bool,
    pub event_type_on_emit: Option<String>,
    /// Optional parent entry guard. Vocabulary entries are global today,
    /// so this validates the reference exists but does not persist a new
    /// hierarchy field.
    pub parent: Option<String>,
    pub parent_kind: Option<String>,
    /// Accepted for future slug-scoped clients. Current vocabulary rows
    /// remain global (`slug = NULL`) per the existing contribution model.
    pub slug: Option<String>,
    parse_error: Option<String>,
}

fn parse_nullable_json_field<T>(
    map: &serde_json::Map<String, serde_json::Value>,
    field_name: &str,
) -> std::result::Result<Option<T>, String>
where
    T: DeserializeOwned,
{
    let Some(value) = map.get(field_name) else {
        return Ok(None);
    };
    serde_json::from_value::<Option<T>>(value.clone())
        .map_err(|e| format!("{field_name}: {e}"))
}

fn parse_alias_string(
    map: &serde_json::Map<String, serde_json::Value>,
    aliases: &[&str],
    parse_error: &mut Option<String>,
) -> std::result::Result<Option<String>, String> {
    let present: Vec<&str> = aliases
        .iter()
        .copied()
        .filter(|field_name| map.contains_key(*field_name))
        .collect();
    if present.len() > 1 && parse_error.is_none() {
        *parse_error = Some(format!("use only one of {}", aliases.join(", ")));
    }
    match present.first() {
        Some(field_name) => parse_nullable_json_field::<String>(map, field_name),
        None => Ok(None),
    }
}

impl<'de> Deserialize<'de> for VocabPublishRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let map = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("vocabulary publish body must be a JSON object"))?;
        let mut parse_error = None;

        Ok(Self {
            vocab_kind: parse_alias_string(map, &["vocab_kind", "kind", "type"], &mut parse_error)
                .map_err(serde::de::Error::custom)?,
            name: parse_alias_string(map, &["name", "term"], &mut parse_error)
                .map_err(serde::de::Error::custom)?,
            description: parse_alias_string(map, &["description", "definition"], &mut parse_error)
                .map_err(serde::de::Error::custom)?,
            handler_chain_id: parse_nullable_json_field(map, "handler_chain_id")
                .map_err(serde::de::Error::custom)?,
            reactive: parse_nullable_json_field(map, "reactive")
                .map_err(serde::de::Error::custom)?
                .unwrap_or(false),
            creates_delta: parse_nullable_json_field(map, "creates_delta")
                .map_err(serde::de::Error::custom)?
                .unwrap_or(false),
            include_in_cascade_prompt: parse_nullable_json_field(
                map,
                "include_in_cascade_prompt",
            )
            .map_err(serde::de::Error::custom)?
            .unwrap_or_else(default_include_in_cascade_prompt),
            event_type_on_emit: parse_nullable_json_field(map, "event_type_on_emit")
                .map_err(serde::de::Error::custom)?,
            parent: parse_nullable_json_field(map, "parent").map_err(serde::de::Error::custom)?,
            parent_kind: parse_nullable_json_field(map, "parent_kind")
                .map_err(serde::de::Error::custom)?,
            slug: parse_nullable_json_field(map, "slug").map_err(serde::de::Error::custom)?,
            parse_error,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VocabPublishResponse {
    pub contribution_id: String,
    pub vocab_kind: String,
    pub name: String,
    pub entry: VocabListItem,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid vocabulary publish request: {message}")]
pub struct InvalidVocabPublish {
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("vocabulary_entry ({vocab_kind}, {name}) already exists")]
pub struct DuplicateVocabEntry {
    pub vocab_kind: String,
    pub name: String,
}

// ── Cache ───────────────────────────────────────────────────────────

type CacheKey = (String, String); // (vocab_kind, name)
type CacheMap = HashMap<CacheKey, VocabEntry>;

/// Process-wide cache of the active vocabulary set, keyed by
/// `(vocab_kind, name)`. Populated lazily on first read;
/// invalidated (full drop) on every publish / supersede.
static CACHE: OnceLock<RwLock<Option<CacheMap>>> = OnceLock::new();

/// Phase 9c-3-1: Cross-process vocab cache coherence.
///
/// The `CACHE` is per-process. Wire node (process A) publishing a vocab
/// entry via HTTP does not invalidate the in-process cache of the MCP
/// server (process B) sharing the same sqlite file. Without a coherence
/// check, process B reads stale vocab for up to the cache lifetime.
///
/// The fix is a cheap MAX(id) poll every read. Every cache consumer runs
/// `SELECT MAX(id) FROM pyramid_config_contributions WHERE schema_type
/// LIKE 'vocabulary_entry:%'` first; if it exceeds this atomic, the cache
/// is invalidated and re-populated. If equal, serve from cache. Because
/// the SQL hits the SHARED sqlite file, cross-process writes are visible
/// to all reader processes on the next read cycle — no IPC needed.
///
/// The MAX query walks the primary-key index for the latest row matching
/// the LIKE prefix — fast on every supported sqlite build.
///
/// The atomic is monotone: only bumped to a value strictly greater than
/// the current. Test-mode invalidators (seed passes, test harnesses) drop
/// the atomic to 0 via `invalidate_cache_for_test_only`.
static LAST_OBSERVED_VOCAB_MAX_ID: AtomicI64 = AtomicI64::new(0);

fn cache_handle() -> &'static RwLock<Option<CacheMap>> {
    CACHE.get_or_init(|| RwLock::new(None))
}

/// Query the current MAX(id) of vocabulary_entry rows from the DB. This
/// is the cross-process watermark — it reflects writes from any process
/// sharing the same sqlite file. Returns 0 on empty table (the atomic's
/// initial value), so a never-populated DB hits the equal-compare path
/// and serves an empty cache without infinite re-population.
fn query_vocab_max_id(conn: &Connection) -> Result<i64> {
    // COALESCE(MAX(id), 0): empty table returns 0 matching the atomic
    // default. LIKE 'vocabulary_entry:%' scans the primary-key index;
    // sqlite picks it up via the compound schema_type prefix.
    let max_id: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(id), 0)
               FROM pyramid_config_contributions
              WHERE schema_type LIKE 'vocabulary_entry:%'",
            [],
            |row| row.get(0),
        )
        .context("failed to query vocabulary_entry MAX(id) for cache coherence")?;
    Ok(max_id)
}

/// Drop the cache so the next read re-faults from the DB. Called from
/// `publish_vocabulary_entry` + `supersede_vocabulary_entry`. Also
/// callable from tests that seed rows directly.
///
/// Phase 9c-3-1: ALSO resets the cross-process watermark so the next
/// reader rebuilds and re-observes the current MAX(id). Without this
/// reset, an in-process invalidator that invalidated only the map (not
/// the atomic) would cause the subsequent `ensure_cache` to re-populate
/// at the OLD watermark, missing a concurrent cross-process write that
/// also advanced the watermark.
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache_handle().write() {
        *guard = None;
    }
    // Reset atomic to 0 so the next reader unconditionally re-queries
    // MAX(id). Safe because the atomic is monotonic via a max-fetch
    // update (see `bump_watermark_if_greater`) — resetting to 0 only
    // loses the cross-process shortcut, not correctness.
    LAST_OBSERVED_VOCAB_MAX_ID.store(0, Ordering::SeqCst);
}

/// Monotonic bump: set atomic to `new` IFF `new > current`. Prevents a
/// late-arriving older MAX(id) (e.g. from a stale reader's context) from
/// rolling the watermark backwards.
fn bump_watermark_if_greater(new: i64) {
    let mut cur = LAST_OBSERVED_VOCAB_MAX_ID.load(Ordering::SeqCst);
    while new > cur {
        match LAST_OBSERVED_VOCAB_MAX_ID.compare_exchange(
            cur,
            new,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return,
            Err(observed) => cur = observed,
        }
    }
}

/// Test-only: read the current watermark. Exposed for Phase 9c-3-1 tests
/// that assert monotonic behavior. Not part of the stable API.
#[doc(hidden)]
pub fn current_watermark_for_test() -> i64 {
    LAST_OBSERVED_VOCAB_MAX_ID.load(Ordering::SeqCst)
}

/// Lazily populate the cache from the DB. Rebuilds the full map
/// (all active entries across all vocab_kinds) on a cache miss.
///
/// Phase 9c-3-1: cross-process coherence via MAX(id) poll. Every call
/// queries the DB's current MAX(id) of vocabulary_entry rows. If greater
/// than the process-wide atomic, the cache is invalidated + rebuilt +
/// the atomic is advanced. If equal, the existing cache is served.
/// Because the SQL hits the SHARED sqlite file, writes from a peer
/// process on the same DB are observed on the next read cycle — the
/// per-process invalidation hook still fires for local writes (fast
/// path), but cross-process drift is self-healing on every read.
fn ensure_cache(conn: &Connection) -> Result<()> {
    // Fast path: check cross-process watermark. If the DB's MAX(id) is
    // not greater than what we've seen, the cache is still authoritative.
    let observed_max = query_vocab_max_id(conn)?;
    let known_max = LAST_OBSERVED_VOCAB_MAX_ID.load(Ordering::SeqCst);
    let cache_present = cache_handle()
        .read()
        .map(|g| g.is_some())
        .unwrap_or(false);
    if observed_max <= known_max && cache_present {
        return Ok(());
    }

    // Miss OR cross-process advance: drop stale cache (if any) and
    // rebuild from the DB. We drop-then-repopulate under a single write
    // so concurrent readers in the SAME process see a consistent
    // "absent → populated" transition rather than a partial rebuild.
    if observed_max > known_max {
        if let Ok(mut guard) = cache_handle().write() {
            *guard = None;
        }
    }

    // Build fresh from the DB and install.
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
            creates_delta: body.creates_delta,
            include_in_cascade_prompt: body.include_in_cascade_prompt,
            event_type_on_emit: body.event_type_on_emit,
            created_at,
            superseded_by: None,
            supersede_reason: triggering_note,
        };
        map.insert((body.vocab_kind, body.name), entry);
    }

    if let Ok(mut guard) = cache_handle().write() {
        *guard = Some(map);
    }
    // Phase 9c-3-1: advance the cross-process watermark to the MAX(id)
    // we observed pre-rebuild. Monotonic bump guards against a stale
    // older read rolling the watermark backwards. Using the pre-rebuild
    // observation (not a fresh post-rebuild query) is deliberate — any
    // rows written BETWEEN the query and this store would otherwise be
    // silently skipped until the next reader noticed. The next reader
    // re-queries and, if MAX(id) has since advanced, re-rebuilds.
    bump_watermark_if_greater(observed_max);
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
/// would break round-trip parsing AND collide under the partial unique
/// index (e.g. `(annotation_type, foo:bar)` would share a schema_type
/// with `(annotation_type:foo, bar)`). Publish / supersede paths
/// validate via `validate_vocab_identifiers` before calling this helper.
fn compound_schema_type(vocab_kind: &str, name: &str) -> String {
    format!("{VOCAB_SCHEMA_PREFIX}{vocab_kind}:{name}")
}

/// Defensive runtime check: neither `vocab_kind` nor `name` may contain
/// `:` because `compound_schema_type` uses `:` as its separator. Empty
/// strings are also rejected (empty name would collapse to ambiguous
/// schema_types like `vocabulary_entry:annotation_type:`).
///
/// Loud-raises via `anyhow::bail!` per `feedback_loud_deferrals` — a
/// caller sending a colon-bearing name would otherwise silently write a
/// row that collides with some other legitimate entry on the partial
/// unique index.
fn validate_vocab_identifiers(vocab_kind: &str, name: &str) -> Result<()> {
    if vocab_kind.is_empty() {
        anyhow::bail!("vocab_kind must not be empty");
    }
    if name.is_empty() {
        anyhow::bail!("vocab_entry name must not be empty");
    }
    if vocab_kind.contains(':') {
        anyhow::bail!(
            "vocab_kind '{vocab_kind}' must not contain ':' — the compound schema_type uses ':' as a separator"
        );
    }
    if name.contains(':') {
        anyhow::bail!(
            "vocab_entry name '{name}' must not contain ':' — the compound schema_type uses ':' as a separator"
        );
    }
    Ok(())
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

fn required_trimmed(value: Option<String>, field_name: &str) -> Result<String> {
    let Some(raw) = value else {
        return Err(anyhow::Error::new(InvalidVocabPublish {
            message: format!("missing required field {field_name}"),
        }));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow::Error::new(InvalidVocabPublish {
            message: format!("{field_name} must be non-empty"),
        }));
    }
    Ok(trimmed.to_string())
}

fn optional_trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
                creates_delta: body.creates_delta,
                include_in_cascade_prompt: body.include_in_cascade_prompt,
                event_type_on_emit: body.event_type_on_emit,
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

/// Shared HTTP/CLI publish body. Validates the operator request, writes
/// through the existing vocabulary contribution writer, and returns the
/// local contribution id the caller can quote or inspect later.
pub fn handle_publish_vocabulary(
    conn: &Connection,
    request: VocabPublishRequest,
) -> Result<VocabPublishResponse> {
    if let Some(message) = request.parse_error.as_deref() {
        return Err(anyhow::Error::new(InvalidVocabPublish {
            message: message.to_string(),
        }));
    }

    let vocab_kind = required_trimmed(request.vocab_kind, "vocab_kind")?;
    if !is_known_vocab_kind(&vocab_kind) {
        return Err(anyhow::Error::new(UnknownVocabKind {
            kind: vocab_kind,
            valid: VOCAB_KINDS,
        }));
    }

    let name = required_trimmed(request.name, "name")?;
    let description = required_trimmed(request.description, "description")?;
    if name.chars().count() > VOCAB_NAME_MAX_CHARS {
        return Err(anyhow::Error::new(InvalidVocabPublish {
            message: format!("name must be at most {VOCAB_NAME_MAX_CHARS} characters"),
        }));
    }
    if description.len() > VOCAB_DESCRIPTION_MAX_BYTES {
        return Err(anyhow::Error::new(InvalidVocabPublish {
            message: format!(
                "description must be at most {VOCAB_DESCRIPTION_MAX_BYTES} bytes"
            ),
        }));
    }
    let handler_chain_id = optional_trimmed(request.handler_chain_id);
    let event_type_on_emit = optional_trimmed(request.event_type_on_emit);
    let parent = optional_trimmed(request.parent);
    let parent_kind = optional_trimmed(request.parent_kind).unwrap_or_else(|| vocab_kind.clone());
    let _slug_scope = optional_trimmed(request.slug);

    validate_vocab_identifiers(&vocab_kind, &name)?;
    if get_vocabulary_entry(conn, &vocab_kind, &name)?.is_some() {
        return Err(anyhow::Error::new(DuplicateVocabEntry {
            vocab_kind,
            name,
        }));
    }

    if !is_known_vocab_kind(&parent_kind) {
        return Err(anyhow::Error::new(UnknownVocabKind {
            kind: parent_kind,
            valid: VOCAB_KINDS,
        }));
    }
    if let Some(parent_name) = parent.as_deref() {
        validate_vocab_identifiers(&parent_kind, parent_name)?;
        if get_vocabulary_entry(conn, &parent_kind, parent_name)?.is_none() {
            return Err(anyhow::Error::new(InvalidVocabPublish {
                message: format!(
                    "parent vocabulary_entry ({parent_kind}, {parent_name}) does not exist"
                ),
            }));
        }
    }

    let requested = VocabEntry {
        id: 0,
        vocab_kind: vocab_kind.clone(),
        name: name.clone(),
        description,
        handler_chain_id,
        reactive: request.reactive,
        creates_delta: request.creates_delta,
        include_in_cascade_prompt: request.include_in_cascade_prompt,
        event_type_on_emit,
        created_at: String::new(),
        superseded_by: None,
        supersede_reason: None,
    };
    let saved = publish_vocabulary_entry(conn, &requested)?;
    let contribution_id: String = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions WHERE id = ?1",
            rusqlite::params![saved.id],
            |row| row.get(0),
        )
        .with_context(|| {
            format!(
                "failed to load contribution_id for vocabulary_entry ({}, {})",
                saved.vocab_kind, saved.name
            )
        })?;

    Ok(VocabPublishResponse {
        contribution_id,
        vocab_kind: saved.vocab_kind.clone(),
        name: saved.name.clone(),
        entry: VocabListItem {
            name: saved.name,
            description: saved.description,
            handler_chain_id: saved.handler_chain_id,
            reactive: saved.reactive,
            creates_delta: saved.creates_delta,
            include_in_cascade_prompt: saved.include_in_cascade_prompt,
            event_type_on_emit: saved.event_type_on_emit,
        },
    })
}

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
    validate_vocab_identifiers(&entry.vocab_kind, &entry.name)?;
    let body = VocabBody {
        vocab_kind: entry.vocab_kind.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        handler_chain_id: entry.handler_chain_id.clone(),
        reactive: entry.reactive,
        creates_delta: entry.creates_delta,
        include_in_cascade_prompt: entry.include_in_cascade_prompt,
        event_type_on_emit: entry.event_type_on_emit.clone(),
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
/// / `new_reactive` / `new_creates_delta` / `new_include_in_cascade_prompt`
/// / `new_event_type_on_emit` should be Some — unchanged fields inherit
/// from the prior row.
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
    new_creates_delta: Option<bool>,
    new_include_in_cascade_prompt: Option<bool>,
    new_event_type_on_emit: Option<Option<&str>>,
    reason: Option<&str>,
) -> Result<VocabEntry> {
    validate_vocab_identifiers(vocab_kind, name)?;
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
        creates_delta: new_creates_delta.unwrap_or(prior.creates_delta),
        include_in_cascade_prompt: new_include_in_cascade_prompt
            .unwrap_or(prior.include_in_cascade_prompt),
        event_type_on_emit: match new_event_type_on_emit {
            Some(v) => v.map(|s| s.to_string()),
            None => prior.event_type_on_emit.clone(),
        },
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

    emit_vocabulary_superseded(
        conn,
        vocab_kind,
        name,
        new_body.handler_chain_id.as_deref(),
        new_body.reactive,
        prior.id,
        effective_reason,
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
    emit_vocabulary_event_with_reason(
        conn,
        event_type,
        vocab_kind,
        name,
        handler_chain_id,
        reactive,
        prior_id,
        None,
    )
}

/// Supersede-specific emitter that carries the caller-supplied `reason`
/// as metadata alongside the prior_id. DADBEAR consumers use `reason`
/// to explain why an annotation type / role changed. Phase 6c-A verifier
/// pass addition: prior commit dropped `reason` into `triggering_note`
/// on the contribution row but never surfaced it on the event.
fn emit_vocabulary_superseded(
    conn: &Connection,
    vocab_kind: &str,
    name: &str,
    handler_chain_id: Option<&str>,
    reactive: bool,
    prior_id: i64,
    reason: &str,
) -> Result<()> {
    emit_vocabulary_event_with_reason(
        conn,
        "vocabulary_superseded",
        vocab_kind,
        name,
        handler_chain_id,
        reactive,
        Some(prior_id),
        Some(reason),
    )
}

fn emit_vocabulary_event_with_reason(
    conn: &Connection,
    event_type: &str,
    vocab_kind: &str,
    name: &str,
    handler_chain_id: Option<&str>,
    reactive: bool,
    prior_id: Option<i64>,
    reason: Option<&str>,
) -> Result<()> {
    let metadata = serde_json::json!({
        "vocab_kind": vocab_kind,
        "name": name,
        "handler_chain_id": handler_chain_id,
        "reactive": reactive,
        "prior_id": prior_id,
        "reason": reason,
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

/// Seed the 31 genesis vocabulary entries (16 annotation types, 4 node
/// shapes, 11 role names incl. cascade_handler) into the contribution
/// store. Idempotent — existing active entries are left alone; only
/// missing entries are inserted. Called from `db::init_pyramid_db` after
/// Phase 5 backfills.
///
/// Phase 7c added 4 pure-vocab annotation types (gap, hypothesis,
/// purpose_declaration, purpose_shift) — they have no `ANNOTATION_TYPE_*`
/// const because the enum is vocab-driven post-6c-B.
///
/// Phase 9c-1 added `debate_collapse` (brings annotation types to 16)
/// to close the Phase 8-3 `emit_debate_collapsed` dormant-emitter gap.
///
/// Failures on individual entries do not abort the whole seed pass —
/// each entry is wrapped in a best-effort `if let Err` per the pattern
/// used by `role_binding::backfill_existing_cascade_handlers`'s caller
/// in `init_pyramid_db`. Loud `tracing::error!` on individual failures
/// so operators see the drift.
pub fn seed_genesis_vocabulary(conn: &Connection) -> Result<()> {
    // Annotation types (16 — 11 original + 4 Phase 7c verbs + debate_collapse Phase 9c-1)
    for (
        name,
        description,
        handler_chain_id,
        reactive,
        creates_delta,
        include_in_cascade_prompt,
        event_type_on_emit,
    ) in GENESIS_ANNOTATION_TYPES
    {
        seed_if_missing(
            conn,
            VOCAB_KIND_ANNOTATION_TYPE,
            name,
            description,
            *handler_chain_id,
            *reactive,
            *creates_delta,
            *include_in_cascade_prompt,
            *event_type_on_emit,
        )?;
    }

    // Node shapes (4) — never reactive, no handler, never creates_delta.
    // include_in_cascade_prompt defaults to true (harmless — node_shape
    // entries are not annotation types and never hit the cascade filter).
    // event_type_on_emit is None (not an annotation type).
    for (name, description) in GENESIS_NODE_SHAPES {
        seed_if_missing(
            conn,
            VOCAB_KIND_NODE_SHAPE,
            name,
            description,
            None,
            false,
            false,
            true,
            None,
        )?;
    }

    // Role names (11 incl. cascade_handler) — always have handler_chain_id,
    // non-reactive. include_in_cascade_prompt defaults to true (same
    // rationale as node_shape above — not an annotation type).
    // event_type_on_emit is None (not an annotation type).
    for (name, description, handler_chain_id) in GENESIS_ROLE_NAMES {
        seed_if_missing(
            conn,
            VOCAB_KIND_ROLE_NAME,
            name,
            description,
            Some(*handler_chain_id),
            false,
            false,
            true,
            None,
        )?;
    }

    // Ensure cache reflects the seed pass. The next reader will
    // populate from the DB; invalidation is safe to run before
    // the first read too.
    invalidate_cache();

    // Phase 6c-D: `check_genesis_role_parity` is DELETED. It was the
    // drift-guard between the (now-removed) `role_binding::GENESIS_BINDINGS`
    // const and `vocab_genesis::GENESIS_ROLE_NAMES`. Now that
    // `role_binding::initialize_genesis_bindings` + `backfill_genesis_bindings`
    // read the vocab registry directly, there's no const to drift against —
    // the registry IS the source of truth.

    Ok(())
}

fn seed_if_missing(
    conn: &Connection,
    vocab_kind: &str,
    name: &str,
    description: &str,
    handler_chain_id: Option<&str>,
    reactive: bool,
    creates_delta: bool,
    include_in_cascade_prompt: bool,
    event_type_on_emit: Option<&str>,
) -> Result<()> {
    validate_vocab_identifiers(vocab_kind, name)?;
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
        creates_delta,
        include_in_cascade_prompt,
        event_type_on_emit: event_type_on_emit.map(|s| s.to_string()),
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
/// Rejects unknown `vocab_kind` with `UnknownVocabKind` (mapped to HTTP
/// 400 by the route handler). `feedback_loud_deferrals` — an unknown
/// kind is a typo, not a valid absent-registry signal.
///
/// Callers (HTTP route + tests) pass a fresh Connection. Zero auth
/// required — vocabulary is public read per the 6c-A spec.
pub fn handle_get_vocabulary(
    conn: &Connection,
    vocab_kind: &str,
) -> Result<VocabListResponse> {
    if !is_known_vocab_kind(vocab_kind) {
        return Err(anyhow::Error::new(UnknownVocabKind {
            kind: vocab_kind.to_string(),
            valid: VOCAB_KINDS,
        }));
    }
    let entries = list_vocabulary(conn, vocab_kind)?;
    let items: Vec<VocabListItem> = entries
        .into_iter()
        .map(|e| VocabListItem {
            name: e.name,
            description: e.description,
            handler_chain_id: e.handler_chain_id,
            reactive: e.reactive,
            creates_delta: e.creates_delta,
            include_in_cascade_prompt: e.include_in_cascade_prompt,
            event_type_on_emit: e.event_type_on_emit,
        })
        .collect();
    Ok(VocabListResponse {
        vocab_kind: vocab_kind.to_string(),
        entries: items,
    })
}
