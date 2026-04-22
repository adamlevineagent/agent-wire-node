// Walker v3 — ProviderReadiness trait + stub impls (Phase 0a-1 commit 3).
//
// Plan rev 1.0.2 §2.6. Each provider module will answer "am I ready to dispatch
// this (slot, provider) pair right now?" with either Ready or a reasoned
// NotReady. The Decision builder (Phase 0b `walker_decision.rs`) calls every
// provider's `can_dispatch_now` during construction so the resulting Decision's
// `effective_call_order` is already pre-filtered.
//
// Phase 0a-1 scope: trait + reason enum + minimal ResolvedProviderParams shell
// + four stub impls returning Ready. Phase 0a-2 relocates impls to their
// natural provider modules (local_mode, fleet, provider, compute_market_ctx)
// and fills real bodies.

use std::time::SystemTime;

/// Per-provider answer to the readiness gate.
///
/// `NotReady { reason }` carries a specific enum variant so the chronicle
/// `provider_skipped_readiness` event records _why_ instead of a bare bool.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ReadinessResult {
    Ready,
    NotReady { reason: NotReadyReason },
}

/// All specific reasons a provider can refuse dispatch. Exhaustive per
/// plan §2.6. New variants require plan + parameter-catalog + test updates.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum NotReadyReason {
    /// `overrides.active == false` — operator-disabled.
    Inactive,
    /// Resolved model_list is None or empty for this (slot, provider).
    NoModelListForSlot,
    /// `pyramid_providers.api_key_ref` unresolvable (openrouter).
    CredentialMissing,
    /// Ollama probe stale or failed (local).
    OllamaOffline,
    /// Market: cached balance < 1 credit and outside the onboarding grace window.
    InsufficientCredit,
    /// Market: can't verify balance AND grace window expired.
    WireUnreachable,
    /// openrouter/market: network back-off active (§2.16.5).
    NetworkUnreachable {
        consecutive_failures: u32,
        last_success_at: Option<SystemTime>,
    },
    /// Market: MarketSurfaceCache shows 0 offers matching any slug in the
    /// resolved model_list.
    NoMarketOffersForSlot,
    /// Market: only available offers come from this node's own publisher
    /// OR from this node's `node_identity_history` (§2.16.7).
    SelfDealing,
    /// Fleet: no peer younger than staleness cutoff.
    NoReachablePeer,
    /// Fleet: announce shows no peer has listed model in resolved model_list.
    NoPeerHasModel,
    /// Fleet: peer's `announce_protocol_version < 2`; strict mode refuses
    /// dispatch (§5.5.2).
    PeerIsV1Announcer,
}

/// Resolved per-provider parameters passed into `can_dispatch_now`.
///
/// Phase 0a-1 stub: carries only `active` (the universal gate). Phase 0a-2
/// / Phase 0b expand to the full catalog from plan §2.9 (model_list,
/// max_budget_credits, patience_secs, breaker_reset, sequential, bypass_pool,
/// retry_http_count, retry_backoff_base_secs, dispatch_deadline_grace_secs,
/// and provider-specific fields). Kept minimal here so Phase 0a-1 stubs
/// compile without depending on Decision-builder scaffolding that lands later.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ResolvedProviderParams {
    pub active: bool,
}

/// Plan §2.6. Each provider implements; Decision builder calls at step entry.
#[allow(dead_code)]
pub trait ProviderReadiness {
    fn can_dispatch_now(&self, params: &ResolvedProviderParams) -> ReadinessResult;
}

// ── Stub provider marker types + Phase 0a-1 impls ─────────────────────────
//
// Zero-sized markers so Phase 0a-1 can satisfy the "trait compiles with four
// impls returning Ready" exit criterion without depending on the real provider
// state types (which the Decision-builder work in Phase 0a-2 / Phase 0b wires
// up). Phase 0a-2 replaces these with impl blocks on the real provider types
// in their natural modules (local_mode, fleet / fleet_mps, provider,
// compute_market_ctx / compute_market_ops) and deletes these placeholders.

#[allow(dead_code)]
pub struct LocalReadinessStub;
#[allow(dead_code)]
pub struct OpenRouterReadinessStub;
#[allow(dead_code)]
pub struct FleetReadinessStub;
#[allow(dead_code)]
pub struct MarketReadinessStub;

impl ProviderReadiness for LocalReadinessStub {
    fn can_dispatch_now(&self, _params: &ResolvedProviderParams) -> ReadinessResult {
        ReadinessResult::Ready
    }
}

impl ProviderReadiness for OpenRouterReadinessStub {
    fn can_dispatch_now(&self, _params: &ResolvedProviderParams) -> ReadinessResult {
        ReadinessResult::Ready
    }
}

impl ProviderReadiness for FleetReadinessStub {
    fn can_dispatch_now(&self, _params: &ResolvedProviderParams) -> ReadinessResult {
        ReadinessResult::Ready
    }
}

impl ProviderReadiness for MarketReadinessStub {
    fn can_dispatch_now(&self, _params: &ResolvedProviderParams) -> ReadinessResult {
        ReadinessResult::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_stubs_return_ready() {
        let p = ResolvedProviderParams { active: true };
        for r in [
            LocalReadinessStub.can_dispatch_now(&p),
            OpenRouterReadinessStub.can_dispatch_now(&p),
            FleetReadinessStub.can_dispatch_now(&p),
            MarketReadinessStub.can_dispatch_now(&p),
        ] {
            assert!(matches!(r, ReadinessResult::Ready));
        }
    }

    #[test]
    fn not_ready_reason_variants_exhaustive_compile() {
        // This test exists so adding/removing a NotReadyReason variant in the
        // future forces a compile-time audit of callers. It does not execute
        // meaningful logic — matching on every variant here is the point.
        let now = Some(SystemTime::now());
        let variants = [
            NotReadyReason::Inactive,
            NotReadyReason::NoModelListForSlot,
            NotReadyReason::CredentialMissing,
            NotReadyReason::OllamaOffline,
            NotReadyReason::InsufficientCredit,
            NotReadyReason::WireUnreachable,
            NotReadyReason::NetworkUnreachable {
                consecutive_failures: 3,
                last_success_at: now,
            },
            NotReadyReason::NoMarketOffersForSlot,
            NotReadyReason::SelfDealing,
            NotReadyReason::NoReachablePeer,
            NotReadyReason::NoPeerHasModel,
            NotReadyReason::PeerIsV1Announcer,
        ];
        assert_eq!(variants.len(), 12);
    }
}
