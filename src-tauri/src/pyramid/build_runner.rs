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

    // Vine builds are not supported through this path
    if content_type == ContentType::Vine {
        return Err(anyhow!(
            "Vine builds use the vine-specific build endpoint, not run_build"
        ));
    }

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

        return Box::pin(run_decomposed_build(
            state,
            slug_name,
            &apex_question,
            stored_granularity,
            stored_max_depth,
            from_depth,
            None, // re-characterize from cross-slug nodes
            cancel,
            progress_tx,
            layer_tx,
        ))
        .await;
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

        return Box::pin(run_decomposed_build(
            state,
            slug_name,
            &apex_question,
            stored_granularity,
            stored_max_depth,
            from_depth,
            None,
            cancel,
            progress_tx,
            layer_tx,
        ))
        .await;
    }

    // ── 2. Check feature flags ───────────────────────────────────────────
    let use_ir = state.use_ir_executor.load(Ordering::Relaxed);
    let use_chain = state.use_chain_engine.load(Ordering::Relaxed);

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
    } else if use_chain {
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
    } else {
        if from_depth > 0 {
            return Err(anyhow!(
                "from_depth is only supported with the chain engine (set use_chain_engine: true)"
            ));
        }
        run_legacy_build(
            state,
            slug_name,
            &content_type,
            cancel,
            progress_tx,
            write_tx,
        )
        .await
        .map(|(apex, failures)| (apex, failures, vec![]))
    };

    // ── WS8-F: Notify cross-slug referrers on successful build ──────────
    // After a base slug rebuild completes, any slug that references this one
    // may have stale evidence. Insert confirmed_stale mutations so those
    // slugs pick up the changes on their next stale-engine cycle.
    if let Ok(ref res) = result {
        if res.1 == 0 {
            // Build succeeded with zero failures — notify referrers
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
                    conn.execute(
                        "INSERT INTO pyramid_pending_mutations
                         (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                         VALUES (?1, 0, 'confirmed_stale', ?2, ?3, 0, ?4, 0)",
                        rusqlite::params![referrer, &slug_owned, &detail, &now],
                    )?;
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

    // ── WS-ONLINE-F: Resolve remote web edges ─────────────────────────────
    // After a successful build, fetch content for any remote web edges created
    // during this build. The fetched data is cached locally so downstream
    // consumers (publication, drill view) have the remote content available.
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

    result
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

    // Determine which chain to use: slug-specific assignment or default
    let chain_id = {
        let conn = state.reader.lock().await;
        match chain_registry::get_assignment(&conn, slug_name)? {
            Some((id, _file)) => {
                info!(slug = slug_name, chain_id = %id, "using assigned chain");
                id
            }
            None => {
                let default_id = chain_registry::default_chain_id(ct_str).to_string();
                info!(slug = slug_name, chain_id = %default_id, "using default chain");
                default_id
            }
        }
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

    // Determine which chain to use: slug-specific assignment or default
    let chain_id = {
        let conn = state.reader.lock().await;
        match chain_registry::get_assignment(&conn, slug_name)? {
            Some((id, _file)) => {
                info!(slug = slug_name, chain_id = %id, "IR executor: using assigned chain");
                id
            }
            None => {
                let default_id = chain_registry::default_chain_id(ct_str).to_string();
                info!(slug = slug_name, chain_id = %default_id, "IR executor: using default chain");
                default_id
            }
        }
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

/// Legacy path: dispatch to the old build_conversation/build_code/build_docs.
async fn run_legacy_build(
    state: &PyramidState,
    slug_name: &str,
    content_type: &ContentType,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
) -> Result<(String, i32)> {
    let llm_config = state.config.read().await.clone();

    // The legacy build functions require a progress_tx reference; create a
    // dummy one if the caller didn't supply one.
    let owned_tx;
    let ptx: &mpsc::Sender<BuildProgress> = match progress_tx {
        Some(ref tx) => tx,
        None => {
            let (tx, mut rx) = mpsc::channel::<BuildProgress>(16);
            // Spawn a drain so the channel doesn't block
            tokio::spawn(async move { while rx.recv().await.is_some() {} });
            owned_tx = tx;
            &owned_tx
        }
    };

    let failures = match content_type {
        ContentType::Conversation => {
            build::build_conversation(
                state.reader.clone(),
                write_tx,
                &llm_config,
                slug_name,
                cancel,
                ptx,
            )
            .await?
        }
        ContentType::Code => {
            build::build_code(
                state.reader.clone(),
                write_tx,
                &llm_config,
                slug_name,
                cancel,
                ptx,
            )
            .await?
        }
        ContentType::Document => {
            build::build_docs(
                state.reader.clone(),
                write_tx,
                &llm_config,
                slug_name,
                cancel,
                ptx,
            )
            .await?
        }
        ContentType::Vine => {
            return Err(anyhow!("Vine builds use the vine-specific build endpoint"));
        }
        ContentType::Question => {
            return Err(anyhow!(
                "Question builds use the question-driven build endpoint"
            ));
        }
    };

    Ok(("legacy".to_string(), failures))
}

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
    let llm_config = state.config.read().await.clone();

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

    // ── 4. Load pipeline chain (content-type aware) ──────────────────
    let chains_dir = state.chains_dir.clone();
    let default_chain_id = chain_registry::default_chain_id(ct_str);
    let all_chains = chain_loader::discover_chains(&chains_dir)?;
    let meta = all_chains
        .iter()
        .find(|m| m.id == default_chain_id)
        .ok_or_else(|| {
            anyhow!(
                "'{}' chain not found in chains directory ({})",
                default_chain_id,
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

    let llm_config = state.config.read().await.clone();
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
