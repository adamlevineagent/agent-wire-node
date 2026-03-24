// pyramid/watcher.rs — File watcher that detects source changes and writes mutations to the WAL
//
// Watches source directories for file create/modify/remove events, computes SHA-256 hashes,
// compares against pyramid_file_hashes, and writes pending mutations to pyramid_pending_mutations.

use anyhow::{Context, Result};
use chrono::Utc;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::db as pyramid_db;
use super::types::AutoUpdateConfig;

// ── Recent Remove tracker for rename-candidate detection ─────────────────────

/// A recently observed Remove event, used to pair with a subsequent Create.
#[derive(Debug, Clone)]
struct RecentRemove {
    path: String,
    timestamp: chrono::DateTime<Utc>,
}

// ── PyramidFileWatcher ───────────────────────────────────────────────────────

pub struct PyramidFileWatcher {
    slug: String,
    watcher: Option<RecommendedWatcher>,
    source_paths: Vec<String>,
    paused_flag: Arc<Mutex<bool>>,
    /// In-memory cache of all file paths tracked in pyramid_file_hashes for this slug.
    tracked_paths: Arc<Mutex<HashSet<String>>>,
    /// Extensions that were ingested during the build (e.g. [".rs", ".ts"]).
    ingested_extensions: Arc<Mutex<Vec<String>>>,
    /// Config filenames that were ingested (e.g. ["Cargo.toml", "package.json"]).
    ingested_config_files: Arc<Mutex<Vec<String>>>,
    /// Channel to notify stale engines when mutations are written to the WAL.
    /// Sends (slug, layer) so the receiver can call engine.notify_mutation(layer).
    mutation_sender: Option<mpsc::UnboundedSender<(String, i32)>>,
}

impl PyramidFileWatcher {
    /// Create a new file watcher. Does not start watching yet.
    pub fn new(slug: &str, source_paths: Vec<String>) -> Self {
        Self {
            slug: slug.to_string(),
            watcher: None,
            source_paths,
            paused_flag: Arc::new(Mutex::new(false)),
            tracked_paths: Arc::new(Mutex::new(HashSet::new())),
            ingested_extensions: Arc::new(Mutex::new(Vec::new())),
            ingested_config_files: Arc::new(Mutex::new(Vec::new())),
            mutation_sender: None,
        }
    }

    /// Set the mutation sender channel. After write_mutation(), sends (slug, layer)
    /// so the stale engine can be notified of new mutations without polling.
    pub fn set_mutation_sender(&mut self, sender: mpsc::UnboundedSender<(String, i32)>) {
        self.mutation_sender = Some(sender);
    }

    /// Populate caches from the database. Called on start() and resume().
    fn populate_caches(&self, db_path: &str) -> Result<()> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open DB for cache population: {}", db_path))?;

        // Load tracked paths
        let paths = pyramid_db::get_tracked_paths(&conn, &self.slug)?;
        if let Ok(mut cache) = self.tracked_paths.lock() {
            *cache = paths;
        }

        // Load ingested extensions
        let extensions = pyramid_db::get_ingested_extensions(&conn, &self.slug)?;
        if let Ok(mut cache) = self.ingested_extensions.lock() {
            *cache = extensions;
        }

        // Load ingested config files from pyramid_file_hashes: filenames without extensions
        // that match known config file patterns
        let config_fnames = pyramid_db::get_ingested_config_files(&conn, &self.slug)?;
        if let Ok(mut cache) = self.ingested_config_files.lock() {
            *cache = config_fnames;
        }

        tracing::info!(
            "Watcher caches populated for slug='{}': {} tracked paths, {} extensions, {} config files",
            self.slug,
            self.tracked_paths.lock().map(|c| c.len()).unwrap_or(0),
            self.ingested_extensions.lock().map(|c| c.len()).unwrap_or(0),
            self.ingested_config_files.lock().map(|c| c.len()).unwrap_or(0),
        );

        Ok(())
    }

    /// Start watching all source paths for file changes.
    ///
    /// Populates in-memory caches from DB on start. Event handlers use caches for reads;
    /// a single DB connection is opened per event batch for writes only.
    pub fn start(&mut self, db_path: &str) -> Result<()> {
        // Populate caches from DB before starting
        self.populate_caches(db_path)?;

        let slug = self.slug.clone();
        let db_path = db_path.to_string();
        let paused_clone = Arc::clone(&self.paused_flag);
        let tracked_paths_clone = Arc::clone(&self.tracked_paths);
        let ingested_extensions_clone = Arc::clone(&self.ingested_extensions);
        let ingested_config_files_clone = Arc::clone(&self.ingested_config_files);
        let mutation_sender_clone = self.mutation_sender.clone();
        // Shared recent-removes tracker for rename-candidate detection
        let recent_removes: Arc<Mutex<Vec<RecentRemove>>> =
            Arc::new(Mutex::new(Vec::new()));
        let recent_removes_clone = Arc::clone(&recent_removes);

        // Store the paused flag reference so pause/resume can update it
        // We keep a second Arc for the struct to toggle later.
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            // Check paused flag
            if let Ok(guard) = paused_clone.lock() {
                if *guard {
                    return; // discard events while paused
                }
            }

            let event = match res {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!("File watcher error: {}", e);
                    return;
                }
            };

            // Process each path in the event
            if let Err(e) = handle_event_with_rename_tracking(
                &event,
                &slug,
                &db_path,
                &recent_removes_clone,
                &tracked_paths_clone,
                &ingested_extensions_clone,
                &ingested_config_files_clone,
                &mutation_sender_clone,
            ) {
                tracing::warn!("Error handling file event: {}", e);
            }
        })
        .context("Failed to create file watcher")?;

        // Store the watcher before adding paths (need the &mut self.watcher to exist)
        self.watcher = Some(watcher);

        // Watch each source path
        for path in &self.source_paths {
            if let Some(ref mut w) = self.watcher {
                w.watch(Path::new(path), RecursiveMode::Recursive)
                    .with_context(|| format!("Failed to watch path: {}", path))?;
            }
        }

        tracing::info!(
            "File watcher started for slug='{}' on {} paths",
            self.slug,
            self.source_paths.len()
        );
        Ok(())
    }

    /// Stop watching (drops the watcher).
    pub fn stop(&mut self) {
        self.watcher = None;
        tracing::info!("File watcher stopped for slug='{}'", self.slug);
    }

    /// Pause the watcher — events are received but discarded.
    pub fn pause(&mut self) {
        *self.paused_flag.lock().unwrap() = true;
    }

    /// Resume the watcher — events are processed again.
    /// Repopulates caches from DB to pick up any changes made while paused.
    pub fn resume(&mut self, db_path: &str) {
        if let Err(e) = self.populate_caches(db_path) {
            tracing::warn!("Failed to repopulate watcher caches on resume for slug='{}': {}", self.slug, e);
        }
        *self.paused_flag.lock().unwrap() = false;
    }
}

// ── Event handling with rename-candidate tracking ────────────────────────────

/// Check if a path is tracked (exists in cached tracked_paths) or is a plausible
/// new source file matching ingested extensions/config files.
/// Uses ONLY in-memory caches — ZERO database connections per event.
fn is_trackable_path(
    path: &str,
    tracked_paths: &Arc<Mutex<HashSet<String>>>,
    ingested_extensions: &Arc<Mutex<Vec<String>>>,
    ingested_config_files: &Arc<Mutex<Vec<String>>>,
) -> bool {
    // Fast reject: skip obvious non-source paths
    let skip_patterns = [
        "/target/", "/node_modules/", "/.git/", "/dist/", "/.next/",
        "/.DS_Store", ".tmp.", ".swp", ".swo", "~", "/build/",
    ];
    for pat in &skip_patterns {
        if path.contains(pat) {
            return false;
        }
    }

    // Check if this file is already tracked in the cache
    if let Ok(cache) = tracked_paths.lock() {
        if cache.contains(path) {
            return true;
        }
    }

    // For untracked files: check if the extension matches ingested extensions
    let file_ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    if let Ok(exts) = ingested_extensions.lock() {
        if exts.iter().any(|ext| ext == &file_ext) {
            return true;
        }
    }

    // Check if filename matches ingested config files (e.g. "Cargo.toml")
    let filename = Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    if let Ok(configs) = ingested_config_files.lock() {
        if configs.iter().any(|cf| cf == &filename) {
            return true;
        }
    }

    false
}

fn handle_event_with_rename_tracking(
    event: &Event,
    slug: &str,
    db_path: &str,
    recent_removes: &Arc<Mutex<Vec<RecentRemove>>>,
    tracked_paths: &Arc<Mutex<HashSet<String>>>,
    ingested_extensions: &Arc<Mutex<Vec<String>>>,
    ingested_config_files: &Arc<Mutex<Vec<String>>>,
    mutation_sender: &Option<mpsc::UnboundedSender<(String, i32)>>,
) -> Result<()> {
    let now = Utc::now();

    // Prune stale entries (older than 3 seconds)
    if let Ok(mut removes) = recent_removes.lock() {
        removes.retain(|r| (now - r.timestamp).num_seconds() < 3);
    }

    // Collect trackable paths first (uses caches only, no DB)
    let trackable_paths: Vec<String> = event
        .paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| is_trackable_path(p, tracked_paths, ingested_extensions, ingested_config_files))
        .collect();

    if trackable_paths.is_empty() {
        return Ok(());
    }

    // Open ONE connection for all write operations in this event
    let conn = open_conn(db_path)?;

    for path_str in &trackable_paths {
        match &event.kind {
            EventKind::Remove(_) => {
                // Track the remove for potential rename-candidate pairing
                if let Ok(mut removes) = recent_removes.lock() {
                    removes.push(RecentRemove {
                        path: path_str.clone(),
                        timestamp: now,
                    });
                }
                // Also handle as a normal remove event
                handle_remove_event_conn(&conn, slug, path_str, tracked_paths)?;
            }
            EventKind::Create(_) => {
                // Check if this Create pairs with a recent Remove (rename candidate)
                let rename_pair = find_rename_candidate(recent_removes, path_str, now);

                if let Some(old_path) = rename_pair {
                    // Write a rename_candidate mutation
                    let detail = serde_json::json!({
                        "old_path": old_path,
                        "new_path": path_str,
                    })
                    .to_string();

                    // Fan-out to all pyramids tracking this file
                    let slugs = get_watched_slugs_for_path(&conn, &old_path)?;
                    for s in &slugs {
                        write_mutation(&conn, s, 0, "rename_candidate", path_str, Some(&detail))?;
                        if check_runaway_for_slug(&conn, s)? {
                            tracing::warn!("Runaway threshold tripped for slug='{}'", s);
                        }
                    }
                    // Also write for the current slug if not already covered
                    if !slugs.contains(&slug.to_string()) {
                        write_mutation(&conn, slug, 0, "rename_candidate", path_str, Some(&detail))?;
                    }
                    // Update tracked_paths cache for rename
                    if let Ok(mut cache) = tracked_paths.lock() {
                        cache.remove(&old_path);
                        cache.insert(path_str.clone());
                    }
                } else {
                    // Normal create event
                    handle_create_event_conn(&conn, slug, path_str, tracked_paths)?;
                }
            }
            EventKind::Modify(_) => {
                handle_modify_event_conn(&conn, slug, path_str)?;
            }
            // Rename is not emitted on macOS (FSEvents reports Remove+Create),
            // but handle it if the platform does emit it.
            EventKind::Other => {
                // Some platforms emit rename as Other — ignore for now
            }
            _ => {
                // Access, Any, etc. — ignore
            }
        }
    }

    // Notify the stale engine that mutations were written for layer 0.
    // This bridges the gap between the file watcher and the stale engine
    // so mutations are processed immediately instead of waiting for the poll loop.
    if let Some(sender) = mutation_sender {
        let _ = sender.send((slug.to_string(), 0));
    }

    Ok(())
}

/// Check if a Create path matches a recent Remove (within 2 seconds, similar filename).
fn find_rename_candidate(
    recent_removes: &Arc<Mutex<Vec<RecentRemove>>>,
    new_path: &str,
    now: chrono::DateTime<Utc>,
) -> Option<String> {
    let mut removes = recent_removes.lock().ok()?;
    let new_filename = Path::new(new_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    // Find a remove within 2 seconds with a similar filename
    let idx = removes.iter().position(|r| {
        let elapsed = (now - r.timestamp).num_milliseconds();
        if elapsed > 2000 {
            return false;
        }
        let old_filename = Path::new(&r.path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        filenames_similar(old_filename, new_filename)
    });

    if let Some(i) = idx {
        let removed = removes.remove(i);
        Some(removed.path)
    } else {
        None
    }
}

/// Check if two filenames are similar enough to be a rename.
/// Same extension + at least 50% character overlap or same base name.
fn filenames_similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let ext_a = Path::new(a).extension().and_then(|e| e.to_str()).unwrap_or("");
    let ext_b = Path::new(b).extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext_a != ext_b {
        return false;
    }
    // Same extension — check character overlap
    let stem_a = Path::new(a).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let stem_b = Path::new(b).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem_a == stem_b {
        return true;
    }
    // Check overlap: count shared chars
    let shorter = stem_a.len().min(stem_b.len());
    if shorter == 0 {
        return false;
    }
    let shared = stem_a
        .chars()
        .zip(stem_b.chars())
        .filter(|(ca, cb)| ca == cb)
        .count();
    shared * 2 >= shorter // at least 50% overlap
}

// ── Individual event handlers (using shared connection) ──────────────────────

fn handle_modify_event_conn(conn: &Connection, slug: &str, path: &str) -> Result<()> {
    let hash = match compute_file_hash(path) {
        Ok(h) => h,
        Err(_) => return Ok(()), // file may have been deleted between event and read
    };

    // Fan-out: find all slugs tracking this file
    let slugs = get_watched_slugs_for_path(conn, path)?;
    let all_slugs = ensure_slug_included(&slugs, slug);

    for s in &all_slugs {
        // Compare hash against stored value
        let stored_hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                rusqlite::params![s, path],
                |row| row.get(0),
            )
            .ok();

        if let Some(ref existing) = stored_hash {
            if existing == &hash {
                continue; // unchanged, skip
            }
        }

        // Hash differs — write file_change mutation
        write_mutation(conn, s, 0, "file_change", path, Some(&hash))?;
        if check_runaway_for_slug(conn, s)? {
            tracing::warn!("Runaway threshold tripped for slug='{}'", s);
        }
    }

    Ok(())
}

fn handle_create_event_conn(
    conn: &Connection,
    slug: &str,
    path: &str,
    tracked_paths: &Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    // Fan-out to all slugs that may track this path
    let slugs = get_watched_slugs_for_path(conn, path)?;
    let all_slugs = ensure_slug_included(&slugs, slug);

    for s in &all_slugs {
        // Check if path already tracked
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                rusqlite::params![s, path],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        if !exists {
            write_mutation(conn, s, 0, "new_file", path, None)?;
            if check_runaway_for_slug(conn, s)? {
                tracing::warn!("Runaway threshold tripped for slug='{}'", s);
            }
        }
    }

    // Update tracked_paths cache with the new file
    if let Ok(mut cache) = tracked_paths.lock() {
        cache.insert(path.to_string());
    }

    Ok(())
}

fn handle_remove_event_conn(
    conn: &Connection,
    slug: &str,
    path: &str,
    tracked_paths: &Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    // Fan-out
    let slugs = get_watched_slugs_for_path(conn, path)?;
    let all_slugs = ensure_slug_included(&slugs, slug);

    for s in &all_slugs {
        // Check if path was tracked
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                rusqlite::params![s, path],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        if exists {
            write_mutation(conn, s, 0, "deleted_file", path, None)?;
            if check_runaway_for_slug(conn, s)? {
                tracing::warn!("Runaway threshold tripped for slug='{}'", s);
            }
        }
    }

    // Update tracked_paths cache — remove the deleted file
    if let Ok(mut cache) = tracked_paths.lock() {
        cache.remove(path);
    }

    Ok(())
}

// ── Core utility functions ───────────────────────────────────────────────────

/// Compute SHA-256 hash of a file and return the hex string.
pub fn compute_file_hash(path: &str) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read file for hashing: {}", path))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let result = hasher.finalize();
    Ok(hex::encode(result))
}

/// Check whether the runaway threshold has been exceeded for a slug.
///
/// Returns true if the ratio of distinct L0 file targets waiting in the WAL
/// to total tracked files exceeds the configured runaway_threshold.
pub fn check_runaway(conn: &Connection, slug: &str, config: &AutoUpdateConfig) -> bool {
    // Count total files tracked for this slug
    let total_files: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_file_hashes WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if total_files == 0 {
        return false; // pyramid just created, no baseline
    }

    // Count distinct pending L0 file targets excluding new_file and deleted_file.
    // This matches the operator-facing meaning of the threshold: "what share of
    // tracked files is currently pending?" Duplicate watcher rows should not
    // inflate the breaker ratio.
    let mutation_count: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT target_ref) FROM pyramid_pending_mutations
             WHERE slug = ?1 AND layer = 0 AND processed = 0
             AND mutation_type NOT IN ('new_file', 'deleted_file')",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let ratio = mutation_count as f64 / total_files as f64;
    // Treat the threshold as inclusive. At 100%, operators are explicitly
    // allowing a full-slug sweep to proceed.
    ratio > config.runaway_threshold
}

/// Write a pending mutation to the WAL. Returns the inserted row ID.
pub fn write_mutation(
    conn: &Connection,
    slug: &str,
    layer: i32,
    mutation_type: &str,
    target_ref: &str,
    detail: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO pyramid_pending_mutations
         (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 0)",
        rusqlite::params![slug, layer, mutation_type, target_ref, detail, now],
    )
    .with_context(|| {
        format!(
            "Failed to write mutation type='{}' for slug='{}'",
            mutation_type, slug
        )
    })?;
    Ok(conn.last_insert_rowid())
}

/// Multi-pyramid fan-out: find all slugs that track the given file path.
pub fn get_watched_slugs_for_path(conn: &Connection, path: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT slug FROM pyramid_file_hashes WHERE file_path = ?1")
        .context("Failed to prepare watched-slugs query")?;
    let slugs = stmt
        .query_map(rusqlite::params![path], |row| row.get::<_, String>(0))
        .context("Failed to query watched slugs")?
        .filter_map(|r| r.ok())
        .collect();
    Ok(slugs)
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Open a new database connection (short-lived, per the design spec).
fn open_conn(db_path: &str) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open DB at {}", db_path))?;
    // Ensure WAL mode for concurrent reads
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .ok();
    Ok(conn)
}

/// Load the AutoUpdateConfig for a slug, returning a default if not found.
fn load_auto_update_config(conn: &Connection, slug: &str) -> AutoUpdateConfig {
    conn.query_row(
        "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
         FROM pyramid_auto_update_config WHERE slug = ?1",
        rusqlite::params![slug],
        |row| {
            Ok(AutoUpdateConfig {
                slug: row.get(0)?,
                auto_update: row.get::<_, i32>(1)? != 0,
                debounce_minutes: row.get(2)?,
                min_changed_files: row.get(3)?,
                runaway_threshold: row.get(4)?,
                breaker_tripped: row.get::<_, i32>(5)? != 0,
                breaker_tripped_at: row.get(6)?,
                frozen: row.get::<_, i32>(7)? != 0,
                frozen_at: row.get(8)?,
            })
        },
    )
    .unwrap_or(AutoUpdateConfig {
        slug: slug.to_string(),
        auto_update: false,
        debounce_minutes: 5,
        min_changed_files: 1,
        runaway_threshold: 0.5,
        breaker_tripped: false,
        breaker_tripped_at: None,
        frozen: false,
        frozen_at: None,
    })
}

/// Check runaway for a slug by loading its config from the database.
fn check_runaway_for_slug(conn: &Connection, slug: &str) -> Result<bool> {
    let config = load_auto_update_config(conn, slug);
    Ok(check_runaway(conn, slug, &config))
}

/// Ensure the given slug is in the list; if not, add it.
fn ensure_slug_included(slugs: &[String], slug: &str) -> Vec<String> {
    let mut all = slugs.to_vec();
    if !all.iter().any(|s| s == slug) {
        all.push(slug.to_string());
    }
    all
}
