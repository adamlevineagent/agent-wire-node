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
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::db::{self, FolderIngestionConfig};
use super::types::{ContentType, DadbearWatchConfig};
use super::PyramidState;

// ── Public types ──────────────────────────────────────────────────────────────

/// A single unit of work in an ingestion plan.
///
/// Phase 18e (D1) retired the `RegisterClaudeCodePyramid` variant
/// (Option A from the workstream prompt). Phase 17 emitted a single
/// `RegisterClaudeCodePyramid` per CC dir; Phase 18e replaces that
/// with a mini-subplan made of the existing primitives:
///
///   1. `CreateVine`             — the CC vine slug
///   2. `CreatePyramid`          — the conversation bedrock
///   3. `AddChildToVine`         — attach the conversation bedrock to
///                                  the CC vine (`child_type='bedrock'`)
///   4. `RegisterDadbearConfig`  — DADBEAR for the conversation pyramid
///   5. `CreatePyramid` (opt.)   — the memory document bedrock
///   6. `AddChildToVine` (opt.)  — attach memory bedrock to CC vine
///   7. `RegisterDadbearConfig` (opt.) — DADBEAR for the memory pyramid
///   8. `AddChildToVine`         — attach the CC vine to the root vine
///                                  with `child_type='vine'`
///
/// Step 8 makes the CC vine a peer of every other folder child of the
/// root vine, exactly per Phase 16's vine-of-vines composition pattern.
/// `RegisterClaudeCodePyramid` had no external callers (Phase 17 was
/// less than two weeks old at the time of retirement), so the variant
/// was deleted outright rather than carried as a deprecation shim.
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
}

/// A complete ingestion plan. Returned by `plan_ingestion`.
///
/// Phase 18e adds three classification sets so the executor can
/// surface CC vine/bedrock counts in the result without re-parsing
/// the operation list. The sets are populated during planning and
/// stay synchronized with the actual ops emitted by `plan_recursive`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestionPlan {
    pub operations: Vec<IngestionOperation>,
    pub root_slug: Option<String>,
    pub root_source_path: String,
    pub total_files: usize,
    pub total_ignored: usize,
    /// Phase 18e: slugs the planner emitted as CC vines (one per CC
    /// dir). Listed in the operation list as `CreateVine`.
    #[serde(default)]
    pub claude_code_vine_slugs: Vec<String>,
    /// Phase 18e: slugs the planner emitted as CC conversation
    /// bedrocks. One per CC dir; listed as `CreatePyramid` with
    /// `content_type = conversation`.
    #[serde(default)]
    pub claude_code_conversation_slugs: Vec<String>,
    /// Phase 18e: slugs the planner emitted as CC memory document
    /// bedrocks. Zero or one per CC dir; listed as `CreatePyramid`
    /// with `content_type = document`.
    #[serde(default)]
    pub claude_code_memory_slugs: Vec<String>,
}

/// Result of executing an ingestion plan. Lists what was actually
/// created so the UI can render a post-commit summary.
///
/// Phase 18e additions: `claude_code_vines`,
/// `claude_code_conversation_pyramids`, and
/// `claude_code_memory_pyramids` so the wizard can break the CC
/// summary into "CC vines / conversation beds / memory beds". The
/// legacy `claude_code_pyramids` field stays around as the union of
/// the conversation + memory bedrocks to keep older callers happy
/// during the same release.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestionResult {
    pub pyramids_created: Vec<String>,
    pub vines_created: Vec<String>,
    pub dadbear_configs: Vec<String>,
    /// Phase 17 / Phase 18e: every conversation OR memory bedrock
    /// created from a Claude Code dir. Useful for the legacy
    /// "CC pyramids: N" summary line.
    pub claude_code_pyramids: Vec<String>,
    /// Phase 18e: only the conversation bedrock slugs.
    #[serde(default)]
    pub claude_code_conversation_pyramids: Vec<String>,
    /// Phase 18e: only the memory document bedrock slugs.
    #[serde(default)]
    pub claude_code_memory_pyramids: Vec<String>,
    /// Phase 18e: the CC vine slugs themselves (one per CC dir,
    /// independent of how many bedrocks the vine ended up containing).
    #[serde(default)]
    pub claude_code_vines: Vec<String>,
    pub compositions_added: usize,
    pub root_slug: Option<String>,
    pub errors: Vec<String>,
}

/// Shape returned by `pyramid_find_claude_code_conversations`.
///
/// Phase 18e (D1) extends this with memory-subfolder metadata so the
/// wizard preview and the planner both know whether a CC dir has a
/// `memory/` subfolder full of `.md` files. A CC dir with a populated
/// memory subfolder produces a memory document bedrock alongside the
/// conversation bedrock during ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCodeConversationDir {
    pub encoded_path: String,
    pub absolute_path: String,
    pub jsonl_count: usize,
    pub earliest_mtime: Option<String>,
    pub latest_mtime: Option<String>,
    pub is_main: bool,
    pub is_worktree: bool,
    /// Phase 18e: true when `{absolute_path}/memory` exists as a
    /// subdirectory. Independent of whether it has any `.md` files —
    /// callers should check `memory_md_count > 0` before deciding to
    /// emit a memory bedrock.
    #[serde(default)]
    pub has_memory_subfolder: bool,
    /// Phase 18e: count of `.md` files anywhere under the `memory/`
    /// subfolder (recursive). Zero when the subfolder is missing or
    /// empty. The recursive walk matches `ingest_docs`'s walker, which
    /// the memory bedrock pre-pop ultimately uses.
    #[serde(default)]
    pub memory_md_count: usize,
    /// Phase 18e: absolute path to `{absolute_path}/memory` when the
    /// subfolder exists, otherwise None. The planner uses this as the
    /// `source_path` for the memory document pyramid + DADBEAR config.
    #[serde(default)]
    pub memory_subfolder_path: Option<String>,
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
///    `.pyramid-ignore` override file is honored via
///    `WalkBuilder::add_custom_ignore_filename`. Note: `.wireignore`
///    is NOT honored here — it's used by the separate
///    `sync.rs::scan_local_folder` path. Unifying the three ignore
///    systems is tracked as Bug #16.
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
    let basename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let path_str = path.to_string_lossy();
    for pat in patterns {
        if pat.is_empty() {
            continue;
        }
        // Directory pattern: `name/` — match if any path component
        // equals `name`.
        if let Some(dir_name) = pat.strip_suffix('/') {
            if path
                .components()
                .any(|c| c.as_os_str().to_str().is_some_and(|s| s == dir_name))
            {
                return true;
            }
            continue;
        }
        // Extension pattern: `*.ext`.
        if let Some(ext) = pat.strip_prefix("*.") {
            if basename
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", ext.to_ascii_lowercase()))
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
/// `~/.claude/projects/` directory names.
///
/// **Phase 18a follow-up (2026-04-11):** the original Phase 17
/// implementation was `replace('/', '-')`, which only handled slashes.
/// That produced `/Users/adam/AI Project Files/foo` →
/// `-Users-adam-AI Project Files-foo` (spaces and dots preserved),
/// but Claude Code actually writes `-Users-adam-AI-Project-Files-foo`
/// (every non-alphanumeric run collapsed to a dash).
///
/// The `/.claude/worktrees/` idiom is what forced us to notice:
/// `.../GoodNewsEveryone/.claude/worktrees/...` on disk is
/// `...GoodNewsEveryone--claude-worktrees-...` — note the double dash
/// where the `/.claude` segment sits. That `--` is the `/` → `-` AND
/// the `.` → `-`, not a placeholder for something else.
///
/// Empirical rule, verified against every entry in one user's
/// `~/.claude/projects/`: any character that isn't an ASCII letter,
/// digit, or existing dash becomes a dash. Runs of dashes are NOT
/// collapsed — `/.` → `--` is load-bearing for the matching logic
/// (otherwise `foo--bar` and `foo-bar` couldn't be distinguished).
///
/// Before this fix, every target folder whose absolute path
/// contained a space (like Adam's `/Users/adamlevine/AI Project Files`)
/// silently failed the Claude Code match-check in the folder
/// ingestion wizard, leaving the "Include Claude Code conversations"
/// checkbox disabled even when matching conversation directories
/// existed on disk. The bug has been latent since Phase 17 shipped.
pub fn encode_path_for_claude_code(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
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

/// Return the list of Claude Code conversation directories that
/// should be attached to the target folder's ingestion.
///
/// Two detection patterns are supported:
///
/// **Pattern A (encoded-subdir root):** the scan location is a root
/// directory like `~/.claude/projects/` that contains encoded-path
/// subdirectories (e.g. `-Users-adam-my-project`). The function
/// scans the root's direct children and returns every subdirectory
/// whose name matches `encode_path_for_claude_code(target_folder)`
/// exactly OR starts with that encoded string followed by a `-`
/// (subfolder / worktree prefix match, per spec lines 264-269).
///
/// **Pattern B (direct conversation folder):** the scan location IS
/// the conversation folder — it directly contains one or more
/// `*.jsonl` files (and optionally a `memory/` subfolder with
/// `*.md` files). Common when the user has exported a conversation
/// history to an arbitrary directory and wants to attach it to the
/// target folder's pyramid. When Pattern B detects jsonls at the
/// scan location's top level, it returns the scan location itself
/// as a single match — callers see one `ClaudeCodeConversationDir`
/// with `is_main = true` and the scan path as both `encoded_path`
/// and `absolute_path`.
///
/// Pattern A is checked first. Pattern B kicks in ONLY when
/// Pattern A finds zero matches — otherwise a user pointing at
/// `~/.claude/projects/` (which contains jsonls in subdirs but
/// none at its own top level) wouldn't be affected.
///
/// Phase 18a follow-up (2026-04-11): Pattern B added to support
/// users whose conversation histories live outside Claude Code's
/// canonical `~/.claude/projects/` tree — the Change… picker in
/// the wizard now accepts either a root-with-encoded-subdirs or a
/// direct-jsonl folder.
pub fn find_claude_code_conversation_dirs(
    target_folder: &Path,
    config: &FolderIngestionConfig,
) -> Vec<PathBuf> {
    let Some(projects_root) =
        expand_claude_code_projects_root(&config.claude_code_conversation_path)
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

    // ── Pattern A: look for encoded-path subdirs inside the root ───────
    let mut matches: Vec<PathBuf> = Vec::new();
    if let Ok(iter) = std::fs::read_dir(&projects_root) {
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
    }

    if !matches.is_empty() {
        matches.sort();
        return matches;
    }

    // ── Pattern B: the scan root IS a conversation folder ────────────
    // When Pattern A finds nothing, check whether the scan root itself
    // looks like a direct conversation folder: has one or more jsonl
    // files at its top level. If so, return the scan root as a single
    // match. Otherwise return empty.
    //
    // We use the canonicalized form of the scan root so downstream
    // consumers compare paths consistently.
    let canonical_root = projects_root
        .canonicalize()
        .unwrap_or_else(|_| projects_root.clone());
    if directly_contains_jsonls(&canonical_root) {
        matches.push(canonical_root);
    }

    matches
}

/// Pattern B detection helper: returns true if the given directory
/// contains at least one `*.jsonl` file at its top level. Does NOT
/// recurse into subdirectories — callers who want recursive counts
/// should use `count_cc_jsonl_files`. Hidden files are ignored per
/// the convention used by `count_memory_md_files`.
fn directly_contains_jsonls(dir: &Path) -> bool {
    let Ok(iter) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in iter.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_file() && path.extension().and_then(OsStr::to_str) == Some("jsonl") {
            return true;
        }
    }
    false
}

/// Build the user-facing metadata list for the pre-flight IPC. For
/// each matching directory, counts the `*.jsonl` files and reports
/// the earliest/latest modification time if available.
///
/// Phase 18e: also populates the `memory/` subfolder metadata so
/// downstream callers (the wizard preview, the planner) can decide
/// whether to emit a memory document bedrock for the CC vine.
pub fn describe_claude_code_dirs(
    target_folder: &Path,
    config: &FolderIngestionConfig,
) -> Vec<ClaudeCodeConversationDir> {
    let canonical_target = target_folder
        .canonicalize()
        .unwrap_or_else(|_| target_folder.to_path_buf());
    let encoded_target = encode_path_for_claude_code(&canonical_target);

    // Canonicalize the configured scan root so we can compare it
    // against each match's path to detect Pattern B (the scan root
    // IS the conversation folder, not a parent containing encoded
    // subdirs).
    let pattern_b_root = expand_claude_code_projects_root(&config.claude_code_conversation_path)
        .and_then(|p| p.canonicalize().ok());

    let matches = find_claude_code_conversation_dirs(target_folder, config);
    let mut out: Vec<ClaudeCodeConversationDir> = Vec::with_capacity(matches.len());

    for dir in matches {
        let is_pattern_b = pattern_b_root
            .as_ref()
            .map(|root| &dir == root)
            .unwrap_or(false);

        let encoded_path = dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let absolute_path = dir.to_string_lossy().to_string();
        // Pattern B: the scan root IS the conversation folder, so
        // by definition it's the "main" one (there's only one), and
        // it's never a worktree-shaped name.
        // Pattern A: use the encoded-name match.
        let is_main = if is_pattern_b {
            true
        } else {
            encoded_path == encoded_target
        };
        let is_worktree = !is_pattern_b && encoded_path.contains("--claude-worktrees-");

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

        // ── Phase 18e: memory subfolder discovery ────────────────────
        // Claude Code's per-project memory lives at `<cc_dir>/memory/`
        // and may contain arbitrarily-nested markdown files. We probe
        // the subfolder once here so the wizard preview and the
        // planner can both make decisions based on a single scan.
        let memory_dir = dir.join("memory");
        let (has_memory_subfolder, memory_md_count, memory_subfolder_path) = if memory_dir.is_dir()
        {
            let count = count_memory_md_files(&memory_dir);
            let path_str = memory_dir.to_string_lossy().to_string();
            (true, count, Some(path_str))
        } else {
            (false, 0usize, None)
        };

        out.push(ClaudeCodeConversationDir {
            encoded_path,
            absolute_path,
            jsonl_count,
            earliest_mtime,
            latest_mtime,
            is_main,
            is_worktree,
            has_memory_subfolder,
            memory_md_count,
            memory_subfolder_path,
        });
    }
    out
}

/// Phase 18e wanderer: count `*.jsonl` files in a CC dir
/// (non-recursive). Used by the planner to decide whether to emit the
/// conversation subplan ops for a given CC dir. Matches the shape of
/// `describe_claude_code_dirs`'s jsonl probe so the planner and the
/// wizard preview agree on when a CC dir is "conversation-populated."
pub(crate) fn count_cc_jsonl_files(cc_dir: &Path) -> usize {
    let Ok(iter) = std::fs::read_dir(cc_dir) else {
        return 0;
    };
    iter.flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        })
        .count()
}

/// Phase 18e: recursively count `.md` files under a CC `memory/`
/// directory. Mirrors what `ingest_docs` walks: it descends into all
/// subdirectories, skips hidden directories (matching the
/// `walk_dir(skip_hidden = true)` call inside `ingest_docs`), and
/// counts files whose extension is exactly `.md` (case-insensitive).
///
/// The count is conservative — `ingest_docs` itself classifies a
/// broader set of doc extensions, but the wizard label says
/// "memory md" so we only count `.md` to keep the displayed number
/// honest. The actual ingest at build time will pick up whatever
/// `ingest_docs` finds.
pub(crate) fn count_memory_md_files(memory_dir: &Path) -> usize {
    let mut total = 0usize;
    let mut stack: Vec<PathBuf> = vec![memory_dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name_os = entry.file_name();
            let name = name_os.to_string_lossy();
            // Match `walk_dir`'s `skip_hidden = true` semantics: skip
            // entries whose name starts with `.`. This keeps `.git`,
            // `.DS_Store`, etc. out of the count.
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .and_then(OsStr::to_str)
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                total += 1;
            }
        }
    }
    total
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
        root_source_path: root_canonical.to_string_lossy().to_string(),
        ..Default::default()
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
    let force_vine_for_cc =
        if is_top_level && include_claude_code && config.claude_code_auto_include {
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
    //
    // Phase 18e (D1): each CC dir now produces a mini-subplan instead
    // of a single op. The subplan looks like:
    //
    //   1. CreateVine for the CC vine slug
    //   2. CreatePyramid (conversation) for the conversation bedrock
    //   3. AddChildToVine attaching #2 to #1 (child_type='bedrock')
    //   4. RegisterDadbearConfig for the conversation pyramid
    //   5. CreatePyramid (document) for the memory bedrock          (optional)
    //   6. AddChildToVine attaching #5 to #1 (child_type='bedrock')  (optional)
    //   7. RegisterDadbearConfig for the memory pyramid              (optional)
    //   8. AddChildToVine attaching #1 to the parent vine
    //      (child_type='vine') so the CC vine sits as a peer of real
    //      folder children inside the root vine.
    //
    // Slug shape: keep the Phase 17 "cc-N" suffix convention but
    // treat that as the CC VINE slug. The bedrocks underneath get
    // `-conversations` and `-memory` suffixes so they're easy to
    // identify in the slug table later.
    if is_top_level && include_claude_code && config.claude_code_auto_include {
        for (idx, cc_dir) in cc_matches.iter().enumerate() {
            // ── Phase 18e wanderer fix: content-aware subplan emission
            // Probe both the jsonl and memory populations up front so
            // we can skip CC dirs that would produce dead-weight slugs.
            // Three cases:
            //   - jsonls  +  memory md files → 8-op subplan (CC vine +
            //     conversation bedrock + memory bedrock + attaches)
            //   - jsonls  +  no memory       → 5-op subplan (CC vine +
            //     conversation bedrock + attaches)
            //   - no jsonls + memory md files → 5-op subplan (CC vine +
            //     memory bedrock + attaches). This path skips Ops 2–4
            //     because the conversation bedrock would have zero
            //     chunks to pre-populate and DADBEAR would watch an
            //     empty conversation source.
            //   - no jsonls + no memory → CC dir skipped entirely. An
            //     empty CC vine has no value on the root vine and the
            //     topical-vine chain would run against zero children.
            // Without these guards the planner emitted a dead
            // conversation bedrock for every memory-only CC dir, and
            // an empty CC vine when both sources were empty.
            let jsonl_count = count_cc_jsonl_files(cc_dir);
            let memory_dir = cc_dir.join("memory");
            let memory_md_count = if memory_dir.is_dir() {
                count_memory_md_files(&memory_dir)
            } else {
                0
            };
            if jsonl_count == 0 && memory_md_count == 0 {
                warn!(
                    cc_dir = %cc_dir.display(),
                    "folder_ingestion: CC dir has no jsonls and no memory md files, skipping"
                );
                continue;
            }

            // Mint the CC vine slug. Slug naming preserves the
            // Phase 17 `{root_vine}-cc-{N}` convention so existing
            // automation expecting that prefix still finds the CC
            // hierarchy. Collision resolution mirrors the rest of
            // the planner: sluggify, then suffix-bump until unique.
            let suffix = format!("cc-{}", idx + 1);
            let base_slug = format!("{}-{}", vine_slug, suffix);
            let mut cc_vine_slug = super::slug::slugify(&base_slug);
            if cc_vine_slug.is_empty() {
                cc_vine_slug = format!("cc-{}", idx + 1);
            }
            let mut dedup_suffix = 2usize;
            while existing.contains(&cc_vine_slug) {
                cc_vine_slug = super::slug::slugify(&format!("{}-{}", base_slug, dedup_suffix));
                dedup_suffix += 1;
            }
            existing.insert(cc_vine_slug.clone());

            let cc_path = cc_dir.to_string_lossy().to_string();

            // ── Op 1: Create the CC vine ─────────────────────────
            plan.operations.push(IngestionOperation::CreateVine {
                slug: cc_vine_slug.clone(),
                source_path: cc_path.clone(),
            });
            plan.claude_code_vine_slugs.push(cc_vine_slug.clone());

            // ── Ops 2–4 (optional): conversation bedrock ─────────
            // Only emit the conversation subplan when the CC dir has
            // at least one jsonl. Memory-only CC dirs (no jsonls)
            // skip this block so no dead conversation slug lands in
            // the DB and DADBEAR doesn't watch an empty jsonl dir.
            if jsonl_count > 0 {
                // Mint the conversation bedrock slug. Use the
                // `{cc_vine_slug}-conversations` suffix; collision-resolve
                // by suffix-bump if anything already claimed it.
                let mut convo_slug =
                    super::slug::slugify(&format!("{}-conversations", cc_vine_slug));
                if convo_slug.is_empty() {
                    convo_slug = format!("{}-conversations", cc_vine_slug);
                }
                let mut convo_dedup = 2usize;
                while existing.contains(&convo_slug) {
                    convo_slug = super::slug::slugify(&format!(
                        "{}-conversations-{}",
                        cc_vine_slug, convo_dedup
                    ));
                    convo_dedup += 1;
                }
                existing.insert(convo_slug.clone());

                // ── Op 2: Create the conversation bedrock ────────
                plan.operations.push(IngestionOperation::CreatePyramid {
                    slug: convo_slug.clone(),
                    content_type: ContentType::Conversation.as_str().to_string(),
                    source_path: cc_path.clone(),
                });
                plan.claude_code_conversation_slugs.push(convo_slug.clone());

                // ── Op 3: Attach conversation bedrock to CC vine ─
                plan.operations.push(IngestionOperation::AddChildToVine {
                    vine_slug: cc_vine_slug.clone(),
                    child_slug: convo_slug.clone(),
                    position: child_position(plan, &cc_vine_slug),
                    child_type: "bedrock".to_string(),
                });

                // ── Op 4: DADBEAR config for the conversation pyramid
                plan.operations
                    .push(IngestionOperation::RegisterDadbearConfig {
                        slug: convo_slug.clone(),
                        source_path: cc_path.clone(),
                        content_type: ContentType::Conversation.as_str().to_string(),
                        scan_interval_secs: config.default_scan_interval_secs,
                    });
            } else {
                info!(
                    cc_dir = %cc_dir.display(),
                    memory_md_count,
                    "folder_ingestion: CC dir has memory md files but no jsonls; skipping conversation bedrock"
                );
            }

            // ── Optional Ops 5-7: memory document bedrock ────────
            // Only emit the memory bedrock when the subfolder has at
            // least one `.md` file. An empty memory/ subfolder is
            // treated as if it doesn't exist — the document chain
            // would fail with "No documents found in {dir}" so it
            // is safer to skip the bedrock entirely.
            if memory_md_count > 0 {
                let mut memory_slug = super::slug::slugify(&format!("{}-memory", cc_vine_slug));
                if memory_slug.is_empty() {
                    memory_slug = format!("{}-memory", cc_vine_slug);
                }
                let mut memory_dedup = 2usize;
                while existing.contains(&memory_slug) {
                    memory_slug =
                        super::slug::slugify(&format!("{}-memory-{}", cc_vine_slug, memory_dedup));
                    memory_dedup += 1;
                }
                existing.insert(memory_slug.clone());

                let memory_path = memory_dir.to_string_lossy().to_string();

                // Op 5: Create the memory document bedrock
                plan.operations.push(IngestionOperation::CreatePyramid {
                    slug: memory_slug.clone(),
                    content_type: ContentType::Document.as_str().to_string(),
                    source_path: memory_path.clone(),
                });
                plan.claude_code_memory_slugs.push(memory_slug.clone());

                // Op 6: Attach memory bedrock to CC vine
                plan.operations.push(IngestionOperation::AddChildToVine {
                    vine_slug: cc_vine_slug.clone(),
                    child_slug: memory_slug.clone(),
                    position: child_position(plan, &cc_vine_slug),
                    child_type: "bedrock".to_string(),
                });

                // Op 7: DADBEAR config for the memory pyramid
                plan.operations
                    .push(IngestionOperation::RegisterDadbearConfig {
                        slug: memory_slug,
                        source_path: memory_path,
                        content_type: ContentType::Document.as_str().to_string(),
                        scan_interval_secs: config.default_scan_interval_secs,
                    });
            }

            // ── Op 8: Attach the CC vine to the root vine ────────
            // The CC vine sits alongside real folder children of
            // the root vine, with `child_type='vine'` per Phase 16's
            // composition pattern. This is the load-bearing line —
            // without it the CC vine would orphan and never get
            // dispatched as part of the root vine cascade.
            plan.operations.push(IngestionOperation::AddChildToVine {
                vine_slug: vine_slug.clone(),
                child_slug: cc_vine_slug,
                position: child_position(plan, &vine_slug),
                child_type: "vine".to_string(),
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
pub async fn execute_plan(state: &PyramidState, plan: IngestionPlan) -> Result<IngestionResult> {
    let mut result = IngestionResult {
        root_slug: plan.root_slug.clone(),
        ..Default::default()
    };

    // Phase 18e: capture the CC slug classifications up front so the
    // executor can push slugs into the right `claude_code_*` buckets
    // as it processes ops. We use HashSets so the lookups are O(1)
    // even on large plans.
    let cc_vine_set: HashSet<String> = plan.claude_code_vine_slugs.iter().cloned().collect();
    let cc_conversation_set: HashSet<String> = plan
        .claude_code_conversation_slugs
        .iter()
        .cloned()
        .collect();
    let cc_memory_set: HashSet<String> = plan.claude_code_memory_slugs.iter().cloned().collect();

    let conn = state.writer.lock().await;

    for op in plan.operations {
        match op {
            IngestionOperation::CreatePyramid {
                slug,
                content_type,
                source_path,
            } => {
                let Some(ct) = ContentType::from_str(&content_type) else {
                    result.errors.push(format!(
                        "unknown content_type '{}' for {}",
                        content_type, slug
                    ));
                    continue;
                };
                // Phase 18e wanderer fix: the Phase 17 idempotency path
                // used `msg.contains("already exists")` on the error
                // string from `db::create_slug`, but sqlite's UNIQUE
                // constraint error wrapped by `.with_context(...)`
                // reports only the top-level context ("Failed to
                // create slug '{slug}'") via `e.to_string()`, so the
                // string match never fired. That silently broke
                // re-running an ingestion against the same folder —
                // every CreatePyramid op would surface as a real error
                // and the wizard would flag the run as failed. Pre-
                // check with `db::get_slug` instead: if the slug
                // exists, verify the content_type matches (treating a
                // mismatch as a hard error because the plan is
                // semantically wrong, not idempotent) and fall through
                // to the idempotent-success path.
                let existing = match db::get_slug(&conn, &slug) {
                    Ok(info) => info,
                    Err(e) => {
                        result.errors.push(format!("get_slug {}: {}", slug, e));
                        continue;
                    }
                };
                let create_outcome: Result<(), String> = if let Some(info) = existing {
                    if info.content_type != ct {
                        Err(format!(
                            "slug '{}' already exists with content_type '{}' but plan expected '{}'",
                            slug,
                            info.content_type.as_str(),
                            ct.as_str()
                        ))
                    } else {
                        Ok(())
                    }
                } else {
                    match db::create_slug(&conn, &slug, &ct, &source_path) {
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("create_slug {}: {:#}", slug, e)),
                    }
                };

                match create_outcome {
                    Ok(()) => {
                        // Phase 18e: route CC bedrock slugs into the
                        // dedicated tracking buckets in addition to
                        // the regular pyramid list, so the wizard can
                        // surface a per-category breakdown.
                        if cc_conversation_set.contains(&slug) {
                            result.claude_code_conversation_pyramids.push(slug.clone());
                            result.claude_code_pyramids.push(slug.clone());
                        } else if cc_memory_set.contains(&slug) {
                            result.claude_code_memory_pyramids.push(slug.clone());
                            result.claude_code_pyramids.push(slug.clone());
                        }
                        result.pyramids_created.push(slug);
                    }
                    Err(msg) => {
                        result.errors.push(msg);
                    }
                }
            }
            IngestionOperation::CreateVine { slug, source_path } => {
                // Phase 18e wanderer fix: same idempotency story as
                // the CreatePyramid arm above. Pre-check with
                // `db::get_slug`, verify content_type is Vine if the
                // slug already exists (a conversation pyramid from an
                // older Phase 17 run with the SAME slug would report
                // as a real error here instead of being silently
                // treated as a vine), and fall through to the
                // idempotent-success path when the match holds.
                let existing = match db::get_slug(&conn, &slug) {
                    Ok(info) => info,
                    Err(e) => {
                        result.errors.push(format!("get_slug {}: {}", slug, e));
                        continue;
                    }
                };
                let create_outcome: Result<(), String> = if let Some(info) = existing {
                    if info.content_type != ContentType::Vine {
                        Err(format!(
                            "slug '{}' already exists with content_type '{}' but plan expected 'vine'",
                            slug,
                            info.content_type.as_str()
                        ))
                    } else {
                        Ok(())
                    }
                } else {
                    match db::create_slug(&conn, &slug, &ContentType::Vine, &source_path) {
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("create_vine {}: {:#}", slug, e)),
                    }
                };

                match create_outcome {
                    Ok(()) => {
                        // Phase 18e: CC vines go into the dedicated
                        // bucket so the wizard knows how many CC
                        // dirs were processed independently of how
                        // many bedrocks were created underneath them.
                        if cc_vine_set.contains(&slug) {
                            result.claude_code_vines.push(slug.clone());
                        }
                        result.vines_created.push(slug);
                    }
                    Err(msg) => {
                        result.errors.push(msg);
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
                    last_scan_at: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                };
                match db::save_dadbear_config_with_contributions(&conn, &config) {
                    Ok(_) => result.dadbear_configs.push(slug),
                    Err(e) => result
                        .errors
                        .push(format!("save_dadbear_config {}: {}", slug, e)),
                }
            }
        }
    }

    drop(conn);
    info!(
        root_slug = ?result.root_slug,
        pyramids = result.pyramids_created.len(),
        vines = result.vines_created.len(),
        cc_vines = result.claude_code_vines.len(),
        cc_conversation_beds = result.claude_code_conversation_pyramids.len(),
        cc_memory_beds = result.claude_code_memory_pyramids.len(),
        dadbear_configs = result.dadbear_configs.len(),
        compositions = result.compositions_added,
        errors = result.errors.len(),
        "folder_ingestion: plan execution complete"
    );

    Ok(result)
}

// ── First-build dispatch (wanderer fix) ───────────────────────────────────────
//
// Phase 17 initially relied on Pipeline B (DADBEAR) to pick up newly-created
// pyramids on its next scan tick. That does not work in practice:
//
//   1. `fire_ingest_chain` explicitly rejects `ContentType::Code` and
//      `ContentType::Document` — see `dadbear_extend.rs:742-748`. Code and
//      document pyramid ingest records get marked `failed` on the first
//      dispatch.
//   2. Topical vines created by Phase 17 are not listed in
//      `pyramid_dadbear_config` at all (vines don't have file sources), so
//      Pipeline B never scans them. `notify_vine_of_child_completion` only
//      enqueues mutations against EXISTING vine nodes, so a brand-new vine
//      with zero nodes never gets a first build.
//   3. The DADBEAR extend loop only starts at boot when configs already
//      exist, or after a conversation/vine build completes via the
//      post_build_seed helper. A user running folder ingestion on a pristine
//      DB would have no loop running at all.
//
// The wanderer fix adds an explicit first-build dispatch that runs AFTER
// `execute_plan` returns. It walks the plan operations in dependency order
// (leaves before vines), pre-populates chunks for code/document/conversation
// pyramids, and spawns a build task for each created slug via
// `question_build::spawn_question_build`. The function is non-blocking — it
// returns as soon as every build is scheduled, so the IPC round-trip stays
// fast.

/// Dispatch parameters extracted from an `IngestionPlan` for a single slug.
#[derive(Debug, Clone)]
struct BuildDispatch {
    slug: String,
    content_type: ContentType,
    source_path: String,
}

/// Extract build dispatches from a plan. Returns two lists:
///   1. leaves — non-vine slugs that need a first build
///   2. vines — vine slugs that need a first build AFTER their children
///
/// Phase 18e: with `RegisterClaudeCodePyramid` retired, every CC dir
/// now contributes a CC vine (via `CreateVine`) and one or two CC
/// bedrocks (via `CreatePyramid`). Both flow through the existing
/// `CreatePyramid` / `CreateVine` arms below — the partitioning is
/// the same as for ordinary folder children.
fn extract_build_dispatches(plan: &IngestionPlan) -> (Vec<BuildDispatch>, Vec<BuildDispatch>) {
    let mut leaves: Vec<BuildDispatch> = Vec::new();
    let mut vines: Vec<BuildDispatch> = Vec::new();
    for op in &plan.operations {
        match op {
            IngestionOperation::CreatePyramid {
                slug,
                content_type,
                source_path,
            } => {
                if let Some(ct) = ContentType::from_str(content_type) {
                    leaves.push(BuildDispatch {
                        slug: slug.clone(),
                        content_type: ct,
                        source_path: source_path.clone(),
                    });
                }
            }
            IngestionOperation::CreateVine { slug, source_path } => {
                vines.push(BuildDispatch {
                    slug: slug.clone(),
                    content_type: ContentType::Vine,
                    source_path: source_path.clone(),
                });
            }
            IngestionOperation::AddChildToVine { .. }
            | IngestionOperation::RegisterDadbearConfig { .. } => {}
        }
    }
    (leaves, vines)
}

/// Content-type-appropriate default apex question used as the seed for the
/// question pipeline. Matches the `DEFAULT_QUESTIONS` lookup in
/// `AddWorkspace.tsx` so folder-ingested pyramids get the same starting
/// question as the legacy single-directory flow.
fn default_apex_question(ct: &ContentType) -> &'static str {
    match ct {
        ContentType::Code => {
            "What are the key systems, patterns, and architecture of this codebase?"
        }
        ContentType::Document => {
            "What are the key concepts, decisions, and relationships in these documents?"
        }
        ContentType::Conversation => {
            "What happened during this conversation? What was discussed, \
             what decisions were made, how did the discussion evolve, \
             and what are the key takeaways?"
        }
        ContentType::Vine => {
            "What are the key themes and structure across the children of this folder collection?"
        }
        ContentType::Question => "What are the most important answers this material can provide?",
    }
}

/// Populate `pyramid_chunks` for a freshly-created code/document/conversation
/// pyramid so that the question pipeline has something to `for_each: $chunks`
/// over. Vines and question slugs skip this step — they don't own chunks.
///
/// For conversation slugs whose source_path is a DIRECTORY, this walks
/// `*.jsonl` files and runs `ingest_conversation` for each one. For
/// directories without any jsonls (or single-file paths) the ingest_conversation
/// path accepts the parent/dir and handles it.
async fn prepopulate_chunks_for(state: &Arc<PyramidState>, dispatch: &BuildDispatch) -> Result<()> {
    let writer = state.writer.clone();
    let slug = dispatch.slug.clone();
    let source_path = dispatch.source_path.clone();
    let content_type = dispatch.content_type.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = writer.blocking_lock();
        let path = Path::new(&source_path);
        match content_type {
            ContentType::Code => {
                super::ingest::ingest_code(&conn, &slug, path)
                    .with_context(|| format!("ingest_code failed for slug '{}'", slug))?;
            }
            ContentType::Document => {
                super::ingest::ingest_docs(&conn, &slug, path)
                    .with_context(|| format!("ingest_docs failed for slug '{}'", slug))?;
            }
            ContentType::Conversation => {
                // A Claude Code conversation pyramid's source_path is a
                // directory containing one or more `.jsonl` session files.
                // `ingest_conversation` is single-file and re-uses chunk_index
                // 0..N per call, so calling it multiple times for the same
                // slug collides on the `UNIQUE(slug, chunk_index)` constraint
                // after the first file.
                //
                // We bootstrap the pyramid with the most-recently-modified
                // jsonl (most likely the active session the user cares about)
                // and leave older sessions to be surfaced by DADBEAR/Pipeline
                // B on subsequent ticks. The per-session chunk_offset bug is
                // the tracked Phase 0b latent issue (implementation log:219).
                // This keeps the first build non-empty without making the
                // latent issue worse than the existing Pipeline B flow.
                if path.is_dir() {
                    let newest_jsonl = if let Ok(entries) = std::fs::read_dir(path) {
                        let mut with_mtime: Vec<(PathBuf, std::time::SystemTime)> = entries
                            .flatten()
                            .filter_map(|e| {
                                let p = e.path();
                                if !p.is_file() {
                                    return None;
                                }
                                if p.extension().and_then(OsStr::to_str) != Some("jsonl") {
                                    return None;
                                }
                                let mtime = e
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                                Some((p, mtime))
                            })
                            .collect();
                        with_mtime.sort_by_key(|(_, t)| *t);
                        with_mtime.pop().map(|(p, _)| p)
                    } else {
                        None
                    };
                    match newest_jsonl {
                        Some(jsonl) => {
                            super::ingest::ingest_conversation(&conn, &slug, &jsonl).with_context(
                                || {
                                    format!(
                                        "ingest_conversation failed for slug '{}' file '{}'",
                                        slug,
                                        jsonl.display()
                                    )
                                },
                            )?;
                        }
                        None => {
                            warn!(
                                slug = %slug,
                                source_path = %source_path,
                                "conversation directory had no ingestible jsonl files"
                            );
                        }
                    }
                } else if path.is_file() {
                    super::ingest::ingest_conversation(&conn, &slug, path).with_context(|| {
                        format!("ingest_conversation failed for slug '{}'", slug)
                    })?;
                }
            }
            ContentType::Vine | ContentType::Question => {
                // Vines compose children via pyramid_vine_compositions; no
                // chunks are needed. Question pyramids derive from cross-slug
                // evidence, also no chunks.
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("prepopulate_chunks spawn_blocking join failed: {e}"))?
}

/// Ensure the DADBEAR extend loop is running. The loop is normally started
/// at app boot when existing configs are found, or after a conversation/vine
/// post-build seeding pass. Phase 17 creates configs directly and needs to
/// kick the loop itself so ongoing file-change dispatch works.
async fn ensure_dadbear_loop_running(state: &Arc<PyramidState>) {
    let mut handle = state.dadbear_handle.lock().await;
    if handle.is_some() {
        return;
    }
    let Some(data_dir) = state.data_dir.as_ref() else {
        warn!("folder_ingestion: cannot start DADBEAR loop — data_dir not set");
        return;
    };
    let db_path = data_dir.join("pyramid.db").to_string_lossy().to_string();
    let bus = state.build_event_bus.clone();
    *handle = Some(super::dadbear_extend::start_dadbear_extend_loop(
        state.clone(),
        db_path,
        bus,
    ));
    info!("folder_ingestion: DADBEAR extend loop started");
}

/// Dispatch first builds for every slug the plan just created.
///
/// Spawns a single background task that:
///   1. Starts the DADBEAR extend loop if it isn't already running.
///   2. Walks every non-vine leaf in plan order, populates chunks, and
///      spawns a question build via `question_build::spawn_question_build`.
///      Each build runs asynchronously — we don't await completion.
///   3. After a short settle delay, walks every vine in plan order and
///      spawns a vine build the same way. The topical-vine chain reads
///      whatever apexes its children have produced so far via
///      `cross_build_input`; if some children are still building the vine
///      picks up stragglers on its own notification cascade later.
///
/// Returns immediately after spawning the background task so the folder
/// ingestion IPC stays snappy.
pub fn spawn_initial_builds(state: &Arc<PyramidState>, plan: &IngestionPlan) {
    let (leaves, vines) = extract_build_dispatches(plan);
    if leaves.is_empty() && vines.is_empty() {
        return;
    }
    let state = state.clone();
    let root_slug = plan.root_slug.clone();

    tokio::spawn(async move {
        ensure_dadbear_loop_running(&state).await;

        // Phase B: check dispatch policy for sequential folder builds.
        let sequential = {
            let cfg = state.config.read().await;
            cfg.dispatch_policy
                .as_ref()
                .map(|p| p.build_coordination.folder_builds_sequential)
                .unwrap_or(false)
        };

        // ── Leaves ────────────────────────────────────────────────────────
        for dispatch in &leaves {
            if let Err(e) = prepopulate_chunks_for(&state, dispatch).await {
                warn!(
                    slug = %dispatch.slug,
                    error = %e,
                    "folder_ingestion: prepopulate_chunks failed, skipping build"
                );
                continue;
            }
            let question = default_apex_question(&dispatch.content_type).to_string();
            match super::question_build::spawn_question_build(
                &state,
                dispatch.slug.clone(),
                question,
                3,    // granularity
                3,    // max_depth
                0,    // from_depth
                None, // characterization: auto
            )
            .await
            {
                Ok((_json, completion_rx)) => {
                    info!(
                        slug = %dispatch.slug,
                        content_type = %dispatch.content_type.as_str(),
                        sequential,
                        "folder_ingestion: first build spawned"
                    );
                    if sequential {
                        // Await completion before spawning the next build.
                        match completion_rx.await {
                            Ok(Ok(())) => info!(
                                slug = %dispatch.slug,
                                "folder_ingestion: sequential build completed"
                            ),
                            Ok(Err(e)) => warn!(
                                slug = %dispatch.slug,
                                error = %e,
                                "folder_ingestion: sequential build finished with error"
                            ),
                            Err(_) => warn!(
                                slug = %dispatch.slug,
                                "folder_ingestion: sequential build completion channel dropped"
                            ),
                        }
                    }
                }
                Err(e) => warn!(
                    slug = %dispatch.slug,
                    content_type = %dispatch.content_type.as_str(),
                    error = %e,
                    "folder_ingestion: first build spawn failed"
                ),
            }
        }

        // ── Vines ─────────────────────────────────────────────────────────
        // Small delay so leaf builds have a chance to start writing apex
        // nodes before the vine chain runs `cross_build_input`. This is
        // best-effort — the vine will still run even if leaves aren't
        // finished, and the regular change-propagation cascade picks up
        // slack on leaf completion via `notify_vine_of_child_completion`
        // (for vines that already have upper-layer nodes).
        if !vines.is_empty() && !sequential {
            // When sequential, leaves are already complete — no settle delay needed.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        for dispatch in &vines {
            let question = default_apex_question(&dispatch.content_type).to_string();
            match super::question_build::spawn_question_build(
                &state,
                dispatch.slug.clone(),
                question,
                3,
                3,
                0,
                None,
            )
            .await
            {
                Ok((_json, completion_rx)) => {
                    info!(
                        slug = %dispatch.slug,
                        sequential,
                        "folder_ingestion: first vine build spawned"
                    );
                    if sequential {
                        match completion_rx.await {
                            Ok(Ok(())) => info!(
                                slug = %dispatch.slug,
                                "folder_ingestion: sequential vine build completed"
                            ),
                            Ok(Err(e)) => warn!(
                                slug = %dispatch.slug,
                                error = %e,
                                "folder_ingestion: sequential vine build finished with error"
                            ),
                            Err(_) => warn!(
                                slug = %dispatch.slug,
                                "folder_ingestion: sequential vine build completion channel dropped"
                            ),
                        }
                    }
                }
                Err(e) => {
                    // Treat "Build already running" as expected — a prior
                    // dispatch may have beaten us to it. Every other error
                    // is worth logging.
                    if e.contains("already running") {
                        info!(
                            slug = %dispatch.slug,
                            "folder_ingestion: vine build already running, skipping"
                        );
                    } else {
                        warn!(
                            slug = %dispatch.slug,
                            error = %e,
                            "folder_ingestion: first vine build spawn failed"
                        );
                    }
                }
            }
        }

        info!(
            leaves = leaves.len(),
            vines = vines.len(),
            root_slug = ?root_slug,
            "folder_ingestion: initial build dispatch complete"
        );
    });
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
        assert_eq!(
            detect_content_type(&files, &config),
            Some(ContentType::Code)
        );
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
        assert_eq!(
            detect_content_type(&files, &config),
            Some(ContentType::Code)
        );
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
    fn test_encode_path_for_claude_code_spaces_and_dots() {
        // Phase 18a follow-up: the encoding rule collapses every
        // non-alphanumeric-non-dash character to a dash. Confirmed
        // against real entries in ~/.claude/projects/ on a user
        // whose target paths contained spaces.
        let path = PathBuf::from("/Users/adam/AI Project Files/agent-wire-node");
        assert_eq!(
            encode_path_for_claude_code(&path),
            "-Users-adam-AI-Project-Files-agent-wire-node"
        );

        // Dots collapse too — the `/.claude/` segment in a worktree
        // path becomes `--claude-` (the `/` and the `.` both become
        // dashes, producing the double-dash run).
        let worktree = PathBuf::from(
            "/Users/adam/AI Project Files/GoodNewsEveryone/.claude/worktrees/loving-clarke",
        );
        assert_eq!(
            encode_path_for_claude_code(&worktree),
            "-Users-adam-AI-Project-Files-GoodNewsEveryone--claude-worktrees-loving-clarke"
        );

        // Pre-existing dashes in the path are preserved verbatim.
        let hyphenated = PathBuf::from("/a/hello-world/foo-bar");
        assert_eq!(
            encode_path_for_claude_code(&hyphenated),
            "-a-hello-world-foo-bar"
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
        let worktree_dir =
            cc_root.join(format!("{}--claude-worktrees-nervous-lichterman", encoded));
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
    fn test_find_cc_dirs_pattern_b_returns_scan_root_when_it_contains_jsonls() {
        // Phase 18a follow-up: Pattern B — when the scan root is
        // a direct conversation folder (jsonls at the top level,
        // no encoded-path subdirs), treat the scan root itself as
        // a single match. This is what users who pick a custom
        // folder via the wizard's "Change…" button typically want.
        let tmp = TempDir::new().unwrap();
        let convo_dir = tmp.path().join("my-exported-chats");
        fs::create_dir_all(&convo_dir).unwrap();
        fs::write(convo_dir.join("sess-1.jsonl"), "{}").unwrap();
        fs::write(convo_dir.join("sess-2.jsonl"), "{}").unwrap();

        // Target folder is unrelated to the convo dir's path.
        let target = tmp.path().join("some-other-project");
        fs::create_dir_all(&target).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: convo_dir.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert_eq!(
            matches.len(),
            1,
            "Pattern B should return the scan root as a single match"
        );
        assert_eq!(matches[0], convo_dir.canonicalize().unwrap());
    }

    #[test]
    fn test_find_cc_dirs_pattern_b_with_memory_subfolder() {
        // Pattern B + memory subfolder: the scan root has jsonls AND
        // a `memory/` subfolder with .md files. The planner should
        // still emit an 8-op subplan for this dir (conversation beds
        // + memory beds), but find_claude_code_conversation_dirs
        // just needs to return the one match.
        let tmp = TempDir::new().unwrap();
        let convo_dir = tmp.path().join("foldervine-test");
        fs::create_dir_all(&convo_dir).unwrap();
        fs::write(convo_dir.join("sess-1.jsonl"), "{}").unwrap();
        let memory = convo_dir.join("memory");
        fs::create_dir_all(&memory).unwrap();
        fs::write(memory.join("notes.md"), "# notes").unwrap();

        let target = tmp.path().join("unrelated-target");
        fs::create_dir_all(&target).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: convo_dir.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], convo_dir.canonicalize().unwrap());

        // describe_claude_code_dirs should mark Pattern B match as
        // is_main=true (there's only one) and is_worktree=false.
        let descriptions = describe_claude_code_dirs(&target, &config);
        assert_eq!(descriptions.len(), 1);
        assert!(descriptions[0].is_main);
        assert!(!descriptions[0].is_worktree);
        assert_eq!(descriptions[0].jsonl_count, 1);
        assert!(descriptions[0].has_memory_subfolder);
        assert_eq!(descriptions[0].memory_md_count, 1);
    }

    #[test]
    fn test_find_cc_dirs_pattern_b_skipped_when_scan_root_has_no_jsonls() {
        // Pattern B should NOT fire on an empty folder — the
        // function must return Vec::new() so the wizard checkbox
        // remains disabled and the planner doesn't emit a dead
        // subplan.
        let tmp = TempDir::new().unwrap();
        let empty_dir = tmp.path().join("empty-not-a-convo-dir");
        fs::create_dir_all(&empty_dir).unwrap();
        // Put a non-jsonl file at the top level to show that only
        // jsonls trigger Pattern B.
        fs::write(empty_dir.join("readme.md"), "# not a conversation").unwrap();

        let target = tmp.path().join("target");
        fs::create_dir_all(&target).unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: empty_dir.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert!(
            matches.is_empty(),
            "Pattern B must not fire without top-level jsonls"
        );
    }

    #[test]
    fn test_find_cc_dirs_pattern_a_takes_precedence_over_pattern_b() {
        // Sanity check: if the scan root contains BOTH encoded-path
        // subdirs AND top-level jsonls (unusual but legal), Pattern A
        // should win — we return the subdir matches, not the scan
        // root itself. Pattern B is the fallback, not the default.
        let tmp = TempDir::new().unwrap();
        let cc_root = tmp.path().join("hybrid-root");
        fs::create_dir_all(&cc_root).unwrap();

        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);

        // Pattern A: encoded subdir inside the root.
        let a_match = cc_root.join(&encoded);
        fs::create_dir_all(&a_match).unwrap();
        fs::write(a_match.join("sess.jsonl"), "{}").unwrap();

        // Pattern B would also fire: put a jsonl at the root level.
        fs::write(cc_root.join("stray.jsonl"), "{}").unwrap();

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        let matches = find_claude_code_conversation_dirs(&target, &config);
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0], a_match,
            "Pattern A should win when both patterns apply"
        );
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
        assert!(pyramids.len() >= 2, "expected at least two pyramids (a, c)");
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
        // Phase 18e: each CC dir now produces exactly one CC vine
        // (visible via `claude_code_vine_slugs`) and at least one
        // conversation bedrock (visible via
        // `claude_code_conversation_slugs`).
        assert_eq!(
            plan.claude_code_vine_slugs.len(),
            1,
            "expected one CC vine slug"
        );
        assert_eq!(
            plan.claude_code_conversation_slugs.len(),
            1,
            "expected one CC conversation bedrock"
        );
        // The conversation bedrock should be a CreatePyramid op with
        // content_type=conversation.
        let convo_creates: Vec<_> = plan
            .operations
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    IngestionOperation::CreatePyramid { content_type, .. }
                        if content_type == "conversation"
                )
            })
            .collect();
        assert_eq!(convo_creates.len(), 1);
    }

    /// Regression test for the verifier-pass fix: below-threshold folder
    /// but WITH Claude Code matches must still create a top-level vine
    /// so the CC pyramids have a parent. Without the `force_vine_for_cc`
    /// bypass, the empty-vine guard would drop the CC pyramids on the
    /// floor.
    ///
    /// Phase 18e: under the new model the same below-threshold-with-CC
    /// path produces a root vine + a CC vine + a conversation bedrock,
    /// so we now expect TWO `CreateVine` ops (root + CC) instead of one.
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
        assert_eq!(
            vines.len(),
            2,
            "below-threshold + CC must create root vine + CC vine"
        );

        // CC bedrock is still attached, but now via the conversation
        // CreatePyramid op stored on the plan's CC tracking lists.
        assert_eq!(plan.claude_code_vine_slugs.len(), 1);
        assert_eq!(plan.claude_code_conversation_slugs.len(), 1);

        assert!(
            plan.root_slug.is_some(),
            "root_slug must be set for CC attachment"
        );
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

    /// Regression: `.lab.bak.1774645342/` is a timestamped experiment
    /// backup dir. The bundled `.lab.bak.` substring pattern must
    /// catch ANY such directory at any depth without requiring the
    /// user to enumerate each timestamp.
    #[test]
    fn test_path_matches_any_ignore_lab_bak_substring() {
        let patterns = vec![".lab.bak.".to_string()];

        let p1 = PathBuf::from("/repo/.lab.bak.1774645342/notes.md");
        let p2 = PathBuf::from("/repo/.lab.bak.20260402200504/foo/bar.rs");
        let p3 = PathBuf::from("/repo/.lab.bak.planner.20260330220504/log.md");
        assert!(path_matches_any_ignore(&p1, &patterns));
        assert!(path_matches_any_ignore(&p2, &patterns));
        assert!(path_matches_any_ignore(&p3, &patterns));

        // Must NOT match a plain `.lab/` directory or a legit backup
        // file that happens to contain the word "lab".
        let not1 = PathBuf::from("/repo/.lab/notes.md");
        let not2 = PathBuf::from("/repo/src/lab/config.rs");
        assert!(!path_matches_any_ignore(&not1, &patterns));
        assert!(!path_matches_any_ignore(&not2, &patterns));
    }

    /// Regression: `.claude/` bundled pattern excludes Claude Code
    /// workspace state (worktrees, handoff docs, settings) at any
    /// depth. CC conversations are ingested separately via
    /// `claude_code_conversation_path`, not as source files.
    #[test]
    fn test_path_matches_any_ignore_claude_directory() {
        let patterns = vec![".claude/".to_string()];

        let worktree = PathBuf::from("/repo/.claude/worktrees/pedantic-hypatia/src/foo.rs");
        let handoff = PathBuf::from("/repo/.claude/handoff-next-session.md");
        let nested = PathBuf::from("/repo/some/sub/.claude/settings.json");
        assert!(path_matches_any_ignore(&worktree, &patterns));
        assert!(path_matches_any_ignore(&handoff, &patterns));
        assert!(path_matches_any_ignore(&nested, &patterns));

        // Must NOT match a file whose name merely contains "claude".
        let not1 = PathBuf::from("/repo/docs/claude-notes.md");
        assert!(!path_matches_any_ignore(&not1, &patterns));
    }

    /// Regression: a literal `~/` directory at the repo root (a
    /// common shell-escape mishap — `mv foo ~/bar` without
    /// expansion) should be excluded by the bundled pattern. These
    /// dirs are never knowledge the pyramid should index.
    #[test]
    fn test_path_matches_any_ignore_literal_tilde_directory() {
        let patterns = vec!["~/".to_string()];

        let p = PathBuf::from("/repo/~/Library/Application Support/wire-node/log.txt");
        assert!(path_matches_any_ignore(&p, &patterns));

        // Must NOT match a normal file whose name contains `~`
        // (e.g. a tilde-denoting temp file like `file~`).
        let not1 = PathBuf::from("/repo/src/foo~");
        assert!(!path_matches_any_ignore(&not1, &patterns));
    }

    #[test]
    fn test_expand_claude_code_projects_root_tilde() {
        if let Some(home) = dirs::home_dir() {
            let expanded = expand_claude_code_projects_root("~/.claude/projects").unwrap();
            assert_eq!(expanded, home.join(".claude").join("projects"));
        }
    }

    // ── Wanderer fix tests: initial-build dispatch planning ─────────────────

    /// `extract_build_dispatches` should separate CreatePyramid /
    /// CreateVine ops into leaves vs vines, preserving the plan order
    /// so dependency-order dispatch works.
    ///
    /// Phase 18e: with `RegisterClaudeCodePyramid` retired, the test
    /// fixture no longer needs a special CC variant — the new CC
    /// model uses standard `CreatePyramid(conversation)` plus
    /// `CreateVine` ops, both of which already flow through the
    /// existing partitioner arms.
    #[test]
    fn test_extract_build_dispatches_partitions_leaves_and_vines() {
        let plan = IngestionPlan {
            operations: vec![
                IngestionOperation::CreateVine {
                    slug: "root-vine".to_string(),
                    source_path: "/tmp/root".to_string(),
                },
                IngestionOperation::CreatePyramid {
                    slug: "root-code".to_string(),
                    content_type: "code".to_string(),
                    source_path: "/tmp/root/src".to_string(),
                },
                IngestionOperation::AddChildToVine {
                    vine_slug: "root-vine".to_string(),
                    child_slug: "root-code".to_string(),
                    position: 0,
                    child_type: "bedrock".to_string(),
                },
                IngestionOperation::RegisterDadbearConfig {
                    slug: "root-code".to_string(),
                    source_path: "/tmp/root/src".to_string(),
                    content_type: "code".to_string(),
                    scan_interval_secs: 30,
                },
                // Phase 18e: a CC vine + CC conversation bedrock now
                // takes the place of the old RegisterClaudeCodePyramid
                // op. Note both flow through the standard CreatePyramid
                // / CreateVine arms.
                IngestionOperation::CreateVine {
                    slug: "root-vine-cc-1".to_string(),
                    source_path: "/home/me/.claude/projects/-tmp-root".to_string(),
                },
                IngestionOperation::CreatePyramid {
                    slug: "root-vine-cc-1-conversations".to_string(),
                    content_type: "conversation".to_string(),
                    source_path: "/home/me/.claude/projects/-tmp-root".to_string(),
                },
                IngestionOperation::CreatePyramid {
                    slug: "root-docs".to_string(),
                    content_type: "document".to_string(),
                    source_path: "/tmp/root/docs".to_string(),
                },
            ],
            root_slug: Some("root-vine".to_string()),
            root_source_path: "/tmp/root".to_string(),
            ..Default::default()
        };
        let (leaves, vines) = extract_build_dispatches(&plan);
        // Two vines: the root vine + the CC vine.
        assert_eq!(vines.len(), 2);
        assert_eq!(vines[0].slug, "root-vine");
        assert_eq!(vines[1].slug, "root-vine-cc-1");
        // Three leaves: code, conversation bedrock, docs (in plan
        // order, since extraction preserves the source order).
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0].slug, "root-code");
        assert!(matches!(leaves[0].content_type, ContentType::Code));
        assert_eq!(leaves[1].slug, "root-vine-cc-1-conversations");
        assert!(matches!(leaves[1].content_type, ContentType::Conversation));
        assert_eq!(leaves[2].slug, "root-docs");
        assert!(matches!(leaves[2].content_type, ContentType::Document));
    }

    /// `default_apex_question` must return non-empty strings for every
    /// content type so `spawn_question_build` (which rejects empty
    /// questions) never trips on a Phase 17 dispatch.
    #[test]
    fn test_default_apex_question_non_empty_for_every_content_type() {
        for ct in [
            ContentType::Code,
            ContentType::Document,
            ContentType::Conversation,
            ContentType::Vine,
            ContentType::Question,
        ] {
            let q = default_apex_question(&ct);
            assert!(
                !q.trim().is_empty(),
                "default_apex_question must be non-empty for {:?}",
                ct
            );
        }
    }

    /// A plan containing only unrelated ops (no CreatePyramid,
    /// CreateVine) must produce empty leaves/vines lists so
    /// `spawn_initial_builds` can short-circuit.
    #[test]
    fn test_extract_build_dispatches_empty_for_plan_without_creates() {
        let plan = IngestionPlan {
            operations: vec![IngestionOperation::AddChildToVine {
                vine_slug: "v".to_string(),
                child_slug: "c".to_string(),
                position: 0,
                child_type: "bedrock".to_string(),
            }],
            ..Default::default()
        };
        let (leaves, vines) = extract_build_dispatches(&plan);
        assert!(leaves.is_empty());
        assert!(vines.is_empty());
    }
}

#[cfg(test)]
mod phase18e_tests {
    //! Phase 18e (D1): tests for the CC-dir-as-vine restructure +
    //! `memory/` subfolder pickup. Each CC dir now produces a CC vine,
    //! a conversation bedrock, and (optionally) a memory document
    //! bedrock instead of a single `RegisterClaudeCodePyramid` op.
    //!
    //! See `docs/plans/phase-18e-workstream-prompt.md` and the
    //! `Discovered-by-use` section of `docs/plans/deferral-ledger.md`
    //! for context.

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn default_config() -> FolderIngestionConfig {
        FolderIngestionConfig::default()
    }

    fn make_file(dir: &Path, name: &str, size_bytes: usize) {
        let content = "a".repeat(size_bytes);
        fs::write(dir.join(name), content).unwrap();
    }

    /// Set up a fake target folder + a fake `~/.claude/projects/`
    /// matching CC dir, optionally with a `memory/` subfolder
    /// containing the requested number of `.md` files. Returns the
    /// target folder, the fake CC root, and a config that points
    /// `claude_code_conversation_path` at the fake root.
    fn setup_target_with_cc(
        with_memory: bool,
        memory_md_count: usize,
        target_files: &[(&str, &str)],
    ) -> (TempDir, PathBuf, PathBuf, FolderIngestionConfig) {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        for (name, body) in target_files {
            fs::write(target.join(name), body).unwrap();
        }

        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        // Always drop a single jsonl so the conversation bedrock has
        // something to point at.
        fs::write(cc_dir.join("sess-1.jsonl"), "{}").unwrap();

        if with_memory {
            let memory = cc_dir.join("memory");
            fs::create_dir_all(&memory).unwrap();
            for i in 0..memory_md_count {
                fs::write(
                    memory.join(format!("note-{}.md", i + 1)),
                    format!("# note {}\n", i + 1),
                )
                .unwrap();
            }
        }

        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        (tmp, target, cc_dir, config)
    }

    /// `describe_claude_code_dirs` must populate the new memory
    /// metadata fields when a `memory/` subfolder is present.
    #[test]
    fn test_find_cc_dirs_populates_memory_subfolder_metadata() {
        let (_tmp, target, cc_dir, config) = setup_target_with_cc(true, 3, &[]);
        let dirs = describe_claude_code_dirs(&target, &config);
        assert_eq!(dirs.len(), 1, "expected exactly one matched CC dir");
        let dir = &dirs[0];
        assert!(
            dir.has_memory_subfolder,
            "memory subfolder should be detected"
        );
        assert_eq!(dir.memory_md_count, 3, "expected 3 .md files in memory/");
        assert_eq!(
            dir.memory_subfolder_path.as_deref(),
            Some(cc_dir.join("memory").to_string_lossy().as_ref())
        );
    }

    /// When the CC dir has no `memory/` subfolder, the new fields
    /// should reflect the absence: `has_memory_subfolder = false`,
    /// `memory_md_count = 0`, `memory_subfolder_path = None`.
    #[test]
    fn test_find_cc_dirs_memory_absent_returns_false() {
        let (_tmp, target, _cc_dir, config) = setup_target_with_cc(false, 0, &[]);
        let dirs = describe_claude_code_dirs(&target, &config);
        assert_eq!(dirs.len(), 1);
        let dir = &dirs[0];
        assert!(!dir.has_memory_subfolder);
        assert_eq!(dir.memory_md_count, 0);
        assert!(dir.memory_subfolder_path.is_none());
    }

    /// `count_memory_md_files` should descend into nested directories
    /// and count `.md` files at any depth, since `ingest_docs` walks
    /// recursively. Hidden directories should be skipped to match
    /// `walk_dir`'s `skip_hidden = true` semantics.
    #[test]
    fn test_count_memory_md_files_walks_recursively_skipping_hidden() {
        let tmp = TempDir::new().unwrap();
        let mem = tmp.path().join("memory");
        fs::create_dir_all(&mem).unwrap();
        make_file(&mem, "top1.md", 5);
        make_file(&mem, "top2.md", 5);

        let nested = mem.join("nested");
        fs::create_dir_all(&nested).unwrap();
        make_file(&nested, "n1.md", 5);
        make_file(&nested, "n2.txt", 5); // not .md

        let hidden = mem.join(".hidden");
        fs::create_dir_all(&hidden).unwrap();
        make_file(&hidden, "secret.md", 5); // skipped — hidden parent

        // Hidden file at the top level should also be skipped.
        make_file(&mem, ".dotfile.md", 5);

        let count = count_memory_md_files(&mem);
        assert_eq!(count, 3, "expected top1, top2, nested/n1");
    }

    /// CC dir with jsonls only (no memory/ subfolder): the planner
    /// should emit
    ///   - the root vine (existing behavior)
    ///   - a CC vine
    ///   - a conversation bedrock + DADBEAR config
    ///   - an AddChildToVine attaching the conversation bedrock to
    ///     the CC vine (`child_type='bedrock'`)
    ///   - an AddChildToVine attaching the CC vine to the root vine
    ///     (`child_type='vine'`)
    /// and NOTHING for memory.
    #[test]
    fn test_plan_generates_cc_vine_plus_conversation_bedrock() {
        let (_tmp, target, _cc_dir, config) =
            setup_target_with_cc(false, 0, &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        // CC vines + conversation slugs are the canonical signal.
        assert_eq!(plan.claude_code_vine_slugs.len(), 1);
        assert_eq!(plan.claude_code_conversation_slugs.len(), 1);
        assert!(plan.claude_code_memory_slugs.is_empty());

        let cc_vine_slug = plan.claude_code_vine_slugs[0].clone();
        let convo_slug = plan.claude_code_conversation_slugs[0].clone();

        // The conversation bedrock slug must be derived from the CC
        // vine slug + the `-conversations` suffix.
        assert_eq!(convo_slug, format!("{}-conversations", cc_vine_slug));

        // Verify the CC vine attaches to the root vine with
        // child_type='vine'.
        let root_vine_slug = plan.root_slug.clone().unwrap();
        let attach_cc_to_root = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, child_slug, child_type, ..
            } if vine_slug == &root_vine_slug
                && child_slug == &cc_vine_slug
                && child_type == "vine")
        });
        assert!(
            attach_cc_to_root,
            "CC vine must attach to root vine with child_type='vine'"
        );

        // Verify the conversation bedrock attaches to the CC vine
        // with child_type='bedrock'.
        let attach_convo_to_cc = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, child_slug, child_type, ..
            } if vine_slug == &cc_vine_slug
                && child_slug == &convo_slug
                && child_type == "bedrock")
        });
        assert!(
            attach_convo_to_cc,
            "conversation bedrock must attach to CC vine with child_type='bedrock'"
        );

        // Verify the conversation pyramid has a DADBEAR config.
        let convo_dadbear = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::RegisterDadbearConfig {
                slug, content_type, ..
            } if slug == &convo_slug && content_type == "conversation")
        });
        assert!(
            convo_dadbear,
            "conversation pyramid must have a DADBEAR config"
        );

        // No memory bedrock at all.
        let memory_creates = plan
            .operations
            .iter()
            .filter(|op| {
                matches!(op, IngestionOperation::CreatePyramid { content_type, slug, .. }
                if content_type == "document" && slug.ends_with("-memory"))
            })
            .count();
        assert_eq!(memory_creates, 0);
    }

    /// CC dir with jsonls AND a populated memory/ subfolder: the
    /// planner should additionally emit a memory document bedrock
    /// hung off the CC vine with its own DADBEAR config.
    #[test]
    fn test_plan_generates_cc_vine_plus_both_bedrocks_when_memory_present() {
        let (_tmp, target, cc_dir, config) =
            setup_target_with_cc(true, 5, &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        assert_eq!(plan.claude_code_vine_slugs.len(), 1);
        assert_eq!(plan.claude_code_conversation_slugs.len(), 1);
        assert_eq!(plan.claude_code_memory_slugs.len(), 1);

        let cc_vine_slug = plan.claude_code_vine_slugs[0].clone();
        let convo_slug = plan.claude_code_conversation_slugs[0].clone();
        let memory_slug = plan.claude_code_memory_slugs[0].clone();
        assert_eq!(memory_slug, format!("{}-memory", cc_vine_slug));

        // Memory bedrock must be a CreatePyramid op pointing at
        // `<cc_dir>/memory` with content_type=document.
        let expected_memory_path = cc_dir.join("memory").to_string_lossy().to_string();
        let memory_create = plan.operations.iter().find(|op| {
            matches!(op, IngestionOperation::CreatePyramid {
                slug, content_type, source_path
            } if slug == &memory_slug
                && content_type == "document"
                && source_path == &expected_memory_path)
        });
        assert!(
            memory_create.is_some(),
            "memory bedrock CreatePyramid op missing"
        );

        // Memory bedrock attaches to the CC vine with child_type='bedrock'.
        let attach_memory_to_cc = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, child_slug, child_type, ..
            } if vine_slug == &cc_vine_slug
                && child_slug == &memory_slug
                && child_type == "bedrock")
        });
        assert!(attach_memory_to_cc);

        // Memory pyramid has a DADBEAR config too.
        let memory_dadbear = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::RegisterDadbearConfig {
                slug, content_type, source_path, ..
            } if slug == &memory_slug
                && content_type == "document"
                && source_path == &expected_memory_path)
        });
        assert!(memory_dadbear);

        // CC vine still attaches to root vine as child_type='vine'.
        let root_vine_slug = plan.root_slug.clone().unwrap();
        let cc_attaches_to_root = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, child_slug, child_type, ..
            } if vine_slug == &root_vine_slug
                && child_slug == &cc_vine_slug
                && child_type == "vine")
        });
        assert!(cc_attaches_to_root);

        // The conversation bedrock attaches at position 0; the
        // memory bedrock attaches at position 1 (deterministic
        // ordering relative to the CC vine).
        let mut child_attachments: Vec<&IngestionOperation> = plan
            .operations
            .iter()
            .filter(|op| {
                matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, ..
            } if vine_slug == &cc_vine_slug)
            })
            .collect();
        // Sort by position to make the assertion order-independent.
        child_attachments.sort_by_key(|op| match op {
            IngestionOperation::AddChildToVine { position, .. } => *position,
            _ => 0,
        });
        assert_eq!(child_attachments.len(), 2);
        if let IngestionOperation::AddChildToVine {
            child_slug: cs0,
            position: p0,
            ..
        } = child_attachments[0]
        {
            assert_eq!(*p0, 0);
            assert_eq!(cs0, &convo_slug);
        }
        if let IngestionOperation::AddChildToVine {
            child_slug: cs1,
            position: p1,
            ..
        } = child_attachments[1]
        {
            assert_eq!(*p1, 1);
            assert_eq!(cs1, &memory_slug);
        }
    }

    /// An empty `memory/` subfolder (exists but contains zero `.md`
    /// files) must NOT trigger a memory bedrock — `ingest_docs`
    /// would fail with "No documents found in {dir}" so the safer
    /// option is to skip the bedrock entirely.
    #[test]
    fn test_plan_skips_memory_bedrock_when_no_md_files() {
        let (_tmp, target, _cc_dir, config) = setup_target_with_cc(
            true,
            0, // empty memory/
            &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")],
        );
        let plan = plan_ingestion(&target, &config, true).unwrap();
        assert_eq!(plan.claude_code_vine_slugs.len(), 1);
        assert_eq!(plan.claude_code_conversation_slugs.len(), 1);
        assert!(
            plan.claude_code_memory_slugs.is_empty(),
            "empty memory/ subfolder should NOT produce a memory bedrock"
        );
    }

    /// `extract_build_dispatches` must partition the new mini-subplan
    /// correctly: each CC vine -> vine bucket, each conversation
    /// bedrock -> leaf bucket (Conversation), each memory bedrock ->
    /// leaf bucket (Document). Since the new model uses standard
    /// `CreatePyramid` / `CreateVine` ops, the partitioner naturally
    /// handles them — this test guards against regression.
    #[test]
    fn test_extract_build_dispatches_partitions_cc_vines_and_bedrocks_correctly() {
        let (_tmp, target, _cc_dir, config) =
            setup_target_with_cc(true, 2, &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        let (leaves, vines) = extract_build_dispatches(&plan);

        // The plan emits:
        //   - root vine (CreateVine)
        //   - root document pyramid (the 3 .md files in target)
        //   - CC vine (CreateVine)
        //   - conversation bedrock (CreatePyramid conversation)
        //   - memory bedrock (CreatePyramid document)
        // → 2 vines, 3 leaves.
        assert_eq!(vines.len(), 2, "expected root vine + CC vine");
        assert_eq!(
            leaves.len(),
            3,
            "expected root doc bedrock + conversation bedrock + memory bedrock"
        );

        // Both CC pyramids must be in the leaves bucket with the
        // right content types.
        let convo_leaf = leaves
            .iter()
            .find(|l| l.slug == plan.claude_code_conversation_slugs[0]);
        assert!(convo_leaf.is_some());
        assert!(matches!(
            convo_leaf.unwrap().content_type,
            ContentType::Conversation
        ));

        let memory_leaf = leaves
            .iter()
            .find(|l| l.slug == plan.claude_code_memory_slugs[0]);
        assert!(memory_leaf.is_some());
        assert!(matches!(
            memory_leaf.unwrap().content_type,
            ContentType::Document
        ));

        // CC vine must be in the vines bucket.
        let cc_vine = vines
            .iter()
            .find(|v| v.slug == plan.claude_code_vine_slugs[0]);
        assert!(cc_vine.is_some());
        assert!(matches!(cc_vine.unwrap().content_type, ContentType::Vine));
    }

    /// The CC vine slug + bedrock slugs must be properly tracked in
    /// `IngestionPlan`'s classification sets so the executor can
    /// surface them in the result. Tests the ordering invariants too:
    /// the conversation bedrock must come before the memory bedrock
    /// in the plan, and the CC vine must come before its bedrocks.
    #[test]
    fn test_plan_records_cc_classification_sets_in_order() {
        let (_tmp, target, _cc_dir, config) =
            setup_target_with_cc(true, 1, &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        // Verify the CC vine slug appears in operations BEFORE its
        // child bedrocks (the executor cares about this ordering).
        let cc_vine_slug = &plan.claude_code_vine_slugs[0];
        let convo_slug = &plan.claude_code_conversation_slugs[0];
        let memory_slug = &plan.claude_code_memory_slugs[0];

        let cc_vine_idx = plan
            .operations
            .iter()
            .position(|op| matches!(op, IngestionOperation::CreateVine { slug, .. } if slug == cc_vine_slug))
            .expect("CC vine CreateVine op missing");
        let convo_idx = plan
            .operations
            .iter()
            .position(|op| matches!(op, IngestionOperation::CreatePyramid { slug, content_type, .. } if slug == convo_slug && content_type == "conversation"))
            .expect("conversation bedrock missing");
        let memory_idx = plan
            .operations
            .iter()
            .position(|op| matches!(op, IngestionOperation::CreatePyramid { slug, content_type, .. } if slug == memory_slug && content_type == "document"))
            .expect("memory bedrock missing");

        assert!(
            cc_vine_idx < convo_idx,
            "CC vine must precede conversation bedrock"
        );
        assert!(
            convo_idx < memory_idx,
            "conversation bedrock must precede memory bedrock"
        );
    }

    // ── Phase 18e wanderer regression tests ─────────────────────────────
    //
    // These cover two cross-cutting bugs the verifier's per-item audit
    // could not see:
    //
    //   1. CC dirs with no jsonls (memory-only or fully empty) must not
    //      emit a conversation bedrock, and a CC dir with NO jsonls AND
    //      NO memory must be skipped entirely instead of producing a
    //      childless CC vine.
    //   2. `execute_plan`'s Phase 17 idempotency path used
    //      `msg.contains("already exists")` on an anyhow-wrapped sqlite
    //      error, which never matched (`to_string()` returns only the
    //      top-level context, not the chain). Re-running an ingestion
    //      now goes through a `db::get_slug` pre-check that both avoids
    //      the string-matching trap and rejects slug re-use when the
    //      existing content_type mismatches the plan's expectation.

    /// Set up a CC dir that has neither jsonls nor memory. The dir
    /// exists (so `find_claude_code_conversation_dirs` matches it) but
    /// contains no ingestible content. The planner must skip it.
    fn setup_target_with_empty_cc_dir(
        target_files: &[(&str, &str)],
    ) -> (TempDir, PathBuf, PathBuf, FolderIngestionConfig) {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        for (name, body) in target_files {
            fs::write(target.join(name), body).unwrap();
        }
        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        // Intentionally leave cc_dir empty — no jsonl, no memory/.
        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        (tmp, target, cc_dir, config)
    }

    /// Set up a CC dir that has ONLY a memory/ subfolder with .md
    /// files — no jsonls. The planner must emit a CC vine + memory
    /// bedrock (Ops 1, 5-7, 8) and skip the conversation subplan
    /// (Ops 2-4). The end state is a CC vine with exactly one child
    /// (the memory bedrock).
    fn setup_target_with_memory_only_cc_dir(
        memory_md_count: usize,
        target_files: &[(&str, &str)],
    ) -> (TempDir, PathBuf, PathBuf, FolderIngestionConfig) {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("myrepo");
        fs::create_dir_all(&target).unwrap();
        for (name, body) in target_files {
            fs::write(target.join(name), body).unwrap();
        }
        let cc_root = tmp.path().join(".claude").join("projects");
        fs::create_dir_all(&cc_root).unwrap();
        let canonical_target = target.canonicalize().unwrap();
        let encoded = encode_path_for_claude_code(&canonical_target);
        let cc_dir = cc_root.join(&encoded);
        fs::create_dir_all(&cc_dir).unwrap();
        // No jsonls here — intentionally omitted so the planner takes
        // the memory-only branch.
        let memory = cc_dir.join("memory");
        fs::create_dir_all(&memory).unwrap();
        for i in 0..memory_md_count {
            fs::write(
                memory.join(format!("note-{}.md", i + 1)),
                format!("# note {}\n", i + 1),
            )
            .unwrap();
        }
        let config = FolderIngestionConfig {
            claude_code_conversation_path: cc_root.to_string_lossy().to_string(),
            ..default_config()
        };
        (tmp, target, cc_dir, config)
    }

    /// CC dir with neither jsonls nor memory files must be skipped
    /// entirely — no CC vine, no bedrocks, no attach-to-root op. The
    /// root folder's plan is whatever it would have been without the
    /// CC auto-include at all.
    #[test]
    fn test_plan_skips_cc_dir_with_no_jsonls_and_no_memory() {
        let (_tmp, target, _cc_dir, config) =
            setup_target_with_empty_cc_dir(&[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        assert!(
            plan.claude_code_vine_slugs.is_empty(),
            "empty CC dir must not produce a CC vine"
        );
        assert!(
            plan.claude_code_conversation_slugs.is_empty(),
            "empty CC dir must not produce a conversation bedrock"
        );
        assert!(
            plan.claude_code_memory_slugs.is_empty(),
            "empty CC dir must not produce a memory bedrock"
        );

        // No CreateVine should name a CC vine slug. The only vine
        // allowed is the root vine (or the plan emitted none at all
        // if the target folder degenerated to a leaf pyramid).
        let any_cc_vine_op = plan.operations.iter().any(|op| match op {
            IngestionOperation::CreateVine { slug, source_path }
                if source_path.contains(".claude") || slug.contains("-cc-") =>
            {
                true
            }
            _ => false,
        });
        assert!(
            !any_cc_vine_op,
            "plan must not contain any CC-vine CreateVine op, got ops: {:?}",
            plan.operations
        );
    }

    /// CC dir with memory md files but no jsonls: the planner must
    /// emit the CC vine + memory bedrock (Ops 1, 5, 6, 7, 8) and skip
    /// the conversation subplan (Ops 2-4). Under the bug, the planner
    /// unconditionally emitted a conversation bedrock pointing at the
    /// CC dir — a dead slug that would fail its first build with
    /// "No chunks found for slug".
    #[test]
    fn test_plan_emits_memory_only_subplan_when_cc_has_no_jsonls() {
        let (_tmp, target, cc_dir, config) =
            setup_target_with_memory_only_cc_dir(3, &[("a.md", "x"), ("b.md", "y"), ("c.md", "z")]);
        let plan = plan_ingestion(&target, &config, true).unwrap();

        // Exactly one CC vine + one memory bedrock; no conversation
        // bedrock at all.
        assert_eq!(
            plan.claude_code_vine_slugs.len(),
            1,
            "expected a CC vine for the memory-only CC dir"
        );
        assert!(
            plan.claude_code_conversation_slugs.is_empty(),
            "memory-only CC dir must NOT produce a conversation bedrock, got {:?}",
            plan.claude_code_conversation_slugs
        );
        assert_eq!(
            plan.claude_code_memory_slugs.len(),
            1,
            "expected exactly one memory bedrock"
        );

        let cc_vine_slug = plan.claude_code_vine_slugs[0].clone();
        let memory_slug = plan.claude_code_memory_slugs[0].clone();
        assert_eq!(memory_slug, format!("{}-memory", cc_vine_slug));

        // No conversation CreatePyramid op exists.
        let has_conversation_op = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::CreatePyramid { content_type, .. }
                if content_type == "conversation")
        });
        assert!(
            !has_conversation_op,
            "plan must not contain any conversation CreatePyramid op"
        );

        // The conversation bedrock DADBEAR config must not exist.
        let has_conversation_dadbear = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::RegisterDadbearConfig { content_type, .. }
                if content_type == "conversation")
        });
        assert!(
            !has_conversation_dadbear,
            "plan must not contain any conversation DADBEAR config"
        );

        // The memory bedrock's source path points at the memory dir.
        let memory_path = cc_dir.join("memory").to_string_lossy().to_string();
        let memory_create = plan.operations.iter().find(|op| {
            matches!(op, IngestionOperation::CreatePyramid {
                slug, source_path, ..
            } if slug == &memory_slug && source_path == &memory_path)
        });
        assert!(
            memory_create.is_some(),
            "memory bedrock CreatePyramid missing"
        );

        // The CC vine attaches to the root vine as child_type='vine'.
        let root_vine_slug = plan.root_slug.clone().unwrap();
        let attached_as_vine = plan.operations.iter().any(|op| {
            matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, child_slug, child_type, ..
            } if vine_slug == &root_vine_slug
                && child_slug == &cc_vine_slug
                && child_type == "vine")
        });
        assert!(
            attached_as_vine,
            "CC vine must still attach to root vine as child_type='vine'"
        );

        // The memory bedrock is the CC vine's ONLY child, and it
        // attaches at position 0 (not 1, because no conversation
        // bedrock precedes it).
        let cc_children: Vec<&IngestionOperation> = plan
            .operations
            .iter()
            .filter(|op| {
                matches!(op, IngestionOperation::AddChildToVine {
                vine_slug, ..
            } if vine_slug == &cc_vine_slug)
            })
            .collect();
        assert_eq!(
            cc_children.len(),
            1,
            "CC vine must have exactly one child (the memory bedrock)"
        );
        if let IngestionOperation::AddChildToVine {
            position,
            child_slug,
            child_type,
            ..
        } = cc_children[0]
        {
            assert_eq!(*position, 0, "memory bedrock attaches at position 0");
            assert_eq!(child_slug, &memory_slug);
            assert_eq!(child_type, "bedrock");
        }
    }

    // ── execute_plan idempotency + content-type mismatch tests ──────────
    //
    // These tests exercise the live `execute_plan` path rather than
    // the planner. They construct an in-memory PyramidState and run
    // the same plan twice (idempotency) or run a plan against a
    // pre-existing slug with the wrong content_type (mismatch). The
    // verifier's live test run passed because it only exercised
    // fresh-DB flows — the idempotency breakage was invisible without
    // a repeat-execution test.

    use crate::pyramid::PyramidState;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc as StdArc;
    use tokio::sync::Mutex as TokioMutex;

    /// Build a minimal `PyramidState` with an initialized in-memory
    /// DB. Mirrors the helper inside dadbear_extend::tests but trimmed
    /// to the fields `execute_plan` actually touches. All other
    /// fields get safe defaults that the folder_ingestion path never
    /// dereferences (chains_dir is set to data_dir/chains to keep the
    /// PyramidState constructor happy).
    fn make_execute_plan_test_state() -> (StdArc<PyramidState>, tempfile::TempDir) {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().to_path_buf();
        let db_path = data_dir.join("pyramid.db");
        let writer_conn = crate::pyramid::db::open_pyramid_db(&db_path).unwrap();
        let reader_conn = crate::pyramid::db::open_pyramid_connection(&db_path).unwrap();
        let llm_config = crate::pyramid::llm::LlmConfig::default();
        let state = StdArc::new(PyramidState {
            reader: StdArc::new(TokioMutex::new(reader_conn)),
            writer: StdArc::new(TokioMutex::new(writer_conn)),
            config: StdArc::new(tokio::sync::RwLock::new(llm_config)),
            active_build: StdArc::new(tokio::sync::RwLock::new(HashMap::new())),
            data_dir: Some(data_dir.clone()),
            stale_engines: StdArc::new(TokioMutex::new(HashMap::new())),
            file_watchers: StdArc::new(TokioMutex::new(HashMap::new())),
            vine_builds: StdArc::new(TokioMutex::new(HashMap::new())),
            use_chain_engine: AtomicBool::new(false),
            use_ir_executor: AtomicBool::new(false),
            event_bus: StdArc::new(crate::pyramid::event_chain::LocalEventBus::new()),
            operational: StdArc::new(crate::pyramid::OperationalConfig::default()),
            chains_dir: data_dir.join("chains"),
            remote_query_rate_limiter: StdArc::new(TokioMutex::new(HashMap::new())),
            absorption_gate: StdArc::new(TokioMutex::new(crate::pyramid::AbsorptionGate::new())),
            build_event_bus: StdArc::new(crate::pyramid::event_bus::BuildEventBus::new()),
            supabase_url: None,
            supabase_anon_key: None,
            csrf_secret: [0u8; 32],
            dadbear_handle: StdArc::new(TokioMutex::new(None)),
            dadbear_supervisor_handle: StdArc::new(TokioMutex::new(None)),
            dadbear_in_flight: StdArc::new(std::sync::Mutex::new(HashMap::new())),
            provider_registry: {
                let store = StdArc::new(
                    crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
                );
                StdArc::new(crate::pyramid::provider::ProviderRegistry::new(store))
            },
            credential_store: StdArc::new(
                crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
            ),
            schema_registry: StdArc::new(crate::pyramid::schema_registry::SchemaRegistry::new()),
            cross_pyramid_router: StdArc::new(
                crate::pyramid::cross_pyramid_router::CrossPyramidEventRouter::new(),
            ),
            ollama_pull_cancel: StdArc::new(std::sync::atomic::AtomicBool::new(false)),
            ollama_pull_in_progress: StdArc::new(tokio::sync::Mutex::new(None)),
        });
        (state, dir)
    }

    /// Re-running `execute_plan` against the same plan must NOT emit
    /// errors. Every CreatePyramid / CreateVine op should go through
    /// the pre-check (slug already exists with matching content_type)
    /// and report success. Without the fix, the Phase 17 code path
    /// tried to detect "already exists" by substring match on
    /// `e.to_string()`, but anyhow's `with_context` wrapper makes
    /// `to_string()` return only the top-level context. Every repeat
    /// op surfaced as a hard error.
    #[tokio::test]
    async fn test_execute_plan_is_idempotent_on_rerun() {
        let (state, _tmp) = make_execute_plan_test_state();
        let plan = IngestionPlan {
            operations: vec![
                IngestionOperation::CreateVine {
                    slug: "root-vine".to_string(),
                    source_path: "/tmp/root".to_string(),
                },
                IngestionOperation::CreatePyramid {
                    slug: "root-doc".to_string(),
                    content_type: "document".to_string(),
                    source_path: "/tmp/root/docs".to_string(),
                },
                IngestionOperation::AddChildToVine {
                    vine_slug: "root-vine".to_string(),
                    child_slug: "root-doc".to_string(),
                    position: 0,
                    child_type: "bedrock".to_string(),
                },
                IngestionOperation::RegisterDadbearConfig {
                    slug: "root-doc".to_string(),
                    source_path: "/tmp/root/docs".to_string(),
                    content_type: "document".to_string(),
                    scan_interval_secs: 30,
                },
            ],
            root_slug: Some("root-vine".to_string()),
            root_source_path: "/tmp/root".to_string(),
            ..Default::default()
        };

        // First run — fresh DB.
        let first = execute_plan(&state, plan.clone()).await.unwrap();
        assert!(
            first.errors.is_empty(),
            "first run must be clean, got errors: {:?}",
            first.errors
        );
        assert_eq!(first.pyramids_created.len(), 1);
        assert_eq!(first.vines_created.len(), 1);
        assert_eq!(first.compositions_added, 1);
        assert_eq!(first.dadbear_configs.len(), 1);

        // Second run — same plan against the populated DB. Under the
        // bug every CreatePyramid/CreateVine op would be pushed into
        // `errors` because the "already exists" substring check never
        // fired. After the fix the pre-check catches the existing
        // slug, confirms the content_type matches, and routes to the
        // idempotent-success path. Vine compositions use ON CONFLICT
        // DO UPDATE, so they stay clean too.
        let second = execute_plan(&state, plan).await.unwrap();
        assert!(
            second.errors.is_empty(),
            "second run must be idempotent, got errors: {:?}",
            second.errors
        );
        assert_eq!(second.pyramids_created.len(), 1);
        assert_eq!(second.vines_created.len(), 1);
        assert_eq!(second.compositions_added, 1);
        assert_eq!(second.dadbear_configs.len(), 1);
    }

    /// A plan that re-uses a slug with a DIFFERENT content_type must
    /// surface a real error instead of silently treating the wrong
    /// slug as an idempotent hit. This protects the old Phase 17
    /// layout (where `{root}-cc-1` was a conversation pyramid) from
    /// being reinterpreted as a vine by a fresh Phase 18e run.
    #[tokio::test]
    async fn test_execute_plan_rejects_slug_with_mismatched_content_type() {
        let (state, _tmp) = make_execute_plan_test_state();

        // Pre-populate the DB with a conversation slug "legacy-cc-1",
        // mimicking what the old Phase 17 planner wrote to the DB.
        {
            let conn = state.writer.lock().await;
            crate::pyramid::db::create_slug(
                &conn,
                "legacy-cc-1",
                &ContentType::Conversation,
                "/tmp/old",
            )
            .unwrap();
        }

        // Now run a plan that wants "legacy-cc-1" to be a vine (the
        // Phase 18e shape). The executor must reject this instead of
        // treating the pre-existing conversation slug as idempotent.
        let plan = IngestionPlan {
            operations: vec![IngestionOperation::CreateVine {
                slug: "legacy-cc-1".to_string(),
                source_path: "/tmp/old".to_string(),
            }],
            ..Default::default()
        };
        let result = execute_plan(&state, plan).await.unwrap();
        assert_eq!(
            result.vines_created.len(),
            0,
            "mismatched slug must not land in vines_created"
        );
        assert_eq!(
            result.errors.len(),
            1,
            "expected one error, got: {:?}",
            result.errors
        );
        let err = &result.errors[0];
        assert!(
            err.contains("legacy-cc-1") && err.contains("content_type"),
            "error must name the slug and the content_type mismatch, got: {}",
            err
        );
    }

    /// A plan that creates a conversation slug, then re-runs against
    /// the same slug but asking for 'document', must reject the
    /// second run. Same content_type-mismatch story as the vine case
    /// but exercising the CreatePyramid arm.
    #[tokio::test]
    async fn test_execute_plan_rejects_pyramid_content_type_mismatch() {
        let (state, _tmp) = make_execute_plan_test_state();

        // Seed with a conversation slug.
        {
            let conn = state.writer.lock().await;
            crate::pyramid::db::create_slug(
                &conn,
                "shared-slug",
                &ContentType::Conversation,
                "/tmp/conv",
            )
            .unwrap();
        }

        let plan = IngestionPlan {
            operations: vec![IngestionOperation::CreatePyramid {
                slug: "shared-slug".to_string(),
                content_type: "document".to_string(),
                source_path: "/tmp/doc".to_string(),
            }],
            ..Default::default()
        };
        let result = execute_plan(&state, plan).await.unwrap();
        assert_eq!(result.pyramids_created.len(), 0);
        assert_eq!(result.errors.len(), 1);
        let err = &result.errors[0];
        assert!(err.contains("shared-slug"));
        assert!(err.contains("conversation") || err.contains("document"));
    }
}
