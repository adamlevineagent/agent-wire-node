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
// Idempotency: every cache insert goes through `db::store_cache_if_absent`
// which is the `INSERT OR IGNORE` semantic the spec mandates
// (see `docs/specs/cache-warming-and-import.md` "Idempotency" section
// ~line 341). Unlike the default `store_cache` helper which uses
// `ON CONFLICT DO UPDATE`, this helper leaves pre-existing rows untouched
// on conflict. That's the load-bearing property for the reroll-then-resume
// case: if a user imports a pyramid, then force-rerolls a step locally
// (which writes a new row at the same content-addressable key with
// `force_fresh = 1`), and then for any reason the import is re-run (resume
// from cursor after crash, or explicit retry), the rerolled row is NOT
// clobbered. The re-imported row is silently skipped at the row that
// already exists, the rerolled state is preserved, and the three counters
// in `ImportReport` still reflect the fact that the slot is occupied. The
// `pyramid_import_state` table provides the cursor so a partially-completed
// import can resume from the last node processed without re-running the
// L0 hashing.
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
    create_config_contribution_with_metadata, load_active_config_contribution,
    load_contribution_by_id, sync_config_to_operational,
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
/// Surviving L0 nodes have their cache entries inserted via
/// `db::store_cache_if_absent` — `INSERT ... ON CONFLICT DO NOTHING`
/// on the unique `(slug, cache_key)` constraint, so any locally-existing
/// row at the same content-addressable key is left untouched.
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
/// the same result with no duplicate rows. The helper uses
/// `INSERT ... ON CONFLICT DO NOTHING` so a second call leaves every
/// already-landed row untouched. Critically, this also preserves any
/// local force-fresh (reroll) rows that the user wrote between import
/// attempts — the spec's `INSERT OR IGNORE` requirement (see
/// "Idempotency" section ~line 341) exists precisely to avoid
/// clobbering local rerolls during resume. Resumption from a partial
/// cursor is the caller's concern (`import_pyramid`); this function
/// always re-runs the full pass.
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

        if normalize_hash(manifest_hash).to_ascii_lowercase() != local_hash.to_ascii_lowercase() {
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
        let inserted =
            insert_cache_entries(conn, target_slug, &import_build_id, &node.cache_entries)?;
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
        let inserted =
            insert_cache_entries(conn, target_slug, &import_build_id, &node.cache_entries)?;
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
/// the target slug. Each insert goes through `db::store_cache_if_absent`
/// which uses `INSERT ... ON CONFLICT(slug, cache_key) DO NOTHING` so
/// re-imports of the same row don't duplicate AND don't overwrite any
/// row the local user may have written between import attempts (e.g. a
/// force-rerolled cache entry).
///
/// Returns the count of entries the report should consider "valid" —
/// the spec's `ImportReport.cache_entries_valid` counts every entry whose
/// content-addressable slot is occupied after the call, regardless of
/// whether THIS import wrote it or a prior attempt did. That matches the
/// importer's mental model ("this many entries are now populated in my
/// cache") and gives a stable report across resumes.
///
/// Per-row failures emit a warning and continue — a single corrupt
/// entry shouldn't abort the whole import.
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
            note: None,
        };
        match db::store_cache_if_absent(conn, &cache_entry) {
            Ok(_row_actually_inserted) => {
                // Count the slot as "valid" whether this call wrote the
                // row or a prior import (or a local reroll) already did.
                // The importer's counter is "how many entries are now
                // present in my cache", not "how many bytes did I just
                // write". That makes the count stable across resumes.
                inserted += 1;
            }
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
///
/// Idempotent on re-import (the resume path): if an active
/// `dadbear_policy` contribution already exists for `target_slug`, this
/// function re-syncs it through `sync_config_to_operational` (so the
/// operational row's `contribution_id` FK is reasserted) but does NOT
/// create a duplicate active contribution. Creating two `status=active`
/// rows for the same `(slug, schema_type)` pair would silently
/// desynchronize the audit trail from the operational row and produce
/// the dangling-active-contribution class of bug the wanderers' "every
/// data path is a contribution" pattern was designed to prevent.
fn enable_dadbear_via_contribution(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    target_slug: &str,
    source_path: &str,
) -> Result<()> {
    // Resume idempotency: if an active dadbear_policy contribution
    // already exists for this slug (e.g. a prior import landed it and
    // the user is re-running the import to resume after a crash), don't
    // create a duplicate. Re-sync the existing one through the
    // dispatcher so the operational row's contribution_id FK is
    // reasserted, then return.
    if let Some(existing) =
        load_active_config_contribution(conn, "dadbear_policy", Some(target_slug))?
    {
        debug!(
            target_slug,
            contribution_id = existing.contribution_id,
            "active dadbear_policy contribution already exists for slug; \
             re-syncing through dispatcher instead of creating a duplicate"
        );
        sync_config_to_operational(conn, bus, &existing)
            .map_err(|e| anyhow!("sync_config_to_operational failed during re-sync: {e}"))?;
        return Ok(());
    }

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

// ─── Cancel: roll back partial import state + cache rows ────────────────────

/// Result returned from `cancel_pyramid_import` so the IPC handler can
/// distinguish "had nothing to cancel" from "rolled back N cache rows".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportCancelReport {
    /// Whether an import state row existed for the target slug at cancel
    /// time. `false` for an idempotent cancel of a slug that was never
    /// imported.
    pub state_row_existed: bool,
    /// Number of cache rows deleted from `pyramid_step_cache` during the
    /// rollback. Counts only rows whose `build_id` matches the import's
    /// synthetic `import:{wire_pyramid_id}` prefix — rows written by
    /// later builds or local LLM calls are NOT touched.
    pub cache_rows_rolled_back: u64,
}

/// Cancel an in-flight or completed pyramid import for `target_slug`.
///
/// Per `docs/specs/cache-warming-and-import.md` "Cleanup" section
/// (~line 345):
///
/// > "On explicit user cancel, the row is deleted along with any
/// > partially inserted cache entries and the target slug's DB rows."
///
/// This function implements the cache-rollback half of that contract.
/// It deletes:
///   1. Every cache row in `pyramid_step_cache` for `target_slug` whose
///      `build_id` starts with the synthetic `import:` prefix the import
///      path stamps. Locally-built rows (build_id from chain executor or
///      local rerolls) are NOT touched.
///   2. The `pyramid_import_state` row for the target slug.
///
/// What this function INTENTIONALLY does NOT touch:
///   - DADBEAR contributions: the contribution is `Canon` maturity and
///     deleting it directly would bypass the contribution path. The
///     user can disable DADBEAR through the existing oversight UI which
///     creates a properly-superseded contribution.
///   - Pyramid node / evidence / chunk rows: Phase 7's import does not
///     populate these. Phase 10's frontend wizard owns slug creation
///     and is responsible for cleaning up its own rows.
///   - Cache rows from prior local builds: filtering by `build_id LIKE
///     'import:%'` keeps the rollback narrowly scoped to the import's
///     own writes. A cache row that the importer skipped (because the
///     local user had already written one at the same content-addressable
///     key, e.g. via a force-fresh reroll) is preserved.
///
/// Idempotent: cancelling a slug with no import state and no imported
/// cache rows is a no-op that returns `ImportCancelReport { state_row_existed:
/// false, cache_rows_rolled_back: 0 }`.
pub fn cancel_pyramid_import(conn: &Connection, target_slug: &str) -> Result<ImportCancelReport> {
    // Resolve the wire_pyramid_id from the import state row so we know
    // which build_id prefix to filter on. If there's no state row, we
    // still attempt to delete any cache rows that look like imports
    // for this slug — this handles the case where a previous cancel
    // deleted the state row but the cache rollback failed mid-way.
    let state = db::load_import_state(conn, target_slug)?;
    let state_row_existed = state.is_some();

    // Collect every distinct build_id under this slug that starts with
    // the `import:` prefix. There may be more than one if the user has
    // re-imported the slug from different source pyramids over time
    // (theoretical — `import_pyramid` refuses different wire_pyramid_ids
    // for the same slug — but defensive cleanup is cheap).
    let mut import_build_ids: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT build_id FROM pyramid_step_cache
             WHERE slug = ?1 AND build_id LIKE 'import:%'",
        )?;
        let iter = stmt.query_map(rusqlite::params![target_slug], |row| {
            row.get::<_, String>(0)
        })?;
        for r in iter {
            import_build_ids.push(r?);
        }
    }

    // Delete all rows under those build_ids. Going through a single
    // statement keeps the rollback atomic per build_id.
    let mut total_deleted: u64 = 0;
    for build_id in &import_build_ids {
        let n = conn.execute(
            "DELETE FROM pyramid_step_cache
             WHERE slug = ?1 AND build_id = ?2",
            rusqlite::params![target_slug, build_id],
        )?;
        total_deleted += n as u64;
    }

    // Drop the import state row last so the rollback is observable.
    db::delete_import_state(conn, target_slug)?;

    info!(
        target_slug,
        state_row_existed,
        cache_rows_rolled_back = total_deleted,
        import_build_ids = ?import_build_ids,
        "pyramid import cancelled"
    );

    Ok(ImportCancelReport {
        state_row_existed,
        cache_rows_rolled_back: total_deleted,
    })
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
                    cache_entries: vec![make_imported_entry("source_extract", 0, 0, "L0a-extract")],
                },
                ImportNodeEntry {
                    node_id: "L0b".into(),
                    layer: 0,
                    source_path: Some(l0b_path.into()),
                    source_hash: Some(l0b_hash.into()),
                    source_size_bytes: Some(20),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry("source_extract", 1, 0, "L0b-extract")],
                },
                ImportNodeEntry {
                    node_id: "L0c".into(),
                    layer: 0,
                    source_path: Some(l0c_path.into()),
                    source_hash: Some(l0c_hash.into()),
                    source_size_bytes: Some(30),
                    derived_from: vec![],
                    cache_entries: vec![make_imported_entry("source_extract", 2, 0, "L0c-extract")],
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
        let err =
            populate_from_import(&conn, &manifest, "test-import", Path::new("/tmp")).unwrap_err();
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

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", bogus_hash);

        let report = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();

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
        let l1b_cache_key = compute_cache_key(
            "inputs:L1b-cluster",
            "prompt:L1b-cluster",
            "openrouter/test-1",
        );
        let l1b_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import' AND cache_key = ?1",
                [&l1b_cache_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            l1b_present, 0,
            "stale L1b cache entry should not be present"
        );
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
            "a.txt",
            &l0a_hash,
            "b.txt",
            &l0b_hash,
            "c.txt",
            "more-deadbeef",
        );

        let report = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();

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

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

        // First import: all 5 nodes are valid (no stale L0s).
        let r1 = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
        assert_eq!(r1.cache_entries_valid, 5);

        let row_count_after_first: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count_after_first, 5);

        // Second import: same manifest. `store_cache_if_absent` uses
        // INSERT ... ON CONFLICT DO NOTHING on the unique (slug, cache_key)
        // constraint, so re-importing produces no duplicate rows AND
        // does not overwrite anything already present.
        let r2 = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
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

    /// Regression guard for the spec's `INSERT OR IGNORE` mandate (see
    /// `docs/specs/cache-warming-and-import.md` "Idempotency" section
    /// ~line 341). If a user imports a pyramid, rerolls one of the cached
    /// steps locally (which writes a fresh row at the SAME
    /// content-addressable key with `force_fresh = 1`, a new `output_json`,
    /// and a supersession link to the archival row), and then — for any
    /// reason — re-runs the import (resume after crash, explicit retry),
    /// the re-import MUST NOT clobber the rerolled row. The reroll is the
    /// user's latest intention and we don't have the right to undo it.
    ///
    /// This test pins that contract directly: import, reroll, re-import,
    /// then assert that the row at the rerolled cache_key still carries
    /// the rerolled `output_json` and `force_fresh = 1`.
    #[test]
    fn test_re_import_preserves_local_reroll_force_fresh_row() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

        // First import: all 5 nodes land in the cache with
        // force_fresh = 0 and the imported output_json.
        let r1 = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
        assert_eq!(r1.cache_entries_valid, 5);

        // Simulate a local force-reroll on the L0a cache row. We reuse the
        // same inputs/prompt/model triple so the cache_key is identical to
        // the imported row (that's the whole point of "reroll at the same
        // content-addressable key"). Going through `supersede_cache_entry`
        // archives the imported row and writes a fresh row at the same
        // cache_key with `force_fresh = 1` and a supersession link.
        let l0a_cache_key = compute_cache_key(
            "inputs:L0a-extract",
            "prompt:L0a-extract",
            "openrouter/test-1",
        );

        let rerolled = CacheEntry {
            slug: "test-import".into(),
            build_id: "local-reroll".into(),
            step_name: "source_extract".into(),
            chunk_index: 0,
            depth: 0,
            cache_key: l0a_cache_key.clone(),
            inputs_hash: "inputs:L0a-extract".into(),
            prompt_hash: "prompt:L0a-extract".into(),
            model_id: "openrouter/test-1".into(),
            output_json: serde_json::json!({"content":"REROLLED","usage":{}}).to_string(),
            token_usage_json: Some("{}".into()),
            cost_usd: Some(0.005),
            latency_ms: Some(123),
            force_fresh: true,
            supersedes_cache_id: None,
            note: None,
        };
        db::supersede_cache_entry(&conn, "test-import", &l0a_cache_key, &rerolled).unwrap();

        // Sanity: the active row at the cache_key is now the rerolled one.
        let (active_output, active_force_fresh): (String, i64) = conn
            .query_row(
                "SELECT output_json, force_fresh FROM pyramid_step_cache
                 WHERE slug = 'test-import' AND cache_key = ?1",
                [&l0a_cache_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            active_output.contains("REROLLED"),
            "pre-check: rerolled row should be active after supersede"
        );
        assert_eq!(
            active_force_fresh, 1,
            "pre-check: rerolled row should have force_fresh = 1"
        );

        // Re-run the import. Under `store_cache_if_absent`, the re-import
        // must leave the rerolled row alone.
        let r2 = populate_from_import(&conn, &manifest, "test-import", dir.path()).unwrap();
        // Report still shows all 5 slots occupied — the importer counts
        // "entries now present" regardless of whether this call wrote them.
        assert_eq!(r2.cache_entries_valid, 5);

        // The row at the rerolled cache_key must still be the reroll, not
        // the re-imported copy.
        let (preserved_output, preserved_force_fresh, preserved_build_id): (String, i64, String) =
            conn.query_row(
                "SELECT output_json, force_fresh, build_id FROM pyramid_step_cache
                 WHERE slug = 'test-import' AND cache_key = ?1",
                [&l0a_cache_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(
            preserved_output.contains("REROLLED"),
            "local reroll output_json was clobbered by re-import: got {preserved_output}"
        );
        assert_eq!(
            preserved_force_fresh, 1,
            "local reroll force_fresh flag was clobbered by re-import"
        );
        assert_eq!(
            preserved_build_id, "local-reroll",
            "local reroll build_id was clobbered by re-import"
        );

        // The other 4 rows (non-rerolled) are still present and unchanged.
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'
                 AND cache_key NOT LIKE 'archived:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            row_count, 5,
            "expected 5 active rows (4 imported + 1 rerolled)"
        );

        // The archival row from the reroll is still present — the
        // supersession chain is intact.
        let archival_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'
                 AND cache_key LIKE 'archived:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            archival_count, 1,
            "expected 1 archival row from the reroll supersession"
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

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

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
        let state = db::load_import_state(&conn, "test-import")
            .unwrap()
            .unwrap();
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

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

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

    /// Wanderer regression: `cancel_pyramid_import` MUST roll back the
    /// cache rows the import wrote, not just delete the import state row.
    /// The spec's "Cleanup" section (~line 345) is explicit: "On explicit
    /// user cancel, the row is deleted along with any partially inserted
    /// cache entries and the target slug's DB rows."
    ///
    /// The first Phase 7 implementation only deleted the state row; this
    /// test pins the corrected behavior.
    #[test]
    fn test_cancel_pyramid_import_rolls_back_cache_rows() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

        // Land an import (5 cache rows + state row + dadbear contribution).
        let _ = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        let cache_count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cache_count_before, 5, "import should land 5 cache rows");

        // Cancel must roll back ALL 5 cache rows (since they were all
        // imported with build_id = 'import:wire:test-pyramid') AND the
        // import state row.
        let report = cancel_pyramid_import(&conn, "test-import").unwrap();
        assert_eq!(report.cache_rows_rolled_back, 5);
        assert!(report.state_row_existed);

        let cache_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            cache_count_after, 0,
            "cancel should have rolled back every imported cache row, got {cache_count_after}"
        );

        // The import state row is gone.
        let state = db::load_import_state(&conn, "test-import").unwrap();
        assert!(
            state.is_none(),
            "import state row should be deleted on cancel"
        );

        // Idempotent re-cancel: no state row, no imported cache rows,
        // returns a zero-counter report without erroring.
        let again = cancel_pyramid_import(&conn, "test-import").unwrap();
        assert_eq!(again.cache_rows_rolled_back, 0);
        assert!(!again.state_row_existed);
    }

    /// Wanderer regression: cancel must NOT touch cache rows that were
    /// written by local LLM calls or other (non-import) build paths.
    /// Locally-built rows have a build_id that does NOT start with
    /// `import:`, and the rollback's `LIKE 'import:%'` filter must
    /// preserve them.
    #[test]
    fn test_cancel_pyramid_import_preserves_non_import_cache_rows() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

        // Land an import.
        let _ = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        // Plant a separate "locally-built" cache row with a different
        // build_id (simulating a fresh chain executor write).
        let local_entry = CacheEntry {
            slug: "test-import".into(),
            build_id: "local-build-7".into(),
            step_name: "local_step".into(),
            chunk_index: 99,
            depth: 0,
            cache_key: "local-content-addressable-key-1".into(),
            inputs_hash: "local-inputs".into(),
            prompt_hash: "local-prompt".into(),
            model_id: "openrouter/local-model".into(),
            output_json: "{\"local\":true}".into(),
            token_usage_json: None,
            cost_usd: None,
            latency_ms: None,
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
        };
        db::store_cache(&conn, &local_entry).unwrap();

        let cache_count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cache_count_before, 6, "5 imported + 1 locally built");

        // Cancel.
        let report = cancel_pyramid_import(&conn, "test-import").unwrap();
        assert_eq!(
            report.cache_rows_rolled_back, 5,
            "5 imported rows rolled back"
        );

        // The locally-built row survives.
        let cache_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = 'test-import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            cache_count_after, 1,
            "exactly 1 row should survive (the local-build-7 row); got {cache_count_after}"
        );
        let surviving_build_id: String = conn
            .query_row(
                "SELECT build_id FROM pyramid_step_cache WHERE slug = 'test-import' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(surviving_build_id, "local-build-7");
    }

    /// Wanderer regression: re-running `import_pyramid` for the same slug
    /// (the spec's "resume" path) MUST NOT create duplicate active
    /// `dadbear_policy` contribution rows. The first import path lands an
    /// active contribution + a synced operational row; the second import
    /// must either supersede the first (preserving the contribution chain)
    /// or skip the contribution-create entirely. Creating a SECOND
    /// status='active' row for the same `(slug, schema_type)` violates
    /// the contributions table invariant and silently desynchronizes the
    /// audit trail from the operational row.
    ///
    /// This test pins the invariant directly: import twice, count active
    /// `dadbear_policy` rows for the slug. There must be exactly one.
    #[test]
    fn test_import_pyramid_resume_does_not_duplicate_dadbear_contributions() {
        let conn = mem_conn();
        let bus = make_bus();
        let dir = TempDir::new().unwrap();
        let l0a_path = dir.path().join("a.txt");
        let l0b_path = dir.path().join("b.txt");
        let l0c_path = dir.path().join("c.txt");
        let l0a_hash = write_and_hash(&l0a_path, "alpha");
        let l0b_hash = write_and_hash(&l0b_path, "beta");
        let l0c_hash = write_and_hash(&l0c_path, "gamma");

        let manifest =
            build_mixed_manifest("a.txt", &l0a_hash, "b.txt", &l0b_hash, "c.txt", &l0c_hash);

        // First import lands one active dadbear_policy contribution + one
        // synced pyramid_dadbear_config row.
        let _ = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        let active_after_first: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE slug = 'test-import' AND schema_type = 'dadbear_policy'
                   AND status = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active_after_first, 1,
            "first import should land exactly 1 active contribution"
        );

        // Second import (same slug + same wire_pyramid_id → resume path).
        let _ = import_pyramid(
            &conn,
            &bus,
            "wire:test-pyramid",
            "test-import",
            dir.path().to_str().unwrap(),
            &manifest,
        )
        .unwrap();

        let active_after_second: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE slug = 'test-import' AND schema_type = 'dadbear_policy'
                   AND status = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active_after_second, 1,
            "re-import (resume) must NOT create duplicate active dadbear_policy contributions; \
             expected 1, got {active_after_second}. The contribution path must check for an \
             existing active row and either supersede it or skip the create."
        );

        // The operational row must still exist with a contribution_id FK
        // that matches the (still-)active contribution.
        let dadbear_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_dadbear_config
                 WHERE slug = 'test-import' AND contribution_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dadbear_count, 1);
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

        enable_dadbear_via_contribution(&conn, &bus, "test-import", dir.path().to_str().unwrap())
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
        assert_ne!(
            row.2, "{}",
            "wire_native_metadata_json should not be the empty stub"
        );
        assert!(
            row.2.contains("\"maturity\""),
            "wire_native_metadata_json should carry the canonical maturity field"
        );
    }
}
