// Walker v3 — scope-chain resolver, typed accessors, SYSTEM_DEFAULTS table
// (Phase 0b Workstream A).
//
// Plan rev 1.0.2 anchors:
//   §2.1 five scopes ordered most-specific → least-specific
//        (slot×provider, slot, call-order×provider, provider-type, system)
//   §2.2 `resolve<T>(chain, param, slot, provider_type) -> Option<T>` —
//        first non-None wins across the scope chain; SYSTEM_DEFAULT is
//        the floor.
//   §2.3 `overrides` map stored as `Map<String, serde_json::Value>`;
//        typed accessors hide the scope walk. Two groups:
//          - Scalar accessors return T with SYSTEM_DEFAULT fallback.
//          - Option-surfacing accessors (`max_budget_credits`,
//            `model_list`) return `Option<T>` directly — None is
//            semantically meaningful.
//        `model_list` is SHAPE-PER-SCOPE: `Vec<String>` at scopes 1–2
//        (slot is implicit), `Map<tier, Vec<String>>` at scopes 3–4.
//   §2.4 override semantics: declared / not-declared / explicit null.
//        This resolver reads post-normalization contributions — the
//        envelope writer converts explicit null to "not declared" at
//        persist time. Resolver treats both as "walk past."
//   §2.5 module home: `src-tauri/src/pyramid/walker_resolver.rs`.
//   §2.7 scope-3 keying: by `provider_type`, not list position.
//   §2.8 tier names self-documenting: the set of known tiers IS the
//        union of `model_list` keys across all active provider configs
//        at scopes 3 and 4.
//   §2.11 shape validation runs at the envelope writer (commit 5 of
//        Phase 0a-1) — this module assumes bodies are shape-valid and
//        treats serde_json::from_value failures as hard bugs.
//   §2.13 growth and failure modes — §2.14.3 schema evolution: adding a
//        new parameter key requires (a) annotation, (b) SYSTEM_DEFAULT,
//        (c) parameter catalog row, (d) typed accessor.
//
// This module is the single resolver function over the scope chain plus
// the typed accessors that hide the scope walk from callers. It does
// NOT build DispatchDecision (WS-D owns that) and does NOT wire the
// rebuild function into the boot sequence (WS-E owns that). It DOES
// expose `build_scope_cache(&Connection) -> Result<ScopeCache>` so
// WS-E can plug it into `spawn_scope_cache_reloader`'s `rebuild_fn`
// slot.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::pyramid::config_contributions::load_active_config_contribution;
use crate::pyramid::walker_cache::ScopeCache;

// ── ProviderType ─────────────────────────────────────────────────────────────
//
// §2.1 / §2.7: the four provider types are the universe for v3. Scope 3
// and scope 4 key on this enum; scope 1 keys on `(slot, ProviderType)`.

/// The four walker-recognized provider types. New provider types in
/// future revisions of this plan add a variant here and a matching
/// schema_type (`walker_provider_<type>`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Local,
    OpenRouter,
    Fleet,
    Market,
}

impl ProviderType {
    /// All four variants, in a stable order. Useful for callers that
    /// need to enumerate (e.g. `build_scope_cache` loading one
    /// `walker_provider_*` contribution per type).
    pub const ALL: [ProviderType; 4] = [
        ProviderType::Local,
        ProviderType::OpenRouter,
        ProviderType::Fleet,
        ProviderType::Market,
    ];

    /// Lowercase string form used in YAML bodies and scope-3
    /// `overrides_by_provider` keys.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderType::Local => "local",
            ProviderType::OpenRouter => "openrouter",
            ProviderType::Fleet => "fleet",
            ProviderType::Market => "market",
        }
    }

    /// The `schema_type` suffix for the per-provider contribution that
    /// carries this provider type's scope-4 overrides.
    pub fn schema_type(self) -> &'static str {
        match self {
            ProviderType::Local => "walker_provider_local",
            ProviderType::OpenRouter => "walker_provider_openrouter",
            ProviderType::Fleet => "walker_provider_fleet",
            ProviderType::Market => "walker_provider_market",
        }
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderType {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "local" => Ok(ProviderType::Local),
            "openrouter" => Ok(ProviderType::OpenRouter),
            "fleet" => Ok(ProviderType::Fleet),
            "market" => Ok(ProviderType::Market),
            other => Err(format!("unknown provider_type: {other}")),
        }
    }
}

// ── BreakerReset tagged union (§2.11 + §3) ───────────────────────────────────
//
// Accepts both shapes at deserialize time:
//   - string shorthand: "per_build", "probe_based", "time_secs:300"
//   - structured form:  {kind: "per_build"} | {kind: "time_secs", value: 300}
// The envelope writer normalizes string shorthand to structured form
// at persist time (§2.11). The resolver reads structured. FromStr +
// the permissive Deserialize impl are still useful for SYSTEM_DEFAULT
// literals and tests.

/// How the market circuit breaker's tripped state clears (§3).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BreakerReset {
    PerBuild,
    ProbeBased,
    #[serde(rename = "time_secs")]
    TimeSecs {
        value: u64,
    },
}

impl FromStr for BreakerReset {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "per_build" => Ok(BreakerReset::PerBuild),
            "probe_based" => Ok(BreakerReset::ProbeBased),
            other if other.starts_with("time_secs:") => {
                let rest = &other["time_secs:".len()..];
                rest.parse::<u64>()
                    .map(|v| BreakerReset::TimeSecs { value: v })
                    .map_err(|e| format!("invalid time_secs value: {e}"))
            }
            other => Err(format!("unknown breaker_reset shorthand: {other}")),
        }
    }
}

impl<'de> Deserialize<'de> for BreakerReset {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        // Accept either a string ("per_build" / "time_secs:300") or a
        // map ({kind: "time_secs", value: 300}). A visitor keeps us
        // off an intermediate serde_json::Value allocation on the
        // hot path.
        struct BreakerResetVisitor;
        impl<'de> serde::de::Visitor<'de> for BreakerResetVisitor {
            type Value = BreakerReset;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "a BreakerReset string (per_build | probe_based | time_secs:N) or a structured map",
                )
            }
            fn visit_str<E: Error>(self, v: &str) -> std::result::Result<BreakerReset, E> {
                BreakerReset::from_str(v).map_err(E::custom)
            }
            fn visit_string<E: Error>(self, v: String) -> std::result::Result<BreakerReset, E> {
                BreakerReset::from_str(&v).map_err(E::custom)
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<BreakerReset, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                // Collect into a serde_json::Value and re-deserialize
                // via the Serialize-mirror tag form. Correctness-first;
                // the map branch fires from the structured form which
                // is cheap either way.
                let mut kind: Option<String> = None;
                let mut value: Option<u64> = None;
                while let Some(k) = map.next_key::<String>()? {
                    match k.as_str() {
                        "kind" => kind = Some(map.next_value()?),
                        "value" => value = Some(map.next_value()?),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let kind = kind.ok_or_else(|| A::Error::custom("BreakerReset map missing 'kind'"))?;
                match kind.as_str() {
                    "per_build" => Ok(BreakerReset::PerBuild),
                    "probe_based" => Ok(BreakerReset::ProbeBased),
                    "time_secs" => {
                        let v = value
                            .ok_or_else(|| A::Error::custom("time_secs variant missing 'value'"))?;
                        Ok(BreakerReset::TimeSecs { value: v })
                    }
                    other => Err(A::Error::custom(format!(
                        "unknown BreakerReset kind: {other}"
                    ))),
                }
            }
        }
        deserializer.deserialize_any(BreakerResetVisitor)
    }
}

// ── PartialFailurePolicy tagged enum (§3 / Root 16) ──────────────────────────
//
// Scope-2 ONLY per §3. At Decision level there's exactly one policy
// per step; allowing scope 4 would create ambiguity. Resolver does
// not enforce scope-2-only (that's the validator's job); it just
// reads wherever declared.

/// What walker does when a provider returns a retryable failure (§3).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialFailurePolicy {
    /// Try next provider in `effective_call_order`. Default.
    Cascade,
    /// Emit `dispatch_failed_policy_blocked` and stop. Privacy-preserving.
    FailLoud,
    /// Stay on same provider; respect breaker and patience budget.
    RetrySame,
}

// ── ScopeEntry / ScopeChain ──────────────────────────────────────────────────
//
// §2.1 scope objects. `overrides` is a `Map<String, serde_json::Value>`
// (§2.3). We also carry the `contribution_id` of the row that produced
// this scope entry for audit trail (surfaces in §2.9 Decision's
// scope_snapshot and the chronicle `decision_built` event after
// redaction per §5.4.3).

/// Per-scope carrier: the `overrides` map plus audit metadata.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ScopeEntry {
    pub overrides: HashMap<String, serde_json::Value>,
    /// Contribution_id of the row this entry was derived from, for
    /// the chronicle audit trail. `None` for SYSTEM_DEFAULT-only.
    pub contribution_id: Option<String>,
}

/// The five-scope chain loaded from active `walker_*` contributions.
///
/// NOT `Serialize` — the chronicle must only see a redacted view
/// (§5.4.3 / Root 27 type-guard). Mirror of the
/// `#[cfg(any())] _scope_snapshot_must_not_be_serializable` guard in
/// `walker_cache.rs`; the same policy applies here because a future
/// dev who calls `serde_json::to_value(&scope_chain)` leaks LAN URLs
/// and other `local_only` parameters.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct ScopeChain {
    /// Scope 1: most specific. Keyed on `(slot, provider_type)`.
    pub slot_provider: HashMap<(String, ProviderType), ScopeEntry>,
    /// Scope 2: slot-wide. Keyed on slot (tier name).
    pub slot: HashMap<String, ScopeEntry>,
    /// Scope 3: per-provider-type overrides within the default order.
    /// Keyed on `provider_type` (§2.7), NOT on list position.
    pub call_order_provider: HashMap<ProviderType, ScopeEntry>,
    /// Scope 4: provider-type defaults. One entry per `walker_provider_*`.
    pub provider: HashMap<ProviderType, ScopeEntry>,
    /// Default call-order from `walker_call_order.order`. Ordered.
    pub call_order: Vec<ProviderType>,
    /// Optional per-slot full-replace call-order override
    /// (§4.3 `slots[tier].order`).
    pub slot_call_order_overrides: HashMap<String, Vec<ProviderType>>,
}

// ── SYSTEM_DEFAULTS (§3) ─────────────────────────────────────────────────────
//
// Storage choice: explicit named constants + `system_default_json`
// function that returns a `serde_json::Value` for the named param.
// Rationale: the 18 params in §3 have five different Rust types
// (bool, u64, u32, i64-ish via Option, String, tagged union). A
// single typed HashMap would need either an enum wrapper or stringly-
// typed values; neither is ergonomic. A function per-param + a
// fall-through `system_default_json(param)` keeps the accessor layer
// clean and the SYSTEM_DEFAULT names greppable when the table from
// §3 needs to move. Explicit constants also mean the system defaults
// appear in the binary as compile-time values — cheap, no LazyLock
// synchronization cost, and they show up in `cargo doc` cleanly.
//
// Per-provider active defaults are NOT captured by the system table.
// §3 footnotes that scope-4 provider carriers ship with their own
// `active` defaults in the bundled manifest (WS-B), so the resolver's
// system fall-through for `active` is `true` only. If a scope-4 carrier
// for a provider defaults-to-false (openrouter/fleet defaults are true,
// local/market default false — the latter per Root 17 "market ships
// inactive, Page 4 flip is the consent record"), that default lands in
// the bundled contribution body, not here. Same story for `sequential`
// (per-provider default true for local/market, false for openrouter/
// fleet). The SYSTEM floor stays at the absolute-safest value.

/// Wall-clock budget for the saturation-retry loop across all retries
/// on this scope's market dispatch (§3).
#[allow(dead_code)]
pub const PATIENCE_SECS_DEFAULT: u64 = 3600;
/// Whether the patience clock resets when walker advances to the next
/// model_id vs. being a single budget across all models on this leg.
#[allow(dead_code)]
pub const PATIENCE_CLOCK_RESETS_PER_MODEL_DEFAULT: bool = false;
/// Default: breaker clears per-build (§3).
#[allow(dead_code)]
pub const BREAKER_RESET_DEFAULT: BreakerReset = BreakerReset::PerBuild;
/// Whether this scope's dispatches serialize at the engine. System
/// floor = `true` (safest). Per-provider overrides at scope 4 differ
/// (see catalog).
#[allow(dead_code)]
pub const SEQUENTIAL_DEFAULT: bool = true;
/// Whether to bypass the local provider-pools semaphore (§3).
#[allow(dead_code)]
pub const BYPASS_POOL_DEFAULT: bool = false;
/// Per-dispatch HTTP retry count (§3).
#[allow(dead_code)]
pub const RETRY_HTTP_COUNT_DEFAULT: u32 = 3;
/// Base for exponential backoff inside HTTP retries (§3).
#[allow(dead_code)]
pub const RETRY_BACKOFF_BASE_SECS_DEFAULT: u64 = 2;
/// Grace appended to Wire's dispatch_deadline_at when computing
/// walker's /fill await timeout (§3).
#[allow(dead_code)]
pub const DISPATCH_DEADLINE_GRACE_SECS_DEFAULT: u64 = 10;
/// How old a peer announcement may be before fleet provider skips it (§3).
#[allow(dead_code)]
pub const FLEET_PEER_MIN_STALENESS_SECS_DEFAULT: u64 = 300;
/// Whether fleet provider prefers peers that have the requested model
/// cached (§3).
#[allow(dead_code)]
pub const FLEET_PREFER_CACHED_DEFAULT: bool = true;
/// Consecutive-failure count before readiness returns
/// `NetworkUnreachable` (§2.16.5 / §5.5.8).
#[allow(dead_code)]
pub const NETWORK_FAILURE_BACKOFF_THRESHOLD_DEFAULT: u32 = 3;
/// Duration in `NetworkUnreachable` state before readiness retries
/// (§2.16.5 / §5.5.8).
#[allow(dead_code)]
pub const NETWORK_FAILURE_BACKOFF_SECS_DEFAULT: u64 = 300;
/// Decision-level policy for retryable provider failures (§3).
#[allow(dead_code)]
pub const ON_PARTIAL_FAILURE_DEFAULT: PartialFailurePolicy = PartialFailurePolicy::Cascade;
/// Local Ollama endpoint (§3).
#[allow(dead_code)]
pub const OLLAMA_BASE_URL_DEFAULT: &str = "http://localhost:11434/v1";
/// How often local provider config probes /api/tags (§3).
#[allow(dead_code)]
pub const OLLAMA_PROBE_INTERVAL_SECS_DEFAULT: u64 = 300;
/// System floor for `active`. Per-provider carriers override at scope 4
/// (openrouter/fleet ship true; local/market ship false).
#[allow(dead_code)]
pub const ACTIVE_DEFAULT: bool = true;

/// Default bundled call-order when no `walker_call_order` contribution
/// is present. Matches §8 tester-smoke acceptance: `[market, local,
/// openrouter, fleet]` — same order the bundled seed will ship (WS-B).
/// `build_scope_cache` falls back to this when the contribution row
/// is absent.
#[allow(dead_code)]
pub const DEFAULT_CALL_ORDER: [ProviderType; 4] = [
    ProviderType::Market,
    ProviderType::Local,
    ProviderType::OpenRouter,
    ProviderType::Fleet,
];

/// Fall-through JSON for SYSTEM_DEFAULTS indexed by parameter name.
/// Returns `None` for parameters whose system floor is `None`
/// (`model_list`, `max_budget_credits`) — the accessor surfaces that
/// as `Option::None`. Returning a concrete Value for every other
/// listed parameter lets the resolver use a single `unwrap_or(system)`
/// step where a scalar is expected.
///
/// Parameters absent from the §3 catalog return `None` here too — the
/// accessor layer catches that at compile time by only calling
/// `system_default_json` for declared params. A call site that passes
/// an unknown string is a programmer bug, not a data issue.
#[allow(dead_code)]
pub fn system_default_json(param: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    let v = match param {
        "active" => json!(ACTIVE_DEFAULT),
        "model_list" => return None,
        "max_budget_credits" => return None,
        "patience_secs" => json!(PATIENCE_SECS_DEFAULT),
        "patience_clock_resets_per_model" => json!(PATIENCE_CLOCK_RESETS_PER_MODEL_DEFAULT),
        "breaker_reset" => serde_json::to_value(BREAKER_RESET_DEFAULT).ok()?,
        "sequential" => json!(SEQUENTIAL_DEFAULT),
        "bypass_pool" => json!(BYPASS_POOL_DEFAULT),
        "retry_http_count" => json!(RETRY_HTTP_COUNT_DEFAULT),
        "retry_backoff_base_secs" => json!(RETRY_BACKOFF_BASE_SECS_DEFAULT),
        "dispatch_deadline_grace_secs" => json!(DISPATCH_DEADLINE_GRACE_SECS_DEFAULT),
        "fleet_peer_min_staleness_secs" => json!(FLEET_PEER_MIN_STALENESS_SECS_DEFAULT),
        "fleet_prefer_cached" => json!(FLEET_PREFER_CACHED_DEFAULT),
        "network_failure_backoff_threshold" => json!(NETWORK_FAILURE_BACKOFF_THRESHOLD_DEFAULT),
        "network_failure_backoff_secs" => json!(NETWORK_FAILURE_BACKOFF_SECS_DEFAULT),
        "on_partial_failure" => serde_json::to_value(ON_PARTIAL_FAILURE_DEFAULT).ok()?,
        "ollama_base_url" => json!(OLLAMA_BASE_URL_DEFAULT),
        "ollama_probe_interval_secs" => json!(OLLAMA_PROBE_INTERVAL_SECS_DEFAULT),
        _ => return None,
    };
    Some(v)
}

// ── Resolver core (§2.2) ─────────────────────────────────────────────────────
//
// Returns the first non-None value along the scope chain, falling
// through to SYSTEM_DEFAULTS last. First extraction uses
// `serde_json::from_value` to coerce the stored Value into T.
//
// §2.11 says shape validation at WRITE time should prevent a
// stored Value from having the wrong shape. `from_value` failure
// at read time is therefore a hard bug — the resolver surfaces it
// as `ResolverError::TypeMismatch` so callers can log loudly
// rather than silently paper over it.

/// Error from a resolver call. `TypeMismatch` is a bug signal —
/// §2.11 shape validation should have caught the ill-shaped Value
/// at envelope-writer time.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    #[error("type mismatch resolving `{param}` at scope {scope}: {source}")]
    TypeMismatch {
        param: String,
        scope: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Walk the scope chain for `param`, returning the first non-None
/// value deserialized into `T`. SYSTEM_DEFAULTS is the fall-through
/// floor; `None` is returned only when neither the chain nor
/// SYSTEM_DEFAULTS declares the param (or when `T`'s system default
/// is semantically `None` — `model_list` / `max_budget_credits`).
///
/// `Err(TypeMismatch)` fires when a scope has the key but the stored
/// Value can't coerce to `T`. This is a validator-layer bug — log
/// loudly at the call site. The typed accessors below convert any
/// Err to a log + SYSTEM_DEFAULT fallback so a single corrupted
/// contribution doesn't brick dispatch (defense in depth past
/// §2.11's save-time guard).
#[allow(dead_code)]
pub fn resolve<T: DeserializeOwned>(
    chain: &ScopeChain,
    param: &str,
    slot: &str,
    provider_type: ProviderType,
) -> std::result::Result<Option<T>, ResolverError> {
    // Helper: extract a key from a scope's overrides. `null` is
    // treated as "not declared" per §2.4. A missing key ditto.
    fn extract<T: DeserializeOwned>(
        entry: Option<&ScopeEntry>,
        param: &str,
        scope: &'static str,
    ) -> std::result::Result<Option<T>, ResolverError> {
        let Some(entry) = entry else { return Ok(None) };
        let Some(v) = entry.overrides.get(param) else {
            return Ok(None);
        };
        if v.is_null() {
            return Ok(None);
        }
        match serde_json::from_value::<T>(v.clone()) {
            Ok(t) => Ok(Some(t)),
            Err(e) => Err(ResolverError::TypeMismatch {
                param: param.to_string(),
                scope,
                source: e,
            }),
        }
    }

    // Scope 1
    if let Some(t) = extract::<T>(
        chain.slot_provider.get(&(slot.to_string(), provider_type)),
        param,
        "slot_provider",
    )? {
        return Ok(Some(t));
    }
    // Scope 2
    if let Some(t) = extract::<T>(chain.slot.get(slot), param, "slot")? {
        return Ok(Some(t));
    }
    // Scope 3
    if let Some(t) = extract::<T>(
        chain.call_order_provider.get(&provider_type),
        param,
        "call_order_provider",
    )? {
        return Ok(Some(t));
    }
    // Scope 4
    if let Some(t) = extract::<T>(chain.provider.get(&provider_type), param, "provider")? {
        return Ok(Some(t));
    }
    // SYSTEM
    let Some(sys) = system_default_json(param) else {
        return Ok(None);
    };
    match serde_json::from_value::<T>(sys) {
        Ok(t) => Ok(Some(t)),
        Err(e) => Err(ResolverError::TypeMismatch {
            param: param.to_string(),
            scope: "system",
            source: e,
        }),
    }
}

// ── Typed accessors (§2.3) ───────────────────────────────────────────────────
//
// Two groups:
//   - Scalar: return concrete T, falling back to SYSTEM_DEFAULT on
//     type mismatch (logged) or when neither scope nor system declares
//     the param (SYSTEM_DEFAULT used).
//   - Option-surfacing: return Option<T>; None is semantically
//     meaningful (no cap / skip this slot).
//
// Slot is passed to every accessor (even ones that don't differ
// per-slot) so Decision construction code can always call
// `resolve_X(&chain, slot, provider_type)` uniformly.

/// Helper: resolve with logging on TypeMismatch, returning the typed
/// default as fallback. The logging path is deliberately `warn!` not
/// `error!` — this is a recoverable fallback, not a crash.
fn resolve_or_default<T: DeserializeOwned>(
    chain: &ScopeChain,
    param: &str,
    slot: &str,
    provider_type: ProviderType,
    fallback: T,
) -> T {
    match resolve::<T>(chain, param, slot, provider_type) {
        Ok(Some(v)) => v,
        Ok(None) => fallback,
        Err(e) => {
            tracing::warn!(param = %param, error = %e, "resolver TypeMismatch — falling back to SYSTEM_DEFAULT");
            fallback
        }
    }
}

/// §3 `patience_secs`.
#[allow(dead_code)]
pub fn resolve_patience_secs(chain: &ScopeChain, slot: &str, provider_type: ProviderType) -> u64 {
    resolve_or_default(chain, "patience_secs", slot, provider_type, PATIENCE_SECS_DEFAULT)
}

/// §3 `patience_clock_resets_per_model`.
#[allow(dead_code)]
pub fn resolve_patience_clock_resets_per_model(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> bool {
    resolve_or_default(
        chain,
        "patience_clock_resets_per_model",
        slot,
        provider_type,
        PATIENCE_CLOCK_RESETS_PER_MODEL_DEFAULT,
    )
}

/// §3 `breaker_reset` (tagged union).
#[allow(dead_code)]
pub fn resolve_breaker_reset(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> BreakerReset {
    resolve_or_default(chain, "breaker_reset", slot, provider_type, BREAKER_RESET_DEFAULT)
}

/// §3 `sequential`.
#[allow(dead_code)]
pub fn resolve_sequential(chain: &ScopeChain, slot: &str, provider_type: ProviderType) -> bool {
    resolve_or_default(chain, "sequential", slot, provider_type, SEQUENTIAL_DEFAULT)
}

/// §3 `bypass_pool`.
#[allow(dead_code)]
pub fn resolve_bypass_pool(chain: &ScopeChain, slot: &str, provider_type: ProviderType) -> bool {
    resolve_or_default(chain, "bypass_pool", slot, provider_type, BYPASS_POOL_DEFAULT)
}

/// §3 `retry_http_count`.
#[allow(dead_code)]
pub fn resolve_retry_http_count(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u32 {
    resolve_or_default(
        chain,
        "retry_http_count",
        slot,
        provider_type,
        RETRY_HTTP_COUNT_DEFAULT,
    )
}

/// §3 `retry_backoff_base_secs`.
#[allow(dead_code)]
pub fn resolve_retry_backoff_base_secs(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u64 {
    resolve_or_default(
        chain,
        "retry_backoff_base_secs",
        slot,
        provider_type,
        RETRY_BACKOFF_BASE_SECS_DEFAULT,
    )
}

/// §3 `dispatch_deadline_grace_secs`.
#[allow(dead_code)]
pub fn resolve_dispatch_deadline_grace_secs(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u64 {
    resolve_or_default(
        chain,
        "dispatch_deadline_grace_secs",
        slot,
        provider_type,
        DISPATCH_DEADLINE_GRACE_SECS_DEFAULT,
    )
}

/// §3 `fleet_peer_min_staleness_secs`.
#[allow(dead_code)]
pub fn resolve_fleet_peer_min_staleness_secs(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u64 {
    resolve_or_default(
        chain,
        "fleet_peer_min_staleness_secs",
        slot,
        provider_type,
        FLEET_PEER_MIN_STALENESS_SECS_DEFAULT,
    )
}

/// §3 `fleet_prefer_cached`.
#[allow(dead_code)]
pub fn resolve_fleet_prefer_cached(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> bool {
    resolve_or_default(
        chain,
        "fleet_prefer_cached",
        slot,
        provider_type,
        FLEET_PREFER_CACHED_DEFAULT,
    )
}

/// §3 `network_failure_backoff_threshold`.
#[allow(dead_code)]
pub fn resolve_network_failure_backoff_threshold(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u32 {
    resolve_or_default(
        chain,
        "network_failure_backoff_threshold",
        slot,
        provider_type,
        NETWORK_FAILURE_BACKOFF_THRESHOLD_DEFAULT,
    )
}

/// §3 `network_failure_backoff_secs`.
#[allow(dead_code)]
pub fn resolve_network_failure_backoff_secs(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u64 {
    resolve_or_default(
        chain,
        "network_failure_backoff_secs",
        slot,
        provider_type,
        NETWORK_FAILURE_BACKOFF_SECS_DEFAULT,
    )
}

/// §3 `on_partial_failure`. Scope-2 only per §3 / Root 16, but the
/// resolver doesn't enforce scope — WS-D's Decision builder respects
/// the scope constraint.
#[allow(dead_code)]
pub fn resolve_on_partial_failure(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> PartialFailurePolicy {
    resolve_or_default(
        chain,
        "on_partial_failure",
        slot,
        provider_type,
        ON_PARTIAL_FAILURE_DEFAULT,
    )
}

/// §3 `ollama_base_url`.
#[allow(dead_code)]
pub fn resolve_ollama_base_url(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> String {
    resolve_or_default(
        chain,
        "ollama_base_url",
        slot,
        provider_type,
        OLLAMA_BASE_URL_DEFAULT.to_string(),
    )
}

/// §3 `ollama_probe_interval_secs`.
#[allow(dead_code)]
pub fn resolve_ollama_probe_interval_secs(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> u64 {
    resolve_or_default(
        chain,
        "ollama_probe_interval_secs",
        slot,
        provider_type,
        OLLAMA_PROBE_INTERVAL_SECS_DEFAULT,
    )
}

/// §3 `active`. Scope-4 carriers ship bundled defaults that differ
/// per provider type (openrouter/fleet = true; local/market = false).
/// The SYSTEM floor is `true`; the resolver returns whatever scope
/// declares or the system floor otherwise. In production, every
/// provider has a scope-4 `walker_provider_*` contribution shipping
/// a deliberate default; this accessor is the fallback path.
#[allow(dead_code)]
pub fn resolve_active(chain: &ScopeChain, slot: &str, provider_type: ProviderType) -> bool {
    resolve_or_default(chain, "active", slot, provider_type, ACTIVE_DEFAULT)
}

/// §3 `max_budget_credits` — Option-surfacing. `None` = no cap; caller
/// passes through to /quote without `max_budget`.
#[allow(dead_code)]
pub fn resolve_max_budget_credits(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> Option<i64> {
    match resolve::<i64>(chain, "max_budget_credits", slot, provider_type) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(param = "max_budget_credits", error = %e, "resolver TypeMismatch — treating as None (no cap)");
            None
        }
    }
}

/// §3 `model_list` — SHAPE-PER-SCOPE Option-surfacing accessor.
///
/// - At scopes 1–2 (slot-scoped), stored as `Vec<String>` (slot is
///   the enclosing scope). Accessor returns that list directly.
/// - At scopes 3–4 (provider-wide), stored as
///   `HashMap<String, Vec<String>>` keyed by tier. Accessor indexes
///   on `slot` and returns the per-tier list, or `None` if that tier
///   isn't declared.
/// - First scope to declare wins. SYSTEM_DEFAULT is `None`.
///
/// `None` at every scope → walker skips this (slot, provider) pair
/// and emits `tier_unresolved` (§3 semantics).
#[allow(dead_code)]
pub fn resolve_model_list(
    chain: &ScopeChain,
    slot: &str,
    provider_type: ProviderType,
) -> Option<Vec<String>> {
    // Scopes 1 + 2: flat Vec<String>.
    if let Some(v) = chain
        .slot_provider
        .get(&(slot.to_string(), provider_type))
        .and_then(|e| e.overrides.get("model_list"))
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
    {
        return Some(v);
    }
    if let Some(v) = chain
        .slot
        .get(slot)
        .and_then(|e| e.overrides.get("model_list"))
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
    {
        return Some(v);
    }
    // Scopes 3 + 4: HashMap<String, Vec<String>>; index on slot.
    if let Some(v) = chain
        .call_order_provider
        .get(&provider_type)
        .and_then(|e| e.overrides.get("model_list"))
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<HashMap<String, Vec<String>>>(v.clone()).ok())
        .and_then(|m| m.get(slot).cloned())
    {
        return Some(v);
    }
    if let Some(v) = chain
        .provider
        .get(&provider_type)
        .and_then(|e| e.overrides.get("model_list"))
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<HashMap<String, Vec<String>>>(v.clone()).ok())
        .and_then(|m| m.get(slot).cloned())
    {
        return Some(v);
    }
    None
}

/// §2.8 tier set = union of `model_list` keys across active provider
/// configs at scopes 3 and 4. Callers: Settings UI autocomplete /
/// validation; `test_bundled_tier_coverage` regression guard.
///
/// Does NOT walk scopes 1-2 — those are `Vec<String>` (slot is the
/// enclosing scope, not a key) and therefore don't contribute new
/// tier names beyond what scopes 3-4 already declare.
#[allow(dead_code)]
pub fn tier_set_from_chain(chain: &ScopeChain) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    let pull = |set: &mut std::collections::BTreeSet<String>, e: &ScopeEntry| {
        if let Some(v) = e.overrides.get("model_list") {
            if let Ok(map) = serde_json::from_value::<HashMap<String, Vec<String>>>(v.clone()) {
                for tier in map.keys() {
                    set.insert(tier.clone());
                }
            }
        }
    };
    for entry in chain.call_order_provider.values() {
        pull(&mut set, entry);
    }
    for entry in chain.provider.values() {
        pull(&mut set, entry);
    }
    set
}

// ── build_scope_cache (integration surface for WS-E) ─────────────────────────
//
// Reads the active `walker_*` contributions and assembles a
// ScopeChain, then wraps in a ScopeCache that WS-E's boot sequence
// plugs into the reloader's `rebuild_fn` slot. Missing contributions
// are treated as empty scopes — the bundled manifest (WS-B) will
// ship defaults; until then, test code reading from an empty DB
// still gets a useable cache.
//
// Malformed YAML bodies are logged and skipped for THAT scope's
// portion — rest of the chain loads normally. Rationale: a single
// hand-edited contribution with a YAML typo shouldn't brick every
// Decision. §2.11's envelope writer catches shape errors at write
// time; this is a defense-in-depth fallback in case a row somehow
// skipped validation (schema_annotation not yet active for the type,
// for example).

/// Build a ScopeCache from active `walker_*` contributions in the DB.
///
/// Returns an empty-scope ScopeCache on a freshly-initialized DB
/// (no walker_* contributions yet). `DEFAULT_CALL_ORDER` is used as
/// the fallback when `walker_call_order` is absent.
///
/// Designed to plug into `spawn_scope_cache_reloader`'s `rebuild_fn`
/// parameter as `build_scope_cache` with signature
/// `Fn(&Connection) -> Result<ScopeCache>`.
#[allow(dead_code)]
pub fn build_scope_cache(conn: &Connection) -> Result<ScopeCache> {
    let mut chain = ScopeChain::default();
    let mut source_ids: Vec<String> = Vec::new();

    // Scope 4: one `walker_provider_<type>` per provider_type.
    for pt in ProviderType::ALL {
        if let Some(row) = load_active_config_contribution(conn, pt.schema_type(), None)? {
            match parse_provider_body(&row.yaml_content) {
                Ok(overrides) => {
                    chain.provider.insert(
                        pt,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(row.contribution_id.clone()),
                        },
                    );
                    source_ids.push(row.contribution_id);
                }
                Err(e) => {
                    tracing::warn!(
                        schema_type = pt.schema_type(),
                        contribution_id = %row.contribution_id,
                        error = %e,
                        "malformed walker_provider_* body — treating scope as empty"
                    );
                }
            }
        }
    }

    // Scope 3 + call_order: single `walker_call_order` contribution.
    if let Some(row) = load_active_config_contribution(conn, "walker_call_order", None)? {
        match parse_call_order_body(&row.yaml_content) {
            Ok((order, per_provider)) => {
                chain.call_order = order;
                for (pt, overrides) in per_provider {
                    chain.call_order_provider.insert(
                        pt,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(row.contribution_id.clone()),
                        },
                    );
                }
                source_ids.push(row.contribution_id);
            }
            Err(e) => {
                tracing::warn!(
                    schema_type = "walker_call_order",
                    contribution_id = %row.contribution_id,
                    error = %e,
                    "malformed walker_call_order body — using DEFAULT_CALL_ORDER"
                );
                chain.call_order = DEFAULT_CALL_ORDER.to_vec();
            }
        }
    } else {
        chain.call_order = DEFAULT_CALL_ORDER.to_vec();
    }

    // Scopes 1 + 2 + slot-level order overrides: single
    // `walker_slot_policy` contribution.
    if let Some(row) = load_active_config_contribution(conn, "walker_slot_policy", None)? {
        match parse_slot_policy_body(&row.yaml_content) {
            Ok(SlotPolicyParsed {
                slot_overrides,
                per_provider,
                slot_order_overrides,
            }) => {
                let cid = row.contribution_id.clone();
                for (slot_name, overrides) in slot_overrides {
                    chain.slot.insert(
                        slot_name,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(cid.clone()),
                        },
                    );
                }
                for ((slot_name, pt), overrides) in per_provider {
                    chain.slot_provider.insert(
                        (slot_name, pt),
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(cid.clone()),
                        },
                    );
                }
                chain.slot_call_order_overrides = slot_order_overrides;
                source_ids.push(row.contribution_id);
            }
            Err(e) => {
                tracing::warn!(
                    schema_type = "walker_slot_policy",
                    contribution_id = %row.contribution_id,
                    error = %e,
                    "malformed walker_slot_policy body — treating slot scopes as empty"
                );
            }
        }
    }

    Ok(scope_cache_from_chain(chain, source_ids))
}

/// Parse a `walker_provider_*` YAML body, returning the `overrides` map.
fn parse_provider_body(yaml: &str) -> Result<HashMap<String, serde_json::Value>> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        overrides: serde_yaml::Value,
    }
    let body: Body = serde_yaml::from_str(yaml)?;
    yaml_map_to_json_map(body.overrides)
}

/// Parse a `walker_call_order` YAML body. Returns
/// `(default_order, per_provider_overrides)`.
fn parse_call_order_body(
    yaml: &str,
) -> Result<(Vec<ProviderType>, HashMap<ProviderType, HashMap<String, serde_json::Value>>)> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        order: Vec<String>,
        #[serde(default)]
        overrides_by_provider: HashMap<String, serde_yaml::Value>,
    }
    let body: Body = serde_yaml::from_str(yaml)?;
    let mut order = Vec::with_capacity(body.order.len());
    for s in body.order {
        match ProviderType::from_str(&s) {
            Ok(pt) => {
                // Defense-in-depth dedup: a YAML with `order: [market, market]`
                // would otherwise make `effective_call_order` contain a
                // duplicate, and dispatch would try the same provider twice.
                // Envelope writer (§2.11) is the validator-of-record, but
                // the resolver is cheap to make idempotent here.
                if !order.contains(&pt) {
                    order.push(pt);
                } else {
                    tracing::warn!(
                        provider_type = %s,
                        "duplicate provider_type in walker_call_order.order — deduping"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    provider_type = %s,
                    error = %e,
                    "unknown provider_type in walker_call_order.order — skipping"
                );
            }
        }
    }
    let mut per_provider = HashMap::new();
    for (k, v) in body.overrides_by_provider {
        match ProviderType::from_str(&k) {
            Ok(pt) => {
                let map = yaml_map_to_json_map(v)?;
                per_provider.insert(pt, map);
            }
            Err(e) => {
                tracing::warn!(
                    provider_type = %k,
                    error = %e,
                    "unknown provider_type key in walker_call_order.overrides_by_provider — skipping"
                );
            }
        }
    }
    if order.is_empty() {
        order = DEFAULT_CALL_ORDER.to_vec();
    }
    Ok((order, per_provider))
}

/// Output of `parse_slot_policy_body` — three distinct surfaces
/// mapping into ScopeChain's scope-1, scope-2, and per-slot order
/// fields.
struct SlotPolicyParsed {
    slot_overrides: HashMap<String, HashMap<String, serde_json::Value>>,
    per_provider: HashMap<(String, ProviderType), HashMap<String, serde_json::Value>>,
    slot_order_overrides: HashMap<String, Vec<ProviderType>>,
}

/// Parse a `walker_slot_policy` YAML body. Slot names are arbitrary
/// strings; the nested `per_provider` + `overrides` + `order` shape
/// follows §4.3.
fn parse_slot_policy_body(yaml: &str) -> Result<SlotPolicyParsed> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        slots: HashMap<String, SlotBody>,
    }
    #[derive(Deserialize)]
    struct SlotBody {
        #[serde(default)]
        overrides: serde_yaml::Value,
        #[serde(default)]
        per_provider: HashMap<String, serde_yaml::Value>,
        #[serde(default)]
        order: Option<Vec<String>>,
    }

    let body: Body = serde_yaml::from_str(yaml)?;
    let mut slot_overrides = HashMap::new();
    let mut per_provider = HashMap::new();
    let mut slot_order_overrides = HashMap::new();

    for (slot_name, slot_body) in body.slots {
        let overrides_map = if slot_body.overrides.is_null() {
            HashMap::new()
        } else {
            yaml_map_to_json_map(slot_body.overrides)?
        };
        if !overrides_map.is_empty() {
            slot_overrides.insert(slot_name.clone(), overrides_map);
        }

        for (pt_key, pp_val) in slot_body.per_provider {
            match ProviderType::from_str(&pt_key) {
                Ok(pt) => {
                    let map = yaml_map_to_json_map(pp_val)?;
                    per_provider.insert((slot_name.clone(), pt), map);
                }
                Err(e) => {
                    tracing::warn!(
                        slot = %slot_name,
                        provider_type = %pt_key,
                        error = %e,
                        "unknown provider_type in walker_slot_policy.slots[x].per_provider — skipping"
                    );
                }
            }
        }

        if let Some(order) = slot_body.order {
            let mut pt_order = Vec::with_capacity(order.len());
            for s in order {
                match ProviderType::from_str(&s) {
                    Ok(pt) => {
                        // Defense-in-depth dedup — same rationale as
                        // walker_call_order.order above.
                        if !pt_order.contains(&pt) {
                            pt_order.push(pt);
                        } else {
                            tracing::warn!(
                                slot = %slot_name,
                                provider_type = %s,
                                "duplicate provider_type in walker_slot_policy.slots[x].order — deduping"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            slot = %slot_name,
                            provider_type = %s,
                            error = %e,
                            "unknown provider_type in walker_slot_policy.slots[x].order — skipping"
                        );
                    }
                }
            }
            if !pt_order.is_empty() {
                slot_order_overrides.insert(slot_name, pt_order);
            }
        }
    }

    Ok(SlotPolicyParsed {
        slot_overrides,
        per_provider,
        slot_order_overrides,
    })
}

/// Convert a `serde_yaml::Value` (expected to be a mapping) into a
/// `HashMap<String, serde_json::Value>` suitable for the
/// `overrides` map. The resolver stores JSON Values because
/// `serde_json::from_value` + a typed `T` is the canonical round-trip
/// at read time (§2.3 pseudocode `overrides.get::<T>(param)`).
fn yaml_map_to_json_map(v: serde_yaml::Value) -> Result<HashMap<String, serde_json::Value>> {
    if v.is_null() {
        return Ok(HashMap::new());
    }
    // serde_yaml::Value → serde_json::Value via the canonical
    // round-trip. serde_yaml's mapping keys can be arbitrary YAML
    // values; the walker_* schemas use string keys exclusively, so
    // any non-string key at the top level is a shape error.
    let json_val = serde_json::to_value(&v)?;
    match json_val {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        serde_json::Value::Null => Ok(HashMap::new()),
        other => anyhow::bail!(
            "expected mapping for overrides, got {:?}",
            other_type_name(&other)
        ),
    }
}

fn other_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ── ScopeCache extension (WS-A option A: attach ScopeChain) ─────────────────
//
// WS-A option A (cleaner) over B (new constructor method): the
// placeholder field on `ScopeCache` stays, and this module's
// `build_scope_cache` returns a fresh ScopeCache whose `scope_chain`
// is carried alongside via the `ScopeCacheWalkerExt` wrapper below.
//
// Why not mutate walker_cache.rs directly: WS-E will wire the chain
// into ScopeCache in the same commit that swaps the rebuild_fn.
// Touching walker_cache.rs here also risks collision with that
// workstream. WS-A's contract is "produce a ScopeCache from a
// Connection"; WS-E's contract is "plug that into the reloader."
// The handoff seam is `ScopeCacheWalkerData` below — a pair type
// holding (ScopeCache, Arc<ScopeChain>) that WS-E can unpack.
//
// WS-E (Phase 0b) took exactly that path: `ScopeCache` now carries
// `pub scope_chain: Arc<ScopeChain>`. `scope_cache_from_chain` below
// wraps the chain in an Arc at construction so dispatchers can read
// `cache.scope_chain` directly off the ArcSwap. `ScopeCacheWalkerData`
// remains available for callers that want (cache, chain) as a pair
// without unwrapping the Arc twice.

/// Constructs a `ScopeCache` from an assembled `ScopeChain` plus the
/// contribution_ids that fed it. `source_contribution_ids` is what
/// the chronicle redacted view carries; the chain itself is stored on
/// `cache.scope_chain: Arc<ScopeChain>` for dispatchers reading off
/// the ArcSwap (WS-D DispatchDecision + future phase dispatchers).
/// `build_scope_cache_pair` remains for callers that want the chain
/// as a separate Arc handle without reaching through the cache.
fn scope_cache_from_chain(chain: ScopeChain, source_contribution_ids: Vec<String>) -> ScopeCache {
    ScopeCache {
        built_at: SystemTime::now(),
        source_contribution_ids,
        scope_chain: Arc::new(chain),
    }
}

/// Pair of (cache envelope, resolved chain). WS-E will consume this
/// at boot: the cache goes into `ArcSwap<ScopeCache>`, the chain
/// goes into whatever resolver-facing surface Decision builder
/// (WS-D) reads from.
#[allow(dead_code)]
#[derive(Debug)]
pub struct ScopeCacheWalkerData {
    pub cache: ScopeCache,
    pub chain: Arc<ScopeChain>,
}

/// Sibling to `build_scope_cache` that returns BOTH the cache
/// envelope and the resolved chain. Prefer this in Decision-builder
/// (WS-D) and boot (WS-E) code paths that need to read from the
/// chain. `build_scope_cache` keeps its single-return signature for
/// drop-in use as `rebuild_fn` in the reloader.
#[allow(dead_code)]
pub fn build_scope_cache_pair(conn: &Connection) -> Result<ScopeCacheWalkerData> {
    // Inline a duplicate of build_scope_cache so we can return the
    // chain too. Kept duplicated (not refactored through a shared
    // helper) because a shared helper would force an Arc<ScopeChain>
    // allocation on the hot path even when callers only need the
    // cache envelope. Parallel maintenance surface is low — both
    // bodies track the same schema_types.
    let mut chain = ScopeChain::default();
    let mut source_ids: Vec<String> = Vec::new();

    for pt in ProviderType::ALL {
        if let Some(row) = load_active_config_contribution(conn, pt.schema_type(), None)? {
            match parse_provider_body(&row.yaml_content) {
                Ok(overrides) => {
                    chain.provider.insert(
                        pt,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(row.contribution_id.clone()),
                        },
                    );
                    source_ids.push(row.contribution_id);
                }
                Err(e) => {
                    tracing::warn!(
                        schema_type = pt.schema_type(),
                        contribution_id = %row.contribution_id,
                        error = %e,
                        "malformed walker_provider_* body — treating scope as empty"
                    );
                }
            }
        }
    }

    if let Some(row) = load_active_config_contribution(conn, "walker_call_order", None)? {
        match parse_call_order_body(&row.yaml_content) {
            Ok((order, per_provider)) => {
                chain.call_order = order;
                for (pt, overrides) in per_provider {
                    chain.call_order_provider.insert(
                        pt,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(row.contribution_id.clone()),
                        },
                    );
                }
                source_ids.push(row.contribution_id);
            }
            Err(e) => {
                tracing::warn!(
                    schema_type = "walker_call_order",
                    contribution_id = %row.contribution_id,
                    error = %e,
                    "malformed walker_call_order body — using DEFAULT_CALL_ORDER"
                );
                chain.call_order = DEFAULT_CALL_ORDER.to_vec();
            }
        }
    } else {
        chain.call_order = DEFAULT_CALL_ORDER.to_vec();
    }

    if let Some(row) = load_active_config_contribution(conn, "walker_slot_policy", None)? {
        match parse_slot_policy_body(&row.yaml_content) {
            Ok(SlotPolicyParsed {
                slot_overrides,
                per_provider,
                slot_order_overrides,
            }) => {
                let cid = row.contribution_id.clone();
                for (slot_name, overrides) in slot_overrides {
                    chain.slot.insert(
                        slot_name,
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(cid.clone()),
                        },
                    );
                }
                for ((slot_name, pt), overrides) in per_provider {
                    chain.slot_provider.insert(
                        (slot_name, pt),
                        ScopeEntry {
                            overrides,
                            contribution_id: Some(cid.clone()),
                        },
                    );
                }
                chain.slot_call_order_overrides = slot_order_overrides;
                source_ids.push(row.contribution_id);
            }
            Err(e) => {
                tracing::warn!(
                    schema_type = "walker_slot_policy",
                    contribution_id = %row.contribution_id,
                    error = %e,
                    "malformed walker_slot_policy body — treating slot scopes as empty"
                );
            }
        }
    }

    let chain_arc = Arc::new(chain);
    let cache = ScopeCache {
        built_at: SystemTime::now(),
        source_contribution_ids: source_ids,
        scope_chain: Arc::clone(&chain_arc),
    };
    Ok(ScopeCacheWalkerData {
        cache,
        chain: chain_arc,
    })
}

// ── Compile-time guard: ScopeChain MUST NOT impl Serialize ───────────────────
//
// Same pattern as walker_cache.rs::ScopeSnapshot. If a future dev adds
// `#[derive(Serialize)]` to ScopeChain, the `#[cfg(any())]` block below
// starts compiling (because `serde_json::to_value(&chain)` now type-
// checks), and we'd want CI to fail. The `#[cfg(any())]` block never
// actually compiles, but removing the attribute reveals the latent
// regression. Kept as documentation + grep-anchor.
#[cfg(any())]
#[allow(dead_code)]
fn _scope_chain_must_not_be_serializable(c: &ScopeChain) {
    // ── DO NOT ADD `#[derive(Serialize)]` TO `ScopeChain` ──
    // If this compiles, a Serialize impl exists and the type-level
    // redaction guard (§5.4.3 / Root 27) regressed on a NEW surface.
    // Fix: remove the derive and route serialization through
    // `ScopeSnapshot::redacted_for_chronicle()` per walker_cache.rs.
    let _ = serde_json::to_value(c).expect("must not compile");
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Make a ScopeEntry from a JSON map literal. Keeps tests terse.
    fn entry(vals: serde_json::Value) -> ScopeEntry {
        let serde_json::Value::Object(map) = vals else {
            panic!("entry() expects a JSON object");
        };
        ScopeEntry {
            overrides: map.into_iter().collect(),
            contribution_id: None,
        }
    }

    #[test]
    fn test_resolve_walks_chain_first_non_none_wins() {
        // Scope 4 only: resolver returns scope-4 value.
        let mut chain = ScopeChain::default();
        chain.provider.insert(
            ProviderType::Market,
            entry(json!({"patience_secs": 900_u64})),
        );
        let got = resolve::<u64>(&chain, "patience_secs", "mid", ProviderType::Market).unwrap();
        assert_eq!(got, Some(900));

        // Scope 2 wins over scope 4.
        chain
            .slot
            .insert("mid".to_string(), entry(json!({"patience_secs": 120_u64})));
        let got = resolve::<u64>(&chain, "patience_secs", "mid", ProviderType::Market).unwrap();
        assert_eq!(got, Some(120));

        // Scope 1 wins over scope 2.
        chain.slot_provider.insert(
            ("mid".to_string(), ProviderType::Market),
            entry(json!({"patience_secs": 30_u64})),
        );
        let got = resolve::<u64>(&chain, "patience_secs", "mid", ProviderType::Market).unwrap();
        assert_eq!(got, Some(30));
    }

    #[test]
    fn test_resolve_explicit_null_skips_scope() {
        // scope 2 declares patience_secs: null → treat as not declared;
        // resolver walks to scope 4 (900).
        let mut chain = ScopeChain::default();
        chain.slot.insert(
            "mid".to_string(),
            entry(json!({"patience_secs": serde_json::Value::Null})),
        );
        chain.provider.insert(
            ProviderType::Market,
            entry(json!({"patience_secs": 900_u64})),
        );
        let got = resolve::<u64>(&chain, "patience_secs", "mid", ProviderType::Market).unwrap();
        assert_eq!(got, Some(900));
    }

    #[test]
    fn test_system_default_on_missing() {
        let chain = ScopeChain::default();
        // accessor returns SYSTEM_DEFAULT (3600)
        let v = resolve_patience_secs(&chain, "mid", ProviderType::Market);
        assert_eq!(v, PATIENCE_SECS_DEFAULT);
        assert_eq!(v, 3600);
    }

    #[test]
    fn test_model_list_shape_per_scope() {
        // Scope 4 as Map<tier, Vec<String>>.
        let mut chain = ScopeChain::default();
        chain.provider.insert(
            ProviderType::OpenRouter,
            entry(json!({
                "model_list": {
                    "mid": ["a", "b"],
                    "high": ["grok"],
                }
            })),
        );
        let got = resolve_model_list(&chain, "mid", ProviderType::OpenRouter);
        assert_eq!(got, Some(vec!["a".to_string(), "b".to_string()]));

        // Scope 1 as flat Vec<String> — wins over scope 4.
        chain.slot_provider.insert(
            ("mid".to_string(), ProviderType::OpenRouter),
            entry(json!({"model_list": ["c"]})),
        );
        let got = resolve_model_list(&chain, "mid", ProviderType::OpenRouter);
        assert_eq!(got, Some(vec!["c".to_string()]));

        // Slot that isn't declared at scope 4 → None.
        let got_missing =
            resolve_model_list(&chain, "extractor", ProviderType::OpenRouter);
        assert_eq!(got_missing, None);
    }

    #[test]
    fn test_breaker_reset_string_shorthand_parses() {
        // FromStr round-trip.
        assert_eq!(
            BreakerReset::from_str("per_build").unwrap(),
            BreakerReset::PerBuild
        );
        assert_eq!(
            BreakerReset::from_str("probe_based").unwrap(),
            BreakerReset::ProbeBased
        );
        assert_eq!(
            BreakerReset::from_str("time_secs:300").unwrap(),
            BreakerReset::TimeSecs { value: 300 }
        );
        assert!(BreakerReset::from_str("garbage").is_err());

        // Deserialize from string form.
        let v: BreakerReset = serde_json::from_value(json!("per_build")).unwrap();
        assert_eq!(v, BreakerReset::PerBuild);
        let v: BreakerReset = serde_json::from_value(json!("time_secs:300")).unwrap();
        assert_eq!(v, BreakerReset::TimeSecs { value: 300 });

        // Deserialize from structured form.
        let v: BreakerReset =
            serde_json::from_value(json!({"kind": "per_build"})).unwrap();
        assert_eq!(v, BreakerReset::PerBuild);
        let v: BreakerReset =
            serde_json::from_value(json!({"kind": "time_secs", "value": 300})).unwrap();
        assert_eq!(v, BreakerReset::TimeSecs { value: 300 });

        // Serialize out the structured form.
        let ser = serde_json::to_value(BreakerReset::TimeSecs { value: 300 }).unwrap();
        assert_eq!(ser, json!({"kind": "time_secs", "value": 300}));
    }

    #[test]
    fn test_tier_set_from_model_list_keys() {
        let mut chain = ScopeChain::default();
        chain.provider.insert(
            ProviderType::OpenRouter,
            entry(json!({
                "model_list": {
                    "mid": ["m1"],
                    "high": ["h1"],
                }
            })),
        );
        chain.call_order_provider.insert(
            ProviderType::Market,
            entry(json!({
                "model_list": {
                    "mid": ["mkt-m1"],
                    "extractor": ["mkt-x"],
                }
            })),
        );

        let set = tier_set_from_chain(&chain);
        assert!(set.contains("mid"));
        assert!(set.contains("high"));
        assert!(set.contains("extractor"));
        assert_eq!(set.len(), 3);
    }

    /// Create a DB with just the pyramid_config_contributions table —
    /// enough for `load_active_config_contribution` to work.
    fn make_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("walker_resolver_test.db");
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

    /// Raw INSERT via the test allow-list path (test code is exempt
    /// from the envelope-writer invariant per scripts/check-insert-sites.sh).
    #[allow(clippy::too_many_arguments)]
    fn insert_active_contribution(
        conn: &Connection,
        contribution_id: &str,
        schema_type: &str,
        slug: Option<&str>,
        yaml_content: &str,
    ) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                 contribution_id, slug, schema_type, yaml_content, status, source
             ) VALUES (?1, ?2, ?3, ?4, 'active', 'bundled')",
            rusqlite::params![contribution_id, slug, schema_type, yaml_content],
        )
        .unwrap();
    }

    #[test]
    fn test_build_scope_cache_from_empty_db() {
        let (_dir, conn) = make_db();
        let data = build_scope_cache_pair(&conn).unwrap();
        // Empty chain, DEFAULT_CALL_ORDER fallback.
        assert!(data.chain.slot.is_empty());
        assert!(data.chain.slot_provider.is_empty());
        assert!(data.chain.call_order_provider.is_empty());
        assert!(data.chain.provider.is_empty());
        assert_eq!(data.chain.call_order, DEFAULT_CALL_ORDER.to_vec());
        assert!(data.cache.source_contribution_ids.is_empty());

        // resolve_patience_secs falls through to SYSTEM_DEFAULT.
        let v = resolve_patience_secs(&data.chain, "mid", ProviderType::Market);
        assert_eq!(v, PATIENCE_SECS_DEFAULT);
    }

    #[test]
    fn test_build_scope_cache_with_seeded_walker_provider_openrouter() {
        let (_dir, conn) = make_db();
        let yaml = r#"
schema_type: walker_provider_openrouter
version: 1
overrides:
  model_list:
    mid: [m1]
    high: [h1, h2]
  retry_http_count: 5
"#;
        insert_active_contribution(&conn, "c-or-1", "walker_provider_openrouter", None, yaml);

        let data = build_scope_cache_pair(&conn).unwrap();
        // model_list accessor reads scope 4 map-by-tier.
        let ml = resolve_model_list(&data.chain, "mid", ProviderType::OpenRouter);
        assert_eq!(ml, Some(vec!["m1".to_string()]));
        let ml_high = resolve_model_list(&data.chain, "high", ProviderType::OpenRouter);
        assert_eq!(ml_high, Some(vec!["h1".to_string(), "h2".to_string()]));
        // retry_http_count picked up from scope 4.
        let rhc = resolve_retry_http_count(&data.chain, "mid", ProviderType::OpenRouter);
        assert_eq!(rhc, 5);
        // Other parameters default.
        let ps = resolve_patience_secs(&data.chain, "mid", ProviderType::OpenRouter);
        assert_eq!(ps, PATIENCE_SECS_DEFAULT);
        // Source contribution id captured.
        assert!(data
            .cache
            .source_contribution_ids
            .iter()
            .any(|s| s == "c-or-1"));
    }

    #[test]
    fn test_build_scope_cache_with_slot_policy_and_call_order() {
        let (_dir, conn) = make_db();
        // Scope 3/call_order
        insert_active_contribution(
            &conn,
            "c-co-1",
            "walker_call_order",
            None,
            r#"
schema_type: walker_call_order
version: 1
order: [market, local, openrouter, fleet]
overrides_by_provider:
  market:
    patience_secs: 777
"#,
        );
        // Scope 1+2+slot-order
        insert_active_contribution(
            &conn,
            "c-sp-1",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  extract:
    overrides:
      patience_secs: 915
    per_provider:
      market:
        breaker_reset: "probe_based"
  synth_heavy:
    order: [openrouter]
"#,
        );

        let data = build_scope_cache_pair(&conn).unwrap();

        // Call-order populated from YAML.
        assert_eq!(
            data.chain.call_order,
            vec![
                ProviderType::Market,
                ProviderType::Local,
                ProviderType::OpenRouter,
                ProviderType::Fleet,
            ]
        );
        // Scope-3 patience_secs.
        let ps_mkt_mid = resolve_patience_secs(&data.chain, "mid", ProviderType::Market);
        assert_eq!(ps_mkt_mid, 777);
        // Scope-2 override wins for "extract" slot.
        let ps_mkt_extract = resolve_patience_secs(&data.chain, "extract", ProviderType::Market);
        assert_eq!(ps_mkt_extract, 915);
        // Scope-1 breaker_reset override.
        let br = resolve_breaker_reset(&data.chain, "extract", ProviderType::Market);
        assert_eq!(br, BreakerReset::ProbeBased);
        // Slot-level call-order override for synth_heavy.
        assert_eq!(
            data.chain.slot_call_order_overrides.get("synth_heavy"),
            Some(&vec![ProviderType::OpenRouter])
        );
    }

    #[test]
    fn test_resolver_type_mismatch_is_err() {
        // Store a string where the accessor expects u64 — resolver
        // bubbles up TypeMismatch; the typed accessor catches it and
        // falls back to SYSTEM_DEFAULT with a warn log.
        let mut chain = ScopeChain::default();
        chain.slot.insert(
            "mid".to_string(),
            entry(json!({"patience_secs": "not a number"})),
        );
        let res = resolve::<u64>(&chain, "patience_secs", "mid", ProviderType::Market);
        assert!(res.is_err(), "expected TypeMismatch, got {:?}", res);
        // Typed accessor survives:
        let v = resolve_patience_secs(&chain, "mid", ProviderType::Market);
        assert_eq!(v, PATIENCE_SECS_DEFAULT);
    }

    #[test]
    fn test_provider_type_roundtrip() {
        for pt in ProviderType::ALL {
            let s = pt.as_str();
            let back = ProviderType::from_str(s).unwrap();
            assert_eq!(pt, back);
            // schema_type suffix pattern.
            assert!(pt.schema_type().starts_with("walker_provider_"));
        }
        assert!(ProviderType::from_str("grok").is_err());
    }

    #[test]
    fn test_call_order_dedup_drops_duplicate_provider_entries() {
        // A hand-edited walker_call_order.order with duplicate entries
        // would otherwise make dispatch try the same provider twice in
        // a row. Resolver dedupes as defense-in-depth past §2.11.
        let (_dir, conn) = make_db();
        insert_active_contribution(
            &conn,
            "c-co-dup",
            "walker_call_order",
            None,
            r#"
schema_type: walker_call_order
version: 1
order: [market, market, local, market, openrouter]
"#,
        );
        let data = build_scope_cache_pair(&conn).unwrap();
        assert_eq!(
            data.chain.call_order,
            vec![
                ProviderType::Market,
                ProviderType::Local,
                ProviderType::OpenRouter,
            ],
            "duplicate provider_type entries must be deduped"
        );
    }

    #[test]
    fn test_slot_policy_order_dedup_drops_duplicate_provider_entries() {
        let (_dir, conn) = make_db();
        insert_active_contribution(
            &conn,
            "c-sp-dup",
            "walker_slot_policy",
            None,
            r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter, openrouter, market]
"#,
        );
        let data = build_scope_cache_pair(&conn).unwrap();
        assert_eq!(
            data.chain.slot_call_order_overrides.get("mid"),
            Some(&vec![ProviderType::OpenRouter, ProviderType::Market])
        );
    }

    #[test]
    fn test_max_budget_credits_option_surfacing() {
        let mut chain = ScopeChain::default();
        // Unset everywhere → None (no cap).
        assert_eq!(
            resolve_max_budget_credits(&chain, "mid", ProviderType::Market),
            None
        );
        // Scope-4 sets a cap.
        chain.provider.insert(
            ProviderType::Market,
            entry(json!({"max_budget_credits": 5000_i64})),
        );
        assert_eq!(
            resolve_max_budget_credits(&chain, "mid", ProviderType::Market),
            Some(5000)
        );
        // Explicit null at scope 2 doesn't shadow scope 4.
        chain.slot.insert(
            "mid".to_string(),
            entry(json!({"max_budget_credits": serde_json::Value::Null})),
        );
        assert_eq!(
            resolve_max_budget_credits(&chain, "mid", ProviderType::Market),
            Some(5000)
        );
    }
}
