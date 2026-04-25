// pyramid/dadbear_supervisor.rs — WS-H: DADBEAR Runtime Supervisor (Phase 5)
//
// The supervisor is a single reconciliation loop that ties together the
// DADBEAR canonical architecture. It handles:
//
//   1. CRASH RECOVERY — scan for in-flight work items on startup, re-dispatch
//      or timeout as appropriate. Runs to completion BEFORE the normal loop.
//   2. DISPATCH — take compiled/previewed work items through preview → commit
//      → dispatch via the compute queue. Materializes prompts at dispatch time.
//   3. RESULT APPLICATION — completed work items get their results applied to
//      the pyramid, with cascade observation events for affected parent nodes.
//   4. RETENTION — periodic cleanup of old observation events.
//
// The supervisor runs ALONGSIDE the existing dadbear_extend tick loop during
// the transition period (Phases 5–7). The extend loop continues to handle
// observation + compilation. The supervisor adds the dispatch + apply layer
// on top, consuming work items created by the compiler (Phase 3).
//
// Key design points:
//   - Idempotent: can crash and restart without losing state
//   - CAS transitions: all state changes use compare-and-set
//   - DAG-aware: only dispatches items whose deps are in 'applied' state
//   - Hold-aware: checks holds projection before dispatch
//   - Law 4: every LLM call gets a StepContext (reconstructed from work item)
//   - Does NOT bypass the compute queue — submits QueueEntries like any caller

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::compute_queue::{ComputeQueueHandle, QueueEntry};
use crate::pyramid::auto_update_ops;
use crate::pyramid::dadbear_compiler;
use crate::pyramid::dadbear_preview::{self, BudgetDecision};
use crate::pyramid::dispatch_policy::DispatchPolicy;
use crate::pyramid::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use crate::pyramid::llm::{DispatchOrigin, LlmCallOptions, LlmConfig, LlmResponse};
use crate::pyramid::lock_manager::LockManager;
use crate::pyramid::observation_events;
use crate::pyramid::step_context::StepContext;
use crate::pyramid::PyramidState;

// ── Constants ──────────────────────────────────────────────────────────────

/// Supervisor tick interval (seconds).
const TICK_INTERVAL_SECS: u64 = 5;

/// SLA timeout for dispatched work items (seconds). If a dispatched item
/// has no completed attempt after this duration, it is timed out and
/// re-dispatched with a new attempt.
const SLA_TIMEOUT_SECS: i64 = 300;

/// Retention pass interval — run once per hour.
const RETENTION_INTERVAL_SECS: u64 = 3600;

/// Default retention window (days) for observation events.
const DEFAULT_RETENTION_DAYS: i64 = 30;

// ── Work item row (read from DB) ──────────────────────────────────────────

/// A work item as read from `dadbear_work_items`.
#[derive(Debug, Clone)]
struct WorkItem {
    id: String,
    slug: String,
    batch_id: String,
    epoch_id: String,
    step_name: String,
    primitive: String,
    layer: i64,
    target_id: Option<String>,
    system_prompt: String,
    user_prompt: String,
    model_tier: String,
    resolved_model_id: Option<String>,
    resolved_provider_id: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<i64>,
    response_format_json: Option<String>,
    build_id: Option<String>,
    chunk_index: Option<i64>,
    prompt_hash: Option<String>,
    force_fresh: bool,
    state: String,
    state_changed_at: String,
    preview_id: Option<String>,
    observation_event_ids: Option<String>,
    result_json: Option<String>,
}

/// A dispatched item found during crash recovery.
#[derive(Debug, Clone)]
struct InFlightItem {
    work_item: WorkItem,
    dispatched_at: String,
    elapsed_secs: i64,
    attempt_count: i64,
}

/// Result of a completed work item dispatch.
struct CompletedItem {
    work_item_id: String,
    attempt_id: String,
    result: Result<LlmResponse>,
    dispatched_at: std::time::Instant,
}

// ── Handle ─────────────────────────────────────────────────────────────────

/// Handle to the running DADBEAR supervisor. Drop to stop.
pub struct DadbearSupervisorHandle {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl DadbearSupervisorHandle {
    /// Stop the supervisor loop.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for DadbearSupervisorHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

// ── Supervisor ─────────────────────────────────────────────────────────────

/// The DADBEAR runtime supervisor — single reconciliation loop for dispatch,
/// result application, and crash recovery.
pub struct DadbearSupervisor {
    /// Shared pyramid state (DB connections, config, event bus, etc.)
    pyramid_state: Arc<PyramidState>,
    /// Compute queue handle for submitting QueueEntries.
    compute_queue: ComputeQueueHandle,
    /// Database path for opening short-lived connections.
    db_path: String,
    /// Event bus for emitting work item state changes.
    event_bus: Arc<BuildEventBus>,
}

impl DadbearSupervisor {
    pub fn new(
        pyramid_state: Arc<PyramidState>,
        compute_queue: ComputeQueueHandle,
        db_path: String,
        event_bus: Arc<BuildEventBus>,
    ) -> Self {
        Self {
            pyramid_state,
            compute_queue,
            db_path,
            event_bus,
        }
    }
}

// ── Public entry point ─────────────────────────────────────────────────────

/// Start the DADBEAR supervisor loop. Returns a handle to stop it.
///
/// The supervisor MUST be spawned AFTER the GPU processing loop is running
/// (main.rs:11405). The GPU loop must be consuming before any producer
/// enqueues work.
pub fn start_dadbear_supervisor(
    pyramid_state: Arc<PyramidState>,
    compute_queue: ComputeQueueHandle,
    db_path: String,
    event_bus: Arc<BuildEventBus>,
) -> DadbearSupervisorHandle {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let supervisor = DadbearSupervisor::new(pyramid_state, compute_queue, db_path, event_bus);

    let handle = tokio::spawn(async move {
        info!("DADBEAR supervisor starting");

        // Phase A: Crash recovery — runs to completion before normal loop.
        if let Err(e) = supervisor.recover_in_flight_items().await {
            error!(error = %e, "DADBEAR supervisor: crash recovery failed, continuing to normal loop");
        }

        info!("DADBEAR supervisor: crash recovery complete, entering normal tick loop");

        // Phase B: Normal reconciliation loop.
        let mut last_retention = std::time::Instant::now();
        let mut join_set: JoinSet<CompletedItem> = JoinSet::new();

        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    info!("DADBEAR supervisor cancelled");
                    break;
                }
                // Poll JoinSet for completed dispatches.
                Some(result) = join_set.join_next(), if !join_set.is_empty() => {
                    match result {
                        Ok(completed) => {
                            if let Err(e) = supervisor.handle_completion(completed).await {
                                error!(error = %e, "DADBEAR supervisor: result handling failed");
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "DADBEAR supervisor: JoinSet task panicked");
                        }
                    }
                }
                // Normal tick on interval.
                _ = tokio::time::sleep(Duration::from_secs(TICK_INTERVAL_SECS)) => {
                    if let Err(e) = supervisor.tick(&mut join_set).await {
                        error!(error = %e, "DADBEAR supervisor tick failed");
                    }

                    // Periodic retention pass.
                    if last_retention.elapsed() > Duration::from_secs(RETENTION_INTERVAL_SECS) {
                        if let Err(e) = supervisor.retention_pass().await {
                            warn!(error = %e, "DADBEAR supervisor: retention pass failed");
                        }
                        last_retention = std::time::Instant::now();
                    }
                }
            }
        }

        info!("DADBEAR supervisor exited");
    });

    DadbearSupervisorHandle {
        cancel,
        handle: Some(handle),
    }
}

// ── Crash recovery ─────────────────────────────────────────────────────────

impl DadbearSupervisor {
    /// Phase A: Scan for `dispatched` work items with no completed attempt.
    /// For each:
    /// - If elapsed time > SLA_TIMEOUT_SECS: mark attempt as 'timeout', create
    ///   new attempt, re-dispatch.
    /// - If elapsed time < SLA_TIMEOUT_SECS: skip (the call may still complete
    ///   from a provider webhook or in-memory queue processing).
    async fn recover_in_flight_items(&self) -> Result<()> {
        let db_path = self.db_path.clone();
        let in_flight = tokio::task::spawn_blocking(move || -> Result<Vec<InFlightItem>> {
            let conn =
                Connection::open(&db_path).context("Failed to open DB for crash recovery")?;
            find_in_flight_items(&conn)
        })
        .await
        .context("spawn_blocking join error")??;

        if in_flight.is_empty() {
            info!("DADBEAR supervisor: no in-flight items found during crash recovery");
            return Ok(());
        }

        info!(
            count = in_flight.len(),
            "DADBEAR supervisor: found in-flight items during crash recovery"
        );

        for item in &in_flight {
            if item.elapsed_secs > SLA_TIMEOUT_SECS {
                info!(
                    work_item_id = %item.work_item.id,
                    elapsed_secs = item.elapsed_secs,
                    "DADBEAR supervisor: timing out stale dispatched item"
                );

                let db_path = self.db_path.clone();
                let wi_id = item.work_item.id.clone();
                let attempt_count = item.attempt_count;

                // Timeout the old attempt and transition work item back to previewed
                // for re-dispatch on the next tick.
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path)?;
                    timeout_stale_attempt(&conn, &wi_id, attempt_count)?;
                    Ok(())
                })
                .await
                .context("spawn_blocking join error")??;
            } else {
                debug!(
                    work_item_id = %item.work_item.id,
                    elapsed_secs = item.elapsed_secs,
                    "DADBEAR supervisor: in-flight item within SLA, skipping"
                );
            }
        }

        Ok(())
    }

    // ── Normal tick ────────────────────────────────────────────────────────

    /// Single reconciliation tick. For each slug with dispatchable work items:
    /// 1. Check holds — if held, mark items as 'blocked'
    /// 2. Preview remaining compiled items (deps met, no holds)
    /// 3. Budget check + auto-commit
    /// 4. Dispatch committed previewed items
    async fn tick(&self, join_set: &mut JoinSet<CompletedItem>) -> Result<()> {
        let db_path = self.db_path.clone();
        let event_bus = self.event_bus.clone();

        // Gather dispatchable work per slug.
        let slug_work =
            tokio::task::spawn_blocking(move || -> Result<HashMap<String, Vec<WorkItem>>> {
                let conn =
                    Connection::open(&db_path).context("Failed to open DB for supervisor tick")?;
                gather_dispatchable_items(&conn, &event_bus)
            })
            .await
            .context("spawn_blocking join error")??;

        if slug_work.is_empty() {
            return Ok(());
        }

        for (slug, items) in &slug_work {
            debug!(
                slug = %slug,
                item_count = items.len(),
                "DADBEAR supervisor: processing dispatchable items"
            );

            // Get LlmConfig for constructing QueueEntries.
            let config = self.pyramid_state.config.read().await.clone();

            for item in items {
                match self.dispatch_item(item, &config, join_set).await {
                    Ok(()) => {
                        debug!(
                            work_item_id = %item.id,
                            "DADBEAR supervisor: dispatched work item"
                        );
                    }
                    Err(e) => {
                        warn!(
                            work_item_id = %item.id,
                            error = %e,
                            "DADBEAR supervisor: failed to dispatch work item"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    // ── Dispatch flow ──────────────────────────────────────────────────────

    /// Dispatch a single work item through the compute queue.
    ///
    /// Steps:
    /// a) Create a work attempt row
    /// b) Construct QueueEntry with work_item_id + attempt_id
    /// c) Submit to compute queue
    /// d) CAS transition: previewed → dispatched
    /// e) Spawn JoinSet handler that awaits the oneshot result
    async fn dispatch_item(
        &self,
        item: &WorkItem,
        config: &LlmConfig,
        join_set: &mut JoinSet<CompletedItem>,
    ) -> Result<()> {
        let db_path = self.db_path.clone();
        let wi_id = item.id.clone();
        let slug = item.slug.clone();

        // (0) Materialize REAL prompts from current pyramid state.
        //     The compiler stores placeholder prompts; we replace them now.
        use crate::pyramid::prompt_materializer::{self, MaterializeResult};

        let mat_db_path = db_path.clone();
        let mat_slug = slug.clone();
        let mat_primitive = item.primitive.clone();
        let mat_layer = item.layer;
        let mat_target_id = item.target_id.clone().unwrap_or_default();
        let mat_obs_ids = item.observation_event_ids.clone();
        let mat_config = config.clone();

        let mat_result = tokio::task::spawn_blocking(move || -> Result<MaterializeResult> {
            let conn = Connection::open(&mat_db_path)
                .context("Failed to open DB for prompt materialization")?;
            prompt_materializer::materialize_prompt(
                &conn,
                &mat_slug,
                &mat_primitive,
                mat_layer,
                &mat_target_id,
                mat_obs_ids.as_deref(),
                &mat_config,
            )
        })
        .await
        .context("spawn_blocking join error for materialization")??;

        // Handle materialization result.
        match mat_result {
            MaterializeResult::TargetGone { reason } => {
                // Target no longer exists — mark work item as stale and skip.
                info!(
                    work_item_id = %wi_id,
                    reason = %reason,
                    "DADBEAR supervisor: target gone during materialization, marking stale"
                );
                let db_path = db_path.clone();
                let wi_id = wi_id.clone();
                let event_bus = self.event_bus.clone();
                let slug = slug.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path)?;
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    conn.execute(
                        "UPDATE dadbear_work_items
                         SET state = 'stale',
                             state_changed_at = ?1
                         WHERE id = ?2 AND state = 'previewed'",
                        params![now, wi_id],
                    )?;
                    emit_state_changed(&event_bus, &slug, &wi_id, "previewed", "stale");
                    Ok(())
                })
                .await
                .context("spawn_blocking join error")??;
                return Ok(());
            }
            MaterializeResult::Mechanical { reason } => {
                // Mechanical operation — apply directly, skip compute queue.
                info!(
                    work_item_id = %wi_id,
                    primitive = %item.primitive,
                    reason = %reason,
                    "DADBEAR supervisor: mechanical primitive, applying directly"
                );
                self.apply_mechanical_primitive(item).await?;
                return Ok(());
            }
            MaterializeResult::Prompt(materialized) => {
                // Real prompt — update work item row and dispatch to compute queue.
                let db_path_update = db_path.clone();
                let wi_id_update = wi_id.clone();
                let sys_prompt = materialized.system_prompt.clone();
                let usr_prompt = materialized.user_prompt.clone();
                let model_id_mat = materialized.resolved_model_id.clone();
                let prompt_hash = materialized.prompt_hash.clone();

                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path_update)?;
                    conn.execute(
                        "UPDATE dadbear_work_items
                         SET system_prompt = ?1,
                             user_prompt = ?2,
                             resolved_model_id = ?3,
                             prompt_hash = ?4,
                             temperature = ?5,
                             max_tokens = ?6
                         WHERE id = ?7",
                        params![
                            sys_prompt,
                            usr_prompt,
                            model_id_mat,
                            prompt_hash,
                            materialized.temperature,
                            materialized.max_tokens,
                            wi_id_update,
                        ],
                    )?;
                    Ok(())
                })
                .await
                .context("spawn_blocking join error for prompt update")??;

                // Continue with dispatch using materialized prompts.
                return self
                    .dispatch_materialized_item(
                        item,
                        config,
                        join_set,
                        &materialized.system_prompt,
                        &materialized.user_prompt,
                        materialized.temperature as f32,
                        materialized.max_tokens as usize,
                    )
                    .await;
            }
        }
    }

    /// Dispatch a work item with already-materialized prompts to the compute queue.
    async fn dispatch_materialized_item(
        &self,
        item: &WorkItem,
        config: &LlmConfig,
        join_set: &mut JoinSet<CompletedItem>,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: usize,
    ) -> Result<()> {
        let db_path = self.db_path.clone();
        let wi_id = item.id.clone();
        let slug = item.slug.clone();

        // (a) Create work attempt row. DADBEAR always dispatches as
        // `DispatchOrigin::Local` (see `prepare_for_replay` below — fleet and
        // market contexts are cleared so this entry flows through the local
        // compute-queue + pool-branch walker path only). The origin seeds
        // `routing`; `complete_attempt` later overwrites it with the actual
        // provider that served the call.
        let dispatch_origin = DispatchOrigin::Local;
        let attempt_id = {
            let db_path = db_path.clone();
            let wi_id = wi_id.clone();
            tokio::task::spawn_blocking(move || -> Result<String> {
                let conn = Connection::open(&db_path)?;
                create_work_attempt(&conn, &wi_id, dispatch_origin)
            })
            .await
            .context("spawn_blocking join error")??
        };

        // (b) Construct QueueEntry.
        let (result_tx, result_rx) = oneshot::channel::<Result<LlmResponse>>();

        // Build a StepContext from the work item (Law 4).
        let step_ctx = reconstruct_step_context(item, &self.db_path, &self.event_bus);

        // Derive LlmConfig for the queue entry via prepare_for_replay —
        // clears compute_queue (re-enqueue guard) + fleet + market
        // contexts so the GPU loop processes this entry as a pool-only
        // local call. See impl LlmConfig::prepare_for_replay for the
        // single-source-of-truth rationale.
        let queue_config = config.prepare_for_replay(crate::pyramid::llm::DispatchOrigin::Local);

        let response_format = item
            .response_format_json
            .as_ref()
            .and_then(|json_str| serde_json::from_str::<serde_json::Value>(json_str).ok());

        let model_id = item
            .resolved_model_id
            .clone()
            .unwrap_or_else(|| item.model_tier.clone());

        // DADBEAR work_item_id is the semantic job_path.
        let job_path = wi_id.clone();
        let entry = QueueEntry {
            result_tx,
            config: queue_config,
            system_prompt: system_prompt.to_string(),
            user_prompt: user_prompt.to_string(),
            temperature,
            max_tokens,
            response_format,
            options: LlmCallOptions {
                min_timeout_secs: None,
                skip_concurrency_gate: true,
                skip_fleet_dispatch: false,
                chronicle_job_path: None,
                dispatch_origin: Default::default(),
                // W3c: explicit per-call model override. DADBEAR enqueues
                // with the slug pinned at the outer dispatch site; this
                // keeps the queue consumer dispatching the same model.
                model_override: Some(model_id.clone()),
            },
            step_ctx: Some(step_ctx),
            model_id: model_id.clone(),
            enqueued_at: std::time::Instant::now(),
            work_item_id: Some(wi_id.clone()),
            attempt_id: Some(attempt_id.clone()),
            source: "local".to_string(),
            job_path,
            chronicle_job_path: None,
        };

        // (c) Submit to compute queue.
        {
            let mut q = self.compute_queue.queue.lock().await;
            q.enqueue_local(&model_id, entry);
        }
        self.compute_queue.notify.notify_one();

        // (d) CAS transition: previewed → dispatched.
        {
            let db_path = db_path.clone();
            let wi_id = wi_id.clone();
            let event_bus = self.event_bus.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = Connection::open(&db_path)?;
                cas_transition(&conn, &wi_id, "previewed", "dispatched")?;
                emit_state_changed(&event_bus, &slug, &wi_id, "previewed", "dispatched");
                Ok(())
            })
            .await
            .context("spawn_blocking join error")??;
        }

        // (e) Spawn JoinSet handler to await the oneshot result.
        let dispatched_at = std::time::Instant::now();
        let wi_id_for_task = wi_id.clone();
        let attempt_id_for_task = attempt_id.clone();
        join_set.spawn(async move {
            let result = result_rx.await.unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Oneshot channel dropped — GPU loop may have crashed"
                ))
            });
            CompletedItem {
                work_item_id: wi_id_for_task,
                attempt_id: attempt_id_for_task,
                result,
                dispatched_at,
            }
        });

        info!(
            work_item_id = %wi_id,
            attempt_id = %attempt_id,
            model_id = %model_id,
            "DADBEAR supervisor: work item dispatched to compute queue"
        );

        Ok(())
    }

    /// Apply a mechanical primitive (extract, tombstone) directly without
    /// going through the compute queue.
    ///
    /// These operations are deterministic — no LLM call needed.
    async fn apply_mechanical_primitive(&self, item: &WorkItem) -> Result<()> {
        let db_path = self.db_path.clone();
        let wi_id = item.id.clone();
        let slug = item.slug.clone();
        let primitive = item.primitive.clone();
        let target_id = item.target_id.clone().unwrap_or_default();
        let event_bus = self.event_bus.clone();

        // For mechanical primitives, the existing stale_helpers already have
        // well-factored implementations. We delegate to them here.
        match primitive.as_str() {
            "extract" => {
                // New file ingest — dispatch_new_file_ingest handles creation of
                // L0 nodes, file_hashes, and parent cascade. We construct a
                // minimal PendingMutation for it.
                use crate::pyramid::types::PendingMutation;
                let mutation = PendingMutation {
                    id: 0,
                    slug: slug.clone(),
                    layer: 0,
                    mutation_type: "new_file".to_string(),
                    target_ref: target_id.clone(),
                    detail: None,
                    cascade_depth: 0,
                    detected_at: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    processed: false,
                    batch_id: Some(item.batch_id.clone()),
                };

                crate::pyramid::stale_helpers::dispatch_new_file_ingest(vec![mutation], &db_path)
                    .await?;
            }
            "tombstone" => {
                use crate::pyramid::types::PendingMutation;
                let mutation = PendingMutation {
                    id: 0,
                    slug: slug.clone(),
                    layer: 0,
                    mutation_type: "deleted".to_string(),
                    target_ref: target_id.clone(),
                    detail: None,
                    cascade_depth: 0,
                    detected_at: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    processed: false,
                    batch_id: Some(item.batch_id.clone()),
                };

                crate::pyramid::stale_helpers::dispatch_tombstone(vec![mutation], &db_path).await?;
            }
            // Post-build accretion v5 Phase 3 (WS3-C): log-only primitive.
            // Observability events (binding_unresolved, cascade_handler_invoked)
            // produce a chronicle trail without triggering any actual work.
            //
            // Phase 3 verifier fix: the shared post-apply block below emits
            // cascade_stale to parent layers unconditionally, which for
            // log_only would create downstream observation work (defeating
            // the "no side effects" contract and risking feedback loops —
            // binding_unresolved itself maps to log_only, so a cascade back
            // into the pipeline would loop). We CAS → applied directly here
            // and early-return before the shared cascade-emitter runs.
            "log_only" => {
                tracing::info!(
                    slug = %slug,
                    work_item_id = %item.id,
                    step_name = %item.step_name,
                    "log_only event processed (chronicle-only, no side effects)"
                );
                let db_path_lo = db_path.clone();
                let wi_id_lo = wi_id.clone();
                let slug_lo = slug.clone();
                let event_bus_lo = event_bus.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path_lo)?;
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    conn.execute(
                        "UPDATE dadbear_work_items
                         SET state = 'applied',
                             state_changed_at = ?1,
                             applied_at = ?1
                         WHERE id = ?2 AND state = 'previewed'",
                        params![now, wi_id_lo],
                    )?;
                    emit_state_changed(&event_bus_lo, &slug_lo, &wi_id_lo, "previewed", "applied");
                    Ok(())
                })
                .await
                .context("spawn_blocking join error for log_only apply")??;
                return Ok(());
            }
            // v5 Phase 8-2: THE original DADBEAR non-firing bug fix.
            //
            // Flow: annotation_written → (8-1) role_bound cascade_handler →
            // starter-cascade-immediate-redistill OR starter-cascade-judge-gated
            // → queue_re_distill_for_target mechanical enqueues a work item with
            // primitive=re_distill → supervisor finds it → prompt_materializer
            // returns Mechanical → we land here.
            //
            // Actually RE-DISTILL the node by delegating to execute_supersession,
            // which loads node context (distilled + topics + children + evidence),
            // generates a change_manifest via the LLM, validates, then UPDATEs
            // pyramid_nodes.distilled / headline / topics / build_version in
            // place. This is the same call path stale_check takes when the
            // judge says "stale"; it is already battle-tested and preserves
            // node identity (no new node id, viz DAG coherence intact).
            //
            // Phase 8 verifier: the arm body lives in
            // `run_re_distill_supervisor_arm` so tests can drive the whole
            // CAS + result_app + chronicle path end-to-end without standing up
            // a full DadbearSupervisor. The match arm stays as thin glue so
            // the dispatch path still reads as a primitive match.
            //
            // Pre-Phase-8 behavior: re_distill fell through to the supervisor
            // default arm ("applied:re_distill") — marked complete without
            // touching the node. Annotations landed but the pyramid never
            // reflected them. Killing that silent no-op is the mission of
            // this phase.
            "re_distill" => {
                let config = self.pyramid_state.config.read().await.clone();
                let model = item
                    .resolved_model_id
                    .as_deref()
                    .unwrap_or(&item.model_tier)
                    .to_string();
                // Phase 8 tail-2: thread the work item's
                // observation_event_ids into the arm so it can resolve
                // the annotated descendant node id set from the event
                // metadata emitted at routes.rs:
                // emit_annotation_observation_events.
                let obs_ids = item.observation_event_ids.clone();
                run_re_distill_supervisor_arm(
                    &db_path,
                    &wi_id,
                    &slug,
                    &target_id,
                    item.layer,
                    &primitive,
                    &config,
                    &model,
                    &event_bus,
                    obs_ids.as_deref(),
                )
                .await?;
                return Ok(());
            }
            // Post-build accretion v5 Phase 3 (WS3-C): role_bound dispatch.
            // The compiler stamps resolved_chain_id onto the work_item when
            // map_event_to_primitive returns "role_bound" for this event_type.
            // Supervisor invokes the bound chain via the lightweight runner
            // chain_executor::execute_chain_for_target.
            //
            // v5 Phase 1 ships execute_chain_for_target as a stub. Phase 5
            // (first starter chain ships) and Phase 8 (cascade rebinding
            // activates annotation_written via role_bound) are when this arm
            // actually produces work. Until then it returns the stub's
            // "implementation pending" error which is surfaced as a work
            // item failure — honest about the state.
            "role_bound" => {
                // Re-query resolved_chain_id from the work_items row to
                // avoid plumbing a new field through WorkItem's 3
                // constructor sites + SELECT statement. One extra DB read
                // per role_bound dispatch; acceptable given these dispatches
                // are rare (judge-gated, sweep, debate-spawn, etc.).
                let chain_id: Option<String> = {
                    let db_path_rq = db_path.clone();
                    let wi_id_rq = item.id.clone();
                    tokio::task::spawn_blocking(move || -> Result<Option<String>> {
                        let conn = Connection::open(&db_path_rq)?;
                        let v: Option<String> = conn
                            .query_row(
                                "SELECT resolved_chain_id FROM dadbear_work_items WHERE id = ?1",
                                params![wi_id_rq],
                                |row| row.get(0),
                            )
                            .ok()
                            .flatten();
                        Ok(v)
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("join error reading resolved_chain_id: {e}"))??
                };
                let chain_id = match chain_id {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        tracing::error!(
                            slug = %slug,
                            work_item_id = %item.id,
                            "role_bound work item missing resolved_chain_id — compiler bug"
                        );
                        // Phase 5 wanderer fix: CAS previewed→failed before returning
                        // Err. Without this, `find_committed_previewed_items` re-selects
                        // this same row every tick, repeating the error log + DB read
                        // forever until operator intervention. Matches the load-failure
                        // + exec-failure paths below which already mark_role_bound_failed.
                        mark_role_bound_failed(&db_path, &wi_id, &slug, &event_bus).await;
                        return Err(anyhow::anyhow!(
                            "role_bound work_item '{}' missing resolved_chain_id",
                            item.id
                        ));
                    }
                };
                // Phase 3 ships the wiring; the chain_executor runner body
                // is stubbed in Phase 1 (chain_executor::execute_chain_for_target
                // returns "implementation pending"). Phase 5 lands the first
                // real starter chain; this arm becomes exercisable then.
                //
                // Phase 3 verifier fix: on chain load/execute failure (the
                // stub bails today), transition the work item to `failed` so
                // the next tick's `find_committed_previewed_items` does NOT
                // keep re-dispatching it forever. Without this CAS the item
                // stays `previewed`, the chain bail repeats every tick, and
                // we spam the log + waste DB reads until operator intervention.
                //
                // Also: the shared cascade_stale emitter at the bottom of this
                // function assumes the primitive has mutated the target node
                // (extract/tombstone). role_bound delegates mutation to the
                // chain runner — cascades are the chain's responsibility, not
                // ours. Early-return on success to skip the shared emitter.
                let chains_dir = self.pyramid_state.chains_dir.clone();
                let load_result = crate::pyramid::chain_loader::load_chain_by_id(
                    &chain_id,
                    chains_dir.as_path(),
                );
                let chain = match load_result {
                    Ok(c) => c,
                    Err(e) => {
                        mark_role_bound_failed(&db_path, &wi_id, &slug, &event_bus).await;
                        return Err(e.context(format!(
                            "role_bound: failed to load chain '{}' for slug '{}'",
                            chain_id, slug
                        )));
                    }
                };
                // Phase 9a-1: enrich chain input with the triggering
                // observation event's metadata_json. Some role_bound chains
                // need source-event context to operate:
                //
                // - starter-synthesizer (bound on meta_layer_crystallized)
                //   requires `purpose_question` + `covered_substrate_node_ids`
                //   from the crystallization event — load_substrate_nodes
                //   raises without them. Pre-9a this was "dead plumbing":
                //   the chain would fire but its first mechanical step
                //   would loud-raise on missing input.
                // - future synthesizer variants + debate_steward gates may
                //   want `purpose_id`, `parent_meta_layer_id`, etc.
                //
                // Strategy: pass the ENTIRE event metadata_json as
                // `trigger_event_metadata` AND splat its top-level fields
                // into the chain input. Chains that need a specific field
                // can read it directly; chains that pass through can thread
                // the whole envelope forward via
                // `trigger_event_metadata`. This is the same generic
                // approach Phase 8 tail-2's helper uses — one shared
                // `load_source_event_metadata` factored above.
                //
                // feedback_loud_deferrals: when the event has no metadata
                // (or the load fails), we still dispatch the chain with a
                // bare input envelope. The chain's first mechanical step
                // (which needs the metadata) will raise with a clear
                // message pointing at the missing field. This is the loud
                // failure we want — silent no-op would be worse.
                let obs_ids_str = item.observation_event_ids.as_deref();
                let trigger_md = load_source_event_metadata(
                    &db_path,
                    &slug,
                    obs_ids_str,
                );
                let mut chain_input = serde_json::json!({
                    "work_item_id": &item.id,
                    "step_name": &item.step_name,
                    "target_id": item.target_id,
                    "layer": item.layer,
                });
                if let Some(md) = trigger_md {
                    // Thread the full envelope under a stable key for chains
                    // that want the whole object.
                    chain_input["trigger_event_metadata"] = md.clone();
                    // Splat top-level fields (non-destructive: never
                    // overwrites work_item_id / step_name / target_id /
                    // layer). This is what gives the synthesizer chain
                    // `purpose_question` + `covered_substrate_node_ids`
                    // at chain-input depth-0 where `load_substrate_nodes`
                    // reads them.
                    if let Some(obj) = md.as_object() {
                        let existing = chain_input.as_object().cloned().unwrap_or_default();
                        for (k, v) in obj {
                            if !existing.contains_key(k) {
                                chain_input[k.as_str()] = v.clone();
                            }
                        }
                        // Name-remap: the meta_layer_crystallized event emits
                        // `covered_substrate_node_ids` (the source of truth
                        // is the crystallizer's output), but the synthesizer
                        // chain's `load_substrate_nodes` step reads
                        // `covered_substrate_nodes`. Pre-9a this field was
                        // never reaching the chain — the mismatch was never
                        // exercised because the arm didn't thread metadata
                        // at all. Bridge the two names here so both readers
                        // see the substrate id list under their expected
                        // key. Alternative would be renaming the event
                        // metadata, but that's a wider surface change.
                        if !chain_input
                            .as_object()
                            .map(|o| o.contains_key("covered_substrate_nodes"))
                            .unwrap_or(false)
                        {
                            if let Some(ids) = obj.get("covered_substrate_node_ids") {
                                chain_input["covered_substrate_nodes"] = ids.clone();
                            }
                        }
                    }
                }
                let run_result = crate::pyramid::chain_executor::execute_chain_for_target(
                    &self.pyramid_state,
                    &chain,
                    &slug,
                    item.target_id.as_deref(),
                    chain_input,
                )
                .await;
                match run_result {
                    Ok(_result) => {
                        tracing::info!(
                            slug = %slug,
                            work_item_id = %item.id,
                            chain_id = %chain_id,
                            "role_bound chain invocation complete"
                        );
                        // CAS previewed → applied; no shared cascade (chain
                        // is responsible for its own downstream effects).
                        let db_path_rb = db_path.clone();
                        let wi_id_rb = wi_id.clone();
                        let slug_rb = slug.clone();
                        let event_bus_rb = event_bus.clone();
                        tokio::task::spawn_blocking(move || -> Result<()> {
                            let conn = Connection::open(&db_path_rb)?;
                            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                            conn.execute(
                                "UPDATE dadbear_work_items
                                 SET state = 'applied',
                                     state_changed_at = ?1,
                                     applied_at = ?1
                                 WHERE id = ?2 AND state = 'previewed'",
                                params![now, wi_id_rb],
                            )?;
                            emit_state_changed(&event_bus_rb, &slug_rb, &wi_id_rb, "previewed", "applied");
                            Ok(())
                        })
                        .await
                        .context("spawn_blocking join error for role_bound apply")??;
                        return Ok(());
                    }
                    Err(e) => {
                        mark_role_bound_failed(&db_path, &wi_id, &slug, &event_bus).await;
                        return Err(e.context(format!(
                            "role_bound: chain '{}' execution failed for slug '{}'",
                            chain_id, slug
                        )));
                    }
                }
            }
            _ => {
                warn!(
                    primitive = %primitive,
                    "apply_mechanical_primitive called for non-mechanical primitive"
                );
            }
        }

        // CAS: previewed → applied (skip dispatched/completed for mechanical).
        let db_path_cas = db_path.clone();
        let wi_id_cas = wi_id.clone();
        let slug_cas = slug.clone();
        let target_id_cas = target_id.clone();
        let primitive_cas = primitive.clone();
        let event_bus_cas = event_bus.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = Connection::open(&db_path_cas)?;
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            conn.execute(
                "UPDATE dadbear_work_items
                 SET state = 'applied',
                     state_changed_at = ?1,
                     applied_at = ?1
                 WHERE id = ?2 AND state = 'previewed'",
                params![now, wi_id_cas],
            )?;
            emit_state_changed(
                &event_bus_cas,
                &slug_cas,
                &wi_id_cas,
                "previewed",
                "applied",
            );

            // Write result_applications row.
            conn.execute(
                "INSERT OR IGNORE INTO dadbear_result_applications
                 (work_item_id, slug, target_id, action, applied_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![wi_id_cas, slug_cas, target_id_cas, primitive_cas, now],
            )?;

            // Write cascade observations for L1 parent layers.
            // Resolve parent targets via evidence DAG (same pattern as
            // stale_helpers::enqueue_parent_confirmed_stales).
            let metadata = serde_json::json!({
                "triggering_work_item_id": wi_id_cas,
                "source_primitive": primitive_cas,
                "mechanical": true,
            });
            let metadata_str = metadata.to_string();

            let parent_targets =
                crate::pyramid::stale_helpers_upper::resolve_evidence_targets_for_node_ids(
                    &conn,
                    &slug_cas,
                    &[target_id_cas.clone()],
                )
                .unwrap_or_default();

            for parent_target in &parent_targets {
                let _ = observation_events::write_observation_event(
                    &conn,
                    &slug_cas,
                    "cascade",
                    "cascade_stale",
                    None,
                    None,
                    None,
                    None,
                    Some(parent_target),
                    Some(1),
                    Some(&metadata_str),
                );
            }

            info!(
                work_item_id = %wi_id_cas,
                primitive = %primitive_cas,
                target_id = %target_id_cas,
                "DADBEAR supervisor: mechanical primitive applied directly"
            );

            Ok(())
        })
        .await
        .context("spawn_blocking join error for mechanical apply")??;

        Ok(())
    }

    // ── Result handling ────────────────────────────────────────────────────

    /// Handle a completed work item dispatch.
    ///
    /// Steps:
    /// a) Write result to work_attempts
    /// b) CAS work item: dispatched → completed
    /// c) Apply result (acquire LockManager::write, write cascade observations)
    /// d) CAS work item: completed → applied
    /// e) Write result_applications row
    async fn handle_completion(&self, completed: CompletedItem) -> Result<()> {
        let latency_ms = completed.dispatched_at.elapsed().as_millis() as i64;
        let wi_id = completed.work_item_id.clone();
        let attempt_id = completed.attempt_id.clone();

        match completed.result {
            Ok(response) => {
                // (a) Write result to work_attempts.
                let db_path = self.db_path.clone();
                let attempt_id_clone = attempt_id.clone();
                let content = response.content.clone();
                let cost_usd = response.actual_cost_usd;
                let tokens_in = response.usage.prompt_tokens as i64;
                let tokens_out = response.usage.completion_tokens as i64;
                let provider_id = response.provider_id.clone();

                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path)?;
                    complete_attempt(
                        &conn,
                        &attempt_id_clone,
                        &content,
                        cost_usd,
                        tokens_in,
                        tokens_out,
                        latency_ms,
                        provider_id.as_deref(),
                    )
                })
                .await
                .context("spawn_blocking join error")??;

                // (b) CAS: dispatched → completed.
                {
                    let db_path = self.db_path.clone();
                    let wi_id = wi_id.clone();
                    let event_bus = self.event_bus.clone();
                    let result_json = serde_json::to_string(&serde_json::json!({
                        "content": response.content,
                        "generation_id": response.generation_id,
                    }))
                    .unwrap_or_default();
                    let cost = response.actual_cost_usd;
                    let t_in = response.usage.prompt_tokens as i64;
                    let t_out = response.usage.completion_tokens as i64;

                    tokio::task::spawn_blocking(move || -> Result<()> {
                        let conn = Connection::open(&db_path)?;
                        complete_work_item(
                            &conn,
                            &wi_id,
                            &result_json,
                            cost,
                            t_in,
                            t_out,
                            latency_ms,
                        )?;
                        // Read slug for event emission.
                        let slug: String = conn
                            .query_row(
                                "SELECT slug FROM dadbear_work_items WHERE id = ?1",
                                params![wi_id],
                                |row| row.get(0),
                            )
                            .unwrap_or_default();
                        emit_state_changed(&event_bus, &slug, &wi_id, "dispatched", "completed");
                        Ok(())
                    })
                    .await
                    .context("spawn_blocking join error")??;
                }

                // (c) Apply result — acquire LockManager::write(slug).
                // For this initial implementation, we mark the work item as
                // 'applied' since the old drain_and_dispatch still handles
                // actual node operations. The supervisor demonstrates the
                // dispatch→result→apply flow works end-to-end.
                if let Err(e) = self.apply_result(&wi_id).await {
                    error!(
                        work_item_id = %wi_id,
                        error = %e,
                        "DADBEAR supervisor: result application failed"
                    );
                }

                info!(
                    work_item_id = %wi_id,
                    attempt_id = %attempt_id,
                    latency_ms = latency_ms,
                    "DADBEAR supervisor: work item completed and applied"
                );
            }
            Err(e) => {
                // Write failure to attempt and mark work item as failed.
                let db_path = self.db_path.clone();
                let attempt_id_clone = attempt_id.clone();
                let error_msg = format!("{:#}", e);

                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path)?;
                    fail_attempt(&conn, &attempt_id_clone, &error_msg)?;
                    Ok(())
                })
                .await
                .context("spawn_blocking join error")??;

                // CAS: dispatched → failed.
                let db_path = self.db_path.clone();
                let wi_id_clone = wi_id.clone();
                let event_bus = self.event_bus.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let conn = Connection::open(&db_path)?;
                    cas_transition(&conn, &wi_id_clone, "dispatched", "failed")?;
                    let slug: String = conn
                        .query_row(
                            "SELECT slug FROM dadbear_work_items WHERE id = ?1",
                            params![wi_id_clone],
                            |row| row.get(0),
                        )
                        .unwrap_or_default();
                    emit_state_changed(&event_bus, &slug, &wi_id_clone, "dispatched", "failed");
                    Ok(())
                })
                .await
                .context("spawn_blocking join error")??;

                warn!(
                    work_item_id = %wi_id,
                    attempt_id = %attempt_id,
                    error = %e,
                    "DADBEAR supervisor: work item dispatch failed"
                );
            }
        }

        Ok(())
    }

    // ── Result application ─────────────────────────────────────────────────

    /// Apply a completed work item's result to the pyramid.
    ///
    /// Steps:
    /// a) Acquire LockManager::write(slug)
    /// b) Parse the LLM response based on primitive type
    /// c) For stale_check: if stale, call execute_supersession; if not stale, skip
    /// d) For rename_candidate: handle rename or tombstone+ingest
    /// e) CAS: completed → applied
    /// f) Write cascade observation events with triggering_work_item_id
    /// g) Write result_applications row
    async fn apply_result(&self, work_item_id: &str) -> Result<()> {
        // Read the work item to get slug, target info, and result.
        let db_path = self.db_path.clone();
        let wi_id = work_item_id.to_string();
        let item = tokio::task::spawn_blocking(move || -> Result<Option<WorkItem>> {
            let conn = Connection::open(&db_path)?;
            read_work_item(&conn, &wi_id)
        })
        .await
        .context("spawn_blocking join error")??;

        let item = match item {
            Some(i) => i,
            None => {
                warn!(
                    work_item_id = %work_item_id,
                    "DADBEAR supervisor: work item not found for result application"
                );
                return Ok(());
            }
        };

        // (a) Acquire LockManager::write(slug).
        let slug = item.slug.clone();
        let _write_guard = LockManager::global().write(&slug).await;

        let target_id = item.target_id.clone().unwrap_or_default();
        let primitive = item.primitive.clone();
        let layer = item.layer;

        // Extract the LLM response content from result_json.
        let llm_content = item.result_json.as_ref().and_then(|rj| {
            serde_json::from_str::<serde_json::Value>(rj)
                .ok()
                .and_then(|v| v.get("content")?.as_str().map(String::from))
        });

        // (b-d) Apply result based on primitive type.
        let mut action = "applied".to_string();

        if let Some(ref content) = llm_content {
            match primitive.as_str() {
                "stale_check" | "node_stale_check" => {
                    // Parse stale check response.
                    let decision = parse_stale_check_decision(content, &target_id);

                    if decision.kind == StaleCheckDecisionKind::Stale {
                        // Node is stale — trigger supersession.
                        info!(
                            work_item_id = %work_item_id,
                            target_id = %target_id,
                            layer = layer,
                            "DADBEAR supervisor: stale check positive — executing supersession"
                        );

                        let config = self.pyramid_state.config.read().await.clone();
                        let model = item
                            .resolved_model_id
                            .as_deref()
                            .unwrap_or(&item.model_tier);

                        // Stale-check path: annotations (if any) live on
                        // THIS target, not on a descendant, so pass None
                        // and let load_cascade_annotations_for_target
                        // fall back to `target_id`. This is the legacy
                        // semantic the helper preserves.
                        match crate::pyramid::stale_helpers_upper::execute_supersession(
                            &target_id,
                            &self.db_path,
                            &slug,
                            &config,
                            model,
                            None,
                        )
                        .await
                        {
                            Ok(new_node_id) => {
                                action = format!("superseded:{}", new_node_id);
                                info!(
                                    work_item_id = %work_item_id,
                                    target_id = %target_id,
                                    new_node_id = %new_node_id,
                                    "DADBEAR supervisor: supersession complete"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    work_item_id = %work_item_id,
                                    target_id = %target_id,
                                    error = %e,
                                    "DADBEAR supervisor: supersession failed, marking applied anyway"
                                );
                                action = format!("supersession_failed:{}", e);
                            }
                        }
                    } else if decision.kind == StaleCheckDecisionKind::Skip {
                        action = "skipped".to_string();
                        info!(
                            work_item_id = %work_item_id,
                            target_id = %target_id,
                            reason = %decision.reason,
                            "DADBEAR supervisor: stale check skip confirmed by LLM"
                        );
                    } else {
                        action = "not_stale".to_string();
                        info!(
                            work_item_id = %work_item_id,
                            target_id = %target_id,
                            "DADBEAR supervisor: stale check negative — node is current"
                        );
                    }
                }
                "rename_candidate" => {
                    // Parse rename check response (bool + reason).
                    let is_rename = parse_rename_result(content);
                    let rename_reason = parse_rename_reason(content);

                    // Extract old_path/new_path from observation event metadata
                    // or from the target_id format (rename/{old}/{new}).
                    let rename_paths = {
                        let db_path = self.db_path.clone();
                        let obs_ids = item.observation_event_ids.clone();
                        let tid = target_id.clone();
                        tokio::task::spawn_blocking(move || -> Option<(String, String)> {
                            extract_rename_paths(&db_path, obs_ids.as_deref(), &tid)
                        })
                        .await
                        .unwrap_or(None)
                    };

                    if let Some((old_path, new_path)) = rename_paths {
                        // Apply the rename result (creates nodes, supersedes,
                        // updates file_hashes, enqueues parent stales).
                        let db_path = self.db_path.clone();
                        let slug_r = slug.clone();
                        let reason_r = rename_reason.clone();
                        let old_r = old_path.clone();
                        let new_r = new_path.clone();
                        tokio::task::spawn_blocking(move || -> Result<()> {
                            let conn = Connection::open(&db_path)
                                .context("Failed to open DB for rename apply")?;
                            crate::pyramid::stale_helpers::apply_rename_result(
                                &conn, &slug_r, &old_r, &new_r, is_rename, &reason_r,
                            )
                        })
                        .await
                        .context("spawn_blocking join error for rename apply")??;

                        action = if is_rename {
                            format!("rename_confirmed:{}→{}", old_path, new_path)
                        } else {
                            format!("rename_rejected:{}→{}", old_path, new_path)
                        };
                        info!(
                            work_item_id = %work_item_id,
                            target_id = %target_id,
                            is_rename = is_rename,
                            old_path = %old_path,
                            new_path = %new_path,
                            "DADBEAR supervisor: rename result applied"
                        );
                    } else {
                        warn!(
                            work_item_id = %work_item_id,
                            target_id = %target_id,
                            "DADBEAR supervisor: could not extract rename paths — skipping application"
                        );
                        action = "rename_paths_missing".to_string();
                    }
                }
                _ => {
                    // v5 Phase 8-2 — loud-deferral discipline. Historically
                    // this arm silently marked `applied:{primitive}` without
                    // applying anything. Per feedback_loud_deferrals, name
                    // the no-op so operators see the deferral in the result
                    // row instead of it blending in with genuinely-applied
                    // rows. `re_distill` specifically used to land here
                    // before it got its own mechanical path above — if any
                    // legacy work item produced in a pre-Phase-8 build
                    // reaches this arm, the action string makes that
                    // visible to audits.
                    tracing::warn!(
                        work_item_id = %work_item_id,
                        primitive = %primitive,
                        target_id = %target_id,
                        "DADBEAR supervisor: apply_result has no arm for primitive — \
                         marking deferred_no_op (no mutation applied). If this is a \
                         re_distill row, it was compiled under pre-Phase-8 code; \
                         the Phase 8-2 arm handles fresh rows via apply_mechanical_primitive."
                    );
                    action = format!("deferred_no_op:{}", primitive);
                }
            }
        }

        // (e-g) CAS transition and observation events.
        let db_path = self.db_path.clone();
        let wi_id = work_item_id.to_string();
        let event_bus = self.event_bus.clone();
        let slug_for_obs = slug.clone();
        let target_id_obs = target_id.clone();
        let primitive_obs = primitive.clone();
        let action_obs = action.clone();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = Connection::open(&db_path)?;

            // CAS: completed → applied.
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let changed = conn.execute(
                "UPDATE dadbear_work_items
                 SET state = 'applied',
                     state_changed_at = ?1,
                     applied_at = ?1
                 WHERE id = ?2 AND state = 'completed'",
                params![now, wi_id],
            )?;

            if changed == 0 {
                warn!(
                    work_item_id = %wi_id,
                    "DADBEAR supervisor: CAS completed→applied failed (already applied?)"
                );
                return Ok(());
            }

            emit_state_changed(&event_bus, &slug_for_obs, &wi_id, "completed", "applied");

            // (f) Cascade propagation is handled INSIDE execute_supersession
            // via propagate_in_place_update, which resolves L1 parents via
            // the evidence DAG and writes cascade observations using the
            // resolved node_id (not the file path). No additional cascade
            // writing needed here — doing so would either be a no-op (L0
            // file paths don't match evidence rows) or a duplicate (L1+
            // node IDs already propagated by execute_supersession).

            // (g) Write result_applications row.
            conn.execute(
                "INSERT OR IGNORE INTO dadbear_result_applications
                 (work_item_id, slug, target_id, action, applied_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![wi_id, slug_for_obs, target_id_obs, action_obs, now,],
            )?;

            info!(
                work_item_id = %wi_id,
                slug = %slug_for_obs,
                target_id = %target_id_obs,
                action = %action_obs,
                "DADBEAR supervisor: result applied"
            );

            Ok(())
        })
        .await
        .context("spawn_blocking join error")??;

        Ok(())
    }

    // ── Retention ──────────────────────────────────────────────────────────

    /// Periodic retention pass: archive observation events older than the
    /// compilation cursor + retention window.
    async fn retention_pass(&self) -> Result<()> {
        let db_path = self.db_path.clone();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = Connection::open(&db_path)?;
            run_retention_pass(&conn, DEFAULT_RETENTION_DAYS)
        })
        .await
        .context("spawn_blocking join error")?
    }
}

// ── DB helper functions (synchronous, run in spawn_blocking) ───────────────

/// Find all dispatched work items with no completed attempt (crash recovery).
fn find_in_flight_items(conn: &Connection) -> Result<Vec<InFlightItem>> {
    let now = Utc::now();
    let mut stmt = conn.prepare(
        "SELECT wi.id, wi.slug, wi.batch_id, wi.epoch_id, wi.step_name, wi.primitive,
                wi.layer, wi.target_id, wi.system_prompt, wi.user_prompt, wi.model_tier,
                wi.resolved_model_id, wi.resolved_provider_id, wi.temperature, wi.max_tokens,
                wi.response_format_json, wi.build_id, wi.chunk_index, wi.prompt_hash,
                wi.force_fresh, wi.state, wi.state_changed_at, wi.preview_id,
                wi.observation_event_ids, wi.result_json
         FROM dadbear_work_items wi
         WHERE wi.state = 'dispatched'
           AND NOT EXISTS (
               SELECT 1 FROM dadbear_work_attempts a
               WHERE a.work_item_id = wi.id AND a.status IN ('completed', 'failed')
           )",
    )?;

    let items: Vec<InFlightItem> = stmt
        .query_map([], |row| {
            let state_changed_at: String = row.get(21)?;
            // Parse the dispatched_at to compute elapsed time.
            let elapsed_secs =
                chrono::NaiveDateTime::parse_from_str(&state_changed_at, "%Y-%m-%d %H:%M:%S")
                    .map(|dt| {
                        let dispatched =
                            chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
                        (now - dispatched).num_seconds()
                    })
                    .unwrap_or(SLA_TIMEOUT_SECS + 1); // Default to timed out if parse fails.

            // Count existing attempts.
            // (We'll compute this separately to avoid nested queries in the row mapper.)

            Ok(InFlightItem {
                work_item: WorkItem {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    batch_id: row.get(2)?,
                    epoch_id: row.get(3)?,
                    step_name: row.get(4)?,
                    primitive: row.get(5)?,
                    layer: row.get(6)?,
                    target_id: row.get(7)?,
                    system_prompt: row.get(8)?,
                    user_prompt: row.get(9)?,
                    model_tier: row.get(10)?,
                    resolved_model_id: row.get(11)?,
                    resolved_provider_id: row.get(12)?,
                    temperature: row.get(13)?,
                    max_tokens: row.get(14)?,
                    response_format_json: row.get(15)?,
                    build_id: row.get(16)?,
                    chunk_index: row.get(17)?,
                    prompt_hash: row.get(18)?,
                    force_fresh: row.get::<_, i64>(19).unwrap_or(0) != 0,
                    state: row.get(20)?,
                    state_changed_at: row.get(21)?,
                    preview_id: row.get(22)?,
                    observation_event_ids: row.get(23)?,
                    result_json: row.get(24)?,
                },
                dispatched_at: state_changed_at,
                elapsed_secs,
                attempt_count: 0, // Filled below.
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Fill in attempt counts.
    let mut result = Vec::with_capacity(items.len());
    for mut item in items {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_work_attempts WHERE work_item_id = ?1",
                params![item.work_item.id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        item.attempt_count = count;
        result.push(item);
    }

    Ok(result)
}

/// Timeout a stale dispatched item: mark existing pending attempts as 'timeout',
/// then CAS the work item back to 'previewed' so it re-enters the dispatch pipeline.
fn timeout_stale_attempt(conn: &Connection, work_item_id: &str, _attempt_count: i64) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Mark all pending attempts for this work item as 'timeout'.
    conn.execute(
        "UPDATE dadbear_work_attempts
         SET status = 'timeout', completed_at = ?1, error = 'SLA timeout during crash recovery'
         WHERE work_item_id = ?2 AND status = 'pending'",
        params![now, work_item_id],
    )?;

    // Determine target state: 'previewed' if the preview is still valid,
    // 'compiled' if the preview has expired. This prevents expired-preview
    // limbo where the item is in 'previewed' but no query picks it up
    // because the preview's TTL has passed. Matches the pattern in
    // unblock_cleared_items() which does the same validity check.
    let preview_valid = conn
        .query_row(
            "SELECT EXISTS(
            SELECT 1 FROM dadbear_dispatch_previews p
            JOIN dadbear_work_items wi ON wi.preview_id = p.id
            WHERE wi.id = ?1
              AND p.committed_at IS NOT NULL
              AND p.expires_at > ?2
        )",
            params![work_item_id, now],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false);

    let target_state = if preview_valid {
        "previewed"
    } else {
        "compiled"
    };

    conn.execute(
        "UPDATE dadbear_work_items
         SET state = ?1, state_changed_at = ?2, preview_id = CASE WHEN ?1 = 'compiled' THEN NULL ELSE preview_id END
         WHERE id = ?3 AND state = 'dispatched'",
        params![target_state, now, work_item_id],
    )?;

    info!(
        work_item_id = %work_item_id,
        target_state = %target_state,
        "DADBEAR supervisor: timed out stale dispatched item"
    );

    Ok(())
}

/// Gather all dispatchable work items, grouped by slug.
///
/// A work item is dispatchable when:
/// - State is 'previewed' (already previewed and committed)
/// - Its slug has no active holds
/// - All dependency items are in 'applied' state
/// - Its preview is committed (committed_at IS NOT NULL) and not expired
///
/// Also handles: blocking held items, and previewing compiled items that
/// are ready for preview.
fn gather_dispatchable_items(
    conn: &Connection,
    event_bus: &Arc<BuildEventBus>,
) -> Result<HashMap<String, Vec<WorkItem>>> {
    let mut result: HashMap<String, Vec<WorkItem>> = HashMap::new();

    // Step 1: Find slugs with compiled or previewed items.
    let slugs: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT slug FROM dadbear_work_items
             WHERE state IN ('compiled', 'previewed')",
        )?;
        let mapped: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        mapped
    };

    for slug in &slugs {
        // Check holds.
        let is_held = auto_update_ops::is_held(conn, slug);

        if is_held {
            // Mark compiled/previewed items as 'blocked' (with blocked_from).
            block_held_items(conn, slug, event_bus)?;
            continue;
        }

        // Unblock any previously blocked items whose holds have cleared.
        unblock_cleared_items(conn, slug, event_bus)?;

        // Step 2: Preview compiled items that are ready (deps met).
        preview_ready_items(conn, slug, event_bus)?;

        // Step 3: Gather previewed items whose previews are committed and valid.
        let items = find_committed_previewed_items(conn, slug)?;

        if !items.is_empty() {
            result.insert(slug.clone(), items);
        }
    }

    Ok(result)
}

/// Block compiled/previewed items for a held slug.
fn block_held_items(conn: &Connection, slug: &str, event_bus: &Arc<BuildEventBus>) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Block compiled items.
    let mut stmt = conn.prepare(
        "SELECT id FROM dadbear_work_items
         WHERE slug = ?1 AND state IN ('compiled', 'previewed')",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![slug], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    for wi_id in &ids {
        // Read current state for blocked_from.
        let current_state: Option<String> = conn
            .query_row(
                "SELECT state FROM dadbear_work_items WHERE id = ?1",
                params![wi_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(state) = current_state {
            let changed = conn.execute(
                "UPDATE dadbear_work_items
                 SET state = 'blocked',
                     state_changed_at = ?1,
                     blocked_from = ?2
                 WHERE id = ?3 AND state = ?2",
                params![now, state, wi_id],
            )?;

            if changed > 0 {
                emit_state_changed(event_bus, slug, wi_id, &state, "blocked");
                debug!(
                    work_item_id = %wi_id,
                    slug = %slug,
                    blocked_from = %state,
                    "DADBEAR supervisor: blocked work item due to holds"
                );
            }
        }
    }

    Ok(())
}

/// Unblock items whose holds have been cleared.
fn unblock_cleared_items(
    conn: &Connection,
    slug: &str,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Find blocked items for this slug.
    let mut stmt = conn.prepare(
        "SELECT id, blocked_from, preview_id FROM dadbear_work_items
         WHERE slug = ?1 AND state = 'blocked'",
    )?;

    let blocked: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map(params![slug], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    for (wi_id, blocked_from, preview_id) in &blocked {
        let restore_to = match blocked_from.as_deref() {
            Some("previewed") => {
                // Check if preview is still valid.
                if let Some(pid) = preview_id {
                    let still_valid = is_preview_still_valid(conn, pid)?;
                    if still_valid {
                        "previewed"
                    } else {
                        "compiled" // Preview expired, must re-preview.
                    }
                } else {
                    "compiled"
                }
            }
            Some("compiled") | _ => "compiled",
        };

        let changed = conn.execute(
            "UPDATE dadbear_work_items
             SET state = ?1,
                 state_changed_at = ?2,
                 blocked_from = NULL
             WHERE id = ?3 AND state = 'blocked'",
            params![restore_to, now, wi_id],
        )?;

        if changed > 0 {
            emit_state_changed(event_bus, slug, wi_id, "blocked", restore_to);
            debug!(
                work_item_id = %wi_id,
                slug = %slug,
                restored_to = %restore_to,
                "DADBEAR supervisor: unblocked work item (holds cleared)"
            );
        }
    }

    Ok(())
}

/// Preview compiled items that are ready for dispatch (deps met).
fn preview_ready_items(
    conn: &Connection,
    slug: &str,
    event_bus: &Arc<BuildEventBus>,
) -> Result<()> {
    // Find compiled items with all deps in 'applied' state.
    let mut stmt = conn.prepare(
        "SELECT wi.id FROM dadbear_work_items wi
         WHERE wi.slug = ?1
           AND wi.state = 'compiled'
           AND NOT EXISTS (
               SELECT 1 FROM dadbear_work_item_deps d
               JOIN dadbear_work_items dep ON d.depends_on_id = dep.id
               WHERE d.work_item_id = wi.id AND dep.state != 'applied'
           )",
    )?;

    let item_ids: Vec<String> = stmt
        .query_map(params![slug], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    if item_ids.is_empty() {
        return Ok(());
    }

    // Get or create a default dispatch policy for preview.
    let policy = DispatchPolicy {
        rules: vec![],
        escalation: Default::default(),
        build_coordination: Default::default(),
        pool_configs: Default::default(),
        max_batch_cost_usd: None,
        max_daily_cost_usd: None,
    };
    let norms_hash = "default"; // Norms hash — will be real when norms contribution lands.

    // Get the batch_id from the first item.
    let batch_id: String = conn
        .query_row(
            "SELECT batch_id FROM dadbear_work_items WHERE id = ?1",
            params![item_ids[0]],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| format!("{slug}:unknown:batch-0"));

    // Create dispatch preview.
    match dadbear_preview::create_dispatch_preview(
        conn, slug, &batch_id, &item_ids, &policy, norms_hash,
    ) {
        Ok(preview_id) => {
            debug!(
                slug = %slug,
                preview_id = %preview_id,
                item_count = item_ids.len(),
                "DADBEAR supervisor: created dispatch preview"
            );

            // Auto-commit + budget enforcement.
            // Read preview cost for budget check.
            let preview_cost: f64 = conn
                .query_row(
                    "SELECT total_cost_usd FROM dadbear_dispatch_previews WHERE id = ?1",
                    params![preview_id],
                    |row| row.get(0),
                )
                .unwrap_or(0.0);

            match dadbear_preview::enforce_budget_and_commit(
                conn,
                event_bus,
                slug,
                &preview_id,
                preview_cost,
                &policy,
            ) {
                Ok(BudgetDecision::AutoCommit) => {
                    debug!(
                        slug = %slug,
                        preview_id = %preview_id,
                        "DADBEAR supervisor: preview auto-committed (within budget)"
                    );
                }
                Ok(BudgetDecision::RequiresApproval) => {
                    info!(
                        slug = %slug,
                        preview_id = %preview_id,
                        cost = preview_cost,
                        "DADBEAR supervisor: preview requires operator approval"
                    );
                }
                Ok(BudgetDecision::CostLimitHold) => {
                    warn!(
                        slug = %slug,
                        preview_id = %preview_id,
                        cost = preview_cost,
                        "DADBEAR supervisor: cost limit hold placed"
                    );
                }
                Err(e) => {
                    warn!(
                        slug = %slug,
                        preview_id = %preview_id,
                        error = %e,
                        "DADBEAR supervisor: budget enforcement failed"
                    );
                }
            }
        }
        Err(e) => {
            // Preview creation can fail due to CAS atomicity (another process
            // already previewed these items). This is expected during the
            // transition period.
            debug!(
                slug = %slug,
                error = %e,
                "DADBEAR supervisor: preview creation skipped (likely CAS contention)"
            );
        }
    }

    Ok(())
}

/// Find previewed items whose previews are committed and valid.
fn find_committed_previewed_items(conn: &Connection, slug: &str) -> Result<Vec<WorkItem>> {
    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut stmt = conn.prepare(
        "SELECT wi.id, wi.slug, wi.batch_id, wi.epoch_id, wi.step_name, wi.primitive,
                wi.layer, wi.target_id, wi.system_prompt, wi.user_prompt, wi.model_tier,
                wi.resolved_model_id, wi.resolved_provider_id, wi.temperature, wi.max_tokens,
                wi.response_format_json, wi.build_id, wi.chunk_index, wi.prompt_hash,
                wi.force_fresh, wi.state, wi.state_changed_at, wi.preview_id,
                wi.observation_event_ids, wi.result_json
         FROM dadbear_work_items wi
         WHERE wi.slug = ?1
           AND wi.state = 'previewed'
           AND wi.preview_id IS NOT NULL
           AND EXISTS (
               SELECT 1 FROM dadbear_dispatch_previews p
               WHERE p.id = wi.preview_id
                 AND p.committed_at IS NOT NULL
                 AND p.expires_at > ?2
           )
           AND NOT EXISTS (
               SELECT 1 FROM dadbear_work_item_deps d
               JOIN dadbear_work_items dep ON d.depends_on_id = dep.id
               WHERE d.work_item_id = wi.id AND dep.state != 'applied'
           )",
    )?;

    let items: Vec<WorkItem> = stmt
        .query_map(params![slug, now_str], |row| {
            Ok(WorkItem {
                id: row.get(0)?,
                slug: row.get(1)?,
                batch_id: row.get(2)?,
                epoch_id: row.get(3)?,
                step_name: row.get(4)?,
                primitive: row.get(5)?,
                layer: row.get(6)?,
                target_id: row.get(7)?,
                system_prompt: row.get(8)?,
                user_prompt: row.get(9)?,
                model_tier: row.get(10)?,
                resolved_model_id: row.get(11)?,
                resolved_provider_id: row.get(12)?,
                temperature: row.get(13)?,
                max_tokens: row.get(14)?,
                response_format_json: row.get(15)?,
                build_id: row.get(16)?,
                chunk_index: row.get(17)?,
                prompt_hash: row.get(18)?,
                force_fresh: row.get::<_, i64>(19).unwrap_or(0) != 0,
                state: row.get(20)?,
                state_changed_at: row.get(21)?,
                preview_id: row.get(22)?,
                observation_event_ids: row.get(23)?,
                result_json: row.get(24)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(items)
}

/// Check whether a preview is still valid (committed, not expired).
fn is_preview_still_valid(conn: &Connection, preview_id: &str) -> Result<bool> {
    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let valid: bool = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM dadbear_dispatch_previews
                WHERE id = ?1
                  AND committed_at IS NOT NULL
                  AND expires_at > ?2
            )",
            params![preview_id, now_str],
            |row| row.get(0),
        )
        .unwrap_or(false);
    Ok(valid)
}

/// Create a work attempt row. Returns the attempt_id.
///
/// `origin` records the dispatch origin of this attempt (see
/// `DispatchOrigin::source_label`). `routing` is seeded with that label and
/// later overwritten by `complete_attempt` with the actual provider that
/// served the call — so a successful attempt's `routing` reflects e.g.
/// `openrouter` / `ollama-local` while in-flight or failed attempts retain
/// the origin label.
fn create_work_attempt(
    conn: &Connection,
    work_item_id: &str,
    origin: DispatchOrigin,
) -> Result<String> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Count existing attempts to determine attempt_number.
    let attempt_number: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dadbear_work_attempts WHERE work_item_id = ?1",
            params![work_item_id],
            |row| row.get(0),
        )
        .unwrap_or(0)
        + 1;

    let attempt_id = dadbear_compiler::attempt_id(work_item_id, attempt_number);

    // Read model_id from the work item; routing is seeded from origin.
    let routing = origin.source_label().to_string();
    let model_id: String = conn
        .query_row(
            "SELECT COALESCE(resolved_model_id, model_tier)
             FROM dadbear_work_items WHERE id = ?1",
            params![work_item_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "unknown".to_string());

    conn.execute(
        "INSERT INTO dadbear_work_attempts
         (id, work_item_id, attempt_number, dispatched_at, model_id, routing, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
        params![
            attempt_id,
            work_item_id,
            attempt_number,
            now,
            model_id,
            routing
        ],
    )?;

    Ok(attempt_id)
}

/// Complete a work attempt with a successful result.
///
/// `provider_id` is the provider that actually served the call (e.g.
/// `openrouter`, `ollama-local`), surfaced from `LlmResponse::provider_id`.
/// When present it overwrites the origin-seeded `routing` label so operator
/// telemetry reflects the real compute lane rather than the dispatch origin.
fn complete_attempt(
    conn: &Connection,
    attempt_id: &str,
    result_json: &str,
    cost_usd: Option<f64>,
    tokens_in: i64,
    tokens_out: i64,
    latency_ms: i64,
    provider_id: Option<&str>,
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    conn.execute(
        "UPDATE dadbear_work_attempts
         SET status = 'completed',
             result_json = ?1,
             cost_usd = ?2,
             tokens_in = ?3,
             tokens_out = ?4,
             latency_ms = ?5,
             completed_at = ?6,
             routing = COALESCE(?7, routing)
         WHERE id = ?8 AND status = 'pending'",
        params![
            result_json,
            cost_usd,
            tokens_in,
            tokens_out,
            latency_ms,
            now,
            provider_id,
            attempt_id,
        ],
    )?;

    Ok(())
}

/// Fail a work attempt with an error.
fn fail_attempt(conn: &Connection, attempt_id: &str, error: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    conn.execute(
        "UPDATE dadbear_work_attempts
         SET status = 'failed',
             error = ?1,
             completed_at = ?2
         WHERE id = ?3 AND status = 'pending'",
        params![error, now, attempt_id],
    )?;

    Ok(())
}

/// Complete a work item with result data. CAS: dispatched → completed.
fn complete_work_item(
    conn: &Connection,
    work_item_id: &str,
    result_json: &str,
    cost_usd: Option<f64>,
    tokens_in: i64,
    tokens_out: i64,
    latency_ms: i64,
) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let changed = conn.execute(
        "UPDATE dadbear_work_items
         SET state = 'completed',
             state_changed_at = ?1,
             result_json = ?2,
             result_cost_usd = ?3,
             result_tokens_in = ?4,
             result_tokens_out = ?5,
             result_latency_ms = ?6,
             completed_at = ?1
         WHERE id = ?7 AND state = 'dispatched'",
        params![
            now,
            result_json,
            cost_usd,
            tokens_in,
            tokens_out,
            latency_ms,
            work_item_id
        ],
    )?;

    if changed == 0 {
        warn!(
            work_item_id = %work_item_id,
            "DADBEAR supervisor: CAS dispatched→completed failed"
        );
    }

    Ok(())
}

/// CAS state transition for a work item.
fn cas_transition(conn: &Connection, work_item_id: &str, from: &str, to: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let changed = conn.execute(
        "UPDATE dadbear_work_items
         SET state = ?1, state_changed_at = ?2
         WHERE id = ?3 AND state = ?4",
        params![to, now, work_item_id, from],
    )?;

    if changed == 0 {
        warn!(
            work_item_id = %work_item_id,
            from = %from,
            to = %to,
            "DADBEAR supervisor: CAS transition failed"
        );
    }

    Ok(())
}

/// Read a single work item from the database.
fn read_work_item(conn: &Connection, work_item_id: &str) -> Result<Option<WorkItem>> {
    let item = conn
        .query_row(
            "SELECT id, slug, batch_id, epoch_id, step_name, primitive,
                    layer, target_id, system_prompt, user_prompt, model_tier,
                    resolved_model_id, resolved_provider_id, temperature, max_tokens,
                    response_format_json, build_id, chunk_index, prompt_hash,
                    force_fresh, state, state_changed_at, preview_id,
                    observation_event_ids, result_json
             FROM dadbear_work_items WHERE id = ?1",
            params![work_item_id],
            |row| {
                Ok(WorkItem {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    batch_id: row.get(2)?,
                    epoch_id: row.get(3)?,
                    step_name: row.get(4)?,
                    primitive: row.get(5)?,
                    layer: row.get(6)?,
                    target_id: row.get(7)?,
                    system_prompt: row.get(8)?,
                    user_prompt: row.get(9)?,
                    model_tier: row.get(10)?,
                    resolved_model_id: row.get(11)?,
                    resolved_provider_id: row.get(12)?,
                    temperature: row.get(13)?,
                    max_tokens: row.get(14)?,
                    response_format_json: row.get(15)?,
                    build_id: row.get(16)?,
                    chunk_index: row.get(17)?,
                    prompt_hash: row.get(18)?,
                    force_fresh: row.get::<_, i64>(19).unwrap_or(0) != 0,
                    state: row.get(20)?,
                    state_changed_at: row.get(21)?,
                    preview_id: row.get(22)?,
                    observation_event_ids: row.get(23)?,
                    result_json: row.get(24)?,
                })
            },
        )
        .ok();

    Ok(item)
}

/// Reconstruct a StepContext from a work item's durable fields.
/// Law 4: every LLM call gets a StepContext.
fn reconstruct_step_context(
    item: &WorkItem,
    db_path: &str,
    event_bus: &Arc<BuildEventBus>,
) -> StepContext {
    StepContext {
        slug: item.slug.clone(),
        build_id: item
            .build_id
            .clone()
            .unwrap_or_else(|| item.batch_id.clone()),
        step_name: item.step_name.clone(),
        primitive: item.primitive.clone(),
        depth: item.layer,
        chunk_index: item.chunk_index,
        db_path: db_path.to_string(),
        force_fresh: item.force_fresh,
        bus: Some(event_bus.clone()),
        model_tier: item.model_tier.clone(),
        resolved_model_id: item.resolved_model_id.clone(),
        resolved_provider_id: item.resolved_provider_id.clone(),
        prompt_hash: item.prompt_hash.clone().unwrap_or_default(),
        // Chronicle integration fields
        chain_name: String::new(),
        content_type: String::new(),
        task_label: format!("dadbear:{}", item.primitive),
        balance_exhausted_emitted: std::sync::OnceLock::new(),
        dispatch_decision: None,
    }
}

/// Phase 3 verifier helper: on role_bound chain load or execution failure,
/// CAS the work item from `previewed` to `failed` so the supervisor's
/// `find_committed_previewed_items` query doesn't re-select it every tick.
/// We don't propagate this helper's own errors — the caller is already
/// returning an Err on the original failure; a CAS blip here becomes a
/// follow-up concern, not a cascade of nested errors.
///
/// Phase 5 wanderer: exposed as `pub(crate)` so the missing-resolved_chain_id
/// regression test at phase5_post_build_tests can call this directly. The
/// arm is inline in apply_mechanical_primitive so the test verifies the
/// helper's CAS behavior that the inline match-arm now depends on for
/// the resolved_chain_id==None path.
pub(crate) async fn mark_role_bound_failed(
    db_path: &str,
    wi_id: &str,
    slug: &str,
    event_bus: &Arc<BuildEventBus>,
) {
    let db_path = db_path.to_string();
    let wi_id = wi_id.to_string();
    let slug = slug.to_string();
    let event_bus = event_bus.clone();
    let wi_id_for_log = wi_id.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<bool> {
        let conn = Connection::open(&db_path)?;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let changed = conn.execute(
            "UPDATE dadbear_work_items
             SET state = 'failed',
                 state_changed_at = ?1
             WHERE id = ?2 AND state = 'previewed'",
            params![now, wi_id],
        )?;
        if changed > 0 {
            emit_state_changed(&event_bus, &slug, &wi_id, "previewed", "failed");
        }
        Ok(changed > 0)
    })
    .await;
    match join {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            warn!(
                work_item_id = %wi_id_for_log,
                error = %e,
                "mark_role_bound_failed: CAS previewed→failed error"
            );
        }
        Err(e) => {
            warn!(
                work_item_id = %wi_id_for_log,
                error = %e,
                "mark_role_bound_failed: spawn_blocking join error"
            );
        }
    }
}

/// Emit a WorkItemStateChanged event on the build event bus.
fn emit_state_changed(
    event_bus: &Arc<BuildEventBus>,
    slug: &str,
    work_item_id: &str,
    old_state: &str,
    new_state: &str,
) {
    let _ = event_bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::WorkItemStateChanged {
            slug: slug.to_string(),
            work_item_id: work_item_id.to_string(),
            old_state: old_state.to_string(),
            new_state: new_state.to_string(),
        },
    });
}

/// v5 Phase 8-2 (Phase 8 verifier): the supervisor's `re_distill` arm body,
/// extracted as a free `pub(crate)` helper so a real end-to-end test can drive
/// the whole arm — including CAS, result_applications row, and the
/// `node_re_distilled` chronicle event — without constructing a full
/// `DadbearSupervisor` with a compute queue + pyramid state.
///
/// Behavior is identical to the inline match-arm body it replaced:
///
/// - empty `target_id` → CAS previewed→failed, return Ok (loud warn logged).
/// - `execute_supersession` success → CAS previewed→applied, insert
///   `re_distilled:{new_canonical_id}` row in `dadbear_result_applications`,
///   emit `node_re_distilled` observation event.
/// - `execute_supersession` failure → CAS previewed→failed, return the error
///   (caller of this helper logs the failure — `execute_supersession` already
///   persists a failed-manifest oversight row on its side).
///
/// The Phase 8 crown-jewel end-to-end test invokes this helper directly after
/// compiling an `annotation_written` event + running the cascade chain that
/// queues the re_distill work item, proving the whole annotation → role_bound
/// → chain → queue_re_distill → supervisor arm → execute_supersession →
/// pyramid_nodes UPDATE path in one test.
///
/// Phase 8 tail — annotation content channel (closes the v6 FIXME).
///
/// `execute_supersession` builds `ManifestGenerationInput` with a
/// `cascade_annotations: Vec<CascadeAnnotation>` field populated from
/// `pyramid_annotations` WHERE node_id = target AND created_at > (last
/// re-distill `applied_at` | node `created_at`). Every annotation type —
/// `observation`, `hypothesis`, `steel_man`, `position`, `correction`, etc.
/// — is surfaced to the change-manifest prompt in its own PENDING
/// ANNOTATIONS section, so the LLM sees non-correction annotation content
/// directly and can decide whether to update distilled / headline / topics.
///
/// Semantic clarity preserved (Option-3 hybrid per scope doc):
/// - `creates_delta=true` (correction only) still routes through
///   `pyramid_deltas` → `recent_deltas` → `changed_children`. That is the
///   mechanical field-level edit channel.
/// - `creates_delta=false` (everything else) flows through
///   `cascade_annotations`. That is the narrative feedback channel.
/// Both channels are visible to the LLM; `creates_delta` remains a
/// truthful statement about WHICH mechanism applies, not a gate on
/// whether the annotation reaches the prompt at all.
///
/// The watermark for `cascade_annotations` is the target's most recent
/// `dadbear_result_applications.action LIKE 're_distilled:%'.applied_at`
/// (falling back to `pyramid_nodes.created_at` on first re-distill).
/// Failed re-distills leave the watermark where it was, so the next run
/// re-includes the same annotations — no silent loss of annotation
/// feedback on retry.
/// Phase 8 tail-2 — production routing fix.
///
/// `observation_event_ids_json` is the work item's `observation_event_ids`
/// column (a JSON array like `[12,13]`). Each referenced event is a
/// `dadbear_observation_events` row whose `metadata_json.annotated_node_id`
/// records the DESCENDANT the annotation was written on. The supervisor
/// collects those descendant ids and passes them to `execute_supersession`
/// as `annotated_node_ids` so `load_cascade_annotations_for_target`
/// queries annotations on the descendants (where they live) rather than
/// on the re-distill target (an ancestor, which holds none).
///
/// When a single annotation rolls up to multiple ancestors the compiler
/// coalesces them into one work item per ancestor, each with the SAME
/// `annotated_node_id`. When multiple annotations on multiple descendants
/// all roll up to ONE ancestor, that ancestor's work item has multiple
/// `observation_event_ids`, each with a different `annotated_node_id` —
/// this helper unions them.
///
/// Absent metadata → loud `RAISE EXCEPTION`-style log + empty list so the
/// routing gap is visible (feedback_loud_deferrals). `None` passes through
/// to the helper's backward-compat fallback (annotations queried on the
/// target itself), which is what stale-check callers want.

/// Phase 9a-1: shared helper to load + union metadata_json from the set of
/// observation events referenced by a work item's `observation_event_ids`.
///
/// Factors the "read each referenced event, parse metadata_json" pattern
/// shared by:
/// - Phase 8 tail-2 `run_re_distill_supervisor_arm` (extracts
///   `annotated_node_id` set from annotation_written events)
/// - Phase 9a-1 role_bound synthesizer enrichment (extracts
///   `purpose_question` / `covered_substrate_node_ids` / `parent_meta_layer_id`
///   from the meta_layer_crystallized event)
/// - Any future consumer that needs the triggering event's full metadata
///
/// Returns the metadata_json of the FIRST event in the list (there is always
/// exactly one for chain-emitted role_bound events — the compiler coalesces
/// by target, not by source, for these emitters). Returns `None` when:
/// - `observation_event_ids_json` is None or empty
/// - The first event id does not resolve to a row
/// - `metadata_json` is NULL on the row
/// - `metadata_json` is not parseable as JSON
///
/// All failure modes log loudly at warn-level so metadata-loading bugs are
/// visible rather than silent (feedback_loud_deferrals).
///
/// The "union multiple events" variant lives inside
/// `run_re_distill_supervisor_arm` because its union semantic is specific
/// to the `annotated_node_id` set — generalizing it here would leak that
/// concern into the signature. When a second role_bound emitter needs
/// union semantics, factor THAT out at the call site that needs it.
fn load_source_event_metadata(
    db_path: &str,
    slug: &str,
    observation_event_ids_json: Option<&str>,
) -> Option<serde_json::Value> {
    let ids_json = observation_event_ids_json?;
    let event_ids: Vec<i64> = serde_json::from_str(ids_json).unwrap_or_default();
    let first_id = *event_ids.first()?;
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                error = %e,
                slug = %slug,
                "load_source_event_metadata: failed to open DB — returning None"
            );
            return None;
        }
    };
    let md: Option<Option<String>> = conn
        .query_row(
            "SELECT metadata_json FROM dadbear_observation_events
              WHERE slug = ?1 AND id = ?2",
            params![slug, first_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok();
    let Some(Some(md_str)) = md else {
        warn!(
            slug = %slug,
            event_id = %first_id,
            "load_source_event_metadata: event row has no metadata_json — \
             returning None (downstream chain input will not carry trigger metadata)"
        );
        return None;
    };
    match serde_json::from_str::<serde_json::Value>(&md_str) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(
                slug = %slug,
                event_id = %first_id,
                error = %e,
                "load_source_event_metadata: metadata_json is not valid JSON — \
                 returning None"
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_re_distill_supervisor_arm(
    db_path: &str,
    wi_id: &str,
    slug: &str,
    target_id: &str,
    layer: i64,
    primitive: &str,
    config: &LlmConfig,
    model: &str,
    event_bus: &Arc<BuildEventBus>,
    observation_event_ids_json: Option<&str>,
) -> Result<()> {
    // Phase 9a-2: enforce the `execute_supersession` caller-holds-lock
    // contract. Acquired here at function entry so every branch below —
    // target-missing fast-fail, supersession success, supersession error —
    // runs under the slug's write lock. Dropped implicitly on return.
    //
    // The stale_check path (apply_result) holds its own lock at a higher
    // scope (line ~1203) and does NOT flow through this arm — it calls
    // execute_supersession directly. So acquiring here is correct for the
    // re_distill arm and does not double-lock the stale_check path.
    let _slug_write_guard = LockManager::global().write(slug).await;

    if target_id.is_empty() {
        tracing::warn!(
            slug = %slug,
            work_item_id = %wi_id,
            "re_distill: missing target_id, cannot re-distill"
        );
        let db_path_rd = db_path.to_string();
        let wi_id_rd = wi_id.to_string();
        let slug_rd = slug.to_string();
        let event_bus_rd = event_bus.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = Connection::open(&db_path_rd)?;
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            conn.execute(
                "UPDATE dadbear_work_items
                 SET state = 'failed',
                     state_changed_at = ?1
                 WHERE id = ?2 AND state = 'previewed'",
                params![now, wi_id_rd],
            )?;
            emit_state_changed(&event_bus_rd, &slug_rd, &wi_id_rd, "previewed", "failed");
            Ok(())
        })
        .await
        .context("spawn_blocking join error for re_distill target-missing fail")??;
        return Ok(());
    }

    // Phase 8 tail-2 — resolve the annotated descendant id set from the
    // work item's observation_event_ids column. Read every referenced
    // event row, parse metadata_json, collect `annotated_node_id`, union
    // into the set the change-manifest prompt should pull annotations
    // from.
    //
    // None is returned ONLY when the work item has no observation event
    // ids at all (e.g. hand-enqueued via the test-only stale path). In
    // that case the helper falls back to target_id. When event ids ARE
    // present but NONE of them carry a valid annotated_node_id we
    // return Some(empty) and let load_cascade_annotations_for_target
    // log loudly + fall back — this is the "loud deferral" the memory
    // feedback wants.
    let annotated_node_ids: Option<Vec<String>> = if let Some(ids_json) =
        observation_event_ids_json
    {
        let db_path_read = db_path.to_string();
        let slug_read = slug.to_string();
        let ids_json_owned = ids_json.to_string();
        let target_id_read = target_id.to_string();
        let wi_id_read = wi_id.to_string();
        tokio::task::spawn_blocking(move || -> Option<Vec<String>> {
            let event_ids: Vec<i64> =
                serde_json::from_str(&ids_json_owned).unwrap_or_default();
            if event_ids.is_empty() {
                // No events on this work item at all — fall through to
                // target_id via None.
                tracing::debug!(
                    work_item_id = %wi_id_read,
                    slug = %slug_read,
                    "re_distill: no observation_event_ids on work item — \
                     cascade_annotations will fall back to target_id"
                );
                return None;
            }
            let conn = match Connection::open(&db_path_read) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        work_item_id = %wi_id_read,
                        "re_distill: failed to open DB for annotated_node_id lookup \
                         — falling back to target_id"
                    );
                    return None;
                }
            };
            let mut seen = std::collections::BTreeSet::new();
            for ev_id in &event_ids {
                let md: Option<Option<String>> = conn
                    .query_row(
                        "SELECT metadata_json FROM dadbear_observation_events
                          WHERE slug = ?1 AND id = ?2",
                        params![slug_read, ev_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .ok();
                let Some(Some(md_str)) = md else {
                    // Event not found or metadata_json null. A loud
                    // log — this is the only signal we'll see that the
                    // routing metadata got lost between emit + dispatch.
                    tracing::warn!(
                        work_item_id = %wi_id_read,
                        event_id = %ev_id,
                        slug = %slug_read,
                        "re_distill: observation event has no metadata_json — \
                         cannot extract annotated_node_id"
                    );
                    continue;
                };
                let parsed: Result<serde_json::Value, _> =
                    serde_json::from_str(&md_str);
                let Ok(val) = parsed else {
                    tracing::warn!(
                        work_item_id = %wi_id_read,
                        event_id = %ev_id,
                        slug = %slug_read,
                        "re_distill: observation event metadata_json is not \
                         valid JSON — cannot extract annotated_node_id"
                    );
                    continue;
                };
                if let Some(nid) =
                    val.get("annotated_node_id").and_then(|v| v.as_str())
                {
                    let trimmed = nid.trim();
                    if !trimmed.is_empty() {
                        seen.insert(trimmed.to_string());
                    }
                }
            }
            if seen.is_empty() {
                // Events exist but none carried annotated_node_id —
                // return Some(empty) so the helper logs the loud
                // warning + falls back. This is the "loud deferral"
                // case, NOT the quiet "no events at all" fallback.
                tracing::warn!(
                    work_item_id = %wi_id_read,
                    slug = %slug_read,
                    target_id = %target_id_read,
                    event_count = event_ids.len(),
                    "re_distill: no annotated_node_id found in any observation \
                     event metadata — cascade_annotations will fall back loudly"
                );
                Some(Vec::new())
            } else {
                Some(seen.into_iter().collect())
            }
        })
        .await
        .unwrap_or(None)
    } else {
        None
    };

    let supersession_result = crate::pyramid::stale_helpers_upper::execute_supersession(
        target_id,
        db_path,
        slug,
        config,
        model,
        annotated_node_ids,
    )
    .await;

    match supersession_result {
        Ok(new_canonical_id) => {
            info!(
                work_item_id = %wi_id,
                target_id = %target_id,
                new_canonical_id = %new_canonical_id,
                slug = %slug,
                "DADBEAR supervisor: re_distill complete via execute_supersession"
            );

            // CAS previewed → applied + emit node_re_distilled chronicle
            // breadcrumb + result_applications row.
            let db_path_rd = db_path.to_string();
            let wi_id_rd = wi_id.to_string();
            let slug_rd = slug.to_string();
            let target_id_rd = target_id.to_string();
            let event_bus_rd = event_bus.clone();
            let primitive_rd = primitive.to_string();
            let new_canonical_id_rd = new_canonical_id.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = Connection::open(&db_path_rd)?;
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                conn.execute(
                    "UPDATE dadbear_work_items
                     SET state = 'applied',
                         state_changed_at = ?1,
                         applied_at = ?1
                     WHERE id = ?2 AND state = 'previewed'",
                    params![now, wi_id_rd],
                )?;
                emit_state_changed(
                    &event_bus_rd, &slug_rd, &wi_id_rd, "previewed", "applied",
                );

                conn.execute(
                    "INSERT OR IGNORE INTO dadbear_result_applications
                     (work_item_id, slug, target_id, action, applied_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        wi_id_rd,
                        slug_rd,
                        target_id_rd,
                        format!("re_distilled:{}", new_canonical_id_rd),
                        now,
                    ],
                )?;

                // Chronicle breadcrumb for the original-bug fix.
                let metadata = serde_json::json!({
                    "triggering_work_item_id": wi_id_rd,
                    "source_primitive": primitive_rd,
                    "new_canonical_id": new_canonical_id_rd,
                })
                .to_string();
                let _ = observation_events::write_observation_event(
                    &conn,
                    &slug_rd,
                    "dadbear",
                    "node_re_distilled",
                    None, None, None, None,
                    Some(&target_id_rd),
                    Some(layer),
                    Some(&metadata),
                );
                Ok(())
            })
            .await
            .context("spawn_blocking join error for re_distill apply")??;
            Ok(())
        }
        Err(e) => {
            warn!(
                work_item_id = %wi_id,
                target_id = %target_id,
                error = %e,
                slug = %slug,
                "DADBEAR supervisor: re_distill via execute_supersession failed"
            );
            // CAS previewed → failed so we don't spin on this row every
            // tick. execute_supersession already persists a failed-manifest
            // oversight row when the LLM call produces unusable output.
            let db_path_rd = db_path.to_string();
            let wi_id_rd = wi_id.to_string();
            let slug_rd = slug.to_string();
            let event_bus_rd = event_bus.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = Connection::open(&db_path_rd)?;
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                conn.execute(
                    "UPDATE dadbear_work_items
                     SET state = 'failed',
                         state_changed_at = ?1
                     WHERE id = ?2 AND state = 'previewed'",
                    params![now, wi_id_rd],
                )?;
                emit_state_changed(
                    &event_bus_rd, &slug_rd, &wi_id_rd, "previewed", "failed",
                );
                Ok(())
            })
            .await
            .context("spawn_blocking join error for re_distill fail")??;
            let slug_err = slug.to_string();
            let target_err = target_id.to_string();
            Err(e.context(format!(
                "re_distill via execute_supersession failed for target '{}' in slug '{}'",
                target_err, slug_err,
            )))
        }
    }
}

// ── LLM result parsers ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum StaleCheckDecisionKind {
    Stale,
    Pass,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleCheckDecision {
    kind: StaleCheckDecisionKind,
    reason: String,
}

fn parse_stale_check_decision(content: &str, target_id: &str) -> StaleCheckDecision {
    // Try to extract JSON from the response.
    let json_val = match super::llm::extract_json(content) {
        Ok(v) => v,
        Err(_) => {
            // If we can't parse JSON, check for obvious indicators.
            let lower = content.to_lowercase();
            if lower.contains("\"decision\": \"skip\"")
                || lower.contains("\"decision\":\"skip\"")
                || lower.contains("\"decision\": \"skipped\"")
                || lower.contains("\"decision\":\"skipped\"")
            {
                return StaleCheckDecision {
                    kind: StaleCheckDecisionKind::Skip,
                    reason: content.trim().to_string(),
                };
            }
            if lower.contains("\"stale\": true") || lower.contains("\"stale\":true") {
                return StaleCheckDecision {
                    kind: StaleCheckDecisionKind::Stale,
                    reason: content.trim().to_string(),
                };
            }
            warn!(
                target_id = %target_id,
                "parse_stale_check_result: could not parse LLM response as JSON, defaulting to stale"
            );
            return StaleCheckDecision {
                kind: StaleCheckDecisionKind::Stale,
                reason: "LLM stale check response was not parseable".to_string(),
            };
        }
    };

    // Response could be an array or a single object.
    let entries = if json_val.is_array() {
        json_val.as_array().cloned().unwrap_or_default()
    } else {
        vec![json_val]
    };

    // Find matching entry.
    let matching = entries
        .iter()
        .find(|e| {
            e.get("file_path")
                .and_then(|v| v.as_str())
                .map(|s| s == target_id)
                .unwrap_or(false)
                || e.get("node_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s == target_id)
                    .unwrap_or(false)
        })
        .or_else(|| entries.first());

    let Some(entry) = matching else {
        return StaleCheckDecision {
            kind: StaleCheckDecisionKind::Stale,
            reason: "LLM stale check response had no decision entries".to_string(),
        };
    };

    let reason = entry
        .get("reason")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("LLM stale check for {target_id} (reason not parseable)"));

    let explicit_decision = entry
        .get("decision")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase().replace('-', "_"));

    let kind = match explicit_decision.as_deref() {
        Some("skip") | Some("skipped") => StaleCheckDecisionKind::Skip,
        Some("pass") | Some("passed") | Some("current") | Some("not_stale")
        | Some("not stale") | Some("no") => StaleCheckDecisionKind::Pass,
        Some("stale") | Some("yes") => StaleCheckDecisionKind::Stale,
        _ => {
            if entry.get("stale").and_then(|v| v.as_bool()).unwrap_or(true) {
                StaleCheckDecisionKind::Stale
            } else {
                StaleCheckDecisionKind::Pass
            }
        }
    };

    StaleCheckDecision { kind, reason }
}

#[cfg(test)]
mod stale_check_decision_tests {
    use super::*;

    #[test]
    fn stale_check_decision_parses_llm_skip_reason_verbatim() {
        let decision = parse_stale_check_decision(
            r#"[{"node_id":"L1-skip","decision":"skip","stale":false,"reason":"LLM confirmed duplicate live thread."}]"#,
            "L1-skip",
        );

        assert_eq!(decision.kind, StaleCheckDecisionKind::Skip);
        assert_eq!(decision.reason, "LLM confirmed duplicate live thread.");
    }

    #[test]
    fn stale_check_decision_keeps_legacy_stale_boolean() {
        let decision = parse_stale_check_decision(
            r#"[{"node_id":"L1-pass","stale":false,"reason":"No semantic change."}]"#,
            "L1-pass",
        );

        assert_eq!(decision.kind, StaleCheckDecisionKind::Pass);
        assert_eq!(decision.reason, "No semantic change.");
    }
}

/// Parse a rename check LLM response to determine if a rename occurred.
///
/// The response is expected to be JSON: `{"rename": true/false, "reason": "..."}`
/// Returns true if the file was renamed.
fn parse_rename_result(content: &str) -> bool {
    let json_val = match super::llm::extract_json(content) {
        Ok(v) => v,
        Err(_) => {
            let lower = content.to_lowercase();
            if lower.contains("\"rename\": true") || lower.contains("\"rename\":true") {
                return true;
            }
            return false; // Default to not-a-rename when uncertain (safe choice).
        }
    };

    json_val
        .get("rename")
        .and_then(|v| v.as_bool())
        .unwrap_or(false) // Default to false (safe — creates tombstone + ingest).
}

/// Parse the "reason" field from a rename check LLM response.
///
/// Falls back to a generic message if the JSON cannot be parsed.
fn parse_rename_reason(content: &str) -> String {
    super::llm::extract_json(content)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_else(|| "LLM rename check (reason not parseable)".to_string())
}

/// Extract `(old_path, new_path)` for a rename_candidate work item.
///
/// Tries two sources in order:
/// 1. The observation event's `metadata_json` (has `old_path` / `new_path` keys).
/// 2. The work item's `target_id` format: `rename/{old_path}/{new_path}`.
fn extract_rename_paths(
    db_path: &str,
    observation_event_ids_json: Option<&str>,
    target_id: &str,
) -> Option<(String, String)> {
    // Strategy 1: observation event metadata.
    if let Some(ids_json) = observation_event_ids_json {
        if let Ok(ids) = serde_json::from_str::<Vec<i64>>(ids_json) {
            if let Some(&first_id) = ids.first() {
                if let Ok(conn) = Connection::open(db_path) {
                    if let Ok(Some(meta)) = conn.query_row(
                        "SELECT metadata_json FROM dadbear_observation_events WHERE id = ?1",
                        params![first_id],
                        |row| row.get::<_, Option<String>>(0),
                    ) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&meta) {
                            let old = parsed
                                .get("old_path")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let new = parsed
                                .get("new_path")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            if let (Some(o), Some(n)) = (old, new) {
                                return Some((o, n));
                            }
                        }
                    }
                }
            }
        }
    }

    // Strategy 2: parse from target_id format.
    parse_rename_target_id(target_id)
}

/// Parse the rename target_id format: `rename/{old_path}/{new_path}`.
///
/// Both paths are absolute (start with `/`), so the string is
/// `rename/{abs_old}/{abs_new}`. We find the boundary by looking for
/// a path separator pattern.
fn parse_rename_target_id(target_id: &str) -> Option<(String, String)> {
    let rest = target_id.strip_prefix("rename/")?;

    // Both paths are absolute on macOS/Linux (start with `/`).
    // The boundary between old and new is where a `/` is followed by another
    // absolute path root. We scan for known root prefixes.
    let roots = [
        "/Users/",
        "/home/",
        "/tmp/",
        "/var/",
        "/opt/",
        "/etc/",
        "/private/",
    ];

    // Skip the first character (the leading `/` of old_path) and look for the
    // start of the second absolute path.
    for (i, _) in rest.char_indices().skip(1) {
        for root in &roots {
            if rest[i..].starts_with(root) {
                let old_path = &rest[..i];
                let new_path = &rest[i..];
                return Some((old_path.to_string(), new_path.to_string()));
            }
        }
    }

    None
}

/// Retention pass: delete observation events older than the retention window
/// that are below all compilation cursors.
fn run_retention_pass(conn: &Connection, retention_days: i64) -> Result<()> {
    let cutoff = (Utc::now() - chrono::Duration::days(retention_days))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    // Find the minimum compilation cursor across all slugs.
    let min_cursor: i64 = conn
        .query_row(
            "SELECT COALESCE(MIN(last_compiled_observation_id), 0)
             FROM dadbear_compilation_state",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Delete events older than cutoff AND below all cursors.
    let deleted = conn.execute(
        "DELETE FROM dadbear_observation_events
         WHERE detected_at < ?1 AND id < ?2",
        params![cutoff, min_cursor],
    )?;

    if deleted > 0 {
        info!(
            deleted = deleted,
            cutoff = %cutoff,
            min_cursor = min_cursor,
            "DADBEAR supervisor: retention pass completed"
        );
    }

    Ok(())
}
