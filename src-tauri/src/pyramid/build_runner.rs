// pyramid/build_runner.rs — Unified build runner
//
// Single entry point for all pyramid builds (routes.rs HTTP handler, main.rs
// Tauri command, and any future callers).  Dispatches to the chain engine or
// legacy build functions based on the `use_chain_engine` feature flag on
// PyramidState.
//
// See docs/plans/action-chain-refactor-v3.md Phase 5.

use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::build::{self, WriteOp};
use super::chain_executor;
use super::chain_loader;
use super::chain_registry;
use super::characterize;
use super::db;
use super::defaults_adapter;
use super::question_decomposition::{
    self, DecompositionConfig, DecompositionPreview, QuestionTree,
};
use super::slug;
use super::types::{
    BuildProgress, CharacterizationResult, ContentType, HandlePath, LayerEvent,
    RemoteWebEdge,
};
use super::vine_composition;
use super::wire_import::RemotePyramidClient;
use super::PyramidState;

use std::collections::HashMap;

// ── WS-ONLINE-G: Absorption build rate limiting ─────────────────────

/// Check whether an external operator is allowed to trigger an absorb-all build
/// on this slug. Enforces per-operator hourly rate limit and daily spend cap.
///
/// Returns `Ok(())` if allowed, or `Err` with a 429-style message if rate limited.
///
/// `estimated_cost` is the estimated credit cost of the build (0 if unknown).
pub async fn check_absorption_rate_limit(
    state: &PyramidState,
    slug_name: &str,
    operator_id: &str,
    estimated_cost: u64,
) -> Result<()> {
    // Check absorption mode
    let mode = {
        let conn = state.reader.lock().await;
        let (mode, _chain_id) = db::get_absorption_mode(&conn, slug_name)?;
        mode
    };

    if mode != "absorb-all" {
        // Not absorb-all — no rate limiting needed (open = requester pays, selective = chain decides)
        return Ok(());
    }

    // Read rate limits from config
    let (max_per_hour, daily_cap) = if let Some(ref data_dir) = state.data_dir {
        let cfg = super::PyramidConfig::load(data_dir);
        (
            cfg.absorption_rate_limit_per_operator,
            cfg.absorption_daily_spend_cap,
        )
    } else {
        (3u32, 100u64)
    };

    // Read rate limit window durations from operational config
    let hourly_window_secs = state.operational.tier2.rate_limit_hourly_window_secs;
    let daily_window_secs = state.operational.tier2.rate_limit_daily_window_secs;

    // ── Atomic check of both limits under a single lock ────────────────
    // Acquiring one Mutex for both the hourly rate limit and the daily spend
    // cap eliminates the TOCTOU race: if either check fails, neither counter
    // is incremented.
    {
        let mut gate = state.absorption_gate.lock().await;
        let now = std::time::Instant::now();

        // --- Per-operator hourly rate limit ---
        let hourly_entry = gate.hourly.entry(operator_id.to_string()).or_insert((0, now));
        let hourly_elapsed = now.duration_since(hourly_entry.1);

        let (new_hourly_count, new_hourly_start) =
            if hourly_elapsed > std::time::Duration::from_secs(hourly_window_secs) {
                // Window expired — will reset to 1 on commit
                (1u32, now)
            } else if hourly_entry.0 >= max_per_hour {
                let retry_after = hourly_window_secs - hourly_elapsed.as_secs();
                return Err(anyhow!(
                    "429: absorption build rate limit exceeded for operator '{}' on slug '{}'. \
                     Limit: {} builds/hour. Retry after {}s",
                    operator_id,
                    slug_name,
                    max_per_hour,
                    retry_after
                ));
            } else {
                (hourly_entry.0 + 1, hourly_entry.1)
            };

        // --- Global daily spend cap ---
        let daily_elapsed = now.duration_since(gate.daily.1);

        let (new_daily_spend, new_daily_start) =
            if daily_elapsed > std::time::Duration::from_secs(daily_window_secs) {
                // Day expired — will reset to estimated_cost on commit
                (estimated_cost, now)
            } else if gate.daily.0 + estimated_cost > daily_cap {
                let retry_after = daily_window_secs - daily_elapsed.as_secs();
                return Err(anyhow!(
                    "429: absorption daily spend cap exceeded for slug '{}'. \
                     Cap: {} credits/day, spent: {}. Retry after {}s",
                    slug_name,
                    daily_cap,
                    gate.daily.0,
                    retry_after
                ));
            } else {
                (gate.daily.0 + estimated_cost, gate.daily.1)
            };

        // Both checks passed — commit both increments atomically
        gate.hourly.insert(operator_id.to_string(), (new_hourly_count, new_hourly_start));
        gate.daily = (new_daily_spend, new_daily_start);
    }

    info!(
        slug = slug_name,
        operator_id = operator_id,
        estimated_cost = estimated_cost,
        "Absorption build rate check passed"
    );

    Ok(())
}

/// Unified build runner — dispatches to the chain engine or legacy build
/// pipeline based on the `use_chain_engine` feature flag.
///
/// Returns `(status_string, failure_count)`.  For the legacy path the status
/// string is always `"legacy"`.  For the chain engine path it is the apex
/// node ID produced by `execute_chain`.
///
/// Callers (routes.rs, main.rs) are responsible for:
/// - active_build guard / conflict detection
/// - cancellation token creation
/// - spawning the writer drain task
/// - progress status bookkeeping
///
/// This function only does the actual build work.
pub async fn run_build(
    state: &PyramidState,
    slug_name: &str,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)> {
    run_build_from(state, slug_name, 0, None, None, cancel, progress_tx, write_tx, layer_tx).await
}

/// Run a build from a specific depth, reusing nodes below that depth.
/// Defaults to evidence_mode "deep" (full evidence loop + gap processing).
pub async fn run_build_from(
    state: &PyramidState,
    slug_name: &str,
    from_depth: i64,
    stop_after: Option<&str>,
    force_from: Option<&str>,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)> {
    run_build_from_with_evidence_mode(
        state, slug_name, from_depth, stop_after, force_from,
        "deep", cancel, progress_tx, write_tx, layer_tx,
    ).await
}

/// Run a build from a specific depth with explicit evidence_mode control.
/// "deep" = full evidence loop + gap processing (default behavior).
/// "fast" = skip evidence loop, defer to query-time demand-gen.
pub async fn run_build_from_with_evidence_mode(
    state: &PyramidState,
    slug_name: &str,
    from_depth: i64,
    stop_after: Option<&str>,
    force_from: Option<&str>,
    evidence_mode: &str,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    // walker-v3 W3a: `write_tx` was consumed by the retired
    // `run_legacy_build` only — chain engine manages its own writer
    // drain. Kept in the signature to preserve the public surface until
    // downstream callers (parity.rs, dadbear_extend.rs, routes.rs,
    // main.rs) can be updated in a separate pass.
    _write_tx: &mpsc::Sender<WriteOp>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)> {
    // ── 0. WS-CONCURRENCY (§15.16 races 1/3/7): serialize builds on the
    // same slug. Two builds, demand-gen vs build, and demand-gen vs
    // stale-refresh all contend for this write lock. Acquire BEFORE any
    // DB work; the guard is held across the entire build and released on
    // drop / cancellation / panic.
    let _slug_write_guard = super::lock_manager::LockManager::global()
        .write(slug_name)
        .await;

    // ── 1. Determine content type ────────────────────────────────────────
    let content_type = {
        let conn = state.reader.lock().await;
        slug::get_slug(&conn, slug_name)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?
            .content_type
    };

    // Phase 16: vine builds now flow through this path via the
    // topical-vine chain. Legacy conversation-only bunch vine builds
    // still go through vine::build_vine (session ingestion, bunches,
    // etc.), but vine-of-vines compositions driven by the chain
    // executor rebuild through `run_chain_build`. See
    // docs/specs/vine-of-vines-and-folder-ingestion.md.

    // ── Question slug dispatch ──────────────────────────────────────────
    // Question slugs route through run_decomposed_build, loading nodes from
    // referenced slugs instead of from their own source path.
    if content_type == ContentType::Question {
        // Retrieve the stored apex question and config from the question tree
        let (apex_question, stored_granularity, stored_max_depth) = {
            let conn = state.reader.lock().await;
            let tree_json = db::get_question_tree(&conn, slug_name)?.ok_or_else(|| {
                anyhow!(
                    "Question slug '{}' has no stored question tree. \
                     Use the question build endpoint to set the initial question.",
                    slug_name
                )
            })?;
            let tree: question_decomposition::QuestionTree = serde_json::from_value(tree_json)?;
            (
                tree.config.apex_question.clone(),
                tree.config.granularity,
                tree.config.max_depth,
            )
        };

        let result = Box::pin(run_decomposed_build(
            state,
            slug_name,
            &apex_question,
            stored_granularity,
            stored_max_depth,
            from_depth,
            None, // re-characterize from cross-slug nodes
            evidence_mode,
            cancel,
            progress_tx,
            layer_tx,
        ))
        .await;

        // Phase 16 wanderer fix: question builds also get the post-build
        // hooks. A question pyramid may be a child of a vine (via
        // pyramid_vine_compositions), and its referrers need stale-mark
        // notifications too.
        run_post_build_hooks(state, slug_name, &result).await;
        return result;
    }

    // ── Conversation dispatch ──────────────────────────────────────────
    // Conversations use the question pipeline with a default apex question.
    // The conversation.yaml chain provides conversation-tuned extraction
    // while reusing the full question decomposition → evidence → gap pipeline.
    if content_type == ContentType::Conversation {
        // Check for stored question tree first (re-build case)
        let (apex_question, stored_granularity, stored_max_depth) = {
            let conn = state.reader.lock().await;
            match db::get_question_tree(&conn, slug_name)? {
                Some(tree_json) => {
                    let tree: question_decomposition::QuestionTree =
                        serde_json::from_value(tree_json)?;
                    (
                        tree.config.apex_question.clone(),
                        tree.config.granularity,
                        tree.config.max_depth,
                    )
                }
                None => {
                    // First build — use default conversation question
                    (
                        "What happened during this conversation? What was discussed, \
                         what decisions were made, how did the discussion evolve, \
                         and what are the key takeaways?".to_string(),
                        3u32,  // balanced granularity
                        3u32,  // reasonable depth for conversations
                    )
                }
            }
        };

        let result = Box::pin(run_decomposed_build(
            state,
            slug_name,
            &apex_question,
            stored_granularity,
            stored_max_depth,
            from_depth,
            None,
            evidence_mode,
            cancel,
            progress_tx,
            layer_tx,
        ))
        .await;

        // Phase 16 wanderer fix: conversation builds also get the
        // post-build hooks. A conversation pyramid can be a child of a
        // vine (a vine composed of per-session conversation bedrocks).
        run_post_build_hooks(state, slug_name, &result).await;
        return result;
    }

    // ── 2. Check feature flags ───────────────────────────────────────────
    //
    // walker-v3 W3a: the `use_chain_engine: false` branch retired. The
    // chain engine is the only supported dispatch path per plan §5.6.3;
    // from_depth is now universally supported. `use_ir_executor` still
    // toggles between the IR executor (ExecutionPlan compile → execute)
    // and the chain executor. Configs that load as `use_chain_engine:
    // false` are handled by the boot-time intervention modal (Phase 0a-2
    // onboarding_state.chain_engine_enable_ack flow), not here.
    // TODO(walker-v3 W3c): decide the fate of the use_chain_engine
    // field on PyramidConfig — either (a) keep it for backward-compat
    // serde and always treat it as true at runtime, or (b) delete the
    // field entirely. W3c's field-deletion commit picks.
    let _ = state.use_chain_engine.load(Ordering::Relaxed); // read-only; value ignored
    let use_ir = state.use_ir_executor.load(Ordering::Relaxed);

    let result = if use_ir {
        // IR executor path: compile chain to ExecutionPlan, execute via execute_plan
        run_ir_build(
            state,
            slug_name,
            &content_type,
            from_depth,
            cancel,
            progress_tx,
        )
        .await
        .map(|(apex, failures)| (apex, failures, vec![]))
    } else {
        run_chain_build(
            state,
            slug_name,
            &content_type,
            from_depth,
            stop_after,
            force_from,
            cancel,
            progress_tx,
            layer_tx,
        )
        .await
    };

    run_post_build_hooks(state, slug_name, &result).await;
    result
}

/// Post-build hooks shared by every content type's build path.
///
/// 1. **WS8-F**: Cross-slug referrer notification. If the just-built slug
///    is referenced by other slugs via `pyramid_slug_references`, those
///    referrers get a `confirmed_stale` pending mutation so the stale
///    engine picks up the changes on its next tick.
/// 2. **Phase 16 wanderer fix**: Vine-of-vines propagation. If the
///    just-built slug is a child (bedrock or sub-vine) of any vine,
///    `notify_vine_of_child_completion` walks the composition hierarchy
///    recursively, updates each parent vine's apex reference, enqueues
///    change-manifest pending mutations, and emits DeltaLanded +
///    SlopeChanged on the event bus. Cycle-guarded and depth-capped.
///    Without this wire, a bedrock rebuild would not propagate to any
///    parent vine until an operator manually triggered
///    `/pyramid/:slug/vine/trigger-delta`.
/// 3. **WS-ONLINE-F**: Remote web edge resolution.
///
/// All three hooks are gated on `result.1 == 0` (zero failures) so only
/// clean builds trigger downstream work. All three are best-effort and
/// non-fatal — a hook failure is logged but does not fail the build.
async fn run_post_build_hooks(
    state: &PyramidState,
    slug_name: &str,
    result: &Result<(String, i32, Vec<super::types::StepActivity>)>,
) {
    // ── WS8-F: Notify cross-slug referrers on successful build ──────────
    if let Ok(ref res) = result {
        if res.1 == 0 {
            let writer = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let notify_result = tokio::task::spawn_blocking(move || {
                let conn = writer.blocking_lock();
                let referrers = db::get_slug_referrers(&conn, &slug_owned)?;
                if referrers.is_empty() {
                    return Ok::<usize, anyhow::Error>(0);
                }
                let now = chrono::Utc::now().to_rfc3339();
                let detail = serde_json::json!({
                    "reason": "base_slug_rebuilt",
                    "source_slug": slug_owned,
                }).to_string();
                let mut notified = 0usize;
                for referrer in &referrers {
                    // Canonical write: observation event (old WAL INSERT removed)
                    let _ = super::observation_events::write_observation_event(
                        &conn,
                        referrer,
                        "vine",
                        "vine_stale",
                        None,
                        None,
                        None,
                        None,
                        Some(slug_owned.as_str()),
                        Some(0),
                        Some(&detail),
                    );

                    notified += 1;
                }
                info!(
                    source_slug = slug_owned.as_str(),
                    referrer_count = notified,
                    "notified cross-slug referrers of base rebuild"
                );
                Ok(notified)
            })
            .await;

            if let Err(e) = notify_result {
                warn!(
                    slug = slug_name,
                    error = %e,
                    "failed to notify cross-slug referrers (non-fatal)"
                );
            }
        }
    }

    // ── Phase 16: Vine-of-vines propagation ──────────────────────────────
    if let Ok(ref res) = result {
        if res.1 == 0 && !res.0.is_empty() {
            let apex_id = res.0.clone();
            if let Err(e) = vine_composition::notify_vine_of_child_completion(
                state,
                slug_name,
                &format!("build-{}-{}", slug_name, chrono::Utc::now().timestamp()),
                &apex_id,
            )
            .await
            {
                warn!(
                    slug = slug_name,
                    apex = %apex_id,
                    error = %e,
                    "failed to propagate vine-of-vines change (non-fatal)"
                );
            }
        }
    }

    // ── WS-ONLINE-F: Resolve remote web edges ─────────────────────────────
    if let Ok(ref res) = result {
        if res.1 == 0 {
            if let Err(e) = resolve_remote_web_edges(state, slug_name).await {
                warn!(
                    slug = slug_name,
                    error = %e,
                    "failed to resolve remote web edges (non-fatal)"
                );
            }
        }
    }

    // ── EVENT_BUILD_NETWORK_CONTRIBUTION emit ──────────────────────────────
    //
    // Aggregates the build's network-dispatched calls into a single
    // chronicle summary row so the UI can render "built in N seconds
    // using M network GPUs" without scanning the full timeline. Runs
    // unconditionally (including on failure + zero-network builds) so
    // every build emits exactly one row — absence never means zero.
    //
    // 2s flush barrier lets the fire-and-forget chronicle writes from
    // the market branch settle before we aggregate. Individual writes
    // complete <10ms under normal load; 2s is a ~200x safety margin.
    // See `docs/plans/call-model-unified-market-integration.md` §4.7.
    if let Some(data_dir) = state.data_dir.as_ref() {
        let db_path = data_dir.join("pyramid.db").to_string_lossy().to_string();
        let slug_owned = slug_name.to_string();
        // NOTE: the first element of the Ok tuple is the apex node id,
        // NOT the chronicle build_id. Chronicle rows carry their own
        // UUID build_id allocated inside the chain executor; the outer
        // Result does not expose it. We resolve the real build_id in
        // the aggregation block below via a subquery against
        // pyramid_compute_events for this slug. Build failures still
        // emit a zeroed BUILD_NETWORK_CONTRIBUTION for the same slug
        // with build_id NULL so the "absence never means zero"
        // invariant from §7.6 holds.
        let build_succeeded = result.is_ok();
        tokio::task::spawn_blocking(move || {
            // Fire-and-forget: spawn returns immediately; any error is
            // a chronicle-write failure and does not affect the build.
            std::thread::sleep(std::time::Duration::from_secs(2));

            let conn = match rusqlite::Connection::open(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "build_network_contribution: open chronicle conn failed"
                    );
                    return;
                }
            };

            // Resolve the actual build_id for the just-completed build.
            // Pick the build_id with the most recent event for this
            // slug. Tester-scale serial builds land on the correct one;
            // pathologically concurrent same-slug builds could pick a
            // still-in-progress one — documented limitation, not a real
            // concern at tester scale.
            let resolved_build_id: Option<String> = conn
                .query_row(
                    "SELECT build_id FROM pyramid_compute_events
                     WHERE slug = ?1 AND build_id IS NOT NULL
                     ORDER BY timestamp DESC LIMIT 1",
                    rusqlite::params![slug_owned],
                    |r| r.get::<_, String>(0),
                )
                .ok();

            // Aggregate query — defensive COALESCE for empty-set safety.
            // SQLite's COUNT returns 0 for empty sets; SUM/AVG return
            // NULL which COALESCE maps to 0. SQLite 3.30+ supports FILTER.
            // local_calls counts source='local' rows (actual pool-served
            // executions) not network_fell_back_local attempts, per the
            // plan §4.7 metadata semantics (total_llm_calls = network +
            // local + openrouter).
            let (network_calls, distinct_providers, avg_latency_ms,
                 total_credits_spent, local_calls, openrouter_calls,
                 total_llm_calls) = match &resolved_build_id {
                Some(bid) => {
                    let sql = "
                        SELECT
                          COUNT(*) FILTER (WHERE event_type = 'network_helped_build') AS network_calls,
                          COUNT(DISTINCT json_extract(metadata, '$.provider_node_id'))
                            FILTER (WHERE event_type = 'network_helped_build') AS distinct_providers,
                          COALESCE(
                            AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
                              FILTER (WHERE event_type = 'network_result_returned'),
                            0.0
                          ) AS avg_network_latency_ms,
                          COALESCE(
                            SUM(CAST(json_extract(metadata, '$.reservation_held') AS INTEGER))
                              FILTER (WHERE event_type = 'network_helped_build'),
                            0
                          ) AS total_credits_spent,
                          COUNT(*) FILTER (WHERE source = 'local') AS local_calls,
                          COUNT(*) FILTER (WHERE source = 'cloud' AND event_type = 'cloud_returned') AS openrouter_calls,
                          COUNT(*) AS total_llm_calls
                        FROM pyramid_compute_events
                        WHERE slug = ?1 AND build_id = ?2
                    ";
                    match conn.query_row(
                        sql,
                        rusqlite::params![slug_owned, bid],
                        |r| Ok((
                            r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?,
                            r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?,
                            r.get::<_, i64>(6)?,
                        )),
                    ) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                slug = %slug_owned,
                                build_id = %bid,
                                "build_network_contribution: aggregation query failed"
                            );
                            (0, 0, 0.0, 0, 0, 0, 0)
                        }
                    }
                }
                // No chronicle rows for this slug (zero-LLM build or
                // build that failed before any dispatch). Emit zeros.
                None => (0, 0, 0.0, 0, 0, 0, 0),
            };

            let build_id_for_event = resolved_build_id
                .clone()
                .unwrap_or_else(|| format!("{}-no-events", slug_owned));
            let job_path = format!(
                "{}:{}",
                super::compute_chronicle::SOURCE_NETWORK, build_id_for_event
            );
            let chronicle_ctx = super::compute_chronicle::ChronicleEventContext::minimal(
                &job_path,
                super::compute_chronicle::EVENT_BUILD_NETWORK_CONTRIBUTION,
                super::compute_chronicle::SOURCE_NETWORK,
            );
            let chronicle_ctx = super::compute_chronicle::ChronicleEventContext {
                slug: Some(slug_owned.clone()),
                build_id: resolved_build_id.clone(),
                ..chronicle_ctx
            };
            let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
                "build_id": resolved_build_id,
                "slug": slug_owned,
                "build_succeeded": build_succeeded,
                "total_llm_calls": total_llm_calls,
                "network_calls": network_calls,
                "local_calls": local_calls,
                "openrouter_calls": openrouter_calls,
                "distinct_providers": distinct_providers,
                "avg_network_latency_ms": avg_latency_ms,
                "total_credits_spent": total_credits_spent,
            }));

            if let Err(e) = super::compute_chronicle::record_event(&conn, &chronicle_ctx) {
                tracing::warn!(
                    error = %e,
                    slug = %slug_owned,
                    "build_network_contribution: record_event failed"
                );
            }
        });
    }
}

/// WS-ONLINE-F: Resolve remote web edges created during a build.
///
/// For each remote web edge, uses `RemotePyramidClient` to fetch the referenced
/// node's content from the remote pyramid. Results are cached in-memory for the
/// build session and logged. If a remote node cannot be reached, a gap report
/// could be published (future: integrated with wire_publish).
async fn resolve_remote_web_edges(state: &PyramidState, slug_name: &str) -> Result<()> {
    // Load all remote web edges for this slug
    let remote_edges: Vec<RemoteWebEdge> = {
        let conn = state.reader.lock().await;
        db::get_all_remote_web_edges(&conn, slug_name)?
    };

    if remote_edges.is_empty() {
        return Ok(());
    }

    info!(
        slug = slug_name,
        edge_count = remote_edges.len(),
        "resolving remote web edges"
    );

    // Get Wire auth for remote requests
    let config = state.config.read().await;
    let wire_jwt = config.auth_token.clone();
    drop(config);

    let wire_server_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());

    // Group edges by remote tunnel URL to reuse clients
    let mut clients: HashMap<String, RemotePyramidClient> = HashMap::new();
    let mut resolved = 0usize;
    let mut failed = 0usize;

    for edge in &remote_edges {
        if edge.remote_tunnel_url.is_empty() {
            warn!(
                slug = slug_name,
                remote_handle_path = edge.remote_handle_path.as_str(),
                "remote web edge has no tunnel URL, skipping"
            );
            failed += 1;
            continue;
        }

        let handle = match HandlePath::parse(&edge.remote_handle_path) {
            Some(h) => h,
            None => {
                warn!(
                    slug = slug_name,
                    remote_handle_path = edge.remote_handle_path.as_str(),
                    "failed to parse remote handle-path, skipping"
                );
                failed += 1;
                continue;
            }
        };

        // Get or create client for this tunnel URL
        let client = clients
            .entry(edge.remote_tunnel_url.clone())
            .or_insert_with(|| {
                RemotePyramidClient::new(
                    edge.remote_tunnel_url.clone(),
                    wire_jwt.clone(),
                    wire_server_url.clone(),
                )
            });

        // Fetch the remote node content via drill endpoint
        match client.remote_drill(&handle.slug, &handle.node_id).await {
            Ok(_drill_response) => {
                info!(
                    slug = slug_name,
                    remote = edge.remote_handle_path.as_str(),
                    "resolved remote web edge"
                );
                // The drill response data is available for downstream consumers.
                // Future: cache this in a local table or in-memory store for
                // use during publication and evidence resolution.
                resolved += 1;
            }
            Err(e) => {
                warn!(
                    slug = slug_name,
                    remote = edge.remote_handle_path.as_str(),
                    error = %e,
                    "failed to resolve remote web edge"
                );
                failed += 1;
            }
        }
    }

    info!(
        slug = slug_name,
        resolved = resolved,
        failed = failed,
        total = remote_edges.len(),
        "remote web edge resolution complete"
    );

    Ok(())
}

/// Chain-engine path: load chain YAML, execute via chain_executor.
async fn run_chain_build(
    state: &PyramidState,
    slug_name: &str,
    content_type: &ContentType,
    from_depth: i64,
    stop_after: Option<&str>,
    force_from: Option<&str>,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)> {
    let ct_str = content_type.as_str();

    // Three-tier chain resolution: per-slug override → content-type default → safety net.
    let chain_id = {
        let conn = state.reader.lock().await;
        chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str, "deep")?
    };

    // Use the pre-resolved chains directory from state
    let chains_dir = state.chains_dir.clone();

    // Discover all chain files and find the one matching our chain_id
    let all_chains = chain_loader::discover_chains(&chains_dir)?;
    let meta = all_chains
        .iter()
        .find(|m| m.id == chain_id)
        .ok_or_else(|| {
            anyhow!(
                "chain '{}' not found in chains directory ({})",
                chain_id,
                chains_dir.display()
            )
        })?;

    let yaml_path = std::path::Path::new(&meta.file_path);
    let chain = chain_loader::load_chain(yaml_path, &chains_dir)?;

    info!(
        slug = slug_name,
        chain = %chain.id,
        steps = chain.steps.len(),
        "starting chain engine build"
    );

    chain_executor::execute_chain_from(state, &chain, slug_name, from_depth, stop_after, force_from, cancel, progress_tx, layer_tx, None)
        .await
}

/// IR executor path: load chain YAML, compile to ExecutionPlan, execute via execute_plan.
async fn run_ir_build(
    state: &PyramidState,
    slug_name: &str,
    content_type: &ContentType,
    from_depth: i64,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<(String, i32)> {
    let ct_str = content_type.as_str();

    // Three-tier chain resolution: per-slug override → content-type default → safety net.
    let chain_id = {
        let conn = state.reader.lock().await;
        chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str, "deep")?
    };

    // Use the pre-resolved chains directory from state
    let chains_dir = state.chains_dir.clone();

    // Discover all chain files and find the one matching our chain_id
    let all_chains = chain_loader::discover_chains(&chains_dir)?;
    let meta = all_chains
        .iter()
        .find(|m| m.id == chain_id)
        .ok_or_else(|| {
            anyhow!(
                "chain '{}' not found in chains directory ({})",
                chain_id,
                chains_dir.display()
            )
        })?;

    let yaml_path = std::path::Path::new(&meta.file_path);
    let chain = chain_loader::load_chain(yaml_path, &chains_dir)?;

    // Compile to ExecutionPlan
    let plan = defaults_adapter::compile_defaults(&chain)?;

    info!(
        slug = slug_name,
        chain = %chain.id,
        ir_steps = plan.steps.len(),
        estimated_nodes = plan.total_estimated_nodes,
        "starting IR executor build"
    );

    chain_executor::execute_plan(state, &plan, slug_name, from_depth, cancel, progress_tx).await
}

// walker-v3 W3a: `run_legacy_build` retired. The chain engine is the
// only supported dispatch path per plan §5.6.3; the `use_chain_engine:
// false` branch in `run_build_from_with_evidence_mode` no longer exists
// and this function had no other callers. Legacy content-type
// dispatchers (`build_conversation` / `build_code` / `build_docs`) and
// their helpers (`build_l1_pairing`, `build_threads_layer`,
// `build_upper_layers`, `flatten_analysis`, `extract_import_graph`,
// `cluster_by_imports`, `truncate_text`, `get_resume_state`) were
// deleted with it; vines still route through `build::build_topical_vine`
// which is a chain-engine entry point.

/// Decomposed question build path: decompose apex question → question tree →
/// QuestionSet → IR → execute.
///
/// This is the P2.2 entry point. The caller provides a natural language question,
/// and the system decomposes it into sub-questions that shape the pyramid topology.
///
/// If `characterization` is Some, the provided characterization is used (user confirmed
/// or overrode the initial characterization). If None, characterize() is called
/// automatically before decomposition proceeds.
pub async fn run_decomposed_build(
    state: &PyramidState,
    slug_name: &str,
    apex_question: &str,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
    characterization: Option<CharacterizationResult>,
    evidence_mode: &str,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)> {
    // ── 1. Determine content type ──────────────────────────────────
    let (content_type, source_path) = {
        let conn = state.reader.lock().await;
        let slug_info = slug::get_slug(&conn, slug_name)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?;
        (slug_info.content_type, slug_info.source_path)
    };
    let ct_str = content_type.as_str();

    // ── 2. Resolve cross-slug references ───────────────────────────
    let referenced_slugs = {
        let conn = state.reader.lock().await;
        db::get_slug_references(&conn, slug_name)?
    };
    let is_cross_slug = !referenced_slugs.is_empty();

    // ── 2b. For question pyramids, resolve the base pyramid's source
    //        path and L0 nodes for characterization context. The question
    //        slug itself has empty source_path and zero L0 nodes.
    let (effective_source_path, effective_l0_slug) =
        if ct_str == "question" && !referenced_slugs.is_empty() {
            let base_slug = &referenced_slugs[0]; // first ref is always the base
            let conn = state.reader.lock().await;
            let base_info = slug::get_slug(&conn, base_slug)?
                .ok_or_else(|| anyhow!("Referenced base slug '{}' not found", base_slug))?;
            info!(
                slug = slug_name,
                base = %base_slug,
                base_source = %base_info.source_path,
                "question pyramid: using base pyramid for characterization"
            );
            (base_info.source_path, base_slug.clone())
        } else if ct_str == "question" {
            return Err(anyhow!(
                "Question pyramid '{}' has no base pyramid reference — cannot build",
                slug_name
            ));
        } else {
            (source_path.clone(), slug_name.to_string())
        };

    // ── 3. Characterize if not provided ────────────────────────────
    // Phase 12 verifier fix: attach cache_access so characterize retrofit
    // reaches the step cache.
    let llm_config = state
        .llm_config_with_cache(slug_name, &format!("question-build-{}", slug_name))
        .await;

    let characterization_result = match characterization {
        Some(c) => {
            info!(
                slug = slug_name,
                material_profile = %c.material_profile,
                "using provided characterization"
            );
            c
        }
        None => {
            info!(slug = slug_name, "running automatic characterization");
            // Build L0 summary fallback for characterization context
            // For question pyramids, use the base pyramid's L0 nodes
            let l0_fallback = {
                let conn = state.reader.lock().await;
                let existing_l0 = db::get_nodes_at_depth(&conn, &effective_l0_slug, 0)
                    .unwrap_or_default();
                if existing_l0.is_empty() {
                    None
                } else {
                    Some(
                        existing_l0
                            .iter()
                            .map(|n| {
                                let summary: String = n.distilled.chars().take(200).collect();
                                format!("- {}: {}", n.headline, summary)
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                }
            };
            characterize::characterize_with_fallback(
                &effective_source_path,
                apex_question,
                &llm_config,
                l0_fallback.as_deref(),
                &state.operational.tier1,
                Some(&state.chains_dir),
            )
            .await?
        }
    };

    // ── 4. Load pipeline chain (three-tier resolution) ────────────────
    // Previously this skipped the per-slug assignment check (tier 1) and
    // went directly to the hardcoded default. Now uses the consolidated
    // resolver so per-slug overrides work for decomposed builds too.
    let chain_id = {
        let conn = state.reader.lock().await;
        chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str, evidence_mode)?
    };
    let chains_dir = state.chains_dir.clone();
    let all_chains = chain_loader::discover_chains(&chains_dir)?;
    let meta = all_chains
        .iter()
        .find(|m| m.id == chain_id)
        .ok_or_else(|| {
            anyhow!(
                "'{}' chain not found in chains directory ({})",
                chain_id,
                chains_dir.display()
            )
        })?;
    let yaml_path = std::path::Path::new(&meta.file_path);
    let chain = chain_loader::load_chain(yaml_path, &chains_dir)?;

    info!(
        slug = slug_name,
        chain = %chain.id,
        steps = chain.steps.len(),
        "starting question pipeline build via chain executor"
    );

    // ── 5. Build initial context ───────────────────────────────────
    // These params become accessible as $apex_question, $granularity, etc.
    // in chain steps via ChainContext.initial_params
    let mut initial_context: HashMap<String, serde_json::Value> = HashMap::new();
    initial_context.insert("apex_question".to_string(), serde_json::json!(apex_question));
    initial_context.insert("granularity".to_string(), serde_json::json!(granularity));
    initial_context.insert("max_depth".to_string(), serde_json::json!(max_depth));
    initial_context.insert("from_depth".to_string(), serde_json::json!(from_depth));
    initial_context.insert("content_type".to_string(), serde_json::json!(ct_str));
    initial_context.insert("audience".to_string(), serde_json::json!(characterization_result.audience));
    initial_context.insert("characterize".to_string(), serde_json::json!(format!(
        "Material Profile: {}\nAudience: {}\nTone: {}",
        characterization_result.material_profile,
        characterization_result.audience,
        characterization_result.tone
    )));
    initial_context.insert("is_cross_slug".to_string(), serde_json::json!(is_cross_slug));
    initial_context.insert("referenced_slugs".to_string(), serde_json::json!(referenced_slugs));
    initial_context.insert("evidence_mode".to_string(), serde_json::json!(evidence_mode));

    // ── 6. Generate build_id and record build start ─────────────────
    // Create a build_id up front so that if the chain fails BEFORE
    // evidence_loop (at characterize, decompose, etc.) we still have
    // a build record in the database.
    let build_id = format!(
        "qb-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("0000")
    );

    // Record build start
    {
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let bid = build_id.clone();
        let q = apex_question.to_string();
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            super::local_store::save_build_start(&c, &slug_owned, &bid, &q, 0, Some(&q))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Build start save panicked: {e}"))??;
    }

    // Make build_id available to chain steps (evidence_loop uses it)
    initial_context.insert("build_id".to_string(), serde_json::json!(build_id));

    // ── 7. Execute the chain ───────────────────────────────────────
    let result = chain_executor::execute_chain_from(
        state,
        &chain,
        slug_name,
        from_depth,
        None,  // stop_after
        None,  // force_from
        cancel,
        progress_tx,
        layer_tx,
        Some(initial_context),
    )
    .await;

    match result {
        Ok((_, node_count, step_activities)) => {
            // Mark build complete
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let bid = build_id.clone();
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                super::local_store::complete_build(&c, &slug_owned, &bid, None)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Build complete save panicked: {e}"))??;

            info!(slug = slug_name, build_id = %build_id, node_count, "question pipeline build complete");
            Ok((build_id, node_count, step_activities))
        }
        Err(e) => {
            // Mark build failed
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let bid = build_id.clone();
            let err_msg = format!("{}", e);
            let _ = tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                super::local_store::fail_build(&c, &slug_owned, &bid, &err_msg)
            })
            .await;

            error!(slug = slug_name, error = %e, "question pipeline build failed");
            Err(e)
        }
    }
}

/// Preview a decomposed question build — returns the question tree and cost estimates
/// without actually building anything.
///
/// Used by the preview endpoint so the user can see what the decomposition will produce
/// before committing to the build.
pub async fn preview_decomposed_build(
    state: &PyramidState,
    slug_name: &str,
    apex_question: &str,
    granularity: u32,
    max_depth: u32,
) -> Result<(QuestionTree, DecompositionPreview)> {
    // ── 1. Determine content type and source path ────────────────────────
    let (content_type, source_path) = {
        let conn = state.reader.lock().await;
        let slug_info = slug::get_slug(&conn, slug_name)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?;
        (slug_info.content_type, slug_info.source_path)
    };

    let ct_str = content_type.as_str();

    // ── 1b. For question pyramids, resolve the base pyramid's source path
    //        and slug for L0 lookup (mirrors run_decomposed_build logic).
    let referenced_slugs = {
        let conn = state.reader.lock().await;
        db::get_slug_references(&conn, slug_name)?
    };
    let (effective_source_path, effective_l0_slug) =
        if ct_str == "question" && !referenced_slugs.is_empty() {
            let base_slug = &referenced_slugs[0];
            let conn = state.reader.lock().await;
            let base_info = slug::get_slug(&conn, base_slug)?
                .ok_or_else(|| anyhow!("Referenced base slug '{}' not found", base_slug))?;
            (base_info.source_path, base_slug.clone())
        } else if ct_str == "question" {
            return Err(anyhow!(
                "Question pyramid '{}' has no base pyramid reference — cannot preview",
                slug_name
            ));
        } else {
            (source_path.clone(), slug_name.to_string())
        };

    // ── 2. Build context from L0 summaries (aligned with actual build path) ──
    // The actual build uses L0 summaries, not folder_map. Align preview to
    // use the same context source so decomposition matches the real build.
    let decomp_context = {
        let conn = state.reader.lock().await;
        let base_l0 = db::get_nodes_at_depth(&conn, &effective_l0_slug, 0)?;
        if base_l0.is_empty() {
            // No base pyramid yet — fall back to folder map
            question_decomposition::build_folder_map(&effective_source_path)
        } else {
            let l0_context = base_l0
                .iter()
                .map(|n| {
                    let summary: String = n.distilled.chars().take(200).collect();
                    format!("- {}: {} — {}", n.id, n.headline, summary)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!(
                "Source material ({} extracted summaries from the base knowledge pyramid):\n{}",
                base_l0.len(),
                l0_context
            ))
        }
    };

    // ── 3. Decompose ─────────────────────────────────────────────────────
    let config = DecompositionConfig {
        apex_question: apex_question.to_string(),
        content_type: ct_str.to_string(),
        granularity,
        max_depth,
        folder_map: decomp_context,
        chains_dir: Some(state.chains_dir.clone()),
        audience: None,
    };

    // Phase 12 verifier fix: attach cache_access so question_decomposition
    // retrofit sites reach the step cache.
    let llm_config = state
        .llm_config_with_cache(slug_name, &format!("decompose-preview-{}", slug_name))
        .await;
    let tree = question_decomposition::decompose_question(
        &config,
        &llm_config,
        &state.operational.tier1,
        &state.operational.tier2,
    )
    .await?;

    // ── 4. Preview ───────────────────────────────────────────────────────
    let preview = question_decomposition::preview_decomposition(&tree);

    Ok((tree, preview))
}
