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
//! and continue. The next mutation-driven nudge re-pushes the current
//! snapshot, so getting stuck at an old seq is self-correcting.
//!
//! # Idle liveness
//!
//! This task does NOT run a periodic heartbeat. Wire's staleness
//! filter accepts either a fresh queue push OR a fresh
//! `wire_nodes.last_heartbeat` (the node heartbeat loop in `main.rs`
//! already pings every 60s), so idle providers remain matchable via
//! the node heartbeat without this task paying `queue_push_fee` on
//! every tick. Adding a periodic mirror push would duplicate the
//! existing node-heartbeat signal and charge the operator credits to
//! do so.
//!
//! # Supervision
//!
//! `spawn_market_mirror_task` wraps the loop in a supervisor that
//! catches panics via `AssertUnwindSafe::catch_unwind`, emits a
//! `market_mirror_task_panicked` chronicle event with the payload,
//! sleeps briefly, and respawns. A clean exit (channel sender dropped)
//! emits `market_mirror_task_exited` before returning — so an operator
//! querying their chronicle can tell the difference between "idle" and
//! "task is dead." Pre-supervisor, a silent panic or channel close
//! looked identical to a healthy idle node.
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

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use serde::Serialize;
use tokio::sync::{mpsc, RwLock};

use crate::auth::AuthState;
use crate::compute_market::ComputeMarketState;
use crate::compute_queue::ComputeQueueHandle;
use crate::pyramid::compute_chronicle::{
    record_event, ChronicleEventContext, EVENT_MIRROR_TASK_EXITED, EVENT_MIRROR_TASK_PANICKED,
    EVENT_QUEUE_MIRROR_PUSH_FAILED, SOURCE_MARKET,
};
use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;
use crate::pyramid::market_dispatch::MarketDispatchContext;
use crate::tunnel::TunnelState;

/// HTTP path Wire exposes for queue-mirror pushes. Corrected from the
/// never-registered `/api/v1/compute/queue-state` in the compute-market
/// structural-fix plan §5.6 — every mirror push was 404'ing silently
/// before this change, which explains why Wire never saw a node as
/// matchable for match_compute_job's staleness filter.
const QUEUE_MIRROR_PATH: &str = "/api/v1/compute/queue-mirror";

/// Per-offer slice of the snapshot pushed to Wire. Body shape matches
/// `src/app/api/v1/compute/queue-mirror/route.ts` validator exactly —
/// any unknown field returns 400 `privacy_field_rejected`. MUST NOT
/// include `local_depth`, `executing_source`, `is_executing`, or
/// `total_depth` (all Privacy J7 tripwires).
///
/// Mapping from old node shape (pre-structural-fix):
///   `market_depth` → `current_queue_depth`  (Phase 2.9)
///   `max_market_depth` → `max_queue_depth`
///   DROP `total_depth`, `is_executing`, `max_total_depth`
///   DROP `est_next_available_s` — moved to offer contribution body
///   ADD `wire_offer_id` — Wire's identifier for the offer row
///   ADD `allow_market_visibility` — per-offer, drives Wire's
///        status transition (Phase 2.11)
#[derive(Debug, Clone, Serialize)]
struct QueueMirrorOffer {
    model_id: String,
    /// Wire-side identifier for this offer (handle-path post-migration,
    /// UUID during transition). Emitted so Wire can match the push to
    /// a specific `wire_compute_offers` row. Per structural-fix plan
    /// §2.10: if None, the offer is SKIPPED entirely — don't push
    /// `null` (validator rejects).
    wire_offer_id: String,
    /// Admission-relevant depth. Renamed from `market_depth` per
    /// Phase 2.9 (single depth dimension; `total_depth` declared dead).
    current_queue_depth: usize,
    /// Per-offer cap. Renamed from `max_market_depth`.
    max_queue_depth: usize,
    /// Per-offer visibility driven by the operator's current
    /// effective-policy visibility. Wire uses this to toggle the
    /// offer's `inactive_reason=visibility_off` without requiring the
    /// operator to re-publish. Phase 2.11.
    allow_market_visibility: bool,
}

/// Top-level snapshot body. Field names must match Wire's validator
/// exactly — unknown fields are rejected 400.
///
/// Mapping from old shape:
///   `seq` → `snapshot_seq`
///   `model_queues` → `offers`
///   DROP `timestamp`  (Wire rejects)
///   ADD `is_serving` — from `ComputeMarketState.is_serving`, drives
///        Wire's "whole-node offline" state transition without
///        requiring individual offer deletes.
#[derive(Debug, Clone, Serialize)]
struct QueueMirrorSnapshot {
    node_id: String,
    snapshot_seq: u64,
    is_serving: bool,
    offers: Vec<QueueMirrorOffer>,
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

/// Spawn the queue mirror push task under a supervisor.
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
/// The supervisor catches panics in `mirror_loop` and respawns with a
/// short backoff so a single bad push can't take the task down for
/// the lifetime of the node. Clean exit (channel closed because every
/// sender was dropped) emits `market_mirror_task_exited` before
/// returning — operators can differentiate "idle" from "dead" in the
/// chronicle.
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
pub fn spawn_market_mirror_task(ctx: MirrorTaskContext, rx: mpsc::UnboundedReceiver<()>) {
    tauri::async_runtime::spawn(async move {
        supervise_mirror_loop(ctx, rx).await;
    });
}

/// Panic-catching supervisor around `mirror_loop`. On panic, emit a
/// loud chronicle event, back off briefly, and respawn (the same rx
/// is passed through — if it panicked mid-drain we keep the remaining
/// items). On clean exit (all senders dropped), emit a final
/// chronicle event and return.
///
/// Backoff is a fixed 5s rather than exponential because a panicking
/// mirror task under retry pressure is an operator problem that needs
/// visibility (loud chronicle), not self-smoothing. The bounded
/// backoff keeps the chronicle from filling up if something is truly
/// wedged.
async fn supervise_mirror_loop(ctx: MirrorTaskContext, mut rx: mpsc::UnboundedReceiver<()>) {
    const PANIC_BACKOFF_SECS: u64 = 5;

    loop {
        // AssertUnwindSafe is required because MirrorTaskContext
        // holds Arcs / RwLocks that aren't statically UnwindSafe.
        // A panic mid-await leaves those in whatever state they were
        // in; we respawn with the same handles and continue — same
        // pattern as fleet_outbox_sweep's sweep loop.
        let result = AssertUnwindSafe(mirror_loop(&ctx, &mut rx))
            .catch_unwind()
            .await;

        match result {
            // Clean exit: channel sender dropped. Emit a loud
            // chronicle event so operators can see the task died
            // rather than assuming it's healthily idle.
            Ok(()) => {
                record_lifecycle_event(
                    &ctx,
                    EVENT_MIRROR_TASK_EXITED,
                    serde_json::json!({
                        "reason": "channel_closed",
                    }),
                )
                .await;
                tracing::info!(
                    "market queue mirror task exited cleanly (channel closed); supervisor stopping"
                );
                return;
            }
            // Panic: extract a best-effort message and emit a loud
            // chronicle event before respawning.
            Err(panic_payload) => {
                let message = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "panic in mirror_loop (payload not string)".to_string()
                };
                record_lifecycle_event(
                    &ctx,
                    EVENT_MIRROR_TASK_PANICKED,
                    serde_json::json!({
                        "message": message,
                        "backoff_secs": PANIC_BACKOFF_SECS,
                    }),
                )
                .await;
                tracing::error!(
                    panic = %message,
                    backoff_secs = PANIC_BACKOFF_SECS,
                    "market queue mirror task panicked; respawning"
                );
                tokio::time::sleep(std::time::Duration::from_secs(PANIC_BACKOFF_SECS)).await;
                // Fall through to loop — respawn mirror_loop.
            }
        }
    }
}

/// Write a lifecycle chronicle event (panic / exit). Fire-and-forget
/// via `spawn_blocking` matching the existing failure-path pattern.
async fn record_lifecycle_event(
    ctx: &MirrorTaskContext,
    event_type: &'static str,
    metadata: serde_json::Value,
) {
    let db_path = ctx.db_path.clone();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let job_path = format!("market/mirror/{}", chrono::Utc::now().timestamp());
        let ctx_ev = ChronicleEventContext::minimal(&job_path, event_type, SOURCE_MARKET)
            .with_metadata(metadata);
        let _ = record_event(&conn, &ctx_ev);
        Ok(())
    })
    .await;
}

/// Main loop — drain nudges, debounce, push. The split exists so the
/// task body can be exercised by a test harness that doesn't want to
/// invoke `tauri::async_runtime::spawn`. Takes references so the
/// supervisor can re-enter after a panic without losing state.
///
/// Emits a synthetic boot nudge on entry: a fresh process with a
/// saved-state offer (e.g. post-restart with `is_serving=true` and an
/// already-published offer row) has nothing that would naturally fire
/// the channel, so without this the offer would sit uninformed on
/// Wire until the next mutation. Matters for the GPU-less-tester
/// story: a provider boots and should be matchable within one
/// debounce window, not only after the next dispatch arrives.
async fn mirror_loop(ctx: &MirrorTaskContext, rx: &mut mpsc::UnboundedReceiver<()>) {
    tracing::info!("market queue mirror task started");

    // Synthetic boot tick — run one push attempt before waiting on
    // the channel so a post-restart serving-true node publishes its
    // current snapshot immediately (subject to should_push gates).
    // Gate failure here is silent (same as any other gate failure);
    // push failure emits the usual chronicle.
    boot_push(ctx).await;

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
        if !should_push(ctx).await {
            // Reset the retry counter — our decision not to push is
            // a policy gate, not a failure.
            retry_count = 0;
            continue;
        }

        match push_snapshot(ctx).await {
            Ok(()) => {
                retry_count = 0;
            }
            Err(e) => {
                retry_count = retry_count.saturating_add(1);
                record_push_failure(ctx, &e, retry_count).await;
                tracing::warn!(
                    err = %e,
                    retry = retry_count,
                    "queue mirror push failed"
                );
            }
        }
    }

    // Fall-through: channel closed. Supervisor emits the loud
    // chronicle event — we just return.
}

/// One-shot push on task entry. Runs the same gates as a normal push
/// (no debounce — we're not coalescing anything). Silent on gate
/// failure; emits the usual failure chronicle on HTTP error.
async fn boot_push(ctx: &MirrorTaskContext) {
    if !should_push(ctx).await {
        return;
    }
    if let Err(e) = push_snapshot(ctx).await {
        record_push_failure(ctx, &e, 1).await;
        tracing::warn!(err = %e, "queue mirror boot push failed");
    }
}

/// Gate check: `is_serving` (runtime toggle) AND tunnel connected.
///
/// Pre-structural-fix this ALSO gated on `allow_market_visibility` —
/// when the operator flipped visibility off we skipped the mirror push
/// entirely. That's wrong: Wire needs the push to know about state
/// transitions (is_serving: true→false, per-offer visibility flips).
/// Skipping the push prevents Wire from learning the operator went
/// offline.
///
/// New model: always push (subject to serving + tunnel gates); emit
/// `is_serving` at the top and `allow_market_visibility` per offer.
/// Wire drives state transitions on its side from those fields.
async fn should_push(ctx: &MirrorTaskContext) -> bool {
    let is_serving = ctx.market_state.read().await.is_serving;
    if !is_serving {
        return false;
    }
    // Don't push if the tunnel isn't up — Wire would never see it,
    // and we don't want to burn request attempts during reconnect.
    let tunnel_connected = matches!(
        ctx.tunnel.read().await.status,
        crate::tunnel::TunnelConnectionStatus::Connected
    );
    tunnel_connected
}

/// Read the operator's current effective `allow_market_visibility` for
/// the push body. Non-fatal on read failure — default to `false` so
/// Wire doesn't publish offers the operator may have intended to hide.
/// Runs in a blocking thread (SQLite open).
async fn read_allow_market_visibility(db_path: &std::path::Path) -> bool {
    let db_path = db_path.to_path_buf();
    match tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
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
                "market mirror allow_market_visibility read failed; defaulting to false"
            );
            false
        }
        Err(je) => {
            tracing::warn!(
                err = %je,
                "market mirror allow_market_visibility join error; defaulting to false"
            );
            false
        }
    }
}

/// Build the snapshot, bump seqs, POST to Wire. Returns the full error
/// message on any failure, for the caller's chronicle write + log.
///
/// Body shape matches Wire's `/queue-mirror` validator exactly per
/// structural-fix plan §2.10. Offers whose `wire_offer_id` is None
/// are SKIPPED (not emitted with null) — per the mapping table,
/// null `wire_offer_id` is validator-rejected.
async fn push_snapshot(ctx: &MirrorTaskContext) -> Result<(), String> {
    // Step 1: resolve node_id.
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

    // Step 2: resolve bearer token.
    let bearer = ctx
        .auth
        .read()
        .await
        .api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "no api_token on AuthState".to_string())?;

    // Step 3: read effective `allow_market_visibility` once per push;
    // emit on every offer entry (Phase 2.11). BEFORE snapshot build
    // so we don't hold the market_state write lock across the
    // DB read.
    let allow_visibility = read_allow_market_visibility(&ctx.db_path).await;

    // Step 4: read is_serving once at snapshot time (may differ from
    // the gate in should_push if toggled between calls — that's fine,
    // should_push is a push-rate gate, this is the truth).
    let is_serving = ctx.market_state.read().await.is_serving;

    // Step 5: snapshot build + seq bump. Hold the write lock briefly
    // so both halves atomic vs another pusher.
    let snapshot = {
        let mut state = ctx.market_state.write().await;

        // Collect per-model admission-relevant depth up front so we can
        // drop the queue lock before iterating offers. Only
        // `market_queue_depth` crosses the wire (Privacy J7 — local
        // depth stays local).
        let market_depths: std::collections::HashMap<String, usize> = {
            let q = ctx.compute_queue.queue.lock().await;
            state
                .offers
                .keys()
                .map(|model_id| (model_id.clone(), q.market_queue_depth(model_id)))
                .collect()
        };

        // Collect the offer snapshot fields BEFORE calling
        // `bump_mirror_seq` — iterating `state.offers` (&) while also
        // mutating `state.queue_mirror_seq` (&mut) would fail the
        // borrow checker. Two-pass: pass 1 collects what we want to
        // emit; pass 2 bumps the seq counter for each emitted model.
        let mut skipped_no_wire_id: Vec<String> = Vec::new();
        let to_emit: Vec<(String, String, usize, usize)> = state
            .offers
            .iter()
            .filter_map(|(model_id, offer)| {
                let wire_offer_id = match offer.wire_offer_id.as_ref() {
                    Some(id) if !id.is_empty() => id.clone(),
                    _ => {
                        skipped_no_wire_id.push(model_id.clone());
                        return None;
                    }
                };
                let current_queue_depth = market_depths.get(model_id).copied().unwrap_or(0);
                Some((
                    model_id.clone(),
                    wire_offer_id,
                    current_queue_depth,
                    offer.max_queue_depth,
                ))
            })
            .collect();
        if !skipped_no_wire_id.is_empty() {
            tracing::debug!(
                skipped = ?skipped_no_wire_id,
                "queue mirror push: offers with no wire_offer_id skipped"
            );
        }

        let mut max_seq: u64 = 0;
        let mut offers: Vec<QueueMirrorOffer> = Vec::with_capacity(to_emit.len());
        for (model_id, wire_offer_id, current_queue_depth, max_queue_depth) in to_emit {
            let seq = state.bump_mirror_seq(&model_id);
            max_seq = max_seq.max(seq);
            offers.push(QueueMirrorOffer {
                model_id,
                wire_offer_id,
                current_queue_depth,
                max_queue_depth,
                allow_market_visibility: allow_visibility,
            });
        }

        QueueMirrorSnapshot {
            node_id: node_id.clone(),
            snapshot_seq: max_seq,
            is_serving,
            offers,
        }
    };

    // Step 6: POST.
    let url = format!("{}{}", ctx.api_url, QUEUE_MIRROR_PATH);
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
    // Bumped from DEBUG to INFO so the happy path is visible in default
    // log output — critical for diagnosing the "is the mirror pushing?"
    // question without needing RUST_LOG=debug on operator boxes.
    tracing::info!(
        snapshot_seq = snapshot.snapshot_seq,
        offers = snapshot.offers.len(),
        is_serving = snapshot.is_serving,
        "queue mirror pushed"
    );

    // Emit a node-local chronicle event so operators can see push
    // activity in their own chronicle (the Wire-side
    // `compute_queue_mirror_pushed` only lands in wire_chronicle,
    // not here). Fire-and-forget via spawn_blocking matching the
    // existing failure-path pattern.
    let db_path = ctx.db_path.clone();
    let snap_seq = snapshot.snapshot_seq;
    let offers_count = snapshot.offers.len();
    let push_is_serving = snapshot.is_serving;
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let job_path = format!("market/mirror/{}", chrono::Utc::now().timestamp());
        let ctx_ev =
            ChronicleEventContext::minimal(&job_path, "queue_mirror_pushed", SOURCE_MARKET)
                .with_metadata(serde_json::json!({
                    "snapshot_seq": snap_seq,
                    "offers": offers_count,
                    "is_serving": push_is_serving,
                }));
        let _ = record_event(&conn, &ctx_ev);
        Ok(())
    })
    .await;

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

    fn sample_offer() -> QueueMirrorOffer {
        QueueMirrorOffer {
            model_id: "llama3.2:70b".into(),
            wire_offer_id: "playful/106/3".into(),
            current_queue_depth: 2,
            max_queue_depth: 8,
            allow_market_visibility: true,
        }
    }

    #[test]
    fn snapshot_omits_j7_fields() {
        // Privacy invariant J7: the JSON body POSTed to Wire MUST NOT
        // contain `local_depth`, `executing_source`, `is_executing`, or
        // `total_depth` (all dropped in structural-fix plan §2.10).
        // Enforced structurally (the struct doesn't carry them) and
        // verified here so a future sibling-struct edit can't silently
        // reintroduce them.
        let snap = QueueMirrorSnapshot {
            node_id: "node-abc".into(),
            snapshot_seq: 7,
            is_serving: true,
            offers: vec![sample_offer()],
        };
        let body = serde_json::to_string(&snap).expect("serialize must succeed");
        for forbidden in [
            "local_depth",
            "executing_source",
            "is_executing",
            "total_depth",
            "market_depth",
            "max_market_depth",
            "max_total_depth",
            "est_next_available_s",
            "timestamp",
        ] {
            assert!(
                !body.contains(forbidden),
                "snapshot must not expose `{forbidden}` (J7 / dropped); got: {body}"
            );
        }
    }

    #[test]
    fn offer_field_names_match_wire_contract() {
        // Pinned to structural-fix plan §2.10 mapping table. A field
        // rename here needs a concurrent Wire-side validator update.
        let j = serde_json::to_value(sample_offer()).unwrap();
        for k in &[
            "model_id",
            "wire_offer_id",
            "current_queue_depth",
            "max_queue_depth",
            "allow_market_visibility",
        ] {
            assert!(
                j.get(*k).is_some(),
                "missing field `{k}` in serialized QueueMirrorOffer"
            );
        }
        // Verify the exact set — no surprise extras.
        let obj = j.as_object().unwrap();
        assert_eq!(obj.len(), 5, "unexpected extra fields: {obj:?}");
    }

    #[test]
    fn snapshot_top_level_fields_match_wire_contract() {
        // Wire expects node_id, snapshot_seq, is_serving, offers.
        // Anything else returns 400 privacy_field_rejected.
        let snap = QueueMirrorSnapshot {
            node_id: "n".into(),
            snapshot_seq: 1,
            is_serving: true,
            offers: vec![],
        };
        let j = serde_json::to_value(&snap).unwrap();
        for k in &["node_id", "snapshot_seq", "is_serving", "offers"] {
            assert!(j.get(*k).is_some(), "missing top-level field `{k}`");
        }
        let obj = j.as_object().unwrap();
        assert_eq!(obj.len(), 4, "unexpected extra top-level fields: {obj:?}");
    }

    #[test]
    fn queue_mirror_path_points_at_queue_mirror_not_queue_state() {
        // Regression guard: the original node code hit
        // /api/v1/compute/queue-state (never registered on Wire).
        // Every mirror push 404'd silently, which in turn made Wire's
        // match_compute_job skip the offer as stale. That's how the
        // market stayed broken for weeks.
        assert_eq!(QUEUE_MIRROR_PATH, "/api/v1/compute/queue-mirror");
    }
}
