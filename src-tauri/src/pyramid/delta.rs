// pyramid/delta.rs — Delta chain engine for progressive crystallization
//
// Functions:
//   match_or_create_thread — route new L1 content to an existing or new thread
//   create_delta           — create an incremental diff against current understanding
//   rewrite_distillation   — rewrite cumulative distillation incorporating a new delta
//   collapse_thread        — collapse accumulated deltas into a new canonical node
//   propagate_staleness    — propagate change signals upward through the pyramid
//   check_collapse_needed  — determine if a thread needs collapsing

use std::collections::HashSet;
use std::time::Instant;
use rusqlite::Connection;
use tokio::sync::Mutex;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::pyramid::db;
use crate::pyramid::types::*;
use crate::pyramid::llm;
use crate::pyramid::config_helper::config_for_model;

// ── Constants ────────────────────────────────────────────────────────────────

const COLLAPSE_THRESHOLD: i64 = 50;
const DISTILLATION_TOKEN_BUDGET: usize = 800;
const DISTILLATION_EARLY_COLLAPSE: usize = 1200;
/// Reserved for future debounce logic in the warm pass.
#[allow(dead_code)]
const STALENESS_DEBOUNCE_SECS: u64 = 10;
const MAX_PROPAGATION_DEPTH: i64 = 10;
const SELF_CHECK_WINDOW: i64 = 5;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Rough token estimate: chars / 3.2 (conservative, matching conversation.rs).
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() as f64 / 3.2) as usize
}

/// Generate a timestamp string in the format used by the pyramid DB.
fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── match_or_create_thread ───────────────────────────────────────────────────

/// Determines which thread a new L1 node belongs to, creating a new thread if needed.
///
/// If no threads exist for this slug, creates one from the L1 content.
/// Otherwise, asks the LLM which thread the content belongs to.
/// Returns the thread_id (existing or newly created).
pub async fn match_or_create_thread(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    l1_content: &str,
    l1_node_id: &str,
    api_key: &str,
    model: &str,
) -> anyhow::Result<String> {
    let threads = {
        let conn = reader.lock().await;
        db::get_threads(&conn, slug)?
    };

    if threads.is_empty() {
        // First thread — create directly from L1 content
        let thread_id = format!("thread-{}", &Uuid::new_v4().to_string()[..8]);
        let thread_name = truncate_for_name(l1_content, 60);
        let thread = PyramidThread {
            slug: slug.to_string(),
            thread_id: thread_id.clone(),
            thread_name,
            current_canonical_id: l1_node_id.to_string(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        let conn = writer.lock().await;
        db::save_thread(&conn, &thread)?;
        info!("[delta] created first thread '{}' for slug '{}'", thread.thread_name, slug);
        return Ok(thread_id);
    }

    // Build thread listing for the LLM
    let thread_listing: String = threads
        .iter()
        .map(|t| format!("- {} (id: {})", t.thread_name, t.thread_id))
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = "You are a thread-matching assistant. Output JSON only.";
    let user_prompt = format!(
        r#"EXISTING THREADS:
{thread_listing}

NEW CONTENT:
{l1_content}

Which existing thread does this content belong to? If it introduces a genuinely new topic not covered by any existing thread, say "NEW".

Output JSON:
{{"match": "thread-id" | "NEW", "thread_name": "name for new thread if NEW"}}"#
    );

    let cfg = config_for_model(api_key, model);
    let raw = llm::call_model(&cfg, system_prompt, &user_prompt, 0.2, 200).await?;
    let parsed = llm::extract_json(&raw)?;

    let match_val = parsed.get("match").and_then(|v| v.as_str()).unwrap_or("NEW");

    if match_val != "NEW" {
        // Verify the thread_id actually exists
        if threads.iter().any(|t| t.thread_id == match_val) {
            info!("[delta] matched content to thread '{}'", match_val);
            return Ok(match_val.to_string());
        }
        warn!("[delta] LLM returned thread_id '{}' which doesn't exist, creating new", match_val);
    }

    // Create a new thread
    let thread_id = format!("thread-{}", &Uuid::new_v4().to_string()[..8]);
    let thread_name = parsed
        .get("thread_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| truncate_for_name(l1_content, 60));

    let thread = PyramidThread {
        slug: slug.to_string(),
        thread_id: thread_id.clone(),
        thread_name: thread_name.clone(),
        current_canonical_id: l1_node_id.to_string(),
        depth: 1,
        delta_count: 0,
        created_at: now_ts(),
        updated_at: now_ts(),
    };
    let conn = writer.lock().await;
    db::save_thread(&conn, &thread)?;
    info!("[delta] created new thread '{}' ({})", thread_name, thread_id);
    Ok(thread_id)
}

/// Truncate text to make a thread name, cutting at a word boundary.
fn truncate_for_name(text: &str, max_len: usize) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() <= max_len {
        return cleaned;
    }
    let truncated = crate::utils::safe_slice_end(&cleaned, max_len);
    match truncated.rfind(' ') {
        Some(pos) => format!("{}...", &truncated[..pos]),
        None => format!("{}...", truncated),
    }
}

// ── create_delta ─────────────────────────────────────────────────────────────

/// Creates a delta against the current understanding of a thread.
///
/// Steps:
/// 1. Load the thread's current canonical node (distilled text)
/// 2. Load the cumulative distillation
/// 3. Load the last SELF_CHECK_WINDOW deltas
/// 4. Call LLM with delta prompt
/// 5. Parse response
/// 6. Save delta with transaction-wrapped sequence assignment
/// 7. Call rewrite_distillation
/// 8. Check if collapse needed
/// 9. Propagate staleness upward
pub async fn create_delta(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    thread_id: &str,
    new_content: &str,
    source_node_id: Option<&str>,
    api_key: &str,
    model: &str,
) -> anyhow::Result<Delta> {
    // 1. Load canonical node
    let (canonical_distilled, canonical_id) = {
        let conn = reader.lock().await;
        let thread = db::get_thread(&conn, slug, thread_id)?
            .ok_or_else(|| anyhow::anyhow!("Thread '{}' not found in slug '{}'", thread_id, slug))?;
        let node = db::get_node(&conn, slug, &thread.current_canonical_id)?
            .ok_or_else(|| anyhow::anyhow!("Canonical node '{}' not found", thread.current_canonical_id))?;
        (node.distilled, thread.current_canonical_id)
    };

    // 2. Load cumulative distillation
    let distillation_content = {
        let conn = reader.lock().await;
        db::get_distillation(&conn, slug, thread_id)?
            .map(|d| d.content)
            .unwrap_or_default()
    };

    // 3. Load last N deltas for continuity check
    let recent_deltas = {
        let conn = reader.lock().await;
        let all = db::get_deltas(&conn, slug, thread_id, None)?;
        let start = if all.len() > SELF_CHECK_WINDOW as usize {
            all.len() - SELF_CHECK_WINDOW as usize
        } else {
            0
        };
        all[start..].to_vec()
    };

    let recent_deltas_text = if recent_deltas.is_empty() {
        "No previous deltas.".to_string()
    } else {
        recent_deltas
            .iter()
            .map(|d| format!("delta-{} ({}): {}", d.sequence, d.relevance.as_str(), d.content))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let distillation_display = if distillation_content.is_empty() {
        "No changes since last collapse.".to_string()
    } else {
        distillation_content.clone()
    };

    // 4. Call LLM
    let system_prompt = "You are analyzing what changed relative to existing understanding. Output JSON only.";
    let user_prompt = format!(
        r#"You are analyzing what changed relative to existing understanding.

CURRENT UNDERSTANDING (canonical + distillation):
{canonical_distilled}
{distillation_display}

LAST {n} DELTAS (for continuity):
{recent_deltas_text}

NEW INFORMATION:
{new_content}

What changed? Be specific: what's new, what's corrected, what's confirmed.
Self-assess relevance:
- low: minor detail, typo fix, confirmation of known info
- medium: meaningful new information or clarification
- high: significant change that affects understanding
- critical: contradicts existing understanding or introduces major new concept

If the recent deltas seem to have drifted from the canonical understanding, set flag to describe the drift.

Output JSON only:
{{"content": "description of what changed", "relevance": "low|medium|high|critical", "flag": null}}"#,
        n = recent_deltas.len(),
    );

    let cfg = config_for_model(api_key, model);
    let raw = llm::call_model(&cfg, system_prompt, &user_prompt, 0.3, 500).await?;
    let parsed = llm::extract_json(&raw)?;

    // 5. Parse response
    let delta_content = parsed
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("Unable to parse delta content")
        .to_string();

    let relevance_str = parsed
        .get("relevance")
        .and_then(|v| v.as_str())
        .unwrap_or("medium");
    let relevance = DeltaRelevance::from_str(relevance_str);

    let flag = parsed
        .get("flag")
        .and_then(|v| {
            if v.is_null() { None } else { v.as_str().map(|s| s.to_string()) }
        });

    // 6. Save delta with transaction-wrapped sequence
    let delta = {
        let conn = writer.lock().await;
        let tx = conn.unchecked_transaction()?;

        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM pyramid_deltas WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, thread_id],
            |r| r.get(0),
        )?;

        let delta = Delta {
            id: 0, // Will be set by DB
            slug: slug.to_string(),
            thread_id: thread_id.to_string(),
            sequence: next_seq,
            content: delta_content,
            relevance,
            source_node_id: source_node_id.map(|s| s.to_string()),
            flag: flag.clone(),
            created_at: now_ts(),
        };

        let row_id = db::save_delta(&tx, &delta)?;

        // Update thread delta_count
        tx.execute(
            "UPDATE pyramid_threads SET delta_count = delta_count + 1, updated_at = ?1 WHERE slug = ?2 AND thread_id = ?3",
            rusqlite::params![now_ts(), slug, thread_id],
        )?;

        tx.commit()?;

        Delta { id: row_id, ..delta }
    };

    if let Some(ref f) = flag {
        warn!("[delta] drift flag on thread '{}': {}", thread_id, f);
    }

    info!(
        "[delta] created delta seq={} relevance={} for thread '{}'",
        delta.sequence,
        delta.relevance.as_str(),
        thread_id
    );

    // 7. Rewrite distillation
    let web_edge_notes = rewrite_distillation(reader, writer, slug, thread_id, &delta, api_key, model).await?;

    // 7b. Process web edge notes (cross-thread connections)
    if let Some(notes) = web_edge_notes {
        if !notes.is_empty() {
            if let Err(e) = crate::pyramid::webbing::process_web_edge_notes(
                reader, writer, slug, thread_id, &notes,
            ).await {
                warn!("[delta] web edge processing failed: {}", e);
            }
        }
    }

    // 8. Check if collapse needed
    let needs_collapse = {
        let conn = reader.lock().await;
        check_collapse_needed(&conn, slug, thread_id)?
    };

    if needs_collapse {
        info!("[delta] collapse threshold reached for thread '{}', collapse should be triggered", thread_id);
        // Note: actual collapse is triggered by the caller or warm pass with a frontier model.
        // We log the signal here rather than auto-collapsing, because collapse uses a different
        // (more expensive) model that the caller provides.
    }

    // 9. Propagate staleness upward
    {
        let conn = reader.lock().await;
        let mut visited = HashSet::new();
        match propagate_staleness(&conn, slug, &canonical_id, 1, &mut visited, MAX_PROPAGATION_DEPTH) {
            Ok(affected) => {
                if !affected.is_empty() {
                    info!("[delta] staleness propagated to {} nodes: {:?}", affected.len(), affected);
                    // Write confirmed_stale WAL entries for each affected node
                    let w = writer.lock().await;
                    let now = chrono::Utc::now().to_rfc3339();
                    for node_id in &affected {
                        let _ = w.execute(
                            "INSERT INTO pyramid_pending_mutations (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed) VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, 0, ?5, 0)",
                            rusqlite::params![slug, 1, node_id, format!("Staleness propagated from {}", canonical_id), now],
                        );
                    }
                    info!("[delta] Wrote {} confirmed_stale WAL entries for affected nodes", affected.len());
                }
            }
            Err(e) => {
                warn!("[delta] staleness propagation failed: {}", e);
            }
        }
    }

    Ok(delta)
}

// ── rewrite_distillation ─────────────────────────────────────────────────────

/// Rewrites the cumulative distillation incorporating a new delta.
/// Returns web edge notes (cross-thread connections detected) for the webbing system.
pub async fn rewrite_distillation(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    thread_id: &str,
    delta: &Delta,
    api_key: &str,
    model: &str,
) -> anyhow::Result<Option<Vec<WebEdgeNote>>> {
    // 1. Load current distillation
    let current_distillation = {
        let conn = reader.lock().await;
        db::get_distillation(&conn, slug, thread_id)?
            .map(|d| d.content)
            .unwrap_or_default()
    };

    // 2. Load all thread names for cross-thread detection
    let thread_names_list = {
        let conn = reader.lock().await;
        let threads = db::get_threads(&conn, slug)?;
        threads
            .iter()
            .filter(|t| t.thread_id != thread_id) // Exclude current thread
            .map(|t| format!("- {} (id: {})", t.thread_name, t.thread_id))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let distillation_display = if current_distillation.is_empty() {
        "Empty -- this is the first delta since collapse.".to_string()
    } else {
        current_distillation.clone()
    };

    // 3. Call LLM
    let system_prompt = "You are maintaining a cumulative understanding of what has changed since the last collapse. Output JSON only.";
    let user_prompt = format!(
        r#"You are maintaining a cumulative understanding of what has changed since the last collapse.

CURRENT DISTILLATION:
{distillation_display}

NEW DELTA:
{content} (relevance: {relevance})

EXISTING THREADS (for cross-thread connection detection):
{thread_names}

Rewrite the distillation incorporating this delta. Rules:
- Keep the distillation focused and bounded (target under {budget} tokens)
- Prioritize high-relevance changes over low-relevance ones
- If low-relevance details must be dropped, note "see delta-{{N}} for details"
- Note any connections to other threads that changed

Output JSON only:
{{
  "distillation": "the rewritten cumulative understanding",
  "web_edge_notes": [{{"thread_id": "id of connected thread", "relationship": "how it connects"}}],
  "drift_detected": false
}}"#,
        content = delta.content,
        relevance = delta.relevance.as_str(),
        thread_names = if thread_names_list.is_empty() { "No other threads.".to_string() } else { thread_names_list },
        budget = DISTILLATION_TOKEN_BUDGET,
    );

    let cfg = config_for_model(api_key, model);
    let raw = llm::call_model(&cfg, system_prompt, &user_prompt, 0.2, 1000).await?;
    let parsed = llm::extract_json(&raw)?;

    // 4. Parse response
    let new_distillation = parsed
        .get("distillation")
        .and_then(|v| v.as_str())
        .unwrap_or(&current_distillation)
        .to_string();

    let drift_detected = parsed
        .get("drift_detected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if drift_detected {
        warn!("[delta] drift detected in distillation for thread '{}'", thread_id);
    }

    // Parse web edge notes
    let web_edge_notes: Option<Vec<WebEdgeNote>> = parsed
        .get("web_edge_notes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let tid = item.get("thread_id")?.as_str()?;
                    let rel = item.get("relationship")?.as_str()?;
                    Some(WebEdgeNote {
                        thread_id: tid.to_string(),
                        relationship: rel.to_string(),
                    })
                })
                .collect()
        });

    // 5. Save updated distillation
    let delta_count = {
        let conn = reader.lock().await;
        db::get_distillation(&conn, slug, thread_id)?
            .map(|d| d.delta_count)
            .unwrap_or(0)
    };

    {
        let conn = writer.lock().await;
        let distillation = CumulativeDistillation {
            slug: slug.to_string(),
            thread_id: thread_id.to_string(),
            content: new_distillation.clone(),
            delta_count: delta_count + 1,
            updated_at: now_ts(),
        };
        db::save_distillation(&conn, &distillation)?;
    }

    info!(
        "[delta] distillation rewritten for thread '{}' ({} tokens est.)",
        thread_id,
        estimate_tokens(&new_distillation)
    );

    // 6. Check early collapse condition
    if estimate_tokens(&new_distillation) > DISTILLATION_EARLY_COLLAPSE {
        warn!(
            "[delta] distillation exceeds early collapse threshold ({} > {}), collapse recommended",
            estimate_tokens(&new_distillation),
            DISTILLATION_EARLY_COLLAPSE
        );
    }

    Ok(web_edge_notes)
}

// ── collapse_thread ──────────────────────────────────────────────────────────

/// Collapses accumulated deltas into a new canonical understanding.
///
/// Uses a frontier model for the collapse (higher quality than delta creation).
/// Returns the new canonical node ID.
pub async fn collapse_thread(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    thread_id: &str,
    api_key: &str,
    collapse_model: &str,
) -> anyhow::Result<String> {
    let start = Instant::now();

    // 1. Load current canonical node
    let (canonical_node, thread) = {
        let conn = reader.lock().await;
        let thread = db::get_thread(&conn, slug, thread_id)?
            .ok_or_else(|| anyhow::anyhow!("Thread '{}' not found", thread_id))?;
        let node = db::get_node(&conn, slug, &thread.current_canonical_id)?
            .ok_or_else(|| anyhow::anyhow!("Canonical node '{}' not found", thread.current_canonical_id))?;
        (node, thread)
    };

    // 2. Load distillation
    let distillation_content = {
        let conn = reader.lock().await;
        db::get_distillation(&conn, slug, thread_id)?
            .map(|d| d.content)
            .unwrap_or_else(|| "No distillation available.".to_string())
    };

    // 3. Load ALL deltas since last collapse
    let all_deltas = {
        let conn = reader.lock().await;
        db::get_deltas(&conn, slug, thread_id, None)?
    };

    let delta_count = all_deltas.len() as i64;
    if delta_count == 0 {
        info!("[delta] no deltas to collapse for thread '{}'", thread_id);
        return Ok(thread.current_canonical_id);
    }

    let deltas_text = all_deltas
        .iter()
        .map(|d| format!("delta-{} ({}): {}", d.sequence, d.relevance.as_str(), d.content))
        .collect::<Vec<_>>()
        .join("\n");

    // 4. Build canonical JSON for the prompt
    let canonical_json = serde_json::json!({
        "distilled": canonical_node.distilled,
        "topics": canonical_node.topics,
        "corrections": canonical_node.corrections,
        "decisions": canonical_node.decisions,
        "terms": canonical_node.terms,
        "dead_ends": canonical_node.dead_ends,
        "self_prompt": canonical_node.self_prompt,
    });

    let system_prompt = "You are collapsing accumulated changes into a new canonical understanding. Output valid JSON only.";
    let user_prompt = format!(
        r#"You are collapsing accumulated changes into a new canonical understanding.

PREVIOUS CANONICAL:
{canonical}

CUMULATIVE DISTILLATION (what changed since canonical):
{distillation}

ALL DELTAS ({n} total):
{deltas}

Produce the NEW canonical understanding that incorporates everything.
Deduplicate corrections and decisions -- keep only the latest version if corrected multiple times.

Output valid JSON matching this schema:
{{
  "distilled": "Complete understanding incorporating all changes",
  "topics": [{{"name": "topic", "current": "state", "entities": ["entity"], "corrections": [{{"wrong": "was", "right": "is", "who": "delta-chain-collapse"}}], "decisions": [{{"decided": "what", "why": "reason", "rejected": "alternatives"}}]}}],
  "corrections": [{{"wrong": "was", "right": "is", "who": "delta-chain-collapse"}}],
  "decisions": [{{"decided": "what", "why": "reason", "rejected": "alternatives"}}],
  "terms": [{{"term": "word", "definition": "meaning"}}],
  "dead_ends": ["abandoned approaches"],
  "self_prompt": "What should I investigate next?"
}}"#,
        canonical = serde_json::to_string_pretty(&canonical_json).unwrap_or_default(),
        distillation = distillation_content,
        n = delta_count,
        deltas = deltas_text,
    );

    let cfg = config_for_model(api_key, collapse_model);
    let raw = llm::call_model(&cfg, system_prompt, &user_prompt, 0.2, 4000).await?;
    let parsed = llm::extract_json(&raw)?;

    // 5. Parse response into PyramidNode fields
    let distilled = parsed
        .get("distilled")
        .and_then(|v| v.as_str())
        .unwrap_or(&canonical_node.distilled)
        .to_string();

    let topics: Vec<Topic> = parsed
        .get("topics")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| canonical_node.topics.clone());

    let corrections: Vec<Correction> = parsed
        .get("corrections")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| canonical_node.corrections.clone());

    let decisions: Vec<Decision> = parsed
        .get("decisions")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| canonical_node.decisions.clone());

    let terms: Vec<Term> = parsed
        .get("terms")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| canonical_node.terms.clone());

    let dead_ends: Vec<String> = parsed
        .get("dead_ends")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| canonical_node.dead_ends.clone());

    let self_prompt = parsed
        .get("self_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or(&canonical_node.self_prompt)
        .to_string();

    // 6. Create new node with versioned ID
    let version = extract_version(&canonical_node.id) + 1;
    let base_id = strip_version(&canonical_node.id);
    let new_node_id = format!("{}-v{}", base_id, version);

    let new_node = PyramidNode {
        id: new_node_id.clone(),
        slug: slug.to_string(),
        depth: canonical_node.depth,
        chunk_index: canonical_node.chunk_index,
        distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends,
        self_prompt,
        children: canonical_node.children.clone(),
        parent_id: canonical_node.parent_id.clone(),
        superseded_by: None,
        created_at: now_ts(),
    };

    let elapsed = start.elapsed().as_secs_f64();

    // 7-11. Save everything in a single transaction
    {
        let conn = writer.lock().await;
        let tx = conn.unchecked_transaction()?;

        // 7. Save new node
        db::save_node(&tx, &new_node, None)?;

        // 8. Mark old canonical as superseded
        tx.execute(
            "UPDATE pyramid_nodes SET superseded_by = ?1 WHERE slug = ?2 AND id = ?3",
            rusqlite::params![new_node_id, slug, canonical_node.id],
        )?;

        // 8b. Re-parent children: any node whose parent was the old canonical now points to the new one
        tx.execute(
            "UPDATE pyramid_nodes SET parent_id = ?1 WHERE slug = ?2 AND parent_id = ?3",
            rusqlite::params![new_node_id, slug, canonical_node.id],
        )?;

        // 8c. Update parent's children array: swap old canonical ID for new canonical ID
        if let Some(ref parent_id) = canonical_node.parent_id {
            if !parent_id.is_empty() {
                let parent_children_json: String = tx
                    .query_row(
                        "SELECT children FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                        rusqlite::params![slug, parent_id],
                        |row| row.get(0),
                    )
                    .unwrap_or_default();

                let mut children: Vec<String> = serde_json::from_str(&parent_children_json)
                    .unwrap_or_default();
                for child in children.iter_mut() {
                    if *child == canonical_node.id {
                        *child = new_node_id.clone();
                    }
                }
                let updated_children_json = serde_json::to_string(&children)
                    .unwrap_or_else(|_| "[]".to_string());

                tx.execute(
                    "UPDATE pyramid_nodes SET children = ?1 WHERE slug = ?2 AND id = ?3",
                    rusqlite::params![updated_children_json, slug, parent_id],
                )?;
            }
        }

        // 9. Update thread
        tx.execute(
            "UPDATE pyramid_threads SET current_canonical_id = ?1, delta_count = 0, updated_at = ?2 WHERE slug = ?3 AND thread_id = ?4",
            rusqlite::params![new_node_id, now_ts(), slug, thread_id],
        )?;

        // 10. Clear distillation
        let empty_distillation = CumulativeDistillation {
            slug: slug.to_string(),
            thread_id: thread_id.to_string(),
            content: String::new(),
            delta_count: 0,
            updated_at: now_ts(),
        };
        db::save_distillation(&tx, &empty_distillation)?;

        // 11. Log collapse event
        let event = CollapseEvent {
            id: 0,
            slug: slug.to_string(),
            thread_id: thread_id.to_string(),
            old_canonical_id: canonical_node.id.clone(),
            new_canonical_id: new_node_id.clone(),
            deltas_absorbed: delta_count,
            model_used: collapse_model.to_string(),
            elapsed_seconds: elapsed,
            created_at: now_ts(),
        };
        db::save_collapse_event(&tx, &event)?;

        // Delete only the absorbed deltas (those with sequence <= max absorbed)
        let max_absorbed_seq = all_deltas.last().map(|d| d.sequence).unwrap_or(0);
        tx.execute(
            "DELETE FROM pyramid_deltas WHERE slug = ?1 AND thread_id = ?2 AND sequence <= ?3",
            rusqlite::params![slug, thread_id, max_absorbed_seq],
        )?;

        tx.commit()?;
    }

    info!(
        "[delta] collapsed thread '{}': {} -> {} ({} deltas, {:.1}s)",
        thread_id, canonical_node.id, new_node_id, delta_count, elapsed
    );

    // 12. Propagate staleness upward after commit
    {
        let conn = reader.lock().await;
        let mut visited = HashSet::new();
        match propagate_staleness(&conn, slug, &new_node_id, 1, &mut visited, MAX_PROPAGATION_DEPTH) {
            Ok(affected) => {
                if !affected.is_empty() {
                    info!("[delta] collapse staleness propagated to {} nodes: {:?}", affected.len(), affected);
                    // Write confirmed_stale WAL entries for each affected node
                    let w = writer.lock().await;
                    let now = chrono::Utc::now().to_rfc3339();
                    for node_id in &affected {
                        let _ = w.execute(
                            "INSERT INTO pyramid_pending_mutations (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed) VALUES (?1, ?2, 'confirmed_stale', ?3, ?4, 0, ?5, 0)",
                            rusqlite::params![slug, 1, node_id, format!("Staleness propagated from {}", new_node_id), now],
                        );
                    }
                    info!("[delta] Wrote {} confirmed_stale WAL entries for affected nodes", affected.len());
                }
            }
            Err(e) => {
                warn!("[delta] collapse staleness propagation failed: {}", e);
            }
        }
    }

    Ok(new_node_id)
}

/// Extract version number from a node ID like "node-abc-v3" -> 3.
fn extract_version(node_id: &str) -> i64 {
    if let Some(pos) = node_id.rfind("-v") {
        node_id[pos + 2..].parse::<i64>().unwrap_or(0)
    } else {
        0
    }
}

/// Strip the version suffix from a node ID: "node-abc-v3" -> "node-abc".
fn strip_version(node_id: &str) -> &str {
    if let Some(pos) = node_id.rfind("-v") {
        if node_id[pos + 2..].parse::<i64>().is_ok() {
            return &node_id[..pos];
        }
    }
    node_id
}

// ── propagate_staleness ──────────────────────────────────────────────────────

/// Propagates change signals upward through the pyramid.
///
/// Returns a list of affected (stale) node IDs — the parent chain above the
/// changed node. The caller decides what to do with these (log, write WAL
/// entries, etc.).
pub fn propagate_staleness(
    conn: &Connection,
    slug: &str,
    changed_node_id: &str,
    changed_depth: i64,
    visited: &mut HashSet<String>,
    max_depth: i64,
) -> anyhow::Result<Vec<String>> {
    let mut affected = Vec::new();

    if changed_depth >= max_depth || visited.contains(changed_node_id) {
        return Ok(affected);
    }
    visited.insert(changed_node_id.to_string());

    // Find the parent node
    let parent_id: Option<String> = conn
        .query_row(
            "SELECT parent_id FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, changed_node_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(Some(pid)) = parent_id.map(|p| if p.is_empty() { None } else { Some(p) }) {
        // Check if parent is already superseded
        let superseded: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![slug, pid],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if superseded.is_none() {
            affected.push(pid.clone());
            // Recurse upward
            let mut upstream = propagate_staleness(conn, slug, &pid, changed_depth + 1, visited, max_depth)?;
            affected.append(&mut upstream);
        }
    }

    Ok(affected)
}

// ── check_collapse_needed ────────────────────────────────────────────────────

/// Determines if a thread needs collapsing based on delta count and distillation size.
pub fn check_collapse_needed(
    conn: &Connection,
    slug: &str,
    thread_id: &str,
) -> anyhow::Result<bool> {
    // Check delta_count from thread
    let delta_count: i64 = conn
        .query_row(
            "SELECT delta_count FROM pyramid_threads WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, thread_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if delta_count >= COLLAPSE_THRESHOLD {
        return Ok(true);
    }

    // Check distillation token count
    let distillation_content: String = conn
        .query_row(
            "SELECT content FROM pyramid_distillations WHERE slug = ?1 AND thread_id = ?2",
            rusqlite::params![slug, thread_id],
            |r| r.get(0),
        )
        .unwrap_or_default();

    if estimate_tokens(&distillation_content) > DISTILLATION_EARLY_COLLAPSE {
        return Ok(true);
    }

    Ok(false)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_for_name() {
        assert_eq!(truncate_for_name("short", 60), "short");
        let long = "This is a very long piece of content that should get truncated at a word boundary";
        let result = truncate_for_name(long, 40);
        assert!(result.len() <= 43); // 40 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_extract_version() {
        assert_eq!(extract_version("node-abc-v3"), 3);
        assert_eq!(extract_version("node-abc-v0"), 0);
        assert_eq!(extract_version("node-abc"), 0);
        assert_eq!(extract_version("node-abc-v12"), 12);
    }

    #[test]
    fn test_strip_version() {
        assert_eq!(strip_version("node-abc-v3"), "node-abc");
        assert_eq!(strip_version("node-abc"), "node-abc");
        assert_eq!(strip_version("node-abc-v12"), "node-abc");
    }

    #[test]
    fn test_estimate_tokens() {
        let text = "This is roughly 40 characters long text.";
        assert_eq!(estimate_tokens(text), (text.len() as f64 / 3.2) as usize);
    }

    #[test]
    fn test_check_collapse_threshold() {
        // check_collapse_needed uses a raw Connection, testable with in-memory DB
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        // Create a thread
        let thread = PyramidThread {
            slug: "test".into(),
            thread_id: "t1".into(),
            thread_name: "Test Thread".into(),
            current_canonical_id: "node-1".into(),
            depth: 1,
            delta_count: 0,
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        db::save_thread(&conn, &thread).unwrap();

        // Should not need collapse initially
        assert!(!check_collapse_needed(&conn, "test", "t1").unwrap());

        // Bump delta_count past threshold
        conn.execute(
            "UPDATE pyramid_threads SET delta_count = ?1 WHERE slug = 'test' AND thread_id = 't1'",
            rusqlite::params![COLLAPSE_THRESHOLD],
        ).unwrap();
        assert!(check_collapse_needed(&conn, "test", "t1").unwrap());
    }
}
