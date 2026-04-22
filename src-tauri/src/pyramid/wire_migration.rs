// pyramid/wire_migration.rs — Phase 5: on-disk prompts + chains → contributions.
//                              Phase 8: on-disk schema annotations → contributions.
//                              Phase 9: bundled contributions manifest → contributions.
//
// Canonical references:
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/wire-contribution-mapping.md
//     — "Migration from On-Disk Prompts and Schemas" section, "Seed Contributions Ship with the Binary"
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/generative-config-pattern.md
//     — "Seed Defaults Architecture" section (Phase 9 extension)
//   /Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-skills.md
//     — skills as contributions
//   /Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-actions.md
//     — chains as action contributions
//
// The Phase 5 build step. On first run (detected via a sentinel
// `_prompt_migration_marker` row in `pyramid_config_contributions`),
// this module walks `chains/prompts/**/*.md` and creates a `skill`
// contribution per file, then walks `chains/defaults/**/*.yaml` and
// creates a `custom_chain` action contribution per chain bundle.
//
// Every seed row is inserted with:
//   - `schema_type` = `"skill"` or `"custom_chain"`
//   - `source` = `"bundled"`
//   - `status` = `"active"`
//   - `maturity` = `canon` (not `draft` — bundled defaults ship as
//     production-ready; users refine via notes to downgrade/replace)
//   - `slug` = the normalized prompt path (for `prompt_cache` lookup)
//     or the chain's `id` field
//   - `triggering_note` = "Phase 5 migration from <origin_path>"
//
// **Idempotency** is guaranteed by two mechanisms:
//   1. A `_prompt_migration_marker` sentinel row. Subsequent runs
//      short-circuit on its presence.
//   2. Per-file check: if a row with the same `(schema_type, slug)`
//      already exists, skip (covers the case where the sentinel is
//      absent but rows exist from a failed previous run).
//
// **Failure handling**: a per-file failure (non-UTF-8 content,
// unreadable file, malformed YAML) is LOGGED and SKIPPED — the
// migration does NOT abort on a single bad file. At the end of the
// run, the sentinel is inserted if at least one file succeeded, so
// a rerun will retry the skipped files.
//
// **Archived prompts**: the `chains/prompts/**/_archived/` subtree is
// excluded — the spec's "Walk recursively, excluding `_archived/`"
// directive.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::pyramid::chain_registry;
use crate::pyramid::config_contributions::create_config_contribution_with_metadata;
use crate::pyramid::db::{ChainAssignmentYaml, ChainDefaultsYaml};
use crate::pyramid::prompt_cache::normalize_prompt_path;
use crate::pyramid::wire_native_metadata::{
    default_wire_native_metadata, WireContributionType, WireMaturity, WireNativeMetadata,
    WireRef, WireSectionOverride,
};

/// Migration-marker sentinel schema_type. Uses the same pattern as
/// Phase 4's DADBEAR migration (`_migration_marker`) but scoped to
/// Phase 5's prompt+chain migration so the two migrations are
/// independent — DADBEAR bootstrap won't block prompt migration and
/// vice versa.
const PROMPT_MIGRATION_MARKER: &str = "_prompt_migration_marker";

/// Report from a single migration run. Counts the number of prompts,
/// chains, and schema annotations successfully inserted, plus any
/// files that were skipped. Phase 8 added the
/// `schema_annotations_*` fields alongside the existing prompt/chain
/// counters. Phase 9 added the `bundled_*` fields for the bundled
/// contributions manifest walk.
#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    pub prompts_inserted: usize,
    pub prompts_skipped_already_present: usize,
    pub prompts_failed: usize,
    pub chains_inserted: usize,
    pub chains_skipped_already_present: usize,
    pub chains_failed: usize,
    /// Phase 8: schema annotation rows inserted this run.
    pub schema_annotations_inserted: usize,
    /// Phase 8: schema annotation rows skipped because a row with
    /// the same slug already existed (e.g. from an interrupted run).
    pub schema_annotations_skipped_already_present: usize,
    /// Phase 8: schema annotation rows that failed to insert.
    pub schema_annotations_failed: usize,
    /// Phase 9: bundled manifest rows inserted this run. Runs on
    /// every boot with per-entry INSERT OR IGNORE semantics — NOT
    /// gated by the `_prompt_migration_marker` sentinel so app
    /// upgrades can add new bundled entries without being blocked
    /// by a stale marker.
    pub bundled_inserted: usize,
    /// Phase 9: bundled manifest rows skipped because a row with the
    /// same `contribution_id` already existed (from a prior run, or
    /// because the user has already superseded the bundled default).
    pub bundled_skipped_already_present: usize,
    /// Phase 9: bundled manifest rows that failed to insert.
    pub bundled_failed: usize,
    pub marker_written: bool,
    pub ran: bool,
}

/// Migrate on-disk prompts and chains into `pyramid_config_contributions`.
///
/// Idempotent via the `_prompt_migration_marker` sentinel. Safe to
/// call on every process start — subsequent calls short-circuit.
///
/// **Arguments:**
/// - `conn`: SQLite connection. Phase 5 migration is not wrapped in
///   a transaction because a partial failure shouldn't abort the
///   whole migration — each file is independent and the sentinel is
///   only written at the end.
/// - `chains_dir`: the directory containing `prompts/` and
///   `defaults/` subdirectories (typically
///   `$RUNTIME_DATA_DIR/chains/`).
///
/// Returns a `MigrationReport` describing the run. Callers decide
/// whether a non-empty `failed` counter should surface a warning.
pub fn migrate_prompts_and_chains_to_contributions(
    conn: &Connection,
    chains_dir: &Path,
) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();

    // ── Phase 9: bundled contributions manifest walk ─────────────
    //
    // Runs on EVERY boot (not gated by the prompt-migration marker)
    // so app upgrades can land new bundled entries without being
    // blocked by a stale sentinel. Per-entry INSERT OR IGNORE
    // semantics make this safe — existing rows are skipped, new
    // rows are inserted.
    //
    // Rationale for running this outside the sentinel guard: if the
    // sentinel is present (prior run completed Phase 5 migration),
    // Phase 9 still needs to be able to ADD new bundled
    // contributions that didn't exist at Phase 5 time. The sentinel
    // only protects the disk-walk steps below, not the manifest walk.
    match walk_bundled_contributions_manifest(conn) {
        Ok(bundled_report) => {
            report.bundled_inserted = bundled_report.inserted;
            report.bundled_skipped_already_present = bundled_report.skipped_already_present;
            report.bundled_failed = bundled_report.failed;
            if bundled_report.inserted > 0 || bundled_report.skipped_already_present > 0 {
                debug!(
                    inserted = bundled_report.inserted,
                    skipped = bundled_report.skipped_already_present,
                    failed = bundled_report.failed,
                    "Phase 9 bundled contributions: walk complete"
                );
            }
        }
        Err(e) => {
            warn!(
                error = %e,
                "Phase 9 bundled contributions walk failed; Phase 5 disk-walk still runs"
            );
        }
    }

    // Idempotency guard: short-circuit the DISK walks (prompts +
    // chains + schema annotations) if the sentinel exists. The
    // Phase 9 bundled walk above is NOT gated by this sentinel.
    let marker_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_config_contributions
         WHERE schema_type = ?1
           AND source = 'migration'
           AND created_by = 'phase5_bootstrap'",
        rusqlite::params![PROMPT_MIGRATION_MARKER],
        |row| row.get(0),
    )?;
    if marker_exists > 0 {
        debug!(
            "Phase 5 prompt migration: sentinel row already present, skipping disk-walk migration"
        );
        return Ok(report);
    }

    report.ran = true;

    // ── Step 1: walk chains/prompts/**/*.md ─────────────────────────
    let prompts_root = chains_dir.join("prompts");
    if prompts_root.exists() && prompts_root.is_dir() {
        debug!(
            path = %prompts_root.display(),
            "Phase 5 prompt migration: walking prompts directory"
        );
        let mut prompt_files: Vec<(std::path::PathBuf, String)> = Vec::new();
        if let Err(e) = walk_prompt_files(&prompts_root, &prompts_root, &mut prompt_files) {
            warn!(
                error = %e,
                "Phase 5 prompt migration: walk failed, continuing with partial results"
            );
        }

        for (rel_path, body) in prompt_files {
            let rel_path_str = rel_path.to_string_lossy().to_string();

            // Per-file idempotency check: skip if a skill with this
            // slug already exists. This covers interrupted previous
            // runs where the sentinel wasn't written.
            let already: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pyramid_config_contributions
                     WHERE schema_type = 'skill' AND slug = ?1",
                    rusqlite::params![rel_path_str],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if already > 0 {
                report.prompts_skipped_already_present += 1;
                continue;
            }

            let metadata = build_skill_metadata(&rel_path_str);

            match create_config_contribution_with_metadata(
                conn,
                "skill",
                Some(&rel_path_str),
                &body,
                Some(&format!(
                    "Phase 5 migration from chains/prompts/{rel_path_str}"
                )),
                "bundled",
                Some("phase5_bootstrap"),
                "active",
                &metadata,
            ) {
                Ok(_id) => {
                    report.prompts_inserted += 1;
                }
                Err(e) => {
                    warn!(
                        prompt = %rel_path_str,
                        error = %e,
                        "Phase 5 prompt migration: failed to insert skill contribution"
                    );
                    report.prompts_failed += 1;
                }
            }
        }
    } else {
        debug!(
            path = %prompts_root.display(),
            "Phase 5 prompt migration: prompts directory missing, skipping prompts step"
        );
    }

    // ── Step 2: walk chains/defaults/**/*.yaml ─────────────────────
    let defaults_root = chains_dir.join("defaults");
    if defaults_root.exists() && defaults_root.is_dir() {
        debug!(
            path = %defaults_root.display(),
            "Phase 5 chain migration: walking defaults directory"
        );
        let mut chain_files: Vec<(std::path::PathBuf, String, String)> = Vec::new();
        if let Err(e) = walk_chain_files(&defaults_root, &mut chain_files) {
            warn!(
                error = %e,
                "Phase 5 chain migration: walk failed, continuing with partial results"
            );
        }

        for (path, chain_id, bundle_yaml) in chain_files {
            let rel_path_str = path
                .strip_prefix(&defaults_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            // Per-file idempotency check: the chain's unique slug is
            // its `id` field (e.g. "question-pipeline").
            let already: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pyramid_config_contributions
                     WHERE schema_type = 'custom_chain' AND slug = ?1",
                    rusqlite::params![chain_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if already > 0 {
                report.chains_skipped_already_present += 1;
                continue;
            }

            let metadata = build_custom_chain_metadata(&chain_id, &bundle_yaml);

            match create_config_contribution_with_metadata(
                conn,
                "custom_chain",
                Some(&chain_id),
                &bundle_yaml,
                Some(&format!(
                    "Phase 5 migration from chains/defaults/{rel_path_str}"
                )),
                "bundled",
                Some("phase5_bootstrap"),
                "active",
                &metadata,
            ) {
                Ok(_id) => {
                    report.chains_inserted += 1;
                }
                Err(e) => {
                    warn!(
                        chain = %chain_id,
                        error = %e,
                        "Phase 5 chain migration: failed to insert custom_chain contribution"
                    );
                    report.chains_failed += 1;
                }
            }
        }
    } else {
        debug!(
            path = %defaults_root.display(),
            "Phase 5 chain migration: defaults directory missing, skipping chains step"
        );
    }

    // ── Step 3: walk chains/schemas/**/*.schema.yaml ────────────────
    //
    // Phase 8 extension: schema annotation files live in
    // `chains/schemas/` and describe how the `YamlConfigRenderer`
    // should present each config type. On first run (same sentinel
    // marker as the prompts+chains walks), we walk this directory and
    // create one `schema_annotation` contribution per file. Per-file
    // idempotency check uses the annotation's `applies_to` / fallback
    // `schema_type` as the slug, so subsequent runs that find the row
    // already present skip it cleanly.
    let schemas_root = chains_dir.join("schemas");
    if schemas_root.exists() && schemas_root.is_dir() {
        debug!(
            path = %schemas_root.display(),
            "Phase 8 schema annotation migration: walking schemas directory"
        );
        let mut schema_files: Vec<(std::path::PathBuf, String)> = Vec::new();
        if let Err(e) = walk_schema_files(&schemas_root, &schemas_root, &mut schema_files) {
            warn!(
                error = %e,
                "Phase 8 schema annotation migration: walk failed, continuing with partial results"
            );
        }

        for (rel_path, body) in schema_files {
            let rel_path_str = rel_path.to_string_lossy().to_string();

            // Resolve the annotation slug (= applies_to / schema_type
            // / filename stem, in that order). Use this as the per-
            // row uniqueness key so re-runs skip cleanly.
            let slug = extract_annotation_slug(&body).unwrap_or_else(|| {
                rel_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown-annotation")
                    .trim_end_matches(".schema")
                    .to_string()
            });

            let already: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pyramid_config_contributions
                     WHERE schema_type = 'schema_annotation' AND slug = ?1",
                    rusqlite::params![slug],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if already > 0 {
                report.schema_annotations_skipped_already_present += 1;
                continue;
            }

            let metadata = build_schema_annotation_metadata(&slug);

            match create_config_contribution_with_metadata(
                conn,
                "schema_annotation",
                Some(&slug),
                &body,
                Some(&format!(
                    "Phase 8 migration from chains/schemas/{rel_path_str}"
                )),
                "bundled",
                Some("phase5_bootstrap"),
                "active",
                &metadata,
            ) {
                Ok(_id) => {
                    report.schema_annotations_inserted += 1;
                }
                Err(e) => {
                    warn!(
                        schema = %slug,
                        error = %e,
                        "Phase 8 schema annotation migration: failed to insert contribution"
                    );
                    report.schema_annotations_failed += 1;
                }
            }
        }
    } else {
        debug!(
            path = %schemas_root.display(),
            "Phase 8 schema annotation migration: schemas directory missing, skipping"
        );
    }

    // Schema DEFINITION migration (JSON Schema validation bodies) is
    // still Phase 9's scope — Phase 8 only touches annotation files.
    debug!("Phase 5 schema definition migration: deferred to Phase 9");

    // ── Step 4: write the sentinel row so subsequent runs short-circuit.
    // Only write if at least one file succeeded — otherwise a fully-
    // failed run would mark itself "done" and the next run would
    // skip entirely. Phase 8 adds schema annotations to the same
    // "succeeded" accounting so a first run that only ships schemas
    // still marks itself done.
    if report.prompts_inserted > 0
        || report.chains_inserted > 0
        || report.schema_annotations_inserted > 0
        || (report.prompts_skipped_already_present > 0
            && report.chains_skipped_already_present > 0)
    {
        let marker_id = uuid::Uuid::new_v4().to_string();
        crate::pyramid::config_contributions::write_contribution_envelope(
            conn,
            crate::pyramid::config_contributions::ContributionEnvelopeInput {
                contribution_id: marker_id,
                slug: None,
                schema_type: PROMPT_MIGRATION_MARKER.to_string(),
                body: String::new(),
                wire_native_metadata_json: None,
                supersedes_id: None,
                triggering_note: None,
                status: "active".to_string(),
                source: "migration".to_string(),
                wire_contribution_id: None,
                created_by: Some("phase5_bootstrap".to_string()),
                accepted_at: crate::pyramid::config_contributions::AcceptedAt::Now,
                needs_migration: None,
                write_mode: crate::pyramid::config_contributions::WriteMode::default(),
            },
            crate::pyramid::config_contributions::TransactionMode::OwnTransaction,
        )?;
        report.marker_written = true;
    }

    Ok(report)
}

/// Build a `WireNativeMetadata` for a migrated prompt (skill
/// contribution). Derives topic tags from the prompt's directory
/// structure and sets `maturity: Canon` since bundled seeds are
/// production-ready.
fn build_skill_metadata(rel_path: &str) -> WireNativeMetadata {
    let mut metadata = default_wire_native_metadata("skill", Some(rel_path));

    // Bundled prompts ship as canon — the user's starting point is
    // "this is battle-tested; refine with notes to change it".
    metadata.maturity = WireMaturity::Canon;

    // Infer topic tags from the path's first segment (e.g.
    // "conversation-episodic/forward.md" → topic "conversation-episodic").
    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() > 1 {
        let first = parts[0];
        if !metadata.topics.iter().any(|t| t == first) {
            metadata.topics.push(first.to_string());
        }
    }

    // Prompts are extraction/synthesis/review operations — add a
    // role tag based on the filename stem when it's a well-known
    // pattern. Avoids a hardcoded switch; the spec's mapping table
    // already provides the baseline.
    let filename_stem = parts
        .last()
        .and_then(|f| f.strip_suffix(".md"))
        .unwrap_or("");
    if !filename_stem.is_empty() {
        let role_tag = match filename_stem {
            s if s.contains("extract") => Some("extraction"),
            s if s.contains("merge") => Some("merge"),
            s if s.contains("heal") => Some("heal"),
            s if s.contains("cluster") => Some("cluster"),
            s if s.contains("assign") => Some("assign"),
            s if s.contains("synth") => Some("synthesis"),
            s if s.contains("review") => Some("review"),
            s if s.contains("forward") => Some("narrative:forward"),
            s if s.contains("reverse") => Some("narrative:reverse"),
            _ => None,
        };
        if let Some(tag) = role_tag {
            if !metadata.topics.iter().any(|t| t == tag) {
                metadata.topics.push(tag.to_string());
            }
        }
    }

    // Default price for a bundled skill is 1 credit (Wire minimum).
    metadata.price = Some(1);

    metadata
}

/// Build a `WireNativeMetadata` for a migrated chain
/// (custom_chain action contribution).
///
/// Best-effort: extracts `derived_from` entries from the chain's
/// prompt references so the chain is already linked to the skills
/// it consumes. The resolved references are path-based
/// (`doc: prompts/<rel>`) — at publish time, these resolve to
/// handle-paths once the underlying skills have been published.
fn build_custom_chain_metadata(chain_id: &str, bundle_yaml: &str) -> WireNativeMetadata {
    let mut metadata = default_wire_native_metadata("custom_chain", Some(chain_id));

    // Bundled chains ship as canon — the user's starting point is
    // "this is battle-tested; refine with notes to change it".
    metadata.maturity = WireMaturity::Canon;
    metadata.contribution_type = WireContributionType::Action;

    // Scan the chain YAML for `$prompts/...` references and add each
    // as a `derived_from` entry. Best-effort: YAML parsing failures
    // produce an empty derived_from list (still a valid chain
    // contribution, just without the source-chain economics).
    let prompt_refs = extract_prompt_refs(bundle_yaml);
    if !prompt_refs.is_empty() {
        let n = prompt_refs.len().min(28); // Max 28 sources per rotator rules
        let equal_weight = 1.0 / n as f64;
        for (i, prompt_path) in prompt_refs.into_iter().take(28).enumerate() {
            // Prefer a `doc:` reference for path-based lookup. At
            // publish time the path resolves to whatever skill
            // contribution was created for the prompt during
            // migration.
            metadata.derived_from.push(WireRef {
                ref_: None,
                doc: Some(prompt_path.clone()),
                corpus: None,
                weight: equal_weight,
                justification: format!("Step {} prompt (migrated seed)", i + 1),
            });
        }
    }

    // Chain bundles ship with an empty `sections` map; the Phase 5
    // spec's "Custom Chain Bundle Serialization" section describes a
    // future format where inline prompts become section entries.
    // For migrated chains the prompts are already separate skill
    // contributions (inserted above), so section decomposition is
    // not needed here.
    let _sections: std::collections::BTreeMap<String, WireSectionOverride> =
        std::collections::BTreeMap::new();

    // Default price for a bundled chain is 1 credit.
    metadata.price = Some(1);

    metadata
}

/// Extract `$prompts/...` references from a chain YAML body. Returns
/// the unique set, preserving first-occurrence order.
///
/// Simple line-by-line scan — avoids a full YAML parse because the
/// chain YAML format is stable enough that a regex-free scan is
/// sufficient, and a YAML parse failure would throw away valid
/// prompt references.
fn extract_prompt_refs(yaml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    const PREFIX: &str = "$prompts/";

    for line in yaml.lines() {
        let mut rest = line;
        while let Some(idx) = rest.find(PREFIX) {
            let tail = &rest[idx + PREFIX.len()..];
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(tail.len());
            let prompt_ref = &tail[..end];
            if !prompt_ref.is_empty() {
                let normalized = normalize_prompt_path(&format!("{PREFIX}{prompt_ref}"));
                if seen.insert(normalized.clone()) {
                    out.push(normalized);
                }
            }
            rest = &tail[end..];
        }
    }

    out
}

/// Walk `chains/prompts/` recursively and collect every `.md` file
/// that ISN'T inside an `_archived/` subdirectory. Accumulates
/// `(rel_path, body)` pairs in the output vector.
fn walk_prompt_files(
    root: &Path,
    cwd: &Path,
    out: &mut Vec<(std::path::PathBuf, String)>,
) -> Result<()> {
    let entries = match std::fs::read_dir(cwd) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %cwd.display(), error = %e, "failed to read prompts dir, skipping");
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to read dir entry, skipping");
                continue;
            }
        };
        let path = entry.path();

        // Skip `_archived/` subdirectories entirely.
        if path.is_dir() {
            let dir_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if dir_name == "_archived" {
                continue;
            }
            walk_prompt_files(root, &path, out)?;
            continue;
        }

        // Only .md files.
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let rel_path = match path.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read prompt file, skipping"
                );
                continue;
            }
        };
        out.push((rel_path, body));
    }
    Ok(())
}

/// Walk `chains/defaults/` and collect every `.yaml` / `.yml` file.
/// Accumulates `(path, chain_id, bundle_yaml)` tuples. The `chain_id`
/// is extracted from the YAML's top-level `id:` field (fallback: the
/// filename stem).
fn walk_chain_files(root: &Path, out: &mut Vec<(std::path::PathBuf, String, String)>) -> Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %root.display(), error = %e, "failed to read defaults dir");
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to read dir entry, skipping");
                continue;
            }
        };
        let path = entry.path();

        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }

        let bundle_yaml = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read chain file, skipping"
                );
                continue;
            }
        };

        // Extract the chain id from the YAML via a minimal scan —
        // avoids a full YAML parse so we don't reject unusual but
        // valid chain files.
        let chain_id = extract_chain_id(&bundle_yaml).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown-chain")
                .to_string()
        });

        out.push((path, chain_id, bundle_yaml));
    }
    Ok(())
}

/// Extract the top-level `id:` value from a chain YAML via a simple
/// scan. Avoids a full YAML parse because some chain YAMLs use
/// anchors/references that would require a heavier parse; the `id:`
/// convention is stable enough that a line-prefix match works.
fn extract_chain_id(yaml: &str) -> Option<String> {
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("id:") {
            let value = rest.trim();
            // Strip surrounding quotes if present.
            let value = value
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Walk `chains/schemas/` recursively and collect every `.schema.yaml`
/// or `.schema.yml` file. Accumulates `(rel_path, body)` pairs in the
/// output vector. Used by the Phase 8 schema annotation migration.
fn walk_schema_files(
    root: &Path,
    cwd: &Path,
    out: &mut Vec<(std::path::PathBuf, String)>,
) -> Result<()> {
    let entries = match std::fs::read_dir(cwd) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %cwd.display(), error = %e, "failed to read schemas dir, skipping");
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to read dir entry, skipping");
                continue;
            }
        };
        let path = entry.path();

        if path.is_dir() {
            // Recurse into subdirectories; excluded `_archived/` dirs
            // for parity with the prompt walker.
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if dir_name == "_archived" {
                continue;
            }
            walk_schema_files(root, &path, out)?;
            continue;
        }

        // Only `.schema.yaml` / `.schema.yml` files.
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(filename.ends_with(".schema.yaml") || filename.ends_with(".schema.yml")) {
            continue;
        }

        let rel_path = match path.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read schema annotation file, skipping"
                );
                continue;
            }
        };
        out.push((rel_path, body));
    }
    Ok(())
}

/// Extract a slug for a schema annotation file. Prefers the
/// `applies_to` field, falls back to `schema_type`. Both live at the
/// top level of the annotation YAML. This is the same keying the
/// Phase 8 renderer uses to look up annotations at runtime.
fn extract_annotation_slug(yaml: &str) -> Option<String> {
    let mut applies_to: Option<String> = None;
    let mut schema_type: Option<String> = None;
    for line in yaml.lines() {
        // Only top-level scalars — indented lines belong to nested
        // structures (e.g. fields.<name>.widget).
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("applies_to:") {
            let value = rest
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .to_string();
            if !value.is_empty() {
                applies_to = Some(value);
            }
        } else if let Some(rest) = trimmed.strip_prefix("schema_type:") {
            let value = rest
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .to_string();
            if !value.is_empty() {
                schema_type = Some(value);
            }
        }
    }
    applies_to.or(schema_type)
}

/// Build a `WireNativeMetadata` for a migrated schema annotation.
/// The Wire mapping table routes `schema_annotation` →
/// `WireContributionType::Template` with `applies_to: ui_annotation`,
/// so canonical metadata lands as a Template contribution with
/// `maturity: Canon` (bundled seed) and a topic tag for the target
/// config type.
fn build_schema_annotation_metadata(slug: &str) -> WireNativeMetadata {
    let mut metadata = default_wire_native_metadata("schema_annotation", Some(slug));
    metadata.contribution_type = WireContributionType::Template;
    metadata.maturity = WireMaturity::Canon;
    // Topic tags let Wire discovery find these by the config type
    // they describe (e.g. "chain_step_config"). The default helper
    // already adds the slug; add a stable "ui_annotation" tag too so
    // browsers can filter the Wire for all annotation templates.
    if !metadata.topics.iter().any(|t| t == "ui_annotation") {
        metadata.topics.push("ui_annotation".to_string());
    }
    // Annotation files are small; price stays at the Wire minimum.
    metadata.price = Some(1);
    metadata
}

// ── Phase 9: Bundled contributions manifest ─────────────────────────────────

/// Bundled contributions manifest shipped inside the binary at
/// `src-tauri/assets/bundled_contributions.json`. Phase 9 loads this on
/// first run (or after an app upgrade that adds new bundled entries)
/// and inserts every contribution with `source = 'bundled'`,
/// `status = 'active'`, `created_by = 'bootstrap'`, and the explicit
/// `contribution_id` from the manifest (NOT an auto-generated UUID).
///
/// The manifest format is documented in `wire-contribution-mapping.md`
/// → "Bundle manifest" and in `generative-config-pattern.md` →
/// "Seed Defaults Architecture".
#[derive(Debug, Deserialize)]
pub struct BundledContributionsManifest {
    pub manifest_version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub generated_at: String,
    pub contributions: Vec<BundledContributionEntry>,
}

/// A single entry inside the bundled contributions manifest. Maps 1:1
/// to a `pyramid_config_contributions` row, with the explicit
/// `contribution_id` and `yaml_content` preserved across app upgrades.
///
/// `topics_extra` is a Phase 9 convenience field — the canonical Wire
/// Native metadata is computed at migration time via
/// `build_bundled_metadata()` (which applies the mapping table + slug +
/// these extra topics), so the manifest itself doesn't need to inline
/// the full canonical metadata object. Keeps the manifest compact and
/// lets Phase 5's mapping table stay the single source of truth for
/// per-schema-type defaults.
#[derive(Debug, Deserialize)]
pub struct BundledContributionEntry {
    pub contribution_id: String,
    pub schema_type: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub yaml_content: String,
    pub triggering_note: String,
    /// Extra topic tags (beyond the mapping-table defaults) added to
    /// the canonical metadata at migration time. Used for generation
    /// skills to tag them with `generation` + the target schema_type.
    #[serde(default)]
    pub topics_extra: Vec<String>,
    /// For `schema_definition` entries: the target config schema_type
    /// the JSON schema applies to. Added as a topic tag so the schema
    /// registry's lookup-by-target-type scan can find it.
    #[serde(default)]
    pub applies_to: Option<String>,
}

/// Load the bundled contributions manifest from the binary assets.
/// Single source of truth: the `include_str!` path. Parse failures
/// abort the migration (the bundled manifest ships with the binary
/// so a parse failure is a bug in the release, not a user-facing
/// error).
pub fn load_bundled_manifest() -> Result<BundledContributionsManifest> {
    // `include_str!` paths are relative to the current Rust source file
    // (src-tauri/src/pyramid/wire_migration.rs). The assets directory
    // lives at src-tauri/assets/, so we go up three levels.
    const MANIFEST_JSON: &str =
        include_str!("../../assets/bundled_contributions.json");
    let manifest: BundledContributionsManifest = serde_json::from_str(MANIFEST_JSON)
        .map_err(|e| anyhow::anyhow!("failed to parse bundled contributions manifest: {e}"))?;
    Ok(manifest)
}

/// Build the canonical `WireNativeMetadata` for a bundled contribution.
/// Starts from the Phase 5 mapping-table default and overrides:
///   * `maturity` = `Canon` (bundled seeds ship as battle-tested)
///   * `price` = 1 (Wire minimum — bundled defaults are free starting
///                  points; users can edit the price at publish time)
///   * `topics` = mapping defaults + the entry's `topics_extra` +
///                the `applies_to` target (if present)
///
/// Keeps the Phase 5 mapping table as the single source of truth for
/// per-schema-type default tags; Phase 9 just adds per-entry extras.
fn build_bundled_metadata(entry: &BundledContributionEntry) -> WireNativeMetadata {
    let mut metadata = default_wire_native_metadata(
        &entry.schema_type,
        entry.slug.as_deref(),
    );
    metadata.maturity = WireMaturity::Canon;
    metadata.price = Some(1);

    for extra in &entry.topics_extra {
        if !metadata.topics.iter().any(|t| t == extra) {
            metadata.topics.push(extra.clone());
        }
    }

    if let Some(applies_to) = &entry.applies_to {
        if !metadata.topics.iter().any(|t| t == applies_to) {
            metadata.topics.push(applies_to.clone());
        }
    }

    metadata
}

/// Insert a bundled contribution with an EXPLICIT contribution_id. The
/// standard `create_config_contribution_with_metadata` auto-generates a
/// UUID — Phase 9 bundled contributions need their IDs to be stable
/// across app upgrades so the schema registry can reference them by
/// durable handle.
///
/// Skip-on-conflict semantics: if a row with this `contribution_id`
/// already exists (second run, app upgrade, or user-superseded
/// default), the insert is a no-op. The caller detects this by
/// inspecting the return value (`Ok(true)` = inserted, `Ok(false)` =
/// skipped).
///
/// **Do NOT use INSERT OR REPLACE here.** A user who has superseded a
/// bundled default with their own refinement would lose their version
/// on the next app launch. Skip-on-conflict is the correct behavior.
///
/// Phase 0a-1 commit 4: routed through `write_contribution_envelope`
/// via a pre-check on `contribution_id` uniqueness. The envelope
/// writer is the single production INSERT site; this function's
/// role is to decide whether to invoke it at all. Pre-check +
/// insert is not atomic, but the boot path is single-threaded, and
/// the prior `INSERT OR IGNORE` was only stronger w.r.t. the
/// `contribution_id` uniqueness the pre-check already enforces.
/// Test-only re-export of the private `insert_bundled_contribution`
/// so `pyramid::config_contributions::tests` can exercise the
/// bundled-vs-user-active pre-check (Phase 0a-1 commit 5, wanderer
/// note 2). Not used by production code.
#[cfg(test)]
pub(crate) fn insert_bundled_contribution_for_test(
    conn: &Connection,
    entry: &BundledContributionEntry,
    metadata: &WireNativeMetadata,
) -> Result<bool> {
    insert_bundled_contribution(conn, entry, metadata)
}

fn insert_bundled_contribution(
    conn: &Connection,
    entry: &BundledContributionEntry,
    metadata: &WireNativeMetadata,
) -> Result<bool> {
    // Phase 0a-1 commit 5: bundled-vs-user-active pre-check (wanderer
    // note 2). Once `uq_config_contrib_active` is enforced, a bundled
    // INSERT for `(schema_type, slug)` where a user-authored active
    // row already exists raises SQLITE_CONSTRAINT before
    // `consolidate_bundled_versions` can run. Detect that case here
    // and skip the bundled insert — the user's supersession implicitly
    // retracts the bundled default.
    let user_active_exists: bool = conn
        .query_row(
            "SELECT 1 FROM pyramid_config_contributions
             WHERE schema_type = ?1
               AND COALESCE(slug, '__global__') = COALESCE(?2, '__global__')
               AND status = 'active'
               AND source != 'bundled'",
            rusqlite::params![entry.schema_type, entry.slug],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if user_active_exists {
        return Ok(false);
    }

    // Pre-check: if contribution_id already exists, mirror the prior
    // `INSERT OR IGNORE` no-op behavior.
    let existing: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![entry.contribution_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(false);
    }

    // Phase 0a-1 commit 5 wanderer fix: bundled v-bump path. Before
    // `uq_config_contrib_active` landed, a v2 manifest entry could be
    // inserted alongside a still-active v1; `consolidate_bundled_versions`
    // (run after the manifest walk) then picked the winner. The unique
    // index now makes that double-active impossible, so a v-bump INSERT
    // fails with SupersessionConflict and the new bundled default is
    // silently lost on upgrade. Pre-emptively supersede any OTHER active
    // bundled row for the same (schema_type, slug) here so the INSERT
    // below respects the index. The post-walk `consolidate_bundled_versions`
    // pass remains as a belt-and-suspenders for any residual state
    // (pre-commit-5 DBs that dedup missed, hypothetical races, etc).
    let prior_bundled_id: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = ?1
               AND COALESCE(slug, '__global__') = COALESCE(?2, '__global__')
               AND status = 'active'
               AND source = 'bundled'
               AND contribution_id != ?3",
            rusqlite::params![entry.schema_type, entry.slug, entry.contribution_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(ref prior_id) = prior_bundled_id {
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1",
            rusqlite::params![prior_id],
        )?;
        debug!(
            schema_type = %entry.schema_type,
            slug = ?entry.slug,
            prior_bundled = %prior_id,
            new_bundled = %entry.contribution_id,
            "insert_bundled_contribution: v-bump detected, prior bundled row superseded ahead of INSERT"
        );
    }

    let metadata_json = metadata
        .to_json()
        .map_err(|e| anyhow::anyhow!("failed to serialize wire_native_metadata: {e}"))?;

    // Supersedes pointer: if this is a v-bump, record the prior
    // bundled row as the parent in the supersession chain.
    let supersedes_ptr = prior_bundled_id.clone();
    match crate::pyramid::config_contributions::write_contribution_envelope(
        conn,
        crate::pyramid::config_contributions::ContributionEnvelopeInput {
            contribution_id: entry.contribution_id.clone(),
            slug: entry.slug.clone(),
            schema_type: entry.schema_type.clone(),
            body: entry.yaml_content.clone(),
            wire_native_metadata_json: Some(metadata_json),
            supersedes_id: supersedes_ptr,
            triggering_note: Some(entry.triggering_note.clone()),
            status: "active".to_string(),
            source: "bundled".to_string(),
            wire_contribution_id: None,
            created_by: Some("bootstrap".to_string()),
            accepted_at: crate::pyramid::config_contributions::AcceptedAt::Now,
            needs_migration: None,
            // Phase 0a-1 commit 5: bundled loader MUST skip-on-fail
            // for shape-validation failures so a malformed bundled
            // row cannot brick the install (§2.11 Root 22 / A-C4).
            write_mode: crate::pyramid::config_contributions::WriteMode::BundledBootSkipOnFail,
        },
        crate::pyramid::config_contributions::TransactionMode::JoinAmbient,
    ) {
        Ok(_) => {
            // Phase 0a-1 commit 5 wanderer fix: back-fill the prior
            // bundled row's forward pointer after the INSERT lands.
            if let Some(ref prior_id) = prior_bundled_id {
                conn.execute(
                    "UPDATE pyramid_config_contributions
                     SET superseded_by_id = ?1
                     WHERE contribution_id = ?2",
                    rusqlite::params![entry.contribution_id, prior_id],
                )?;
            }
            Ok(true)
        }
        Err(crate::pyramid::config_contributions::ContributionWriterError::BundledValidationSkipped {
            contribution_id,
            schema_type,
        }) => {
            // §2.11 skip-on-fail path. The warn! emission inside the
            // writer already includes chronicle event name + details;
            // caller reports the skip via Ok(false) and continues.
            //
            // Phase 0a-1 commit 5 wanderer fix: if we pre-emptively
            // superseded a prior bundled row (v-bump path) and then
            // the new row failed validation, restore the prior to
            // active so the node is not left with zero active rows
            // for this (schema_type, slug).
            if let Some(ref prior_id) = prior_bundled_id {
                conn.execute(
                    "UPDATE pyramid_config_contributions
                     SET status = 'active'
                     WHERE contribution_id = ?1",
                    rusqlite::params![prior_id],
                )?;
                warn!(
                    prior_bundled = %prior_id,
                    new_bundled = %contribution_id,
                    "bundled v-bump aborted due to validation failure; prior bundled row restored to active"
                );
            }
            debug!(
                contribution_id = %contribution_id,
                schema_type = %schema_type,
                "bundled contribution skipped by writer: shape validation failed"
            );
            Ok(false)
        }
        Err(e) => {
            // Phase 0a-1 commit 5 wanderer fix: same restore-on-failure
            // handling for non-validation INSERT errors (e.g. disk
            // full, unexpected constraint). Never leave the DB with
            // zero active rows for this key.
            if let Some(ref prior_id) = prior_bundled_id {
                if let Err(restore_err) = conn.execute(
                    "UPDATE pyramid_config_contributions
                     SET status = 'active'
                     WHERE contribution_id = ?1",
                    rusqlite::params![prior_id],
                ) {
                    warn!(
                        prior_bundled = %prior_id,
                        error = %restore_err,
                        "bundled v-bump aborted and prior-row restore also failed"
                    );
                }
            }
            Err(anyhow::anyhow!("write_contribution_envelope: {e}"))
        }
    }
}

/// After the bundled manifest walk, ensure at most one `active` row
/// exists per `(schema_type, slug)` pair. Called from
/// `walk_bundled_contributions_manifest` to handle the version-bump
/// scenario where an upgraded bundled entry (e.g. `-v2`) lands while
/// the prior version (e.g. `-v1`) is still sitting active in the DB.
///
/// Rules:
/// - If multiple rows have `status='active' AND superseded_by_id IS
///   NULL` for the same `(schema_type, slug)`, keep the one whose row
///   is chosen by the canonical `load_active_config_contribution`
///   ordering (ORDER BY created_at DESC, id DESC LIMIT 1) unless a
///   NON-bundled row is present — user refinements always win.
/// - Mark the losers as `status='superseded'` pointing at the winner.
///
/// This is idempotent: once a pair has one active row, subsequent
/// boots short-circuit with no changes.
fn consolidate_bundled_versions(conn: &Connection) -> Result<()> {
    // Find every (schema_type, slug_key) pair with more than one
    // active row. `slug_key` is a synthetic column that treats NULL
    // slugs as their own bucket (GROUP BY can't distinguish NULL
    // values otherwise).
    let mut stmt = conn.prepare(
        "SELECT schema_type, COALESCE(slug, '__NULL__') AS slug_key
         FROM pyramid_config_contributions
         WHERE status = 'active' AND superseded_by_id IS NULL
         GROUP BY schema_type, slug_key
         HAVING COUNT(*) > 1",
    )?;
    let dupes: Vec<(String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (schema_type, slug_key) in dupes {
        let slug_opt: Option<&str> = if slug_key == "__NULL__" {
            None
        } else {
            Some(slug_key.as_str())
        };

        // Load every active row in this pair, annotated with source
        // so we can prefer user refinements over bundled seeds.
        let rows = if let Some(slug) = slug_opt {
            let sql = "SELECT contribution_id, source, created_at, id
                       FROM pyramid_config_contributions
                       WHERE status = 'active' AND superseded_by_id IS NULL
                         AND schema_type = ?1 AND slug = ?2
                       ORDER BY created_at DESC, id DESC";
            conn.prepare(sql)?
                .query_map(rusqlite::params![schema_type, slug], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let sql = "SELECT contribution_id, source, created_at, id
                       FROM pyramid_config_contributions
                       WHERE status = 'active' AND superseded_by_id IS NULL
                         AND schema_type = ?1 AND slug IS NULL
                       ORDER BY created_at DESC, id DESC";
            conn.prepare(sql)?
                .query_map(rusqlite::params![schema_type], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        if rows.len() < 2 {
            continue;
        }

        // Pick the winner. User refinements (source != 'bundled')
        // always beat bundled seeds. Among bundled rows, the
        // newest (first in DESC order) wins. Among user rows, the
        // newest wins.
        let user_winner = rows
            .iter()
            .find(|(_, source, _, _)| source != "bundled")
            .cloned();
        let winner = user_winner.unwrap_or_else(|| rows[0].clone());
        let winner_id = winner.0;

        // Pick the loser whose supersession chain link gets attached
        // to the winner's `supersedes_id` — by convention, the most
        // recent predecessor. With only 2 rows (the common case), the
        // one loser is both "the loser" and "the most recent
        // predecessor," so either ordering picks it. With 3+ rows,
        // we link the NEWEST loser (rows are DESC-ordered) because
        // that's the most recent thing the winner is superseding.
        let newest_loser_id: Option<String> = rows
            .iter()
            .find(|(id, _, _, _)| id != &winner_id)
            .map(|(id, _, _, _)| id.clone());

        // Only update winner.supersedes_id if it's currently NULL —
        // a user refinement that the operator wrote via
        // `supersede_config_contribution` already has a valid
        // supersedes_id pointing at the row it superseded. Don't
        // overwrite that.
        if let Some(ref newest_loser) = newest_loser_id {
            conn.execute(
                "UPDATE pyramid_config_contributions
                 SET supersedes_id = ?1
                 WHERE contribution_id = ?2
                   AND supersedes_id IS NULL",
                rusqlite::params![newest_loser, winner_id],
            )?;
        }

        for (loser_id, _, _, _) in rows.iter().filter(|(id, _, _, _)| id != &winner_id) {
            conn.execute(
                "UPDATE pyramid_config_contributions
                 SET status = 'superseded', superseded_by_id = ?1
                 WHERE contribution_id = ?2",
                rusqlite::params![winner_id, loser_id],
            )?;
            debug!(
                schema_type = %schema_type,
                loser = %loser_id,
                winner = %winner_id,
                "consolidate_bundled_versions: marked duplicate-active row superseded"
            );
        }
    }

    Ok(())
}

/// Rewrite any pre-WS1a `compute_participation_policy` contribution
/// (user refinement — source != 'bundled') from its legacy 5/6-field
/// shape to the canonical 10-field shape, preserving the operator's
/// effective intent.
///
/// The shipped bundled v2 default already has all 10 fields explicit.
/// This function targets USER refinements that were written against
/// the pre-WS1a struct and therefore have the 5 new fields absent. On
/// read, absent fields deserialize as `None` and project from mode —
/// which for `mode: hybrid` (the common pre-WS1a setting) projects
/// every new market to `true`. That's the silent-flip we want to
/// prevent.
///
/// Resolution: parse the legacy YAML via the 10-field struct (missing
/// fields → None), call `effective_booleans()` to resolve the
/// operator's intent, then emit a 10-field YAML with every boolean
/// explicit. The resulting YAML round-trips to the same
/// `EffectiveParticipationPolicy` as before — so nothing about the
/// operator's node behavior changes at the moment the rewrite runs.
/// But the EXPLICIT 10-field storage means any future WS1a-aware code
/// that happens to add new booleans won't silently flip this
/// operator's markets on.
///
/// Idempotent: a YAML that already has all 10 booleans explicit
/// produces the same YAML output, so the UPDATE is a no-op.
/// Contributions with source='bundled' are skipped entirely — they're
/// already canonical via `bundled_contributions.json`.
#[allow(deprecated)] // reads deprecated ComputeParticipationPolicy fields for serde-compat pass-through; Wave 5 removes.
fn canonicalize_legacy_participation_policy(conn: &Connection) -> Result<()> {
    use crate::pyramid::local_mode::ComputeParticipationPolicy;

    let mut stmt = conn.prepare(
        "SELECT contribution_id, yaml_content
         FROM pyramid_config_contributions
         WHERE schema_type = 'compute_participation_policy'
           AND status = 'active'
           AND source != 'bundled'",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (contribution_id, yaml_content) in rows {
        let parsed: ComputeParticipationPolicy = match serde_yaml::from_str(&yaml_content) {
            Ok(p) => p,
            Err(e) => {
                // Malformed YAML shouldn't block boot — log and skip.
                warn!(
                    contribution_id = %contribution_id,
                    error = %e,
                    "canonicalize_legacy_participation_policy: parse failed, skipping row"
                );
                continue;
            }
        };

        // Short-circuit the no-op case: if every projectable boolean
        // is already Some(_), the YAML is canonical. No rewrite, no
        // UPDATE, no DB write.
        let already_canonical = parsed.allow_fleet_dispatch.is_some()
            && parsed.allow_fleet_serving.is_some()
            && parsed.allow_market_dispatch.is_some()
            && parsed.allow_market_visibility.is_some()
            && parsed.allow_storage_pulling.is_some()
            && parsed.allow_storage_hosting.is_some()
            && parsed.allow_relay_usage.is_some()
            && parsed.allow_relay_serving.is_some();
        if already_canonical {
            continue;
        }

        // Resolve the operator's intent via the same projection the
        // read path uses. Then emit a 10-field policy with every
        // boolean explicit. The effective meaning is unchanged.
        let eff = parsed.effective_booleans();
        let canonical = ComputeParticipationPolicy {
            schema_type: "compute_participation_policy".to_string(),
            mode: parsed.mode,
            allow_fleet_dispatch: Some(eff.allow_fleet_dispatch),
            allow_fleet_serving: Some(eff.allow_fleet_serving),
            allow_market_dispatch: Some(eff.allow_market_dispatch),
            allow_market_visibility: Some(eff.allow_market_visibility),
            allow_storage_pulling: Some(eff.allow_storage_pulling),
            allow_storage_hosting: Some(eff.allow_storage_hosting),
            allow_relay_usage: Some(eff.allow_relay_usage),
            allow_relay_serving: Some(eff.allow_relay_serving),
            allow_serving_while_degraded: eff.allow_serving_while_degraded,
            // Phase 3 dispatch wall-clock — carry through unchanged.
            // The two retired knobs (`market_dispatch_threshold_queue_depth`,
            // `market_dispatch_eager`) were deleted in Wave 5; legacy
            // rows that still carry them are silently absorbed by the
            // struct deserializer and then dropped on canonical rewrite.
            market_dispatch_max_wait_ms: eff.market_dispatch_max_wait_ms,
            // Rev 2.1.1 saturation-backoff patience — carry through.
            market_saturation_patience_secs: eff.market_saturation_patience_secs,
        };
        let new_yaml = match serde_yaml::to_string(&canonical) {
            Ok(y) => y,
            Err(e) => {
                warn!(
                    contribution_id = %contribution_id,
                    error = %e,
                    "canonicalize_legacy_participation_policy: serialize failed, skipping row"
                );
                continue;
            }
        };

        conn.execute(
            "UPDATE pyramid_config_contributions
             SET yaml_content = ?1,
                 triggering_note = COALESCE(triggering_note, '') ||
                   CASE
                     WHEN triggering_note IS NULL OR triggering_note = ''
                     THEN 'canonicalized to 10-field DD-I shape at upgrade'
                     ELSE ' | canonicalized to 10-field DD-I shape at upgrade'
                   END
             WHERE contribution_id = ?2",
            rusqlite::params![new_yaml, contribution_id],
        )?;

        debug!(
            contribution_id = %contribution_id,
            "canonicalize_legacy_participation_policy: rewrote legacy YAML to 10-field DD-I canonical"
        );
    }

    Ok(())
}

/// Report from the Phase 9 bundled contributions walk. Counts how many
/// bundled entries were freshly inserted, skipped (because a row with
/// the same `contribution_id` already existed — either from a prior
/// run or a user supersession), and failed.
#[derive(Debug, Default, Clone)]
pub struct BundledMigrationReport {
    pub inserted: usize,
    pub skipped_already_present: usize,
    pub failed: usize,
}

/// Walk the bundled contributions manifest and insert each entry as a
/// contribution via `insert_bundled_contribution`. Idempotent — the
/// per-entry INSERT OR IGNORE semantics mean re-running this function
/// (or running it after an app upgrade) is safe.
///
/// Unlike the Phase 5 prompt/chain walk which uses a whole-run sentinel,
/// Phase 9 relies entirely on per-entry idempotency. Rationale:
/// bundled contributions may be ADDED on app upgrades (new schema
/// types, new defaults), and a whole-run sentinel would prevent those
/// new entries from ever landing. Per-entry `INSERT OR IGNORE` is
/// exactly the right primitive.
///
/// **Edge case:** if a user has superseded a bundled default with their
/// own refinement, the bundled contribution_id still exists in the
/// table (marked `superseded`, `superseded_by_id = user's refinement`).
/// INSERT OR IGNORE therefore skips the bundled row on re-run, which
/// correctly preserves the user's work. The user's refinement remains
/// the active version for that (schema_type, slug) pair.
pub fn walk_bundled_contributions_manifest(
    conn: &Connection,
) -> Result<BundledMigrationReport> {
    let mut report = BundledMigrationReport::default();

    let manifest = match load_bundled_manifest() {
        Ok(m) => m,
        Err(e) => {
            warn!(
                error = %e,
                "Phase 9 bundled manifest load failed; skipping bundled contributions"
            );
            report.failed += 1;
            return Ok(report);
        }
    };

    debug!(
        manifest_version = manifest.manifest_version,
        app_version = %manifest.app_version,
        contributions = manifest.contributions.len(),
        "Phase 9 bundled contributions: processing manifest"
    );

    for entry in &manifest.contributions {
        let metadata = build_bundled_metadata(entry);
        match insert_bundled_contribution(conn, entry, &metadata) {
            Ok(true) => {
                report.inserted += 1;
            }
            Ok(false) => {
                report.skipped_already_present += 1;
            }
            Err(e) => {
                warn!(
                    contribution_id = %entry.contribution_id,
                    error = %e,
                    "Phase 9 bundled contributions: failed to insert entry"
                );
                report.failed += 1;
            }
        }
    }

    // ── Bundled version bump consolidation ───────────────────────────
    //
    // Bundled contributions are versioned by suffix (`-v1`, `-v2`, …).
    // On an app upgrade that bumps the bundled version, both rows land
    // in the DB with `status='active'` and `superseded_by_id IS NULL`
    // because `INSERT OR IGNORE` only dedupes by contribution_id — it
    // does NOT understand that v2 logically supersedes v1.
    //
    // `load_active_config_contribution` orders by `created_at DESC`
    // and returns `LIMIT 1`, so v2 wins the read race. But the old v1
    // row sits phantom-active in the table, and worse, a pre-upgrade
    // USER refinement (active row supersedes=v1) has `created_at`
    // earlier than v2's fresh-insertion timestamp — v2 silently
    // overrides the user's explicit choice.
    //
    // Fix: after the manifest walk, consolidate each (schema_type,
    // slug) pair so that at most ONE bundled row is active. The
    // oldest-v2-wins-ordering bug on user refinements is also fixed —
    // a user's active refinement (source != 'bundled', or supersedes
    // a bundled row) marks any newer bundled default as superseded so
    // the read path returns the user's choice.
    if let Err(e) = consolidate_bundled_versions(conn) {
        warn!(
            error = %e,
            "bundled version consolidation failed; duplicate-active rows may exist"
        );
    }

    // ── Legacy 6-field participation policy canonicalization ─────────
    //
    // Pre-WS1a nodes had `ComputeParticipationPolicy` with 5 boolean
    // fields + schema_type + mode. Post-WS1a the canonical shape has 8
    // Option<bool> booleans + allow_serving_while_degraded + mode.
    //
    // A pre-WS1a operator who SUPERSEDED the bundled default with their
    // own refinement has a user row whose YAML stores only the 5 old
    // booleans. On read, the 5 new fields (allow_market_dispatch,
    // allow_storage_*, allow_relay_*) deserialize as None → project
    // from mode. For an operator on `mode: hybrid`, that means every
    // market silently switches ON at upgrade time — surprising behavior
    // the operator never opted into.
    //
    // Fix: detect legacy-shape user refinements and rewrite them in
    // place with explicit 10-field values. The effective meaning is
    // preserved by calling `effective_booleans()` on the parsed struct
    // and storing each resolved value as an explicit Some(v). Fresh
    // installs and already-10-field contributions are no-ops.
    //
    // This runs only AFTER consolidation so we rewrite the canonical
    // active row, never a phantom.
    if let Err(e) = canonicalize_legacy_participation_policy(conn) {
        warn!(
            error = %e,
            "legacy participation policy canonicalization failed; \
             pre-WS1a user refinements may silently project new markets ON"
        );
    }

    // ── Boot-time sync: hydrate chain_defaults operational table ─────
    //
    // The `insert_bundled_contribution` path above writes to
    // `pyramid_config_contributions` but does NOT call
    // `sync_config_to_operational` (it can't — no BuildEventBus is
    // available at migration time). For most schema types this is fine
    // because `init_pyramid_db` legacy-seeds their operational tables.
    // But `pyramid_chain_defaults` has NO legacy seed — it was
    // introduced alongside the contribution-backed chain registry and
    // relies entirely on the sync dispatcher to populate it.
    //
    // Without this block, `pyramid_chain_defaults` stays empty on
    // every boot, and the three-tier resolver in
    // `chain_registry::resolve_chain_for_slug` always falls through
    // to the tier 3 compile-time hardcoded fallback. Tier 2 (the
    // contribution-driven content-type default mapping) is dead code.
    //
    // Fix: on every boot, read the active `chain_defaults`
    // contribution and replay it to the operational table. This is
    // idempotent (DELETE + re-INSERT) and fast (handful of rows).
    sync_chain_defaults_to_operational(conn);
    sync_chain_assignments_to_operational(conn);
    sync_dispatch_policy_to_operational(conn);

    Ok(report)
}

/// Read the active `chain_defaults` contribution and sync its
/// mappings to `pyramid_chain_defaults`. No-op if no active
/// contribution exists (fresh install before any chain_defaults
/// contribution has been created — the tier 3 fallback covers this).
fn sync_chain_defaults_to_operational(conn: &Connection) {
    let row: Option<(String, String)> = conn
        .prepare(
            "SELECT contribution_id, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'chain_defaults'
               AND status = 'active'
             ORDER BY accepted_at DESC
             LIMIT 1",
        )
        .and_then(|mut stmt| {
            stmt.query_row([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
        })
        .unwrap_or_else(|e| {
            warn!(
                error = %e,
                "boot sync: failed to query active chain_defaults contribution"
            );
            None
        });

    if let Some((contribution_id, yaml_content)) = row {
        match serde_yaml::from_str::<ChainDefaultsYaml>(&yaml_content) {
            Ok(yaml) => {
                if let Err(e) =
                    chain_registry::upsert_chain_defaults(conn, &yaml.mappings, &contribution_id)
                {
                    warn!(
                        error = %e,
                        "boot sync: failed to upsert chain_defaults to operational table"
                    );
                } else {
                    debug!(
                        contribution_id = %contribution_id,
                        mappings = yaml.mappings.len(),
                        "boot sync: chain_defaults operational table hydrated"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    contribution_id = %contribution_id,
                    "boot sync: failed to parse chain_defaults YAML"
                );
            }
        }
    }
}

/// Replay all active `chain_assignment` contributions to the
/// `pyramid_chain_assignments` operational table. Ensures per-slug
/// overrides survive a table rebuild (schema migration) and are
/// consistent with the contribution store on every boot.
fn sync_chain_assignments_to_operational(conn: &Connection) {
    let rows: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT contribution_id, slug, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'chain_assignment'
               AND status = 'active'
             ORDER BY accepted_at DESC",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .unwrap_or_else(|e| {
            warn!(error = %e, "boot sync: failed to query chain_assignment contributions");
            Vec::new()
        });

    let mut replayed = 0u32;
    for (contribution_id, slug, yaml_content) in &rows {
        match serde_yaml::from_str::<ChainAssignmentYaml>(yaml_content) {
            Ok(yaml) => {
                if yaml.chain_id == "default" {
                    // "default" means "no override" — skip on boot sync since
                    // the table starts empty (or was just rebuilt).
                    continue;
                }
                if let Err(e) = chain_registry::assign_chain(conn, slug, &yaml.chain_id) {
                    warn!(
                        error = %e,
                        slug = %slug,
                        "boot sync: failed to replay chain_assignment"
                    );
                } else {
                    replayed += 1;
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    contribution_id = %contribution_id,
                    "boot sync: failed to parse chain_assignment YAML"
                );
            }
        }
    }

    if replayed > 0 {
        debug!(count = replayed, "boot sync: chain_assignments replayed");
    }
}

/// Read the active `dispatch_policy` contribution and sync its YAML
/// body to the singleton `pyramid_dispatch_policy` operational table.
/// No-op if no active contribution exists (pre-bundled-seed state).
///
/// Mirrors `sync_chain_defaults_to_operational` at :1449 — same
/// rationale: the contribution-insert path in
/// `insert_bundled_contribution` writes to `pyramid_config_contributions`
/// but cannot dispatch through `sync_config_to_operational` at migration
/// time (no BuildEventBus). `pyramid_dispatch_policy` has no legacy seed,
/// so without this helper the operational row stays empty on fresh-DB
/// boot and `LlmConfig.dispatch_policy` hydration at main.rs:11824-11887
/// falls through to the hardcoded fallback instead of the bundled
/// dispatch_policy seed.
///
/// Called from `walk_bundled_contributions_manifest` alongside the other
/// boot-sync helpers (Wave 0 task 2, walker re-plan wire 2.1).
fn sync_dispatch_policy_to_operational(conn: &Connection) {
    let row: Option<(String, String)> = conn
        .prepare(
            "SELECT contribution_id, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'dispatch_policy'
               AND status = 'active'
             ORDER BY accepted_at DESC
             LIMIT 1",
        )
        .and_then(|mut stmt| {
            stmt.query_row([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
        })
        .unwrap_or_else(|e| {
            warn!(
                error = %e,
                "boot sync: failed to query active dispatch_policy contribution"
            );
            None
        });

    if let Some((contribution_id, yaml_content)) = row {
        // Parse for validation — surfaces malformed YAML at boot rather
        // than at first dispatch. Parsed struct is discarded; the
        // operational table stores the raw YAML for hot-reload.
        match serde_yaml::from_str::<crate::pyramid::dispatch_policy::DispatchPolicyYaml>(
            &yaml_content,
        ) {
            Ok(_) => {
                if let Err(e) = crate::pyramid::db::upsert_dispatch_policy(
                    conn,
                    &None,
                    &yaml_content,
                    &contribution_id,
                ) {
                    warn!(
                        error = %e,
                        "boot sync: failed to upsert dispatch_policy to operational table"
                    );
                } else {
                    debug!(
                        contribution_id = %contribution_id,
                        "boot sync: dispatch_policy operational table hydrated"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    contribution_id = %contribution_id,
                    "boot sync: failed to parse dispatch_policy YAML"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use std::fs;
    use tempfile::TempDir;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn setup_chains_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let prompts = dir.path().join("prompts");
        fs::create_dir_all(prompts.join("conversation")).unwrap();
        fs::create_dir_all(prompts.join("conversation/_archived")).unwrap();
        fs::create_dir_all(prompts.join("shared")).unwrap();
        fs::create_dir_all(dir.path().join("defaults")).unwrap();

        fs::write(
            prompts.join("conversation").join("forward.md"),
            "# Forward prompt body\n\n## Instructions\nExtract...",
        )
        .unwrap();
        fs::write(
            prompts.join("conversation").join("reverse.md"),
            "# Reverse prompt body\n\n## Instructions\nReverse...",
        )
        .unwrap();
        // Archived file — must be skipped.
        fs::write(
            prompts.join("conversation/_archived").join("legacy.md"),
            "# Legacy (should not be migrated)",
        )
        .unwrap();
        fs::write(
            prompts.join("shared").join("heal_json.md"),
            "# Shared heal prompt",
        )
        .unwrap();

        fs::write(
            dir.path().join("defaults").join("question.yaml"),
            r#"schema_version: 1
id: question-pipeline
name: Question Pipeline
content_type: question
version: "2.0.0"
steps:
  - name: extract
    instruction: "$prompts/question/source_extract.md"
  - name: merge
    merge_instruction: "$prompts/shared/merge_sub_chunks.md"
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("defaults").join("code.yaml"),
            r#"schema_version: 1
id: code-pipeline
name: Code Pipeline
content_type: code
version: "1.0.0"
steps:
  - name: extract
    instruction: "$prompts/code/code_extract.md"
"#,
        )
        .unwrap();

        // Phase 8: seed two schema annotation files under
        // `chains/schemas/`. The migration walks this directory and
        // creates `schema_annotation` contributions for each file.
        fs::create_dir_all(dir.path().join("schemas")).unwrap();
        fs::write(
            dir.path().join("schemas").join("chain-step.schema.yaml"),
            r#"schema_type: chain_step_config
applies_to: chain_step_config
version: 1
label: "Chain Step Configuration"
fields:
  model_tier:
    label: "Model Tier"
    help: "Compute tier to use for this step"
    widget: select
    options_from: tier_registry
    visibility: basic
    show_cost: true
  temperature:
    label: "Temperature"
    help: "LLM sampling temperature"
    widget: slider
    min: 0.0
    max: 1.0
    step: 0.05
    visibility: basic
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("schemas").join("dadbear.schema.yaml"),
            r#"schema_type: dadbear_policy
applies_to: dadbear_policy
version: 1
fields:
  enabled:
    label: "Enabled"
    help: "Run DADBEAR on this pyramid"
    widget: toggle
    visibility: basic
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn migration_inserts_prompts_skipping_archived() {
        let conn = mem_conn();
        let chains = setup_chains_dir();

        let report =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();

        assert!(report.ran);
        assert_eq!(report.prompts_inserted, 3);
        assert_eq!(report.prompts_skipped_already_present, 0);
        assert_eq!(report.prompts_failed, 0);

        // Archived file must NOT be in the database.
        let archived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'skill' AND slug = ?1",
                rusqlite::params!["conversation/_archived/legacy.md"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived, 0, "archived prompt must not be migrated");

        // Forward prompt should exist and carry the canonical metadata.
        let forward: (String, String) = conn
            .query_row(
                "SELECT yaml_content, wire_native_metadata_json
                 FROM pyramid_config_contributions
                 WHERE schema_type = 'skill' AND slug = ?1",
                rusqlite::params!["conversation/forward.md"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(forward.0.contains("Forward prompt body"));
        let meta = WireNativeMetadata::from_json(&forward.1).unwrap();
        assert_eq!(meta.contribution_type, WireContributionType::Skill);
        assert_eq!(meta.maturity, WireMaturity::Canon);
        assert!(meta.topics.iter().any(|t| t == "conversation"));
        assert!(meta.topics.iter().any(|t| t == "narrative:forward"));
        assert_eq!(meta.price, Some(1));
    }

    #[test]
    fn migration_inserts_chains_with_derived_from_links() {
        let conn = mem_conn();
        let chains = setup_chains_dir();

        let report =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert_eq!(report.chains_inserted, 2);

        // question-pipeline chain should have 2 derived_from entries
        // pointing at its two prompt references.
        let meta_json: String = conn
            .query_row(
                "SELECT wire_native_metadata_json FROM pyramid_config_contributions
                 WHERE schema_type = 'custom_chain' AND slug = ?1",
                rusqlite::params!["question-pipeline"],
                |row| row.get(0),
            )
            .unwrap();
        let meta = WireNativeMetadata::from_json(&meta_json).unwrap();
        assert_eq!(meta.contribution_type, WireContributionType::Action);
        assert_eq!(meta.maturity, WireMaturity::Canon);
        assert_eq!(meta.derived_from.len(), 2);
        for entry in &meta.derived_from {
            entry.validate().unwrap();
            assert!(entry.doc.is_some());
            assert!(entry.weight > 0.0);
        }
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = mem_conn();
        let chains = setup_chains_dir();

        let first = migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert_eq!(first.prompts_inserted, 3);
        assert_eq!(first.chains_inserted, 2);
        assert!(first.marker_written);

        // Second call: short-circuit on disk-walk sentinel. Phase 9
        // bundled walk still runs but every bundled row is already
        // present (per-entry INSERT OR IGNORE), so nothing new lands.
        let second = migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert!(!second.ran);
        assert_eq!(second.prompts_inserted, 0);
        assert_eq!(second.chains_inserted, 0);
        assert_eq!(second.bundled_inserted, 0);
        assert!(second.bundled_skipped_already_present > 0);

        // Total number of DISK-migrated skill rows should equal
        // prompts_inserted from the first run (no duplicates). Phase 9
        // bundled skills (from the manifest) are filtered out by
        // created_by = 'phase5_bootstrap'.
        let skill_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'skill' AND created_by = 'phase5_bootstrap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(skill_count, 3);
    }

    #[test]
    fn migration_with_missing_chains_dir_does_not_abort() {
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        // No prompts/ or defaults/ subdirs.
        let report =
            migrate_prompts_and_chains_to_contributions(&conn, dir.path()).unwrap();
        assert!(report.ran);
        assert_eq!(report.prompts_inserted, 0);
        assert_eq!(report.chains_inserted, 0);
        // No marker written because nothing succeeded.
        assert!(!report.marker_written);
    }

    #[test]
    fn extract_prompt_refs_finds_all_forms() {
        let yaml = r#"
steps:
  - name: extract
    instruction: "$prompts/question/source_extract.md"
  - name: cluster
    cluster_instruction: "$prompts/shared/cluster.md"
  - name: merge
    merge_instruction: "$prompts/shared/merge_sub_chunks.md"
  # Same reference appearing twice — should dedupe.
  - name: other
    instruction: "$prompts/question/source_extract.md"
"#;
        let refs = extract_prompt_refs(yaml);
        assert_eq!(
            refs,
            vec![
                "question/source_extract.md",
                "shared/cluster.md",
                "shared/merge_sub_chunks.md"
            ]
        );
    }

    #[test]
    fn extract_chain_id_handles_quoted_and_bare() {
        assert_eq!(
            extract_chain_id("schema_version: 1\nid: question-pipeline\n"),
            Some("question-pipeline".to_string())
        );
        assert_eq!(
            extract_chain_id("schema_version: 1\nid: \"question-pipeline\"\n"),
            Some("question-pipeline".to_string())
        );
        assert_eq!(extract_chain_id("schema_version: 1\nname: x\n"), None);
    }

    #[test]
    fn extract_annotation_slug_prefers_applies_to() {
        let yaml = r#"schema_type: chain_step_config
applies_to: per_step_overrides
version: 1
fields: {}
"#;
        assert_eq!(
            extract_annotation_slug(yaml),
            Some("per_step_overrides".to_string())
        );
    }

    #[test]
    fn extract_annotation_slug_falls_back_to_schema_type() {
        let yaml = r#"schema_type: dadbear_policy
version: 1
fields: {}
"#;
        assert_eq!(
            extract_annotation_slug(yaml),
            Some("dadbear_policy".to_string())
        );
    }

    #[test]
    fn extract_annotation_slug_handles_quoted_values() {
        let yaml = r#"schema_type: "chain_step_config"
applies_to: 'chain_step_config'
"#;
        assert_eq!(
            extract_annotation_slug(yaml),
            Some("chain_step_config".to_string())
        );
    }

    #[test]
    fn phase8_migration_inserts_schema_annotations() {
        let conn = mem_conn();
        let chains = setup_chains_dir();

        let report =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert!(report.ran);
        assert_eq!(
            report.schema_annotations_inserted, 2,
            "expected both seeded schema annotations to land"
        );
        assert_eq!(report.schema_annotations_skipped_already_present, 0);
        assert_eq!(report.schema_annotations_failed, 0);
        assert!(report.marker_written);

        // chain_step_config annotation should be present and carry
        // Template contribution_type + Canon maturity.
        let meta_json: String = conn
            .query_row(
                "SELECT wire_native_metadata_json FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_annotation' AND slug = ?1",
                rusqlite::params!["chain_step_config"],
                |row| row.get(0),
            )
            .unwrap();
        let meta = WireNativeMetadata::from_json(&meta_json).unwrap();
        assert_eq!(meta.contribution_type, WireContributionType::Template);
        assert_eq!(meta.maturity, WireMaturity::Canon);
        assert!(meta.topics.iter().any(|t| t == "chain_step_config"));
        assert!(meta.topics.iter().any(|t| t == "ui_annotation"));

        // Body must round-trip through yaml_renderer::SchemaAnnotation.
        let body: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_annotation' AND slug = ?1",
                rusqlite::params!["chain_step_config"],
                |row| row.get(0),
            )
            .unwrap();
        let parsed: crate::pyramid::yaml_renderer::SchemaAnnotation =
            serde_yaml::from_str(&body).unwrap();
        assert_eq!(parsed.schema_type, "chain_step_config");
        assert_eq!(parsed.fields.len(), 2);
    }

    #[test]
    fn phase8_schema_annotation_migration_idempotent() {
        let conn = mem_conn();
        let chains = setup_chains_dir();

        let first =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert_eq!(first.schema_annotations_inserted, 2);

        // Second run short-circuits on the disk-walk sentinel — no
        // duplicates from the disk walk. Phase 9 bundled annotations
        // (from the manifest) are still present but counted separately.
        let second =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert!(!second.ran);

        // Total schema_annotation count = 2 disk-migrated (from
        // setup_chains_dir) + however many the Phase 9 bundled
        // manifest ships. Filter by created_by to isolate the disk-
        // walk rows we actually care about.
        let disk_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_annotation' AND created_by = 'phase5_bootstrap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(disk_count, 2);
    }

    // ── Phase 9 bundled manifest tests ─────────────────────────────

    #[test]
    fn phase9_bundled_manifest_parses() {
        // Smoke test: the shipped manifest must parse successfully.
        let manifest = load_bundled_manifest().unwrap();
        assert_eq!(manifest.manifest_version, 1);
        assert!(
            manifest.contributions.len() >= 15,
            "manifest should have >= 15 entries, got {}",
            manifest.contributions.len()
        );
        // Every contribution_id must start with "bundled-".
        for entry in &manifest.contributions {
            assert!(
                entry.contribution_id.starts_with("bundled-"),
                "contribution_id {} does not start with 'bundled-'",
                entry.contribution_id
            );
        }
        // IDs must be unique across the manifest.
        let mut seen = std::collections::HashSet::new();
        for entry in &manifest.contributions {
            assert!(
                seen.insert(&entry.contribution_id),
                "duplicate contribution_id {} in manifest",
                entry.contribution_id
            );
        }
    }

    #[test]
    fn phase9_bundled_walk_inserts_all_entries() {
        let conn = mem_conn();
        let report = walk_bundled_contributions_manifest(&conn).unwrap();
        assert!(
            report.inserted >= 15,
            "expected >= 15 bundled rows inserted, got {}",
            report.inserted
        );
        assert_eq!(report.skipped_already_present, 0);
        assert_eq!(report.failed, 0);

        // Every bundled row should land with source = 'bundled' and
        // created_by = 'bootstrap'.
        let bundled_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE source = 'bundled' AND created_by = 'bootstrap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(bundled_count >= 15);
    }

    #[test]
    fn phase9_bundled_walk_is_idempotent() {
        let conn = mem_conn();
        let first = walk_bundled_contributions_manifest(&conn).unwrap();
        let inserted_first = first.inserted;
        assert!(inserted_first > 0);

        // Second run: every entry already exists, nothing inserts.
        let second = walk_bundled_contributions_manifest(&conn).unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.skipped_already_present, inserted_first);
        assert_eq!(second.failed, 0);
    }

    #[test]
    fn phase9_bundled_walk_skips_user_superseded() {
        let conn = mem_conn();
        // First run: bundled defaults land.
        walk_bundled_contributions_manifest(&conn).unwrap();

        // Simulate the user superseding the bundled evidence_policy
        // default with their own refinement. Phase 0a-1 commit 5:
        // mark the bundled row superseded BEFORE the user INSERT so
        // `uq_config_contrib_active` never sees two active rows.
        let bundled_id = "bundled-evidence_policy-default-v1";
        let user_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1",
            rusqlite::params![bundled_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, NULL, 'evidence_policy', 'schema_type: evidence_policy',
                '{}', '{}',
                ?2, NULL, 'user refinement',
                'active', 'local', NULL, 'user', datetime('now')
             )",
            rusqlite::params![user_id, bundled_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![user_id, bundled_id],
        )
        .unwrap();

        // Second walk: bundled id already exists (as superseded),
        // INSERT OR IGNORE skips it, user's version remains active.
        let second = walk_bundled_contributions_manifest(&conn).unwrap();
        assert_eq!(second.inserted, 0);

        // Verify the user's refinement is still active.
        let active_source: String = conn
            .query_row(
                "SELECT source FROM pyramid_config_contributions
                 WHERE schema_type = 'evidence_policy'
                   AND status = 'active'
                   AND superseded_by_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_source, "local");
    }

    #[test]
    fn phase9_bundled_walk_runs_before_sentinel_check() {
        // Regression test for the Phase 9 extension: even when the
        // Phase 5 disk-walk sentinel is present, the bundled walk
        // must still run so app upgrades can add new manifest entries.
        let conn = mem_conn();
        // Manually insert the sentinel as though a prior run finished.
        let marker_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                status, source, created_by, accepted_at
             ) VALUES (
                ?1, NULL, ?2, '',
                '{}', '{}',
                'active', 'migration', 'phase5_bootstrap', datetime('now')
             )",
            rusqlite::params![marker_id, PROMPT_MIGRATION_MARKER],
        )
        .unwrap();

        let dir = TempDir::new().unwrap();
        let report =
            migrate_prompts_and_chains_to_contributions(&conn, dir.path()).unwrap();
        // Disk walk short-circuits (sentinel present).
        assert!(!report.ran);
        // But Phase 9 bundled walk still ran.
        assert!(report.bundled_inserted >= 15);
    }

    // ── Bundled version-bump consolidation tests (WS1a verifier) ──

    /// Helper: insert an arbitrary active config contribution with an
    /// explicit contribution_id and source, skipping the standard
    /// create_config_contribution path. Used to simulate pre-existing
    /// rows in the DB before calling the consolidation helper.
    ///
    /// Phase 0a-1 commit 5 note: these tests deliberately seed
    /// duplicate `status='active'` rows for the same
    /// (schema_type, slug) to simulate pre-upgrade dev-DB state.
    /// Walker-v3 added `uq_config_contrib_active` which would
    /// otherwise reject the second INSERT. Tests drop the index
    /// before seeding; the dedup helper under test restores a
    /// single-active invariant and the index is re-created in the
    /// post-dedup assertion by calling `ensure_config_contrib_active_unique_index`.
    fn insert_active_row(
        conn: &Connection,
        contribution_id: &str,
        schema_type: &str,
        slug: Option<&str>,
        source: &str,
        yaml_content: &str,
    ) {
        conn.execute("DROP INDEX IF EXISTS uq_config_contrib_active", [])
            .unwrap();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, ?3, ?4,
                '{}', '{}',
                NULL, NULL, 'test fixture',
                'active', ?5, NULL, 'test', datetime('now')
             )",
            rusqlite::params![contribution_id, slug, schema_type, yaml_content, source],
        )
        .unwrap();
    }

    #[test]
    fn consolidate_supersedes_old_bundled_with_newest_bundled() {
        // WS1a scenario: both v1 and v2 of a bundled default sit
        // active-without-user-refinement. Consolidation must mark v1
        // as superseded by v2 so `load_active_config_contribution`
        // no longer sees dual-active rows.
        let conn = mem_conn();
        insert_active_row(
            &conn,
            "bundled-test_policy-default-v1",
            "test_policy",
            None,
            "bundled",
            "schema_type: test_policy\nversion: 1\n",
        );
        // Sleep 1s to ensure v2 has a later `datetime('now')` than v1.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        insert_active_row(
            &conn,
            "bundled-test_policy-default-v2",
            "test_policy",
            None,
            "bundled",
            "schema_type: test_policy\nversion: 2\n",
        );

        consolidate_bundled_versions(&conn).unwrap();

        // v1 must be superseded by v2; v2 remains active.
        let v1_state: (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-test_policy-default-v1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(v1_state.0, "superseded");
        assert_eq!(
            v1_state.1.as_deref(),
            Some("bundled-test_policy-default-v2")
        );

        let v2_state: (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-test_policy-default-v2"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(v2_state.0, "active");
        assert!(v2_state.1.is_none());
    }

    #[test]
    fn consolidate_preserves_user_refinement_over_newer_bundled() {
        // Most important case: a user has refined the policy, their
        // refinement sits active. Then an app upgrade inserts a new
        // bundled v2 with a later `created_at`. Without consolidation,
        // `load_active_config_contribution` would return v2 (newer
        // timestamp) and silently override the user's choice.
        //
        // Consolidation must recognize the non-bundled row as the
        // winner and mark the bundled row superseded instead.
        let conn = mem_conn();
        insert_active_row(
            &conn,
            "user-refined-policy-abc",
            "test_policy",
            None,
            "local",
            "schema_type: test_policy\nuser_choice: true\n",
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // Bundled v2 arrives later from an app upgrade.
        insert_active_row(
            &conn,
            "bundled-test_policy-default-v2",
            "test_policy",
            None,
            "bundled",
            "schema_type: test_policy\nversion: 2\n",
        );

        consolidate_bundled_versions(&conn).unwrap();

        // The user's refinement wins even though the bundled row is
        // newer by created_at.
        let user_state: (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["user-refined-policy-abc"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(user_state.0, "active");
        assert!(user_state.1.is_none());

        let v2_state: (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-test_policy-default-v2"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(v2_state.0, "superseded");
        assert_eq!(v2_state.1.as_deref(), Some("user-refined-policy-abc"));
    }

    #[test]
    fn consolidate_is_idempotent_on_already_clean_table() {
        // Fresh-install scenario: only one active row per pair.
        // Consolidation must not touch anything.
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        consolidate_bundled_versions(&conn).unwrap();
        consolidate_bundled_versions(&conn).unwrap();

        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, after, "no-op on a clean table");
    }

    #[test]
    fn consolidate_handles_slug_scoped_pairs_independently() {
        // Regression guard: slug-scoped and slug=NULL rows must be
        // consolidated as distinct pairs. A duplicate on one slug
        // shouldn't affect rows on a different slug.
        let conn = mem_conn();
        // Two active bundled rows on slug 'pyramid-A' (dup).
        insert_active_row(
            &conn,
            "bundled-viz-A-v1",
            "pyramid_viz_config",
            Some("pyramid-A"),
            "bundled",
            "schema_type: pyramid_viz_config\n",
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        insert_active_row(
            &conn,
            "bundled-viz-A-v2",
            "pyramid_viz_config",
            Some("pyramid-A"),
            "bundled",
            "schema_type: pyramid_viz_config\n",
        );
        // Single active row on slug 'pyramid-B' (not dup).
        insert_active_row(
            &conn,
            "bundled-viz-B-v1",
            "pyramid_viz_config",
            Some("pyramid-B"),
            "bundled",
            "schema_type: pyramid_viz_config\n",
        );

        consolidate_bundled_versions(&conn).unwrap();

        // A-v1 superseded, A-v2 active.
        let a_v1_status: String = conn
            .query_row(
                "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
                rusqlite::params!["bundled-viz-A-v1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_v1_status, "superseded");

        // B-v1 untouched — no duplicate on that slug.
        let b_v1_status: String = conn
            .query_row(
                "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
                rusqlite::params!["bundled-viz-B-v1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_v1_status, "active");
    }

    #[test]
    fn consolidate_sets_winner_supersedes_id_when_null() {
        // WS1a wanderer MAJOR-3: when consolidate picks a winner that
        // has no supersedes_id (common: freshly-inserted bundled row),
        // the winner's supersedes_id must get updated to point at the
        // most-recent loser so chain-walk APIs (load_config_version_
        // history) can traverse back through demoted rows.
        let conn = mem_conn();
        insert_active_row(
            &conn,
            "bundled-test-v1",
            "test_schema",
            None,
            "bundled",
            "schema_type: test_schema\n",
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        insert_active_row(
            &conn,
            "bundled-test-v2",
            "test_schema",
            None,
            "bundled",
            "schema_type: test_schema\n",
        );

        consolidate_bundled_versions(&conn).unwrap();

        // Winner's supersedes_id now points at the loser.
        let v2_supersedes: Option<String> = conn
            .query_row(
                "SELECT supersedes_id FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params!["bundled-test-v2"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v2_supersedes.as_deref(), Some("bundled-test-v1"));
    }

    #[test]
    fn consolidate_does_not_overwrite_winner_existing_supersedes_id() {
        // WS1a wanderer MAJOR-3 corollary: when the winner is a user
        // refinement (source != 'bundled') that already has a valid
        // supersedes_id — because it was written via the real
        // `supersede_config_contribution` path pointing at the
        // pre-existing bundled row — consolidate must NOT overwrite
        // that link. The user's historical chain is load-bearing.
        let conn = mem_conn();
        insert_active_row(
            &conn,
            "bundled-test-v1",
            "test_schema",
            None,
            "bundled",
            "schema_type: test_schema\n",
        );
        // Simulate user refinement that properly supersedes v1 — they
        // wrote via `supersede_config_contribution` so supersedes_id is
        // set to v1 already.
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                'user-test-refinement', NULL, 'test_schema',
                'schema_type: test_schema\ntuned: true\n',
                '{}', '{}',
                'bundled-test-v1', NULL, 'user tuned',
                'active', 'local', NULL, 'user', datetime('now')
             )",
            [],
        )
        .unwrap();
        // Mark v1 superseded by the user refinement as real
        // supersede_config_contribution would do.
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded', superseded_by_id = 'user-test-refinement'
             WHERE contribution_id = 'bundled-test-v1'",
            [],
        )
        .unwrap();
        // App upgrade drops v2 in.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        insert_active_row(
            &conn,
            "bundled-test-v2",
            "test_schema",
            None,
            "bundled",
            "schema_type: test_schema\n",
        );

        consolidate_bundled_versions(&conn).unwrap();

        // User refinement stays active; its supersedes_id is still v1
        // (NOT overwritten to point at v2). v2 is marked superseded.
        let user_state: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, supersedes_id, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = 'user-test-refinement'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(user_state.0, "active");
        assert_eq!(user_state.1.as_deref(), Some("bundled-test-v1"));
        assert!(user_state.2.is_none());

        let v2_status: String = conn
            .query_row(
                "SELECT status FROM pyramid_config_contributions
                 WHERE contribution_id = 'bundled-test-v2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v2_status, "superseded");
    }

    #[test]
    fn canonicalize_rewrites_legacy_6_field_user_refinement() {
        // WS1a wanderer MAJOR-1: a user-refined pre-WS1a policy with
        // only the 5 old booleans must be rewritten to explicit 10-field
        // at upgrade time. Otherwise the 5 new fields deserialize as
        // None and project from mode, silently turning on markets the
        // operator never opted into.
        let conn = mem_conn();
        let legacy_yaml = "schema_type: compute_participation_policy\n\
                           mode: hybrid\n\
                           allow_market_visibility: false\n\
                           allow_serving_while_degraded: false\n\
                           allow_fleet_dispatch: true\n\
                           allow_fleet_serving: true\n";
        // User refinement — source='local' (not bundled).
        insert_active_row(
            &conn,
            "user-ppolicy-pre-ws1a",
            "compute_participation_policy",
            None,
            "local",
            legacy_yaml,
        );

        canonicalize_legacy_participation_policy(&conn).unwrap();

        // Read back the rewritten YAML — must now have all 10 booleans
        // explicit and the original explicit overrides preserved.
        let new_yaml: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE contribution_id = 'user-ppolicy-pre-ws1a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // The original explicit override (market_visibility=false) must survive.
        assert!(new_yaml.contains("allow_market_visibility: false"),
            "original explicit override must be preserved:\n{new_yaml}");
        // Fleet fields still true.
        assert!(new_yaml.contains("allow_fleet_dispatch: true"));
        assert!(new_yaml.contains("allow_fleet_serving: true"));
        // Previously-absent fields: now explicit at their projected
        // values for hybrid mode (true — the DD-I hybrid projection).
        assert!(new_yaml.contains("allow_market_dispatch: true"),
            "hybrid projection for market_dispatch must be explicit:\n{new_yaml}");
        assert!(new_yaml.contains("allow_storage_pulling: true"));
        assert!(new_yaml.contains("allow_storage_hosting: true"));
        assert!(new_yaml.contains("allow_relay_usage: true"));
        assert!(new_yaml.contains("allow_relay_serving: true"));

        // Idempotency: running a second time on the now-canonical YAML
        // must be a no-op (same YAML, no UPDATE).
        let yaml_before: String = new_yaml.clone();
        canonicalize_legacy_participation_policy(&conn).unwrap();
        let yaml_after: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE contribution_id = 'user-ppolicy-pre-ws1a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(yaml_after, yaml_before,
            "canonicalization must be idempotent on already-canonical YAML");
    }

    #[test]
    fn canonicalize_skips_bundled_source_rows() {
        // Only user refinements get rewritten. Bundled rows are already
        // canonical via bundled_contributions.json and must be left
        // untouched (rewriting them would change their content-hash
        // and defeat the INSERT OR IGNORE dedup on next boot).
        let conn = mem_conn();
        let bundled_yaml = "schema_type: compute_participation_policy\nmode: hybrid\n";
        insert_active_row(
            &conn,
            "bundled-ppolicy-test-v1",
            "compute_participation_policy",
            None,
            "bundled",
            bundled_yaml,
        );

        canonicalize_legacy_participation_policy(&conn).unwrap();

        let after: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE contribution_id = 'bundled-ppolicy-test-v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, bundled_yaml, "bundled rows must be untouched");
    }

    #[test]
    fn canonicalize_handles_malformed_yaml_gracefully() {
        // A corrupt YAML must not block the migration — log and skip.
        // Other rows in the same batch must still be processed.
        let conn = mem_conn();
        insert_active_row(
            &conn,
            "user-ppolicy-corrupt",
            "compute_participation_policy",
            None,
            "local",
            "this is not: valid\n  yaml:: at all [[[\n",
        );
        insert_active_row(
            &conn,
            "user-ppolicy-valid-legacy",
            "compute_participation_policy",
            None,
            "local",
            "schema_type: compute_participation_policy\nmode: worker\nallow_fleet_dispatch: false\n",
        );

        // Must not panic or error.
        canonicalize_legacy_participation_policy(&conn).unwrap();

        // Corrupt row: left untouched.
        let corrupt: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE contribution_id = 'user-ppolicy-corrupt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(corrupt.contains("not: valid"));

        // Valid row: canonicalized to 10 fields.
        let valid: String = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE contribution_id = 'user-ppolicy-valid-legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(valid.contains("allow_relay_serving:"),
            "valid row must have the 5 new fields explicit");
    }

    #[test]
    fn phase8_migration_with_schemas_only_still_writes_marker() {
        // First-run edge case: only schemas present, no prompts/chains.
        let conn = mem_conn();
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("schemas")).unwrap();
        fs::write(
            dir.path().join("schemas").join("chain-step.schema.yaml"),
            r#"schema_type: chain_step_config
applies_to: chain_step_config
version: 1
fields:
  x:
    label: "X"
    help: "x field"
    widget: text
    visibility: basic
"#,
        )
        .unwrap();

        let report =
            migrate_prompts_and_chains_to_contributions(&conn, dir.path()).unwrap();
        assert!(report.ran);
        assert_eq!(report.prompts_inserted, 0);
        assert_eq!(report.chains_inserted, 0);
        assert_eq!(report.schema_annotations_inserted, 1);
        assert!(
            report.marker_written,
            "marker should be written when only schemas land"
        );
    }

    #[test]
    fn sync_dispatch_policy_to_operational_hydrates_row() {
        // Walker re-plan wire 2.1 Wave 0 task 2 — mirrors
        // sync_chain_defaults_to_operational. Active dispatch_policy
        // contribution → operational pyramid_dispatch_policy row.
        let conn = mem_conn();
        let yaml = "version: 1\nprovider_pools:\n  openrouter: { concurrency: 4 }\nrouting_rules: []\n";
        insert_active_row(
            &conn,
            "test-dispatch-policy-v1",
            "dispatch_policy",
            None,
            "local",
            yaml,
        );

        sync_dispatch_policy_to_operational(&conn);

        let (content, contrib_id): (String, String) = conn
            .query_row(
                "SELECT yaml_content, contribution_id FROM pyramid_dispatch_policy WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, yaml);
        assert_eq!(contrib_id, "test-dispatch-policy-v1");
    }

    /// Phase 0a-1 commit 5 wanderer fix regression guard: when the
    /// bundled manifest ships a v-bump (new contribution_id for an
    /// existing (schema_type, slug) where the prior row is still
    /// `source='bundled'`), `insert_bundled_contribution` must
    /// pre-emptively supersede the prior so the `uq_config_contrib_active`
    /// unique index does not reject the INSERT. Before the fix, the
    /// new bundled default silently failed to land on upgrade.
    #[test]
    fn insert_bundled_v_bump_supersedes_prior_bundled() {
        use crate::pyramid::wire_native_metadata::default_wire_native_metadata;
        let conn = mem_conn();

        // Seed a v1 bundled active row via the production path (index
        // in force, no duplicate-actives).
        let v1 = BundledContributionEntry {
            contribution_id: "bundled-vbump_test-v1".to_string(),
            slug: Some("vbump-slug".to_string()),
            schema_type: "vbump_test_schema".to_string(),
            yaml_content: "version: 1\n".to_string(),
            triggering_note: "v1".to_string(),
            topics_extra: Vec::new(),
            applies_to: None,
        };
        let metadata_v1 = default_wire_native_metadata(&v1.schema_type, v1.slug.as_deref());
        assert!(
            insert_bundled_contribution(&conn, &v1, &metadata_v1).unwrap(),
            "v1 insert should return Ok(true)"
        );

        // Now ship a v-bump with a new contribution_id, same slug +
        // schema_type. Pre-fix, this would fail SupersessionConflict.
        // Post-fix, it supersedes v1 and lands v2.
        let v2 = BundledContributionEntry {
            contribution_id: "bundled-vbump_test-v2".to_string(),
            slug: Some("vbump-slug".to_string()),
            schema_type: "vbump_test_schema".to_string(),
            yaml_content: "version: 2\n".to_string(),
            triggering_note: "v2".to_string(),
            topics_extra: Vec::new(),
            applies_to: None,
        };
        let metadata_v2 = default_wire_native_metadata(&v2.schema_type, v2.slug.as_deref());
        assert!(
            insert_bundled_contribution(&conn, &v2, &metadata_v2).unwrap(),
            "v2 insert should return Ok(true) after v-bump handling"
        );

        // v1 must now be superseded with forward pointer to v2.
        let (v1_status, v1_fwd): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by_id FROM pyramid_config_contributions
                 WHERE contribution_id = 'bundled-vbump_test-v1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(v1_status, "superseded");
        assert_eq!(v1_fwd.as_deref(), Some("bundled-vbump_test-v2"));

        // v2 must be the sole active row with supersedes_id pointing
        // back at v1 (chain symmetric).
        let (v2_status, v2_back): (String, Option<String>) = conn
            .query_row(
                "SELECT status, supersedes_id FROM pyramid_config_contributions
                 WHERE contribution_id = 'bundled-vbump_test-v2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(v2_status, "active");
        assert_eq!(v2_back.as_deref(), Some("bundled-vbump_test-v1"));

        // Unique index still holds: exactly one active for this pair.
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'vbump_test_schema' AND slug = 'vbump-slug'
                   AND status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 1);
    }
}
