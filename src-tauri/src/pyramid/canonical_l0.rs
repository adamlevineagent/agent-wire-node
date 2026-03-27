// pyramid/canonical_l0.rs — Canonical L0 extraction module
//
// Extracts generic, question-independent L0 nodes from source chunks.
// These nodes form the reusable foundation for all question-shaped builds.
//
// Node IDs use the pattern `C-L0-{index:03}` (deterministic, matches source order).
// self_prompt is empty (no question bias).
//
// See docs/plans/two-pass-l0-contracts.md §Canonical L0 Extraction Contract.

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use super::chain_dispatch::build_node_from_output;
use super::db;
use super::llm::{self, LlmConfig};
use super::types::BuildProgress;
use super::PyramidState;

/// The system prompt for canonical (generic) extraction.
const CANONICAL_EXTRACTION_SYSTEM: &str =
    "You are extracting knowledge from source material. Describe what this document contains \
     comprehensively. Cover all major concepts, systems, decisions, relationships, and technical \
     details. Be thorough — this extraction will be the foundation for answering many different \
     questions later. Write in clear, accessible language.\n\n\
     Respond with a JSON object containing:\n\
     - \"headline\": A concise title for this chunk's content (under 80 chars)\n\
     - \"distilled\": A comprehensive summary of the chunk's content (multiple paragraphs OK)\n\
     - \"topics\": Array of {\"name\": string, \"current\": string, \"entities\": [string], \
       \"corrections\": [], \"decisions\": []} for each major topic covered\n\
     - \"corrections\": Array of {\"wrong\": string, \"right\": string, \"who\": string} for any \
       corrections found\n\
     - \"decisions\": Array of {\"decided\": string, \"why\": string, \"rejected\": string} for \
       any design decisions found\n\
     - \"terms\": Array of {\"term\": string, \"definition\": string} for domain-specific terminology\n\
     - \"dead_ends\": Array of strings describing approaches that were tried and abandoned";

/// JSON schema for structured output, matching the PyramidNode extraction format.
fn canonical_extraction_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "headline": { "type": "string" },
            "distilled": { "type": "string" },
            "topics": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "current": { "type": "string" },
                        "entities": { "type": "array", "items": { "type": "string" } },
                        "corrections": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "wrong": { "type": "string" },
                                    "right": { "type": "string" },
                                    "who": { "type": "string" }
                                },
                                "required": ["wrong", "right", "who"],
                                "additionalProperties": false
                            }
                        },
                        "decisions": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "decided": { "type": "string" },
                                    "why": { "type": "string" },
                                    "rejected": { "type": "string" }
                                },
                                "required": ["decided", "why", "rejected"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["name", "current", "entities", "corrections", "decisions"],
                    "additionalProperties": false
                }
            },
            "corrections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "wrong": { "type": "string" },
                        "right": { "type": "string" },
                        "who": { "type": "string" }
                    },
                    "required": ["wrong", "right", "who"],
                    "additionalProperties": false
                }
            },
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "decided": { "type": "string" },
                        "why": { "type": "string" },
                        "rejected": { "type": "string" }
                    },
                    "required": ["decided", "why", "rejected"],
                    "additionalProperties": false
                }
            },
            "terms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "term": { "type": "string" },
                        "definition": { "type": "string" }
                    },
                    "required": ["term", "definition"],
                    "additionalProperties": false
                }
            },
            "dead_ends": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["headline", "distilled", "topics", "corrections", "decisions", "terms", "dead_ends"],
        "additionalProperties": false
    })
}

/// Extract canonical L0 from source material.
///
/// Uses a generic (non-question-shaped) extraction prompt to comprehensively
/// describe what each chunk contains. These canonical L0 nodes are reused
/// across different question builds for the same corpus.
///
/// Returns the count of canonical L0 nodes created (or existing if skipped).
pub async fn extract_canonical_l0(
    state: &Arc<PyramidState>,
    slug: &str,
    cancel: &tokio_util::sync::CancellationToken,
    progress_tx: Option<tokio::sync::watch::Sender<BuildProgress>>,
) -> Result<i32> {
    // 1. Check if canonical L0 already exists — reuse if so
    {
        let conn = state.reader.lock().await;
        if db::has_canonical_l0(&conn, slug)? {
            let nodes = db::get_canonical_l0_nodes(&conn, slug)?;
            let count = nodes.len() as i32;
            info!(
                "[canonical_l0] slug '{}': {} canonical L0 nodes already exist, skipping extraction",
                slug, count
            );
            return Ok(count);
        }
    }

    // 2. Load all chunks for this slug
    let chunks: Vec<(i64, String)> = {
        let conn = state.reader.lock().await;
        db::get_all_chunks(&conn, slug)?
    };

    if chunks.is_empty() {
        warn!(
            "[canonical_l0] slug '{}': no chunks found, cannot extract canonical L0",
            slug
        );
        return Ok(0);
    }

    let total_chunks = chunks.len();
    info!(
        "[canonical_l0] slug '{}': extracting canonical L0 from {} chunks",
        slug, total_chunks
    );

    // Send initial progress
    if let Some(ref tx) = progress_tx {
        let _ = tx.send(BuildProgress {
            done: 0,
            total: total_chunks as i64,
        });
    }

    // 3. Get LLM config
    let config = state.config.read().await.clone();

    // 4. Build response format for structured output
    let schema = canonical_extraction_schema();
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "canonical_l0_extraction",
            "strict": true,
            "schema": schema
        }
    });

    // 5. Parallel LLM calls with semaphore(8)
    let semaphore = Arc::new(Semaphore::new(8));
    let mut handles = Vec::new();

    for (idx, (chunk_index, content)) in chunks.into_iter().enumerate() {
        if cancel.is_cancelled() {
            warn!("[canonical_l0] slug '{}': cancelled during extraction", slug);
            break;
        }

        let permit = semaphore.clone().acquire_owned().await?;
        let config = config.clone();
        let slug_owned = slug.to_string();
        let response_format = response_format.clone();
        let cancel = cancel.clone();
        let progress_tx = progress_tx.clone();
        let total = total_chunks;

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if cancel.is_cancelled() {
                return Err(anyhow::anyhow!("cancelled"));
            }

            let node_id = format!("C-L0-{:03}", chunk_index);

            info!(
                "[canonical_l0] extracting {} (chunk {} of {})",
                node_id,
                idx + 1,
                total
            );

            let response = llm::call_model_unified(
                &config,
                CANONICAL_EXTRACTION_SYSTEM,
                &content,
                0.2,
                4096,
                Some(&response_format),
            )
            .await?;

            let parsed = llm::extract_json(&response.content).map_err(|e| {
                anyhow::anyhow!(
                    "canonical L0 {}: JSON parse failed: {}",
                    node_id,
                    e
                )
            })?;

            // Build node using same parser as chain_dispatch
            let mut node = build_node_from_output(
                &parsed,
                &node_id,
                &slug_owned,
                0,             // depth = 0
                Some(chunk_index), // preserve chunk_index
            )?;

            // Override self_prompt to empty (canonical has no question)
            node.self_prompt = String::new();

            Ok::<_, anyhow::Error>((node, idx))
        });

        handles.push(handle);
    }

    // 6. Collect results and save nodes
    let mut completed = 0i64;
    let mut saved_count = 0i32;
    let mut node_ids: Vec<String> = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok((node, _idx))) => {
                node_ids.push(node.id.clone());

                // Save to DB
                let conn = state.writer.lock().await;
                db::save_node(&conn, &node, None)?;
                drop(conn);

                saved_count += 1;
                completed += 1;

                // Update progress
                if let Some(ref tx) = progress_tx {
                    let _ = tx.send(BuildProgress {
                        done: completed,
                        total: total_chunks as i64,
                    });
                }
            }
            Ok(Err(e)) => {
                if e.to_string().contains("cancelled") {
                    info!("[canonical_l0] extraction task cancelled");
                } else {
                    warn!("[canonical_l0] extraction failed for a chunk: {}", e);
                }
                completed += 1;
            }
            Err(e) => {
                warn!("[canonical_l0] task join error: {}", e);
                completed += 1;
            }
        }
    }

    // 7. Update pyramid_file_hashes with canonical L0 node IDs
    if !node_ids.is_empty() {
        let node_ids_json = serde_json::to_string(&node_ids)?;
        let conn = state.writer.lock().await;
        // Use a synthetic file path for canonical L0 tracking
        db::upsert_file_hash(
            &conn,
            slug,
            "__canonical_l0__",
            "canonical",
            saved_count,
            &node_ids_json,
        )?;
    }

    info!(
        "[canonical_l0] slug '{}': created {} canonical L0 nodes",
        slug, saved_count
    );

    Ok(saved_count)
}
