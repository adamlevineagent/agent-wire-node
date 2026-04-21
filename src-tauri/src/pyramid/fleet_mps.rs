// pyramid/fleet_mps.rs — Fleet MPS three-objects
// (ServiceDescriptor, AvailabilitySnapshot, PeerKnowledgeState) +
// local derivation helpers.
//
// Per `docs/plans/fleet-mps-build-plan.md` "Canonical Runtime Model"
// and WS3 "Local Derived State Split":
//
//   - `ServiceDescriptor` — what this node CAN do (durable capability
//     shape). Separated from runtime state so that descriptor churn is
//     independent of queue flux.
//   - `AvailabilitySnapshot` — what this node IS doing RIGHT NOW.
//     Queue depths, health, tunnel reachability. Separated from
//     descriptor so availability churn doesn't invalidate capability
//     knowledge.
//   - `PeerKnowledgeState` — belief about a specific fleet peer,
//     merged from announce/pull/cache. Holds peer identity plus
//     optional descriptor + availability (which may be absent when
//     status is `unknown`).
//
// This module defines the types + pure derivation helpers only. The
// reducer (WS4), pull endpoint + reconciliation (WS5), warm cache
// (WS6), and dispatch-engine refactor (WS7) are separate Fleet MPS
// workstreams; this module's surface does NOT depend on them. Phase
// 2 WS5 (compute market dispatch handler) reads
// `AvailabilitySnapshot.health_status` + `.tunnel_status` for its
// admission gate and `ServiceDescriptor.visibility` for offer
// publication; those call sites construct the snapshot/descriptor
// on demand via the helpers here.
//
// Version fields (`descriptor_version`, `availability_version`) are
// monotonic per-object counters. Callers own the "prior version"
// storage and increment on each new snapshot — see
// `derive_service_descriptor` and `derive_availability_snapshot`.
// This mirrors the anti-entropy pattern in `fleet-mps-build-plan.md`
// WS5 ("Use version fields so announce can later degrade to 'changed,
// fetch me'").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pyramid::dispatch_policy::DispatchPolicy;
use crate::pyramid::local_mode::{
    ComputeParticipationMode, ComputeParticipationPolicy,
};
use crate::pyramid::tunnel_url::TunnelUrl;

/// Current on-wire protocol version for ServiceDescriptor + related
/// fleet RPCs. Bumped on breaking protocol changes — receivers with a
/// lower `protocol_version` should either skip the peer entirely or
/// degrade to a compatibility path. Receivers with an EQUAL or HIGHER
/// protocol_version should assume forward compatibility.
///
/// Current value: 1. Reserved for future bumps when:
///   - The `CallbackKind` enum grows new variants that change wire shape.
///   - `ServableRule` gains required fields.
///   - A new RPC is added that peers must implement to stay servable.
pub const FLEET_MPS_PROTOCOL_VERSION: u32 = 1;

/// What audience is this node offering service to? Derived from the
/// operator's `ComputeParticipationPolicy` effective booleans. Drives
/// offer publication gating (market-visible → publish to market;
/// private-fleet → fleet-only; disabled → no inbound offers at all).
///
/// Note: this enum reflects *serving* visibility only, not dispatch.
/// A `Coordinator`-mode node (all dispatch on, all serving off)
/// resolves to `Disabled` here — it accepts no inbound work — but may
/// still actively dispatch outward via the separate
/// `allow_*_dispatch` / `allow_*_usage` policy gates. Downstream
/// consumers that care about dispatch intent must read the
/// participation policy directly, not this field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceVisibility {
    /// Publish offers to the compute market and accept inbound market
    /// jobs. In the canonical DD-I projection `market-visible` implies
    /// `fleet-serving` (market-visible is a superset); explicit
    /// operator overrides can violate that invariant — the derivation
    /// here does NOT auto-correct, it trusts `allow_market_visibility`
    /// when set and treats inconsistent-policy violations as the
    /// operator's responsibility.
    MarketVisible,
    /// Fleet peers only (same-operator). No market offers, no market
    /// jobs accepted. Fleet serving still allowed.
    PrivateFleet,
    /// Not accepting inbound work (neither fleet nor market). Covers
    /// two operator states: (1) `Coordinator`-mode (all serving off,
    /// still dispatches outward) and (2) fully-disabled nodes (every
    /// participation flag off). Consumers that need to distinguish
    /// "actively dispatches out" from "totally off" must consult the
    /// participation policy.
    Disabled,
}

/// Overall node health signal. Drives the "accept work when degraded"
/// admission branch (Phase 2 §III: "if `degraded` AND
/// `allow_serving_while_degraded == false`, reject with 503").
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded,
}

/// Tunnel reachability status. Drives the "can we even deliver the
/// result back?" admission branch (Phase 2 §III: "if not `healthy`,
/// reject"). Separate from `HealthStatus` because a node can be
/// overall-healthy but tunnel-unreachable (flaky upstream, cert
/// expiry, rate limit) — and the operational response is different.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelStatus {
    Healthy,
    Unhealthy,
}

/// Freshness state of a peer's capability belief. Per fleet-mps-build-
/// plan.md "Remote belief object":
///   - `Unknown` — we've discovered the peer (via heartbeat roster)
///     but never received a descriptor from them. Distinguish from
///     "empty descriptor" — an empty `servable_rules` on a FRESH
///     descriptor means "known empty"; unknown means "we don't know."
///   - `Fresh` — descriptor was received within the freshness
///     window.
///   - `Stale` — descriptor was received but is past the freshness
///     window. Callers may still use it with a degradation signal.
///   - `Failed` — the last pull/announce attempt errored. Caller
///     should retry before skipping the peer entirely.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Unknown,
    Fresh,
    Stale,
    Failed,
}

/// Which transport brought us the current belief. Relevant for the
/// reducer (Fleet MPS WS4) to decide freshness precedence when
/// multiple sources race.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Peer pushed an announcement to us.
    Announce,
    /// We pulled from the peer's `/v1/fleet/capabilities` endpoint.
    Pull,
    /// Local disk cache hydrated at startup (last known before
    /// restart).
    Cache,
}

/// What this node CAN do — durable capability shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDescriptor {
    /// Operator's declared mode (coordinator/hybrid/worker). Pulled
    /// straight from `ComputeParticipationPolicy.mode`.
    pub declared_role: ComputeParticipationMode,
    /// Routing rule names this node can serve locally. Derived from
    /// the dispatch policy via `fleet::derive_serving_rules`.
    pub servable_rules: Vec<String>,
    /// Model IDs currently loaded (Ollama / local-mode).
    pub models_loaded: Vec<String>,
    /// Who sees this node's offers — market-visible, fleet-only, or
    /// fully disabled. Derived from `effective_booleans()`.
    pub visibility: ServiceVisibility,
    /// On-wire protocol version. Receivers with a lower version
    /// should skip or degrade.
    pub protocol_version: u32,
    /// Monotonic per-node counter; bumped on every descriptor change.
    /// Used by the anti-entropy protocol (WS5) so receivers can
    /// detect "changed, refetch me" without downloading the whole
    /// descriptor.
    pub descriptor_version: u64,
    /// When this descriptor was computed.
    pub computed_at: DateTime<Utc>,
}

/// What this node IS doing right now — runtime state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailabilitySnapshot {
    /// Per-model queue depth (`model_id` → count).
    pub queue_depths: HashMap<String, usize>,
    /// Sum of `queue_depths`. Stored rather than recomputed because
    /// downstream consumers (load balancer, admission gate) read this
    /// field many times per call and we'd rather pay the summation
    /// cost once at derive time.
    pub total_queue_depth: usize,
    pub health_status: HealthStatus,
    pub tunnel_status: TunnelStatus,
    /// Convenience flag: `health_status == Degraded`. Exists so
    /// matching gates (`if degraded { reject }`) don't have to
    /// remember which variant name is "bad."
    pub degraded: bool,
    /// Monotonic per-node counter; bumped on every snapshot change.
    /// Anti-entropy partner for `descriptor_version`.
    pub availability_version: u64,
    pub last_updated: DateTime<Utc>,
}

/// Belief about a specific fleet peer, merged from
/// announce/pull/cache. The reducer (Fleet MPS WS4) merges updates
/// from different transport sources into one of these.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerKnowledgeState {
    // ── Peer identity ────────────────────────────────────────────────
    pub node_id: String,
    pub handle_path: Option<String>,
    pub name: String,
    pub tunnel_url: TunnelUrl,

    // ── Capability belief ────────────────────────────────────────────
    /// Peer's declared mode (copied from descriptor when Fresh).
    /// Separate field so cards can show "Role: Worker" even when the
    /// descriptor is stale or unknown.
    pub declared_role: Option<ComputeParticipationMode>,
    /// Present when `capability_status != Unknown`. May be stale if
    /// `capability_status == Stale`.
    pub service_descriptor: Option<ServiceDescriptor>,
    /// Present when we've ever received availability (i.e. from a
    /// Fresh descriptor exchange). Availability is high-churn so
    /// expect this to go stale quickly.
    pub availability_snapshot: Option<AvailabilitySnapshot>,

    // ── Freshness metadata ───────────────────────────────────────────
    pub capability_status: CapabilityStatus,
    pub last_capability_sync_at: Option<DateTime<Utc>>,
    pub last_capability_error: Option<String>,
    /// Which transport gave us the current belief. `None` when
    /// `capability_status == Unknown` (never synced).
    pub last_capability_source: Option<CapabilitySource>,
}

impl PeerKnowledgeState {
    /// Construct a peer in the `Unknown` state — discovered via
    /// heartbeat roster but no descriptor received yet. Distinguish
    /// from an empty descriptor (which would be
    /// `capability_status = Fresh` with `servable_rules = []`).
    pub fn unknown(
        node_id: String,
        name: String,
        tunnel_url: TunnelUrl,
        handle_path: Option<String>,
    ) -> Self {
        Self {
            node_id,
            handle_path,
            name,
            tunnel_url,
            declared_role: None,
            service_descriptor: None,
            availability_snapshot: None,
            capability_status: CapabilityStatus::Unknown,
            last_capability_sync_at: None,
            last_capability_error: None,
            last_capability_source: None,
        }
    }
}

// ── Pure derivation helpers ──────────────────────────────────────────
//
// These take explicit inputs (no hidden global reads) so they're
// deterministic and testable. The call sites that currently derive
// capability data inline at heartbeat time — see `main.rs` ConfigSynced
// handler and heartbeat response path — should be migrated to call
// these helpers instead. That migration is WS4 scope (reducer) and
// WS5 scope (capabilities endpoint); WS1b just ships the helpers.

/// Compute the effective `ServiceVisibility` from a participation
/// policy. Per `fleet-mps-build-plan.md` WS3 "visibility field":
///   - `market-visible` → publish to market
///   - `private fleet` → fleet-only
///   - `disabled` → no offers at all
///
/// Mapping:
///   - `allow_market_visibility == true` → `MarketVisible`
///     (market-visible implies fleet-serving; the superset is
///     accepting market jobs AND fleet jobs)
///   - `allow_fleet_serving == true && !allow_market_visibility` →
///     `PrivateFleet`
///   - neither → `Disabled`
pub fn derive_visibility(policy: &ComputeParticipationPolicy) -> ServiceVisibility {
    let eff = policy.effective_booleans();
    if eff.allow_market_visibility {
        ServiceVisibility::MarketVisible
    } else if eff.allow_fleet_serving {
        ServiceVisibility::PrivateFleet
    } else {
        ServiceVisibility::Disabled
    }
}

/// Compute a fresh `ServiceDescriptor` from the node's current
/// participation policy + dispatch policy + loaded models.
///
/// `prior_version` is the previous descriptor's `descriptor_version`
/// (0 if this is the first descriptor ever computed for this node).
/// The returned descriptor has `prior_version + 1`. The caller owns
/// storage of "what was the last version?" and must pass it in on
/// each call — this keeps the helper pure and avoids hidden
/// module-level state.
///
/// Contract: the helper TRUSTS `prior_version`. Passing a value less
/// than the real prior would cause the descriptor to appear to
/// regress to anti-entropy peers and would be interpreted as
/// malicious/buggy and rejected. The reducer (WS4) is responsible
/// for sourcing `prior_version` from the single canonical cell
/// (e.g. the persisted warm cache) — don't compute it ad hoc at
/// call sites.
pub fn derive_service_descriptor(
    policy: &ComputeParticipationPolicy,
    dispatch_policy: &DispatchPolicy,
    loaded_models: &[String],
    prior_version: u64,
) -> ServiceDescriptor {
    ServiceDescriptor {
        declared_role: policy.mode,
        servable_rules: crate::fleet::derive_serving_rules(dispatch_policy, loaded_models),
        models_loaded: loaded_models.to_vec(),
        visibility: derive_visibility(policy),
        protocol_version: FLEET_MPS_PROTOCOL_VERSION,
        descriptor_version: prior_version.saturating_add(1),
        computed_at: Utc::now(),
    }
}

/// Compute a fresh `AvailabilitySnapshot` from the node's current
/// queue state + tunnel health + overall health signal.
///
/// Inputs by reference so tests can feed small fixtures without
/// synthesizing the full runtime state. `prior_version` is the
/// previous snapshot's `availability_version` (0 if first-ever).
///
/// Contract: same trust semantics as `derive_service_descriptor` —
/// `prior_version` must come from the canonical cell, not
/// recomputed ad hoc. A regressing version breaks anti-entropy.
pub fn derive_availability_snapshot(
    queue_depths: &HashMap<String, usize>,
    tunnel_healthy: bool,
    overall_healthy: bool,
    prior_version: u64,
) -> AvailabilitySnapshot {
    let total_queue_depth = queue_depths.values().copied().sum();
    let health_status = if overall_healthy {
        HealthStatus::Healthy
    } else {
        HealthStatus::Degraded
    };
    let tunnel_status = if tunnel_healthy {
        TunnelStatus::Healthy
    } else {
        TunnelStatus::Unhealthy
    };
    AvailabilitySnapshot {
        queue_depths: queue_depths.clone(),
        total_queue_depth,
        health_status,
        tunnel_status,
        degraded: matches!(health_status, HealthStatus::Degraded),
        availability_version: prior_version.saturating_add(1),
        last_updated: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pyramid::dispatch_policy::{
        BuildCoordinationConfig, DispatchPolicy, EscalationConfig, MatchConfig,
        RouteEntry, RoutingRule,
    };
    use crate::pyramid::local_mode::ComputeParticipationPolicy;
    use std::collections::BTreeMap;

    // ── Helpers for tests ────────────────────────────────────────────

    fn hybrid_with_markets_off() -> ComputeParticipationPolicy {
        // Hybrid mode, fleet on, market EXPLICITLY off. Used by tests
        // that want to exercise PrivateFleet visibility semantics.
        // Post-purpose-lock, the Default has market ON (None → Hybrid
        // projection), so this helper must explicitly override to Some(false)
        // to get the "markets off" test scenario.
        let mut p = ComputeParticipationPolicy::default();
        p.allow_market_dispatch = Some(false);
        p.allow_market_visibility = Some(false);
        p
    }

    fn hybrid_with_market_on() -> ComputeParticipationPolicy {
        let mut p = ComputeParticipationPolicy::default();
        p.allow_market_visibility = Some(true);
        p
    }

    fn everything_off() -> ComputeParticipationPolicy {
        let mut p = ComputeParticipationPolicy::default();
        p.allow_fleet_dispatch = Some(false);
        p.allow_fleet_serving = Some(false);
        p.allow_market_dispatch = Some(false);
        p.allow_market_visibility = Some(false);
        p.allow_storage_pulling = Some(false);
        p.allow_storage_hosting = Some(false);
        p.allow_relay_usage = Some(false);
        p.allow_relay_serving = Some(false);
        p
    }

    fn minimal_dispatch_policy() -> DispatchPolicy {
        DispatchPolicy {
            rules: vec![RoutingRule {
                name: "code_l0".to_string(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to: vec![
                    RouteEntry {
                        provider_id: "ollama".to_string(),
                        model_id: Some("gemma3:27b".to_string()),
                        tier_name: None,
                        is_local: true,
                        max_budget_credits: None,
                    },
                    RouteEntry {
                        provider_id: "fleet".to_string(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    },
                ],
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        }
    }

    /// Dispatch policy where the rule has ONLY a fleet route (no local
    /// entry). Used to verify fleet-only rules are NOT servable locally.
    fn dispatch_policy_fleet_only() -> DispatchPolicy {
        DispatchPolicy {
            rules: vec![RoutingRule {
                name: "remote_only".to_string(),
                match_config: MatchConfig {
                    work_type: None,
                    min_depth: None,
                    step_pattern: None,
                },
                route_to: vec![RouteEntry {
                    provider_id: "fleet".to_string(),
                    model_id: None,
                    tier_name: None,
                    is_local: false,
                    max_budget_credits: None,
                }],
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        }
    }

    /// A coordinator-mode policy with every projectable boolean cleared
    /// (None) so the DD-I projection wins. The default()'s explicit
    /// Some(_) values would otherwise override the coordinator preset —
    /// we want to test the pure coordinator projection here.
    fn coordinator_policy_pure_projection() -> ComputeParticipationPolicy {
        ComputeParticipationPolicy {
            mode: ComputeParticipationMode::Coordinator,
            allow_fleet_dispatch: None,
            allow_fleet_serving: None,
            allow_market_dispatch: None,
            allow_market_visibility: None,
            allow_storage_pulling: None,
            allow_storage_hosting: None,
            allow_relay_usage: None,
            allow_relay_serving: None,
            ..ComputeParticipationPolicy::default()
        }
    }

    // ── ServiceVisibility derivation ─────────────────────────────────

    #[test]
    fn derive_visibility_market_on_returns_market_visible() {
        let p = hybrid_with_market_on();
        assert_eq!(derive_visibility(&p), ServiceVisibility::MarketVisible);
    }

    #[test]
    fn derive_visibility_fleet_only_returns_private_fleet() {
        // Default: fleet on, market off → PrivateFleet.
        let p = hybrid_with_markets_off();
        assert_eq!(derive_visibility(&p), ServiceVisibility::PrivateFleet);
    }

    #[test]
    fn derive_visibility_everything_off_returns_disabled() {
        let p = everything_off();
        assert_eq!(derive_visibility(&p), ServiceVisibility::Disabled);
    }

    #[test]
    fn derive_visibility_coordinator_mode_returns_disabled() {
        // Coordinator mode per DD-I: all dispatch/usage on, all
        // serving/hosting/visibility off. From the POV of
        // ServiceVisibility (which is about INBOUND work acceptance),
        // coordinator is Disabled — the node dispatches outward but
        // accepts nothing inbound. The enum docstring explicitly
        // documents this case so consumers don't mistake Disabled for
        // "node fully off."
        let p = coordinator_policy_pure_projection();
        assert_eq!(derive_visibility(&p), ServiceVisibility::Disabled);
    }

    #[test]
    fn derive_visibility_market_visibility_wins_over_fleet_serving_off() {
        // An operator who turned off fleet_serving but kept
        // market_visibility on is "market visible" — the market
        // superset wins.
        let mut p = hybrid_with_markets_off();
        p.allow_fleet_serving = Some(false);
        p.allow_market_visibility = Some(true);
        assert_eq!(derive_visibility(&p), ServiceVisibility::MarketVisible);
    }

    // ── ServiceDescriptor derivation ─────────────────────────────────

    #[test]
    fn derive_service_descriptor_populates_all_fields() {
        let policy = hybrid_with_markets_off();
        let dispatch_policy = minimal_dispatch_policy();
        let models = vec!["gemma3:27b".to_string()];

        let d = derive_service_descriptor(&policy, &dispatch_policy, &models, 0);

        assert_eq!(d.declared_role, ComputeParticipationMode::Hybrid);
        assert_eq!(d.servable_rules, vec!["code_l0".to_string()]);
        assert_eq!(d.models_loaded, models);
        assert_eq!(d.visibility, ServiceVisibility::PrivateFleet);
        assert_eq!(d.protocol_version, FLEET_MPS_PROTOCOL_VERSION);
        assert_eq!(d.descriptor_version, 1); // prior 0 + 1
    }

    #[test]
    fn derive_service_descriptor_increments_version() {
        let policy = hybrid_with_markets_off();
        let dispatch_policy = minimal_dispatch_policy();
        let models = vec!["gemma3:27b".to_string()];

        let d1 = derive_service_descriptor(&policy, &dispatch_policy, &models, 0);
        let d2 = derive_service_descriptor(
            &policy,
            &dispatch_policy,
            &models,
            d1.descriptor_version,
        );
        let d3 = derive_service_descriptor(
            &policy,
            &dispatch_policy,
            &models,
            d2.descriptor_version,
        );
        assert_eq!(d1.descriptor_version, 1);
        assert_eq!(d2.descriptor_version, 2);
        assert_eq!(d3.descriptor_version, 3);
    }

    #[test]
    fn derive_service_descriptor_version_saturates_at_u64_max() {
        // Defensive: if prior_version is somehow u64::MAX, adding 1
        // must saturate rather than wrap to 0. A version number that
        // wraps to 0 would make anti-entropy think the node "went
        // backwards" and refuse to accept newer snapshots.
        let policy = hybrid_with_markets_off();
        let dispatch_policy = minimal_dispatch_policy();
        let models = vec![];

        let d = derive_service_descriptor(&policy, &dispatch_policy, &models, u64::MAX);
        assert_eq!(d.descriptor_version, u64::MAX);
    }

    #[test]
    fn derive_service_descriptor_empty_models_produces_empty_servable_rules() {
        // "Known empty" — per fleet-mps-build-plan WS3 note:
        // "Empty `servable_rules` on a computed descriptor means
        // 'known empty'."
        let policy = hybrid_with_markets_off();
        let dispatch_policy = minimal_dispatch_policy();
        let d = derive_service_descriptor(&policy, &dispatch_policy, &[], 0);
        assert!(d.servable_rules.is_empty());
        // And the descriptor is still fully populated otherwise.
        assert_eq!(d.declared_role, ComputeParticipationMode::Hybrid);
        assert_eq!(d.protocol_version, FLEET_MPS_PROTOCOL_VERSION);
    }

    #[test]
    fn derive_service_descriptor_excludes_fleet_only_rules() {
        // A rule whose only route_to entry is provider_id="fleet"
        // (is_local=false) is NOT locally servable. Confirms the
        // behavior of `fleet::derive_serving_rules` — fleet entries are
        // skipped and a rule with no local entries produces no
        // servable name. The fleet MPS spec depends on this: a peer's
        // descriptor must only advertise rules it can actually execute
        // locally.
        let policy = hybrid_with_markets_off();
        let dispatch_policy = dispatch_policy_fleet_only();
        let models = vec!["gemma3:27b".to_string()];
        let d = derive_service_descriptor(&policy, &dispatch_policy, &models, 0);
        assert!(
            d.servable_rules.is_empty(),
            "fleet-only rules must not appear in servable_rules"
        );
    }

    #[test]
    fn derive_service_descriptor_coordinator_mode_disables_visibility() {
        // Coordinator-mode node: servable_rules may still be populated
        // (if loaded models match local routes — the descriptor
        // doesn't gate itself on ServiceVisibility), but the
        // `visibility` field is `Disabled`. Downstream consumers use
        // visibility to gate offer publication; serving filtering is
        // the admission gate's responsibility.
        let policy = coordinator_policy_pure_projection();
        let dispatch_policy = minimal_dispatch_policy();
        let models = vec!["gemma3:27b".to_string()];
        let d = derive_service_descriptor(&policy, &dispatch_policy, &models, 0);
        assert_eq!(d.visibility, ServiceVisibility::Disabled);
        assert_eq!(d.declared_role, ComputeParticipationMode::Coordinator);
        // servable_rules is independent of visibility — the rule data
        // itself supports local serving even though policy disables
        // inbound acceptance.
        assert_eq!(d.servable_rules, vec!["code_l0".to_string()]);
    }

    // ── AvailabilitySnapshot derivation ──────────────────────────────

    #[test]
    fn derive_availability_snapshot_healthy_path() {
        let mut depths = HashMap::new();
        depths.insert("gemma3:27b".to_string(), 3);
        depths.insert("llama3.2:latest".to_string(), 1);

        let s = derive_availability_snapshot(&depths, true, true, 0);

        assert_eq!(s.queue_depths, depths);
        assert_eq!(s.total_queue_depth, 4);
        assert_eq!(s.health_status, HealthStatus::Healthy);
        assert_eq!(s.tunnel_status, TunnelStatus::Healthy);
        assert!(!s.degraded);
        assert_eq!(s.availability_version, 1);
    }

    #[test]
    fn derive_availability_snapshot_degraded_signals_all_three_flags() {
        let depths = HashMap::new();
        let s = derive_availability_snapshot(&depths, true, false, 0);
        assert_eq!(s.health_status, HealthStatus::Degraded);
        assert!(s.degraded);
        // degraded is a convenience mirror — must match health_status.
        assert_eq!(s.degraded, matches!(s.health_status, HealthStatus::Degraded));
    }

    #[test]
    fn derive_availability_snapshot_tunnel_unhealthy_independent_of_health() {
        // A node can be overall-healthy but tunnel-unhealthy. Both
        // signals must surface independently so gating code can
        // differentiate the operational response.
        let depths = HashMap::new();
        let s = derive_availability_snapshot(&depths, false, true, 0);
        assert_eq!(s.health_status, HealthStatus::Healthy);
        assert_eq!(s.tunnel_status, TunnelStatus::Unhealthy);
        assert!(!s.degraded,
            "degraded mirrors health_status, not tunnel_status");
    }

    #[test]
    fn derive_availability_snapshot_empty_queue_depths_total_zero() {
        let depths = HashMap::new();
        let s = derive_availability_snapshot(&depths, true, true, 0);
        assert_eq!(s.total_queue_depth, 0);
        assert!(s.queue_depths.is_empty());
    }

    #[test]
    fn derive_availability_snapshot_increments_version() {
        let depths = HashMap::new();
        let s1 = derive_availability_snapshot(&depths, true, true, 0);
        let s2 = derive_availability_snapshot(&depths, true, true, s1.availability_version);
        assert_eq!(s1.availability_version, 1);
        assert_eq!(s2.availability_version, 2);
    }

    #[test]
    fn derive_availability_snapshot_version_saturates_at_u64_max() {
        let depths = HashMap::new();
        let s = derive_availability_snapshot(&depths, true, true, u64::MAX);
        assert_eq!(s.availability_version, u64::MAX);
    }

    // ── PeerKnowledgeState constructors + defaults ───────────────────

    #[test]
    fn peer_knowledge_state_unknown_leaves_capability_fields_none() {
        let tunnel = TunnelUrl::parse("https://example.com").unwrap();
        let p = PeerKnowledgeState::unknown(
            "peer-x".to_string(),
            "PeerX".to_string(),
            tunnel,
            Some("@foo/PeerX".to_string()),
        );
        assert_eq!(p.node_id, "peer-x");
        assert_eq!(p.name, "PeerX");
        assert_eq!(p.handle_path.as_deref(), Some("@foo/PeerX"));
        assert!(p.declared_role.is_none());
        assert!(p.service_descriptor.is_none());
        assert!(p.availability_snapshot.is_none());
        assert_eq!(p.capability_status, CapabilityStatus::Unknown);
        assert!(p.last_capability_sync_at.is_none());
        assert!(p.last_capability_error.is_none());
        assert!(p.last_capability_source.is_none());
    }

    // ── Serde round-trips ────────────────────────────────────────────

    #[test]
    fn service_descriptor_yaml_roundtrips() {
        let policy = hybrid_with_markets_off();
        let dispatch_policy = minimal_dispatch_policy();
        let d1 = derive_service_descriptor(
            &policy,
            &dispatch_policy,
            &["gemma3:27b".to_string()],
            0,
        );
        let yaml = serde_yaml::to_string(&d1).unwrap();
        let d2: ServiceDescriptor = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn availability_snapshot_yaml_roundtrips() {
        let mut depths = HashMap::new();
        depths.insert("m1".to_string(), 7);
        let s1 = derive_availability_snapshot(&depths, true, false, 42);
        let yaml = serde_yaml::to_string(&s1).unwrap();
        let s2: AvailabilitySnapshot = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn peer_knowledge_state_json_roundtrips() {
        let tunnel = TunnelUrl::parse("https://example.com/v1").unwrap();
        let p1 = PeerKnowledgeState::unknown(
            "peer-x".to_string(),
            "PeerX".to_string(),
            tunnel,
            None,
        );
        let json = serde_json::to_string(&p1).unwrap();
        let p2: PeerKnowledgeState = serde_json::from_str(&json).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn service_visibility_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ServiceVisibility::MarketVisible).unwrap(),
            "\"market_visible\""
        );
        assert_eq!(
            serde_json::to_string(&ServiceVisibility::PrivateFleet).unwrap(),
            "\"private_fleet\""
        );
        assert_eq!(
            serde_json::to_string(&ServiceVisibility::Disabled).unwrap(),
            "\"disabled\""
        );
    }

    #[test]
    fn capability_status_serializes_as_snake_case() {
        // "Unknown" is the critical one — distinguishes "empty
        // descriptor" from "no descriptor yet." On-wire shape must be
        // stable across the reducer (WS4) and the pull endpoint (WS5).
        assert_eq!(
            serde_json::to_string(&CapabilityStatus::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&CapabilityStatus::Fresh).unwrap(),
            "\"fresh\""
        );
    }
}
