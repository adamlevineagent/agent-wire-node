// pyramid/vine.rs — Vine Conversation System
//
// The vine is a meta-pyramid connecting multiple conversation session pyramids temporally.
// Terminology:
//   Grape = any single node in a conversation pyramid
//   Bunch = one complete conversation pyramid (all grapes from one session)
//   Vine  = meta-pyramid where bunches connect at the top
//
// Vine L0 nodes = apex + penultimate layer from each bunch (~3 nodes per session).
// Everything is a contribution: ERAs, decisions, entities, thread continuity,
// corrections are annotations/FAQ/web edges on the vine pyramid. No parallel infrastructure.

#![allow(dead_code, unused_imports)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde_json;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::build;
use super::chain_dispatch;
use super::db;
use super::ingest;
use super::llm::{self, LlmConfig};
use super::query;
use super::types::*;
use super::PyramidState;

/// Episodic synthesis prompt — loaded at compile time from the canonical source.
const SYNTHESIZE_RECURSIVE_PROMPT: &str =
    include_str!("../../../chains/prompts/conversation-episodic/synthesize_recursive.md");

// ── Writer Drain Helper ──────────────────────────────────────────────────────

/// Spawn a writer drain task that consumes WriteOps from the channel.
/// Returns (sender, join_handle). Drop the sender to signal completion.
fn spawn_write_drain(
    writer: Arc<Mutex<Connection>>,
) -> (mpsc::Sender<build::WriteOp>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<build::WriteOp>(256);
    let handle = tokio::spawn(async move {
        while let Some(op) = rx.recv().await {
            let result = {
                let conn = writer.lock().await;
                match op {
                    build::WriteOp::SaveNode {
                        ref node,
                        ref topics_json,
                    } => db::save_node(&conn, node, topics_json.as_deref()),
                    build::WriteOp::SaveStep {
                        ref slug,
                        ref step_type,
                        chunk_index,
                        depth,
                        ref node_id,
                        ref output_json,
                        ref model,
                        elapsed,
                    } => db::save_step(
                        &conn,
                        slug,
                        step_type,
                        chunk_index,
                        depth,
                        node_id,
                        output_json,
                        model,
                        elapsed,
                    ),
                    build::WriteOp::UpdateParent {
                        ref slug,
                        ref node_id,
                        ref parent_id,
                    } => db::update_parent(&conn, slug, node_id, parent_id),
                    build::WriteOp::UpdateStats { ref slug } => db::update_slug_stats(&conn, slug),
                    build::WriteOp::UpdateFileHash { ref slug, ref file_path, ref node_id } => {
                        db::append_node_id_to_file_hash(&conn, slug, file_path, node_id)
                    }
                    build::WriteOp::Flush { done } => {
                        let _ = done.send(());
                        Ok(())
                    }
                }
            };
            if let Err(e) = result {
                error!("WriteOp failed: {e}");
            }
        }
    });
    (tx, handle)
}

// ── JSONL Parsing ────────────────────────────────────────────────────────────

/// A parsed JSONL line with metadata extracted.
#[derive(Debug, Clone)]
pub struct JsonlRecord {
    pub line_type: String, // "user", "assistant", "progress", "system", etc.
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub role: Option<String>,
    pub content_text: Option<String>,
    pub has_tool_use_result: bool,
}

/// Parse a single JSONL line into a structured record.
pub fn parse_jsonl_line(line: &str) -> Option<JsonlRecord> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;

    let line_type = obj.get("type")?.as_str()?.to_string();
    let session_id = obj
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let has_tool_use_result = obj.contains_key("toolUseResult");

    let (role, content_text) = if let Some(msg) = obj.get("message").and_then(|v| v.as_object()) {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let content = msg.get("content");
        let text = match content {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(arr)) => {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|block| {
                        let obj = block.as_object()?;
                        match obj.get("type")?.as_str()? {
                            "text" => obj
                                .get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            "tool_use" => {
                                let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                Some(format!("[Tool: {name}]"))
                            }
                            _ => None,
                        }
                    })
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n"))
                }
            }
            _ => None,
        };
        (role, text)
    } else {
        (None, None)
    };

    Some(JsonlRecord {
        line_type,
        session_id,
        timestamp,
        role,
        content_text,
        has_tool_use_result,
    })
}

// ── Bunch Discovery ──────────────────────────────────────────────────────────

/// Discover conversation JSONL files and extract metadata for vine construction.
/// Scans top-level files only (skips subdirectories containing subagent files).
/// Returns bunches sorted by first_timestamp ascending, with session_id tiebreaker.
pub fn discover_bunches(jsonl_dirs: &[PathBuf]) -> Result<Vec<BunchDiscovery>> {
    let mut bunches = Vec::new();
    let mut seen_session_ids: HashSet<String> = HashSet::new();

    for jsonl_dir in jsonl_dirs {
        let entries = std::fs::read_dir(jsonl_dir)
            .with_context(|| format!("Failed to read JSONL directory: {}", jsonl_dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            // Skip directories (subagent folders) and non-JSONL files
            if !path.is_file() || path.extension().map_or(true, |ext| ext != "jsonl") {
                continue;
            }

            match scan_jsonl_metadata(&path) {
                Ok(Some(discovery)) => {
                    if discovery.message_count < 3 {
                        info!(
                            "Skipping {} (only {} messages)",
                            path.display(),
                            discovery.message_count
                        );
                        continue;
                    }
                    // Deduplicate by session_id (same session in multiple dirs = keep first found)
                    if seen_session_ids.contains(&discovery.session_id) {
                        info!(
                            "Skipping duplicate session {} at {}",
                            discovery.session_id,
                            path.display()
                        );
                        continue;
                    }
                    seen_session_ids.insert(discovery.session_id.clone());
                    bunches.push(discovery);
                }
                Ok(None) => {
                    info!("Skipping {} (no user/assistant messages)", path.display());
                }
                Err(e) => {
                    warn!("Failed to scan {}: {e}", path.display());
                }
            }
        }
    }

    // Sort globally by first_ts ascending, session_id as tiebreaker
    bunches.sort_by(|a, b| {
        a.first_ts
            .cmp(&b.first_ts)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });

    let dir_display: Vec<String> = jsonl_dirs.iter().map(|d| d.display().to_string()).collect();
    info!(
        "Discovered {} conversation bunches across {} dirs: {:?}",
        bunches.len(),
        jsonl_dirs.len(),
        dir_display
    );
    Ok(bunches)
}

/// Scan a JSONL file to extract session metadata without full parsing.
/// Reads to last complete newline to avoid race conditions with concurrent writers.
fn scan_jsonl_metadata(path: &Path) -> Result<Option<BunchDiscovery>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    // Truncate at last complete newline to avoid partial-line race with concurrent writers
    let content = match content.rfind('\n') {
        Some(pos) => &content[..pos],
        None => &content,
    };

    let mut session_id: Option<String> = None;
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut message_count: i64 = 0;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let record = match parse_jsonl_line(line) {
            Some(r) => r,
            None => continue,
        };

        // Extract session_id from first record that has one
        if session_id.is_none() {
            if let Some(ref sid) = record.session_id {
                session_id = Some(sid.clone());
            }
        }

        // Only count user/assistant messages (skip tool results)
        if (record.line_type == "user" || record.line_type == "assistant")
            && !record.has_tool_use_result
            && record
                .content_text
                .as_ref()
                .map_or(false, |t| !t.trim().is_empty())
        {
            message_count += 1;

            if let Some(ref ts) = record.timestamp {
                if first_ts.is_none() {
                    first_ts = Some(ts.clone());
                }
                last_ts = Some(ts.clone());
            }
        }
    }

    match (session_id, first_ts, last_ts) {
        (Some(sid), Some(fts), Some(lts)) => Ok(Some(BunchDiscovery {
            session_id: sid,
            jsonl_path: path.to_path_buf(),
            first_ts: fts,
            last_ts: lts,
            message_count,
        })),
        _ => Ok(None),
    }
}

// ── Bunch Building ───────────────────────────────────────────────────────────

/// Extract metadata from a bunch's apex + penultimate layer nodes.
/// Mechanical — no LLM calls.
pub fn extract_bunch_metadata(
    apex: &PyramidNode,
    penultimate_nodes: &[PyramidNode],
    bunch_index: i64,
    bunch_ts: &str,
) -> VineBunchMetadata {
    let mut topics = Vec::new();
    let mut entities = Vec::new();
    let mut decisions = Vec::new();
    let mut corrections = Vec::new();
    let mut seen_decisions = HashSet::new();
    let mut seen_corrections = HashSet::new();

    // Process apex + penultimate nodes together (apex may have data not in penultimate)
    let all_nodes = std::iter::once(apex).chain(penultimate_nodes.iter());
    for node in all_nodes {
        // Collect from topic-level
        for topic in &node.topics {
            topics.push(topic.name.clone());
            entities.extend(topic.entities.iter().cloned());

            for d in &topic.decisions {
                if seen_decisions.insert(d.decided.clone()) {
                    decisions.push(VineDecision {
                        decision: d.clone(),
                        bunch_index,
                        bunch_ts: bunch_ts.to_string(),
                    });
                }
            }
            for c in &topic.corrections {
                let key = format!("{}→{}", c.wrong, c.right);
                if seen_corrections.insert(key) {
                    corrections.push(VineCorrection {
                        correction: c.clone(),
                        bunch_index,
                        bunch_ts: bunch_ts.to_string(),
                    });
                }
            }
        }

        // Collect from node-level
        for d in &node.decisions {
            if seen_decisions.insert(d.decided.clone()) {
                decisions.push(VineDecision {
                    decision: d.clone(),
                    bunch_index,
                    bunch_ts: bunch_ts.to_string(),
                });
            }
        }
        for c in &node.corrections {
            let key = format!("{}→{}", c.wrong, c.right);
            if seen_corrections.insert(key) {
                corrections.push(VineCorrection {
                    correction: c.clone(),
                    bunch_index,
                    bunch_ts: bunch_ts.to_string(),
                });
            }
        }
    }

    // Deduplicate
    entities.sort();
    entities.dedup();
    topics.sort();
    topics.dedup();

    let open_questions = if apex.self_prompt.is_empty() {
        Vec::new()
    } else {
        vec![apex.self_prompt.clone()]
    };

    let penultimate_summaries: Vec<String> = penultimate_nodes
        .iter()
        .map(|n| n.distilled.clone())
        .collect();

    VineBunchMetadata {
        topics,
        entities,
        decisions,
        corrections,
        open_questions,
        penultimate_summaries,
    }
}

// ── Vine L0 Assembly ─────────────────────────────────────────────────────────

/// Assemble vine L0 nodes from built bunches.
/// Creates one vine L0 node per apex + one per penultimate node from each bunch.
/// Returns (total L0 count, mapping from L0 node ID → bunch_index).
pub fn assemble_vine_l0(
    conn: &Connection,
    vine_slug: &str,
    bunches: &[VineBunch],
) -> Result<(i64, HashMap<String, i64>)> {
    let mut global_index: i64 = 0;
    let mut l0_to_bunch: HashMap<String, i64> = HashMap::new();

    for bunch in bunches {
        let metadata = match &bunch.metadata {
            Some(m) => m,
            None => {
                warn!(
                    "Bunch {} has no metadata, skipping vine L0 assembly",
                    bunch.bunch_slug
                );
                continue;
            }
        };

        // Read the apex node from the bunch pyramid
        let apex = match query::get_apex(conn, &bunch.bunch_slug)? {
            Some(a) => a,
            None => {
                warn!("Bunch {} has no apex, skipping", bunch.bunch_slug);
                continue;
            }
        };

        // Create vine L0 node for the apex
        let apex_l0_id = format!("L0-{:03}", global_index);
        let topic_list = metadata.topics.join(", ");
        let date_range = format!(
            "{} → {}",
            bunch.first_ts.as_deref().unwrap_or("?"),
            bunch.last_ts.as_deref().unwrap_or("?"),
        );
        let apex_content = format!(
            "## Session [{}]: {}\nDate: {}\nMessages: {}\nTopics: {}\n\n### Summary\n{}",
            bunch.bunch_index,
            apex.headline,
            date_range,
            bunch.message_count.unwrap_or(0),
            topic_list,
            apex.distilled,
        );

        let apex_l0_node = PyramidNode {
            id: apex_l0_id.clone(),
            slug: vine_slug.to_string(),
            depth: 0,
            chunk_index: Some(global_index),
            headline: format!("Session {}: {}", bunch.bunch_index, apex.headline),
            distilled: apex_content,
            topics: apex.topics.clone(),
            corrections: apex.corrections.clone(),
            decisions: apex.decisions.clone(),
            terms: apex.terms.clone(),
            dead_ends: Vec::new(),
            self_prompt: apex.self_prompt.clone(),
            children: Vec::new(),
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(), // db fills this
            narrative: apex.narrative.clone(),
            entities: apex.entities.clone(),
            key_quotes: apex.key_quotes.clone(),
            transitions: apex.transitions.clone(),
            time_range: apex.time_range.clone(),
            weight: apex.weight,
            ..Default::default()
        };

        db::save_node(
            conn,
            &apex_l0_node,
            Some(&serde_json::to_string(&apex.topics)?),
        )?;
        l0_to_bunch.insert(apex_l0_id, bunch.bunch_index);
        global_index += 1;

        // Create vine L0 nodes for penultimate layer
        for pen_node_id in &bunch.penultimate_node_ids {
            let pen_node = db::get_node(conn, &bunch.bunch_slug, pen_node_id)?;
            if let Some(pn) = pen_node {
                let pen_l0_id = format!("L0-{:03}", global_index);
                let pen_content = format!(
                    "## Session [{}] Thread: {}\nDate: {}\n\n{}",
                    bunch.bunch_index, pn.headline, date_range, pn.distilled,
                );

                let pen_l0_node = PyramidNode {
                    id: pen_l0_id.clone(),
                    slug: vine_slug.to_string(),
                    depth: 0,
                    chunk_index: Some(global_index),
                    headline: format!("Session {} / {}", bunch.bunch_index, pn.headline),
                    distilled: pen_content,
                    topics: pn.topics.clone(),
                    corrections: pn.corrections.clone(),
                    decisions: pn.decisions.clone(),
                    terms: pn.terms.clone(),
                    dead_ends: Vec::new(),
                    self_prompt: String::new(),
                    children: Vec::new(),
                    parent_id: None,
                    superseded_by: None,
                    build_id: None,
                    created_at: String::new(),
                    narrative: pn.narrative.clone(),
                    entities: pn.entities.clone(),
                    key_quotes: pn.key_quotes.clone(),
                    transitions: pn.transitions.clone(),
                    time_range: pn.time_range.clone(),
                    weight: pn.weight,
                    ..Default::default()
                };

                db::save_node(
                    conn,
                    &pen_l0_node,
                    Some(&serde_json::to_string(&pn.topics)?),
                )?;
                l0_to_bunch.insert(pen_l0_id, bunch.bunch_index);
                global_index += 1;
            }
        }
    }

    info!(
        "Assembled {global_index} vine L0 nodes for {} ({} bunch mappings)",
        vine_slug,
        l0_to_bunch.len()
    );
    Ok((global_index, l0_to_bunch))
}

// ── Vine Build Pipeline Helper ───────────────────────────────────────────────

/// Run the build pipeline for a single slug, setting up all required channels.
/// Extracted from routes.rs to be shared between HTTP builds and vine bunch builds.
pub async fn run_build_pipeline(
    reader: Arc<Mutex<Connection>>,
    writer: Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    content_type: ContentType,
    cancel: &CancellationToken,
    bus: Option<&super::event_bus::BuildEventBus>,
) -> Result<i32> {
    // Use shared writer drain helper (single implementation, not duplicated)
    let (write_tx, writer_handle) = spawn_write_drain(writer);

    // Create progress channel. When a BuildEventBus is supplied, tee onto the
    // bus so the public web surface (post-agents-retro WS) can subscribe
    // per-slug. The downstream debug-log consumer is unaffected.
    let (progress_tx, raw_progress_rx) = mpsc::channel::<BuildProgress>(64);
    let mut progress_rx = if let Some(bus) = bus {
        super::event_bus::tee_build_progress_to_bus(bus, slug.to_string(), raw_progress_rx)
    } else {
        raw_progress_rx
    };
    let slug_for_progress = slug.to_string();
    let progress_handle = tokio::spawn(async move {
        while let Some(prog) = progress_rx.recv().await {
            tracing::debug!(
                "Build progress for '{}': {}/{}",
                slug_for_progress,
                prog.done,
                prog.total
            );
        }
    });

    // Dispatch by content type
    let result = match content_type {
        ContentType::Conversation => {
            build::build_conversation(reader, &write_tx, llm_config, slug, cancel, &progress_tx)
                .await
        }
        ContentType::Code => {
            build::build_code(reader, &write_tx, llm_config, slug, cancel, &progress_tx).await
        }
        ContentType::Document => {
            build::build_docs(reader, &write_tx, llm_config, slug, cancel, &progress_tx).await
        }
        ContentType::Vine => Err(anyhow!(
            "Vine build uses vine-specific pipeline, not run_build_pipeline"
        )),
        ContentType::Question => Err(anyhow!(
            "Question build uses question-driven pipeline, not run_build_pipeline"
        )),
    };

    // Clean up channels
    drop(write_tx);
    drop(progress_tx);
    let _ = writer_handle.await;
    let _ = progress_handle.await;

    // WS-EVENTS §15.21: SlopeChanged catch-all at legacy build-pipeline
    // completion. `build::build_conversation` / `build_code` / `build_docs`
    // don't have the bus threaded through yet (intentionally unrefactored
    // per WS-EVENTS scope — brief forbids restructuring those call sites),
    // so we emit once here on success to guarantee WS-PRIMER subscribers
    // see a cache-invalidation edge after every successful legacy build.
    // Empty `affected_layers` = "revalidate everything".
    if result.is_ok() {
        if let Some(bus) = bus {
            let _ = bus.tx.send(super::event_bus::TaggedBuildEvent {
                slug: slug.to_string(),
                kind: super::event_bus::TaggedKind::SlopeChanged {
                    affected_layers: Vec::new(),
                },
            });
        }
    }

    result
}

// ── Vine DB CRUD ─────────────────────────────────────────────────────────────

/// Insert a vine bunch record. Sets status to 'pending'.
pub fn insert_vine_bunch(
    conn: &Connection,
    vine_slug: &str,
    bunch_slug: &str,
    session_id: &str,
    jsonl_path: &str,
    bunch_index: i64,
    first_ts: Option<&str>,
    last_ts: Option<&str>,
    message_count: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO vine_bunches
         (vine_slug, bunch_slug, session_id, jsonl_path, bunch_index, first_ts, last_ts, message_count, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')
         ON CONFLICT(vine_slug, bunch_slug) DO UPDATE SET
            session_id = excluded.session_id,
            jsonl_path = excluded.jsonl_path,
            bunch_index = excluded.bunch_index,
            first_ts = excluded.first_ts,
            last_ts = excluded.last_ts,
            message_count = excluded.message_count,
            status = CASE WHEN vine_bunches.status = 'built' THEN vine_bunches.status ELSE 'pending' END,
            updated_at = datetime('now')",
        rusqlite::params![vine_slug, bunch_slug, session_id, jsonl_path, bunch_index, first_ts, last_ts, message_count],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update a vine bunch record after build completes.
pub fn update_vine_bunch_built(
    conn: &Connection,
    vine_slug: &str,
    bunch_slug: &str,
    apex_node_id: &str,
    penultimate_node_ids: &[String],
    chunk_count: i64,
    metadata: &VineBunchMetadata,
) -> Result<()> {
    let pen_json = serde_json::to_string(penultimate_node_ids)?;
    let meta_json = serde_json::to_string(metadata)?;
    conn.execute(
        "UPDATE vine_bunches SET status = 'built', apex_node_id = ?1, penultimate_node_ids = ?2,
         chunk_count = ?3, metadata = ?4, updated_at = datetime('now')
         WHERE vine_slug = ?5 AND bunch_slug = ?6",
        rusqlite::params![
            apex_node_id,
            pen_json,
            chunk_count,
            meta_json,
            vine_slug,
            bunch_slug
        ],
    )?;
    Ok(())
}

/// Update vine bunch status (e.g., to 'building', 'error').
pub fn update_vine_bunch_status(
    conn: &Connection,
    vine_slug: &str,
    bunch_slug: &str,
    status: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE vine_bunches SET status = ?1, updated_at = datetime('now')
         WHERE vine_slug = ?2 AND bunch_slug = ?3",
        rusqlite::params![status, vine_slug, bunch_slug],
    )?;
    Ok(())
}

/// Get all vine bunches for a vine, ordered by bunch_index.
pub fn get_vine_bunches(conn: &Connection, vine_slug: &str) -> Result<Vec<VineBunch>> {
    let mut stmt = conn.prepare(
        "SELECT id, vine_slug, bunch_slug, session_id, jsonl_path, bunch_index,
                first_ts, last_ts, message_count, chunk_count, apex_node_id,
                penultimate_node_ids, status, metadata, created_at, updated_at
         FROM vine_bunches WHERE vine_slug = ?1 ORDER BY bunch_index ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![vine_slug], |row| {
        let pen_json: String = row
            .get::<_, String>(11)
            .unwrap_or_else(|_| "[]".to_string());
        let pen_ids: Vec<String> = serde_json::from_str(&pen_json).unwrap_or_default();
        let meta_json: Option<String> = row.get(13).ok();
        let metadata: Option<VineBunchMetadata> =
            meta_json.and_then(|j| serde_json::from_str(&j).ok());

        Ok(VineBunch {
            id: row.get(0)?,
            vine_slug: row.get(1)?,
            bunch_slug: row.get(2)?,
            session_id: row.get(3)?,
            jsonl_path: row.get(4)?,
            bunch_index: row.get(5)?,
            first_ts: row.get(6)?,
            last_ts: row.get(7)?,
            message_count: row.get(8)?,
            chunk_count: row.get(9)?,
            apex_node_id: row.get(10)?,
            penultimate_node_ids: pen_ids,
            status: row.get(12)?,
            metadata,
        })
    })?;

    let mut bunches = Vec::new();
    for row in rows {
        bunches.push(row?);
    }
    Ok(bunches)
}

/// Get a single vine bunch by slug.
pub fn get_vine_bunch(
    conn: &Connection,
    vine_slug: &str,
    bunch_slug: &str,
) -> Result<Option<VineBunch>> {
    let result = conn.query_row(
        "SELECT id, vine_slug, bunch_slug, session_id, jsonl_path, bunch_index,
                first_ts, last_ts, message_count, chunk_count, apex_node_id,
                penultimate_node_ids, status, metadata, created_at, updated_at
         FROM vine_bunches WHERE vine_slug = ?1 AND bunch_slug = ?2",
        rusqlite::params![vine_slug, bunch_slug],
        |row| {
            let pen_json: String = row
                .get::<_, String>(11)
                .unwrap_or_else(|_| "[]".to_string());
            let pen_ids: Vec<String> = serde_json::from_str(&pen_json).unwrap_or_default();
            let meta_json: Option<String> = row.get(13).ok();
            let metadata: Option<VineBunchMetadata> =
                meta_json.and_then(|j| serde_json::from_str(&j).ok());

            Ok(VineBunch {
                id: row.get(0)?,
                vine_slug: row.get(1)?,
                bunch_slug: row.get(2)?,
                session_id: row.get(3)?,
                jsonl_path: row.get(4)?,
                bunch_index: row.get(5)?,
                first_ts: row.get(6)?,
                last_ts: row.get(7)?,
                message_count: row.get(8)?,
                chunk_count: row.get(9)?,
                apex_node_id: row.get(10)?,
                penultimate_node_ids: pen_ids,
                status: row.get(12)?,
                metadata,
            })
        },
    );

    match result {
        Ok(bunch) => Ok(Some(bunch)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Clean up bunches left in 'building' state (crash recovery).
/// Deletes their partial pyramid data and resets to 'pending'.
///
/// NOTE: Cannot use db::delete_slug() because that cascade-deletes the pyramid_slugs row,
/// which in turn cascade-deletes the vine_bunches row (FK on bunch_slug → pyramid_slugs).
/// Instead, delete nodes/chunks/steps directly while preserving the slug entry.
pub fn cleanup_building_bunches(conn: &Connection, vine_slug: &str) -> Result<i64> {
    let building: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT bunch_slug FROM vine_bunches WHERE vine_slug = ?1 AND status = 'building'",
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let count = building.len() as i64;
    for bunch_slug in &building {
        info!("Cleaning up partial build for bunch '{bunch_slug}'");
        // Supersede all nodes (partial builds are still contributions)
        let cleanup_build_id = format!("vine-cleanup-{}", uuid::Uuid::new_v4());
        db::supersede_all_nodes(conn, bunch_slug, &cleanup_build_id)?;
        // Scope execution tables by build_id (old records retained as history)
        conn.execute(
            "UPDATE pyramid_pipeline_steps SET build_id = ?2 WHERE slug = ?1 AND build_id IS NULL",
            rusqlite::params![bunch_slug, &cleanup_build_id],
        )?;
        conn.execute(
            "UPDATE pyramid_threads SET build_id = ?2 WHERE slug = ?1 AND build_id IS NULL",
            rusqlite::params![bunch_slug, &cleanup_build_id],
        )?;
        conn.execute(
            "UPDATE pyramid_deltas SET build_id = ?2 WHERE slug = ?1 AND build_id IS NULL",
            rusqlite::params![bunch_slug, &cleanup_build_id],
        )?;
        conn.execute(
            "UPDATE pyramid_distillations SET build_id = ?2 WHERE slug = ?1 AND build_id IS NULL",
            rusqlite::params![bunch_slug, &cleanup_build_id],
        )?;
        // Reset slug stats
        conn.execute(
            "UPDATE pyramid_slugs SET node_count = 0, max_depth = 0, last_built_at = NULL WHERE slug = ?1",
            rusqlite::params![bunch_slug],
        )?;
        // Reset vine_bunches status
        conn.execute(
            "UPDATE vine_bunches SET status = 'pending', apex_node_id = NULL, penultimate_node_ids = NULL,
             chunk_count = NULL, metadata = NULL, updated_at = datetime('now')
             WHERE vine_slug = ?1 AND bunch_slug = ?2",
            rusqlite::params![vine_slug, bunch_slug],
        )?;
    }

    if count > 0 {
        info!("Cleaned up {count} partial bunch builds for vine '{vine_slug}'");
    }
    Ok(count)
}

// ── Build Bunch ──────────────────────────────────────────────────────────────

/// Build a single conversation bunch: ingest JSONL → build pyramid → extract metadata.
pub async fn build_bunch(
    state: &PyramidState,
    vine_slug: &str,
    bunch: &BunchDiscovery,
    bunch_index: i64,
    evidence_mode: &str,
    cancel: &CancellationToken,
) -> Result<VineBunch> {
    // Find a non-colliding bunch slug — increment index if the slug already has chunks
    let bunch_slug = {
        let conn = state.reader.lock().await;
        let mut idx = bunch_index;
        loop {
            let candidate = format!("{vine_slug}--bunch-{idx:03}");
            let has_chunks = conn.query_row(
                "SELECT COUNT(*) FROM pyramid_chunks WHERE slug = ?1",
                rusqlite::params![candidate],
                |row| row.get::<_, i64>(0),
            ).unwrap_or(0);
            if has_chunks == 0 {
                break candidate;
            }
            idx += 1;
        }
    };
    let jsonl_path = bunch.jsonl_path.clone();
    let session_id = bunch.session_id.clone();

    info!(
        "Building bunch {bunch_index}: session={}, messages={}, slug='{bunch_slug}'",
        &session_id[..8.min(session_id.len())],
        bunch.message_count
    );

    // Mark as building
    {
        let conn = state.writer.lock().await;
        update_vine_bunch_status(&conn, vine_slug, &bunch_slug, "building")?;
    }

    // Step 1: Pre-create the bunch slug (avoid unvalidated path in ingest.rs)
    {
        let conn = state.writer.lock().await;
        if db::get_slug(&conn, &bunch_slug)?.is_none() {
            db::create_slug(
                &conn,
                &bunch_slug,
                &ContentType::Conversation,
                &jsonl_path.to_string_lossy(),
            )?;
        }
    }

    // Step 2: Ingest (synchronous — lock writer, call in spawn_blocking)
    {
        let writer = state.writer.clone();
        let slug_clone = bunch_slug.clone();
        let path_clone = jsonl_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = writer.blocking_lock();
            ingest::ingest_conversation(&conn, &slug_clone, &path_clone)
        })
        .await
        .map_err(|e| anyhow!("Ingest task panicked: {e}"))??;
    }

    // Step 3: Build pyramid via chain executor (uses conversation-episodic chain).
    // run_build_from dispatches through the chain engine, producing episodic-quality
    // bedrocks with the full schema (decisions, quotes, transitions, narrative).
    let build_state = match state.with_build_reader() {
        Ok(s) => s,
        Err(_) => std::sync::Arc::new(PyramidState {
            reader: state.reader.clone(),
            writer: state.writer.clone(),
            config: state.config.clone(),
            active_build: state.active_build.clone(),
            data_dir: state.data_dir.clone(),
            stale_engines: state.stale_engines.clone(),
            file_watchers: state.file_watchers.clone(),
            vine_builds: state.vine_builds.clone(),
            use_chain_engine: std::sync::atomic::AtomicBool::new(
                state.use_chain_engine.load(std::sync::atomic::Ordering::Relaxed),
            ),
            use_ir_executor: std::sync::atomic::AtomicBool::new(
                state.use_ir_executor.load(std::sync::atomic::Ordering::Relaxed),
            ),
            event_bus: state.event_bus.clone(),
            operational: state.operational.clone(),
            chains_dir: state.chains_dir.clone(),
            remote_query_rate_limiter: state.remote_query_rate_limiter.clone(),
            absorption_gate: state.absorption_gate.clone(),
            build_event_bus: state.build_event_bus.clone(),
            supabase_url: state.supabase_url.clone(),
            supabase_anon_key: state.supabase_anon_key.clone(),
            csrf_secret: state.csrf_secret,
            dadbear_handle: state.dadbear_handle.clone(),
            dadbear_in_flight: state.dadbear_in_flight.clone(),
            provider_registry: state.provider_registry.clone(),
            credential_store: state.credential_store.clone(),
            schema_registry: state.schema_registry.clone(),
        }),
    };

    let (write_tx, mut write_rx) = mpsc::channel::<build::WriteOp>(256);
    let write_writer = state.writer.clone();
    let writer_handle = tokio::spawn(async move {
        while let Some(op) = write_rx.recv().await {
            let conn = write_writer.lock().await;
            let result = match op {
                build::WriteOp::SaveNode { ref node, ref topics_json } => {
                    db::save_node(&conn, node, topics_json.as_deref())
                }
                build::WriteOp::SaveStep { ref slug, ref step_type, chunk_index, depth, ref node_id, ref output_json, ref model, elapsed } => {
                    db::save_step(&conn, slug, step_type, chunk_index, depth, node_id, output_json, model, elapsed)
                }
                build::WriteOp::UpdateParent { ref slug, ref node_id, ref parent_id } => {
                    db::update_parent(&conn, slug, node_id, parent_id)
                }
                build::WriteOp::UpdateStats { ref slug } => {
                    db::update_slug_stats(&conn, slug)
                }
                build::WriteOp::UpdateFileHash { ref slug, ref file_path, ref node_id } => {
                    db::append_node_id_to_file_hash(&conn, slug, file_path, node_id)
                }
                build::WriteOp::Flush { done } => {
                    let _ = done.send(());
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::error!("Vine bunch WriteOp failed: {e}");
            }
        }
    });

    let (progress_tx, _progress_rx) = mpsc::channel::<BuildProgress>(64);

    let result = super::build_runner::run_build_from_with_evidence_mode(
        &build_state,
        &bunch_slug,
        0,
        None,
        None,
        evidence_mode,
        cancel,
        Some(progress_tx),
        &write_tx,
        None,
    )
    .await;

    drop(write_tx);
    let _ = writer_handle.await;

    match &result {
        Ok((_build_id, node_count, _activities)) => {
            info!("Bunch '{bunch_slug}' built via chain executor: {node_count} nodes");
        }
        Err(e) => {
            warn!("Bunch '{bunch_slug}' chain build failed, falling back to legacy: {e}");
            // Fallback to legacy pipeline if chain executor fails
            let reader = if let Some(data_dir) = state.data_dir.as_ref() {
                let build_conn = db::open_pyramid_connection(&data_dir.join("pyramid.db"))?;
                Arc::new(Mutex::new(build_conn))
            } else {
                state.reader.clone()
            };
            let writer = state.writer.clone();
            let llm_config = {
                let cfg = state.config.read().await;
                cfg.clone()
            };
            run_build_pipeline(
                reader, writer, &llm_config, &bunch_slug,
                ContentType::Conversation, cancel, Some(&state.build_event_bus),
            ).await?;
        }
    }

    // Step 4: Read apex + penultimate layer
    let (apex, penultimate_nodes, chunk_count) = {
        let conn = state.reader.lock().await;
        let apex = query::get_apex(&conn, &bunch_slug)?
            .ok_or_else(|| anyhow!("No apex found for bunch '{bunch_slug}'"))?;

        let pen_depth = if apex.depth > 0 { apex.depth - 1 } else { 0 };
        let pen_nodes = if pen_depth > 0 && pen_depth < apex.depth {
            db::get_nodes_at_depth(&conn, &bunch_slug, pen_depth)?
        } else {
            Vec::new()
        };

        let chunks = db::count_chunks(&conn, &bunch_slug)?;
        (apex, pen_nodes, chunks)
    };

    let penultimate_node_ids: Vec<String> =
        penultimate_nodes.iter().map(|n| n.id.clone()).collect();

    // Step 5: Extract metadata
    let metadata = extract_bunch_metadata(
        &apex,
        &penultimate_nodes,
        bunch_index,
        bunch.first_ts.as_str(),
    );

    // Step 6: Update vine_bunches record
    {
        let conn = state.writer.lock().await;
        update_vine_bunch_built(
            &conn,
            vine_slug,
            &bunch_slug,
            &apex.id,
            &penultimate_node_ids,
            chunk_count,
            &metadata,
        )?;
    }

    info!(
        "Bunch '{bunch_slug}' built: apex={}, penultimate={}, chunks={chunk_count}",
        apex.id,
        penultimate_node_ids.len()
    );

    Ok(VineBunch {
        id: 0, // DB-assigned
        vine_slug: vine_slug.to_string(),
        bunch_slug,
        session_id,
        jsonl_path: jsonl_path.to_string_lossy().to_string(),
        bunch_index,
        first_ts: Some(bunch.first_ts.clone()),
        last_ts: Some(bunch.last_ts.clone()),
        message_count: Some(bunch.message_count),
        chunk_count: Some(chunk_count),
        apex_node_id: Some(apex.id),
        penultimate_node_ids,
        status: "built".to_string(),
        metadata: Some(metadata),
    })
}

// ── Build All Bunches ────────────────────────────────────────────────────────

/// Build all bunches for a vine from a JSONL directory.
/// Crash-safe: resumes from last incomplete bunch.
/// State machine: pending → building → built | error
pub async fn build_all_bunches(
    state: &PyramidState,
    vine_slug: &str,
    jsonl_dirs: &[PathBuf],
    evidence_mode: &str,
    cancel: &CancellationToken,
) -> Result<Vec<VineBunch>> {
    // Step 1: Discover conversations
    let discoveries = discover_bunches(jsonl_dirs)?;
    let total = discoveries.len();
    info!("Vine '{vine_slug}': discovered {total} conversation bunches");

    if total == 0 {
        return Ok(Vec::new());
    }

    // Step 2: Create vine slug if it doesn't exist
    {
        let source_display: Vec<String> =
            jsonl_dirs.iter().map(|d| d.display().to_string()).collect();
        let conn = state.writer.lock().await;
        if db::get_slug(&conn, vine_slug)?.is_none() {
            db::create_slug(
                &conn,
                vine_slug,
                &ContentType::Vine,
                &source_display.join(";"),
            )?;
        }
    }

    // Step 3: Pre-create bunch slugs in pyramid_slugs, then register in vine_bunches
    {
        let conn = state.writer.lock().await;
        for (i, discovery) in discoveries.iter().enumerate() {
            let bunch_slug = format!("{vine_slug}--bunch-{:03}", i);
            // Create the bunch slug in pyramid_slugs first (FK target for vine_bunches)
            if db::get_slug(&conn, &bunch_slug)?.is_none() {
                db::create_slug(
                    &conn,
                    &bunch_slug,
                    &ContentType::Conversation,
                    &discovery.jsonl_path.to_string_lossy(),
                )?;
            }
            insert_vine_bunch(
                &conn,
                vine_slug,
                &bunch_slug,
                &discovery.session_id,
                &discovery.jsonl_path.to_string_lossy(),
                i as i64,
                Some(&discovery.first_ts),
                Some(&discovery.last_ts),
                Some(discovery.message_count),
            )?;
        }
    }

    // Step 4: Clean up any bunches left in 'building' state from a previous crash
    {
        let conn = state.writer.lock().await;
        cleanup_building_bunches(&conn, vine_slug)?;
    }

    // Step 5: Build each bunch sequentially
    let mut built_bunches = Vec::new();

    for (i, discovery) in discoveries.iter().enumerate() {
        if cancel.is_cancelled() {
            info!("Vine build cancelled at bunch {i}/{total}");
            break;
        }

        let bunch_slug = format!("{vine_slug}--bunch-{:03}", i);

        // Check if already built
        let already_built = {
            let conn = state.reader.lock().await;
            let status: Option<String> = conn
                .query_row(
                    "SELECT status FROM vine_bunches WHERE vine_slug = ?1 AND bunch_slug = ?2",
                    rusqlite::params![vine_slug, bunch_slug],
                    |row| row.get(0),
                )
                .ok();
            status.as_deref() == Some("built")
        };

        if already_built {
            info!("Bunch {i}/{total} '{bunch_slug}' already built, skipping");
            // Load this specific bunch's data
            let conn = state.reader.lock().await;
            let bunch = get_vine_bunch(&conn, vine_slug, &bunch_slug)?;
            if let Some(b) = bunch {
                built_bunches.push(b);
            }
            continue;
        }

        info!("Building bunch {i}/{total}...");

        let max_retries = 3;
        let mut attempt = 0;
        let mut succeeded = false;

        while attempt < max_retries && !succeeded {
            attempt += 1;
            if attempt > 1 {
                info!("Retrying bunch {i}/{total} (attempt {attempt}/{max_retries})...");
            }

            match build_bunch(state, vine_slug, discovery, i as i64, evidence_mode, cancel).await {
                Ok(bunch) => {
                    built_bunches.push(bunch);
                    succeeded = true;
                }
                Err(e) => {
                    warn!("Bunch {i}/{total} '{bunch_slug}' attempt {attempt} failed: {e}");
                    if attempt < max_retries {
                        // Brief pause before retry
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    } else {
                        error!("Bunch {i}/{total} '{bunch_slug}' failed after {max_retries} attempts: {e}");
                        let conn = state.writer.lock().await;
                        let _ = update_vine_bunch_status(&conn, vine_slug, &bunch_slug, "error");
                        // Continue to next bunch
                    }
                }
            }
        }
    }

    info!(
        "Vine '{vine_slug}': {}/{total} bunches built successfully",
        built_bunches.len()
    );

    Ok(built_bunches)
}

// ── Vine L1: LLM Clustering ─────────────────────────────────────────────────

/// Build vine L1 nodes by LLM-clustering bunches into temporal-topical neighborhoods.
/// Each cluster becomes one vine L1 node, synthesized with THREAD_NARRATIVE_PROMPT.
pub async fn build_vine_l1(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
    cancel: &CancellationToken,
) -> Result<i64> {
    if bunches.is_empty() {
        return Ok(0);
    }

    let llm_config = {
        let cfg = state.config.read().await;
        cfg.clone()
    };

    // Step 1: Build inventory of bunch summaries for the clustering prompt
    let mut bunch_summaries = Vec::new();
    for bunch in bunches {
        let meta = match &bunch.metadata {
            Some(m) => m,
            None => continue,
        };
        bunch_summaries.push(serde_json::json!({
            "bunch_index": bunch.bunch_index,
            "date_range": format!("{} → {}",
                bunch.first_ts.as_deref().unwrap_or("?"),
                bunch.last_ts.as_deref().unwrap_or("?")),
            "messages": bunch.message_count.unwrap_or(0),
            "topics": meta.topics,
            "entities": &meta.entities[..meta.entities.len().min(20)], // cap for prompt size
        }));
    }

    let inventory_json = serde_json::to_string_pretty(&bunch_summaries)?;
    info!("Vine L1 clustering: {} bunches", bunches.len());

    // Step 2: LLM clustering call
    let clusters_value = build::call_and_parse(
        &llm_config,
        super::vine_prompts::VINE_CLUSTER_PROMPT,
        &inventory_json,
        "vine-l1-cluster",
    )
    .await?;

    let clusters = clusters_value
        .get("clusters")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("LLM returned no 'clusters' array"))?;

    info!("Vine L1: LLM produced {} clusters", clusters.len());

    // Validate cluster coverage: every bunch should appear in exactly one cluster
    {
        let valid_indices: HashSet<i64> = bunches.iter().map(|b| b.bunch_index).collect();
        let mut seen_indices: HashSet<i64> = HashSet::new();
        for cluster in clusters {
            if let Some(indices) = cluster.get("bunch_indices").and_then(|b| b.as_array()) {
                for idx in indices.iter().filter_map(|v| v.as_i64()) {
                    if !valid_indices.contains(&idx) {
                        warn!("LLM cluster references unknown bunch_index {idx}, ignoring");
                    } else if !seen_indices.insert(idx) {
                        warn!("Bunch index {idx} assigned to multiple clusters");
                    }
                }
            }
        }
        let mut uncovered: Vec<i64> = valid_indices.difference(&seen_indices).copied().collect();
        uncovered.sort();
        if !uncovered.is_empty() {
            warn!(
                "LLM clustering missed {} bunches: {:?}. They will become orphan L0 nodes.",
                uncovered.len(),
                uncovered
            );
        }
    }

    // Step 3: For each cluster, gather the vine L0 nodes from those bunches and synthesize
    let (write_tx, writer_handle) = spawn_write_drain(state.writer.clone());

    // Load all L0 nodes once (not per-cluster)
    let all_l0_nodes = {
        let conn = state.reader.lock().await;
        db::get_nodes_at_depth(&conn, vine_slug, 0)?
    };

    let mut l1_count: i64 = 0;

    for (cluster_idx, cluster) in clusters.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }

        let cluster_name = cluster
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("Unnamed Cluster");
        let bunch_indices: Vec<i64> = cluster
            .get("bunch_indices")
            .and_then(|b| b.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();

        if bunch_indices.is_empty() {
            warn!("Cluster '{cluster_name}' has no bunch_indices, skipping");
            continue;
        }

        // Gather all vine L0 nodes for the bunches in this cluster
        let l0_nodes = {
            // Filter pre-loaded L0 nodes using delimiter-aware prefix matching
            // "Session 1:" must not match "Session 10:" or "Session 11:"
            let mut cluster_nodes = Vec::new();
            for node in &all_l0_nodes {
                for &bi in &bunch_indices {
                    let prefix_colon = format!("Session {}:", bi);
                    let prefix_slash = format!("Session {} /", bi);
                    if node.headline.starts_with(&prefix_colon)
                        || node.headline.starts_with(&prefix_slash)
                    {
                        cluster_nodes.push(node.clone());
                        break;
                    }
                }
            }
            cluster_nodes
        };

        if l0_nodes.is_empty() {
            warn!("Cluster '{cluster_name}' matched no L0 nodes, skipping");
            continue;
        }

        let l1_id = format!("L1-{cluster_idx:03}");

        // Check if already built (crash-safe resume)
        let exists = {
            let conn = state.reader.lock().await;
            db::step_exists(&conn, vine_slug, "synth", -1, 1, &l1_id)?
        };
        if exists {
            l1_count += 1;
            continue;
        }

        // Build episodic node payloads for synthesize_recursive.md
        let mut node_payloads = Vec::new();
        for (order, node) in l0_nodes.iter().enumerate() {
            let mut payload = build::episodic_child_payload_json(node);
            payload["input_order"] = serde_json::json!(order);
            node_payloads.push(payload);
        }

        let user_prompt = format!(
            "## CLUSTER: {cluster_name}\n## INPUT NODES ({} nodes)\n{}",
            node_payloads.len(),
            serde_json::to_string_pretty(&node_payloads)?
        );

        info!(
            "  L1-{cluster_idx:03}: '{cluster_name}' ({} nodes, {} payloads)",
            l0_nodes.len(),
            node_payloads.len()
        );

        let t0 = Instant::now();

        match build::call_and_parse(
            &llm_config,
            SYNTHESIZE_RECURSIVE_PROMPT,
            &user_prompt,
            &format!("vine-l1-{cluster_idx}"),
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();
                let topics_json = serde_json::to_string(
                    analysis.get("topics").unwrap_or(&serde_json::json!([])),
                )?;
                let output_json = serde_json::to_string(&analysis)?;

                build::send_save_step(
                    &write_tx,
                    vine_slug,
                    "synth",
                    -1,
                    1,
                    &l1_id,
                    &output_json,
                    &llm_config.primary_model,
                    elapsed,
                )
                .await;

                let children: Vec<String> = l0_nodes.iter().map(|n| n.id.clone()).collect();
                let mut node = match chain_dispatch::build_node_from_output(
                    &analysis,
                    &l1_id,
                    vine_slug,
                    1,
                    None,
                ) {
                    Ok(n) => n,
                    Err(e) => {
                        error!("build_node_from_output failed for vine L1 '{l1_id}': {e}");
                        continue;
                    }
                };
                node.children = children.clone();
                // Backfill distilled from narrative for downstream consumers
                node.distilled = node.narrative.levels.first().map(|l| l.text.clone()).unwrap_or_default();
                build::send_save_node(&write_tx, node, Some(topics_json)).await;

                // Set parent pointers on L0 nodes
                for child_id in &children {
                    build::send_update_parent(&write_tx, vine_slug, child_id, &l1_id).await;
                }

                l1_count += 1;
            }
            Err(e) => {
                error!("Vine L1 synthesis failed for cluster '{cluster_name}': {e}");
            }
        }
    }

    drop(write_tx);
    let _ = writer_handle.await;

    // Update slug stats
    {
        let conn = state.writer.lock().await;
        db::update_slug_stats(&conn, vine_slug)?;
    }

    info!("Vine L1: built {l1_count} cluster nodes for '{vine_slug}'");
    Ok(l1_count)
}

// ── Vine L2+/Apex: Pair-and-Distill ─────────────────────────────────────────

/// Build vine L2+ layers by pairing adjacent nodes and distilling with DISTILL_PROMPT.
/// Recurses upward until a single apex node remains.
/// Includes step_exists() checks for crash-safe resumability.
/// Returns (apex_id, failure_count).
pub async fn build_vine_upper(
    state: &PyramidState,
    vine_slug: &str,
    cancel: &CancellationToken,
) -> Result<(String, i32)> {
    let llm_config = {
        let cfg = state.config.read().await;
        cfg.clone()
    };

    // Set up write channel
    let (write_tx, writer_handle) = spawn_write_drain(state.writer.clone());

    let mut depth: i64 = 1; // Start from L1
    let mut apex_id = String::new();
    let mut failures: i32 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Get current layer nodes
        let current_nodes = {
            let conn = state.reader.lock().await;
            db::get_nodes_at_depth(&conn, vine_slug, depth)?
        };

        if current_nodes.len() <= 1 {
            if let Some(node) = current_nodes.first() {
                apex_id = node.id.clone();
                info!("Vine apex reached: {} at depth {depth}", node.id);
            }
            break;
        }

        let next_depth = depth + 1;
        let expected = (current_nodes.len() + 1) / 2;
        info!(
            "Vine L{next_depth}: pairing {} nodes → ~{expected}",
            current_nodes.len()
        );

        let mut pair_idx: usize = 0;
        let mut i: usize = 0;

        while i < current_nodes.len() {
            if cancel.is_cancelled() {
                break;
            }

            let node_id = format!("L{next_depth}-{pair_idx:03}");

            // Crash-safe resume: skip if already built
            let exists = {
                let conn = state.reader.lock().await;
                db::step_exists(&conn, vine_slug, "synth", -1, next_depth, &node_id)?
            };
            if exists {
                pair_idx += 1;
                i += 2;
                continue;
            }

            if i + 1 < current_nodes.len() {
                // Pair two nodes
                let left = &current_nodes[i];
                let right = &current_nodes[i + 1];

                let left_payload = build::episodic_child_payload_json(left);
                let right_payload = build::episodic_child_payload_json(right);

                let user_prompt = format!(
                    "## SIBLING A (earlier)\n{}\n\n## SIBLING B (later)\n{}",
                    serde_json::to_string_pretty(&left_payload)?,
                    serde_json::to_string_pretty(&right_payload)?
                );

                info!("  [{} + {}] -> {node_id}", left.id, right.id);
                let t0 = Instant::now();

                // Retry up to 3 times
                let mut analysis_result = None;
                for attempt in 0..3 {
                    match build::call_and_parse(
                        &llm_config,
                        SYNTHESIZE_RECURSIVE_PROMPT,
                        &user_prompt,
                        &format!("vine-synth-d{next_depth}-{pair_idx}"),
                    )
                    .await
                    {
                        Ok(a) => {
                            analysis_result = Some(a);
                            break;
                        }
                        Err(e) => {
                            warn!(
                                "  Vine synthesis attempt {}/{} failed for {node_id}: {e}",
                                attempt + 1,
                                3
                            );
                            if attempt < 2 {
                                tokio::time::sleep(std::time::Duration::from_secs(
                                    2u64.pow(attempt as u32 + 1),
                                ))
                                .await;
                            }
                        }
                    }
                }

                if let Some(analysis) = analysis_result {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    let output_json = serde_json::to_string(&analysis)?;

                    build::send_save_step(
                        &write_tx,
                        vine_slug,
                        "synth",
                        -1,
                        next_depth,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let mut node = match chain_dispatch::build_node_from_output(
                        &analysis,
                        &node_id,
                        vine_slug,
                        next_depth,
                        None,
                    ) {
                        Ok(n) => n,
                        Err(e) => {
                            error!("  build_node_from_output failed for {node_id}: {e}");
                            failures += 1;
                            let mut fallback = current_nodes[i].clone();
                            fallback.id = node_id.clone();
                            fallback.depth = next_depth;
                            fallback.chunk_index = None;
                            fallback.children = vec![left.id.clone(), right.id.clone()];
                            build::send_save_node(&write_tx, fallback, None).await;
                            build::send_update_parent(&write_tx, vine_slug, &left.id, &node_id).await;
                            build::send_update_parent(&write_tx, vine_slug, &right.id, &node_id).await;
                            i += 2;
                            pair_idx += 1;
                            continue;
                        }
                    };
                    node.children = vec![left.id.clone(), right.id.clone()];
                    // Backfill distilled from narrative for downstream consumers
                    node.distilled = node.narrative.levels.first().map(|l| l.text.clone()).unwrap_or_default();
                    build::send_save_node(&write_tx, node, Some(topics_json)).await;
                    build::send_update_parent(&write_tx, vine_slug, &left.id, &node_id).await;
                    build::send_update_parent(&write_tx, vine_slug, &right.id, &node_id).await;
                } else {
                    // All retries failed — carry left node up as fallback
                    error!(
                        "  All 3 vine synthesis attempts failed for {node_id}. Carrying left node."
                    );
                    failures += 1;
                    let mut node = current_nodes[i].clone();
                    node.id = node_id.clone();
                    node.depth = next_depth;
                    node.chunk_index = None;
                    node.children = vec![left.id.clone(), right.id.clone()];
                    build::send_save_node(&write_tx, node, None).await;
                    build::send_update_parent(&write_tx, vine_slug, &left.id, &node_id).await;
                    build::send_update_parent(&write_tx, vine_slug, &right.id, &node_id).await;
                }

                i += 2;
            } else {
                // Odd node: carry up
                let carry = &current_nodes[i];
                info!("  Carry up: {} -> {node_id}", carry.id);
                let mut node = carry.clone();
                node.id = node_id.clone();
                node.depth = next_depth;
                node.chunk_index = None;
                node.children = vec![carry.id.clone()];
                build::send_save_node(&write_tx, node, None).await;
                build::send_update_parent(&write_tx, vine_slug, &carry.id, &node_id).await;
                i += 1;
            }

            pair_idx += 1;
        }

        // Flush: wait for the writer to commit all nodes at this depth before
        // reading them back in the next iteration. Without this, the reader
        // (separate WAL connection) may see 0 nodes, causing premature apex
        // declaration. Same fix as chain_executor.rs::execute_recursive_pair.
        build::flush_writes(&write_tx).await;

        depth = next_depth;
    }

    // If the loop ended with multiple nodes at top depth (synthesis failures prevented
    // convergence), force a single apex by merging all top-depth nodes mechanically.
    if apex_id.is_empty() {
        // Flush before reading — same WAL visibility issue as the main loop
        build::flush_writes(&write_tx).await;
        let top_nodes = {
            let conn = state.reader.lock().await;
            db::get_nodes_at_depth(&conn, vine_slug, depth)?
        };
        if top_nodes.len() > 1 {
            let apex_depth = depth + 1;
            let forced_id = format!("L{apex_depth}-000");
            info!("Forcing apex: merging {} L{depth} nodes into {forced_id}", top_nodes.len());

            let combined_headline = top_nodes.iter()
                .map(|n| n.headline.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            let combined_distilled = top_nodes.iter()
                .filter_map(|n| if n.distilled.is_empty() { None } else { Some(n.distilled.as_str()) })
                .collect::<Vec<_>>()
                .join("\n\n");
            let children: Vec<String> = top_nodes.iter().map(|n| n.id.clone()).collect();
            let mut merged_topics = Vec::new();
            for n in &top_nodes {
                for t in &n.topics {
                    if !merged_topics.iter().any(|mt: &super::types::Topic| mt.name == t.name) {
                        merged_topics.push(t.clone());
                    }
                }
            }

            // Merge episodic fields from the best top node (first non-empty)
            let best = top_nodes.first().unwrap(); // top_nodes.len() > 1 guaranteed
            let truncated_headline = if combined_headline.chars().count() > 200 {
                format!("{}...", combined_headline.chars().take(197).collect::<String>())
            } else {
                combined_headline
            };

            let apex_node = PyramidNode {
                id: forced_id.clone(),
                slug: vine_slug.to_string(),
                depth: apex_depth,
                headline: truncated_headline,
                distilled: combined_distilled,
                topics: merged_topics,
                children,
                narrative: best.narrative.clone(),
                entities: best.entities.clone(),
                key_quotes: best.key_quotes.clone(),
                transitions: best.transitions.clone(),
                time_range: best.time_range.clone(),
                weight: top_nodes.iter().map(|n| n.weight).sum(),
                ..Default::default()
            };
            build::send_save_node(&write_tx, apex_node, None).await;
            for n in &top_nodes {
                build::send_update_parent(&write_tx, vine_slug, &n.id, &forced_id).await;
            }
            apex_id = forced_id;
        } else if let Some(node) = top_nodes.first() {
            apex_id = node.id.clone();
        }
    }

    drop(write_tx);
    let _ = writer_handle.await;

    // Update slug stats
    {
        let conn = state.writer.lock().await;
        db::update_slug_stats(&conn, vine_slug)?;
    }

    if failures > 0 {
        warn!("Vine upper layers complete for '{vine_slug}' with {failures} synthesis failure(s). Apex: {apex_id}");
    } else {
        info!("Vine upper layers complete for '{vine_slug}'. Apex: {apex_id}");
    }
    Ok((apex_id, failures))
}

/// Rebuild the L0→bunch_index mapping from existing L0 nodes and bunches.
/// Used when L0 assembly is skipped (nodes already exist from a previous build).
async fn rebuild_l0_to_bunch_map(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
) -> Result<HashMap<String, i64>> {
    let conn = state.reader.lock().await;
    let l0_nodes = db::get_nodes_at_depth(&conn, vine_slug, 0)?;
    let mut mapping = HashMap::new();

    for node in &l0_nodes {
        for bunch in bunches {
            let prefix_colon = format!("Session {}:", bunch.bunch_index);
            let prefix_slash = format!("Session {} /", bunch.bunch_index);
            if node.headline.starts_with(&prefix_colon) || node.headline.starts_with(&prefix_slash)
            {
                mapping.insert(node.id.clone(), bunch.bunch_index);
                break;
            }
        }
    }

    info!(
        "Rebuilt L0→bunch mapping: {} entries for '{vine_slug}'",
        mapping.len()
    );
    Ok(mapping)
}

// ── Full Vine Build Orchestrator ─────────────────────────────────────────────

/// Build an entire vine from a JSONL directory: discover → build bunches → assemble L0 → L1 → L2+/apex.
pub async fn build_vine(
    state: &PyramidState,
    vine_slug: &str,
    jsonl_dirs: &[PathBuf],
    evidence_mode: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    let dir_display: Vec<String> = jsonl_dirs.iter().map(|d| d.display().to_string()).collect();
    info!(
        "=== Starting vine build: '{vine_slug}' from {:?} ===",
        dir_display
    );

    // Phase 2: Build all bunches
    let bunches = build_all_bunches(state, vine_slug, jsonl_dirs, evidence_mode, cancel).await?;
    if bunches.is_empty() {
        return Err(anyhow!("No bunches were built successfully"));
    }

    if cancel.is_cancelled() {
        return Err(anyhow!("Vine build cancelled after bunch construction"));
    }

    // Phase 3a: Assemble vine L0 (skip if already done)
    let existing_l0 = {
        let conn = state.reader.lock().await;
        db::count_nodes_at_depth(&conn, vine_slug, 0)?
    };
    let l0_to_bunch = if existing_l0 > 0 {
        info!("Vine L0: {existing_l0} nodes already exist, skipping assembly");
        // Rebuild the mapping from existing L0 nodes (needed for intelligence passes)
        rebuild_l0_to_bunch_map(state, vine_slug, &bunches).await?
    } else {
        let (count, mapping) = {
            let conn = state.writer.lock().await;
            assemble_vine_l0(&conn, vine_slug, &bunches)?
        };
        info!("Vine L0: {count} nodes assembled");
        mapping
    };

    if cancel.is_cancelled() {
        return Err(anyhow!("Vine build cancelled after L0 assembly"));
    }

    // Phase 3b: LLM clustering → L1
    let l1_count = build_vine_l1(state, vine_slug, &bunches, cancel).await?;
    info!("Vine L1: {l1_count} cluster nodes");

    if cancel.is_cancelled() {
        return Err(anyhow!("Vine build cancelled after L1 clustering"));
    }

    // Phase 3c: L2+/apex
    if l1_count == 0 {
        return Err(anyhow!(
            "No L1 clusters were built — cannot construct vine upper layers"
        ));
    }
    let (apex_id, upper_failures) = build_vine_upper(state, vine_slug, cancel).await?;
    if apex_id.is_empty() {
        return Err(anyhow!("Vine build produced no apex node"));
    }
    if upper_failures > 0 {
        warn!("Vine upper layers had {upper_failures} synthesis failure(s)");
    }

    // Update slug stats one final time
    {
        let conn = state.writer.lock().await;
        db::update_slug_stats(&conn, vine_slug)?;
    }

    // Phase 4: Intelligence passes
    info!("Running intelligence passes for vine '{vine_slug}'...");
    run_intelligence_passes(state, vine_slug, &bunches, &l0_to_bunch, cancel).await?;

    // Phase 5h: Sub-apex directory wiring
    wire_sub_apex_directory(state, vine_slug).await?;

    // Phase 5g: Post-build integrity check
    let integrity = run_integrity_check(state, vine_slug).await?;
    info!("Post-build: {integrity}");

    // Final stats update
    {
        let conn = state.writer.lock().await;
        db::update_slug_stats(&conn, vine_slug)?;
    }

    info!("=== Vine build complete: '{vine_slug}' apex={apex_id} ===");
    Ok(apex_id)
}

// ══════════════════════════════════════════════════════════════════════════════
// PHASE 4: INTELLIGENCE PASSES
// ══════════════════════════════════════════════════════════════════════════════

/// Run all six intelligence passes on a built vine.
/// All outputs are contributions: annotations, FAQ entries, web edges.
pub async fn run_intelligence_passes(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
    l0_to_bunch: &HashMap<String, i64>,
    cancel: &CancellationToken,
) -> Result<()> {
    // Idempotency: clear all vine-intelligence contributions before re-run.
    // 11-B: Use INSERT OR REPLACE pattern via unique constraint.
    // Until the annotation schema gets a UNIQUE(slug, node_id, annotation_type, author) constraint,
    // we scope the cleanup to specific annotation types produced by vine-intelligence.
    // This is tightly scoped (not a blanket DELETE) and only affects machine-generated annotations.
    {
        let conn = state.writer.lock().await;
        conn.execute(
            "DELETE FROM pyramid_annotations WHERE slug = ?1 AND author = 'vine-intelligence' \
             AND annotation_type IN ('era', 'transition', 'health_check', 'directory', 'observation')",
            rusqlite::params![vine_slug],
        )?;
        info!("Cleared previous vine-intelligence annotations for '{vine_slug}'");
    }

    // 4a: ERA detection
    if !cancel.is_cancelled() {
        detect_vine_eras(state, vine_slug, bunches, l0_to_bunch, cancel).await?;
    }
    // 4b: Transition classification
    if !cancel.is_cancelled() {
        classify_vine_transitions(state, vine_slug, cancel).await?;
    }
    // 4c: Entity resolution
    if !cancel.is_cancelled() {
        resolve_vine_entities(state, vine_slug, bunches, cancel).await?;
    }
    // 4d: Decision tracking
    if !cancel.is_cancelled() {
        track_vine_decisions(state, vine_slug, bunches).await?;
    }
    // 4e: Thread continuity
    if !cancel.is_cancelled() {
        map_vine_thread_continuity(state, vine_slug, bunches).await?;
    }
    // 4f: Correction chains
    if !cancel.is_cancelled() {
        trace_vine_corrections(state, vine_slug, bunches, l0_to_bunch).await?;
    }

    info!("All intelligence passes complete for vine '{vine_slug}'");
    Ok(())
}

// ── 4a: ERA Detection ────────────────────────────────────────────────────────

/// Detect ERA boundaries using entity overlap sliding window + LLM phase classifier.
/// Produces annotations with annotation_type = Era on vine L1/L2 nodes.
async fn detect_vine_eras(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
    l0_to_bunch: &HashMap<String, i64>,
    cancel: &CancellationToken,
) -> Result<()> {
    if bunches.len() < 3 {
        info!("ERA detection: fewer than 3 bunches, skipping");
        return Ok(());
    }

    let llm_config = {
        let cfg = state.config.read().await;
        cfg.clone()
    };

    // Step 1: Entity overlap sliding window (mechanical)
    let window_size: usize = 5;
    let threshold: f64 = 0.3;
    let mut boundary_candidates: Vec<(usize, f64)> = Vec::new(); // (bunch_index, overlap_score)

    for i in window_size..bunches.len() {
        let prev_entities: HashSet<String> = bunches[i.saturating_sub(window_size)..i]
            .iter()
            .filter_map(|b| b.metadata.as_ref())
            .flat_map(|m| m.entities.iter().map(|e| e.to_lowercase()))
            .collect();

        let curr_entities: HashSet<String> = bunches[i..bunches.len().min(i + window_size)]
            .iter()
            .filter_map(|b| b.metadata.as_ref())
            .flat_map(|m| m.entities.iter().map(|e| e.to_lowercase()))
            .collect();

        if prev_entities.is_empty() && curr_entities.is_empty() {
            continue;
        }

        let intersection = prev_entities.intersection(&curr_entities).count() as f64;
        let union = prev_entities.union(&curr_entities).count() as f64;
        let overlap = if union > 0.0 {
            intersection / union
        } else {
            0.0
        };

        if overlap < threshold {
            boundary_candidates.push((i, overlap));
        }
    }

    // Step 2: Temporal gap reinforcement (3+ day gaps strengthen boundaries)
    for i in 1..bunches.len() {
        if let (Some(prev_ts), Some(curr_ts)) = (&bunches[i - 1].last_ts, &bunches[i].first_ts) {
            // Parse ISO date portions and compute day gap
            let prev_date = &prev_ts[..10.min(prev_ts.len())];
            let curr_date = &curr_ts[..10.min(curr_ts.len())];

            // Parse YYYY-MM-DD into days-since-epoch for gap calculation
            let day_gap = parse_date_gap(prev_date, curr_date);

            if day_gap >= 3 && !boundary_candidates.iter().any(|(idx, _)| *idx == i) {
                boundary_candidates.push((i, 0.2)); // Add as weak candidate for 3+ day gap
            }
        }
    }

    boundary_candidates.sort_by_key(|(idx, _)| *idx);
    boundary_candidates.dedup_by_key(|(idx, _)| *idx);

    // Step 3: LLM phase classifier for ambiguous boundaries
    let mut confirmed_boundaries: Vec<usize> = Vec::new();

    for (idx, overlap) in &boundary_candidates {
        if cancel.is_cancelled() {
            break;
        }

        // Strong signal (overlap < 0.15): auto-confirm
        if *overlap < 0.15 {
            confirmed_boundaries.push(*idx);
            continue;
        }

        // Ambiguous zone (0.15-0.3): ask LLM
        let prev_bunch = &bunches[idx.saturating_sub(1)];
        let curr_bunch = &bunches[*idx.min(&(bunches.len() - 1))];

        let prev_meta = prev_bunch.metadata.as_ref();
        let curr_meta = curr_bunch.metadata.as_ref();

        let user_prompt = format!(
            "## Session A (earlier)\nTopics: {:?}\nEntities: {:?}\n\n## Session B (later)\nTopics: {:?}\nEntities: {:?}",
            prev_meta.map(|m| &m.topics).unwrap_or(&Vec::new()),
            prev_meta.map(|m| &m.entities).unwrap_or(&Vec::new()),
            curr_meta.map(|m| &m.topics).unwrap_or(&Vec::new()),
            curr_meta.map(|m| &m.entities).unwrap_or(&Vec::new()),
        );

        match build::call_and_parse(
            &llm_config,
            super::vine_prompts::VINE_PHASE_CHECK_PROMPT,
            &user_prompt,
            &format!("vine-era-check-{idx}"),
        )
        .await
        {
            Ok(result) => {
                let same_phase = result
                    .get("same_phase")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if !same_phase {
                    confirmed_boundaries.push(*idx);
                }
            }
            Err(e) => {
                warn!("ERA phase check failed at boundary {idx}: {e}");
                // On LLM failure, include the boundary (bias toward detection)
                confirmed_boundaries.push(*idx);
            }
        }
    }

    confirmed_boundaries.sort();
    confirmed_boundaries.dedup();

    // Step 4: Build ERA annotations
    // ERAs are the segments between boundaries
    let mut era_starts = vec![0usize];
    era_starts.extend(confirmed_boundaries.iter());
    let mut era_ends: Vec<usize> = confirmed_boundaries.clone();
    era_ends.push(bunches.len());

    let conn = state.writer.lock().await;

    for (era_num, (start, end)) in era_starts.iter().zip(era_ends.iter()).enumerate() {
        let era_bunches = &bunches[*start..*end];
        if era_bunches.is_empty() {
            continue;
        }

        let all_topics: Vec<String> = era_bunches
            .iter()
            .filter_map(|b| b.metadata.as_ref())
            .flat_map(|m| m.topics.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let date_range = format!(
            "{} → {}",
            era_bunches
                .first()
                .and_then(|b| b.first_ts.as_deref())
                .unwrap_or("?"),
            era_bunches
                .last()
                .and_then(|b| b.last_ts.as_deref())
                .unwrap_or("?"),
        );

        let era_label = if all_topics.len() <= 3 {
            all_topics.join(", ")
        } else {
            format!(
                "{}, {} + {} more",
                all_topics[0],
                all_topics[1],
                all_topics.len() - 2
            )
        };

        // Find the vine L1 node that best covers this era's bunches
        let l1_nodes = db::get_nodes_at_depth(&conn, vine_slug, 1)?;
        let era_bunch_indices: Vec<i64> = era_bunches.iter().map(|b| b.bunch_index).collect();
        let target_node_id = find_best_l1_for_bunches(&l1_nodes, &era_bunch_indices, l0_to_bunch);

        let era_content = serde_json::json!({
            "era_number": era_num,
            "label": era_label,
            "date_range": date_range,
            "bunch_indices": era_bunches.iter().map(|b| b.bunch_index).collect::<Vec<_>>(),
            "bunch_count": era_bunches.len(),
            "dominant_topics": all_topics,
        });

        let annotation = PyramidAnnotation {
            id: 0,
            slug: vine_slug.to_string(),
            node_id: target_node_id,
            annotation_type: AnnotationType::Era,
            content: serde_json::to_string_pretty(&era_content)?,
            question_context: Some(format!("What was ERA {} about?", era_num)),
            author: "vine-intelligence".to_string(),
            created_at: String::new(),
        };

        db::save_annotation(&conn, &annotation)?;
    }

    info!(
        "ERA detection: {} boundaries → {} eras for '{vine_slug}'",
        confirmed_boundaries.len(),
        era_starts.len()
    );
    Ok(())
}

// ── 4b: Transition Classification ────────────────────────────────────────────

/// Classify transitions between adjacent ERAs.
/// Produces annotations with annotation_type = Transition.
async fn classify_vine_transitions(
    state: &PyramidState,
    vine_slug: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let llm_config = {
        let cfg = state.config.read().await;
        cfg.clone()
    };

    // Read existing ERA annotations
    let era_annotations = {
        let conn = state.reader.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at
             FROM pyramid_annotations WHERE slug = ?1 AND annotation_type = 'era'
             ORDER BY id ASC"
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| {
            Ok(PyramidAnnotation {
                id: row.get(0)?,
                slug: row.get(1)?,
                node_id: row.get(2)?,
                annotation_type: AnnotationType::Era,
                content: row.get(4)?,
                question_context: row.get(5)?,
                author: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.filter_map(|r| r.ok()).collect::<Vec<_>>()
    };

    if era_annotations.len() < 2 {
        info!("Transition classification: fewer than 2 ERAs, skipping");
        return Ok(());
    }

    let conn = state.writer.lock().await;

    for i in 0..era_annotations.len() - 1 {
        if cancel.is_cancelled() {
            break;
        }

        let era_a = &era_annotations[i];
        let era_b = &era_annotations[i + 1];

        let user_prompt = format!(
            "## ERA A (earlier)\n{}\n\n## ERA B (later)\n{}",
            era_a.content, era_b.content
        );

        match build::call_and_parse(
            &llm_config,
            super::vine_prompts::VINE_TRANSITION_PROMPT,
            &user_prompt,
            &format!("vine-transition-{i}"),
        )
        .await
        {
            Ok(result) => {
                let annotation = PyramidAnnotation {
                    id: 0,
                    slug: vine_slug.to_string(),
                    node_id: era_b.node_id.clone(), // Annotate on the receiving ERA's node
                    annotation_type: AnnotationType::Transition,
                    content: serde_json::to_string_pretty(&result)?,
                    question_context: Some(format!(
                        "What drove the transition from ERA {} to ERA {}?",
                        i,
                        i + 1
                    )),
                    author: "vine-intelligence".to_string(),
                    created_at: String::new(),
                };
                db::save_annotation(&conn, &annotation)?;
            }
            Err(e) => {
                warn!(
                    "Transition classification failed between ERA {i} and {}: {e}",
                    i + 1
                );
            }
        }
    }

    info!(
        "Transition classification: {} transitions for '{vine_slug}'",
        era_annotations.len() - 1
    );
    Ok(())
}

// ── 4c: Entity Resolution ────────────────────────────────────────────────────

/// Resolve entity name variants across bunches into canonical forms.
/// Produces FAQ entries with match_triggers for alias lookup.
async fn resolve_vine_entities(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
    cancel: &CancellationToken,
) -> Result<()> {
    // Step 1: Collect all entities across all bunches
    let mut all_entities: Vec<String> = bunches
        .iter()
        .filter_map(|b| b.metadata.as_ref())
        .flat_map(|m| m.entities.iter().cloned())
        .collect();
    all_entities.sort();
    all_entities.dedup();

    if all_entities.len() < 5 {
        info!("Entity resolution: fewer than 5 unique entities, skipping");
        return Ok(());
    }

    // Step 2: Mechanical pre-clustering by fuzzy match
    let mut clusters: Vec<Vec<String>> = Vec::new();
    let mut assigned: HashSet<usize> = HashSet::new();

    for i in 0..all_entities.len() {
        if assigned.contains(&i) {
            continue;
        }
        let mut cluster = vec![all_entities[i].clone()];
        assigned.insert(i);

        for j in (i + 1)..all_entities.len() {
            if assigned.contains(&j) {
                continue;
            }
            let a = all_entities[i].to_lowercase();
            let b = all_entities[j].to_lowercase();
            // Normalized Levenshtein or substring containment (min 4 chars to avoid false matches)
            let max_len = a.len().max(b.len());
            let normalized_lev = if max_len > 0 {
                levenshtein_distance(&a, &b) as f64 / max_len as f64
            } else {
                1.0
            };
            let substring_match =
                a.len() >= 4 && b.len() >= 4 && (a.contains(&b) || b.contains(&a));
            if substring_match || normalized_lev < 0.3 {
                cluster.push(all_entities[j].clone());
                assigned.insert(j);
            }
        }

        if cluster.len() > 1 {
            clusters.push(cluster);
        }
    }

    if clusters.is_empty() {
        info!("Entity resolution: no fuzzy-match clusters found");
        return Ok(());
    }

    let llm_config = {
        let cfg = state.config.read().await;
        cfg.clone()
    };

    // Step 3: LLM resolution
    let clusters_json = serde_json::to_string_pretty(&clusters)?;

    if cancel.is_cancelled() {
        return Ok(());
    }

    match build::call_and_parse(
        &llm_config,
        super::vine_prompts::VINE_ENTITY_RESOLUTION_PROMPT,
        &clusters_json,
        "vine-entity-resolution",
    )
    .await
    {
        Ok(result) => {
            let conn = state.writer.lock().await;

            if let Some(resolved) = result.get("resolved").and_then(|r| r.as_array()) {
                for entry in resolved {
                    let canonical = entry
                        .get("canonical")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let aliases: Vec<String> = entry
                        .get("aliases")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let description = entry
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let faq_id = format!(
                        "FAQ-vine-entity-{}",
                        canonical.to_lowercase().replace(' ', "-")
                    );
                    let faq = FaqNode {
                        id: faq_id,
                        slug: vine_slug.to_string(),
                        question: format!("What is {}?", canonical),
                        answer: description.to_string(),
                        related_node_ids: Vec::new(),
                        annotation_ids: Vec::new(),
                        hit_count: 0,
                        match_triggers: aliases,
                        created_at: String::new(),
                        updated_at: String::new(),
                    };
                    db::save_faq_node(&conn, &faq)?;
                }
                info!(
                    "Entity resolution: {} canonical entities for '{vine_slug}'",
                    resolved.len()
                );
            }
        }
        Err(e) => {
            warn!("Entity resolution LLM call failed: {e}");
        }
    }

    Ok(())
}

// ── 4d: Decision Tracking ────────────────────────────────────────────────────

/// Track decision evolution across bunches.
/// Produces FAQ entries with evolution chains.
async fn track_vine_decisions(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
) -> Result<()> {
    // Collect all decisions with temporal context
    let mut all_decisions: Vec<&VineDecision> = bunches
        .iter()
        .filter_map(|b| b.metadata.as_ref())
        .flat_map(|m| m.decisions.iter())
        .collect();

    if all_decisions.is_empty() {
        info!("Decision tracking: no decisions found");
        return Ok(());
    }

    // Sort by temporal order
    all_decisions.sort_by_key(|d| d.bunch_index);

    // Group decisions by topic similarity (simple keyword overlap)
    let mut decision_chains: Vec<Vec<&VineDecision>> = Vec::new();

    for decision in &all_decisions {
        let words: HashSet<String> = decision
            .decision
            .decided
            .to_lowercase()
            .split_whitespace()
            .filter(|w| w.len() > 3) // skip short words
            .map(String::from)
            .collect();

        let mut matched_chain = None;
        for (ci, chain) in decision_chains.iter().enumerate() {
            if let Some(last) = chain.last() {
                let chain_words: HashSet<String> = last
                    .decision
                    .decided
                    .to_lowercase()
                    .split_whitespace()
                    .filter(|w| w.len() > 3)
                    .map(String::from)
                    .collect();
                let overlap = words.intersection(&chain_words).count();
                if overlap >= 2 {
                    matched_chain = Some(ci);
                    break;
                }
            }
        }

        if let Some(ci) = matched_chain {
            decision_chains[ci].push(decision);
        } else {
            decision_chains.push(vec![decision]);
        }
    }

    // Create FAQ entries for chains with evolution (2+ decisions)
    let conn = state.writer.lock().await;
    let mut chain_count = 0;

    for chain in &decision_chains {
        if chain.len() < 2 {
            continue; // Single decisions don't need evolution tracking
        }

        let evolution: Vec<serde_json::Value> = chain
            .iter()
            .map(|d| {
                serde_json::json!({
                    "decided": d.decision.decided,
                    "why": d.decision.why,
                    "bunch_index": d.bunch_index,
                    "bunch_ts": d.bunch_ts,
                })
            })
            .collect();

        let first = chain.first().unwrap();
        let last = chain.last().unwrap();
        let faq_id = format!("FAQ-vine-decision-{chain_count}");

        let faq = FaqNode {
            id: faq_id,
            slug: vine_slug.to_string(),
            question: format!(
                "How did the decision about '{}' evolve?",
                &last.decision.decided[..last.decision.decided.len().min(60)]
            ),
            answer: serde_json::to_string_pretty(&serde_json::json!({
                "current_decision": last.decision.decided,
                "evolution_chain": evolution,
                "first_proposed_in_bunch": first.bunch_index,
                "last_modified_in_bunch": last.bunch_index,
            }))?,
            related_node_ids: Vec::new(),
            annotation_ids: Vec::new(),
            hit_count: 0,
            match_triggers: chain.iter().map(|d| d.decision.decided.clone()).collect(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_faq_node(&conn, &faq)?;
        chain_count += 1;
    }

    info!(
        "Decision tracking: {} evolution chains from {} decisions for '{vine_slug}'",
        chain_count,
        all_decisions.len()
    );
    Ok(())
}

// ── 4e: Thread Continuity ────────────────────────────────────────────────────

/// Map cross-bunch thread continuity using topic name matching.
/// Creates pyramid_threads entries and web edges between them.
async fn map_vine_thread_continuity(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
) -> Result<()> {
    // Build topic → bunch_indices mapping
    let mut topic_to_bunches: HashMap<String, Vec<i64>> = HashMap::new();

    for bunch in bunches {
        if let Some(meta) = &bunch.metadata {
            for topic in &meta.topics {
                let normalized = topic.to_lowercase();
                topic_to_bunches
                    .entry(normalized)
                    .or_default()
                    .push(bunch.bunch_index);
            }
        }
    }

    // Filter to topics that span multiple bunches (those are cross-bunch threads)
    let cross_bunch_topics: Vec<(String, Vec<i64>)> = topic_to_bunches
        .into_iter()
        .filter(|(_, indices)| indices.len() > 1)
        .collect();

    if cross_bunch_topics.is_empty() {
        info!("Thread continuity: no cross-bunch topics found");
        return Ok(());
    }

    let conn = state.writer.lock().await;
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Fetch L1 nodes once (not per-topic)
    let l1_nodes = db::get_nodes_at_depth(&conn, vine_slug, 1)?;

    // Create a pyramid_thread for each cross-bunch topic
    let mut thread_count = 0;
    for (topic_name, _bunch_indices) in &cross_bunch_topics {
        let thread_id = format!("vine-thread-{}", topic_name.replace(' ', "-"));

        // Find the L1 node that best matches this topic
        let canonical_id = find_best_l1_for_topic(&l1_nodes, topic_name);

        let thread = PyramidThread {
            slug: vine_slug.to_string(),
            thread_id: thread_id.clone(),
            thread_name: topic_name.clone(),
            current_canonical_id: canonical_id,
            depth: 1,
            delta_count: 0,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        db::save_thread(&conn, &thread)?;
        thread_count += 1;
    }

    // Create web edges between threads that share bunches
    // topic_names used for future cross-referencing
    let _topic_names: Vec<&String> = cross_bunch_topics.iter().map(|(name, _)| name).collect();
    let mut edge_count = 0;

    for i in 0..cross_bunch_topics.len() {
        for j in (i + 1)..cross_bunch_topics.len() {
            let (name_a, indices_a) = &cross_bunch_topics[i];
            let (name_b, indices_b) = &cross_bunch_topics[j];

            // Check if they share any bunches
            let shared: Vec<&i64> = indices_a
                .iter()
                .filter(|idx| indices_b.contains(idx))
                .collect();

            if !shared.is_empty() {
                let thread_a = format!("vine-thread-{}", name_a.replace(' ', "-"));
                let thread_b = format!("vine-thread-{}", name_b.replace(' ', "-"));

                // Web edges require thread_a_id < thread_b_id (CHECK constraint)
                let (a_id, b_id) = if thread_a < thread_b {
                    (thread_a, thread_b)
                } else {
                    (thread_b, thread_a)
                };

                let edge = WebEdge {
                    id: 0,
                    slug: vine_slug.to_string(),
                    thread_a_id: a_id,
                    thread_b_id: b_id,
                    relationship: format!("Co-occur in {} sessions", shared.len()),
                    relevance: shared.len() as f64 / bunches.len().max(1) as f64,
                    delta_count: 0,
                    build_id: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                db::save_web_edge(&conn, &edge)?;
                edge_count += 1;
            }
        }
    }

    info!(
        "Thread continuity: {} threads, {} edges for '{vine_slug}'",
        thread_count, edge_count
    );
    Ok(())
}

// ── 4f: Correction Chains ────────────────────────────────────────────────────

/// Trace correction chains across bunches.
/// When a correction in bunch N fixes something from an earlier bunch, create an annotation linking them.
async fn trace_vine_corrections(
    state: &PyramidState,
    vine_slug: &str,
    bunches: &[VineBunch],
    l0_to_bunch: &HashMap<String, i64>,
) -> Result<()> {
    // Collect all corrections with temporal context
    let mut all_corrections: Vec<&VineCorrection> = bunches
        .iter()
        .filter_map(|b| b.metadata.as_ref())
        .flat_map(|m| m.corrections.iter())
        .collect();

    if all_corrections.is_empty() {
        info!("Correction chains: no corrections found");
        return Ok(());
    }

    all_corrections.sort_by_key(|c| c.bunch_index);

    // Match corrections across bunches: if correction.right in bunch N matches
    // something stated (as correction.wrong or as a decision) in an earlier bunch M
    let conn = state.writer.lock().await;
    let l1_nodes = db::get_nodes_at_depth(&conn, vine_slug, 1)?;

    let mut chain_count = 0;

    for i in 0..all_corrections.len() {
        for j in (i + 1)..all_corrections.len() {
            let earlier = &all_corrections[i];
            let later = &all_corrections[j];

            // Check if the later correction references the same subject
            // Require exact match on wrong field, or high token overlap (not substring)
            let earlier_subject = earlier.correction.wrong.to_lowercase();
            let later_subject = later.correction.wrong.to_lowercase();

            let subjects_match = earlier_subject == later_subject;
            // Token overlap: at least 2 shared significant words
            let earlier_words: HashSet<&str> = earlier
                .correction
                .right
                .split_whitespace()
                .filter(|w| w.len() > 3)
                .collect();
            let later_words: HashSet<&str> = later
                .correction
                .wrong
                .split_whitespace()
                .filter(|w| w.len() > 3)
                .collect();
            let token_overlap = earlier_words.intersection(&later_words).count() >= 2;

            if subjects_match || token_overlap {
                // Target the L1 node covering the later correction's bunch
                let target_node =
                    find_best_l1_for_bunches(&l1_nodes, &[later.bunch_index], l0_to_bunch);
                let chain_content = serde_json::json!({
                    "chain": [
                        {
                            "bunch_index": earlier.bunch_index,
                            "wrong": earlier.correction.wrong,
                            "right": earlier.correction.right,
                            "who": earlier.correction.who,
                        },
                        {
                            "bunch_index": later.bunch_index,
                            "wrong": later.correction.wrong,
                            "right": later.correction.right,
                            "who": later.correction.who,
                        }
                    ],
                    "current_truth": later.correction.right,
                });

                let annotation = PyramidAnnotation {
                    id: 0,
                    slug: vine_slug.to_string(),
                    node_id: target_node.clone(),
                    annotation_type: AnnotationType::Correction,
                    content: serde_json::to_string_pretty(&chain_content)?,
                    question_context: Some(format!(
                        "How did understanding of '{}' change?",
                        earlier.correction.wrong
                    )),
                    author: "vine-intelligence".to_string(),
                    created_at: String::new(),
                };
                db::save_annotation(&conn, &annotation)?;
                chain_count += 1;
            }
        }
    }

    info!(
        "Correction chains: {} cross-bunch chains for '{vine_slug}'",
        chain_count
    );
    Ok(())
}

// ── Utility: Date Gap ─────────────────────────────────────────────────────────

/// Parse two "YYYY-MM-DD" date strings and return the absolute day gap.
/// Returns 0 on parse failure.
fn parse_date_gap(date_a: &str, date_b: &str) -> i64 {
    fn days_from_date(s: &str) -> Option<i64> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 3 {
            return None;
        }
        let y: i64 = parts[0].parse().ok()?;
        let m: i64 = parts[1].parse().ok()?;
        let d: i64 = parts[2].parse().ok()?;
        // Approximate days (good enough for gap detection, not calendar-precise)
        Some(y * 365 + m * 30 + d)
    }
    match (days_from_date(date_a), days_from_date(date_b)) {
        (Some(a), Some(b)) => (b - a).abs(),
        _ => 0,
    }
}

// ── Utility: L1 Node Matching ─────────────────────────────────────────────────

/// Find the L1 node that best matches a set of bunch indices.
/// Uses the explicit l0_to_bunch mapping (built during assemble_vine_l0) to avoid
/// the broken /3 approximation that fails when bunches have variable L0 counts.
fn find_best_l1_for_bunches(
    l1_nodes: &[PyramidNode],
    target_bunch_indices: &[i64],
    l0_to_bunch: &HashMap<String, i64>,
) -> String {
    let target_set: HashSet<i64> = target_bunch_indices.iter().copied().collect();
    let mut best_id = l1_nodes
        .first()
        .map(|n| n.id.clone())
        .unwrap_or_else(|| "L1-000".to_string());
    let mut best_overlap = 0;

    for node in l1_nodes {
        let mut overlap = 0;
        for child_id in &node.children {
            if let Some(&bunch_idx) = l0_to_bunch.get(child_id) {
                if target_set.contains(&bunch_idx) {
                    overlap += 1;
                }
            }
        }
        if overlap > best_overlap {
            best_overlap = overlap;
            best_id = node.id.clone();
        }
    }

    best_id
}

/// Find the L1 node that best matches a topic name.
fn find_best_l1_for_topic(l1_nodes: &[PyramidNode], topic_name: &str) -> String {
    let topic_lower = topic_name.to_lowercase();

    for node in l1_nodes {
        // Check if any of this node's topics match
        for topic in &node.topics {
            if topic.name.to_lowercase().contains(&topic_lower)
                || topic_lower.contains(&topic.name.to_lowercase())
            {
                return node.id.clone();
            }
        }
        // Check headline
        if node.headline.to_lowercase().contains(&topic_lower) {
            return node.id.clone();
        }
    }

    l1_nodes
        .first()
        .map(|n| n.id.clone())
        .unwrap_or_else(|| "L1-000".to_string())
}

// ── Utility: Levenshtein Distance ────────────────────────────────────────────

/// Simple Levenshtein distance for entity fuzzy matching.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

// ══════════════════════════════════════════════════════════════════════════════
// PHASE 5: LIVE VINE MODE
// ══════════════════════════════════════════════════════════════════════════════

// ── 5a: Direct Notification After Bunch Change ───────────────────────────────

/// Check if a bunch's pyramid has changed since the last vine snapshot.
/// Compares current apex + penultimate nodes against stored metadata.
/// Returns true if the vine L0 nodes need updating.
pub async fn check_bunch_staleness(
    state: &PyramidState,
    _vine_slug: &str,
    bunch: &VineBunch,
) -> Result<bool> {
    let stored_meta = match &bunch.metadata {
        Some(m) => m,
        None => return Ok(true), // No metadata = definitely needs update
    };

    let conn = state.reader.lock().await;

    // Read current apex
    let apex = match query::get_apex(&conn, &bunch.bunch_slug)? {
        Some(a) => a,
        None => return Ok(false), // No apex = bunch not built yet, nothing to compare
    };

    // Read current penultimate layer
    let pen_depth = if apex.depth > 0 { apex.depth - 1 } else { 0 };
    let pen_nodes = if pen_depth > 0 && pen_depth < apex.depth {
        db::get_nodes_at_depth(&conn, &bunch.bunch_slug, pen_depth)?
    } else {
        Vec::new()
    };

    // Compare: apex distilled text
    if Some(apex.id.as_str()) != bunch.apex_node_id.as_deref() {
        return Ok(true); // Apex node ID changed (supersession)
    }

    // Compare: penultimate summaries
    let current_summaries: Vec<String> = pen_nodes.iter().map(|n| n.distilled.clone()).collect();
    let stored_summaries: Vec<String> = stored_meta.penultimate_summaries.clone();
    if current_summaries.len() != stored_summaries.len() {
        return Ok(true);
    }

    // Even if topics didn't change, check entity count as a proxy for content change
    let current_entities: HashSet<String> = pen_nodes
        .iter()
        .flat_map(|n| n.topics.iter())
        .flat_map(|t| t.entities.iter().cloned())
        .collect();
    let stored_entities: HashSet<String> = stored_meta.entities.iter().cloned().collect();
    if current_entities != stored_entities {
        return Ok(true);
    }

    Ok(false)
}

/// Notify the vine that a bunch has changed. Updates vine L0 nodes and marks vine L1+ as stale.
pub async fn notify_vine_of_bunch_change(
    state: &PyramidState,
    vine_slug: &str,
    bunch: &mut VineBunch,
) -> Result<bool> {
    let is_stale = check_bunch_staleness(state, vine_slug, bunch).await?;
    if !is_stale {
        return Ok(false);
    }

    // WS-CONCURRENCY (§15.16 race 2): composition delta while a child
    // rebuild lands. We mutate BOTH the bunch (child) and the vine (parent)
    // below — supersede L0 nodes, update vine_bunches, reassemble. Acquire
    // both write locks in the single process-wide deadlock-free order
    // (child → parent) via write_child_then_parent. Guards drop at end of
    // scope (end of function) releasing the locks.
    let (_bunch_guard, _vine_guard) = super::lock_manager::LockManager::global()
        .write_child_then_parent(&bunch.bunch_slug, vine_slug)
        .await;

    info!(
        "Bunch '{}' has changed — updating vine L0 nodes",
        bunch.bunch_slug
    );

    // Re-extract metadata from bunch's current pyramid
    let (apex, pen_nodes) = {
        let conn = state.reader.lock().await;
        let apex = query::get_apex(&conn, &bunch.bunch_slug)?
            .ok_or_else(|| anyhow!("No apex for bunch '{}'", bunch.bunch_slug))?;
        let pen_depth = if apex.depth > 0 { apex.depth - 1 } else { 0 };
        let pen = if pen_depth > 0 && pen_depth < apex.depth {
            db::get_nodes_at_depth(&conn, &bunch.bunch_slug, pen_depth)?
        } else {
            Vec::new()
        };
        (apex, pen)
    };

    let pen_ids: Vec<String> = pen_nodes.iter().map(|n| n.id.clone()).collect();
    let new_metadata = extract_bunch_metadata(
        &apex,
        &pen_nodes,
        bunch.bunch_index,
        bunch.first_ts.as_deref().unwrap_or(""),
    );

    // Update vine_bunches record
    {
        let conn = state.writer.lock().await;
        update_vine_bunch_built(
            &conn,
            vine_slug,
            &bunch.bunch_slug,
            &apex.id,
            &pen_ids,
            bunch.chunk_count.unwrap_or(0),
            &new_metadata,
        )?;
    }

    bunch.apex_node_id = Some(apex.id);
    bunch.penultimate_node_ids = pen_ids;
    bunch.metadata = Some(new_metadata);

    // Step: Supersede existing vine L0 nodes for this bunch and reassemble
    {
        let conn = state.writer.lock().await;

        // Supersede L0 nodes whose headline starts with "Session {bunch_index}:" or "Session {bunch_index} /"
        let prefix_colon = format!("Session {}:", bunch.bunch_index);
        let prefix_slash = format!("Session {} /", bunch.bunch_index);
        let reassembly_build_id = format!("vine-reassembly-{}", uuid::Uuid::new_v4());
        db::supersede_nodes_by_headline_pattern(
            &conn,
            vine_slug,
            0,
            &format!("{}%", prefix_colon),
            &format!("{}%", prefix_slash),
            &reassembly_build_id,
        )?;

        // Re-read the apex from the bunch pyramid (already extracted above, reuse it)
        let apex_node = query::get_apex(&conn, &bunch.bunch_slug)?.ok_or_else(|| {
            anyhow!(
                "No apex for bunch '{}' during L0 reassembly",
                bunch.bunch_slug
            )
        })?;

        // Determine the next available chunk_index for vine L0 nodes
        let max_chunk: i64 = conn.query_row(
            "SELECT COALESCE(MAX(chunk_index), -1) FROM live_pyramid_nodes WHERE slug = ?1 AND depth = 0",
            rusqlite::params![vine_slug],
            |row| row.get(0),
        )?;
        let mut global_index = max_chunk + 1;

        let metadata = bunch
            .metadata
            .as_ref()
            .ok_or_else(|| anyhow!("Bunch '{}' has no metadata after update", bunch.bunch_slug))?;

        // Create vine L0 node for the apex (mirrors assemble_vine_l0 logic)
        let topic_list = metadata.topics.join(", ");
        let date_range = format!(
            "{} → {}",
            bunch.first_ts.as_deref().unwrap_or("?"),
            bunch.last_ts.as_deref().unwrap_or("?"),
        );
        let apex_content = format!(
            "## Session [{}]: {}\nDate: {}\nMessages: {}\nTopics: {}\n\n### Summary\n{}",
            bunch.bunch_index,
            apex_node.headline,
            date_range,
            bunch.message_count.unwrap_or(0),
            topic_list,
            apex_node.distilled,
        );

        let apex_l0_id = format!("L0-{:03}", global_index);
        let apex_l0_node = PyramidNode {
            id: apex_l0_id.clone(),
            slug: vine_slug.to_string(),
            depth: 0,
            chunk_index: Some(global_index),
            headline: format!("Session {}: {}", bunch.bunch_index, apex_node.headline),
            distilled: apex_content,
            topics: apex_node.topics.clone(),
            corrections: apex_node.corrections.clone(),
            decisions: apex_node.decisions.clone(),
            terms: apex_node.terms.clone(),
            dead_ends: Vec::new(),
            self_prompt: apex_node.self_prompt.clone(),
            children: Vec::new(),
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(),
            narrative: apex_node.narrative.clone(),
            entities: apex_node.entities.clone(),
            key_quotes: apex_node.key_quotes.clone(),
            transitions: apex_node.transitions.clone(),
            time_range: apex_node.time_range.clone(),
            weight: apex_node.weight,
            ..Default::default()
        };
        db::save_node(
            &conn,
            &apex_l0_node,
            Some(&serde_json::to_string(&apex_node.topics)?),
        )?;
        global_index += 1;

        // Create vine L0 nodes for penultimate layer
        for pen_node_id in &bunch.penultimate_node_ids {
            let pen_node = db::get_node(&conn, &bunch.bunch_slug, pen_node_id)?;
            if let Some(pn) = pen_node {
                let pen_l0_id = format!("L0-{:03}", global_index);
                let pen_content = format!(
                    "## Session [{}] Thread: {}\nDate: {}\n\n{}",
                    bunch.bunch_index, pn.headline, date_range, pn.distilled,
                );

                let pen_l0_node = PyramidNode {
                    id: pen_l0_id.clone(),
                    slug: vine_slug.to_string(),
                    depth: 0,
                    chunk_index: Some(global_index),
                    headline: format!("Session {} / {}", bunch.bunch_index, pn.headline),
                    distilled: pen_content,
                    topics: pn.topics.clone(),
                    corrections: pn.corrections.clone(),
                    decisions: pn.decisions.clone(),
                    terms: pn.terms.clone(),
                    dead_ends: Vec::new(),
                    self_prompt: String::new(),
                    children: Vec::new(),
                    parent_id: None,
                    superseded_by: None,
                    build_id: None,
                    created_at: String::new(),
                    narrative: pn.narrative.clone(),
                    entities: pn.entities.clone(),
                    key_quotes: pn.key_quotes.clone(),
                    transitions: pn.transitions.clone(),
                    time_range: pn.time_range.clone(),
                    weight: pn.weight,
                    ..Default::default()
                };
                db::save_node(
                    &conn,
                    &pen_l0_node,
                    Some(&serde_json::to_string(&pn.topics)?),
                )?;
                global_index += 1;
            }
        }

        info!(
            "Reassembled vine L0 nodes for bunch {} (vine '{}')",
            bunch.bunch_index, vine_slug
        );
    }

    // Rebuild L1+ from the updated L0s
    let cancel = CancellationToken::new();
    force_rebuild_vine_upper(state, vine_slug, &cancel).await?;

    info!("Bunch '{}' vine notification complete", bunch.bunch_slug);
    Ok(true)
}

// ── 5b: JSONL Mtime Polling Watcher ──────────────────────────────────────────

/// Watcher that polls a JSONL directory for changes and triggers vine updates.
pub struct VineJSONLWatcher {
    pub vine_slug: String,
    pub jsonl_dirs: Vec<PathBuf>,
    pub known_files: HashMap<PathBuf, (u64, std::time::SystemTime)>, // path → (size, mtime)
    pub debounce_seconds: u64,
    pub poll_interval_seconds: u64,
    pub pending: HashMap<PathBuf, Instant>, // path → last_change_detected
}

impl VineJSONLWatcher {
    pub fn new(vine_slug: &str, jsonl_dirs: Vec<PathBuf>, debounce_seconds: u64) -> Self {
        Self {
            vine_slug: vine_slug.to_string(),
            jsonl_dirs,
            known_files: HashMap::new(),
            debounce_seconds,
            poll_interval_seconds: 10,
            pending: HashMap::new(),
        }
    }

    /// Poll all directories for changed/new JSONL files.
    /// Returns list of paths that have changed and passed debounce.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut triggered = Vec::new();
        let now = Instant::now();

        for jsonl_dir in &self.jsonl_dirs {
            // Scan directory
            let entries = match std::fs::read_dir(jsonl_dir) {
                Ok(e) => e,
                Err(e) => {
                    warn!(
                        "VineJSONLWatcher: failed to read {}: {e}",
                        jsonl_dir.display()
                    );
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().map_or(true, |ext| ext != "jsonl") {
                    continue;
                }

                let metadata = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                let size = metadata.len();
                let mtime = metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                if let Some(&(old_size, old_mtime)) = self.known_files.get(&path) {
                    if size != old_size || mtime != old_mtime {
                        // File changed — start or reset debounce
                        self.pending.insert(path.clone(), now);
                        self.known_files.insert(path, (size, mtime));
                    }
                } else {
                    // New file
                    self.pending.insert(path.clone(), now);
                    self.known_files.insert(path, (size, mtime));
                }
            }
        } // end for jsonl_dir

        // Check debounce timers
        let debounce_duration = std::time::Duration::from_secs(self.debounce_seconds);
        let expired: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, &detected_at)| now.duration_since(detected_at) >= debounce_duration)
            .map(|(path, _)| path.clone())
            .collect();

        for path in expired {
            self.pending.remove(&path);
            triggered.push(path);
        }

        triggered
    }

    /// Run the watcher loop: poll on interval, build new bunches, rebuild changed bunches.
    /// Acquires the writer lock for all rebuild operations to prevent concurrent mutations.
    pub async fn run(
        mut self,
        state: Arc<PyramidState>,
        vine_slug: String,
        _llm: LlmConfig,
        cancel: CancellationToken,
    ) {
        let rebuild_lock = Arc::new(Mutex::new(()));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("VineJSONLWatcher: cancelled for vine '{vine_slug}'");
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.poll_interval_seconds)) => {
                    let triggered = self.poll();
                    if triggered.is_empty() {
                        continue;
                    }

                    // Determine which triggered paths are new vs changed by checking existing bunches
                    let existing_bunches = {
                        let conn = state.reader.lock().await;
                        match get_vine_bunches(&conn, &vine_slug) {
                            Ok(b) => b,
                            Err(e) => {
                                error!("VineJSONLWatcher: failed to get bunches: {e}");
                                continue;
                            }
                        }
                    };

                    let known_paths: HashSet<String> = existing_bunches.iter()
                        .map(|b| b.jsonl_path.clone())
                        .collect();

                    for path in triggered {
                        let path_str = path.to_string_lossy().to_string();
                        let _lock = rebuild_lock.lock().await;

                        if known_paths.contains(&path_str) {
                            // Changed bunch — find it, rebuild, and notify vine
                            info!("VineJSONLWatcher: bunch changed at {}", path.display());
                            let mut bunch = match existing_bunches.iter()
                                .find(|b| b.jsonl_path == path_str)
                                .cloned()
                            {
                                Some(b) => b,
                                None => continue,
                            };

                            // Rebuild the bunch pyramid
                            let discovery = match scan_jsonl_metadata(&path) {
                                Ok(Some(d)) => d,
                                Ok(None) => continue,
                                Err(e) => {
                                    error!("VineJSONLWatcher: scan failed for {}: {e}", path.display());
                                    continue;
                                }
                            };
                            if let Err(e) = build_bunch(&state, &vine_slug, &discovery, bunch.bunch_index, "deep", &cancel).await {
                                error!("VineJSONLWatcher: rebuild bunch failed: {e}");
                                continue;
                            }

                            // Notify vine of the change (updates L0 + rebuilds L1+)
                            if let Err(e) = notify_vine_of_bunch_change(&state, &vine_slug, &mut bunch).await {
                                error!("VineJSONLWatcher: vine notification failed: {e}");
                            }
                        } else {
                            // New bunch — discover, build, and update vine
                            info!("VineJSONLWatcher: new bunch at {}", path.display());
                            let discovery = match scan_jsonl_metadata(&path) {
                                Ok(Some(d)) => d,
                                Ok(None) => continue,
                                Err(e) => {
                                    error!("VineJSONLWatcher: scan failed for {}: {e}", path.display());
                                    continue;
                                }
                            };

                            if discovery.message_count < 3 {
                                info!("VineJSONLWatcher: skipping {} (only {} messages)", path.display(), discovery.message_count);
                                continue;
                            }

                            let next_index = existing_bunches.len() as i64;
                            match build_bunch(&state, &vine_slug, &discovery, next_index, "deep", &cancel).await {
                                Ok(_vine_bunch) => {
                                    info!("VineJSONLWatcher: built new bunch {next_index} for vine '{vine_slug}'");
                                    // Trigger L1+ rebuild to incorporate the new bunch
                                    if let Err(e) = force_rebuild_vine_upper(&state, &vine_slug, &cancel).await {
                                        error!("VineJSONLWatcher: vine upper rebuild failed: {e}");
                                    }
                                }
                                Err(e) => {
                                    error!("VineJSONLWatcher: build_bunch failed: {e}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── 5f: Force L2+ Rebuild ────────────────────────────────────────────────────

/// Force rebuild vine L2+ layers from L1.
/// Clears L2+ nodes and pipeline steps, then rebuilds and re-runs relevant intelligence passes.
pub async fn force_rebuild_vine_upper(
    state: &PyramidState,
    vine_slug: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    info!("Force rebuilding vine L2+ for '{vine_slug}'...");

    // Validate that L1 nodes exist before attempting L2+ rebuild
    {
        let conn = state.reader.lock().await;
        let l1_count = db::count_nodes_at_depth(&conn, vine_slug, 1)?;
        if l1_count == 0 {
            anyhow::bail!(
                "Cannot rebuild L2+: no L1 nodes exist for vine '{}'",
                vine_slug
            );
        }
    }

    // Step 1-2: Supersede L2+ nodes and scope pipeline steps
    {
        let conn = state.writer.lock().await;
        let rebuild_build_id = format!("vine-rebuild-{}", uuid::Uuid::new_v4());
        let nodes_superseded = db::supersede_nodes_above(&conn, vine_slug, 1, &rebuild_build_id)?;
        // 11-A: Scope step deletion by build_id (None = legacy steps without build_id)
        let steps_deleted = db::delete_steps_above_depth(&conn, vine_slug, 1, None)?;
        info!("Superseded {nodes_superseded} nodes and cleared {steps_deleted} steps above L1");

        // Clear stale parent_ids on live L1 nodes left behind after L2+ supersession
        conn.execute(
            "UPDATE pyramid_nodes SET parent_id = NULL WHERE slug = ?1 AND depth = 1 AND superseded_by IS NULL",
            rusqlite::params![vine_slug],
        )?;
    }

    // Step 3: Rebuild from L1 up
    let (apex_id, failures) = match build_vine_upper(state, vine_slug, cancel).await {
        Ok(result) => result,
        Err(e) => {
            warn!("Force rebuild failed after L2+ deletion: {e}. Vine has L0+L1 only. Re-run to retry.");
            return Ok(format!("PARTIAL: L2+ deleted but rebuild failed: {e}"));
        }
    };
    if failures > 0 {
        warn!("Force rebuild had {failures} synthesis failure(s)");
    }

    // Step 4: Directory wiring on new sub-apex layer
    wire_sub_apex_directory(state, vine_slug).await?;

    // Step 5: Re-run ERA detection and transition classification
    // (only these two — entity resolution, decisions, threads, corrections are unaffected by L2+ changes)
    let bunches = {
        let conn = state.reader.lock().await;
        get_vine_bunches(&conn, vine_slug)?
    };
    let l0_to_bunch = rebuild_l0_to_bunch_map(state, vine_slug, &bunches).await?;

    // 11-B: Scoped cleanup of machine-generated annotations before regeneration.
    // Tightly scoped to specific annotation_type + author — not a blanket DELETE.
    {
        let conn = state.writer.lock().await;
        conn.execute(
            "DELETE FROM pyramid_annotations WHERE slug = ?1 AND author = 'vine-intelligence' AND annotation_type IN ('era', 'transition')",
            rusqlite::params![vine_slug],
        )?;
    }

    detect_vine_eras(state, vine_slug, &bunches, &l0_to_bunch, cancel).await?;
    classify_vine_transitions(state, vine_slug, cancel).await?;

    // Step 6: Integrity check
    let integrity = run_integrity_check(state, vine_slug).await?;
    info!("Post-rebuild integrity: {}", integrity);

    // Update stats
    {
        let conn = state.writer.lock().await;
        db::update_slug_stats(&conn, vine_slug)?;
    }

    info!("Force rebuild complete for '{vine_slug}'. Apex: {apex_id}");
    Ok(apex_id)
}

// ── 5g: Post-Build Integrity Check ───────────────────────────────────────────

/// Run structural integrity check on the vine pyramid.
/// Returns a summary string and stores results as a HealthCheck annotation.
pub async fn run_integrity_check(state: &PyramidState, vine_slug: &str) -> Result<String> {
    let conn = state.reader.lock().await;

    let mut barren_interior: Vec<String> = Vec::new();
    let true_orphans: Vec<String>;
    let mut broken_parents: Vec<String> = Vec::new();
    let unclustered_l0: Vec<String>;

    // Get all live nodes
    let all_node_ids: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT id FROM live_pyramid_nodes WHERE slug = ?1")?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // Check for barren interior nodes (depth > 0 with no children)
    {
        let mut stmt = conn.prepare(
            "SELECT id, depth, children FROM live_pyramid_nodes WHERE slug = ?1 AND depth > 0",
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            let (id, _depth, children_json) = row;
            let children: Vec<String> = serde_json::from_str(&children_json).unwrap_or_default();
            if children.is_empty() {
                barren_interior.push(id);
            }
        }
    }

    // Check for true orphans: non-apex, non-L0 nodes with no parent
    {
        let mut stmt = conn.prepare(
            "SELECT id FROM live_pyramid_nodes WHERE slug = ?1 AND depth > 0 AND depth < (SELECT MAX(depth) FROM live_pyramid_nodes WHERE slug = ?1) AND (parent_id IS NULL OR parent_id = '')"
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| row.get::<_, String>(0))?;
        true_orphans = rows.filter_map(|r| r.ok()).collect();
    }

    // Check for broken parent references
    {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id FROM live_pyramid_nodes WHERE slug = ?1 AND parent_id IS NOT NULL AND parent_id != ''"
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows.flatten() {
            let (id, parent_id) = row;
            if !all_node_ids.contains(&parent_id) {
                broken_parents.push(format!("{} → {}", id, parent_id));
            }
        }
    }

    // Check for unclustered L0 nodes (no parent)
    {
        let mut stmt = conn.prepare(
            "SELECT id FROM live_pyramid_nodes WHERE slug = ?1 AND depth = 0 AND (parent_id IS NULL OR parent_id = '')"
        )?;
        let rows = stmt.query_map(rusqlite::params![vine_slug], |row| row.get::<_, String>(0))?;
        unclustered_l0 = rows.filter_map(|r| r.ok()).collect();
    }

    // Reachability check: BFS from apex, find unreachable nodes
    let mut unreachable: Vec<String> = Vec::new();
    {
        let apex_opt = query::get_apex(&conn, vine_slug)?;
        if let Some(apex) = apex_opt {
            let mut reached: HashSet<String> = HashSet::new();
            let mut bfs_queue: Vec<String> = vec![apex.id.clone()];
            while let Some(nid) = bfs_queue.pop() {
                if !reached.insert(nid.clone()) {
                    continue;
                }
                if let Ok(Some(node)) = db::get_node(&conn, vine_slug, &nid) {
                    bfs_queue.extend(node.children.iter().cloned());
                }
            }
            for nid in &all_node_ids {
                if !reached.contains(nid) {
                    unreachable.push(nid.clone());
                }
            }
        }
    }

    let summary = format!(
        "Integrity: {} barren interior, {} true orphans, {} broken parent refs, {} unclustered L0, {} unreachable",
        barren_interior.len(), true_orphans.len(), broken_parents.len(), unclustered_l0.len(), unreachable.len()
    );

    drop(conn); // Release reader before acquiring writer

    // Store as HealthCheck annotation on the highest node available
    let target_node = {
        let conn = state.reader.lock().await;
        query::get_apex(&conn, vine_slug)?
            .map(|n| n.id)
            .or_else(|| {
                // Fallback: highest depth node
                db::get_nodes_at_depth(&conn, vine_slug, 1)
                    .ok()
                    .and_then(|nodes| nodes.first().map(|n| n.id.clone()))
            })
    };

    if let Some(node_id) = target_node {
        let health_content = serde_json::json!({
            "barren_interior_nodes": barren_interior,
            "true_orphan_nodes": true_orphans,
            "broken_parent_refs": broken_parents,
            "unclustered_l0": unclustered_l0,
            "unreachable_nodes": unreachable,
            "total_issues": barren_interior.len() + true_orphans.len() + broken_parents.len() + unclustered_l0.len() + unreachable.len(),
        });

        let conn = state.writer.lock().await;

        // 11-B: Scoped cleanup of previous health_check annotation before regeneration
        conn.execute(
            "DELETE FROM pyramid_annotations WHERE slug = ?1 AND annotation_type = 'health_check' AND author = 'vine-intelligence'",
            rusqlite::params![vine_slug],
        )?;

        let annotation = PyramidAnnotation {
            id: 0,
            slug: vine_slug.to_string(),
            node_id,
            annotation_type: AnnotationType::HealthCheck,
            content: serde_json::to_string_pretty(&health_content)?,
            question_context: Some("What is the vine's structural health?".to_string()),
            author: "vine-intelligence".to_string(),
            created_at: String::new(),
        };
        db::save_annotation(&conn, &annotation)?;
    }

    info!("{summary} for '{vine_slug}'");
    Ok(summary)
}

// ── 5h: Sub-Apex Directory Wiring ────────────────────────────────────────────

/// Wire sub-apex nodes with directory annotations listing all L1 clusters they transitively cover.
/// Enables quick navigation: sub-apex → any L1 cluster without drilling through intermediate layers.
pub async fn wire_sub_apex_directory(state: &PyramidState, vine_slug: &str) -> Result<i64> {
    let conn = state.reader.lock().await;

    // Find apex
    let apex = match query::get_apex(&conn, vine_slug)? {
        Some(a) => a,
        None => {
            info!("No apex found for directory wiring in '{vine_slug}'");
            return Ok(0);
        }
    };

    if apex.depth < 2 {
        info!(
            "Vine too shallow (depth {}) for directory wiring",
            apex.depth
        );
        return Ok(0);
    }

    // Get sub-apex nodes (one level below apex)
    let sub_apex_depth = apex.depth - 1;
    let sub_apex_nodes = db::get_nodes_at_depth(&conn, vine_slug, sub_apex_depth)?;

    // Get all L1 nodes for reference
    let l1_nodes = db::get_nodes_at_depth(&conn, vine_slug, 1)?;
    let l1_map: HashMap<String, &PyramidNode> =
        l1_nodes.iter().map(|n| (n.id.clone(), n)).collect();

    // For each sub-apex node, walk its subtree down to L1
    let mut directory_count: i64 = 0;
    let mut directory_annotations: Vec<(String, String)> = Vec::new(); // (node_id, content_json)

    for sub_apex in &sub_apex_nodes {
        let mut l1_refs = Vec::new();
        let mut queue: Vec<String> = sub_apex.children.clone();

        while let Some(child_id) = queue.pop() {
            if let Some(l1_node) = l1_map.get(&child_id) {
                // This is an L1 node — add to directory
                let topic_names: Vec<String> =
                    l1_node.topics.iter().map(|t| t.name.clone()).collect();
                l1_refs.push(serde_json::json!({
                    "id": l1_node.id,
                    "headline": l1_node.headline,
                    "topics": topic_names,
                }));
            } else {
                // Not L1 — get its children and keep walking down
                match db::get_node(&conn, vine_slug, &child_id) {
                    Ok(Some(node)) => {
                        queue.extend(node.children.iter().cloned());
                    }
                    Ok(None) => {
                        warn!("Directory wiring: intermediate node '{}' not found, some L1 refs may be missing", child_id);
                    }
                    Err(_e) => {
                        warn!("Directory wiring: intermediate node '{}' not found, some L1 refs may be missing", child_id);
                    }
                }
            }
        }

        if !l1_refs.is_empty() {
            let content = serde_json::json!({
                "l1_refs": l1_refs,
                "sub_apex_headline": sub_apex.headline,
            });
            directory_annotations
                .push((sub_apex.id.clone(), serde_json::to_string_pretty(&content)?));
            directory_count += 1;
        }
    }

    drop(conn); // Release reader before writer

    // Write directory annotations
    {
        let conn = state.writer.lock().await;

        // 11-B: Scoped cleanup of old directory annotations before regeneration
        conn.execute(
            "DELETE FROM pyramid_annotations WHERE slug = ?1 AND annotation_type = 'directory' AND author = 'vine-intelligence'",
            rusqlite::params![vine_slug],
        )?;

        for (node_id, content) in &directory_annotations {
            let annotation = PyramidAnnotation {
                id: 0,
                slug: vine_slug.to_string(),
                node_id: node_id.clone(),
                annotation_type: AnnotationType::Directory,
                content: content.clone(),
                question_context: Some(format!("What L1 clusters does this sub-apex node cover?")),
                author: "vine-intelligence".to_string(),
                created_at: String::new(),
            };
            db::save_annotation(&conn, &annotation)?;
        }
    }

    info!("Directory wiring: {directory_count} sub-apex nodes wired for '{vine_slug}'");
    Ok(directory_count)
}
