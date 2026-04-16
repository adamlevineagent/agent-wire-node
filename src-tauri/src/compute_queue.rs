// compute_queue.rs — Per-model FIFO compute queue.
//
// Replaces the global LOCAL_PROVIDER_SEMAPHORE as the serializer for
// local LLM calls. Each model_id gets its own FIFO queue. The GPU
// processing loop (spawned in main.rs) drains items round-robin across
// models so no single model starves.
//
// Phase 1 (shipped): local builds. The queue is transparent: LlmConfig
// carries an optional ComputeQueueHandle, and
// call_model_unified_with_audit_and_ctx checks it. When present, the
// call enqueues + awaits a oneshot; when absent (tests, pre-init),
// the call goes straight to HTTP.
//
// Phase 2 (WS4, this commit): adds `enqueue_market` — the same queue,
// a different admission gate. Local enqueues block until the GPU loop
// picks them up and NEVER reject (the calling builder already paid
// its admission with the step's local-provider priority). Market
// enqueues respect a per-offer `max_market_depth` cap — when reached,
// the dispatch handler rejects with 503 + Retry-After per
// phase-2-exchange.md §III, handing the job back to the Wire for
// re-match instead of starving fleet or local work. The `source`
// field on QueueEntry is the discriminator the depth counter uses to
// scope the cap to market entries only — local + fleet-received
// entries don't count against the market quota.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, oneshot};

use crate::pyramid::llm::{LlmCallOptions, LlmConfig, LlmResponse};
use crate::pyramid::step_context::StepContext;

/// Handle for both enqueueing and consuming. Cloneable (Arc-wrapped).
/// Lives on LlmConfig (for enqueueing) and AppState (for the GPU loop).
#[derive(Clone)]
pub struct ComputeQueueHandle {
    pub queue: Arc<Mutex<ComputeQueueManager>>,
    pub notify: Arc<Notify>,
}

impl ComputeQueueHandle {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(ComputeQueueManager::new())),
            notify: Arc::new(Notify::new()),
        }
    }
}

pub struct ComputeQueueManager {
    queues: HashMap<String, ModelQueue>,
    round_robin_keys: Vec<String>,
    round_robin_index: usize,
}

struct ModelQueue {
    entries: VecDeque<QueueEntry>,
}

/// Everything the GPU loop needs to execute the LLM call.
/// StepContext MUST be preserved (Law 4).
pub struct QueueEntry {
    /// Oneshot sender to return the result to the waiting caller.
    pub result_tx: oneshot::Sender<anyhow::Result<LlmResponse>>,
    /// Full config — with compute_queue: None to prevent re-enqueue.
    pub config: LlmConfig,
    /// System prompt for the LLM call.
    pub system_prompt: String,
    /// User prompt for the LLM call.
    pub user_prompt: String,
    /// Temperature for the call.
    pub temperature: f32,
    /// Max tokens for the call.
    pub max_tokens: usize,
    /// Optional response format (structured output).
    pub response_format: Option<serde_json::Value>,
    /// Call options — with skip_concurrency_gate: true.
    pub options: LlmCallOptions,
    /// Law 4: StepContext MUST flow through the queue.
    pub step_ctx: Option<StepContext>,
    /// Queue routing key (model id or "default").
    pub model_id: String,
    /// When the item was enqueued (for latency tracking).
    pub enqueued_at: std::time::Instant,
    /// DADBEAR work item ID for correlating queue results back to durable
    /// work items. None for non-DADBEAR callers (interactive, manual builds).
    pub work_item_id: Option<String>,
    /// DADBEAR attempt ID for this dispatch attempt. None for non-DADBEAR callers.
    pub attempt_id: Option<String>,
    /// Compute source: "local" for own builds, "fleet_received" for fleet peer work.
    /// Set explicitly at enqueue time — the GPU loop reads this directly.
    pub source: String,
    /// Semantic job_path for chronicle event grouping.
    /// Generated at enqueue time via generate_job_path.
    pub job_path: String,
    /// Pre-assigned job_path from upstream handlers (fleet_received).
    /// When Some, the enqueue site uses this instead of generating a new path,
    /// so one logical fleet job keeps a single job_path across its entire lifecycle.
    pub chronicle_job_path: Option<String>,
}

impl ComputeQueueManager {
    pub fn new() -> Self {
        Self {
            queues: HashMap::new(),
            round_robin_keys: Vec::new(),
            round_robin_index: 0,
        }
    }

    /// Push an entry to the model's FIFO queue.
    pub fn enqueue_local(&mut self, model_id: &str, entry: QueueEntry) {
        let queue = self
            .queues
            .entry(model_id.to_string())
            .or_insert_with(|| {
                // New model queue — add to round-robin rotation.
                self.round_robin_keys.push(model_id.to_string());
                ModelQueue {
                    entries: VecDeque::new(),
                }
            });
        queue.entries.push_back(entry);
    }

    /// Push a market-received entry to the model's FIFO queue, subject
    /// to the offer's `max_market_depth` cap.
    ///
    /// Differs from `enqueue_local` in three ways per phase-2-exchange.md
    /// §III lines 326-399:
    ///   1. **Respects a per-model cap** scoped to market entries only.
    ///      Local + fleet-received entries on the same model's queue
    ///      are NOT counted against the cap — they're paid-for-
    ///      elsewhere admission that the market quota shouldn't
    ///      displace.
    ///   2. **Can reject** with `QueueError::DepthExceeded`. The Phase 2
    ///      WS5 dispatch handler translates this into HTTP 503 +
    ///      Retry-After so the Wire re-matches the job to a different
    ///      provider.
    ///   3. **Forces `source = "market_received"` on the entry** before
    ///      pushing. This makes the depth-count invariant
    ///      self-enforcing — a caller that forgot to set source would
    ///      silently slip past the cap, which is exactly the failure
    ///      mode "market admission can't starve fleet" exists to
    ///      prevent. The caller can still set work_item_id + attempt_id
    ///      + the other DADBEAR-correlation fields; only source is
    ///      overwritten.
    ///
    /// The caller (Phase 2 WS5 dispatch handler) MUST create the
    /// DADBEAR work item BEFORE calling this method — queue entries
    /// without a work_item_id/attempt_id break the GPU-loop chronicle
    /// correlation.
    ///
    /// Returns the zero-based position in the queue on success (useful
    /// for the MarketDispatchAck's `peer_queue_depth` and for the
    /// queue-mirror push).
    pub fn enqueue_market(
        &mut self,
        model_id: &str,
        mut entry: QueueEntry,
        max_market_depth: usize,
    ) -> Result<usize, QueueError> {
        // Force the source field — see method docstring for rationale.
        entry.source = "market_received".to_string();

        let queue = self
            .queues
            .entry(model_id.to_string())
            .or_insert_with(|| {
                self.round_robin_keys.push(model_id.to_string());
                ModelQueue {
                    entries: VecDeque::new(),
                }
            });

        // Count current market entries only. Local + fleet_received
        // entries on the same queue don't count against the market cap.
        let current_market_depth = queue
            .entries
            .iter()
            .filter(|e| e.source == "market_received")
            .count();

        if current_market_depth >= max_market_depth {
            return Err(QueueError::DepthExceeded {
                model_id: model_id.to_string(),
                current: current_market_depth,
                max: max_market_depth,
            });
        }

        let position = queue.entries.len();
        queue.entries.push_back(entry);
        Ok(position)
    }

    /// Count of market-source entries in a specific model's queue.
    /// Used by the admission gate at phase-2-exchange.md §III to
    /// determine Retry-After suggestions, and by the queue mirror
    /// push (WS6) to surface per-offer availability to the Wire.
    pub fn market_queue_depth(&self, model_id: &str) -> usize {
        self.queues
            .get(model_id)
            .map(|q| {
                q.entries
                    .iter()
                    .filter(|e| e.source == "market_received")
                    .count()
            })
            .unwrap_or(0)
    }

    /// Pop from the next non-empty queue (round-robin for fairness).
    pub fn dequeue_next(&mut self) -> Option<QueueEntry> {
        if self.round_robin_keys.is_empty() {
            return None;
        }

        let key_count = self.round_robin_keys.len();
        for _ in 0..key_count {
            let idx = self.round_robin_index % key_count;
            self.round_robin_index = idx + 1;

            let key = &self.round_robin_keys[idx];
            if let Some(queue) = self.queues.get_mut(key) {
                if let Some(entry) = queue.entries.pop_front() {
                    return Some(entry);
                }
            }
        }

        None
    }

    /// Depth of a specific model's queue.
    pub fn queue_depth(&self, model_id: &str) -> usize {
        self.queues
            .get(model_id)
            .map(|q| q.entries.len())
            .unwrap_or(0)
    }

    /// Total depth across all model queues.
    pub fn total_depth(&self) -> usize {
        self.queues.values().map(|q| q.entries.len()).sum()
    }

    /// Per-model queue depths for fleet announcements.
    pub fn all_depths(&self) -> HashMap<String, usize> {
        self.queues
            .iter()
            .map(|(k, q)| (k.clone(), q.entries.len()))
            .collect()
    }
}

/// Categorized failure modes for compute-queue admission.
///
/// Phase 2 WS5 dispatch handler maps:
///   - `DepthExceeded` → HTTP 503 with `Retry-After` (offer's
///     `max_queue_depth` is the per-offer cap; the Wire re-matches
///     the job to a different provider).
///   - `ModelNotLoaded` → HTTP 503 with `X-Wire-Reason: model_not_loaded`
///     (operator unloaded the model; offer should deactivate on the
///     next descriptor push).
///
/// Only `DepthExceeded` is returned by `enqueue_market` itself — the
/// `ModelNotLoaded` variant is provided so WS5's admission gate can
/// return the same error type whether it fails at model lookup (no
/// loaded model matches `req.model`) or at queue push (depth cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// The model's market-source queue is at `max_market_depth`.
    /// Current count + cap are carried so the handler can suggest a
    /// useful `Retry-After` (e.g. `current / throughput`).
    DepthExceeded {
        model_id: String,
        current: usize,
        max: usize,
    },
    /// The requested `model_id` is not loaded on this node. Used by
    /// the WS5 admission gate before it even looks up the offer.
    ModelNotLoaded { model_id: String },
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::DepthExceeded { model_id, current, max } => write!(
                f,
                "compute queue for model '{model_id}' at market-source depth {current} (cap {max})"
            ),
            QueueError::ModelNotLoaded { model_id } => write!(
                f,
                "compute queue: model '{model_id}' is not loaded on this node"
            ),
        }
    }
}

impl std::error::Error for QueueError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::llm::LlmCallOptions;

    // ── Helpers ──────────────────────────────────────────────────────

    fn sample_entry(model_id: &str, source: &str) -> QueueEntry {
        let (tx, _rx) = oneshot::channel();
        QueueEntry {
            result_tx: tx,
            config: LlmConfig::default(),
            system_prompt: String::new(),
            user_prompt: String::new(),
            temperature: 0.0,
            max_tokens: 0,
            response_format: None,
            options: LlmCallOptions::default(),
            step_ctx: None,
            model_id: model_id.to_string(),
            enqueued_at: std::time::Instant::now(),
            work_item_id: None,
            attempt_id: None,
            source: source.to_string(),
            job_path: "test/path".to_string(),
            chronicle_job_path: None,
        }
    }

    // ── enqueue_market success path ──────────────────────────────────

    #[test]
    fn enqueue_market_accepts_under_cap_and_returns_position() {
        let mut mgr = ComputeQueueManager::new();
        let p0 = mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 2)
            .unwrap();
        let p1 = mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 2)
            .unwrap();
        assert_eq!(p0, 0);
        assert_eq!(p1, 1);
        assert_eq!(mgr.queue_depth("m"), 2);
        assert_eq!(mgr.market_queue_depth("m"), 2);
    }

    #[test]
    fn enqueue_market_rejects_when_market_depth_at_cap() {
        let mut mgr = ComputeQueueManager::new();
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 1)
            .unwrap();
        let err = mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 1)
            .unwrap_err();
        match err {
            QueueError::DepthExceeded { model_id, current, max } => {
                assert_eq!(model_id, "m");
                assert_eq!(current, 1);
                assert_eq!(max, 1);
            }
            other => panic!("expected DepthExceeded, got {other:?}"),
        }
        // The rejected push must not have landed on the queue.
        assert_eq!(mgr.queue_depth("m"), 1);
    }

    #[test]
    fn enqueue_market_forces_source_field() {
        // Caller accidentally passes source="local" — enqueue_market
        // MUST overwrite to "market_received" so the depth counter
        // sees it correctly. Without this, a bad caller could bypass
        // the market quota silently.
        let mut mgr = ComputeQueueManager::new();
        let entry = sample_entry("m", "local"); // WRONG source
        mgr.enqueue_market("m", entry, 1).unwrap();
        assert_eq!(mgr.market_queue_depth("m"), 1,
            "enqueue_market must force source='market_received'");
    }

    // ── Cap scoping to market entries only ───────────────────────────

    #[test]
    fn local_entries_do_not_count_against_market_cap() {
        // max_market_depth=1 means one MARKET entry max. Local/fleet
        // entries on the same model don't count.
        let mut mgr = ComputeQueueManager::new();
        // Three local entries in front.
        mgr.enqueue_local("m", sample_entry("m", "local"));
        mgr.enqueue_local("m", sample_entry("m", "local"));
        mgr.enqueue_local("m", sample_entry("m", "local"));
        // One fleet entry.
        mgr.enqueue_local("m", sample_entry("m", "fleet_received"));
        // First market entry — should succeed since market count=0.
        let p = mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 1)
            .unwrap();
        assert_eq!(p, 4, "position is total queue depth, not market depth");
        // Second market entry — market count=1, hits cap, rejects.
        assert!(mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 1)
            .is_err());
        // But queue_depth(total) is 5, market_queue_depth is 1.
        assert_eq!(mgr.queue_depth("m"), 5);
        assert_eq!(mgr.market_queue_depth("m"), 1);
    }

    #[test]
    fn depth_check_uses_count_of_market_source_not_total() {
        // A queue with 10 total entries (9 local + 1 market) should
        // still accept a market push under max_market_depth=3.
        let mut mgr = ComputeQueueManager::new();
        for _ in 0..9 {
            mgr.enqueue_local("m", sample_entry("m", "local"));
        }
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 3)
            .unwrap();
        // Still room: market count=1 < cap=3.
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 3)
            .unwrap();
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 3)
            .unwrap();
        // Cap hit at market count=3.
        assert!(mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 3)
            .is_err());
        assert_eq!(mgr.queue_depth("m"), 12);
        assert_eq!(mgr.market_queue_depth("m"), 3);
    }

    // ── Independent models ───────────────────────────────────────────

    #[test]
    fn enqueue_market_models_are_independent() {
        // Cap on model A full; model B should still accept market
        // work because they're separate queues.
        let mut mgr = ComputeQueueManager::new();
        mgr.enqueue_market("A", sample_entry("A", "market_received"), 1)
            .unwrap();
        assert!(mgr
            .enqueue_market("A", sample_entry("A", "market_received"), 1)
            .is_err());
        mgr.enqueue_market("B", sample_entry("B", "market_received"), 1)
            .unwrap();
        assert_eq!(mgr.market_queue_depth("A"), 1);
        assert_eq!(mgr.market_queue_depth("B"), 1);
    }

    #[test]
    fn enqueue_market_registers_model_for_round_robin() {
        // A fresh market enqueue to a previously-unseen model must
        // add the model to the round-robin rotation, otherwise
        // dequeue_next would skip it.
        let mut mgr = ComputeQueueManager::new();
        mgr.enqueue_market("new-model", sample_entry("new-model", "market_received"), 1)
            .unwrap();
        // dequeue_next must be able to find the entry.
        let popped = mgr.dequeue_next();
        assert!(popped.is_some());
        assert_eq!(popped.unwrap().model_id, "new-model");
    }

    // ── market_queue_depth accessor ──────────────────────────────────

    #[test]
    fn market_queue_depth_returns_zero_for_unknown_model() {
        let mgr = ComputeQueueManager::new();
        assert_eq!(mgr.market_queue_depth("ghost"), 0);
    }

    #[test]
    fn market_queue_depth_ignores_non_market_entries() {
        let mut mgr = ComputeQueueManager::new();
        mgr.enqueue_local("m", sample_entry("m", "local"));
        mgr.enqueue_local("m", sample_entry("m", "fleet_received"));
        assert_eq!(mgr.queue_depth("m"), 2);
        assert_eq!(mgr.market_queue_depth("m"), 0,
            "only source='market_received' counts");
    }

    // ── QueueError shape ─────────────────────────────────────────────

    #[test]
    fn queue_error_display_covers_all_variants() {
        let e1 = QueueError::DepthExceeded {
            model_id: "gemma3:27b".into(),
            current: 3,
            max: 3,
        };
        let s = format!("{e1}");
        assert!(s.contains("gemma3:27b"));
        assert!(s.contains("3"));
        assert!(s.contains("cap"));

        let e2 = QueueError::ModelNotLoaded {
            model_id: "llama-unloaded".into(),
        };
        let s = format!("{e2}");
        assert!(s.contains("llama-unloaded"));
        assert!(s.contains("not loaded"));
    }

    #[test]
    fn queue_error_is_clone_and_partial_eq() {
        // Important: WS5 handler will want to log the error and then
        // construct a 503 body from a cloned copy. Clone + PartialEq
        // let tests assert specific error shapes.
        let e = QueueError::DepthExceeded {
            model_id: "m".into(),
            current: 1,
            max: 1,
        };
        let e_clone = e.clone();
        assert_eq!(e, e_clone);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn enqueue_market_with_zero_cap_rejects_first_push() {
        // A zero-cap offer is the operator saying "stop accepting market
        // work on this model". The strict >= semantics mean the first
        // push rejects with current=0, max=0.
        let mut mgr = ComputeQueueManager::new();
        let err = mgr
            .enqueue_market("m", sample_entry("m", "market_received"), 0)
            .unwrap_err();
        match err {
            QueueError::DepthExceeded { current, max, .. } => {
                assert_eq!(current, 0);
                assert_eq!(max, 0);
            }
            other => panic!("expected DepthExceeded, got {other:?}"),
        }
        assert_eq!(mgr.queue_depth("m"), 0);
        assert_eq!(mgr.market_queue_depth("m"), 0);
    }

    #[test]
    fn enqueue_market_does_not_double_register_existing_model() {
        // If enqueue_local registered the model first, a subsequent
        // enqueue_market to the same model must NOT push the key to
        // round_robin_keys again. Duplicate keys cause that model to
        // drain twice per round — a silent fairness bug for the other
        // models in the rotation.
        let mut mgr = ComputeQueueManager::new();
        mgr.enqueue_local("m", sample_entry("m", "local"));
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 5)
            .unwrap();
        mgr.enqueue_market("m", sample_entry("m", "market_received"), 5)
            .unwrap();
        // Round-robin should cycle through exactly one model. After
        // draining all entries, the next dequeue_next must return None.
        let a = mgr.dequeue_next();
        let b = mgr.dequeue_next();
        let c = mgr.dequeue_next();
        assert!(a.is_some());
        assert!(b.is_some());
        assert!(c.is_some());
        assert!(mgr.dequeue_next().is_none(), "queue must be fully drained");

        // Sibling scenario: enqueue_market before enqueue_local must
        // also not double-register.
        let mut mgr2 = ComputeQueueManager::new();
        mgr2.enqueue_market("n", sample_entry("n", "market_received"), 5)
            .unwrap();
        mgr2.enqueue_local("n", sample_entry("n", "local"));
        mgr2.enqueue_market("n", sample_entry("n", "market_received"), 5)
            .unwrap();
        let a = mgr2.dequeue_next();
        let b = mgr2.dequeue_next();
        let c = mgr2.dequeue_next();
        assert!(a.is_some());
        assert!(b.is_some());
        assert!(c.is_some());
        assert!(
            mgr2.dequeue_next().is_none(),
            "queue must be fully drained after 3 pushes + 3 pops"
        );
    }
}
