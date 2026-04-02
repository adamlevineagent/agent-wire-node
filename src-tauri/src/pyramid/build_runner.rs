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
use super::evidence_answering;
use super::extraction_schema;
use super::ingest;
use super::llm;
use super::local_store;
use super::question_compiler;
use super::question_decomposition::{
    self, DecompositionConfig, DecompositionPreview, QuestionTree,
};
use super::question_loader;
use super::reconciliation;
use super::slug;
use super::types::{
    self, BuildProgress, CharacterizationResult, ContentType, HandlePath, LayerEvent,
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

    // Per-operator hourly rate limit
    {
        let mut limiter = state.absorption_build_rate_limiter.lock().await;
        let now = std::time::Instant::now();
        let entry = limiter.entry(operator_id.to_string()).or_insert((0, now));
        let elapsed = now.duration_since(entry.1);

        if elapsed > std::time::Duration::from_secs(hourly_window_secs) {
            // Window expired, reset
            *entry = (1, now);
        } else if entry.0 >= max_per_hour {
            let retry_after = hourly_window_secs - elapsed.as_secs();
            return Err(anyhow!(
                "429: absorption build rate limit exceeded for operator '{}' on slug '{}'. \
                 Limit: {} builds/hour. Retry after {}s",
                operator_id,
                slug_name,
                max_per_hour,
                retry_after
            ));
        } else {
            entry.0 += 1;
        }
    }

    // Daily spend cap
    {
        let mut spend = state.absorption_daily_spend.lock().await;
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(spend.1);

        if elapsed > std::time::Duration::from_secs(daily_window_secs) {
            // Day expired, reset
            *spend = (estimated_cost, now);
        } else if spend.0 + estimated_cost > daily_cap {
            let retry_after = daily_window_secs - elapsed.as_secs();
            return Err(anyhow!(
                "429: absorption daily spend cap exceeded for slug '{}'. \
                 Cap: {} credits/day, spent: {}. Retry after {}s",
                slug_name,
                daily_cap,
                spend.0,
                retry_after
            ));
        } else {
            spend.0 += estimated_cost;
        }
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
) -> Result<(String, i32)> {
    run_build_from(state, slug_name, 0, cancel, progress_tx, write_tx, layer_tx).await
}

/// Run a build from a specific depth, reusing nodes below that depth.
pub async fn run_build_from(
    state: &PyramidState,
    slug_name: &str,
    from_depth: i64,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32)> {
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
    } else if use_chain {
        run_chain_build(
            state,
            slug_name,
            &content_type,
            from_depth,
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
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32)> {
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

    chain_executor::execute_chain_from(state, &chain, slug_name, from_depth, cancel, progress_tx, layer_tx)
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

/// Question-driven build path: load question YAML, compile to IR, execute.
///
/// This is the Phase 2 entry point for question pyramid builds.
/// The caller specifies "build with questions" (e.g., via a route parameter
/// or a config flag) to reach this path instead of defaults or legacy.
pub async fn run_question_build(
    state: &PyramidState,
    slug_name: &str,
    from_depth: i64,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<(String, i32)> {
    // ── 1. Determine content type ────────────────────────────────────────
    let content_type = {
        let conn = state.reader.lock().await;
        slug::get_slug(&conn, slug_name)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?
            .content_type
    };

    let ct_str = content_type.as_str();

    // ── 2. Resolve chains directory ──────────────────────────────────────
    let chains_dir = state.chains_dir.clone();

    // ── 3. Discover and load the question set for this content type ──────
    let question_sets = question_loader::discover_question_sets(&chains_dir)?;
    let meta = question_sets
        .iter()
        .find(|m| m.content_type == ct_str)
        .ok_or_else(|| {
            anyhow!(
                "no question set found for content type '{}' in {}",
                ct_str,
                chains_dir.join("questions").display()
            )
        })?;

    let yaml_path = std::path::Path::new(&meta.file_path);
    let qs = question_loader::load_question_set(yaml_path, &chains_dir)?;

    // ── 4. Compile to ExecutionPlan ──────────────────────────────────────
    let plan = question_compiler::compile_question_set(&qs, &chains_dir)?;

    info!(
        slug = slug_name,
        content_type = ct_str,
        ir_steps = plan.steps.len(),
        estimated_nodes = plan.total_estimated_nodes,
        "starting question-driven build"
    );

    // ── 5. Execute through the same IR executor ──────────────────────────
    chain_executor::execute_plan(state, &plan, slug_name, from_depth, cancel, progress_tx).await
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
) -> Result<(String, i32)> {
    // ── 1. Determine content type ────────────────────────────────────────
    let (content_type, source_path) = {
        let conn = state.reader.lock().await;
        let slug_info = slug::get_slug(&conn, slug_name)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?;
        (slug_info.content_type, slug_info.source_path)
    };

    let ct_str = content_type.as_str();

    // ── 1a. Check for cross-slug references ─────────────────────────────
    // Question slugs may reference other slugs. When references exist, we load
    // nodes from ALL referenced slugs instead of looking at our own source_path.
    let referenced_slugs = {
        let conn = state.reader.lock().await;
        db::get_slug_references(&conn, slug_name)?
    };
    let is_cross_slug = !referenced_slugs.is_empty();

    // ── 1b. Cross-slug node loading ──────────────────────────────────────
    // For cross-slug builds: load nodes from referenced slugs.
    // - Mechanical (base) slugs: load L0 nodes
    // - Question slugs in references: load ALL live nodes (answers become source material)
    let mut source_content_type: Option<String> = None;
    let cross_slug_nodes: Option<Vec<types::PyramidNode>> = if is_cross_slug {
        let conn = state.reader.lock().await;
        let mut all_nodes = Vec::new();

        for ref_slug in &referenced_slugs {
            let ref_info = slug::get_slug(&conn, ref_slug)?;
            match ref_info {
                Some(info) if info.content_type == ContentType::Question => {
                    // Question slug: load ALL live nodes (answers are source material)
                    let nodes = db::get_all_live_nodes(&conn, ref_slug)?;
                    info!(
                        slug = slug_name,
                        ref_slug = ref_slug,
                        node_count = nodes.len(),
                        "loaded all live nodes from referenced question slug"
                    );
                    all_nodes.extend(nodes);
                }
                Some(info) => {
                    // Mechanical (base) slug: load L0 only
                    if source_content_type.is_none() {
                        source_content_type = Some(info.content_type.as_str().to_string());
                    }
                    let nodes = db::get_nodes_at_depth(&conn, ref_slug, 0)?;
                    info!(
                        slug = slug_name,
                        ref_slug = ref_slug,
                        l0_count = nodes.len(),
                        "loaded L0 nodes from referenced base slug"
                    );
                    all_nodes.extend(nodes);
                }
                None => {
                    warn!(
                        slug = slug_name,
                        ref_slug = ref_slug,
                        "referenced slug not found, skipping"
                    );
                }
            }
        }

        if all_nodes.is_empty() {
            return Err(anyhow!(
                "Build your base pyramid first. Referenced slugs {:?} have no nodes.",
                referenced_slugs
            ));
        }

        info!(
            slug = slug_name,
            total_cross_slug_nodes = all_nodes.len(),
            referenced_slugs = ?referenced_slugs,
            "cross-slug nodes loaded"
        );
        Some(all_nodes)
    } else {
        None
    };

    // ── 1c. Characterize if not provided ─────────────────────────────────
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
            // Build L0 summary fallback — from cross-slug nodes or own L0
            let l0_fallback = if let Some(ref cs_nodes) = cross_slug_nodes {
                // Cross-slug: characterize from loaded cross-slug nodes
                Some(
                    cs_nodes
                        .iter()
                        .map(|n| {
                            let summary: String = n.distilled.chars().take(200).collect();
                            format!("- {}: {}", n.headline, summary)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                let conn = state.reader.lock().await;
                let existing_l0 = db::get_nodes_at_depth(&conn, slug_name, 0).unwrap_or_default();
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
                &source_path,
                apex_question,
                &llm_config,
                l0_fallback.as_deref(),
                &state.operational.tier1,
                Some(&state.chains_dir),
            )
            .await?
        }
    };

    // ── 3. Ensure base nodes exist ──────────────────────────────────────
    if is_cross_slug {
        // Cross-slug path: nodes already loaded above. No mechanical build fallback.
        info!(
            slug = slug_name,
            cross_slug_node_count = cross_slug_nodes.as_ref().map(|n| n.len()).unwrap_or(0),
            "cross-slug mode — skipping mechanical build, using referenced slug nodes"
        );
    } else {
        // Single-slug overlay architecture: question pyramids are OVERLAYS on an
        // existing mechanical pyramid. If no L0 nodes exist, build the base first.
        let base_l0_nodes = {
            let conn = state.reader.lock().await;
            db::get_nodes_at_depth(&conn, slug_name, 0)?
        };

        if base_l0_nodes.is_empty() {
            info!(
                slug = slug_name,
                "no base L0 nodes found — running mechanical build first"
            );
            let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<WriteOp>(512);
            let drain_handle =
                tokio::spawn(async move { while write_rx.recv().await.is_some() {} });
            run_build(state, slug_name, cancel, progress_tx.clone(), &write_tx, None).await?;
            drop(write_tx);
            let _ = drain_handle.await;

            let conn = state.reader.lock().await;
            let fresh_l0 = db::get_nodes_at_depth(&conn, slug_name, 0)?;
            info!(
                slug = slug_name,
                l0_count = fresh_l0.len(),
                "base pyramid built — L0 nodes available"
            );
            drop(conn);
        } else {
            info!(
                slug = slug_name,
                l0_count = base_l0_nodes.len(),
                "base pyramid exists — using as overlay base"
            );
        }
    }

    // Build decomposition context from base nodes (cross-slug or own L0)
    let base_l0_for_context = if let Some(ref cs_nodes) = cross_slug_nodes {
        cs_nodes.clone()
    } else {
        let conn = state.reader.lock().await;
        db::get_nodes_at_depth(&conn, slug_name, 0)?
    };
    let l0_context = base_l0_for_context
        .iter()
        .map(|n| {
            let summary: String = n.distilled.chars().take(200).collect();
            format!("- {}: {} — {}", n.id, n.headline, summary)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let decomp_context = format!(
        "Source material ({} extracted summaries from {}):\n{}",
        base_l0_for_context.len(),
        if is_cross_slug {
            "referenced slugs"
        } else {
            "the base knowledge pyramid"
        },
        l0_context
    );

    // ── 3b. Enhance the apex question ─────────────────────────────────────
    // Before decomposition, expand the user's short question into a comprehensive
    // apex question that captures what they're actually asking. This helps the
    // decomposer produce better sub-questions.
    let enhanced_question = {
        // Load the enhancement prompt from the contribution file (chains/prompts/question/)
        // Per Pillar #2: the prompt itself is a contribution, not hardcoded Rust.
        let enhance_system_prompt = {
            let prompt_path = state
                .data_dir
                .as_ref()
                .map(|d| d.join("chains/prompts/question/enhance_question.md"))
                .unwrap_or_else(|| {
                    std::path::PathBuf::from("chains/prompts/question/enhance_question.md")
                });
            match std::fs::read_to_string(&prompt_path) {
                Ok(p) => p,
                Err(e) => {
                    warn!(slug = slug_name, path = %prompt_path.display(), error = %e,
                        "could not load enhance_question.md — using inline fallback");
                    "You expand brief questions into comprehensive apex questions for knowledge pyramids. Default to non-technical human-interest framing unless the user specifically asks about technical details.".to_string()
                }
            }
        };

        let enhance_user_prompt = format!(
            "Original question: \"{apex_question}\"\n\n\
             Source material: {count} extracted summaries. Sample headlines:\n{sample}\n\n\
             Audience: {audience}\n\n\
             Expand this into a comprehensive apex question.",
            apex_question = apex_question,
            count = base_l0_for_context.len(),
            sample = base_l0_for_context
                .iter()
                .take(10)
                .map(|n| format!("- {}", n.headline))
                .collect::<Vec<_>>()
                .join("\n"),
            audience = if characterization_result.audience.is_empty() {
                "a curious, intelligent non-developer"
            } else {
                &characterization_result.audience
            },
        );

        let response = llm::call_model_unified(
            &llm_config,
            &enhance_system_prompt,
            &enhance_user_prompt,
            0.3,
            1024,
            None,
        )
        .await;

        match response {
            Ok(r) => {
                let enhanced = r.content.trim().trim_matches('"').to_string();
                info!(
                    original = %apex_question,
                    enhanced = %enhanced,
                    "question enhancement complete"
                );
                enhanced
            }
            Err(e) => {
                warn!(slug = slug_name, error = %e, "prompt enhancement failed — using original question");
                apex_question.to_string()
            }
        }
    };

    // ── 4. Decompose the apex question into a question tree ──────────────
    // 11-G/H: Pass chains_dir so decomposition prompts load from .md files
    let decomp_chains_dir = Some(state.chains_dir.clone());
    let config = DecompositionConfig {
        apex_question: enhanced_question.clone(),
        content_type: ct_str.to_string(),
        granularity,
        max_depth,
        folder_map: Some(decomp_context), // Pass L0 summaries instead of folder listing
        chains_dir: decomp_chains_dir,
        audience: Some(characterization_result.audience.clone()),
    };

    info!(
        slug = slug_name,
        apex = %enhanced_question,
        granularity = granularity,
        max_depth = max_depth,
        from_depth = from_depth,
        "starting question decomposition"
    );

    // Check for existing question overlay BEFORE decomposition to decide delta vs fresh path
    let pre_decomp_overlay_check = {
        let conn = state.reader.lock().await;
        db::has_existing_question_overlay(&conn, slug_name)?
    };

    // Delta decomposition result (populated only if delta path)
    // reused_question_ids: question IDs from decomposition that map to existing answers
    // (used to skip these questions in the evidence loop)
    let mut reused_question_ids_for_skip: Vec<String> = Vec::new();

    let mut tree = if pre_decomp_overlay_check {
        // Delta path: load existing tree and overlay answers for context
        info!(
            slug = slug_name,
            "delta decomposition — existing overlay detected"
        );
        let existing_tree = {
            let conn = state.reader.lock().await;
            let tree_json = db::get_question_tree(&conn, slug_name)?
                .ok_or_else(|| anyhow!("existing overlay but no question tree found"))?;
            serde_json::from_value::<question_decomposition::QuestionTree>(tree_json)?
        };
        let existing_answers = {
            let conn = state.reader.lock().await;
            db::get_existing_overlay_answers(&conn, slug_name)?
        };

        let delta_result = question_decomposition::decompose_question_delta(
            &config,
            &llm_config,
            &existing_tree,
            &existing_answers,
            Some(&state.chains_dir),
        )
        .await?;

        // Track reused question IDs to skip in evidence loop
        reused_question_ids_for_skip = delta_result.reused_question_ids.clone();

        info!(
            slug = slug_name,
            reused_questions = delta_result.reused_question_ids.len(),
            new_questions = delta_result.new_questions.len(),
            "delta decomposition complete"
        );

        delta_result.tree
    } else {
        // Fresh path: full decomposition
        // Check for existing partial tree (resume support)
        let existing_node_count = {
            let conn = state.reader.lock().await;
            db::count_question_nodes(&conn, slug_name).unwrap_or(0)
        };
        if existing_node_count > 0 {
            info!(
                slug = slug_name,
                existing_nodes = existing_node_count,
                "found existing question nodes — will resume decomposition"
            );
        }

        question_decomposition::decompose_question_incremental(
            &config,
            &llm_config,
            state.writer.clone(),
            slug_name,
            &state.operational.tier1,
            &state.operational.tier2,
        )
        .await?
    };

    // Attach the audience from characterization so it flows through all downstream prompts
    tree.audience = Some(characterization_result.audience.clone());

    // Also persist as the legacy JSON blob for backward compat
    {
        let tree_json = serde_json::to_value(&tree)?;
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            db::save_question_tree(&c, &slug_owned, &tree_json)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Question tree save panicked: {e}"))??;
    }

    // ── 4b. Generate extraction schema from leaf questions ────────────────
    // This is the critical quality lever (Step 1.3): makes L0 extraction
    // question-shaped instead of generic. The extraction prompt tells L0
    // exactly what to look for based on the decomposed questions.
    let leaf_questions = extraction_schema::collect_leaf_questions(&tree);
    let leaf_refs: Vec<_> = leaf_questions.into_iter().cloned().collect();

    let ext_schema = extraction_schema::generate_extraction_schema(
        &leaf_refs,
        &characterization_result.material_profile,
        &characterization_result.audience,
        &characterization_result.tone,
        &llm_config,
        &state.operational.tier1,
        Some(&state.chains_dir),
    )
    .await?;

    info!(
        slug = slug_name,
        topic_fields = ext_schema.topic_schema.len(),
        extraction_prompt_len = ext_schema.extraction_prompt.len(),
        "extraction schema generated — L0 will use question-shaped prompts"
    );

    // ── 5. Overlay architecture: skip L0 extraction, go straight to evidence loop ──
    // The base pyramid's L0 nodes ARE the canonical extraction.
    // The question pyramid is an OVERLAY — it creates answer layers (L1+) on top
    // of the existing base, without re-extracting source material.
    //
    // This means:
    // - No IR executor call needed (no L0 extraction, no plan compilation)
    // - No chunk ingestion needed (base pyramid already did this)
    // - Second question on same corpus = instant (no re-extraction)
    // - The evidence loop reads base L0 directly

    // Record build start
    let build_id = format!("qb-{}", uuid::Uuid::new_v4());
    let layer_questions = question_decomposition::extract_layer_questions(&tree);
    let max_layer = layer_questions.keys().copied().max().unwrap_or(0);
    {
        info!(slug = slug_name, build_id = %build_id, "acquiring writer lock: save_build_start");
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let bid = build_id.clone();
        // 11-X: enhanced_question goes in `question` column, apex_question (user's original) in `original_question`
        let q = enhanced_question.clone();
        let orig_q = apex_question.to_string();
        let ml = max_layer + 1;
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            local_store::save_build_start(&c, &slug_owned, &bid, &q, ml, Some(&orig_q))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Build start save panicked: {e}"))??;
        info!(slug = slug_name, "writer lock released: save_build_start");
    }

    // ── Overlay cleanup: delta vs fresh path ────────────────────────────
    // pre_decomp_overlay_check was set before decomposition.
    // Delta path: only supersede existing overlay apex nodes (shared answers preserved).
    // Fresh path: supersede all L1+ overlay nodes.
    let existing_overlay_answers = if pre_decomp_overlay_check {
        let conn = state.reader.lock().await;
        db::get_existing_overlay_answers(&conn, slug_name)?
    } else {
        Vec::new()
    };

    if pre_decomp_overlay_check {
        // Delta path: only supersede existing overlay apex nodes (highest depth overlay nodes).
        // Shared answer nodes at lower depths are preserved for reuse.
        let max_overlay_depth = existing_overlay_answers
            .iter()
            .map(|n| n.depth)
            .max()
            .unwrap_or(0);
        if max_overlay_depth > 0 {
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let overlay_build_id = build_id.clone();
            let depth_threshold = max_overlay_depth;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                // Only supersede at the apex depth of the existing overlay
                c.execute(
                    "UPDATE pyramid_nodes SET superseded_by = ?3
                     WHERE slug = ?1 AND depth >= ?2 AND build_id LIKE 'qb-%' AND superseded_by IS NULL",
                    rusqlite::params![slug_owned, depth_threshold, overlay_build_id],
                )?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Delta overlay cleanup panicked: {e}"))??;
        }
        info!(
            slug = slug_name,
            "delta path — old apex superseded, shared answers preserved"
        );
    } else {
        // Fresh path: supersede all prior overlay nodes (L1+) but keep base L0.
        // Evidence and gaps are retained — live_pyramid_evidence view filters by live nodes.
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let overlay_build_id = build_id.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            db::supersede_nodes_above(&c, &slug_owned, 0, &overlay_build_id)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Overlay cleanup panicked: {e}"))??;
        info!(
            slug = slug_name,
            "fresh path — all prior L1+ nodes superseded"
        );
    }

    info!(
        slug = slug_name,
        "overlay mode — using base pyramid L0, starting evidence loop"
    );

    // ── 10. Evidence-weighted upper layer loop ───────────────────────────
    // Load L0 nodes for evidence loop: cross-slug nodes or own L0
    let l0_nodes = if let Some(cs_nodes) = cross_slug_nodes {
        info!(
            slug = slug_name,
            l0_count = cs_nodes.len(),
            "using cross-slug nodes as L0 for evidence loop"
        );
        cs_nodes
    } else {
        let conn = state.reader.lock().await;
        db::get_nodes_at_depth(&conn, slug_name, 0)?
    };
    info!(
        slug = slug_name,
        l0_count = l0_nodes.len(),
        "loaded L0 nodes for evidence loop"
    );

    let l0_summary = evidence_answering::build_l0_summary(&l0_nodes, &state.operational);
    info!(
        slug = slug_name,
        summary_len = l0_summary.len(),
        "built L0 summary"
    );

    let synth_prompts = match extraction_schema::generate_synthesis_prompts(
        &tree,
        &l0_summary,
        &ext_schema,
        tree.audience.as_deref(),
        &llm_config,
        &state.operational.tier1,
        Some(&state.chains_dir),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            error!(slug = slug_name, error = %e, "generate_synthesis_prompts failed");
            return Err(e);
        }
    };

    info!(
        slug = slug_name,
        answering_prompt_len = synth_prompts.answering_prompt.len(),
        "synthesis prompts generated — entering per-layer evidence loop"
    );

    let actual_l0_count = l0_nodes.len() as i32;
    let mut total_nodes = actual_l0_count;
    // Exclude layer 0 from estimate (L0 already counted via actual_l0_count from executor)
    let estimated_total: i64 = layer_questions
        .iter()
        .filter(|(&k, _)| k > 0)
        .map(|(_, qs)| qs.len() as i64)
        .sum::<i64>()
        + actual_l0_count as i64;
    let mut layers_completed: i64 = 0;
    let mut build_error: Option<String> = None;

    let evidence_start_layer = std::cmp::max(1, from_depth);
    for layer in evidence_start_layer..=max_layer {
        // Check cancellation at each layer boundary
        if cancel.is_cancelled() {
            warn!(
                slug = slug_name,
                layer, "build cancelled during evidence loop"
            );
            build_error = Some(format!("Cancelled at layer {}", layer));
            break;
        }

        let layer_qs_raw = match layer_questions.get(&layer) {
            Some(qs) => qs.clone(),
            None => {
                info!(slug = slug_name, layer, "no questions at layer, skipping");
                continue;
            }
        };

        // Filter out reused questions (delta path) — their answers already exist
        let reused_set: std::collections::HashSet<&str> = reused_question_ids_for_skip
            .iter()
            .map(|s| s.as_str())
            .collect();
        let layer_qs: Vec<_> = layer_qs_raw
            .into_iter()
            .filter(|q| !reused_set.contains(q.question_id.as_str()))
            .collect();

        if layer_qs.is_empty() {
            info!(
                slug = slug_name,
                layer, "all questions at layer reused from existing overlay, skipping"
            );
            continue;
        }

        // Load lower-layer nodes.
        // For cross-slug builds at layer 1, the "L0" nodes live in referenced slugs,
        // not under our own slug. Use the pre-loaded l0_nodes in that case.
        let lower_nodes = if is_cross_slug && layer == 1 {
            l0_nodes.clone()
        } else {
            let conn = state.reader.lock().await;
            db::get_nodes_at_depth(&conn, slug_name, layer - 1)?
        };

        info!(
            slug = slug_name,
            layer,
            questions = layer_qs.len(),
            lower_nodes = lower_nodes.len(),
            "starting evidence answering for layer"
        );

        // Step a: Pre-map questions to candidate evidence nodes
        let candidate_map = match evidence_answering::pre_map_layer(
            &layer_qs,
            &lower_nodes,
            &llm_config,
            &state.operational,
            tree.audience.as_deref(),
            Some(&state.chains_dir),
            source_content_type.as_deref(),
        )
        .await
        {
            Ok(map) => map,
            Err(e) => {
                warn!(slug = slug_name, layer, error = %e, "pre-mapping failed, stopping at layer");
                build_error = Some(format!("Pre-mapping failed at layer {}: {}", layer, e));
                break;
            }
        };

        // Step b: Answer questions with evidence (NO DB writes — returns results only)
        let batch_result = match evidence_answering::answer_questions(
            &layer_qs,
            &candidate_map,
            &lower_nodes,
            Some(&synth_prompts.answering_prompt),
            tree.audience.as_deref(),
            &llm_config,
            slug_name,
            slug_name, // answer_slug — same as slug for single-pyramid builds
            Some(&state.chains_dir),
            source_content_type.as_deref(),
            &state.operational,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                warn!(slug = slug_name, layer, error = %e, "answer_questions failed, stopping at layer");
                build_error = Some(format!("Answer failed at layer {}: {}", layer, e));
                break;
            }
        };

        let mut answered = batch_result.answered;
        let failed = batch_result.failed;

        // Stamp build_id on each answered node so they belong to this overlay
        for a in &mut answered {
            a.node.build_id = Some(build_id.clone());
        }

        let answered_ids: Vec<String> = answered.iter().map(|a| a.node.id.clone()).collect();
        let lower_ids: Vec<String> = lower_nodes.iter().map(|n| n.id.clone()).collect();
        let layer_node_count = answered.len() as i32;

        if !failed.is_empty() {
            warn!(
                slug = slug_name,
                layer,
                failed_count = failed.len(),
                "some questions failed — recording as gap reports"
            );
        }

        // Step c: Persist answered nodes + evidence links + gaps in spawn_blocking
        {
            info!(
                slug = slug_name,
                layer,
                nodes = layer_node_count,
                "acquiring writer lock: evidence persist"
            );
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let bid_for_gaps = build_id.clone();
            let answered_owned = answered;
            let failed_owned = failed;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                // Wrap per-layer persistence in a transaction for atomicity
                c.execute_batch("BEGIN")?;
                let result = (|| -> anyhow::Result<()> {
                    for a in &answered_owned {
                        db::save_node(&c, &a.node, None)?;
                        // Back-patch parent_id on child nodes so upward navigation works
                        // 11-AG: Skip handle-path children — they live in other slugs,
                        // parent_id is not meaningful cross-slug
                        for child_id in &a.node.children {
                            if child_id.contains('/') {
                                continue; // cross-slug reference, skip
                            }
                            let _ = db::update_parent(&c, &slug_owned, child_id, &a.node.id);
                        }
                        for link in &a.evidence {
                            db::save_evidence_link(&c, link)?;
                        }
                        // Save missing items as gap reports
                        // 11-W: Use node ID (not question text) so drill() can look up gaps by node_id
                        for missing_desc in &a.missing {
                            let gap = types::GapReport {
                                question_id: a.node.id.clone(),
                                description: missing_desc.clone(),
                                layer: a.node.depth as i64,
                            };
                            db::save_gap(&c, &slug_owned, &gap, Some(&bid_for_gaps))?;
                        }
                    }
                    // Save gap reports for questions that failed entirely
                    for fq in &failed_owned {
                        let gap = types::GapReport {
                            question_id: fq.question_id.clone(),
                            description: format!(
                                "Question failed during evidence answering: {}. Error: {}",
                                fq.question_text, fq.error
                            ),
                            layer: fq.layer,
                        };
                        db::save_gap(&c, &slug_owned, &gap, Some(&bid_for_gaps))?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        c.execute_batch("COMMIT")?;
                        Ok(())
                    }
                    Err(e) => {
                        let _ = c.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            })
            .await
            .map_err(|e| anyhow!("Evidence save panicked: {e}"))??;
        }

        // Step d: Reconcile layer in spawn_blocking
        {
            info!(
                slug = slug_name,
                layer, "acquiring writer lock: reconcile_layer"
            );
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let aids = answered_ids;
            let lids = lower_ids;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                let _result =
                    reconciliation::reconcile_layer(&c, &slug_owned, layer, &aids, &lids)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Reconciliation panicked: {e}"))??;
        }

        total_nodes += layer_node_count;
        layers_completed = layer;

        // Step e: Update build progress
        {
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let bid = build_id.clone();
            let tn = total_nodes;
            let al0 = actual_l0_count;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                local_store::update_build_progress(
                    &c,
                    &slug_owned,
                    &bid,
                    layer,
                    al0 as i64,
                    tn as i64,
                )?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Progress update panicked: {e}"))??;
        }

        // Step f: Send progress update if channel available
        if let Some(ref tx) = progress_tx {
            let _ = tx
                .send(BuildProgress {
                    done: total_nodes as i64,
                    total: estimated_total,
                })
                .await;
        }

        info!(
            slug = slug_name,
            layer,
            nodes_created = layer_node_count,
            total_nodes,
            "layer complete"
        );
    }

    // Mark build complete or failed
    {
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let bid = build_id;
        let err = build_error.clone();
        let lc = layers_completed;
        let ml = max_layer;
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            if let Some(error_msg) = err {
                local_store::fail_build(
                    &c,
                    &slug_owned,
                    &bid,
                    &format!("Stopped at layer {}/{}: {}", lc, ml, error_msg),
                )?;
            } else {
                local_store::complete_build(&c, &slug_owned, &bid, None)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Build status save panicked: {e}"))??;
    }

    // Update slug stats so node_count/max_depth reflect evidence-answered nodes
    {
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            let _ = db::update_slug_stats(&c, &slug_owned);
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Slug stats update panicked: {e}"))??;
    }

    let failure_count = if build_error.is_some() { 1 } else { 0 };

    if let Some(ref err) = build_error {
        info!(
            slug = slug_name,
            total_nodes,
            layers_completed,
            max_layers = max_layer,
            error = %err,
            "question pyramid build PARTIAL (stopped early)"
        );
    } else {
        info!(
            slug = slug_name,
            total_nodes,
            layers = max_layer,
            "question pyramid build complete"
        );
    }

    Ok((slug_name.to_string(), failure_count))
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

    // ── 2. Build context from L0 summaries (aligned with actual build path) ──
    // The actual build uses L0 summaries, not folder_map. Align preview to
    // use the same context source so decomposition matches the real build.
    let decomp_context = {
        let conn = state.reader.lock().await;
        let base_l0 = db::get_nodes_at_depth(&conn, slug_name, 0)?;
        if base_l0.is_empty() {
            // No base pyramid yet — fall back to folder map
            question_decomposition::build_folder_map(&source_path)
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
