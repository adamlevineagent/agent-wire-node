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
    /// Market: no model is declared for this slot, or cached positive
    /// market-surface evidence shows 0 offers for every declared model.
    NoMarketOffersForSlot,
    /// Market: at least one offer exists for the model_list, but every
    /// offer's queue is full (current_queue_depth >= max_queue_depth).
    /// Distinct from `NoMarketOffersForSlot` because the condition is
    /// transient — operator guidance differs ("wait for drain" vs
    /// "the market doesn't serve this model").
    AllOffersSaturatedForModel,
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
    /// Phase 5 (§2.16.6 / §E): per-build circuit breaker tripped for
    /// this (build_id, slot, provider_type) tuple. Provider was removed
    /// from `effective_call_order` by the Decision builder; readiness
    /// itself may or may not still be Ready. Reset policy determines
    /// whether the breaker will untrip within the build.
    BreakerTripped { consecutive_failures: u32 },
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
pub struct OpenRouterReadinessStub;

impl ProviderReadiness for OpenRouterReadinessStub {
    fn can_dispatch_now(&self, _params: &ResolvedProviderParams) -> ReadinessResult {
        ReadinessResult::Ready
    }
}

// ── FleetReadiness (§2.6 Phase 4 real impl) ─────────────────────────────────
//
// Replaces the Phase 0a-1 FleetReadinessStub. Readiness ladder:
//   1. params.active == false                   → NotReady { Inactive }
//   2. params.model_list absent/empty           → NotReady { NoPeerHasModel }
//      (Interpreted per plan §2.6: operator declared no fleet models for
//      this tier, so the fleet doesn't serve this slot.)
//   3. No peer's `last_seen_at` is within
//      params.fleet_peer_min_staleness_secs     → NotReady { NoReachablePeer }
//   4. Of reachable peers, at least one has
//      announced any model in params.model_list → continue; else
//                                               → NotReady { NoPeerHasModel }
//   5. Every matching peer is a v1 announcer
//      (announce_protocol_version < 2)          → NotReady { PeerIsV1Announcer }
//   6. otherwise                                → Ready
//
// The fleet roster probe cache (`walker_fleet_probe`) is the sync-read
// shared state populated by the boot-spawned refresh task. FleetReadiness
// never blocks on async I/O — it only reads what the refresh task has
// already observed.
//
// Order rationale: inexpensive checks first (active flag, model_list),
// then roster-cache reads. The v1-announcer verdict comes LAST because
// it requires first confirming that reachable peers have matching
// models — a peer running v1 that doesn't have our model is
// `NoPeerHasModel` (cohort absent), not `PeerIsV1Announcer` (cohort
// present-but-incompatible). Plan §5.5.2 is explicit: NoPeerHasModel is
// reserved for same-protocol peers without the slug; PeerIsV1Announcer
// fires when the matching cohort is only v1.
#[allow(dead_code)]
pub struct FleetReadiness;

impl FleetReadiness {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl Default for FleetReadiness {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderReadiness for FleetReadiness {
    fn can_dispatch_now(&self, params: &ResolvedProviderParams) -> ReadinessResult {
        use crate::pyramid::walker_fleet_probe::snapshot_reachable_peers;

        // 1. Operator-disabled fleet provider.
        if !params.active {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            };
        }

        // 2. No model declared at this (slot, Fleet) scope. Per §2.6
        //    this is interpreted as "fleet doesn't serve this slot" —
        //    operator declared nothing, so no peer could match.
        let model_list = match params.model_list.as_ref() {
            Some(list) if !list.is_empty() => list,
            _ => {
                return ReadinessResult::NotReady {
                    reason: NotReadyReason::NoPeerHasModel,
                };
            }
        };

        // 3. Consult the roster probe cache with the resolver-supplied
        //    staleness cutoff. `None` at this layer shouldn't happen for
        //    Fleet (the Decision builder only populates it for Fleet),
        //    but defensively fall back to the SYSTEM_DEFAULT.
        let cutoff = params
            .fleet_peer_min_staleness_secs
            .unwrap_or(crate::pyramid::walker_resolver::FLEET_PEER_MIN_STALENESS_SECS_DEFAULT);
        let reachable = snapshot_reachable_peers(cutoff);
        if reachable.is_empty() {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::NoReachablePeer,
            };
        }

        // 4-5. Partition reachable peers by whether they announce at
        //      least one model in our model_list. If zero peers match,
        //      NoPeerHasModel. If the matching cohort is entirely v1
        //      announcers, PeerIsV1Announcer (§5.5.2 strict mode). If
        //      at least one v2+ peer matches, Ready.
        let mut any_peer_has_model = false;
        let mut any_matching_peer_is_v2_plus = false;
        for peer in reachable.iter() {
            let has_match = model_list
                .iter()
                .any(|m| peer.announced_models.iter().any(|am| am == m));
            if !has_match {
                continue;
            }
            any_peer_has_model = true;
            if !peer.is_v1_announcer {
                any_matching_peer_is_v2_plus = true;
            }
        }

        if !any_peer_has_model {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::NoPeerHasModel,
            };
        }

        if !any_matching_peer_is_v2_plus {
            // All matching peers are v1 announcers.
            return ReadinessResult::NotReady {
                reason: NotReadyReason::PeerIsV1Announcer,
            };
        }

        // `params.fleet_prefer_cached` is consulted by the dispatch path
        // for peer-selection ranking, not the readiness gate — any
        // matching v2+ peer existing is sufficient for readiness. The
        // ranking preference is plumbed through walker_decision into
        // the fleet dispatch branch separately.
        ReadinessResult::Ready
    }
}

// ── MarketReadiness (§2.6 Phase 3 real impl) ───────────────────────────────
//
// Replaces the Phase 0a-1 MarketReadinessStub. Readiness ladder:
//   1. params.active == false                  → Inactive
//   2. params.model_list absent/empty          → NoMarketOffersForSlot
//   3. Wire unreachable (consecutive failures
//      >= network_failure_backoff_threshold)   → WireUnreachable
//   4. credit balance < max_budget_credits     → InsufficientCredit
//   5. any declared model is absent from the
//      sync probe cache                       → Ready (/quote is authoritative)
//   6. every model in model_list has cache
//      entry with active_offers == 0           → NoMarketOffersForSlot
//   7. every model's cache entry flags
//      only_self_offers == true                → SelfDealing
//   8. every model's cache entry flags
//      all_offers_saturated == true            → AllOffersSaturatedForModel
//   9. otherwise                               → Ready
//
// The market-surface + balance + Wire-reachability caches are the sync-
// read shared state populated by the boot-spawned background task.
// MarketReadiness never blocks on network I/O — it only reads what the
// task has already observed.
//
// The credit check runs BEFORE the per-model-liquidity checks so an
// operator whose balance zeroed out sees `InsufficientCredit` regardless
// of whether a given slug is saturated or missing offers.
//
// Missing per-model cache entries are "unknown", not "no offers". The
// probe cache is advisory and can be cold at app boot; `/quote` is the
// authoritative market viability check. Only positive cached evidence
// may remove Market from the decision.
#[allow(dead_code)]
pub struct MarketReadiness;

impl MarketReadiness {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl Default for MarketReadiness {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderReadiness for MarketReadiness {
    fn can_dispatch_now(&self, params: &ResolvedProviderParams) -> ReadinessResult {
        use crate::pyramid::walker_market_probe::{read_cached_model, read_node_state};

        // 1. Operator-disabled market provider (bundled default is
        //    active=false — operator opts in).
        if !params.active {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            };
        }

        // 2. No model declared at this (slot, Market) scope.
        let model_list = match params.model_list.as_ref() {
            Some(list) if !list.is_empty() => list,
            _ => {
                return ReadinessResult::NotReady {
                    reason: NotReadyReason::NoMarketOffersForSlot,
                };
            }
        };

        // 3. Wire reachability — check the node-state failure counter.
        let node_state = read_node_state();
        if node_state.consecutive_network_failures >= params.network_failure_backoff_threshold {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::WireUnreachable,
            };
        }

        // 4. Budget check. `max_budget_credits = None` means "no cap" —
        //    skip. `Some(cap)` + cached balance < cap → InsufficientCredit.
        //    Unknown balance (None) is permissive — the Phase 3 probe
        //    hasn't observed the balance yet, so the readiness gate
        //    must not fail closed during cold-cache.
        if let (Some(cap), Some(balance)) = (params.max_budget_credits, node_state.credit_balance) {
            if balance < cap {
                return ReadinessResult::NotReady {
                    reason: NotReadyReason::InsufficientCredit,
                };
            }
        }

        // 5-7. Per-model cache consultation. Strictest verdict wins —
        //      if ANY model has headroom, we return Ready immediately.
        //      Otherwise aggregate the reasons: if every model says
        //      SelfDealing, that beats saturation (operator should see
        //      the self-deal diagnosis first); if every model says
        //      AllOffersSaturatedForModel, that beats NoMarketOffersForSlot.
        let mut any_headroom = false;
        let mut every_self_dealing = true;
        let mut every_all_saturated = true;
        let mut any_has_cache = false;
        let mut any_cache_miss = false;

        for model_id in model_list {
            match read_cached_model(model_id) {
                Some(entry) => {
                    any_has_cache = true;
                    if entry.active_offers == 0 {
                        every_self_dealing = false;
                        every_all_saturated = false;
                        continue;
                    }
                    if entry.only_self_offers {
                        // All offers are self-dealing; doesn't contribute
                        // to "saturation" accounting but does count as
                        // "no usable offer" for headroom purposes.
                        every_all_saturated = false;
                        continue;
                    } else {
                        every_self_dealing = false;
                    }
                    if entry.all_offers_saturated {
                        continue;
                    }
                    every_all_saturated = false;
                    any_headroom = true;
                }
                None => {
                    // Cache miss for this slug → unknown, not no offers.
                    // The first build step can race ahead of the async
                    // market-surface poller/projector; keep Market in the
                    // decision so the dispatch path can ask `/quote`.
                    any_cache_miss = true;
                    every_self_dealing = false;
                    every_all_saturated = false;
                }
            }
        }

        if any_headroom {
            return ReadinessResult::Ready;
        }

        if any_cache_miss {
            return ReadinessResult::Ready;
        }

        // No headroom anywhere. Pick the most specific reason.
        // Precedence: SelfDealing > AllOffersSaturated > NoOffers.
        // Cache-miss cases already returned Ready above; at this point
        // the remaining verdicts are backed by positive cached evidence.
        if any_has_cache && every_self_dealing {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::SelfDealing,
            };
        }
        if any_has_cache && every_all_saturated {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::AllOffersSaturatedForModel,
            };
        }
        ReadinessResult::NotReady {
            reason: NotReadyReason::NoMarketOffersForSlot,
        }
    }
}

// ── LocalReadiness (§2.6 Phase 2 real impl) ────────────────────────────────
//
// Replaces the Phase 0a-1 LocalReadinessStub. Readiness order:
//   1. params.active == false                  → NotReady { Inactive }
//   2. params.model_list absent/empty          → NotReady { NoModelListForSlot }
//   3. probe cache miss OR reachable==false    → NotReady { OllamaOffline }
//   4. probe cache has NO overlap with
//      params.model_list                       → NotReady { OllamaOffline }
//   5. otherwise                               → Ready
//
// The probe cache is the sync-read shared state populated by the
// background task spawned at boot. `LocalReadiness` never blocks on
// network I/O — it only reads what the task has already stored.
//
// The probe check is deliberately positioned after the cheaper checks
// so a common misconfiguration ("operator disabled local" / "operator
// never declared models at this slot") returns a specific reason
// without consulting the probe cache at all.
#[allow(dead_code)]
pub struct LocalReadiness {
    /// Handle to the shared Ollama probe cache. `Default::default()`
    /// wires up to the global singleton; tests that need explicit
    /// setup call `seed_cache_for_test` on `walker_ollama_probe`
    /// directly before invoking readiness.
    probe_handle: crate::pyramid::walker_ollama_probe::LocalProbeHandle,
}

impl LocalReadiness {
    /// Production constructor. Reads the global cache the background
    /// probe task writes into.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            probe_handle: crate::pyramid::walker_ollama_probe::LocalProbeHandle::global(),
        }
    }
}

impl Default for LocalReadiness {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderReadiness for LocalReadiness {
    fn can_dispatch_now(&self, params: &ResolvedProviderParams) -> ReadinessResult {
        // 1. Operator-disabled local provider.
        if !params.active {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            };
        }

        // 2. No model declared at this (slot, Local) scope.
        let model_list = match params.model_list.as_ref() {
            Some(list) if !list.is_empty() => list,
            _ => {
                return ReadinessResult::NotReady {
                    reason: NotReadyReason::NoModelListForSlot,
                };
            }
        };

        // 3. Consult the probe cache. Absent entry = "background task
        // has not yet populated this base_url" → conservative offline
        // until the first probe lands. The SYSTEM_DEFAULT base_url is
        // used when the resolver didn't surface one (shouldn't happen
        // for Local, but defensive).
        let base_url = params.ollama_base_url.clone().unwrap_or_else(|| {
            crate::pyramid::walker_resolver::OLLAMA_BASE_URL_DEFAULT.to_string()
        });
        let probe = match self.probe_handle.probe_for(&base_url) {
            Some(p) => p,
            None => {
                return ReadinessResult::NotReady {
                    reason: NotReadyReason::OllamaOffline,
                };
            }
        };
        if !probe.reachable {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline,
            };
        }

        // 4. At least one declared model must be installed in Ollama.
        // `probe.models` comes from /api/tags and uses exact slugs
        // (e.g. `llama3.2:latest`, `gemma3:27b`). Matching is exact
        // so an operator declaring `llama3.2` in walker_provider_local
        // but with `llama3.2:latest` installed will NOT match — this
        // is intentional; tag-ambiguity is a real misconfig mode and
        // the Decision chronicle's NotReady reason surfaces it cleanly.
        let any_installed = model_list
            .iter()
            .any(|m| probe.models.iter().any(|p| p == m));
        if !any_installed {
            return ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline,
            };
        }

        ReadinessResult::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_stub_returns_ready() {
        // LocalReadinessStub → LocalReadiness (Phase 2).
        // MarketReadinessStub → MarketReadiness (Phase 3).
        // FleetReadinessStub → FleetReadiness (Phase 4).
        // OpenRouterReadinessStub stays stub — natural follow-up; no
        // phase explicitly owns it (see Phase 4 scope note F).
        let p = ResolvedProviderParams::default();
        assert!(matches!(
            OpenRouterReadinessStub.can_dispatch_now(&p),
            ReadinessResult::Ready
        ));
    }

    // ── LocalReadiness (Phase 2) ─────────────────────────────────────────────

    use crate::pyramid::walker_ollama_probe::{
        clear_cache_for_tests, invalidate_cached_probe, write_cached_probe, CachedProbe,
    };

    fn params_for_local(
        active: bool,
        model_list: Option<Vec<String>>,
        base_url: &str,
    ) -> ResolvedProviderParams {
        ResolvedProviderParams {
            active,
            model_list,
            ollama_base_url: Some(base_url.to_string()),
            ollama_probe_interval_secs: Some(300),
            ..ResolvedProviderParams::default()
        }
    }

    #[test]
    fn local_readiness_inactive_returns_not_ready_inactive() {
        let p = params_for_local(
            false,
            Some(vec!["gemma3:27b".into()]),
            "http://test-inactive.invalid:11434/v1",
        );
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            } => {}
            other => panic!("expected Inactive, got {:?}", other),
        }
    }

    #[test]
    fn local_readiness_no_model_list_returns_not_ready_no_model_list_for_slot() {
        // model_list = None
        let p = params_for_local(true, None, "http://test-nomodel.invalid:11434/v1");
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoModelListForSlot,
            } => {}
            other => panic!("expected NoModelListForSlot (None), got {:?}", other),
        }
        // model_list = Some(empty)
        let p2 = params_for_local(
            true,
            Some(vec![]),
            "http://test-emptymodel.invalid:11434/v1",
        );
        match LocalReadiness::new().can_dispatch_now(&p2) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoModelListForSlot,
            } => {}
            other => panic!("expected NoModelListForSlot (empty), got {:?}", other),
        }
    }

    #[test]
    fn local_readiness_ollama_offline_returns_not_ready_ollama_offline() {
        // No cache entry for this base_url → OllamaOffline.
        let url = "http://test-offline.invalid:11434/v1";
        invalidate_cached_probe(url);
        let p = params_for_local(true, Some(vec!["gemma3:27b".into()]), url);
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline,
            } => {}
            other => panic!("expected OllamaOffline (absent cache), got {:?}", other),
        }

        // Cache entry with reachable=false → OllamaOffline.
        write_cached_probe(
            url,
            CachedProbe {
                reachable: false,
                models: vec![],
                at: std::time::Instant::now(),
            },
        );
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline,
            } => {}
            other => panic!("expected OllamaOffline (reachable=false), got {:?}", other),
        }
        invalidate_cached_probe(url);
    }

    #[test]
    fn local_readiness_model_in_installed_list_returns_ready() {
        let url = "http://test-ready.invalid:11434/v1";
        write_cached_probe(
            url,
            CachedProbe {
                reachable: true,
                models: vec!["gemma3:27b".into(), "llama3.2:latest".into()],
                at: std::time::Instant::now(),
            },
        );
        let p = params_for_local(true, Some(vec!["gemma3:27b".into()]), url);
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::Ready => {}
            other => panic!("expected Ready, got {:?}", other),
        }
        invalidate_cached_probe(url);
    }

    #[test]
    fn local_readiness_model_not_installed_returns_not_ready_ollama_offline() {
        let url = "http://test-nomatch.invalid:11434/v1";
        write_cached_probe(
            url,
            CachedProbe {
                reachable: true,
                // Operator declared `gemma3:27b` but Ollama has only llama3.
                models: vec!["llama3.2:latest".into()],
                at: std::time::Instant::now(),
            },
        );
        let p = params_for_local(true, Some(vec!["gemma3:27b".into()]), url);
        match LocalReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline,
            } => {}
            other => panic!("expected OllamaOffline (no model overlap), got {:?}", other),
        }
        invalidate_cached_probe(url);
    }

    #[test]
    fn local_readiness_probe_cached_hits_dont_reprobe() {
        // The trait is sync; readiness never performs I/O itself. A
        // cache write is the only source of truth. This test verifies
        // that successive readiness calls against the same base_url
        // read the same entry (no self-mutation, no staleness advance).
        let url = "http://test-cached.invalid:11434/v1";
        write_cached_probe(
            url,
            CachedProbe {
                reachable: true,
                models: vec!["gemma3:27b".into()],
                at: std::time::Instant::now(),
            },
        );
        let p = params_for_local(true, Some(vec!["gemma3:27b".into()]), url);
        let r = LocalReadiness::new();
        for _ in 0..5 {
            assert!(matches!(r.can_dispatch_now(&p), ReadinessResult::Ready));
        }
        invalidate_cached_probe(url);
    }

    // NOTE: Phase 3 intentionally does NOT reintroduce a
    // `zzz_local_readiness_cache_cleanup` helper that wiped the whole
    // Ollama probe cache at the end of the module. That helper was a
    // race against parallel tests in the same module — it would clear
    // probe entries mid-run of a sibling test. Each LocalReadiness test
    // already owns a unique base_url + calls `invalidate_cached_probe`
    // on exit, so the integration-test isolation it was intended to
    // provide is redundant.

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
            NotReadyReason::AllOffersSaturatedForModel,
            NotReadyReason::SelfDealing,
            NotReadyReason::NoReachablePeer,
            NotReadyReason::NoPeerHasModel,
            NotReadyReason::PeerIsV1Announcer,
        ];
        assert_eq!(variants.len(), 13);
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

    // ── MarketReadiness (Phase 3) ────────────────────────────────────────────
    //
    // Unit tests for the real impl. Each test sets up a minimal world
    // via the walker_market_probe test helpers. Tests that mutate the
    // shared global node_state cache (credit balance, network failure
    // counters, self-handles) must serialize access — parallel test
    // execution would otherwise have one test clear state mid-run of
    // another. Use a static Mutex acquired at the top of every
    // MarketReadiness test that touches node_state.
    //
    // Per-model entries (`write_cached_model`) are safe to exercise in
    // parallel because every test uses a unique model_id string.

    use crate::pyramid::walker_market_probe::{
        clear_model_cache_for_tests, clear_node_state_for_tests, invalidate_cached_model,
        node_state_test_lock, record_network_failure, set_credit_balance, set_self_handles,
        write_cached_model, CachedMarketModel, CachedOffer,
    };

    /// Serialize MarketReadiness tests that mutate `node_state_cache`
    /// (credit balance, failure counter, self-handles) through the
    /// shared module-level `node_state_test_lock` so sibling tests in
    /// walker_decision + walker_market_probe see the same ordering.
    fn market_test_lock() -> &'static std::sync::Mutex<()> {
        node_state_test_lock()
    }

    fn params_for_market(
        active: bool,
        model_list: Option<Vec<String>>,
        max_budget: Option<i64>,
    ) -> ResolvedProviderParams {
        ResolvedProviderParams {
            active,
            model_list,
            max_budget_credits: max_budget,
            // Tight threshold so WireUnreachable tests can drive the
            // counter to breach without excessive iteration.
            network_failure_backoff_threshold: 3,
            ..ResolvedProviderParams::default()
        }
    }

    fn cached_with_offers(
        active_offers: i64,
        all_saturated: bool,
        only_self: bool,
    ) -> CachedMarketModel {
        // Synthesize offers_detail that MATCHES the flags — the flag
        // computation runs in walker_market_probe::project_model; tests
        // here build CachedMarketModel directly to exercise readiness
        // without driving the projector.
        let offers = if active_offers > 0 {
            vec![CachedOffer {
                offer_id: "o1".into(),
                node_handle: if only_self {
                    "me".into()
                } else {
                    "other".into()
                },
                operator_handle: "op".into(),
                typical_serve_ms_p50_7d: Some(1000.0),
                execution_concurrency: 1,
                current_queue_depth: if all_saturated { 5 } else { 0 },
                max_queue_depth: 5,
            }]
        } else {
            vec![]
        };
        CachedMarketModel {
            active_offers,
            all_offers_saturated: all_saturated,
            only_self_offers: only_self,
            model_typical_serve_ms_p50_7d: Some(1000.0),
            offers_detail: offers,
            at: std::time::Instant::now(),
        }
    }

    #[test]
    fn test_market_readiness_inactive_returns_not_ready_inactive() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let p = params_for_market(false, Some(vec!["m1".into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            } => {}
            other => panic!("expected Inactive, got {:?}", other),
        }
    }

    #[test]
    fn test_market_readiness_no_model_list_returns_not_ready_no_market_offers_for_slot() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        // None
        let p = params_for_market(true, None, None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoMarketOffersForSlot,
            } => {}
            other => panic!("expected NoMarketOffersForSlot (None), got {:?}", other),
        }
        // Empty
        let p2 = params_for_market(true, Some(vec![]), None);
        match MarketReadiness::new().can_dispatch_now(&p2) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoMarketOffersForSlot,
            } => {}
            other => panic!("expected NoMarketOffersForSlot (empty), got {:?}", other),
        }
    }

    #[test]
    fn test_market_readiness_surface_empty_returns_not_ready_no_market_offers_for_slot() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let slug = "market-readiness-empty-surface";
        invalidate_cached_model(slug);
        // Cache has the slug, but active_offers == 0.
        write_cached_model(slug, cached_with_offers(0, false, false));
        let p = params_for_market(true, Some(vec![slug.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoMarketOffersForSlot,
            } => {}
            other => panic!("expected NoMarketOffersForSlot, got {:?}", other),
        }
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_cold_model_cache_is_ready() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let slug = "market-readiness-cold-cache";
        invalidate_cached_model(slug);
        let p = params_for_market(true, Some(vec![slug.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::Ready => {}
            other => panic!("expected Ready for cold market cache, got {:?}", other),
        }
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_all_offers_saturated_returns_all_saturated_variant() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let slug = "market-readiness-all-saturated";
        invalidate_cached_model(slug);
        write_cached_model(slug, cached_with_offers(1, true, false));
        let p = params_for_market(true, Some(vec![slug.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::AllOffersSaturatedForModel,
            } => {}
            other => panic!("expected AllOffersSaturatedForModel, got {:?}", other),
        }
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_insufficient_credit_returns_insufficient_credit() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let slug = "market-readiness-insufficient-credit";
        invalidate_cached_model(slug);
        // Cache says offers exist with headroom, but balance < cap.
        write_cached_model(slug, cached_with_offers(1, false, false));
        set_credit_balance(Some(100));
        let p = params_for_market(true, Some(vec![slug.into()]), Some(1_000));
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::InsufficientCredit,
            } => {}
            other => panic!("expected InsufficientCredit, got {:?}", other),
        }
        clear_node_state_for_tests();
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_wire_unreachable_returns_wire_unreachable() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        // Drive the failure counter over the default threshold (3).
        record_network_failure();
        record_network_failure();
        record_network_failure();
        let slug = "market-readiness-wire-unreachable";
        write_cached_model(slug, cached_with_offers(1, false, false));
        let p = params_for_market(true, Some(vec![slug.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::WireUnreachable,
            } => {}
            other => panic!("expected WireUnreachable, got {:?}", other),
        }
        clear_node_state_for_tests();
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_self_dealing_filters_own_offers() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        set_self_handles("me", "op");
        let slug = "market-readiness-self-dealing";
        invalidate_cached_model(slug);
        // Cache says offers exist, but flagged only_self_offers.
        write_cached_model(slug, cached_with_offers(1, false, true));
        let p = params_for_market(true, Some(vec![slug.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::SelfDealing,
            } => {}
            other => panic!("expected SelfDealing, got {:?}", other),
        }
        clear_node_state_for_tests();
        invalidate_cached_model(slug);
    }

    #[test]
    fn test_market_readiness_ready_when_any_model_has_headroom() {
        let _g = market_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_model_cache_for_tests();
        clear_node_state_for_tests();
        let a = "market-readiness-headroom-a";
        let b = "market-readiness-headroom-b";
        invalidate_cached_model(a);
        invalidate_cached_model(b);
        // One saturated, one with headroom → Ready (strictest-wins does
        // NOT mean strictest-aggregates; ANY headroom → Ready).
        write_cached_model(a, cached_with_offers(1, true, false));
        write_cached_model(b, cached_with_offers(1, false, false));
        let p = params_for_market(true, Some(vec![a.into(), b.into()]), None);
        match MarketReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::Ready => {}
            other => panic!("expected Ready (any-model-headroom), got {:?}", other),
        }
        invalidate_cached_model(a);
        invalidate_cached_model(b);
    }

    // ── FleetReadiness (Phase 4) ─────────────────────────────────────────────
    //
    // Unit tests for the real impl. Each test sets up a minimal world
    // via the walker_fleet_probe test helpers. Tests mutate the shared
    // global probe cache; serialize via the module-level
    // `fleet_probe_test_lock` so sibling tests in walker_decision +
    // walker_fleet_probe + the integration test crate see the same
    // ordering.

    use crate::pyramid::walker_fleet_probe::{
        clear_fleet_cache_for_tests, fleet_probe_test_lock, write_cached_peer, CachedFleetPeer,
    };

    /// Serialize FleetReadiness tests that mutate the fleet probe
    /// cache. Routed through `fleet_probe_test_lock` so the lock is
    /// shared with sibling unit tests + the integration test crate.
    fn fleet_test_lock() -> &'static std::sync::Mutex<()> {
        fleet_probe_test_lock()
    }

    fn params_for_fleet(
        active: bool,
        model_list: Option<Vec<String>>,
        min_staleness_secs: u64,
    ) -> ResolvedProviderParams {
        ResolvedProviderParams {
            active,
            model_list,
            fleet_peer_min_staleness_secs: Some(min_staleness_secs),
            fleet_prefer_cached: Some(true),
            ..ResolvedProviderParams::default()
        }
    }

    fn make_cached_peer(
        node_id: &str,
        models: &[&str],
        last_seen: chrono::DateTime<chrono::Utc>,
        is_v1: bool,
    ) -> CachedFleetPeer {
        CachedFleetPeer {
            node_id: node_id.to_string(),
            node_handle: Some(format!("@op/{node_id}")),
            announced_models: models.iter().map(|s| s.to_string()).collect(),
            last_seen_at: last_seen,
            is_v1_announcer: is_v1,
        }
    }

    #[test]
    fn test_fleet_readiness_inactive_returns_not_ready_inactive() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let p = params_for_fleet(false, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::Inactive,
            } => {}
            other => panic!("expected Inactive, got {:?}", other),
        }
    }

    #[test]
    fn test_fleet_readiness_no_model_list_returns_not_ready_no_peer_has_model() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        // model_list = None
        let p = params_for_fleet(true, None, 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoPeerHasModel,
            } => {}
            other => panic!("expected NoPeerHasModel (None), got {:?}", other),
        }
        // model_list = Some(empty)
        let p2 = params_for_fleet(true, Some(vec![]), 300);
        match FleetReadiness::new().can_dispatch_now(&p2) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoPeerHasModel,
            } => {}
            other => panic!("expected NoPeerHasModel (empty), got {:?}", other),
        }
    }

    #[test]
    fn test_fleet_readiness_no_reachable_peers_returns_not_ready_no_reachable_peer() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        // Empty cache → no reachable peers.
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoReachablePeer,
            } => {}
            other => panic!("expected NoReachablePeer (empty cache), got {:?}", other),
        }
    }

    #[test]
    fn test_fleet_readiness_peer_too_stale_filtered_out() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let stale_when = chrono::Utc::now() - chrono::Duration::seconds(1000);
        write_cached_peer(make_cached_peer(
            "stale-peer",
            &["gemma3:27b"],
            stale_when,
            false,
        ));
        // Cutoff 300s, peer 1000s old → not reachable.
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoReachablePeer,
            } => {}
            other => panic!("expected NoReachablePeer (stale), got {:?}", other),
        }
        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_fleet_readiness_no_peer_has_matching_model_returns_no_peer_has_model() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        // Peer reachable but announces a different model.
        write_cached_peer(make_cached_peer(
            "other-model-peer",
            &["llama3.2:latest"],
            chrono::Utc::now(),
            false,
        ));
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::NoPeerHasModel,
            } => {}
            other => panic!("expected NoPeerHasModel (no match), got {:?}", other),
        }
        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_fleet_readiness_all_matching_peers_are_v1_announcers_returns_peer_is_v1_announcer() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        // Two peers: one matches model but is v1; another is v2 but no
        // matching model. The matching cohort is exclusively v1.
        write_cached_peer(make_cached_peer(
            "v1-match",
            &["gemma3:27b"],
            chrono::Utc::now(),
            true,
        ));
        write_cached_peer(make_cached_peer(
            "v2-no-match",
            &["llama3.2:latest"],
            chrono::Utc::now(),
            false,
        ));
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::NotReady {
                reason: NotReadyReason::PeerIsV1Announcer,
            } => {}
            other => panic!("expected PeerIsV1Announcer, got {:?}", other),
        }
        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_fleet_readiness_at_least_one_peer_has_model_returns_ready() {
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        write_cached_peer(make_cached_peer(
            "v2-match",
            &["gemma3:27b"],
            chrono::Utc::now(),
            false,
        ));
        // Extra peer with no match — must not affect the Ready verdict.
        write_cached_peer(make_cached_peer(
            "v2-other",
            &["llama3.2:latest"],
            chrono::Utc::now(),
            false,
        ));
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        match FleetReadiness::new().can_dispatch_now(&p) {
            ReadinessResult::Ready => {}
            other => panic!("expected Ready, got {:?}", other),
        }
        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_fleet_readiness_mixed_v1_v2_matches_returns_ready() {
        // Both a v1 and a v2 peer match — v2 wins and overall is Ready
        // (PeerIsV1Announcer only fires when the entire matching cohort
        // is v1, per §5.5.2).
        let _g = fleet_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        write_cached_peer(make_cached_peer(
            "v1-match",
            &["gemma3:27b"],
            chrono::Utc::now(),
            true,
        ));
        write_cached_peer(make_cached_peer(
            "v2-match",
            &["gemma3:27b"],
            chrono::Utc::now(),
            false,
        ));
        let p = params_for_fleet(true, Some(vec!["gemma3:27b".into()]), 300);
        assert!(matches!(
            FleetReadiness::new().can_dispatch_now(&p),
            ReadinessResult::Ready
        ));
        clear_fleet_cache_for_tests();
    }
}
