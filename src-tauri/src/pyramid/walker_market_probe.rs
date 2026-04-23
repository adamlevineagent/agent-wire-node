// Walker v3 — Market surface probe cache (Phase 3, plan rev 1.0.2 §2.6 + §3).
//
// Background:
//   The `ProviderReadiness::can_dispatch_now` trait is synchronous
//   (Decision builder runs from any context — walker_decision::build
//   holds no async runtime). Market readiness needs to know, per
//   (slot, model_list) pair:
//     * do any offers exist at all?
//     * are all known offers currently saturated
//       (MarketSurfaceOffer.current_queue_depth >= max_queue_depth
//       for every offer in the cohort)?
//     * do all offers point at OUR own node (self-dealing)?
//     * is the credit balance sufficient for a per-dispatch budget?
//     * is Wire reachable right now?
//
//   The answers come from async sources: `MarketSurfaceCache` (polled
//   `/api/v1/compute/market-surface` snapshot) + `/api/v1/credits/balance`
//   + network-failure counters. These are bridged by a module-local
//   sync cache: a background task (boot.rs step 7.6) refreshes the
//   cache on a cadence, and `MarketReadiness` reads the cached
//   snapshot without blocking.
//
// Design notes:
//   * OnceLock<Mutex<_>> cache — mirrors the precedent in
//     walker_ollama_probe.rs. Sync std Mutex because every reader is
//     in-memory and the ~millisecond-scale contention matters less
//     than dragging tokio into readiness code.
//   * Absent entry = "not yet probed". `MarketReadiness` treats this
//     conservatively — for per-model liquidity queries a missing
//     entry yields `NoMarketOffersForSlot`; for credit balance a
//     missing entry yields "assume sufficient" (opt-out by design —
//     cold cache MUST NOT block the only market path).
//   * The cache never stores "has the operator declared this model
//     for this slot" — that's a ResolvedProviderParams.model_list
//     question, resolved per-call by readiness. The cache only
//     projects what Wire said about the liquidity + our own balance.
//
// Integration:
//   boot.rs step 7.6 spawns `spawn_market_probe_task` once
//   `LlmConfig.market_surface_cache` is populated. The task ticks
//   at a configurable cadence (default 60s, aligned with Wire's
//   `Cache-Control: max-age=60`) and calls `refresh_*` helpers that
//   project the async `MarketSurfaceCache` snapshot into this sync
//   cache.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Per-model snapshot of market liquidity, projected from
/// `MarketSurfaceCache`. Written by the background task; read by
/// `MarketReadiness::can_dispatch_now`.
///
/// Three semantic shapes:
///   * `active_offers == 0`: Wire reports no offers for this model.
///     → NoMarketOffersForSlot
///   * `active_offers > 0 && all_offers_saturated`: every offer's
///     current_queue_depth >= max_queue_depth in the last snapshot.
///     → AllOffersSaturatedForModel
///   * `active_offers > 0 && !all_offers_saturated`: at least one
///     offer has headroom — readiness proceeds, /quote is the
///     authoritative viability check.
///
/// `only_self_offers` is independently set when every MarketSurfaceOffer
/// for this model has a (node_handle, operator_handle) matching this
/// node's identity. Self-dealing is a distinct gate from saturation —
/// a saturated self-deal is still a self-deal.
///
/// `offers_detail` carries the per-offer metadata needed by the
/// /quote pre-gate (§2.16 pre-gate formula). Populated only when the
/// most recent refresh fetched with `?model_id=X` (the detailed
/// projection) — the catalog refresh without `model_id` leaves it
/// empty since the catalog view omits the offers array.
#[derive(Debug, Clone)]
pub struct CachedMarketModel {
    pub active_offers: i64,
    pub all_offers_saturated: bool,
    pub only_self_offers: bool,
    /// Network-median typical serve time across offers for this model
    /// with observations (median of per-offer p50s). Walker pre-gate
    /// fallback when an offer's own typical_serve_ms_p50_7d is NULL.
    pub model_typical_serve_ms_p50_7d: Option<f64>,
    /// Per-offer projection for the /quote pre-gate. Empty when the
    /// last refresh used the catalog view.
    pub offers_detail: Vec<CachedOffer>,
    pub at: Instant,
}

/// Per-offer projection used by the /quote pre-gate and self-dealing
/// filter. Mirrors the fields of `MarketSurfaceOffer` relevant to
/// walker readiness; stays narrow so the sync cache doesn't fan out
/// into a full contract clone.
#[derive(Debug, Clone)]
pub struct CachedOffer {
    pub offer_id: String,
    pub node_handle: String,
    pub operator_handle: String,
    /// Walker pre-gate ingredient: per-offer serve-time p50. None
    /// when Wire has fewer than 10 successful observations for this
    /// (node, model); walker falls back to
    /// `model_typical_serve_ms_p50_7d` → skip pre-gate.
    pub typical_serve_ms_p50_7d: Option<f64>,
    /// Operator-declared engine concurrency. Used to compute
    /// `peer_queue_depth = current_queue_depth + execution_concurrency`
    /// for the pre-gate formula (rev 2.1.1 saturation-fix).
    pub execution_concurrency: i64,
    pub current_queue_depth: i64,
    pub max_queue_depth: i64,
}

impl CachedOffer {
    /// Pre-gate "peer queue depth" = buffer (current_queue_depth) +
    /// engine_occupancy (execution_concurrency). Per
    /// project_compute_market_saturation_fix.md rev 2.1.1.
    pub fn peer_queue_depth(&self) -> i64 {
        self.current_queue_depth.saturating_add(self.execution_concurrency)
    }

    /// True if the offer's queue is at or beyond `max_queue_depth`.
    /// Unbounded (`max_queue_depth == 0`) is treated as never saturated.
    pub fn is_saturated(&self) -> bool {
        self.max_queue_depth > 0 && self.current_queue_depth >= self.max_queue_depth
    }
}

/// Node-level snapshot — credit balance + Wire reachability counters.
/// Keyed as a singleton (there's only one "us" per process).
#[derive(Debug, Clone, Default)]
pub struct CachedNodeState {
    /// Most recent credit balance from Wire. `None` = never fetched
    /// OR fetch failed AND grace window expired. Readiness treats
    /// `None` as "can't confirm balance" — falls through to
    /// WireUnreachable if also out of grace, else no-op.
    pub credit_balance: Option<i64>,
    pub credit_balance_at: Option<Instant>,
    /// Consecutive failures of the most recent fetch path. Resets on
    /// success. Readiness returns WireUnreachable when this exceeds
    /// the resolver's `network_failure_backoff_threshold`.
    pub consecutive_network_failures: u32,
    pub last_network_success_at: Option<std::time::SystemTime>,
    /// Our own node_handle + operator_handle, snapshotted by the
    /// probe task at refresh time so readiness can filter self-deal
    /// offers synchronously. Empty strings when not yet known.
    pub self_node_handle: String,
    pub self_operator_handle: String,
}

type ModelMap = Mutex<HashMap<String, CachedMarketModel>>;
type NodeState = Mutex<CachedNodeState>;

static MARKET_PROBE_CACHE: OnceLock<ModelMap> = OnceLock::new();
static NODE_STATE_CACHE: OnceLock<NodeState> = OnceLock::new();

fn model_cache() -> &'static ModelMap {
    MARKET_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn node_cache() -> &'static NodeState {
    NODE_STATE_CACHE.get_or_init(|| Mutex::new(CachedNodeState::default()))
}

// ── Per-model API ──────────────────────────────────────────────────────────

/// Look up the cached market-surface projection for `model_id`. Returns
/// `None` when the model has never been probed.
pub fn read_cached_model(model_id: &str) -> Option<CachedMarketModel> {
    let guard = model_cache().lock().ok()?;
    guard.get(model_id).cloned()
}

/// Overwrite the cached entry for `model_id`. Called by the background
/// task after every refresh and by tests.
pub fn write_cached_model(model_id: &str, entry: CachedMarketModel) {
    if let Ok(mut guard) = model_cache().lock() {
        guard.insert(model_id.to_string(), entry);
    }
}

/// Drop the cached entry for `model_id`. Used by tests; production
/// overwrites via `write_cached_model`.
#[allow(dead_code)]
pub fn invalidate_cached_model(model_id: &str) {
    if let Ok(mut guard) = model_cache().lock() {
        guard.remove(model_id);
    }
}

/// Test-only: wipe the entire model cache. Exposed for integration
/// tests too (cargo gates `#[cfg(test)]` away from the `tests/` dir);
/// production code MUST NOT call it.
#[allow(dead_code)]
pub fn clear_model_cache_for_tests() {
    if let Ok(mut guard) = model_cache().lock() {
        guard.clear();
    }
}

// ── Node-state API ─────────────────────────────────────────────────────────

/// Read the current node-state snapshot.
pub fn read_node_state() -> CachedNodeState {
    node_cache()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Overwrite the whole node-state snapshot. Called by the background
/// task after each tick.
pub fn write_node_state(state: CachedNodeState) {
    if let Ok(mut guard) = node_cache().lock() {
        *guard = state;
    }
}

/// Increment the consecutive-failure counter; return the new value.
pub fn record_network_failure() -> u32 {
    if let Ok(mut guard) = node_cache().lock() {
        guard.consecutive_network_failures =
            guard.consecutive_network_failures.saturating_add(1);
        return guard.consecutive_network_failures;
    }
    0
}

/// Record a network success — reset the failure counter and stamp
/// `last_network_success_at`.
pub fn record_network_success() {
    if let Ok(mut guard) = node_cache().lock() {
        guard.consecutive_network_failures = 0;
        guard.last_network_success_at = Some(std::time::SystemTime::now());
    }
}

/// Update the node's own handles (from AuthState). Safe to call
/// repeatedly; the latest snapshot wins.
pub fn set_self_handles(node_handle: &str, operator_handle: &str) {
    if let Ok(mut guard) = node_cache().lock() {
        guard.self_node_handle = node_handle.to_string();
        guard.self_operator_handle = operator_handle.to_string();
    }
}

/// Update the credit-balance snapshot.
pub fn set_credit_balance(balance: Option<i64>) {
    if let Ok(mut guard) = node_cache().lock() {
        guard.credit_balance = balance;
        guard.credit_balance_at = Some(Instant::now());
    }
}

/// Test-only: wipe the node-state snapshot. Exposed for integration
/// tests too (cargo gates `#[cfg(test)]` away from the `tests/` dir);
/// production code MUST NOT call it.
#[allow(dead_code)]
pub fn clear_node_state_for_tests() {
    if let Ok(mut guard) = node_cache().lock() {
        *guard = CachedNodeState::default();
    }
}

/// Test-only: shared serialization lock for tests that mutate
/// `node_state_cache`. Any test that calls
/// `clear_node_state_for_tests` / `set_credit_balance` /
/// `set_self_handles` / `record_network_failure` should acquire this
/// lock first to avoid racing with a parallel sibling test.
///
/// Poisoned guards recover to the inner state — a prior panicking test
/// shouldn't break the current one. Production code MUST NOT call it.
#[allow(dead_code)]
pub fn node_state_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

// ── /quote pre-gate (§3 + saturation-fix) ──────────────────────────────────
//
// Pre-gate formula (project_compute_market_saturation_fix.md rev 2.1.1):
//
//     if typical_serve_ms_p50_7d * peer_queue_depth
//          > (dispatch_deadline - dispatch_deadline_grace_secs * 1000)
//     then skip (offer can't meet the deadline)
//
// Where:
//   * typical_serve_ms_p50_7d: per-offer metadata (from the surface
//     cache); falls back to model-level when per-offer is NULL; both
//     NULL → skip the pre-gate entirely (trust the static deadline).
//   * peer_queue_depth = current_queue_depth + execution_concurrency
//     (buffer + engine_occupancy; `CachedOffer::peer_queue_depth`).
//   * dispatch_deadline: walker-supplied static deadline. Load-bearing
//     economic contract — the pre-gate DETECTS unviability, never
//     stretches the deadline.
//   * dispatch_deadline_grace_secs: resolver-supplied grace. Same value
//     used for the /fill-await safety rail. Both use the same grace
//     intentionally — "the window in which we trust our clocks match
//     Wire's" is a single concept.

/// Pre-gate verdict for a single offer. Pure function; no I/O.
#[derive(Debug, Clone, PartialEq)]
pub enum PreGateVerdict {
    /// Offer may meet the deadline — continue to /quote.
    Proceed,
    /// Offer can't meet the deadline even in the best case — skip
    /// without paying a reservation fee. `estimated_serve_ms` is the
    /// computed `typical_serve_ms_p50_7d × peer_queue_depth` product
    /// so the chronicle event can record why.
    Skip { estimated_serve_ms: u64, usable_deadline_ms: u64 },
    /// Neither per-offer nor model-level typical_serve_ms_p50_7d is
    /// populated. Walker trusts the static deadline; do not pre-gate.
    /// (Equivalent to Proceed for control flow — separate variant so
    /// chronicle emission can tag this path distinctly if useful.)
    Indeterminate,
}

/// Decide whether an offer should be skipped pre-/quote. Pure: no I/O.
///
/// * `dispatch_deadline_ms`: full wall-clock budget until Wire's
///   dispatch_deadline, in milliseconds.
/// * `dispatch_deadline_grace_secs`: resolver-supplied grace, in
///   seconds. Subtracted from the deadline to leave room for clock
///   skew + transit.
/// * `offer`: the CachedOffer projection from the surface cache.
/// * `model_typical_serve_ms_p50_7d`: fallback when per-offer is NULL.
pub fn evaluate_pre_gate(
    dispatch_deadline_ms: u64,
    dispatch_deadline_grace_secs: u64,
    offer: &CachedOffer,
    model_typical_serve_ms_p50_7d: Option<f64>,
) -> PreGateVerdict {
    // Three-tier serve-time lookup: offer.p50 → model.p50 → skip gate.
    let serve_ms = match offer
        .typical_serve_ms_p50_7d
        .or(model_typical_serve_ms_p50_7d)
    {
        Some(ms) if ms > 0.0 => ms,
        _ => return PreGateVerdict::Indeterminate,
    };

    // Usable deadline = full budget minus grace. Saturating math keeps
    // the formula defined even when grace > deadline (in which case
    // usable is 0 and any nonzero serve time skips).
    let grace_ms = dispatch_deadline_grace_secs.saturating_mul(1_000);
    let usable_deadline_ms = dispatch_deadline_ms.saturating_sub(grace_ms);

    let peer_depth = offer.peer_queue_depth().max(0) as f64;
    let estimated_ms = (serve_ms * peer_depth).max(0.0) as u64;

    if estimated_ms > usable_deadline_ms {
        PreGateVerdict::Skip {
            estimated_serve_ms: estimated_ms,
            usable_deadline_ms,
        }
    } else {
        PreGateVerdict::Proceed
    }
}

// ── Projection helpers ─────────────────────────────────────────────────────

/// Project a `MarketSurfaceModel` + `Vec<MarketSurfaceOffer>` (when
/// available from a `?model_id=X` fetch) into a `CachedMarketModel`.
/// The self-handles arg comes from the node-state cache so the
/// projection is a pure sync operation.
pub fn project_model(
    model: &agent_wire_contracts::MarketSurfaceModel,
    self_node_handle: &str,
    self_operator_handle: &str,
) -> CachedMarketModel {
    let offers_detail: Vec<CachedOffer> = model
        .offers
        .as_ref()
        .map(|list| {
            list.iter()
                .map(|o| CachedOffer {
                    offer_id: o.offer_id.clone(),
                    node_handle: o.node_handle.clone(),
                    operator_handle: o.operator_handle.clone(),
                    typical_serve_ms_p50_7d: o.typical_serve_ms_p50_7d,
                    execution_concurrency: o.execution_concurrency,
                    current_queue_depth: o.current_queue_depth,
                    max_queue_depth: o.max_queue_depth,
                })
                .collect()
        })
        .unwrap_or_default();

    // Saturation = every offer is saturated AND at least one offer
    // exists. Zero offers → not "all saturated", just "none at all"
    // (distinct semantic via active_offers).
    let all_offers_saturated = !offers_detail.is_empty()
        && offers_detail.iter().all(CachedOffer::is_saturated);

    // Self-deal = every offer points at us. Ignore empty offer lists
    // (can't self-deal when nothing is published).
    let only_self_offers = !offers_detail.is_empty()
        && !self_node_handle.is_empty()
        && offers_detail.iter().all(|o| {
            o.node_handle == self_node_handle
                && (self_operator_handle.is_empty()
                    || o.operator_handle == self_operator_handle)
        });

    CachedMarketModel {
        active_offers: model.active_offers,
        all_offers_saturated,
        only_self_offers,
        model_typical_serve_ms_p50_7d: model.model_typical_serve_ms_p50_7d,
        offers_detail,
        at: Instant::now(),
    }
}

/// Refresh the sync cache from an async `MarketSurfaceCache` snapshot.
/// Walks every model the snapshot knows about and writes a
/// `CachedMarketModel`. Cheap (in-memory projection only). Called by
/// the boot.rs background task.
///
/// The catalog refresh does NOT populate `offers_detail`; for walker
/// pre-gate ingredients the caller must hit `?model_id=X` for each
/// model of interest and feed the result through `project_model`.
pub async fn refresh_from_surface_cache(
    surface: &crate::pyramid::market_surface_cache::MarketSurfaceCache,
) {
    // Snapshot per-offer-less projection for every known model.
    let snapshot = surface.snapshot_ui_models().await;
    let state = read_node_state();
    // UI snapshot doesn't give us the full model; use get_model for
    // each. This is O(n_models) reads on the async cache's RwLock but
    // all reads are cheap clones.
    for row in snapshot {
        if let Some(model) = surface.get_model(&row.model_id).await {
            let projected =
                project_model(&model, &state.self_node_handle, &state.self_operator_handle);
            write_cached_model(&row.model_id, projected);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wire_contracts::{
        MarketSurfaceModel, MarketSurfaceOffer, MarketSurfaceProviderType,
    };

    fn make_offer(
        offer_id: &str,
        node_handle: &str,
        operator_handle: &str,
        current_queue_depth: i64,
        max_queue_depth: i64,
        typical_serve: Option<f64>,
        execution_concurrency: i64,
    ) -> MarketSurfaceOffer {
        MarketSurfaceOffer {
            offer_id: offer_id.into(),
            operator_handle: operator_handle.into(),
            node_handle: node_handle.into(),
            provider_type: MarketSurfaceProviderType::Local,
            rate_per_m_input: 100,
            rate_per_m_output: 200,
            reservation_fee: 0,
            queue_discount_curve: vec![],
            current_queue_depth,
            max_queue_depth,
            max_tokens_supported: 4096,
            observed_median_tps_7d: None,
            observed_p95_latency_ms_7d: None,
            observed_success_rate_7d: None,
            observed_job_count_7d: 0,
            last_heartbeat_at: None,
            operator_reputation_compute: None,
            typical_serve_ms_p50_7d: typical_serve,
            execution_concurrency,
        }
    }

    fn make_model(
        model_id: &str,
        active_offers: i64,
        offers: Option<Vec<MarketSurfaceOffer>>,
        model_serve_p50: Option<f64>,
    ) -> MarketSurfaceModel {
        serde_json::from_value(serde_json::json!({
            "model_id": model_id,
            "provider_count": 1,
            "active_offers": active_offers,
            "price": {
                "rate_per_m_input": { "min": null, "median": null, "max": null },
                "rate_per_m_output": { "min": null, "median": null, "max": null },
            },
            "queue": { "total_capacity": 0, "current_depth": 0, "unbounded_offers": 0 },
            "performance": {
                "p50_latency_ms": null, "p95_latency_ms": null,
                "median_tps": null, "success_rate_7d": null,
            },
            "top_of_book": { "cheapest_with_headroom": null },
            "demand_24h": { "jobs_matched": 0, "jobs_settled": 0, "queue_fill_events": 0 },
            "last_offer_update_at": null,
            "model_typical_serve_ms_p50_7d": model_serve_p50,
            "offers": offers,
        }))
        .expect("model fixture shape")
    }

    #[test]
    fn project_model_zero_offers_yields_none_not_saturated_not_self() {
        let m = make_model("foo/bar", 0, None, None);
        let p = project_model(&m, "me-handle", "me-op");
        assert_eq!(p.active_offers, 0);
        assert!(!p.all_offers_saturated);
        assert!(!p.only_self_offers);
        assert!(p.offers_detail.is_empty());
    }

    #[test]
    fn project_model_all_saturated_flags_saturated() {
        let offers = vec![
            make_offer("o1", "other-node", "other-op", 5, 5, Some(1000.0), 1),
            make_offer("o2", "other-node2", "other-op", 10, 10, Some(2000.0), 2),
        ];
        let m = make_model("foo/bar", 2, Some(offers), Some(1500.0));
        let p = project_model(&m, "me-handle", "me-op");
        assert!(p.all_offers_saturated);
        assert!(!p.only_self_offers);
        assert_eq!(p.offers_detail.len(), 2);
    }

    #[test]
    fn project_model_partial_saturation_is_not_all_saturated() {
        let offers = vec![
            make_offer("o1", "a", "op", 5, 5, None, 1),       // saturated
            make_offer("o2", "b", "op", 1, 10, None, 1),      // has headroom
        ];
        let m = make_model("foo/bar", 2, Some(offers), None);
        let p = project_model(&m, "me", "meop");
        assert!(!p.all_offers_saturated);
    }

    #[test]
    fn project_model_self_dealing_filters_all_own_offers() {
        let offers = vec![
            make_offer("o1", "my-node", "my-op", 1, 10, None, 1),
            make_offer("o2", "my-node", "my-op", 2, 10, None, 1),
        ];
        let m = make_model("foo/bar", 2, Some(offers), None);
        let p = project_model(&m, "my-node", "my-op");
        assert!(p.only_self_offers);
    }

    #[test]
    fn project_model_mixed_own_and_others_is_not_self_dealing() {
        let offers = vec![
            make_offer("o1", "my-node", "my-op", 1, 10, None, 1),
            make_offer("o2", "other-node", "my-op", 2, 10, None, 1),
        ];
        let m = make_model("foo/bar", 2, Some(offers), None);
        let p = project_model(&m, "my-node", "my-op");
        assert!(!p.only_self_offers);
    }

    #[test]
    fn project_model_unbounded_queue_is_never_saturated() {
        // max_queue_depth=0 means unbounded per contract. An offer with
        // current_queue_depth=100 and max=0 must not register as saturated.
        let offers = vec![make_offer("o1", "x", "y", 100, 0, None, 1)];
        let m = make_model("foo/bar", 1, Some(offers), None);
        let p = project_model(&m, "me", "meop");
        assert!(!p.all_offers_saturated);
    }

    #[test]
    fn peer_queue_depth_sums_buffer_and_engine_occupancy() {
        let o = make_offer("o", "x", "y", 7, 20, Some(1000.0), 3);
        let cached = CachedOffer {
            offer_id: o.offer_id.clone(),
            node_handle: o.node_handle.clone(),
            operator_handle: o.operator_handle.clone(),
            typical_serve_ms_p50_7d: o.typical_serve_ms_p50_7d,
            execution_concurrency: o.execution_concurrency,
            current_queue_depth: o.current_queue_depth,
            max_queue_depth: o.max_queue_depth,
        };
        assert_eq!(cached.peer_queue_depth(), 10);
    }

    #[test]
    fn write_then_read_roundtrips_model_cache() {
        clear_model_cache_for_tests();
        let model = CachedMarketModel {
            active_offers: 3,
            all_offers_saturated: false,
            only_self_offers: false,
            model_typical_serve_ms_p50_7d: Some(2000.0),
            offers_detail: vec![],
            at: Instant::now(),
        };
        write_cached_model("foo/bar", model);
        let got = read_cached_model("foo/bar").expect("must be present");
        assert_eq!(got.active_offers, 3);
        invalidate_cached_model("foo/bar");
        assert!(read_cached_model("foo/bar").is_none());
    }

    // Node-state tests race on a shared global cache under cargo's
    // parallel test runner. Use the shared module-level
    // `node_state_test_lock` so sibling tests in walker_readiness +
    // walker_decision (and integration tests) see the same ordering.
    fn probe_test_lock() -> &'static std::sync::Mutex<()> {
        super::node_state_test_lock()
    }

    #[test]
    fn node_state_network_failure_counter_increments_and_resets() {
        let _g = probe_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        assert_eq!(record_network_failure(), 1);
        assert_eq!(record_network_failure(), 2);
        record_network_success();
        let s = read_node_state();
        assert_eq!(s.consecutive_network_failures, 0);
        assert!(s.last_network_success_at.is_some());
    }

    #[test]
    fn node_state_self_handles_stored() {
        let _g = probe_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        set_self_handles("my-node", "my-op");
        let s = read_node_state();
        assert_eq!(s.self_node_handle, "my-node");
        assert_eq!(s.self_operator_handle, "my-op");
    }

    // ── /quote pre-gate tests ────────────────────────────────────────────

    fn mk_offer(
        typical_serve: Option<f64>,
        current_queue_depth: i64,
        execution_concurrency: i64,
    ) -> CachedOffer {
        CachedOffer {
            offer_id: "o".into(),
            node_handle: "n".into(),
            operator_handle: "op".into(),
            typical_serve_ms_p50_7d: typical_serve,
            execution_concurrency,
            current_queue_depth,
            max_queue_depth: 100,
        }
    }

    #[test]
    fn test_pre_gate_skips_when_serve_time_exceeds_deadline() {
        // typical_serve_ms = 30_000, peer_queue_depth = 3+2 = 5,
        // estimated = 150_000 ms. Deadline = 60_000 ms (60s), grace 10s
        // → usable = 50_000. 150_000 > 50_000 → Skip.
        let offer = mk_offer(Some(30_000.0), 3, 2);
        let verdict = evaluate_pre_gate(60_000, 10, &offer, None);
        match verdict {
            PreGateVerdict::Skip { estimated_serve_ms, usable_deadline_ms } => {
                assert_eq!(estimated_serve_ms, 150_000);
                assert_eq!(usable_deadline_ms, 50_000);
            }
            other => panic!("expected Skip, got {:?}", other),
        }
    }

    #[test]
    fn test_pre_gate_accepts_when_serve_time_within_deadline() {
        // typical_serve_ms = 5_000, peer_queue_depth = 2+1 = 3
        // estimated = 15_000 ms. Deadline = 60_000, grace 10s → usable
        // 50_000. 15_000 <= 50_000 → Proceed.
        let offer = mk_offer(Some(5_000.0), 2, 1);
        let verdict = evaluate_pre_gate(60_000, 10, &offer, None);
        assert_eq!(verdict, PreGateVerdict::Proceed);
    }

    #[test]
    fn test_pre_gate_honors_dispatch_deadline_grace_secs() {
        // Same offer, two different grace values. With grace=0 the full
        // 60s is usable → Proceed (estimated 50_000 <= 60_000). With
        // grace=20s only 40_000 is usable → Skip.
        let offer = mk_offer(Some(10_000.0), 4, 1); // peer_depth=5 → est=50_000
        assert_eq!(
            evaluate_pre_gate(60_000, 0, &offer, None),
            PreGateVerdict::Proceed
        );
        match evaluate_pre_gate(60_000, 20, &offer, None) {
            PreGateVerdict::Skip { .. } => {}
            other => panic!("expected Skip with large grace, got {:?}", other),
        }
    }

    #[test]
    fn test_pre_gate_falls_back_to_model_p50_when_offer_is_null() {
        // Per-offer p50 None; model-level provided. Fallback path.
        let offer = mk_offer(None, 5, 1); // peer_depth = 6
        // Model p50 = 5_000 → estimated = 30_000 → Proceed at 60s/10s grace
        // (usable=50_000).
        let verdict = evaluate_pre_gate(60_000, 10, &offer, Some(5_000.0));
        assert_eq!(verdict, PreGateVerdict::Proceed);
    }

    #[test]
    fn test_pre_gate_indeterminate_when_both_p50_null() {
        let offer = mk_offer(None, 5, 1);
        assert_eq!(
            evaluate_pre_gate(60_000, 10, &offer, None),
            PreGateVerdict::Indeterminate
        );
    }

    #[test]
    fn test_pre_gate_indeterminate_when_p50_is_zero() {
        // Zero is a degenerate value; treat as "no observations" per
        // the same semantic as None (would otherwise trivially pass).
        let offer = mk_offer(Some(0.0), 5, 1);
        assert_eq!(
            evaluate_pre_gate(60_000, 10, &offer, None),
            PreGateVerdict::Indeterminate
        );
    }

    #[test]
    fn test_node_state_credit_balance_snapshots() {
        let _g = probe_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        set_credit_balance(Some(1_000_000));
        let s = read_node_state();
        assert_eq!(s.credit_balance, Some(1_000_000));
        assert!(s.credit_balance_at.is_some());

        // Clearing: set None.
        set_credit_balance(None);
        let s2 = read_node_state();
        assert_eq!(s2.credit_balance, None);
    }
}
