// pyramid/stale_helpers_upper.rs — L1+ node stale-check, connection carryforward,
// edge re-evaluation, and supersession helpers.
//
// Phase 4b: Real LLM-powered implementations replacing the Phase 3 placeholders
// in stale_engine.rs for node stale-checks, connection checks, and edge checks.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tracing::{info, warn, error};
use uuid::Uuid;

use super::config_helper::{config_for_model, estimate_cost};
use super::llm::{call_model_with_usage, extract_json};
use super::stale_engine::batch_items;
use super::types::{
    ConnectionCheckResult, ConnectionResult, NodeStaleResult, PendingMutation, StaleCheckResult,
};

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

    let node_data = tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, String, i32)>> {
        let conn = Connection::open(&db).context("Failed to open DB for node stale-check")?;
        let mut results = Vec::new();

        for (i, node_id) in node_ids.iter().enumerate() {
            let slug = &slugs[i];

            // Get node distillation and depth
            let (distilled, depth): (String, i32) = conn
                .query_row(
                    "SELECT distilled, depth FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                    rusqlite::params![node_id, slug],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or_else(|_| (String::new(), 0));

            // Get the thread_id for this node, then fetch recent deltas
            let thread_id: Option<String> = conn
                .query_row(
                    "SELECT thread_id FROM pyramid_threads
                     WHERE slug = ?1 AND current_canonical_id = ?2",
                    rusqlite::params![slug, node_id],
                    |row| row.get(0),
                )
                .ok();

            let mut delta_content = String::new();
            if let Some(ref tid) = thread_id {
                let mut stmt = conn
                    .prepare(
                        "SELECT content FROM pyramid_deltas
                         WHERE slug = ?1 AND thread_id = ?2
                         ORDER BY sequence DESC LIMIT 10",
                    )
                    .unwrap();
                let rows = stmt
                    .query_map(rusqlite::params![slug, tid], |row| {
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
            }

            results.push((node_id.clone(), distilled, delta_content, depth));
        }

        Ok(results)
    })
    .await??;

    // Build Template 2 prompt
    let system_prompt = "You are evaluating whether changes to lower-level knowledge nodes require \
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

    for (i, (node_id, distilled, delta_content, depth)) in node_data.iter().enumerate() {
        user_prompt.push_str(&format!(
            "NODE {} of {}: {}\nLayer: L{}\n\nCurrent distillation:\n{}\n\nDelta(s):\n{}\n\n---\n\n",
            i + 1,
            batch_size,
            node_id,
            depth,
            distilled,
            if delta_content.is_empty() {
                "(no deltas found)"
            } else {
                delta_content
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
            if let Ok(conn) = Connection::open(&db_cost) {
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
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
    let results: Vec<StaleCheckResult> = node_results
        .iter()
        .enumerate()
        .map(|(i, nr)| {
            let m = batch.iter().find(|m| m.target_ref == nr.node_id).unwrap_or(&batch[i]);
            StaleCheckResult {
                id: 0,
                slug: m.slug.clone(),
                batch_id: m.batch_id.clone().unwrap_or_default(),
                layer: m.layer,
                target_id: nr.node_id.clone(),
                stale: nr.stale,
                reason: nr.reason.clone(),
                checker_index: i as i32,
                checker_batch_size: batch_size,
                checked_at: now.clone(),
                cost_tokens: Some(usage.prompt_tokens + usage.completion_tokens),
                cost_usd: Some(estimate_cost(&usage)),
            }
        })
        .collect();

    info!(
        count = results.len(),
        stale_count = results.iter().filter(|r| r.stale).count(),
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

    let (old_content, new_content, connections) = tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<ConnectionItem>)> {
        let conn = Connection::open(&db).context("Failed to open DB for connection check")?;

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
                            items.last().map(|i| &i.connection_id).unwrap_or(&String::new()),
                            question,
                            triggers_str,
                            answer_truncated
                        ),
                    });
                    // Fix: use the actual faq_id in the content
                    if let Some(last) = items.last_mut() {
                        last.content = format!(
                            "Q: {} / Triggers: {} / A: {}",
                            question,
                            triggers_str,
                            answer_truncated
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
            Connection::open(&db)
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
                if let Ok(conn) = Connection::open(&db_cost) {
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
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
            let conn = Connection::open(&db).context("Failed to open DB for connection post-processing")?;

            for cr in &results_for_db {
                // Find the connection type for this result
                let conn_info = batch_for_db
                    .iter()
                    .find(|(_, id)| id == &cr.connection_id);

                let conn_type = conn_info.map(|(t, _)| t.as_str()).unwrap_or("unknown");

                if conn_type == "annotation" {
                    if cr.still_valid {
                        // Carry forward: update annotation node_id to new node
                        conn.execute(
                            "UPDATE pyramid_annotations SET node_id = ?1
                             WHERE id = ?2 AND slug = ?3",
                            rusqlite::params![new_nid, cr.connection_id.parse::<i64>().unwrap_or(0), s],
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

                    let updated_json = serde_json::to_string(&related_ids).unwrap_or_else(|_| "[]".to_string());
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
            let conn = Connection::open(&db).context("Failed to open DB for edge stale-check")?;

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
                    stale: false,
                    reason: "Edge not found".to_string(),
                    checker_index: idx as i32,
                    checker_batch_size: batch_size,
                    checked_at: now.clone(),
                    cost_tokens: None,
                    cost_usd: None,
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
                if let Ok(conn) = Connection::open(&db_cost) {
                    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
                        rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, lyr, "edge_stale", now],
                    );
                }
            }).await;
        }

        let json = extract_json(&response)?;
        let is_stale = json
            .get("stale")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
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
                let conn = Connection::open(&db).context("Failed to open DB for edge update")?;
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
            stale: is_stale,
            reason,
            checker_index: idx as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: Some(usage.prompt_tokens + usage.completion_tokens),
            cost_usd: Some(estimate_cost(&usage)),
        });
    }

    info!(
        count = results.len(),
        stale_count = results.iter().filter(|r| r.stale).count(),
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
    let new_node_id = format!("N-{}", Uuid::new_v4());

    // Gather node data from DB
    let db = db_path.to_string();
    let nid = node_id.to_string();
    let s = slug.to_string();

    #[derive(Debug, Clone)]
    struct NodeData {
        distilled: String,
        depth: i64,
        parent_id: Option<String>,
        children: Vec<String>,
        thread_id: Option<String>,
        delta_content: String,
        topics: String,
        corrections: String,
        decisions: String,
        terms: String,
        dead_ends: String,
        self_prompt: String,
    }

    let node_data = tokio::task::spawn_blocking({
        let db = db.clone();
        let nid = nid.clone();
        let s = s.clone();
        move || -> Result<NodeData> {
            let conn = Connection::open(&db).context("Failed to open DB for supersession")?;

            let (distilled, depth, parent_id, children_json, topics, corrections, decisions, terms, dead_ends, self_prompt): (
                String, i64, Option<String>, String, String, String, String, String, String, String,
            ) = conn.query_row(
                "SELECT distilled, depth, parent_id, children, topics, corrections, decisions, terms, dead_ends, self_prompt
                 FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
                rusqlite::params![nid, s],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(7)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(8)?.unwrap_or_else(|| "[]".to_string()),
                        row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    ))
                },
            )?;

            let children: Vec<String> = serde_json::from_str(&children_json).unwrap_or_default();

            // Get thread_id
            let thread_id: Option<String> = conn
                .query_row(
                    "SELECT thread_id FROM pyramid_threads
                     WHERE slug = ?1 AND current_canonical_id = ?2",
                    rusqlite::params![s, nid],
                    |row| row.get(0),
                )
                .ok();

            // Gather deltas
            let mut delta_content = String::new();
            if let Some(ref tid) = thread_id {
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

            Ok(NodeData {
                distilled,
                depth,
                parent_id,
                children,
                thread_id,
                delta_content,
                topics,
                corrections,
                decisions,
                terms,
                dead_ends,
                self_prompt,
            })
        }
    })
    .await??;

    // Generate updated distillation via LLM
    let system_prompt = "You are updating a knowledge pyramid node distillation to incorporate \
        new information from deltas. Produce an updated distillation that integrates the delta \
        information. Output the updated distillation text only, no JSON wrapping.";

    let user_prompt = format!(
        "Current distillation (Layer L{}):\n{}\n\n\
        New delta(s) to incorporate:\n{}\n\n\
        Write the updated distillation that incorporates these changes. \
        Keep the same style and level of detail as the original.",
        node_data.depth,
        node_data.distilled,
        if node_data.delta_content.is_empty() {
            "(no deltas)".to_string()
        } else {
            node_data.delta_content.clone()
        }
    );

    let config = config_for_model(api_key, model);
    let (new_distillation, supersession_usage) =
        call_model_with_usage(&config, system_prompt, &user_prompt, 0.2, 4096).await?;

    // Log cost to pyramid_cost_log
    {
        let db_cost = db_path.to_string();
        let slug_cost = slug.to_string();
        let model_cost = model.to_string();
        let pt = supersession_usage.prompt_tokens;
        let ct = supersession_usage.completion_tokens;
        let cost = estimate_cost(&supersession_usage);
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = Connection::open(&db_cost) {
                let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
                    rusqlite::params![slug_cost, "supersession", model_cost, pt, ct, cost, 0i32, "supersession", now],
                );
            }
        }).await;
    }

    // Write new node, update old node, re-parent children, update thread
    let db = db_path.to_string();
    let s = slug.to_string();
    let nid = node_id.to_string();
    let new_nid = new_node_id.clone();
    let nd = node_data.clone();
    let new_dist = new_distillation.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = Connection::open(&db).context("Failed to open DB for supersession write")?;
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        // Insert new node
        conn.execute(
            "INSERT INTO pyramid_nodes
             (id, slug, depth, distilled, topics, corrections, decisions, terms,
              dead_ends, self_prompt, children, parent_id, build_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)",
            rusqlite::params![
                new_nid,
                s,
                nd.depth,
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
        if let Some(ref tid) = nd.thread_id {
            conn.execute(
                "UPDATE pyramid_threads SET current_canonical_id = ?1, updated_at = ?2
                 WHERE slug = ?3 AND thread_id = ?4",
                rusqlite::params![new_nid, now_str, s, tid],
            )?;
        }

        // Write confirmed_stale mutations for: parent layer (layer+1) and all edges
        let next_layer = (nd.depth as i32 + 1).min(3);
        if let Some(ref pid) = nd.parent_id {
            conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                 VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, 0, ?5, 0)",
                rusqlite::params![
                    s,
                    next_layer,
                    pid,
                    format!("Child node {} superseded by {}", nid, new_nid),
                    now_str,
                ],
            )?;
        }

        // Write edge_stale mutations for all edges touching this node's thread
        if let Some(ref tid) = nd.thread_id {
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
    let conn_results = dispatch_connection_check(
        node_id, &new_node_id, db_path, slug, api_key, model,
    )
    .await;

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
