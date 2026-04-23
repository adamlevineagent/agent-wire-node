// pyramid/ingest.rs — Phase 2: Ingestion
//
// Three ingestion pipelines (conversation, code, documents) plus continuation support.
// Each reads source material, chunks it, and stores it in the pyramid SQLite database
// using the pyramid_slugs / pyramid_batches / pyramid_chunks schema.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::db;
use super::types::{ChangeSet, ContentType, IngestConfig, IngestRecord, SourceFile};

/// Metadata about files ingested during a build, used for post-build seeding.
#[derive(Debug, Clone)]
pub struct IngestFileInfo {
    /// Absolute path to the file
    pub abs_path: String,
    /// SHA-256 hash of the file
    pub hash: String,
    /// The chunk_index this file was assigned (1 file = 1 chunk for code/doc)
    pub chunk_index: i64,
}

/// Result of an ingestion that includes extension/config metadata for seeding.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub slug: String,
    /// Extensions that were ingested (e.g. [".rs", ".ts"])
    pub ingested_extensions: Vec<String>,
    /// Config filenames that were ingested (e.g. ["Cargo.toml"])
    pub ingested_config_files: Vec<String>,
    /// Per-file hash information
    pub file_infos: Vec<IngestFileInfo>,
}

/// Compute SHA-256 hash of file content bytes.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ── Constants ────────────────────────────────────────────────────────────────

fn chunk_target_lines() -> usize {
    super::Tier2Config::default().chunk_target_lines
}

/// Directories to skip during code/doc ingestion.
fn skip_dirs() -> HashSet<&'static str> {
    [
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".next",
        "__pycache__",
        ".vscode",
        ".idea",
        "coverage",
        ".cache",
    ]
    .into_iter()
    .collect()
}

/// File extensions recognized as source code.
pub fn code_extensions() -> HashSet<&'static str> {
    [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".swift", ".kt",
    ]
    .into_iter()
    .collect()
}

/// Filenames recognized as config files.
pub fn config_files() -> HashSet<&'static str> {
    [
        "package.json",
        "Cargo.toml",
        "tauri.conf.json",
        "tsconfig.json",
        "vite.config.ts",
        "vite.config.js",
        "build.rs",
        "pyproject.toml",
    ]
    .into_iter()
    .collect()
}

/// Document file extensions for doc ingestion.
pub fn doc_extensions() -> HashSet<&'static str> {
    [".txt", ".md"].into_iter().collect()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Detect programming language from file extension.
fn detect_language(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "swift" => "swift",
        "kt" => "kotlin",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        "css" => "css",
        "html" | "htm" => "html",
        "sql" => "sql",
        _ => "unknown",
    }
}

/// Extract plain text from a JSONL message's `content` field.
///
/// Handles two forms:
/// - String: returned as-is
/// - Array of blocks: concatenates `text` blocks and labels `tool_use` blocks
fn extract_text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(obj) = block.as_object() {
                    match obj.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            parts.push(format!("[Tool: {name}]"));
                        }
                        _ => {}
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// Parse JSONL messages from a conversation file, applying speaker labels.
///
/// Returns a Vec of formatted message strings:
///   `--- PLAYFUL [2026-03-20T14:30] ---\n<text>\n`
///
/// If `skip_messages > 0`, the first N qualifying messages are skipped.
/// Also returns total qualifying message count (before skipping).
fn parse_conversation_messages(
    jsonl_path: &Path,
    skip_messages: usize,
) -> Result<(Vec<String>, usize)> {
    let content = std::fs::read_to_string(jsonl_path)
        .with_context(|| format!("Failed to read {}", jsonl_path.display()))?;

    let mut messages = Vec::new();
    let mut total_count: usize = 0;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let d: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Filter: only user/assistant types
        let msg_type = match d.get("type").and_then(|t| t.as_str()) {
            Some("user") | Some("assistant") => d["type"].as_str().unwrap(),
            _ => continue,
        };

        // Get the message object
        let msg = match d.get("message") {
            Some(Value::Object(_)) => &d["message"],
            _ => continue,
        };

        // Extract text
        let text = extract_text_from_content(msg.get("content").unwrap_or(&Value::Null));
        if text.trim().is_empty() {
            continue;
        }

        // Skip toolUseResult entries
        if d.get("toolUseResult").is_some() {
            continue;
        }

        total_count += 1;

        // Apply skip
        if total_count <= skip_messages {
            continue;
        }

        // Determine role and label
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or(msg_type);
        let label = if role == "user" {
            "PLAYFUL"
        } else {
            "CONDUCTOR"
        };

        // Timestamp: take first 19 chars
        let ts = d.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        let ts = if ts.len() >= 19 { &ts[..19] } else { ts };

        messages.push(format!("--- {label} [{ts}] ---\n{}\n", text.trim()));
    }

    Ok((messages, total_count))
}

/// Chunk a transcript (as lines) using soft/hard boundary logic.
///
/// Soft boundary: line starts with `--- ` AND we've accumulated >= 70% of target lines.
/// Hard limit: >= 130% of target lines.
fn chunk_transcript(transcript: &str) -> Vec<String> {
    let lines: Vec<&str> = transcript.split('\n').collect();
    let mut chunks = Vec::new();
    let mut current_chunk: Vec<&str> = Vec::new();
    let mut current_count: usize = 0;

    let soft_threshold = (chunk_target_lines() as f64 * 0.7) as usize;
    let hard_limit = (chunk_target_lines() as f64 * 1.3) as usize;

    for line in &lines {
        current_chunk.push(line);
        current_count += 1;

        let at_boundary = line.starts_with("--- ") && current_count >= soft_threshold;
        let at_hard_limit = current_count >= hard_limit;

        if at_boundary || at_hard_limit {
            if at_boundary {
                // Pop the boundary line — it starts the next chunk
                current_chunk.pop();
                let chunk_text = current_chunk.join("\n");
                chunks.push(chunk_text);
                current_chunk = vec![line];
                current_count = 1;
            } else {
                // Hard limit: flush everything including this line
                chunks.push(current_chunk.join("\n"));
                current_chunk = Vec::new();
                current_count = 0;
            }
        }
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk.join("\n"));
    }

    chunks
}

/// Recursively walk a directory, respecting skip_dirs and hidden-file rules.
/// Returns sorted list of (absolute_path, relative_path) pairs.
fn walk_dir(dir: &Path, skip: &HashSet<&str>, skip_hidden: bool) -> Vec<(PathBuf, PathBuf)> {
    let mut results = Vec::new();
    walk_dir_inner(dir, dir, skip, skip_hidden, &mut results);
    results.sort_by(|a, b| a.1.cmp(&b.1));
    results
}

fn walk_dir_inner(
    base: &Path,
    current: &Path,
    skip: &HashSet<&str>,
    skip_hidden: bool,
    out: &mut Vec<(PathBuf, PathBuf)>,
) {
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Collect and sort entries for deterministic order
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if skip_hidden && name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            if !skip.contains(name_str.as_ref()) {
                dirs.push(path);
            }
        } else {
            if let Ok(rel) = path.strip_prefix(base) {
                files.push((path.clone(), rel.to_path_buf()));
            }
        }
    }

    // Sort files by name within each directory (matching Python's sorted(filenames))
    files.sort_by(|a, b| a.1.cmp(&b.1));
    out.extend(files);

    // Recurse into subdirectories
    dirs.sort();
    for dir in dirs {
        walk_dir_inner(base, &dir, skip, skip_hidden, out);
    }
}

// ── Public Ingestion Functions ───────────────────────────────────────────────

/// Ingest a Claude Code conversation JSONL into the pyramid database.
///
/// - Reads the JSONL line by line
/// - Filters for user/assistant messages, skipping toolUseResult entries
/// - Labels speakers as PLAYFUL (user) or CONDUCTOR (assistant) with timestamps
/// - Chunks at ~100 lines with soft boundaries at speaker labels
/// - Returns the slug name
pub fn ingest_conversation(conn: &Connection, slug: &str, jsonl_path: &Path) -> Result<String> {
    let path_str = jsonl_path.to_string_lossy().to_string();

    let (messages, _total) = parse_conversation_messages(jsonl_path, 0)?;
    let transcript = messages.join("\n");
    tracing::info!(
        "Ingesting {}: {} messages, {} chars",
        jsonl_path.file_name().unwrap_or_default().to_string_lossy(),
        messages.len(),
        transcript.len()
    );

    // Create slug if it doesn't exist yet (may have been pre-created by the wizard)
    if db::get_slug(conn, slug)?.is_none() {
        db::create_slug(conn, slug, &ContentType::Conversation, &path_str)?;
    }
    let batch_id = db::create_batch(conn, slug, "initial", &path_str, 0)?;

    // Chunk
    let chunks = chunk_transcript(&transcript);

    // Save chunks
    for (i, chunk_text) in chunks.iter().enumerate() {
        db::insert_chunk(conn, slug, batch_id, i as i64, chunk_text)?;
    }

    tracing::info!("Slug '{slug}': {} chunks saved", chunks.len());
    Ok(slug.to_string())
}

/// Ingest only the NEW portion of a JSONL (messages after skip_messages).
///
/// Used for the "grow" command to extend existing pyramids with new conversation data.
/// Returns `None` if no new messages were found.
pub fn ingest_continuation(
    conn: &Connection,
    slug: &str,
    jsonl_path: &Path,
    skip_messages: usize,
) -> Result<Option<String>> {
    let (messages, total_count) = parse_conversation_messages(jsonl_path, skip_messages)?;

    if messages.is_empty() {
        tracing::info!("No new messages found (total: {total_count}, skipped: {skip_messages})");
        return Ok(None);
    }

    let transcript = messages.join("\n");
    tracing::info!(
        "Ingesting continuation of {} (skipping first {skip_messages} messages): {} new messages, {} chars",
        jsonl_path.file_name().unwrap_or_default().to_string_lossy(),
        messages.len(),
        transcript.len()
    );

    // Continuation batch source path includes marker
    let cont_path = format!(
        "{}:continuation:{}+",
        jsonl_path.to_string_lossy(),
        skip_messages
    );

    // Get existing chunk count for offset
    let chunk_offset = db::count_chunks(conn, slug)?;

    let batch_id = db::create_batch(conn, slug, "continuation", &cont_path, chunk_offset)?;

    // Chunk
    let chunks = chunk_transcript(&transcript);

    for (i, chunk_text) in chunks.iter().enumerate() {
        let chunk_index = chunk_offset + i as i64;
        db::insert_chunk(conn, slug, batch_id, chunk_index, chunk_text)?;
    }

    tracing::info!("Slug '{slug}': {} continuation chunks saved", chunks.len());
    Ok(Some(slug.to_string()))
}

/// Ingest a code directory into the pyramid database.
///
/// - Walks the directory recursively, skipping SKIP_DIRS and hidden files
/// - Collects files matching CODE_EXTENSIONS or CONFIG_FILES
/// - Each file = 1 chunk, formatted with metadata header
/// - Computes SHA-256 per file for file hash tracking
/// - Returns IngestResult with slug name, extensions, config files, and file hash info
pub fn ingest_code(conn: &Connection, slug: &str, dir_path: &Path) -> Result<IngestResult> {
    let path_str = dir_path.to_string_lossy().to_string();

    // Check if slug exists — create if not, otherwise append chunks
    let chunk_offset = if let Some(_info) = db::get_slug(conn, slug)? {
        db::count_chunks(conn, slug)?
    } else {
        db::create_slug(conn, slug, &ContentType::Code, &path_str)?;
        0
    };

    let skip = skip_dirs();
    let code_exts = code_extensions();
    let config_fnames = config_files();

    let all_files = walk_dir(dir_path, &skip, true);

    // Filter to code + config files
    struct FileEntry {
        abs_path_str: String,
        rel_path: String,
        content: String,
        raw_bytes: Vec<u8>,
        language: &'static str,
        file_type: &'static str,
        lines: usize,
        is_config: bool,
        ext: String,
        filename: String,
    }

    let mut files: Vec<FileEntry> = Vec::new();

    for (abs_path, rel_path) in &all_files {
        let fname = abs_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Skip hidden files
        if fname.starts_with('.') {
            continue;
        }

        let ext = abs_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_default();

        let is_config = config_fnames.contains(fname.as_str());
        let is_code = code_exts.contains(ext.as_str());

        if !is_code && !is_config {
            continue;
        }

        let raw_bytes = match std::fs::read(abs_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let content = match std::str::from_utf8(&raw_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => String::from_utf8_lossy(&raw_bytes).to_string(),
        };

        if content.trim().is_empty() {
            continue;
        }

        let language = detect_language(abs_path);
        let file_type = if is_config { "config" } else { "code" };
        let line_count = content.matches('\n').count() + 1;

        files.push(FileEntry {
            abs_path_str: abs_path.to_string_lossy().to_string(),
            rel_path: rel_path.to_string_lossy().to_string(),
            content,
            raw_bytes,
            language,
            file_type,
            lines: line_count,
            is_config,
            ext: ext.clone(),
            filename: fname,
        });
    }

    let total_lines: usize = files.iter().map(|f| f.lines).sum();
    let languages: HashSet<&str> = files.iter().map(|f| f.language).collect();
    tracing::info!(
        "Ingesting code from {}: {} files ({total_lines} lines), chunk_offset={chunk_offset}",
        dir_path.display(),
        files.len()
    );

    // Collect unique extensions and config filenames
    let mut collected_extensions: HashSet<String> = HashSet::new();
    let mut collected_config_files: HashSet<String> = HashSet::new();
    for f in &files {
        if f.is_config {
            collected_config_files.insert(f.filename.clone());
        } else {
            collected_extensions.insert(f.ext.clone());
        }
    }

    // Create batch for this path
    let metadata = serde_json::json!({
        "files": files.len(),
        "total_lines": total_lines,
        "languages": languages.into_iter().collect::<Vec<_>>(),
    });
    let batch_type = if chunk_offset == 0 {
        "initial"
    } else {
        "additional"
    };
    let batch_id = db::create_batch(conn, slug, batch_type, &path_str, chunk_offset)?;

    tracing::info!("Code metadata: {}", metadata);

    // Create chunks — 1 file = 1 chunk, and collect file hash info
    let mut file_infos: Vec<IngestFileInfo> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let chunk_content = format!(
            "## FILE: {}\n## LANGUAGE: {}\n## TYPE: {}\n## LINES: {}\n\n{}",
            f.rel_path, f.language, f.file_type, f.lines, f.content
        );
        let chunk_index = chunk_offset + i as i64;
        db::insert_chunk(conn, slug, batch_id, chunk_index, &chunk_content)?;

        // Compute SHA-256 hash of raw file bytes
        let hash = sha256_hex(&f.raw_bytes);
        file_infos.push(IngestFileInfo {
            abs_path: f.abs_path_str.clone(),
            hash,
            chunk_index,
        });
    }

    tracing::info!(
        "Slug '{slug}': {} chunks saved (1 file = 1 chunk, offset {chunk_offset})",
        files.len()
    );

    Ok(IngestResult {
        slug: slug.to_string(),
        ingested_extensions: collected_extensions.into_iter().collect(),
        ingested_config_files: collected_config_files.into_iter().collect(),
        file_infos,
    })
}

/// Ingest a directory of documents (.txt, .md) into the pyramid database.
///
/// - Walks the directory, skipping hidden dirs/files
/// - Each document = 1 chunk, formatted as `## FILE: <rel_path>\n\n<content>`
/// - Computes SHA-256 per file for file hash tracking
/// - Returns IngestResult with slug name, extensions, and file hash info
pub fn ingest_docs(conn: &Connection, slug: &str, dir_path: &Path) -> Result<IngestResult> {
    let path_str = dir_path.to_string_lossy().to_string();

    // Check if slug exists — create if not, otherwise append chunks
    let chunk_offset = if let Some(_info) = db::get_slug(conn, slug)? {
        db::count_chunks(conn, slug)?
    } else {
        db::create_slug(conn, slug, &ContentType::Document, &path_str)?;
        0
    };

    let doc_exts = doc_extensions();
    let empty_skip: HashSet<&str> = HashSet::new();

    let all_files = walk_dir(dir_path, &empty_skip, true);

    // Filter to document files
    struct DocEntry {
        abs_path_str: String,
        rel_path: String,
        content: String,
        raw_bytes: Vec<u8>,
        ext: String,
    }

    let mut doc_entries: Vec<DocEntry> = Vec::new();

    for (abs_path, rel_path) in &all_files {
        let ext = abs_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_default();

        if !doc_exts.contains(ext.as_str()) {
            continue;
        }

        let raw_bytes = match std::fs::read(abs_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let content = match std::str::from_utf8(&raw_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => String::from_utf8_lossy(&raw_bytes).to_string(),
        };

        if content.trim().is_empty() {
            continue;
        }

        doc_entries.push(DocEntry {
            abs_path_str: abs_path.to_string_lossy().to_string(),
            rel_path: rel_path.to_string_lossy().to_string(),
            content,
            raw_bytes,
            ext,
        });
    }

    if doc_entries.is_empty() {
        anyhow::bail!("No documents found in {}", dir_path.display());
    }

    let total_lines: usize = doc_entries
        .iter()
        .map(|d| d.content.matches('\n').count() + 1)
        .sum();
    tracing::info!(
        "Ingesting documents from {}: {} files ({total_lines} lines), chunk_offset={chunk_offset}",
        dir_path.display(),
        doc_entries.len()
    );

    // Collect unique extensions
    let mut collected_extensions: HashSet<String> = HashSet::new();
    for d in &doc_entries {
        collected_extensions.insert(d.ext.clone());
    }

    // Create batch for this path
    let batch_type = if chunk_offset == 0 {
        "initial"
    } else {
        "additional"
    };
    let batch_id = db::create_batch(conn, slug, batch_type, &path_str, chunk_offset)?;

    // Each document = 1 chunk, collect file hash info
    let mut file_infos: Vec<IngestFileInfo> = Vec::new();
    for (i, d) in doc_entries.iter().enumerate() {
        let chunk_content = format!("## FILE: {}\n\n{}", d.rel_path, d.content);
        let chunk_index = chunk_offset + i as i64;
        db::insert_chunk(conn, slug, batch_id, chunk_index, &chunk_content)?;

        let hash = sha256_hex(&d.raw_bytes);
        file_infos.push(IngestFileInfo {
            abs_path: d.abs_path_str.clone(),
            hash,
            chunk_index,
        });
    }

    tracing::info!(
        "Slug '{slug}': {} documents saved (offset {chunk_offset})",
        doc_entries.len()
    );

    Ok(IngestResult {
        slug: slug.to_string(),
        ingested_extensions: collected_extensions.into_iter().collect(),
        ingested_config_files: Vec::new(), // docs have no config files
        file_infos,
    })
}

// ── WS-INGEST-PRIMITIVE: Ingest signature, scanning, change detection ───────

/// Compute the `ingest_signature` that uniquely identifies how a source was
/// chunked. Formula locked in knowledge-transfer Q11:
///
///  - conversation: sha256("conversation:" + chunk_target_lines + ":" + chunk_target_tokens)
///  - code: sha256("code:" + sorted(code_extensions) + ":" + sorted(skip_dirs) + ":" + sorted(config_files) + ":" + chunk_target_lines + ":" + chunk_target_tokens)
///  - document: sha256("document:" + sorted(doc_extensions) + ":" + chunk_target_lines + ":" + chunk_target_tokens)
///  - vocabulary / question: unique per-slug (use slug itself)
///
/// Consumed by WS-MULTI-CHAIN-OVERLAY to detect whether two pyramids share
/// the same chunking.
pub fn ingest_signature(content_type: &ContentType, config: &IngestConfig) -> String {
    match content_type {
        ContentType::Conversation => {
            let input = format!(
                "conversation:{}:{}",
                config.chunk_target_lines, config.chunk_target_tokens
            );
            sha256_hex(input.as_bytes())
        }
        ContentType::Code => {
            let mut exts = config.code_extensions.clone();
            exts.sort();
            let mut dirs = config.skip_dirs.clone();
            dirs.sort();
            let mut cfgs = config.config_files.clone();
            cfgs.sort();
            let input = format!(
                "code:{}:{}:{}:{}:{}",
                exts.join(","),
                dirs.join(","),
                cfgs.join(","),
                config.chunk_target_lines,
                config.chunk_target_tokens
            );
            sha256_hex(input.as_bytes())
        }
        ContentType::Document => {
            let mut exts = config.doc_extensions.clone();
            exts.sort();
            let input = format!(
                "document:{}:{}:{}",
                exts.join(","),
                config.chunk_target_lines,
                config.chunk_target_tokens
            );
            sha256_hex(input.as_bytes())
        }
        ContentType::Vine | ContentType::Question => {
            // Unique per-slug; callers should pass slug as the signature.
            // Return a sentinel — actual slug-based signature is handled
            // at the call site where the slug is known.
            "slug-unique".to_string()
        }
    }
}

/// Build a default `IngestConfig` from the current Tier2Config defaults and
/// the hardcoded extension/directory sets in this module.
pub fn default_ingest_config() -> IngestConfig {
    let t2 = super::Tier2Config::default();
    IngestConfig {
        chunk_target_lines: t2.chunk_target_lines,
        chunk_target_tokens: 0, // Not yet used; kept for forward compat with Q11 formula
        code_extensions: {
            let mut v: Vec<String> = code_extensions()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            v.sort();
            v
        },
        skip_dirs: {
            let mut v: Vec<String> = skip_dirs().into_iter().map(|s| s.to_string()).collect();
            v.sort();
            v
        },
        config_files: {
            let mut v: Vec<String> = config_files().into_iter().map(|s| s.to_string()).collect();
            v.sort();
            v
        },
        doc_extensions: {
            let mut v: Vec<String> = doc_extensions()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            v.sort();
            v
        },
    }
}

/// Scan a directory for ingestible files based on content type.
///
/// For conversation: scans for `.jsonl` files.
/// For code: scans for source files matching configured extensions + config files.
/// For document: scans for document files matching configured extensions.
pub fn scan_source_directory(path: &str, content_type: &ContentType) -> Result<Vec<SourceFile>> {
    let dir = Path::new(path);
    if !dir.exists() {
        anyhow::bail!("Source path does not exist: {}", path);
    }

    // Conversation type accepts either a single .jsonl file OR a directory
    // containing .jsonl files. All other types require a directory.
    if dir.is_file() {
        if *content_type == ContentType::Conversation {
            if let Some(ext) = dir.extension().and_then(|e| e.to_str()) {
                if ext == "jsonl" {
                    if let Some(sf) = source_file_from_path(dir) {
                        return Ok(vec![sf]);
                    }
                }
            }
            anyhow::bail!("Conversation file must be .jsonl: {}", path);
        } else {
            anyhow::bail!("Source path is not a directory: {}", path);
        }
    }

    if !dir.is_dir() {
        anyhow::bail!("Source path is not a file or directory: {}", path);
    }

    let mut results = Vec::new();

    match content_type {
        ContentType::Conversation => {
            // Scan for .jsonl files (non-recursive — conversations are flat)
            let entries = std::fs::read_dir(dir)
                .with_context(|| format!("Failed to read directory: {}", path))?;
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                        if ext == "jsonl" {
                            if let Some(sf) = source_file_from_path(&p) {
                                results.push(sf);
                            }
                        }
                    }
                }
            }
        }
        ContentType::Code => {
            let skip = skip_dirs();
            let code_exts = code_extensions();
            let config_fnames = config_files();
            let all_files = walk_dir(dir, &skip, true);
            for (abs_path, _rel_path) in &all_files {
                let fname = abs_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if fname.starts_with('.') {
                    continue;
                }
                let ext = abs_path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                    .unwrap_or_default();
                let is_config = config_fnames.contains(fname.as_str());
                let is_code = code_exts.contains(ext.as_str());
                if !is_code && !is_config {
                    continue;
                }
                if let Some(sf) = source_file_from_path(abs_path) {
                    results.push(sf);
                }
            }
        }
        ContentType::Document => {
            let doc_exts = doc_extensions();
            let empty_skip: HashSet<&str> = HashSet::new();
            let all_files = walk_dir(dir, &empty_skip, true);
            for (abs_path, _rel_path) in &all_files {
                let ext = abs_path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                    .unwrap_or_default();
                if !doc_exts.contains(ext.as_str()) {
                    continue;
                }
                if let Some(sf) = source_file_from_path(abs_path) {
                    results.push(sf);
                }
            }
        }
        ContentType::Vine | ContentType::Question => {
            // Vine/Question types don't have file-based scanning
            tracing::warn!(
                "scan_source_directory called for {:?} — no file scanning for this type",
                content_type
            );
        }
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Build a `SourceFile` from a filesystem path, reading mtime and computing hash.
fn source_file_from_path(p: &Path) -> Option<SourceFile> {
    let metadata = std::fs::metadata(p).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .map(|t| {
            let dt: chrono::DateTime<Utc> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_default();
    let raw_bytes = std::fs::read(p).ok()?;
    let file_hash = sha256_hex(&raw_bytes);
    let size = metadata.len();
    Some(SourceFile {
        path: p.to_string_lossy().to_string(),
        mtime,
        file_hash,
        size,
    })
}

/// Compare current scan results against existing ingest records to detect
/// new, modified, and deleted files.
pub fn detect_changes(
    conn: &Connection,
    slug: &str,
    sig: &str,
    current_files: &[SourceFile],
) -> Result<ChangeSet> {
    let existing_records = db::get_ingest_records_for_slug(conn, slug)?;

    // Build a lookup from (source_path, ingest_signature) -> record
    let mut existing_map: std::collections::HashMap<String, &IngestRecord> =
        std::collections::HashMap::new();
    for rec in &existing_records {
        if rec.ingest_signature == sig {
            existing_map.insert(rec.source_path.clone(), rec);
        }
    }

    let mut new_files = Vec::new();
    let mut modified_files = Vec::new();
    let mut unchanged_count: usize = 0;

    // Track which existing paths we've seen in the current scan
    let mut seen_paths: HashSet<String> = HashSet::new();

    for sf in current_files {
        seen_paths.insert(sf.path.clone());
        match existing_map.get(&sf.path) {
            None => {
                new_files.push(sf.clone());
            }
            Some(rec) => {
                // Check if hash or mtime changed
                let hash_changed = rec
                    .file_hash
                    .as_ref()
                    .map(|h| h != &sf.file_hash)
                    .unwrap_or(true);
                let mtime_changed = rec
                    .file_mtime
                    .as_ref()
                    .map(|m| m != &sf.mtime)
                    .unwrap_or(true);
                if hash_changed || mtime_changed {
                    modified_files.push(sf.clone());
                } else {
                    unchanged_count += 1;
                }
            }
        }
    }

    // Deleted = existing records whose paths aren't in current scan
    let deleted_paths: Vec<String> = existing_map
        .keys()
        .filter(|p| !seen_paths.contains(p.as_str()))
        .cloned()
        .collect();

    Ok(ChangeSet {
        new_files,
        modified_files,
        deleted_paths,
        unchanged_count,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language(Path::new("foo.rs")), "rust");
        assert_eq!(detect_language(Path::new("bar.tsx")), "typescript");
        assert_eq!(detect_language(Path::new("baz.py")), "python");
        assert_eq!(detect_language(Path::new("x.json")), "json");
        assert_eq!(detect_language(Path::new("y.toml")), "toml");
        assert_eq!(detect_language(Path::new("z.yaml")), "yaml");
        assert_eq!(detect_language(Path::new("a.md")), "markdown");
        assert_eq!(detect_language(Path::new("b.css")), "css");
        assert_eq!(detect_language(Path::new("c.html")), "html");
        assert_eq!(detect_language(Path::new("d.sql")), "sql");
        assert_eq!(detect_language(Path::new("e.swift")), "swift");
        assert_eq!(detect_language(Path::new("f.kt")), "kotlin");
        assert_eq!(detect_language(Path::new("no_ext")), "unknown");
    }

    #[test]
    fn test_extract_text_string() {
        let v = Value::String("hello world".into());
        assert_eq!(extract_text_from_content(&v), "hello world");
    }

    #[test]
    fn test_extract_text_array() {
        let v = serde_json::json!([
            {"type": "text", "text": "First part"},
            {"type": "tool_use", "name": "grep"},
            {"type": "text", "text": "Second part"}
        ]);
        let result = extract_text_from_content(&v);
        assert!(result.contains("First part"));
        assert!(result.contains("[Tool: grep]"));
        assert!(result.contains("Second part"));
    }

    #[test]
    fn test_extract_text_null() {
        assert_eq!(extract_text_from_content(&Value::Null), "");
    }

    #[test]
    fn test_chunk_transcript_soft_boundary() {
        // Build a transcript with a speaker label at line 80 (above 70% of 100)
        let mut lines = Vec::new();
        for i in 0..79 {
            lines.push(format!("line {i}"));
        }
        lines.push("--- PLAYFUL [2026-03-20T14:30] ---".to_string());
        for i in 80..120 {
            lines.push(format!("line {i}"));
        }
        let transcript = lines.join("\n");
        let chunks = chunk_transcript(&transcript);

        // Should split at the speaker label
        assert_eq!(chunks.len(), 2);
        // First chunk should NOT contain the boundary line
        assert!(!chunks[0].contains("--- PLAYFUL"));
        // Second chunk should start with the boundary line
        assert!(chunks[1].starts_with("--- PLAYFUL"));
    }

    #[test]
    fn test_chunk_transcript_hard_limit() {
        // Build a transcript with 140 lines, no speaker labels
        let mut lines = Vec::new();
        for i in 0..140 {
            lines.push(format!("line {i}"));
        }
        let transcript = lines.join("\n");
        let chunks = chunk_transcript(&transcript);

        // Should hit hard limit at 130 lines
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn test_chunk_transcript_below_threshold() {
        // 50 lines — no splitting
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!("line {i}"));
        }
        let transcript = lines.join("\n");
        let chunks = chunk_transcript(&transcript);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_skip_dirs_contains_expected() {
        let dirs = skip_dirs();
        assert!(dirs.contains(".git"));
        assert!(dirs.contains("node_modules"));
        assert!(dirs.contains("target"));
        assert!(dirs.contains("__pycache__"));
        assert!(dirs.contains(".cache"));
        assert!(dirs.contains("coverage"));
        // Project-specific entries removed
        assert!(!dirs.contains("relay-app"));
        assert!(!dirs.contains("pyramid-prototype"));
        assert!(!dirs.contains("agentwire"));
        assert!(!dirs.contains("agentspace"));
    }

    // ── WS-INGEST-PRIMITIVE tests ───────────────────────────────────────

    #[test]
    fn test_ingest_signature_deterministic() {
        let config = default_ingest_config();
        let sig1 = ingest_signature(&ContentType::Code, &config);
        let sig2 = ingest_signature(&ContentType::Code, &config);
        assert_eq!(sig1, sig2, "Same config should produce same signature");
        // Should be a valid hex SHA-256 (64 chars)
        assert_eq!(sig1.len(), 64);
    }

    #[test]
    fn test_ingest_signature_differs_by_content_type() {
        let config = default_ingest_config();
        let sig_conv = ingest_signature(&ContentType::Conversation, &config);
        let sig_code = ingest_signature(&ContentType::Code, &config);
        let sig_doc = ingest_signature(&ContentType::Document, &config);
        assert_ne!(sig_conv, sig_code);
        assert_ne!(sig_conv, sig_doc);
        assert_ne!(sig_code, sig_doc);
    }

    #[test]
    fn test_ingest_signature_differs_by_config() {
        let mut config1 = default_ingest_config();
        let mut config2 = default_ingest_config();
        config2.chunk_target_lines = 200; // different from default 100
        let sig1 = ingest_signature(&ContentType::Conversation, &config1);
        let sig2 = ingest_signature(&ContentType::Conversation, &config2);
        assert_ne!(
            sig1, sig2,
            "Different chunk_target_lines should produce different signatures"
        );

        // Also test code with different extensions
        config1.code_extensions = vec![".rs".to_string()];
        config2.code_extensions = vec![".rs".to_string(), ".py".to_string()];
        config2.chunk_target_lines = config1.chunk_target_lines; // same lines
        let sig3 = ingest_signature(&ContentType::Code, &config1);
        let sig4 = ingest_signature(&ContentType::Code, &config2);
        assert_ne!(
            sig3, sig4,
            "Different extensions should produce different signatures"
        );
    }

    #[test]
    fn test_ingest_signature_vine_question_slug_unique() {
        let config = default_ingest_config();
        let sig_vine = ingest_signature(&ContentType::Vine, &config);
        let sig_question = ingest_signature(&ContentType::Question, &config);
        assert_eq!(sig_vine, "slug-unique");
        assert_eq!(sig_question, "slug-unique");
    }

    #[test]
    fn test_ingest_record_roundtrip() {
        // Create an in-memory DB and initialize schema
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let sig = ingest_signature(&ContentType::Code, &default_ingest_config());

        // Create a slug first (FK not enforced on ingest_records, but let's
        // be thorough)
        db::create_slug(&conn, "test-slug", &ContentType::Code, "/src").unwrap();

        // Save a record
        let record = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/main.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("abc123".to_string()),
            file_mtime: Some("2026-04-08T10:00:00Z".to_string()),
            status: "pending".to_string(),
            build_id: None,
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let row_id = db::save_ingest_record(&conn, &record).unwrap();
        assert!(row_id > 0);

        // Get it back
        let fetched = db::get_ingest_record(&conn, "test-slug", "/src/main.rs", &sig)
            .unwrap()
            .expect("record should exist");
        assert_eq!(fetched.slug, "test-slug");
        assert_eq!(fetched.source_path, "/src/main.rs");
        assert_eq!(fetched.status, "pending");
        assert_eq!(fetched.file_hash, Some("abc123".to_string()));
        assert_eq!(fetched.ingest_signature, sig);

        // Mark processing
        db::mark_ingest_processing(&conn, fetched.id).unwrap();
        let updated = db::get_ingest_record(&conn, "test-slug", "/src/main.rs", &sig)
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, "processing");

        // Mark complete
        db::mark_ingest_complete(&conn, fetched.id, "build-001").unwrap();
        let completed = db::get_ingest_record(&conn, "test-slug", "/src/main.rs", &sig)
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "complete");
        assert_eq!(completed.build_id, Some("build-001".to_string()));

        // Get all for slug
        let all = db::get_ingest_records_for_slug(&conn, "test-slug").unwrap();
        assert_eq!(all.len(), 1);

        // Upsert — update existing record
        let updated_record = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/main.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("def456".to_string()),
            file_mtime: Some("2026-04-08T11:00:00Z".to_string()),
            status: "pending".to_string(),
            build_id: None,
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &updated_record).unwrap();
        let upserted = db::get_ingest_record(&conn, "test-slug", "/src/main.rs", &sig)
            .unwrap()
            .unwrap();
        assert_eq!(upserted.file_hash, Some("def456".to_string()));
        assert_eq!(upserted.status, "pending"); // status reset by upsert

        // Mark stale
        db::mark_ingest_stale(&conn, "test-slug", "/src/main.rs").unwrap();
        let staled = db::get_ingest_record(&conn, "test-slug", "/src/main.rs", &sig)
            .unwrap()
            .unwrap();
        assert_eq!(staled.status, "stale");

        // Mark failed
        let record2 = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/lib.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("ghi789".to_string()),
            file_mtime: Some("2026-04-08T10:00:00Z".to_string()),
            status: "pending".to_string(),
            build_id: None,
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let id2 = db::save_ingest_record(&conn, &record2).unwrap();
        // id2 might be 0 on upsert when ON CONFLICT fires; get from DB
        let rec2 = db::get_ingest_record(&conn, "test-slug", "/src/lib.rs", &sig)
            .unwrap()
            .unwrap();
        db::mark_ingest_failed(&conn, rec2.id, "LLM timeout").unwrap();
        let failed = db::get_ingest_record(&conn, "test-slug", "/src/lib.rs", &sig)
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.error_message, Some("LLM timeout".to_string()));

        // get_pending_ingests — lib.rs is failed, main.rs is stale, neither pending
        let pending = db::get_pending_ingests(&conn, "test-slug").unwrap();
        assert_eq!(pending.len(), 0);

        // Save another as pending and verify it shows up
        let record3 = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/mod.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("jkl012".to_string()),
            file_mtime: None,
            status: "pending".to_string(),
            build_id: None,
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &record3).unwrap();
        let pending2 = db::get_pending_ingests(&conn, "test-slug").unwrap();
        assert_eq!(pending2.len(), 1);
        assert_eq!(pending2[0].source_path, "/src/mod.rs");
    }

    #[test]
    fn test_detect_changes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        db::create_slug(&conn, "test-slug", &ContentType::Code, "/src").unwrap();

        let sig = ingest_signature(&ContentType::Code, &default_ingest_config());

        // Insert an existing record for file_a
        let existing = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/file_a.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("hash_a".to_string()),
            file_mtime: Some("2026-04-08T10:00:00Z".to_string()),
            status: "complete".to_string(),
            build_id: Some("build-001".to_string()),
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &existing).unwrap();

        // Also insert a record for file_b (will be "deleted")
        let existing_b = IngestRecord {
            id: 0,
            slug: "test-slug".to_string(),
            source_path: "/src/file_b.rs".to_string(),
            content_type: "code".to_string(),
            ingest_signature: sig.clone(),
            file_hash: Some("hash_b".to_string()),
            file_mtime: Some("2026-04-08T09:00:00Z".to_string()),
            status: "complete".to_string(),
            build_id: Some("build-001".to_string()),
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &existing_b).unwrap();

        // Current scan has: file_a (modified), file_c (new), no file_b (deleted)
        let current_files = vec![
            SourceFile {
                path: "/src/file_a.rs".to_string(),
                mtime: "2026-04-08T11:00:00Z".to_string(), // different mtime
                file_hash: "hash_a_v2".to_string(),        // different hash
                size: 100,
            },
            SourceFile {
                path: "/src/file_c.rs".to_string(),
                mtime: "2026-04-08T10:30:00Z".to_string(),
                file_hash: "hash_c".to_string(),
                size: 200,
            },
        ];

        let changes = detect_changes(&conn, "test-slug", &sig, &current_files).unwrap();
        assert_eq!(changes.new_files.len(), 1, "file_c is new");
        assert_eq!(changes.new_files[0].path, "/src/file_c.rs");
        assert_eq!(changes.modified_files.len(), 1, "file_a is modified");
        assert_eq!(changes.modified_files[0].path, "/src/file_a.rs");
        assert_eq!(changes.deleted_paths.len(), 1, "file_b is deleted");
        assert_eq!(changes.deleted_paths[0], "/src/file_b.rs");
        assert_eq!(changes.unchanged_count, 0);
    }

    #[test]
    fn test_scan_source_directory_conversation() {
        // Create a temp dir with a .jsonl file
        let tmp = std::env::temp_dir().join("test_ingest_scan_conv");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("session.jsonl"), "{}").unwrap();
        std::fs::write(tmp.join("notes.txt"), "not a jsonl").unwrap();

        let files =
            scan_source_directory(tmp.to_str().unwrap(), &ContentType::Conversation).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("session.jsonl"));
        assert_eq!(files[0].file_hash.len(), 64); // SHA-256 hex

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_default_ingest_config() {
        let config = default_ingest_config();
        assert_eq!(config.chunk_target_lines, 100);
        assert_eq!(config.chunk_target_tokens, 0);
        assert!(!config.code_extensions.is_empty());
        assert!(!config.skip_dirs.is_empty());
        assert!(!config.config_files.is_empty());
        assert!(!config.doc_extensions.is_empty());
        // Should be sorted
        let mut sorted_exts = config.code_extensions.clone();
        sorted_exts.sort();
        assert_eq!(config.code_extensions, sorted_exts);
    }
}
