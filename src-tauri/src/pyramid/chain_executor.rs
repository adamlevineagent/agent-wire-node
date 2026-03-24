// pyramid/chain_executor.rs — Chain runtime executor (the main execution loop)
//
// Executes YAML-driven chain definitions against a pyramid slug.
// Handles forEach, pair_adjacent, recursive_pair modes with full
// resume support, cancellation, error strategies, and progress reporting.
//
// Delegates to:
//   - chain_resolve: $ref and {{template}} resolution (ChainContext)
//   - chain_dispatch: LLM/mechanical step dispatch (StepContext, dispatch_step)
//
// See docs/plans/action-chain-refactor-v3.md Phase 4 for the full spec.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn, error};

use super::build::{
    WriteOp, child_payload_json, send_save_node, send_save_step, send_update_parent,
};
use super::chain_dispatch::{self, build_node_from_output, generate_node_id};
use super::chain_engine::{ChainDefinition, ChainStep};
use super::chain_resolve::{ChainContext, resolve_prompt_template};
use super::db;
use super::types::{BuildProgress, PyramidNode};
use super::PyramidState;

// ── Error strategy ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ErrorStrategy {
    Abort,
    Skip,
    Retry(u32),
    CarryLeft,
    CarryUp,
}

fn parse_error_strategy(s: &str) -> ErrorStrategy {
    match s {
        "abort" => ErrorStrategy::Abort,
        "skip" => ErrorStrategy::Skip,
        "carry_left" => ErrorStrategy::CarryLeft,
        "carry_up" => ErrorStrategy::CarryUp,
        other => {
            if let Some(inner) = other.strip_prefix("retry(").and_then(|s| s.strip_suffix(')')) {
                if let Ok(n) = inner.parse::<u32>() {
                    return ErrorStrategy::Retry(n.min(10).max(1));
                }
            }
            ErrorStrategy::Retry(2)
        }
    }
}

fn resolve_error_strategy(step: &ChainStep, defaults: &super::chain_engine::ChainDefaults) -> ErrorStrategy {
    let on_error_str = step.on_error.as_deref().unwrap_or(&defaults.on_error);
    parse_error_strategy(on_error_str)
}

// ── Resume state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeState {
    Missing,
    Complete,
    StaleStep,
}

/// Check if a specific step iteration is already complete.
/// Returns Complete if BOTH the pipeline step AND the output artifact exist (when saves_node).
/// Returns StaleStep if step exists but node is missing.
/// Returns Missing if step does not exist at all.
async fn get_resume_state(
    reader: &Arc<Mutex<Connection>>,
    slug: &str,
    step_name: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
    saves_node: bool,
) -> Result<ResumeState> {
    let slug_owned = slug.to_string();
    let step_name_owned = step_name.to_string();
    let node_id_owned = node_id.to_string();

    let db = reader.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        if !db::step_exists(&conn, &slug_owned, &step_name_owned, chunk_index, depth, &node_id_owned)? {
            return Ok(ResumeState::Missing);
        }
        if saves_node {
            if db::get_node(&conn, &slug_owned, &node_id_owned)?.is_some() {
                Ok(ResumeState::Complete)
            } else {
                Ok(ResumeState::StaleStep)
            }
        } else {
            Ok(ResumeState::Complete)
        }
    })
    .await?
}

/// Read from the DB on a blocking task.
async fn db_read<F, T>(db: &Arc<Mutex<Connection>>, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        f(&conn)
    })
    .await?
}

// ── Writer drain (same pattern as vine.rs) ──────────────────────────────────

/// Spawn a writer drain task that consumes WriteOps from the channel.
/// Returns (sender, join_handle). Drop the sender to signal completion.
fn spawn_write_drain(
    writer: Arc<Mutex<Connection>>,
) -> (mpsc::Sender<WriteOp>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<WriteOp>(256);
    let handle = tokio::spawn(async move {
        while let Some(op) = rx.recv().await {
            let result = {
                let conn = writer.lock().await;
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
                        &conn, slug, step_type, chunk_index, depth, node_id,
                        output_json, model, elapsed,
                    ),
                    WriteOp::UpdateParent {
                        ref slug,
                        ref node_id,
                        ref parent_id,
                    } => db::update_parent(&conn, slug, node_id, parent_id),
                    WriteOp::UpdateStats { ref slug } => db::update_slug_stats(&conn, slug),
                }
            };
            if let Err(e) = result {
                error!("WriteOp failed: {e}");
            }
        }
    });
    (tx, handle)
}

// ── Progress helpers ────────────────────────────────────────────────────────

async fn send_progress(
    progress_tx: &Option<mpsc::Sender<BuildProgress>>,
    done: i64,
    total: i64,
) {
    if let Some(ref tx) = progress_tx {
        let _ = tx.send(BuildProgress { done, total }).await;
    }
}

/// Estimate total iterations for progress reporting.
fn estimate_total(chain: &ChainDefinition, num_chunks: i64) -> i64 {
    let mut total: i64 = 0;
    for step in &chain.steps {
        if step.for_each.is_some() {
            total += num_chunks;
        } else if step.pair_adjacent {
            total += (num_chunks + 1) / 2;
        } else if step.recursive_pair {
            // Sum of ceil(n / 2^k) for k = 0,1,2,...
            let mut n = num_chunks;
            while n > 1 {
                let pairs = (n + 1) / 2;
                total += pairs;
                n = pairs;
            }
        } else {
            total += 1;
        }
    }
    total
}

// ── Condition evaluation ────────────────────────────────────────────────────

/// Evaluate a `when` condition. Returns true if condition is met or when is None.
fn evaluate_when(when: Option<&str>, ctx: &ChainContext) -> bool {
    let expr = match when {
        Some(e) => e.trim(),
        None => return true,
    };

    // Simple ref check: $has_prior_build → resolve and check truthiness
    if expr.starts_with('$') && !expr.contains(' ') {
        match ctx.resolve_ref(expr) {
            Ok(val) => {
                return match val {
                    Value::Bool(b) => b,
                    Value::Null => false,
                    Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
                    Value::String(s) => !s.is_empty() && s != "false",
                    Value::Array(a) => !a.is_empty(),
                    Value::Object(o) => !o.is_empty(),
                };
            }
            Err(_) => return false,
        }
    }

    // Comparison: $var > N, $var == N, etc.
    for op in &[" > ", " >= ", " < ", " <= ", " == ", " != "] {
        if let Some(pos) = expr.find(op) {
            let lhs_expr = expr[..pos].trim();
            let rhs_str = expr[pos + op.len()..].trim();

            let lhs = ctx.resolve_ref(lhs_expr)
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let rhs = rhs_str.parse::<f64>().unwrap_or(0.0);

            return match *op {
                " > " => lhs > rhs,
                " >= " => lhs >= rhs,
                " < " => lhs < rhs,
                " <= " => lhs <= rhs,
                " == " => (lhs - rhs).abs() < f64::EPSILON,
                " != " => (lhs - rhs).abs() >= f64::EPSILON,
                _ => true,
            };
        }
    }

    warn!("Unknown when expression: '{expr}', defaulting to true");
    true
}

// ── Dispatch helpers ────────────────────────────────────────────────────────

/// Dispatch a step with retry logic (exponential backoff).
/// Returns Ok(analysis) or Err on final failure.
async fn dispatch_with_retry(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &super::chain_engine::ChainDefaults,
    dispatch_ctx: &chain_dispatch::StepContext,
    error_strategy: &ErrorStrategy,
    fallback_key: &str,
) -> Result<Value> {
    let max_attempts = match error_strategy {
        ErrorStrategy::Retry(n) => *n,
        _ => 1,
    };

    let mut last_err = None;
    for attempt in 0..max_attempts {
        match chain_dispatch::dispatch_step(step, resolved_input, system_prompt, defaults, dispatch_ctx).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                warn!(
                    "  Dispatch attempt {}/{} failed for {fallback_key}: {e}",
                    attempt + 1,
                    max_attempts,
                );
                last_err = Some(e);
                if attempt + 1 < max_attempts {
                    let delay = std::time::Duration::from_secs(2u64.pow(attempt + 1));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("dispatch failed for {fallback_key}")))
}

/// Carry a node up to a higher depth (copy without LLM call).
async fn carry_node_up(
    writer_tx: &mpsc::Sender<WriteOp>,
    source: &PyramidNode,
    new_id: &str,
    slug: &str,
    target_depth: i64,
    children: &[&str],
) {
    let mut node = source.clone();
    node.id = new_id.to_string();
    node.depth = target_depth;
    node.chunk_index = None;
    node.children = children.iter().map(|s| s.to_string()).collect();
    send_save_node(writer_tx, node, None).await;

    for child_id in children {
        send_update_parent(writer_tx, slug, child_id, new_id).await;
    }
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Execute a chain definition against a pyramid slug.
/// Returns (apex_node_id, failure_count).
pub async fn execute_chain(
    state: &PyramidState,
    chain: &ChainDefinition,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<(String, i32)> {
    let llm_config = state.config.read().await.clone();

    // Count chunks
    let slug_owned = slug.to_string();
    let num_chunks = db_read(&state.reader, {
        let s = slug_owned.clone();
        move |conn| db::count_chunks(conn, &s)
    })
    .await?;

    if num_chunks == 0 {
        return Err(anyhow!("No chunks found for slug '{slug}'"));
    }

    // Load chunks as Value array for ChainContext
    let chunks: Vec<Value> = {
        let mut items = Vec::new();
        for i in 0..num_chunks {
            let s = slug.to_string();
            let content = db_read(&state.reader, move |conn| {
                db::get_chunk(conn, &s, i)
            })
            .await?
            .unwrap_or_default();
            items.push(serde_json::json!({
                "index": i,
                "content": content,
            }));
        }
        items
    };

    // Check if prior build exists (for $has_prior_build)
    let has_prior_build = db_read(&state.reader, {
        let s = slug_owned.clone();
        move |conn| {
            let count = db::count_nodes_at_depth(conn, &s, 0)?;
            Ok(count > 0)
        }
    })
    .await?;

    let total = estimate_total(chain, num_chunks);
    let mut done: i64 = 0;
    let mut total_failures: i32 = 0;
    let mut apex_node_id = String::new();

    // Build chain context (from chain_resolve)
    let mut ctx = ChainContext::new(slug, &chain.content_type, chunks);
    ctx.has_prior_build = has_prior_build;

    // Build dispatch context (from chain_dispatch)
    let dispatch_ctx = chain_dispatch::StepContext {
        db_reader: state.reader.clone(),
        db_writer: state.writer.clone(),
        slug: slug.to_string(),
        config: llm_config.clone(),
    };

    // Set up writer channel + drain task
    let (writer_tx, writer_handle) = spawn_write_drain(state.writer.clone());

    send_progress(&progress_tx, 0, total).await;

    // Execute each step
    for (step_idx, step) in chain.steps.iter().enumerate() {
        if cancel.is_cancelled() {
            info!("Chain execution cancelled at step '{}'", step.name);
            break;
        }

        // Check `when` condition
        if !evaluate_when(step.when.as_deref(), &ctx) {
            info!("  Step '{}' skipped (when condition false)", step.name);
            continue;
        }

        let error_strategy = resolve_error_strategy(step, &chain.defaults);
        let saves_node = step.save_as.as_deref() == Some("node");

        info!(
            "[CHAIN] step \"{}\" started ({}/{}, primitive: {})",
            step.name,
            step_idx + 1,
            chain.steps.len(),
            step.primitive,
        );

        let step_result = if step.mechanical {
            execute_mechanical(step, &mut ctx, &dispatch_ctx, &chain.defaults).await
        } else if step.recursive_pair {
            let starting_depth = step.depth.unwrap_or(1);
            let (apex_id, failures) = execute_recursive_pair(
                step, starting_depth, &mut ctx, &dispatch_ctx, &chain.defaults,
                &error_strategy, saves_node, &writer_tx, &state.reader,
                cancel, &progress_tx, &mut done, total,
            )
            .await?;
            apex_node_id = apex_id;
            total_failures += failures;
            Ok(Value::Null)
        } else if step.pair_adjacent {
            let source_depth = step.depth.unwrap_or(0);
            let (outputs, failures) = execute_pair_adjacent(
                step, source_depth, &mut ctx, &dispatch_ctx, &chain.defaults,
                &error_strategy, saves_node, &writer_tx, &state.reader,
                cancel, &progress_tx, &mut done, total,
            )
            .await?;
            total_failures += failures;
            ctx.step_outputs.insert(step.name.clone(), Value::Array(outputs));
            Ok(Value::Null)
        } else if step.for_each.is_some() {
            let (outputs, failures) = execute_for_each(
                step, &mut ctx, &dispatch_ctx, &chain.defaults,
                &error_strategy, saves_node, &writer_tx, &state.reader,
                cancel, &progress_tx, &mut done, total,
            )
            .await?;
            total_failures += failures;
            ctx.step_outputs.insert(step.name.clone(), Value::Array(outputs));
            Ok(Value::Null)
        } else {
            execute_single(
                step, &mut ctx, &dispatch_ctx, &chain.defaults,
                &error_strategy, saves_node, &writer_tx, &state.reader,
                cancel, &progress_tx, &mut done, total,
            )
            .await
        };

        match step_result {
            Ok(output) => {
                info!("[CHAIN] step \"{}\" complete", step.name);
                if !output.is_null() {
                    ctx.step_outputs.insert(step.name.clone(), output);
                }
            }
            Err(e) => {
                match error_strategy {
                    ErrorStrategy::Abort => {
                        error!("[CHAIN] step \"{}\" FAILED (abort): {e}", step.name);
                        drop(writer_tx);
                        let _ = writer_handle.await;
                        return Err(anyhow!("Chain aborted at step '{}': {e}", step.name));
                    }
                    ErrorStrategy::Skip => {
                        warn!("[CHAIN] step \"{}\" FAILED (skip): {e}", step.name);
                        total_failures += 1;
                    }
                    _ => {
                        warn!("[CHAIN] step \"{}\" FAILED: {e}", step.name);
                        total_failures += 1;
                    }
                }
            }
        }
    }

    // Drop writer channel, await drain
    drop(writer_tx);
    let _ = writer_handle.await;

    // Update slug stats
    {
        let writer = state.writer.clone();
        let slug_owned = slug.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = writer.blocking_lock();
            db::update_slug_stats(&conn, &slug_owned)
        })
        .await;
    }

    // If no recursive_pair step produced an apex, find the highest-depth node
    if apex_node_id.is_empty() {
        let slug_owned = slug.to_string();
        apex_node_id = db_read(&state.reader, move |conn| {
            let max_depth: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(depth), 0) FROM pyramid_nodes WHERE slug = ?1",
                    rusqlite::params![&slug_owned],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            let nodes = db::get_nodes_at_depth(conn, &slug_owned, max_depth)?;
            Ok(nodes.first().map(|n| n.id.clone()).unwrap_or_default())
        })
        .await?;
    }

    info!(
        "Chain '{}' complete for slug '{}': apex={}, failures={}",
        chain.name, slug, apex_node_id, total_failures
    );

    Ok((apex_node_id, total_failures))
}

// ── forEach execution ───────────────────────────────────────────────────────

async fn execute_for_each(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
    reader: &Arc<Mutex<Connection>>,
    cancel: &CancellationToken,
    progress_tx: &Option<mpsc::Sender<BuildProgress>>,
    done: &mut i64,
    total: i64,
) -> Result<(Vec<Value>, i32)> {
    // Resolve the items to iterate over
    let for_each_ref = step.for_each.as_deref().unwrap_or("$chunks");
    let items = match ctx.resolve_ref(for_each_ref) {
        Ok(Value::Array(arr)) => arr,
        Ok(_) => {
            warn!("forEach ref '{}' did not resolve to array", for_each_ref);
            Vec::new()
        }
        Err(e) => {
            warn!("Could not resolve forEach ref '{}': {e}", for_each_ref);
            Vec::new()
        }
    };

    info!("[CHAIN] [{}] forEach: {} items", step.name, items.len());
    let mut outputs: Vec<Value> = Vec::with_capacity(items.len());
    let mut failures: i32 = 0;

    // Initialize accumulators from step.accumulate config
    if let Some(ref acc_config) = step.accumulate {
        if let Value::Object(acc_map) = acc_config {
            for (name, config) in acc_map {
                let init = config
                    .get("init")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                ctx.accumulators.insert(name.clone(), init.to_string());
            }
        }
    }

    let instruction = step.instruction.as_deref().unwrap_or("");

    for (index, item) in items.iter().enumerate() {
        if cancel.is_cancelled() {
            info!("forEach cancelled at iteration {index}");
            break;
        }

        let chunk_index = item
            .get("index")
            .and_then(|v| v.as_i64())
            .unwrap_or(index as i64);

        let depth = step.depth.unwrap_or(0);
        let node_id = if let Some(ref pattern) = step.node_id_pattern {
            generate_node_id(pattern, index, Some(depth))
        } else {
            format!("L{depth}-{index:03}")
        };

        // Check resume state
        let resume = get_resume_state(
            reader, &ctx.slug, &step.name, chunk_index, depth, &node_id, saves_node,
        )
        .await?;

        match resume {
            ResumeState::Complete => {
                info!("[CHAIN] [{}] {} -- resumed (complete)", step.name, node_id);

                // For sequential steps, replay output to reconstruct accumulators
                if step.sequential {
                    let slug_owned = ctx.slug.clone();
                    let step_name = step.name.clone();
                    let ci = chunk_index;
                    if let Ok(Some(json_str)) = db_read(reader, move |conn| {
                        db::get_step_output(conn, &slug_owned, &step_name, ci)
                    })
                    .await
                    {
                        if let Ok(prior_output) = serde_json::from_str::<Value>(&json_str) {
                            update_accumulators(&mut ctx.accumulators, &prior_output, step);
                            outputs.push(prior_output);
                        } else {
                            outputs.push(Value::Null);
                        }
                    } else {
                        outputs.push(Value::Null);
                    }
                } else {
                    outputs.push(Value::Null);
                }

                *done += 1;
                send_progress(progress_tx, *done, total).await;
                continue;
            }
            ResumeState::StaleStep => {
                warn!("[CHAIN] [{}] {} -- stale step (node missing), rebuilding", step.name, node_id);
            }
            ResumeState::Missing => {}
        }

        // Set up forEach loop variables on the context
        ctx.current_item = Some(item.clone());
        ctx.current_index = Some(index);

        // Resolve step input using the context (handles $item, $index, $running_context, etc.)
        let resolved_input = if let Some(ref input) = step.input {
            ctx.resolve_value(input)?
        } else {
            item.clone()
        };

        // Resolve prompt template
        let system_prompt = match resolve_prompt_template(instruction, &resolved_input) {
            Ok(s) => s,
            Err(_) => instruction.to_string(), // Fallback: use raw instruction
        };

        // Dispatch with retry
        let fallback_key = format!("{}-{index}", step.name);
        let t0 = Instant::now();

        match dispatch_with_retry(
            step, &resolved_input, &system_prompt, defaults, dispatch_ctx,
            error_strategy, &fallback_key,
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();

                // Save step output
                let output_json = serde_json::to_string(&analysis)?;
                send_save_step(
                    writer_tx, &ctx.slug, &step.name, chunk_index, depth,
                    &node_id, &output_json,
                    &dispatch_ctx.config.primary_model, elapsed,
                )
                .await;

                // Save node if configured
                if saves_node {
                    let node = build_node_from_output(
                        &analysis, &node_id, &ctx.slug, depth, Some(chunk_index),
                    )?;
                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    // Wire parent_id on children (e.g., L1 thread → L0 source nodes)
                    let child_ids = node.children.clone();
                    send_save_node(writer_tx, node, Some(topics_json)).await;
                    for child_id in &child_ids {
                        send_update_parent(writer_tx, &ctx.slug, child_id, &node_id).await;
                    }
                }

                // Update accumulators for sequential steps
                if step.sequential {
                    update_accumulators(&mut ctx.accumulators, &analysis, step);
                }

                outputs.push(analysis);
                info!("[CHAIN] [{}] {node_id} complete ({elapsed:.1}s)", step.name);
            }
            Err(e) => {
                match error_strategy {
                    ErrorStrategy::Abort => {
                        return Err(anyhow!("forEach abort at index {index}: {e}"));
                    }
                    _ => {
                        warn!("[CHAIN] [{}] {node_id} FAILED (skip): {e}", step.name);
                        failures += 1;
                        outputs.push(Value::Null);
                    }
                }
            }
        }

        *done += 1;
        send_progress(progress_tx, *done, total).await;
    }

    // Clear forEach loop variables
    ctx.current_item = None;
    ctx.current_index = None;

    Ok((outputs, failures))
}

/// Update accumulators from a step output based on the accumulate config.
fn update_accumulators(
    accumulators: &mut HashMap<String, String>,
    output: &Value,
    step: &ChainStep,
) {
    if let Some(ref acc_config) = step.accumulate {
        if let Value::Object(acc_map) = acc_config {
            for (name, config) in acc_map {
                if let Some(from_expr) = config.get("from").and_then(|v| v.as_str()) {
                    // Strip "$item.output." or "$item." prefix to navigate the output
                    let path = from_expr
                        .strip_prefix("$item.output.")
                        .or_else(|| from_expr.strip_prefix("$item."))
                        .unwrap_or(from_expr);

                    // Navigate the output value
                    let mut current = output.clone();
                    let mut found = true;
                    for part in path.split('.') {
                        if let Some(next) = current.get(part) {
                            current = next.clone();
                        } else {
                            found = false;
                            break;
                        }
                    }

                    if found {
                        let new_val = match &current {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };

                        // Apply max_chars truncation if configured
                        let max_chars = config
                            .get("max_chars")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(u64::MAX) as usize;

                        let truncated = if new_val.len() > max_chars {
                            new_val[..max_chars].to_string()
                        } else {
                            new_val
                        };

                        accumulators.insert(name.clone(), truncated);
                    }
                }
            }
        }
    }
}

// ── pair_adjacent execution ─────────────────────────────────────────────────

async fn execute_pair_adjacent(
    step: &ChainStep,
    source_depth: i64,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
    reader: &Arc<Mutex<Connection>>,
    cancel: &CancellationToken,
    progress_tx: &Option<mpsc::Sender<BuildProgress>>,
    done: &mut i64,
    total: i64,
) -> Result<(Vec<Value>, i32)> {
    let target_depth = source_depth + 1;

    // Get nodes at source depth
    let slug_owned = ctx.slug.clone();
    let source_nodes = db_read(reader, {
        let s = slug_owned.clone();
        move |conn| db::get_nodes_at_depth(conn, &s, source_depth)
    })
    .await?;

    if source_nodes.len() <= 1 {
        info!("[CHAIN] pair_adjacent: {} node(s) at depth {source_depth}, nothing to pair", source_nodes.len());
        return Ok((Vec::new(), 0));
    }

    let mut outputs = Vec::new();
    let mut failures: i32 = 0;
    let instruction = step.instruction.as_deref().unwrap_or("");

    let mut pair_idx: usize = 0;
    let mut i: usize = 0;

    while i < source_nodes.len() {
        if cancel.is_cancelled() {
            break;
        }

        let node_id = if let Some(ref pattern) = step.node_id_pattern {
            generate_node_id(pattern, pair_idx, Some(target_depth))
        } else {
            format!("L{target_depth}-{pair_idx:03}")
        };

        // Resume check
        let resume = get_resume_state(
            reader, &ctx.slug, &step.name, -1, target_depth, &node_id, saves_node,
        )
        .await?;

        match resume {
            ResumeState::Complete => {
                info!("  [{}] {node_id} -- resumed (complete)", step.name);
                pair_idx += 1;
                i += 2;
                *done += 1;
                send_progress(progress_tx, *done, total).await;
                outputs.push(Value::Null);
                continue;
            }
            ResumeState::StaleStep => {
                warn!("[CHAIN] [{}] {node_id} -- stale, rebuilding", step.name);
            }
            ResumeState::Missing => {}
        }

        if i + 1 < source_nodes.len() {
            let left = &source_nodes[i];
            let right = &source_nodes[i + 1];

            match dispatch_pair(
                step, ctx, dispatch_ctx, defaults, error_strategy,
                instruction, left, right, &node_id, target_depth, pair_idx,
                saves_node, writer_tx,
            )
            .await
            {
                Ok(analysis) => outputs.push(analysis),
                Err(e) => {
                    match error_strategy {
                        ErrorStrategy::Abort => {
                            return Err(anyhow!("pair_adjacent abort at pair {pair_idx}: {e}"));
                        }
                        ErrorStrategy::CarryLeft | ErrorStrategy::CarryUp => {
                            warn!("[CHAIN] [{}] pair {pair_idx} FAILED, carrying left node: {e}", step.name);
                            carry_node_up(
                                writer_tx, left, &node_id, &ctx.slug, target_depth,
                                &[&left.id, &right.id],
                            )
                            .await;
                            failures += 1;
                            outputs.push(Value::Null);
                        }
                        _ => {
                            warn!("[CHAIN] [{}] pair {pair_idx} FAILED (skip): {e}", step.name);
                            failures += 1;
                            outputs.push(Value::Null);
                        }
                    }
                }
            }

            i += 2;
        } else {
            // Odd node: carry up without LLM call
            let carry = &source_nodes[i];
            info!("[CHAIN] [{}] carry up odd node: {} -> {node_id}", step.name, carry.id);
            carry_node_up(writer_tx, carry, &node_id, &ctx.slug, target_depth, &[&carry.id]).await;
            outputs.push(Value::Null);
            i += 1;
        }

        pair_idx += 1;
        *done += 1;
        send_progress(progress_tx, *done, total).await;
    }

    // Clear pair variables
    ctx.pair_left = None;
    ctx.pair_right = None;
    ctx.pair_depth = None;
    ctx.pair_index = None;
    ctx.pair_is_carry = false;

    Ok((outputs, failures))
}

/// Dispatch a pair of nodes through the LLM and save the result.
async fn dispatch_pair(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    instruction: &str,
    left: &PyramidNode,
    right: &PyramidNode,
    node_id: &str,
    target_depth: i64,
    pair_idx: usize,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
) -> Result<Value> {
    let left_payload = child_payload_json(left);
    let right_payload = child_payload_json(right);

    // Set pair variables on context for resolution
    ctx.pair_left = Some(serde_json::to_value(&left_payload)?);
    ctx.pair_right = Some(serde_json::to_value(&right_payload)?);
    ctx.pair_depth = Some(target_depth);
    ctx.pair_index = Some(pair_idx);
    ctx.pair_is_carry = false;

    // Resolve step input
    let resolved_input = if let Some(ref input) = step.input {
        ctx.resolve_value(input)?
    } else {
        serde_json::json!({
            "left": left_payload,
            "right": right_payload,
        })
    };

    // Resolve prompt template
    let system_prompt = match resolve_prompt_template(instruction, &resolved_input) {
        Ok(s) => s,
        Err(_) => instruction.to_string(),
    };

    let fallback_key = format!("{}-d{target_depth}-{pair_idx}", step.name);
    let t0 = Instant::now();

    let analysis = dispatch_with_retry(
        step, &resolved_input, &system_prompt, defaults, dispatch_ctx,
        error_strategy, &fallback_key,
    )
    .await?;

    let elapsed = t0.elapsed().as_secs_f64();

    // Save step
    let output_json = serde_json::to_string(&analysis)?;
    send_save_step(
        writer_tx, &ctx.slug, &step.name, -1, target_depth, node_id,
        &output_json, &dispatch_ctx.config.primary_model, elapsed,
    )
    .await;

    // Save node
    if saves_node {
        let mut node = build_node_from_output(
            &analysis, node_id, &ctx.slug, target_depth, None,
        )?;
        node.children = vec![left.id.clone(), right.id.clone()];
        let topics_json = serde_json::to_string(
            analysis.get("topics").unwrap_or(&serde_json::json!([])),
        )?;
        send_save_node(writer_tx, node, Some(topics_json)).await;

        send_update_parent(writer_tx, &ctx.slug, &left.id, node_id).await;
        send_update_parent(writer_tx, &ctx.slug, &right.id, node_id).await;
    }

    info!("[CHAIN] [{} + {}] -> {node_id} ({elapsed:.1}s)", left.id, right.id);

    Ok(analysis)
}

// ── recursive_pair execution ────────────────────────────────────────────────

async fn execute_recursive_pair(
    step: &ChainStep,
    starting_depth: i64,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
    reader: &Arc<Mutex<Connection>>,
    cancel: &CancellationToken,
    progress_tx: &Option<mpsc::Sender<BuildProgress>>,
    done: &mut i64,
    total: i64,
) -> Result<(String, i32)> {
    let mut total_failures: i32 = 0;
    let mut depth = starting_depth;
    let slug_owned = ctx.slug.clone();
    let instruction = step.instruction.as_deref().unwrap_or("");

    loop {
        if cancel.is_cancelled() {
            return Ok((String::new(), total_failures));
        }

        let current_nodes = db_read(reader, {
            let s = slug_owned.clone();
            move |conn| db::get_nodes_at_depth(conn, &s, depth)
        })
        .await?;

        if current_nodes.len() <= 1 {
            let apex_id = current_nodes
                .first()
                .map(|n| n.id.clone())
                .unwrap_or_default();
            if !apex_id.is_empty() {
                info!("[CHAIN] === APEX: {apex_id} at depth {depth} ===");
            }
            return Ok((apex_id, total_failures));
        }

        let target_depth = depth + 1;
        let expected = (current_nodes.len() + 1) / 2;

        // Check if this depth is already complete
        let existing = db_read(reader, {
            let s = slug_owned.clone();
            move |conn| db::count_nodes_at_depth(conn, &s, target_depth)
        })
        .await?;

        if existing >= expected as i64 {
            info!("[CHAIN] depth {target_depth}: {existing} nodes (already complete)");
            *done += existing;
            send_progress(progress_tx, *done, total).await;
            depth = target_depth;
            continue;
        }

        info!(
            "=== DEPTH {target_depth}: PAIR {} -> {expected} ===",
            current_nodes.len()
        );

        let mut pair_idx: usize = 0;
        let mut i: usize = 0;

        while i < current_nodes.len() {
            if cancel.is_cancelled() {
                return Ok((String::new(), total_failures));
            }

            let node_id = if let Some(ref pattern) = step.node_id_pattern {
                generate_node_id(pattern, pair_idx, Some(target_depth))
            } else {
                format!("L{target_depth}-{pair_idx:03}")
            };

            // Resume check
            let resume = get_resume_state(
                reader, &ctx.slug, &step.name, -1, target_depth, &node_id, saves_node,
            )
            .await?;

            match resume {
                ResumeState::Complete => {
                    pair_idx += 1;
                    i += 2;
                    *done += 1;
                    send_progress(progress_tx, *done, total).await;
                    continue;
                }
                ResumeState::StaleStep => {
                    warn!("[CHAIN] [{}] {node_id} -- stale, rebuilding", step.name);
                }
                ResumeState::Missing => {}
            }

            if i + 1 < current_nodes.len() {
                let left = &current_nodes[i];
                let right = &current_nodes[i + 1];

                match dispatch_pair(
                    step, ctx, dispatch_ctx, defaults, error_strategy,
                    instruction, left, right, &node_id, target_depth, pair_idx,
                    saves_node, writer_tx,
                )
                .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        match error_strategy {
                            ErrorStrategy::Abort => {
                                return Err(anyhow!(
                                    "recursive_pair abort at depth {target_depth} pair {pair_idx}: {e}"
                                ));
                            }
                            ErrorStrategy::CarryLeft | ErrorStrategy::CarryUp => {
                                warn!(
                                    "  [{}] Pair {pair_idx} at depth {target_depth} failed, carrying left: {e}",
                                    step.name
                                );
                                carry_node_up(
                                    writer_tx, left, &node_id, &ctx.slug, target_depth,
                                    &[&left.id, &right.id],
                                )
                                .await;
                                total_failures += 1;
                            }
                            _ => {
                                warn!(
                                    "  [{}] Pair {pair_idx} at depth {target_depth} failed (skip): {e}",
                                    step.name
                                );
                                total_failures += 1;
                            }
                        }
                    }
                }

                i += 2;
            } else {
                // Carry up odd node
                let carry = &current_nodes[i];
                info!("[CHAIN] [{}] carry up odd: {} -> {node_id}", step.name, carry.id);
                carry_node_up(writer_tx, carry, &node_id, &ctx.slug, target_depth, &[&carry.id]).await;
                i += 1;
            }

            pair_idx += 1;
            *done += 1;
            send_progress(progress_tx, *done, total).await;
        }

        // Flush: wait for the async writer to commit all pending nodes at
        // target_depth before we read them back in the next iteration.
        // Without this, the DB read may see fewer nodes than were just created,
        // causing premature apex declaration.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        depth = target_depth;
    }
}

// ── Single step execution ───────────────────────────────────────────────────

async fn execute_single(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
    reader: &Arc<Mutex<Connection>>,
    _cancel: &CancellationToken,
    progress_tx: &Option<mpsc::Sender<BuildProgress>>,
    done: &mut i64,
    total: i64,
) -> Result<Value> {
    let depth = step.depth.unwrap_or(0);
    let node_id = if let Some(ref pattern) = step.node_id_pattern {
        generate_node_id(pattern, 0, Some(depth))
    } else {
        format!("L{depth}-000")
    };

    // Resume check
    let resume = get_resume_state(
        reader, &ctx.slug, &step.name, -1, depth, &node_id, saves_node,
    )
    .await?;

    if resume == ResumeState::Complete {
        info!("  [{}] {node_id} -- resumed (complete)", step.name);
        *done += 1;
        send_progress(progress_tx, *done, total).await;

        // Load and return prior output
        let slug_owned = ctx.slug.clone();
        let step_name = step.name.clone();
        if let Ok(Some(json_str)) = db_read(reader, move |conn| {
            db::get_step_output(conn, &slug_owned, &step_name, -1)
        })
        .await
        {
            if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                return Ok(val);
            }
        }
        return Ok(Value::Null);
    }

    if resume == ResumeState::StaleStep {
        warn!("[CHAIN] [{}] {node_id} -- stale, rebuilding", step.name);
    }

    let instruction = step.instruction.as_deref().unwrap_or("");

    // Resolve step input
    let resolved_input = if let Some(ref input) = step.input {
        ctx.resolve_value(input)?
    } else {
        Value::Object(serde_json::Map::new())
    };

    // Resolve prompt template
    let system_prompt = match resolve_prompt_template(instruction, &resolved_input) {
        Ok(s) => s,
        Err(_) => instruction.to_string(),
    };

    let fallback_key = format!("{}-single", step.name);
    let t0 = Instant::now();

    let analysis = dispatch_with_retry(
        step, &resolved_input, &system_prompt, defaults, dispatch_ctx,
        error_strategy, &fallback_key,
    )
    .await?;

    let elapsed = t0.elapsed().as_secs_f64();

    // Save step
    let output_json = serde_json::to_string(&analysis)?;
    send_save_step(
        writer_tx, &ctx.slug, &step.name, -1, depth, &node_id,
        &output_json, &dispatch_ctx.config.primary_model, elapsed,
    )
    .await;

    // Save node if configured
    if saves_node {
        let node = build_node_from_output(
            &analysis, &node_id, &ctx.slug, depth, None,
        )?;
        let topics_json = serde_json::to_string(
            analysis.get("topics").unwrap_or(&serde_json::json!([])),
        )?;
        send_save_node(writer_tx, node, Some(topics_json)).await;
    }

    *done += 1;
    send_progress(progress_tx, *done, total).await;

    info!("[CHAIN] [{}] {node_id} complete ({elapsed:.1}s)", step.name);

    Ok(analysis)
}

// ── Mechanical step execution ───────────────────────────────────────────────

/// Execute a mechanical (non-LLM) step by dispatching through chain_dispatch.
async fn execute_mechanical(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
) -> Result<Value> {
    info!("[CHAIN] mechanical step \"{}\" dispatching...", step.name);

    // Resolve input
    let resolved_input = if let Some(ref input) = step.input {
        ctx.resolve_value(input)?
    } else {
        Value::Object(serde_json::Map::new())
    };

    chain_dispatch::dispatch_step(step, &resolved_input, "", defaults, dispatch_ctx).await
}
