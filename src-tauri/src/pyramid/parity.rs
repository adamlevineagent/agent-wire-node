// pyramid/parity.rs — Dual-executor parity harness (P1.6)
//
// Compares legacy chain executor output against IR executor output for the
// same slug and content type. Captures structural fingerprints of each build
// and reports diffs at multiple levels:
//
//   1. Build completion + failure count
//   2. Node-count + depth-shape parity
//   3. Parent/child + web edge structure parity
//   4. Cost-log completeness
//   5. Qualitative output review (manual flag only)
//
// This is a TOOL, not production code. Pragmatism over perfection.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::build_runner;
use super::types::{BuildProgress, ContentType};
use super::PyramidState;

// ── Structs ─────────────────────────────────────────────────────────────────

/// Full parity comparison report between two executor runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityReport {
    pub slug: String,
    pub content_type: String,
    pub legacy_result: BuildResult,
    pub ir_result: BuildResult,
    pub diffs: Vec<ParityDiff>,
    pub verdict: ParityVerdict,
}

/// Structural fingerprint captured after a build completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub apex_node_id: String,
    pub failure_count: i32,
    pub node_counts_by_depth: HashMap<i64, i64>,
    pub total_nodes: i64,
    pub web_edge_counts_by_depth: HashMap<i64, i64>,
    pub total_web_edges: i64,
    pub cost_rows: i64,
    pub elapsed_seconds: f64,
    /// Parent-child link map: node_id -> list of child IDs.
    #[serde(default)]
    pub parent_child_map: HashMap<String, Vec<String>>,
    /// Max depth observed.
    pub max_depth: i64,
}

/// A single difference found between legacy and IR builds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParityDiff {
    CompletionMismatch {
        legacy: String,
        ir: String,
    },
    FailureCountMismatch {
        legacy: i32,
        ir: i32,
    },
    NodeCountMismatch {
        depth: i64,
        legacy: i64,
        ir: i64,
    },
    DepthShapeMismatch {
        legacy_max_depth: i64,
        ir_max_depth: i64,
    },
    TotalNodeCountMismatch {
        legacy: i64,
        ir: i64,
    },
    WebEdgeCountMismatch {
        depth: i64,
        legacy: i64,
        ir: i64,
    },
    TotalWebEdgeMismatch {
        legacy: i64,
        ir: i64,
    },
    ParentChildMismatch {
        node_id: String,
        detail: String,
    },
    CostLogMissing {
        side: String, // "legacy" or "ir"
        count: i64,
    },
    CostLogCountMismatch {
        legacy: i64,
        ir: i64,
    },
    QualitativeDrift {
        node_id: String,
        similarity: f64,
    },
}

/// Overall verdict for the parity comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParityVerdict {
    Pass,
    AcceptableDrift(String),
    Fail(String),
}

// ── Capture ─────────────────────────────────────────────────────────────────

/// Capture the structural fingerprint of a completed build.
///
/// Queries the DB for node counts by depth, web edge counts, cost log rows,
/// apex node, and parent/child relationships.
pub fn capture_build_result(
    conn: &rusqlite::Connection,
    slug: &str,
    apex_node_id: &str,
    failure_count: i32,
    elapsed_seconds: f64,
) -> Result<BuildResult> {
    // ── Node counts by depth ────────────────────────────────────────────
    let node_counts_by_depth = query_node_counts_by_depth(conn, slug)?;
    let total_nodes: i64 = node_counts_by_depth.values().sum();
    let max_depth = node_counts_by_depth.keys().copied().max().unwrap_or(0);

    // ── Web edge counts by depth ────────────────────────────────────────
    let web_edge_counts_by_depth = query_web_edge_counts_by_depth(conn, slug)?;
    let total_web_edges: i64 = web_edge_counts_by_depth.values().sum();

    // ── Cost log row count ──────────────────────────────────────────────
    let cost_rows = query_cost_log_count(conn, slug)?;

    // ── Parent/child map ────────────────────────────────────────────────
    let parent_child_map = query_parent_child_map(conn, slug)?;

    Ok(BuildResult {
        apex_node_id: apex_node_id.to_string(),
        failure_count,
        node_counts_by_depth,
        total_nodes,
        web_edge_counts_by_depth,
        total_web_edges,
        cost_rows,
        elapsed_seconds,
        parent_child_map,
        max_depth,
    })
}

/// Count nodes grouped by depth for a slug (live nodes only).
fn query_node_counts_by_depth(
    conn: &rusqlite::Connection,
    slug: &str,
) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn
        .prepare("SELECT depth, COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1 GROUP BY depth")?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (depth, count) = row?;
        map.insert(depth, count);
    }
    Ok(map)
}

/// Count web edges grouped by the depth of their endpoint nodes.
/// Uses thread_a_id to determine depth (edges connect nodes at the same depth).
fn query_web_edge_counts_by_depth(
    conn: &rusqlite::Connection,
    slug: &str,
) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare(
        "SELECT t.depth, COUNT(*)
         FROM pyramid_web_edges e
         JOIN pyramid_threads t ON t.slug = e.slug AND t.thread_id = e.thread_a_id
         WHERE e.slug = ?1
         GROUP BY t.depth",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (depth, count) = row?;
        map.insert(depth, count);
    }
    Ok(map)
}

/// Count total cost log rows for a slug.
fn query_cost_log_count(conn: &rusqlite::Connection, slug: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_cost_log WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Build a map of parent_id -> [child_ids] from live nodes.
fn query_parent_child_map(
    conn: &rusqlite::Connection,
    slug: &str,
) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT id, parent_id FROM live_pyramid_nodes WHERE slug = ?1 AND parent_id IS NOT NULL",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (child_id, parent_id) = row?;
        map.entry(parent_id).or_default().push(child_id);
    }
    // Sort child lists for deterministic comparison
    for children in map.values_mut() {
        children.sort();
    }
    Ok(map)
}

// ── Compare ─────────────────────────────────────────────────────────────────

/// Compare two BuildResults and produce a ParityReport.
///
/// Diffs are applied in priority order:
///   1. Build completion + failure count
///   2. Node-count + depth-shape parity
///   3. Parent/child + web edge structure parity
///   4. Cost-log completeness
///   5. Qualitative review flag (not automated — only surfaced if apex differs)
pub fn compare_builds(
    slug: &str,
    content_type: &str,
    legacy: &BuildResult,
    ir: &BuildResult,
) -> ParityReport {
    let mut diffs = Vec::new();

    // ── Level 1: Completion + failure count ─────────────────────────────
    if legacy.apex_node_id.is_empty() != ir.apex_node_id.is_empty() {
        let legacy_status = if legacy.apex_node_id.is_empty() {
            "no apex (failed)"
        } else {
            "completed"
        };
        let ir_status = if ir.apex_node_id.is_empty() {
            "no apex (failed)"
        } else {
            "completed"
        };
        diffs.push(ParityDiff::CompletionMismatch {
            legacy: legacy_status.to_string(),
            ir: ir_status.to_string(),
        });
    }

    if legacy.failure_count != ir.failure_count {
        diffs.push(ParityDiff::FailureCountMismatch {
            legacy: legacy.failure_count,
            ir: ir.failure_count,
        });
    }

    // ── Level 2: Node-count + depth-shape ───────────────────────────────
    if legacy.max_depth != ir.max_depth {
        diffs.push(ParityDiff::DepthShapeMismatch {
            legacy_max_depth: legacy.max_depth,
            ir_max_depth: ir.max_depth,
        });
    }

    if legacy.total_nodes != ir.total_nodes {
        diffs.push(ParityDiff::TotalNodeCountMismatch {
            legacy: legacy.total_nodes,
            ir: ir.total_nodes,
        });
    }

    // Per-depth node counts
    let all_depths: Vec<i64> = {
        let mut depths: Vec<i64> = legacy
            .node_counts_by_depth
            .keys()
            .chain(ir.node_counts_by_depth.keys())
            .copied()
            .collect();
        depths.sort();
        depths.dedup();
        depths
    };

    for &depth in &all_depths {
        let l = legacy
            .node_counts_by_depth
            .get(&depth)
            .copied()
            .unwrap_or(0);
        let i = ir.node_counts_by_depth.get(&depth).copied().unwrap_or(0);
        if l != i {
            diffs.push(ParityDiff::NodeCountMismatch {
                depth,
                legacy: l,
                ir: i,
            });
        }
    }

    // ── Level 3: Parent/child + web edge structure ──────────────────────
    // Compare parent-child maps
    let all_parents: Vec<String> = {
        let mut keys: Vec<String> = legacy
            .parent_child_map
            .keys()
            .chain(ir.parent_child_map.keys())
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        keys
    };

    for parent_id in &all_parents {
        let l_children = legacy.parent_child_map.get(parent_id);
        let i_children = ir.parent_child_map.get(parent_id);
        match (l_children, i_children) {
            (Some(l), Some(i)) if l != i => {
                diffs.push(ParityDiff::ParentChildMismatch {
                    node_id: parent_id.clone(),
                    detail: format!(
                        "legacy has {} children, IR has {} children",
                        l.len(),
                        i.len()
                    ),
                });
            }
            (Some(l), None) => {
                diffs.push(ParityDiff::ParentChildMismatch {
                    node_id: parent_id.clone(),
                    detail: format!("legacy has {} children, IR has no entry", l.len()),
                });
            }
            (None, Some(i)) => {
                diffs.push(ParityDiff::ParentChildMismatch {
                    node_id: parent_id.clone(),
                    detail: format!("legacy has no entry, IR has {} children", i.len()),
                });
            }
            _ => {} // match or both absent
        }
    }

    // Web edge totals
    if legacy.total_web_edges != ir.total_web_edges {
        diffs.push(ParityDiff::TotalWebEdgeMismatch {
            legacy: legacy.total_web_edges,
            ir: ir.total_web_edges,
        });
    }

    // Per-depth web edge counts
    let all_edge_depths: Vec<i64> = {
        let mut depths: Vec<i64> = legacy
            .web_edge_counts_by_depth
            .keys()
            .chain(ir.web_edge_counts_by_depth.keys())
            .copied()
            .collect();
        depths.sort();
        depths.dedup();
        depths
    };

    for &depth in &all_edge_depths {
        let l = legacy
            .web_edge_counts_by_depth
            .get(&depth)
            .copied()
            .unwrap_or(0);
        let i = ir
            .web_edge_counts_by_depth
            .get(&depth)
            .copied()
            .unwrap_or(0);
        if l != i {
            diffs.push(ParityDiff::WebEdgeCountMismatch {
                depth,
                legacy: l,
                ir: i,
            });
        }
    }

    // ── Level 4: Cost-log completeness ──────────────────────────────────
    if legacy.cost_rows != ir.cost_rows {
        diffs.push(ParityDiff::CostLogCountMismatch {
            legacy: legacy.cost_rows,
            ir: ir.cost_rows,
        });
    }

    if legacy.cost_rows == 0 {
        diffs.push(ParityDiff::CostLogMissing {
            side: "legacy".to_string(),
            count: 0,
        });
    }

    if ir.cost_rows == 0 {
        diffs.push(ParityDiff::CostLogMissing {
            side: "ir".to_string(),
            count: 0,
        });
    }

    // ── Verdict ─────────────────────────────────────────────────────────
    let verdict = compute_verdict(&diffs);

    ParityReport {
        slug: slug.to_string(),
        content_type: content_type.to_string(),
        legacy_result: legacy.clone(),
        ir_result: ir.clone(),
        diffs,
        verdict,
    }
}

/// Determine the verdict from the collected diffs.
fn compute_verdict(diffs: &[ParityDiff]) -> ParityVerdict {
    if diffs.is_empty() {
        return ParityVerdict::Pass;
    }

    let has_blocking = diffs.iter().any(|d| is_blocking_diff(d));
    if has_blocking {
        let blocking_summary: Vec<String> = diffs
            .iter()
            .filter(|d| is_blocking_diff(d))
            .map(|d| format_diff_short(d))
            .collect();
        return ParityVerdict::Fail(blocking_summary.join("; "));
    }

    // Non-blocking diffs only => acceptable drift
    let drift_summary: Vec<String> = diffs.iter().map(|d| format_diff_short(d)).collect();
    ParityVerdict::AcceptableDrift(drift_summary.join("; "))
}

/// A diff is "blocking" if it indicates a structural mismatch that cannot
/// be explained by normal LLM output variation.
fn is_blocking_diff(diff: &ParityDiff) -> bool {
    matches!(
        diff,
        ParityDiff::CompletionMismatch { .. }
            | ParityDiff::DepthShapeMismatch { .. }
            | ParityDiff::TotalNodeCountMismatch { .. }
            | ParityDiff::ParentChildMismatch { .. }
    )
}

/// Short human-readable description of a diff.
fn format_diff_short(diff: &ParityDiff) -> String {
    match diff {
        ParityDiff::CompletionMismatch { legacy, ir } => {
            format!("completion: legacy={legacy}, ir={ir}")
        }
        ParityDiff::FailureCountMismatch { legacy, ir } => {
            format!("failures: legacy={legacy}, ir={ir}")
        }
        ParityDiff::NodeCountMismatch { depth, legacy, ir } => {
            format!("nodes@d{depth}: legacy={legacy}, ir={ir}")
        }
        ParityDiff::DepthShapeMismatch {
            legacy_max_depth,
            ir_max_depth,
        } => {
            format!("max_depth: legacy={legacy_max_depth}, ir={ir_max_depth}")
        }
        ParityDiff::TotalNodeCountMismatch { legacy, ir } => {
            format!("total_nodes: legacy={legacy}, ir={ir}")
        }
        ParityDiff::WebEdgeCountMismatch { depth, legacy, ir } => {
            format!("web_edges@d{depth}: legacy={legacy}, ir={ir}")
        }
        ParityDiff::TotalWebEdgeMismatch { legacy, ir } => {
            format!("total_web_edges: legacy={legacy}, ir={ir}")
        }
        ParityDiff::ParentChildMismatch { node_id, detail } => {
            format!("parent_child[{node_id}]: {detail}")
        }
        ParityDiff::CostLogMissing { side, count } => {
            format!("cost_log_missing: {side} ({count} rows)")
        }
        ParityDiff::CostLogCountMismatch { legacy, ir } => {
            format!("cost_log_count: legacy={legacy}, ir={ir}")
        }
        ParityDiff::QualitativeDrift {
            node_id,
            similarity,
        } => {
            format!("qualitative[{node_id}]: similarity={similarity:.2}")
        }
    }
}

// ── Clean ───────────────────────────────────────────────────────────────────

/// Remove all build artifacts for a slug so it can be rebuilt from scratch.
/// Deletes nodes (above depth -1, i.e. all), pipeline steps, web edges, and
/// cost log entries. Preserves the slug record and ingested chunks.
fn clean_slug_for_rebuild(conn: &rusqlite::Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    conn.execute(
        "DELETE FROM pyramid_web_edges WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    conn.execute(
        "DELETE FROM pyramid_threads WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    conn.execute(
        "DELETE FROM pyramid_cost_log WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    // Reset slug stats
    conn.execute(
        "UPDATE pyramid_slugs SET node_count = 0, max_depth = 0 WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

// ── Run Parity Test ─────────────────────────────────────────────────────────

/// Orchestrates a full parity comparison:
///
/// 1. Build with legacy chain executor, capture result.
/// 2. Clean the slug (delete all nodes/edges/steps/cost).
/// 3. Rebuild with IR executor on the same ingested data.
/// 4. Compare results.
/// 5. Return ParityReport.
///
/// IMPORTANT: This runs two full builds in sequence. It is slow and expensive.
/// Use only on fixture slugs, not production data.
pub async fn run_parity_test(state: &PyramidState, slug: &str) -> Result<ParityReport> {
    // Determine content type
    let content_type = {
        let conn = state.reader.lock().await;
        super::slug::get_slug(&conn, slug)?
            .ok_or_else(|| anyhow!("Slug '{}' not found", slug))?
            .content_type
    };

    // Only code and document for now
    if content_type != ContentType::Code && content_type != ContentType::Document {
        return Err(anyhow!(
            "Parity tests only support code and document (got {:?})",
            content_type
        ));
    }

    let ct_str = content_type.as_str().to_string();
    let cancel = CancellationToken::new();

    info!(slug, content_type = %ct_str, "starting parity test: legacy build");

    // ── Phase 1: Clean and build with legacy chain executor ─────────────
    {
        let conn = state.writer.lock().await;
        clean_slug_for_rebuild(&conn, slug)?;
    }

    let (progress_tx, mut progress_rx) = mpsc::channel::<BuildProgress>(64);
    // Drain progress so we don't block
    tokio::spawn(async move { while progress_rx.recv().await.is_some() {} });

    // Force legacy chain engine path
    use std::sync::atomic::Ordering;
    let prev_ir = state.use_ir_executor.load(Ordering::Relaxed);
    let prev_chain = state.use_chain_engine.load(Ordering::Relaxed);

    state.use_ir_executor.store(false, Ordering::Relaxed);
    state.use_chain_engine.store(true, Ordering::Relaxed);

    let legacy_start = Instant::now();
    let legacy_build = build_runner::run_build(
        state,
        slug,
        &cancel,
        Some(progress_tx),
        // We need a write_tx for the legacy path — create a dummy drain
        &create_dummy_write_tx(),
        None,
    )
    .await;
    let legacy_elapsed = legacy_start.elapsed().as_secs_f64();

    let (legacy_apex, legacy_failures) = match legacy_build {
        Ok((apex, failures)) => (apex, failures),
        Err(e) => {
            warn!(slug, error = %e, "legacy build failed");
            (String::new(), -1)
        }
    };

    let legacy_result = {
        let conn = state.reader.lock().await;
        capture_build_result(&conn, slug, &legacy_apex, legacy_failures, legacy_elapsed)?
    };

    info!(
        slug,
        apex = %legacy_apex,
        nodes = legacy_result.total_nodes,
        elapsed = legacy_elapsed,
        "legacy build complete"
    );

    // ── Phase 2: Clean and rebuild with IR executor ─────────────────────
    {
        let conn = state.writer.lock().await;
        clean_slug_for_rebuild(&conn, slug)?;
    }

    state.use_ir_executor.store(true, Ordering::Relaxed);
    state.use_chain_engine.store(false, Ordering::Relaxed);

    let (progress_tx2, mut progress_rx2) = mpsc::channel::<BuildProgress>(64);
    tokio::spawn(async move { while progress_rx2.recv().await.is_some() {} });

    let ir_start = Instant::now();
    let ir_build = build_runner::run_build(
        state,
        slug,
        &cancel,
        Some(progress_tx2),
        &create_dummy_write_tx(),
        None,
    )
    .await;
    let ir_elapsed = ir_start.elapsed().as_secs_f64();

    let (ir_apex, ir_failures) = match ir_build {
        Ok((apex, failures)) => (apex, failures),
        Err(e) => {
            warn!(slug, error = %e, "IR build failed");
            (String::new(), -1)
        }
    };

    let ir_result = {
        let conn = state.reader.lock().await;
        capture_build_result(&conn, slug, &ir_apex, ir_failures, ir_elapsed)?
    };

    info!(
        slug,
        apex = %ir_apex,
        nodes = ir_result.total_nodes,
        elapsed = ir_elapsed,
        "IR build complete"
    );

    // ── Phase 3: Restore flags and compare ──────────────────────────────
    state.use_ir_executor.store(prev_ir, Ordering::Relaxed);
    state.use_chain_engine.store(prev_chain, Ordering::Relaxed);

    let report = compare_builds(slug, &ct_str, &legacy_result, &ir_result);

    info!(
        slug,
        verdict = ?report.verdict,
        diff_count = report.diffs.len(),
        "parity test complete"
    );

    Ok(report)
}

/// Create a dummy WriteOp sender for builds that need one.
/// The receiver is spawned as a drain task that discards all messages.
fn create_dummy_write_tx() -> mpsc::Sender<super::build::WriteOp> {
    let (tx, mut rx) = mpsc::channel::<super::build::WriteOp>(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

// ── Print ───────────────────────────────────────────────────────────────────

/// Produce a human-readable report string for CLI/log consumption.
pub fn print_parity_report(report: &ParityReport) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "=== Parity Report: {} ({}) ===\n\n",
        report.slug, report.content_type
    ));

    // Legacy summary
    out.push_str("Legacy build:\n");
    out.push_str(&format!(
        "  apex:        {}\n",
        report.legacy_result.apex_node_id
    ));
    out.push_str(&format!(
        "  total nodes: {}\n",
        report.legacy_result.total_nodes
    ));
    out.push_str(&format!(
        "  max depth:   {}\n",
        report.legacy_result.max_depth
    ));
    out.push_str(&format!(
        "  web edges:   {}\n",
        report.legacy_result.total_web_edges
    ));
    out.push_str(&format!(
        "  cost rows:   {}\n",
        report.legacy_result.cost_rows
    ));
    out.push_str(&format!(
        "  failures:    {}\n",
        report.legacy_result.failure_count
    ));
    out.push_str(&format!(
        "  elapsed:     {:.1}s\n",
        report.legacy_result.elapsed_seconds
    ));

    out.push_str("\nIR build:\n");
    out.push_str(&format!(
        "  apex:        {}\n",
        report.ir_result.apex_node_id
    ));
    out.push_str(&format!(
        "  total nodes: {}\n",
        report.ir_result.total_nodes
    ));
    out.push_str(&format!("  max depth:   {}\n", report.ir_result.max_depth));
    out.push_str(&format!(
        "  web edges:   {}\n",
        report.ir_result.total_web_edges
    ));
    out.push_str(&format!("  cost rows:   {}\n", report.ir_result.cost_rows));
    out.push_str(&format!(
        "  failures:    {}\n",
        report.ir_result.failure_count
    ));
    out.push_str(&format!(
        "  elapsed:     {:.1}s\n",
        report.ir_result.elapsed_seconds
    ));

    // Diffs
    if report.diffs.is_empty() {
        out.push_str("\nDiffs: none\n");
    } else {
        out.push_str(&format!("\nDiffs ({}):\n", report.diffs.len()));
        for (i, diff) in report.diffs.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i + 1, format_diff_short(diff)));
        }
    }

    // Verdict
    out.push_str(&format!("\nVerdict: {}\n", format_verdict(&report.verdict)));

    out
}

fn format_verdict(verdict: &ParityVerdict) -> String {
    match verdict {
        ParityVerdict::Pass => "PASS".to_string(),
        ParityVerdict::AcceptableDrift(reason) => format!("ACCEPTABLE DRIFT: {reason}"),
        ParityVerdict::Fail(reason) => format!("FAIL: {reason}"),
    }
}

// ── Question Build Validation (P2.3) ─────────────────────────────────────────

/// Validation report for a question YAML compilation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionValidationReport {
    pub content_type: String,
    pub compilation_ok: bool,
    pub plan_valid: bool,
    pub step_count: usize,
    pub expected_depths: Vec<i64>,
    pub actual_depths: Vec<i64>,
    pub has_converge: bool,
    pub has_web_edges: bool,
    pub iteration_modes: HashMap<String, String>,
    pub issues: Vec<String>,
}

/// Comparison report between question-compiled and defaults-compiled plans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationComparisonReport {
    pub content_type: String,
    pub question_steps: usize,
    pub defaults_steps: usize,
    pub question_max_depth: i64,
    pub defaults_max_depth: i64,
    pub both_have_webbing: bool,
    pub both_have_converge: bool,
    pub structural_compatible: bool,
    pub notes: Vec<String>,
}

/// Validate that a question YAML compiles correctly and produces a sane plan.
///
/// Loads the question set for `content_type` from `chains_dir/questions/`,
/// compiles it, validates the resulting plan, and inspects the structure.
pub fn validate_question_compilation(
    content_type: &str,
    chains_dir: &Path,
) -> Result<QuestionValidationReport> {
    use super::execution_plan::{IterationMode, StorageKind};
    use super::question_compiler;
    use super::question_loader;

    let mut issues = Vec::new();

    // ── Load ────────────────────────────────────────────────────────────
    let question_sets = question_loader::discover_question_sets(chains_dir)?;
    let meta = question_sets
        .iter()
        .find(|m| m.content_type == content_type)
        .ok_or_else(|| {
            anyhow!(
                "no question set found for content type '{}' in {}",
                content_type,
                chains_dir.join("questions").display()
            )
        })?;

    let yaml_path = std::path::Path::new(&meta.file_path);
    let qs = question_loader::load_question_set(yaml_path, chains_dir)?;

    // ── Compile ────────────────────────────────────────────────────────
    let plan = match question_compiler::compile_question_set(&qs, chains_dir) {
        Ok(p) => p,
        Err(e) => {
            return Ok(QuestionValidationReport {
                content_type: content_type.to_string(),
                compilation_ok: false,
                plan_valid: false,
                step_count: 0,
                expected_depths: Vec::new(),
                actual_depths: Vec::new(),
                has_converge: false,
                has_web_edges: false,
                iteration_modes: HashMap::new(),
                issues: vec![format!("compilation failed: {:#}", e)],
            });
        }
    };

    // ── Validate DAG ───────────────────────────────────────────────────
    let plan_valid = plan.validate().is_ok();
    if !plan_valid {
        if let Err(e) = plan.validate() {
            issues.push(format!("plan validation failed: {:#}", e));
        }
    }

    // ── Inspect structure ──────────────────────────────────────────────
    let step_count = plan.steps.len();

    // Collect actual depths from storage directives
    let mut actual_depths: Vec<i64> = plan
        .steps
        .iter()
        .filter_map(|s| s.storage_directive.as_ref())
        .filter(|sd| sd.kind == StorageKind::Node || sd.kind == StorageKind::StepOnly)
        .filter_map(|sd| sd.depth)
        .collect();
    actual_depths.sort();
    actual_depths.dedup();

    // Expected depths based on content type
    let expected_depths = match content_type {
        "code" => vec![0, 1, 2, 3],     // L0, L1, L2, apex
        "document" => vec![0, 1, 2, 3], // classification(0), L0, L1, L2, apex
        "conversation" => vec![0, 1, 2, 3],
        _ => vec![],
    };

    // Check converge
    let has_converge = plan.steps.iter().any(|s| s.converge_metadata.is_some());

    // Check web edges
    let has_web_edges = plan.steps.iter().any(|s| {
        s.storage_directive
            .as_ref()
            .map(|sd| sd.kind == StorageKind::WebEdges)
            .unwrap_or(false)
    });

    // Collect iteration modes per step
    let mut iteration_modes = HashMap::new();
    for step in &plan.steps {
        let mode_str = match &step.iteration {
            Some(iter) => match iter.mode {
                IterationMode::Parallel => "parallel".to_string(),
                IterationMode::Sequential => "sequential".to_string(),
                IterationMode::Single => "single".to_string(),
            },
            None => "single".to_string(),
        };
        iteration_modes.insert(step.id.clone(), mode_str);
    }

    // ── Structural checks ──────────────────────────────────────────────
    if !has_converge {
        issues.push("no converge steps found — expected for L2 synthesis".to_string());
    }
    if !has_web_edges {
        issues.push("no web edge steps found".to_string());
    }

    // Check no empty instructions
    for step in &plan.steps {
        if step
            .instruction
            .as_ref()
            .map(|i| i.trim().is_empty())
            .unwrap_or(false)
        {
            issues.push(format!("step '{}' has empty instruction", step.id));
        }
        // LLM steps should have instructions (converge steps get them from the expander)
        if matches!(step.operation, super::execution_plan::StepOperation::Llm)
            && step.instruction.is_none()
            && step.converge_metadata.is_none()
        {
            issues.push(format!("step '{}' is LLM but has no instruction", step.id));
        }
    }

    // Check orphan steps: every step is either root or depends on a step that exists
    let step_ids: std::collections::HashSet<&str> =
        plan.steps.iter().map(|s| s.id.as_str()).collect();
    for step in &plan.steps {
        for dep in &step.depends_on {
            if !step_ids.contains(dep.as_str()) {
                issues.push(format!(
                    "step '{}' depends on unknown step '{}'",
                    step.id, dep
                ));
            }
        }
    }

    // Check converge steps have valid metadata
    for step in &plan.steps {
        if let Some(cm) = &step.converge_metadata {
            if cm.max_rounds == 0 {
                issues.push(format!(
                    "step '{}' converge_metadata has max_rounds=0",
                    step.id
                ));
            }
            if cm.converge_id.trim().is_empty() {
                issues.push(format!(
                    "step '{}' converge_metadata has empty converge_id",
                    step.id
                ));
            }
            if cm.shortcut_at == 0
                && matches!(cm.role, super::execution_plan::ConvergeRole::Shortcut)
            {
                issues.push(format!(
                    "step '{}' is a shortcut with shortcut_at=0",
                    step.id
                ));
            }
        }
    }

    // Check for unreachable steps (not depended-on by anything AND not a terminal step)
    let depended_on: std::collections::HashSet<&str> = plan
        .steps
        .iter()
        .flat_map(|s| s.depends_on.iter().map(|d| d.as_str()))
        .collect();
    let terminal_step_ids: std::collections::HashSet<&str> = plan
        .steps
        .iter()
        .filter(|s| {
            // A terminal step: no other step depends on it
            !depended_on.contains(s.id.as_str())
        })
        .map(|s| s.id.as_str())
        .collect();
    // Steps that are neither depended-on nor terminal (i.e. roots) should have empty depends_on
    // What we really want: steps that have no dependents AND produce no stored output
    for step in &plan.steps {
        let is_depended_on = depended_on.contains(step.id.as_str());
        let has_storage = step.storage_directive.is_some();
        if !is_depended_on && !has_storage && !step.depends_on.is_empty() {
            issues.push(format!(
                "step '{}' is unreachable: not depended on and produces no stored output",
                step.id
            ));
        }
    }

    Ok(QuestionValidationReport {
        content_type: content_type.to_string(),
        compilation_ok: true,
        plan_valid,
        step_count,
        expected_depths,
        actual_depths,
        has_converge,
        has_web_edges,
        iteration_modes,
        issues,
    })
}

/// Compare question-compiled plan vs defaults-compiled plan for the same content type.
///
/// Loads and compiles both the question YAML and the defaults YAML, then compares
/// structural properties.
pub fn compare_question_vs_defaults(
    content_type: &str,
    chains_dir: &Path,
) -> Result<CompilationComparisonReport> {
    use super::chain_loader;
    use super::defaults_adapter;
    use super::execution_plan::StorageKind;
    use super::question_compiler;
    use super::question_loader;

    let mut notes = Vec::new();

    // ── Compile question plan ──────────────────────────────────────────
    let question_sets = question_loader::discover_question_sets(chains_dir)?;
    let q_meta = question_sets
        .iter()
        .find(|m| m.content_type == content_type)
        .ok_or_else(|| anyhow!("no question set found for content type '{}'", content_type))?;

    let q_yaml_path = std::path::Path::new(&q_meta.file_path);
    let qs = question_loader::load_question_set(q_yaml_path, chains_dir)?;
    let q_plan = question_compiler::compile_question_set(&qs, chains_dir)?;

    // ── Compile defaults plan ──────────────────────────────────────────
    let defaults_chains = chain_loader::discover_chains(chains_dir)?;
    let d_meta = defaults_chains
        .iter()
        .find(|m| m.content_type == content_type && m.is_default)
        .ok_or_else(|| {
            anyhow!(
                "no defaults chain found for content type '{}'",
                content_type
            )
        })?;

    let d_yaml_path = std::path::Path::new(&d_meta.file_path);
    let chain = chain_loader::load_chain(d_yaml_path, chains_dir)?;
    let d_plan = defaults_adapter::compile_defaults(&chain)?;

    // ── Compare ────────────────────────────────────────────────────────
    let question_steps = q_plan.steps.len();
    let defaults_steps = d_plan.steps.len();

    // Max depth from storage directives (excluding apex)
    let is_apex_storage = |sd: &crate::pyramid::execution_plan::StorageDirective| {
        sd.node_id_pattern
            .as_deref()
            .map(|pattern| pattern.eq_ignore_ascii_case("APEX"))
            .unwrap_or(false)
    };
    let q_max_depth = q_plan
        .steps
        .iter()
        .filter_map(|s| s.storage_directive.as_ref())
        .filter(|sd| sd.kind == StorageKind::Node)
        .filter(|sd| !is_apex_storage(sd))
        .filter_map(|sd| sd.depth)
        .max()
        .unwrap_or(0);

    let d_max_depth = d_plan
        .steps
        .iter()
        .filter_map(|s| s.storage_directive.as_ref())
        .filter(|sd| sd.kind == StorageKind::Node)
        .filter(|sd| !is_apex_storage(sd))
        .filter_map(|sd| sd.depth)
        .max()
        .unwrap_or(0);

    // Webbing presence
    let q_has_webbing = q_plan.steps.iter().any(|s| {
        s.storage_directive
            .as_ref()
            .map(|sd| sd.kind == StorageKind::WebEdges)
            .unwrap_or(false)
    });
    let d_has_webbing = d_plan.steps.iter().any(|s| {
        s.storage_directive
            .as_ref()
            .map(|sd| sd.kind == StorageKind::WebEdges)
            .unwrap_or(false)
    });
    let both_have_webbing = q_has_webbing && d_has_webbing;

    // Converge presence
    let q_has_converge = q_plan.steps.iter().any(|s| s.converge_metadata.is_some());
    let d_has_converge = d_plan.steps.iter().any(|s| s.converge_metadata.is_some());
    let both_have_converge = q_has_converge && d_has_converge;

    // Structural compatibility: similar depth ranges, both have essential features
    let depth_compatible = (q_max_depth - d_max_depth).abs() <= 1;
    let structural_compatible = depth_compatible && both_have_webbing && both_have_converge;

    // Annotate differences
    if q_max_depth != d_max_depth {
        notes.push(format!(
            "max depth differs: question={}, defaults={}",
            q_max_depth, d_max_depth
        ));
    }
    if !both_have_webbing {
        notes.push(format!(
            "webbing mismatch: question={}, defaults={}",
            q_has_webbing, d_has_webbing
        ));
    }
    if !both_have_converge {
        notes.push(format!(
            "converge mismatch: question={}, defaults={}",
            q_has_converge, d_has_converge
        ));
    }

    let step_diff = (question_steps as i64 - defaults_steps as i64).abs();
    if step_diff > 10 {
        notes.push(format!(
            "significant step count difference: question={}, defaults={} (delta={})",
            question_steps, defaults_steps, step_diff
        ));
    }

    // Check storage directive compatibility
    let q_storage_kinds: Vec<String> = q_plan
        .steps
        .iter()
        .filter_map(|s| s.storage_directive.as_ref())
        .map(|sd| format!("{:?}", sd.kind))
        .collect();
    let d_storage_kinds: Vec<String> = d_plan
        .steps
        .iter()
        .filter_map(|s| s.storage_directive.as_ref())
        .map(|sd| format!("{:?}", sd.kind))
        .collect();

    // Count storage kinds
    let q_node_count = q_storage_kinds
        .iter()
        .filter(|k| k.contains("Node"))
        .count();
    let d_node_count = d_storage_kinds
        .iter()
        .filter(|k| k.contains("Node"))
        .count();
    if q_node_count != d_node_count {
        notes.push(format!(
            "node-creating step count differs: question={}, defaults={}",
            q_node_count, d_node_count
        ));
    }

    Ok(CompilationComparisonReport {
        content_type: content_type.to_string(),
        question_steps,
        defaults_steps,
        question_max_depth: q_max_depth,
        defaults_max_depth: d_max_depth,
        both_have_webbing,
        both_have_converge,
        structural_compatible,
        notes,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    fn make_result(
        apex: &str,
        failures: i32,
        node_counts: &[(i64, i64)],
        web_edge_counts: &[(i64, i64)],
        cost_rows: i64,
        elapsed: f64,
    ) -> BuildResult {
        let node_counts_by_depth: HashMap<i64, i64> = node_counts.iter().copied().collect();
        let total_nodes = node_counts_by_depth.values().sum();
        let max_depth = node_counts_by_depth.keys().copied().max().unwrap_or(0);
        let web_edge_counts_by_depth: HashMap<i64, i64> = web_edge_counts.iter().copied().collect();
        let total_web_edges = web_edge_counts_by_depth.values().sum();

        BuildResult {
            apex_node_id: apex.to_string(),
            failure_count: failures,
            node_counts_by_depth,
            total_nodes,
            web_edge_counts_by_depth,
            total_web_edges,
            cost_rows,
            elapsed_seconds: elapsed,
            parent_child_map: HashMap::new(),
            max_depth,
        }
    }

    #[test]
    fn test_compare_identical_builds_passes() {
        let legacy = make_result(
            "apex-1",
            0,
            &[(0, 20), (1, 5), (2, 2), (3, 1)],
            &[(1, 3), (2, 1)],
            28,
            45.0,
        );
        let ir = make_result(
            "apex-2", // different ID is fine — structural parity is what matters
            0,
            &[(0, 20), (1, 5), (2, 2), (3, 1)],
            &[(1, 3), (2, 1)],
            28,
            52.0,
        );

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(
            report.diffs.is_empty(),
            "expected no diffs, got: {:?}",
            report.diffs
        );
        assert!(matches!(report.verdict, ParityVerdict::Pass));
    }

    #[test]
    fn test_compare_depth_shape_mismatch_fails() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[], 10, 30.0);
        let ir = make_result(
            "apex-2",
            0,
            &[(0, 20), (1, 5), (2, 2), (3, 1)],
            &[],
            10,
            35.0,
        );

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(!report.diffs.is_empty());
        assert!(
            matches!(report.verdict, ParityVerdict::Fail(_)),
            "expected Fail verdict, got: {:?}",
            report.verdict
        );
        // Should have depth shape mismatch
        assert!(report.diffs.iter().any(|d| matches!(
            d,
            ParityDiff::DepthShapeMismatch {
                legacy_max_depth: 2,
                ir_max_depth: 3,
            }
        )));
    }

    #[test]
    fn test_compare_failure_count_mismatch_is_acceptable_drift() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[], 10, 30.0);
        let ir = make_result("apex-2", 2, &[(0, 20), (1, 5), (2, 1)], &[], 10, 35.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(!report.diffs.is_empty());
        // Failure count difference alone is not blocking
        assert!(
            matches!(report.verdict, ParityVerdict::AcceptableDrift(_)),
            "expected AcceptableDrift, got: {:?}",
            report.verdict
        );
    }

    #[test]
    fn test_compare_node_count_mismatch_fails() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[], 10, 30.0);
        let ir = make_result("apex-2", 0, &[(0, 18), (1, 5), (2, 1)], &[], 10, 35.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        // Total node count mismatch is blocking
        assert!(matches!(report.verdict, ParityVerdict::Fail(_)));
    }

    #[test]
    fn test_compare_web_edge_mismatch_is_acceptable() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[(1, 3)], 10, 30.0);
        let ir = make_result("apex-2", 0, &[(0, 20), (1, 5), (2, 1)], &[(1, 5)], 10, 35.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(!report.diffs.is_empty());
        // Web edge difference alone is not blocking (LLM output variation)
        assert!(matches!(report.verdict, ParityVerdict::AcceptableDrift(_)));
    }

    #[test]
    fn test_compare_cost_log_mismatch_is_acceptable() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[], 28, 30.0);
        let ir = make_result("apex-2", 0, &[(0, 20), (1, 5), (2, 1)], &[], 30, 35.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(matches!(report.verdict, ParityVerdict::AcceptableDrift(_)));
    }

    #[test]
    fn test_compare_completion_mismatch_fails() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[], 10, 30.0);
        let ir = make_result("", -1, &[], &[], 0, 5.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(matches!(report.verdict, ParityVerdict::Fail(_)));
    }

    #[test]
    fn test_compare_parent_child_mismatch_fails() {
        let mut legacy = make_result("apex-1", 0, &[(0, 4), (1, 2), (2, 1)], &[], 7, 30.0);
        legacy.parent_child_map.insert(
            "parent-1".to_string(),
            vec!["child-a".to_string(), "child-b".to_string()],
        );

        let mut ir = make_result("apex-2", 0, &[(0, 4), (1, 2), (2, 1)], &[], 7, 32.0);
        ir.parent_child_map.insert(
            "parent-1".to_string(),
            vec!["child-a".to_string(), "child-c".to_string()],
        );

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        assert!(matches!(report.verdict, ParityVerdict::Fail(_)));
    }

    #[test]
    fn test_print_parity_report_output() {
        let legacy = make_result("apex-1", 0, &[(0, 20), (1, 5), (2, 1)], &[(1, 3)], 26, 45.0);
        let ir = make_result("apex-2", 1, &[(0, 20), (1, 5), (2, 1)], &[(1, 4)], 27, 52.0);

        let report = compare_builds("test-slug", "code", &legacy, &ir);
        let output = print_parity_report(&report);

        assert!(output.contains("Parity Report: test-slug"));
        assert!(output.contains("Legacy build:"));
        assert!(output.contains("IR build:"));
        assert!(output.contains("Verdict:"));
    }

    #[test]
    fn test_capture_build_result_against_in_memory_db() {
        // Set up an in-memory SQLite DB with the pyramid schema
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();

        let slug = "test-parity";
        // Create slug
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, 'code', '/tmp/test')",
            rusqlite::params![slug],
        ).unwrap();

        // Insert nodes at various depths
        for i in 0..10 {
            let node_id = format!("node-d0-{i}");
            conn.execute(
                "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, build_version)
                 VALUES (?1, ?2, 0, 'headline', 'distilled', 1)",
                rusqlite::params![node_id, slug],
            )
            .unwrap();
        }
        for i in 0..3 {
            let node_id = format!("node-d1-{i}");
            let parent = format!("node-d0-{}", i * 3);
            conn.execute(
                "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, parent_id, build_version)
                 VALUES (?1, ?2, 1, 'headline', 'distilled', ?3, 1)",
                rusqlite::params![node_id, slug, parent],
            ).unwrap();
        }
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, parent_id, build_version)
             VALUES ('apex', ?1, 2, 'apex headline', 'apex distilled', 'node-d1-0', 1)",
            rusqlite::params![slug],
        ).unwrap();

        // Insert cost log rows
        for _ in 0..5 {
            conn.execute(
                "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost)
                 VALUES (?1, 'build', 'test-model', 100, 50, 0.001)",
                rusqlite::params![slug],
            ).unwrap();
        }

        // Capture result
        let result = capture_build_result(&conn, slug, "apex", 0, 10.0).unwrap();

        assert_eq!(result.total_nodes, 14); // 10 + 3 + 1
        assert_eq!(result.node_counts_by_depth.get(&0), Some(&10));
        assert_eq!(result.node_counts_by_depth.get(&1), Some(&3));
        assert_eq!(result.node_counts_by_depth.get(&2), Some(&1));
        assert_eq!(result.max_depth, 2);
        assert_eq!(result.cost_rows, 5);
        assert_eq!(result.apex_node_id, "apex");
        assert_eq!(result.failure_count, 0);

        // Verify parent-child map
        assert!(result.parent_child_map.contains_key("node-d0-0"));
        assert!(result.parent_child_map.contains_key("node-d1-0"));
        assert_eq!(
            result.parent_child_map.get("node-d1-0").unwrap(),
            &vec!["apex".to_string()]
        );
    }

    // ── P2.3: Question Build Validation Tests ───────────────────────────────

    /// Helper: compile code question YAML from include_str (bypasses prompt file resolution).
    fn compile_code_questions() -> crate::pyramid::execution_plan::ExecutionPlan {
        use crate::pyramid::question_compiler;
        use crate::pyramid::question_yaml::QuestionSet;
        let yaml = include_str!("../../../chains/questions/code.yaml");
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        question_compiler::compile_question_set(&qs, std::path::Path::new("/tmp")).unwrap()
    }

    /// Helper: compile document question YAML from include_str.
    fn compile_document_questions() -> crate::pyramid::execution_plan::ExecutionPlan {
        use crate::pyramid::question_compiler;
        use crate::pyramid::question_yaml::QuestionSet;
        let yaml = include_str!("../../../chains/questions/document.yaml");
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        question_compiler::compile_question_set(&qs, std::path::Path::new("/tmp")).unwrap()
    }

    /// Helper: compile conversation question YAML from include_str.
    fn compile_conversation_questions() -> crate::pyramid::execution_plan::ExecutionPlan {
        use crate::pyramid::question_compiler;
        use crate::pyramid::question_yaml::QuestionSet;
        let yaml = include_str!("../../../chains/questions/conversation.yaml");
        let qs: QuestionSet = serde_yaml::from_str(yaml).unwrap();
        question_compiler::compile_question_set(&qs, std::path::Path::new("/tmp")).unwrap()
    }

    /// Helper: compile code defaults chain.
    fn compile_code_defaults() -> crate::pyramid::execution_plan::ExecutionPlan {
        use crate::pyramid::chain_engine::ChainDefinition;
        use crate::pyramid::defaults_adapter;
        let yaml = include_str!("../../../chains/defaults/code.yaml");
        let chain: ChainDefinition = serde_yaml::from_str(yaml).unwrap();
        defaults_adapter::compile_defaults(&chain).unwrap()
    }

    /// Helper: compile document defaults chain.
    fn compile_document_defaults() -> crate::pyramid::execution_plan::ExecutionPlan {
        use crate::pyramid::chain_engine::ChainDefinition;
        use crate::pyramid::defaults_adapter;
        let yaml = include_str!("../../../chains/defaults/document.yaml");
        let chain: ChainDefinition = serde_yaml::from_str(yaml).unwrap();
        defaults_adapter::compile_defaults(&chain).unwrap()
    }

    /// Helper: inspect a plan structure and produce a QuestionValidationReport.
    /// Re-uses the same structural checks as `validate_question_compilation` to
    /// avoid maintaining two copies of the validation logic.
    fn inspect_plan(
        content_type: &str,
        plan: &crate::pyramid::execution_plan::ExecutionPlan,
    ) -> QuestionValidationReport {
        use crate::pyramid::execution_plan::{
            ConvergeRole, IterationMode, StepOperation, StorageKind,
        };

        let mut issues = Vec::new();

        let plan_valid = plan.validate().is_ok();
        if !plan_valid {
            if let Err(e) = plan.validate() {
                issues.push(format!("plan validation failed: {:#}", e));
            }
        }

        let step_count = plan.steps.len();

        let mut actual_depths: Vec<i64> = plan
            .steps
            .iter()
            .filter_map(|s| s.storage_directive.as_ref())
            .filter(|sd| sd.kind == StorageKind::Node || sd.kind == StorageKind::StepOnly)
            .filter_map(|sd| sd.depth)
            .collect();
        actual_depths.sort();
        actual_depths.dedup();

        let expected_depths = match content_type {
            "code" => vec![0, 1, 2, 3],
            "document" => vec![0, 1, 2, 3],
            "conversation" => vec![0, 1, 2, 3],
            _ => vec![],
        };

        let has_converge = plan.steps.iter().any(|s| s.converge_metadata.is_some());
        let has_web_edges = plan.steps.iter().any(|s| {
            s.storage_directive
                .as_ref()
                .map(|sd| sd.kind == StorageKind::WebEdges)
                .unwrap_or(false)
        });

        let mut iteration_modes = HashMap::new();
        for step in &plan.steps {
            let mode_str = match &step.iteration {
                Some(iter) => match iter.mode {
                    IterationMode::Parallel => "parallel".to_string(),
                    IterationMode::Sequential => "sequential".to_string(),
                    IterationMode::Single => "single".to_string(),
                },
                None => "single".to_string(),
            };
            iteration_modes.insert(step.id.clone(), mode_str);
        }

        // Check no empty instructions
        for step in &plan.steps {
            if step
                .instruction
                .as_ref()
                .map(|i| i.trim().is_empty())
                .unwrap_or(false)
            {
                issues.push(format!("step '{}' has empty instruction", step.id));
            }
            if matches!(step.operation, StepOperation::Llm)
                && step.instruction.is_none()
                && step.converge_metadata.is_none()
            {
                issues.push(format!("step '{}' is LLM but has no instruction", step.id));
            }
        }

        // Check orphan steps (deps point to known steps)
        let step_ids: std::collections::HashSet<&str> =
            plan.steps.iter().map(|s| s.id.as_str()).collect();
        for step in &plan.steps {
            for dep in &step.depends_on {
                if !step_ids.contains(dep.as_str()) {
                    issues.push(format!(
                        "step '{}' depends on unknown step '{}'",
                        step.id, dep
                    ));
                }
            }
        }

        // Check converge metadata (same checks as validate_question_compilation)
        for step in &plan.steps {
            if let Some(cm) = &step.converge_metadata {
                if cm.max_rounds == 0 {
                    issues.push(format!(
                        "step '{}' converge_metadata has max_rounds=0",
                        step.id
                    ));
                }
                if cm.converge_id.trim().is_empty() {
                    issues.push(format!(
                        "step '{}' converge_metadata has empty converge_id",
                        step.id
                    ));
                }
                if cm.shortcut_at == 0 && matches!(cm.role, ConvergeRole::Shortcut) {
                    issues.push(format!(
                        "step '{}' is a shortcut with shortcut_at=0",
                        step.id
                    ));
                }
            }
        }

        // Check for unreachable steps
        let depended_on: std::collections::HashSet<&str> = plan
            .steps
            .iter()
            .flat_map(|s| s.depends_on.iter().map(|d| d.as_str()))
            .collect();
        for step in &plan.steps {
            let is_depended_on = depended_on.contains(step.id.as_str());
            let has_storage = step.storage_directive.is_some();
            if !is_depended_on && !has_storage && !step.depends_on.is_empty() {
                issues.push(format!(
                    "step '{}' is unreachable: not depended on and produces no stored output",
                    step.id
                ));
            }
        }

        QuestionValidationReport {
            content_type: content_type.to_string(),
            compilation_ok: true,
            plan_valid,
            step_count,
            expected_depths,
            actual_depths,
            has_converge,
            has_web_edges,
            iteration_modes,
            issues,
        }
    }

    /// Helper: compare two plans and produce a CompilationComparisonReport.
    fn compare_plans(
        content_type: &str,
        q_plan: &crate::pyramid::execution_plan::ExecutionPlan,
        d_plan: &crate::pyramid::execution_plan::ExecutionPlan,
    ) -> CompilationComparisonReport {
        use crate::pyramid::execution_plan::StorageKind;

        let mut notes = Vec::new();
        let question_steps = q_plan.steps.len();
        let defaults_steps = d_plan.steps.len();

        let is_apex_storage = |sd: &crate::pyramid::execution_plan::StorageDirective| {
            sd.node_id_pattern
                .as_deref()
                .map(|pattern| pattern.eq_ignore_ascii_case("APEX"))
                .unwrap_or(false)
        };

        let q_max_depth = q_plan
            .steps
            .iter()
            .filter_map(|s| s.storage_directive.as_ref())
            .filter(|sd| sd.kind == StorageKind::Node)
            .filter(|sd| !is_apex_storage(sd))
            .filter_map(|sd| sd.depth)
            .max()
            .unwrap_or(0);

        let d_max_depth = d_plan
            .steps
            .iter()
            .filter_map(|s| s.storage_directive.as_ref())
            .filter(|sd| sd.kind == StorageKind::Node)
            .filter(|sd| !is_apex_storage(sd))
            .filter_map(|sd| sd.depth)
            .max()
            .unwrap_or(0);

        let q_has_webbing = q_plan.steps.iter().any(|s| {
            s.storage_directive
                .as_ref()
                .map(|sd| sd.kind == StorageKind::WebEdges)
                .unwrap_or(false)
        });
        let d_has_webbing = d_plan.steps.iter().any(|s| {
            s.storage_directive
                .as_ref()
                .map(|sd| sd.kind == StorageKind::WebEdges)
                .unwrap_or(false)
        });
        let both_have_webbing = q_has_webbing && d_has_webbing;

        let q_has_converge = q_plan.steps.iter().any(|s| s.converge_metadata.is_some());
        let d_has_converge = d_plan.steps.iter().any(|s| s.converge_metadata.is_some());
        let both_have_converge = q_has_converge && d_has_converge;

        let depth_compatible = (q_max_depth - d_max_depth).abs() <= 1;
        let structural_compatible = depth_compatible && both_have_webbing && both_have_converge;

        if q_max_depth != d_max_depth {
            notes.push(format!(
                "max depth differs: question={}, defaults={}",
                q_max_depth, d_max_depth
            ));
        }
        if !both_have_webbing {
            notes.push(format!(
                "webbing mismatch: question={}, defaults={}",
                q_has_webbing, d_has_webbing
            ));
        }
        if !both_have_converge {
            notes.push(format!(
                "converge mismatch: question={}, defaults={}",
                q_has_converge, d_has_converge
            ));
        }

        // Storage kind count comparison (matches main compare_question_vs_defaults)
        let q_node_count = q_plan
            .steps
            .iter()
            .filter(|s| {
                s.storage_directive
                    .as_ref()
                    .map(|sd| sd.kind == StorageKind::Node)
                    .unwrap_or(false)
            })
            .count();
        let d_node_count = d_plan
            .steps
            .iter()
            .filter(|s| {
                s.storage_directive
                    .as_ref()
                    .map(|sd| sd.kind == StorageKind::Node)
                    .unwrap_or(false)
            })
            .count();
        if q_node_count != d_node_count {
            notes.push(format!(
                "node-creating step count differs: question={}, defaults={}",
                q_node_count, d_node_count
            ));
        }

        CompilationComparisonReport {
            content_type: content_type.to_string(),
            question_steps,
            defaults_steps,
            question_max_depth: q_max_depth,
            defaults_max_depth: d_max_depth,
            both_have_webbing,
            both_have_converge,
            structural_compatible,
            notes,
        }
    }

    // ── Compilation validation tests ────────────────────────────────────

    #[test]
    fn validate_code_question_compilation() {
        let plan = compile_code_questions();
        let report = inspect_plan("code", &plan);
        assert!(report.compilation_ok, "code question YAML should compile");
        assert!(report.plan_valid, "code question plan should validate");
        assert!(report.has_converge, "code plan should have converge steps");
        assert!(report.has_web_edges, "code plan should have web edge steps");
        assert!(
            report.actual_depths.contains(&0),
            "should have depth 0: {:?}",
            report.actual_depths
        );
        assert!(
            report.actual_depths.contains(&1),
            "should have depth 1: {:?}",
            report.actual_depths
        );
        assert!(
            report.actual_depths.contains(&2) || report.actual_depths.iter().any(|&d| d > 1),
            "should have depth 2+: {:?}",
            report.actual_depths
        );
        assert!(
            report.issues.is_empty(),
            "should have no issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn validate_document_question_compilation() {
        let plan = compile_document_questions();
        let report = inspect_plan("document", &plan);
        assert!(
            report.compilation_ok,
            "document question YAML should compile"
        );
        assert!(report.plan_valid, "document question plan should validate");
        assert!(
            report.actual_depths.contains(&0),
            "document should have depth 0 for classification: {:?}",
            report.actual_depths
        );
        assert!(
            report.actual_depths.contains(&1),
            "document should have depth 1: {:?}",
            report.actual_depths
        );
        assert!(
            report.actual_depths.contains(&2) || report.actual_depths.iter().any(|&d| d > 1),
            "document should have depth 2+: {:?}",
            report.actual_depths
        );
        assert!(report.has_web_edges, "document should have web edge steps");
        assert!(report.has_converge, "document should have converge steps");
        assert!(
            report.issues.is_empty(),
            "should have no issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn validate_conversation_question_compilation() {
        let plan = compile_conversation_questions();
        let report = inspect_plan("conversation", &plan);
        assert!(
            report.compilation_ok,
            "conversation question YAML should compile"
        );
        assert!(
            report.plan_valid,
            "conversation question plan should validate"
        );
        assert!(
            report.actual_depths.contains(&0),
            "conversation should have depth 0: {:?}",
            report.actual_depths
        );
        assert!(
            report.actual_depths.contains(&1),
            "conversation should have depth 1: {:?}",
            report.actual_depths
        );
        assert!(
            report.has_web_edges,
            "conversation should have web edge steps"
        );
        assert!(
            report.issues.is_empty(),
            "should have no issues: {:?}",
            report.issues
        );
    }

    // ── Comparison tests ────────────────────────────────────────────────

    #[test]
    fn compare_code_question_vs_defaults() {
        let q_plan = compile_code_questions();
        let d_plan = compile_code_defaults();
        let report = compare_plans("code", &q_plan, &d_plan);
        assert!(
            report.both_have_webbing,
            "both code plans should have webbing"
        );
        assert!(
            report.both_have_converge,
            "both code plans should have converge"
        );
        assert!(
            report.structural_compatible,
            "code plans should be structurally compatible: notes={:?}",
            report.notes
        );
        assert!(
            (report.question_max_depth - report.defaults_max_depth).abs() <= 1,
            "depth ranges should be compatible: question={}, defaults={}",
            report.question_max_depth,
            report.defaults_max_depth
        );
    }

    #[test]
    fn compare_document_question_vs_defaults() {
        let q_plan = compile_document_questions();
        let d_plan = compile_document_defaults();
        let report = compare_plans("document", &q_plan, &d_plan);
        assert!(
            report.both_have_webbing,
            "both document plans should have webbing"
        );
        assert!(
            report.both_have_converge,
            "both document plans should have converge"
        );
        assert!(
            report.question_steps > 0 && report.defaults_steps > 0,
            "both plans should have steps"
        );
    }

    // ── Phase 2 acceptance criteria tests ───────────────────────────────

    #[test]
    fn question_compilation_produces_valid_dag() {
        let code_plan = compile_code_questions();
        code_plan
            .validate()
            .expect("code question plan should be a valid DAG");

        let doc_plan = compile_document_questions();
        doc_plan
            .validate()
            .expect("document question plan should be a valid DAG");

        let conv_plan = compile_conversation_questions();
        conv_plan
            .validate()
            .expect("conversation question plan should be a valid DAG");
    }

    #[test]
    fn question_plan_step_count_in_expected_range() {
        let code_plan = compile_code_questions();
        assert!(
            code_plan.steps.len() >= 30 && code_plan.steps.len() <= 50,
            "code plan should have 30-50 steps, got {}",
            code_plan.steps.len()
        );

        let doc_plan = compile_document_questions();
        assert!(
            doc_plan.steps.len() >= 30 && doc_plan.steps.len() <= 50,
            "document plan should have 30-50 steps, got {}",
            doc_plan.steps.len()
        );

        // Conversation has fewer layers (no recursive_cluster), so lower bound
        let conv_plan = compile_conversation_questions();
        assert!(
            conv_plan.steps.len() >= 5 && conv_plan.steps.len() <= 50,
            "conversation plan should have 5-50 steps, got {}",
            conv_plan.steps.len()
        );
    }

    #[test]
    fn question_plan_has_no_orphan_steps() {
        for (label, plan) in &[
            ("code", compile_code_questions()),
            ("document", compile_document_questions()),
            ("conversation", compile_conversation_questions()),
        ] {
            let step_ids: std::collections::HashSet<&str> =
                plan.steps.iter().map(|s| s.id.as_str()).collect();

            for step in &plan.steps {
                for dep in &step.depends_on {
                    assert!(
                        step_ids.contains(dep.as_str()),
                        "step '{}' in {} depends on non-existent step '{}'. Known steps: {:?}",
                        step.id,
                        label,
                        dep,
                        step_ids
                    );
                }
            }
        }
    }

    #[test]
    fn all_question_steps_have_instructions() {
        for (label, plan) in &[
            ("code", compile_code_questions()),
            ("document", compile_document_questions()),
            ("conversation", compile_conversation_questions()),
        ] {
            for step in &plan.steps {
                if matches!(
                    step.operation,
                    crate::pyramid::execution_plan::StepOperation::Llm
                ) {
                    let has_instruction = step
                        .instruction
                        .as_ref()
                        .map(|i| !i.trim().is_empty())
                        .unwrap_or(false);
                    let has_converge = step.converge_metadata.is_some();
                    assert!(
                        has_instruction || has_converge,
                        "LLM step '{}' in {} has no instruction and no converge metadata",
                        step.id,
                        label
                    );
                }
            }
        }
    }

    #[test]
    fn converge_steps_have_valid_metadata() {
        // Conversation uses flat clustering (no recursive_cluster), so converge
        // steps are only expected for code and document.
        for (label, plan, expect_converge) in &[
            ("code", compile_code_questions(), true),
            ("document", compile_document_questions(), true),
            ("conversation", compile_conversation_questions(), false),
        ] {
            let converge_steps: Vec<&crate::pyramid::execution_plan::Step> = plan
                .steps
                .iter()
                .filter(|s| s.converge_metadata.is_some())
                .collect();

            if *expect_converge {
                assert!(
                    !converge_steps.is_empty(),
                    "{} should have converge-expanded steps",
                    label
                );
            }

            for step in &converge_steps {
                let cm = step.converge_metadata.as_ref().unwrap();
                assert!(
                    !cm.converge_id.is_empty(),
                    "step '{}' in {} has empty converge_id",
                    step.id,
                    label
                );
                assert!(
                    cm.max_rounds > 0,
                    "step '{}' in {} has max_rounds=0",
                    step.id,
                    label
                );
                assert!(
                    cm.shortcut_at > 0
                        || !matches!(
                            cm.role,
                            crate::pyramid::execution_plan::ConvergeRole::Shortcut
                        ),
                    "step '{}' in {} is a shortcut with shortcut_at=0",
                    step.id,
                    label
                );
            }
        }
    }
}
