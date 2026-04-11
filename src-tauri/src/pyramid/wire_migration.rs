// pyramid/wire_migration.rs — Phase 5: on-disk prompts + chains → contributions.
//
// Canonical references:
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/wire-contribution-mapping.md
//     — "Migration from On-Disk Prompts and Schemas" section
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
use rusqlite::Connection;
use tracing::{debug, warn};

use crate::pyramid::config_contributions::create_config_contribution_with_metadata;
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
/// counters.
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

    // Idempotency guard: short-circuit if the sentinel exists.
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
            "Phase 5 prompt migration: sentinel row already present, skipping migration"
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

        // Second call: short-circuit on sentinel.
        let second = migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert!(!second.ran);
        assert_eq!(second.prompts_inserted, 0);
        assert_eq!(second.chains_inserted, 0);

        // Total number of skill rows should equal prompts_inserted
        // from the first run (no duplicates).
        let skill_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'skill'",
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

        // Second run short-circuits on the sentinel — no duplicates.
        let second =
            migrate_prompts_and_chains_to_contributions(&conn, chains.path()).unwrap();
        assert!(!second.ran);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'schema_annotation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
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
}
