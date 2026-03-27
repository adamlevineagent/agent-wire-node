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
use tracing::info;

use super::build::{self, WriteOp};
use super::chain_executor;
use super::chain_loader;
use super::chain_registry;
use super::defaults_adapter;
use super::question_compiler;
use super::question_decomposition::{
    self, DecompositionConfig, DecompositionPreview, QuestionTree,
};
use super::question_loader;
use super::slug;
use super::types::{BuildProgress, ContentType};
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
pub async fn run_decomposed_build(
    state: &PyramidState,
    slug_name: &str,
    apex_question: &str,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
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

    let llm_config = state.config.read().await.clone();

    info!(
        slug = slug_name,
        apex = apex_question,
        granularity = granularity,
        max_depth = max_depth,
        from_depth = from_depth,
        "starting question decomposition"
    );

    let tree = question_decomposition::decompose_question(&config, &llm_config).await?;

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

    // ── 7. Execute through the same IR executor ──────────────────────────
    chain_executor::execute_plan(state, &plan, slug_name, from_depth, cancel, progress_tx).await
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
