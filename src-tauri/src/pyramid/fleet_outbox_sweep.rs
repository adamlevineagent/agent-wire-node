//! Peer-side outbox sweep loop for async fleet dispatch.
//!
//! Spawns a background task that drives two predicates on the
//! `fleet_result_outbox` table:
//!
//! - **Predicate A** — state transitions by `expires_at`. Walks every row
//!   whose `expires_at <= now` and branches on `status`:
//!   - `pending` → promote to `ready` with a synth `FleetAsyncResult::Error`
//!     payload and record `fleet_worker_heartbeat_lost` (the worker died
//!     before completing; dispatcher hears about it via the normal
//!     delivery path).
//!   - `ready` → CAS-promote to `failed` (ready retention exhausted) and
//!     record `fleet_callback_exhausted`.
//!   - `delivered` / `failed` → DELETE (final retention expired).
//!
//! - **Predicate B** — delivery retry. Walks every `ready` row whose
//!   backoff has elapsed and attempts `deliver_fleet_result` again.
//!   On 2xx: `ready → delivered` via CAS. On failure: bump
//!   `delivery_attempts`. Rows that exhaust `max_delivery_attempts` get
//!   their `expires_at` bumped one second into the past — Predicate A
//!   on the next tick transitions them `ready → failed` via the
//!   standard CAS path, keeping `failed`-writing centralized.
//!
//! The loop's signature is `fn(db_path, Arc<FleetDispatchContext>)` per
//! the spec. All state the sweep needs — policy, roster, pending jobs,
//! tunnel state — is bundled in the context. The sweep NEVER holds any
//! async read lock across a blocking sqlite call and NEVER holds a
//! `std::sync::Mutex` across `.await` (`PendingFleetJobs` is used only
//! by the dispatcher-side sweep, not this one).
//!
//! Runs inside `tauri::async_runtime::spawn`. Sqlite work is wrapped in
//! `tokio::task::spawn_blocking` so a busy outbox doesn't starve the
//! reactor.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::fleet::{
    deliver_fleet_result, FleetAsyncResultEnvelope, FleetDispatchContext, FleetDeliveryError,
    FleetRoster,
};
use crate::pyramid::compute_chronicle::{
    record_event, ChronicleEventContext, EVENT_FLEET_CALLBACK_DELIVERED,
    EVENT_FLEET_CALLBACK_EXHAUSTED, EVENT_FLEET_CALLBACK_FAILED,
    EVENT_FLEET_WORKER_HEARTBEAT_LOST, SOURCE_FLEET_RECEIVED,
};
use crate::pyramid::db::{
    fleet_outbox_bump_delivery_attempt, fleet_outbox_delete, fleet_outbox_expire_exhausted,
    fleet_outbox_mark_delivered_if_ready, fleet_outbox_mark_failed_if_ready,
    fleet_outbox_promote_ready_if_pending, fleet_outbox_retry_candidates,
    fleet_outbox_sweep_expired, market_outbox_expire_exhausted, market_outbox_sweep_expired,
    synthesize_worker_error_json, OutboxRow,
};
use crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy;
use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;

/// Number of delivery attempts processed per yield point. Prevents reactor
/// starvation on deep outboxes without forcing an await between every row.
const RETRY_BATCH_YIELD_AT: usize = 20;

/// Event job_path prefix for peer-side sweep events. Each row stamps a
/// unique path from `(dispatcher_node_id, job_id)` so chronicle queries
/// can correlate worker → sweep → callback events for one job.
fn job_path_for(row: &OutboxRow) -> String {
    format!("fleet-recv:{}:{}", row.dispatcher_node_id, row.job_id)
}

/// Main sweep loop. Runs forever until the async runtime shuts down.
pub async fn fleet_outbox_sweep_loop(
    db_path: PathBuf,
    ctx: Arc<FleetDispatchContext>,
) {
    tracing::info!(
        db_path = %db_path.display(),
        "Fleet outbox sweep loop started"
    );
    loop {
        // Read policy fresh each tick so hot-reload takes effect.
        let policy = ctx.policy.read().await.clone();
        let interval = policy.outbox_sweep_interval_secs.max(1);
        tokio::time::sleep(Duration::from_secs(interval)).await;

        // Predicate A: expiry-driven transitions. Sync sqlite + chronicle
        // writes live inside spawn_blocking; return nothing — the loop is
        // fire-and-forget at this granularity, the next tick re-scans.
        if let Err(e) = sweep_expired_once(db_path.clone(), policy.clone()).await {
            tracing::warn!(err = %e, "Fleet outbox sweep (Predicate A) errored");
        }

        // Predicate B: delivery retries. Candidate SELECT via spawn_blocking;
        // callback POSTs async.
        if let Err(e) = sweep_retries_once(&db_path, &ctx, &policy).await {
            tracing::warn!(err = %e, "Fleet outbox sweep (Predicate B) errored");
        }
    }
}

/// Predicate A: state transitions by `expires_at`. One `spawn_blocking`
/// pass opens a connection, runs the expiry scan, and writes chronicle
/// events for each transition inside the same connection. No async here.
async fn sweep_expired_once(
    db_path: PathBuf,
    policy: FleetDeliveryPolicy,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let rows = fleet_outbox_sweep_expired(&conn)?;
        for row in rows {
            match row.status.as_str() {
                "pending" => {
                    // Worker heartbeat stopped before inference finished. Synth
                    // an Error payload and promote ready — delivery goes through
                    // the normal Predicate B path.
                    let synth = synthesize_worker_error_json(
                        "worker heartbeat lost — sweep promoted",
                    );
                    match fleet_outbox_promote_ready_if_pending(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                        &synth,
                        policy.ready_retention_secs,
                    ) {
                        Ok(1) => {
                            let ev = ChronicleEventContext::minimal(
                                &job_path_for(&row),
                                EVENT_FLEET_WORKER_HEARTBEAT_LOST,
                                SOURCE_FLEET_RECEIVED,
                            )
                            .with_metadata(serde_json::json!({
                                "peer_id": row.dispatcher_node_id,
                                "job_id": row.job_id,
                            }));
                            let _ = record_event(&conn, &ev);
                        }
                        Ok(_) => {
                            // Worker raced in and wrote ready/delivered between
                            // our SELECT and this CAS — fine, drop quietly.
                        }
                        Err(e) => {
                            tracing::warn!(
                                err = %e,
                                job_id = %row.job_id,
                                "fleet_outbox_promote_ready_if_pending failed in sweep"
                            );
                        }
                    }
                }
                "ready" => {
                    // Wall-clock retention on ready exhausted. Terminal failure.
                    match fleet_outbox_mark_failed_if_ready(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                        policy.failed_retention_secs,
                    ) {
                        Ok(1) => {
                            let ev = ChronicleEventContext::minimal(
                                &job_path_for(&row),
                                EVENT_FLEET_CALLBACK_EXHAUSTED,
                                SOURCE_FLEET_RECEIVED,
                            )
                            .with_metadata(serde_json::json!({
                                "peer_id": row.dispatcher_node_id,
                                "job_id": row.job_id,
                                "delivery_attempts": row.delivery_attempts,
                            }));
                            let _ = record_event(&conn, &ev);
                        }
                        Ok(_) => { /* raced — fine */ }
                        Err(e) => {
                            tracing::warn!(
                                err = %e,
                                job_id = %row.job_id,
                                "fleet_outbox_mark_failed_if_ready failed in sweep"
                            );
                        }
                    }
                }
                "delivered" | "failed" => {
                    // Final retention elapsed — clean up.
                    if let Err(e) = fleet_outbox_delete(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                    ) {
                        tracing::warn!(
                            err = %e,
                            job_id = %row.job_id,
                            status = %row.status,
                            "fleet_outbox_delete failed in sweep"
                        );
                    }
                }
                other => {
                    tracing::warn!(
                        status = %other,
                        job_id = %row.job_id,
                        "fleet_outbox_sweep_expired returned unknown status — skipping"
                    );
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??;
    Ok(())
}

/// Predicate B: retry `ready` rows whose backoff has elapsed. Candidate
/// SELECT runs in `spawn_blocking` and the per-row delivery POST runs in
/// async context (needs to `await` on `deliver_fleet_result`). Exhausted
/// rows (delivery_attempts ≥ max) get their `expires_at` bumped into the
/// past so Predicate A promotes them ready → failed on the next tick —
/// keeps "who writes `failed`" centralized in Predicate A.
async fn sweep_retries_once(
    db_path: &PathBuf,
    ctx: &Arc<FleetDispatchContext>,
    policy: &FleetDeliveryPolicy,
) -> anyhow::Result<()> {
    // Step 1: promote exhausted rows so they get picked up by Predicate A
    // on the next tick. Done up front so we don't waste delivery attempts
    // on rows that are already over the budget.
    {
        let db_path = db_path.clone();
        let max_attempts = policy.max_delivery_attempts;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let n = fleet_outbox_expire_exhausted(&conn, max_attempts)?;
            if n > 0 {
                tracing::debug!(
                    count = n,
                    "Fleet outbox sweep: {n} rows pushed to expired (retries exhausted)"
                );
            }
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??;
    }

    // Step 2: SELECT remaining candidates.
    let candidates: Vec<OutboxRow> = {
        let db_path = db_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OutboxRow>> {
            let conn = rusqlite::Connection::open(&db_path)?;
            Ok(fleet_outbox_retry_candidates(&conn)?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??
    };

    if candidates.is_empty() {
        return Ok(());
    }

    // Step 3: filter by backoff.
    let now = chrono::Utc::now();
    let eligible: Vec<OutboxRow> = candidates
        .into_iter()
        .filter(|row| is_eligible_for_retry(row, policy, now))
        .collect();

    // Step 4: attempt delivery on each eligible row. Yield every
    // RETRY_BATCH_YIELD_AT rows to keep the reactor responsive.
    for (idx, row) in eligible.into_iter().enumerate() {
        retry_deliver_one(db_path, ctx, policy, row).await;
        if (idx + 1) % RETRY_BATCH_YIELD_AT == 0 {
            tokio::task::yield_now().await;
        }
    }

    Ok(())
}

/// Returns `true` if `row` has waited long enough since its last failed
/// delivery attempt to try again. `delivery_attempts == 0` (never tried)
/// is always eligible. Backoff formula:
///   `delay = min(backoff_base_secs << min(delivery_attempts, 20),
///                 backoff_cap_secs)`
/// The shift cap at 20 prevents overflow for pathological attempt counts;
/// by then `backoff_cap_secs` dominates regardless.
fn is_eligible_for_retry(
    row: &OutboxRow,
    policy: &FleetDeliveryPolicy,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let attempts = row.delivery_attempts.max(0) as u32;
    let shift = attempts.min(20);
    let raw = policy.backoff_base_secs.checked_shl(shift).unwrap_or(u64::MAX);
    let delay_secs = raw.min(policy.backoff_cap_secs);

    let Some(last_str) = row.last_attempt_at.as_deref() else {
        return true;
    };
    // SQLite `datetime('now')` produces `YYYY-MM-DD HH:MM:SS` (no TZ).
    // Parse as naive UTC; compare.
    let parsed = chrono::NaiveDateTime::parse_from_str(last_str, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|ndt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(ndt, chrono::Utc));
    let Some(last) = parsed else {
        // Unparseable — treat as eligible (don't lose the row forever).
        tracing::warn!(
            last_attempt_at = %last_str,
            job_id = %row.job_id,
            "Fleet outbox: unparseable last_attempt_at; treating as eligible"
        );
        return true;
    };
    let elapsed = now.signed_duration_since(last);
    elapsed.num_seconds() >= delay_secs as i64
}

/// Attempt to deliver a single row. Snapshots the data needed from the
/// roster (tunnel_url + fleet_jwt + self_operator_id) under a short
/// read lock, then drops the lock before the HTTP POST — prevents
/// starvation of roster writers during slow callbacks.
async fn retry_deliver_one(
    db_path: &PathBuf,
    ctx: &Arc<FleetDispatchContext>,
    policy: &FleetDeliveryPolicy,
    row: OutboxRow,
) {
    let Some(raw_json) = row.result_json.as_deref() else {
        tracing::warn!(
            job_id = %row.job_id,
            "Fleet outbox: ready row has NULL result_json; skipping delivery"
        );
        return;
    };
    let outcome = match serde_json::from_str::<crate::fleet::FleetAsyncResult>(raw_json) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                err = %e,
                job_id = %row.job_id,
                "Fleet outbox: malformed result_json; skipping delivery"
            );
            return;
        }
    };
    let envelope = FleetAsyncResultEnvelope {
        job_id: row.job_id.clone(),
        outcome,
    };

    // Snapshot minimal roster data: clone the dispatcher entry only, plus
    // JWT and self_operator_id. Drop the read guard before calling
    // `deliver_fleet_result` so writers aren't blocked during the POST.
    let snapshot: FleetRoster = {
        let roster = ctx.fleet_roster.read().await;
        let mut peers = std::collections::HashMap::new();
        if let Some(peer) = roster.peers.get(&row.dispatcher_node_id) {
            peers.insert(row.dispatcher_node_id.clone(), peer.clone());
        }
        FleetRoster {
            peers,
            fleet_jwt: roster.fleet_jwt.clone(),
            self_operator_id: roster.self_operator_id.clone(),
        }
    };

    let delivery_result =
        deliver_fleet_result(&row.dispatcher_node_id, &row.callback_url, &envelope, &snapshot, policy)
            .await;

    // Apply CAS or bump-attempts under spawn_blocking; chronicle event in
    // the same connection.
    let db_path = db_path.clone();
    let row_cloned = row.clone();
    let delivered_retention = policy.delivered_retention_secs;
    let outcome_str = describe_delivery_outcome(&delivery_result);

    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        match delivery_result {
            Ok(()) => {
                match fleet_outbox_mark_delivered_if_ready(
                    &conn,
                    &row_cloned.dispatcher_node_id,
                    &row_cloned.job_id,
                    delivered_retention,
                ) {
                    Ok(1) => {
                        let ev = ChronicleEventContext::minimal(
                            &job_path_for(&row_cloned),
                            EVENT_FLEET_CALLBACK_DELIVERED,
                            SOURCE_FLEET_RECEIVED,
                        )
                        .with_metadata(serde_json::json!({
                            "peer_id": row_cloned.dispatcher_node_id,
                            "job_id": row_cloned.job_id,
                            "attempts": row_cloned.delivery_attempts + 1,
                        }));
                        let _ = record_event(&conn, &ev);
                    }
                    Ok(_) => {
                        // Sweep concurrently promoted ready→failed. The 2xx
                        // callback already landed on the dispatcher — nothing
                        // to do, but log it for visibility.
                        tracing::debug!(
                            job_id = %row_cloned.job_id,
                            "Fleet outbox: ready→delivered CAS lost to sweep; 2xx already landed"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            err = %e,
                            job_id = %row_cloned.job_id,
                            "fleet_outbox_mark_delivered_if_ready failed after 2xx"
                        );
                    }
                }
            }
            Err(err) => {
                let err_msg = err.to_string();
                if let Err(e) = fleet_outbox_bump_delivery_attempt(
                    &conn,
                    &row_cloned.dispatcher_node_id,
                    &row_cloned.job_id,
                    &err_msg,
                ) {
                    tracing::warn!(
                        err = %e,
                        job_id = %row_cloned.job_id,
                        "fleet_outbox_bump_delivery_attempt failed"
                    );
                }
                let ev = ChronicleEventContext::minimal(
                    &job_path_for(&row_cloned),
                    EVENT_FLEET_CALLBACK_FAILED,
                    SOURCE_FLEET_RECEIVED,
                )
                .with_metadata(serde_json::json!({
                    "peer_id": row_cloned.dispatcher_node_id,
                    "job_id": row_cloned.job_id,
                    "attempts": row_cloned.delivery_attempts + 1,
                    "error": err_msg,
                    "outcome_kind": outcome_str,
                }));
                let _ = record_event(&conn, &ev);
            }
        }
        Ok(())
    })
    .await;
}

/// Tag the delivery outcome for observability. Kept inline so the
/// chronicle metadata can distinguish transport vs JWT vs HTTP-status
/// failures at a glance without parsing the error string.
fn describe_delivery_outcome(r: &Result<(), FleetDeliveryError>) -> &'static str {
    match r {
        Ok(()) => "ok",
        Err(FleetDeliveryError::Transport(_)) => "transport",
        Err(FleetDeliveryError::NoJwt) => "no_jwt",
        Err(FleetDeliveryError::JwtExpired) => "jwt_expired",
        Err(FleetDeliveryError::HttpStatus { .. }) => "http_status",
        Err(FleetDeliveryError::ResponseParse(_)) => "response_parse",
    }
}

// ══════════════════════════════════════════════════════════════════════
// Market outbox sweep (Phase 2 WS6)
// ══════════════════════════════════════════════════════════════════════
//
// Shape-parallel to the Fleet sweep above, but only ships Predicate A
// (expiry-driven state transitions) + `market_outbox_expire_exhausted`
// (push exhausted rows into the past so Predicate A's next tick
// transitions them to `failed` via the centralized CAS path).
//
// Predicate B (delivery retries) is Phase 3 territory. Exists as a db
// helper (`market_outbox_retry_candidates`) so the later workstream
// can drop in without adding more SELECT helpers.
//
// Partition guarantee: every market sweep SELECT filters on
// `callback_kind != 'Fleet'`; every Fleet sweep SELECT filters on
// `callback_kind = 'Fleet'`. The two loops MUST NOT race on the same
// rows. A market row slipping into the fleet sweep would synthesize a
// Fleet-shaped error payload into a market callback URL and likely
// 4xx at the Wire; a fleet row slipping into the market sweep would
// be held to market's admission budget, orphaning it.

/// job_path prefix for market sweep events. Each row stamps a unique
/// path from `job_id` alone — market's `dispatcher_node_id` is always
/// the WIRE_PLATFORM_DISPATCHER sentinel, so including it doesn't
/// disambiguate.
fn market_job_path_for(row: &OutboxRow) -> String {
    format!("market-recv:{}", row.job_id)
}

/// Spawn the market outbox sweep loop. Mirror of
/// `fleet_outbox_sweep_loop` for market/relay rows. Runs forever until
/// the async runtime shuts down.
///
/// WHY a dedicated spawn wrapper (vs the caller doing `tauri::async_runtime::spawn`
/// directly like the Fleet init path does): the market loop needs its
/// OWN `MarketDeliveryPolicy` read cadence — the two policies tune
/// differently (fleet is peer-to-peer, market is node ↔ Wire), so
/// sharing the fleet policy would let a fleet-tuning change silently
/// alter market sweep behavior. Making the market loop a public `fn`
/// keeps its signature pinned.
/// Market outbox sweep loop. See §"Trigger model" for architecture.
///
/// `delivery_nudge` (Phase 3): optional sender for the market delivery
/// worker's nudge channel. `Some` in production (wired from
/// MarketDispatchContext); `None` in tests that exercise the sweep
/// directly. When `Some`, sweep fires a nudge after any pending→ready
/// promotion (heartbeat-lost synth path) so the delivery worker picks
/// up the synthesized-error row without waiting for its next natural
/// tick.
pub async fn market_outbox_sweep_loop(
    db_path: PathBuf,
    policy_handle: Arc<tokio::sync::RwLock<MarketDeliveryPolicy>>,
    delivery_nudge: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) {
    tracing::info!(
        db_path = %db_path.display(),
        "Market outbox sweep loop started"
    );
    loop {
        // Read policy fresh each tick so hot-reload takes effect.
        let policy = policy_handle.read().await.clone();
        let interval = policy.outbox_sweep_interval_secs.max(1);
        tokio::time::sleep(Duration::from_secs(interval)).await;

        // Predicate B companion: push exhausted rows into the past
        // BEFORE Predicate A runs so they get transitioned by the
        // same tick instead of having to wait a full interval.
        if let Err(e) = expire_exhausted_market_once(db_path.clone(), policy.clone()).await {
            tracing::warn!(
                err = %e,
                "Market outbox sweep (expire_exhausted) errored"
            );
        }

        // Predicate A: expiry-driven transitions. Sync sqlite +
        // chronicle writes inside spawn_blocking. The sweep returns
        // the number of pending→ready promotions so we can nudge the
        // delivery worker if any fired.
        match sweep_expired_market_once(db_path.clone(), policy.clone()).await {
            Ok(promoted) => {
                if promoted > 0 {
                    if let Some(nudge) = delivery_nudge.as_ref() {
                        let _ = nudge.send(());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(err = %e, "Market outbox sweep (Predicate A) errored");
            }
        }
    }
}

/// Predicate A for the market sweep: walk every expired market/relay
/// row, branch on `status`, and transition. Parallel to
/// `sweep_expired_once` with the market-filtered SELECT and market
/// chronicle event names.
async fn sweep_expired_market_once(
    db_path: PathBuf,
    policy: MarketDeliveryPolicy,
) -> anyhow::Result<usize> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let rows = market_outbox_sweep_expired(&conn)?;
        let mut promoted_count: usize = 0;
        for row in rows {
            match row.status.as_str() {
                "pending" => {
                    // Worker heartbeat stopped before inference finished.
                    // Synth an Error payload and promote to ready — the
                    // callback-delivery worker (Phase 3) owns the POST.
                    let synth = synthesize_worker_error_json(
                        "worker heartbeat lost — market sweep promoted",
                    );
                    match fleet_outbox_promote_ready_if_pending(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                        &synth,
                        policy.ready_retention_secs,
                    ) {
                        Ok(1) => {
                            promoted_count += 1;
                            let ev = ChronicleEventContext::minimal(
                                &market_job_path_for(&row),
                                crate::pyramid::compute_chronicle::EVENT_MARKET_WORKER_HEARTBEAT_LOST,
                                crate::pyramid::compute_chronicle::SOURCE_MARKET_RECEIVED,
                            )
                            .with_metadata(serde_json::json!({
                                "job_id": row.job_id,
                            }));
                            let _ = record_event(&conn, &ev);
                        }
                        Ok(_) => {
                            // Worker raced in and wrote ready/delivered
                            // between our SELECT and this CAS — fine.
                        }
                        Err(e) => {
                            tracing::warn!(
                                err = %e,
                                job_id = %row.job_id,
                                "market promote_ready_if_pending failed in sweep"
                            );
                        }
                    }
                }
                "ready" => {
                    // Wall-clock retention on ready exhausted — terminal
                    // failure for the market row. Writes `failed` with
                    // `expires_at = now + failed_retention_secs` so the
                    // row stays visible for post-mortem before final
                    // cleanup.
                    match fleet_outbox_mark_failed_if_ready(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                        policy.failed_retention_secs,
                    ) {
                        Ok(1) => {
                            let ev = ChronicleEventContext::minimal(
                                &market_job_path_for(&row),
                                crate::pyramid::compute_chronicle::EVENT_MARKET_CALLBACK_EXHAUSTED,
                                crate::pyramid::compute_chronicle::SOURCE_MARKET_RECEIVED,
                            )
                            .with_metadata(serde_json::json!({
                                "job_id": row.job_id,
                                "delivery_attempts": row.delivery_attempts,
                            }));
                            let _ = record_event(&conn, &ev);
                        }
                        Ok(_) => { /* raced — fine */ }
                        Err(e) => {
                            tracing::warn!(
                                err = %e,
                                job_id = %row.job_id,
                                "market mark_failed_if_ready failed in sweep"
                            );
                        }
                    }
                }
                "delivered" | "failed" => {
                    // Final retention elapsed — clean up.
                    if let Err(e) = fleet_outbox_delete(
                        &conn,
                        &row.dispatcher_node_id,
                        &row.job_id,
                    ) {
                        tracing::warn!(
                            err = %e,
                            job_id = %row.job_id,
                            status = %row.status,
                            "market outbox_delete failed in sweep"
                        );
                    }
                }
                other => {
                    tracing::warn!(
                        status = %other,
                        job_id = %row.job_id,
                        "market_outbox_sweep_expired returned unknown status — skipping"
                    );
                }
            }
        }
        Ok(promoted_count)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))?
}

/// Push market rows at/above their attempt budget into the past so
/// Predicate A transitions them on the next tick. Mirror of the fleet
/// path's step-1 inside `sweep_retries_once`, but scoped to market rows.
async fn expire_exhausted_market_once(
    db_path: PathBuf,
    policy: MarketDeliveryPolicy,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let max_attempts = policy.max_delivery_attempts;
        let n = market_outbox_expire_exhausted(&conn, max_attempts)?;
        if n > 0 {
            tracing::debug!(
                count = n,
                "Market outbox sweep: {n} rows pushed to expired (retries exhausted)"
            );
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_with(attempts: i64, last: Option<&str>) -> OutboxRow {
        OutboxRow {
            dispatcher_node_id: "did-A".into(),
            job_id: "job-1".into(),
            status: "ready".into(),
            callback_url: "https://x/v1/fleet/result".into(),
            result_json: Some(r#"{"kind":"Error","data":"x"}"#.into()),
            delivery_attempts: attempts,
            last_attempt_at: last.map(|s| s.to_string()),
            expires_at: "2099-01-01 00:00:00".into(),
            created_at: "1970-01-01 00:00:00".into(),
            callback_auth_token: None,
            delivery_lease_until: None,
            delivery_next_attempt_at: None,
            inference_latency_ms: None,
            request_id: None,
            requester_callback_url: None,
            requester_delivery_jwt: None,
            content_posted_ok: 0,
            content_lease_until: None,
            content_next_attempt_at: None,
            content_last_error: None,
            settlement_posted_ok: 0,
            settlement_delivery_attempts: 0,
            settlement_lease_until: None,
            settlement_next_attempt_at: None,
            settlement_last_error: None,
        }
    }

    #[test]
    fn first_attempt_is_always_eligible() {
        let policy = FleetDeliveryPolicy::default();
        let now = chrono::Utc::now();
        assert!(is_eligible_for_retry(&row_with(0, None), &policy, now));
    }

    #[test]
    fn recent_attempt_not_eligible() {
        let mut policy = FleetDeliveryPolicy::default();
        policy.backoff_base_secs = 10;
        policy.backoff_cap_secs = 300;
        let now = chrono::Utc::now();
        let last = (now - chrono::Duration::seconds(5))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(!is_eligible_for_retry(&row_with(1, Some(&last)), &policy, now));
    }

    #[test]
    fn backoff_expired_is_eligible() {
        let mut policy = FleetDeliveryPolicy::default();
        policy.backoff_base_secs = 10;
        policy.backoff_cap_secs = 300;
        let now = chrono::Utc::now();
        // attempts=2 → delay = min(10<<2, 300) = 40 seconds
        let last = (now - chrono::Duration::seconds(60))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(is_eligible_for_retry(&row_with(2, Some(&last)), &policy, now));
    }

    #[test]
    fn pathological_attempts_dont_overflow() {
        let policy = FleetDeliveryPolicy::default();
        let now = chrono::Utc::now();
        // attempts=50 must still work (shift capped at 20)
        assert!(is_eligible_for_retry(&row_with(50, None), &policy, now));
    }

    #[test]
    fn unparseable_last_attempt_falls_through_to_eligible() {
        let policy = FleetDeliveryPolicy::default();
        let now = chrono::Utc::now();
        assert!(is_eligible_for_retry(
            &row_with(1, Some("not a timestamp")),
            &policy,
            now
        ));
    }

    // ── Market sweep helpers (Phase 2 WS6) ────────────────────────────

    #[test]
    fn market_job_path_for_uses_job_id_only() {
        // The dispatcher_node_id is always the WIRE_PLATFORM_DISPATCHER
        // sentinel for market rows. Including it in the path adds a
        // constant prefix to every row without disambiguation — the
        // job_id is UUID-unique on its own (job_id carries UNIQUE).
        let row = OutboxRow {
            dispatcher_node_id: crate::fleet::WIRE_PLATFORM_DISPATCHER.into(),
            job_id: "abc-123".into(),
            status: "pending".into(),
            callback_url: "https://wire.example.com/v1/compute/result-relay".into(),
            result_json: None,
            delivery_attempts: 0,
            last_attempt_at: None,
            expires_at: "1970-01-01 00:00:00".into(),
            created_at: "1970-01-01 00:00:00".into(),
            callback_auth_token: None,
            delivery_lease_until: None,
            delivery_next_attempt_at: None,
            inference_latency_ms: None,
            request_id: None,
            requester_callback_url: None,
            requester_delivery_jwt: None,
            content_posted_ok: 0,
            content_lease_until: None,
            content_next_attempt_at: None,
            content_last_error: None,
            settlement_posted_ok: 0,
            settlement_delivery_attempts: 0,
            settlement_lease_until: None,
            settlement_next_attempt_at: None,
            settlement_last_error: None,
        };
        assert_eq!(market_job_path_for(&row), "market-recv:abc-123");
        // Note: parallel `job_path_for` (fleet) uses the pattern
        // "fleet-recv:{dispatcher}:{job}"; we DON'T use the same pattern
        // here because the dispatcher string is redundant for market.
    }

    #[tokio::test]
    async fn market_sweep_promotes_expired_pending_to_ready() {
        // End-to-end smoke: an expired market pending row gets promoted
        // to ready with a synth Error payload after one sweep tick.
        use crate::pyramid::db::{fleet_outbox_lookup, market_outbox_insert_or_ignore};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("pyramid.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        // Insert an already-expired MarketStandard row.
        market_outbox_insert_or_ignore(
            &conn,
            "mkt-expired",
            "https://wire.example.com/v1/compute/result-relay",
            "MarketStandard",
            "1970-01-01 00:00:00",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        drop(conn);

        let policy = MarketDeliveryPolicy::default();
        sweep_expired_market_once(db_path.clone(), policy).await.unwrap();

        // Row should now be ready with a synth Error payload.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let lookup = fleet_outbox_lookup(&conn, "mkt-expired").unwrap().unwrap();
        assert_eq!(lookup.status, "ready");
        let result_json: Option<String> = conn
            .query_row(
                "SELECT result_json FROM fleet_result_outbox WHERE job_id = 'mkt-expired'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let body = result_json.unwrap();
        assert!(body.contains("\"kind\":\"Error\""),
            "sweep must synth Error payload, got: {body}");
        assert!(body.contains("worker heartbeat lost"),
            "payload must include the heartbeat-lost reason");
    }

    #[tokio::test]
    async fn market_sweep_does_not_touch_fleet_rows() {
        // Regression: market sweep partition. A fleet row and a market
        // row both expired — only the market row must be transitioned.
        use crate::pyramid::db::{
            fleet_outbox_insert_or_ignore, fleet_outbox_lookup,
            market_outbox_insert_or_ignore,
        };
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("pyramid.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        fleet_outbox_insert_or_ignore(
            &conn,
            "peer-A",
            "fleet-expired",
            "https://peer.example/v1/fleet/result",
            "1970-01-01 00:00:00",
        )
        .unwrap();
        market_outbox_insert_or_ignore(
            &conn,
            "mkt-expired",
            "https://wire.example.com/v1/compute/result-relay",
            "MarketStandard",
            "1970-01-01 00:00:00",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        drop(conn);

        let policy = MarketDeliveryPolicy::default();
        sweep_expired_market_once(db_path.clone(), policy).await.unwrap();

        // Market row: ready. Fleet row: still pending (owned by fleet sweep).
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mkt = fleet_outbox_lookup(&conn, "mkt-expired").unwrap().unwrap();
        let flt = fleet_outbox_lookup(&conn, "fleet-expired").unwrap().unwrap();
        assert_eq!(mkt.status, "ready");
        assert_eq!(flt.status, "pending",
            "market sweep must NOT transition fleet rows");
    }

    #[tokio::test]
    async fn market_sweep_expire_exhausted_pushes_only_market_rows() {
        // Regression: `market_outbox_expire_exhausted` must not push
        // Fleet rows past their expires_at. A Fleet row at the same
        // attempts count is owned by `fleet_outbox_expire_exhausted`
        // with its own policy budget.
        use crate::pyramid::db::market_outbox_insert_or_ignore;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("pyramid.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let far_future = "9999-12-31 23:59:59";
        // Insert a Fleet row directly at `status=ready, attempts=5,
        // expires_at=far_future` to pin the exact starting state —
        // using the helpers would overwrite expires_at during
        // promote_ready_if_pending.
        conn.execute(
            "INSERT INTO fleet_result_outbox
                (dispatcher_node_id, job_id, callback_url, callback_kind,
                 status, expires_at, delivery_attempts, ready_at)
             VALUES ('peer-A', 'fleet-busy', 'https://peer/x', 'Fleet',
                 'ready', ?1, 5, datetime('now'))",
            rusqlite::params![far_future],
        )
        .unwrap();
        market_outbox_insert_or_ignore(
            &conn,
            "mkt-busy",
            "https://wire/x",
            "MarketStandard",
            far_future,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        conn.execute(
            "UPDATE fleet_result_outbox
                SET status = 'ready',
                    delivery_attempts = 5,
                    ready_at = datetime('now')
              WHERE job_id = 'mkt-busy'",
            [],
        )
        .unwrap();
        drop(conn);

        let mut policy = MarketDeliveryPolicy::default();
        policy.max_delivery_attempts = 5;
        expire_exhausted_market_once(db_path.clone(), policy).await.unwrap();

        // Market row expires_at pushed into the past; fleet row unchanged.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mkt_exp: String = conn
            .query_row(
                "SELECT expires_at FROM fleet_result_outbox WHERE job_id = 'mkt-busy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let flt_exp: String = conn
            .query_row(
                "SELECT expires_at FROM fleet_result_outbox WHERE job_id = 'fleet-busy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            flt_exp, far_future,
            "fleet row's expires_at must be untouched by market helper"
        );
        // Market row was pushed to `now - 1s`; lexicographic compare
        // is fine here because the far-future fixture starts with
        // `9999-` which sorts after any realistic `now`.
        assert_ne!(mkt_exp, far_future,
            "market row's expires_at should have been pushed into the past");
    }
}
