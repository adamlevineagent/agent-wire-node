// pyramid/folder_ingestion.rs — Phase 17: recursive folder ingestion
//
// Walks a target folder, detects content types, and produces an
// `IngestionPlan` describing the set of pyramids and topical vines to
// create. The plan is returned by a dry-run and then executed (or
// preview-rendered) by the caller.
//
// Features:
// - Content type detection (majority extension wins, mixed → None)
// - Recursive folder walk with depth cap + homogeneity check
// - `.pyramid-ignore` + `.gitignore` + bundled default ignore patterns
// - Slug generation from the last 2-3 path segments, kebab-cased,
//   with suffix-based collision resolution
// - Plan + execute split so the UI can preview before committing
// - Claude Code conversation auto-include: discovers
//   `~/.claude/projects/` directories whose encoded path matches the
//   target folder or any of its subfolders via prefix match, and
//   attaches them to the top-level vine as conversation pyramids
//
// See `docs/specs/vine-of-vines-and-folder-ingestion.md` Part 2 for
// the canonical spec. The Claude Code auto-include section (spec
// lines 229-344) is implemented verbatim: path encoding is a simple
// `replace('/', '-')`, matches are the exact encoding OR the prefix
// `encoded_target + "-"` to cover subfolders and worktrees, and the
// scan runs only at the top-level call (the `is_top_level_call`
// guard is expressed as a boolean flag threaded through the
// recursive walker).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::db::{self, FolderIngestionConfig};
use super::types::{ContentType, DadbearWatchConfig};
use super::PyramidState;

// ── Public types ──────────────────────────────────────────────────────────────

/// A single unit of work in an ingestion plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IngestionOperation {
    CreatePyramid {
        slug: String,
        content_type: String, // ContentType::as_str() — serialized as lowercase
        source_path: String,
    },
    CreateVine {
        slug: String,
        source_path: String, // informational; vines don't ingest files directly
    },
    AddChildToVine {
        vine_slug: String,
        child_slug: String,
        position: i32,
        child_type: String, // "bedrock" | "vine"
    },
    RegisterDadbearConfig {
        slug: String,
        source_path: String,
        content_type: String,
        scan_interval_secs: u64,
    },
    RegisterClaudeCodePyramid {
        slug: String,
        source_path: String,
        is_main: bool,
        is_worktree: bool,
    },
}

/// A complete ingestion plan. Returned by `plan_ingestion`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestionPlan {
    pub operations: Vec<IngestionOperation>,
    pub root_slug: Option<String>,
    pub root_source_path: String,
    pub total_files: usize,
    pub total_ignored: usize,
}

/// Result of executing an ingestion plan. Lists what was actually
/// created so the UI can render a post-commit summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestionResult {
    pub pyramids_created: Vec<String>,
    pub vines_created: Vec<String>,
    pub dadbear_configs: Vec<String>,
    pub claude_code_pyramids: Vec<String>,
    pub compositions_added: usize,
    pub root_slug: Option<String>,
    pub errors: Vec<String>,
}

/// Shape returned by `pyramid_find_claude_code_conversations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCodeConversationDir {
    pub encoded_path: String,
    pub absolute_path: String,
    pub jsonl_count: usize,
    pub earliest_mtime: Option<String>,
    pub latest_mtime: Option<String>,
    pub is_main: bool,
    pub is_worktree: bool,
}

/// The raw result of scanning a single directory (non-recursive).
pub struct ScanResult {
    pub subfolders: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub ignored_count: usize,
}

// ── Content type detection ────────────────────────────────────────────────────

/// Return the lowercase extension (with leading dot) of a file path, or
/// None if the path has no extension.
fn lowercase_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|s| format!(".{}", s.to_ascii_lowercase()))
}

/// Detect the content type of a homogeneous file set.
///
/// Strategy:
/// 1. Classify each file as `code`, `document`, or `unknown` against
///    the heuristic config.
/// 2. Ignore `unknown` files for the purposes of the majority vote.
/// 3. If one category reaches a strict majority of the classified
///    files, return that content type. Otherwise return `None` which
///    forces the caller to create a topical vine instead.
pub fn detect_content_type(
    files: &[PathBuf],
    config: &FolderIngestionConfig,
) -> Option<ContentType> {
    if files.is_empty() {
        return None;
    }

    let mut code_count = 0usize;
    let mut doc_count = 0usize;
    let mut convo_count = 0usize;

    for f in files {
        let Some(ext) = lowercase_extension(f) else {
            continue;
        };
        // Claude Code style `.jsonl` files are treated as conversation
        // candidates regardless of config.
        if ext == ".jsonl" {
            convo_count += 1;
            continue;
        }
        if config.code_extensions.iter().any(|e| e == &ext) {
            code_count += 1;
        } else if config.document_extensions.iter().any(|e| e == &ext) {
            doc_count += 1;
        }
    }

    let classified = code_count + doc_count + convo_count;
    if classified == 0 {
        return None;
    }

    // Strict majority: the winning category must be >= half and be
    // strictly greater than the other categories combined. If the top
    // two are tied, we treat the folder as mixed.
    let mut entries = [
        (ContentType::Code, code_count),
        (ContentType::Document, doc_count),
        (ContentType::Conversation, convo_count),
    ];
    entries.sort_by_key(|(_, c)| std::cmp::Reverse(*c));

    let (top_type, top_count) = (entries[0].0.clone(), entries[0].1);
    let (_, runner_up) = (entries[1].0.clone(), entries[1].1);
    if top_count == 0 {
        return None;
    }
    if top_count == runner_up {
        return None;
    }
    if top_count * 2 < classified {
        // Not a strict majority — treat as mixed.
        return None;
    }
    Some(top_type)
}

/// Return true if every classified file in the set maps to the same
/// detected content type. Unclassified files are ignored.
pub fn is_homogeneous(files: &[PathBuf], config: &FolderIngestionConfig) -> bool {
    detect_content_type(files, config).is_some()
}

// ── Scanning ──────────────────────────────────────────────────────────────────

/// Scan a single directory non-recursively.
///
/// Applies ignore matching in two layers:
/// 1. The `ignore` crate's `WalkBuilder` respects `.gitignore`,
///    `.git/info/exclude`, and the global gitignore. A temporary
///    `.pyramid-ignore` or `.wireignore` override file is honored via
///    `WalkBuilder::add_custom_ignore_filename`.
/// 2. The heuristic config's `ignore_patterns` list is applied as a
///    post-filter using simple glob-style matching (segment-suffix +
///    substring + exact basename).
///
/// Files larger than `config.max_file_size_bytes` are also skipped.
pub fn scan_folder(path: &Path, config: &FolderIngestionConfig) -> Result<ScanResult> {
    let mut subfolders: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut ignored_count = 0usize;

    // Use the `ignore` crate's walker so we pick up any .gitignore or
    // .pyramid-ignore side-files rooted at `path`. Max depth 1 keeps
    // this non-recursive — the caller orchestrates the recursion.
    //
    // `require_git(false)` lets the walker honor a `.gitignore` even
    // when the directory isn't inside a git repo — which is the
    // common case for freshly-cloned or plain project folders the
    // user points at. Without this flag the `ignore` crate would
    // only consult `.gitignore` when it finds a `.git` directory.
    let mut builder = WalkBuilder::new(path);
    builder
        .follow_links(false)
        .hidden(false)
        .max_depth(Some(1))
        .require_git(false)
        .git_ignore(config.respect_gitignore)
        .git_global(config.respect_gitignore)
        .git_exclude(config.respect_gitignore);

    if config.respect_pyramid_ignore {
        // Pyramid-style override file. The `ignore` crate treats it
        // exactly like .gitignore; users can opt a subtree out of the
        // folder walker without touching their git state.
        builder.add_custom_ignore_filename(".pyramid-ignore");
    }

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                ignored_count += 1;
                continue;
            }
        };
        // The walker yields the root itself; skip it.
        if entry.depth() == 0 {
            continue;
        }
        let entry_path = entry.path();

        // Heuristic-level ignore: bundled defaults + any custom
        // patterns from the contribution YAML.
        if path_matches_any_ignore(entry_path, &config.ignore_patterns) {
            ignored_count += 1;
            continue;
        }

        let file_type = entry.file_type();
        if file_type.as_ref().is_some_and(|ft| ft.is_dir()) {
            subfolders.push(entry_path.to_path_buf());
            continue;
        }
        if file_type.as_ref().is_some_and(|ft| ft.is_file()) {
            // Enforce the per-file size cap. fs::metadata may fail on
            // a racy delete — treat that as "skip, count ignored".
            match std::fs::metadata(entry_path) {
                Ok(meta) => {
                    if meta.len() > config.max_file_size_bytes {
                        ignored_count += 1;
                        continue;
                    }
                }
                Err(_) => {
                    ignored_count += 1;
                    continue;
                }
            }
            files.push(entry_path.to_path_buf());
        }
    }

    subfolders.sort();
    files.sort();

    Ok(ScanResult {
        subfolders,
        files,
        ignored_count,
    })
}

/// Match a path against a list of simple glob-style patterns.
///
/// Supported patterns:
/// - `name/` — matches a directory component named `name` (trailing
///   slash signals "this is a directory name, match it anywhere in
///   the path")
/// - `*.ext` — suffix match on the file name's extension
/// - bare name — exact basename match OR contained as a path segment
///
/// This is deliberately conservative: real glob parsing lives in the
/// `ignore` crate which we already used above. The heuristic list is
/// a last-resort filter for patterns the user specifies in YAML.
pub fn path_matches_any_ignore(path: &Path, patterns: &[String]) -> bool {
    let basename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let path_str = path.to_string_lossy();
    for pat in patterns {
        if pat.is_empty() {
            continue;
        }
        // Directory pattern: `name/` — match if any path component
        // equals `name`.
        if let Some(dir_name) = pat.strip_suffix('/') {
            if path.components().any(|c| {
                c.as_os_str()
                    .to_str()
                    .is_some_and(|s| s == dir_name)
            }) {
                return true;
            }
            continue;
        }
        // Extension pattern: `*.ext`.
        if let Some(ext) = pat.strip_prefix("*.") {
            if basename.to_ascii_lowercase().ends_with(&format!(".{}", ext.to_ascii_lowercase()))
            {
                return true;
            }
            continue;
        }
        // Plain name: exact basename OR component match.
        if basename == pat {
            return true;
        }
        if path
            .components()
            .any(|c| c.as_os_str().to_str() == Some(pat.as_str()))
        {
            return true;
        }
        if path_str.contains(pat) {
            return true;
        }
    }
    false
}

// ── Slug generation ───────────────────────────────────────────────────────────

/// Generate a slug from the last 2-3 path segments, joined with
/// dashes and kebab-cased. Resolves collisions against `existing`
/// by appending `-2`, `-3`, … until a free slot is found.
pub fn generate_slug(path: &Path, existing: &HashSet<String>) -> String {
    let segments: Vec<&str> = path
        .components()
        .rev()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|s| !s.is_empty() && *s != "/")
        .take(3)
        .collect();

    // Segments are in reverse order; flip so the slug reads root→leaf.
    let ordered: Vec<&str> = segments.into_iter().rev().collect();

    // Pick the final 2 segments for brevity, padding with 3 only if
    // the pair would collide (handled via suffix resolution below).
    let pick = |n: usize| -> String {
        let start = ordered.len().saturating_sub(n);
        let joined = ordered[start..].join("-");
        super::slug::slugify(&joined)
    };

    let base_two = pick(2);
    let base_three = pick(3);

    let candidates: Vec<String> = if base_two == base_three {
        vec![base_two.clone()]
    } else {
        vec![base_two.clone(), base_three.clone()]
    };

    // First pass: return the first candidate that doesn't collide.
    for candidate in &candidates {
        if candidate.is_empty() {
            continue;
        }
        if !existing.contains(candidate) {
            return candidate.clone();
        }
    }

    // Collision: fall back to 2-segment base with a numeric suffix.
    let fallback_base = if base_two.is_empty() {
        "folder".to_string()
    } else {
        base_two
    };
    for suffix in 2..1000 {
        let candidate = format!("{}-{}", fallback_base, suffix);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    // Give up and return the base — the caller will surface the
    // conflict via the DB-level duplicate check.
    fallback_base
}

// ── Claude Code path encoding ─────────────────────────────────────────────────

/// Encode an absolute path into the form Claude Code uses for its
/// `~/.claude/projects/` directory names. The rule is a simple
/// `/` → `-` substitution, with the leading dash from the root
/// preserved verbatim.
pub fn encode_path_for_claude_code(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// Expand a user-supplied conversation path. Accepts `~` prefixes on
/// Unix and `%USERPROFILE%` on Windows.
pub fn expand_claude_code_projects_root(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if trimmed == "~" {
        return dirs::home_dir();
    }
    Some(PathBuf::from(trimmed))
}

/// Return the list of Claude Code project directories whose encoded
/// name matches the target folder or any of its subfolders.
///
/// Matching uses the exact encoded string OR the encoded string
/// followed by a literal `-`, which is how Claude Code writes
/// subfolder and worktree directories (spec lines 264-269).
pub fn find_claude_code_conversation_dirs(
    target_folder: &Path,
    config: &FolderIngestionConfig,
) -> Vec<PathBuf> {
    let Some(projects_root) = expand_claude_code_projects_root(&config.claude_code_conversation_path)
    else {
        return Vec::new();
    };
    if !projects_root.exists() {
        return Vec::new();
    }

    // Canonicalize the target so encoding is stable regardless of how
    // the caller spelled the path (trailing slash, symlink, etc.).
    let canonical_target = target_folder
        .canonicalize()
        .unwrap_or_else(|_| target_folder.to_path_buf());
    let encoded_target = encode_path_for_claude_code(&canonical_target);
    let prefix = format!("{}-", encoded_target);

    let mut matches: Vec<PathBuf> = Vec::new();
    let Ok(iter) = std::fs::read_dir(&projects_root) else {
        return matches;
    };
    for entry in iter.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if name == encoded_target || name.starts_with(&prefix) {
            matches.push(path);
        }
    }
    matches.sort();
    matches
}

/// Build the user-facing metadata list for the pre-flight IPC. For
/// each matching directory, counts the `*.jsonl` files and reports
/// the earliest/latest modification time if available.
pub fn describe_claude_code_dirs(
    target_folder: &Path,
    config: &FolderIngestionConfig,
) -> Vec<ClaudeCodeConversationDir> {
    let canonical_target = target_folder
        .canonicalize()
        .unwrap_or_else(|_| target_folder.to_path_buf());
    let encoded_target = encode_path_for_claude_code(&canonical_target);
    let matches = find_claude_code_conversation_dirs(target_folder, config);
    let mut out: Vec<ClaudeCodeConversationDir> = Vec::with_capacity(matches.len());

    for dir in matches {
        let encoded_path = dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let absolute_path = dir.to_string_lossy().to_string();
        let is_main = encoded_path == encoded_target;
        let is_worktree = encoded_path.contains("--claude-worktrees-");

        let mut jsonl_count = 0usize;
        let mut earliest: Option<std::time::SystemTime> = None;
        let mut latest: Option<std::time::SystemTime> = None;
        if let Ok(iter) = std::fs::read_dir(&dir) {
            for entry in iter.flatten() {
                let path = entry.path();
                if path.extension().and_then(OsStr::to_str) != Some("jsonl") {
                    continue;
                }
                jsonl_count += 1;
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if earliest.is_none_or(|e| mtime < e) {
                            earliest = Some(mtime);
                        }
                        if latest.is_none_or(|l| mtime > l) {
                            latest = Some(mtime);
                        }
                    }
                }
            }
        }

        let earliest_mtime = earliest.map(system_time_to_iso);
        let latest_mtime = latest.map(system_time_to_iso);

        out.push(ClaudeCodeConversationDir {
            encoded_path,
            absolute_path,
            jsonl_count,
            earliest_mtime,
            latest_mtime,
            is_main,
            is_worktree,
        });
    }
    out
}

fn system_time_to_iso(t: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = t.into();
    datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── Planning ──────────────────────────────────────────────────────────────────

/// Plan an ingestion walk over `target_folder` and return an
/// `IngestionPlan`. This is a pure read-only dry run: no DB writes,
/// no disk writes, just the list of operations the executor would
/// perform.
pub fn plan_ingestion(
    target_folder: &Path,
    config: &FolderIngestionConfig,
    include_claude_code: bool,
) -> Result<IngestionPlan> {
    let root_canonical = target_folder
        .canonicalize()
        .with_context(|| format!("target folder does not exist: {}", target_folder.display()))?;
    if !root_canonical.is_dir() {
        return Err(anyhow!(
            "target path is not a directory: {}",
            root_canonical.display()
        ));
    }

    let mut plan = IngestionPlan {
        operations: Vec::new(),
        root_slug: None,
        root_source_path: root_canonical.to_string_lossy().to_string(),
        total_files: 0,
        total_ignored: 0,
    };
    let mut existing: HashSet<String> = HashSet::new();

    // Recursive walk starting at depth 0 with no parent vine.
    let root_slug = plan_recursive(
        &root_canonical,
        None,
        0,
        config,
        &mut plan,
        &mut existing,
        /*is_top_level=*/ true,
        include_claude_code,
    )?;

    plan.root_slug = root_slug;

    Ok(plan)
}

/// Recursively plan ingestion for `path`.
///
/// Returns the slug assigned to the folder (vine OR leaf pyramid) so
/// the caller can wire an AddChildToVine at the parent level.
#[allow(clippy::too_many_arguments)]
fn plan_recursive(
    path: &Path,
    parent_vine_slug: Option<&str>,
    depth: usize,
    config: &FolderIngestionConfig,
    plan: &mut IngestionPlan,
    existing: &mut HashSet<String>,
    is_top_level: bool,
    include_claude_code: bool,
) -> Result<Option<String>> {
    // Depth cap.
    if depth >= config.max_recursion_depth {
        warn!(
            path = %path.display(),
            depth,
            max_depth = config.max_recursion_depth,
            "folder_ingestion: max recursion depth reached, treating as leaf"
        );
    }

    let scan = scan_folder(path, config)?;
    plan.total_files += scan.files.len();
    plan.total_ignored += scan.ignored_count;

    // At the top level, if Claude Code auto-include is requested and
    // there's at least one matching conversation directory, force
    // the folder to be a topical vine so we have a handle to hang
    // the CC pyramids off of (spec: "all of them are added as
    // bedrocks of the target folder's vine").
    let mut cc_matches: Vec<PathBuf> = Vec::new();
    let force_vine_for_cc = if is_top_level && include_claude_code && config.claude_code_auto_include
    {
        cc_matches = find_claude_code_conversation_dirs(path, config);
        !cc_matches.is_empty()
    } else {
        false
    };

    // Empty directory → nothing to plan. Unless Claude Code has
    // matches, in which case we still create a top-level vine so
    // the CC pyramids have a parent.
    if scan.files.is_empty() && scan.subfolders.is_empty() && !force_vine_for_cc {
        return Ok(None);
    }

    // Below-threshold guard: if the folder has only a handful of files
    // that won't meet `min_files_for_pyramid`, no subfolders, and no CC
    // matches, there's nothing worth creating. Dropping this case
    // avoids emitting an empty vine with no children — which would
    // show up in the wizard preview as a useless "1 vine, 0 pyramids"
    // summary and pollute the slug table.
    if !force_vine_for_cc
        && scan.subfolders.is_empty()
        && scan.files.len() < config.min_files_for_pyramid
    {
        return Ok(None);
    }

    // Leaf case: homogeneous content, no subfolders, above threshold.
    // Disabled at the top level when Claude Code matches are going
    // to be attached — the CC pyramids need a vine parent.
    let leaf_eligible = depth < config.max_recursion_depth
        && scan.subfolders.is_empty()
        && scan.files.len() >= config.min_files_for_pyramid
        && is_homogeneous(&scan.files, config)
        && !force_vine_for_cc;

    if leaf_eligible {
        let content_type = detect_content_type(&scan.files, config)
            .expect("is_homogeneous true → detect_content_type Some");
        let slug = generate_slug(path, existing);
        existing.insert(slug.clone());
        plan.operations.push(IngestionOperation::CreatePyramid {
            slug: slug.clone(),
            content_type: content_type.as_str().to_string(),
            source_path: path.to_string_lossy().to_string(),
        });
        plan.operations
            .push(IngestionOperation::RegisterDadbearConfig {
                slug: slug.clone(),
                source_path: path.to_string_lossy().to_string(),
                content_type: content_type.as_str().to_string(),
                scan_interval_secs: config.default_scan_interval_secs,
            });
        if let Some(parent) = parent_vine_slug {
            plan.operations.push(IngestionOperation::AddChildToVine {
                vine_slug: parent.to_string(),
                child_slug: slug.clone(),
                position: child_position(plan, parent),
                child_type: "bedrock".to_string(),
            });
        }
        return Ok(Some(slug));
    }

    // Vine case: mixed content, subfolders present, or leaf above
    // depth cap. Create a topical vine and recurse into subfolders.
    let vine_slug = generate_slug(path, existing);
    existing.insert(vine_slug.clone());
    plan.operations.push(IngestionOperation::CreateVine {
        slug: vine_slug.clone(),
        source_path: path.to_string_lossy().to_string(),
    });
    if let Some(parent) = parent_vine_slug {
        plan.operations.push(IngestionOperation::AddChildToVine {
            vine_slug: parent.to_string(),
            child_slug: vine_slug.clone(),
            position: child_position(plan, parent),
            child_type: "vine".to_string(),
        });
    }

    // Recurse into subfolders unless we've hit the depth cap.
    if depth < config.max_recursion_depth {
        for subfolder in scan.subfolders {
            let _ = plan_recursive(
                &subfolder,
                Some(&vine_slug),
                depth + 1,
                config,
                plan,
                existing,
                /*is_top_level=*/ false,
                include_claude_code,
            )?;
        }
    }

    // Handle the vine's own loose files, if any. We split by content
    // type so a folder holding e.g. both `.md` notes and `.rs`
    // sources yields one document pyramid + one code pyramid as
    // bedrocks of the same vine instead of forcing another nested
    // level.
    if scan.files.len() >= config.min_files_for_pyramid {
        let groups = group_files_by_type(&scan.files, config);
        for (content_type, group_files) in groups {
            if group_files.len() < config.min_files_for_pyramid {
                continue;
            }
            let synthetic = path.join(format!("_{}", content_type.as_str()));
            let mut files_slug = generate_slug(&synthetic, existing);
            if files_slug.is_empty() {
                files_slug = format!("{}-{}", vine_slug, content_type.as_str());
            }
            existing.insert(files_slug.clone());
            plan.operations.push(IngestionOperation::CreatePyramid {
                slug: files_slug.clone(),
                content_type: content_type.as_str().to_string(),
                source_path: path.to_string_lossy().to_string(),
            });
            plan.operations
                .push(IngestionOperation::RegisterDadbearConfig {
                    slug: files_slug.clone(),
                    source_path: path.to_string_lossy().to_string(),
                    content_type: content_type.as_str().to_string(),
                    scan_interval_secs: config.default_scan_interval_secs,
                });
            plan.operations.push(IngestionOperation::AddChildToVine {
                vine_slug: vine_slug.clone(),
                child_slug: files_slug,
                position: child_position(plan, &vine_slug),
                child_type: "bedrock".to_string(),
            });
        }
    }

    // Claude Code auto-include runs ONLY at the top-level call. The
    // encoded-path prefix match covers subfolders, so we don't need
    // to revisit it at deeper recursion levels (spec lines 294-296).
    // `cc_matches` was populated up front so the scan happens once.
    if is_top_level && include_claude_code && config.claude_code_auto_include {
        for (idx, cc_dir) in cc_matches.iter().enumerate() {
            let encoded = cc_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("cc-unknown");
            let is_main = {
                let canonical_target =
                    path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                encode_path_for_claude_code(&canonical_target) == encoded
            };
            let is_worktree = encoded.contains("--claude-worktrees-");

            // Build a stable, non-colliding slug for the CC pyramid.
            let suffix = format!("cc-{}", idx + 1);
            let base_slug = format!("{}-{}", vine_slug, suffix);
            let mut cc_slug = super::slug::slugify(&base_slug);
            if cc_slug.is_empty() {
                cc_slug = format!("cc-{}", idx + 1);
            }
            // Collision resolution if the caller already minted an
            // identical slug for something else.
            let mut dedup_suffix = 2usize;
            while existing.contains(&cc_slug) {
                cc_slug = super::slug::slugify(&format!("{}-{}", base_slug, dedup_suffix));
                dedup_suffix += 1;
            }
            existing.insert(cc_slug.clone());

            let cc_path = cc_dir.to_string_lossy().to_string();
            plan.operations
                .push(IngestionOperation::RegisterClaudeCodePyramid {
                    slug: cc_slug.clone(),
                    source_path: cc_path.clone(),
                    is_main,
                    is_worktree,
                });
            plan.operations
                .push(IngestionOperation::RegisterDadbearConfig {
                    slug: cc_slug.clone(),
                    source_path: cc_path,
                    content_type: ContentType::Conversation.as_str().to_string(),
                    scan_interval_secs: config.default_scan_interval_secs,
                });
            plan.operations.push(IngestionOperation::AddChildToVine {
                vine_slug: vine_slug.clone(),
                child_slug: cc_slug,
                position: child_position(plan, &vine_slug),
                child_type: "bedrock".to_string(),
            });
        }
    }

    Ok(Some(vine_slug))
}

/// Group a file list by detected single-file content type. Returns
/// content types in a stable order so the generated slugs are
/// deterministic across runs.
fn group_files_by_type(
    files: &[PathBuf],
    config: &FolderIngestionConfig,
) -> Vec<(ContentType, Vec<PathBuf>)> {
    let mut code: Vec<PathBuf> = Vec::new();
    let mut docs: Vec<PathBuf> = Vec::new();
    let mut convos: Vec<PathBuf> = Vec::new();
    for f in files {
        let Some(ext) = lowercase_extension(f) else {
            continue;
        };
        if ext == ".jsonl" {
            convos.push(f.clone());
            continue;
        }
        if config.code_extensions.iter().any(|e| e == &ext) {
            code.push(f.clone());
        } else if config.document_extensions.iter().any(|e| e == &ext) {
            docs.push(f.clone());
        }
    }
    let mut out: Vec<(ContentType, Vec<PathBuf>)> = Vec::new();
    if !code.is_empty() {
        out.push((ContentType::Code, code));
    }
    if !docs.is_empty() {
        out.push((ContentType::Document, docs));
    }
    if !convos.is_empty() {
        out.push((ContentType::Conversation, convos));
    }
    out
}

/// Count the existing AddChildToVine ops for a given parent vine, so
/// the next child lands at position `n`.
fn child_position(plan: &IngestionPlan, vine_slug: &str) -> i32 {
    plan.operations
        .iter()
        .filter(|op| matches!(op, IngestionOperation::AddChildToVine { vine_slug: v, .. } if v == vine_slug))
        .count() as i32
}

// ── Execution ─────────────────────────────────────────────────────────────────

/// Execute an ingestion plan against the live pyramid state. Each
/// operation is idempotent and failures on individual ops are
/// logged to `IngestionResult::errors` rather than aborting the
/// whole plan — a partially-applied plan is still useful to the
/// user and safer than a full-or-nothing rollback at this scale.
pub async fn execute_plan(
    state: &PyramidState,
    plan: IngestionPlan,
) -> Result<IngestionResult> {
    let mut result = IngestionResult {
        root_slug: plan.root_slug.clone(),
        ..Default::default()
    };

    let conn = state.writer.lock().await;

    for op in plan.operations {
        match op {
            IngestionOperation::CreatePyramid {
                slug,
                content_type,
                source_path,
            } => {
                let Some(ct) = ContentType::from_str(&content_type) else {
                    result
                        .errors
                        .push(format!("unknown content_type '{}' for {}", content_type, slug));
                    continue;
                };
                match db::create_slug(&conn, &slug, &ct, &source_path) {
                    Ok(_) => result.pyramids_created.push(slug),
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("already exists") {
                            // Idempotent: treat existing slugs as success.
                            result.pyramids_created.push(slug);
                        } else {
                            result.errors.push(format!("create_slug {}: {}", slug, msg));
                        }
                    }
                }
            }
            IngestionOperation::CreateVine { slug, source_path } => {
                match db::create_slug(&conn, &slug, &ContentType::Vine, &source_path) {
                    Ok(_) => result.vines_created.push(slug),
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("already exists") {
                            result.vines_created.push(slug);
                        } else {
                            result.errors.push(format!("create_vine {}: {}", slug, msg));
                        }
                    }
                }
            }
            IngestionOperation::AddChildToVine {
                vine_slug,
                child_slug,
                position,
                child_type,
            } => {
                match db::insert_vine_composition(
                    &conn,
                    &vine_slug,
                    &child_slug,
                    position,
                    &child_type,
                ) {
                    Ok(_) => result.compositions_added += 1,
                    Err(e) => result.errors.push(format!(
                        "insert_vine_composition {}→{}: {}",
                        vine_slug, child_slug, e
                    )),
                }
            }
            IngestionOperation::RegisterDadbearConfig {
                slug,
                source_path,
                content_type,
                scan_interval_secs,
            } => {
                let config = DadbearWatchConfig {
                    id: 0,
                    slug: slug.clone(),
                    source_path,
                    content_type,
                    scan_interval_secs,
                    debounce_secs: 30,
                    session_timeout_secs: 1800,
                    batch_size: 1,
                    enabled: true,
                    created_at: String::new(),
                    updated_at: String::new(),
                };
                match db::save_dadbear_config(&conn, &config) {
                    Ok(_) => result.dadbear_configs.push(slug),
                    Err(e) => result
                        .errors
                        .push(format!("save_dadbear_config {}: {}", slug, e)),
                }
            }
            IngestionOperation::RegisterClaudeCodePyramid {
                slug,
                source_path,
                is_main,
                is_worktree,
            } => {
                match db::create_slug(&conn, &slug, &ContentType::Conversation, &source_path) {
                    Ok(_) => {
                        info!(
                            slug = %slug,
                            source_path = %source_path,
                            is_main,
                            is_worktree,
                            "folder_ingestion: created Claude Code conversation pyramid"
                        );
                        result.claude_code_pyramids.push(slug);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("already exists") {
                            result.claude_code_pyramids.push(slug);
                        } else {
                            result
                                .errors
                                .push(format!("create_cc_pyramid {}: {}", slug, msg));
                        }
                    }
                }
            }
        }
    }

    drop(conn);
    info!(
        root_slug = ?result.root_slug,
        pyramids = result.pyramids_created.len(),
        vines = result.vines_created.len(),
        cc_pyramids = result.claude_code_pyramids.len(),
        dadbear_configs = result.dadbear_configs.len(),
        compositions = result.compositions_added,
        errors = result.errors.len(),
        "folder_ingestion: plan execution complete"
    );

    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod phase17_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn default_config() -> FolderIngestionConfig {
        FolderIngestionConfig::default()
    }

    fn make_dir(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_file(dir: &Path, name: &str, size_bytes: usize) {
        let content = "a".repeat(size_bytes);
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn test_detect_content_type_homogeneous_code() {
        let files = vec![
            PathBuf::from("lib.rs"),
            PathBuf::from("main.rs"),
            PathBuf::from("util.rs"),
        ];
        let config = default_config();
        assert_eq!(detect_content_type(&files, &config), Some(ContentType::Code));
    }

    #[test]
    fn test_detect_content_type_homogeneous_document() {
        let files = vec![
            PathBuf::from("a.md"),
            PathBuf::from("b.md"),
            PathBuf::from("c.txt"),
        ];
        let config = default_config();
        assert_eq!(
            detect_content_type(&files, &config),
            Some(ContentType::Document)
        );
    }

    #[test]
    fn test_detect_content_type_mixed_returns_none() {
        let files = vec![
            PathBuf::from("a.md"),
            PathBuf::from("b.md"),
            PathBuf::from("c.rs"),
            PathBuf::from("d.rs"),
        ];
        let config = default_config();
        assert_eq!(detect_content_type(&files, &config), None);
    }

    #[test]
    fn test_detect_content_type_conversation_jsonl_wins() {
        let files = vec![
            PathBuf::from("sess-1.jsonl"),
            PathBuf::from("sess-2.jsonl"),
            PathBuf::from("sess-3.jsonl"),
        ];
        let config = default_config();
        assert_eq!(
            detect_content_type(&files, &config),
            Some(ContentType::Conversation)
        );
    }

    #[test]
    fn test_detect_content_type_ignores_unclassified() {
        let files = vec![
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
            PathBuf::from("c.rs"),
            PathBuf::from("d.png"),
            PathBuf::from("e.svg"),
        ];
        let config = default_config();
        assert_eq!(detect_content_type(&files, &config), Some(ContentType::Code));
    }

    #[test]
    fn test_scan_folder_respects_gitignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "a.rs", 10);
        make_file(root, "b.rs", 10);
        make_file(root, "secret.key", 10);
        fs::write(root.join(".gitignore"), "*.key\n").unwrap();

        let config = default_config();
        let scan = scan_folder(root, &config).unwrap();
        let names: Vec<String> = scan
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(!names.contains(&"secret.key".to_string()));
        assert!(names.contains(&"a.rs".to_string()));
        assert!(names.contains(&"b.rs".to_string()));
    }

    #[test]
    fn test_scan_folder_respects_pyramid_ignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "a.rs", 10);
        make_file(root, "ignored.rs", 10);
        fs::write(root.join(".pyramid-ignore"), "ignored.rs\n").unwrap();

        let config = default_config();
        let scan = scan_folder(root, &config).unwrap();
        let names: Vec<String> = scan
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(!names.contains(&"ignored.rs".to_string()));
        assert!(names.contains(&"a.rs".to_string()));
    }

    #[test]
    fn test_scan_folder_skips_large_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "small.rs", 10);
        make_file(root, "huge.rs", 2000);
        let config = FolderIngestionConfig {
            max_file_size_bytes: 100,
            ..default_config()
        };
        let scan = scan_folder(root, &config).unwrap();
        let names: Vec<String> = scan
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"small.rs".to_string()));
        assert!(!names.contains(&"huge.rs".to_string()));
        assert!(scan.ignored_count >= 1);
    }

    #[test]
    fn test_scan_folder_lists_subfolders() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_dir(root, "src");
        make_dir(root, "docs");
        make_file(root, "a.rs", 10);
        let scan = scan_folder(root, &default_config()).unwrap();
        assert_eq!(scan.subfolders.len(), 2);
        assert_eq!(scan.files.len(), 1);
    }

    #[test]
    fn test_generate_slug_kebab_cases_path_segments() {
        let path = PathBuf::from("/Users/adam/AI Project Files/GoodNewsEveryone/src");
        let existing = HashSet::new();
        let slug = generate_slug(&path, &existing);
        assert_eq!(slug, "goodnewseveryone-src");
    }

    #[test]
    fn test_generate_slug_handles_collision_with_suffix() {
        let path = PathBuf::from("/a/b/src");
        let mut existing = HashSet::new();
        existing.insert("b-src".to_string());
        existing.insert("a-b-src".to_string());
        let slug = generate_slug(&path, &existing);
        assert!(slug.starts_with("b-src-") || slug.starts_with("a-b-src-"));
    }

    #[test]
    fn test_encode_path_for_claude_code() {
        let path = PathBuf::from("/Users/adam/AI Project Files/agent-wire-node");
        assert_eq!(
            encode_path_for_claude_code(&path),
            "-Users-adam-AI Project Files-agent-wire-node"
        );
    }

    #[test]
    fn test_find_claude_code_conversation_dirs_matches_encoded_target() {
        let tmp = TempDir::new().unwrap();
        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();

        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);

        // Create a matching CC directory.
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        fs::write(cc_dir.join("sess-1.jsonl"), "{}").unwrap();

        // Unrelated directory — must not match.
        let other_dir = cc_root.join("-unrelated-repo");
        fs::create_dir_all(&other_dir).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], cc_dir);
    }

    #[test]
    fn test_find_claude_code_conversation_dirs_matches_subfolders_via_prefix() {
        let tmp = TempDir::new().unwrap();
        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();

        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);

        // Create a subfolder match using the prefix rule.
        let sub_dir = cc_root.join(format!("{}-docs-architecture", encoded));
        fs::create_dir_all(&sub_dir).unwrap();
        // And the worktree form.
        let worktree_dir = cc_root.join(format!("{}--claude-worktrees-nervous-lichterman", encoded));
        fs::create_dir_all(&worktree_dir).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&sub_dir));
        assert!(matches.contains(&worktree_dir));
    }

    #[test]
    fn test_plan_ingestion_single_level_homogeneous() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("my-code");
        fs::create_dir_all(&root).unwrap();
        make_file(&root, "a.rs", 10);
        make_file(&root, "b.rs", 10);
        make_file(&root, "c.rs", 10);

        let plan = plan_ingestion(&root, &default_config(), false).unwrap();
        // One CreatePyramid + one RegisterDadbearConfig (no parent vine).
        let creates: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreatePyramid { .. }))
            .collect();
        assert_eq!(creates.len(), 1);
        let dadbear: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::RegisterDadbearConfig { .. }))
            .collect();
        assert_eq!(dadbear.len(), 1);
        assert!(plan
            .operations
            .iter()
            .all(|o| !matches!(o, IngestionOperation::CreateVine { .. })));
    }

    #[test]
    fn test_plan_ingestion_mixed_folder_creates_vine() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("my-mixed");
        fs::create_dir_all(&root).unwrap();
        // Subfolders trigger vine creation regardless of homogeneity.
        let src = root.join("src");
        let docs = root.join("docs");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&docs).unwrap();
        make_file(&src, "a.rs", 10);
        make_file(&src, "b.rs", 10);
        make_file(&src, "c.rs", 10);
        make_file(&docs, "a.md", 10);
        make_file(&docs, "b.md", 10);
        make_file(&docs, "c.md", 10);

        let plan = plan_ingestion(&root, &default_config(), false).unwrap();
        let vines: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreateVine { .. }))
            .collect();
        assert_eq!(vines.len(), 1, "expected a root topical vine");
        let pyramids: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreatePyramid { .. }))
            .collect();
        assert_eq!(pyramids.len(), 2, "expected two child pyramids");
        let compositions: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::AddChildToVine { .. }))
            .collect();
        assert_eq!(compositions.len(), 2);
    }

    #[test]
    fn test_plan_ingestion_recursive_multi_level() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("multi");
        fs::create_dir_all(&root).unwrap();
        let a = root.join("a");
        let b = root.join("b");
        let c = b.join("c");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::create_dir_all(&c).unwrap();
        make_file(&a, "a1.rs", 10);
        make_file(&a, "a2.rs", 10);
        make_file(&a, "a3.rs", 10);
        // b has its own subfolder c, so b becomes a vine, c becomes a pyramid.
        make_file(&c, "c1.rs", 10);
        make_file(&c, "c2.rs", 10);
        make_file(&c, "c3.rs", 10);

        let plan = plan_ingestion(&root, &default_config(), false).unwrap();
        let vines: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreateVine { .. }))
            .collect();
        // root + b (both have subfolders / mixed)
        assert!(
            vines.len() >= 2,
            "expected at least two vines, got {}",
            vines.len()
        );
        let pyramids: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreatePyramid { .. }))
            .collect();
        assert!(
            pyramids.len() >= 2,
            "expected at least two pyramids (a, c)"
        );
    }

    #[test]
    fn test_plan_ingestion_respects_max_recursion_depth() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("depth");
        fs::create_dir_all(&root).unwrap();
        // Create a 5-deep path with code at the bottom.
        let mut current = root.clone();
        for level in 0..5 {
            current = current.join(format!("lvl{}", level));
            fs::create_dir_all(&current).unwrap();
        }
        make_file(&current, "a.rs", 10);
        make_file(&current, "b.rs", 10);
        make_file(&current, "c.rs", 10);

        let config = FolderIngestionConfig {
            max_recursion_depth: 2,
            ..default_config()
        };
        // Should still succeed — deep levels just get treated as
        // vines instead of recursing further.
        let plan = plan_ingestion(&root, &config, false).unwrap();
        assert!(!plan.operations.is_empty());
    }

    #[test]
    fn test_plan_ingestion_skips_below_threshold_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("tiny");
        fs::create_dir_all(&root).unwrap();
        make_file(&root, "only.rs", 10);

        let config = FolderIngestionConfig {
            min_files_for_pyramid: 3,
            ..default_config()
        };
        let plan = plan_ingestion(&root, &config, false).unwrap();
        // Below threshold + no subfolders + no CC → nothing to plan.
        // The walker refuses to emit an empty vine in this case so
        // the wizard preview doesn't show a useless "1 vine, 0
        // pyramids" summary.
        let pyramids: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreatePyramid { .. }))
            .collect();
        assert_eq!(pyramids.len(), 0);
        let vines: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreateVine { .. }))
            .collect();
        assert_eq!(vines.len(), 0);
        assert!(plan.root_slug.is_none());
    }

    #[test]
    fn test_plan_ingestion_with_claude_code_attaches_cc_pyramids() {
        let tmp = TempDir::new().unwrap();
        // Build a minimal filesystem layout. The target repo lives
        // inside tmp, and so does the fake Claude Code projects root.
        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        make_file(&target, "a.md", 10);
        make_file(&target, "b.md", 10);
        make_file(&target, "c.md", 10);

        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        fs::write(cc_dir.join("sess-1.jsonl"), "{}").unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };

        let plan = plan_ingestion(&target, &config, true).unwrap();
        let cc_ops: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::RegisterClaudeCodePyramid { .. }))
            .collect();
        assert_eq!(cc_ops.len(), 1, "expected one CC pyramid op");
    }

    /// Regression test for the verifier-pass fix: below-threshold folder
    /// but WITH Claude Code matches must still create a top-level vine
    /// so the CC pyramids have a parent. Without the `force_vine_for_cc`
    /// bypass, the empty-vine guard would drop the CC pyramids on the
    /// floor.
    #[test]
    fn test_plan_ingestion_below_threshold_with_cc_still_creates_vine() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("solo");
        fs::create_dir_all(&target).unwrap();
        // Single file — below the default min_files_for_pyramid (3).
        make_file(&target, "only.md", 10);

        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        fs::write(cc_dir.join("sess-1.jsonl"), "{}").unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };

        let plan = plan_ingestion(&target, &config, true).unwrap();

        let vines: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::CreateVine { .. }))
            .collect();
        assert_eq!(vines.len(), 1, "below-threshold + CC must still create a vine");

        let cc_ops: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| matches!(o, IngestionOperation::RegisterClaudeCodePyramid { .. }))
            .collect();
        assert_eq!(cc_ops.len(), 1, "CC pyramid must still be attached");

        assert!(plan.root_slug.is_some(), "root_slug must be set for CC attachment");
    }

    /// Regression test for the verifier-pass fix: below-threshold folder
    /// with NO subfolders and NO CC matches should short-circuit to an
    /// empty plan instead of emitting a childless vine.
    #[test]
    fn test_plan_ingestion_below_threshold_no_cc_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("tiny-no-cc");
        fs::create_dir_all(&root).unwrap();
        make_file(&root, "one.rs", 10);
        make_file(&root, "two.rs", 10); // 2 files, below default threshold of 3

        // Redirect Claude Code path to an empty temp dir so the scan
        // can't accidentally pick up real `~/.claude/projects` hits.
        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };

        let plan = plan_ingestion(&root, &config, true).unwrap();
        assert!(
            plan.operations.is_empty(),
            "expected empty plan, got {:?}",
            plan.operations
        );
        assert!(plan.root_slug.is_none());
    }

    #[test]
    fn test_path_matches_any_ignore_directory_pattern() {
        let path = PathBuf::from("/repo/node_modules/foo/index.js");
        let patterns = vec!["node_modules/".to_string()];
        assert!(path_matches_any_ignore(&path, &patterns));
    }

    #[test]
    fn test_path_matches_any_ignore_extension_pattern() {
        let path = PathBuf::from("/repo/pkg.lock");
        let patterns = vec!["*.lock".to_string()];
        assert!(path_matches_any_ignore(&path, &patterns));
    }

    #[test]
    fn test_expand_claude_code_projects_root_tilde() {
        if let Some(home) = dirs::home_dir() {
            let expanded = expand_claude_code_projects_root("~/.claude/projects").unwrap();
            assert_eq!(expanded, home.join(".claude").join("projects"));
        }
    }
}
