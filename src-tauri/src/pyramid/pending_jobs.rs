//! Pending-jobs map — the shared rendezvous between the requester-side
//! dispatch path and the Wire → Requester push inbound route.
//!
//! When the node calls `POST /api/v1/compute/fill`, it expects the
//! result to arrive via push to `/v1/compute/job-result` (contract §2.5,
//! rev 1.4). The dispatch path needs to *await* that push: it inserts
//! a oneshot channel here keyed by UUID job_id, then `await`s the
//! receiver. The inbound handler looks up the entry, fires the sender,
//! and removes the key.
//!
//! Timeout semantics: `await_result` in `compute_quote_flow` owns the
//! timeout; on expiry it `remove`s its own entry to prevent a late
//! push from firing a channel whose receiver has been dropped. A
//! late push that finds no entry returns a 2xx `already_settled`
//! response so Wire marks delivery done (contract §2.5).
//!
//! Phase 3 scope: in-memory only. Node restart loses pending jobs;
//! the affected `pyramid_build` step retries at the next build pass.
//! Persistence is a later phase if restart-loss becomes a real
//! tester pain point.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

/// The payload delivered to the awaiting dispatcher. Carries either a
/// success envelope's inference result or a failure envelope's error
/// code + message. Matches the §2.3 envelope `type` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeliveryPayload {
    Success {
        content: String,
        input_tokens: i64,
        output_tokens: i64,
        model_used: String,
        latency_ms: i64,
        finish_reason: Option<String>,
    },
    Failure {
        code: String,
        message: String,
    },
}

/// Shared pending-jobs registry. Cloning the outer `Arc` is cheap;
/// internally a tokio `Mutex` guards the map so `insert` from the
/// dispatcher thread and `take` from the inbound handler are serialized.
///
/// `tokio::Mutex` (not `std::Mutex`) because `insert`/`take` may be
/// called from `await` points; we never hold the guard across an
/// `await` ourselves, but the type choice keeps us honest if that
/// changes.
#[derive(Clone, Default)]
pub struct PendingJobs {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<DeliveryPayload>>>>,
}

impl PendingJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a oneshot sender for `uuid_job_id`. Returns the
    /// receiver the caller will `await`. If an entry for this job_id
    /// already exists, the prior sender is dropped (its receiver gets
    /// `RecvError` — caller should treat as cancellation). In
    /// practice this shouldn't happen: `/match` mints unique UUIDs.
    pub async fn register(&self, uuid_job_id: String) -> oneshot::Receiver<DeliveryPayload> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.inner.lock().await;
        // Overwrite semantics: the prior sender's receiver gets
        // `RecvError::Closed` when the old sender drops, which is
        // what we want — a stale pending job for the same id gets
        // implicitly cancelled.
        map.insert(uuid_job_id, tx);
        rx
    }

    /// Remove the entry for `uuid_job_id` and return the sender, if any.
    ///
    /// Used by:
    ///   - inbound handler on push arrival: `take` then `send` the payload.
    ///   - `await_result` on timeout: `take` to drop the sender so any
    ///     racing push returns `already_settled` without firing a
    ///     dropped channel.
    pub async fn take(&self, uuid_job_id: &str) -> Option<oneshot::Sender<DeliveryPayload>> {
        let mut map = self.inner.lock().await;
        map.remove(uuid_job_id)
    }

    /// Observability accessor — current number of pending entries.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_deliver_roundtrip() {
        let pending = PendingJobs::new();
        let job_id = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let rx = pending.register(job_id.clone()).await;
        assert_eq!(pending.len().await, 1);

        let sender = pending.take(&job_id).await.expect("sender present");
        assert_eq!(pending.len().await, 0);
        sender
            .send(DeliveryPayload::Success {
                content: "hello".into(),
                input_tokens: 5,
                output_tokens: 1,
                model_used: "test".into(),
                latency_ms: 10,
                finish_reason: Some("stop".into()),
            })
            .expect("send");

        let payload = rx.await.expect("recv");
        match payload {
            DeliveryPayload::Success { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn take_on_unknown_job_id_returns_none() {
        let pending = PendingJobs::new();
        assert!(pending.take("no-such-id").await.is_none());
    }

    #[tokio::test]
    async fn double_register_drops_old_receiver() {
        let pending = PendingJobs::new();
        let job_id = "dup".to_string();
        let rx_old = pending.register(job_id.clone()).await;
        let _rx_new = pending.register(job_id.clone()).await;
        // Old receiver's sender was dropped — recv returns Err.
        assert!(rx_old.await.is_err());
        assert_eq!(pending.len().await, 1); // only new entry remains
    }

    #[tokio::test]
    async fn timeout_remove_then_late_push_returns_none() {
        // Simulates: await_result times out, removes its own entry;
        // a late push arrives and finds nothing — handler should
        // respond `already_settled`.
        let pending = PendingJobs::new();
        let job_id = "late".to_string();
        let _rx = pending.register(job_id.clone()).await;
        assert!(pending.take(&job_id).await.is_some()); // timeout path removes
        assert!(pending.take(&job_id).await.is_none()); // late-push path finds nothing
    }
}
