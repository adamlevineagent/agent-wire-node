// pyramid/ingest.rs — Phase 2: Ingestion
//
// Three ingestion pipelines (conversation, code, documents) plus continuation support.
// Each reads source material, chunks it, and stores it in the pyramid SQLite database
// using the pyramid_slugs / pyramid_batches / pyramid_chunks schema.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::db;
use super::types::ContentType;

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

fn chunk_target_lines() -> usize { super::Tier2Config::default().chunk_target_lines }

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
/// - Each document = 1 chunk, formatted as `## DOCUMENT: <rel_path>\n\n<content>`
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
        let chunk_content = format!("## DOCUMENT: {}\n\n{}", d.rel_path, d.content);
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
}
