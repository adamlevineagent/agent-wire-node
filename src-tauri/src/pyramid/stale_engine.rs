// pyramid/stale_engine.rs — Per-layer timer engine for stale detection
//
// Manages debounce timers per pyramid layer, drains mutations from the WAL,
// batches them using the rotator-arm algorithm, and dispatches helper tasks.
// Phase 4a: L0 helpers use real LLM calls via stale_helpers module.
// L1+ helpers (node_stale, edge_stale, connection_check) remain as placeholders
// until Phase 4b.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{info, warn, error};
use uuid::Uuid;

use super::stale_helpers;
use super::stale_helpers_upper;
use super::types::{
    AutoUpdateConfig, ConnectionCheckResult, PendingMutation, StaleCheckResult,
};

// cascade_depth is tracked for observability (cost observatory) but NOT enforced as a cap.
// The LLM naturally terminates cascades by answering "not stale" on unchanged content.
// The runaway breaker is the safety net for degenerate LLM behavior.
pub const MAX_CONCURRENT_HELPERS: usize = 3;

/// Cap constants for the rotator-arm batching algorithm.
const BATCH_CAP_NODES: usize = 5;
const BATCH_CAP_CONNECTIONS: usize = 20;
const BATCH_CAP_RENAMES: usize = 1;

/// Per-layer debounce timer state.
pub struct LayerTimer {
    pub slug: String,
    pub layer: i32,
    pub debounce: Duration,
    pub has_pending: bool,
    pub timer_handle: Option<JoinHandle<()>>,
}

/// Core stale-detection engine for a single pyramid.
///
/// Owns per-layer timers, drains the WAL, batches mutations, and dispatches
/// helper tasks. L0 helpers use real LLM calls (Phase 4a); L1+ are placeholders.
pub struct PyramidStaleEngine {
    pub slug: String,
    pub layers: HashMap<i32, LayerTimer>,
    pub config: AutoUpdateConfig,
    pub breaker_tripped: bool,
    pub frozen: bool,
    pub concurrent_helpers: Arc<Semaphore>,
    pub db_path: String,
    pub api_key: String,
    pub model: String,
}

impl PyramidStaleEngine {
    /// Create an engine with layer timers for L0, L1, L2, L3 (apex).
    pub fn new(slug: &str, config: AutoUpdateConfig, db_path: &str, api_key: &str, model: &str) -> Self {
        let debounce = Duration::from_secs((config.debounce_minutes as u64) * 60);
        let mut layers = HashMap::new();
        for layer in 0..=3 {
            layers.insert(
                layer,
                LayerTimer {
                    slug: slug.to_string(),
                    layer,
                    debounce,
                    has_pending: false,
                    timer_handle: None,
                },
            );
        }

        Self {
            slug: slug.to_string(),
            layers,
            breaker_tripped: config.breaker_tripped,
            frozen: config.frozen,
            config,
            concurrent_helpers: Arc::new(Semaphore::new(MAX_CONCURRENT_HELPERS)),
            db_path: db_path.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    /// Called when a mutation is written to WAL for this slug.
    /// Sets `has_pending = true` and restarts the debounce timer for the layer.
    pub fn notify_mutation(&mut self, layer: i32) {
        if self.breaker_tripped || self.frozen {
            info!(
                slug = %self.slug,
                layer,
                "Ignoring mutation notification: engine is {}",
                if self.breaker_tripped { "breaker-tripped" } else { "frozen" }
            );
            return;
        }

        if let Some(timer) = self.layers.get_mut(&layer) {
            timer.has_pending = true;
            self.start_timer(layer);
        } else {
            warn!(
                slug = %self.slug,
                layer,
                "notify_mutation called for unknown layer"
            );
        }
    }

    /// Spawns a tokio task that sleeps for the debounce duration, then
    /// calls `drain_and_dispatch`. Cancels any previous timer for the layer.
    pub fn start_timer(&mut self, layer: i32) {
        let timer = match self.layers.get_mut(&layer) {
            Some(t) => t,
            None => return,
        };

        // Cancel existing timer if running
        if let Some(handle) = timer.timer_handle.take() {
            handle.abort();
        }

        let slug = self.slug.clone();
        let debounce = timer.debounce;
        let db_path = self.db_path.clone();
        let semaphore = self.concurrent_helpers.clone();
        let min_changed_files = self.config.min_changed_files;
        let api_key = self.api_key.clone();
        let model = self.model.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(debounce).await;
            info!(slug = %slug, layer, "Debounce timer fired, draining WAL");

            if let Err(e) = drain_and_dispatch(
                &slug,
                layer,
                min_changed_files,
                &db_path,
                semaphore,
                &api_key,
                &model,
            )
            .await
            {
                error!(slug = %slug, layer, error = %e, "drain_and_dispatch failed");
            }
        });

        if let Some(timer) = self.layers.get_mut(&layer) {
            timer.timer_handle = Some(handle);
            timer.has_pending = false;
        }
    }

    // ── Breaker & Freeze ────────────────────────────────────────────────────

    /// Trip the circuit breaker. Cancels all timers and persists to DB.
    pub fn trip_breaker(&mut self) {
        warn!(slug = %self.slug, "Circuit breaker tripped!");
        self.breaker_tripped = true;

        for timer in self.layers.values_mut() {
            if let Some(handle) = timer.timer_handle.take() {
                handle.abort();
            }
        }

        if let Ok(conn) = Connection::open(&self.db_path) {
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let _ = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET breaker_tripped = 1, breaker_tripped_at = ?1
                 WHERE slug = ?2",
                rusqlite::params![now, self.slug],
            );
        }
    }

    /// Resume from breaker trip. Restarts timers for layers with pending mutations.
    pub fn resume_breaker(&mut self) {
        info!(slug = %self.slug, "Resuming from circuit breaker trip");
        self.breaker_tripped = false;

        if let Ok(conn) = Connection::open(&self.db_path) {
            let _ = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET breaker_tripped = 0, breaker_tripped_at = NULL
                 WHERE slug = ?1",
                rusqlite::params![self.slug],
            );

            for layer in 0..=3 {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM pyramid_pending_mutations
                         WHERE processed = 0 AND slug = ?1 AND layer = ?2",
                        rusqlite::params![self.slug, layer],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if count > 0 {
                    if let Some(timer) = self.layers.get_mut(&layer) {
                        timer.has_pending = true;
                    }
                    self.start_timer(layer);
                }
            }
        }
    }

    /// Freeze the engine: cancel timers, mark all WAL entries processed.
    pub fn freeze(&mut self) {
        info!(slug = %self.slug, "Freezing stale engine");
        self.frozen = true;

        for timer in self.layers.values_mut() {
            if let Some(handle) = timer.timer_handle.take() {
                handle.abort();
            }
            timer.has_pending = false;
        }

        if let Ok(conn) = Connection::open(&self.db_path) {
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let _ = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET frozen = 1, frozen_at = ?1
                 WHERE slug = ?2",
                rusqlite::params![now, self.slug],
            );
            let _ = conn.execute(
                "UPDATE pyramid_pending_mutations
                 SET processed = 1
                 WHERE processed = 0 AND slug = ?1",
                rusqlite::params![self.slug],
            );
        }
    }

    /// Unfreeze the engine. Hash rescan will be triggered by Phase 7 startup.
    pub fn unfreeze(&mut self) {
        info!(slug = %self.slug, "Unfreezing stale engine");
        self.frozen = false;

        if let Ok(conn) = Connection::open(&self.db_path) {
            let _ = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET frozen = 0, frozen_at = NULL
                 WHERE slug = ?1",
                rusqlite::params![self.slug],
            );
        }
    }
}

// ── Core Drain Logic (free functions for Send safety) ────────────────────────

/// Core drain function. Reads unprocessed mutations from WAL, batches them,
/// and dispatches helpers. This is a free async function (not a method) so it
/// can be called from spawned tasks without Send issues around `&Connection`.
async fn drain_and_dispatch(
    slug: &str,
    layer: i32,
    min_changed_files: i32,
    db_path: &str,
    semaphore: Arc<Semaphore>,
    api_key: &str,
    model: &str,
) -> Result<()> {
    let slug_owned = slug.to_string();
    let db_owned = db_path.to_string();
    let api_key_owned = api_key.to_string();
    let model_owned = model.to_string();

    // Check runaway threshold before processing — trip breaker if exceeded
    {
        let s = slug_owned.clone();
        let db = db_owned.clone();
        let runaway_tripped = tokio::task::spawn_blocking(move || -> bool {
            if let Ok(conn) = Connection::open(&db) {
                // Load config from DB
                let config = conn.query_row(
                    "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                            runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
                     FROM pyramid_auto_update_config WHERE slug = ?1",
                    rusqlite::params![s],
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
                ).ok();

                if let Some(config) = config {
                    if super::watcher::check_runaway(&conn, &s, &config) {
                        // Trip the breaker in the database
                        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                        let _ = conn.execute(
                            "UPDATE pyramid_auto_update_config SET breaker_tripped = 1, breaker_tripped_at = ?1 WHERE slug = ?2",
                            rusqlite::params![now, s],
                        );
                        return true;
                    }
                }
            }
            false
        }).await.unwrap_or(false);

        if runaway_tripped {
            warn!(slug = %slug_owned, layer, "Runaway threshold exceeded — circuit breaker tripped, aborting drain");
            return Ok(());
        }
    }

    let mutations = {
        let s = slug_owned.clone();
        let db = db_owned.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<PendingMutation>> {
            let conn = Connection::open(&db)
                .context("Failed to open DB for drain")?;

            if layer == 0 {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM pyramid_pending_mutations
                     WHERE processed = 0 AND slug = ?1 AND layer = 0
                     AND mutation_type NOT IN ('new_file', 'deleted_file')",
                    rusqlite::params![s],
                    |row| row.get(0),
                )?;

                if count < min_changed_files as i64 {
                    info!(
                        slug = %s,
                        count,
                        min = min_changed_files,
                        "L0 below min_changed_files threshold, re-arming timer"
                    );
                    return Ok(Vec::new());
                }
            }

            let batch_id = Uuid::new_v4().to_string();
            atomic_drain(&conn, &s, layer, &batch_id)
        })
        .await??
    };

    if mutations.is_empty() {
        info!(slug = %slug_owned, layer, "No pending mutations to drain (or below threshold)");
        return Ok(());
    }

    let batch_id = mutations
        .first()
        .and_then(|m| m.batch_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    info!(
        slug = %slug_owned,
        layer,
        count = mutations.len(),
        batch_id = %batch_id,
        "Drained mutations from WAL"
    );

    // (c) Group mutations by type
    let mut file_changes: Vec<PendingMutation> = Vec::new();
    let mut new_files: Vec<PendingMutation> = Vec::new();
    let mut deleted_files: Vec<PendingMutation> = Vec::new();
    let mut rename_candidates: Vec<PendingMutation> = Vec::new();
    let mut confirmed_stales: Vec<PendingMutation> = Vec::new();
    let mut edge_stales: Vec<PendingMutation> = Vec::new();
    let mut node_stales: Vec<PendingMutation> = Vec::new();

    for m in mutations {
        match m.mutation_type.as_str() {
            "file_change" => file_changes.push(m),
            "new_file" => new_files.push(m),
            "deleted_file" => deleted_files.push(m),
            "rename_candidate" => rename_candidates.push(m),
            "confirmed_stale" => confirmed_stales.push(m),
            "edge_stale" => edge_stales.push(m),
            "node_stale" => node_stales.push(m),
            other => {
                warn!(slug = %slug_owned, mutation_type = other, "Unknown mutation type, treating as node_stale");
                node_stales.push(m);
            }
        }
    }

    // (d) Batch using rotator-arm algorithm
    let file_batches = batch_items(file_changes, BATCH_CAP_NODES);
    let new_file_batches = batch_items(new_files, BATCH_CAP_NODES);
    let deleted_batches = batch_items(deleted_files, BATCH_CAP_NODES);
    let rename_batches = batch_items(rename_candidates, BATCH_CAP_RENAMES);
    let confirmed_batches = batch_items(confirmed_stales, BATCH_CAP_NODES);
    let edge_batches = batch_items(edge_stales, BATCH_CAP_CONNECTIONS);
    let node_batches = batch_items(node_stales, BATCH_CAP_NODES);

    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    // ── L0: File stale checks (real LLM via stale_helpers) ──────────────────
    for batch in file_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let key = api_key_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers::dispatch_file_stale_check(batch, &db, &key, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_file_stale_check failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = Connection::open(&db) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            }).await;
            drop(permit);
        }));
    }

    // ── L1+: Node stale checks (real LLM via stale_helpers_upper) ───────────
    for batch in node_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let key = api_key_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_node_stale_check(batch, &db, &key, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_node_stale_check (upper) failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = Connection::open(&db) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            }).await;
            drop(permit);
        }));
    }

    // ── L1+: Confirmed stale dispatch (real LLM via stale_helpers_upper) ────
    for batch in confirmed_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let key = api_key_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_node_stale_check(batch, &db, &key, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_node_stale_check (confirmed) failed");
                    Vec::new()
                }
            };
            // For confirmed stale results, execute supersession
            for result in &results {
                if result.stale {
                    if let Err(e) = stale_helpers_upper::execute_supersession(
                        &result.target_id, &db, &s, &key, &mdl,
                    ).await {
                        error!(slug = %s, target = %result.target_id, error = %e, "execute_supersession failed");
                    }
                }
            }
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = Connection::open(&db) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            }).await;
            drop(permit);
        }));
    }

    // ── Edge stale checks (real LLM via stale_helpers_upper) ────────────────
    for batch in edge_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let key = api_key_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_edge_stale_check(batch, &db, &key, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_edge_stale_check (upper) failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = Connection::open(&db) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            }).await;
            drop(permit);
        }));
    }

    // ── L0: New file ingestion (real via stale_helpers) ─────────────────────
    for batch in new_file_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = stale_helpers::dispatch_new_file_ingest(batch, &db).await {
                error!(slug = %s, error = %e, "dispatch_new_file_ingest failed");
            }
            drop(permit);
        }));
    }

    // ── L0: Deleted file tombstoning (real via stale_helpers) ───────────────
    for batch in deleted_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = stale_helpers::dispatch_tombstone(batch, &db).await {
                error!(slug = %s, error = %e, "dispatch_tombstone failed");
            }
            drop(permit);
        }));
    }

    // ── L0: Rename checks (real LLM via stale_helpers) ──────────────────────
    for batch in rename_batches {
        for mutation in batch {
            let permit = semaphore.clone().acquire_owned().await?;
            let db = db_owned.clone();
            let s = slug_owned.clone();
            let key = api_key_owned.clone();
            let mdl = model_owned.clone();
            handles.push(tokio::spawn(async move {
                match stale_helpers::dispatch_rename_check(mutation, &db, &key, &mdl).await {
                    Ok(result) => {
                        info!(
                            slug = %s,
                            rename = result.rename,
                            reason = %result.reason,
                            "Rename check completed"
                        );
                    }
                    Err(e) => {
                        error!(slug = %s, error = %e, "dispatch_rename_check failed");
                    }
                }
                drop(permit);
            }));
        }
    }

    for handle in handles {
        let _ = handle.await;
    }

    info!(
        slug = %slug_owned,
        layer,
        batch_id = %batch_id,
        "All helpers completed for batch"
    );

    Ok(())
}

/// Atomically drain unprocessed mutations from the WAL.
fn atomic_drain(
    conn: &Connection,
    slug: &str,
    layer: i32,
    batch_id: &str,
) -> Result<Vec<PendingMutation>> {
    let tx = conn.unchecked_transaction()?;

    let mutations = {
        let mut stmt = tx.prepare(
            "SELECT id, slug, layer, mutation_type, target_ref, detail,
                    cascade_depth, detected_at, processed, batch_id
             FROM pyramid_pending_mutations
             WHERE processed = 0 AND slug = ?1 AND layer = ?2
             ORDER BY id ASC",
        )?;

        let result: Vec<PendingMutation> = stmt.query_map(
            rusqlite::params![slug, layer],
            |row| {
                Ok(PendingMutation {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    layer: row.get(2)?,
                    mutation_type: row.get(3)?,
                    target_ref: row.get(4)?,
                    detail: row.get(5)?,
                    cascade_depth: row.get(6)?,
                    detected_at: row.get(7)?,
                    processed: row.get::<_, i32>(8)? != 0,
                    batch_id: row.get(9)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
        result
    };

    if !mutations.is_empty() {
        let update_sql = "UPDATE pyramid_pending_mutations SET processed = 1, batch_id = ?1 WHERE id = ?2";
        for m in &mutations {
            tx.execute(update_sql, rusqlite::params![batch_id, m.id])?;
        }
    }

    tx.commit()?;
    Ok(mutations)
}

// ── Rotator-Arm Batching ─────────────────────────────────────────────────────

pub fn batch_items<T>(items: Vec<T>, cap: usize) -> Vec<Vec<T>> {
    if items.is_empty() || cap == 0 {
        return Vec::new();
    }
    let num_batches = (items.len() + cap - 1) / cap;
    let mut batches: Vec<Vec<T>> = (0..num_batches).map(|_| Vec::new()).collect();
    for (i, item) in items.into_iter().enumerate() {
        batches[i % num_batches].push(item);
    }
    batches
}

// ── Result Processing Helpers ────────────────────────────────────────────────

fn log_stale_results(
    conn: &Connection,
    slug: &str,
    batch_id: &str,
    layer: i32,
    results: &[StaleCheckResult],
) -> Result<()> {
    for result in results {
        conn.execute(
            "INSERT INTO pyramid_stale_check_log
             (slug, batch_id, layer, target_id, stale, reason,
              checker_index, checker_batch_size, checked_at, cost_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                slug,
                batch_id,
                layer,
                result.target_id,
                result.stale as i32,
                result.reason,
                result.checker_index,
                result.checker_batch_size,
                result.checked_at,
                result.cost_tokens,
                result.cost_usd,
            ],
        )
        .context("Failed to insert stale check log")?;
    }
    Ok(())
}

fn propagate_confirmed_stales(
    conn: &Connection,
    slug: &str,
    layer: i32,
    results: &[StaleCheckResult],
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let next_layer = (layer + 1).min(3);

    for result in results {
        if !result.stale {
            continue;
        }

        info!(
            slug = %slug,
            target = %result.target_id,
            layer,
            next_layer,
            "Propagating confirmed stale to layer {}",
            next_layer
        );

        // Look up the original mutation's cascade_depth from the WAL by target_ref and batch_id
        let original_depth: i32 = conn
            .query_row(
                "SELECT cascade_depth FROM pyramid_pending_mutations
                 WHERE slug = ?1 AND target_ref = ?2 AND batch_id = ?3
                 ORDER BY id DESC LIMIT 1",
                rusqlite::params![slug, result.target_id, result.batch_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let new_depth = original_depth + 1;

        conn.execute(
            "INSERT INTO pyramid_pending_mutations
             (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
             VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, ?5, ?6, 0)",
            rusqlite::params![
                slug,
                next_layer,
                result.target_id,
                result.reason,
                new_depth,
                now,
            ],
        )
        .context("Failed to propagate confirmed stale mutation")?;
    }

    Ok(())
}

// ── Placeholder Dispatch Functions (Phase 4b will replace these) ──────────────

#[allow(dead_code)]
async fn dispatch_node_stale_check(batch: Vec<PendingMutation>) -> Vec<StaleCheckResult> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "PLACEHOLDER: dispatch_node_stale_check (Phase 4b)"
    );

    batch
        .iter()
        .enumerate()
        .map(|(i, m)| StaleCheckResult {
            id: 0,
            slug: m.slug.clone(),
            batch_id: m.batch_id.clone().unwrap_or_default(),
            layer: m.layer,
            target_id: m.target_ref.clone(),
            stale: false,
            reason: "Phase 4b placeholder — no L1+ LLM check performed yet".to_string(),
            checker_index: i as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: None,
            cost_usd: None,
        })
        .collect()
}

#[allow(dead_code)]
async fn dispatch_connection_check(
    batch: Vec<PendingMutation>,
) -> Vec<ConnectionCheckResult> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "PLACEHOLDER: dispatch_connection_check (Phase 4b)"
    );

    batch
        .iter()
        .map(|m| ConnectionCheckResult {
            id: 0,
            slug: m.slug.clone(),
            supersession_node_id: m.target_ref.clone(),
            new_node_id: String::new(),
            connection_type: "placeholder".to_string(),
            connection_id: String::new(),
            still_valid: true,
            reason: "Phase 4b placeholder — no LLM check performed yet".to_string(),
            checked_at: now.clone(),
        })
        .collect()
}

#[allow(dead_code)]
async fn dispatch_edge_stale_check(batch: Vec<PendingMutation>) -> Vec<StaleCheckResult> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "PLACEHOLDER: dispatch_edge_stale_check (Phase 4b)"
    );

    batch
        .iter()
        .enumerate()
        .map(|(i, m)| StaleCheckResult {
            id: 0,
            slug: m.slug.clone(),
            batch_id: m.batch_id.clone().unwrap_or_default(),
            layer: m.layer,
            target_id: m.target_ref.clone(),
            stale: false,
            reason: "Phase 4b placeholder — no edge LLM check performed yet".to_string(),
            checker_index: i as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: None,
            cost_usd: None,
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_items_even() {
        let items: Vec<i32> = (1..=12).collect();
        let batches = batch_items(items, 5);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches[1].len(), 4);
        assert_eq!(batches[2].len(), 4);
    }

    #[test]
    fn test_batch_items_uneven() {
        let items: Vec<i32> = (1..=13).collect();
        let batches = batch_items(items, 5);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 5);
        assert_eq!(batches[1].len(), 4);
        assert_eq!(batches[2].len(), 4);
    }

    #[test]
    fn test_batch_items_empty() {
        let items: Vec<i32> = vec![];
        let batches = batch_items(items, 5);
        assert!(batches.is_empty());
    }

    #[test]
    fn test_batch_items_single() {
        let items = vec![42];
        let batches = batch_items(items, 5);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec![42]);
    }

    #[test]
    fn test_batch_items_exact_cap() {
        let items: Vec<i32> = (1..=5).collect();
        let batches = batch_items(items, 5);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 5);
    }

    #[test]
    fn test_engine_new() {
        let config = AutoUpdateConfig {
            slug: "test".to_string(),
            auto_update: true,
            debounce_minutes: 5,
            min_changed_files: 3,
            runaway_threshold: 0.5,
            breaker_tripped: false,
            breaker_tripped_at: None,
            frozen: false,
            frozen_at: None,
        };
        let engine = PyramidStaleEngine::new("test", config, "/tmp/test.db", "", "inception/mercury-2");
        assert_eq!(engine.slug, "test");
        assert_eq!(engine.layers.len(), 4);
        assert!(!engine.breaker_tripped);
        assert!(!engine.frozen);
        assert_eq!(
            engine.layers[&0].debounce,
            Duration::from_secs(5 * 60)
        );
    }
}
