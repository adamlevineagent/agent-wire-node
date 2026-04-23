// Walker v3 — per-build circuit breaker (Phase 5, plan rev 1.0.2 §2.16.6).
//
// Plan anchors:
//   §2.16.6  per-build circuit-breaker semantics: state keys on
//            (build_id, slot, provider_type). Per-build is the default;
//            `probe_based` and `time_secs:N` variants clear the tripped
//            flag differently. DADBEAR's long-lived maintenance build
//            uses time-bucketed sub-build-ids, so the effective key
//            becomes (parent_build_id:bucket, slot, provider_type).
//   §2.18    BreakerState HashMap is in-memory-only; ephemeral per-build
//            runtime state, rehydrate from empty at boot.
//   §3       `breaker_reset` parameter catalog row + SYSTEM_DEFAULT
//            `per_build`.
//   §5.4.6   `breaker_tripped` local-only chronicle event declared
//            in compute_chronicle.rs.
//   §6 Phase 5  body: ships `PerBuild` + `TimeSecs` fully; scaffolds
//            `ProbeBased` with a TODO for the future phase that wires
//            probe-success signals from each provider.
//
// Storage design:
//   OnceLock<Mutex<HashMap<BreakerKey, BreakerState>>>.
//   Pattern mirrors the probe caches (walker_ollama_probe /
//   walker_market_probe / walker_fleet_probe) — sync std Mutex because
//   readers/writers are tiny in-memory ops with nanosecond contention.
//   Async-lock frameworks would add unnecessary weight for a field
//   update + comparison.
//
// Trip threshold: `TRIP_THRESHOLD = 3` consecutive failures. Hardcoded
// in Phase 5 per plan §6. Future phases can promote this to a §3
// parameter with SYSTEM_DEFAULT + resolver wiring if operators need to
// tune it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::pyramid::walker_resolver::{BreakerReset, ProviderType};

/// Three consecutive failures trips the breaker. Phase 5 ships this as
/// a constant; §3 promotion lives in a future phase when an operator
/// surface is genuinely required.
pub const TRIP_THRESHOLD: u32 = 3;

/// Composite key identifying a single breaker cell.
///
/// `build_id` is the StepContext-scoped build id (NOT the apex node id).
/// DADBEAR bucket-rotated ids (e.g. `dadbear-maint:bucket_458212`)
/// funnel through here too — the key just treats them as opaque strings.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct BreakerKey {
    pub build_id: String,
    pub slot: String,
    pub provider_type: ProviderType,
}

impl BreakerKey {
    pub fn new(
        build_id: impl Into<String>,
        slot: impl Into<String>,
        provider_type: ProviderType,
    ) -> Self {
        Self {
            build_id: build_id.into(),
            slot: slot.into(),
            provider_type,
        }
    }
}

/// Per-cell breaker state. `tripped` flips true once
/// `consecutive_failures >= TRIP_THRESHOLD`; reset policy decides if
/// it ever untrips.
#[derive(Debug, Clone)]
pub struct BreakerState {
    /// Running count. Reset to 0 on `record_success`.
    pub consecutive_failures: u32,
    /// Set on every `record_failure`. Consulted by the TimeSecs reset
    /// variant and for chronicle payload visibility.
    pub last_failure_at: Option<Instant>,
    /// Once true, `is_tripped` returns true regardless of
    /// `consecutive_failures` — UNLESS the reset policy untrips it.
    pub tripped: bool,
}

impl Default for BreakerState {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_at: None,
            tripped: false,
        }
    }
}

// ── Storage ──────────────────────────────────────────────────────────────────

type BreakerMap = Mutex<HashMap<BreakerKey, BreakerState>>;

static BREAKER_STATE: OnceLock<BreakerMap> = OnceLock::new();

fn map_handle() -> &'static BreakerMap {
    BREAKER_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Lifecycle helpers ────────────────────────────────────────────────────────

/// Record a genuine dispatch failure for this (build, slot, provider).
///
/// A "genuine" failure is one that should count against the breaker:
/// NOT saturation (market offers full — transient), NOT deadline
/// exceeded (economic contract timer, not a provider fault), NOT a
/// policy-block (operator chose fail_loud). Call sites in llm.rs /
/// chain_dispatch.rs filter before invoking this helper.
///
/// The increment is monotonic; reaching `TRIP_THRESHOLD` sets
/// `tripped = true`. `tripped` stays true for PerBuild — only reset
/// policies that CAN untrip (TimeSecs, ProbeBased) observe the clock.
#[allow(dead_code)]
pub fn record_failure(build_id: &str, slot: &str, provider_type: ProviderType) {
    let key = BreakerKey::new(build_id, slot, provider_type);
    if let Ok(mut guard) = map_handle().lock() {
        let entry = guard.entry(key).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.last_failure_at = Some(Instant::now());
        if entry.consecutive_failures >= TRIP_THRESHOLD {
            entry.tripped = true;
        }
    }
}

/// Record a successful dispatch for this (build, slot, provider).
///
/// Resets `consecutive_failures` to 0 so the next failure starts from
/// a clean slate. Does NOT clear the `tripped` flag — per §2.16.6,
/// `PerBuild` stays tripped for the build lifetime; `TimeSecs` untrips
/// on its own clock (`is_tripped` handles that); `ProbeBased` untrip
/// is scaffolded (see `note_probe_success`). A provider that was
/// tripped for part of a build and later recovers leaves a tripped
/// cell behind — the cell never actually re-entered the call order
/// (Decision builder filtered it out), so a "success" from the same
/// (build, slot, provider) tuple under PerBuild is impossible by
/// construction.
#[allow(dead_code)]
pub fn record_success(build_id: &str, slot: &str, provider_type: ProviderType) {
    let key = BreakerKey::new(build_id, slot, provider_type);
    if let Ok(mut guard) = map_handle().lock() {
        if let Some(entry) = guard.get_mut(&key) {
            entry.consecutive_failures = 0;
        }
    }
}

/// Is this (build, slot, provider) currently breaker-tripped?
///
/// Consults `BreakerState.tripped` and applies the `breaker_reset`
/// policy:
///   - `PerBuild`: tripped is absolute for the build lifetime;
///     `clear_build` is the only reset.
///   - `TimeSecs { value }`: tripped untrips after `value` seconds
///     elapsed since the last failure. We mutate the stored state
///     so subsequent reads and chronicle payloads reflect the untrip.
///   - `ProbeBased`: scaffold. Phase 5 returns the stored `tripped`
///     value unchanged. TODO(future phase): wire probe-success signals
///     from each provider to `note_probe_success` which clears tripped.
///
/// Absent entry = not tripped. A provider is tripped only after at
/// least one recorded failure AND crossing the threshold.
#[allow(dead_code)]
pub fn is_tripped(
    build_id: &str,
    slot: &str,
    provider_type: ProviderType,
    reset: BreakerReset,
) -> bool {
    let key = BreakerKey::new(build_id, slot, provider_type);
    let Ok(mut guard) = map_handle().lock() else {
        return false;
    };
    let Some(entry) = guard.get_mut(&key) else {
        return false;
    };
    if !entry.tripped {
        return false;
    }
    match reset {
        BreakerReset::PerBuild => true,
        BreakerReset::TimeSecs { value } => {
            // If the last failure is older than `value` seconds, untrip
            // and reset counter. Mutation is deliberate: a subsequent
            // call returns false without re-computing elapsed.
            let Some(last) = entry.last_failure_at else {
                return true;
            };
            if last.elapsed().as_secs() >= value {
                entry.tripped = false;
                entry.consecutive_failures = 0;
                false
            } else {
                true
            }
        }
        BreakerReset::ProbeBased => {
            // Phase 5 scaffold: probe-success signals are not wired yet.
            // Return stored state unchanged — the cell stays tripped
            // until a future phase calls `note_probe_success` from the
            // provider's health-probe path.
            //
            // TODO(walker-v3 phase-6+): wire Ollama /api/tags probe
            // success (local), /quote-against-different-offer success
            // (market), and peer heartbeat success (fleet) into
            // `note_probe_success` so this branch untrips.
            true
        }
    }
}

/// Scaffold entry point for `BreakerReset::ProbeBased` untrip.
///
/// Probe-success signals from each provider should call this once the
/// provider's health probe returns a fresh Ready. Phase 5 ships the
/// helper + the call-site TODOs; wiring is future work.
#[allow(dead_code)]
pub fn note_probe_success(build_id: &str, slot: &str, provider_type: ProviderType) {
    let key = BreakerKey::new(build_id, slot, provider_type);
    if let Ok(mut guard) = map_handle().lock() {
        if let Some(entry) = guard.get_mut(&key) {
            entry.tripped = false;
            entry.consecutive_failures = 0;
        }
    }
}

/// Drop every breaker cell keyed on `build_id`. Called when a build
/// transitions to a terminal status (completed / failed / cancelled)
/// so the HashMap doesn't grow unbounded across a process lifetime.
///
/// Idempotent: clearing a build_id with no entries is a no-op. Keys
/// not matching are left alone (multi-build-concurrent safety).
#[allow(dead_code)]
pub fn clear_build(build_id: &str) {
    if let Ok(mut guard) = map_handle().lock() {
        guard.retain(|k, _| k.build_id != build_id);
    }
}

/// Read-only snapshot for observability / tests. Returns `None` when
/// no entry exists for the key.
#[allow(dead_code)]
pub fn peek_state(build_id: &str, slot: &str, provider_type: ProviderType) -> Option<BreakerState> {
    let key = BreakerKey::new(build_id, slot, provider_type);
    map_handle().lock().ok()?.get(&key).cloned()
}

/// Test-only: wipe the entire breaker map so adjacent tests don't see
/// each other's writes. Production code MUST NOT call this.
///
/// Not `#[cfg(test)]` because integration tests (`tests/*.rs`) need
/// this helper and would otherwise fail to link against it. The
/// helper is still harmless in production binaries: documented
/// contract is "only call from tests," and the function is a
/// straightforward `HashMap::clear`.
#[allow(dead_code)]
pub fn clear_all_for_tests() {
    if let Ok(mut guard) = map_handle().lock() {
        guard.clear();
    }
}

/// Test-only: shared serialization lock for breaker tests that mutate
/// the global map. Any test that relies on deterministic content in
/// the singleton (clear → write → assert) should acquire this lock
/// first so parallel sibling tests don't race.
///
/// Poisoned guards recover to the inner state — a prior panicking
/// test shouldn't break the current one. Production code MUST NOT call
/// this.
#[allow(dead_code)]
pub fn breaker_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

// ── RAII clear-on-drop guard (§G build lifecycle) ────────────────────────────
//
// Phase 5 §G: when a build finishes (completed / failed / cancelled /
// panicked), drop every breaker cell keyed on its build_id so the
// HashMap doesn't grow unbounded. Held in scope at every entry point
// that owns a chain build's lifetime (`execute_chain_from`,
// `execute_plan`, etc.). RAII pattern guarantees the cleanup fires
// even on panic / cancellation.

/// Guard that calls `clear_build(build_id)` on Drop. Attach at the
/// top of a build's entry-point function so the breaker map is
/// reclaimed whenever the build's owning scope unwinds — whether by
/// normal completion, early-return error, or panic.
#[allow(dead_code)]
pub struct BuildBreakerGuard {
    build_id: String,
}

impl BuildBreakerGuard {
    #[allow(dead_code)]
    pub fn new(build_id: impl Into<String>) -> Self {
        Self {
            build_id: build_id.into(),
        }
    }
}

impl Drop for BuildBreakerGuard {
    fn drop(&mut self) {
        clear_build(&self.build_id);
    }
}

// ── StepContext-aware convenience wrappers ───────────────────────────────────
//
// Phase 5 §F: call sites in `llm.rs` / `chain_dispatch.rs` have
// access to the StepContext (which carries `build_id` + `model_tier`)
// and to a ProviderType-equivalent sentinel (RouteBranch::Fleet /
// RouteBranch::Market, plus is_local for pool entries). These helpers
// accept `Option<&StepContext>` because the walker loop is reachable
// from paths that don't carry a StepContext (test fixtures,
// legacy-bring-up helpers); absent context is a no-op.

use crate::pyramid::step_context::StepContext;

/// Record a genuine failure against the step-context build_id +
/// model_tier if present. No-op when `ctx` is None or the tier is
/// empty.
#[allow(dead_code)]
pub fn record_failure_from_ctx(ctx: Option<&StepContext>, provider_type: ProviderType) {
    let Some(ctx) = ctx else {
        return;
    };
    if ctx.build_id.is_empty() || ctx.model_tier.is_empty() {
        return;
    }
    record_failure(&ctx.build_id, &ctx.model_tier, provider_type);
}

/// Record a successful dispatch against the step-context build_id +
/// model_tier if present. No-op when `ctx` is None or the tier is
/// empty.
#[allow(dead_code)]
pub fn record_success_from_ctx(ctx: Option<&StepContext>, provider_type: ProviderType) {
    let Some(ctx) = ctx else {
        return;
    };
    if ctx.build_id.is_empty() || ctx.model_tier.is_empty() {
        return;
    }
    record_success(&ctx.build_id, &ctx.model_tier, provider_type);
}

// ── on_partial_failure runtime branch (§C) ───────────────────────────────────
//
// Phase 5 §C: the Decision's `on_partial_failure` field becomes an
// active runtime switch (instead of being merely threaded through
// for visibility). Three variants, documented at §3:
//   - Cascade (DEFAULT): advance to next provider. Matches existing
//     walker behavior; callers `continue` as before.
//   - FailLoud: stop the cascade. Emit `dispatch_failed_policy_blocked`
//     and bubble a terminal error. Privacy-preserving posture for
//     slots where cross-provider prompt leakage matters.
//   - RetrySame: stay on same provider. Respects breaker state. If
//     the provider has hit its breaker, treat as FailLoud (no
//     meaningful retry available).

/// Outcome of a post-failure `on_partial_failure` consultation.
///
/// `Cascade` preserves existing walker behavior — callers `continue`.
/// `FailLoud` / `RetrySame` are Phase 5 additions.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialFailureAction {
    /// Advance to next provider in effective_call_order. Default.
    Cascade,
    /// Stop cascade; emit dispatch_failed_policy_blocked; return
    /// terminal error to caller. Used when privacy matters more than
    /// availability.
    FailLoud,
    /// Stay on same provider; caller re-tries the same dispatch.
    /// Degrades to FailLoud if the breaker has tripped.
    RetrySame,
}

/// Given the Decision's `on_partial_failure` policy and the current
/// breaker state for (build, slot, provider), return what walker
/// should do next after a failure.
///
/// Caller-side branching:
///   - Cascade   → `continue` (the existing default behavior).
///   - FailLoud  → break out of the walker loop, bubble terminal.
///   - RetrySame → re-enter dispatch for same provider.
///
/// `breaker_reset` is read from the Decision's per_provider params
/// so all policies stay consistent with the Decision snapshot.
#[allow(dead_code)]
pub fn on_partial_failure_action(
    policy: crate::pyramid::walker_resolver::PartialFailurePolicy,
    build_id: Option<&str>,
    slot: &str,
    provider_type: ProviderType,
    breaker_reset: BreakerReset,
) -> PartialFailureAction {
    use crate::pyramid::walker_resolver::PartialFailurePolicy as P;
    match policy {
        P::Cascade => PartialFailureAction::Cascade,
        P::FailLoud => PartialFailureAction::FailLoud,
        P::RetrySame => {
            // If the breaker is tripped, RetrySame cannot meaningfully
            // retry — the Decision builder would have dropped the
            // provider already. Degrade to FailLoud so the walker
            // stops loud instead of spinning.
            if let Some(bid) = build_id {
                if is_tripped(bid, slot, provider_type, breaker_reset) {
                    return PartialFailureAction::FailLoud;
                }
            }
            PartialFailureAction::RetrySame
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_breaker_starts_untripped() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        assert!(!is_tripped(
            "build-start",
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild,
        ));
        assert!(peek_state("build-start", "mid", ProviderType::Market).is_none());
    }

    #[test]
    fn test_breaker_trips_after_threshold_failures() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-trip";
        // Below threshold — not tripped.
        for _ in 0..(TRIP_THRESHOLD - 1) {
            record_failure(bid, "mid", ProviderType::Market);
        }
        assert!(!is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
        // Crossing the threshold flips the flag.
        record_failure(bid, "mid", ProviderType::Market);
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
        let st = peek_state(bid, "mid", ProviderType::Market).unwrap();
        assert!(st.tripped);
        assert_eq!(st.consecutive_failures, TRIP_THRESHOLD);
    }

    #[test]
    fn test_breaker_reset_per_build_persists_across_decision_builds() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-perbuild";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        // Simulate a second Decision build for the same (build, slot,
        // provider) — breaker state has to persist, and `PerBuild`
        // reset never untrips.
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
        // A fresh check still says tripped.
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
    }

    #[test]
    fn test_breaker_reset_time_secs_untrips_after_interval() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-timesecs";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        // Immediately: tripped.
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::TimeSecs { value: 1 },
        ));
        // Wait past the interval.
        sleep(Duration::from_millis(1100));
        assert!(!is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::TimeSecs { value: 1 },
        ));
        // State was mutated to reflect untrip.
        let st = peek_state(bid, "mid", ProviderType::Market).unwrap();
        assert!(!st.tripped);
        assert_eq!(st.consecutive_failures, 0);
    }

    #[test]
    fn test_breaker_reset_probe_based_placeholder() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-probe";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        // Scaffold: stays tripped without an explicit probe-success.
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::ProbeBased
        ));
        // Explicit probe-success untrips.
        note_probe_success(bid, "mid", ProviderType::Market);
        assert!(!is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::ProbeBased
        ));
    }

    #[test]
    fn test_breaker_clear_build_removes_state() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-clear";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        record_failure(bid, "high", ProviderType::Local);
        // Sanity.
        assert!(peek_state(bid, "mid", ProviderType::Market).is_some());
        assert!(peek_state(bid, "high", ProviderType::Local).is_some());
        // Clear drops all cells for this build_id.
        clear_build(bid);
        assert!(peek_state(bid, "mid", ProviderType::Market).is_none());
        assert!(peek_state(bid, "high", ProviderType::Local).is_none());
    }

    #[test]
    fn test_breaker_clear_build_leaves_other_builds_intact() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let keep_bid = "build-keep";
        let drop_bid = "build-drop";
        record_failure(keep_bid, "mid", ProviderType::Market);
        record_failure(drop_bid, "mid", ProviderType::Market);
        clear_build(drop_bid);
        assert!(peek_state(keep_bid, "mid", ProviderType::Market).is_some());
        assert!(peek_state(drop_bid, "mid", ProviderType::Market).is_none());
    }

    #[test]
    fn test_breaker_record_success_resets_counter_but_not_tripped_flag() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-record-success";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        assert!(
            peek_state(bid, "mid", ProviderType::Market)
                .unwrap()
                .tripped
        );
        // record_success resets counter; tripped flag persists under
        // PerBuild semantics.
        record_success(bid, "mid", ProviderType::Market);
        let st = peek_state(bid, "mid", ProviderType::Market).unwrap();
        assert_eq!(st.consecutive_failures, 0);
        assert!(st.tripped);
        // Still reads tripped under PerBuild.
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
    }

    #[test]
    fn test_on_partial_failure_cascade_advances() {
        use crate::pyramid::walker_resolver::PartialFailurePolicy;
        let action = on_partial_failure_action(
            PartialFailurePolicy::Cascade,
            Some("build-x"),
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild,
        );
        assert_eq!(action, PartialFailureAction::Cascade);
    }

    #[test]
    fn test_on_partial_failure_fail_loud_stops() {
        use crate::pyramid::walker_resolver::PartialFailurePolicy;
        let action = on_partial_failure_action(
            PartialFailurePolicy::FailLoud,
            Some("build-x"),
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild,
        );
        assert_eq!(action, PartialFailureAction::FailLoud);
    }

    #[test]
    fn test_on_partial_failure_retry_same_stays_on_provider() {
        use crate::pyramid::walker_resolver::PartialFailurePolicy;
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let action = on_partial_failure_action(
            PartialFailurePolicy::RetrySame,
            Some("build-retry"),
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild,
        );
        assert_eq!(action, PartialFailureAction::RetrySame);
    }

    #[test]
    fn test_on_partial_failure_retry_same_degrades_to_fail_loud_when_tripped() {
        use crate::pyramid::walker_resolver::PartialFailurePolicy;
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-retry-tripped";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        // With the breaker tripped, RetrySame degrades to FailLoud.
        let action = on_partial_failure_action(
            PartialFailurePolicy::RetrySame,
            Some(bid),
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild,
        );
        assert_eq!(action, PartialFailureAction::FailLoud);
    }

    #[test]
    fn test_breaker_key_isolates_by_tuple() {
        let _g = breaker_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        // Three failures on (b, mid, Market) must NOT trip (b, mid, Local).
        let bid = "build-iso";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::Market);
        }
        assert!(is_tripped(
            bid,
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
        assert!(!is_tripped(
            bid,
            "mid",
            ProviderType::Local,
            BreakerReset::PerBuild
        ));
        assert!(!is_tripped(
            bid,
            "high",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
        assert!(!is_tripped(
            "other",
            "mid",
            ProviderType::Market,
            BreakerReset::PerBuild
        ));
    }
}
