// pyramid/build.rs — shared chain-engine helpers + topical-vine dispatch.
//
// walker-v3 W3a retired the legacy content-type dispatchers
// (`build_conversation` / `build_code` / `build_docs`) and their
// helpers (`build_l1_pairing`, `build_threads_layer`,
// `build_upper_layers`, `flatten_analysis`, `node_from_analysis`,
// `get_resume_state`, `extract_import_graph`, `cluster_by_imports`,
// `ResumeState`, `ImportGraph` + friends) along with the
// `use_chain_engine: false` branch in `build_runner`. What stays:
//
//   - `WriteOp` + `call_and_parse` — used by chain_executor and vine.rs
//   - `send_save_node` / `send_save_step` / `send_update_parent` /
//     `flush_writes` — shared chain-engine save primitives
//   - `child_payload_json` / `episodic_child_payload_json` /
//     `compact_child_payload` / `truncate_text` — shared payload helpers
//   - `build_topical_vine` — the one content-type-specific build entry
//     still here; internally it loads the topical-vine chain YAML and
//     delegates to `chain_executor::execute_chain_from`.

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::llm::{call_model_and_ctx, extract_json, LlmConfig};
use super::step_context::make_step_ctx_from_llm_config;
use super::types::*;

// ── WriteOp ──────────────────────────────────────────────────────────────────

/// Message type for the DB writer channel.  All DB mutations flow through a
/// single writer task so the rusqlite `Connection` is never shared across threads.
#[derive(Debug)]
pub enum WriteOp {
    SaveNode {
        node: PyramidNode,
        topics_json: Option<String>,
    },
    SaveStep {
        slug: String,
        step_type: String,
        chunk_index: i64,
        depth: i64,
        node_id: String,
        output_json: String,
        model: String,
        elapsed: f64,
    },
    UpdateParent {
        slug: String,
        node_id: String,
        parent_id: String,
    },
    UpdateStats {
        slug: String,
    },
    /// Record a file→node mapping in pyramid_file_hashes after L0 extraction.
    UpdateFileHash {
        slug: String,
        file_path: String,
        node_id: String,
    },
    Flush {
        done: oneshot::Sender<()>,
    },
}

// ── HELPERS ──────────────────────────────────────────────────────────────────

/// Call the LLM and parse JSON from the response.  On parse failure, retry once
/// at temperature 0.1.  Returns the parsed JSON value.
pub(crate) async fn call_and_parse(
    config: &LlmConfig,
    system: &str,
    user: &str,
    fallback_key: &str,
) -> Result<Value> {
    let cache_ctx = make_step_ctx_from_llm_config(
        config,
        fallback_key,
        "build_call_and_parse",
        0,
        None,
        system,
    );
    let resp = call_model_and_ctx(config, cache_ctx.as_ref(), system, user, 0.3, 50_000).await?;
    match extract_json(&resp) {
        Ok(v) => Ok(v),
        Err(_) => {
            info!("  JSON parse error on {fallback_key}, retrying at temp 0.1...");
            // Retry at lower temperature uses the same StepContext so
            // cache key is different but provenance is consistent.
            let retry_ctx = make_step_ctx_from_llm_config(
                config,
                &format!("{}_retry", fallback_key),
                "build_call_and_parse",
                0,
                None,
                system,
            );
            let resp2 =
                call_model_and_ctx(config, retry_ctx.as_ref(), system, user, 0.1, 50_000).await?;
            extract_json(&resp2)
                .map_err(|e| anyhow!("JSON parse failed twice for {fallback_key}: {e}"))
        }
    }
}

/// Send a SaveNode WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_save_node(
    writer_tx: &mpsc::Sender<WriteOp>,
    node: PyramidNode,
    topics_json: Option<String>,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::SaveNode { node, topics_json })
        .await
    {
        warn!("Writer channel closed, SaveNode dropped: {e}");
    }
}

/// Send a SaveStep WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_save_step(
    writer_tx: &mpsc::Sender<WriteOp>,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
    output_json: &str,
    model: &str,
    elapsed: f64,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::SaveStep {
            slug: slug.to_string(),
            step_type: step_type.to_string(),
            chunk_index,
            depth,
            node_id: node_id.to_string(),
            output_json: output_json.to_string(),
            model: model.to_string(),
            elapsed,
        })
        .await
    {
        warn!("Writer channel closed, SaveStep dropped: {e}");
    }
}

/// Send an UpdateParent WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_update_parent(
    writer_tx: &mpsc::Sender<WriteOp>,
    slug: &str,
    node_id: &str,
    parent_id: &str,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::UpdateParent {
            slug: slug.to_string(),
            node_id: node_id.to_string(),
            parent_id: parent_id.to_string(),
        })
        .await
    {
        warn!("Writer channel closed, UpdateParent dropped: {e}");
    }
}

/// Wait until all previously queued writer operations have been applied.
pub(crate) async fn flush_writes(writer_tx: &mpsc::Sender<WriteOp>) {
    let (tx, rx) = oneshot::channel();
    if let Err(e) = writer_tx.send(WriteOp::Flush { done: tx }).await {
        warn!("Writer channel closed, Flush dropped: {e}");
        return;
    }
    let _ = rx.await;
}

// ── TOPICAL VINE PIPELINE (Phase 16) ─────────────────────────────────────────

/// Build a topical vine by dispatching the `topical-vine` chain through the
/// chain executor. Used by vine-of-vines composition (Phase 16) and folder
/// ingestion (Phase 17).
///
/// Unlike the other `build_*` functions in this module, this function takes a
/// `&PyramidState` reference because the chain executor owns all of the
/// pipeline state (reader, writer, operational config, event bus, cache
/// access, etc.). The function loads the topical-vine chain via
/// `chain_loader::discover_chains` + `chain_loader::load_chain`, then invokes
/// `chain_executor::execute_chain_from` exactly the way `build_runner::run_chain_build`
/// does for non-vine content types. Returns `Result<i32>` where the
/// value is the failure count; 0 = clean build.
///
/// See `docs/specs/vine-of-vines-and-folder-ingestion.md` (Part 1) and
/// `chains/defaults/topical-vine.yaml`.
pub async fn build_topical_vine(
    state: &crate::pyramid::PyramidState,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
) -> Result<i32> {
    use crate::pyramid::chain_executor;
    use crate::pyramid::chain_loader;
    use crate::pyramid::chain_registry;

    // Three-tier chain resolution: per-slug override → content-type default → safety net.
    let chain_id = {
        let conn = state.reader.lock().await;
        chain_registry::resolve_chain_for_slug(&conn, slug, "vine", "deep")?
    };

    // Locate the chain YAML in the chains directory.
    let chains_dir = state.chains_dir.clone();
    let all_chains = chain_loader::discover_chains(&chains_dir)?;
    let meta = all_chains
        .iter()
        .find(|m| m.id == chain_id)
        .ok_or_else(|| {
            anyhow!(
                "topical-vine chain '{}' not found in chains directory ({})",
                chain_id,
                chains_dir.display()
            )
        })?;

    let yaml_path = std::path::Path::new(&meta.file_path);
    let chain = chain_loader::load_chain(yaml_path, &chains_dir)?;

    info!(
        slug,
        chain = %chain.id,
        steps = chain.steps.len(),
        "build_topical_vine: dispatching topical vine chain"
    );

    // Execute. The chain executor handles cross_build_input (which returns
    // the vine's registered children as of Phase 16), topical clustering,
    // per-cluster synthesis, webbing, and recursive_pair up to apex.
    let (apex_id, failures, _step_activities) = chain_executor::execute_chain_from(
        state,
        &chain,
        slug,
        0,    // from_depth
        None, // stop_after
        None, // force_from
        cancel,
        Some(progress_tx.clone()),
        None, // layer_tx
        None, // initial_context
    )
    .await?;

    info!(
        slug,
        chain = %chain.id,
        apex = %apex_id,
        failures,
        "build_topical_vine: topical vine chain complete"
    );

    Ok(failures)
}

/// Build a JSON payload from a node, preferring topics if available.
pub(crate) fn child_payload_json(node: &PyramidNode) -> Value {
    if !node.topics.is_empty() {
        serde_json::json!({
            "headline": node.headline,
            "distilled": node.distilled,
            "topics": node.topics,
        })
    } else {
        serde_json::json!({
            "headline": node.headline,
            "distilled": node.distilled,
            "corrections": node.corrections,
            "decisions": node.decisions,
            "terms": node.terms,
        })
    }
}

/// Episodic-aware child payload for vine synthesis with synthesize_recursive.md.
/// Includes narrative, entities, key_quotes, transitions, time_range, weight —
/// the fields that child_payload_json strips but the episodic prompt expects.
pub(crate) fn episodic_child_payload_json(node: &PyramidNode) -> Value {
    // Narrative: prefer multi-zoom level 0, fallback to distilled
    let narrative_text = node
        .narrative
        .levels
        .first()
        .map(|l| l.text.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(&node.distilled);

    let mut payload = serde_json::json!({
        "headline": node.headline,
        "topics": node.topics,
        "narrative": narrative_text,
    });

    if let Some(ref tr) = node.time_range {
        payload["time_range"] = serde_json::to_value(tr).unwrap_or_default();
    }

    // Weight as object shape matching synthesize_recursive.md contract
    if node.weight > 0.0 {
        payload["weight"] =
            serde_json::json!({"tokens": node.weight, "turns": 0, "fraction_of_parent": 0.0});
    }

    if !node.decisions.is_empty() {
        payload["decisions"] = serde_json::to_value(&node.decisions).unwrap_or_default();
    }

    if !node.entities.is_empty() {
        payload["entities"] = serde_json::to_value(&node.entities).unwrap_or_default();
    }

    if !node.key_quotes.is_empty() {
        payload["key_quotes"] = serde_json::to_value(&node.key_quotes).unwrap_or_default();
    }

    if !node.transitions.prior.is_empty() || !node.transitions.next.is_empty() {
        payload["transitions"] = serde_json::to_value(&node.transitions).unwrap_or_default();
    }

    payload
}

/// Compact version of child_payload_json for upper-layer synthesis.
/// Preserves topics (needed for synthesis quality) but truncates verbose text:
/// - `distilled` capped to `max_distilled_chars`
/// - each topic's `current` capped to `max_topic_chars`
/// - corrections/decisions dropped (only topic summaries matter at L3+)
pub(crate) fn compact_child_payload(
    node: &PyramidNode,
    max_distilled_chars: usize,
    max_topic_chars: usize,
) -> Value {
    let distilled = truncate_text(&node.distilled, max_distilled_chars);

    let compact_topics: Vec<Value> = node
        .topics
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "current": truncate_text(&t.current, max_topic_chars),
                "entities": t.entities,
            })
        })
        .collect();

    serde_json::json!({
        "headline": node.headline,
        "distilled": distilled,
        "topics": compact_topics,
    })
}

/// Truncate text to max_chars (char-safe), appending "…" if truncated.
fn truncate_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(max_chars).collect();
        format!("{prefix}…")
    }
}
