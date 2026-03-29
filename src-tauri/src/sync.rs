use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncDirection {
    Upload,   // Push local → Wire (steward)
    Download, // Pull Wire → local (reader)
    Both,     // Bidirectional sync
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedFolder {
    pub corpus_slug: String,
    pub direction: SyncDirection,
}

/// Tracks the state of document sync
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct SyncState {
    pub linked_folders: HashMap<String, LinkedFolder>, // folder_path -> LinkedFolder
    pub cached_documents: Vec<CachedDocument>,
    pub total_size_bytes: u64,
    pub last_sync_at: Option<String>,
    pub is_syncing: bool,
    #[serde(default)]
    pub auto_sync_enabled: bool,
    #[serde(default = "default_auto_sync_interval")]
    pub auto_sync_interval_secs: u64,
    #[serde(default)]
    pub sync_progress: Option<String>, // e.g. "Pulling 3/52..."
    #[serde(default)]
    pub pinned_versions: Vec<String>, // document IDs of pinned versions
    #[serde(default = "default_storage_quota")]
    pub storage_quota_mb: u64,
    #[serde(default)]
    pub conflicts: Vec<ConflictInfo>,
}

fn default_auto_sync_interval() -> u64 {
    900
} // 15 minutes
fn default_storage_quota() -> u64 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileStatus {
    InSync,    // Local matches remote
    NeedsPull, // Remote has newer version (or file doesn't exist locally)
    NeedsPush, // Local has newer version (or file doesn't exist remotely)
    Pulling,   // Currently downloading
    Pushing,   // Currently uploading
    Skipped,   // Already exists remotely (409) or no action needed
    Error,     // Last sync attempt failed
}

impl Default for FileStatus {
    fn default() -> Self {
        FileStatus::InSync
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDocument {
    pub document_id: String,
    pub corpus_slug: String,
    pub source_path: String,
    pub body_hash: String,
    pub file_size_bytes: u64,
    pub cached_at: String,
    #[serde(default)]
    pub sync_status: FileStatus,
    #[serde(default)]
    pub error_message: Option<String>,
    /// Document publish status on the server: "draft", "published", "retracted"
    #[serde(default)]
    pub document_status: Option<String>,
}

/// Document info returned from Wire API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub id: String,
    pub body_hash: String,
    pub source_path: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub family_id: Option<String>,
    #[serde(default)]
    pub version_number: Option<i32>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl DocumentInfo {
    /// Get the effective local path for this document.
    /// Falls back to generating a filename from the title or document ID.
    pub fn effective_path(&self) -> String {
        if let Some(ref p) = self.source_path {
            if !p.is_empty() {
                return p.clone();
            }
        }
        // Generate filename from title or ID
        let base = self.title.as_deref().unwrap_or(&self.id);
        let slug: String = base
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let slug = if slug.is_empty() {
            self.id.clone()
        } else {
            slug
        };
        // Add extension based on format
        let ext = match self.format.as_deref() {
            Some("text/markdown") => ".md",
            Some("text/html") => ".html",
            Some("text/plain") => ".txt",
            Some("application/pdf") => ".pdf",
            _ => ".md",
        };
        if slug.ends_with(ext) {
            slug
        } else {
            format!("{}{}", slug, ext)
        }
    }
}

/// Diff result for a single corpus
#[derive(Debug)]
pub struct SyncDiff {
    pub to_push: Vec<LocalDocument>, // local files not on server
    pub to_pull: Vec<DocumentInfo>,  // server docs not on local
    pub to_update: Vec<(LocalDocument, DocumentInfo)>, // local files with different hash
    pub hash_matched: Vec<(LocalDocument, DocumentInfo)>, // local files matched to remote by body_hash (path mismatch)
}

/// Local document found via directory walking
#[derive(Debug, Clone)]
pub struct LocalDocument {
    pub path: PathBuf,
    pub relative_path: String,
    pub body_hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub id: String,
    pub family_id: Option<String>,
    pub version_number: i32,
    pub title: Option<String>,
    pub status: String,
    pub body_hash: String,
    pub word_count: Option<i32>,
    pub format: Option<String>,
    pub source_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionHistoryResponse {
    pub family_id: String,
    pub document_id: String,
    pub total_versions: i32,
    pub versions: Vec<VersionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    pub tag: String, // "equal", "insert", "delete"
    pub content: String,
    pub old_offset: Option<usize>,
    pub new_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    pub source_path: String,
    pub corpus_slug: String,
    pub local_hash: String,
    pub remote_hash: String,
    pub local_mtime: Option<String>,
    pub remote_updated_at: Option<String>,
}

// --- Persistence ------------------------------------------------------------

/// Save sync state to disk
pub fn save_sync_state(data_dir: &Path, state: &SyncState) {
    let path = data_dir.join("sync_state.json");
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
        tracing::debug!(
            "Sync state saved ({} linked folders)",
            state.linked_folders.len()
        );
    }
}

/// Load sync state from disk
pub fn load_sync_state(data_dir: &Path) -> Option<SyncState> {
    let path = data_dir.join("sync_state.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

// --- Folder Linking ---------------------------------------------------------

/// Link a local folder to a Wire corpus
pub fn link_folder(
    sync_state: &mut SyncState,
    folder_path: &str,
    corpus_slug: &str,
    direction: SyncDirection,
) -> Result<(), String> {
    let path = Path::new(folder_path);
    if !path.exists() || !path.is_dir() {
        return Err(format!("Directory does not exist: {}", folder_path));
    }
    tracing::info!(
        "Linked folder {} -> corpus {} ({:?})",
        folder_path,
        corpus_slug,
        direction
    );
    sync_state.linked_folders.insert(
        folder_path.to_string(),
        LinkedFolder {
            corpus_slug: corpus_slug.to_string(),
            direction,
        },
    );
    Ok(())
}

/// Unlink a folder from a corpus
pub fn unlink_folder(sync_state: &mut SyncState, folder_path: &str) -> Result<(), String> {
    if sync_state.linked_folders.remove(folder_path).is_some() {
        tracing::info!("Unlinked folder {}", folder_path);
        Ok(())
    } else {
        Err(format!("Folder not linked: {}", folder_path))
    }
}

// --- Document Sync ----------------------------------------------------------

#[derive(Deserialize)]
struct DocumentListResponse {
    items: Vec<DocumentInfo>,
    total: Option<i64>,
}

/// Fetch document list for a corpus from the Wire API (with pagination)
pub async fn fetch_corpus_documents(
    api_url: &str,
    access_token: &str,
    corpus_slug: &str,
) -> Result<Vec<DocumentInfo>, String> {
    let client = reqwest::Client::new();
    let mut all_docs = Vec::new();
    let mut offset: i64 = 0;
    let limit: i64 = 100;

    loop {
        let url = format!(
            "{}/api/v1/wire/corpora/{}/documents?limit={}&offset={}",
            api_url, corpus_slug, limit, offset
        );

        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| format!("Failed to fetch corpus documents: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Corpus fetch failed ({}): {}", status, text));
        }

        let page: DocumentListResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse documents response: {}", e))?;

        let page_count = page.items.len() as i64;
        all_docs.extend(page.items);

        let total = page.total.unwrap_or(all_docs.len() as i64);
        offset += limit;
        if offset >= total || page_count == 0 {
            break;
        }
    }

    tracing::info!(
        "Found {} documents in corpus {}",
        all_docs.len(),
        corpus_slug
    );
    Ok(all_docs)
}

/// Walk a local directory and compute hashes for all files.
/// Respects .gitignore at every directory level (via the `ignore` crate).
/// Also respects .wireignore if present (same glob syntax as .gitignore).
pub fn scan_local_folder(folder_path: &str) -> Result<Vec<LocalDocument>, String> {
    let root = Path::new(folder_path);
    if !root.exists() || !root.is_dir() {
        return Err(format!("Directory does not exist: {}", folder_path));
    }

    // Build walker that respects .gitignore files at every level.
    // The `ignore` crate automatically reads .gitignore, .git/info/exclude,
    // and the global gitignore. We also add .wireignore as a custom ignore file.
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(true) // skip hidden files/dirs (dotfiles)
        .git_ignore(true) // respect .gitignore
        .git_global(true) // respect global gitignore
        .git_exclude(true); // respect .git/info/exclude

    // Add .wireignore support — same syntax as .gitignore, project-specific overrides
    let wireignore_path = root.join(".wireignore");
    if wireignore_path.exists() {
        builder.add_custom_ignore_filename(".wireignore");
    }

    let mut docs = Vec::new();
    for entry in builder.build().filter_map(|e| e.ok()) {
        // Skip symlinks entirely — they could point outside the directory boundary
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            let path = entry.path().to_path_buf();

            // Skip system files that may not be in .gitignore
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if file_name == "Thumbs.db" || file_name == "desktop.ini" {
                continue;
            }

            // Skip .versions directory (used for pinned version archives)
            let path_str = path.to_string_lossy();
            if path_str.contains("/.versions/") || path_str.contains("\\.versions\\") {
                continue;
            }

            let relative_path = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            // Read file as UTF-8 string and compute hash (matches TS TextEncoder)
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let body_hash = compute_sha256(&content);
                    let size = content.len() as u64;
                    docs.push(LocalDocument {
                        path,
                        relative_path,
                        body_hash,
                        size,
                    });
                }
                Err(e) => {
                    // Binary files will fail read_to_string — this is expected and fine
                    tracing::debug!("Skipping non-UTF8 file {}: {}", path.display(), e);
                }
            }
        }
    }

    tracing::info!(
        "Scanned {} files in {} (gitignore-aware)",
        docs.len(),
        folder_path
    );
    Ok(docs)
}

/// Compute diff between local folder and remote corpus
pub fn compute_diff(local_docs: &[LocalDocument], remote_docs: &[DocumentInfo]) -> SyncDiff {
    // Build remote lookup using effective_path (handles missing source_path)
    let remote_paths: Vec<(String, &DocumentInfo)> = remote_docs
        .iter()
        .map(|d| (d.effective_path(), d))
        .collect();
    let remote_by_path: HashMap<&str, &DocumentInfo> =
        remote_paths.iter().map(|(p, d)| (p.as_str(), *d)).collect();

    let local_by_path: HashMap<&str, &LocalDocument> = local_docs
        .iter()
        .map(|d| (d.relative_path.as_str(), d))
        .collect();

    let mut to_push = Vec::new();
    let mut to_update = Vec::new();
    // Track which remote docs were matched by path so we can do hash fallback on the rest
    let mut matched_remote_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut unmatched_local: Vec<LocalDocument> = Vec::new();

    for local_doc in local_docs {
        match remote_by_path.get(local_doc.relative_path.as_str()) {
            Some(remote_doc) => {
                matched_remote_ids.insert(remote_doc.id.as_str());
                if local_doc.body_hash != remote_doc.body_hash {
                    to_update.push((local_doc.clone(), (*remote_doc).clone()));
                }
            }
            None => {
                unmatched_local.push(local_doc.clone());
            }
        }
    }

    // Build a set of body_hash -> DocumentInfo for unmatched remote docs (hash-based fallback)
    let unmatched_remote: Vec<&DocumentInfo> = remote_docs
        .iter()
        .filter(|d| !matched_remote_ids.contains(d.id.as_str()))
        .collect();
    let mut remote_by_hash: HashMap<&str, &DocumentInfo> = HashMap::new();
    for doc in &unmatched_remote {
        // First match wins; duplicates are ignored
        remote_by_hash.entry(doc.body_hash.as_str()).or_insert(doc);
    }

    // Collect local file hashes for the pull-side fallback
    let local_hashes: std::collections::HashSet<&str> =
        local_docs.iter().map(|d| d.body_hash.as_str()).collect();

    // Hash-based fallback for unmatched local files
    let mut hash_matched_remote_ids: std::collections::HashSet<&str> =
        std::collections::HashSet::new();
    let mut hash_matched: Vec<(LocalDocument, DocumentInfo)> = Vec::new();
    for local_doc in unmatched_local {
        if let Some(remote_doc) = remote_by_hash.get(local_doc.body_hash.as_str()) {
            // Same content exists on server — treat as in sync (path mismatch only)
            hash_matched_remote_ids.insert(remote_doc.id.as_str());
            tracing::debug!(
                "Hash fallback match: local '{}' == remote '{}' (hash {})",
                local_doc.relative_path,
                remote_doc.id,
                &local_doc.body_hash[..12]
            );
            hash_matched.push((local_doc, (*remote_doc).clone()));
        } else {
            to_push.push(local_doc);
        }
    }

    // Pull: remote docs whose effective_path doesn't exist locally,
    // excluding those matched by hash fallback (content already exists locally)
    let to_pull: Vec<DocumentInfo> = remote_docs
        .iter()
        .filter(|d| {
            let path = d.effective_path();
            !local_by_path.contains_key(path.as_str())
                && !hash_matched_remote_ids.contains(d.id.as_str())
                && !local_hashes.contains(d.body_hash.as_str())
        })
        .cloned()
        .collect();

    SyncDiff {
        to_push,
        to_pull,
        to_update,
        hash_matched,
    }
}

/// Push a new document to the Wire API
pub async fn push_document(
    api_url: &str,
    access_token: &str,
    corpus_slug: &str,
    local_doc: &LocalDocument,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/corpora/{}/documents", api_url, corpus_slug);

    let body_text = std::fs::read_to_string(&local_doc.path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Derive title from filename
    let title = Path::new(&local_doc.relative_path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace('-', " ").replace('_', " "))
        .unwrap_or_else(|| local_doc.relative_path.clone().into());

    // Infer format from extension.
    // Code files and unknown extensions default to text/plain (not markdown).
    let format = match Path::new(&local_doc.relative_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("pdf") => "application/pdf",
        _ => "text/plain", // code files, .txt, and anything else
    };

    let body = serde_json::json!({
        "title": title,
        "body": body_text,
        "format": format,
        "source_path": local_doc.relative_path,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Push failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Push failed ({}): {}", status, text));
    }

    #[derive(Deserialize)]
    struct PushResponse {
        id: String,
    }
    let result: PushResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse push response: {}", e))?;

    tracing::info!(
        "Pushed document: {} -> {}",
        local_doc.relative_path,
        result.id
    );
    Ok(result.id)
}

/// Update an existing document (create new version for published docs)
pub async fn update_document(
    api_url: &str,
    access_token: &str,
    doc_id: &str,
    local_doc: &LocalDocument,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/{}", api_url, doc_id);

    let body_text = std::fs::read_to_string(&local_doc.path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Derive title from filename for update too
    let title = Path::new(&local_doc.relative_path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace('-', " ").replace('_', " "))
        .unwrap_or_else(|| local_doc.relative_path.clone().into());

    let body = serde_json::json!({
        "title": title,
        "body": body_text,
        "source_path": local_doc.relative_path,
    });

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Update failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Update failed ({}): {}", status, text));
    }

    tracing::info!("Updated document: {} ({})", doc_id, local_doc.relative_path);
    Ok(())
}

/// Pull a document from the Wire API and write to local file
pub async fn pull_document(
    api_url: &str,
    access_token: &str,
    doc: &DocumentInfo,
    sync_root: &Path,
    corpus_slug: &str,
) -> Result<CachedDocument, String> {
    let effective = doc.effective_path();
    let source_path = effective.as_str();

    // Reject any source_path containing ".." segments
    if source_path.split('/').any(|seg| seg == "..")
        || source_path.split('\\').any(|seg| seg == "..")
    {
        return Err(format!(
            "Path traversal detected (.. segment): {}",
            source_path
        ));
    }

    // Client-side path validation: resolve path, confirm within sync root
    let target_path = sync_root.join(source_path);

    // Canonicalize the sync root
    let sync_root_canonical = sync_root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize sync root: {}", e))?;

    // Ensure parent directory exists before canonicalizing
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent directory: {}", e))?;
    }

    // Canonicalize the parent directory (which now exists), then join the filename
    let filename = target_path
        .file_name()
        .ok_or_else(|| format!("Invalid file path: {}", source_path))?;
    let canonical_parent = target_path
        .parent()
        .ok_or_else(|| format!("No parent directory for: {}", source_path))?
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize parent: {}", e))?;
    let resolved = canonical_parent.join(filename);

    if !resolved.starts_with(&sync_root_canonical) {
        return Err(format!("Path traversal detected: {}", source_path));
    }

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/{}/body", api_url, doc.id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("Pull failed for {}: {}", doc.id, e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Pull failed ({}): {}", status, text));
    }

    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read document body: {}", e))?;

    let file_size = body_text.len() as u64;
    if file_size == 0 {
        return Err(format!("Downloaded empty document: {}", doc.id));
    }

    // Verify hash
    let actual_hash = compute_sha256(&body_text);
    if actual_hash != doc.body_hash {
        return Err(format!(
            "Hash mismatch for {}: expected {}, got {}",
            doc.id,
            doc.body_hash.get(..12).unwrap_or(&doc.body_hash),
            actual_hash.get(..12).unwrap_or(&actual_hash)
        ));
    }

    // Ensure parent directory exists
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    fs::write(&resolved, &body_text)
        .await
        .map_err(|e| format!("Failed to write document: {}", e))?;

    tracing::info!("Pulled document: {} -> {}", doc.id, resolved.display());

    Ok(CachedDocument {
        document_id: doc.id.clone(),
        corpus_slug: corpus_slug.to_string(),
        source_path: source_path.to_string(),
        body_hash: actual_hash,
        file_size_bytes: file_size,
        cached_at: chrono::Utc::now().to_rfc3339(),
        sync_status: FileStatus::InSync,
        error_message: None,
        document_status: doc.status.clone(),
    })
}

/// Download a document body to the local cache for serving
pub async fn cache_document_for_serving(
    api_url: &str,
    access_token: &str,
    document_id: &str,
    corpus_id: &str,
    expected_hash: &str,
    cache_dir: &Path,
) -> Result<u64, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/{}/body", api_url, document_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("Document download failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Document download failed ({})", resp.status()));
    }

    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read document body: {}", e))?;

    // Verify hash
    let actual_hash = compute_sha256(&body_text);
    if actual_hash != expected_hash {
        return Err(format!(
            "Hash mismatch: expected {}, got {}",
            expected_hash.get(..12).unwrap_or(expected_hash),
            actual_hash.get(..12).unwrap_or(&actual_hash)
        ));
    }

    // Store in cache: {cache_dir}/{corpus_id}/{document_id}.body
    let corpus_dir = cache_dir.join(corpus_id);
    fs::create_dir_all(&corpus_dir)
        .await
        .map_err(|e| format!("Failed to create corpus cache dir: {}", e))?;

    let file_path = corpus_dir.join(format!("{}.body", document_id));
    let file_size = body_text.len() as u64;

    fs::write(&file_path, &body_text)
        .await
        .map_err(|e| format!("Failed to write cached document: {}", e))?;

    tracing::info!(
        "Cached document {}/{} ({} bytes)",
        corpus_id,
        document_id,
        file_size
    );
    Ok(file_size)
}

// --- Hash Verification ------------------------------------------------------

/// Compute SHA-256 hash of a UTF-8 string (matches TypeScript's TextEncoder round-trip)
pub fn compute_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Get total size of all files in cache directory
pub async fn get_cache_size(cache_dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut dirs_to_scan = vec![cache_dir.to_path_buf()];
    while let Some(dir) = dirs_to_scan.pop() {
        if let Ok(mut entries) = fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(meta) = entry.metadata().await {
                    if meta.is_dir() {
                        dirs_to_scan.push(entry.path());
                    } else {
                        total += meta.len();
                    }
                }
            }
        }
    }
    total
}

/// Check if a document is cached for serving
pub fn is_document_cached(cache_dir: &Path, corpus_id: &str, document_id: &str) -> bool {
    cache_dir
        .join(corpus_id)
        .join(format!("{}.body", document_id))
        .exists()
}

/// Get the local file path for a cached document body
pub fn get_cached_document_path(cache_dir: &Path, corpus_id: &str, document_id: &str) -> PathBuf {
    cache_dir
        .join(corpus_id)
        .join(format!("{}.body", document_id))
}

/// Compute SHA-256 hash of a specific byte range in a file (raw bytes).
/// Uses std::fs::read (not read_to_string) to avoid panics on multi-byte
/// UTF-8 boundaries. This matches PostgreSQL's byte-level behavior.
pub fn hash_byte_range(file_path: &Path, start: usize, end: usize) -> Result<String, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read file: {}", e))?;

    if end > bytes.len() {
        return Err(format!(
            "Byte range {}-{} exceeds file size {}",
            start,
            end,
            bytes.len()
        ));
    }

    let slice = &bytes[start..end];
    let mut hasher = Sha256::new();
    hasher.update(slice);
    Ok(hex::encode(hasher.finalize()))
}

/// Delete a cached document file
pub async fn delete_cached_document(
    cache_dir: &Path,
    corpus_id: &str,
    document_id: &str,
) -> Result<(), String> {
    let file_path = cache_dir
        .join(corpus_id)
        .join(format!("{}.body", document_id));
    if file_path.exists() {
        fs::remove_file(&file_path)
            .await
            .map_err(|e| format!("Failed to delete document: {}", e))?;
        tracing::info!("Deleted cached document {}/{}", corpus_id, document_id);
    }
    Ok(())
}

/// Find a cached document by ID across all corpus subdirectories.
/// Returns (corpus_id, file_path) if found.
pub async fn find_cached_document_by_id(
    cache_dir: &Path,
    document_id: &str,
) -> Option<(String, PathBuf)> {
    let target_filename = format!("{}.body", document_id);

    if let Ok(mut entries) = fs::read_dir(cache_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                let candidate = entry.path().join(&target_filename);
                if candidate.exists() {
                    let corpus_id = entry.file_name().to_string_lossy().to_string();
                    return Some((corpus_id, candidate));
                }
            }
        }
    }
    None
}

/// Delete a cached document by scanning all corpus subdirectories.
/// Used when corpus_id is not known.
pub async fn delete_cached_document_by_id(
    cache_dir: &Path,
    document_id: &str,
) -> Result<(), String> {
    match find_cached_document_by_id(cache_dir, document_id).await {
        Some((corpus_id, file_path)) => {
            fs::remove_file(&file_path)
                .await
                .map_err(|e| format!("Failed to delete document: {}", e))?;
            tracing::info!("Deleted cached document {}/{}", corpus_id, document_id);
            Ok(())
        }
        None => {
            tracing::debug!(
                "Document {} not found in cache, nothing to purge",
                document_id
            );
            Ok(())
        }
    }
}

// --- Version History --------------------------------------------------------

pub async fn fetch_version_history(
    api_url: &str,
    access_token: &str,
    document_id: &str,
) -> Result<VersionHistoryResponse, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/{}/versions", api_url, document_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch version history: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Version history request failed ({}): {}",
            status, text
        ));
    }

    resp.json::<VersionHistoryResponse>()
        .await
        .map_err(|e| format!("Failed to parse version history: {}", e))
}

pub async fn create_version(
    api_url: &str,
    access_token: &str,
    original_doc_id: &str,
    body: &str,
    source_path: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/wire/documents/version", api_url);

    // Derive a title from source_path (filename without extension)
    let title = std::path::Path::new(source_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(source_path)
        .to_string();

    let payload = serde_json::json!({
        "original_document_id": original_doc_id,
        "title": title,
        "body": body,
        "source_path": source_path,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Version creation failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Version creation failed ({}): {}", status, text));
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse version response: {}", e))?;

    result["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No id in version response".to_string())
}

// --- Word Diff --------------------------------------------------------------

pub fn compute_word_diff(old_text: &str, new_text: &str) -> Vec<DiffHunk> {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_secs(5))
        .diff_words(old_text, new_text);

    let mut hunks = Vec::new();
    let mut old_offset = 0usize;
    let mut new_offset = 0usize;

    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            ChangeTag::Equal => "equal",
            ChangeTag::Insert => "insert",
            ChangeTag::Delete => "delete",
        };

        let content = change.value().to_string();
        let len = content.len();

        hunks.push(DiffHunk {
            tag: tag.to_string(),
            content,
            old_offset: Some(old_offset),
            new_offset: Some(new_offset),
        });

        match change.tag() {
            ChangeTag::Equal => {
                old_offset += len;
                new_offset += len;
            }
            ChangeTag::Delete => {
                old_offset += len;
            }
            ChangeTag::Insert => {
                new_offset += len;
            }
        }
    }

    // Merge consecutive equal hunks to reduce payload size
    let mut merged: Vec<DiffHunk> = Vec::new();
    for hunk in hunks {
        if let Some(last) = merged.last_mut() {
            if last.tag == "equal" && hunk.tag == "equal" {
                last.content.push_str(&hunk.content);
                continue;
            }
        }
        merged.push(hunk);
    }

    merged
}
