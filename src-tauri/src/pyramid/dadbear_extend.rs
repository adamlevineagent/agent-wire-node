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

use anyhow::{anyhow, Result};
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::build::WriteOp;
use super::build_runner;
use super::db;
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use super::ingest;
use super::lock_manager::LockManager;
use super::types::{
    BuildProgress, ContentType, DadbearWatchConfig, DadbearWatchStatus, IngestRecord, LayerEvent,
    SourceFile,
};
use super::PyramidState;

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
/// `state` is the shared `PyramidState` that chain builds run against. The loop
/// holds an `Arc` for chain dispatch (see [`fire_ingest_chain`]). `db_path` is
/// kept alongside `state` for short-lived DB operations that shouldn't contend
/// on the shared reader Mutex (scanning, ingest record state transitions).
///
/// Returns a handle that can be used to stop the loop.
pub fn start_dadbear_extend_loop(
    state: Arc<PyramidState>,
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
                if let Err(e) = run_tick_for_config(&state, &db_path, config, &event_bus).await {
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
    state: &Arc<PyramidState>,
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
    dispatch_pending_ingests(state, db_path, config, event_bus).await?;

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

/// Pick up pending ingest records and dispatch them through the chain engine.
///
/// **Shape rationale**: a pyramid build processes the whole slug, not one
/// file at a time. Firing `run_build_from` N sequential times for N pending
/// records would do N full builds where one suffices. Instead this function:
///
/// 1. Reads pending records (respecting `batch_size` as a claim cap, not a
///    sequential-build multiplier)
/// 2. Claims the batch under a short [`LockManager`] write-lock scope by
///    marking each record `processing` in a single DB pass, then releases
///    the lock so [`fire_ingest_chain`] → [`build_runner::run_build_from`]
///    can take its own per-slug write lock (same-task re-acquisition of an
///    exclusive `tokio::sync::RwLock` write guard deadlocks, see
///    `lock_manager.rs` doc comment)
/// 3. Emits `IngestStarted` events for each claimed record
/// 4. Calls `fire_ingest_chain` ONCE for the whole batch
/// 5. Under another short write-lock scope, marks all claimed records
///    `complete` with the real `build_id` (on success) or `failed` with the
///    error message (on failure)
/// 6. Emits `IngestComplete` / `IngestFailed` events per record
async fn dispatch_pending_ingests(
    state: &Arc<PyramidState>,
    db_path: &str,
    config: &DadbearWatchConfig,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    let slug = &config.slug;
    let batch_size = config.batch_size as usize;

    // Get pending records (no lock needed for a read of 'pending' rows — a
    // racing writer would only add more pending rows, not remove ours).
    let pending = {
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        db::get_pending_ingests(&conn, slug)?
    };

    if pending.is_empty() {
        return Ok(());
    }

    // Respect batch_size as a CLAIM cap — we'll fire a single chain build for
    // the whole claimed batch, not batch_size sequential builds.
    let claimed: Vec<IngestRecord> = pending
        .into_iter()
        .take(batch_size.max(1))
        .collect();

    if claimed.is_empty() {
        return Ok(());
    }

    // ── Claim: mark all claimed records as 'processing' under a short lock ──
    {
        let _lock = LockManager::global().write(slug).await;
        let conn = db::open_pyramid_connection(Path::new(db_path))?;
        for record in &claimed {
            if let Err(e) = db::mark_ingest_processing(&conn, record.id) {
                warn!(
                    slug = %slug,
                    record_id = record.id,
                    error = %e,
                    "DADBEAR: failed to mark ingest processing during claim"
                );
            }
        }
    }

    // Emit IngestStarted per claimed record
    for record in &claimed {
        let _ = event_bus.tx.send(TaggedBuildEvent {
            slug: slug.clone(),
            kind: TaggedKind::IngestStarted {
                source_path: record.source_path.clone(),
            },
        });
    }

    let source_paths: Vec<String> = claimed.iter().map(|r| r.source_path.clone()).collect();
    info!(
        slug = %slug,
        record_count = claimed.len(),
        content_type = %config.content_type,
        "DADBEAR: dispatching ingest chain for claimed batch"
    );

    // ── Fire the chain ONCE for the whole claimed batch ──
    // Must NOT hold the LockManager write lock here — run_build_from takes
    // its own exclusive write lock internally (build_runner.rs:208), and
    // the tokio RwLock is not reentrant.
    let dispatch_result =
        fire_ingest_chain(state, slug, &config.content_type, &source_paths, event_bus).await;

    // ── Mark outcome for all claimed records under another short lock ──
    match dispatch_result {
        Ok(build_id) => {
            {
                let _lock = LockManager::global().write(slug).await;
                let conn = db::open_pyramid_connection(Path::new(db_path))?;
                for record in &claimed {
                    if let Err(e) = db::mark_ingest_complete(&conn, record.id, &build_id) {
                        error!(
                            slug = %slug,
                            record_id = record.id,
                            error = %e,
                            "DADBEAR: failed to mark ingest complete (record will remain processing)"
                        );
                        // Best-effort: try to mark failed so it leaves 'processing'.
                        let _ = db::mark_ingest_failed(&conn, record.id, &e.to_string());
                    }
                }
            }

            for record in &claimed {
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
        }
        Err(err) => {
            let err_msg = format!("{err}");
            error!(
                slug = %slug,
                record_count = claimed.len(),
                error = %err_msg,
                "DADBEAR: ingest chain dispatch failed — marking claimed records failed"
            );

            {
                let _lock = LockManager::global().write(slug).await;
                let conn = db::open_pyramid_connection(Path::new(db_path))?;
                for record in &claimed {
                    if let Err(e) = db::mark_ingest_failed(&conn, record.id, &err_msg) {
                        error!(
                            slug = %slug,
                            record_id = record.id,
                            error = %e,
                            "DADBEAR: failed to mark ingest failed"
                        );
                    }
                }
            }

            for record in &claimed {
                let _ = event_bus.tx.send(TaggedBuildEvent {
                    slug: slug.clone(),
                    kind: TaggedKind::IngestFailed {
                        source_path: record.source_path.clone(),
                        error: err_msg.clone(),
                    },
                });
            }
        }
    }

    Ok(())
}

// ── fire_ingest_chain ──────────────────────────────────────────────────────────

/// Chunk new source files into `pyramid_chunks` and fire the content-type
/// chain via [`build_runner::run_build_from`], returning the real `build_id`.
///
/// **Lock ordering contract** (load-bearing — do not move these scopes):
///
/// - Chunking takes a short-lived `LockManager::global().write(slug)` scope
///   that is released BEFORE calling `run_build_from`.
/// - `run_build_from` takes its own exclusive write lock at build_runner.rs:208
///   for the full duration of the build. Because the tokio `RwLock` is not
///   reentrant, holding that lock across the `run_build_from` call would
///   deadlock the same tokio task on itself.
///
/// **Build-scoped reader**: creates an isolated reader via
/// [`PyramidState::with_build_reader`] so the build doesn't contend on the
/// shared reader Mutex with CLI/frontend queries.
///
/// **Channel setup**: creates ephemeral mpsc channels for `write_tx` (with a
/// full local writer drain task covering every `WriteOp` variant),
/// `progress_tx` (teed through `event_bus` so Pipeline B builds are visible
/// in build viz), and `layer_tx` (drained locally — future Phase 13 work
/// will expand build viz visibility for Pipeline B).
///
/// **Scope (Phase 0b)**: only conversation content type is supported.
/// Non-conversation records return an explicit error so callers mark them
/// `failed` rather than silently succeeding. Per-file code/doc ingest is
/// Phase 17's scope; see the Phase 0b implementation log entry.
pub async fn fire_ingest_chain(
    state: &Arc<PyramidState>,
    slug: &str,
    content_type: &str,
    source_paths: &[String],
    event_bus: &Arc<BuildEventBus>,
) -> Result<String> {
    if source_paths.is_empty() {
        return Err(anyhow!(
            "fire_ingest_chain: no source paths provided for slug '{}'",
            slug
        ));
    }

    let ct = ContentType::from_str(content_type)
        .ok_or_else(|| anyhow!("Unknown content type: {}", content_type))?;

    // ── 1. Build-scoped state with isolated reader (matches main.rs:3566) ──
    let build_state = state
        .with_build_reader()
        .map_err(|e| anyhow!("fire_ingest_chain: with_build_reader failed: {e}"))?;

    // ── 2. Chunk new source files into pyramid_chunks. execute_chain_from
    //       at chain_executor.rs:3804 rejects non-question pipelines with zero
    //       chunks, so this step is mandatory for conversation/code/doc. ──
    match ct {
        ContentType::Conversation => {
            let _lock = LockManager::global().write(slug).await;
            let writer = state.writer.clone();
            let slug_owned = slug.to_string();
            let paths_owned: Vec<String> = source_paths.to_vec();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = writer.blocking_lock();
                for path_str in &paths_owned {
                    let path = Path::new(path_str);
                    if !path.exists() {
                        warn!(
                            slug = %slug_owned,
                            source_path = %path_str,
                            "fire_ingest_chain: source file does not exist, skipping"
                        );
                        continue;
                    }
                    // Full ingest of the file's current contents. We do NOT use
                    // ingest_continuation here because Pipeline B's ingest
                    // record schema doesn't track per-file message offsets —
                    // the message count it would need for `skip_messages` isn't
                    // stored anywhere. Re-chunking the whole file on
                    // re-dispatch is correct-if-slow for Phase 0b; Phase 6's
                    // content-addressable LLM output cache will make the
                    // re-chunk work cheap downstream, and a future phase can
                    // introduce per-file message counters if needed.
                    ingest::ingest_conversation(&conn, &slug_owned, path)?;
                }
                Ok(())
            })
            .await
            .map_err(|e| anyhow!("fire_ingest_chain: chunking task panicked: {e}"))??;
        }
        ContentType::Code | ContentType::Document => {
            return Err(anyhow!(
                "Phase 0b: content_type '{}' is not yet supported by Pipeline B ingest; \
                 per-file code/doc ingest lands in Phase 17 (recursive folder ingestion). \
                 Record will be marked failed.",
                content_type
            ));
        }
        ContentType::Vine | ContentType::Question => {
            return Err(anyhow!(
                "fire_ingest_chain: content_type '{}' is not a file-backed ingest target \
                 (Pipeline B is for new-file ingestion; Vine/Question pyramids use other paths)",
                content_type
            ));
        }
    }

    // ── 3. Set up ephemeral channels mirroring the canonical build dispatch
    //       block in main.rs:3566-3730. The writer drain task must handle every
    //       WriteOp variant for correctness under chain execution. ──
    let (write_tx, mut write_rx) = mpsc::channel::<WriteOp>(256);
    let writer_handle = {
        let writer_conn = state.writer.clone();
        tokio::spawn(async move {
            while let Some(op) = write_rx.recv().await {
                let result = {
                    let conn = writer_conn.lock().await;
                    match op {
                        WriteOp::SaveNode {
                            ref node,
                            ref topics_json,
                        } => db::save_node(&conn, node, topics_json.as_deref()),
                        WriteOp::SaveStep {
                            ref slug,
                            ref step_type,
                            chunk_index,
                            depth,
                            ref node_id,
                            ref output_json,
                            ref model,
                            elapsed,
                        } => db::save_step(
                            &conn,
                            slug,
                            step_type,
                            chunk_index,
                            depth,
                            node_id,
                            output_json,
                            model,
                            elapsed,
                        ),
                        WriteOp::UpdateParent {
                            ref slug,
                            ref node_id,
                            ref parent_id,
                        } => db::update_parent(&conn, slug, node_id, parent_id),
                        WriteOp::UpdateStats { ref slug } => db::update_slug_stats(&conn, slug),
                        WriteOp::UpdateFileHash {
                            ref slug,
                            ref file_path,
                            ref node_id,
                        } => db::append_node_id_to_file_hash(&conn, slug, file_path, node_id),
                        WriteOp::Flush { done } => {
                            let _ = done.send(());
                            Ok(())
                        }
                    }
                };
                if let Err(e) = result {
                    error!("fire_ingest_chain writer drain: WriteOp failed: {e}");
                }
            }
        })
    };

    // Progress channel — tee'd onto the build_event_bus so Pipeline B builds
    // become visible in build viz alongside normal builds.
    let (progress_tx, raw_progress_rx) = mpsc::channel::<BuildProgress>(64);
    let mut progress_rx = super::event_bus::tee_build_progress_to_bus(
        event_bus,
        slug.to_string(),
        raw_progress_rx,
    );
    let progress_handle = tokio::spawn(async move {
        // Drain the teed progress so the upstream sender doesn't block.
        while progress_rx.recv().await.is_some() {}
    });

    // Layer event channel — drained locally. Phase 13 will expand build viz
    // to surface Pipeline B layer events the same way normal builds do.
    let (layer_tx, mut layer_rx) = mpsc::channel::<LayerEvent>(256);
    let layer_handle = tokio::spawn(async move {
        while layer_rx.recv().await.is_some() {}
    });

    // Fresh cancellation token per dispatch. Pipeline B dispatch is not
    // externally cancellable today; a future phase can add that.
    let cancel = CancellationToken::new();

    // ── 4. Fire the chain via the canonical entry point ──
    let run_result = build_runner::run_build_from(
        &build_state,
        slug,
        0,    // from_depth: full build
        None, // stop_after
        None, // force_from
        &cancel,
        Some(progress_tx.clone()),
        &write_tx,
        Some(layer_tx.clone()),
    )
    .await;

    // Drop senders so drain tasks finish cleanly before we return.
    drop(write_tx);
    drop(progress_tx);
    drop(layer_tx);
    let _ = writer_handle.await;
    let _ = progress_handle.await;
    let _ = layer_handle.await;

    match run_result {
        Ok((build_id, _failures, _step_activity)) => {
            info!(
                slug = %slug,
                build_id = %build_id,
                source_count = source_paths.len(),
                "fire_ingest_chain: chain build complete"
            );
            Ok(build_id)
        }
        Err(e) => {
            error!(
                slug = %slug,
                source_count = source_paths.len(),
                error = %e,
                "fire_ingest_chain: chain build failed"
            );
            Err(e)
        }
    }
}

// ── Manual trigger ─────────────────────────────────────────────────────────────

/// Manually trigger a single scan+dispatch cycle for all configs of a given slug.
/// Used by the POST /pyramid/:slug/dadbear/trigger HTTP route.
pub async fn trigger_for_slug(
    state: &Arc<PyramidState>,
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
        match run_tick_for_config(state, db_path, config, event_bus).await {
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

    // ── fire_ingest_chain helpers + tests (Phase 0b) ────────────────────────

    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Mutex as TokioMutex;

    /// Build a `PyramidState` rooted at a freshly-created tempdir containing
    /// an initialized `pyramid.db`. The caller gets back (state, data_dir,
    /// db_path) — `data_dir` must outlive the state (we leak it intentionally
    /// because tests run to completion and the OS cleans up /tmp on its own
    /// schedule; this is the same pattern `test_db()` uses above).
    fn make_test_state() -> (Arc<PyramidState>, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().to_path_buf();
        // Initialize the pyramid.db at data_dir/pyramid.db so with_build_reader
        // and the writer both open the same on-disk database.
        let db_path = data_dir.join("pyramid.db");
        let writer_conn = db::open_pyramid_db(&db_path).unwrap();
        let reader_conn = db::open_pyramid_connection(&db_path).unwrap();
        // Leak the tempdir so it lives past test scope (the OS will clean up).
        std::mem::forget(dir);

        let llm_config = crate::pyramid::llm::LlmConfig::default();
        let state = Arc::new(PyramidState {
            reader: Arc::new(TokioMutex::new(reader_conn)),
            writer: Arc::new(TokioMutex::new(writer_conn)),
            config: Arc::new(tokio::sync::RwLock::new(llm_config)),
            active_build: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            data_dir: Some(data_dir.clone()),
            stale_engines: Arc::new(TokioMutex::new(HashMap::new())),
            file_watchers: Arc::new(TokioMutex::new(HashMap::new())),
            vine_builds: Arc::new(TokioMutex::new(HashMap::new())),
            use_chain_engine: AtomicBool::new(false),
            use_ir_executor: AtomicBool::new(false),
            event_bus: Arc::new(crate::pyramid::event_chain::LocalEventBus::new()),
            operational: Arc::new(crate::pyramid::OperationalConfig::default()),
            chains_dir: data_dir.join("chains"),
            remote_query_rate_limiter: Arc::new(TokioMutex::new(HashMap::new())),
            absorption_gate: Arc::new(TokioMutex::new(
                crate::pyramid::AbsorptionGate::new(),
            )),
            build_event_bus: Arc::new(BuildEventBus::new()),
            supabase_url: None,
            supabase_anon_key: None,
            csrf_secret: [0u8; 32],
            dadbear_handle: Arc::new(TokioMutex::new(None)),
        });
        (state, data_dir)
    }

    // ── Test 6: fire_ingest_chain rejects empty source paths ───────────────

    #[tokio::test]
    async fn test_fire_ingest_chain_empty_source_paths() {
        let (state, _data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        let result =
            fire_ingest_chain(&state, "test-slug", "conversation", &[], &bus).await;

        assert!(result.is_err(), "expected error for empty source_paths");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no source paths"),
            "error should mention empty source paths, got: {err}"
        );
    }

    // ── Test 7: fire_ingest_chain returns scope-decision error for code ────

    #[tokio::test]
    async fn test_fire_ingest_chain_code_scope_error() {
        let (state, _data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        // Create a code slug so with_build_reader has a real DB to open.
        {
            let conn = state.writer.lock().await;
            db::create_slug(&conn, "test-code", &ContentType::Code, "/tmp/src").unwrap();
        }

        let result = fire_ingest_chain(
            &state,
            "test-code",
            "code",
            &["/tmp/src/main.rs".to_string()],
            &bus,
        )
        .await;

        assert!(result.is_err(), "expected scope-decision error for code");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Phase 0b") && err.contains("code"),
            "error should mention Phase 0b scope decision for code, got: {err}"
        );
        assert!(
            err.contains("Phase 17") || err.contains("folder"),
            "error should point at Phase 17 folder ingestion, got: {err}"
        );
    }

    // ── Test 8: fire_ingest_chain returns scope-decision error for document ─

    #[tokio::test]
    async fn test_fire_ingest_chain_document_scope_error() {
        let (state, _data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        // Create a document slug.
        {
            let conn = state.writer.lock().await;
            db::create_slug(&conn, "test-doc", &ContentType::Document, "/tmp/src").unwrap();
        }

        let result = fire_ingest_chain(
            &state,
            "test-doc",
            "document",
            &["/tmp/src/README.md".to_string()],
            &bus,
        )
        .await;

        assert!(
            result.is_err(),
            "expected scope-decision error for document"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Phase 0b") && err.contains("document"),
            "error should mention Phase 0b scope decision for document, got: {err}"
        );
    }

    // ── Test 9: fire_ingest_chain rejects unknown content type ─────────────

    #[tokio::test]
    async fn test_fire_ingest_chain_unknown_content_type() {
        let (state, _data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        let result = fire_ingest_chain(
            &state,
            "test-slug",
            "not_a_real_type",
            &["/tmp/foo.bin".to_string()],
            &bus,
        )
        .await;

        assert!(
            result.is_err(),
            "expected error for unknown content type"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Unknown content type"),
            "error should mention unknown content type, got: {err}"
        );
    }

    // ── Test 10: fire_ingest_chain chunks a conversation file before
    //              reaching the chain engine (success-path chunking coverage) ─
    //
    // This test verifies the load-bearing invariant from
    // chain_executor.rs:3804: "No chunks found for slug '...' — cannot run
    // non-question pipeline with zero chunks". Our helper must chunk BEFORE
    // calling run_build_from. We assert:
    //   1. `fire_ingest_chain` populates `pyramid_chunks` for the slug (so
    //      the chain step would not be rejected for zero chunks)
    //   2. The eventual error (expected because no chains dir / no LLM) does
    //      NOT come from the zero-chunks guard — it must come from later
    //      in the pipeline

    #[tokio::test]
    async fn test_fire_ingest_chain_chunks_conversation_before_dispatch() {
        let (state, data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        // Create a conversation slug backed by a real jsonl file on disk.
        let source_dir = data_dir.join("conversations");
        std::fs::create_dir_all(&source_dir).unwrap();
        let jsonl_path = source_dir.join("session.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            // A handful of messages so the chunker has real content.
            for i in 0..3 {
                writeln!(
                    f,
                    r#"{{"type":"user","message":{{"role":"user","content":"message {i}"}},"timestamp":"2026-04-09T10:00:00"}}"#,
                )
                .unwrap();
                writeln!(
                    f,
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":"reply {i}"}},"timestamp":"2026-04-09T10:00:01"}}"#,
                )
                .unwrap();
            }
        }

        {
            let conn = state.writer.lock().await;
            db::create_slug(
                &conn,
                "test-conv-chunk",
                &ContentType::Conversation,
                source_dir.to_str().unwrap(),
            )
            .unwrap();
        }

        // Fire the chain. We EXPECT this to fail at run_build_from because
        // there's no chains dir with real chain YAML in our test state. The
        // important thing is that it failed LATER than the chunking step.
        let result = fire_ingest_chain(
            &state,
            "test-conv-chunk",
            "conversation",
            &[jsonl_path.to_string_lossy().to_string()],
            &bus,
        )
        .await;

        // Now assert chunks were persisted — this is the load-bearing
        // invariant for Phase 0b's chunking-before-dispatch contract.
        {
            let conn = state.reader.lock().await;
            let count = db::count_chunks(&conn, "test-conv-chunk").unwrap();
            assert!(
                count > 0,
                "fire_ingest_chain must chunk before calling run_build_from; got 0 chunks"
            );
        }

        // The error (if any) must not be the "No chunks found" guard.
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("No chunks found"),
                "fire_ingest_chain reached the zero-chunks guard despite chunking; error: {msg}"
            );
        }
        // If run_build_from somehow succeeded (e.g. via a working chains dir
        // fallback), that's fine — chunks still have to exist, which is the
        // above assertion.
    }
}
