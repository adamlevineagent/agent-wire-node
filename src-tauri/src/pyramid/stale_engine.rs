// pyramid/stale_engine.rs — Per-layer timer engine for stale detection
//
// Manages debounce timers per pyramid layer, drains mutations from the WAL,
// batches them using the rotator-arm algorithm, and dispatches helper tasks.
// Phase 4a: L0 helpers use real LLM calls via stale_helpers module.
// L1+ helpers (node_stale, edge_stale, connection_check) remain as placeholders
// until Phase 4b.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::faq;
use super::llm::LlmConfig;
use super::stale_helpers;
use super::stale_helpers_upper;
use super::types::{AutoUpdateConfig, PendingMutation, StaleCheckResult};

// cascade_depth is tracked for observability (cost observatory) but NOT enforced as a cap.
// The LLM naturally terminates cascades by answering "not stale" on unchanged content.
// The runaway breaker is the safety net for degenerate LLM behavior.

use super::{OperationalConfig, Tier1Config, Tier3Config};

pub fn max_concurrent_helpers() -> usize {
    Tier1Config::default().stale_max_concurrent_helpers
}

/// Query the actual maximum depth of the pyramid from the database.
/// Falls back to 3 (the default for a 4-layer pyramid: L0..L3) if the
/// query fails or the pyramid has no nodes yet.
fn query_max_depth(db_path: &str, slug: &str) -> i32 {
    match super::db::open_pyramid_connection(Path::new(db_path)) {
        Ok(conn) => conn
            .query_row(
                "SELECT COALESCE(MAX(depth), 3) FROM pyramid_nodes WHERE slug = ?1",
                rusqlite::params![slug],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_or(3),
        Err(_) => 3,
    }
}

fn batch_cap_nodes(t3: &Tier3Config) -> usize {
    t3.batch_cap_nodes
}
fn batch_cap_connections(t3: &Tier3Config) -> usize {
    t3.batch_cap_connections
}
fn batch_cap_renames(t3: &Tier3Config) -> usize {
    t3.batch_cap_renames
}

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
    /// Phase 3 fix pass: live LlmConfig (with provider_registry +
    /// credential_store) cloned at engine construction. Replaces the prior
    /// `api_key: String` field which dropped both runtime handles and forced
    /// every helper into the legacy fallback path. The model is read from
    /// `base_config.primary_model` so per-tier routing can override at the
    /// helper layer in a future refactor; today the engine still passes a
    /// single primary model through to dispatch helpers.
    pub base_config: LlmConfig,
    pub model: String,
    poll_handle: Option<tokio::task::JoinHandle<()>>,
    /// Current lifecycle phase: "idle", "debounce", "evaluating", "cascading", "done_stale", "done_clean"
    pub current_phase: Arc<std::sync::Mutex<String>>,
    /// Detail text for the current phase, e.g. "batch 2 of 3 at L0"
    pub phase_detail: Arc<std::sync::Mutex<String>>,
    /// ISO timestamp when the debounce timer will fire (None when not in debounce)
    pub timer_fires_at: Arc<std::sync::Mutex<Option<String>>>,
    /// Summary of the last completed run, e.g. "updated 3 understandings"
    pub last_result_summary: Arc<std::sync::Mutex<Option<String>>>,
    /// Runtime operational config (WS5 fix: wired instead of using defaults)
    pub ops: Arc<OperationalConfig>,
}

impl PyramidStaleEngine {
    /// Create an engine with layer timers for L0, L1, L2, L3 (apex).
    ///
    /// Phase 3 fix pass: takes a live `LlmConfig` (with provider_registry +
    /// credential_store) instead of raw `api_key`. The caller is expected
    /// to clone `pyramid_state.config.read().await.clone()` (which is built
    /// via `PyramidConfig::to_llm_config_with_runtime` at boot). The
    /// `model` parameter still exists separately for callers that want to
    /// pin a different model than `base_config.primary_model`; today both
    /// boot paths just pass `base_config.primary_model.clone()`.
    pub fn new(
        slug: &str,
        config: AutoUpdateConfig,
        db_path: &str,
        base_config: LlmConfig,
        model: &str,
        ops: OperationalConfig,
    ) -> Self {
        let debounce = Duration::from_secs((config.debounce_minutes as u64) * 60);
        let mut layers = HashMap::new();
        let max_depth = query_max_depth(db_path, slug);
        for layer in 0..=max_depth {
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
            concurrent_helpers: Arc::new(Semaphore::new(max_concurrent_helpers())),
            db_path: db_path.to_string(),
            base_config,
            model: model.to_string(),
            poll_handle: None,
            current_phase: Arc::new(std::sync::Mutex::new("idle".to_string())),
            phase_detail: Arc::new(std::sync::Mutex::new(String::new())),
            timer_fires_at: Arc::new(std::sync::Mutex::new(None)),
            last_result_summary: Arc::new(std::sync::Mutex::new(None)),
            ops: Arc::new(ops),
        }
    }

    /// Start a background poll loop that checks the WAL every 60 seconds.
    /// This is the belt-and-suspenders fallback: even if the watcher can't
    /// signal the engine directly, pending mutations will be picked up.
    pub fn start_poll_loop(&mut self) {
        // Cancel any existing poll loop
        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
        }

        let slug = self.slug.clone();
        let db_path = self.db_path.clone();
        let _debounce = self
            .layers
            .get(&0)
            .map(|t| t.debounce)
            .expect("Layer 0 timer must exist — engine was constructed without layers");
        let semaphore = self.concurrent_helpers.clone();
        let min_changed_files = self.config.min_changed_files;
        // Phase 3 fix pass: clone the live LlmConfig (with provider_registry +
        // credential_store) so the spawned poll loop keeps the registry path
        // active for every dispatched helper. Replaces the prior raw api_key
        // string clone which dropped both runtime handles.
        let base_config = self.base_config.clone();
        let model = self.model.clone();
        let phase_arc = self.current_phase.clone();
        let detail_arc = self.phase_detail.clone();
        let summary_arc = self.last_result_summary.clone();
        let timer_fires_arc = self.timer_fires_at.clone();
        let ops_arc = self.ops.clone();

        let handle = tokio::spawn(async move {
            loop {
                // TODO(config): WAL poll interval (60s) should move to config once a
                // wal_poll_interval_secs field is added to Tier2Config / Tier3Config.
                // Currently structural: belt-and-suspenders fallback for mutation pickup.
                tokio::time::sleep(Duration::from_secs(60)).await;

                // Check each layer for unprocessed mutations
                let pending_by_layer: Vec<(i32, i64)> = match tokio::task::spawn_blocking({
                    let db_path = db_path.clone();
                    let slug = slug.clone();
                    move || {
                        let conn = match super::db::open_pyramid_connection(Path::new(&db_path)) {
                            Ok(c) => c,
                            Err(_) => return vec![],
                        };
                        let max_depth = query_max_depth(&db_path, &slug);
                        let mut results = vec![];
                        for layer in 0..=max_depth {
                            let count: i64 = conn
                                .query_row(
                                    "SELECT COUNT(*) FROM pyramid_pending_mutations
                                     WHERE processed = 0 AND slug = ?1 AND layer = ?2",
                                    rusqlite::params![slug, layer],
                                    |row| row.get(0),
                                )
                                .unwrap_or(0);
                            if count > 0 {
                                results.push((layer, count));
                            }
                        }
                        results
                    }
                })
                .await
                {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // For each layer with pending mutations, fire drain_and_dispatch directly
                for (layer, count) in pending_by_layer {
                    info!(
                        slug = %slug,
                        layer,
                        count,
                        "Poll loop found pending mutations, dispatching"
                    );

                    // Wait for the configured debounce before dispatching
                    // (the mutations may still be accumulating).
                    let debounce_secs = _debounce.as_secs();
                    {
                        let mut phase = phase_arc.lock().unwrap();
                        *phase = "debounce".to_string();
                    }
                    {
                        let mut tfa = timer_fires_arc.lock().unwrap();
                        *tfa = Some(
                            (Utc::now() + chrono::Duration::seconds(debounce_secs as i64))
                                .to_rfc3339(),
                        );
                    }

                    tokio::time::sleep(_debounce).await;

                    {
                        let mut tfa = timer_fires_arc.lock().unwrap();
                        *tfa = None;
                    }

                    if let Err(e) = drain_and_dispatch(
                        &slug,
                        layer,
                        min_changed_files,
                        &db_path,
                        semaphore.clone(),
                        &base_config,
                        &model,
                        phase_arc.clone(),
                        detail_arc.clone(),
                        summary_arc.clone(),
                        &ops_arc,
                    )
                    .await
                    {
                        error!(slug = %slug, layer, error = %e, "Poll-triggered drain failed");
                    }
                }

                // ── Phase 12: deferred question scanner ────────────────
                //
                // Once per tick, scan `pyramid_deferred_questions` for
                // rows whose `next_check_at <= now` AND whose
                // `check_interval` is not "never"/"on_demand". For each
                // expired row, re-run the triage DSL against the
                // active policy. Outcomes:
                //   Answer → remove_deferred (next build picks it up)
                //   Defer  → update_deferred_next_check with new interval
                //   Skip   → remove_deferred
                {
                    let db = db_path.clone();
                    let s = slug.clone();
                    let _ = tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                        let conn = super::db::open_pyramid_connection(Path::new(&db))?;
                        let expired = super::db::list_expired_deferred(&conn, &s)?;
                        if expired.is_empty() {
                            return Ok(());
                        }
                        let policy = super::db::load_active_evidence_policy(&conn, Some(&s))?;
                        info!(
                            slug = %s,
                            count = expired.len(),
                            "Phase 12: deferred question scanner processing expired rows"
                        );
                        for row in expired {
                            let question: super::types::LayerQuestion =
                                match serde_json::from_str(&row.question_json) {
                                    Ok(q) => q,
                                    Err(_) => continue,
                                };
                            let has_demand_signals = policy.demand_signals.iter().any(|rule| {
                                // Short-form "Nd" normalization done inline
                                let w = rule.window.trim();
                                let window = if w.starts_with('-') || w.contains(' ') {
                                    w.to_string()
                                } else {
                                    let (num_part, unit_part): (String, String) = w
                                        .chars()
                                        .partition(|c| c.is_ascii_digit());
                                    let n: i64 = num_part.parse().unwrap_or(14);
                                    let (n, unit) = match unit_part.as_str() {
                                        "d" => (n, "days"),
                                        "h" => (n, "hours"),
                                        "w" => (n * 7, "days"),
                                        "m" => (n, "minutes"),
                                        _ => (n, "days"),
                                    };
                                    format!("-{} {}", n, unit)
                                };
                                super::db::sum_demand_weight(
                                    &conn,
                                    &row.slug,
                                    &question.question_id,
                                    &rule.r#type,
                                    &window,
                                )
                                .unwrap_or(0.0)
                                    >= rule.threshold
                            });
                            let facts = super::triage::TriageFacts {
                                question: &question,
                                target_node_distilled: None,
                                target_node_depth: Some(question.layer),
                                is_first_build: false,
                                is_stale_check: true,
                                has_demand_signals,
                                evidence_question_trivial: None,
                                evidence_question_high_value: None,
                            };
                            match super::triage::resolve_decision(&policy, &facts) {
                                Ok(super::triage::TriageDecision::Answer { .. }) => {
                                    let _ = super::db::remove_deferred(
                                        &conn,
                                        &row.slug,
                                        &question.question_id,
                                    );
                                }
                                Ok(super::triage::TriageDecision::Defer {
                                    check_interval,
                                    ..
                                }) => {
                                    let _ = super::db::update_deferred_next_check(
                                        &conn,
                                        &row.slug,
                                        &question.question_id,
                                        &check_interval,
                                        policy.contribution_id.as_deref(),
                                    );
                                }
                                Ok(super::triage::TriageDecision::Skip { .. }) => {
                                    let _ = super::db::remove_deferred(
                                        &conn,
                                        &row.slug,
                                        &question.question_id,
                                    );
                                }
                                Err(_) => {}
                            }
                        }
                        Ok(())
                    })
                    .await;
                }

                // Check if breaker was tripped during dispatch (M1 fix)
                let breaker_tripped_in_db = {
                    let db = db_path.clone();
                    let s = slug.clone();
                    tokio::task::spawn_blocking(move || -> bool {
                        if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                            conn.query_row(
                                "SELECT breaker_tripped FROM pyramid_auto_update_config WHERE slug = ?1",
                                rusqlite::params![s],
                                |row| row.get::<_, i32>(0),
                            ).unwrap_or(0) != 0
                        } else {
                            false
                        }
                    }).await.unwrap_or(false)
                };
                if breaker_tripped_in_db {
                    warn!(slug = %slug, "Breaker tripped in DB — poll loop exiting");
                    break;
                }
            }
        });

        self.poll_handle = Some(handle);
        info!(slug = %self.slug, "WAL poll loop started (60s interval)");
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

            // Update phase tracking
            {
                let mut phase = self.current_phase.lock().unwrap();
                *phase = "debounce".to_string();
            }
            {
                let fires_at = Utc::now()
                    + chrono::Duration::from_std(timer.debounce)
                        .expect("Debounce duration must be convertible to chrono::Duration — config is invalid");
                let mut tfa = self.timer_fires_at.lock().unwrap();
                *tfa = Some(fires_at.to_rfc3339());
            }

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
        // Phase 3 fix pass: clone the live LlmConfig so the spawned timer
        // task keeps the provider_registry + credential_store handles.
        let base_config = self.base_config.clone();
        let model = self.model.clone();
        let phase_arc = self.current_phase.clone();
        let detail_arc = self.phase_detail.clone();
        let tfa_arc = self.timer_fires_at.clone();
        let summary_arc = self.last_result_summary.clone();
        let ops_arc = self.ops.clone();

        // Update timer_fires_at
        {
            let fires_at = Utc::now()
                + chrono::Duration::from_std(debounce)
                    .expect("Debounce duration must be convertible to chrono::Duration — config is invalid");
            let mut tfa = tfa_arc.lock().unwrap();
            *tfa = Some(fires_at.to_rfc3339());
        }

        let handle = tokio::spawn(async move {
            tokio::time::sleep(debounce).await;
            info!(slug = %slug, layer, "Debounce timer fired, draining WAL");

            // Clear timer_fires_at since debounce has expired
            {
                let mut tfa = tfa_arc.lock().unwrap();
                *tfa = None;
            }

            if let Err(e) = drain_and_dispatch(
                &slug,
                layer,
                min_changed_files,
                &db_path,
                semaphore,
                &base_config,
                &model,
                phase_arc,
                detail_arc,
                summary_arc,
                &ops_arc,
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

        if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&self.db_path)) {
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            if let Err(e) = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET breaker_tripped = 1, breaker_tripped_at = ?1
                 WHERE slug = ?2",
                rusqlite::params![now, self.slug],
            ) {
                warn!(slug = %self.slug, "Failed to persist circuit breaker trip to DB: {e}");
            }
        }
    }

    /// Resume from breaker trip. Restarts timers for layers with pending mutations.
    pub fn resume_breaker(&mut self) {
        info!(slug = %self.slug, "Resuming from circuit breaker trip");
        self.breaker_tripped = false;

        if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&self.db_path)) {
            if let Err(e) = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET breaker_tripped = 0, breaker_tripped_at = NULL
                 WHERE slug = ?1",
                rusqlite::params![self.slug],
            ) {
                warn!(slug = %self.slug, "Failed to persist circuit breaker reset to DB: {e}");
            }

            let max_depth = query_max_depth(&self.db_path, &self.slug);
            for layer in 0..=max_depth {
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

    /// Run a specific layer immediately, skipping the debounce timer.
    /// Used by the "Run Now" button to flush pending mutations on demand.
    pub async fn run_layer_now(&self, layer: i32) {
        let slug = self.slug.clone();
        let db_path = self.db_path.clone();
        // Phase 3 fix pass: clone the live LlmConfig instead of api_key so
        // the manual run keeps the registry path active.
        let base_config = self.base_config.clone();
        let model = self.model.clone();
        let semaphore = self.concurrent_helpers.clone();

        // min_changed_files = 0 to force run regardless of threshold
        let _ = drain_and_dispatch(
            &slug,
            layer,
            0,
            &db_path,
            semaphore,
            &base_config,
            &model,
            self.current_phase.clone(),
            self.phase_detail.clone(),
            self.last_result_summary.clone(),
            &self.ops,
        )
        .await;
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

        if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&self.db_path)) {
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            if let Err(e) = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET frozen = 1, frozen_at = ?1
                 WHERE slug = ?2",
                rusqlite::params![now, self.slug],
            ) {
                warn!(slug = %self.slug, "Failed to persist frozen state to DB: {e}");
            }
            if let Err(e) = conn.execute(
                "UPDATE pyramid_pending_mutations
                 SET processed = 1
                 WHERE processed = 0 AND slug = ?1",
                rusqlite::params![self.slug],
            ) {
                warn!(slug = %self.slug, "Failed to mark pending mutations as processed on freeze: {e}");
            }
        }
    }

    /// Abort the poll loop task to prevent orphan background tasks when replacing an engine.
    pub fn abort_poll_loop(&mut self) {
        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
        }
    }

    /// Unfreeze the engine. Hash rescan will be triggered by Phase 7 startup.
    pub fn unfreeze(&mut self) {
        info!(slug = %self.slug, "Unfreezing stale engine");
        self.frozen = false;

        if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&self.db_path)) {
            if let Err(e) = conn.execute(
                "UPDATE pyramid_auto_update_config
                 SET frozen = 0, frozen_at = NULL
                 WHERE slug = ?1",
                rusqlite::params![self.slug],
            ) {
                warn!(slug = %self.slug, "Failed to persist unfrozen state to DB: {e}");
            }
        }
    }
}

// ── Core Drain Logic (free functions for Send safety) ────────────────────────

/// Core drain function. Reads unprocessed mutations from WAL, batches them,
/// and dispatches helpers. This is a free async function (not a method) so it
/// can be called from spawned tasks without Send issues around `&Connection`.
///
/// Phase 3 fix pass: takes `base_config: &LlmConfig` (with provider_registry +
/// credential_store) instead of `api_key: &str` so every dispatched helper
/// stays on the registry path. The function clones the config once into
/// `base_config_owned` and then re-clones per spawned task.
pub async fn drain_and_dispatch(
    slug: &str,
    layer: i32,
    min_changed_files: i32,
    db_path: &str,
    semaphore: Arc<Semaphore>,
    base_config: &LlmConfig,
    model: &str,
    phase_arc: Arc<std::sync::Mutex<String>>,
    detail_arc: Arc<std::sync::Mutex<String>>,
    summary_arc: Arc<std::sync::Mutex<Option<String>>>,
    ops: &OperationalConfig,
) -> Result<()> {
    let slug_owned = slug.to_string();
    let db_owned = db_path.to_string();
    let base_config_owned = base_config.clone();
    let model_owned = model.to_string();

    // Set phase to evaluating at entry; record start time for minimum display duration
    let phase_started_at = std::time::Instant::now();
    {
        let mut phase = phase_arc.lock().unwrap();
        *phase = if layer > 0 {
            "cascading".to_string()
        } else {
            "evaluating".to_string()
        };
    }
    {
        let mut detail = detail_arc.lock().unwrap();
        *detail = if layer > 0 {
            format!("climbing to L{}", layer)
        } else {
            format!("L{}: draining WAL", layer)
        };
    }

    // Check runaway threshold before processing — trip breaker if exceeded
    {
        let s = slug_owned.clone();
        let db = db_owned.clone();
        let runaway_tripped = tokio::task::spawn_blocking(move || -> bool {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
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
                        if let Err(e) = conn.execute(
                            "UPDATE pyramid_auto_update_config SET breaker_tripped = 1, breaker_tripped_at = ?1 WHERE slug = ?2",
                            rusqlite::params![now, s],
                        ) {
                            warn!(slug = %s, "Failed to persist runaway-detected breaker trip to DB: {e}");
                        }
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
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for drain")?;

            if layer == 0 {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM pyramid_pending_mutations
                     WHERE processed = 0 AND slug = ?1 AND layer = 0",
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
        // Set phase to done_clean briefly, then revert to idle after configured display duration
        {
            let mut phase = phase_arc.lock().unwrap();
            *phase = "done_clean".to_string();
        }
        {
            let mut summary = summary_arc.lock().unwrap();
            *summary = Some("found nothing actionable".to_string());
        }
        let pa = phase_arc.clone();
        let display_secs = ops.tier2.phase_display_duration_secs;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(display_secs)).await;
            let mut phase = pa.lock().unwrap();
            if *phase == "done_clean" || *phase == "done_stale" {
                *phase = "idle".to_string();
            }
        });
        return Ok(());
    }

    // Update detail with mutation count
    {
        let mut detail = detail_arc.lock().unwrap();
        *detail = format!("L{}: {} files", layer, mutations.len());
    }

    // Dedup by (target_ref, mutation_type) — keep latest (highest id) to avoid
    // double-firing when the file watcher writes duplicate WAL entries.
    let mutations = {
        let pre_dedup = mutations.len();
        let mut seen: HashMap<(String, String), usize> = HashMap::new();
        for (i, m) in mutations.iter().enumerate() {
            let key = (m.target_ref.clone(), m.mutation_type.clone());
            seen.insert(key, i); // last one wins (highest id due to ORDER BY id ASC)
        }
        let mut deduped_indices: Vec<usize> = seen.into_values().collect();
        deduped_indices.sort();
        let deduped: Vec<PendingMutation> = deduped_indices
            .into_iter()
            .map(|i| mutations[i].clone())
            .collect();
        if deduped.len() < pre_dedup {
            info!(
                slug = %slug_owned,
                layer,
                before = pre_dedup,
                after = deduped.len(),
                "Deduplicated WAL mutations by (target_ref, mutation_type)"
            );
        }
        deduped
    };

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
    let mut faq_category_stales: Vec<PendingMutation> = Vec::new();
    let mut evidence_set_mutations: Vec<PendingMutation> = Vec::new();
    let mut targeted_l0_stales: Vec<PendingMutation> = Vec::new();

    for m in mutations {
        match m.mutation_type.as_str() {
            "file_change" => file_changes.push(m),
            "new_file" => new_files.push(m),
            "deleted_file" => deleted_files.push(m),
            "rename_candidate" => rename_candidates.push(m),
            "confirmed_stale" => confirmed_stales.push(m),
            "edge_stale" => edge_stales.push(m),
            "node_stale" => node_stales.push(m),
            "faq_category_stale" => faq_category_stales.push(m),
            "evidence_set_growth" => evidence_set_mutations.push(m),
            "targeted_l0_stale" => targeted_l0_stales.push(m),
            other => {
                warn!(slug = %slug_owned, mutation_type = other, "Unknown mutation type, treating as node_stale");
                node_stales.push(m);
            }
        }
    }

    // (d) Batch using rotator-arm algorithm
    let file_batches = batch_items(file_changes, batch_cap_nodes(&ops.tier3));
    let new_file_batches = batch_items(new_files, batch_cap_nodes(&ops.tier3));
    let deleted_batches = batch_items(deleted_files, batch_cap_nodes(&ops.tier3));
    let rename_batches = batch_items(rename_candidates, batch_cap_renames(&ops.tier3));
    let confirmed_batches = batch_items(confirmed_stales, batch_cap_nodes(&ops.tier3));
    let edge_batches = batch_items(edge_stales, batch_cap_connections(&ops.tier3));
    let node_batches = batch_items(node_stales, batch_cap_nodes(&ops.tier3));
    let es_batches = batch_items(evidence_set_mutations, batch_cap_nodes(&ops.tier3));
    let tl0_batches = batch_items(targeted_l0_stales, batch_cap_nodes(&ops.tier3));

    let total_batches = file_batches.len()
        + new_file_batches.len()
        + deleted_batches.len()
        + rename_batches.len()
        + confirmed_batches.len()
        + edge_batches.len()
        + node_batches.len()
        + es_batches.len()
        + tl0_batches.len();
    {
        let mut detail = detail_arc.lock().unwrap();
        *detail = format!("L{}: {} batches to process", layer, total_batches);
    }

    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    // ── L0: File stale checks (real LLM via stale_helpers) ──────────────────
    for batch in file_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers::dispatch_file_stale_check(batch, &db, &cfg, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_file_stale_check failed");
                    Vec::new()
                }
            };
            // For stale L0 results, resolve file path to node IDs and execute
            // supersession so nodes get updated before propagating to L1.
            let mut results = results;
            for result in &mut results {
                if result.stale == 1 {
                    // result.target_id is a file path for L0 file_change mutations.
                    // Resolve it to node IDs via pyramid_file_hashes.
                    let db_resolve = db.clone();
                    let s_resolve = s.clone();
                    let target = result.target_id.clone();
                    let node_ids: Vec<String> = tokio::task::spawn_blocking(move || {
                        let conn = match super::db::open_pyramid_connection(Path::new(&db_resolve)) {
                            Ok(c) => c,
                            Err(_) => return Vec::new(),
                        };
                        let node_ids_json: String = conn.query_row(
                            "SELECT node_ids FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                            rusqlite::params![s_resolve, target],
                            |row| row.get(0),
                        ).unwrap_or_else(|_| "[]".to_string());
                        serde_json::from_str(&node_ids_json).unwrap_or_default()
                    }).await.unwrap_or_default();

                    if node_ids.is_empty() {
                        warn!(slug = %s, target = %result.target_id, "No node IDs found for file path — skipping supersession");
                        continue;
                    }

                    for node_id in &node_ids {
                        if let Err(e) = stale_helpers_upper::execute_supersession(
                            node_id, &db, &s, &cfg, &mdl,
                        ).await {
                            error!(slug = %s, target = %result.target_id, node_id = %node_id, error = %e, "execute_supersession (L0 file_change) failed");
                        } else {
                            result.reason = format!("{} (node {} superseded)", result.reason, node_id);
                        }
                    }
                }
            }
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);

                    // Propagate to targeted L0 nodes from the same source files
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    for result in &results {
                        if result.stale != 1 || layer != 0 {
                            continue;
                        }
                        // Resolve file path → canonical node IDs
                        let node_ids_json: String = conn
                            .query_row(
                                "SELECT node_ids FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                                rusqlite::params![s, result.target_id],
                                |row| row.get(0),
                            )
                            .unwrap_or_else(|_| "[]".to_string());
                        let canonical_ids: Vec<String> =
                            serde_json::from_str(&node_ids_json).unwrap_or_default();

                        if canonical_ids.is_empty() {
                            continue;
                        }

                        match super::db::get_targeted_l0_for_canonical_nodes(&conn, &s, &canonical_ids) {
                            Ok(targeted_ids) => {
                                for tid in &targeted_ids {
                                    if let Err(e) = conn.execute(
                                        "INSERT INTO pyramid_pending_mutations
                                         (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                                         VALUES (?1, 0, 'targeted_l0_stale', ?2, ?3, ?4, ?5, 0)",
                                        rusqlite::params![
                                            s,
                                            tid,
                                            format!("Canonical L0 stale: {}", result.reason),
                                            result.cascade_depth + 1,
                                            now,
                                        ],
                                    ) {
                                        warn!(slug = %s, target = %tid, "Failed to insert targeted_l0_stale pending mutation: {e}");
                                    }
                                }
                                if !targeted_ids.is_empty() {
                                    info!(
                                        slug = %s,
                                        file = %result.target_id,
                                        count = targeted_ids.len(),
                                        "Propagated staleness to targeted L0 nodes"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!(
                                    slug = %s,
                                    file = %result.target_id,
                                    error = %e,
                                    "Failed to find targeted L0 nodes for canonical stale propagation"
                                );
                            }
                        }
                    }
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
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_node_stale_check(
                batch, &db, &cfg, &mdl,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_node_stale_check (upper) failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            })
            .await;
            drop(permit);
        }));
    }

    // ── L1+: Confirmed stale dispatch (real LLM via stale_helpers_upper) ────
    for batch in confirmed_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_node_stale_check(batch, &db, &cfg, &mdl).await {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_node_stale_check (confirmed) failed");
                    Vec::new()
                }
            };
            // For confirmed stale results, execute supersession
            let mut results = results;
            for result in &mut results {
                if result.stale == 1 {
                    if let Err(e) = stale_helpers_upper::execute_supersession(
                        &result.target_id, &db, &s, &cfg, &mdl,
                    ).await {
                        error!(slug = %s, target = %result.target_id, error = %e, "execute_supersession failed");
                    } else {
                        // Bug 4 fix: Update reason to reflect supersession so propagated
                        // mutations reference post-supersession state, not pre-supersession content.
                        result.reason = format!("{} (node superseded)", result.reason);
                    }
                }
            }
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
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
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers_upper::dispatch_edge_stale_check(
                batch, &db, &cfg, &mdl,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_edge_stale_check (upper) failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            })
            .await;
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
            let cfg = base_config_owned.clone();
            let mdl = model_owned.clone();
            handles.push(tokio::spawn(async move {
                match stale_helpers::dispatch_rename_check(mutation, &db, &cfg, &mdl).await {
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

    // ── Evidence set apex synthesis ──────────────────────────────────────────
    for batch in es_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers::dispatch_evidence_set_apex_synthesis(
                batch, &db, &cfg, &mdl,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_evidence_set_apex_synthesis failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            })
            .await;
            drop(permit);
        }));
    }

    // ── L0: Targeted L0 stale checks (LLM via stale_helpers) ─────────────────
    for batch in tl0_batches {
        let permit = semaphore.clone().acquire_owned().await?;
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        handles.push(tokio::spawn(async move {
            let results = match stale_helpers::dispatch_targeted_l0_stale_check(
                batch, &db, &cfg, &mdl,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(slug = %s, error = %e, "dispatch_targeted_l0_stale_check failed");
                    Vec::new()
                }
            };
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                    let _ = log_stale_results(&conn, &s, &bid, layer, &results);
                    let _ = propagate_confirmed_stales(&conn, &s, layer, &results);
                }
            })
            .await;
            drop(permit);
        }));
    }

    // ── FAQ category stale: re-distill affected categories ──────────────────
    if !faq_category_stales.is_empty() {
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let cfg = base_config_owned.clone();
        let mdl = model_owned.clone();
        let permit = semaphore.clone().acquire_owned().await?;
        handles.push(tokio::spawn(async move {
            // Collect unique category IDs from the mutations
            let category_ids: Vec<String> = faq_category_stales
                .iter()
                .map(|m| m.target_ref.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            // Re-run category meta-pass scoped to stale categories only
            // Use a single DB connection for both read and write (L3 fix)
            let conn = match super::db::open_pyramid_connection(std::path::Path::new(&db)) {
                Ok(c) => c,
                Err(e) => {
                    error!(slug = %s, error = %e, "Failed to open DB for faq_category_stale");
                    drop(permit);
                    return;
                }
            };
            let shared_conn = Arc::new(tokio::sync::Mutex::new(conn));
            let reader = shared_conn.clone();
            let writer = shared_conn;

            // Load FAQs scoped to stale categories only
            let faqs = {
                let conn = reader.lock().await;

                // Look up which FAQ IDs belong to the stale categories
                let mut stale_faq_ids = std::collections::HashSet::new();
                for cat_id in &category_ids {
                    if let Ok(Some(cat)) = super::db::get_faq_category(&conn, cat_id) {
                        for faq_id in &cat.faq_ids {
                            stale_faq_ids.insert(faq_id.clone());
                        }
                    }
                }

                match super::db::get_faq_nodes(&conn, &s) {
                    Ok(all_faqs) => {
                        if stale_faq_ids.is_empty() {
                            // Fallback: if we couldn't resolve category → faq mappings, re-distill all
                            all_faqs
                        } else {
                            all_faqs
                                .into_iter()
                                .filter(|f| stale_faq_ids.contains(&f.id))
                                .collect()
                        }
                    }
                    Err(e) => {
                        error!(slug = %s, error = %e, "Failed to load FAQs for category re-distillation");
                        drop(permit);
                        return;
                    }
                }
            };

            info!(slug = %s, faq_count = faqs.len(), stale_categories = category_ids.len(), "Scoped FAQ re-distillation to stale categories");

            if let Err(e) = faq::run_faq_category_meta_pass(&reader, &writer, &s, &faqs, &cfg, &mdl).await {
                warn!(slug = %s, error = %e, "FAQ category meta-pass failed during stale dispatch");
            } else {
                info!(slug = %s, "FAQ category meta-pass completed via stale dispatch");
            }
            drop(permit);
        }));
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

    // Ensure the evaluating/cascading phase is visible for at least a fraction of
    // the configured phase display duration so the frontend poll can catch it.
    // Uses ~1/3 of the display duration as the minimum visibility window.
    let min_display = Duration::from_secs((ops.tier2.phase_display_duration_secs / 3).max(1));
    let elapsed = phase_started_at.elapsed();
    if elapsed < min_display {
        tokio::time::sleep(min_display - elapsed).await;
    }

    // Determine outcome by checking stale log for this batch
    let stale_count = {
        let db = db_owned.clone();
        let s = slug_owned.clone();
        let bid = batch_id.clone();
        tokio::task::spawn_blocking(move || -> i64 {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db)) {
                conn.query_row(
                    "SELECT COUNT(*) FROM pyramid_stale_check_log
                     WHERE slug = ?1 AND batch_id = ?2 AND stale = 1",
                    rusqlite::params![s, bid],
                    |row| row.get(0),
                )
                .unwrap_or(0)
            } else {
                0
            }
        })
        .await
        .unwrap_or(0)
    };

    if stale_count > 0 {
        let mut phase = phase_arc.lock().unwrap();
        *phase = "done_stale".to_string();
        let mut summary = summary_arc.lock().unwrap();
        *summary = Some(format!(
            "updated {} understanding{}",
            stale_count,
            if stale_count != 1 { "s" } else { "" }
        ));
    } else {
        let mut phase = phase_arc.lock().unwrap();
        *phase = "done_clean".to_string();
        let mut summary = summary_arc.lock().unwrap();
        *summary = Some("found nothing actionable".to_string());
    }

    // Revert to idle after configured phase display duration
    let pa = phase_arc.clone();
    let display_secs = ops.tier2.phase_display_duration_secs;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(display_secs)).await;
        let mut phase = pa.lock().unwrap();
        if *phase == "done_clean" || *phase == "done_stale" {
            *phase = "idle".to_string();
        }
    });

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

        let result: Vec<PendingMutation> = stmt
            .query_map(rusqlite::params![slug, layer], |row| {
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
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        result
    };

    if !mutations.is_empty() {
        let update_sql =
            "UPDATE pyramid_pending_mutations SET processed = 1, batch_id = ?1 WHERE id = ?2";
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
                result.stale,
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
    // Derive max depth from actual pyramid data, not a hardcoded constant.
    let max_depth: i32 = conn
        .query_row(
            "SELECT COALESCE(MAX(depth), 3) FROM pyramid_nodes WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(3);
    let next_layer = (layer + 1).min(max_depth);

    // Don't propagate from the apex layer back to itself
    if next_layer == layer {
        info!(
            slug = %slug,
            layer,
            "Skipping propagation: already at apex layer"
        );
        return Ok(());
    }

    // All pyramids now use the question chain internally regardless of content_type.
    // Propagation always follows evidence KEEP links through the evidence DAG.

    for result in results {
        if result.stale != 1 {
            debug!(
                slug = %slug,
                target = %result.target_id,
                layer,
                "Skipping propagation for non-stale result"
            );
            continue;
        }

        // Bug 1 fix: Use cascade_depth from the StaleCheckResult directly instead of
        // a DB lookup that would fail because propagated mutations lack batch_id.
        let new_depth = result.cascade_depth + 1;

        // Resolve the propagation target via evidence KEEP links.
        // For L0, target_id is a file path — resolve to node IDs via pyramid_file_hashes first.
        // For L1+, target_id is already a node ID.
        let propagation_targets = if layer == 0 {
            // L0: resolve file path → canonical node IDs → evidence KEEP targets
            let node_ids_json: Option<String> = conn
                .query_row(
                    "SELECT node_ids FROM pyramid_file_hashes
                     WHERE slug = ?1 AND file_path = ?2",
                    rusqlite::params![slug, result.target_id],
                    |row| row.get(0),
                )
                .ok();
            let node_ids: Vec<String> = node_ids_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();
            let targets = stale_helpers_upper::resolve_evidence_targets_for_node_ids(
                conn,
                slug,
                &node_ids,
            )?;
            if targets.is_empty() {
                warn!(
                    slug = %slug,
                    target = %result.target_id,
                    "L0 stale file has no evidence KEEP targets — skipping propagation"
                );
                continue;
            }
            targets
        } else {
            // L1+: follow evidence KEEP links upward
            let targets = stale_helpers_upper::resolve_evidence_targets_for_node_ids(
                conn,
                slug,
                std::slice::from_ref(&result.target_id),
            )?;
            if targets.is_empty() {
                warn!(
                    slug = %slug,
                    target = %result.target_id,
                    layer,
                    "Node has no evidence KEEP targets — skipping propagation to L{}",
                    next_layer
                );
                continue;
            }
            targets
        };

        for propagation_target in propagation_targets {
            info!(
                slug = %slug,
                target = %propagation_target,
                layer,
                next_layer,
                cascade_depth = new_depth,
                "Propagating confirmed stale to layer {}",
                next_layer
            );

            // Note (Bug 8): notify_mutation cannot be called from within spawn_blocking
            // because the engine is not accessible here. Propagated mutations are picked
            // up by the 60s poll loop — this is accepted latency.
            conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                 VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, ?5, ?6, 0)",
                rusqlite::params![
                    slug,
                    next_layer,
                    propagation_target,
                    result.reason,
                    new_depth,
                    now,
                ],
            )
            .context("Failed to propagate confirmed stale mutation")?;
        }
    }

    Ok(())
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
        // Phase 3 fix pass: tests build a default LlmConfig (no registry/credential
        // store attached) since this test doesn't exercise the dispatch path —
        // it only checks the engine's struct construction. Production callers
        // pass a live LlmConfig built via PyramidConfig::to_llm_config_with_runtime.
        let test_config = LlmConfig::default();
        let engine = PyramidStaleEngine::new(
            "test",
            config,
            "/tmp/test.db",
            test_config,
            "inception/mercury-2",
            super::OperationalConfig::default(),
        );
        assert_eq!(engine.slug, "test");
        assert_eq!(engine.layers.len(), 4);
        assert!(!engine.breaker_tripped);
        assert!(!engine.frozen);
        assert_eq!(engine.layers[&0].debounce, Duration::from_secs(5 * 60));
    }
}
