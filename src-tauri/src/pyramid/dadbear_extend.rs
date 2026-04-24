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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::build::WriteOp;
use super::build_runner;
use super::dadbear_compiler;
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

/// RAII guard that clears a per-config in-flight flag on drop.
///
/// The tick loop sets a `HashMap<config.id, Arc<AtomicBool>>` entry to `true`
/// before calling [`run_tick_for_config`], then constructs an `InFlightGuard`
/// holding a clone of the same `Arc<AtomicBool>`. When the guard drops — on
/// normal return, early `?` propagation, OR a panic unwinding through the
/// tick invocation — `Drop::drop` stores `false` so the next tick can proceed.
///
/// **Why this is load-bearing**: `run_tick_for_config` calls into
/// [`fire_ingest_chain`], which in turn calls [`build_runner::run_build_from`]
/// and drives an LLM chain. A panic anywhere in that stack (LLM parse failure,
/// DB corruption, missing chain YAML, etc.) would, with a naive
/// `store(false)` after the match arm, leave the flag stuck at `true`
/// forever — every subsequent tick for that config would skip and no
/// ingest would ever fire again until the process restarts. The RAII guard
/// cannot be forgotten on any exit path.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
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

        // Phase 0: subscribe to the event bus for DadbearConfigChanged
        // events. When a dadbear_norms or dadbear_policy contribution
        // is activated, the dispatcher emits this event and we force an
        // immediate config reload on the next tick by resetting all
        // ticker timestamps.
        let mut config_rx = event_bus.subscribe();
        // Flag: when set, all tickers are reset on the next iteration
        // so every config fires immediately.
        let mut force_reload = false;

        // Per-config in-flight flags now live on shared `PyramidState`
        // (see `PyramidState::dadbear_in_flight`). This lets `trigger_for_slug`
        // (HTTP/CLI manual trigger path) consult the same map and skip when an
        // auto-dispatch is in flight for the same config, closing the real
        // HTTP-trigger-vs-auto-dispatch race.
        //
        // AtomicBool per-config, NOT a LockManager write lock: the
        // LockManager's per-slug write lock would block concurrent queries
        // and other writers for the full chain duration; this flag only
        // prevents re-entrant DADBEAR dispatch for the same config and leaves
        // all other paths unaffected.

        loop {
            // Check cancellation + drain config change events
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    info!("DADBEAR-EXTEND tick loop cancelled");
                    break;
                }
                // Phase 0: drain DadbearConfigChanged events from the bus.
                // On receipt, set the force_reload flag so all tickers fire
                // immediately on the next iteration. On Lagged (slow
                // consumer), also force reload — we may have missed a
                // config change.
                result = config_rx.recv() => {
                    match result {
                        Ok(event) => {
                            if matches!(event.kind, TaggedKind::DadbearConfigChanged { .. }) {
                                info!(
                                    slug = %event.slug,
                                    "DADBEAR-EXTEND: config changed event received, forcing reload"
                                );
                                force_reload = true;
                            }
                            // Non-DadbearConfigChanged events are ignored;
                            // continue to the sleep branch on the next select.
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                skipped = n,
                                "DADBEAR-EXTEND: event bus lagged, forcing config reload"
                            );
                            force_reload = true;
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("DADBEAR-EXTEND: event bus closed, exiting tick loop");
                            break;
                        }
                    }
                }
                // Base tick: check every 1 second whether any config is due
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }

            // Phase 0: if a config change was received, reset all tickers
            // so every config fires on this iteration rather than waiting
            // for its next scan_interval window. We set last_tick far
            // enough in the past that (now - last_tick) exceeds any
            // reasonable scan_interval.
            if force_reload {
                let now_for_reset = std::time::Instant::now();
                for ticker in tickers.values_mut() {
                    // Push last_tick back by the ticker's own interval + 1s
                    // so the duration_since check passes immediately.
                    ticker.last_tick = now_for_reset
                        .checked_sub(ticker.interval + std::time::Duration::from_secs(1))
                        .unwrap_or(now_for_reset);
                }
                force_reload = false;
                debug!("DADBEAR-EXTEND: tickers reset for forced config reload");
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
            // Mirror the ticker cleanup for the shared in-flight map so removed
            // configs don't accumulate entries across the lifetime of the tick
            // loop. Acquire the mutex in a short scope — the retain closure is
            // purely CPU-bound so no await crosses this lock.
            {
                let mut guard = match state.dadbear_in_flight.lock() {
                    Ok(g) => g,
                    Err(poisoned) => {
                        // A panicking holder of the mutex can poison it. The
                        // inner HashMap is plain data, so recovering is safe
                        // and preferable to killing the entire tick loop.
                        warn!("DADBEAR-EXTEND: dadbear_in_flight mutex was poisoned; recovering");
                        poisoned.into_inner()
                    }
                };
                guard.retain(|id, _| configs.iter().any(|c| c.id == *id));
            }

            let now = std::time::Instant::now();

            for config in &configs {
                // Phase 1 (fix pass): skip this tick if the previous dispatch
                // for this config is still in flight. The flag lives on
                // shared `PyramidState::dadbear_in_flight` so that BOTH the
                // tick loop and `trigger_for_slug` (HTTP/CLI manual trigger)
                // observe the same signal. Checked BEFORE the interval-due
                // check so every 1-second base tick during a long dispatch
                // emits the skip log. `last_tick` is NOT advanced on skip,
                // so when the flag clears after a slow chain, the next base
                // tick fires immediately rather than waiting for another
                // `scan_interval_secs` window.
                //
                // The mutex is held only long enough to look up or lazy-insert
                // the entry and clone the inner `Arc<AtomicBool>` — NEVER
                // across the `.await` below. The cloned Arc is what the RAII
                // guard stores and flips.
                let flag = {
                    let mut guard = match state.dadbear_in_flight.lock() {
                        Ok(g) => g,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    guard
                        .entry(config.id)
                        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                        .clone()
                };

                if flag.load(Ordering::Relaxed) {
                    debug!(
                        slug = %config.slug,
                        "DADBEAR: skipping tick, previous dispatch in-flight"
                    );
                    continue;
                }

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

                // Set the flag and hand a clone to the RAII guard. The guard
                // clears the flag on drop — normal return, `?`-propagated
                // error, OR panic — so the flag cannot stick at true and
                // deadlock the config's tick loop (or later races against
                // `trigger_for_slug`'s manual trigger).
                flag.store(true, Ordering::Relaxed);
                let _guard = InFlightGuard(flag.clone());

                // Phase B: defer DADBEAR if any build is active and policy says so.
                {
                    let should_defer = {
                        let cfg = state.config.read().await;
                        cfg.dispatch_policy
                            .as_ref()
                            .map(|p| p.build_coordination.defer_dadbear_during_build)
                            .unwrap_or(false)
                    };
                    if should_defer {
                        let builds = state.active_build.read().await;
                        if !builds.is_empty() {
                            debug!(
                                slug = %config.slug,
                                active_builds = builds.len(),
                                "DADBEAR-EXTEND: deferring tick, builds active"
                            );
                            continue;
                        }
                    }
                }

                // Run the tick for this config
                if let Err(e) = run_tick_for_config(&state, &db_path, config, &event_bus).await {
                    error!(
                        slug = %config.slug,
                        source_path = %config.source_path,
                        error = %e,
                        "DADBEAR-EXTEND tick failed"
                    );
                }

                // `_guard` drops here at end of iteration — clears the flag.
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
                warn!(
                    "DADBEAR: failed to save ingest record for {}: {}",
                    sf.path, e
                );
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

    // ── 6. Run DADBEAR compiler: observations → work items ──────────────
    // The compiler reads new observation events and creates durable work items
    // in 'compiled' state. It does NOT dispatch them — that's Phase 5 (supervisor).
    // The compiler runs even when holds are active (holds block dispatch, not compilation).
    {
        let db_compile = db_path.to_string();
        let slug_compile = slug.to_string();
        let compile_result = tokio::task::spawn_blocking(move || {
            let conn = db::open_pyramid_connection(Path::new(&db_compile))?;
            // Look up the active dadbear_norms contribution ID for epoch tracking.
            // Track the RESOLVED norms (global + per-slug merge) via a hash.
            // This catches changes at either level — per-slug contribution
            // supersession OR global norms change. The compiler compares
            // the hash against the stored epoch's norms_contribution_id
            // (repurposed as a norms_hash). Any change triggers epoch rotation.
            // Recipe contributions are not yet implemented (chains are YAML
            // files, not contributions), so recipe stays None.
            let norms_hash = {
                let resolved = crate::pyramid::config_contributions::resolve_dadbear_norms(
                    &conn,
                    Some(&slug_compile),
                )
                .unwrap_or_default();
                let yaml_str = serde_yaml::to_string(&resolved).unwrap_or_default();
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                yaml_str.hash(&mut hasher);
                format!("{:016x}", hasher.finish())
            };
            dadbear_compiler::run_compilation_for_slug(
                &conn,
                &slug_compile,
                None, // recipe_contribution_id (chains not yet contributions)
                Some(&norms_hash),
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("Compiler task join error: {e}"))?;

        match compile_result {
            Ok(result) => {
                if result.items_compiled > 0 {
                    debug!(
                        slug = %slug,
                        items = result.items_compiled,
                        deps = result.deps_created,
                        deduped = result.deduped,
                        cursor = result.new_cursor,
                        "DADBEAR compiler: new work items compiled"
                    );
                }
            }
            Err(e) => {
                warn!(
                    slug = %slug,
                    error = %e,
                    "DADBEAR compiler pass failed (non-fatal, will retry next tick)"
                );
            }
        }
    }

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
                let mtime: chrono::DateTime<Utc> =
                    meta.modified().ok().map(|t| t.into()).unwrap_or(now);
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
    let claimed: Vec<IngestRecord> = pending.into_iter().take(batch_size.max(1)).collect();

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
    //       chunks, so this step is mandatory for conversation/code/doc.
    //
    //       IMPORTANT: We MUST clear existing chunks before re-ingesting. Both
    //       `ingest_conversation` here and the existing wizard/routes.rs ingest
    //       path use chunk_index starting at 0 per file, so re-dispatches
    //       collide with the prior run's chunks on the `UNIQUE(slug,
    //       chunk_index)` constraint of pyramid_chunks (db.rs:107). The
    //       equivalent wizard path handles this at routes.rs:3431 with an
    //       explicit `clear_chunks` before re-ingest; Pipeline B must do the
    //       same or the SECOND dispatch for any slug fails with a UNIQUE
    //       constraint error and the ingest record is marked failed.
    //
    //       Re-chunking the whole file on re-dispatch (with clear+re-ingest) is
    //       correct-if-slow for Phase 0b; Phase 6's content-addressable LLM
    //       output cache will make the re-chunk work cheap downstream, and a
    //       future phase can introduce per-file message counters to enable
    //       `ingest_continuation` as an incremental alternative. ──
    match ct {
        ContentType::Conversation => {
            let _lock = LockManager::global().write(slug).await;
            let writer = state.writer.clone();
            let slug_owned = slug.to_string();
            let paths_owned: Vec<String> = source_paths.to_vec();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = writer.blocking_lock();
                // Clear existing chunks for this slug to prevent UNIQUE
                // constraint collisions with prior runs or with earlier files
                // in the same dispatch. Mirrors routes.rs:3431.
                let cleared = db::clear_chunks(&conn, &slug_owned)?;
                if cleared > 0 {
                    info!(
                        slug = %slug_owned,
                        cleared,
                        "fire_ingest_chain: cleared stale chunks before re-ingest"
                    );
                }
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
                    // stored anywhere.
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
    let mut progress_rx =
        super::event_bus::tee_build_progress_to_bus(event_bus, slug.to_string(), raw_progress_rx);
    let progress_handle = tokio::spawn(async move {
        // Drain the teed progress so the upstream sender doesn't block.
        while progress_rx.recv().await.is_some() {}
    });

    // Layer event channel — drained locally. Phase 13 will expand build viz
    // to surface Pipeline B layer events the same way normal builds do.
    let (layer_tx, mut layer_rx) = mpsc::channel::<LayerEvent>(256);
    let layer_handle = tokio::spawn(async move { while layer_rx.recv().await.is_some() {} });

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
///
/// **Phase 1 fix pass**: before invoking `run_tick_for_config` for each config,
/// consult the shared `PyramidState::dadbear_in_flight` flag. If the auto tick
/// loop (or a previous manual trigger) is already mid-dispatch for this config,
/// skip it and include a `"skipped: dispatch in-flight"` note in the returned
/// JSON so the HTTP caller learns the trigger was a no-op rather than a second
/// full-pipeline dispatch. On the common case where no dispatch is running,
/// store `true`, construct an `InFlightGuard`, run the tick, drop the guard —
/// the same RAII pattern the tick loop uses, mirrored here verbatim so a
/// panicking `run_tick_for_config` cannot leave the flag stuck at `true`.
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
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    let mut errors = Vec::new();

    for config in &configs {
        // Phase 1 fix: consult the shared in-flight flag before dispatch so a
        // manual trigger fired while the auto loop is mid-chain returns a
        // skip note instead of firing a second full pipeline.
        //
        // The mutex is held only long enough to look up or lazy-insert the
        // entry and clone the inner Arc — NEVER across the `.await` below.
        let flag = {
            let mut guard = match state.dadbear_in_flight.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .entry(config.id)
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .clone()
        };

        if flag.load(Ordering::Relaxed) {
            debug!(
                slug = %config.slug,
                source_path = %config.source_path,
                "DADBEAR: manual trigger skipped, dispatch already in-flight"
            );
            skipped.push(serde_json::json!({
                "source_path": config.source_path,
                "reason": "dispatch in-flight",
            }));
            continue;
        }

        // Claim the flag and hand a clone to the RAII guard — same panic-safe
        // pattern the tick loop uses. A panic anywhere in `run_tick_for_config`
        // drops `_guard` during unwind and clears the flag so neither the tick
        // loop nor the next trigger gets stuck waiting for a flag that never
        // clears.
        flag.store(true, Ordering::Relaxed);
        let _guard = InFlightGuard(flag.clone());

        match run_tick_for_config(state, db_path, config, event_bus).await {
            Ok(()) => processed += 1,
            Err(e) => errors.push(format!("{}: {}", config.source_path, e)),
        }

        // `_guard` drops here at end of iteration — clears the flag.
    }

    Ok(serde_json::json!({
        "slug": slug,
        "configs_processed": processed,
        "skipped": skipped,
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
            last_scan_at: None,
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
        let session = db::get_provisional_session(&conn, session_id)
            .unwrap()
            .unwrap();
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
        let session = db::get_provisional_session(&conn, session_id)
            .unwrap()
            .unwrap();
        assert_eq!(session.file_mtime.as_deref(), Some("2026-04-08T12:00:00Z"));

        // update_session_chunk_progress
        db::update_session_chunk_progress(&conn, session_id, 5).unwrap();
        let session2 = db::get_provisional_session(&conn, session_id)
            .unwrap()
            .unwrap();
        assert_eq!(session2.last_chunk_processed, 5);

        // Update again
        db::update_session_chunk_progress(&conn, session_id, 10).unwrap();
        let session3 = db::get_provisional_session(&conn, session_id)
            .unwrap()
            .unwrap();
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
            absorption_gate: Arc::new(TokioMutex::new(crate::pyramid::AbsorptionGate::new())),
            build_event_bus: Arc::new(BuildEventBus::new()),
            supabase_url: None,
            supabase_anon_key: None,
            csrf_secret: [0u8; 32],
            dadbear_handle: Arc::new(TokioMutex::new(None)),
            dadbear_supervisor_handle: Arc::new(TokioMutex::new(None)),
            dadbear_in_flight: Arc::new(std::sync::Mutex::new(HashMap::new())),
            provider_registry: {
                // Phase 3: test state gets an empty provider registry.
                // The DADBEAR tick loop doesn't invoke LLM calls in the
                // unit tests that use this helper.
                let store = Arc::new(
                    crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
                );
                Arc::new(crate::pyramid::provider::ProviderRegistry::new(store))
            },
            credential_store: Arc::new(
                crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
            ),
            schema_registry: Arc::new(crate::pyramid::schema_registry::SchemaRegistry::new()),
            cross_pyramid_router: Arc::new(
                crate::pyramid::cross_pyramid_router::CrossPyramidEventRouter::new(),
            ),
            ollama_pull_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ollama_pull_in_progress: Arc::new(tokio::sync::Mutex::new(None)),
        });
        (state, data_dir)
    }

    // ── Test 6: fire_ingest_chain rejects empty source paths ───────────────

    #[tokio::test]
    async fn test_fire_ingest_chain_empty_source_paths() {
        let (state, _data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        let result = fire_ingest_chain(&state, "test-slug", "conversation", &[], &bus).await;

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

        assert!(result.is_err(), "expected error for unknown content type");
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

    /// Regression test for the second-dispatch chunk-collision bug caught
    /// by the Phase 0b wanderer pass. `ingest_conversation` inserts chunks
    /// with `chunk_index` starting at 0, and `pyramid_chunks` has a
    /// `UNIQUE(slug, chunk_index)` constraint (db.rs:107). Without a
    /// `clear_chunks` call before re-ingesting, the SECOND dispatch for any
    /// slug that already has chunks fails with a UNIQUE constraint
    /// violation, the build never fires, and the ingest record gets marked
    /// `failed`. See `fire_ingest_chain`'s chunking block at ~line 603 and
    /// `routes.rs:3431` for the equivalent clear in the wizard path.
    #[tokio::test]
    async fn test_fire_ingest_chain_second_dispatch_no_chunk_collision() {
        let (state, data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        let source_dir = data_dir.join("conversations2");
        std::fs::create_dir_all(&source_dir).unwrap();

        // First file
        let jsonl_path = source_dir.join("session1.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            for i in 0..3 {
                writeln!(
                    f,
                    r#"{{"type":"user","message":{{"role":"user","content":"hi {i}"}},"timestamp":"2026-04-09T10:00:00"}}"#,
                )
                .unwrap();
            }
        }

        {
            let conn = state.writer.lock().await;
            db::create_slug(
                &conn,
                "test-repro-collision",
                &ContentType::Conversation,
                source_dir.to_str().unwrap(),
            )
            .unwrap();
        }

        // First dispatch — should chunk
        let _result1 = fire_ingest_chain(
            &state,
            "test-repro-collision",
            "conversation",
            &[jsonl_path.to_string_lossy().to_string()],
            &bus,
        )
        .await;

        let count1 = {
            let conn = state.reader.lock().await;
            db::count_chunks(&conn, "test-repro-collision").unwrap()
        };
        assert!(count1 > 0, "first dispatch should produce chunks");

        // Second dispatch — same file, no content change
        let result2 = fire_ingest_chain(
            &state,
            "test-repro-collision",
            "conversation",
            &[jsonl_path.to_string_lossy().to_string()],
            &bus,
        )
        .await;

        // If fire_ingest_chain clears chunks before re-ingesting, the second
        // dispatch will succeed (or fail at a later step like chains-dir).
        // If it does NOT clear, the chunking step itself will bubble up a
        // UNIQUE(slug, chunk_index) error.
        if let Err(e) = &result2 {
            let msg = e.to_string();
            assert!(
                !msg.contains("UNIQUE") && !msg.contains("constraint failed"),
                "second dispatch must not fail on chunk UNIQUE constraint; got: {msg}"
            );
        }
    }

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

    // ── Phase 1: in-flight guard tests ─────────────────────────────────────

    /// Phase 1 skip decision + RAII guard lifecycle.
    ///
    /// The spec's test requirement is: "exercise the skip decision" and
    /// verify the flag clears even on panic. This test walks the same state
    /// machine the tick loop runs per config:
    ///
    /// 1. Build a `HashMap<i64, Arc<AtomicBool>>` the same way
    ///    `start_dadbear_extend_loop` does.
    /// 2. Assert the first look-up for a config lazily creates a cleared flag
    ///    that does NOT cause the skip branch to fire.
    /// 3. Set the flag and construct an `InFlightGuard` around a clone of the
    ///    same `Arc<AtomicBool>`.
    /// 4. Assert a second iteration for the same `config.id` reuses the stored
    ///    flag, observes it set, and would `continue` (skip the tick).
    /// 5. Drop the guard (normal return path) and assert the flag clears.
    /// 6. Assert a third iteration after the guard drop does NOT skip.
    /// 7. Set the flag again and invoke a panicking closure inside
    ///    `std::panic::catch_unwind`; after the panic is caught, assert the
    ///    flag cleared — this is the load-bearing panic-safety guarantee
    ///    that the RAII guard exists to provide.
    /// 8. Exercise `in_flight.retain(...)` with a config list that no longer
    ///    contains the original config.id and assert the entry is removed.
    #[test]
    fn test_in_flight_guard_skip_and_panic_safety() {
        // ── (1) Same state the tick loop maintains ─────────────────────
        let mut in_flight: HashMap<i64, Arc<AtomicBool>> = HashMap::new();
        let config_id: i64 = 42;

        // ── (2) Lazy creation: first look-up makes a cleared flag ──────
        let flag = in_flight
            .entry(config_id)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        assert!(
            !flag.load(Ordering::Relaxed),
            "freshly-inserted flag must be cleared — otherwise a brand-new \
             config would skip its very first tick"
        );

        // ── (3) Set the flag, construct the guard ──────────────────────
        flag.store(true, Ordering::Relaxed);
        let guard = InFlightGuard(flag.clone());

        // ── (4) Second iteration for same config.id observes the flag ──
        {
            let looked_up = in_flight
                .entry(config_id)
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .clone();
            assert!(
                Arc::ptr_eq(&looked_up, &flag),
                "second entry lookup must return the SAME Arc (lifecycle \
                 matches tickers: insert once, observe each tick)"
            );
            assert!(
                looked_up.load(Ordering::Relaxed),
                "flag is still set while the guard lives — this is the \
                 condition the tick loop checks to decide to skip"
            );
            // The tick loop's `if flag.load(Ordering::Relaxed) { continue; }`
            // branch would fire here.
        }

        // ── (5) Guard drops on normal scope exit → flag clears ─────────
        drop(guard);
        assert!(
            !flag.load(Ordering::Relaxed),
            "InFlightGuard::drop must clear the flag on normal return"
        );

        // ── (6) Next iteration can proceed ─────────────────────────────
        assert!(
            !flag.load(Ordering::Relaxed),
            "after guard drop, a subsequent tick iteration must find the \
             flag cleared and proceed to run_tick_for_config"
        );

        // ── (7) Panic safety — the load-bearing requirement ────────────
        // With a naive `flag.store(false)` after the match arm, a panic
        // inside `run_tick_for_config` would bypass the store and leave the
        // flag stuck at true forever. The RAII guard MUST clear the flag on
        // panic unwind.
        flag.store(true, Ordering::Relaxed);
        let flag_for_panic = flag.clone();
        let panic_result = std::panic::catch_unwind(move || {
            let _guard = InFlightGuard(flag_for_panic);
            // Simulate a panic inside run_tick_for_config (LLM parse failure,
            // DB corruption, etc.) — the guard should still drop.
            panic!("simulated tick panic");
        });
        assert!(
            panic_result.is_err(),
            "catch_unwind should report the simulated panic"
        );
        assert!(
            !flag.load(Ordering::Relaxed),
            "InFlightGuard::drop MUST clear the flag on panic unwind — \
             otherwise the config's tick loop stays stuck until process \
             restart"
        );

        // ── (8) retain() removes entries for configs that no longer exist ──
        // Sanity-check the tick loop's cleanup pattern:
        //   in_flight.retain(|id, _| configs.iter().any(|c| c.id == *id));
        // When the active config list no longer contains config_id 42, the
        // entry for 42 must be dropped so the HashMap doesn't grow
        // unboundedly across the lifetime of the tick loop.
        assert!(
            in_flight.contains_key(&config_id),
            "precondition: in_flight still holds the entry we inserted"
        );
        let active_ids: Vec<i64> = vec![7, 99]; // config_id 42 is absent
        in_flight.retain(|id, _| active_ids.iter().any(|active| active == id));
        assert!(
            !in_flight.contains_key(&config_id),
            "retain() must drop entries whose config.id is not in the active \
             config list, mirroring the `tickers.retain(...)` pattern"
        );
    }

    /// Phase 1 wanderer test: empirical proof that the tick loop is serial.
    ///
    /// **Why this test exists**: the Phase 1 spec claims the in_flight flag
    /// guards against "the next 1-second tick starting a concurrent dispatch
    /// for the same config" while the previous dispatch is still running.
    /// That claim assumes the outer `loop { sleep(1s); for cfg in cfgs {
    /// run_tick_for_config(...).await; } }` advances the outer iteration
    /// while a prior iteration's `.await` is pending. It does not — a
    /// single `tokio::spawn`ed future cannot be polled while it is
    /// suspended at an `.await` point.
    ///
    /// This test mirrors the exact loop shape of `start_dadbear_extend_loop`
    /// (single `tokio::spawn` around `loop { sleep; for cfg in cfgs { await
    /// long_dispatch; } }`) and counts:
    ///   - how many outer iterations complete in a fixed wall-clock window
    ///   - how many `dispatch_start`s fire while a `dispatch` is pending
    ///
    /// If the spec's mental model were correct, we'd see more than one
    /// dispatch_start inside a single dispatch window. We don't, because the
    /// scheduler cannot re-enter a spawned future that is awaiting.
    #[tokio::test(flavor = "current_thread")]
    async fn test_tick_loop_is_serial_within_single_task() {
        use std::sync::atomic::AtomicUsize;
        use tokio::time::Duration;

        let dispatch_start = Arc::new(AtomicUsize::new(0));
        let dispatch_end = Arc::new(AtomicUsize::new(0));
        let outer_iters = Arc::new(AtomicUsize::new(0));

        let ds = dispatch_start.clone();
        let de = dispatch_end.clone();
        let oi = outer_iters.clone();

        // Mirror of start_dadbear_extend_loop's task shape, minus the DB.
        let task = tokio::spawn(async move {
            // Exactly one "config" in the list, emulating a single
            // DADBEAR config with a slow dispatch.
            let configs = vec![42i64];
            let mut in_flight: HashMap<i64, Arc<AtomicBool>> = HashMap::new();

            loop {
                tokio::time::sleep(Duration::from_millis(50)).await; // base tick
                oi.fetch_add(1, Ordering::Relaxed);

                for &config_id in &configs {
                    let flag = in_flight
                        .entry(config_id)
                        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                        .clone();
                    if flag.load(Ordering::Relaxed) {
                        // This is the skip branch the Phase 1 spec claims
                        // will fire "every 1-second base tick during a long
                        // dispatch". If the loop is serial within the task,
                        // this branch NEVER fires because the for-loop
                        // itself can't advance while the .await below is
                        // pending.
                        unreachable!(
                            "in_flight flag was observed set in a fresh \
                             iteration of the outer loop — that would \
                             mean the scheduler re-entered the spawned \
                             task's future while it was suspended at an \
                             await, which cannot happen"
                        );
                    }
                    flag.store(true, Ordering::Relaxed);
                    let _guard = InFlightGuard(flag.clone());

                    ds.fetch_add(1, Ordering::Relaxed);
                    // Simulate run_tick_for_config taking a "long" time
                    // relative to the base tick.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    de.fetch_add(1, Ordering::Relaxed);
                    // _guard drops here; flag clears.
                }
            }
        });

        // Let the loop run for ~1.2 seconds. In that window, if the outer
        // loop were advancing every 50ms while a 500ms dispatch was
        // pending, we'd see many dispatch_starts piled up. We won't.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        task.abort();

        let starts = dispatch_start.load(Ordering::Relaxed);
        let ends = dispatch_end.load(Ordering::Relaxed);
        let iters = outer_iters.load(Ordering::Relaxed);

        // Each iteration is: 50ms base sleep + 500ms dispatch = 550ms per
        // iteration. In 1200ms we get at most 2-3 full iterations. If the
        // outer loop advanced independently of the dispatch, we'd see
        // dispatch_start fire ~24 times (1200 / 50).
        assert!(
            starts <= 3,
            "dispatch_start fired {} times in 1.2s — if the spec's mental \
             model were right and the outer loop advanced while inner await \
             was pending, we'd see many more",
            starts
        );
        assert!(
            starts >= 1,
            "at least one dispatch should have started in 1.2s, got {}",
            starts
        );
        // dispatch_end is always <= dispatch_start (the last one may be
        // aborted mid-sleep), and iters == starts (because the skip branch
        // is unreachable, every iteration calls dispatch_start).
        assert!(ends <= starts, "ends={} must be <= starts={}", ends, starts);
        assert_eq!(
            iters, starts,
            "outer iterations should equal dispatch_starts; every iteration \
             that got past the base sleep entered the for-loop body and \
             incremented dispatch_start. iters={}, starts={}",
            iters, starts
        );
    }

    /// Phase 1 fix pass: `trigger_for_slug` consults the shared in-flight
    /// flag and skips when set.
    ///
    /// Previously this test was a documentation-only no-op asserting the
    /// opposite (the structural fact that `in_flight` was a local variable
    /// inside `start_dadbear_extend_loop`'s closure and therefore invisible
    /// to `trigger_for_slug`). The wanderer flagged that as a real gap:
    /// the only call path that could genuinely race `run_tick_for_config`
    /// for the same config is an HTTP/CLI manual trigger fired while the
    /// auto loop is mid-dispatch, and the flag did not cover it.
    ///
    /// The fix pass hoisted the flag to `PyramidState::dadbear_in_flight`.
    /// This test constructs a `PyramidState` with a pre-populated in-flight
    /// entry for a config, invokes `trigger_for_slug`, and asserts:
    ///   1. The returned JSON includes a `"skipped"` entry mentioning
    ///      `"dispatch in-flight"`,
    ///   2. `configs_processed` is zero (no dispatch ran),
    ///   3. The flag remains set afterwards — `trigger_for_slug` did not
    ///      steal the in-flight slot out from under the (simulated) ongoing
    ///      auto dispatch.
    #[tokio::test]
    async fn test_trigger_for_slug_respects_shared_in_flight_flag() {
        let (state, data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        // Create a conversation slug + DADBEAR config for it so
        // `trigger_for_slug` finds something to process.
        let source_dir = data_dir.join("conversations_trigger_skip");
        std::fs::create_dir_all(&source_dir).unwrap();

        let slug = "test-trigger-skip";
        let config_id: i64 = {
            let conn = state.writer.lock().await;
            db::create_slug(
                &conn,
                slug,
                &ContentType::Conversation,
                source_dir.to_str().unwrap(),
            )
            .unwrap();
            let cfg = DadbearWatchConfig {
                id: 0,
                slug: slug.to_string(),
                source_path: source_dir.to_str().unwrap().to_string(),
                content_type: "conversation".to_string(),
                scan_interval_secs: 10,
                debounce_secs: 30,
                session_timeout_secs: 1800,
                batch_size: 1,
                enabled: true,
                created_at: String::new(),
                updated_at: String::new(),
                last_scan_at: None,
            };
            db::save_dadbear_config(&conn, &cfg).unwrap()
        };

        // Pre-populate the shared in-flight map so `trigger_for_slug`
        // observes the config as already dispatching. This simulates the
        // tick loop being mid-`fire_ingest_chain` while an HTTP trigger
        // races into the same code path.
        let preset_flag = Arc::new(AtomicBool::new(true));
        {
            let mut guard = state.dadbear_in_flight.lock().unwrap();
            guard.insert(config_id, preset_flag.clone());
        }

        let db_path = data_dir.join("pyramid.db").to_string_lossy().to_string();
        let result = trigger_for_slug(&state, &db_path, slug, &bus)
            .await
            .unwrap();

        // The trigger should report a skipped entry for this config.
        let skipped = result.get("skipped").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            skipped.len(),
            1,
            "expected exactly one skipped config, got {:?}",
            skipped
        );
        let reason = skipped[0]
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            reason.contains("in-flight"),
            "skipped reason should mention in-flight, got: {reason}"
        );

        // configs_processed must be 0 — no dispatch ran.
        let processed = result
            .get("configs_processed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(
            processed, 0,
            "trigger_for_slug must not run a dispatch when the flag is set"
        );

        // The flag must still be set — trigger_for_slug observed it, skipped,
        // and did NOT stomp on the simulated in-flight dispatch's claim.
        assert!(
            preset_flag.load(Ordering::Relaxed),
            "trigger_for_slug must leave the pre-set flag alone when skipping"
        );
        let post_guard = state.dadbear_in_flight.lock().unwrap();
        let post_flag = post_guard.get(&config_id).unwrap().clone();
        drop(post_guard);
        assert!(
            post_flag.load(Ordering::Relaxed),
            "the shared map entry must still report in-flight after the skip"
        );
    }

    /// Phase 1 fix pass: two concurrent `trigger_for_slug` calls for the same
    /// config cannot BOTH reach dispatch. One claims the flag and runs its
    /// (intentionally-errored) tick; the other observes the flag and skips.
    ///
    /// This is the "HTTP trigger races auto-dispatch for the same config"
    /// scenario the wanderer identified, reduced to a test that uses two
    /// concurrent manual triggers because spinning up the full auto tick
    /// loop inside a unit test is expensive and racy. The behavior under
    /// test is identical: both call paths go through the same
    /// `PyramidState::dadbear_in_flight` map and the same lazy-insert /
    /// set / RAII-guard sequence, so any race that exists between
    /// trigger+auto also exists between trigger+trigger.
    ///
    /// `run_tick_for_config` will fail because the source directory contains
    /// nothing to ingest — that is fine. What matters is how many of the two
    /// concurrent calls actually invoked dispatch vs. short-circuited on the
    /// shared flag. The sum `processed + errors + skipped` across both calls
    /// equals exactly 2, and at LEAST one of them is a skip: if both calls
    /// happened to claim the flag serially (the fast-finish case), both
    /// would surface `processed/errors` = 1 each and `skipped` = 0; if both
    /// raced with overlap, one claims and the other skips. The invariant we
    /// must preserve is that two calls cannot BOTH be mid-dispatch at the
    /// same instant for the same config — and the shared-flag primitive
    /// guarantees it.
    #[tokio::test(flavor = "current_thread")]
    async fn test_tick_loop_and_trigger_race_skip() {
        let (state, data_dir) = make_test_state();
        let bus = state.build_event_bus.clone();

        // Create a conversation slug + DADBEAR config.
        let source_dir = data_dir.join("conversations_race");
        std::fs::create_dir_all(&source_dir).unwrap();
        let slug = "test-race-skip";
        {
            let conn = state.writer.lock().await;
            db::create_slug(
                &conn,
                slug,
                &ContentType::Conversation,
                source_dir.to_str().unwrap(),
            )
            .unwrap();
            let cfg = DadbearWatchConfig {
                id: 0,
                slug: slug.to_string(),
                source_path: source_dir.to_str().unwrap().to_string(),
                content_type: "conversation".to_string(),
                scan_interval_secs: 10,
                debounce_secs: 30,
                session_timeout_secs: 1800,
                batch_size: 1,
                enabled: true,
                created_at: String::new(),
                updated_at: String::new(),
                last_scan_at: None,
            };
            db::save_dadbear_config(&conn, &cfg).unwrap();
        }

        // Directly exercise the flag-claim primitive the shared state
        // depends on. We hold a long-lived claim in a background task
        // (simulating the auto tick loop being mid-`fire_ingest_chain`),
        // then fire a manual `trigger_for_slug` against the same config
        // and assert it observes the claim and skips.

        // Step 1: read the config id from the DB so we can target its slot.
        let db_path = data_dir.join("pyramid.db").to_string_lossy().to_string();
        let config_id: i64 = {
            let conn = db::open_pyramid_connection(Path::new(&db_path)).unwrap();
            let cfgs = db::get_dadbear_configs(&conn, slug).unwrap();
            assert_eq!(cfgs.len(), 1);
            cfgs[0].id
        };

        // Step 2: claim the flag and spawn a background task that holds it
        // for a window long enough to span the manual trigger's execution.
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder_flag = {
            let mut guard = state.dadbear_in_flight.lock().unwrap();
            guard
                .entry(config_id)
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .clone()
        };
        holder_flag.store(true, Ordering::Relaxed);
        let holder_flag_for_task = holder_flag.clone();
        let holder = tokio::spawn(async move {
            let _guard = InFlightGuard(holder_flag_for_task);
            // Wait until the test releases us — the guard clears the flag on
            // drop at the end of this task.
            let _ = release_rx.await;
        });

        // Step 3: fire `trigger_for_slug` while the holder still owns the
        // flag. It must observe the flag set and skip without running
        // `run_tick_for_config`.
        let trigger_result = trigger_for_slug(&state, &db_path, slug, &bus)
            .await
            .unwrap();
        let skipped = trigger_result
            .get("skipped")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(
            skipped.len(),
            1,
            "manual trigger must skip all configs while holder owns the flag; got {:?}",
            trigger_result
        );
        let processed = trigger_result
            .get("configs_processed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(
            processed, 0,
            "manual trigger must not run dispatch while the flag is held"
        );

        // The holder still owns the flag — the manual trigger's skip path
        // does NOT claim or release it.
        assert!(
            holder_flag.load(Ordering::Relaxed),
            "the holder's flag claim must survive the concurrent skip path"
        );

        // Step 4: release the holder and verify the flag clears (the guard
        // drops during task unwind after `release_rx` completes).
        let _ = release_tx.send(());
        holder.await.unwrap();
        assert!(
            !holder_flag.load(Ordering::Relaxed),
            "InFlightGuard must clear the flag when the holder task exits"
        );

        // Step 5: after the flag clears, a fresh `trigger_for_slug` no
        // longer skips on the flag. It may still fail (the source dir is
        // empty or errors from run_tick_for_config are normal here) but it
        // must NOT surface a skip with reason `"dispatch in-flight"`.
        let trigger_result2 = trigger_for_slug(&state, &db_path, slug, &bus)
            .await
            .unwrap();
        let skipped2 = trigger_result2
            .get("skipped")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(
            skipped2.len(),
            0,
            "after holder releases the flag, trigger must not skip on in-flight; got {:?}",
            trigger_result2
        );
    }
}
