// pyramid/dadbear_extend.rs — WS-DADBEAR-EXTEND (Phase 2b)
//
// Extends DADBEAR from maintenance-only to also handling CREATION of pyramids.
// Implements:
//   1. Source folder watcher (periodic scan via scan_source_directory + detect_changes)
//   2. Ingest dispatcher (pending records → processing → build chain → complete/failed)
//   3. Session boundary detection (file mtime stale > session_timeout → promote)
//   4. Core tick loop orchestrating all of the above
//
// DADBEAR is a scheduler, not an orchestrator. It fires chains in response to
// filesystem events and session boundaries. The chain executor handles what
// gets fired and how it runs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use tracing::{debug, error, info, warn};

use super::db;
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use super::ingest;
use super::lock_manager::LockManager;
use super::types::{
    ContentType, DadbearWatchConfig, DadbearWatchStatus, IngestRecord, SourceFile,
};

/// Handle to the running DADBEAR extend tick loop. Drop to stop.
pub struct DadbearExtendHandle {
    cancel: tokio_util::sync::CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl DadbearExtendHandle {
    /// Stop the tick loop.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for DadbearExtendHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

/// Per-config runtime tracking for scan timing.
struct ConfigTicker {
    last_tick: std::time::Instant,
    interval: std::time::Duration,
}

// ── Core tick loop ─────────────────────────────────────────────────────────────

/// Start the DADBEAR extend tick loop. Spawns a background task that:
///   - Every second, checks which configs are due for a scan tick
///   - For each due config: scan → detect → upsert ingest records → check session timeouts → dispatch
///
/// Returns a handle that can be used to stop the loop.
pub fn start_dadbear_extend_loop(
    db_path: String,
    event_bus: Arc<BuildEventBus>,
) -> DadbearExtendHandle {
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        info!("DADBEAR-EXTEND tick loop started");

        // Track per-config tick intervals
        let mut tickers: HashMap<i64, ConfigTicker> = HashMap::new();

        loop {
            // Check cancellation
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    info!("DADBEAR-EXTEND tick loop cancelled");
                    break;
                }
                // Base tick: check every 1 second whether any config is due
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }

            // Load all enabled configs
            let configs = match load_enabled_configs(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("DADBEAR-EXTEND: failed to load configs: {}", e);
                    continue;
                }
            };

            // Remove tickers for configs that no longer exist
            tickers.retain(|id, _| configs.iter().any(|c| c.id == *id));

            let now = std::time::Instant::now();

            for config in &configs {
                // Initialize or check ticker
                let ticker = tickers.entry(config.id).or_insert_with(|| ConfigTicker {
                    last_tick: now - std::time::Duration::from_secs(config.scan_interval_secs + 1), // fire immediately on first load
                    interval: std::time::Duration::from_secs(config.scan_interval_secs),
                });

                // Update interval if config changed
                ticker.interval = std::time::Duration::from_secs(config.scan_interval_secs);

                if now.duration_since(ticker.last_tick) < ticker.interval {
                    continue; // not due yet
                }
                ticker.last_tick = now;

                // Run the tick for this config
                if let Err(e) = run_tick_for_config(&db_path, config, &event_bus).await {
                    error!(
                        slug = %config.slug,
                        source_path = %config.source_path,
                        error = %e,
                        "DADBEAR-EXTEND tick failed"
                    );
                }
            }
        }

        info!("DADBEAR-EXTEND tick loop exited");
    });

    DadbearExtendHandle {
        cancel,
        handle: Some(handle),
    }
}

/// Load enabled configs from DB (blocking read, short-lived connection).
fn load_enabled_configs(db_path: &str) -> Result<Vec<DadbearWatchConfig>> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;
    db::get_enabled_dadbear_configs(&conn)
}

// ── Per-config tick ────────────────────────────────────────────────────────────

/// Execute one DADBEAR tick for a single watch config. The tick performs:
///   1. scan source directory
///   2. detect changes
///   3. upsert ingest records for new/modified files
///   4. check for session timeout → fire promotion
///   5. dispatch pending ingest records
pub async fn run_tick_for_config(
    db_path: &str,
    config: &DadbearWatchConfig,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    let slug = &config.slug;
    let source_path = &config.source_path;

    let content_type = ContentType::from_str(&config.content_type)
        .ok_or_else(|| anyhow::anyhow!("Unknown content type: {}", config.content_type))?;

    // ── 1. Scan source directory ─────────────────────────────────────────
    let current_files = match ingest::scan_source_directory(source_path, &content_type) {
        Ok(files) => files,
        Err(e) => {
            debug!(
                slug = %slug,
                source_path = %source_path,
                error = %e,
                "DADBEAR scan skipped (directory may not exist yet)"
            );
            return Ok(());
        }
    };

    // ── 2. Detect changes ────────────────────────────────────────────────
    let ingest_config = ingest::default_ingest_config();
    let sig = ingest::ingest_signature(&content_type, &ingest_config);

    let change_set = {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        ingest::detect_changes(&conn, slug, &sig, &current_files)?
    };

    // ── 3. Upsert ingest records for new/modified ────────────────────────
    let has_changes = !change_set.new_files.is_empty() || !change_set.modified_files.is_empty();

    if has_changes || !change_set.deleted_paths.is_empty() {
        let _lock = LockManager::global().write(slug).await;
        let conn = db::open_pyramid_connection(Path::new(db_path))?;

        let all_pending: Vec<&SourceFile> = change_set
            .new_files
            .iter()
            .chain(change_set.modified_files.iter())
            .collect();

        for sf in &all_pending {
            let record = IngestRecord {
                id: 0,
                slug: slug.clone(),
                source_path: sf.path.clone(),
                content_type: config.content_type.clone(),
                ingest_signature: sig.clone(),
                file_hash: Some(sf.file_hash.clone()),
                file_mtime: Some(sf.mtime.clone()),
                status: "pending".to_string(),
                build_id: None,
                error_message: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            if let Err(e) = db::save_ingest_record(&conn, &record) {
                warn!("DADBEAR: failed to save ingest record for {}: {}", sf.path, e);
            }
        }

        // Mark deleted paths as stale
        for path in &change_set.deleted_paths {
            if let Err(e) = db::mark_ingest_stale(&conn, slug, path) {
                warn!("DADBEAR: failed to mark stale for {}: {}", path, e);
            }
        }

        // Update last_scan_at
        let _ = db::touch_dadbear_last_scan(&conn, config.id);

        // Emit scan complete event
        let _ = event_bus.tx.send(TaggedBuildEvent {
            slug: slug.clone(),
            kind: TaggedKind::IngestScanComplete {
                new_count: change_set.new_files.len(),
                modified_count: change_set.modified_files.len(),
                deleted_count: change_set.deleted_paths.len(),
            },
        });

        debug!(
            slug = %slug,
            new = change_set.new_files.len(),
            modified = change_set.modified_files.len(),
            deleted = change_set.deleted_paths.len(),
            "DADBEAR scan detected changes"
        );
    }

    // ── 4. Check for session timeout → fire promotion ────────────────────
    if content_type == ContentType::Conversation {
        check_session_timeouts(db_path, config, event_bus).await?;
    }

    // ── 5. Dispatch pending ingest records ───────────────────────────────
    dispatch_pending_ingests(db_path, config, event_bus).await?;

    Ok(())
}

// ── Session timeout detection ──────────────────────────────────────────────────

/// Check active provisional sessions for this config's slug. If any session's
/// source file has `now - mtime > session_timeout_secs`, fire promotion.
///
/// Per Q10: session boundary = file mtime. DADBEAR 10s tick + now - mtime > 30min
/// fires promotion.
async fn check_session_timeouts(
    db_path: &str,
    config: &DadbearWatchConfig,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    let slug = &config.slug;
    let timeout_secs = config.session_timeout_secs;

    // Get active sessions for this slug
    let sessions = {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        db::get_active_provisional_sessions(&conn, slug)?
    };

    if sessions.is_empty() {
        return Ok(());
    }

    let now = Utc::now();

    for session in &sessions {
        // Get current mtime of the source file
        let file_mtime = match std::fs::metadata(&session.source_path) {
            Ok(meta) => {
                let mtime: chrono::DateTime<Utc> = meta
                    .modified()
                    .ok()
                    .map(|t| t.into())
                    .unwrap_or(now);
                mtime
            }
            Err(_) => continue, // file gone — skip
        };

        let elapsed_secs = (now - file_mtime).num_seconds();

        if elapsed_secs < timeout_secs as i64 {
            // File still active — update session mtime for tracking
            let mtime_str = file_mtime.to_rfc3339();
            let conn = db::open_pyramid_connection(Path::new(db_path))?;
            let _ = db::update_session_mtime(&conn, &session.session_id, &mtime_str);
            continue;
        }

        // Session timed out — fire canonical build + promote
        info!(
            slug = %slug,
            session_id = %session.session_id,
            elapsed_secs = elapsed_secs,
            timeout = timeout_secs,
            "DADBEAR: session timeout detected, firing promotion"
        );

        // Generate a build_id for the canonical build
        let build_id = format!("canonical-{}-{}", slug, uuid::Uuid::new_v4());

        // Promote the session under write lock
        {
            let _lock = LockManager::global().write(slug).await;
            let conn = db::open_pyramid_connection(Path::new(db_path))?;
            match db::promote_session(&conn, &session.session_id, &build_id, Some(event_bus)) {
                Ok(count) => {
                    info!(
                        slug = %slug,
                        session_id = %session.session_id,
                        build_id = %build_id,
                        promoted_count = count,
                        "DADBEAR: session promoted to canonical"
                    );
                }
                Err(e) => {
                    error!(
                        slug = %slug,
                        session_id = %session.session_id,
                        error = %e,
                        "DADBEAR: session promotion failed"
                    );
                }
            }
        }
    }

    Ok(())
}

// ── Ingest dispatch ────────────────────────────────────────────────────────────

/// Pick up pending ingest records and dispatch them. Marks each as 'processing',
/// emits IngestStarted, and on completion marks 'complete' with build_id (or
/// 'failed' with error). Respects config.batch_size.
async fn dispatch_pending_ingests(
    db_path: &str,
    config: &DadbearWatchConfig,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    let slug = &config.slug;
    let batch_size = config.batch_size as usize;

    // Get pending records
    let pending = {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        db::get_pending_ingests(&conn, slug)?
    };

    if pending.is_empty() {
        return Ok(());
    }

    // Process up to batch_size records
    let to_process = &pending[..pending.len().min(batch_size)];

    for record in to_process {
        // Mark as processing
        {
            let _lock = LockManager::global().write(slug).await;
            let conn = db::open_pyramid_connection(Path::new(db_path))?;
            db::mark_ingest_processing(&conn, record.id)?;
        }

        // Emit IngestStarted
        let _ = event_bus.tx.send(TaggedBuildEvent {
            slug: slug.clone(),
            kind: TaggedKind::IngestStarted {
                source_path: record.source_path.clone(),
            },
        });

        info!(
            slug = %slug,
            source_path = %record.source_path,
            record_id = record.id,
            "DADBEAR: dispatching ingest"
        );

        // The actual build chain firing is done via invoke_chain or run_build.
        // For now, mark as complete with a placeholder build_id. The real
        // chain dispatch will be wired by WS-EM-CHAIN / WS-VINE-UNIFY when
        // the chain YAML lands.
        //
        // FUTURE: Replace this stub with actual chain dispatch:
        //   let build_id = fire_ingest_chain(state, slug, &record).await?;
        let build_id = format!("dadbear-ingest-{}-{}", slug, uuid::Uuid::new_v4());

        // Mark complete
        {
            let _lock = LockManager::global().write(slug).await;
            let conn = db::open_pyramid_connection(Path::new(db_path))?;
            if let Err(e) = db::mark_ingest_complete(&conn, record.id, &build_id) {
                error!(
                    slug = %slug,
                    record_id = record.id,
                    error = %e,
                    "DADBEAR: failed to mark ingest complete"
                );
                // Try to mark as failed instead
                let _ = db::mark_ingest_failed(&conn, record.id, &e.to_string());

                let _ = event_bus.tx.send(TaggedBuildEvent {
                    slug: slug.clone(),
                    kind: TaggedKind::IngestFailed {
                        source_path: record.source_path.clone(),
                        error: e.to_string(),
                    },
                });
                continue;
            }
        }

        // Emit IngestComplete
        let _ = event_bus.tx.send(TaggedBuildEvent {
            slug: slug.clone(),
            kind: TaggedKind::IngestComplete {
                source_path: record.source_path.clone(),
                build_id: build_id.clone(),
            },
        });

        info!(
            slug = %slug,
            source_path = %record.source_path,
            build_id = %build_id,
            "DADBEAR: ingest complete"
        );
    }

    Ok(())
}

// ── Manual trigger ─────────────────────────────────────────────────────────────

/// Manually trigger a single scan+dispatch cycle for all configs of a given slug.
/// Used by the POST /pyramid/:slug/dadbear/trigger HTTP route.
pub async fn trigger_for_slug(
    db_path: &str,
    slug: &str,
    event_bus: &Arc<BuildEventBus>,
) -> Result<serde_json::Value> {
    let configs = {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        db::get_dadbear_configs(&conn, slug)?
    };

    if configs.is_empty() {
        return Ok(serde_json::json!({
            "slug": slug,
            "error": "No DADBEAR watch configs found for this slug",
            "configs_processed": 0,
        }));
    }

    let mut processed = 0usize;
    let mut errors = Vec::new();

    for config in &configs {
        match run_tick_for_config(db_path, config, event_bus).await {
            Ok(()) => processed += 1,
            Err(e) => errors.push(format!("{}: {}", config.source_path, e)),
        }
    }

    Ok(serde_json::json!({
        "slug": slug,
        "configs_processed": processed,
        "errors": errors,
    }))
}

/// Get status information for all DADBEAR configs of a slug.
pub fn get_status_for_slug(db_path: &str, slug: &str) -> Result<Vec<DadbearWatchStatus>> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;
    let configs = db::get_dadbear_configs(&conn, slug)?;

    let mut statuses = Vec::new();
    for config in configs {
        let pending_ingests = db::get_pending_ingests(&conn, slug)?.len();
        let active_sessions = db::get_active_provisional_sessions(&conn, slug)?.len();

        // Read last_scan_at from the config row directly
        let last_scan_at: Option<String> = conn
            .query_row(
                "SELECT last_scan_at FROM pyramid_dadbear_config WHERE id = ?1",
                rusqlite::params![config.id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        statuses.push(DadbearWatchStatus {
            config,
            pending_ingests,
            active_sessions,
            last_scan_at,
        });
    }

    Ok(statuses)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;
    use crate::pyramid::types::{ContentType, DadbearWatchConfig, IngestRecord};
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a test DB with full schema.
    fn test_db() -> (rusqlite::Connection, String) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path.to_str().unwrap().to_string();
        let conn = db::open_pyramid_db(&db_path).unwrap();
        // Keep dir alive by leaking it (test only)
        std::mem::forget(dir);
        (conn, db_path_str)
    }

    fn make_config(slug: &str, source_path: &str) -> DadbearWatchConfig {
        DadbearWatchConfig {
            id: 0,
            slug: slug.to_string(),
            source_path: source_path.to_string(),
            content_type: "conversation".to_string(),
            scan_interval_secs: 10,
            debounce_secs: 30,
            session_timeout_secs: 1800,
            batch_size: 1,
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    // ── Test 1: DADBEAR config CRUD ────────────────────────────────────────

    #[test]
    fn test_dadbear_config_crud() {
        let (conn, _db_path) = test_db();

        // Create slug first (FK reference)
        db::create_slug(&conn, "test-slug", &ContentType::Conversation, "/tmp/src").unwrap();

        // Save config
        let config = make_config("test-slug", "/tmp/src");
        let id = db::save_dadbear_config(&conn, &config).unwrap();
        assert!(id > 0);

        // Read back
        let configs = db::get_dadbear_configs(&conn, "test-slug").unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].slug, "test-slug");
        assert_eq!(configs[0].source_path, "/tmp/src");
        assert_eq!(configs[0].scan_interval_secs, 10);
        assert_eq!(configs[0].session_timeout_secs, 1800);
        assert!(configs[0].enabled);

        // Update: change scan_interval
        let updated = DadbearWatchConfig {
            scan_interval_secs: 30,
            enabled: false,
            ..config.clone()
        };
        db::save_dadbear_config(&conn, &updated).unwrap();

        let configs2 = db::get_dadbear_configs(&conn, "test-slug").unwrap();
        assert_eq!(configs2.len(), 1);
        assert_eq!(configs2[0].scan_interval_secs, 30);
        assert!(!configs2[0].enabled);

        // Enable/disable
        db::enable_dadbear_for_slug(&conn, "test-slug").unwrap();
        let configs3 = db::get_dadbear_configs(&conn, "test-slug").unwrap();
        assert!(configs3[0].enabled);

        db::disable_dadbear_for_slug(&conn, "test-slug").unwrap();
        let configs4 = db::get_dadbear_configs(&conn, "test-slug").unwrap();
        assert!(!configs4[0].enabled);

        // Delete
        let deleted = db::delete_dadbear_config(&conn, "test-slug", "/tmp/src").unwrap();
        assert!(deleted);
        let configs5 = db::get_dadbear_configs(&conn, "test-slug").unwrap();
        assert!(configs5.is_empty());
    }

    // ── Test 2: scan+detect cycle creates pending ingest records ───────────

    #[test]
    fn test_scan_detect_creates_pending_records() {
        let (conn, _db_path) = test_db();

        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_str().unwrap().to_string();

        // Create a .jsonl file in the temp directory
        let file_path = dir.path().join("session1.jsonl");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-04-01T10:00:00"}}"#).unwrap();
        }

        // Create slug
        db::create_slug(&conn, "test-scan", &ContentType::Conversation, &dir_path).unwrap();

        // Scan
        let content_type = ContentType::Conversation;
        let current_files = ingest::scan_source_directory(&dir_path, &content_type).unwrap();
        assert_eq!(current_files.len(), 1);

        // Detect changes
        let ingest_config = ingest::default_ingest_config();
        let sig = ingest::ingest_signature(&content_type, &ingest_config);
        let change_set = ingest::detect_changes(&conn, "test-scan", &sig, &current_files).unwrap();
        assert_eq!(change_set.new_files.len(), 1);
        assert!(change_set.modified_files.is_empty());

        // Upsert as pending
        for sf in &change_set.new_files {
            let record = IngestRecord {
                id: 0,
                slug: "test-scan".to_string(),
                source_path: sf.path.clone(),
                content_type: "conversation".to_string(),
                ingest_signature: sig.clone(),
                file_hash: Some(sf.file_hash.clone()),
                file_mtime: Some(sf.mtime.clone()),
                status: "pending".to_string(),
                build_id: None,
                error_message: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            db::save_ingest_record(&conn, &record).unwrap();
        }

        // Verify pending records exist
        let pending = db::get_pending_ingests(&conn, "test-scan").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, "pending");
    }

    // ── Test 3: ingest dispatch marks records processing→complete ──────────

    #[test]
    fn test_ingest_dispatch_lifecycle() {
        let (conn, _db_path) = test_db();

        db::create_slug(&conn, "test-dispatch", &ContentType::Conversation, "/tmp").unwrap();

        // Create a pending record
        let record = IngestRecord {
            id: 0,
            slug: "test-dispatch".to_string(),
            source_path: "/tmp/session.jsonl".to_string(),
            content_type: "conversation".to_string(),
            ingest_signature: "sig-abc".to_string(),
            file_hash: Some("hash123".to_string()),
            file_mtime: Some("2026-04-01T10:00:00Z".to_string()),
            status: "pending".to_string(),
            build_id: None,
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_ingest_record(&conn, &record).unwrap();

        // Get pending
        let pending = db::get_pending_ingests(&conn, "test-dispatch").unwrap();
        assert_eq!(pending.len(), 1);
        let id = pending[0].id;

        // Mark processing
        db::mark_ingest_processing(&conn, id).unwrap();
        let pending2 = db::get_pending_ingests(&conn, "test-dispatch").unwrap();
        assert!(pending2.is_empty()); // no longer pending

        // Verify it's processing
        let all = db::get_ingest_records_for_slug(&conn, "test-dispatch").unwrap();
        assert_eq!(all[0].status, "processing");

        // Mark complete
        db::mark_ingest_complete(&conn, id, "build-123").unwrap();
        let all2 = db::get_ingest_records_for_slug(&conn, "test-dispatch").unwrap();
        assert_eq!(all2[0].status, "complete");
        assert_eq!(all2[0].build_id.as_deref(), Some("build-123"));
    }

    // ── Test 4: session timeout detection fires promotion ──────────────────

    #[test]
    fn test_session_timeout_promotion() {
        let (conn, _db_path) = test_db();

        db::create_slug(&conn, "test-timeout", &ContentType::Conversation, "/tmp").unwrap();

        // Create a provisional session
        let session_id = "session-timeout-test-001";
        db::create_provisional_session(&conn, "test-timeout", "/tmp/old.jsonl", session_id)
            .unwrap();

        // Set a very old mtime on the session (simulating stale file)
        let old_mtime = "2026-01-01T00:00:00Z";
        db::update_session_mtime(&conn, session_id, old_mtime).unwrap();

        // Verify session is active
        let active = db::get_active_provisional_sessions(&conn, "test-timeout").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].status, "active");

        // Promote it (simulating what check_session_timeouts would do)
        let build_id = "canonical-test-timeout-001";
        let _count = db::promote_session(&conn, session_id, build_id, None).unwrap();

        // The session should now be promoted
        let session = db::get_provisional_session(&conn, session_id).unwrap().unwrap();
        assert_eq!(session.status, "promoted");
        assert_eq!(session.canonical_build_id.as_deref(), Some(build_id));

        // No more active sessions
        let active2 = db::get_active_provisional_sessions(&conn, "test-timeout").unwrap();
        assert!(active2.is_empty());
    }

    // ── Test 5: update_session_mtime and update_session_chunk_progress ─────

    #[test]
    fn test_session_helper_updates() {
        let (conn, _db_path) = test_db();

        db::create_slug(&conn, "test-helpers", &ContentType::Conversation, "/tmp").unwrap();

        let session_id = "session-helper-test-001";
        db::create_provisional_session(&conn, "test-helpers", "/tmp/chat.jsonl", session_id)
            .unwrap();

        // update_session_mtime
        db::update_session_mtime(&conn, session_id, "2026-04-08T12:00:00Z").unwrap();
        let session = db::get_provisional_session(&conn, session_id).unwrap().unwrap();
        assert_eq!(session.file_mtime.as_deref(), Some("2026-04-08T12:00:00Z"));

        // update_session_chunk_progress
        db::update_session_chunk_progress(&conn, session_id, 5).unwrap();
        let session2 = db::get_provisional_session(&conn, session_id).unwrap().unwrap();
        assert_eq!(session2.last_chunk_processed, 5);

        // Update again
        db::update_session_chunk_progress(&conn, session_id, 10).unwrap();
        let session3 = db::get_provisional_session(&conn, session_id).unwrap().unwrap();
        assert_eq!(session3.last_chunk_processed, 10);
    }
}
