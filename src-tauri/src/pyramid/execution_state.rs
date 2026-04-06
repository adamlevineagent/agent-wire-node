// pyramid/execution_state.rs — Runtime context for IR plan execution (Task A of P1.4)
//
// Owns:
//   - Step output storage (HashMap<String, Value> keyed by step ID)
//   - Independent write drain (mpsc channel for async DB writes)
//   - Resume state checker (3-state: Missing / Complete / StaleStep)
//   - Progress tracking (BuildProgress channel)
//   - Cancellation (CancellationToken wrapper)
//   - Accumulator management (sequential forEach state)
//   - Cost logging (db::insert_cost_log wrapper)
//   - DB access (reader + writer connections)
//
// This module does NOT touch chain_executor.rs or chain_dispatch.rs.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::chain_executor::ChunkProvider;
use super::db;
use super::execution_plan::{AccumulatorConfig, Step, StorageKind};
use super::types::{BuildProgress, PyramidNode};
use crate::utils::safe_slice_end;

// ── IR-specific WriteOp ─────────────────────────────────────────────────────
//
// Independent of build.rs WriteOp. Same variants (the IR executor owns its
// own drain so it never shares a channel with the legacy build pipeline).

#[derive(Debug)]
pub enum IrWriteOp {
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
    /// Flush guarantees all preceding writes are committed before the
    /// oneshot fires.  Used between converge rounds and before web-edge
    /// persistence.
    Flush {
        done: oneshot::Sender<()>,
    },
}

// ── Resume State ────────────────────────────────────────────────────────────

/// Three-state resume check.  Matches the semantics in chain_executor.rs
/// lines 77-127: Missing (needs execution), Complete (skip + load output),
/// StaleStep (step record exists but node is gone — re-execute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeState {
    /// No step record found — needs execution.
    Missing,
    /// Step record exists AND artifact (node) exists when the step saves nodes.
    Complete,
    /// Step record exists but the expected node is missing — re-execute.
    StaleStep,
}

// ── ExecutionState ──────────────────────────────────────────────────────────

/// Runtime context for a single IR plan execution.
///
/// Created once per `execute_plan` invocation.  The main execution loop
/// (Task C) reads and mutates this struct throughout the build.
pub struct ExecutionState {
    // ── identity ────────────────────────────────────────────────────────
    pub slug: String,
    pub content_type: String,
    pub chain_id: Option<String>,

    // ── data ────────────────────────────────────────────────────────────
    /// Lazy chunk provider — loads content on-demand from SQLite.
    pub chunks: ChunkProvider,
    /// Step outputs keyed by step ID.  Downstream steps resolve `$ref`
    /// expressions against this map.
    pub step_outputs: HashMap<String, Value>,

    // ── accumulator state (sequential forEach) ──────────────────────────
    pub accumulators: HashMap<String, String>,

    // ── per-iteration context (set/cleared by the loop) ─────────────────
    pub current_item: Option<Value>,
    pub current_index: Option<usize>,

    // ── resume ──────────────────────────────────────────────────────────
    pub has_prior_build: bool,

    // ── write drain ─────────────────────────────────────────────────────
    pub writer_tx: mpsc::Sender<IrWriteOp>,

    // ── progress ────────────────────────────────────────────────────────
    pub progress_tx: Option<mpsc::Sender<BuildProgress>>,
    pub done: i64,
    pub total: i64,

    // ── cancellation ────────────────────────────────────────────────────
    pub cancel: CancellationToken,

    // ── DB handles ──────────────────────────────────────────────────────
    pub reader: Arc<Mutex<Connection>>,
    pub writer: Arc<Mutex<Connection>>,
}

impl ExecutionState {
    // ── Constructor ─────────────────────────────────────────────────────

    /// Create a new ExecutionState and spawn the write drain task.
    ///
    /// Returns `(state, drain_join_handle)`.  The caller MUST drop the
    /// `ExecutionState` (which drops `writer_tx`) and then await the join
    /// handle to ensure all pending writes are flushed.
    pub fn new(
        slug: String,
        content_type: String,
        chain_id: Option<String>,
        chunks: ChunkProvider,
        has_prior_build: bool,
        total_estimated_nodes: u32,
        cancel: CancellationToken,
        progress_tx: Option<mpsc::Sender<BuildProgress>>,
        reader: Arc<Mutex<Connection>>,
        writer: Arc<Mutex<Connection>>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (writer_tx, drain_handle) = spawn_write_drain_ir(writer.clone());

        let state = Self {
            slug,
            content_type,
            chain_id,
            chunks,
            step_outputs: HashMap::new(),
            accumulators: HashMap::new(),
            current_item: None,
            current_index: None,
            has_prior_build,
            writer_tx,
            progress_tx,
            done: 0,
            total: total_estimated_nodes as i64,
            cancel,
            reader,
            writer,
        };

        (state, drain_handle)
    }

    // ── Cancellation ───────────────────────────────────────────────────

    /// Check whether cancellation has been requested.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    // ── Progress reporting ──────────────────────────────────────────────

    /// Report build progress.  Increments `done` and sends a progress
    /// message through the channel (if present).
    pub async fn report_progress(&mut self) {
        self.done += 1;
        if let Some(ref tx) = self.progress_tx {
            let _ = tx
                .send(BuildProgress {
                    done: self.done,
                    total: self.total,
                })
                .await;
        }
    }

    /// Send progress without incrementing done (e.g. initial 0/N report).
    pub async fn send_progress(&self) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx
                .send(BuildProgress {
                    done: self.done,
                    total: self.total,
                })
                .await;
        }
    }

    // ── Step output storage ─────────────────────────────────────────────

    /// Store a step's output for downstream `$ref` resolution.
    pub fn store_step_output(&mut self, step_id: &str, output: Value) {
        self.step_outputs.insert(step_id.to_string(), output);
    }

    /// Retrieve a step's output by step ID.
    pub fn get_step_output(&self, step_id: &str) -> Option<&Value> {
        self.step_outputs.get(step_id)
    }

    // ── Resume state checking ───────────────────────────────────────────

    /// Check resume state for a specific step iteration.
    ///
    /// Wraps `db::step_exists` + `db::get_node` into the 3-state check
    /// matching the semantics in chain_executor.rs lines 77-127.
    ///
    /// `saves_node`: true when the step's storage_directive.kind == Node.
    pub async fn check_resume_state(
        &self,
        step_name: &str,
        chunk_index: i64,
        depth: i64,
        node_id: &str,
        saves_node: bool,
    ) -> Result<ResumeState> {
        let slug = self.slug.clone();
        let step_name = step_name.to_string();
        let node_id = node_id.to_string();
        let db = self.reader.clone();

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            if !db::step_exists(&conn, &slug, &step_name, chunk_index, depth, &node_id)? {
                return Ok(ResumeState::Missing);
            }
            if saves_node {
                if db::get_node(&conn, &slug, &node_id)?.is_some() {
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

    /// Load a previously saved step output from the DB (for resume hydration).
    pub async fn load_step_output_from_db(
        &self,
        step_name: &str,
        chunk_index: i64,
    ) -> Result<Option<String>> {
        let slug = self.slug.clone();
        let step_name = step_name.to_string();
        let db = self.reader.clone();

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::get_step_output(&conn, &slug, &step_name, chunk_index)
        })
        .await?
    }

    /// Load a previously saved step output with exact match (slug + step + chunk + depth + node_id).
    pub async fn load_step_output_exact(
        &self,
        step_name: &str,
        chunk_index: i64,
        depth: i64,
        node_id: &str,
    ) -> Result<Option<String>> {
        let slug = self.slug.clone();
        let step_name = step_name.to_string();
        let node_id = node_id.to_string();
        let db = self.reader.clone();

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::get_step_output_exact(&conn, &slug, &step_name, chunk_index, depth, &node_id)
        })
        .await?
    }

    // ── Accumulator management ──────────────────────────────────────────

    /// Update accumulators from a step output based on the IR AccumulatorConfig.
    ///
    /// Matches the logic in chain_executor.rs lines 3464-3516:
    ///  - Navigate output via dot-path (stripping $item.output. / $item. prefix)
    ///  - Apply max_chars truncation (UTF-8 safe)
    pub fn update_accumulators(&mut self, output: &Value, config: &AccumulatorConfig) {
        let path = config
            .field
            .strip_prefix("$item.output.")
            .or_else(|| config.field.strip_prefix("$item."))
            .unwrap_or(&config.field);

        // Navigate the output value along the dot-path
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

        if !found {
            return;
        }

        let new_val = match &current {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        // Apply max_chars truncation (UTF-8 safe via safe_slice_end)
        let max_chars = config.max_chars.unwrap_or(usize::MAX);
        let truncated = if new_val.len() > max_chars {
            safe_slice_end(&new_val, max_chars).to_string()
        } else {
            new_val
        };

        // The accumulator name is derived from the field path.
        // Use the last segment as the accumulator key (e.g. "running_context").
        let acc_name = path.rsplit('.').next().unwrap_or(path);
        self.accumulators.insert(acc_name.to_string(), truncated);
    }

    /// Seed accumulators from config before the first iteration.
    pub fn seed_accumulators(&mut self, config: &AccumulatorConfig) {
        let acc_name = config
            .field
            .strip_prefix("$item.output.")
            .or_else(|| config.field.strip_prefix("$item."))
            .unwrap_or(&config.field);
        let acc_name = acc_name.rsplit('.').next().unwrap_or(acc_name);

        if let Some(ref seed) = config.seed {
            let seed_str = match seed {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            self.accumulators.insert(acc_name.to_string(), seed_str);
        } else {
            self.accumulators.entry(acc_name.to_string()).or_default();
        }
    }

    // ── Write drain helpers ─────────────────────────────────────────────

    /// Send a SaveNode operation through the write drain.
    pub async fn send_save_node(&self, node: PyramidNode, topics_json: Option<String>) {
        if let Err(e) = self
            .writer_tx
            .send(IrWriteOp::SaveNode { node, topics_json })
            .await
        {
            warn!("IR writer channel closed, SaveNode dropped: {e}");
        }
    }

    /// Send a SaveStep operation through the write drain.
    pub async fn send_save_step(
        &self,
        step_type: &str,
        chunk_index: i64,
        depth: i64,
        node_id: &str,
        output_json: &str,
        model: &str,
        elapsed: f64,
    ) {
        if let Err(e) = self
            .writer_tx
            .send(IrWriteOp::SaveStep {
                slug: self.slug.clone(),
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
            warn!("IR writer channel closed, SaveStep dropped: {e}");
        }
    }

    /// Send an UpdateParent operation through the write drain.
    pub async fn send_update_parent(&self, node_id: &str, parent_id: &str) {
        if let Err(e) = self
            .writer_tx
            .send(IrWriteOp::UpdateParent {
                slug: self.slug.clone(),
                node_id: node_id.to_string(),
                parent_id: parent_id.to_string(),
            })
            .await
        {
            warn!("IR writer channel closed, UpdateParent dropped: {e}");
        }
    }

    /// Send an UpdateStats operation through the write drain.
    pub async fn send_update_stats(&self) {
        if let Err(e) = self
            .writer_tx
            .send(IrWriteOp::UpdateStats {
                slug: self.slug.clone(),
            })
            .await
        {
            warn!("IR writer channel closed, UpdateStats dropped: {e}");
        }
    }

    /// Flush all pending writes, blocking until the drain has processed them.
    /// Critical between converge rounds and before web-edge persistence.
    pub async fn flush_writes(&self) {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.writer_tx.send(IrWriteOp::Flush { done: tx }).await {
            warn!("IR writer channel closed, Flush dropped: {e}");
            return;
        }
        let _ = rx.await;
    }

    // ── Cost logging ────────────────────────────────────────────────────

    /// Log an LLM call cost to the database.
    ///
    /// Uses the writer connection directly (cost logs are non-critical
    /// and don't need to go through the write drain ordering).
    pub async fn log_cost(
        &self,
        step_name: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        estimated_cost: f64,
        tier: Option<&str>,
        latency_ms: Option<i64>,
        generation_id: Option<&str>,
        estimated_cost_usd: Option<f64>,
    ) -> Result<()> {
        let slug = self.slug.clone();
        let chain_id = self.chain_id.clone();
        let step_name = step_name.to_string();
        let model = model.to_string();
        let tier = tier.map(|s| s.to_string());
        let generation_id = generation_id.map(|s| s.to_string());
        let db = self.writer.clone();

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::insert_cost_log(
                &conn,
                &slug,
                &step_name,
                &model,
                input_tokens,
                output_tokens,
                estimated_cost,
                "ir_executor",
                None, // layer
                None, // check_type
                chain_id.as_deref(),
                Some(&step_name),
                tier.as_deref(),
                latency_ms,
                generation_id.as_deref(),
                estimated_cost_usd,
            )
        })
        .await?
    }

    // ── Cleanup (from_depth rebuild) ──────────────────────────────────

    /// Supersede nodes and scope execution tables at and above `from_depth`.
    ///
    /// Wraps `chain_executor::cleanup_from_depth_sync` (now public) so that
    /// both the legacy and IR execution paths share the same cleanup logic.
    pub async fn cleanup_from_depth(&self, from_depth: i64) -> Result<()> {
        let slug = self.slug.clone();
        let build_id = format!("rebuild-{}", uuid::Uuid::new_v4());
        let db = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            super::chain_executor::cleanup_from_depth_sync(&conn, &slug, from_depth, &build_id)
        })
        .await?
    }

    // ── Helpers for step metadata ───────────────────────────────────────

    /// Determine whether a step saves nodes (checks storage_directive.kind == Node).
    pub fn step_saves_node(step: &Step) -> bool {
        step.storage_directive
            .as_ref()
            .map(|sd| sd.kind == StorageKind::Node)
            .unwrap_or(false)
    }

    /// Get the depth from a step's storage directive (defaults to 0).
    pub fn step_depth(step: &Step) -> i64 {
        step.storage_directive
            .as_ref()
            .and_then(|sd| sd.depth)
            .or_else(|| {
                step.metadata
                    .as_ref()
                    .and_then(|meta| meta.get("target_depth"))
                    .and_then(|depth| depth.as_i64())
            })
            .unwrap_or(0)
    }
}

// ── Write drain spawn ───────────────────────────────────────────────────────
//
// Independent of build.rs spawn_write_drain.  Same pattern: single-writer
// task consuming IrWriteOp from an mpsc channel.

/// Spawn an independent write drain task for the IR executor.
///
/// Returns `(sender, join_handle)`.  Drop the sender to signal completion,
/// then await the handle to ensure all queued writes are flushed.
fn spawn_write_drain_ir(
    writer: Arc<Mutex<Connection>>,
) -> (mpsc::Sender<IrWriteOp>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<IrWriteOp>(256);
    let handle = tokio::spawn(async move {
        while let Some(op) = rx.recv().await {
            let result = {
                let conn = writer.lock().await;
                match op {
                    IrWriteOp::SaveNode {
                        ref node,
                        ref topics_json,
                    } => db::save_node(&conn, node, topics_json.as_deref()),
                    IrWriteOp::SaveStep {
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
                    IrWriteOp::UpdateParent {
                        ref slug,
                        ref node_id,
                        ref parent_id,
                    } => db::update_parent(&conn, slug, node_id, parent_id),
                    IrWriteOp::UpdateStats { ref slug } => db::update_slug_stats(&conn, slug),
                    IrWriteOp::Flush { done } => {
                        let _ = done.send(());
                        Ok(())
                    }
                }
            };
            if let Err(e) = result {
                error!("IR WriteOp failed: {e}");
            }
        }
    });
    (tx, handle)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Accumulator tests ───────────────────────────────────────────────

    fn make_accumulator_config(field: &str, max_chars: Option<usize>) -> AccumulatorConfig {
        AccumulatorConfig {
            field: field.to_string(),
            seed: None,
            max_chars,
            trim_to: None,
            trim_side: None,
        }
    }

    #[test]
    fn accumulator_extracts_from_dot_path() {
        let output = json!({
            "running_context": "The project uses Rust and TypeScript."
        });
        let config = make_accumulator_config("$item.output.running_context", None);

        let mut accumulators = HashMap::new();
        // Simulate the update logic directly
        let path = config
            .field
            .strip_prefix("$item.output.")
            .or_else(|| config.field.strip_prefix("$item."))
            .unwrap_or(&config.field);

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

        assert!(found);
        let val = match &current {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let acc_name = path.rsplit('.').next().unwrap_or(path);
        accumulators.insert(acc_name.to_string(), val);

        assert_eq!(
            accumulators.get("running_context").unwrap(),
            "The project uses Rust and TypeScript."
        );
    }

    #[test]
    fn accumulator_applies_max_chars_truncation() {
        let output = json!({
            "running_context": "A very long string that exceeds the max chars limit set in config"
        });
        let config = make_accumulator_config("$item.output.running_context", Some(20));

        let mut accumulators = HashMap::new();
        let path = "running_context";
        let val = output
            .get(path)
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();

        let max_chars = config.max_chars.unwrap_or(usize::MAX);
        let truncated = if val.len() > max_chars {
            safe_slice_end(&val, max_chars).to_string()
        } else {
            val
        };
        accumulators.insert(path.to_string(), truncated);

        let result = accumulators.get("running_context").unwrap();
        assert!(result.len() <= 20);
    }

    #[test]
    fn accumulator_handles_nested_dot_path() {
        let output = json!({
            "analysis": {
                "summary": {
                    "text": "Nested value"
                }
            }
        });
        let config = make_accumulator_config("$item.output.analysis.summary.text", None);

        let path = config
            .field
            .strip_prefix("$item.output.")
            .unwrap_or(&config.field);

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

        assert!(found);
        assert_eq!(current.as_str().unwrap(), "Nested value");
    }

    #[test]
    fn accumulator_missing_path_does_not_crash() {
        let output = json!({"other_field": "value"});
        let config = make_accumulator_config("$item.output.running_context", None);

        let path = config
            .field
            .strip_prefix("$item.output.")
            .unwrap_or(&config.field);

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

        assert!(!found);
    }

    #[test]
    fn accumulator_non_string_value_serializes() {
        let output = json!({"count": 42});
        let config = make_accumulator_config("$item.output.count", None);

        let path = config
            .field
            .strip_prefix("$item.output.")
            .unwrap_or(&config.field);

        let current = output.get(path).unwrap().clone();
        let val = match &current {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        assert_eq!(val, "42");
    }

    // ── Resume state tests ──────────────────────────────────────────────

    #[test]
    fn resume_state_enum_values() {
        assert_ne!(ResumeState::Missing, ResumeState::Complete);
        assert_ne!(ResumeState::Complete, ResumeState::StaleStep);
        assert_ne!(ResumeState::Missing, ResumeState::StaleStep);
    }

    // ── Step metadata helper tests ──────────────────────────────────────

    fn make_test_step(id: &str) -> Step {
        use super::super::execution_plan::*;
        Step {
            id: id.to_string(),
            operation: StepOperation::Llm,
            primitive: Some("extract".to_string()),
            depends_on: vec![],
            iteration: None,
            input: json!({}),
            instruction: Some("prompt".to_string()),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: None,
            converge_metadata: None,
            metadata: None,
            scope: None,
        }
    }

    #[test]
    fn step_saves_node_true_for_node_storage() {
        use super::super::execution_plan::*;
        let mut step = make_test_step("extract_l0");
        step.storage_directive = Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(0),
            node_id_pattern: Some("C-L0-{index:03}".to_string()),
            target: None,
        });
        assert!(ExecutionState::step_saves_node(&step));
    }

    #[test]
    fn step_saves_node_false_for_step_only() {
        use super::super::execution_plan::*;
        let mut step = make_test_step("classify");
        step.storage_directive = Some(StorageDirective {
            kind: StorageKind::StepOnly,
            depth: None,
            node_id_pattern: None,
            target: None,
        });
        assert!(!ExecutionState::step_saves_node(&step));
    }

    #[test]
    fn step_saves_node_false_for_no_directive() {
        let step = make_test_step("transform");
        assert!(!ExecutionState::step_saves_node(&step));
    }

    #[test]
    fn step_depth_from_directive() {
        use super::super::execution_plan::*;
        let mut step = make_test_step("synth_l2");
        step.storage_directive = Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(2),
            node_id_pattern: Some("L2-{index:03}".to_string()),
            target: None,
        });
        assert_eq!(ExecutionState::step_depth(&step), 2);
    }

    #[test]
    fn step_depth_uses_target_depth_metadata_when_storage_missing() {
        let mut step = make_test_step("l2_synthesis_r0_classify");
        step.metadata = Some(serde_json::json!({
            "target_depth": 2,
        }));
        assert_eq!(ExecutionState::step_depth(&step), 2);
    }

    #[test]
    fn step_depth_defaults_to_zero() {
        let step = make_test_step("no_storage");
        assert_eq!(ExecutionState::step_depth(&step), 0);
    }

    // ── Write drain tests (async) ───────────────────────────────────────

    #[tokio::test]
    async fn write_drain_processes_flush() {
        let db = db_for_test();
        let (tx, handle) = spawn_write_drain_ir(db);

        // Send a flush and verify it completes
        let (flush_tx, flush_rx) = oneshot::channel();
        tx.send(IrWriteOp::Flush { done: flush_tx }).await.unwrap();
        flush_rx.await.unwrap();

        // Drop sender, drain should exit
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn write_drain_processes_update_stats() {
        let db = db_for_test();
        let (tx, handle) = spawn_write_drain_ir(db);

        // UpdateStats on a non-existent slug won't crash (the SQL just updates 0 rows)
        tx.send(IrWriteOp::UpdateStats {
            slug: "test-slug".to_string(),
        })
        .await
        .unwrap();

        // Flush to ensure it was processed
        let (flush_tx, flush_rx) = oneshot::channel();
        tx.send(IrWriteOp::Flush { done: flush_tx }).await.unwrap();
        flush_rx.await.unwrap();

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn write_drain_save_step_and_resume_check() {
        let db = db_for_test();
        let db_clone = db.clone();
        let (tx, handle) = spawn_write_drain_ir(db.clone());

        // Create the slug first so the FK constraint is satisfied
        {
            let conn = db_clone.lock().await;
            conn.execute(
                "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, ?2, ?3)",
                rusqlite::params!["test-slug", "code", "/tmp/test"],
            )
            .unwrap();
        }

        // Send a SaveStep
        tx.send(IrWriteOp::SaveStep {
            slug: "test-slug".to_string(),
            step_type: "extract_l0".to_string(),
            chunk_index: 0,
            depth: 0,
            node_id: "C-L0-000".to_string(),
            output_json: r#"{"headline":"test"}"#.to_string(),
            model: "test-model".to_string(),
            elapsed: 1.5,
        })
        .await
        .unwrap();

        // Flush to ensure it was written
        let (flush_tx, flush_rx) = oneshot::channel();
        tx.send(IrWriteOp::Flush { done: flush_tx }).await.unwrap();
        flush_rx.await.unwrap();

        // Now verify via db::step_exists
        let exists = {
            let conn = db_clone.lock().await;
            db::step_exists(&conn, "test-slug", "extract_l0", 0, 0, "C-L0-000").unwrap()
        };
        assert!(exists, "step should exist after SaveStep + Flush");

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn execution_state_new_and_progress() {
        let db = db_for_test();
        let cancel = CancellationToken::new();
        let (ptx, mut prx) = mpsc::channel(16);

        let (mut state, drain_handle) = ExecutionState::new(
            "test".to_string(),
            "code".to_string(),
            Some("code-default".to_string()),
            ChunkProvider::empty(),
            false,
            10,
            cancel.clone(),
            Some(ptx),
            db.clone(),
            db.clone(),
        );

        // Initial progress
        state.send_progress().await;
        let p = prx.recv().await.unwrap();
        assert_eq!(p.done, 0);
        assert_eq!(p.total, 10);

        // Report progress increments done
        state.report_progress().await;
        let p = prx.recv().await.unwrap();
        assert_eq!(p.done, 1);
        assert_eq!(p.total, 10);

        state.report_progress().await;
        let p = prx.recv().await.unwrap();
        assert_eq!(p.done, 2);

        // Cancellation
        assert!(!state.is_cancelled());
        cancel.cancel();
        assert!(state.is_cancelled());

        drop(state);
        drain_handle.await.unwrap();
    }

    #[tokio::test]
    async fn execution_state_step_output_storage() {
        let db = db_for_test();
        let cancel = CancellationToken::new();

        let (mut state, drain_handle) = ExecutionState::new(
            "test".to_string(),
            "code".to_string(),
            None,
            ChunkProvider::empty(),
            false,
            0,
            cancel,
            None,
            db.clone(),
            db.clone(),
        );

        assert!(state.get_step_output("step_a").is_none());

        state.store_step_output("step_a", json!({"headline": "hello"}));
        let output = state.get_step_output("step_a").unwrap();
        assert_eq!(output["headline"], "hello");

        // Overwrite
        state.store_step_output("step_a", json!({"headline": "updated"}));
        assert_eq!(
            state.get_step_output("step_a").unwrap()["headline"],
            "updated"
        );

        drop(state);
        drain_handle.await.unwrap();
    }

    #[tokio::test]
    async fn execution_state_accumulator_update() {
        let db = db_for_test();
        let cancel = CancellationToken::new();

        let (mut state, drain_handle) = ExecutionState::new(
            "test".to_string(),
            "code".to_string(),
            None,
            ChunkProvider::empty(),
            false,
            0,
            cancel,
            None,
            db.clone(),
            db.clone(),
        );

        let config = make_accumulator_config("$item.output.running_context", Some(30));
        state.seed_accumulators(&config);
        assert_eq!(state.accumulators.get("running_context").unwrap(), "");

        let output = json!({"running_context": "First iteration context value here"});
        state.update_accumulators(&output, &config);
        let val = state.accumulators.get("running_context").unwrap();
        assert!(val.len() <= 30);
        assert!(val.starts_with("First iteration context value "));

        // Second update overwrites
        let output2 = json!({"running_context": "Short"});
        state.update_accumulators(&output2, &config);
        assert_eq!(state.accumulators.get("running_context").unwrap(), "Short");

        drop(state);
        drain_handle.await.unwrap();
    }

    #[tokio::test]
    async fn execution_state_accumulator_seed_with_value() {
        let db = db_for_test();
        let cancel = CancellationToken::new();

        let (mut state, drain_handle) = ExecutionState::new(
            "test".to_string(),
            "code".to_string(),
            None,
            ChunkProvider::empty(),
            false,
            0,
            cancel,
            None,
            db.clone(),
            db.clone(),
        );

        let config = AccumulatorConfig {
            field: "$item.output.running_context".to_string(),
            seed: Some(json!("initial seed value")),
            max_chars: None,
            trim_to: None,
            trim_side: None,
        };
        state.seed_accumulators(&config);
        assert_eq!(
            state.accumulators.get("running_context").unwrap(),
            "initial seed value"
        );

        drop(state);
        drain_handle.await.unwrap();
    }

    // ── Test helpers ────────────────────────────────────────────────────

    /// Create an in-memory SQLite connection with the pyramid schema initialized.
    fn db_for_test() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }
}
