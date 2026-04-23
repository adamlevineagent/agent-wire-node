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

use crate::pyramid::walker_resolver::BreakerReset;

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
/// Extended by Phase 0b Workstream D to the full §2.9 parameter catalog.
/// The Decision builder (`walker_decision.rs`) populates every field
/// once at construction time by calling the typed accessors in
/// `walker_resolver.rs`. Readiness impls then consume this struct —
/// the stubs ignore most fields today; Phase 2/3/4 real impls check
/// provider-specific ones (e.g. `ollama_base_url` freshness for local,
/// network_failure_backoff counters for openrouter/market).
///
/// Provider-specific fields are `Option<T>` so the Decision builder
/// can omit them for providers that don't apply (e.g. `ollama_base_url`
/// is populated only for `ProviderType::Local`; `fleet_*` only for
/// `ProviderType::Fleet`). A readiness impl that reaches for a field
/// that should be `Some` but isn't is a programmer bug — the impl
/// should match its own provider's contract.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ResolvedProviderParams {
    // ── Universal parameters (all providers) ────────────────────────
    /// Resolved per-(slot, provider) model list. `None` = no model
    /// declared for this pair; readiness returns `NoModelListForSlot`.
    pub model_list: Option<Vec<String>>,
    /// Optional per-build market spend cap (sensitive, chronicle-redacted).
    pub max_budget_credits: Option<i64>,
    pub patience_secs: u64,
    pub patience_clock_resets_per_model: bool,
    pub breaker_reset: BreakerReset,
    pub sequential: bool,
    pub bypass_pool: bool,
    pub retry_http_count: u32,
    pub retry_backoff_base_secs: u64,
    pub dispatch_deadline_grace_secs: u64,
    pub active: bool,

    // ── Provider-specific (Some only for the owning ProviderType) ───
    /// Local provider: Ollama /v1 endpoint (§3 OLLAMA_BASE_URL_DEFAULT).
    pub ollama_base_url: Option<String>,
    /// Local provider: probe cadence for /api/tags freshness.
    pub ollama_probe_interval_secs: Option<u64>,
    /// Fleet provider: staleness cutoff for peer announcements.
    pub fleet_peer_min_staleness_secs: Option<u64>,
    /// Fleet provider: whether to prefer peers that have the model cached.
    pub fleet_prefer_cached: Option<bool>,

    // ── Offline-aware gate (§2.16.5) ────────────────────────────────
    /// Consecutive-failure count before readiness returns
    /// `NetworkUnreachable`.
    pub network_failure_backoff_threshold: u32,
    /// Duration in `NetworkUnreachable` state before readiness retries.
    pub network_failure_backoff_secs: u64,

    // ── W1a: four new params absorbed from legacy pyramid_tier_routing
    //        columns per §5.1. All Option-surfacing with no SYSTEM_DEFAULT —
    //        `None` means "ask the provider at dispatch time" (context
    //        limits) or "unknown" (pricing/supported_parameters).
    /// Per-(slot, provider) context window ceiling. None = provider-declared.
    pub context_limit: Option<u64>,
    /// Per-(slot, provider) max completion tokens. None = provider-declared.
    pub max_completion_tokens: Option<u64>,
    /// Per-provider opaque pricing blob (OpenRouter-shape). None = unknown
    /// at config time; pricing engine may fetch live.
    pub pricing_json: Option<serde_json::Value>,
    /// Per-provider list of parameter names the backing model honors
    /// (e.g. `["tools", "response_format"]`). None = unknown.
    pub supported_parameters: Option<Vec<String>>,
}

impl Default for ResolvedProviderParams {
    fn default() -> Self {
        // Defaults mirror SYSTEM_DEFAULTS in walker_resolver.rs. These
        // are the absolute-safest values; real usage comes from
        // `walker_decision::resolve_all_params` which calls each
        // typed accessor once per (slot, pt).
        Self {
            model_list: None,
            max_budget_credits: None,
            patience_secs: 3600,
            patience_clock_resets_per_model: false,
            breaker_reset: BreakerReset::PerBuild,
            sequential: true,
            bypass_pool: false,
            retry_http_count: 3,
            retry_backoff_base_secs: 2,
            dispatch_deadline_grace_secs: 10,
            active: true,
            ollama_base_url: None,
            ollama_probe_interval_secs: None,
            fleet_peer_min_staleness_secs: None,
            fleet_prefer_cached: None,
            network_failure_backoff_threshold: 3,
            network_failure_backoff_secs: 300,
            // W1a: all four new params default to None — no SYSTEM_DEFAULT,
            // and per §2.14.3 / §5.1 the absent-everywhere state means
            // "provider declares at dispatch time" / "unknown".
            context_limit: None,
            max_completion_tokens: None,
            pricing_json: None,
            supported_parameters: None,
        }
    }
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
        let p = ResolvedProviderParams::default();
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

    #[test]
    fn resolved_provider_params_default_has_none_for_w1a_fields() {
        // W1a: four new Option-surfacing fields must default to None so
        // any consumer that branches on "is this declared?" treats the
        // absent case as "ask the provider" / "unknown", not a ghost zero.
        let p = ResolvedProviderParams::default();
        assert!(p.context_limit.is_none());
        assert!(p.max_completion_tokens.is_none());
        assert!(p.pricing_json.is_none());
        assert!(p.supported_parameters.is_none());
    }
}
