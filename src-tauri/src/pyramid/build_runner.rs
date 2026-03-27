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
use tracing::{info, warn};

use super::build::{self, WriteOp};
use super::chain_executor;
use super::chain_loader;
use super::chain_registry;
use super::characterize;
use super::db;
use super::evidence_answering;
use super::extraction_schema;
use super::ingest;
use super::local_store;
use super::question_compiler;
use super::question_decomposition::{
    self, DecompositionConfig, DecompositionPreview, QuestionTree,
};
use super::question_loader;
use super::reconciliation;
use super::slug;
use super::types::{self, BuildProgress, CharacterizationResult, ContentType};
use super::defaults_adapter;
use super::PyramidState;

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
) -> Result<(String, i32)> {
    run_build_from(state, slug_name, 0, cancel, progress_tx, write_tx).await
}

/// Run a build from a specific depth, reusing nodes below that depth.
pub async fn run_build_from(
    state: &PyramidState,
    slug_name: &str,
    from_depth: i64,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
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

    // ── 2. Check feature flags ───────────────────────────────────────────
    let use_ir = state.use_ir_executor.load(Ordering::Relaxed);
    let use_chain = state.use_chain_engine.load(Ordering::Relaxed);

    if use_ir {
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
    }
}

/// Chain-engine path: load chain YAML, execute via chain_executor.
async fn run_chain_build(
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

    // Resolve chain file path from the chains directory
    let chains_dir = state
        .data_dir
        .as_ref()
        .ok_or_else(|| anyhow!("data_dir not set on PyramidState"))?
        .join("chains");

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

    chain_executor::execute_chain_from(state, &chain, slug_name, from_depth, cancel, progress_tx)
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

    // Resolve chain file path from the chains directory
    let chains_dir = state
        .data_dir
        .as_ref()
        .ok_or_else(|| anyhow!("data_dir not set on PyramidState"))?
        .join("chains");

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
    let chains_dir = state
        .data_dir
        .as_ref()
        .ok_or_else(|| anyhow!("data_dir not set on PyramidState"))?
        .join("chains");

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

    // ── 1b. Characterize if not provided ─────────────────────────────────
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
            characterize::characterize(&source_path, apex_question, &llm_config).await?
        }
    };

    // ── 2. Resolve chains directory ──────────────────────────────────────
    let chains_dir = state
        .data_dir
        .as_ref()
        .ok_or_else(|| anyhow!("data_dir not set on PyramidState"))?
        .join("chains");

    // ── 3. Build folder map from source path for LLM context ─────────────
    let folder_map = question_decomposition::build_folder_map(&source_path);

    // ── 4. Decompose the apex question into a question tree ──────────────
    let config = DecompositionConfig {
        apex_question: apex_question.to_string(),
        content_type: ct_str.to_string(),
        granularity,
        max_depth,
        folder_map,
    };

    info!(
        slug = slug_name,
        apex = apex_question,
        granularity = granularity,
        max_depth = max_depth,
        from_depth = from_depth,
        "starting question decomposition"
    );

    let tree = question_decomposition::decompose_question(&config, &llm_config).await?;

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
    )
    .await?;

    info!(
        slug = slug_name,
        topic_fields = ext_schema.topic_schema.len(),
        extraction_prompt_len = ext_schema.extraction_prompt.len(),
        "extraction schema generated — L0 will use question-shaped prompts"
    );

    // ── 5. Convert tree to QuestionSet ───────────────────────────────────
    let qs = question_decomposition::question_tree_to_question_set(&tree, ct_str, &chains_dir)?;

    // ── 6. Compile to ExecutionPlan ──────────────────────────────────────
    let plan = question_compiler::compile_question_set(&qs, &chains_dir)?;

    info!(
        slug = slug_name,
        content_type = ct_str,
        ir_steps = plan.steps.len(),
        estimated_nodes = plan.total_estimated_nodes,
        from_depth = from_depth,
        "starting decomposed question build"
    );

    // ── 7. Patch L0 extraction steps with question-shaped prompt ────────
    // Instead of modifying the executor, we patch the plan's L0 steps
    // to use the dynamically generated extraction prompt. This makes L0
    // extraction question-shaped without changing execute_plan()'s signature.
    //
    // CRITICAL: We must also clear instruction_map because the executor's
    // resolve_ir_instruction() checks instruction_map FIRST. If it finds a
    // match, it ignores step.instruction entirely. Clearing the map forces
    // the executor to fall through to step.instruction (our question-shaped prompt).
    //
    // ── Heuristic coupling note ──
    // L0 steps are identified by `storage_directive.depth == Some(0)` combined
    // with `step.instruction.is_some()`. This coupling exists because we need
    // to patch L0 extraction steps *outside* chain_executor.rs — the executor
    // is a general-purpose engine that knows nothing about question-shaped
    // pyramids. Rather than threading a question-mode flag through the executor's
    // API (which would leak pyramid-specific concerns into the chain layer),
    // we intercept the IR plan here in the build runner and rewrite the L0
    // instructions before the executor ever sees them. The depth+instruction
    // predicate is sufficient because only L0 extraction steps carry both a
    // depth-0 storage directive and an instruction body.
    let mut patched_plan = plan;
    let extraction_instruction = ext_schema.extraction_prompt.clone();
    for step in &mut patched_plan.steps {
        if let Some(ref sd) = step.storage_directive {
            if sd.depth == Some(0) {
                step.instruction = Some(extraction_instruction.clone());
                step.instruction_map = None; // Force question-shaped prompt over file-type dispatch
            }
        }
    }

    info!(
        slug = slug_name,
        patched_l0_steps = patched_plan.steps.iter()
            .filter(|s| s.storage_directive.as_ref().map(|sd| sd.depth == Some(0)).unwrap_or(false))
            .count(),
        "patched L0 steps with question-shaped extraction prompt"
    );

    // ── 8. Ingest source files into chunks (required before executor) ────
    // The IR executor reads chunks from SQLite. For a fresh slug or changed
    // source files, we need to ingest first. This is the same ingestion that
    // the /pyramid/:slug/ingest endpoint and the legacy build path perform.
    {
        let slug_info = {
            let conn = state.reader.lock().await;
            slug::get_slug(&conn, slug_name)?
                .ok_or_else(|| anyhow!("Slug '{}' not found", slug_name))?
        };
        let paths = slug::resolve_validated_source_paths(
            &slug_info.source_path,
            &slug_info.content_type,
            state.data_dir.as_deref(),
        )?;
        let writer = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let ct = slug_info.content_type;
        tokio::task::spawn_blocking(move || {
            let conn = writer.blocking_lock();
            for path in &paths {
                match ct {
                    ContentType::Code => { ingest::ingest_code(&conn, &slug_owned, path)?; }
                    ContentType::Conversation => { ingest::ingest_conversation(&conn, &slug_owned, path)?; }
                    ContentType::Document => { ingest::ingest_docs(&conn, &slug_owned, path)?; }
                    ContentType::Vine => { return Err(anyhow!("Vine builds use a separate pipeline")); }
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Ingest task panicked: {e}"))??;

        let chunk_count = {
            let conn = state.reader.lock().await;
            super::db::count_chunks(&conn, slug_name)?
        };
        info!(slug = slug_name, chunks = chunk_count, "ingestion complete");
    }

    // ── 9. Filter plan to L0-only steps and execute ────────────────────
    // Under the evidence pipeline, the IR executor only handles L0 extraction.
    // Upper layers (L1+) are built by the evidence-weighted answering loop below.
    let l0_plan = {
        let mut filtered = patched_plan.clone();
        filtered.steps.retain(|step| {
            step.storage_directive
                .as_ref()
                .and_then(|sd| sd.depth)
                .map(|d| d == 0)
                .unwrap_or(false)
                // Also keep steps without storage directives (setup steps)
                || step.storage_directive.is_none()
        });
        filtered
    };

    // Record build start
    let build_id = format!("qb-{}", uuid::Uuid::new_v4());
    let layer_questions = question_decomposition::extract_layer_questions(&tree);
    let max_layer = layer_questions.keys().copied().max().unwrap_or(0);
    {
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let bid = build_id.clone();
        let q = apex_question.to_string();
        let ml = max_layer;
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            local_store::save_build_start(&c, &slug_owned, &bid, &q, ml)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Build start save panicked: {e}"))??;
    }

    let (_, l0_count) = chain_executor::execute_plan(
        state, &l0_plan, slug_name, from_depth, cancel, progress_tx.clone(),
    )
    .await?;

    info!(slug = slug_name, l0_nodes = l0_count, "L0 extraction complete — starting evidence loop");

    // ── 10. Evidence-weighted upper layer loop ───────────────────────────
    // Load L0 nodes and generate synthesis prompts
    let l0_nodes = {
        let conn = state.reader.lock().await;
        db::get_nodes_at_depth(&conn, slug_name, 0)?
    };

    let l0_summary = evidence_answering::build_l0_summary(&l0_nodes);

    let synth_prompts = extraction_schema::generate_synthesis_prompts(
        &tree,
        &l0_summary,
        &ext_schema,
        &llm_config,
    )
    .await?;

    info!(
        slug = slug_name,
        answering_prompt_len = synth_prompts.answering_prompt.len(),
        "synthesis prompts generated — entering per-layer evidence loop"
    );

    let mut total_nodes = l0_count;

    for layer in 1..=max_layer {
        // Check cancellation at each layer boundary
        if cancel.is_cancelled() {
            warn!(slug = slug_name, layer, "build cancelled during evidence loop");
            break;
        }

        let layer_qs = match layer_questions.get(&layer) {
            Some(qs) => qs.clone(),
            None => {
                info!(slug = slug_name, layer, "no questions at layer, skipping");
                continue;
            }
        };

        // Load lower-layer nodes
        let lower_nodes = {
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
            &layer_qs, &lower_nodes, &llm_config,
        )
        .await
        {
            Ok(map) => map,
            Err(e) => {
                warn!(slug = slug_name, layer, error = %e, "pre-mapping failed, skipping layer");
                break;
            }
        };

        // Step b: Answer questions with evidence (NO DB writes — returns results only)
        let answered = match evidence_answering::answer_questions(
            &layer_qs,
            &candidate_map,
            &lower_nodes,
            Some(&synth_prompts.answering_prompt),
            &llm_config,
            slug_name,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                warn!(slug = slug_name, layer, error = %e, "answer_questions failed, skipping layer");
                break;
            }
        };

        let answered_ids: Vec<String> = answered.iter().map(|a| a.node.id.clone()).collect();
        let lower_ids: Vec<String> = lower_nodes.iter().map(|n| n.id.clone()).collect();
        let layer_node_count = answered.len() as i32;

        // Step c: Persist answered nodes + evidence links + gaps in spawn_blocking
        {
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let answered_owned = answered;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                for a in &answered_owned {
                    db::save_node(&c, &a.node, None)?;
                    for link in &a.evidence {
                        db::save_evidence_link(&c, link)?;
                    }
                    // Save missing items as gap reports
                    for missing_desc in &a.missing {
                        let gap = types::GapReport {
                            question_id: a.node.self_prompt.clone(),
                            description: missing_desc.clone(),
                            layer: a.node.depth as i64,
                        };
                        db::save_gap(&c, &slug_owned, &gap)?;
                    }
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Evidence save panicked: {e}"))??;
        }

        // Step d: Reconcile layer in spawn_blocking
        {
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let aids = answered_ids;
            let lids = lower_ids;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                let _result = reconciliation::reconcile_layer(
                    &c, &slug_owned, layer, &aids, &lids,
                )?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Reconciliation panicked: {e}"))??;
        }

        total_nodes += layer_node_count;

        // Step e: Update build progress
        {
            let conn = state.writer.clone();
            let slug_owned = slug_name.to_string();
            let bid = build_id.clone();
            let tn = total_nodes;
            tokio::task::spawn_blocking(move || {
                let c = conn.blocking_lock();
                local_store::update_build_progress(&c, &slug_owned, &bid, layer, l0_count as i64, tn as i64)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map_err(|e| anyhow!("Progress update panicked: {e}"))??;
        }

        // Step f: Send progress update if channel available
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(BuildProgress {
                done: total_nodes as i64,
                total: (total_nodes as i64) + (max_layer - layer) as i64 * layer_node_count as i64,
            }).await;
        }

        info!(
            slug = slug_name,
            layer,
            nodes_created = layer_node_count,
            total_nodes,
            "layer complete"
        );
    }

    // Mark build complete
    {
        let conn = state.writer.clone();
        let slug_owned = slug_name.to_string();
        let bid = build_id;
        tokio::task::spawn_blocking(move || {
            let c = conn.blocking_lock();
            local_store::complete_build(&c, &slug_owned, &bid, None)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Build complete save panicked: {e}"))??;
    }

    info!(
        slug = slug_name,
        total_nodes,
        layers = max_layer,
        "question pyramid build complete"
    );

    Ok((slug_name.to_string(), total_nodes))
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

    // ── 2. Build folder map ──────────────────────────────────────────────
    let folder_map = question_decomposition::build_folder_map(&source_path);

    // ── 3. Decompose ─────────────────────────────────────────────────────
    let config = DecompositionConfig {
        apex_question: apex_question.to_string(),
        content_type: ct_str.to_string(),
        granularity,
        max_depth,
        folder_map,
    };

    let llm_config = state.config.read().await.clone();
    let tree = question_decomposition::decompose_question(&config, &llm_config).await?;

    // ── 4. Preview ───────────────────────────────────────────────────────
    let preview = question_decomposition::preview_decomposition(&tree);

    Ok((tree, preview))
}
