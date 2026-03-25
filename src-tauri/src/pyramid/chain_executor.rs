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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::build::{
    child_payload_json, send_save_node, send_save_step, send_update_parent, WriteOp,
};
use super::chain_dispatch::{self, build_node_from_output, generate_node_id, normalize_node_id};
use super::chain_engine::{ChainDefinition, ChainStep};
use super::chain_resolve::{resolve_prompt_template, ChainContext};
use super::db;
use super::stale_helpers_upper::resolve_stale_target_for_node;
use super::types::{BuildProgress, PyramidNode, WebEdge};
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
            if let Some(inner) = other
                .strip_prefix("retry(")
                .and_then(|s| s.strip_suffix(')'))
            {
                if let Ok(n) = inner.parse::<u32>() {
                    return ErrorStrategy::Retry(n.min(10).max(1));
                }
            }
            ErrorStrategy::Retry(2)
        }
    }
}

fn resolve_error_strategy(
    step: &ChainStep,
    defaults: &super::chain_engine::ChainDefaults,
) -> ErrorStrategy {
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
        if !db::step_exists(
            &conn,
            &slug_owned,
            &step_name_owned,
            chunk_index,
            depth,
            &node_id_owned,
        )? {
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

async fn cleanup_from_depth(
    db: &Arc<Mutex<Connection>>,
    slug: &str,
    from_depth: i64,
) -> Result<()> {
    let slug_owned = slug.to_string();
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        cleanup_from_depth_sync(&conn, &slug_owned, from_depth)
    })
    .await?
}

fn cleanup_from_depth_sync(conn: &Connection, slug: &str, from_depth: i64) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    conn.execute(
        "UPDATE pyramid_nodes SET parent_id = NULL WHERE slug = ?1 AND depth < ?2",
        rusqlite::params![slug, from_depth],
    )?;
    conn.execute(
        "DELETE FROM pyramid_annotations WHERE slug = ?1 AND node_id IN \
         (SELECT id FROM pyramid_nodes WHERE slug = ?1 AND depth >= ?2)",
        rusqlite::params![slug, from_depth],
    )?;
    conn.execute(
        "DELETE FROM pyramid_threads WHERE slug = ?1 AND current_canonical_id IN \
         (SELECT id FROM pyramid_nodes WHERE slug = ?1 AND depth >= ?2)",
        rusqlite::params![slug, from_depth],
    )
    .ok();
    conn.execute(
        "DELETE FROM pyramid_web_edges WHERE slug = ?1 AND (thread_a_id NOT IN \
         (SELECT thread_id FROM pyramid_threads WHERE slug = ?1) OR thread_b_id NOT IN \
         (SELECT thread_id FROM pyramid_threads WHERE slug = ?1))",
        rusqlite::params![slug],
    )
    .ok();
    conn.execute(
        "DELETE FROM pyramid_distillations WHERE slug = ?1 AND thread_id NOT IN \
         (SELECT thread_id FROM pyramid_threads WHERE slug = ?1)",
        rusqlite::params![slug],
    )
    .ok();
    conn.execute(
        "DELETE FROM pyramid_deltas WHERE slug = ?1 AND thread_id NOT IN \
         (SELECT thread_id FROM pyramid_threads WHERE slug = ?1)",
        rusqlite::params![slug],
    )
    .ok();
    conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ?1 AND depth >= ?2",
        rusqlite::params![slug, from_depth],
    )?;
    conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND depth >= ?2",
        rusqlite::params![slug, from_depth],
    )?;

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    if from_depth <= 1 {
        conn.execute(
            "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND step_type IN ('thread_cluster', 'thread_narrative', 'synth')",
            rusqlite::params![slug],
        )?;
    }

    Ok(())
}

/// Load the exact saved output for a prior step execution.
async fn load_prior_step_output(
    reader: &Arc<Mutex<Connection>>,
    slug: &str,
    step_name: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
) -> Result<Option<Value>> {
    let slug_owned = slug.to_string();
    let step_name_owned = step_name.to_string();
    let node_id_owned = node_id.to_string();
    let json_str = db_read(reader, move |conn| {
        db::get_step_output_exact(
            conn,
            &slug_owned,
            &step_name_owned,
            chunk_index,
            depth,
            &node_id_owned,
        )
    })
    .await?;

    Ok(json_str.and_then(|s| serde_json::from_str::<Value>(&s).ok()))
}

fn decorate_step_output(mut output: Value, node_id: &str, chunk_index: i64) -> Value {
    if let Some(map) = output.as_object_mut() {
        map.insert("node_id".to_string(), Value::String(node_id.to_string()));
        map.insert(
            "source_node".to_string(),
            Value::String(node_id.to_string()),
        );
        map.insert(
            "chunk_index".to_string(),
            Value::Number(serde_json::Number::from(chunk_index)),
        );
    }
    output
}

fn parse_node_index(node_id: &str) -> Option<usize> {
    node_id.rsplit('-').next()?.parse::<usize>().ok()
}

fn normalize_context_ref(reference: &str) -> String {
    let trimmed = reference.trim();
    if trimmed.starts_with('$') {
        trimmed.to_string()
    } else {
        format!("${trimmed}")
    }
}

fn looks_like_analysis_output(value: &Value) -> bool {
    match value {
        Value::Object(map) => [
            "headline",
            "orientation",
            "distilled",
            "purpose",
            "topics",
            "exports",
            "key_functions",
            "logic_flows",
        ]
        .iter()
        .any(|key| map.contains_key(*key)),
        _ => false,
    }
}

fn candidate_score_for_node_id(step_name: &str, node_id: &str, value: &Value) -> i32 {
    let mut score = 0;

    if value.get("topics").and_then(|v| v.as_array()).is_some() {
        score += 3;
    }
    if step_name.contains("extract") {
        score += 6;
    }
    if step_name.contains("combine") || step_name.contains("group") {
        score += 5;
    }
    if step_name.contains("pair") || step_name.contains("narrative") {
        score += 2;
    }
    if step_name.contains("forward") || step_name.contains("reverse") {
        score -= 4;
    }
    if node_id.contains("L0") && (step_name.contains("extract") || step_name.contains("combine")) {
        score += 4;
    }
    if node_id.contains("L1")
        && (step_name.contains("pair")
            || step_name.contains("group")
            || step_name.contains("narrative"))
    {
        score += 4;
    }

    score
}

fn lookup_analysis_by_node_id(ctx: &ChainContext, node_id: &str) -> Option<Value> {
    let idx = parse_node_index(node_id)?;
    let mut best: Option<(i32, Value)> = None;

    for (step_name, output) in &ctx.step_outputs {
        let Some(candidate) = output.as_array().and_then(|arr| arr.get(idx)) else {
            continue;
        };
        if !looks_like_analysis_output(candidate) {
            continue;
        }

        let score = candidate_score_for_node_id(step_name, node_id, candidate);
        let should_replace = best
            .as_ref()
            .map(|(best_score, _)| score > *best_score)
            .unwrap_or(true);
        if should_replace {
            best = Some((score, candidate.clone()));
        }
    }

    best.map(|(_, value)| value)
}

fn candidate_node_id_from_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.rsplitn(2, '-');
    let index_part = parts.next()?;
    let prefix_part = parts.next()?;
    let layer_part = prefix_part.rsplit('-').next().unwrap_or(prefix_part);
    let looks_like_layer = (layer_part.starts_with('L') || layer_part.starts_with('l'))
        && layer_part[1..].chars().all(|c| c.is_ascii_digit());
    let looks_like_index = !index_part.is_empty() && index_part.chars().all(|c| c.is_ascii_digit());

    if looks_like_layer && looks_like_index {
        return Some(normalize_node_id(trimmed));
    }
    None
}

fn candidate_node_id_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => candidate_node_id_from_str(raw),
        Value::Object(map) => {
            for key in ["source_node", "sourceNode", "node_id", "nodeId", "id"] {
                if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
                    if let Some(node_id) = candidate_node_id_from_str(value) {
                        return Some(node_id);
                    }
                }
            }

            map.values().find_map(candidate_node_id_from_value)
        }
        _ => None,
    }
}

fn normalize_authoritative_child_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter_map(|id| candidate_node_id_from_str(&id))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn extract_assignment_source_node(assignment: &Value) -> Option<String> {
    match assignment {
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return extract_assignment_source_node(&parsed);
                }
            }

            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => {
            for key in ["source_node", "sourceNode", "node_id", "nodeId", "id"] {
                if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
                    return Some(value.to_string());
                }
            }

            map.values().find_map(|value| match value {
                Value::String(value) if value.contains("-L") || value.contains("-l") => {
                    Some(value.clone())
                }
                _ => None,
            })
        }
        _ => None,
    }
}

fn extract_assignment_label(assignment: &Value) -> Option<String> {
    match assignment {
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return extract_assignment_label(&parsed);
                }
            }
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => {
            for key in [
                "source_node",
                "sourceNode",
                "topic_name",
                "topicName",
                "headline",
                "name",
                "label",
            ] {
                if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_assignment_topic_index(assignment: &Value) -> Option<usize> {
    match assignment {
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return extract_assignment_topic_index(&parsed);
                }
            }
            None
        }
        Value::Object(map) => map
            .get("topic_index")
            .or_else(|| map.get("topicIndex"))
            .or_else(|| map.get("index"))
            .and_then(|value| value.as_u64())
            .map(|value| value as usize),
        _ => None,
    }
}

fn collect_child_ref_specs(item: &Value) -> Vec<(Option<usize>, Option<String>)> {
    if let Some(assignments) = item.get("assignments").and_then(|value| value.as_array()) {
        return assignments
            .iter()
            .map(|assignment| {
                (
                    extract_assignment_topic_index(assignment),
                    extract_assignment_label(assignment),
                )
            })
            .collect();
    }

    for key in ["node_ids", "source_nodes"] {
        if let Some(values) = item.get(key).and_then(|value| value.as_array()) {
            return values
                .iter()
                .map(|value| {
                    (
                        None,
                        value
                            .as_str()
                            .map(|raw| raw.trim().to_string())
                            .filter(|raw| !raw.is_empty()),
                    )
                })
                .collect();
        }
    }

    Vec::new()
}

fn extract_authoritative_child_ids(item: &Value) -> Vec<String> {
    if let Some(assignments) = item.get("assignments").and_then(|v| v.as_array()) {
        let extracted = normalize_authoritative_child_ids(
            assignments
                .iter()
                .filter_map(extract_assignment_source_node)
                .collect(),
        );
        if !extracted.is_empty() {
            return extracted;
        }
    }

    if let Some(node_ids) = item.get("node_ids").and_then(|v| v.as_array()) {
        let extracted = normalize_authoritative_child_ids(
            node_ids
                .iter()
                .filter_map(|value| value.as_str().map(String::from))
                .collect(),
        );
        if !extracted.is_empty() {
            return extracted;
        }
    }

    if let Some(source_nodes) = item.get("source_nodes").and_then(|v| v.as_array()) {
        return normalize_authoritative_child_ids(
            source_nodes
                .iter()
                .filter_map(|value| value.as_str().map(String::from))
                .collect(),
        );
    }

    Vec::new()
}

fn step_output_label_matches(item: &Value, label: &str) -> bool {
    let trimmed_label = label.trim();
    if trimmed_label.is_empty() {
        return false;
    }

    let Some(object) = item.as_object() else {
        return false;
    };

    ["headline", "topic_name", "topicName", "name", "label"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(|value| value.as_str()))
        .any(|value| value.trim().eq_ignore_ascii_case(trimmed_label))
}

fn resolve_node_id_from_context(
    ctx: &ChainContext,
    label: Option<&str>,
    topic_index: Option<usize>,
) -> Option<String> {
    let mut best: Option<(i32, String)> = None;

    for (step_name, output) in &ctx.step_outputs {
        let Some(items) = output.as_array() else {
            continue;
        };

        let mut consider_candidate = |item: &Value, matched_topic_index: bool| {
            let Some(candidate_id) = candidate_node_id_from_value(item) else {
                return;
            };

            let label_matches = label
                .map(|value| step_output_label_matches(item, value))
                .unwrap_or(false);
            if label.is_some() && !label_matches && !matched_topic_index {
                return;
            }

            let mut score = candidate_score_for_node_id(step_name, &candidate_id, item);
            if matched_topic_index {
                score += 20;
            }
            if label_matches {
                score += 10;
            }

            let should_replace = best
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true);
            if should_replace {
                best = Some((score, candidate_id));
            }
        };

        if let Some(index) = topic_index {
            if let Some(item) = items.get(index) {
                consider_candidate(item, true);
            }
        }

        if label.is_some() {
            for item in items {
                consider_candidate(item, false);
            }
        }
    }

    best.map(|(_, node_id)| node_id)
}

fn resolve_assignment_source_node(assignment: &Value, ctx: &ChainContext) -> Option<String> {
    let raw = extract_assignment_source_node(assignment);
    if let Some(raw) = raw.as_deref() {
        if let Some(node_id) = candidate_node_id_from_str(raw) {
            return Some(node_id);
        }
    }

    resolve_node_id_from_context(
        ctx,
        extract_assignment_label(assignment).as_deref(),
        extract_assignment_topic_index(assignment),
    )
}

fn resolve_authoritative_child_ids(item: &Value, ctx: &ChainContext) -> Vec<String> {
    let specs = collect_child_ref_specs(item);
    if specs.is_empty() {
        return extract_authoritative_child_ids(item);
    }

    normalize_authoritative_child_ids(
        specs
            .into_iter()
            .filter_map(|(topic_index, label)| {
                label
                    .as_deref()
                    .and_then(candidate_node_id_from_str)
                    .or_else(|| resolve_node_id_from_context(ctx, label.as_deref(), topic_index))
            })
            .collect(),
    )
}

async fn resolve_authoritative_child_ids_with_db(
    item: &Value,
    ctx: &ChainContext,
    reader: &Arc<Mutex<Connection>>,
) -> Result<Vec<String>> {
    let specs = collect_child_ref_specs(item);
    if specs.is_empty() {
        return Ok(extract_authoritative_child_ids(item));
    }

    let mut resolved_ids = Vec::new();
    let mut chunk_cache: HashMap<i64, Option<String>> = HashMap::new();
    let mut headline_cache: HashMap<String, Option<String>> = HashMap::new();

    for (topic_index, label) in specs {
        let mut resolved = label
            .as_deref()
            .and_then(candidate_node_id_from_str)
            .or_else(|| resolve_node_id_from_context(ctx, label.as_deref(), topic_index));

        if resolved.is_none() {
            if let Some(index) = topic_index {
                let cache_key = index as i64;
                if let Some(cached) = chunk_cache.get(&cache_key).cloned() {
                    resolved = cached;
                } else {
                    let slug = ctx.slug.clone();
                    let lookup = db_read(reader, move |conn| {
                        db::get_node_id_by_depth_and_chunk_index(conn, &slug, 0, cache_key)
                    })
                    .await?;
                    chunk_cache.insert(cache_key, lookup.clone());
                    resolved = lookup;
                }
            }
        }

        if resolved.is_none() {
            if let Some(label) = label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if let Some(cached) = headline_cache.get(label).cloned() {
                    resolved = cached;
                } else {
                    let slug = ctx.slug.clone();
                    let headline = label.to_string();
                    let lookup = db_read(reader, move |conn| {
                        db::get_node_id_by_depth_and_headline(conn, &slug, 0, &headline)
                    })
                    .await?;
                    headline_cache.insert(label.to_string(), lookup.clone());
                    resolved = lookup;
                }
            }
        }

        if let Some(node_id) = resolved {
            resolved_ids.push(node_id);
        }
    }

    Ok(normalize_authoritative_child_ids(resolved_ids))
}

fn enrich_group_item_input(item: &Value, ctx: &ChainContext) -> Value {
    let source_nodes = resolve_authoritative_child_ids(item, ctx);
    if source_nodes.is_empty() {
        return item.clone();
    }

    let mut enriched = item.as_object().cloned().unwrap_or_default();
    enriched.insert(
        "source_nodes".to_string(),
        Value::Array(source_nodes.iter().cloned().map(Value::String).collect()),
    );
    enriched.insert(
        "source_count".to_string(),
        Value::Number(serde_json::Number::from(source_nodes.len() as u64)),
    );

    let assigned_items = if let Some(assignments) =
        item.get("assignments").and_then(|v| v.as_array())
    {
        assignments
            .iter()
            .filter_map(|assignment| {
                let normalized = resolve_assignment_source_node(assignment, ctx)?;
                let mut assignment_obj = assignment.as_object().cloned().unwrap_or_default();
                assignment_obj.insert("source_node".to_string(), Value::String(normalized.clone()));
                if let Some(analysis) = lookup_analysis_by_node_id(ctx, &normalized) {
                    assignment_obj.insert("analysis".to_string(), analysis);
                }
                Some(Value::Object(assignment_obj))
            })
            .collect::<Vec<_>>()
    } else {
        source_nodes
            .iter()
            .map(|source_node| {
                let mut child = serde_json::Map::new();
                child.insert(
                    "source_node".to_string(),
                    Value::String(source_node.clone()),
                );
                if let Some(analysis) = lookup_analysis_by_node_id(ctx, source_node) {
                    child.insert("analysis".to_string(), analysis);
                }
                Value::Object(child)
            })
            .collect::<Vec<_>>()
    };

    if !assigned_items.is_empty() {
        enriched.insert("assigned_items".to_string(), Value::Array(assigned_items));
    }

    let source_analyses: Vec<Value> = source_nodes
        .iter()
        .filter_map(|source_node| {
            lookup_analysis_by_node_id(ctx, source_node).map(|analysis| {
                serde_json::json!({
                    "source_node": source_node,
                    "analysis": analysis,
                })
            })
        })
        .collect();
    if !source_analyses.is_empty() {
        enriched.insert("source_analyses".to_string(), Value::Array(source_analyses));
    }

    Value::Object(enriched)
}

fn recursive_cluster_layer_complete(
    current_nodes: &[PyramidNode],
    target_nodes: &[PyramidNode],
) -> bool {
    if current_nodes.is_empty() {
        return true;
    }

    let target_ids: HashSet<&str> = target_nodes.iter().map(|node| node.id.as_str()).collect();
    !target_ids.is_empty()
        && current_nodes.iter().all(|node| {
            node.parent_id
                .as_deref()
                .map(|parent_id| target_ids.contains(parent_id))
                .unwrap_or(false)
        })
}

async fn hydrate_skipped_step_output(
    step: &ChainStep,
    ctx: &ChainContext,
    reader: &Arc<Mutex<Connection>>,
) -> Result<Option<Value>> {
    if let Some(for_each_ref) = step.for_each.as_deref() {
        let resolved_ref = normalize_context_ref(for_each_ref);
        let items = match ctx.resolve_ref(&resolved_ref)? {
            Value::Array(items) => items,
            other => {
                return Err(anyhow!(
                    "Skipped step '{}' ref '{}' resolved to {}, expected array",
                    step.name,
                    resolved_ref,
                    other
                ));
            }
        };

        let depth = step.depth.unwrap_or(0);
        let mut outputs = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let chunk_index = item
                .get("index")
                .and_then(|value| value.as_i64())
                .unwrap_or(index as i64);
            let node_id = if let Some(pattern) = step.node_id_pattern.as_deref() {
                generate_node_id(pattern, index, Some(depth))
            } else {
                format!("L{depth}-{index:03}")
            };
            let prior_output =
                load_prior_step_output(reader, &ctx.slug, &step.name, chunk_index, depth, &node_id)
                    .await?
                    .ok_or_else(|| {
                        anyhow!(
                            "Skipped step '{}' is missing saved output for {}",
                            step.name,
                            node_id
                        )
                    })?;
            outputs.push(decorate_step_output(prior_output, &node_id, chunk_index));
        }

        return Ok(Some(Value::Array(outputs)));
    }

    let depth = step.depth.unwrap_or(0);
    let node_id = if let Some(pattern) = step.node_id_pattern.as_deref() {
        generate_node_id(pattern, 0, Some(depth))
    } else {
        format!("L{depth}-000")
    };

    let prior_output = load_prior_step_output(reader, &ctx.slug, &step.name, -1, depth, &node_id)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Skipped step '{}' is missing saved output for {}",
                step.name,
                node_id
            )
        })?;

    Ok(Some(decorate_step_output(prior_output, &node_id, -1)))
}

fn validate_step_output(step: &ChainStep, output: &Value) -> Result<()> {
    if let Some(schema) = step.response_schema.as_ref() {
        if let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) {
            let output_object = output.as_object();
            for (key, property_schema) in properties {
                let Some(min_items) = property_schema
                    .get("minItems")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as usize)
                else {
                    continue;
                };

                let actual_len = output_object
                    .and_then(|object| object.get(key))
                    .and_then(|value| value.as_array())
                    .map(|items| items.len())
                    .unwrap_or(0);

                if actual_len < min_items {
                    return Err(anyhow!(
                        "Step '{}' returned {} item(s) for '{}', expected at least {}",
                        step.name,
                        actual_len,
                        key,
                        min_items
                    ));
                }
            }
        }
    }

    for key in ["threads", "clusters"] {
        if let Some(items) = output.get(key).and_then(|value| value.as_array()) {
            if items.is_empty() {
                return Err(anyhow!("Step '{}' returned an empty '{}'", step.name, key));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct PendingWebEdge {
    source_node_id: String,
    target_node_id: String,
    relationship: String,
    strength: f64,
}

fn truncate_for_webbing(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let prefix: String = trimmed.chars().take(max_chars).collect();
    format!("{prefix}...")
}

fn collect_web_entities(node: &PyramidNode) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut entities = Vec::new();

    for topic in &node.topics {
        for entity in &topic.entities {
            let trimmed = entity.trim();
            if trimmed.is_empty() {
                continue;
            }

            let key = trimmed.to_ascii_lowercase();
            if seen.insert(key) {
                entities.push(trimmed.to_string());
            }
        }
    }

    entities.sort();
    entities
}

fn build_webbing_input(nodes: &[PyramidNode], depth: i64, resolved_input: &Value) -> Value {
    let node_payloads: Vec<Value> = nodes
        .iter()
        .map(|node| {
            let topic_payloads: Vec<Value> = node
                .topics
                .iter()
                .map(|topic| {
                    serde_json::json!({
                        "name": topic.name.clone(),
                        "current": truncate_for_webbing(&topic.current, 240),
                        "entities": topic.entities.clone(),
                    })
                })
                .collect();

            serde_json::json!({
                "node_id": node.id.clone(),
                "headline": node.headline.clone(),
                "orientation": truncate_for_webbing(&node.distilled, 1200),
                "topics": topic_payloads,
                "entities": collect_web_entities(node),
            })
        })
        .collect();

    let mut payload = serde_json::Map::new();
    payload.insert(
        "depth".to_string(),
        Value::Number(serde_json::Number::from(depth)),
    );
    payload.insert(
        "node_count".to_string(),
        Value::Number(serde_json::Number::from(nodes.len() as u64)),
    );
    payload.insert("nodes".to_string(), Value::Array(node_payloads));

    if let Some(extra) = resolved_input.as_object() {
        for (key, value) in extra {
            if key != "nodes" {
                payload.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(payload)
}

fn extract_explicit_web_node_ids(resolved_input: &Value) -> Vec<String> {
    let items = resolved_input
        .get("nodes")
        .and_then(|value| value.as_array())
        .or_else(|| resolved_input.as_array());

    let Some(items) = items else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut node_ids = Vec::new();

    for item in items {
        let candidate = match item {
            Value::String(raw) => candidate_node_id_from_str(raw),
            Value::Object(map) => ["node_id", "source_node", "id"]
                .iter()
                .filter_map(|key| map.get(*key).and_then(|value| value.as_str()))
                .find_map(candidate_node_id_from_str),
            _ => None,
        };

        if let Some(node_id) = candidate {
            if seen.insert(node_id.clone()) {
                node_ids.push(node_id);
            }
        }
    }

    node_ids
}

async fn load_nodes_for_webbing(
    reader: &Arc<Mutex<Connection>>,
    slug: &str,
    depth: i64,
    expected_ids: &[String],
) -> Result<Vec<PyramidNode>> {
    let expected_order = expected_ids.to_vec();
    let expected_set: HashSet<String> = expected_order.iter().cloned().collect();
    let max_attempts = if expected_order.is_empty() { 1 } else { 5 };

    for attempt in 0..max_attempts {
        let slug_owned = slug.to_string();
        let mut nodes = db_read(reader, move |conn| {
            db::get_nodes_at_depth(conn, &slug_owned, depth)
        })
        .await?;

        if !expected_set.is_empty() {
            nodes.retain(|node| expected_set.contains(&node.id));

            if nodes.len() >= expected_order.len() || attempt + 1 == max_attempts {
                let mut by_id: HashMap<String, PyramidNode> = nodes
                    .into_iter()
                    .map(|node| (node.id.clone(), node))
                    .collect();
                let ordered = expected_order
                    .iter()
                    .filter_map(|node_id| by_id.remove(node_id))
                    .collect();
                return Ok(ordered);
            }
        } else {
            return Ok(nodes);
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    Ok(Vec::new())
}

fn normalize_web_relationship(raw: &str, shared_resources: &[String]) -> String {
    let mut relationship = raw.trim().to_string();
    let shared_summary = shared_resources
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .take(4)
        .collect::<Vec<_>>();

    if relationship.is_empty() {
        if shared_summary.is_empty() {
            relationship = "Related sibling nodes".to_string();
        } else {
            relationship = format!("Shared resources: {}", shared_summary.join(", "));
        }
    } else if !shared_summary.is_empty() {
        relationship = format!(
            "{relationship} Shared resources: {}",
            shared_summary.join(", ")
        );
    }

    relationship
}

fn resolve_web_node_ref(
    raw: &str,
    node_ids: &HashSet<String>,
    headline_lookup: &HashMap<String, Vec<String>>,
) -> Option<String> {
    if let Some(node_id) = candidate_node_id_from_str(raw) {
        if node_ids.contains(&node_id) {
            return Some(node_id);
        }
    }

    let normalized = raw.trim().to_ascii_lowercase();
    headline_lookup
        .get(&normalized)
        .filter(|ids| ids.len() == 1)
        .and_then(|ids| ids.first().cloned())
}

fn parse_web_edges(step_name: &str, output: &Value, nodes: &[PyramidNode]) -> Vec<PendingWebEdge> {
    let mut node_ids = HashSet::new();
    let mut headline_lookup: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes {
        node_ids.insert(node.id.clone());
        headline_lookup
            .entry(node.headline.trim().to_ascii_lowercase())
            .or_default()
            .push(node.id.clone());
    }

    let mut deduped: HashMap<(String, String), PendingWebEdge> = HashMap::new();
    let edges = output
        .get("edges")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    for edge in edges {
        let source_raw = edge
            .get("source")
            .or_else(|| edge.get("source_node"))
            .or_else(|| edge.get("from"))
            .and_then(|value| value.as_str());
        let target_raw = edge
            .get("target")
            .or_else(|| edge.get("target_node"))
            .or_else(|| edge.get("to"))
            .and_then(|value| value.as_str());

        let (Some(source_raw), Some(target_raw)) = (source_raw, target_raw) else {
            continue;
        };

        let Some(source_node_id) = resolve_web_node_ref(source_raw, &node_ids, &headline_lookup)
        else {
            warn!(
                "[CHAIN] [{}] web edge source did not resolve to a node: {:?}",
                step_name, source_raw
            );
            continue;
        };
        let Some(target_node_id) = resolve_web_node_ref(target_raw, &node_ids, &headline_lookup)
        else {
            warn!(
                "[CHAIN] [{}] web edge target did not resolve to a node: {:?}",
                step_name, target_raw
            );
            continue;
        };

        if source_node_id == target_node_id {
            continue;
        }

        let (source_node_id, target_node_id) = if source_node_id < target_node_id {
            (source_node_id, target_node_id)
        } else {
            (target_node_id, source_node_id)
        };

        let shared_resources: Vec<String> = edge
            .get("shared_resources")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let relationship = normalize_web_relationship(
            edge.get("relationship")
                .or_else(|| edge.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or(""),
            &shared_resources,
        );
        let strength = edge
            .get("strength")
            .or_else(|| edge.get("relevance"))
            .and_then(|value| value.as_f64())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);

        let pending = PendingWebEdge {
            source_node_id: source_node_id.clone(),
            target_node_id: target_node_id.clone(),
            relationship,
            strength,
        };

        let key = (source_node_id, target_node_id);
        match deduped.get(&key) {
            Some(existing)
                if existing.strength > pending.strength
                    || (existing.strength == pending.strength
                        && existing.relationship.len() >= pending.relationship.len()) => {}
            _ => {
                deduped.insert(key, pending);
            }
        }
    }

    deduped.into_values().collect()
}

async fn persist_web_edges_for_depth(
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    depth: i64,
    edges: &[PendingWebEdge],
) -> Result<usize> {
    let slug_owned = slug.to_string();
    let edges_owned = edges.to_vec();
    let mut node_ids = HashSet::new();
    for edge in edges {
        node_ids.insert(edge.source_node_id.clone());
        node_ids.insert(edge.target_node_id.clone());
    }
    let node_ids: Vec<String> = node_ids.into_iter().collect();

    let writer = writer.clone();
    tokio::task::spawn_blocking(move || -> Result<usize> {
        let conn = writer.blocking_lock();
        let mut node_to_thread: HashMap<String, String> = HashMap::new();

        for node_id in node_ids {
            if let Some(thread_id) = resolve_stale_target_for_node(&conn, &slug_owned, &node_id)? {
                node_to_thread.insert(node_id, thread_id);
            }
        }

        db::delete_web_edges_for_depth(&conn, &slug_owned, depth)?;

        let mut saved = 0;
        for edge in edges_owned {
            let Some(thread_a_id) = node_to_thread.get(&edge.source_node_id).cloned() else {
                warn!(
                    "[CHAIN] web edge source node missing thread target: {}",
                    edge.source_node_id
                );
                continue;
            };
            let Some(thread_b_id) = node_to_thread.get(&edge.target_node_id).cloned() else {
                warn!(
                    "[CHAIN] web edge target node missing thread target: {}",
                    edge.target_node_id
                );
                continue;
            };

            if thread_a_id == thread_b_id {
                continue;
            }

            let (thread_a_id, thread_b_id) = if thread_a_id < thread_b_id {
                (thread_a_id, thread_b_id)
            } else {
                (thread_b_id, thread_a_id)
            };

            let edge_row = WebEdge {
                id: 0,
                slug: slug_owned.clone(),
                thread_a_id,
                thread_b_id,
                relationship: edge.relationship,
                relevance: edge.strength,
                delta_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
            };
            db::save_web_edge(&conn, &edge_row)?;
            saved += 1;
        }

        Ok(saved)
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

async fn send_progress(progress_tx: &Option<mpsc::Sender<BuildProgress>>, done: i64, total: i64) {
    if let Some(ref tx) = progress_tx {
        let _ = tx.send(BuildProgress { done, total }).await;
    }
}

/// Estimate total iterations for progress reporting.
fn step_saves_node(step: &ChainStep) -> bool {
    step.save_as.as_deref() == Some("node")
}

fn estimate_for_each_count(step: &ChainStep, ctx: &ChainContext, num_chunks: i64) -> i64 {
    let resolved_ref = normalize_context_ref(step.for_each.as_deref().unwrap_or("$chunks"));
    if resolved_ref == "$chunks" {
        return num_chunks;
    }

    ctx.resolve_ref(&resolved_ref)
        .ok()
        .and_then(|value| value.as_array().map(|items| items.len() as i64))
        .unwrap_or(0)
}

fn node_id_matches_depth(node_id: &str, depth: i64) -> bool {
    if depth == 0 {
        return node_id.contains("L0");
    }
    node_id.starts_with(&format!("L{depth}-"))
}

fn estimate_nodes_at_depth(depth: i64, ctx: &ChainContext, num_chunks: i64) -> i64 {
    if depth == 0 {
        return num_chunks;
    }

    let mut best = 0;

    for output in ctx.step_outputs.values() {
        if let Some(items) = output.as_array() {
            let count = items
                .iter()
                .filter_map(candidate_node_id_from_value)
                .filter(|node_id| node_id_matches_depth(node_id, depth))
                .count() as i64;
            best = best.max(count);
            continue;
        }

        if depth == 1 {
            if let Some(threads) = output.get("threads").and_then(|value| value.as_array()) {
                best = best.max(threads.len() as i64);
            }
        }
    }

    best
}

fn estimate_recursive_pair_nodes(mut source_count: i64) -> i64 {
    let mut total = 0;
    while source_count > 1 {
        let pairs = (source_count + 1) / 2;
        total += pairs;
        source_count = pairs;
    }
    total
}

fn estimate_recursive_cluster_nodes(mut source_count: i64) -> i64 {
    if source_count <= 1 {
        return 0;
    }

    let mut total = 0;
    while source_count > 4 {
        let clusters = (source_count + 4) / 5;
        total += clusters;
        source_count = clusters;
    }

    total + 1
}

fn estimate_total(chain: &ChainDefinition, ctx: &ChainContext, num_chunks: i64) -> i64 {
    let mut total: i64 = 0;
    for step in &chain.steps {
        if !step_saves_node(step) {
            continue;
        }

        if step.for_each.is_some() {
            total += estimate_for_each_count(step, ctx, num_chunks);
        } else if step.pair_adjacent {
            let source_count = estimate_nodes_at_depth(step.depth.unwrap_or(0), ctx, num_chunks);
            total += (source_count + 1) / 2;
        } else if step.recursive_pair {
            let source_count = estimate_nodes_at_depth(step.depth.unwrap_or(0), ctx, num_chunks);
            total += estimate_recursive_pair_nodes(source_count);
        } else if step.recursive_cluster {
            let source_count = estimate_nodes_at_depth(step.depth.unwrap_or(1), ctx, num_chunks);
            total += estimate_recursive_cluster_nodes(source_count);
        } else {
            total += 1;
        }
    }
    total.max(1)
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

            let lhs = ctx
                .resolve_ref(lhs_expr)
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
        match chain_dispatch::dispatch_step(
            step,
            resolved_input,
            system_prompt,
            defaults,
            dispatch_ctx,
        )
        .await
        {
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
/// If `from_depth` > 0, skips steps producing nodes below that depth and
/// deletes existing nodes/steps at `from_depth` and above before running.
pub async fn execute_chain(
    state: &PyramidState,
    chain: &ChainDefinition,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<(String, i32)> {
    execute_chain_from(state, chain, slug, 0, cancel, progress_tx).await
}

/// Execute a chain from a specific depth, reusing nodes below that depth.
pub async fn execute_chain_from(
    state: &PyramidState,
    chain: &ChainDefinition,
    slug: &str,
    from_depth: i64,
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
            let content = db_read(&state.reader, move |conn| db::get_chunk(conn, &s, i))
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

    // If from_depth > 0, clean up nodes and steps at/above that depth
    if from_depth > 0 {
        info!(
            "[CHAIN] Layered rebuild from depth {from_depth}: deleting nodes and steps at depth >= {from_depth}"
        );
        cleanup_from_depth(&state.writer, slug, from_depth).await?;
    }

    // Build chain context (from chain_resolve)
    let mut ctx = ChainContext::new(slug, &chain.content_type, chunks);
    ctx.has_prior_build = has_prior_build;

    let mut total = estimate_total(chain, &ctx, num_chunks);
    let mut done: i64 = 0;
    let mut total_failures: i32 = 0;
    let mut apex_node_id = String::new();

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

        // Skip steps below from_depth (layered rebuild)
        // Only skip extract steps that produce nodes below from_depth.
        // Never skip classify/synthesize steps — they read lower depths
        // and produce assignments or nodes at/above from_depth.
        if from_depth > 0 {
            let step_depth = step.depth.unwrap_or(0);
            let is_extract = step.primitive == "extract"
                || step.primitive == "compress"
                || step.primitive == "fuse";
            let is_spanning = step.recursive_cluster || step.recursive_pair;
            if step_depth < from_depth && is_extract && !is_spanning {
                info!(
                    "[CHAIN] step \"{}\" skipped (extract at depth {} < from_depth {})",
                    step.name, step_depth, from_depth
                );
                if let Some(hydrated_output) =
                    hydrate_skipped_step_output(step, &ctx, &state.reader).await?
                {
                    if step_saves_node(step) {
                        done += match &hydrated_output {
                            Value::Array(items) => items.len() as i64,
                            Value::Null => 0,
                            _ => 1,
                        };
                    }
                    ctx.step_outputs.insert(step.name.clone(), hydrated_output);
                    total = estimate_total(chain, &ctx, num_chunks).max(done);
                    send_progress(&progress_tx, done, total).await;
                }
                continue;
            }
        }

        let error_strategy = resolve_error_strategy(step, &chain.defaults);
        let saves_node = step.save_as.as_deref() == Some("node");

        info!(
            "[CHAIN] step \"{}\" started ({}/{}, primitive: {}, done={}/{})",
            step.name,
            step_idx + 1,
            chain.steps.len(),
            step.primitive,
            done,
            total,
        );

        let step_result = if step.mechanical {
            execute_mechanical(step, &mut ctx, &dispatch_ctx, &chain.defaults).await
        } else if step.primitive == "web" {
            execute_web_step(
                step,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                &state.reader,
                &state.writer,
            )
            .await
        } else if step.recursive_pair {
            let starting_depth = step.depth.unwrap_or(1);
            let (apex_id, failures) = execute_recursive_pair(
                step,
                starting_depth,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                saves_node,
                &writer_tx,
                &state.reader,
                cancel,
                &progress_tx,
                &mut done,
                total,
            )
            .await?;
            apex_node_id = apex_id;
            total_failures += failures;
            Ok(Value::Null)
        } else if step.recursive_cluster {
            let starting_depth = step.depth.unwrap_or(1);
            let (apex_id, failures) = execute_recursive_cluster(
                step,
                starting_depth,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                saves_node,
                &writer_tx,
                &state.reader,
                cancel,
                &progress_tx,
                &mut done,
                total,
            )
            .await?;
            apex_node_id = apex_id;
            total_failures += failures;
            Ok(Value::Null)
        } else if step.pair_adjacent {
            let source_depth = step.depth.unwrap_or(0);
            let (outputs, failures) = execute_pair_adjacent(
                step,
                source_depth,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                saves_node,
                &writer_tx,
                &state.reader,
                cancel,
                &progress_tx,
                &mut done,
                total,
            )
            .await?;
            total_failures += failures;
            ctx.step_outputs
                .insert(step.name.clone(), Value::Array(outputs));
            Ok(Value::Null)
        } else if step.for_each.is_some() {
            let (outputs, failures) = execute_for_each(
                step,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                saves_node,
                &writer_tx,
                &state.reader,
                cancel,
                &progress_tx,
                &mut done,
                total,
            )
            .await?;
            total_failures += failures;
            ctx.step_outputs
                .insert(step.name.clone(), Value::Array(outputs));
            Ok(Value::Null)
        } else {
            execute_single(
                step,
                &mut ctx,
                &dispatch_ctx,
                &chain.defaults,
                &error_strategy,
                saves_node,
                &writer_tx,
                &state.reader,
                cancel,
                &progress_tx,
                &mut done,
                total,
            )
            .await
        };

        match step_result {
            Ok(output) => {
                info!("[CHAIN] step \"{}\" complete", step.name);
                if !output.is_null() {
                    ctx.step_outputs.insert(step.name.clone(), output);
                }
                total = estimate_total(chain, &ctx, num_chunks).max(done);
                send_progress(&progress_tx, done, total).await;
            }
            Err(e) => match error_strategy {
                ErrorStrategy::Abort | ErrorStrategy::Retry(_) => {
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
            },
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

#[derive(Clone)]
struct ForEachPendingWork {
    index: usize,
    item: Value,
    chunk_index: i64,
    depth: i64,
    node_id: String,
    resolved_input: Value,
    system_prompt: String,
}

struct ForEachTaskOutcome {
    index: usize,
    node_id: String,
    output: Result<Value>,
}

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
    let for_each_ref = normalize_context_ref(step.for_each.as_deref().unwrap_or("$chunks"));
    let items = match ctx.resolve_ref(&for_each_ref) {
        Ok(Value::Array(arr)) => arr,
        Ok(other) => {
            return Err(anyhow!(
                "forEach ref '{}' resolved to {}, expected array",
                for_each_ref,
                other
            ));
        }
        Err(e) => {
            return Err(anyhow!(
                "Could not resolve forEach ref '{}': {e}",
                for_each_ref
            ));
        }
    };

    info!("[CHAIN] [{}] forEach: {} items", step.name, items.len());
    let mut outputs: Vec<Value> = Vec::with_capacity(items.len());
    let mut failures: i32 = 0;

    // Initialize accumulators from step.accumulate config
    if let Some(ref acc_config) = step.accumulate {
        if let Value::Object(acc_map) = acc_config {
            for (name, config) in acc_map {
                let init = config.get("init").and_then(|v| v.as_str()).unwrap_or("");
                ctx.accumulators.insert(name.clone(), init.to_string());
            }
        }
    }

    let instruction = step.instruction.as_deref().unwrap_or("");
    let concurrency = step.concurrency.max(1);

    if !step.sequential && concurrency > 1 {
        info!(
            "[CHAIN] [{}] forEach: dispatching with concurrency={}",
            step.name, concurrency
        );
        return execute_for_each_concurrent(
            step,
            ctx,
            items,
            instruction,
            dispatch_ctx,
            defaults,
            error_strategy,
            saves_node,
            writer_tx,
            reader,
            cancel,
            progress_tx,
            done,
            total,
        )
        .await;
    }

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
            reader,
            &ctx.slug,
            &step.name,
            chunk_index,
            depth,
            &node_id,
            saves_node,
        )
        .await?;

        match resume {
            ResumeState::Complete => {
                info!("[CHAIN] [{}] {} -- resumed (complete)", step.name, node_id);

                if let Some(prior_output) = load_prior_step_output(
                    reader,
                    &ctx.slug,
                    &step.name,
                    chunk_index,
                    depth,
                    &node_id,
                )
                .await?
                {
                    if step.sequential {
                        update_accumulators(&mut ctx.accumulators, &prior_output, step);
                    }
                    outputs.push(decorate_step_output(prior_output, &node_id, chunk_index));
                } else {
                    warn!(
                        "[CHAIN] [{}] {} -- resume hit without saved output_json",
                        step.name, node_id
                    );
                    outputs.push(Value::Null);
                }

                if saves_node {
                    *done += 1;
                    send_progress(progress_tx, *done, total).await;
                }
                continue;
            }
            ResumeState::StaleStep => {
                warn!(
                    "[CHAIN] [{}] {} -- stale step (node missing), rebuilding",
                    step.name, node_id
                );
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
            enrich_group_item_input(item, ctx)
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
            step,
            &resolved_input,
            &system_prompt,
            defaults,
            dispatch_ctx,
            error_strategy,
            &fallback_key,
        )
        .await
        {
            Ok(analysis) => {
                validate_step_output(step, &analysis)?;
                let elapsed = t0.elapsed().as_secs_f64();
                let decorated_output =
                    decorate_step_output(analysis.clone(), &node_id, chunk_index);

                // Save step output
                let output_json = serde_json::to_string(&decorated_output)?;
                send_save_step(
                    writer_tx,
                    &ctx.slug,
                    &step.name,
                    chunk_index,
                    depth,
                    &node_id,
                    &output_json,
                    &dispatch_ctx.config.primary_model,
                    elapsed,
                )
                .await;

                // Save node if configured
                if saves_node {
                    let mut node = build_node_from_output(
                        &analysis,
                        &node_id,
                        &ctx.slug,
                        depth,
                        Some(chunk_index),
                    )?;

                    // ALWAYS prefer assignment/cluster IDs over LLM source_nodes.
                    // The LLM frequently returns headlines or wrong IDs in source_nodes.
                    // Assignments from clustering are authoritative.
                    let mut used_authoritative = false;
                    let authoritative_children = {
                        let extracted =
                            resolve_authoritative_child_ids_with_db(item, ctx, reader).await?;
                        if extracted.is_empty() {
                            resolve_authoritative_child_ids_with_db(&resolved_input, ctx, reader)
                                .await?
                        } else {
                            extracted
                        }
                    };
                    if !authoritative_children.is_empty() {
                        info!(
                            "[CHAIN] [{}] {node_id}: using {} authoritative child IDs (replacing {} LLM children)",
                            step.name, authoritative_children.len(), node.children.len()
                        );
                        node.children = authoritative_children;
                        used_authoritative = true;
                    }

                    // Third: fall back to LLM source_nodes only if no authoritative source
                    if !used_authoritative {
                        if let Some(assignments) =
                            item.get("assignments").and_then(|v| v.as_array())
                        {
                            if let Some(first_assignment) = assignments.first() {
                                let first_keys = first_assignment
                                    .as_object()
                                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                                    .unwrap_or_default();
                                warn!(
                                    "[CHAIN] [{}] {node_id}: assignments present but no child IDs extracted; first_assignment={}; first_assignment_keys={:?}",
                                    step.name,
                                    first_assignment,
                                    first_keys,
                                );
                            }
                        }

                        let has_valid_children = !node.children.is_empty()
                            && node
                                .children
                                .iter()
                                .all(|c| c.contains("-L") || c.contains("-l"));
                        if !has_valid_children {
                            let item_keys = item
                                .as_object()
                                .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                                .unwrap_or_default();
                            let resolved_keys = resolved_input
                                .as_object()
                                .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                                .unwrap_or_default();
                            warn!(
                                "[CHAIN] [{}] {node_id}: no authoritative children in item/resolved_input; item_keys={:?}; resolved_keys={:?}; LLM children invalid ({:?})",
                                step.name,
                                item_keys,
                                resolved_keys,
                                node.children.iter().take(3).collect::<Vec<_>>()
                            );
                            node.children = Vec::new();
                        }
                    }

                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    // Wire parent_id on children
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

                outputs.push(decorated_output);
                info!("[CHAIN] [{}] {node_id} complete ({elapsed:.1}s)", step.name);
            }
            Err(e) => match error_strategy {
                ErrorStrategy::Abort | ErrorStrategy::Retry(_) => {
                    return Err(anyhow!("forEach abort at index {index}: {e}"));
                }
                _ => {
                    warn!("[CHAIN] [{}] {node_id} FAILED (skip): {e}", step.name);
                    failures += 1;
                    outputs.push(Value::Null);
                }
            },
        }

        if saves_node {
            *done += 1;
            send_progress(progress_tx, *done, total).await;
        }
    }

    // Clear forEach loop variables
    ctx.current_item = None;
    ctx.current_index = None;

    Ok((outputs, failures))
}

#[allow(clippy::too_many_arguments)]
async fn execute_for_each_concurrent(
    step: &ChainStep,
    ctx: &mut ChainContext,
    items: Vec<Value>,
    instruction: &str,
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
    let mut outputs = vec![Value::Null; items.len()];
    let mut failures: i32 = 0;
    let depth = step.depth.unwrap_or(0);
    let ctx_snapshot = Arc::new(ctx.clone());
    let mut pending = Vec::new();

    for (index, item) in items.iter().enumerate() {
        if cancel.is_cancelled() {
            info!("forEach cancelled while preparing iteration {index}");
            break;
        }

        let chunk_index = item
            .get("index")
            .and_then(|v| v.as_i64())
            .unwrap_or(index as i64);
        let node_id = if let Some(ref pattern) = step.node_id_pattern {
            generate_node_id(pattern, index, Some(depth))
        } else {
            format!("L{depth}-{index:03}")
        };

        let resume = get_resume_state(
            reader,
            &ctx.slug,
            &step.name,
            chunk_index,
            depth,
            &node_id,
            saves_node,
        )
        .await?;

        match resume {
            ResumeState::Complete => {
                info!("[CHAIN] [{}] {} -- resumed (complete)", step.name, node_id);

                if let Some(prior_output) = load_prior_step_output(
                    reader,
                    &ctx.slug,
                    &step.name,
                    chunk_index,
                    depth,
                    &node_id,
                )
                .await?
                {
                    outputs[index] = decorate_step_output(prior_output, &node_id, chunk_index);
                } else {
                    warn!(
                        "[CHAIN] [{}] {} -- resume hit without saved output_json",
                        step.name, node_id
                    );
                }

                if saves_node {
                    *done += 1;
                    send_progress(progress_tx, *done, total).await;
                }
                continue;
            }
            ResumeState::StaleStep => {
                warn!(
                    "[CHAIN] [{}] {} -- stale step (node missing), rebuilding",
                    step.name, node_id
                );
            }
            ResumeState::Missing => {}
        }

        let mut item_ctx = (*ctx_snapshot).clone();
        item_ctx.current_item = Some(item.clone());
        item_ctx.current_index = Some(index);

        let resolved_input = if let Some(ref input) = step.input {
            item_ctx.resolve_value(input)?
        } else {
            enrich_group_item_input(item, &item_ctx)
        };
        let system_prompt = match resolve_prompt_template(instruction, &resolved_input) {
            Ok(s) => s,
            Err(_) => instruction.to_string(),
        };

        pending.push(ForEachPendingWork {
            index,
            item: item.clone(),
            chunk_index,
            depth,
            node_id,
            resolved_input,
            system_prompt,
        });
    }

    if pending.is_empty() {
        return Ok((outputs, failures));
    }

    let semaphore = Arc::new(Semaphore::new(step.concurrency.max(1)));
    let (result_tx, mut result_rx) =
        mpsc::channel::<ForEachTaskOutcome>((step.concurrency.max(1) * 2).max(2));
    let mut handles = Vec::new();

    for work in pending {
        let semaphore = semaphore.clone();
        let result_tx = result_tx.clone();
        let step_owned = step.clone();
        let defaults_owned = defaults.clone();
        let dispatch_ctx_owned = dispatch_ctx.clone();
        let error_strategy_owned = error_strategy.clone();
        let writer_tx = writer_tx.clone();
        let reader = reader.clone();
        let ctx_snapshot = ctx_snapshot.clone();

        let handle = tokio::spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("for_each semaphore should remain open");

            let output = execute_for_each_work_item(
                &step_owned,
                &work,
                ctx_snapshot.as_ref(),
                &dispatch_ctx_owned,
                &defaults_owned,
                &error_strategy_owned,
                saves_node,
                &writer_tx,
                &reader,
            )
            .await;

            let _ = result_tx
                .send(ForEachTaskOutcome {
                    index: work.index,
                    node_id: work.node_id.clone(),
                    output,
                })
                .await;
        });
        handles.push(handle);
    }
    drop(result_tx);

    let mut remaining = handles.len();
    while remaining > 0 {
        if cancel.is_cancelled() {
            info!("[CHAIN] [{}] cancelling concurrent forEach work", step.name);
            for handle in &handles {
                handle.abort();
            }
            break;
        }

        let Some(result) = result_rx.recv().await else {
            break;
        };
        remaining -= 1;

        match result.output {
            Ok(output) => {
                outputs[result.index] = output;
            }
            Err(e) => match error_strategy {
                ErrorStrategy::Abort | ErrorStrategy::Retry(_) => {
                    for handle in &handles {
                        handle.abort();
                    }
                    for handle in handles {
                        let _ = handle.await;
                    }
                    return Err(anyhow!("forEach abort at index {}: {e}", result.index));
                }
                _ => {
                    warn!(
                        "[CHAIN] [{}] {} FAILED (skip): {e}",
                        step.name, result.node_id
                    );
                    failures += 1;
                }
            },
        }

        if saves_node {
            *done += 1;
            send_progress(progress_tx, *done, total).await;
        }
    }

    for handle in handles {
        let _ = handle.await;
    }

    Ok((outputs, failures))
}

#[allow(clippy::too_many_arguments)]
async fn execute_for_each_work_item(
    step: &ChainStep,
    work: &ForEachPendingWork,
    ctx_snapshot: &ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
    reader: &Arc<Mutex<Connection>>,
) -> Result<Value> {
    let fallback_key = format!("{}-{}", step.name, work.index);
    let t0 = Instant::now();

    let analysis = dispatch_with_retry(
        step,
        &work.resolved_input,
        &work.system_prompt,
        defaults,
        dispatch_ctx,
        error_strategy,
        &fallback_key,
    )
    .await?;

    validate_step_output(step, &analysis)?;
    let elapsed = t0.elapsed().as_secs_f64();
    let decorated_output = decorate_step_output(analysis.clone(), &work.node_id, work.chunk_index);

    let output_json = serde_json::to_string(&decorated_output)?;
    send_save_step(
        writer_tx,
        &ctx_snapshot.slug,
        &step.name,
        work.chunk_index,
        work.depth,
        &work.node_id,
        &output_json,
        &dispatch_ctx.config.primary_model,
        elapsed,
    )
    .await;

    if saves_node {
        let mut node = build_node_from_output(
            &analysis,
            &work.node_id,
            &ctx_snapshot.slug,
            work.depth,
            Some(work.chunk_index),
        )?;

        let mut used_authoritative = false;
        let authoritative_children = {
            let extracted =
                resolve_authoritative_child_ids_with_db(&work.item, ctx_snapshot, reader).await?;
            if extracted.is_empty() {
                resolve_authoritative_child_ids_with_db(&work.resolved_input, ctx_snapshot, reader)
                    .await?
            } else {
                extracted
            }
        };
        if !authoritative_children.is_empty() {
            info!(
                "[CHAIN] [{}] {}: using {} authoritative child IDs (replacing {} LLM children)",
                step.name,
                work.node_id,
                authoritative_children.len(),
                node.children.len()
            );
            node.children = authoritative_children;
            used_authoritative = true;
        }

        if !used_authoritative {
            if let Some(assignments) = work.item.get("assignments").and_then(|v| v.as_array()) {
                if let Some(first_assignment) = assignments.first() {
                    let first_keys = first_assignment
                        .as_object()
                        .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    warn!(
                        "[CHAIN] [{}] {}: assignments present but no child IDs extracted; first_assignment={}; first_assignment_keys={:?}",
                        step.name,
                        work.node_id,
                        first_assignment,
                        first_keys,
                    );
                }
            }

            let has_valid_children = !node.children.is_empty()
                && node
                    .children
                    .iter()
                    .all(|c| c.contains("-L") || c.contains("-l"));
            if !has_valid_children {
                let item_keys = work
                    .item
                    .as_object()
                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let resolved_keys = work
                    .resolved_input
                    .as_object()
                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                warn!(
                    "[CHAIN] [{}] {}: no authoritative children in item/resolved_input; item_keys={:?}; resolved_keys={:?}; LLM children invalid ({:?})",
                    step.name,
                    work.node_id,
                    item_keys,
                    resolved_keys,
                    node.children.iter().take(3).collect::<Vec<_>>()
                );
                node.children = Vec::new();
            }
        }

        let topics_json =
            serde_json::to_string(analysis.get("topics").unwrap_or(&serde_json::json!([])))?;
        let child_ids = node.children.clone();
        send_save_node(writer_tx, node, Some(topics_json)).await;
        for child_id in &child_ids {
            send_update_parent(writer_tx, &ctx_snapshot.slug, child_id, &work.node_id).await;
        }
    }

    info!(
        "[CHAIN] [{}] {} complete ({elapsed:.1}s)",
        step.name, work.node_id
    );

    Ok(decorated_output)
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
        info!(
            "[CHAIN] pair_adjacent: {} node(s) at depth {source_depth}, nothing to pair",
            source_nodes.len()
        );
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
            reader,
            &ctx.slug,
            &step.name,
            -1,
            target_depth,
            &node_id,
            saves_node,
        )
        .await?;

        match resume {
            ResumeState::Complete => {
                info!("  [{}] {node_id} -- resumed (complete)", step.name);
                pair_idx += 1;
                i += 2;
                if saves_node {
                    *done += 1;
                    send_progress(progress_tx, *done, total).await;
                }
                if let Some(prior_output) = load_prior_step_output(
                    reader,
                    &ctx.slug,
                    &step.name,
                    -1,
                    target_depth,
                    &node_id,
                )
                .await?
                {
                    outputs.push(decorate_step_output(prior_output, &node_id, -1));
                } else {
                    warn!(
                        "[CHAIN] [{}] {} -- resume hit without saved output_json",
                        step.name, node_id
                    );
                    outputs.push(Value::Null);
                }
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
                step,
                ctx,
                dispatch_ctx,
                defaults,
                error_strategy,
                instruction,
                left,
                right,
                &node_id,
                target_depth,
                pair_idx,
                saves_node,
                writer_tx,
            )
            .await
            {
                Ok(analysis) => outputs.push(analysis),
                Err(e) => match error_strategy {
                    ErrorStrategy::Abort | ErrorStrategy::Retry(_) => {
                        return Err(anyhow!("pair_adjacent abort at pair {pair_idx}: {e}"));
                    }
                    ErrorStrategy::CarryLeft | ErrorStrategy::CarryUp => {
                        warn!(
                            "[CHAIN] [{}] pair {pair_idx} FAILED, carrying left node: {e}",
                            step.name
                        );
                        carry_node_up(
                            writer_tx,
                            left,
                            &node_id,
                            &ctx.slug,
                            target_depth,
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
                },
            }

            i += 2;
        } else {
            // Odd node: carry up without LLM call
            let carry = &source_nodes[i];
            info!(
                "[CHAIN] [{}] carry up odd node: {} -> {node_id}",
                step.name, carry.id
            );
            carry_node_up(
                writer_tx,
                carry,
                &node_id,
                &ctx.slug,
                target_depth,
                &[&carry.id],
            )
            .await;
            outputs.push(Value::Null);
            i += 1;
        }

        pair_idx += 1;
        if saves_node {
            *done += 1;
            send_progress(progress_tx, *done, total).await;
        }
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
        step,
        &resolved_input,
        &system_prompt,
        defaults,
        dispatch_ctx,
        error_strategy,
        &fallback_key,
    )
    .await?;

    validate_step_output(step, &analysis)?;
    let elapsed = t0.elapsed().as_secs_f64();
    let decorated_output = decorate_step_output(analysis.clone(), node_id, -1);

    // Save step
    let output_json = serde_json::to_string(&decorated_output)?;
    send_save_step(
        writer_tx,
        &ctx.slug,
        &step.name,
        -1,
        target_depth,
        node_id,
        &output_json,
        &dispatch_ctx.config.primary_model,
        elapsed,
    )
    .await;

    // Save node
    if saves_node {
        let mut node = build_node_from_output(&analysis, node_id, &ctx.slug, target_depth, None)?;
        node.children = vec![left.id.clone(), right.id.clone()];
        let topics_json =
            serde_json::to_string(analysis.get("topics").unwrap_or(&serde_json::json!([])))?;
        send_save_node(writer_tx, node, Some(topics_json)).await;

        send_update_parent(writer_tx, &ctx.slug, &left.id, node_id).await;
        send_update_parent(writer_tx, &ctx.slug, &right.id, node_id).await;
    }

    info!(
        "[CHAIN] [{} + {}] -> {node_id} ({elapsed:.1}s)",
        left.id, right.id
    );

    Ok(decorated_output)
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
                reader,
                &ctx.slug,
                &step.name,
                -1,
                target_depth,
                &node_id,
                saves_node,
            )
            .await?;

            match resume {
                ResumeState::Complete => {
                    pair_idx += 1;
                    i += 2;
                    if saves_node {
                        *done += 1;
                        send_progress(progress_tx, *done, total).await;
                    }
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
                    step,
                    ctx,
                    dispatch_ctx,
                    defaults,
                    error_strategy,
                    instruction,
                    left,
                    right,
                    &node_id,
                    target_depth,
                    pair_idx,
                    saves_node,
                    writer_tx,
                )
                .await
                {
                    Ok(_) => {}
                    Err(e) => match error_strategy {
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
                                writer_tx,
                                left,
                                &node_id,
                                &ctx.slug,
                                target_depth,
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
                    },
                }

                i += 2;
            } else {
                // Carry up odd node
                let carry = &current_nodes[i];
                info!(
                    "[CHAIN] [{}] carry up odd: {} -> {node_id}",
                    step.name, carry.id
                );
                carry_node_up(
                    writer_tx,
                    carry,
                    &node_id,
                    &ctx.slug,
                    target_depth,
                    &[&carry.id],
                )
                .await;
                i += 1;
            }

            pair_idx += 1;
            if saves_node {
                *done += 1;
                send_progress(progress_tx, *done, total).await;
            }
        }

        // Flush: wait for the async writer to commit all pending nodes at
        // target_depth before we read them back in the next iteration.
        // Without this, the DB read may see fewer nodes than were just created,
        // causing premature apex declaration.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        depth = target_depth;
    }
}

// ── Recursive cluster execution ─────────────────────────────────────────────

/// Execute a recursive clustering step: at each layer, LLM clusters current nodes
/// into 3-5 semantic groups, then synthesizes each group into a parent node.
/// Repeats until single apex.
#[allow(clippy::too_many_arguments)]
async fn execute_recursive_cluster(
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
    let mut depth = starting_depth;
    let mut failures: i32 = 0;

    let synthesis_instruction = step
        .instruction
        .as_deref()
        .unwrap_or("Synthesize these nodes.");
    let cluster_instruction = step
        .cluster_instruction
        .as_deref()
        .unwrap_or("Group these nodes into 3-5 semantic clusters.");

    loop {
        if cancel.is_cancelled() {
            return Ok((String::new(), failures));
        }

        // Read current nodes at this depth
        let slug_owned = ctx.slug.clone();
        let d = depth;
        let current_nodes: Vec<PyramidNode> = db_read(reader, move |conn| {
            db::get_nodes_at_depth(conn, &slug_owned, d)
        })
        .await?;

        info!(
            "[CHAIN] recursive_cluster depth {}: {} nodes",
            depth,
            current_nodes.len()
        );

        // Done — single node = apex
        if current_nodes.len() <= 1 {
            let apex_id = current_nodes
                .first()
                .map(|n| n.id.clone())
                .unwrap_or_default();
            if !apex_id.is_empty() {
                info!("[CHAIN] === APEX: {apex_id} at depth {depth} ===");
            }
            return Ok((apex_id, failures));
        }

        let target_depth = depth + 1;

        // Check if target depth already has nodes (resume)
        let slug_owned = ctx.slug.clone();
        let td = target_depth;
        let existing: i64 = db_read(reader, move |conn| {
            db::count_nodes_at_depth(conn, &slug_owned, td)
        })
        .await?;

        if existing > 0 {
            let slug_owned = ctx.slug.clone();
            let td = target_depth;
            let target_nodes: Vec<PyramidNode> = db_read(reader, move |conn| {
                db::get_nodes_at_depth(conn, &slug_owned, td)
            })
            .await?;
            if recursive_cluster_layer_complete(&current_nodes, &target_nodes) {
                info!("[CHAIN] depth {target_depth}: {existing} nodes (already complete)");
                depth = target_depth;
                continue;
            }

            warn!(
                "[CHAIN] [{}] detected partial recursive_cluster state at depth {} ({} target nodes exist but not all depth {} nodes point to them); cleaning up depth >= {} and rebuilding",
                step.name,
                target_depth,
                existing,
                depth,
                target_depth
            );
            cleanup_from_depth(&dispatch_ctx.db_writer, &ctx.slug, target_depth).await?;
        }

        // ≤4 nodes: synthesize directly into apex without clustering
        if current_nodes.len() <= 4 {
            info!(
                "[CHAIN] [{}] direct synthesis: {} nodes → apex at depth {}",
                step.name,
                current_nodes.len(),
                target_depth
            );
            let node_id = generate_node_id(
                step.node_id_pattern
                    .as_deref()
                    .unwrap_or("L{depth}-{index:03}"),
                0,
                Some(target_depth),
            );
            let result = dispatch_group(
                step,
                ctx,
                dispatch_ctx,
                defaults,
                error_strategy,
                synthesis_instruction,
                &current_nodes,
                &node_id,
                target_depth,
                0,
                Some(serde_json::json!({
                    "merge_mode": "direct_apex",
                    "child_ids": current_nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>(),
                    "child_headlines": current_nodes.iter().map(|node| node.headline.clone()).collect::<Vec<_>>(),
                })),
                saves_node,
                writer_tx,
            )
            .await;

            match result {
                Ok(_) => {
                    if saves_node {
                        *done += 1;
                        send_progress(progress_tx, *done, total).await;
                    }
                }
                Err(e) => {
                    if matches!(
                        error_strategy,
                        ErrorStrategy::Abort | ErrorStrategy::Retry(_)
                    ) {
                        return Err(anyhow!(
                            "[{}] direct synthesis FAILED at depth {}: {}",
                            step.name,
                            target_depth,
                            e
                        ));
                    }
                    warn!("[CHAIN] [{}] direct synthesis FAILED: {e}", step.name);
                    failures += 1;
                }
            }

            // Flush writer
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            info!("[CHAIN] === APEX: {node_id} at depth {target_depth} ===");
            return Ok((node_id, failures));
        }

        // Step A: CLUSTER — ask LLM to group current nodes into semantic clusters
        info!(
            "[CHAIN] [{}] clustering {} nodes at depth {} → depth {}",
            step.name,
            current_nodes.len(),
            depth,
            target_depth
        );

        let cluster_input: Vec<serde_json::Value> = current_nodes
            .iter()
            .map(|n| {
                let topic_names: Vec<String> = n.topics.iter().map(|t| t.name.clone()).collect();
                serde_json::json!({
                    "node_id": n.id,
                    "headline": n.headline,
                    "orientation": if n.distilled.len() > 500 {
                        format!("{}...", &n.distilled[..500])
                    } else {
                        n.distilled.clone()
                    },
                    "topics": topic_names,
                })
            })
            .collect();

        let cluster_input_value = serde_json::json!(cluster_input);

        // Build a temporary step-like config for the clustering LLM call
        let cluster_model = step.cluster_model.clone().or_else(|| step.model.clone());
        let mut cluster_step = step.clone();
        cluster_step.model = cluster_model;
        // Use cluster_response_schema if available for structured output
        cluster_step.response_schema = step.cluster_response_schema.clone();

        let cluster_system =
            match resolve_prompt_template(cluster_instruction, &cluster_input_value) {
                Ok(s) => s,
                Err(_) => cluster_instruction.to_string(),
            };

        let cluster_result = dispatch_with_retry(
            &cluster_step,
            &cluster_input_value,
            &cluster_system,
            defaults,
            dispatch_ctx,
            &ErrorStrategy::Retry(3),
            &format!("{}-cluster-d{target_depth}", step.name),
        )
        .await;

        let cluster_assignments = match cluster_result {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "[CHAIN] [{}] clustering FAILED at depth {}, falling back to positional groups of 3: {e}",
                    step.name, depth
                );
                // Fallback: chunk into groups of 3
                let mut fallback_clusters = Vec::new();
                for (i, chunk) in current_nodes.chunks(3).enumerate() {
                    let ids: Vec<String> = chunk.iter().map(|n| n.id.clone()).collect();
                    fallback_clusters.push(serde_json::json!({
                        "name": format!("Group {}", i + 1),
                        "description": "Positional fallback group",
                        "node_ids": ids,
                    }));
                }
                serde_json::json!({ "clusters": fallback_clusters })
            }
        };

        // Parse cluster assignments — try "clusters" key first, fall back to "groups"
        let clusters = cluster_assignments
            .get("clusters")
            .or_else(|| cluster_assignments.get("groups"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if clusters.is_empty() {
            let keys: Vec<&String> = cluster_assignments
                .as_object()
                .map(|o| o.keys().collect())
                .unwrap_or_default();
            let raw_preview: String = serde_json::to_string(&cluster_assignments)
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            warn!("[CHAIN] [{}] clustering returned 0 clusters. Keys: {:?}. Raw JSON (first 500 chars): {}", step.name, keys, raw_preview);
        }

        // If clustering returned 0 clusters (LLM returned wrong key or empty array),
        // fall back to positional groups of 3 instead of aborting
        let mut clusters = if clusters.is_empty() {
            warn!("[CHAIN] [{}] clustering returned 0 clusters, falling back to positional groups of 3", step.name);
            let mut fallback: Vec<serde_json::Value> = Vec::new();
            for (i, chunk) in current_nodes.chunks(3).enumerate() {
                let ids: Vec<String> = chunk.iter().map(|n| n.id.clone()).collect();
                fallback.push(serde_json::json!({
                    "name": format!("Group {}", i + 1),
                    "description": "Positional fallback group",
                    "node_ids": ids,
                }));
            }
            fallback
        } else {
            clusters
        };

        info!(
            "[CHAIN] [{}] clustering produced {} clusters",
            step.name,
            clusters.len()
        );

        // Validate: check all current nodes are assigned
        let mut assigned_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for cluster in &clusters {
            if let Some(ids) = cluster.get("node_ids").and_then(|v| v.as_array()) {
                for id in ids {
                    if let Some(s) = id.as_str() {
                        assigned_ids.insert(s.to_string());
                    }
                }
            }
        }
        let missing: Vec<&str> = current_nodes
            .iter()
            .filter(|n| !assigned_ids.contains(&n.id))
            .map(|n| n.id.as_str())
            .collect();
        if !missing.is_empty() {
            warn!(
                "[CHAIN] [{}] clustering missed {} nodes: {:?}",
                step.name,
                missing.len(),
                missing
            );
            for missing_id in &missing {
                if let Some((target_idx, _)) =
                    clusters.iter().enumerate().min_by_key(|(_, cluster)| {
                        cluster
                            .get("node_ids")
                            .and_then(|value| value.as_array())
                            .map(|ids| ids.len())
                            .unwrap_or(usize::MAX)
                    })
                {
                    if let Some(node_ids) = clusters[target_idx]
                        .get_mut("node_ids")
                        .and_then(|value| value.as_array_mut())
                    {
                        node_ids.push(Value::String((*missing_id).to_string()));
                    }
                }
            }
            info!(
                "[CHAIN] [{}] repaired clustering by reassigning missing nodes into existing clusters",
                step.name
            );
        }

        // Step B: SYNTHESIZE — for each cluster, synthesize assigned nodes into one parent
        for (cluster_idx, cluster) in clusters.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok((String::new(), failures));
            }

            let cluster_name = cluster
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unnamed");

            let cluster_node_ids: Vec<String> = cluster
                .get("node_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Gather the actual nodes for this cluster
            let cluster_nodes: Vec<&PyramidNode> = cluster_node_ids
                .iter()
                .filter_map(|id| current_nodes.iter().find(|n| n.id == *id))
                .collect();

            if cluster_nodes.is_empty() {
                warn!(
                    "[CHAIN] [{}] cluster '{}' has no valid nodes, skipping",
                    step.name, cluster_name
                );
                continue;
            }

            let node_id = generate_node_id(
                step.node_id_pattern
                    .as_deref()
                    .unwrap_or("L{depth}-{index:03}"),
                cluster_idx,
                Some(target_depth),
            );

            // Resume check
            let resume = get_resume_state(
                reader,
                &ctx.slug,
                &step.name,
                -1,
                target_depth,
                &node_id,
                saves_node,
            )
            .await?;
            match resume {
                ResumeState::Complete => {
                    info!("[CHAIN] [{}] {node_id} -- resumed (complete)", step.name);
                    if saves_node {
                        *done += 1;
                        send_progress(progress_tx, *done, total).await;
                    }
                    continue;
                }
                ResumeState::StaleStep => {
                    warn!("[CHAIN] [{}] {node_id} -- stale, rebuilding", step.name);
                }
                ResumeState::Missing => {}
            }

            info!(
                "[CHAIN] [{}] synthesizing cluster '{}' ({} nodes) → {node_id}",
                step.name,
                cluster_name,
                cluster_nodes.len()
            );

            let owned_nodes: Vec<PyramidNode> =
                cluster_nodes.iter().map(|n| (*n).clone()).collect();
            let sibling_clusters: Vec<Value> = clusters
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != cluster_idx)
                .map(|(_, sibling)| {
                    serde_json::json!({
                        "name": sibling.get("name").and_then(|value| value.as_str()).unwrap_or("Unnamed"),
                        "description": sibling.get("description").and_then(|value| value.as_str()).unwrap_or(""),
                        "node_ids": sibling.get("node_ids").cloned().unwrap_or_else(|| serde_json::json!([])),
                    })
                })
                .collect();
            let result = dispatch_group(
                step,
                ctx,
                dispatch_ctx,
                defaults,
                error_strategy,
                synthesis_instruction,
                &owned_nodes,
                &node_id,
                target_depth,
                cluster_idx,
                Some(serde_json::json!({
                    "cluster_name": cluster_name,
                    "cluster_description": cluster.get("description").and_then(|value| value.as_str()).unwrap_or(""),
                    "cluster_node_ids": cluster_node_ids,
                    "child_headlines": owned_nodes.iter().map(|node| node.headline.clone()).collect::<Vec<_>>(),
                    "sibling_clusters": sibling_clusters,
                    "headline_constraints": {
                        "must_be_distinct_from_siblings": true,
                        "avoid_project_name_repetition": true,
                        "prefer_architectural_domain_naming": true
                    }
                })),
                saves_node,
                writer_tx,
            )
            .await;

            match result {
                Ok(_) => {
                    *done += 1;
                    send_progress(progress_tx, *done, total).await;
                }
                Err(e) => {
                    if matches!(
                        error_strategy,
                        ErrorStrategy::Abort | ErrorStrategy::Retry(_)
                    ) {
                        return Err(anyhow!(
                            "[{}] cluster '{}' synthesis FAILED at depth {}: {}",
                            step.name,
                            cluster_name,
                            target_depth,
                            e
                        ));
                    }
                    warn!(
                        "[CHAIN] [{}] cluster '{}' synthesis FAILED: {e}",
                        step.name, cluster_name
                    );
                    failures += 1;
                }
            }
        }

        // Flush writer before reading next layer
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        depth = target_depth;
    }
}

/// Dispatch synthesis for a group of N nodes (generalized dispatch_pair).
#[allow(clippy::too_many_arguments)]
async fn dispatch_group(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    instruction: &str,
    nodes: &[PyramidNode],
    node_id: &str,
    target_depth: i64,
    group_idx: usize,
    extra_input: Option<Value>,
    saves_node: bool,
    writer_tx: &mpsc::Sender<WriteOp>,
) -> Result<Value> {
    // Build input: array of child payloads with headers
    let mut sections = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        sections.push(format!(
            "## CHILD NODE {}: \"{}\"\n{}",
            i + 1,
            node.headline,
            serde_json::to_string_pretty(&child_payload_json(node))?
        ));
    }
    let combined_input = sections.join("\n\n");

    let mut resolved_input_map = serde_json::Map::new();
    resolved_input_map.insert("children".to_string(), Value::String(combined_input));
    resolved_input_map.insert(
        "child_count".to_string(),
        Value::Number(serde_json::Number::from(nodes.len() as u64)),
    );
    if let Some(extra_input) = extra_input {
        match extra_input {
            Value::Object(map) => {
                for (key, value) in map {
                    resolved_input_map.insert(key, value);
                }
            }
            other => {
                resolved_input_map.insert("context".to_string(), other);
            }
        }
    }
    let resolved_input = Value::Object(resolved_input_map);

    let system_prompt = match resolve_prompt_template(instruction, &resolved_input) {
        Ok(s) => s,
        Err(_) => instruction.to_string(),
    };

    let fallback_key = format!("{}-d{target_depth}-g{group_idx}", step.name);
    let t0 = Instant::now();

    let analysis = dispatch_with_retry(
        step,
        &resolved_input,
        &system_prompt,
        defaults,
        dispatch_ctx,
        error_strategy,
        &fallback_key,
    )
    .await?;

    validate_step_output(step, &analysis)?;
    let elapsed = t0.elapsed().as_secs_f64();
    let decorated_output = decorate_step_output(analysis.clone(), node_id, -1);

    // Save step
    let output_json = serde_json::to_string(&decorated_output)?;
    send_save_step(
        writer_tx,
        &ctx.slug,
        &step.name,
        -1,
        target_depth,
        node_id,
        &output_json,
        &dispatch_ctx.config.primary_model,
        elapsed,
    )
    .await;

    // Save node
    if saves_node {
        let mut node = build_node_from_output(&analysis, node_id, &ctx.slug, target_depth, None)?;
        node.children = nodes.iter().map(|n| n.id.clone()).collect();
        let topics_json =
            serde_json::to_string(analysis.get("topics").unwrap_or(&serde_json::json!([])))?;
        send_save_node(writer_tx, node, Some(topics_json)).await;

        // Update parent pointers for all children
        for child in nodes {
            send_update_parent(writer_tx, &ctx.slug, &child.id, node_id).await;
        }
    }

    let child_ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    info!("[CHAIN] [{:?}] -> {node_id} ({elapsed:.1}s)", child_ids);

    Ok(decorated_output)
}

// ── Web step execution ──────────────────────────────────────────────────────

async fn execute_web_step(
    step: &ChainStep,
    ctx: &mut ChainContext,
    dispatch_ctx: &chain_dispatch::StepContext,
    defaults: &super::chain_engine::ChainDefaults,
    error_strategy: &ErrorStrategy,
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
) -> Result<Value> {
    let depth = step.depth.unwrap_or(1);
    let synthetic_id = format!("WEB-L{depth}");

    let resume = get_resume_state(
        reader,
        &ctx.slug,
        &step.name,
        -1,
        depth,
        &synthetic_id,
        false,
    )
    .await?;

    if resume == ResumeState::Complete {
        info!(
            "[CHAIN] [{}] {} -- resumed (complete)",
            step.name, synthetic_id
        );
        if let Some(prior_output) =
            load_prior_step_output(reader, &ctx.slug, &step.name, -1, depth, &synthetic_id).await?
        {
            return Ok(prior_output);
        }
        return Ok(serde_json::json!({ "edges": [] }));
    }

    let instruction = step.instruction.as_deref().unwrap_or("");
    let resolved_input = if let Some(ref input) = step.input {
        ctx.resolve_value(input)?
    } else {
        Value::Object(serde_json::Map::new())
    };

    let explicit_node_ids = extract_explicit_web_node_ids(&resolved_input);
    let nodes = load_nodes_for_webbing(reader, &ctx.slug, depth, &explicit_node_ids).await?;
    if explicit_node_ids.len() > 1 && nodes.len() < explicit_node_ids.len() {
        return Err(anyhow!(
            "Web step '{}' expected {} node(s) at depth {}, but only {} were available",
            step.name,
            explicit_node_ids.len(),
            depth,
            nodes.len()
        ));
    }

    let normalized_edges = if nodes.len() >= 2 {
        let web_input = build_webbing_input(&nodes, depth, &resolved_input);
        let system_prompt = match resolve_prompt_template(instruction, &web_input) {
            Ok(s) => s,
            Err(_) => instruction.to_string(),
        };
        let fallback_key = format!("{}-d{depth}", step.name);
        let analysis = dispatch_with_retry(
            step,
            &web_input,
            &system_prompt,
            defaults,
            dispatch_ctx,
            error_strategy,
            &fallback_key,
        )
        .await?;
        validate_step_output(step, &analysis)?;
        parse_web_edges(&step.name, &analysis, &nodes)
    } else {
        Vec::new()
    };

    let saved_edge_count =
        persist_web_edges_for_depth(writer, &ctx.slug, depth, &normalized_edges).await?;

    let edges_json: Vec<Value> = normalized_edges
        .iter()
        .map(|edge| {
            serde_json::json!({
                "source": edge.source_node_id,
                "target": edge.target_node_id,
                "relationship": edge.relationship,
                "strength": edge.strength,
            })
        })
        .collect();
    let output = serde_json::json!({
        "edges": edges_json,
        "webbed_depth": depth,
        "node_count": nodes.len(),
        "saved_edge_count": saved_edge_count,
    });

    let output_json = serde_json::to_string(&output)?;
    let save_slug = ctx.slug.clone();
    let save_step_name = step.name.clone();
    let save_synthetic_id = synthetic_id.clone();
    let save_model = dispatch_ctx.config.primary_model.clone();
    let writer = writer.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = writer.blocking_lock();
        db::save_step(
            &conn,
            &save_slug,
            &save_step_name,
            -1,
            depth,
            &save_synthetic_id,
            &output_json,
            &save_model,
            0.0,
        )
    })
    .await??;

    info!(
        "[CHAIN] [{}] depth {} webbing complete ({} nodes, {} edges)",
        step.name,
        depth,
        nodes.len(),
        saved_edge_count
    );

    Ok(output)
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
        if saves_node {
            *done += 1;
            send_progress(progress_tx, *done, total).await;
        }

        if let Some(prior_output) =
            load_prior_step_output(reader, &ctx.slug, &step.name, -1, depth, &node_id).await?
        {
            return Ok(decorate_step_output(prior_output, &node_id, -1));
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
        step,
        &resolved_input,
        &system_prompt,
        defaults,
        dispatch_ctx,
        error_strategy,
        &fallback_key,
    )
    .await?;

    validate_step_output(step, &analysis)?;
    let elapsed = t0.elapsed().as_secs_f64();
    let decorated_output = decorate_step_output(analysis.clone(), &node_id, -1);

    // Save step
    let output_json = serde_json::to_string(&decorated_output)?;
    send_save_step(
        writer_tx,
        &ctx.slug,
        &step.name,
        -1,
        depth,
        &node_id,
        &output_json,
        &dispatch_ctx.config.primary_model,
        elapsed,
    )
    .await;

    // Save node if configured
    if saves_node {
        let node = build_node_from_output(&analysis, &node_id, &ctx.slug, depth, None)?;
        let topics_json =
            serde_json::to_string(analysis.get("topics").unwrap_or(&serde_json::json!([])))?;
        send_save_node(writer_tx, node, Some(topics_json)).await;
    }

    if saves_node {
        *done += 1;
        send_progress(progress_tx, *done, total).await;
    }

    info!("[CHAIN] [{}] {node_id} complete ({elapsed:.1}s)", step.name);

    Ok(decorated_output)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::chain_engine::{ChainDefaults, ChainDefinition};
    use serde_json::json;

    fn test_step(name: &str) -> ChainStep {
        ChainStep {
            name: name.to_string(),
            primitive: "synthesize".to_string(),
            instruction: None,
            mechanical: false,
            rust_function: None,
            input: None,
            output_schema: None,
            model_tier: None,
            model: None,
            temperature: None,
            sequential: false,
            accumulate: None,
            for_each: None,
            concurrency: 1,
            pair_adjacent: false,
            recursive_pair: false,
            recursive_cluster: false,
            cluster_instruction: None,
            cluster_model: None,
            cluster_response_schema: None,
            target_clusters: None,
            response_schema: None,
            batch_threshold: None,
            merge_instruction: None,
            when: None,
            on_error: None,
            save_as: None,
            node_id_pattern: None,
            depth: None,
        }
    }

    #[test]
    fn test_extract_authoritative_child_ids_prefers_assignments_and_dedupes() {
        let item = json!({
            "assignments": [
                {"source_node": "C-L0-5"},
                {"source_node": "C-L0-005"},
                {"source_node": "not-a-node"}
            ],
            "node_ids": ["L1-000"]
        });

        assert_eq!(extract_authoritative_child_ids(&item), vec!["C-L0-005"]);
    }

    #[test]
    fn test_extract_authoritative_child_ids_accepts_string_and_alt_key_assignments() {
        let string_item = json!({
            "assignments": [
                "C-L0-000",
                "{\"sourceNode\":\"C-L0-001\"}"
            ]
        });
        let alt_key_item = json!({
            "assignments": [
                {"sourceNode": "C-L0-002"},
                {"node_id": "C-L0-003"},
                {"unexpected": "C-L0-004"}
            ]
        });

        assert_eq!(
            extract_authoritative_child_ids(&string_item),
            vec!["C-L0-000", "C-L0-001"]
        );
        assert_eq!(
            extract_authoritative_child_ids(&alt_key_item),
            vec!["C-L0-002", "C-L0-003", "C-L0-004"]
        );
    }

    #[test]
    fn test_estimate_total_uses_dynamic_for_each_counts() {
        let chain = ChainDefinition {
            schema_version: 1,
            id: "code-default".to_string(),
            name: "Code Pyramid".to_string(),
            description: String::new(),
            content_type: "code".to_string(),
            version: "1".to_string(),
            author: "test".to_string(),
            defaults: ChainDefaults {
                model_tier: "mid".to_string(),
                model: None,
                temperature: 0.3,
                on_error: "retry(2)".to_string(),
            },
            steps: vec![
                {
                    let mut step = test_step("l0_code_extract");
                    step.for_each = Some("$chunks".to_string());
                    step.save_as = Some("node".to_string());
                    step.depth = Some(0);
                    step
                },
                test_step("thread_clustering"),
                {
                    let mut step = test_step("thread_narrative");
                    step.for_each = Some("$thread_clustering.threads".to_string());
                    step.save_as = Some("node".to_string());
                    step.depth = Some(1);
                    step
                },
            ],
            post_build: vec![],
        };

        let mut ctx = ChainContext::new("slug", "code", vec![]);
        assert_eq!(estimate_total(&chain, &ctx, 112), 112);

        ctx.step_outputs.insert(
            "thread_clustering".to_string(),
            json!({
                "threads": vec![json!({}); 10]
            }),
        );

        assert_eq!(estimate_total(&chain, &ctx, 112), 122);
    }

    #[test]
    fn test_resolve_authoritative_child_ids_maps_headlines_back_to_l0_ids() {
        let mut ctx = ChainContext::new("slug", "code", vec![]);
        ctx.step_outputs.insert(
            "l0_code_extract".to_string(),
            json!([
                {
                    "node_id": "C-L0-000",
                    "source_node": "C-L0-000",
                    "headline": "MCP Server Package Config",
                    "orientation": "Configures MCP package metadata",
                    "topics": []
                },
                {
                    "node_id": "C-L0-001",
                    "source_node": "C-L0-001",
                    "headline": "Chain Executor",
                    "orientation": "Executes chains",
                    "topics": []
                }
            ]),
        );

        let item = json!({
            "assignments": [
                {
                    "source_node": "MCP Server Package Config",
                    "topic_index": 0,
                    "topic_name": "MCP Server Package Config"
                }
            ]
        });

        assert_eq!(
            resolve_authoritative_child_ids(&item, &ctx),
            vec!["C-L0-000"]
        );
    }

    #[test]
    fn test_resolve_authoritative_child_ids_uses_topic_index_to_break_headline_ties() {
        let mut ctx = ChainContext::new("slug", "code", vec![]);
        ctx.step_outputs.insert(
            "l0_code_extract".to_string(),
            json!([
                {
                    "node_id": "C-L0-000",
                    "source_node": "C-L0-000",
                    "headline": "mod.rs",
                    "orientation": "Root module",
                    "topics": []
                },
                {
                    "node_id": "C-L0-001",
                    "source_node": "C-L0-001",
                    "headline": "mod.rs",
                    "orientation": "Nested module",
                    "topics": []
                }
            ]),
        );

        let item = json!({
            "assignments": [
                {
                    "source_node": "mod.rs",
                    "topic_index": 1,
                    "topic_name": "mod.rs"
                }
            ]
        });

        assert_eq!(
            resolve_authoritative_child_ids(&item, &ctx),
            vec!["C-L0-001"]
        );
    }

    #[test]
    fn test_enrich_group_item_input_hydrates_child_analyses() {
        let mut ctx = ChainContext::new("slug", "code", vec![]);
        ctx.step_outputs.insert(
            "forward_pass".to_string(),
            json!([
                {"running_context": "ignore me"},
                {"running_context": "ignore me too"}
            ]),
        );
        ctx.step_outputs.insert(
            "l0_code_extract".to_string(),
            json!([
                {
                    "node_id": "C-L0-000",
                    "source_node": "C-L0-000",
                    "headline": "Chain Executor",
                    "orientation": "Executes chains",
                    "topics": []
                },
                {
                    "node_id": "C-L0-001",
                    "source_node": "C-L0-001",
                    "headline": "Chain Dispatch",
                    "orientation": "Dispatches steps",
                    "topics": []
                }
            ]),
        );

        let item = json!({
            "name": "Runtime",
            "description": "Chain runtime files",
            "assignments": [
                {"source_node": "Chain Executor", "topic_index": 0, "topic_name": "Chain Executor"},
                {"source_node": "Chain Dispatch", "topic_index": 1, "topic_name": "Chain Dispatch"}
            ]
        });

        let enriched = enrich_group_item_input(&item, &ctx);
        let source_nodes = enriched
            .get("source_nodes")
            .and_then(|value| value.as_array())
            .unwrap();
        let source_analyses = enriched
            .get("source_analyses")
            .and_then(|value| value.as_array())
            .unwrap();
        let assigned_items = enriched
            .get("assigned_items")
            .and_then(|value| value.as_array())
            .unwrap();

        assert_eq!(source_nodes.len(), 2);
        assert_eq!(source_analyses.len(), 2);
        assert_eq!(
            source_analyses[0]
                .get("analysis")
                .and_then(|value| value.get("headline"))
                .and_then(|value| value.as_str()),
            Some("Chain Executor")
        );
        assert!(assigned_items[0].get("analysis").is_some());
        assert!(assigned_items[1].get("analysis").is_some());
    }

    #[test]
    fn test_normalize_context_ref_accepts_bare_and_dollar_refs() {
        assert_eq!(normalize_context_ref("chunks"), "$chunks");
        assert_eq!(
            normalize_context_ref("$thread_clustering.threads"),
            "$thread_clustering.threads"
        );
    }

    #[test]
    fn test_extract_explicit_web_node_ids_ignores_child_source_nodes() {
        let resolved_input = json!({
            "nodes": [
                {
                    "node_id": "L1-000",
                    "source_nodes": ["C-L0-000", "C-L0-001"]
                },
                {
                    "source_node": "L1-001",
                    "source_nodes": ["C-L0-002"]
                }
            ]
        });

        assert_eq!(
            extract_explicit_web_node_ids(&resolved_input),
            vec!["L1-000", "L1-001"]
        );
    }

    #[test]
    fn test_parse_web_edges_resolves_headlines_and_dedupes_pairs() {
        let nodes = vec![
            PyramidNode {
                id: "L1-000".to_string(),
                slug: "s".to_string(),
                depth: 1,
                chunk_index: None,
                headline: "Build Engine".to_string(),
                distilled: "Builds the pyramid".to_string(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            },
            PyramidNode {
                id: "L1-001".to_string(),
                slug: "s".to_string(),
                depth: 1,
                chunk_index: None,
                headline: "Desktop UI".to_string(),
                distilled: "Shows build state".to_string(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            },
        ];

        let output = json!({
            "edges": [
                {
                    "source": "Build Engine",
                    "target": "Desktop UI",
                    "relationship": "UI monitors executor progress.",
                    "shared_resources": ["ipc: build_progress"],
                    "strength": 0.82
                },
                {
                    "source": "L1-001",
                    "target": "L1-000",
                    "relationship": "Duplicate weaker edge",
                    "shared_resources": [],
                    "strength": 0.40
                }
            ]
        });

        let edges = parse_web_edges("l1_webbing", &output, &nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source_node_id, "L1-000");
        assert_eq!(edges[0].target_node_id, "L1-001");
        assert!(edges[0].relationship.contains("ipc: build_progress"));
        assert_eq!(edges[0].strength, 0.82);
    }

    #[test]
    fn test_recursive_cluster_layer_complete_requires_parent_coverage() {
        let current_nodes = vec![
            PyramidNode {
                id: "L1-000".to_string(),
                slug: "s".to_string(),
                depth: 1,
                chunk_index: None,
                headline: "A".to_string(),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: Some("L2-000".to_string()),
                superseded_by: None,
                created_at: String::new(),
            },
            PyramidNode {
                id: "L1-001".to_string(),
                slug: "s".to_string(),
                depth: 1,
                chunk_index: None,
                headline: "B".to_string(),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: Some("L2-001".to_string()),
                superseded_by: None,
                created_at: String::new(),
            },
        ];
        let target_nodes = vec![
            PyramidNode {
                id: "L2-000".to_string(),
                slug: "s".to_string(),
                depth: 2,
                chunk_index: None,
                headline: "A".to_string(),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            },
            PyramidNode {
                id: "L2-001".to_string(),
                slug: "s".to_string(),
                depth: 2,
                chunk_index: None,
                headline: "B".to_string(),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            },
        ];
        let mut partial_current_nodes = current_nodes.clone();
        partial_current_nodes[1].parent_id = None;

        assert!(recursive_cluster_layer_complete(
            &current_nodes,
            &target_nodes
        ));
        assert!(!recursive_cluster_layer_complete(
            &partial_current_nodes,
            &target_nodes
        ));
    }
}
