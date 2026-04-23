// pyramid/compute_chronicle.rs — Persistent compute observability layer.
//
// Every LLM call that touches any compute resource gets recorded in
// `pyramid_compute_events`. The operator sees one unified history with
// source as a dimension. Append-only, immutable — events never update
// or delete.
//
// This module provides:
//   - `ChronicleEventContext` — struct carrying all fields for a single event
//   - `record_event` — append-only INSERT
//   - `generate_job_path` — semantic path generation (NO UUIDs)
//   - `query_events` — filterable query with all 11 dimensions
//   - `query_summary` — grouped aggregation for stats dashboard
//   - `query_timeline` — bucketed data for timeline visualization
//   - `query_distinct_dimensions` — distinct values for filter dropdowns
//   - Market stubs (called when market phases ship)

use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use super::step_context::StepContext;
use crate::compute_queue::QueueEntry;

// ── Canonical source constants ────────────────────────────────────────────
// The `source` column records which compute lane produced the event.
// Dispatcher-side fleet events use SOURCE_FLEET.
// Peer-side (received-from-peer) events and queue entries use SOURCE_FLEET_RECEIVED.
pub const SOURCE_FLEET: &str = "fleet";
pub const SOURCE_FLEET_RECEIVED: &str = "fleet_received";

// Market-side source constants (Phase 2 WS8).
// `SOURCE_MARKET`       — requester side + provider-side offer publication
//                         (offer publication is not tied to a job, but the
//                         event is attributed to the provider's market
//                         activity so SOURCE_MARKET keeps it grouped with
//                         other market-dispatcher events).
// `SOURCE_MARKET_RECEIVED` — provider side: job arrived at the provider
//                         from the Wire. Parallels SOURCE_FLEET_RECEIVED.
// See `docs/plans/compute-market-phase-2-exchange.md` §III L603-632.
pub const SOURCE_MARKET: &str = "market";
pub const SOURCE_MARKET_RECEIVED: &str = "market_received";

// ── Canonical event_type constants ────────────────────────────────────────
// These are the canonical event_type string values emitted by the async
// fleet dispatch path. Later workstreams migrate their emission sites to
// use these constants instead of raw string literals.

// Dispatcher-side events (source='fleet') — the node that sent the job.
pub const EVENT_FLEET_DISPATCHED_ASYNC: &str = "fleet_dispatched_async";
pub const EVENT_FLEET_DISPATCH_FAILED: &str = "fleet_dispatch_failed";
pub const EVENT_FLEET_PEER_OVERLOADED: &str = "fleet_peer_overloaded";
pub const EVENT_FLEET_DISPATCH_TIMEOUT: &str = "fleet_dispatch_timeout";
pub const EVENT_FLEET_RESULT_RECEIVED: &str = "fleet_result_received";
pub const EVENT_FLEET_RESULT_FAILED: &str = "fleet_result_failed";
pub const EVENT_FLEET_RESULT_ORPHANED: &str = "fleet_result_orphaned";
pub const EVENT_FLEET_RESULT_FORGERY_ATTEMPT: &str = "fleet_result_forgery_attempt";
pub const EVENT_FLEET_PENDING_ORPHANED: &str = "fleet_pending_orphaned";

// Peer-side events (source='fleet_received') — the node that received the job.
pub const EVENT_FLEET_JOB_ACCEPTED: &str = "fleet_job_accepted";
pub const EVENT_FLEET_ADMISSION_REJECTED: &str = "fleet_admission_rejected";
pub const EVENT_FLEET_JOB_COMPLETED: &str = "fleet_job_completed";
pub const EVENT_FLEET_CALLBACK_DELIVERED: &str = "fleet_callback_delivered";
pub const EVENT_FLEET_CALLBACK_FAILED: &str = "fleet_callback_failed";
pub const EVENT_FLEET_CALLBACK_EXHAUSTED: &str = "fleet_callback_exhausted";
pub const EVENT_FLEET_WORKER_HEARTBEAT_LOST: &str = "fleet_worker_heartbeat_lost";
pub const EVENT_FLEET_WORKER_SWEEP_LOST: &str = "fleet_worker_sweep_lost";
pub const EVENT_FLEET_DELIVERY_CAS_LOST: &str = "fleet_delivery_cas_lost";

// Market events (Phase 2 WS8).
//
// Per `docs/plans/compute-market-phase-2-exchange.md` §III L603-632.
//
//   EVENT_MARKET_OFFERED  — provider published an offer. Fires from
//                           `compute_offer_create` (main.rs) on successful
//                           Wire publication. Source: SOURCE_MARKET.
//                           job_path: `market/offer/{model_id}`.
//                           work_item_id: None (offer management is not
//                           DADBEAR-tracked).
//   EVENT_MARKET_RECEIVED — provider received a matched job dispatch.
//                           Fires from `handle_market_dispatch` (server.rs)
//                           after the outbox admission commits.
//                           Source: SOURCE_MARKET_RECEIVED.
//                           job_path: `market/{job_id}`.
//                           work_item_id + attempt_id: populated from the
//                           DADBEAR work item created in WS8 Part A.
//   EVENT_MARKET_MATCHED  — requester matched a job. Phase 3 scope; the
//                           constant is defined in Phase 2 so the chronicle
//                           schema is stable across phases. NO emission
//                           site in Phase 2.
//   EVENT_QUEUE_MIRROR_PUSH_FAILED — queue-mirror push to the Wire failed.
//                           Emitted by WS6's mirror task (not this WS).
//                           Constant lives here so WS6 can import it.
pub const EVENT_MARKET_OFFERED: &str = "market_offered";
pub const EVENT_MARKET_RECEIVED: &str = "market_received";
pub const EVENT_MARKET_MATCHED: &str = "market_matched";
pub const EVENT_QUEUE_MIRROR_PUSH_FAILED: &str = "queue_mirror_push_failed";

// Mirror-task lifecycle events — loud signals when the task is not
// doing its job. Before these existed, 54 hours of silent non-pushing
// looked identical to a healthy idle node from the operator's view.
pub const EVENT_MIRROR_TASK_PANICKED: &str = "market_mirror_task_panicked";
pub const EVENT_MIRROR_TASK_EXITED: &str = "market_mirror_task_exited";

// Phase 3 provider-delivery worker events. Node-side only — Wire's
// chronicle has its own `compute_result_delivered` for the Wire→requester
// hop, which we avoid colliding with by keeping this prefix `market_*`
// matching the existing compute_chronicle.rs taxonomy.
//
// Emission sites are all in `pyramid::market_delivery` + the integration
// points in server.rs (spawn_market_worker) and fleet_outbox_sweep.rs
// (heartbeat-lost path).
// Rev 0.5 (Wire-in-middle) — DEPRECATED for new emissions as of rev 0.6.1.
// Constant kept so downstream chronicle queries can UNION the deprecated
// name against its rev-0.6.1 replacements during the grandfathering window.
// New code MUST NOT emit this event; the rev 0.6.1 delivery worker emits
// `EVENT_MARKET_RESULT_DELIVERED` (both legs OK) instead.
pub const EVENT_MARKET_RESULT_DELIVERED_TO_WIRE: &str = "market_result_delivered_to_wire";
pub const EVENT_MARKET_RESULT_DELIVERY_CAS_LOST: &str = "market_result_delivery_cas_lost";
pub const EVENT_MARKET_RESULT_DELIVERY_ATTEMPT_FAILED: &str =
    "market_result_delivery_attempt_failed";
pub const EVENT_MARKET_RESULT_DELIVERY_FAILED: &str = "market_result_delivery_failed";
pub const EVENT_MARKET_DELIVERY_TASK_PANICKED: &str = "market_delivery_task_panicked";
pub const EVENT_MARKET_DELIVERY_TASK_EXITED: &str = "market_delivery_task_exited";
pub const EVENT_MARKET_WIRE_PARAMETERS_UPDATED: &str = "market_wire_parameters_updated";

// Phase 3 rev 0.6.1 (two-POST P2P delivery) — final taxonomy per spec §
// "Chronicle events (rev 0.6 final taxonomy)". The delivery worker now owns
// two independent legs (content → requester direct, settlement → Wire).
// Events split into per-leg attempt/success/terminal + a final summary
// event for both-legs-OK and a final dual-terminal for both-legs-dead.
//
// Emission sites live in `pyramid::market_delivery` (`deliver_leg` and
// friends). The legacy `EVENT_MARKET_RESULT_DELIVERED_TO_WIRE` is kept in
// this module purely for grandfathered chronicle rows; rev 0.6.1 code
// path MUST NOT emit it (grep enforced at build time by the wanderer).
pub const EVENT_MARKET_RESULT_DELIVERED: &str = "market_result_delivered";
pub const EVENT_MARKET_CONTENT_LEG_SUCCEEDED: &str = "market_content_leg_succeeded";
pub const EVENT_MARKET_SETTLEMENT_LEG_SUCCEEDED: &str = "market_settlement_leg_succeeded";
pub const EVENT_MARKET_CONTENT_DELIVERY_ATTEMPT_FAILED: &str =
    "market_content_delivery_attempt_failed";
pub const EVENT_MARKET_SETTLEMENT_DELIVERY_ATTEMPT_FAILED: &str =
    "market_settlement_delivery_attempt_failed";
pub const EVENT_MARKET_CONTENT_DELIVERY_FAILED: &str = "market_content_delivery_failed";
pub const EVENT_MARKET_SETTLEMENT_DELIVERY_FAILED: &str = "market_settlement_delivery_failed";

// Market sweep companions to the fleet sweep events (Phase 2 WS6).
// Emitted by the market outbox sweep loop in
// `pyramid::fleet_outbox_sweep::market_outbox_sweep_loop` when a market
// row transitions due to expiry. Kept distinct from the Fleet versions
// so operator dashboards can slice by lane — same event shape.
pub const EVENT_MARKET_WORKER_HEARTBEAT_LOST: &str = "market_worker_heartbeat_lost";
pub const EVENT_MARKET_CALLBACK_EXHAUSTED: &str = "market_callback_exhausted";

// Network-framed requester-side constants (call_model_unified market
// integration — invisibility-safe names for the cross-operator peer
// dispatch path). See
// `docs/plans/call-model-unified-market-integration.md` §4.1. These
// are the names the chronicle surfaces to UIs + agent-facing tooling;
// the underlying transport is the compute market.
pub const SOURCE_NETWORK: &str = "network";
pub const SOURCE_NETWORK_RECEIVED: &str = "network_received";
pub const EVENT_NETWORK_HELPED_BUILD: &str = "network_helped_build";
pub const EVENT_NETWORK_RESULT_RETURNED: &str = "network_result_returned";
pub const EVENT_NETWORK_FELL_BACK_LOCAL: &str = "network_fell_back_local";
pub const EVENT_NETWORK_LATE_ARRIVAL: &str = "network_late_arrival";
pub const EVENT_NETWORK_BALANCE_EXHAUSTED: &str = "network_balance_exhausted";
pub const EVENT_BUILD_NETWORK_CONTRIBUTION: &str = "build_network_contribution";

// ── Walker lifecycle events (rev 2.1 compute dispatch walker) ───────────
//
// Walker Re-Plan Wire 2.1 — plan §5 chronicle vocabulary. Emitted from
// the per-entry walker loop in `call_model_unified_with_audit_and_ctx`
// (llm.rs). Wave 1 emits a subset (`walker_resolved`, `walker_exhausted`,
// `network_route_skipped`, `network_route_saturated`,
// `network_route_unavailable`, `network_route_retryable_fail`,
// `network_route_terminal_fail`); the remaining constants land now so
// Waves 2-4 don't re-introduce them piecemeal. `#[allow(dead_code)]`
// until their emission sites wire up.
#[allow(dead_code)]
pub const EVENT_WALKER_RESOLVED: &str = "walker_resolved";
#[allow(dead_code)]
pub const EVENT_WALKER_EXHAUSTED: &str = "walker_exhausted";
#[allow(dead_code)]
pub const EVENT_WALKER_PATH_DISTRIBUTION: &str = "walker_path_distribution";
#[allow(dead_code)]
pub const EVENT_WALKER_QUOTE_RACE_STATS: &str = "walker_quote_race_stats";

#[allow(dead_code)]
pub const EVENT_NETWORK_ROUTE_SKIPPED: &str = "network_route_skipped";
#[allow(dead_code)]
pub const EVENT_NETWORK_ROUTE_SATURATED: &str = "network_route_saturated";
#[allow(dead_code)]
pub const EVENT_NETWORK_ROUTE_UNAVAILABLE: &str = "network_route_unavailable";
#[allow(dead_code)]
pub const EVENT_NETWORK_ROUTE_RETRYABLE_FAIL: &str = "network_route_retryable_fail";
#[allow(dead_code)]
pub const EVENT_NETWORK_ROUTE_TERMINAL_FAIL: &str = "network_route_terminal_fail";
#[allow(dead_code)]
pub const EVENT_NETWORK_MODEL_UNAVAILABLE: &str = "network_model_unavailable";

#[allow(dead_code)]
pub const EVENT_NETWORK_QUOTED: &str = "network_quoted";
#[allow(dead_code)]
pub const EVENT_NETWORK_PURCHASED: &str = "network_purchased";
#[allow(dead_code)]
pub const EVENT_NETWORK_QUOTE_EXPIRED: &str = "network_quote_expired";
#[allow(dead_code)]
pub const EVENT_NETWORK_PURCHASE_RECOVERED: &str = "network_purchase_recovered";
#[allow(dead_code)]
pub const EVENT_NETWORK_RATE_ABOVE_BUDGET: &str = "network_rate_above_budget";
#[allow(dead_code)]
pub const EVENT_NETWORK_DISPATCH_DEADLINE_MISSED: &str = "network_dispatch_deadline_missed";
#[allow(dead_code)]
pub const EVENT_NETWORK_PROVIDER_SATURATED: &str = "network_provider_saturated";
#[allow(dead_code)]
pub const EVENT_NETWORK_BALANCE_INSUFFICIENT_FOR_MARKET: &str =
    "network_balance_insufficient_for_market";
#[allow(dead_code)]
pub const EVENT_NETWORK_AUTH_EXPIRED: &str = "network_auth_expired";

/// Rev 2.1.1 saturation-backoff visibility. Emitted by walker between
/// successive retries of a market entry while `all_offers_saturated_for_model`
/// keeps firing. Gives operators live feedback during queue-drain waits:
///   - next_attempt_at (RFC 3339 UTC)
///   - min_expected_drain_ms (from Wire's AllOffersSaturatedDetail)
///   - elapsed_secs_in_backoff_loop (cumulative walker wait for this chunk)
///   - patience_budget_secs (compute_participation_policy value)
///
/// Operators expect slow-and-steady throughput on large corpora; this
/// event is what makes "walker is waiting, not stuck" legible in the UI.
#[allow(dead_code)]
pub const EVENT_MARKET_BACKOFF_WAITING: &str = "market_backoff_waiting";

/// Walker v3 Phase 3: /quote pre-gate skipped an offer. Emitted when
/// `typical_serve_ms_p50_7d × peer_queue_depth` exceeds the usable
/// dispatch deadline (`dispatch_deadline - dispatch_deadline_grace_secs`).
/// Walker advances to the next market entry instead of paying a
/// reservation fee on an offer that can't meet the deadline.
///
/// Payload fields:
///   - offer_id
///   - typical_serve_ms_p50_7d (per-offer or model-level fallback)
///   - peer_queue_depth (current_queue_depth + execution_concurrency)
///   - estimated_serve_ms (product above)
///   - usable_deadline_ms (dispatch_deadline − grace)
///   - branch: "market"
#[allow(dead_code)]
pub const EVENT_OFFER_SKIPPED_PRE_GATE_DEADLINE: &str =
    "offer_skipped_pre_gate_deadline";

/// Walker v3 Phase 3: /quote pre-gate skipped an offer because the
/// quote would exceed the per-dispatch `max_budget_credits` cap. Rare
/// — the static max_budget is also passed to Wire's /quote and surfaces
/// as `network_rate_above_budget` when Wire 409s — but Phase 3's
/// resolver-driven `max_budget_credits` comes from per-provider config
/// (not RouteEntry), so a pre-gate check avoids the HTTP round-trip
/// when the resolver is stricter than the RouteEntry default.
#[allow(dead_code)]
pub const EVENT_OFFER_SKIPPED_OVER_BUDGET: &str = "offer_skipped_over_budget";

#[allow(dead_code)]
pub const EVENT_DISPATCH_POLICY_SUPERSEDED: &str = "dispatch_policy_superseded";

// ── Walker v3 chronicle events ────────────────────────────────────────────
//
// 22 local-only events introduced by walker v3 (plan §5.4.6, rev 1.0.2).
// Declared in Phase 0a-1; emission sites land in later phases. Authoritative
// category split and total = 22; plan-integrity Check 9 counts §5.4.6 and
// verifies every "adds N events" / "all N new events" prose claim matches.

// Decision lifecycle (4)
#[allow(dead_code)]
pub const EVENT_DECISION_BUILT: &str = "decision_built";
#[allow(dead_code)]
pub const EVENT_DECISION_PREVIEWED: &str = "decision_previewed";
#[allow(dead_code)]
pub const EVENT_DECISION_BUILD_FAILED: &str = "decision_build_failed";
#[allow(dead_code)]
pub const EVENT_DISPATCH_FAILED_POLICY_BLOCKED: &str = "dispatch_failed_policy_blocked";

// Readiness & breaker (4)
#[allow(dead_code)]
pub const EVENT_PROVIDER_SKIPPED_READINESS: &str = "provider_skipped_readiness";
#[allow(dead_code)]
pub const EVENT_BREAKER_TRIPPED: &str = "breaker_tripped";
#[allow(dead_code)]
pub const EVENT_BREAKER_SKIPPED: &str = "breaker_skipped";
#[allow(dead_code)]
pub const EVENT_DISPATCH_EXHAUSTED: &str = "dispatch_exhausted";

// Config lifecycle (6)
#[allow(dead_code)]
pub const EVENT_CONFIG_SUPERSEDED: &str = "config_superseded";
#[allow(dead_code)]
pub const EVENT_CONFIG_RETRACTED: &str = "config_retracted";
#[allow(dead_code)]
pub const EVENT_CONFIG_RETRACTED_TO_BUNDLED: &str = "config_retracted_to_bundled";
#[allow(dead_code)]
pub const EVENT_RETRACTION_WALKED_DEEP: &str = "retraction_walked_deep";
#[allow(dead_code)]
pub const EVENT_SENSITIVE_SUPERSESSION_CONFIRMED: &str = "sensitive_supersession_confirmed";
#[allow(dead_code)]
pub const EVENT_CONFIG_SUPERSESSION_CONFLICT: &str = "config_supersession_conflict";

// Plan integrity / drift (3)
#[allow(dead_code)]
pub const EVENT_TIER_UNRESOLVED: &str = "tier_unresolved";
#[allow(dead_code)]
pub const EVENT_PREVIEW_VS_APPLY_DRIFT: &str = "preview_vs_apply_drift";
#[allow(dead_code)]
pub const EVENT_REQUESTER_PROVIDER_PARAM_DRIFT: &str = "requester_provider_param_drift";

// Infrastructure (5)
#[allow(dead_code)]
pub const EVENT_SCOPE_CACHE_LISTENER_RESTARTED: &str = "scope_cache_listener_restarted";
#[allow(dead_code)]
pub const EVENT_SCOPE_CACHE_QUARANTINED: &str = "scope_cache_quarantined";
#[allow(dead_code)]
pub const EVENT_BUNDLED_CONTRIBUTION_VALIDATION_FAILED: &str =
    "bundled_contribution_validation_failed";
#[allow(dead_code)]
pub const EVENT_V3_MIGRATION_SNAPSHOTS_PRUNED: &str = "v3_migration_snapshots_pruned";
#[allow(dead_code)]
pub const EVENT_FLEET_PEER_VERSION_SKEW: &str = "fleet_peer_version_skew";

// ── Event context ─────────────────────────────────────────────────────────

/// All context needed to record a chronicle event.
/// Constructed from StepContext (when available) + queue/dispatch metadata.
#[derive(Debug, Clone)]
pub struct ChronicleEventContext {
    pub job_path: String,
    pub event_type: String,
    pub timestamp: String,
    pub source: String,
    pub model_id: Option<String>,
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub task_label: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub work_item_id: Option<String>,
    pub attempt_id: Option<String>,
}

impl ChronicleEventContext {
    /// Build from a StepContext (local builds, fleet dispatches — rich context).
    /// Captures `chrono::Utc::now()` at construction time so the timestamp
    /// reflects actual event time, not async write execution time.
    pub fn from_step_ctx(
        ctx: &StepContext,
        job_path: &str,
        event_type: &str,
        source: &str,
    ) -> Self {
        Self {
            job_path: job_path.to_string(),
            event_type: event_type.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            model_id: ctx.resolved_model_id.clone(),
            slug: Some(ctx.slug.clone()),
            build_id: Some(ctx.build_id.clone()),
            chain_name: if ctx.chain_name.is_empty() {
                None
            } else {
                Some(ctx.chain_name.clone())
            },
            content_type: if ctx.content_type.is_empty() {
                None
            } else {
                Some(ctx.content_type.clone())
            },
            step_name: Some(ctx.step_name.clone()),
            primitive: Some(ctx.primitive.clone()),
            depth: Some(ctx.depth),
            task_label: if ctx.task_label.is_empty() {
                None
            } else {
                Some(ctx.task_label.clone())
            },
            metadata: None,
            work_item_id: None,
            attempt_id: None,
        }
    }

    /// Build with minimal context (fleet-received, market-received — no local build ctx).
    /// Captures `chrono::Utc::now()` at construction time.
    pub fn minimal(job_path: &str, event_type: &str, source: &str) -> Self {
        Self {
            job_path: job_path.to_string(),
            event_type: event_type.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            model_id: None,
            slug: None,
            build_id: None,
            chain_name: None,
            content_type: None,
            step_name: None,
            primitive: None,
            depth: None,
            task_label: None,
            metadata: None,
            work_item_id: None,
            attempt_id: None,
        }
    }

    pub fn with_model_id(mut self, model_id: String) -> Self {
        self.model_id = Some(model_id);
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_work_item(mut self, work_item_id: Option<String>, attempt_id: Option<String>) -> Self {
        self.work_item_id = work_item_id;
        self.attempt_id = attempt_id;
        self
    }
}

// ── Job path generation ───────────────────────────────────────────────────

/// Generate a semantic job_path from available context.
/// For DADBEAR work: uses entry.work_item_id (already a semantic path).
/// For non-DADBEAR work: derives from StepContext or queue entry metadata.
/// NO UUIDs — paths are human-readable and LLM-parseable.
pub fn generate_job_path(ctx: Option<&StepContext>, work_item_id: Option<&str>, model_id: &str, source: &str) -> String {
    // DADBEAR already set a semantic path
    if let Some(wid) = work_item_id {
        if !wid.is_empty() {
            return wid.to_string();
        }
    }
    // Derive from StepContext (local builds, stale checks)
    if let Some(c) = ctx {
        let build_short = if c.build_id.len() > 8 {
            &c.build_id[..8]
        } else {
            &c.build_id
        };
        return format!("{}:{}:{}:d{}", c.slug, build_short, c.step_name, c.depth);
    }
    // Fleet received (no ctx, no work_item_id)
    if source == "fleet_received" {
        let ts = chrono::Utc::now().timestamp();
        return format!("fleet-recv:{}:{}", model_id, ts);
    }
    // Market received (no ctx, no work_item_id — handler path fell back
    // to anon because the DADBEAR work item wasn't created upstream).
    // Parallel to fleet-recv branch. The canonical handler path (WS8
    // `handle_market_dispatch`) passes work_item_id `market/{job_id}`
    // and returns at the top of this function, so this branch is a
    // defensive fallback for call sites that emit market_received
    // without creating a DADBEAR work item first.
    if source == "market_received" {
        let ts = chrono::Utc::now().timestamp();
        return format!("market-recv:{}:{}", model_id, ts);
    }
    // Market source (SOURCE_MARKET) — offer publication, requester-side
    // events, and the Phase 3 `market_matched` event. Offer publication
    // uses job_path `market/offer/{model_id}` (WS8 wires this at the
    // call site via ChronicleEventContext::minimal, so this branch
    // also only runs as a defensive fallback).
    if source == "market" {
        let ts = chrono::Utc::now().timestamp();
        return format!("market:{}:{}", model_id, ts);
    }
    // Fallback: model + timestamp (always readable)
    let ts = chrono::Utc::now().timestamp();
    format!("anon:{}:{}", model_id, ts)
}

/// Generate a job_path from a QueueEntry (convenience wrapper).
pub fn generate_job_path_from_entry(ctx: Option<&StepContext>, entry: &QueueEntry) -> String {
    generate_job_path(ctx, entry.work_item_id.as_deref(), &entry.model_id, &entry.source)
}

// ── Record (append-only write) ────────────────────────────────────────────

/// Record a single compute event. Append-only — never updates or deletes.
/// Timestamp is passed explicitly from the context (captured at event site),
/// NOT relying on SQLite DEFAULT.
pub fn record_event(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_compute_events
            (job_path, event_type, timestamp, model_id, source, slug, build_id,
             chain_name, content_type, step_name, primitive, depth, task_label,
             metadata, work_item_id, attempt_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            ctx.job_path,
            ctx.event_type,
            ctx.timestamp,
            ctx.model_id,
            ctx.source,
            ctx.slug,
            ctx.build_id,
            ctx.chain_name,
            ctx.content_type,
            ctx.step_name,
            ctx.primitive,
            ctx.depth,
            ctx.task_label,
            ctx.metadata.as_ref().map(|m| m.to_string()),
            ctx.work_item_id,
            ctx.attempt_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Query types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComputeEvent {
    pub id: i64,
    pub job_path: String,
    pub event_type: String,
    pub timestamp: String,
    pub model_id: Option<String>,
    pub source: String,
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub task_label: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComputeSummary {
    pub group_key: String,
    pub total_events: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub total_latency_ms: i64,
    pub avg_latency_ms: f64,
    pub total_tokens_prompt: i64,
    pub total_tokens_completion: i64,
    pub total_cost_usd: f64,
    pub fleet_count: i64,
    pub local_count: i64,
    pub cloud_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimelineBucket {
    pub bucket_start: String,
    pub bucket_end: String,
    pub event_count: i64,
    pub completed_count: i64,
    pub avg_latency_ms: f64,
    pub total_cost_usd: f64,
    pub by_source: HashMap<String, i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChronicleDimensions {
    pub slugs: Vec<String>,
    pub models: Vec<String>,
    pub sources: Vec<String>,
    pub chain_names: Vec<String>,
    pub event_types: Vec<String>,
}

/// Query filters — every indexed column is a filterable dimension.
pub struct ChronicleQueryFilters {
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub model_id: Option<String>,
    pub source: Option<String>,
    pub event_type: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

// ── Query implementations ─────────────────────────────────────────────────

/// Query compute events with optional filters. Every field is a filterable
/// dimension — combine any subset for precise queries.
pub fn query_events(
    conn: &Connection,
    filters: &ChronicleQueryFilters,
) -> Result<Vec<ComputeEvent>> {
    let mut sql = String::from(
        "SELECT id, job_path, event_type, timestamp, model_id, slug,
                build_id, chain_name, content_type, step_name, primitive,
                depth, task_label, source, metadata
         FROM pyramid_compute_events WHERE 1=1",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    macro_rules! add_filter {
        ($field:expr, $col:expr) => {
            if let Some(ref val) = $field {
                sql.push_str(&format!(" AND {} = ?{}", $col, param_idx));
                param_values.push(Box::new(val.clone()));
                param_idx += 1;
            }
        };
    }

    add_filter!(filters.slug, "slug");
    add_filter!(filters.build_id, "build_id");
    add_filter!(filters.chain_name, "chain_name");
    add_filter!(filters.content_type, "content_type");
    add_filter!(filters.step_name, "step_name");
    add_filter!(filters.primitive, "primitive");
    add_filter!(filters.model_id, "model_id");
    add_filter!(filters.source, "source");
    add_filter!(filters.event_type, "event_type");

    if let Some(ref d) = filters.depth {
        sql.push_str(&format!(" AND depth = ?{}", param_idx));
        param_values.push(Box::new(*d));
        param_idx += 1;
    }
    if let Some(ref a) = filters.after {
        sql.push_str(&format!(" AND timestamp >= ?{}", param_idx));
        param_values.push(Box::new(a.clone()));
        param_idx += 1;
    }
    if let Some(ref b) = filters.before {
        sql.push_str(&format!(" AND timestamp <= ?{}", param_idx));
        param_values.push(Box::new(b.clone()));
        param_idx += 1;
    }

    sql.push_str(" ORDER BY timestamp DESC");
    sql.push_str(&format!(" LIMIT ?{} OFFSET ?{}", param_idx, param_idx + 1));
    param_values.push(Box::new(filters.limit));
    param_values.push(Box::new(filters.offset));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_refs.as_slice(), |r| {
        Ok(ComputeEvent {
            id: r.get("id")?,
            job_path: r.get("job_path")?,
            event_type: r.get("event_type")?,
            timestamp: r.get("timestamp")?,
            model_id: r.get("model_id")?,
            source: r.get("source")?,
            slug: r.get("slug")?,
            build_id: r.get("build_id")?,
            chain_name: r.get("chain_name")?,
            content_type: r.get("content_type")?,
            step_name: r.get("step_name")?,
            primitive: r.get("primitive")?,
            depth: r.get("depth")?,
            task_label: r.get("task_label")?,
            metadata: r
                .get::<_, Option<String>>("metadata")?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Summary query: aggregated stats for a time period grouped by a dimension.
pub fn query_summary(
    conn: &Connection,
    period_start: &str,
    period_end: &str,
    group_by: &str,
) -> Result<Vec<ComputeSummary>> {
    // Determine the SQL column to GROUP BY based on the requested dimension.
    let group_col = match group_by {
        "model" => "model_id",
        "source" => "source",
        "slug" => "slug",
        "hour" => "strftime('%Y-%m-%d %H:00:00', timestamp)",
        _ => "source", // safe fallback
    };

    let sql = format!(
        "SELECT
            COALESCE({group_col}, 'unknown') AS group_key,
            COUNT(*) AS total_events,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS completed_count,
            COUNT(CASE WHEN event_type = 'failed' THEN 1 END) AS failed_count,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS INTEGER) ELSE 0 END), 0) AS total_latency_ms,
            COALESCE(AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER) ELSE 0 END), 0) AS total_tokens_prompt,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER) ELSE 0 END), 0) AS total_tokens_completion,
            COALESCE(SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END), 0.0) AS total_cost_usd,
            COUNT(CASE WHEN source = 'fleet' THEN 1 END) AS fleet_count,
            COUNT(CASE WHEN source = 'local' THEN 1 END) AS local_count,
            COUNT(CASE WHEN source = 'cloud' THEN 1 END) AS cloud_count
         FROM pyramid_compute_events
         WHERE timestamp >= ?1 AND timestamp <= ?2
         GROUP BY {group_col}
         ORDER BY total_events DESC",
        group_col = group_col,
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![period_start, period_end], |r| {
        Ok(ComputeSummary {
            group_key: r.get("group_key")?,
            total_events: r.get("total_events")?,
            completed_count: r.get("completed_count")?,
            failed_count: r.get("failed_count")?,
            total_latency_ms: r.get("total_latency_ms")?,
            avg_latency_ms: r.get("avg_latency_ms")?,
            total_tokens_prompt: r.get("total_tokens_prompt")?,
            total_tokens_completion: r.get("total_tokens_completion")?,
            total_cost_usd: r.get("total_cost_usd")?,
            fleet_count: r.get("fleet_count")?,
            local_count: r.get("local_count")?,
            cloud_count: r.get("cloud_count")?,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Timeline query: bucketed event counts for visualization.
/// `bucket_size_minutes` is required — no hardcoded default (Pillar 37).
pub fn query_timeline(
    conn: &Connection,
    start: &str,
    end: &str,
    bucket_size_minutes: i64,
) -> Result<Vec<TimelineBucket>> {
    // Use strftime to bucket timestamps. SQLite doesn't have native bucket
    // functions, so we compute the bucket index from the Julian day offset.
    let bucket_secs = bucket_size_minutes * 60;

    let sql = format!(
        "SELECT
            strftime('%Y-%m-%dT%H:%M:%S', (CAST(strftime('%s', timestamp) AS INTEGER) / {bs}) * {bs}, 'unixepoch') AS bucket_start,
            strftime('%Y-%m-%dT%H:%M:%S', ((CAST(strftime('%s', timestamp) AS INTEGER) / {bs}) * {bs}) + {bs}, 'unixepoch') AS bucket_end,
            COUNT(*) AS event_count,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS completed_count,
            COALESCE(AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END), 0.0) AS total_cost_usd,
            COUNT(CASE WHEN source = 'local' THEN 1 END) AS local_count,
            COUNT(CASE WHEN source = 'fleet' THEN 1 END) AS fleet_count,
            COUNT(CASE WHEN source = 'cloud' THEN 1 END) AS cloud_count,
            COUNT(CASE WHEN source = 'fleet_received' THEN 1 END) AS fleet_received_count,
            COUNT(CASE WHEN source = 'market' THEN 1 END) AS market_count,
            COUNT(CASE WHEN source = 'market_received' THEN 1 END) AS market_received_count
         FROM pyramid_compute_events
         WHERE timestamp >= ?1 AND timestamp <= ?2
         GROUP BY bucket_start
         ORDER BY bucket_start ASC",
        bs = bucket_secs,
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![start, end], |r| {
        let mut by_source = HashMap::new();
        let local_count: i64 = r.get("local_count")?;
        let fleet_count: i64 = r.get("fleet_count")?;
        let cloud_count: i64 = r.get("cloud_count")?;
        let fleet_received_count: i64 = r.get("fleet_received_count")?;
        let market_count: i64 = r.get("market_count")?;
        let market_received_count: i64 = r.get("market_received_count")?;

        if local_count > 0 { by_source.insert("local".to_string(), local_count); }
        if fleet_count > 0 { by_source.insert("fleet".to_string(), fleet_count); }
        if cloud_count > 0 { by_source.insert("cloud".to_string(), cloud_count); }
        if fleet_received_count > 0 { by_source.insert("fleet_received".to_string(), fleet_received_count); }
        if market_count > 0 { by_source.insert("market".to_string(), market_count); }
        if market_received_count > 0 { by_source.insert("market_received".to_string(), market_received_count); }

        Ok(TimelineBucket {
            bucket_start: r.get("bucket_start")?,
            bucket_end: r.get("bucket_end")?,
            event_count: r.get("event_count")?,
            completed_count: r.get("completed_count")?,
            avg_latency_ms: r.get("avg_latency_ms")?,
            total_cost_usd: r.get("total_cost_usd")?,
            by_source,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// SELECT DISTINCT non-null values for a column (for filter dropdowns).
pub fn query_distinct(conn: &Connection, column: &str) -> Result<Vec<String>, String> {
    // Whitelist columns to prevent SQL injection
    let allowed = [
        "slug",
        "model_id",
        "source",
        "chain_name",
        "event_type",
        "step_name",
        "primitive",
    ];
    if !allowed.contains(&column) {
        return Err(format!("Invalid column for distinct query: {}", column));
    }
    let sql = format!(
        "SELECT DISTINCT {} FROM pyramid_compute_events WHERE {} IS NOT NULL ORDER BY {}",
        column, column, column
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Query all distinct dimension values for populating filter dropdowns.
pub fn query_distinct_dimensions(conn: &Connection) -> Result<ChronicleDimensions, String> {
    let slugs = query_distinct(conn, "slug")?;
    let models = query_distinct(conn, "model_id")?;
    let sources = query_distinct(conn, "source")?;
    let chain_names = query_distinct(conn, "chain_name")?;
    let event_types = query_distinct(conn, "event_type")?;
    Ok(ChronicleDimensions {
        slugs,
        models,
        sources,
        chain_names,
        event_types,
    })
}

// ── Market stubs ──────────────────────────────────────────────────────────

/// Stub: record market_matched event. Called from market exchange client (Phase 2).
pub fn record_market_matched(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_settled event. Called from settlement handler (Phase 3).
pub fn record_market_settled(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_received event. Called from market job handler (Phase 5).
pub fn record_market_received(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Phase 2 WS8 additions: market source/event constants + the
    //! `generate_job_path` branches that route `market_received` /
    //! `market` sources to semantic paths instead of the `anon:`
    //! fallback.
    //!
    //! The canonical handler path (`handle_market_dispatch`) ALWAYS
    //! populates `work_item_id` before emitting the chronicle event,
    //! so the fallback branches exercised below are defensive. They
    //! exist so a future emission site that forgets to thread the
    //! DADBEAR work item id gets a readable path instead of `anon:...`.
    use super::*;

    #[test]
    fn market_source_constants_match_spec() {
        // Pinned to the strings in compute-market-phase-2-exchange.md
        // §III "Chronicle Events This Phase" L603-632. A rename in
        // either direction requires a spec update.
        assert_eq!(SOURCE_MARKET, "market");
        assert_eq!(SOURCE_MARKET_RECEIVED, "market_received");
        assert_eq!(EVENT_MARKET_OFFERED, "market_offered");
        assert_eq!(EVENT_MARKET_RECEIVED, "market_received");
        assert_eq!(EVENT_MARKET_MATCHED, "market_matched");
        assert_eq!(EVENT_QUEUE_MIRROR_PUSH_FAILED, "queue_mirror_push_failed");
    }

    #[test]
    fn generate_job_path_market_received_fallback() {
        // No ctx, no work_item_id, source=market_received → semantic
        // path using model + timestamp. Parallel to fleet-recv.
        let path = generate_job_path(None, None, "llama-3", "market_received");
        assert!(path.starts_with("market-recv:llama-3:"));
        assert!(!path.starts_with("anon:"));
    }

    #[test]
    fn generate_job_path_market_source_fallback() {
        // No ctx, no work_item_id, source=market → semantic path using
        // model + timestamp.
        let path = generate_job_path(None, None, "llama-3", "market");
        assert!(path.starts_with("market:llama-3:"));
        assert!(!path.starts_with("anon:"));
    }

    #[test]
    fn generate_job_path_prefers_work_item_id_over_source_fallback() {
        // The canonical path: the caller passes the DADBEAR work item
        // id and `generate_job_path` returns it verbatim regardless
        // of source. This is the branch `handle_market_dispatch` hits
        // after it creates the `market/{job_id}` work item.
        let path = generate_job_path(
            None,
            Some("market/abc-123"),
            "llama-3",
            "market_received",
        );
        assert_eq!(path, "market/abc-123");
    }

    #[test]
    fn generate_job_path_fleet_received_still_works() {
        // Regression: adding market branches must not break the
        // fleet-received fallback.
        let path = generate_job_path(None, None, "llama-3", "fleet_received");
        assert!(path.starts_with("fleet-recv:llama-3:"));
    }

    #[test]
    fn generate_job_path_unknown_source_uses_anon_fallback() {
        // Unrecognized source still hits the anon: fallback so the
        // chronicle never has an empty job_path.
        let path = generate_job_path(None, None, "llama-3", "some-unknown-source");
        assert!(path.starts_with("anon:llama-3:"));
    }
}
