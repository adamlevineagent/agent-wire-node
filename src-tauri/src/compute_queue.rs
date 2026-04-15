// compute_queue.rs — Per-model FIFO compute queue (Phase 1).
//
// Replaces the global LOCAL_PROVIDER_SEMAPHORE as the serializer for
// local LLM calls. Each model_id gets its own FIFO queue. The GPU
// processing loop (spawned in main.rs) drains items round-robin across
// models so no single model starves.
//
// Phase 1: local builds only — no market exchange, no settlement, no
// relay. The queue is transparent: LlmConfig carries an optional
// ComputeQueueHandle, and call_model_unified_with_audit_and_ctx checks
// it. When present, the call enqueues + awaits a oneshot; when absent
// (tests, pre-init), the call goes straight to HTTP.

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
