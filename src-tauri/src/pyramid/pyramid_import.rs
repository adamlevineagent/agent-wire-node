// pyramid/pyramid_import.rs — Phase 7: Cache warming on pyramid import
//
// Implements the import-side counterpart to Phase 5's `wire_publish.rs`
// (publication path) and Phase 6's `pyramid_step_cache` (the local cache
// table). When a user pulls a pyramid from Wire, the source node's exported
// cache manifest is downloaded and populated into the local
// `pyramid_step_cache` so unchanged nodes cache-hit on the first build
// instead of re-running every LLM call.
//
// The insight (per `docs/specs/cache-warming-and-import.md`): the cache key
// is content-addressable, so a cache entry produced by another node is just
// as valid for this node as long as the source file content matches by
// SHA-256 hash. The import is NOT a build — it's a data transfer plus a
// staleness pass that walks the in-manifest dependency graph.
//
// Three correctness gates carry the spec:
//
//   1. The L0 staleness check: every L0 node's `source_path` is resolved
//      against the local source root, the file is hashed, and a mismatch
//      marks the node stale. Any L0 node that survives this pass has its
//      cache entries inserted as-is into `pyramid_step_cache`.
//
//   2. The upward propagation: BFS over the manifest's `derived_from` lists
//      starting from the stale L0 frontier. Every downstream node touched
//      by a stale L0 ancestor is also stale, no matter how deep the chain.
//      The dependency graph is built from the manifest itself — we never
//      consult the local `pyramid_evidence` table during import, so import
//      cannot be poisoned by stale local state.
//
//   3. The upper-layer cache pass: only nodes NOT in the stale set get
//      their cache entries inserted. Nodes in the stale set are skipped
//      entirely — their cache entries are dropped, the next build will
//      re-run them fresh.
//
// Idempotency: every cache insert uses the existing `db::store_cache` helper
// (which uses `INSERT ... ON CONFLICT ... DO UPDATE` keyed on the unique
// `(slug, cache_key)` constraint). Re-importing the same manifest is a
// no-op for the rows that have already landed. The `pyramid_import_state`
// table provides the cursor so a partially-completed import can resume from
// the last node processed without re-running the L0 hashing.
//
// Privacy: this module IMPORTS only — `wire_publish.rs::export_cache_manifest`
// is the gate that decides whether the source node ships a manifest in the
// first place. By default a public-source pyramid ships its manifest; a
// private/circle-scoped pyramid ships nothing and the importer's first
// build runs cold (no cache, but no leak).
//
// DADBEAR auto-enable: post-import, this module creates a `dadbear_policy`
// contribution via `config_contributions::create_config_contribution_with_metadata`
// + `sync_config_to_operational`. It does NOT write directly to
// `pyramid_dadbear_config` — that's the Phase 4 anti-pattern the wanderers
// caught. The contribution path is the canonical route.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use super::config_contributions::{
    create_config_contribution_with_metadata, load_contribution_by_id,
    sync_config_to_operational,
};
use super::db;
use super::event_bus::BuildEventBus;
use super::step_context::CacheEntry;
use super::wire_native_metadata::{default_wire_native_metadata, WireMaturity};

// ─── Cache manifest types ────────────────────────────────────────────────────
//
// These types match the JSON shape defined in
// `docs/specs/cache-warming-and-import.md` "Cache Manifest Format" section
// (~line 151). They are SHARED between the publication side (encoded by
// `wire_publish.rs::export_cache_manifest`) and the import side (decoded
// here). Phase 7 ships v1 of `manifest_version`; future schema changes
// must remain additive.

/// The top-level cache manifest. Carries every node's metadata + the
/// content-addressable cache rows that ran against it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheManifest {
    /// Manifest schema version. Phase 7 = 1. Future additions must remain
    /// backwards-compatible (additive only). Importers reject unknown
    /// `manifest_version` values explicitly.
    pub manifest_version: u32,
    /// The Wire pyramid identifier this manifest came from. Recorded on the
    /// import state row + every cache row's `build_id` for audit trails.
    pub source_pyramid_id: String,
    /// ISO 8601 timestamp when the manifest was exported by the source
    /// node. Diagnostic only; not used for staleness decisions.
    pub exported_at: String,
    /// One entry per node in the source pyramid (L0 + upper layers).
    pub nodes: Vec<ImportNodeEntry>,
}

/// One node in the cache manifest. L0 nodes carry source file metadata
/// (`source_path`, `source_hash`, `source_size_bytes`); upper-layer nodes
/// carry `derived_from` (the list of ancestor node IDs that fed them).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportNodeEntry {
    pub node_id: String,
    pub layer: i64,
    /// Relative path from the source pyramid's root, only set on L0 nodes.
    /// Resolved against `local_source_root` at import time.
    #[serde(default)]
    pub source_path: Option<String>,
    /// SHA-256 of the source file content at export time. Compared against
    /// the local file's hash to determine staleness.
    #[serde(default)]
    pub source_hash: Option<String>,
    /// Source file byte count at export time. Diagnostic — not part of
    /// the staleness check.
    #[serde(default)]
    pub source_size_bytes: Option<u64>,
    /// Ancestor node IDs that this upper-layer node was derived from.
    /// Empty for L0 nodes. Walked during the upward propagation pass.
    #[serde(default)]
    pub derived_from: Vec<String>,
    /// Cache entries for the steps that ran against this node. May be
    /// empty if the source node had no cached calls for this node.
    #[serde(default)]
    pub cache_entries: Vec<ImportedCacheEntry>,
}

/// One cache entry within a node manifest. Maps 1:1 to a row that will
/// land in `pyramid_step_cache`. The fields mirror the table columns;
/// `created_at` is recomputed on insert.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportedCacheEntry {
    pub step_name: String,
    #[serde(default)]
    pub chunk_index: Option<i64>,
    #[serde(default)]
    pub depth: Option<i64>,
    pub cache_key: String,
    pub inputs_hash: String,
    pub prompt_hash: String,
    pub model_id: String,
    pub output_json: String,
    #[serde(default)]
    pub token_usage_json: Option<String>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub latency_ms: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Returned by `populate_from_import` and `import_pyramid` once the
/// staleness pass + cache population are complete. The four counters give
/// the caller everything ToolsMode needs to render a "first build will
/// cost ~$X to refill stale nodes" preview.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Cache entries actually inserted into `pyramid_step_cache`.
    pub cache_entries_valid: u64,
    /// Cache entries skipped because their owning node was stale.
    pub cache_entries_stale: u64,
    /// Number of nodes (L0 + upper) that did NOT cache and will need a
    /// fresh build.
    pub nodes_needing_rebuild: u64,
    /// Number of nodes whose cache entries were preserved from the import.
    pub nodes_with_valid_cache: u64,
}

// ─── SHA-256 file hashing helper ─────────────────────────────────────────────

/// Compute SHA-256 of a file's raw bytes, returning a hex string. Streams
/// the file in 64KiB chunks to avoid loading multi-megabyte sources into
/// memory at once. Mirrors the publication-side hash format
/// (`sha256:<hex>`) so direct equality is the staleness predicate.
///
/// Returns the bare hex string WITHOUT the `sha256:` prefix; the
/// comparison logic in `populate_from_import` strips the prefix from the
/// stored manifest hash before comparing.
fn sha256_file_hex(path: &Path) -> Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut file =
        File::open(path).with_context(|| format!("opening source file {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading source file {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    use std::fmt::Write as _;
    for byte in digest.iter() {
        let _ = write!(&mut out, "{:02x}", byte);
    }
    Ok(out)
}

/// Strip a `sha256:` (case-insensitive) prefix if present. Manifest hashes
/// MAY carry the prefix per the spec example; locally computed hashes
/// don't. We normalize both to bare hex before comparing.
fn normalize_hash(hash: &str) -> &str {
    if let Some(rest) = hash.strip_prefix("sha256:") {
        rest
    } else if let Some(rest) = hash.strip_prefix("SHA256:") {
        rest
    } else {
        hash
    }
}

// ─── Staleness pass: populate_from_import ────────────────────────────────────

/// Run the three-pass staleness check + cache population from a manifest.
///
/// This is the heart of Phase 7. It runs entirely against the in-memory
/// manifest plus the local SQLite connection — no network, no external
/// state. The caller is responsible for downloading the manifest and
/// providing the connection.
///
/// Pass 1 (L0 staleness): for each L0 node, resolve `local_source_root` +
/// `node.source_path`, SHA-256 the file, compare to `node.source_hash`.
/// Files that are missing OR mismatch are stale; their owning L0 nodes
/// are added to the stale set and their cache entries are dropped.
/// Surviving L0 nodes have their cache entries inserted via the existing
/// `db::store_cache` helper (idempotent INSERT OR REPLACE on the unique
/// `(slug, cache_key)` constraint).
///
/// Pass 2 (upward propagation): build an in-memory `dependents` map from
/// the manifest's `derived_from` lists, then BFS from the stale L0 set
/// outward. Every node reachable from a stale L0 ancestor joins the
/// stale set.
///
/// Pass 3 (upper-layer cache): for every upper-layer node NOT in the
/// stale set, insert its cache entries into `pyramid_step_cache`. Nodes
/// in the stale set are skipped entirely.
///
/// Returns an `ImportReport` summarizing the four counters.
///
/// Idempotency: re-running this function with the same manifest produces
/// the same result with no duplicate rows. The cache table's unique
/// `(slug, cache_key)` constraint catches the second insert and reduces
/// it to a no-op update. Resumption from a partial cursor is the caller's
/// concern (`import_pyramid`); this function always re-runs the full pass.
pub fn populate_from_import(
    conn: &Connection,
    manifest: &CacheManifest,
    target_slug: &str,
    local_source_root: &Path,
) -> Result<ImportReport> {
    // Pre-flight: validate the manifest version. Phase 7 ships v1 only.
    if manifest.manifest_version != 1 {
        return Err(anyhow!(
            "unsupported manifest_version {}: this build of Wire Node ships cache-warming v1 only",
            manifest.manifest_version
        ));
    }

    let mut report = ImportReport::default();
    let mut stale_node_ids: HashSet<String> = HashSet::new();
    // build_id stamped on every imported cache row so audit trails can
    // distinguish "imported from manifest M" from "built locally".
    let import_build_id = format!("import:{}", manifest.source_pyramid_id);

    // Build a quick `node_id → entry` lookup so passes 2/3 can skip the
    // O(N²) loop. Both passes need this map.
    let nodes_by_id: HashMap<&str, &ImportNodeEntry> = manifest
        .nodes
        .iter()
        .map(|n| (n.node_id.as_str(), n))
        .collect();

    // ── Pass 1: L0 staleness ───────────────────────────────────────────────
    //
    // For every L0 node (layer == 0), resolve the source file and compare
    // its hash to the manifest's recorded hash. Stale nodes get added to
    // the set; surviving nodes have their cache rows inserted.
    for node in manifest.nodes.iter().filter(|n| n.layer == 0) {
        let source_path = match node.source_path.as_deref() {
            Some(p) if !p.is_empty() => p,
            _ => {
                // L0 node without a source path is treated as stale —
                // we have no way to verify its provenance.
                debug!(
                    node_id = node.node_id,
                    "L0 node missing source_path; marking stale"
                );
                stale_node_ids.insert(node.node_id.clone());
                report.cache_entries_stale += node.cache_entries.len() as u64;
                continue;
            }
        };

        let local_path = resolve_source_path(local_source_root, source_path);

        if !local_path.exists() {
            debug!(
                node_id = node.node_id,
                local_path = %local_path.display(),
                "L0 source file missing locally; marking stale"
            );
            stale_node_ids.insert(node.node_id.clone());
            report.cache_entries_stale += node.cache_entries.len() as u64;
            continue;
        }

        let manifest_hash = match node.source_hash.as_deref() {
            Some(h) if !h.is_empty() => h,
            _ => {
                // Manifest declared a source path but no hash — treat as
                // stale rather than blindly trust an unverified entry.
                warn!(
                    node_id = node.node_id,
                    "L0 node has source_path but no source_hash; marking stale"
                );
                stale_node_ids.insert(node.node_id.clone());
                report.cache_entries_stale += node.cache_entries.len() as u64;
                continue;
            }
        };

        // Hash the local file. Failure is treated as a stale-mark rather
        // than a hard error so a single unreadable file doesn't abort the
        // whole import.
        let local_hash = match sha256_file_hex(&local_path) {
            Ok(h) => h,
            Err(e) => {
                warn!(
                    node_id = node.node_id,
                    local_path = %local_path.display(),
                    error = %e,
                    "failed to hash L0 source file; marking stale"
                );
                stale_node_ids.insert(node.node_id.clone());
                report.cache_entries_stale += node.cache_entries.len() as u64;
                continue;
            }
        };

        if normalize_hash(manifest_hash).to_ascii_lowercase()
            != local_hash.to_ascii_lowercase()
        {
            debug!(
                node_id = node.node_id,
                manifest_hash = manifest_hash,
                local_hash = local_hash,
                "L0 source hash mismatch; marking stale"
            );
            stale_node_ids.insert(node.node_id.clone());
            report.cache_entries_stale += node.cache_entries.len() as u64;
            continue;
        }

        // Source matches — insert this L0 node's cache entries.
        let inserted = insert_cache_entries(
            conn,
            target_slug,
            &import_build_id,
            &node.cache_entries,
        )?;
        report.cache_entries_valid += inserted;
        report.nodes_with_valid_cache += 1;
    }

    // ── Pass 2: upward propagation ─────────────────────────────────────────
    //
    // Build the dependents graph from the manifest's `derived_from` lists.
    // For each node N, `dependents[parent]` contains every child that lists
    // `parent` in its `derived_from`. BFS from the stale L0 frontier.
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for node in &manifest.nodes {
        for parent in &node.derived_from {
            dependents
                .entry(parent.clone())
                .or_default()
                .push(node.node_id.clone());
        }
    }

    let mut frontier: VecDeque<String> = stale_node_ids.iter().cloned().collect();
    while let Some(node_id) = frontier.pop_front() {
        if let Some(children) = dependents.get(&node_id) {
            for child in children {
                if stale_node_ids.insert(child.clone()) {
                    frontier.push_back(child.clone());
                }
            }
        }
    }

    // ── Pass 3: upper-layer cache ──────────────────────────────────────────
    //
    // Insert cache entries for every upper-layer node NOT in the stale set.
    // Stale upper nodes have their cache entries dropped — when the user
    // runs the next build the executor will re-run them fresh against the
    // new (stale-but-locally-correct) inputs.
    for node in manifest.nodes.iter().filter(|n| n.layer > 0) {
        if stale_node_ids.contains(&node.node_id) {
            report.cache_entries_stale += node.cache_entries.len() as u64;
            continue;
        }
        let inserted = insert_cache_entries(
            conn,
            target_slug,
            &import_build_id,
            &node.cache_entries,
        )?;
        report.cache_entries_valid += inserted;
        report.nodes_with_valid_cache += 1;
    }

    // Final tallies. The stale set holds the count of nodes that need a
    // fresh build — that includes both stale L0s and every dependent we
    // walked to in pass 2.
    report.nodes_needing_rebuild = stale_node_ids.len() as u64;

    // Touch nodes_by_id so the unused-binding warning doesn't fire.
    let _ = nodes_by_id;

    Ok(report)
}

/// Insert a slice of imported cache entries into `pyramid_step_cache` for
/// the target slug. Each insert goes through `db::store_cache` which uses
/// `INSERT ... ON CONFLICT(slug, cache_key) DO UPDATE` so re-imports of
/// the same row don't duplicate.
///
/// Returns the count of entries actually attempted (== entries.len() on
/// success). Per-row failures emit a warning and continue — a single
/// corrupt entry shouldn't abort the whole import.
fn insert_cache_entries(
    conn: &Connection,
    target_slug: &str,
    build_id: &str,
    entries: &[ImportedCacheEntry],
) -> Result<u64> {
    let mut inserted: u64 = 0;
    for entry in entries {
        let cache_entry = CacheEntry {
            slug: target_slug.to_string(),
            build_id: build_id.to_string(),
            step_name: entry.step_name.clone(),
            chunk_index: entry.chunk_index.unwrap_or(-1),
            depth: entry.depth.unwrap_or(0),
            cache_key: entry.cache_key.clone(),
            inputs_hash: entry.inputs_hash.clone(),
            prompt_hash: entry.prompt_hash.clone(),
            model_id: entry.model_id.clone(),
            output_json: entry.output_json.clone(),
            token_usage_json: entry.token_usage_json.clone(),
            cost_usd: entry.cost_usd,
            latency_ms: entry.latency_ms,
            // Imported entries are NOT force-fresh — they're cached
            // outputs from a peer node. The reroll flag is reserved for
            // user-initiated cache invalidations.
            force_fresh: false,
            supersedes_cache_id: None,
        };
        match db::store_cache(conn, &cache_entry) {
            Ok(_) => inserted += 1,
            Err(e) => {
                warn!(
                    target_slug,
                    cache_key = entry.cache_key,
                    step_name = entry.step_name,
                    error = %e,
                    "failed to insert imported cache entry; skipping"
                );
            }
        }
    }
    Ok(inserted)
}

/// Resolve a relative source path against a local root, normalizing both
/// `/` and `\` separators so a manifest exported on a different OS still
/// resolves correctly.
fn resolve_source_path(local_source_root: &Path, manifest_relative: &str) -> PathBuf {
    let normalized = manifest_relative.replace('\\', "/");
    let mut path = local_source_root.to_path_buf();
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            // Refuse parent traversal — the manifest cannot escape the
            // local source root.
            warn!(
                manifest_relative,
                "refusing parent-directory segment in manifest source_path"
            );
            return PathBuf::new();
        }
        path.push(segment);
    }
    path
}

// ─── Top-level entry point: import_pyramid ───────────────────────────────────

/// Public entry for the import flow. Runs the resumable import in three
/// phases: (1) check or create the import state row, (2) call
/// `populate_from_import`, (3) enable DADBEAR via the Phase 4
/// contribution path. The manifest is supplied by the caller — this
/// function does NOT do the network fetch itself, because the existing
/// `WireImportClient` only knows about chain definitions and pulling the
/// pyramid manifest is a Phase 10 / Phase 14 frontend wiring concern.
///
/// Phase 7 ships the staleness pass + DADBEAR auto-enable + report
/// assembly. Phase 10's ImportPyramidWizard will call this entry point
/// once it has the manifest in hand.
///
/// `bus` is the build event bus that the contribution sync path emits
/// `ConfigSynced` events on. `event_bus` is plumbed through to keep
/// downstream listeners informed when DADBEAR turns on.
pub fn import_pyramid(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    wire_pyramid_id: &str,
    target_slug: &str,
    source_path: &str,
    manifest: &CacheManifest,
) -> Result<ImportReport> {
    // Validate inputs up front.
    if target_slug.trim().is_empty() {
        return Err(anyhow!("target_slug must not be empty"));
    }
    if source_path.trim().is_empty() {
        return Err(anyhow!("source_path must not be empty"));
    }
    let source_root = Path::new(source_path);
    if !source_root.exists() {
        return Err(anyhow!(
            "source_path {} does not exist on the local filesystem",
            source_path
        ));
    }
    if !source_root.is_dir() {
        return Err(anyhow!(
            "source_path {} is not a directory; pyramid imports need a folder root",
            source_path
        ));
    }

    // Check for an existing import state row. If one exists for a
    // DIFFERENT pyramid id, refuse — the caller has to cancel first.
    let existing = db::load_import_state(conn, target_slug)?;
    if let Some(state) = &existing {
        if state.wire_pyramid_id != wire_pyramid_id {
            return Err(anyhow!(
                "target slug {} already has an in-flight import for pyramid {} \
                 — cancel it before importing a different pyramid",
                target_slug,
                state.wire_pyramid_id
            ));
        }
        // Same pyramid → resume. We don't need to redo any work that
        // already landed; the staleness pass below is idempotent.
        debug!(
            target_slug,
            wire_pyramid_id,
            status = state.status,
            "resuming existing import"
        );
    } else {
        db::create_import_state(conn, target_slug, wire_pyramid_id, source_path)?;
    }

    // Mark the import as validating sources, count totals.
    let nodes_total = manifest.nodes.len() as i64;
    let cache_entries_total: i64 = manifest
        .nodes
        .iter()
        .map(|n| n.cache_entries.len() as i64)
        .sum();
    db::update_import_state(
        conn,
        target_slug,
        &db::ImportStateProgress {
            status: Some("validating_sources".to_string()),
            nodes_total: Some(nodes_total),
            cache_entries_total: Some(cache_entries_total),
            ..Default::default()
        },
    )?;

    // Run the staleness pass + cache population. This is the bulk of the
    // import work. Failures here are reported but the state row is left
    // behind so the user can retry / cancel.
    let report = match populate_from_import(conn, manifest, target_slug, source_root) {
        Ok(r) => r,
        Err(e) => {
            let _ = db::update_import_state(
                conn,
                target_slug,
                &db::ImportStateProgress {
                    status: Some("failed".to_string()),
                    error_message: Some(e.to_string()),
                    ..Default::default()
                },
            );
            return Err(e);
        }
    };

    // Bump progress to "populating_cache" complete and update counters.
    db::update_import_state(
        conn,
        target_slug,
        &db::ImportStateProgress {
            status: Some("populating_cache".to_string()),
            nodes_processed: Some(nodes_total),
            cache_entries_validated: Some(cache_entries_total),
            cache_entries_inserted: Some(report.cache_entries_valid as i64),
            ..Default::default()
        },
    )?;

    // ── DADBEAR auto-enable via Phase 4 contribution path ─────────────────
    //
    // Build a minimal `dadbear_policy` YAML, create the contribution row
    // through `create_config_contribution_with_metadata`, then call
    // `sync_config_to_operational` which routes to the upsert helper.
    // We do NOT write directly to `pyramid_dadbear_config` — that's the
    // Phase 4 anti-pattern the wanderer flagged on multiple bypass
    // paths. The contribution path is the canonical route.
    if let Err(e) = enable_dadbear_via_contribution(conn, bus, target_slug, source_path) {
        warn!(
            target_slug,
            error = %e,
            "DADBEAR auto-enable failed during import; cache populated but DADBEAR \
             will need to be enabled manually"
        );
    }

    // Mark complete.
    db::update_import_state(
        conn,
        target_slug,
        &db::ImportStateProgress {
            status: Some("complete".to_string()),
            ..Default::default()
        },
    )?;

    info!(
        target_slug,
        wire_pyramid_id,
        cache_entries_valid = report.cache_entries_valid,
        cache_entries_stale = report.cache_entries_stale,
        nodes_needing_rebuild = report.nodes_needing_rebuild,
        nodes_with_valid_cache = report.nodes_with_valid_cache,
        "pyramid import complete"
    );

    Ok(report)
}

/// Build a `dadbear_policy` YAML for the imported pyramid and route it
/// through Phase 4's contribution + sync path. The contribution_id is
/// recorded on the resulting `pyramid_dadbear_config` row via
/// `upsert_dadbear_policy`.
///
/// Per the Phase 4 wanderer findings, every DADBEAR mutation MUST flow
/// through the contribution path so the audit trail and supersession
/// chain stay coherent. Direct INSERT into `pyramid_dadbear_config`
/// would create a row with NULL `contribution_id` and break the
/// invariant.
fn enable_dadbear_via_contribution(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    target_slug: &str,
    source_path: &str,
) -> Result<()> {
    // Build the YAML directly. We use the canonical key set so the
    // `db::DadbearPolicyYaml` deserializer parses it without needing
    // any extra fields. `content_type` is required by the operational
    // table but not part of the spec's auto-enable shape — we default
    // to `document` since the manifest doesn't carry the source's
    // declared content type. Phase 10's wizard can override.
    let yaml_body = format!(
        "source_path: {source_path}\n\
         content_type: document\n\
         scan_interval_secs: 60\n\
         debounce_secs: 30\n\
         session_timeout_secs: 1800\n\
         batch_size: 1\n\
         enabled: true\n",
        source_path = yaml_escape(source_path),
    );

    // Build canonical Wire metadata. The migration path's "imported pyramid"
    // case lands as a `Canon` maturity contribution because it's a
    // verified config from another node, not a draft proposal. Source is
    // explicitly `import` so future audits can distinguish.
    let mut metadata = default_wire_native_metadata("dadbear_policy", Some(target_slug));
    metadata.maturity = WireMaturity::Canon;

    let contribution_id = create_config_contribution_with_metadata(
        conn,
        "dadbear_policy",
        Some(target_slug),
        &yaml_body,
        Some("Imported pyramid — DADBEAR auto-enabled"),
        "import",
        Some("pyramid_import"),
        "active",
        &metadata,
    )?;

    // Re-load the contribution to dispatch through sync_config_to_operational.
    // The sync path is the only way to land a row on `pyramid_dadbear_config`
    // with the contribution_id FK populated.
    let contribution = load_contribution_by_id(conn, &contribution_id)?
        .ok_or_else(|| anyhow!("contribution {contribution_id} disappeared after create"))?;
    sync_config_to_operational(conn, bus, &contribution)
        .map_err(|e| anyhow!("sync_config_to_operational failed: {e}"))?;

    info!(
        target_slug,
        contribution_id, "DADBEAR auto-enabled via Phase 4 contribution path"
    );
    Ok(())
}

/// Best-effort YAML string escape — wraps the value in double quotes if
/// it contains characters that would break a bare YAML scalar (whitespace
/// other than spaces, special characters, or a leading dash).
fn yaml_escape(value: &str) -> String {
    let needs_quoting = value.is_empty()
        || value.starts_with('-')
        || value.starts_with('?')
        || value.starts_with(':')
        || value.starts_with('#')
        || value.contains(':')
        || value.contains('\n')
        || value.contains('"')
        || value.contains('\'')
        || value.chars().any(|c| c.is_control());
    if needs_quoting {
        // Wrap in single quotes; double any embedded single quotes.
        let escaped = value.replace('\'', "''");
        format!("'{escaped}'")
    } else {
        value.to_string()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::event_bus::BuildEventBus;
    use crate::pyramid::step_context::compute_cache_key;
    use rusqlite::Connection;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Open an in-memory SQLite + run init_pyramid_db. Mirrors the helper
    /// used in `db::step_cache_tests`.
    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        // Most tests need to reference the target slug as a foreign key
        // implicitly via pyramid_dadbear_config, so we create it up front
        // for ergonomic test setup.
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path)
             VALUES ('test-import', 'document', '')",
            [],
        )
        .unwrap();
        conn
    }

    fn make_bus() -> Arc<BuildEventBus> {
        Arc::new(BuildEventBus::new())
    }

    /// Create a cache entry for a given node + step. The cache_key is
    /// derived from the inputs/prompt/model triple so two entries with
    /// different seeds get different keys.
    fn make_imported_entry(
        step_name: &str,
        chunk_index: i64,
        depth: i64,
        seed: &str,
    ) -> ImportedCacheEntry {
        let inputs_hash = format!("inputs:{seed}");
        let prompt_hash = format!("prompt:{seed}");
        let model_id = "openrouter/test-1".to_string();
        let cache_key = compute_cache_key(&inputs_hash, &prompt_hash, &model_id);
        ImportedCacheEntry {
            step_name: step_name.into(),
            chunk_index: Some(chunk_index),
            depth: Some(depth),
            cache_key,
            inputs_hash,
            prompt_hash,
            model_id,
            output_json: serde_json::json!({"content":"hello","usage":{}}).to_string(),
            token_usage_json: Some("{}".into()),
            cost_usd: Some(0.001),
            latency_ms: Some(42),
            created_at: Some("2026-04-09T15:30:00Z".into()),
        }
    }

    /// Build a manifest with three L0 nodes and two upper-layer nodes
    /// where:
    ///   - L0a + L0b have files matching their hashes (will cache-hit)
    ///   - L0c has a hash that won't match (will mark stale)
    ///   - L1a derives from L0a + L0b (will cache-hit)
    ///   - L1b derives from L0b + L0c (will be marked stale by propagation)
    fn build_mixed_manifest(
        l0a_path: &str,
        l0a_hash: &str,
        l0b_path: &str,
        l0b_hash: &str,
        l0c_path: &str,
        l0c_hash: &str,
    ) -> CacheManifest {
        CacheManifest {
            manifest_version: 1,
            source_pyramid_id: "wire:test-pyramid".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            nodes: vec![
                ImportNodeEntry {
                    node_id: "L0a".into(),
                    layer: 0,
                    source_path: Some(l0a_path.into()),
                    source_hash: Some(l0a_hash.into()),
                    source_size_bytes: Some(10),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry(
                        "source_extract",
                        0,
                        0,
                        "L0a-extract",
                    )],
                },
                ImportNodeEntry {
                    node_id: "L0b".into(),
                    layer: 0,
                    source_path: Some(l0b_path.into()),
                    source_hash: Some(l0b_hash.into()),
                    source_size_bytes: Some(20),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry(
                        "source_extract",
                        1,
                        0,
                        "L0b-extract",
                    )],
                },
                ImportNodeEntry {
                    node_id: "L0c".into(),
                    layer: 0,
                    source_path: Some(l0c_path.into()),
                    source_hash: Some(l0c_hash.into()),
                    source_size_bytes: Some(30),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry(
                        "source_extract",
                        2,
                        0,
                        "L0c-extract",
                    )],
                },
                ImportNodeEntry {
                    node_id: "L1a".into(),
                    layer: 1,
                    source_path: None,
                    source_hash: None,
                    source_size_bytes: None,
                    derived_from: vec!["L0a".into(), "L0b".into()],
                    cache_entries: vec![make_imported_entry(
                        "cluster_synthesize",
                        -1,
                        1,
                        "L1a-cluster",
                    )],
                },
                ImportNodeEntry {
                    node_id: "L1b".into(),
                    layer: 1,
                    source_path: None,
                    source_hash: None,
                    source_size_bytes: None,
                    derived_from: vec!["L0b".into(), "L0c".into()],
                    cache_entries: vec![make_imported_entry(
                        "cluster_synthesize",
                        -1,
                        1,
                        "L1b-cluster",
                    )],
                },
            ],
        }
    }

    /// Write `content` to `path` and return its hash hex.
    fn write_and_hash(path: &Path, content: &str) -> String {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        sha256_file_hex(path).unwrap()
    }

    #[test]
    fn test_normalize_hash_strips_sha256_prefix() {
        assert_eq!(normalize_hash("sha256:abc123"), "abc123");
        assert_eq!(normalize_hash("SHA256:abc123"), "abc123");
        assert_eq!(normalize_hash("abc123"), "abc123");
        assert_eq!(normalize_hash(""), "");
    }

    #[test]
    fn test_resolve_source_path_normalizes_separators() {
        let root = Path::new("/tmp/proj");
        assert_eq!(
            resolve_source_path(root, "src/main.rs"),
            PathBuf::from("/tmp/proj/src/main.rs")
        );
        assert_eq!(
            resolve_source_path(root, "src\\main.rs"),
            PathBuf::from("/tmp/proj/src/main.rs")
        );
    }

    #[test]
    fn test_resolve_source_path_rejects_parent_traversal() {
        let root = Path::new("/tmp/proj");
        // `..` segments produce an empty path so the caller's
        // `.exists()` check turns into a clean stale-mark, not a
        // file system escape.
        assert_eq!(resolve_source_path(root, "../etc/passwd"), PathBuf::new());
    }

    #[test]
    fn test_yaml_escape_quotes_special_characters() {
        assert_eq!(yaml_escape("/tmp/normal-path"), "/tmp/normal-path");
        // Colons trigger quoting (paths on Windows would otherwise break
        // YAML's key:value parser).
        assert_eq!(yaml_escape("C:/Users/foo"), "'C:/Users/foo'");
        // Single quotes get doubled inside the wrap.
        assert_eq!(yaml_escape("foo's bar"), "'foo''s bar'");
    }

    #[test]
    fn test_unsupported_manifest_version_returns_error() {
        let conn = mem_conn();
        let manifest = CacheManifest {
            manifest_version: 99,
            source_pyramid_id: "wire:bad".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            nodes: vec![],
        };
        let err = populate_from_import(&conn, &manifest, "test-import", Path::new("/tmp"))
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unsupported manifest_version"),
            "expected version error, got: {msg}"
        );
    }

    #[test]
    fn test_populate_from_import_mixed_stale_l0_propagates_to_upper_layers() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");

        let l0a_hash = write_and_hash(&l0a_path, "alpha content");
        let l0b_hash = write_and_hash(&l0b_path, "beta content");
        // L0c gets one content on disk but the manifest has a different
        // hash → mismatch → stale.
        let _ = write_and_hash(&l0c_path, "gamma content (real)");
        let bogus_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let manifest = build_mixed_manifest(
            "a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", bogus_hash,
        );

        let report =
            populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();

        // L0a + L0b cache-hit (2 entries), L0c is stale (1 entry dropped).
        // Of the upper layers: L1a derives from L0a + L0b (both fresh) →
        // cache-hits (1 entry); L1b derives from L0b + L0c → L0c stale
        // propagates → L1b stale → 1 entry dropped.
        assert_eq!(
            report.cache_entries_valid, 3,
            "expected 3 valid (L0a, L0b, L1a), got {report:?}"
        );
        assert_eq!(
            report.cache_entries_stale, 2,
            "expected 2 stale (L0c, L1b), got {report:?}"
        );
        // Stale set: L0c + L1b = 2 nodes needing rebuild.
        assert_eq!(
            report.nodes_needing_rebuild, 2,
            "expected 2 nodes needing rebuild, got {report:?}"
        );
        // Valid nodes: L0a + L0b + L1a = 3 nodes with valid cache.
        assert_eq!(report.nodes_with_valid_cache, 3);

        // Verify the cache rows actually landed in pyramid_step_cache.
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 3, "expected 3 rows in pyramid_step_cache");

        // Verify the stale L1b entry is NOT in the cache.
        let l1b_cache_key =
            compute_cache_key("inputs:L1b-cluster", "prompt:L1b-cluster", "openrouter/test-1");
        let l1b_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import' AND cache_key = ?1",
                [&l1b_cache_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(l1b_present, 0, "stale L1b cache entry should not be present");
    }

    #[test]
    fn test_populate_from_import_missing_l0_file_marks_stale() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        // L0b is referenced in the manifest but no file on disk.
        let l0b_hash = "deadbeef".to_string();

        let manifest = build_mixed_manifest(
            "a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", "more-deadbeef",
        );

        let report =
            populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();

        // Only L0a is valid (1 entry). Both L0b and L0c are stale, and
        // every upper layer that touches them propagates stale.
        // L1a depends on L0a + L0b → stale (L0b missing). L1b depends on
        // L0b + L0c → stale.
        assert_eq!(report.cache_entries_valid, 1);
        // 4 stale entries: L0b, L0c, L1a, L1b.
        assert_eq!(report.cache_entries_stale, 4);
        assert_eq!(report.nodes_needing_rebuild, 4);
        assert_eq!(report.nodes_with_valid_cache, 1);
    }

    #[test]
    fn test_populate_from_import_idempotent() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest = build_mixed_manifest(
            "a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash,
        );

        // First import: all 5 nodes are valid (no stale L0s).
        let r1 =
            populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
        assert_eq!(r1.cache_entries_valid, 5);

        let row_count_after_first: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count_after_first, 5);

        // Second import: same manifest. The store_cache helper uses
        // INSERT OR REPLACE on the unique (slug, cache_key) constraint,
        // so re-importing produces no duplicate rows.
        let r2 =
            populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
        assert_eq!(r2.cache_entries_valid, 5);

        let row_count_after_second: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            row_count_after_second, 5,
            "re-import should not duplicate rows"
        );
    }

    #[test]
    fn test_import_pyramid_full_flow_creates_state_then_completes() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest = build_mixed_manifest(
            "a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash,
        );

        let report = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        assert_eq!(report.cache_entries_valid, 5);
        assert_eq!(report.cache_entries_stale, 0);

        // The import state row is now `complete`.
        let state = db::load_import_state(&conn, "test-import").unwrap().unwrap();
        assert_eq!(state.status, "complete");
        assert_eq!(state.nodes_total, Some(5));
        assert_eq!(state.cache_entries_total, Some(5));
        assert_eq!(state.cache_entries_inserted, 5);

        // DADBEAR contribution exists with source = 'import'.
        let contrib_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE slug = 'test-import' AND schema_type = 'dadbear_policy'
                   AND source = 'import' AND status = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            contrib_count, 1,
            "expected one active dadbear_policy contribution from import"
        );

        // The synced operational row exists with the contribution_id FK.
        let dadbear_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_dadbear_config
                 WHERE slug = 'test-import' AND contribution_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dadbear_count, 1,
            "expected pyramid_dadbear_config row with contribution_id FK"
        );
    }

    #[test]
    fn test_import_pyramid_resume_same_pyramid_succeeds() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest = build_mixed_manifest(
            "a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash,
        );

        // First call lands a complete state row + cache.
        let _ = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        // Second call with the same pyramid id is treated as a resume
        // and re-runs idempotently.
        let report = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        assert_eq!(report.cache_entries_valid, 5);
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 5);
    }

    #[test]
    fn test_import_pyramid_refuses_different_pyramid_for_same_slug() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();

        // Plant an import state for pyramid A.
        db::create_import_state(
            &conn,
            "test-import",
            "wire:pyramid-A",
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        let bus = make_bus();
        let manifest = CacheManifest {
            manifest_version: 1,
            source_pyramid_id: "wire:pyramid-B".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            nodes: vec![],
        };

        let err = import_pyramid(
            &conn,
            &bus,
            "wire:pyramid-B",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("already has an in-flight import for pyramid"),
            "expected slug-collision error, got: {msg}"
        );
    }

    #[test]
    fn test_import_pyramid_rejects_missing_source_path() {
        let conn = mem_conn();
        let bus = make_bus();
        let manifest = CacheManifest {
            manifest_version: 1,
            source_pyramid_id: "wire:test".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            nodes: vec![],
        };
        let err = import_pyramid(
            &conn,
            &bus,
            "wire:test",
            "test-import",
            "/nonexistent/dir",
            &manifest,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not exist"),
            "expected does-not-exist error, got: {msg}"
        );
    }

    #[test]
    fn test_cache_manifest_serde_round_trip() {
        let manifest = CacheManifest {
            manifest_version: 1,
            source_pyramid_id: "wire:abc".into(),
            exported_at: "2026-04-09T15:30:00Z".into(),
            nodes: vec![
                ImportNodeEntry {
                    node_id: "L0a".into(),
                    layer: 0,
                    source_path: Some("src/main.rs".into()),
                    source_hash: Some("sha256:abc".into()),
                    source_size_bytes: Some(123),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry("source_extract", 0, 0, "seed")],
                },
                ImportNodeEntry {
                    node_id: "L1a".into(),
                    layer: 1,
                    source_path: None,
                    source_hash: None,
                    source_size_bytes: None,
                    derived_from: vec!["L0a".into()],
                    cache_entries: vec![],
                },
            ],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: CacheManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn test_dadbear_contribution_has_canonical_metadata() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();

        enable_dadbear_via_contribution(
            &conn,
            &bus,
            "test-import",
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        // Look up the contribution and verify its metadata reflects
        // the canonical Wire Native Documents shape — non-empty JSON,
        // maturity = canon, source = import.
        let row: (String, String, String) = conn
            .query_row(
                "SELECT yaml_content, source, wire_native_metadata_json
                 FROM pyramid_config_contributions
                 WHERE slug = 'test-import' AND schema_type = 'dadbear_policy'
                 LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(row.0.contains("source_path"));
        assert!(row.0.contains("content_type"));
        assert!(row.0.contains("scan_interval_secs: 60"));
        assert_eq!(row.1, "import");
        assert_ne!(row.2, "{}", "wire_native_metadata_json should not be the empty stub");
        assert!(
            row.2.contains("\"maturity\""),
            "wire_native_metadata_json should carry the canonical maturity field"
        );
    }
}
