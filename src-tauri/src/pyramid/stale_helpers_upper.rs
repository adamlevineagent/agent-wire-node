// pyramid/stale_helpers_upper.rs — L1+ node stale-check, connection carryforward,
// edge re-evaluation, and supersession helpers.
//
// Phase 4b: Real LLM-powered implementations replacing the Phase 3 placeholders
// in stale_engine.rs for node stale-checks, connection checks, and edge checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tracing::{error, info, warn};

use super::config_helper::{config_for_model, estimate_cost};
use super::llm::{call_model_with_usage, extract_json};
use super::naming::{clean_headline, headline_for_node};
use super::stale_engine::batch_items;
use super::types::{
    ConnectionCheckResult, ConnectionResult, NodeStaleResult, PendingMutation, StaleCheckResult,
};

#[derive(Debug, Clone)]
struct ThreadTarget {
    thread_id: String,
    canonical_node_id: String,
    depth: i32,
}

fn lookup_thread_target_by_canonical(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<ThreadTarget>> {
    Ok(conn
        .query_row(
            "SELECT thread_id, current_canonical_id, depth FROM pyramid_threads
         WHERE slug = ?1 AND current_canonical_id = ?2",
            rusqlite::params![slug, node_id],
            |row| {
                Ok(ThreadTarget {
                    thread_id: row.get(0)?,
                    canonical_node_id: row.get(1)?,
                    depth: row.get(2)?,
                })
            },
        )
        .ok())
}

fn lookup_thread_target_by_thread_id(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<ThreadTarget>> {
    Ok(conn
        .query_row(
            "SELECT thread_id, current_canonical_id, depth FROM pyramid_threads
         WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, node_id],
            |row| {
                Ok(ThreadTarget {
                    thread_id: row.get(0)?,
                    canonical_node_id: row.get(1)?,
                    depth: row.get(2)?,
                })
            },
        )
        .ok())
}

pub(crate) fn resolve_live_canonical_node_id(
    conn: &Connection,
    slug: &str,
    target_id: &str,
) -> Result<Option<String>> {
    let thread_canonical: Option<String> = conn
        .query_row(
            "SELECT current_canonical_id FROM pyramid_threads
             WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, target_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(current) = thread_canonical {
        return Ok(Some(current));
    }

    let exists = conn
        .query_row(
            "SELECT 1 FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, target_id],
            |_| Ok(()),
        )
        .is_ok();

    if !exists {
        return Ok(None);
    }

    let mut current = target_id.to_string();
    let mut visited = BTreeSet::new();

    loop {
        if !visited.insert(current.clone()) {
            warn!(slug = %slug, target_id = %target_id, current = %current, "Detected supersession cycle while resolving live canonical node");
            return Ok(Some(current));
        }

        let next_id: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM pyramid_nodes
                 WHERE slug = ?1 AND id = ?2 AND superseded_by IS NOT NULL",
                rusqlite::params![slug, current],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        let Some(next_id) = next_id else {
            return Ok(Some(current));
        };

        if next_id == current {
            return Ok(Some(current));
        }

        current = next_id;
    }
}

fn summarize_for_thread_name(text: &str, max_chars: usize) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    let summary: String = cleaned.chars().take(max_chars).collect();
    if summary.is_empty() {
        "Untitled".to_string()
    } else if cleaned.chars().count() > max_chars {
        format!("{summary}...")
    } else {
        summary
    }
}

fn excerpt(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn can_self_heal_thread(node_id: &str, depth: i32) -> bool {
    if depth == 0 {
        node_id.contains("-L0-") || node_id.starts_with("L0-")
    } else if depth == 1 {
        node_id.starts_with("C-L1-") || node_id.starts_with("L1-")
    } else if depth >= 2 {
        node_id.starts_with(&format!("L{depth}-"))
    } else {
        false
    }
}

fn ensure_thread_target(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<ThreadTarget>> {
    if let Some(thread_target) = lookup_thread_target_by_canonical(conn, slug, node_id)? {
        return Ok(Some(thread_target));
    }

    if let Some(thread_target) = lookup_thread_target_by_thread_id(conn, slug, node_id)? {
        return Ok(Some(thread_target));
    }

    let mut cursor = node_id.to_string();
    for _ in 0..16 {
        let next_id: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM pyramid_nodes
                 WHERE slug = ?1 AND id = ?2 AND superseded_by IS NOT NULL",
                rusqlite::params![slug, cursor],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        let Some(next_id) = next_id else {
            break;
        };

        if next_id == cursor {
            break;
        }

        if let Some(thread_target) = lookup_thread_target_by_canonical(conn, slug, &next_id)? {
            return Ok(Some(thread_target));
        }

        if let Some(thread_target) = lookup_thread_target_by_thread_id(conn, slug, &next_id)? {
            return Ok(Some(thread_target));
        }

        cursor = next_id;
    }

    let node_row: Option<(i32, String, Option<String>, String)> = conn
        .query_row(
            "SELECT depth,
                    headline,
                    json_extract(topics, '$[0].name'),
                    distilled
             FROM pyramid_nodes
             WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok();

    let Some((depth, headline, topic_name, distilled)) = node_row else {
        return Ok(None);
    };

    if !can_self_heal_thread(node_id, depth) {
        return Ok(None);
    }

    let thread_name = clean_headline(&headline)
        .or_else(|| topic_name.filter(|name| !name.trim().is_empty()))
        .unwrap_or_else(|| summarize_for_thread_name(&distilled, 60));

    conn.execute(
        "INSERT OR IGNORE INTO pyramid_threads
         (slug, thread_id, thread_name, current_canonical_id, depth, delta_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, datetime('now'), datetime('now'))",
        rusqlite::params![slug, node_id, thread_name, node_id, depth],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO pyramid_distillations
         (slug, thread_id, content, delta_count, updated_at)
         VALUES (?1, ?2, '', 0, datetime('now'))",
        rusqlite::params![slug, node_id],
    )?;

    Ok(Some(ThreadTarget {
        thread_id: node_id.to_string(),
        canonical_node_id: node_id.to_string(),
        depth,
    }))
}

fn lookup_source_file_path_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<String>> {
    let direct_path: Option<String> = conn
        .query_row(
            "SELECT file_path FROM pyramid_file_hashes pfh
             WHERE pfh.slug = ?1
               AND EXISTS (
                   SELECT 1 FROM json_each(pfh.node_ids)
                   WHERE value = ?2
               )
             LIMIT 1",
            rusqlite::params![slug, node_id],
            |row| row.get(0),
        )
        .ok();

    if direct_path.is_some() {
        return Ok(direct_path);
    }

    let mut stmt = conn.prepare(
        "SELECT file_path, node_ids FROM pyramid_file_hashes
         WHERE slug = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (file_path, node_ids_json) = row?;
        let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
        for tracked_id in node_ids {
            if resolve_live_canonical_node_id(conn, slug, &tracked_id)?.as_deref() == Some(node_id)
            {
                return Ok(Some(file_path));
            }
        }
    }

    Ok(None)
}

fn rewrite_file_hash_node_reference(
    conn: &Connection,
    slug: &str,
    file_path: &str,
    old_node_id: &str,
    new_node_id: &str,
) -> Result<()> {
    let node_ids_json: Option<String> = conn
        .query_row(
            "SELECT node_ids FROM pyramid_file_hashes
             WHERE slug = ?1 AND file_path = ?2",
            rusqlite::params![slug, file_path],
            |row| row.get(0),
        )
        .ok();

    let Some(node_ids_json) = node_ids_json else {
        return Ok(());
    };

    let mut changed = false;
    let mut seen = BTreeSet::new();
    let mut updated_node_ids = Vec::new();
    for node_id in serde_json::from_str::<Vec<String>>(&node_ids_json).unwrap_or_default() {
        let replacement = if node_id == old_node_id {
            changed = true;
            new_node_id.to_string()
        } else {
            node_id
        };

        if seen.insert(replacement.clone()) {
            updated_node_ids.push(replacement);
        }
    }

    if !changed {
        return Ok(());
    }

    conn.execute(
        "UPDATE pyramid_file_hashes
         SET node_ids = ?1, last_ingested_at = datetime('now')
         WHERE slug = ?2 AND file_path = ?3",
        rusqlite::params![
            serde_json::to_string(&updated_node_ids).unwrap_or_else(|_| "[]".to_string()),
            slug,
            file_path,
        ],
    )?;

    Ok(())
}

pub(crate) fn resolve_stale_target_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<String>> {
    Ok(ensure_thread_target(conn, slug, node_id)?.map(|target| target.thread_id))
}

pub(crate) fn resolve_parent_targets_for_node_ids(
    conn: &Connection,
    slug: &str,
    node_ids: &[String],
) -> Result<Vec<String>> {
    let mut targets = BTreeSet::new();

    for node_id in node_ids {
        let parent_id: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM pyramid_nodes
                 WHERE slug = ?1 AND id = ?2 AND parent_id IS NOT NULL",
                rusqlite::params![slug, node_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        let Some(parent_id) = parent_id else {
            continue;
        };

        if let Some(target) = resolve_stale_target_for_node(conn, slug, &parent_id)? {
            targets.insert(target);
        } else {
            warn!(slug = %slug, node_id = %node_id, parent_id = %parent_id, "Parent node does not map to a live thread target");
        }
    }

    Ok(targets.into_iter().collect())
}

/// Resolve propagation targets for question pyramids by following the evidence DAG.
///
/// For each node_id, finds all answer nodes that KEEP it as evidence (across all slugs),
/// then resolves those answer nodes to their thread targets for staleness propagation.
pub(crate) fn resolve_evidence_targets_for_node_ids(
    conn: &Connection,
    slug: &str,
    node_ids: &[String],
) -> Result<Vec<String>> {
    let mut targets = BTreeSet::new();

    for node_id in node_ids {
        let evidence_links = super::db::get_evidence_for_source_cross(conn, node_id)?;

        for link in evidence_links {
            if link.verdict != super::types::EvidenceVerdict::Keep {
                continue;
            }

            if let Some(target) = resolve_stale_target_for_node(conn, &link.slug, &link.target_node_id)? {
                targets.insert(target);
            } else {
                warn!(
                    slug = %slug,
                    node_id = %node_id,
                    evidence_target = %link.target_node_id,
                    evidence_slug = %link.slug,
                    "Evidence KEEP target does not map to a live thread target"
                );
            }
        }
    }

    Ok(targets.into_iter().collect())
}

pub(crate) fn resolve_parent_targets_for_file(
    conn: &Connection,
    slug: &str,
    file_path: &str,
) -> Result<Vec<String>> {
    let node_ids_json: Option<String> = conn
        .query_row(
            "SELECT node_ids FROM pyramid_file_hashes
             WHERE slug = ?1 AND file_path = ?2",
            rusqlite::params![slug, file_path],
            |row| row.get(0),
        )
        .ok();

    let Some(node_ids_json) = node_ids_json else {
        return Ok(Vec::new());
    };

    let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
    resolve_parent_targets_for_node_ids(conn, slug, &node_ids)
}

// ── 1. Node Stale-Check (Template 2) ─────────────────────────────────────────

/// Dispatch a batch of L1+ node stale-checks using Template 2.
///
/// For each node in the batch, looks up its current distillation and recent
/// deltas, then asks the LLM whether the distillation is stale.
pub async fn dispatch_node_stale_check(
    batch: Vec<PendingMutation>,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<StaleCheckResult>> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    // Gather node data from DB (blocking)
    let db = db_path.to_string();
    let node_ids: Vec<String> = batch.iter().map(|m| m.target_ref.clone()).collect();
    let slugs: Vec<String> = batch.iter().map(|m| m.slug.clone()).collect();

    #[derive(Debug, Clone)]
    struct PromptNode {
        source_index: usize,
        requested_target_id: String,
        canonical_node_id: String,
        thread_id: String,
        distilled: String,
        delta_content: String,
        depth: i32,
    }

    #[derive(Debug, Clone)]
    struct SkippedNode {
        source_index: usize,
        node_id: String,
        reason: String,
    }

    let (node_data, skipped_nodes) = tokio::task::spawn_blocking(move || -> Result<(Vec<PromptNode>, Vec<SkippedNode>)> {
        let conn = super::db::open_pyramid_connection(Path::new(&db)).context("Failed to open DB for node stale-check")?;
        let mut results = Vec::new();
        let mut skipped = Vec::new();
        let mut covered_threads = BTreeSet::new();

        for (i, node_id) in node_ids.iter().enumerate() {
            let slug = &slugs[i];

            let thread_target = ensure_thread_target(&conn, slug, node_id)?;
            let fallback_node: (String, i32) = conn
                .query_row(
                    "SELECT distilled, depth FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![node_id, slug],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or_else(|_| (String::new(), 0));

            let Some(thread_target) = thread_target else {
                skipped.push(SkippedNode {
                    source_index: i,
                    node_id: node_id.clone(),
                    reason: "Skipped stale check: target does not map to a live thread in this pyramid.".to_string(),
                });
                continue;
            };

            if !covered_threads.insert(thread_target.thread_id.clone()) {
                skipped.push(SkippedNode {
                    source_index: i,
                    node_id: node_id.clone(),
                    reason: format!(
                        "Skipped duplicate stale check: target resolves to live thread {} already covered in this batch.",
                        thread_target.thread_id
                    ),
                });
                continue;
            }

            let (distilled, depth): (String, i32) = conn
                .query_row(
                    "SELECT distilled, depth FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![thread_target.canonical_node_id, slug],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or((fallback_node.0, thread_target.depth));

            let mut delta_content = String::new();
            let mut stmt = conn
                .prepare(
                    "SELECT content FROM pyramid_deltas
                     WHERE slug = ?1 AND thread_id = ?2
                     ORDER BY sequence DESC LIMIT 10",
                )
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params![slug, thread_target.thread_id], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap();
            for row in rows {
                if let Ok(content) = row {
                    if !delta_content.is_empty() {
                        delta_content.push_str("\n\n");
                    }
                    delta_content.push_str(&content);
                }
            }

            results.push(PromptNode {
                source_index: i,
                requested_target_id: node_id.clone(),
                canonical_node_id: thread_target.canonical_node_id.clone(),
                thread_id: thread_target.thread_id.clone(),
                distilled,
                delta_content,
                depth,
            });
        }

        Ok((results, skipped))
    })
    .await??;

    if node_data.is_empty() {
        let results: Vec<StaleCheckResult> = skipped_nodes
            .into_iter()
            .map(|skipped| {
                let m = &batch[skipped.source_index];
                StaleCheckResult {
                    id: 0,
                    slug: m.slug.clone(),
                    batch_id: m.batch_id.clone().unwrap_or_default(),
                    layer: m.layer,
                    target_id: skipped.node_id,
                    stale: 5, // skipped — node didn't map to a live thread
                    reason: skipped.reason,
                    checker_index: skipped.source_index as i32,
                    checker_batch_size: batch_size,
                    checked_at: now.clone(),
                    cost_tokens: None,
                    cost_usd: None,
                    cascade_depth: m.cascade_depth,
                }
            })
            .collect();

        return Ok(results);
    }

    // Build Template 2 prompt
    let system_prompt =
        "You are evaluating whether changes to lower-level knowledge nodes require \
        updating higher-level distillations. Output JSON only.";

    let mut user_prompt = String::from(
        "You are evaluating whether changes to lower-level knowledge nodes require \
        updating higher-level distillations. For each node below, you see the \
        CURRENT distillation and the new delta(s) that have landed since it was written.\n\n\
        \"stale: true\" means: the delta(s) represent information that meaningfully \
        changes what this distillation says. The summary is now incomplete, inaccurate, \
        or misleading without incorporating these changes.\n\n\
        \"stale: false\" means: the delta(s) are minor refinements that don't change \
        the thrust of the distillation. It's still accurate enough.\n\n\
        When in doubt, choose true.\n\n---\n\n",
    );

    for (i, node) in node_data.iter().enumerate() {
        user_prompt.push_str(&format!(
            "NODE {} of {}:\nCanonical node ID: {}\nThread ID: {}\nLayer: L{}\n\nCurrent distillation:\n{}\n\nDelta(s):\n{}\n\n---\n\n",
            i + 1,
            node_data.len(),
            node.canonical_node_id,
            node.thread_id,
            node.depth,
            node.distilled,
            if node.delta_content.is_empty() {
                "(no deltas found)"
            } else {
                &node.delta_content
            }
        ));
    }

    user_prompt.push_str(
        "Output JSON only. Array of objects, one per node:\n\n\
        [{\"node_id\": \"...\", \"stale\": true, \"reason\": \"one sentence\"}]",
    );

    // Call LLM
    let config = config_for_model(api_key, model);
    let (response, usage) =
        call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 2048).await?;

    // Log cost to pyramid_cost_log
    {
        let db_cost = db_path.to_string();
        let slug_cost = batch[0].slug.clone();
        let model_cost = model.to_string();
        let pt = usage.prompt_tokens;
        let ct = usage.completion_tokens;
        let cost = estimate_cost(&usage);
        let lyr = batch[0].layer;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                    rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, lyr, "node_stale", now],
                );
            }
        }).await;
    }

    // Parse response
    let json = extract_json(&response)?;
    let node_results: Vec<NodeStaleResult> = serde_json::from_value(json)
        .context("Failed to parse NodeStaleResult array from LLM response")?;

    // Convert to StaleCheckResult
    let mut results: Vec<StaleCheckResult> = node_results
        .iter()
        .enumerate()
        .map(|(i, nr)| {
            let matched_node = node_data.iter().find(|node| {
                node.canonical_node_id == nr.node_id
                    || node.thread_id == nr.node_id
                    || node.requested_target_id == nr.node_id
            });
            let source_index = matched_node
                .map(|node| node.source_index)
                .unwrap_or(i.min(batch.len().saturating_sub(1)));
            let target_id = matched_node
                .map(|node| node.canonical_node_id.clone())
                .or_else(|| node_data.get(i).map(|node| node.canonical_node_id.clone()))
                .unwrap_or_else(|| nr.node_id.clone());
            let m = &batch[source_index];
            StaleCheckResult {
                id: 0,
                slug: m.slug.clone(),
                batch_id: m.batch_id.clone().unwrap_or_default(),
                layer: m.layer,
                target_id,
                stale: if nr.stale { 1 } else { 0 },
                reason: nr.reason.clone(),
                checker_index: i as i32,
                checker_batch_size: batch_size,
                checked_at: now.clone(),
                cost_tokens: Some(usage.prompt_tokens + usage.completion_tokens),
                cost_usd: Some(estimate_cost(&usage)),
                cascade_depth: m.cascade_depth,
            }
        })
        .collect();

    results.extend(skipped_nodes.into_iter().map(|skipped| {
        let m = &batch[skipped.source_index];
        StaleCheckResult {
            id: 0,
            slug: m.slug.clone(),
            batch_id: m.batch_id.clone().unwrap_or_default(),
            layer: m.layer,
            target_id: skipped.node_id,
            stale: 5, // skipped — node didn't map to a live thread
            reason: skipped.reason,
            checker_index: skipped.source_index as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: None,
            cost_usd: None,
            cascade_depth: m.cascade_depth,
        }
    }));

    info!(
        count = results.len(),
        stale_count = results.iter().filter(|r| r.stale == 1).count(),
        "dispatch_node_stale_check completed"
    );

    Ok(results)
}

// ── 2. Connection Check (Template 3) ─────────────────────────────────────────

/// Dispatch connection checks for a superseded node.
///
/// Checks whether annotations and FAQ entries attached to the old node
/// should be carried forward to the new node, using Template 3.
pub async fn dispatch_connection_check(
    node_id: &str,
    new_node_id: &str,
    db_path: &str,
    slug: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<ConnectionCheckResult>> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Gather data from DB
    let db = db_path.to_string();
    let nid = node_id.to_string();
    let new_nid = new_node_id.to_string();
    let s = slug.to_string();

    #[derive(Debug, Clone)]
    struct ConnectionItem {
        connection_type: String, // "annotation" or "faq"
        connection_id: String,
        content: String,
    }

    let (old_content, new_content, connections) =
        tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<ConnectionItem>)> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for connection check")?;

            // Get old and new node content
            let old_content: String = conn
                .query_row(
                    "SELECT distilled FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![nid, s],
                    |row| row.get(0),
                )
                .unwrap_or_default();

            let new_content: String = conn
                .query_row(
                    "SELECT distilled FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![new_nid, s],
                    |row| row.get(0),
                )
                .unwrap_or_default();

            let mut items: Vec<ConnectionItem> = Vec::new();

            // Get annotations on the old node
            {
                let mut stmt = conn.prepare(
                    "SELECT id, annotation_type, content FROM pyramid_annotations
                 WHERE node_id = ?1 AND slug = ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![nid, s], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, ann_type, content) = row?;
                    items.push(ConnectionItem {
                        connection_type: "annotation".to_string(),
                        connection_id: id.to_string(),
                        content: format!("{}: {}", ann_type, content),
                    });
                }
            }

            // Get FAQ entries that reference the old node
            {
                let mut stmt = conn.prepare(
                    "SELECT id, question, answer, match_triggers FROM pyramid_faq_nodes
                 WHERE slug = ?1",
                )?;
                let rows = stmt.query_map(rusqlite::params![s], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                for row in rows {
                    let (faq_id, question, answer, triggers_json) = row?;

                    // Check if this FAQ references our old node
                    let related: String = conn
                        .query_row(
                            "SELECT related_node_ids FROM pyramid_faq_nodes WHERE id = ?1",
                            rusqlite::params![faq_id],
                            |row| row.get(0),
                        )
                        .unwrap_or_else(|_| "[]".to_string());

                    let related_ids: Vec<String> =
                        serde_json::from_str(&related).unwrap_or_default();

                    if related_ids.contains(&nid) {
                        let triggers: Vec<String> =
                            serde_json::from_str(&triggers_json).unwrap_or_default();
                        let triggers_str = triggers.join(", ");
                        let answer_truncated: String = answer.chars().take(200).collect();
                        items.push(ConnectionItem {
                            connection_type: "faq".to_string(),
                            connection_id: faq_id,
                            content: format!(
                                "FAQ — {}: Q: {} / Triggers: {} / A: {}",
                                items
                                    .last()
                                    .map(|i| &i.connection_id)
                                    .unwrap_or(&String::new()),
                                question,
                                triggers_str,
                                answer_truncated
                            ),
                        });
                        // Fix: use the actual faq_id in the content
                        if let Some(last) = items.last_mut() {
                            last.content = format!(
                                "Q: {} / Triggers: {} / A: {}",
                                question, triggers_str, answer_truncated
                            );
                        }
                    }
                }
            }

            Ok((old_content, new_content, items))
        })
        .await??;

    if connections.is_empty() {
        info!(
            node_id = node_id,
            new_node_id = new_node_id,
            "No connections to check for superseded node"
        );
        return Ok(Vec::new());
    }

    // Batch connections at cap 20
    let connection_batches = batch_items(connections, 20);
    let mut all_results: Vec<ConnectionCheckResult> = Vec::new();

    let old_depth: i32 = {
        let db = db_path.to_string();
        let nid = node_id.to_string();
        let s = slug.to_string();
        tokio::task::spawn_blocking(move || -> i32 {
            super::db::open_pyramid_connection(Path::new(&db))
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT depth FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                        rusqlite::params![nid, s],
                        |row| row.get(0),
                    )
                    .ok()
                })
                .unwrap_or(0)
        })
        .await?
    };

    for batch in connection_batches {
        let batch_len = batch.len();

        // Build Template 3 prompt
        let system_prompt = "You are checking whether annotations and FAQ entries connected to \
            a superseded node are still valid. Output JSON only.";

        let mut user_prompt = format!(
            "A node has been superseded (updated). You are checking whether annotations \
            and FAQ entries connected to it are still valid given the change.\n\n\
            SUPERSEDED NODE:\n\
            Layer: L{}\n\
            Old content: {}\n\
            New content: {}\n\n\
            For EACH connection below, determine: is this still accurate relative to \
            the NEW content?\n\n\
            \"still_valid: true\" means: this connection is still accurate for the new \
            version. It should be carried forward to the new node.\n\n\
            \"still_valid: false\" means: this connection refers to something that has \
            changed or no longer exists in the new version. It should stay attached \
            to the old (superseded) node as historical record.\n\n---\n\n",
            old_depth, old_content, new_content
        );

        for (i, item) in batch.iter().enumerate() {
            user_prompt.push_str(&format!(
                "CONNECTION {} of {}: {} — {}\nContent: {}\n\n---\n\n",
                i + 1,
                batch_len,
                item.connection_type,
                item.connection_id,
                item.content
            ));
        }

        user_prompt.push_str(
            "Output JSON only. Array of objects, one per connection:\n\n\
            [{\"connection_id\": \"...\", \"still_valid\": true, \"reason\": \"one sentence\"}]",
        );

        // Call LLM
        let config = config_for_model(api_key, model);
        let (response, conn_usage) =
            call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 2048).await?;

        // Log cost to pyramid_cost_log
        {
            let db_cost = db_path.to_string();
            let slug_cost = slug.to_string();
            let model_cost = model.to_string();
            let pt = conn_usage.prompt_tokens;
            let ct = conn_usage.completion_tokens;
            let cost = estimate_cost(&conn_usage);
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                        rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, old_depth, "connection_check", now],
                    );
                }
            }).await;
        }

        let json = extract_json(&response)?;
        let conn_results: Vec<ConnectionResult> = serde_json::from_value(json)
            .context("Failed to parse ConnectionResult array from LLM response")?;

        // Post-processing: update annotations and FAQs
        let db = db_path.to_string();
        let new_nid = new_node_id.to_string();
        let old_nid = node_id.to_string();
        let s = slug.to_string();
        let results_for_db = conn_results.clone();
        let batch_for_db: Vec<(String, String)> = batch
            .iter()
            .map(|item| (item.connection_type.clone(), item.connection_id.clone()))
            .collect();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for connection post-processing")?;

            for cr in &results_for_db {
                // Find the connection type for this result
                let conn_info = batch_for_db.iter().find(|(_, id)| id == &cr.connection_id);

                let conn_type = conn_info.map(|(t, _)| t.as_str()).unwrap_or("unknown");

                if conn_type == "annotation" {
                    if cr.still_valid {
                        // Carry forward: update annotation node_id to new node
                        conn.execute(
                            "UPDATE pyramid_annotations SET node_id = ?1
                             WHERE id = ?2 AND slug = ?3",
                            rusqlite::params![
                                new_nid,
                                cr.connection_id.parse::<i64>().unwrap_or(0),
                                s
                            ],
                        )?;
                    }
                    // still_valid: false → annotation stays on old node (no action)
                } else if conn_type == "faq" {
                    let related: String = conn
                        .query_row(
                            "SELECT related_node_ids FROM pyramid_faq_nodes WHERE id = ?1",
                            rusqlite::params![cr.connection_id],
                            |row| row.get(0),
                        )
                        .unwrap_or_else(|_| "[]".to_string());

                    let mut related_ids: Vec<String> =
                        serde_json::from_str(&related).unwrap_or_default();

                    if cr.still_valid {
                        // Replace old node_id with new
                        for id in related_ids.iter_mut() {
                            if id == &old_nid {
                                *id = new_nid.clone();
                            }
                        }
                    } else {
                        // Remove old node_id
                        related_ids.retain(|id| id != &old_nid);
                    }

                    let updated_json =
                        serde_json::to_string(&related_ids).unwrap_or_else(|_| "[]".to_string());
                    conn.execute(
                        "UPDATE pyramid_faq_nodes SET related_node_ids = ?1, updated_at = ?2
                         WHERE id = ?3",
                        rusqlite::params![
                            updated_json,
                            Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                            cr.connection_id
                        ],
                    )?;
                }

                // Log to pyramid_connection_check_log
                conn.execute(
                    "INSERT INTO pyramid_connection_check_log
                     (slug, supersession_node_id, new_node_id, connection_type, connection_id,
                      still_valid, reason, checked_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        s,
                        old_nid,
                        new_nid,
                        conn_type,
                        cr.connection_id,
                        cr.still_valid as i32,
                        cr.reason,
                        Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    ],
                )?;
            }

            Ok(())
        })
        .await??;

        // Build ConnectionCheckResult entries
        for cr in conn_results {
            let conn_type = batch
                .iter()
                .find(|item| item.connection_id == cr.connection_id)
                .map(|item| item.connection_type.clone())
                .unwrap_or_else(|| "unknown".to_string());

            all_results.push(ConnectionCheckResult {
                id: 0,
                slug: slug.to_string(),
                supersession_node_id: node_id.to_string(),
                new_node_id: new_node_id.to_string(),
                connection_type: conn_type,
                connection_id: cr.connection_id,
                still_valid: cr.still_valid,
                reason: cr.reason,
                checked_at: now.clone(),
            });
        }
    }

    info!(
        node_id = node_id,
        new_node_id = new_node_id,
        total = all_results.len(),
        carried = all_results.iter().filter(|r| r.still_valid).count(),
        "dispatch_connection_check completed"
    );

    Ok(all_results)
}

// ── 3. Edge Stale-Check ──────────────────────────────────────────────────────

/// Dispatch a batch of edge stale-checks.
///
/// For each edge mutation, checks whether the edge relationship is still
/// accurate after a node supersession. If stale, re-evaluates the relationship
/// and writes a cross-thread propagation mutation.
pub async fn dispatch_edge_stale_check(
    batch: Vec<PendingMutation>,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<StaleCheckResult>> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;
    let mut results: Vec<StaleCheckResult> = Vec::new();

    for (idx, mutation) in batch.iter().enumerate() {
        let edge_id_str = &mutation.target_ref;
        let edge_id: i64 = edge_id_str.parse().unwrap_or(0);

        // Get edge data and node content
        let db = db_path.to_string();
        let s = mutation.slug.clone();
        let eid = edge_id;
        let detail = mutation.detail.clone().unwrap_or_default();

        #[derive(Debug, Clone)]
        struct EdgeData {
            thread_a_id: String,
            thread_b_id: String,
            relationship: String,
            old_content: String,
            new_content: String,
            other_thread_id: String,
        }

        let edge_data = tokio::task::spawn_blocking(move || -> Result<Option<EdgeData>> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for edge stale-check")?;

            let edge = conn.query_row(
                "SELECT thread_a_id, thread_b_id, relationship FROM pyramid_web_edges
                 WHERE id = ?1 AND slug = ?2",
                rusqlite::params![eid, s],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            );

            let (thread_a_id, thread_b_id, relationship) = match edge {
                Ok(e) => e,
                Err(_) => {
                    warn!(edge_id = eid, "Edge not found for stale-check");
                    return Ok(None);
                }
            };

            // The detail field contains the superseded node_id
            // Determine which thread was affected
            let superseded_node_id = &detail;

            // Get thread for the superseded node to determine which side
            let affected_thread: Option<String> = conn
                .query_row(
                    "SELECT thread_id FROM pyramid_threads
                     WHERE slug = ?1 AND current_canonical_id = ?2",
                    rusqlite::params![s, superseded_node_id],
                    |row| row.get(0),
                )
                .ok();

            // If we can't find the thread by canonical, try the superseded_by lookup
            let affected_thread = affected_thread.or_else(|| {
                conn.query_row(
                    "SELECT t.thread_id FROM pyramid_threads t
                     JOIN pyramid_nodes n ON t.slug = n.slug AND t.thread_id = (
                         SELECT thread_id FROM pyramid_threads
                         WHERE slug = ?1 AND current_canonical_id = (
                             SELECT superseded_by FROM pyramid_nodes
                             WHERE id = ?2 AND slug = ?1
                         )
                     )
                     WHERE t.slug = ?1",
                    rusqlite::params![s, superseded_node_id],
                    |row| row.get(0),
                )
                .ok()
            });

            let other_thread_id = if affected_thread.as_deref() == Some(&thread_a_id) {
                thread_b_id.clone()
            } else {
                thread_a_id.clone()
            };

            // Get old and new node content
            let old_content: String = conn
                .query_row(
                    "SELECT distilled FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![superseded_node_id, s],
                    |row| row.get(0),
                )
                .unwrap_or_default();

            let new_content = if let Ok(new_id) = conn.query_row(
                "SELECT superseded_by FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                rusqlite::params![superseded_node_id, s],
                |row| row.get::<_, Option<String>>(0),
            ) {
                if let Some(ref nid) = new_id {
                    conn.query_row(
                        "SELECT distilled FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                        rusqlite::params![nid, s],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap_or_default()
                } else {
                    old_content.clone()
                }
            } else {
                old_content.clone()
            };

            Ok(Some(EdgeData {
                thread_a_id,
                thread_b_id,
                relationship,
                old_content,
                new_content,
                other_thread_id,
            }))
        })
        .await??;

        let edge_data = match edge_data {
            Some(d) => d,
            None => {
                results.push(StaleCheckResult {
                    id: 0,
                    slug: mutation.slug.clone(),
                    batch_id: mutation.batch_id.clone().unwrap_or_default(),
                    layer: mutation.layer,
                    target_id: edge_id_str.clone(),
                    stale: 0,
                    reason: "Edge not found".to_string(),
                    checker_index: idx as i32,
                    checker_batch_size: batch_size,
                    checked_at: now.clone(),
                    cost_tokens: None,
                    cost_usd: None,
                    cascade_depth: mutation.cascade_depth,
                });
                continue;
            }
        };

        // Build edge stale-check prompt
        let system_prompt = "You are evaluating whether an edge relationship between two threads \
            is still accurate after a node was superseded. Output JSON only.";

        let user_prompt = format!(
            "This edge connects thread {} to thread {} with relationship: \"{}\". \
            Node was superseded. Old content: \"{}\". New content: \"{}\". \
            Is this edge relationship still accurate?\n\n\
            Output JSON only:\n\n\
            {{\"stale\": true, \"reason\": \"one sentence\"}}",
            edge_data.thread_a_id,
            edge_data.thread_b_id,
            edge_data.relationship,
            truncate_str(&edge_data.old_content, 500),
            truncate_str(&edge_data.new_content, 500),
        );

        let config = config_for_model(api_key, model);
        let (response, usage) =
            call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 1024).await?;

        // Log cost to pyramid_cost_log
        {
            let db_cost = db_path.to_string();
            let slug_cost = mutation.slug.clone();
            let model_cost = model.to_string();
            let pt = usage.prompt_tokens;
            let ct = usage.completion_tokens;
            let cost = estimate_cost(&usage);
            let lyr = mutation.layer;
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                        rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, lyr, "edge_stale", now],
                    );
                }
            }).await;
        }

        let json = extract_json(&response)?;
        let is_stale = json.get("stale").and_then(|v| v.as_bool()).unwrap_or(false);
        let reason = json
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("no reason given")
            .to_string();

        if is_stale {
            // Re-evaluate: ask LLM for updated relationship description
            let re_eval_prompt = format!(
                "The relationship between thread {} and thread {} was: \"{}\"\n\n\
                The content has changed. Old: \"{}\"\nNew: \"{}\"\n\n\
                Write a brief updated relationship description (one sentence). \
                Output JSON only:\n\n\
                {{\"relationship\": \"updated description\"}}",
                edge_data.thread_a_id,
                edge_data.thread_b_id,
                edge_data.relationship,
                truncate_str(&edge_data.old_content, 300),
                truncate_str(&edge_data.new_content, 300),
            );

            let (re_eval_response, _) =
                call_model_with_usage(&config, system_prompt, &re_eval_prompt, 0.3, 512).await?;

            let re_eval_json = extract_json(&re_eval_response)?;
            let new_relationship = re_eval_json
                .get("relationship")
                .and_then(|v| v.as_str())
                .unwrap_or(&edge_data.relationship)
                .to_string();

            // Update the edge in DB and write cross-thread propagation mutation
            let db = db_path.to_string();
            let s = mutation.slug.clone();
            let eid = edge_id;
            let new_rel = new_relationship.clone();
            let other_thread = edge_data.other_thread_id.clone();
            let cascade_depth = mutation.cascade_depth;
            let layer = mutation.layer;

            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = super::db::open_pyramid_connection(Path::new(&db)).context("Failed to open DB for edge update")?;
                let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

                // Update edge relationship text and reset delta_count
                conn.execute(
                    "UPDATE pyramid_web_edges SET relationship = ?1, delta_count = 0, updated_at = ?2
                     WHERE id = ?3 AND slug = ?4",
                    rusqlite::params![new_rel, now_str, eid, s],
                )?;

                // Write confirmed_stale mutation for the OTHER side node (cross-thread propagation)
                // Find the canonical node of the other thread
                let other_node_id: Option<String> = conn
                    .query_row(
                        "SELECT current_canonical_id FROM pyramid_threads
                         WHERE slug = ?1 AND thread_id = ?2",
                        rusqlite::params![s, other_thread],
                        |row| row.get(0),
                    )
                    .ok();

                if let Some(ref onid) = other_node_id {
                    conn.execute(
                        "INSERT INTO pyramid_pending_mutations
                         (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                         VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, ?5, ?6, 0)",
                        rusqlite::params![
                            s,
                            layer,
                            onid,
                            format!("Cross-thread propagation from edge {} re-evaluation", eid),
                            cascade_depth + 1,
                            now_str,
                        ],
                    )?;
                }

                Ok(())
            })
            .await??;

            info!(
                edge_id = edge_id,
                new_relationship = %new_relationship,
                "Edge re-evaluated and cross-thread propagation written"
            );
        }

        results.push(StaleCheckResult {
            id: 0,
            slug: mutation.slug.clone(),
            batch_id: mutation.batch_id.clone().unwrap_or_default(),
            layer: mutation.layer,
            target_id: edge_id_str.clone(),
            stale: if is_stale { 1 } else { 0 },
            reason,
            checker_index: idx as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: Some(usage.prompt_tokens + usage.completion_tokens),
            cost_usd: Some(estimate_cost(&usage)),
            cascade_depth: mutation.cascade_depth,
        });
    }

    info!(
        count = results.len(),
        stale_count = results.iter().filter(|r| r.stale == 1).count(),
        "dispatch_edge_stale_check completed"
    );

    Ok(results)
}

// ── 4. Execute Supersession ──────────────────────────────────────────────────

/// Execute a supersession for a confirmed-stale node.
///
/// Creates a new version of the node with updated distillation (incorporating
/// deltas via LLM), sets `superseded_by` on old node, re-parents children
/// deterministically, runs connection check, and writes propagation mutations.
///
/// Returns the new node ID.
pub async fn execute_supersession(
    node_id: &str,
    db_path: &str,
    slug: &str,
    api_key: &str,
    model: &str,
) -> Result<String> {
    let requested_node_id = node_id.to_string();
    let resolved_node_id = tokio::task::spawn_blocking({
        let db = db_path.to_string();
        let slug = slug.to_string();
        let target_id = requested_node_id.clone();
        move || -> Result<String> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB to resolve supersession target")?;
            resolve_live_canonical_node_id(&conn, &slug, &target_id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "No live canonical node found for stale target {}",
                    target_id
                )
            })
        }
    })
    .await??;

    if resolved_node_id != requested_node_id {
        info!(
            requested_target = %requested_node_id,
            resolved_target = %resolved_node_id,
            slug = %slug,
            "Resolved stale target to live canonical node before supersession"
        );
    }

    // Gather node data from DB
    let db = db_path.to_string();
    let nid = resolved_node_id.clone();
    let s = slug.to_string();

    #[derive(Debug, Clone)]
    struct NodeData {
        headline: String,
        distilled: String,
        depth: i64,
        parent_id: Option<String>,
        children: Vec<String>,
        self_thread_id: Option<String>,
        parent_thread_id: Option<String>,
        delta_content: String,
        source_file_path: Option<String>,
        source_snapshot: Option<String>,
        topics: String,
        corrections: String,
        decisions: String,
        terms: String,
        dead_ends: String,
        self_prompt: String,
    }

    let (node_data, new_node_id) = tokio::task::spawn_blocking({
        let db = db.clone();
        let nid = nid.clone();
        let s = s.clone();
        move || -> Result<(NodeData, String)> {
            let conn = super::db::open_pyramid_connection(Path::new(&db)).context("Failed to open DB for supersession")?;

            let (headline, distilled, depth, parent_id, children_json, topics, corrections, decisions, terms, dead_ends, self_prompt): (
                String, String, i64, Option<String>, String, String, String, String, String, String, String,
            ) = conn.query_row(
                "SELECT headline, distilled, depth, parent_id, children, topics, corrections, decisions, terms, dead_ends, self_prompt
                 FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                rusqlite::params![nid, s],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(7)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(8)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(9)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    ))
                },
            )?;

            let children: Vec<String> = serde_json::from_str(&children_json).unwrap_or_default();

            // Get thread_id
            let self_thread_id = resolve_stale_target_for_node(&conn, &s, &nid)?;
            let parent_thread_id = parent_id
                .as_deref()
                .map(|pid| resolve_stale_target_for_node(&conn, &s, pid))
                .transpose()?
                .flatten();

            // Gather deltas
            let mut delta_content = String::new();
            if let Some(ref tid) = self_thread_id {
                let mut stmt = conn.prepare(
                    "SELECT content FROM pyramid_deltas
                     WHERE slug = ?1 AND thread_id = ?2
                     ORDER BY sequence DESC LIMIT 10",
                )?;
                let rows = stmt.query_map(rusqlite::params![s, tid], |row| {
                    row.get::<_, String>(0)
                })?;
                for row in rows {
                    if let Ok(content) = row {
                        if !delta_content.is_empty() {
                            delta_content.push_str("\n\n");
                        }
                        delta_content.push_str(&content);
                    }
                }
            }

            let source_file_path: Option<String> = if depth == 0 {
                lookup_source_file_path_for_node(&conn, &s, &nid)?
            } else {
                None
            };

            let source_snapshot = source_file_path.as_ref().and_then(|path| {
                fs::read_to_string(path).ok().map(|content| {
                    let line_excerpt = content.lines().take(400).collect::<Vec<_>>().join("\n");
                    line_excerpt.chars().take(20_000).collect::<String>()
                })
            });

            // Generate sequential node ID (not UUID) for LLM-friendly pyramid IDs
            let new_nid = super::db::next_sequential_node_id(&conn, &s, depth, "S");

            Ok((NodeData {
                headline,
                distilled,
                depth,
                parent_id,
                children,
                self_thread_id,
                parent_thread_id,
                delta_content,
                source_file_path,
                source_snapshot,
                topics,
                corrections,
                decisions,
                terms,
                dead_ends,
                self_prompt,
            }, new_nid))
        }
    })
    .await??;

    // Generate updated headline + distillation via LLM
    let system_prompt = "You are updating a knowledge pyramid node after new information arrived. \
        Produce JSON only with a short human-friendly headline and the updated distillation.";

    let user_prompt = if node_data.depth == 0 {
        let source_label = node_data
            .source_file_path
            .as_deref()
            .unwrap_or("(unknown source file)");
        let source_snapshot = node_data
            .source_snapshot
            .clone()
            .unwrap_or_else(|| "(source snapshot unavailable)".to_string());

        format!(
            "Current headline (Layer L0):\n{}\n\n\
            Current distillation (Layer L0):\n{}\n\n\
            Current source file:\n{}\n\n\
            Current source content snapshot:\n{}\n\n\
            Rewrite the node so it accurately reflects the current file. \
            Return JSON only:\n\
            {{\"headline\":\"2-6 word file or module label\",\"distilled\":\"updated distillation\"}}\n\
            Keep the same style and level of detail as the original, but prefer the source file over the old distillation. \
            The headline must be concrete and human-friendly. No 'This file...' or 'This node...'.",
            node_data.headline,
            node_data.distilled,
            source_label,
            source_snapshot,
        )
    } else {
        format!(
            "Current headline (Layer L{}):\n{}\n\n\
            Current distillation (Layer L{}):\n{}\n\n\
            New delta(s) to incorporate:\n{}\n\n\
            Write the updated node that incorporates these changes. \
            Return JSON only:\n\
            {{\"headline\":\"2-6 word node label\",\"distilled\":\"updated distillation\"}}\n\
            Keep the same style and level of detail as the original. \
            The headline must be concrete and human-friendly. No 'This node...'.",
            node_data.depth,
            node_data.headline,
            node_data.depth,
            node_data.distilled,
            if node_data.delta_content.is_empty() {
                "(no deltas)".to_string()
            } else {
                node_data.delta_content.clone()
            }
        )
    };

    let config = config_for_model(api_key, model);
    let (supersession_response, supersession_usage) =
        call_model_with_usage(&config, system_prompt, &user_prompt, 0.2, 4096).await?;
    let supersession_json = extract_json(&supersession_response).ok();
    let new_headline = supersession_json
        .as_ref()
        .and_then(|json| json.get("headline"))
        .and_then(|value| value.as_str())
        .and_then(clean_headline)
        .unwrap_or_else(|| {
            headline_for_node(
                &super::types::PyramidNode {
                    id: nid.clone(),
                    slug: s.clone(),
                    depth: node_data.depth,
                    chunk_index: None,
                    headline: node_data.headline.clone(),
                    distilled: node_data.distilled.clone(),
                    topics: serde_json::from_str(&node_data.topics).unwrap_or_default(),
                    corrections: serde_json::from_str(&node_data.corrections).unwrap_or_default(),
                    decisions: serde_json::from_str(&node_data.decisions).unwrap_or_default(),
                    terms: serde_json::from_str(&node_data.terms).unwrap_or_default(),
                    dead_ends: serde_json::from_str(&node_data.dead_ends).unwrap_or_default(),
                    self_prompt: node_data.self_prompt.clone(),
                    children: node_data.children.clone(),
                    parent_id: node_data.parent_id.clone(),
                    superseded_by: None,
                    build_id: None,
                    created_at: String::new(),
                },
                node_data.source_file_path.as_deref(),
            )
        });
    let new_distillation = supersession_json
        .as_ref()
        .and_then(|json| json.get("distilled"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| supersession_response.trim().to_string());

    // Log cost to pyramid_cost_log
    {
        let db_cost = db_path.to_string();
        let slug_cost = slug.to_string();
        let model_cost = model.to_string();
        let pt = supersession_usage.prompt_tokens;
        let ct = supersession_usage.completion_tokens;
        let cost = estimate_cost(&supersession_usage);
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                    rusqlite::params![slug_cost, "supersession", model_cost, pt, ct, cost, 0i32, "supersession", now],
                );
            }
        }).await;
    }

    // Write new node, update old node, re-parent children, update thread
    let db = db_path.to_string();
    let s = slug.to_string();
    let nid = resolved_node_id.clone();
    let new_nid = new_node_id.clone();
    let nd = node_data.clone();
    let new_head = new_headline.clone();
    let new_dist = new_distillation.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = super::db::open_pyramid_connection(Path::new(&db)).context("Failed to open DB for supersession write")?;
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        // Insert new node
        conn.execute(
            "INSERT INTO pyramid_nodes
             (id, slug, depth, headline, distilled, topics, corrections, decisions, terms,
              dead_ends, self_prompt, children, parent_id, build_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14)",
            rusqlite::params![
                new_nid,
                s,
                nd.depth,
                new_head,
                new_dist,
                nd.topics,
                nd.corrections,
                nd.decisions,
                nd.terms,
                nd.dead_ends,
                nd.self_prompt,
                serde_json::to_string(&nd.children).unwrap_or_else(|_| "[]".to_string()),
                nd.parent_id,
                now_str,
            ],
        )?;

        // Set superseded_by on old node
        conn.execute(
            "UPDATE pyramid_nodes SET superseded_by = ?1 WHERE id = ?2 AND slug = ?3",
            rusqlite::params![new_nid, nid, s],
        )?;

        // Re-parent children: update their parent_id to new node
        for child_id in &nd.children {
            conn.execute(
                "UPDATE pyramid_nodes SET parent_id = ?1
                 WHERE id = ?2 AND slug = ?3",
                rusqlite::params![new_nid, child_id, s],
            )?;
        }

        // Update parent's children array: replace old node_id with new
        if let Some(ref pid) = nd.parent_id {
            let parent_children: String = conn
                .query_row(
                    "SELECT children FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![pid, s],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap_or(None)
                .unwrap_or_else(|| "[]".to_string());

            let mut children_arr: Vec<String> =
                serde_json::from_str(&parent_children).unwrap_or_default();
            for child in children_arr.iter_mut() {
                if child == &nid {
                    *child = new_nid.clone();
                }
            }
            let updated = serde_json::to_string(&children_arr).unwrap_or_else(|_| "[]".to_string());
            conn.execute(
                "UPDATE pyramid_nodes SET children = ?1 WHERE id = ?2 AND slug = ?3",
                rusqlite::params![updated, pid, s],
            )?;
        }

        // Update thread canonical ID
        if let Some(ref tid) = nd.self_thread_id {
            conn.execute(
                "UPDATE pyramid_threads SET current_canonical_id = ?1, thread_name = ?2, updated_at = ?3
                 WHERE slug = ?4 AND thread_id = ?5",
                rusqlite::params![new_nid, new_head, now_str, s, tid],
            )?;
        }

        if nd.depth == 0 {
            if let Some(ref file_path) = nd.source_file_path {
                rewrite_file_hash_node_reference(&conn, &s, file_path, &nid, &new_nid)?;
            }
        }

        // Record the supersession delta on all upstream threads so the next layer's
        // stale checker has content to evaluate. For question pyramids, upstream
        // relationships are through evidence KEEP links, not parent_id.
        let delta_summary = format!(
            "Child node {} superseded by {}.\n\nPrevious child distillation:\n{}\n\nUpdated child distillation:\n{}",
            nid,
            new_nid,
            excerpt(&nd.distilled, 400),
            excerpt(&new_dist, 400),
        );

        // Find all upstream threads via evidence KEEP links
        let upstream_threads: Vec<String> = {
            let evidence_targets =
                resolve_evidence_targets_for_node_ids(&conn, &s, std::slice::from_ref(&nid))?;
            evidence_targets
        };

        // Also include the mechanical parent_thread_id as fallback
        let mut all_target_threads: std::collections::BTreeSet<String> = upstream_threads.into_iter().collect();
        if let Some(ref tid) = nd.parent_thread_id {
            all_target_threads.insert(tid.clone());
        }

        for tid in &all_target_threads {
            let next_seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM pyramid_deltas
                 WHERE slug = ?1 AND thread_id = ?2",
                rusqlite::params![s, tid],
                |row| row.get(0),
            ).unwrap_or(1);

            conn.execute(
                "INSERT INTO pyramid_deltas
                 (slug, thread_id, sequence, content, relevance, source_node_id, flag, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    s,
                    tid,
                    next_seq,
                    delta_summary,
                    "high",
                    nid,
                    Option::<String>::None,
                    now_str,
                ],
            )?;

            conn.execute(
                "UPDATE pyramid_threads
                 SET delta_count = delta_count + 1, updated_at = ?1
                 WHERE slug = ?2 AND thread_id = ?3",
                rusqlite::params![now_str, s, tid],
            )?;
        }

        // Write confirmed_stale mutations for: parent layer (layer+1) and all edges
        let max_depth: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(depth), 3) FROM pyramid_nodes WHERE slug = ?1",
                rusqlite::params![s],
                |row| row.get(0),
            )
            .unwrap_or(3);
        let next_layer = (nd.depth as i32 + 1).min(max_depth);

        // All pyramids now use the question chain — always propagate via evidence DAG.
        let propagation_targets =
            resolve_evidence_targets_for_node_ids(&conn, &s, std::slice::from_ref(&nid))?;
        for target in propagation_targets {
            conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                 VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, 0, ?5, 0)",
                rusqlite::params![
                    s,
                    next_layer,
                    target,
                    format!("Child node {} superseded by {}", nid, new_nid),
                    now_str,
                ],
            )?;
        }

        // Write edge_stale mutations for all edges touching this node's thread
        if let Some(ref tid) = nd.self_thread_id {
            let mut stmt = conn.prepare(
                "SELECT id FROM pyramid_web_edges
                 WHERE slug = ?1 AND (thread_a_id = ?2 OR thread_b_id = ?2)",
            )?;
            let edge_ids: Vec<i64> = stmt
                .query_map(rusqlite::params![s, tid], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            for eid in edge_ids {
                conn.execute(
                    "INSERT INTO pyramid_pending_mutations
                     (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                     VALUES (?1, ?2, 'edge_stale', ?3, ?4, 0, ?5, 0)",
                    rusqlite::params![
                        s,
                        nd.depth as i32,
                        eid.to_string(),
                        nid,
                        now_str,
                    ],
                )?;
            }
        }

        Ok(())
    })
    .await??;

    // Run connection check on the superseded node
    let conn_results =
        dispatch_connection_check(node_id, &new_node_id, db_path, slug, api_key, model).await;

    match conn_results {
        Ok(results) => {
            info!(
                node_id = node_id,
                new_node_id = %new_node_id,
                connections = results.len(),
                "Supersession complete with connection check"
            );
        }
        Err(e) => {
            error!(
                node_id = node_id,
                new_node_id = %new_node_id,
                error = %e,
                "Connection check failed during supersession (node still superseded)"
            );
        }
    }

    Ok(new_node_id)
}

#[cfg(test)]
mod tests {
    use super::{
        lookup_source_file_path_for_node, resolve_live_canonical_node_id,
        rewrite_file_hash_node_reference,
    };
    use crate::pyramid::db::open_pyramid_db;
    use rusqlite::{params, Connection};
    use tempfile::NamedTempFile;

    fn setup_test_db() -> (NamedTempFile, Connection) {
        let file = NamedTempFile::new().expect("temp db");
        let conn = open_pyramid_db(file.path()).expect("open pyramid db");
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'document', ?2)",
            params!["test-slug", "/tmp/source"],
        )
        .expect("insert slug");
        (file, conn)
    }

    fn insert_node(conn: &Connection, node_id: &str, parent_id: Option<&str>) {
        conn.execute(
            "INSERT INTO pyramid_nodes
             (id, slug, depth, headline, distilled, children, parent_id, build_version, created_at)
             VALUES (?1, 'test-slug', 1, ?2, ?3, '[]', ?4, 1, datetime('now'))",
            params![
                node_id,
                format!("Headline {node_id}"),
                format!("Distilled {node_id}"),
                parent_id
            ],
        )
        .expect("insert node");
    }

    #[test]
    fn resolves_live_canonical_for_thread_and_historical_ids() {
        let (_file, conn) = setup_test_db();
        insert_node(&conn, "node-a", None);
        insert_node(&conn, "node-b", None);
        insert_node(&conn, "node-c", None);

        conn.execute(
            "UPDATE pyramid_nodes SET superseded_by = 'node-b'
             WHERE slug = 'test-slug' AND id = 'node-a'",
            [],
        )
        .expect("supersede node-a");
        conn.execute(
            "UPDATE pyramid_nodes SET superseded_by = 'node-c'
             WHERE slug = 'test-slug' AND id = 'node-b'",
            [],
        )
        .expect("supersede node-b");
        conn.execute(
            "INSERT INTO pyramid_threads
             (slug, thread_id, thread_name, current_canonical_id, depth, delta_count, created_at, updated_at)
             VALUES ('test-slug', 'thread-1', 'Thread 1', 'node-c', 1, 0, datetime('now'), datetime('now'))",
            [],
        )
        .expect("insert thread");

        assert_eq!(
            resolve_live_canonical_node_id(&conn, "test-slug", "thread-1").unwrap(),
            Some("node-c".to_string())
        );
        assert_eq!(
            resolve_live_canonical_node_id(&conn, "test-slug", "node-a").unwrap(),
            Some("node-c".to_string())
        );
        assert_eq!(
            resolve_live_canonical_node_id(&conn, "test-slug", "node-c").unwrap(),
            Some("node-c".to_string())
        );
        assert_eq!(
            resolve_live_canonical_node_id(&conn, "test-slug", "missing").unwrap(),
            None
        );
    }

    #[test]
    fn file_hash_lookup_and_rewrite_follow_live_node() {
        let (_file, conn) = setup_test_db();
        insert_node(&conn, "node-a", None);
        insert_node(&conn, "node-b", None);

        conn.execute(
            "UPDATE pyramid_nodes SET superseded_by = 'node-b'
             WHERE slug = 'test-slug' AND id = 'node-a'",
            [],
        )
        .expect("supersede node-a");
        conn.execute(
            "INSERT INTO pyramid_file_hashes
             (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
             VALUES ('test-slug', '/tmp/doc.md', 'hash', 1, '[\"node-a\"]', datetime('now'))",
            [],
        )
        .expect("insert file hash");

        assert_eq!(
            lookup_source_file_path_for_node(&conn, "test-slug", "node-b").unwrap(),
            Some("/tmp/doc.md".to_string())
        );

        rewrite_file_hash_node_reference(&conn, "test-slug", "/tmp/doc.md", "node-a", "node-b")
            .expect("rewrite file hash");

        let node_ids_json: String = conn
            .query_row(
                "SELECT node_ids FROM pyramid_file_hashes
                 WHERE slug = 'test-slug' AND file_path = '/tmp/doc.md'",
                [],
                |row| row.get(0),
            )
            .expect("load node ids");
        assert_eq!(node_ids_json, "[\"node-b\"]");
    }
}

// ── Utility Functions ────────────────────────────────────────────────────────

// estimate_cost moved to config_helper.rs

/// Truncate a string to a maximum character count, appending "..." if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}
