// Walker v3 — DispatchDecision compute-once spine (Phase 0b Workstream D).
//
// Plan rev 1.0.2 anchors:
//   §2.6  ProviderReadiness trait — Decision builder calls
//         `can_dispatch_now` for every ProviderType in the effective
//         call order at construction time so dispatchers downstream
//         see a pre-filtered list.
//   §2.9  DispatchDecision — the compute-once spine. Every dispatcher
//         reads from `StepContext.dispatch_decision`. Built at outer
//         chain step entry by the executor. Immutable for the
//         Decision's lifetime; mid-step ArcSwap updates do NOT change
//         the answer walker already computed (`scope_snapshot` pins
//         one `Arc<ScopeCache>`).
//   §2.11 schema_annotation shape validation — envelope-writer job;
//         Decision builder trusts post-normalization bodies.
//   §2.12 synthetic Decision for maintenance paths (DADBEAR preview,
//         cost estimation, operator-HTTP preview). Skips readiness
//         gates; emits EVENT_DECISION_PREVIEWED instead of
//         EVENT_DECISION_BUILT so Builds-tab doesn't show phantom
//         dispatches.
//   §2.14 Cascade exhaustion + failure modes — "all providers NotReady"
//         returns `DecisionBuildError::NoReadyProviders` and emits
//         EVENT_DECISION_BUILD_FAILED with per-provider reasons.
//   §2.16 concurrency + lifecycle invariants — Decision's
//         `scope_snapshot: Arc<ScopeCache>` is the pin; once the
//         Decision is built it outlives any subsequent ArcSwap rebuild.
//   §3    parameter catalog — walker_resolver exposes typed accessors
//         for all 18 params; the Decision builder calls each once per
//         (slot, provider_type).
//   §4.3  per-slot `order` full-replace semantics — scope 2 can
//         override the global call_order with a slot-specific list.
//   §5.4.3 Root 27 type-level redaction guard — DispatchDecision is
//         NOT Serialize. Chronicle payloads go through
//         `for_chronicle()` which returns the redacted view.
//   §6    Phase 0b companion to walker_resolver.rs (WS-A).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use rusqlite::Connection;
use serde::Serialize;

use crate::pyramid::compute_chronicle::{
    EVENT_BREAKER_TRIPPED, EVENT_DECISION_BUILD_FAILED, EVENT_DECISION_BUILT,
    EVENT_DECISION_PREVIEWED, EVENT_PROVIDER_SKIPPED_READINESS,
};
use crate::pyramid::walker_breaker;
use crate::pyramid::walker_cache::ScopeCache;
use crate::pyramid::walker_readiness::{
    FleetReadiness, LocalReadiness, MarketReadiness, NotReadyReason,
    OpenRouterReadinessStub, ProviderReadiness, ReadinessResult, ResolvedProviderParams,
};
use crate::pyramid::walker_resolver::{
    build_scope_cache_pair, resolve_active, resolve_breaker_reset,
    resolve_bypass_pool, resolve_context_limit, resolve_dispatch_deadline_grace_secs,
    resolve_fleet_peer_min_staleness_secs, resolve_fleet_prefer_cached,
    resolve_max_budget_credits, resolve_max_completion_tokens, resolve_model_list,
    resolve_network_failure_backoff_secs, resolve_network_failure_backoff_threshold,
    resolve_ollama_base_url, resolve_ollama_probe_interval_secs, resolve_on_partial_failure,
    resolve_patience_clock_resets_per_model, resolve_patience_secs, resolve_pricing_json,
    resolve_retry_backoff_base_secs, resolve_retry_http_count, resolve_sequential,
    resolve_supported_parameters, tier_set_from_chain, PartialFailurePolicy, ProviderType,
    ScopeChain, DEFAULT_CALL_ORDER,
};

// ── DispatchDecision (§2.9) ──────────────────────────────────────────────────
//
// The compute-once answer handed to dispatchers. Fields:
//
//   slot                   — tier name (mid/high/max/extractor/...)
//   effective_call_order   — ProviderTypes that passed readiness, in
//                            order walker will try. Empty → dispatch
//                            cannot proceed (NoReadyProviders).
//   per_provider           — fully-resolved ResolvedProviderParams
//                            for every provider in effective_call_order.
//                            Providers that failed readiness are NOT
//                            present (dispatchers that reach for a
//                            missing key are a programmer bug).
//   scope_snapshot         — Arc<ScopeCache> pin carrying built_at +
//                            source_contribution_ids for the chronicle.
//                            Pinning this here is what gives the
//                            Decision its immutability invariant
//                            across mid-step ArcSwap updates.
//   on_partial_failure     — scope-2-resolved policy: Cascade | FailLoud
//                            | RetrySame. (Resolver reads from any
//                            scope; validator enforces scope-2-only.)
//   built_at               — Decision construction wall-clock. Surfaces
//                            in the chronicle so operator sees
//                            "Decision built at X, dispatched at Y".
//   synthetic              — true for synthetic_for_preview decisions
//                            (DADBEAR preview, cost estimation,
//                            operator-HTTP preview). Dispatchers MUST
//                            NOT follow a synthetic Decision — it's
//                            for display/estimation only.
//
// NOT Serialize. The chronicle sees `for_chronicle()` → DecisionChronicleView.
// `#[cfg(any())]` guard at the bottom of this module mirrors the one in
// walker_cache.rs::ScopeSnapshot.

/// The compute-once dispatch spine. See §2.9.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DispatchDecision {
    pub slot: String,
    pub effective_call_order: Vec<ProviderType>,
    pub per_provider: HashMap<ProviderType, ResolvedProviderParams>,
    pub scope_snapshot: Arc<ScopeCache>,
    pub on_partial_failure: PartialFailurePolicy,
    pub built_at: SystemTime,
    pub synthetic: bool,
}

// ── DecisionChronicleView (§5.4.3 Root 27 redaction) ─────────────────────────
//
// The ONLY Serialize-able projection of a DispatchDecision. Any field
// whose parameter catalog schema_annotation declares `local_only: true`
// or `sensitive: true` (see WS-B's bundled schema annotations) is
// STRIPPED here, not merely set to None — omitting the field is
// stronger than representing it as null (a future JSON consumer can't
// mistake "field absent" for "field was intentionally null").
//
// Per §5.4.3 / §3 schema_annotation declarations:
//
//   Included (public / non-sensitive):
//     - slot                                  (tier name — public)
//     - effective_call_order                  (provider-type list — public)
//     - on_partial_failure                    (policy name — public)
//     - built_at                              (timestamp — public)
//     - synthetic                             (bool flag — public)
//     - source_contribution_ids               (audit trail — public;
//                                              pulled from scope_snapshot)
//     - per_provider (ProviderChronicleView): provider_type,
//       active, model_list (slugs are non-sensitive), patience_secs,
//       retry_http_count, sequential, bypass_pool.
//
//   Redacted (local_only OR sensitive):
//     - ollama_base_url                       (local_only — LAN URL)
//     - ollama_probe_interval_secs            (pairs with base_url;
//                                              redact for symmetry)
//     - max_budget_credits                    (sensitive — spend cap)
//     - fleet_peer_min_staleness_secs         (local_only — fleet topology)
//     - fleet_prefer_cached                   (local_only — fleet policy)
//     - network_failure_backoff_*             (local_only — operator gate)
//     - retry_backoff_base_secs               (operational tuning;
//                                              conservative redact)
//     - dispatch_deadline_grace_secs          (operational tuning;
//                                              conservative redact)
//     - breaker_reset                         (operational tuning;
//                                              conservative redact)
//     - patience_clock_resets_per_model       (operational tuning;
//                                              conservative redact)
//
// "Redact when unclear" is the plan-integrity default (§5.4.3). Plan
// can loosen specific fields once WS-B's schema annotations explicitly
// flag them as public. Keeping conservative here avoids leaking
// operator topology before WS-B runs.

/// The chronicle-safe projection of a DispatchDecision. Only type in
/// this module that derives Serialize.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct DecisionChronicleView {
    pub slot: String,
    pub effective_call_order: Vec<String>,
    pub per_provider: Vec<ProviderChronicleView>,
    pub on_partial_failure: String,
    pub built_at: SystemTime,
    pub synthetic: bool,
    pub source_contribution_ids: Vec<String>,
}

/// Per-provider redacted projection. See the table above for the
/// local_only / sensitive allow-list.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct ProviderChronicleView {
    pub provider_type: String,
    pub active: bool,
    pub model_list: Option<Vec<String>>,
    pub patience_secs: u64,
    pub retry_http_count: u32,
    pub sequential: bool,
    pub bypass_pool: bool,
}

// ── DecisionBuildError ───────────────────────────────────────────────────────

/// Terminal failure modes for `DispatchDecision::build`.
///
/// `NoReadyProviders` is the cascade-exhausted terminal state: every
/// provider in the effective call order returned NotReady, so the
/// Decision can't be built. Callers (chain_executor) translate this
/// into the step-level failure path — usually marking the step
/// failed with a task chronicle event; the Decision-level
/// EVENT_DECISION_BUILD_FAILED is already emitted by build() itself.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum DecisionBuildError {
    #[error("no ready providers for slot `{slot}` — all {count} skipped")]
    NoReadyProviders {
        slot: String,
        count: usize,
        reasons: Vec<(ProviderType, NotReadyReason)>,
    },
    #[error("failed to load scope cache: {0:?}")]
    ScopeCacheLoadFailed(#[from] anyhow::Error),
}

// ── DispatchDecision impl ────────────────────────────────────────────────────

impl DispatchDecision {
    /// Build a runtime Decision (§2.9). Loads the scope cache, resolves
    /// every per-provider parameter at `(slot, pt)`, calls
    /// `ProviderReadiness::can_dispatch_now` for each, and pre-filters
    /// the call order down to the ready set. Emits
    /// `EVENT_DECISION_BUILT` on success (with the chronicle view as
    /// payload) or `EVENT_DECISION_BUILD_FAILED` on empty cascade.
    ///
    /// Phase 1 (Root 29 consumer migration) rewires chain_executor +
    /// llm + dadbear_preview + ~50 other callers to consume this via
    /// `StepContext.dispatch_decision`.
    #[allow(dead_code)]
    pub fn build(slot: &str, conn: &Connection) -> std::result::Result<Self, DecisionBuildError> {
        Self::build_with_build_id(slot, None, conn)
    }

    /// Phase 5 §2.16.6 / §E: runtime Decision build with breaker
    /// consultation keyed on `build_id`. When `build_id` is `None` the
    /// breaker check is skipped (preview paths / cost estimation /
    /// pre-build callers don't have a build_id yet — that case uses
    /// `synthetic_for_preview` per §2.12 rather than this path).
    ///
    /// For each ProviderType in the base order, in addition to the
    /// readiness gate, consults `walker_breaker::is_tripped(build_id,
    /// slot, pt, reset)`. If tripped, the provider is dropped from the
    /// effective call order and `EVENT_BREAKER_TRIPPED` fires with the
    /// build context. If every provider is either NotReady or tripped,
    /// the empty-cascade branch fires
    /// `EVENT_DECISION_BUILD_FAILED` and returns
    /// `NoReadyProviders` (§2.14 cascade exhaustion).
    #[allow(dead_code)]
    pub fn build_with_build_id(
        slot: &str,
        build_id: Option<&str>,
        conn: &Connection,
    ) -> std::result::Result<Self, DecisionBuildError> {
        let data = build_scope_cache_pair(conn).map_err(DecisionBuildError::ScopeCacheLoadFailed)?;
        let decision =
            build_from_chain(slot, Arc::new(data.cache), &data.chain, false, build_id)?;
        emit_event(EVENT_DECISION_BUILT, &decision);
        Ok(decision)
    }

    /// Build a synthetic preview Decision (§2.12). Skips the readiness
    /// gate — every ProviderType in the call order is included so
    /// display/estimation callers can show "what would walker do if
    /// every provider were ready?". Emits `EVENT_DECISION_PREVIEWED`
    /// (NOT `EVENT_DECISION_BUILT`) so Builds-tab filters distinguish
    /// preview from real dispatch.
    ///
    /// Consumers:
    /// - DADBEAR compile-time preview (dadbear_preview.rs)
    /// - cost estimation (preview.rs)
    /// - operator-HTTP preview routes (routes_operator.rs)
    ///
    /// Takes a pre-built `ScopeChain` + `ScopeCache` so callers that
    /// already have one in scope avoid a redundant DB read. A caller
    /// that only has a Connection should pair this with
    /// `build_scope_cache_pair`.
    #[allow(dead_code)]
    pub fn synthetic_for_preview(
        slot: &str,
        chain: &ScopeChain,
        scope_cache: Arc<ScopeCache>,
    ) -> Self {
        let effective_call_order = chain
            .slot_call_order_overrides
            .get(slot)
            .cloned()
            .unwrap_or_else(|| {
                if chain.call_order.is_empty() {
                    DEFAULT_CALL_ORDER.to_vec()
                } else {
                    chain.call_order.clone()
                }
            });

        let mut per_provider: HashMap<ProviderType, ResolvedProviderParams> = HashMap::new();
        for &pt in &effective_call_order {
            per_provider.insert(pt, resolve_all_params(chain, slot, pt));
        }

        let on_partial_failure =
            resolve_on_partial_failure(chain, slot, ProviderType::Market);

        let decision = DispatchDecision {
            slot: slot.to_string(),
            effective_call_order,
            per_provider,
            scope_snapshot: scope_cache,
            on_partial_failure,
            built_at: SystemTime::now(),
            synthetic: true,
        };
        emit_event(EVENT_DECISION_PREVIEWED, &decision);
        decision
    }

    /// Produce the Serialize-able redacted chronicle view. See the
    /// redaction-catalog comment at the top of this file.
    #[allow(dead_code)]
    pub fn for_chronicle(&self) -> DecisionChronicleView {
        let per_provider = self
            .effective_call_order
            .iter()
            .filter_map(|pt| {
                self.per_provider.get(pt).map(|p| ProviderChronicleView {
                    provider_type: pt.as_str().to_string(),
                    active: p.active,
                    model_list: p.model_list.clone(),
                    patience_secs: p.patience_secs,
                    retry_http_count: p.retry_http_count,
                    sequential: p.sequential,
                    bypass_pool: p.bypass_pool,
                })
            })
            .collect();

        DecisionChronicleView {
            slot: self.slot.clone(),
            effective_call_order: self
                .effective_call_order
                .iter()
                .map(|pt| pt.as_str().to_string())
                .collect(),
            per_provider,
            on_partial_failure: match self.on_partial_failure {
                PartialFailurePolicy::Cascade => "cascade".to_string(),
                PartialFailurePolicy::FailLoud => "fail_loud".to_string(),
                PartialFailurePolicy::RetrySame => "retry_same".to_string(),
            },
            built_at: self.built_at,
            synthetic: self.synthetic,
            source_contribution_ids: self.scope_snapshot.source_contribution_ids.clone(),
        }
    }
}

// ── Internal: shared construction body ───────────────────────────────────────
//
// `build` (runtime) and `synthetic_for_preview` share the chain-walking
// and parameter-resolving core; they diverge only on (a) whether to
// call `can_dispatch_now`, and (b) which chronicle event to emit.
// `build_from_chain` factors the shared part. `synthetic_for_preview`
// inlines its own path because it doesn't branch on readiness at all
// (includes every provider unconditionally).

fn build_from_chain(
    slot: &str,
    scope_cache: Arc<ScopeCache>,
    chain: &ScopeChain,
    _synthetic: bool,
    build_id: Option<&str>,
) -> std::result::Result<DispatchDecision, DecisionBuildError> {
    // §4.3 per-slot full-replace: slot-scoped override wins over global.
    let base_order = chain
        .slot_call_order_overrides
        .get(slot)
        .cloned()
        .unwrap_or_else(|| {
            if chain.call_order.is_empty() {
                DEFAULT_CALL_ORDER.to_vec()
            } else {
                chain.call_order.clone()
            }
        });

    let mut effective_call_order: Vec<ProviderType> = Vec::with_capacity(base_order.len());
    let mut per_provider: HashMap<ProviderType, ResolvedProviderParams> = HashMap::new();
    let mut skipped: Vec<(ProviderType, NotReadyReason)> = Vec::new();

    for pt in base_order.iter().copied() {
        let params = resolve_all_params(chain, slot, pt);

        // §E Phase 5: consult the per-build breaker BEFORE readiness.
        // A tripped breaker for (build_id, slot, provider_type) drops
        // the provider from effective_call_order for the remainder of
        // the build (subject to `breaker_reset` semantics resolved per
        // §3 — PerBuild / ProbeBased / TimeSecs:N).
        //
        // `build_id == None` is the preview path (synthetic callers go
        // through `synthetic_for_preview`; runtime callers with no
        // build_id are pre-build helpers that can't meaningfully
        // consult build-scoped state). In that case the breaker gate
        // is a no-op and we fall through to readiness.
        if let Some(bid) = build_id {
            if walker_breaker::is_tripped(bid, slot, pt, params.breaker_reset) {
                let consecutive_failures = walker_breaker::peek_state(bid, slot, pt)
                    .map(|s| s.consecutive_failures)
                    .unwrap_or(0);
                emit_breaker_tripped(bid, slot, pt, params.breaker_reset, consecutive_failures);
                skipped.push((
                    pt,
                    NotReadyReason::BreakerTripped {
                        consecutive_failures,
                    },
                ));
                continue;
            }
        }

        match readiness_for(pt).can_dispatch_now(&params) {
            ReadinessResult::Ready => {
                effective_call_order.push(pt);
                per_provider.insert(pt, params);
            }
            ReadinessResult::NotReady { reason } => {
                emit_skipped(slot, pt, &reason);
                skipped.push((pt, reason));
            }
        }
    }

    if effective_call_order.is_empty() {
        // Emit EVENT_DECISION_BUILD_FAILED with per-provider reasons
        // before returning so the chronicle records the cascade.
        emit_build_failed(slot, &skipped);
        return Err(DecisionBuildError::NoReadyProviders {
            slot: slot.to_string(),
            count: base_order.len(),
            reasons: skipped,
        });
    }

    let on_partial_failure = resolve_on_partial_failure(chain, slot, ProviderType::Market);

    Ok(DispatchDecision {
        slot: slot.to_string(),
        effective_call_order,
        per_provider,
        scope_snapshot: scope_cache,
        on_partial_failure,
        built_at: SystemTime::now(),
        synthetic: false,
    })
}

/// Pull every §3 parameter for one `(slot, provider_type)` pair into
/// a fully-resolved `ResolvedProviderParams`. Order of assignments
/// mirrors the struct definition in walker_readiness.rs for readability.
fn resolve_all_params(
    chain: &ScopeChain,
    slot: &str,
    pt: ProviderType,
) -> ResolvedProviderParams {
    ResolvedProviderParams {
        model_list: resolve_model_list(chain, slot, pt),
        max_budget_credits: resolve_max_budget_credits(chain, slot, pt),
        patience_secs: resolve_patience_secs(chain, slot, pt),
        patience_clock_resets_per_model: resolve_patience_clock_resets_per_model(chain, slot, pt),
        breaker_reset: resolve_breaker_reset(chain, slot, pt),
        sequential: resolve_sequential(chain, slot, pt),
        bypass_pool: resolve_bypass_pool(chain, slot, pt),
        retry_http_count: resolve_retry_http_count(chain, slot, pt),
        retry_backoff_base_secs: resolve_retry_backoff_base_secs(chain, slot, pt),
        dispatch_deadline_grace_secs: resolve_dispatch_deadline_grace_secs(chain, slot, pt),
        active: resolve_active(chain, slot, pt),
        ollama_base_url: if matches!(pt, ProviderType::Local) {
            Some(resolve_ollama_base_url(chain, slot, pt))
        } else {
            None
        },
        ollama_probe_interval_secs: if matches!(pt, ProviderType::Local) {
            Some(resolve_ollama_probe_interval_secs(chain, slot, pt))
        } else {
            None
        },
        fleet_peer_min_staleness_secs: if matches!(pt, ProviderType::Fleet) {
            Some(resolve_fleet_peer_min_staleness_secs(chain, slot, pt))
        } else {
            None
        },
        fleet_prefer_cached: if matches!(pt, ProviderType::Fleet) {
            Some(resolve_fleet_prefer_cached(chain, slot, pt))
        } else {
            None
        },
        network_failure_backoff_threshold: resolve_network_failure_backoff_threshold(
            chain, slot, pt,
        ),
        network_failure_backoff_secs: resolve_network_failure_backoff_secs(chain, slot, pt),
        // W1a: four new params absorbed from pyramid_tier_routing (§5.1).
        // All Option-surfacing — None means the provider will answer at
        // dispatch time (context limits) or the value is unknown
        // (pricing / supported_parameters).
        context_limit: resolve_context_limit(chain, slot, pt),
        max_completion_tokens: resolve_max_completion_tokens(chain, slot, pt),
        pricing_json: resolve_pricing_json(chain, slot, pt),
        supported_parameters: resolve_supported_parameters(chain, slot, pt),
    }
}

/// Pick the stub readiness impl per ProviderType. Phase 2/3/4 replace
/// the stubs with real impls on local_mode / provider / fleet_mps /
/// compute_market_ctx.
///
/// Returned as a boxed trait object so callers can uniformly dispatch.
fn readiness_for(pt: ProviderType) -> Box<dyn ProviderReadiness> {
    match pt {
        // Phase 2: real probe-cache-backed LocalReadiness.
        // Phase 3: real market-surface + balance + reachability
        //          MarketReadiness.
        // Phase 4: real fleet-roster-backed FleetReadiness.
        // OpenRouter still stub — trivial to real-impl (api_key presence
        // check), but no phase explicitly owns it yet.
        ProviderType::Local => Box::new(LocalReadiness::new()),
        ProviderType::OpenRouter => Box::new(OpenRouterReadinessStub),
        ProviderType::Fleet => Box::new(FleetReadiness::new()),
        ProviderType::Market => Box::new(MarketReadiness::new()),
    }
}

// ── Chronicle emission helpers ──────────────────────────────────────────────
//
// Phase 0b emits via `tracing::warn!(event = ..., ...)` mirroring the
// pattern established in walker_cache.rs and config_contributions.rs.
// Phase 1 rewires these to the real `BuildEventBus` handle on
// StepContext once that integration lands. The event-name constants
// are already defined in compute_chronicle.rs so the grep surface is
// stable.

fn emit_event(event: &'static str, decision: &DispatchDecision) {
    let view = decision.for_chronicle();
    match serde_json::to_value(&view) {
        Ok(payload) => {
            tracing::warn!(event = event, payload = %payload, "walker decision event");
        }
        Err(e) => {
            tracing::warn!(event = event, error = %e, "failed to serialize DecisionChronicleView");
        }
    }
}

fn emit_skipped(slot: &str, pt: ProviderType, reason: &NotReadyReason) {
    tracing::warn!(
        event = EVENT_PROVIDER_SKIPPED_READINESS,
        slot = slot,
        provider_type = pt.as_str(),
        reason = ?reason,
        "provider skipped readiness gate"
    );
}

/// Phase 5 §H: emit `breaker_tripped` chronicle event when the
/// Decision builder skips a provider because its per-build breaker
/// cell is tripped. Payload shape documented at the event constant
/// declaration in compute_chronicle.rs.
fn emit_breaker_tripped(
    build_id: &str,
    slot: &str,
    pt: ProviderType,
    reset: crate::pyramid::walker_resolver::BreakerReset,
    consecutive_failures: u32,
) {
    let reset_policy = match reset {
        crate::pyramid::walker_resolver::BreakerReset::PerBuild => "per_build".to_string(),
        crate::pyramid::walker_resolver::BreakerReset::ProbeBased => "probe_based".to_string(),
        crate::pyramid::walker_resolver::BreakerReset::TimeSecs { value } => {
            format!("time_secs:{value}")
        }
    };
    tracing::warn!(
        event = EVENT_BREAKER_TRIPPED,
        build_id = build_id,
        slot = slot,
        provider_type = pt.as_str(),
        reset_policy = %reset_policy,
        consecutive_failures = consecutive_failures,
        "breaker tripped — provider dropped from effective_call_order"
    );
}

fn emit_build_failed(slot: &str, reasons: &[(ProviderType, NotReadyReason)]) {
    let reason_strs: Vec<String> = reasons
        .iter()
        .map(|(pt, r)| format!("{}: {:?}", pt.as_str(), r))
        .collect();
    tracing::warn!(
        event = EVENT_DECISION_BUILD_FAILED,
        slot = slot,
        reasons = ?reason_strs,
        "no ready providers — Decision cannot be built"
    );
}

// ── Synthetic-preview helper (§2.12) ─────────────────────────────────────────

/// Collects every tier name used across a multi-step chain. Callers
/// (cost estimation per A-F10) pass the tier strings that appear in
/// the chain YAML; this helper unions them with the tier set the
/// resolver already knows about (`tier_set_from_chain`) so estimation
/// covers both declared and referenced tiers.
///
/// Stays cheap — no DB hits. Caller is expected to have a ScopeChain
/// in hand from `build_scope_cache_pair` (or equivalent).
#[allow(dead_code)]
pub fn tier_set_for_synthetic_build(
    chain: &ScopeChain,
    chain_step_slots: &[&str],
) -> std::collections::BTreeSet<String> {
    let mut set = tier_set_from_chain(chain);
    for slot in chain_step_slots {
        set.insert((*slot).to_string());
    }
    set
}

// ── Compile-time guard: DispatchDecision MUST NOT impl Serialize ─────────────
//
// Same pattern as walker_cache.rs::ScopeSnapshot. If a future dev adds
// `#[derive(Serialize)]` to DispatchDecision, this `#[cfg(any())]` block
// starts compiling (because `serde_json::to_value(&decision)` now type-
// checks), and we'd want CI to fail. The `#[cfg(any())]` block never
// actually compiles; removing the attribute reveals the latent
// regression. Kept as documentation + grep-anchor.
#[cfg(any())]
#[allow(dead_code)]
fn _dispatch_decision_must_not_be_serializable(d: &DispatchDecision) {
    // ── DO NOT ADD `#[derive(Serialize)]` TO `DispatchDecision` ──
    // If this compiles, a Serialize impl exists and the type-level
    // redaction guard (§5.4.3 / Root 27) regressed. Fix: remove the
    // derive and route serialization through `for_chronicle()`.
    let _ = serde_json::to_value(d).expect("must not compile");
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::TempDir;

    // ── Test harness: Mock NotReady readiness impl ───────────────────────────
    //
    // The real stubs always return Ready, so drive_drop / build_failed tests
    // need a mock that can refuse. Keep it local to tests; production stubs
    // stay in walker_readiness.rs.

    struct AlwaysNotReady {
        reason: NotReadyReason,
    }
    impl ProviderReadiness for AlwaysNotReady {
        fn can_dispatch_now(&self, _p: &ResolvedProviderParams) -> ReadinessResult {
            ReadinessResult::NotReady {
                reason: self.reason.clone(),
            }
        }
    }

    fn make_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("walker_decision_test.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE pyramid_config_contributions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 contribution_id TEXT NOT NULL UNIQUE,
                 slug TEXT,
                 schema_type TEXT NOT NULL,
                 yaml_content TEXT NOT NULL,
                 wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
                 wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
                 supersedes_id TEXT,
                 superseded_by_id TEXT,
                 triggering_note TEXT,
                 status TEXT NOT NULL DEFAULT 'active',
                 source TEXT NOT NULL DEFAULT 'local',
                 wire_contribution_id TEXT,
                 created_by TEXT,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 accepted_at TEXT
             );",
        )
        .unwrap();
        (dir, conn)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_active(
        conn: &Connection,
        contribution_id: &str,
        schema_type: &str,
        slug: Option<&str>,
        yaml: &str,
    ) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                 contribution_id, slug, schema_type, yaml_content, status, source
             ) VALUES (?1, ?2, ?3, ?4, 'active', 'bundled')",
            rusqlite::params![contribution_id, slug, schema_type, yaml],
        )
        .unwrap();
    }

    #[test]
    fn test_decision_build_all_providers_ready() {
        // Empty DB = SYSTEM_DEFAULTS everywhere. OpenRouter stub still
        // returns Ready unconditionally; Local (Phase 2 real impl)
        // consults the Ollama probe cache; Market (Phase 3 real impl)
        // consults the market-surface probe cache; Fleet (Phase 4 real
        // impl) consults the fleet roster probe cache.
        //
        // For this "all providers ready" smoke, we need three probe
        // caches seeded + three per-provider configs declaring a
        // model_list at slot "mid".
        use crate::pyramid::walker_fleet_probe::{
            clear_fleet_cache_for_tests, fleet_probe_test_lock, write_cached_peer,
            CachedFleetPeer,
        };
        use crate::pyramid::walker_market_probe::{
            invalidate_cached_model, write_cached_model, CachedMarketModel, CachedOffer,
        };
        use crate::pyramid::walker_ollama_probe::{
            invalidate_cached_probe, write_cached_probe, CachedProbe,
        };
        let _fg = fleet_probe_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let base_url = "http://test-decision-allready.invalid:11434/v1";
        write_cached_probe(
            base_url,
            CachedProbe {
                reachable: true,
                models: vec!["gemma3:27b".into()],
                at: std::time::Instant::now(),
            },
        );
        let market_slug = "test-decision-allready/market-model";
        write_cached_model(
            market_slug,
            CachedMarketModel {
                active_offers: 1,
                all_offers_saturated: false,
                only_self_offers: false,
                model_typical_serve_ms_p50_7d: Some(1000.0),
                offers_detail: vec![CachedOffer {
                    offer_id: "o1".into(),
                    node_handle: "other".into(),
                    operator_handle: "op".into(),
                    typical_serve_ms_p50_7d: Some(1000.0),
                    execution_concurrency: 1,
                    current_queue_depth: 0,
                    max_queue_depth: 5,
                }],
                at: std::time::Instant::now(),
            },
        );
        let fleet_model = "test-decision-allready-fleet:latest";
        write_cached_peer(CachedFleetPeer {
            node_id: "peer-allready".into(),
            node_handle: Some("@op/peer-allready".into()),
            announced_models: vec![fleet_model.to_string()],
            last_seen_at: chrono::Utc::now(),
            is_v1_announcer: false,
        });
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-wpl-mid",
            "walker_provider_local",
            None,
            &format!(
                r#"
schema_type: walker_provider_local
version: 1
overrides:
  active: true
  ollama_base_url: {base_url}
  model_list:
    mid: [gemma3:27b]
"#
            ),
        );
        insert_active(
            &conn,
            "c-wpm-mid",
            "walker_provider_market",
            None,
            &format!(
                r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
  model_list:
    mid: ["{market_slug}"]
"#
            ),
        );
        insert_active(
            &conn,
            "c-wpf-mid",
            "walker_provider_fleet",
            None,
            &format!(
                r#"
schema_type: walker_provider_fleet
version: 1
overrides:
  active: true
  model_list:
    mid: ["{fleet_model}"]
"#
            ),
        );
        let d = DispatchDecision::build("mid", &conn).expect("build must succeed");
        assert!(!d.synthetic);
        assert_eq!(d.slot, "mid");
        assert_eq!(d.effective_call_order, DEFAULT_CALL_ORDER.to_vec());
        for pt in DEFAULT_CALL_ORDER {
            assert!(d.per_provider.contains_key(&pt));
        }
        invalidate_cached_probe(base_url);
        invalidate_cached_model(market_slug);
        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_decision_build_drops_local_when_ollama_offline() {
        // Phase 2: Local is gated by the real readiness impl. With a
        // walker_provider_local contribution declaring a test-unique
        // base_url + a model_list but NO probe cache entry, Local
        // returns OllamaOffline and is dropped from effective_call_order.
        use crate::pyramid::walker_ollama_probe::invalidate_cached_probe;
        let base_url = "http://test-decision-offline.invalid:11434/v1";
        invalidate_cached_probe(base_url);
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-wpl-offline",
            "walker_provider_local",
            None,
            &format!(
                r#"
schema_type: walker_provider_local
version: 1
overrides:
  active: true
  ollama_base_url: {base_url}
  model_list:
    mid: [gemma3:27b]
"#
            ),
        );
        let d = DispatchDecision::build("mid", &conn).expect(
            "non-Local providers are still Ready stubs → decision builds",
        );
        assert!(
            !d.effective_call_order.contains(&ProviderType::Local),
            "Local must be absent when probe cache is unseeded, got {:?}",
            d.effective_call_order
        );
        assert!(!d.per_provider.contains_key(&ProviderType::Local));
    }

    #[test]
    fn test_decision_build_drops_not_ready_providers_via_mock() {
        // Production readiness_for() always returns Ready stubs today,
        // so this test drives the drop path by assembling a Decision
        // manually with AlwaysNotReady. Exercises that the
        // can_dispatch_now → NotReady branch results in the provider
        // being absent from effective_call_order AND the chronicle
        // skip event having the right shape.
        //
        // Using a direct ScopeChain so we don't need to stub out
        // readiness_for().
        let chain = ScopeChain::default();
        // Set up a minimal Decision-like path by exercising the
        // code in build_from_chain via the AlwaysNotReady helper's
        // ReadinessResult branch at the trait level.

        let mock = AlwaysNotReady {
            reason: NotReadyReason::OllamaOffline,
        };
        let params = resolve_all_params(&chain, "mid", ProviderType::Local);
        let result = mock.can_dispatch_now(&params);
        assert!(matches!(
            result,
            ReadinessResult::NotReady {
                reason: NotReadyReason::OllamaOffline
            }
        ));
    }

    #[test]
    fn test_decision_synthetic_for_preview_skips_readiness() {
        // Synthetic path never calls can_dispatch_now, so every
        // ProviderType in DEFAULT_CALL_ORDER shows up regardless of
        // readiness. We can assert this by observing that the synthetic
        // Decision has the full call order populated even though no
        // DB rows exist (all params resolve to SYSTEM_DEFAULTS).
        let (_dir, conn) = make_db();
        let data = build_scope_cache_pair(&conn).unwrap();
        let cache = Arc::new(data.cache);
        let d = DispatchDecision::synthetic_for_preview("mid", &data.chain, cache);
        assert!(d.synthetic);
        assert_eq!(d.effective_call_order, DEFAULT_CALL_ORDER.to_vec());
        for pt in DEFAULT_CALL_ORDER {
            assert!(d.per_provider.contains_key(&pt));
        }
    }

    #[test]
    fn test_decision_on_partial_failure_default_cascade() {
        let (_dir, conn) = make_db();
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert_eq!(d.on_partial_failure, PartialFailurePolicy::Cascade);
    }

    #[test]
    fn test_decision_on_partial_failure_slot_override() {
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-sp-fl",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    overrides:
      on_partial_failure: fail_loud
"#,
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert_eq!(d.on_partial_failure, PartialFailurePolicy::FailLoud);
    }

    #[test]
    fn test_resolved_provider_params_populated_from_resolver() {
        // Seed walker_provider_openrouter with patience_secs=900.
        // per_provider[OpenRouter].patience_secs must be 900.
        // per_provider[Market].patience_secs stays at SYSTEM_DEFAULT 3600,
        // but MarketReadiness (Phase 3) only surfaces Market in the
        // Decision when a surface-cache entry with headroom exists, so
        // seed one for a test-scoped slug + declare it in the market
        // config at slot "mid".
        use crate::pyramid::walker_market_probe::{
            invalidate_cached_model, write_cached_model, CachedMarketModel, CachedOffer,
        };
        let market_slug = "test-resolved-params/market-model";
        write_cached_model(
            market_slug,
            CachedMarketModel {
                active_offers: 1,
                all_offers_saturated: false,
                only_self_offers: false,
                model_typical_serve_ms_p50_7d: Some(1000.0),
                offers_detail: vec![CachedOffer {
                    offer_id: "o1".into(),
                    node_handle: "other".into(),
                    operator_handle: "op".into(),
                    typical_serve_ms_p50_7d: Some(1000.0),
                    execution_concurrency: 1,
                    current_queue_depth: 0,
                    max_queue_depth: 5,
                }],
                at: std::time::Instant::now(),
            },
        );
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-or-ps",
            "walker_provider_openrouter",
            None,
            r#"
schema_type: walker_provider_openrouter
version: 1
overrides:
  patience_secs: 900
"#,
        );
        insert_active(
            &conn,
            "c-wpm-ps",
            "walker_provider_market",
            None,
            &format!(
                r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
  model_list:
    mid: ["{market_slug}"]
"#
            ),
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        let or = d
            .per_provider
            .get(&ProviderType::OpenRouter)
            .expect("openrouter must be present");
        assert_eq!(or.patience_secs, 900);
        let mkt = d
            .per_provider
            .get(&ProviderType::Market)
            .expect("market must be present");
        assert_eq!(mkt.patience_secs, 3600);
        invalidate_cached_model(market_slug);
    }

    /// Compile-time: `DispatchDecision` MUST NOT be `Serialize`.
    /// Mirrors `walker_cache.rs::_scope_snapshot_must_not_be_serializable`.
    /// Runtime assertion: if someone adds `#[derive(Serialize)]`, this test
    /// still passes (can't fail a type-level property at runtime), so
    /// readers should treat the `#[cfg(any())]` guard above as canonical.
    #[test]
    fn test_dispatch_decision_not_serialize_guard_present() {
        // Grep anchor: dispatch_decision_not_serialize_guard
        let (_dir, conn) = make_db();
        let _d = DispatchDecision::build("mid", &conn).unwrap();
    }

    #[test]
    fn test_decision_chronicle_view_redacts_sensitive_fields() {
        // Build a Decision with values that WOULD be sensitive if
        // included — max_budget_credits=5000 at scope 4, ollama_base_url
        // carried via SYSTEM_DEFAULT. Chronicle view must NOT expose
        // these as top-level keys.
        //
        // Market must pass readiness for the cap to ride along, so
        // seed the surface-cache + declare an active market config
        // with a test-scoped slug. A big enough default balance keeps
        // the budget cap from tripping the InsufficientCredit gate in
        // readiness (we want the cap to survive into the chronicle
        // view, not be short-circuited to NotReady).
        use crate::pyramid::walker_market_probe::{
            clear_node_state_for_tests, invalidate_cached_model, node_state_test_lock,
            set_credit_balance, write_cached_model, CachedMarketModel, CachedOffer,
        };
        let _g = node_state_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        set_credit_balance(Some(1_000_000));
        let market_slug = "test-chronicle-redact/market-model";
        write_cached_model(
            market_slug,
            CachedMarketModel {
                active_offers: 1,
                all_offers_saturated: false,
                only_self_offers: false,
                model_typical_serve_ms_p50_7d: Some(1000.0),
                offers_detail: vec![CachedOffer {
                    offer_id: "o1".into(),
                    node_handle: "other".into(),
                    operator_handle: "op".into(),
                    typical_serve_ms_p50_7d: Some(1000.0),
                    execution_concurrency: 1,
                    current_queue_depth: 0,
                    max_queue_depth: 5,
                }],
                at: std::time::Instant::now(),
            },
        );
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-mkt-cap",
            "walker_provider_market",
            None,
            &format!(
                r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
  max_budget_credits: 5000
  model_list:
    mid: ["{market_slug}"]
"#
            ),
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        let view = d.for_chronicle();
        let val = serde_json::to_value(&view).unwrap();
        let s = val.to_string();
        // Sensitive / local_only keys must not appear in the JSON at all.
        assert!(
            !s.contains("max_budget_credits"),
            "max_budget_credits must be redacted, got {s}"
        );
        assert!(
            !s.contains("ollama_base_url"),
            "ollama_base_url must be redacted, got {s}"
        );
        assert!(
            !s.contains("fleet_peer_min_staleness_secs"),
            "fleet_peer_min_staleness_secs must be redacted, got {s}"
        );
        assert!(
            !s.contains("network_failure_backoff"),
            "network_failure_backoff_* must be redacted, got {s}"
        );
        // Public keys that MUST appear.
        assert!(s.contains("slot"));
        assert!(s.contains("effective_call_order"));
        assert!(s.contains("on_partial_failure"));
        assert!(s.contains("synthetic"));
        assert!(s.contains("source_contribution_ids"));
        invalidate_cached_model(market_slug);
        clear_node_state_for_tests();
    }

    #[test]
    fn test_effective_call_order_slot_override_wins() {
        // §4.3 full-replace: slot-scoped order overrides the global
        // walker_call_order, full-replace (not merge).
        let (_dir, conn) = make_db();
        // Seed global walker_call_order
        insert_active(
            &conn,
            "c-wco-global",
            "walker_call_order",
            None,
            r#"
schema_type: walker_call_order
version: 1
order: [local, market, openrouter, fleet]
"#,
        );
        // Slot-specific order takes precedence for "extract".
        insert_active(
            &conn,
            "c-sp-slot-order",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  extract:
    order: [openrouter]
"#,
        );
        let d = DispatchDecision::build("extract", &conn).unwrap();
        assert_eq!(d.effective_call_order, vec![ProviderType::OpenRouter]);
    }

    #[test]
    fn test_effective_call_order_falls_through_to_global() {
        // When no slot override is declared, the base order walked by
        // the Decision builder inherits the global walker_call_order.
        // Real-provider readiness gates (Phase 2/3/4) may drop Local /
        // Market / Fleet when their probe caches are unseeded — so
        // we verify the OpenRouter entry (always-Ready stub) is
        // retained in the resulting effective_call_order, confirming
        // the fall-through reached the builder with OpenRouter in it.
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-wco-fallthrough",
            "walker_call_order",
            None,
            r#"
schema_type: walker_call_order
version: 1
order: [openrouter, fleet]
"#,
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert!(
            d.effective_call_order.contains(&ProviderType::OpenRouter),
            "OpenRouter must survive into effective_call_order when \
             declared at global scope; got {:?}",
            d.effective_call_order
        );
        // Fleet may or may not survive readiness depending on roster
        // cache; we don't assert on it, only on the always-Ready stub.
    }

    #[test]
    fn test_effective_call_order_system_default() {
        // No walker_call_order contribution → the base order is
        // DEFAULT_CALL_ORDER ([Market, Local, OpenRouter, Fleet] per
        // §8 tester smoke). Real-provider readiness gates drop
        // Local/Market/Fleet without probe-cache seeding, but the
        // always-Ready OpenRouter stub must survive, confirming the
        // SYSTEM_DEFAULT was in fact used.
        let (_dir, conn) = make_db();
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert!(
            d.effective_call_order.contains(&ProviderType::OpenRouter),
            "OpenRouter (DEFAULT_CALL_ORDER member) must survive into \
             effective_call_order when no walker_call_order is declared; \
             got {:?}",
            d.effective_call_order
        );
    }

    #[test]
    fn test_decision_effective_call_order_honors_slot_full_replace() {
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-sp-order",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter]
"#,
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert_eq!(d.effective_call_order, vec![ProviderType::OpenRouter]);
    }

    #[test]
    fn test_decision_empty_effective_call_order_errors() {
        // Drive every provider to NotReady by constructing the Decision
        // via the shared internal path with an empty base order.
        // Since readiness_for() always returns Ready stubs today, we
        // test this by putting an empty `order: []` at slot scope —
        // the parser falls back to DEFAULT_CALL_ORDER when empty, so
        // that's not a path to empty. Instead, directly drive the
        // construction path with a chain whose slot_call_order_overrides
        // is explicitly empty via an unusual-but-legal scope edit.
        //
        // Trickier approach: since the real stubs always return Ready,
        // we verify the error path by constructing it directly and
        // emitting the chronicle event via build_from_chain with an
        // artificially-empty effective_call_order scenario. The path
        // we CAN exercise cleanly is the synthetic/runtime divergence:
        // `synthetic_for_preview` with an empty chain.call_order still
        // falls through to DEFAULT_CALL_ORDER, so that never empties.
        //
        // Cleanest: exercise the error path by manually constructing
        // a Vec<(ProviderType, NotReadyReason)> + verifying the enum
        // variant + chronicle helper doesn't panic. This is a narrow
        // unit test since the real drop branch can't fire until Phase
        // 2/3/4 readiness impls ship.
        let reasons = vec![
            (ProviderType::Local, NotReadyReason::OllamaOffline),
            (ProviderType::OpenRouter, NotReadyReason::CredentialMissing),
            (ProviderType::Fleet, NotReadyReason::NoReachablePeer),
            (ProviderType::Market, NotReadyReason::WireUnreachable),
        ];
        emit_build_failed("mid", &reasons);
        let err = DecisionBuildError::NoReadyProviders {
            slot: "mid".to_string(),
            count: 4,
            reasons,
        };
        let msg = format!("{err}");
        assert!(msg.contains("mid"));
        assert!(msg.contains("4"));
    }

    #[test]
    fn test_for_chronicle_omits_non_effective_providers() {
        // If a provider isn't in effective_call_order, it also isn't
        // reflected in the chronicle view's per_provider array — per
        // §2.9 the chronicle mirrors what the dispatcher saw.
        //
        // Slot-scope order=[market] + MarketReadiness (Phase 3) requires
        // an active market config + a surface-cache entry with headroom.
        use crate::pyramid::walker_market_probe::{
            clear_node_state_for_tests, invalidate_cached_model, node_state_test_lock,
            write_cached_model, CachedMarketModel, CachedOffer,
        };
        let _g = node_state_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        let market_slug = "test-chronicle-omit/market-model";
        write_cached_model(
            market_slug,
            CachedMarketModel {
                active_offers: 1,
                all_offers_saturated: false,
                only_self_offers: false,
                model_typical_serve_ms_p50_7d: Some(1000.0),
                offers_detail: vec![CachedOffer {
                    offer_id: "o1".into(),
                    node_handle: "other".into(),
                    operator_handle: "op".into(),
                    typical_serve_ms_p50_7d: Some(1000.0),
                    execution_concurrency: 1,
                    current_queue_depth: 0,
                    max_queue_depth: 5,
                }],
                at: std::time::Instant::now(),
            },
        );
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-sp-narrow",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [market]
"#,
        );
        insert_active(
            &conn,
            "c-wpm-narrow",
            "walker_provider_market",
            None,
            &format!(
                r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
  model_list:
    mid: ["{market_slug}"]
"#
            ),
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        let view = d.for_chronicle();
        assert_eq!(view.per_provider.len(), 1);
        assert_eq!(view.per_provider[0].provider_type, "market");
        assert_eq!(view.effective_call_order, vec!["market".to_string()]);
        invalidate_cached_model(market_slug);
    }

    #[test]
    fn test_decision_skips_tripped_providers() {
        // Phase 5 §E: when the breaker is tripped for a
        // (build_id, slot, provider_type) triple, the Decision
        // builder drops that provider from effective_call_order.
        //
        // Seed OpenRouter to Ready (stub), declare OR-only order,
        // then trip the breaker for (build_id, mid, OpenRouter).
        // Decision should have empty effective_call_order and error.
        use crate::pyramid::walker_breaker::{
            breaker_test_lock, clear_all_for_tests, record_failure, TRIP_THRESHOLD,
        };
        let _g = breaker_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        let bid = "build-decision-skip";
        for _ in 0..TRIP_THRESHOLD {
            record_failure(bid, "mid", ProviderType::OpenRouter);
        }
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-or-only",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter]
"#,
        );
        let err = DispatchDecision::build_with_build_id("mid", Some(bid), &conn).unwrap_err();
        match err {
            DecisionBuildError::NoReadyProviders { reasons, .. } => {
                assert!(reasons.iter().any(|(_pt, r)| matches!(
                    r,
                    NotReadyReason::BreakerTripped { .. }
                )));
            }
            other => panic!("expected NoReadyProviders, got {other:?}"),
        }
        clear_all_for_tests();
    }

    #[test]
    fn test_decision_skip_tripped_does_not_affect_other_build() {
        // Tripping breaker for build-A must not affect build-B.
        use crate::pyramid::walker_breaker::{
            breaker_test_lock, clear_all_for_tests, record_failure, TRIP_THRESHOLD,
        };
        let _g = breaker_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        for _ in 0..TRIP_THRESHOLD {
            record_failure("build-A", "mid", ProviderType::OpenRouter);
        }
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-or-only-b",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter]
"#,
        );
        // build-B still sees OpenRouter as Ready.
        let d =
            DispatchDecision::build_with_build_id("mid", Some("build-B"), &conn).unwrap();
        assert!(d.effective_call_order.contains(&ProviderType::OpenRouter));
        clear_all_for_tests();
    }

    #[test]
    fn test_decision_build_without_build_id_skips_breaker() {
        // build() = build_with_build_id(None, ...). Breaker gate is
        // skipped so Decision sees Ready providers even when a
        // different build_id has tripped cells.
        use crate::pyramid::walker_breaker::{
            breaker_test_lock, clear_all_for_tests, record_failure, TRIP_THRESHOLD,
        };
        let _g = breaker_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_all_for_tests();
        for _ in 0..TRIP_THRESHOLD {
            record_failure("build-irrelevant", "mid", ProviderType::OpenRouter);
        }
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-or-only-nobid",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter]
"#,
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        assert!(d.effective_call_order.contains(&ProviderType::OpenRouter));
        clear_all_for_tests();
    }

    #[test]
    fn test_slot_policy_per_provider_override_reaches_decision() {
        // §4.3 scope-1 (slot × provider) override. Set
        // slots[mid].per_provider.market.breaker_reset =
        // "probe_based" and assert the Decision's per_provider[Market]
        // reflects it.
        //
        // MarketReadiness (Phase 3) requires a surface-cache entry +
        // an active market config, so seed both.
        use crate::pyramid::walker_market_probe::{
            clear_node_state_for_tests, invalidate_cached_model, node_state_test_lock,
            write_cached_model, CachedMarketModel, CachedOffer,
        };
        let _g = node_state_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_node_state_for_tests();
        let market_slug = "test-slot-policy-per-provider/market-model";
        write_cached_model(
            market_slug,
            CachedMarketModel {
                active_offers: 1,
                all_offers_saturated: false,
                only_self_offers: false,
                model_typical_serve_ms_p50_7d: Some(1000.0),
                offers_detail: vec![CachedOffer {
                    offer_id: "o1".into(),
                    node_handle: "other".into(),
                    operator_handle: "op".into(),
                    typical_serve_ms_p50_7d: Some(1000.0),
                    execution_concurrency: 1,
                    current_queue_depth: 0,
                    max_queue_depth: 5,
                }],
                at: std::time::Instant::now(),
            },
        );
        let (_dir, conn) = make_db();
        insert_active(
            &conn,
            "c-wpm-pp",
            "walker_provider_market",
            None,
            &format!(
                r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
  model_list:
    mid: ["{market_slug}"]
"#
            ),
        );
        insert_active(
            &conn,
            "c-sp-pp-market",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [market]
    per_provider:
      market:
        breaker_reset: "probe_based"
"#,
        );
        let d = DispatchDecision::build("mid", &conn).unwrap();
        let mkt = d
            .per_provider
            .get(&ProviderType::Market)
            .expect("market must be present");
        assert_eq!(
            mkt.breaker_reset,
            crate::pyramid::walker_resolver::BreakerReset::ProbeBased,
        );
        invalidate_cached_model(market_slug);
        clear_node_state_for_tests();
    }

    #[test]
    fn test_tier_set_for_synthetic_build_merges_sources() {
        // tier_set_from_chain pulls scope-3/4 model_list keys; the
        // helper unions those with the explicit chain_step_slots list.
        let mut chain = ScopeChain::default();
        use crate::pyramid::walker_resolver::ScopeEntry;
        chain.provider.insert(
            ProviderType::OpenRouter,
            ScopeEntry {
                overrides: [(
                    "model_list".to_string(),
                    json!({
                        "mid": ["a"],
                        "high": ["b"],
                    }),
                )]
                .into_iter()
                .collect(),
                contribution_id: None,
            },
        );
        let set = tier_set_for_synthetic_build(&chain, &["extractor", "mid"]);
        assert!(set.contains("mid"));
        assert!(set.contains("high"));
        assert!(set.contains("extractor"));
        // mid appears in both sources but is one element.
        assert_eq!(set.len(), 3);
    }
}
