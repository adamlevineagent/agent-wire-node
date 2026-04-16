//! Phase 2 WS6 — queue mirror push task.
//!
//! Pushes the node's per-model market queue state to the Wire after every
//! state mutation so the matcher always has an accurate view of our
//! availability. The canonical spec section is
//! `docs/plans/compute-market-phase-2-exchange.md` §III "Queue Mirror"
//! (L401-448) and "Chronicle Events" (L627-632).
//!
//! # Architecture
//!
//! A `tokio::sync::mpsc::UnboundedSender<()>` nudge channel lives on
//! `MarketDispatchContext`. Every call site that mutates market queue
//! state sends a `()` token. The mirror task receives these tokens,
//! coalesces bursts via a debounce window
//! (`market_delivery_policy.queue_mirror_debounce_ms`), and on timer
//! expiry POSTs a snapshot to `/api/v1/compute/queue-state`.
//!
//! # Privacy (audit J7)
//!
//! The per-model snapshot MUST NOT include `local_depth` or
//! `executing_source`. The Wire only needs to know our MARKET depth for
//! matching; local and fleet-received depth reveal work patterns and
//! belong to the operator only. The `ModelQueueState` struct below
//! enforces this by omitting those fields — never reintroduce them.
//!
//! # Gating
//!
//! Pushes are gated on `ComputeMarketState.is_serving` AND
//! `compute_participation_policy.effective_booleans().allow_market_visibility`.
//! Both must be true. A policy read happens on each push (not at nudge
//! time) so a supersede-in-flight doesn't get a stale snapshot out.
//!
//! # Failure handling
//!
//! On any push failure (network, non-2xx, JSON parse of error body) we
//! write a `queue_mirror_push_failed` chronicle event with the
//! specific error + current seq + a rolling retry counter in metadata,
//! and continue. Aggressive retry is intentional NOT — the next nudge
//! (state changed or not, any mutation triggers one) re-pushes the
//! current snapshot, so getting stuck at an old seq is self-correcting.
//!
//! # Seq semantics
//!
//! The per-model mirror seq is bumped via
//! `ComputeMarketState::bump_mirror_seq(model_id)` — monotonic, never
//! decrements, `saturating_add` at u64::MAX. The Wire rejects pushes
//! where `seq <= current`. On crash + restart we LOSE the in-memory seq
//! and rebuild from persisted state (which does round-trip the map):
//! if the Wire-side seq drifted ahead, the post returns 409 and we log
//! it as a failure — Phase 3's reconnect path handles seq re-sync.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc, RwLock};

use crate::auth::AuthState;
use crate::compute_market::ComputeMarketState;
use crate::compute_queue::ComputeQueueHandle;
use crate::pyramid::compute_chronicle::{
    record_event, ChronicleEventContext, EVENT_QUEUE_MIRROR_PUSH_FAILED, SOURCE_MARKET,
};
use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;
use crate::pyramid::market_dispatch::MarketDispatchContext;
use crate::tunnel::TunnelState;

/// HTTP path the Wire exposes for queue-state pushes. Spec §III L412.
const QUEUE_STATE_PATH: &str = "/api/v1/compute/queue-state";

/// Per-model slice of the snapshot pushed to the Wire. MUST NOT include
/// `local_depth` or `executing_source` (J7). Field names match the
/// spec's ModelQueueState wire form (L437-444).
#[derive(Debug, Clone, Serialize)]
struct ModelQueueState {
    model_id: String,
    /// Local + fleet + market entries for this model. Hint to the
    /// matcher that the model's GPU is contended even if our own
    /// market depth is low.
    total_depth: usize,
    /// Market-source entries only. Drives admission and pricing.
    market_depth: usize,
    /// Whether a market job for this model is currently executing. The
    /// mirror task tracks this by consulting `ComputeMarketState.active_jobs`
    /// (not the compute_queue, which doesn't distinguish executing vs
    /// queued market source).
    is_executing: bool,
    /// Optional estimate for when the next slot opens. `None` if we
    /// can't predict (no throughput data yet). Phase 2 always emits
    /// `None` — Phase 4+ fills this from observed inference latencies.
    #[serde(skip_serializing_if = "Option::is_none")]
    est_next_available_s: Option<u32>,
    /// The operator's configured per-offer cap. The Wire mirrors this
    /// so its admission-control UI can render the operator's intent.
    max_market_depth: usize,
    /// Node-wide cap across all sources for this model. For Phase 2 the
    /// node doesn't have a separate total-depth knob — we surface the
    /// `max_market_depth` here too so the Wire has a stable field in
    /// the snapshot struct. Phase 4+ may split this.
    max_total_depth: usize,
}

/// Top-level snapshot body sent to the Wire. Field names match the
/// spec's `QueueMirrorSnapshot` wire form (L430-435).
#[derive(Debug, Clone, Serialize)]
struct QueueMirrorSnapshot {
    node_id: String,
    /// Max seq across all per-model seqs in this push — the Wire uses
    /// this as a tiebreaker for rate-limiting but the per-model seqs
    /// are the canonical conflict check.
    seq: u64,
    model_queues: Vec<ModelQueueState>,
    timestamp: String,
}

/// Everything the mirror task's push body needs to reach the Wire.
/// Kept separate from `MarketDispatchContext` so the task can be unit-
/// tested without standing up a full context.
pub struct MirrorTaskContext {
    /// Owned handle to market state — read for snapshot + write to
    /// bump per-model seqs.
    pub market_state: Arc<RwLock<ComputeMarketState>>,
    /// Owned handle to the market dispatch context — read `policy`
    /// for debounce tuning on each tick.
    pub dispatch: Arc<MarketDispatchContext>,
    /// Borrowed from `AppState`: the compute queue's depth counts feed
    /// the per-model `total_depth` field. Market depth comes from
    /// `market_queue_depth` specifically (not `queue_depth`) to keep
    /// the privacy boundary honest.
    pub compute_queue: ComputeQueueHandle,
    /// Borrowed from `AppState`: Wire API URL + bearer token source.
    /// The reader lock is held only briefly per push; never across
    /// the HTTP call itself.
    pub auth: Arc<RwLock<AuthState>>,
    /// Borrowed from `AppState`: tunnel state — currently only used to
    /// gate pushes on connection status (don't send when we know we
    /// can't reach the Wire). Phase 3 reconnect-trigger will read this
    /// too.
    pub tunnel: Arc<RwLock<TunnelState>>,
    /// Borrowed from config: Wire API base URL (e.g. `https://newsbleach.com`).
    pub api_url: String,
    /// SQLite path used for chronicle writes on push failure.
    pub db_path: std::path::PathBuf,
    /// Opaque pyramid slug used for chronicle `slug` attribution. The
    /// mirror task itself isn't tied to a slug, but chronicle queries
    /// key off it; passing `None` is fine (omitted in the event row).
    pub node_id_override: Option<String>,
}

/// Spawn the queue mirror push task.
///
/// The caller supplies BOTH the context and the receiver half of the
/// nudge channel. The sender half is constructed by the caller FIRST
/// (via `tokio::sync::mpsc::unbounded_channel()`) and placed on
/// `MarketDispatchContext.mirror_nudge` so mutation sites can signal
/// via `ctx.mirror_nudge.send(()).ok()` — then the receiver is handed
/// here to drive the loop.
///
/// This split exists because the `MirrorTaskContext` holds an
/// `Arc<MarketDispatchContext>` (to read `policy` each tick); a
/// self-creating-channel API would recurse through the Arc and
/// deadlock initialization.
///
/// The task body never exits on its own (only on app shutdown or
/// channel close). It's infallible to spawn — a
/// `tauri::async_runtime::spawn` at startup is fire-and-forget.
///
/// WHY this shape (nudge channel + task-owned context) rather than
/// triggering the push inline on each mutation:
///   - Debouncing bursts — a single HTTP dispatch touches
///     `upsert_active_job` + `transition_job_status` + worker
///     heartbeat in quick succession; coalescing these into one
///     push avoids paying the wire cost per mutation and keeps the
///     Wire's queue view stable.
///   - Unblocking call sites — a mutation site never awaits on the
///     HTTP push, it just drops a nudge token and continues. HTTP
///     slowness can't back-pressure the dispatch handler.
pub fn spawn_market_mirror_task(
    ctx: MirrorTaskContext,
    rx: mpsc::UnboundedReceiver<()>,
) {
    tauri::async_runtime::spawn(async move {
        mirror_loop(ctx, rx).await;
    });
}

/// Main loop — drain nudges, debounce, push. The split exists so the
/// task body can be exercised by a test harness that doesn't want to
/// invoke `tauri::async_runtime::spawn`.
async fn mirror_loop(ctx: MirrorTaskContext, mut rx: mpsc::UnboundedReceiver<()>) {
    tracing::info!("market queue mirror task started");
    // Rolling retry counter for the `retry_count` field on chronicle
    // failure events. Reset on success. Observability aid only — does
    // NOT gate push attempts (every nudge re-pushes).
    let mut retry_count: u32 = 0;

    while let Some(()) = rx.recv().await {
        // Drain any further nudges queued up — they all represent the
        // same eventual "push current state" action.
        while rx.try_recv().is_ok() {}

        // Read the debounce window fresh so operator supersession of
        // the policy applies on the very next tick.
        let debounce_ms = ctx
            .dispatch
            .policy
            .read()
            .await
            .queue_mirror_debounce_ms
            .max(1);
        tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;

        // Drain any nudges that arrived during the debounce window —
        // same coalescing. The sleep already caught the burst.
        while rx.try_recv().is_ok() {}

        // Gate the push. Both gates need to be true; either false is
        // silent (no push, no chronicle).
        if !should_push(&ctx).await {
            // Reset the retry counter — our decision not to push is
            // a policy gate, not a failure.
            retry_count = 0;
            continue;
        }

        match push_snapshot(&ctx).await {
            Ok(()) => {
                retry_count = 0;
            }
            Err(e) => {
                retry_count = retry_count.saturating_add(1);
                record_push_failure(&ctx, &e, retry_count).await;
                tracing::warn!(
                    err = %e,
                    retry = retry_count,
                    "queue mirror push failed"
                );
            }
        }
    }

    tracing::info!("market queue mirror task exited (channel closed)");
}

/// Gate check: `is_serving` (runtime toggle) AND `allow_market_visibility`
/// (durable operator intent). Either false skips the push silently.
async fn should_push(ctx: &MirrorTaskContext) -> bool {
    let is_serving = ctx.market_state.read().await.is_serving;
    if !is_serving {
        return false;
    }
    // Participation-policy check requires a blocking sqlite read; run
    // off the reactor thread.
    let db_path = ctx.db_path.clone();
    let policy_allowed: bool = match tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let p = crate::pyramid::local_mode::get_compute_participation_policy(&conn)?;
        Ok(p.effective_booleans().allow_market_visibility)
    })
    .await
    {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            tracing::warn!(
                err = %e,
                "market mirror participation-policy read failed; skipping push"
            );
            false
        }
        Err(je) => {
            tracing::warn!(
                err = %je,
                "market mirror participation-policy join error; skipping push"
            );
            false
        }
    };
    if !policy_allowed {
        return false;
    }
    // Don't push if the tunnel isn't up — the Wire would never see it.
    let tunnel_connected = matches!(
        ctx.tunnel.read().await.status,
        crate::tunnel::TunnelConnectionStatus::Connected
    );
    tunnel_connected
}

/// Build the snapshot, bump seqs, POST to the Wire. Returns the full
/// error message on any step that fails, for the caller's chronicle
/// write + log.
async fn push_snapshot(ctx: &MirrorTaskContext) -> Result<(), String> {
    // Step 1: resolve node_id. The explicit override (if provided) wins;
    // otherwise read from AuthState.
    let node_id = match ctx.node_id_override.clone() {
        Some(s) if !s.is_empty() => s,
        _ => ctx
            .auth
            .read()
            .await
            .node_id
            .clone()
            .ok_or_else(|| "no node_id on AuthState".to_string())?,
    };

    // Step 2: resolve bearer token. Mirror of get_api_token helper in
    // main.rs — we don't have access to it here, so inline.
    let bearer = ctx
        .auth
        .read()
        .await
        .api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "no api_token on AuthState".to_string())?;

    // Step 3: snapshot build + seq bump. Hold the write lock briefly
    // so both halves atomic vs another pusher. (There's only one
    // mirror task; this is future-proofing against a second instance
    // landing in a later phase.)
    let snapshot = {
        let mut state = ctx.market_state.write().await;
        let queue_snapshot = {
            let q = ctx.compute_queue.queue.lock().await;
            // Collect (model_id, total_depth, market_depth) upfront so
            // we drop the queue lock before reading offers from
            // market_state. Intentionally does NOT expose local_depth
            // or executing_source (J7).
            state
                .offers
                .keys()
                .map(|model_id| {
                    let total = q.queue_depth(model_id);
                    let market = q.market_queue_depth(model_id);
                    (model_id.clone(), total, market)
                })
                .collect::<Vec<_>>()
        };

        let mut max_seq: u64 = 0;
        let mut model_queues = Vec::with_capacity(queue_snapshot.len());
        for (model_id, total_depth, market_depth) in queue_snapshot {
            let is_executing = state.active_jobs.values().any(|j| {
                j.model_id == model_id
                    && j.status == crate::compute_market::ComputeJobStatus::Executing
            });
            let (max_market_depth, max_total_depth) = match state.offers.get(&model_id) {
                Some(offer) => (offer.max_queue_depth, offer.max_queue_depth),
                None => (0, 0),
            };
            let seq = state.bump_mirror_seq(&model_id);
            max_seq = max_seq.max(seq);
            model_queues.push(ModelQueueState {
                model_id,
                total_depth,
                market_depth,
                is_executing,
                est_next_available_s: None,
                max_market_depth,
                max_total_depth,
            });
        }

        QueueMirrorSnapshot {
            node_id: node_id.clone(),
            seq: max_seq,
            model_queues,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    };

    // Step 4: POST. Reuse the reqwest::Client pattern from main.rs's
    // `send_api_request` — a fresh client per push is fine at the
    // debounce cadence (default 500ms max + whatever work precedes it).
    let url = format!("{}{}", ctx.api_url, QUEUE_STATE_PATH);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", bearer))
        .json(&snapshot)
        .send()
        .await
        .map_err(|e| format!("transport: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("http {}: {}", status.as_u16(), body));
    }
    tracing::debug!(
        seq = snapshot.seq,
        models = snapshot.model_queues.len(),
        "queue mirror pushed"
    );
    Ok(())
}

/// Write the `queue_mirror_push_failed` chronicle event. The job_path
/// per spec uses the push timestamp so repeated failures form a
/// traceable timeline rather than collapsing into one row.
async fn record_push_failure(ctx: &MirrorTaskContext, err: &str, retry_count: u32) {
    let db_path = ctx.db_path.clone();
    // Read the current max seq for metadata. Non-critical — on lock
    // contention we fall back to 0.
    let current_seq = {
        let state = ctx.market_state.read().await;
        state.queue_mirror_seq.values().copied().max().unwrap_or(0)
    };
    let err = err.to_string();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let job_path = format!("market/mirror/{}", chrono::Utc::now().timestamp());
        let ctx_ev = ChronicleEventContext::minimal(
            &job_path,
            EVENT_QUEUE_MIRROR_PUSH_FAILED,
            SOURCE_MARKET,
        )
        .with_metadata(serde_json::json!({
            "error": err,
            "seq": current_seq,
            "retry_count": retry_count,
        }));
        let _ = record_event(&conn, &ctx_ev);
        Ok(())
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tests in this module cover the *pure* pieces of the mirror
    // task — snapshot shape + privacy invariants + serde contract.
    // Orchestration (nudge → debounce → push) needs a tokio-test
    // harness with fake HTTP; we skip that for Phase 2 WS6 per the
    // scope note in the builder prompt. Correctness concerns that
    // would push us past "light-to-medium" testing are:
    //   - rely on Wire-side rejection of out-of-order seqs (server
    //     already enforces);
    //   - rely on `saturating_add` in `bump_mirror_seq` (tested in
    //     compute_market.rs).

    #[test]
    fn snapshot_omits_local_depth_and_executing_source() {
        // Privacy invariant J7: the JSON body POSTed to the Wire MUST
        // NOT contain either field. Enforced structurally (the struct
        // doesn't carry them) and verified here against the serialized
        // form so a future sibling struct edit can't silently
        // reintroduce them.
        let snap = QueueMirrorSnapshot {
            node_id: "node-abc".into(),
            seq: 7,
            model_queues: vec![ModelQueueState {
                model_id: "llama3.2:70b".into(),
                total_depth: 5,
                market_depth: 2,
                is_executing: true,
                est_next_available_s: None,
                max_market_depth: 8,
                max_total_depth: 8,
            }],
            timestamp: "2026-04-17T12:00:00Z".into(),
        };
        let body = serde_json::to_string(&snap).expect("serialize must succeed");
        assert!(
            !body.contains("local_depth"),
            "snapshot must not expose local_depth (J7); got: {body}"
        );
        assert!(
            !body.contains("executing_source"),
            "snapshot must not expose executing_source (J7); got: {body}"
        );
    }

    #[test]
    fn model_queue_state_field_names_match_spec() {
        // Pinned to the wire-form shape in
        // docs/plans/compute-market-phase-2-exchange.md §III L437-444.
        // A field rename here needs a concurrent Wire-side rename.
        let mqs = ModelQueueState {
            model_id: "m".into(),
            total_depth: 1,
            market_depth: 1,
            is_executing: false,
            est_next_available_s: None,
            max_market_depth: 1,
            max_total_depth: 1,
        };
        let j = serde_json::to_value(&mqs).unwrap();
        for k in &[
            "model_id",
            "total_depth",
            "market_depth",
            "is_executing",
            "max_market_depth",
            "max_total_depth",
        ] {
            assert!(
                j.get(*k).is_some(),
                "missing field {k} in serialized ModelQueueState"
            );
        }
    }

    #[test]
    fn est_next_available_omitted_when_none() {
        // Phase 2 always emits None here. The Wire tolerates omission
        // of optional fields; we save bytes on every push.
        let mqs = ModelQueueState {
            model_id: "m".into(),
            total_depth: 0,
            market_depth: 0,
            is_executing: false,
            est_next_available_s: None,
            max_market_depth: 0,
            max_total_depth: 0,
        };
        let s = serde_json::to_string(&mqs).unwrap();
        assert!(
            !s.contains("est_next_available_s"),
            "optional None must be skipped; got: {s}"
        );
    }

    #[test]
    fn est_next_available_emitted_when_some() {
        // Phase 4+ populates this. Pin the serialization contract so
        // future emission sites get the exact wire name.
        let mqs = ModelQueueState {
            model_id: "m".into(),
            total_depth: 0,
            market_depth: 0,
            is_executing: false,
            est_next_available_s: Some(42),
            max_market_depth: 0,
            max_total_depth: 0,
        };
        let s = serde_json::to_string(&mqs).unwrap();
        assert!(s.contains("\"est_next_available_s\":42"));
    }

    #[test]
    fn snapshot_field_order_includes_required_top_level_fields() {
        // Wire expects node_id, seq, model_queues, timestamp.
        let snap = QueueMirrorSnapshot {
            node_id: "n".into(),
            seq: 1,
            model_queues: vec![],
            timestamp: "t".into(),
        };
        let j = serde_json::to_value(&snap).unwrap();
        for k in &["node_id", "seq", "model_queues", "timestamp"] {
            assert!(j.get(*k).is_some(), "missing top-level field {k}");
        }
    }
}
