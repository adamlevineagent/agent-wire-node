# Walker Provider Configs + Slot Policy (v3)

**Date:** 2026-04-21
**Status:** DRAFT (rev 0.6) — Stage 2 discovery audit absorbed. Pending Adam GO; implementation-ready.
**Rev:** 0.6
**Supersedes:** `inference-routing-v2-model-aware-config.md` (retired); rev 0.1 (six-API model, obsolete); rev 0.2 (resolver-chain reframe, this doc continues it).
**Author context:** planning thread, 2026-04-21. Rev 0.1 modeled this as six schemas with field lists. Rev 0.2 reframed as ONE resolver over a scope chain with schemas as thin value carriers. Rev 0.3 absorbs findings F1–F11 from `walker-v3-yaml-drafts.md` — shape-per-scope for `model_list`, `Option`-typed accessors replacing sentinels, `per_provider` block on slot-policy, provider readiness gates as a named layer parallel to the resolver, tier-set as union of provider `model_list` keys.

---

## 1. TL;DR

All walker-behavioral parameters resolve through a single chain at **chain-step entry**, producing one immutable **DispatchDecision** that carries every dispatcher through to the end of the step:

```
Slot × Provider-entry → Slot → Call-order × Provider-type →
    Provider-type → System default (bundled floor)          ↓
                                                    DispatchDecision
                                              (effective call_order +
                                               resolved params per provider +
                                               scope snapshot)
                                                          ↓
                                              StepContext.dispatch_decision
                                                          ↓
                                    All dispatchers read from the Decision
```

Most-specific → least-specific. First non-None wins. Any parameter — `max_budget_credits`, `patience_secs`, `breaker_reset`, `model_list`, `retry_count`, `sequential`, `bypass_pool`, anything future — resolves the same way. No special cases.

The schemas are value carriers at each scope, not API contracts with field lists. Adding a parameter = declare it at whatever scope, no schema change. Operator mental model: "declare where it matters; everything else cascades." Implementer mental model: one resolver function; one Decision per step; every lookup routes through the Decision.

**Walker reads all behavioral parameters through the Decision; orchestration logic (saturation-retry loop, X-Wire-Retry precedence, deadline-driven /fill await, engine serialization) remains in walker.** Rev 2.1.1 mechanics preserved — they now consume Decision fields instead of hardcoded Rust constants. Tier names are arbitrary strings, self-documenting via §2.8.

---

## 2. The resolution chain

### 2.1 Scopes

Five scopes, ordered most-specific to least-specific:

| # | Scope | Contribution | Who declares |
|---|---|---|---|
| 1 | **Slot × Provider entry** | `walker_slot_policy.slots[tier].order[N].overrides` | Operator, per-tier-per-provider |
| 2 | **Slot** | `walker_slot_policy.slots[tier].overrides` | Operator, per-tier |
| 3 | **Call-order × Provider type** | `walker_call_order.order[type].overrides` | Operator, per-type in default order (keyed on provider_type, not list position — see §2.7) |
| 4 | **Provider-type** | `walker_provider_<type>.overrides` | Operator, per-provider-type defaults |
| 5 | **System** | Rust const table | Bundled absolute floor |

### 2.2 Resolver

```rust
fn resolve<T>(
    param: &str,
    slot: &str,               // tier string from chain YAML step
    provider_type: &str,      // "local" | "fleet" | "openrouter" | "market"
) -> Option<T> {
    scope_slot_provider(slot, provider_type).overrides.get::<T>(param)
        .or_else(|| scope_slot(slot).overrides.get::<T>(param))
        .or_else(|| scope_call_order_provider(provider_type).overrides.get::<T>(param))
        .or_else(|| scope_provider(provider_type).overrides.get::<T>(param))
        .or_else(|| SYSTEM_DEFAULTS.get::<T>(param))
}
```

First non-None wins. All scope lookups key on `(slot, provider_type)`; list positions are not semantically meaningful. If no level declares, returns None; the caller (typed accessor) chooses what None means — usually the SYSTEM_DEFAULT, or `None` surfaced to the consumer (see §2.6). Resolver is `O(1)` per parameter given the scope objects are loaded in memory.

### 2.3 Override storage

Each scope's `overrides` is a `Map<String, serde_json::Value>` (or a typed enum with a serde-heterogeneous variant). Keeps schemas static — adding a parameter requires no schema migration; operator just writes a new key in YAML and the resolver picks it up.

For type safety at call sites, the resolver is wrapped in typed accessors per parameter. Accessors fall into two groups:

**Scalar accessors (most params):** return a concrete `T` with SYSTEM_DEFAULT as fallback.

```rust
fn resolve_patience_secs(slot: &str, provider_type: &str) -> u64 {
    resolve::<u64>("patience_secs", slot, provider_type).unwrap_or(PATIENCE_SECS_DEFAULT)
}
```

**Option-surfacing accessors (`max_budget_credits`, `model_list`):** return `Option<T>` directly so the consumer branches on absence instead of checking sentinels. Replaces rev 0.2's `(1<<53)-1` NO_BUDGET_CAP sentinel and the "empty-list-means-skip" convention — see F5.

```rust
fn resolve_max_budget_credits(slot: &str, provider_type: &str) -> Option<i64> {
    resolve::<i64>("max_budget_credits", slot, provider_type)
    // None = no cap; consumer passes through to /quote without max_budget
}
```

**`model_list` is shape-per-scope** — see F2. At scopes 3–4 (provider-wide) it is stored as `{tier: [models]}` because those scopes span multiple tiers. At scopes 1–2 (slot-scoped) the enclosing scope IS the tier, so the stored shape is `[models]`. The typed accessor unifies:

```rust
fn resolve_model_list(slot: &str, provider_type: &str) -> Option<Vec<String>> {
    // Scopes 1-2: stored as flat Vec<String> (slot is implicit).
    if let Some(v) = scope_slot_provider(slot, provider_type).overrides.get_flat("model_list") { return Some(v); }
    if let Some(v) = scope_slot(slot).overrides.get_flat("model_list")                         { return Some(v); }
    // Scopes 3-4: stored as Map<tier, Vec<String>>; index on slot.
    if let Some(v) = scope_call_order_provider(provider_type).overrides.get_tiered("model_list", slot) { return Some(v); }
    if let Some(v) = scope_provider(provider_type).overrides.get_tiered("model_list", slot)            { return Some(v); }
    None  // No scope declares this tier for this provider -> skip provider for this slot.
}
```

The shape split is honest: `model_list` is intrinsically tier-aware at broad scopes and already-scoped at narrow ones. The accessor is the only place that knows; the resolver core stays uniform.

Each typed accessor names the SYSTEM_DEFAULT (or `None` semantics) explicitly. Call sites call the typed accessor, never the raw resolver.

### 2.4 Override semantics

- **Declared at scope level N with a value:** that value wins for scopes > N. Lower-numbered scopes can still override.
- **Not declared:** scope is transparent for this parameter; resolver walks past it.
- **Declared with explicit `null`:** interpreted as "use the default from the next scope up" (same as not-declared). Distinction isn't useful at runtime but is useful in UIs that show "inherited from parent" vs "unset."

### 2.5 Resolver lives in its own module

`src-tauri/src/pyramid/walker_resolver.rs` — hosts the chain walker, the scope loaders (reading the 6 active contributions from config store), typed accessors per parameter, and the SYSTEM_DEFAULTS table. Walker dispatch, saturation-retry loop, fill deadline, etc. all call typed accessors here.

### 2.6 Provider readiness — each provider answers for itself

Each provider module implements:

```rust
pub trait ProviderReadiness {
    fn can_dispatch_now(&self, params: &ResolvedProviderParams) -> ReadinessResult;
}

pub enum ReadinessResult {
    Ready,
    NotReady { reason: NotReadyReason },
}

pub enum NotReadyReason {
    Inactive,                     // overrides.active == false
    NoModelListForSlot,           // resolved model_list is None or empty
    CredentialMissing,            // pyramid_providers.api_key_ref unresolvable (openrouter)
    OllamaOffline,                // probe stale or failed
    InsufficientCredit,           // market: cached balance < 1 credit
    WireUnreachable,              // market: can't verify balance AND grace window expired
    NoMarketOffersForSlot,        // market: MarketSurfaceCache shows 0 offers matching any slug in resolved model_list (Root 10 / Issue 2)
    SelfDealing,                  // market: only available offers come from this node's own publisher (Root 12 / Issue 1)
    NoReachablePeer,              // fleet: no peer younger than staleness cutoff
    NoPeerHasModel,               // fleet: announce shows no peer has listed model
}
```

Walker iterates the effective_call_order from the Decision, asking each provider. The gate answer is a reason (for the chronicle `provider_skipped_readiness` event), not a bare bool.

| Provider | Readiness answer |
|---|---|
| `local` | `active != false` AND last Ollama probe within `ollama_probe_interval_secs × 2` AND probe reported online AND model_list for this slot is non-empty. |
| `openrouter` | `active != false` AND `pyramid_providers.api_key_ref` resolves in the credential store AND model_list for this slot is non-empty. |
| `market` | `active != false` AND (cached server-credit balance ≥ 1 (TTL: 60s) **OR** `onboarding_complete_at` within last 5 minutes — Root 10 first-boot grace) AND model_list for this slot is non-empty AND `MarketSurfaceCache` shows ≥ 1 active offer matching any slug in resolved model_list where `offer.node_id != self.node_id` (NoMarketOffersForSlot / SelfDealing — Root 10 + 12). On balance-fetch failure outside the grace window, use last cached balance if younger than 5 minutes; only fail `WireUnreachable` if no recent cache AND no grace (F-D6). |
| `fleet` | `active != false` AND at least one peer in `FleetRoster.models_loaded` with `last_seen_at` younger than `fleet_peer_min_staleness_secs` has announced at least one model in the resolved model_list. **Announce-only for v3 (no on-demand probe infra exists today, per Q1).** |

The readiness check is called during Decision construction (pre-step), so the Decision's `effective_call_order` already excludes not-ready providers. Walker's loop never encounters a not-ready provider — it encounters a pre-filtered order and its `per_provider` map. This keeps dispatch-time logic simple and moves observability (reasons) to step entry.

Providers that COULD become ready mid-step (peer announce arrives, credit deposit settles) are not reconsidered within the current step. Next step builds a fresh Decision and picks them up. Tradeoff acknowledged: within a single step, a provider that comes online is skipped; but this preserves the Decision-is-immutable property and matches the credit-balance-TTL behavior.

### 2.7 Scope-3 keying — by provider_type, not position

Per F11: `walker_call_order.order` is a list for ordering, but per-entry overrides are keyed on `provider_type`, not list index. If the operator reorders market and local, their scope-3 overrides travel with the provider_type. The resolver's `scope_call_order_provider(provider_type)` reflects this.

Implementation-wise: load `walker_call_order.order` as a `Vec<OrderEntry>` (preserving order) AND build a `HashMap<provider_type, OrderEntry>` (for scope lookup). Two views, one source of truth.

### 2.8 Tier names are self-documenting

Per F6: there is no `walker_tiers` contribution and no canonical tier enumeration. The set of known tiers IS the union of `model_list` keys across all active provider configs (scope 3 and scope 4). The Settings UI reads that union as its autocomplete / validation source for chain-YAML tier references. Runtime `tier_unresolved` chronicle event fires if a chain references a tier no provider declares.

Consequence: operators can introduce new tier strings by editing any provider's `model_list` — no schema change, no registry. Typos remain a runtime concern; Phase 0's `test_bundled_tier_coverage` catches bundled regressions, and Settings UI catches operator typos before save.

### 2.9 DispatchDecision — the compute-once spine

At the start of each outer chain step (the call into `call_model_unified_with_audit_and_ctx`), a DispatchDecision is built and attached to `StepContext`:

```rust
pub struct DispatchDecision {
    pub slot: String,                              // tier from chain YAML step
    pub effective_call_order: Vec<ProviderType>,   // resolved at step entry
    pub per_provider: HashMap<ProviderType, ResolvedProviderParams>,
    pub scope_snapshot: Arc<ScopeCache>,           // for audit trail / chronicle
    pub on_partial_failure: PartialFailurePolicy,  // cascade | fail_loud | retry_same
    pub built_at: SystemTime,
}

pub struct ResolvedProviderParams {
    pub model_list: Option<Vec<String>>,           // None -> skip this (slot, provider)
    pub max_budget_credits: Option<i64>,
    pub patience_secs: u64,
    pub patience_clock_resets_per_model: bool,
    pub breaker_reset: BreakerReset,               // tagged union (see §2.11)
    pub sequential: bool,
    pub bypass_pool: bool,
    pub retry_http_count: u32,
    pub retry_backoff_base_secs: u64,
    pub dispatch_deadline_grace_secs: u64,
    pub active: bool,                              // readiness precondition
    // ...provider-specific fields (ollama_base_url, openrouter_credential_ref, etc.)
}
```

**Why this is the spine, not the resolver:**

- The Decision is built ONCE per step. All 194 existing callers of `config.primary_model` / `fallback_model_1` / `RouteEntry` become readers of `decision.per_provider[pt].model_list` (or the singular chosen-model field populated by the dispatcher after winning the call_order loop). Phase 1's touch surface drops from 194 sites to ~4-8 — the dispatchers, StepContext construction, and the decision builder.
- The Decision is IMMUTABLE for its step's lifetime. Mid-step supersession is impossible by construction; the snapshot is pinned the moment the Decision is built. This collapses the Q8 vs Q10 contradiction — "snapshot at dispatch start" becomes "snapshot at Decision construction," and "retried dispatch reads the same config" follows for free.
- `on_partial_failure` makes the bridge→OR cascade question an explicit policy field instead of emergent fallthrough behavior. Privacy becomes a choice, not an accident.
- `resolver_trace` chronicle event = serialize the Decision. No new event plumbing; observability is "what Decision did walker build for this step."
- Empty `model_list` + `active: true` fails loud at Decision construction with a `decision_build_failed` chronicle event, not silently at dispatch time.

**DADBEAR + the Decision:** DADBEAR's maintenance loop also calls `call_model_unified_with_audit_and_ctx`, so each DADBEAR dispatch builds its own Decision at entry. Config edits between DADBEAR compile and apply affect only the NEXT Decision. Preview/apply consistency is the property that DADBEAR snapshots config at compile time for preview, then the apply-time Decision is built fresh — document mismatch is visible in chronicle.

### 2.10 Bundled contributions extend the shipped manifest

**Stage 2 discovery caught that rev 0.5 reinvented existing infrastructure.** The shipped pattern is a single compile-embedded JSON manifest at `src-tauri/assets/bundled_contributions.json`, processed by `walk_bundled_contributions_manifest` at `src-tauri/src/pyramid/wire_migration.rs:1318`. It already carries ~12 bundled schemas (evidence_policy, dispatch_policy, triage, vocabulary, etc.) each with exactly the four-part pattern (`schema_definition` / `schema_annotation` / `generation_skill` / `default_seed`). The existing loader already sets `source: bundled` on the contribution envelope, so YAML bodies never carry `source:` — rev 0.5's "fix" for that was solving a non-problem.

**Rev 0.6 extends the shipped manifest** with walker_* entries. No sibling directory tree, no parallel loader.

Consequences:
- **Authoring ergonomics:** if per-schema YAML files in the repo are genuinely more pleasant to edit, add a build-time manifest-generator step that reads `bundled_contributions/walker_*/` files and emits the merged `bundled_contributions.json`. Runtime stays one loader; authoring stays ergonomic.
- `test_bundled_tier_coverage` reads the existing manifest via the same `include_str!`/`walk_bundled_contributions_manifest` path used by other tests.
- Operators editing via Tools > Create generate `source: operator_authored` supersessions. Bundled rows stay put.
- Skill slug lists are queried LIVE at skill-use time via prompt-template interpolation — e.g. `{{openrouter_live_slugs}}`, `{{ollama_available_models}}`, `{{market_surface_slugs}}`. Skill prompt bodies are authoring-time text; the dynamic values are injected by the skill runtime at invocation. This resolves F-D11 (Phase 0 said "baked at authoring time" — that's wrong and is now removed; §2.10 is the single source of truth for skill slug freshness).

### 2.11 Overrides map shape validation via schema_annotation

The `overrides` map holds `serde_json::Value` at runtime, but **per-parameter shape is declared in each schema's `schema_annotation` contribution**, and validated at the **contribution envelope writer** — the single choke point every write path must go through (Root 11 / Issue 4). Write paths that validate:

- Settings save handler (operator editing UI)
- Operator HTTP routes (`routes_operator.rs` config CRUD)
- Bundled manifest loader at boot
- Generation-skill confirmed supersessions
- Any future programmatic contribution writer

The envelope writer runs normalize-then-validate: normalization converts string shorthand (`"time_secs:300"` → `{kind: "time_secs", value: 300}`) and coerces empty lists to None; validation rejects anything that doesn't match the declared shape for the target schema's parameter. Failures return an error; no silent persist.

Schema_annotation carries, per parameter:

- `shape`: `scalar | list | tagged_union | tiered_map`
- `scope_behavior`: for `tiered_map` params (model_list), declares that shape at scopes 3-4 is `{tier: [values]}` and at scopes 1-2 is `[values]`
- `normalize`: rules like "empty list → None", "string `\"time_secs:300\"` → `{kind: \"time_secs\", value: 300}`"
- `sensitive`: `bool` (§2.12)

**Worked example — `breaker_reset` as a tagged union:**

```yaml
# schema_annotation for walker_provider_*
parameters:
  breaker_reset:
    shape: tagged_union
    variants:
      per_build: {}
      probe_based: {}
      time_secs:
        fields: { value: { type: u64, min: 1 } }
    accepts_string_shorthand:
      - pattern: "^(per_build|probe_based)$" -> { kind: "$1" }
      - pattern: "^time_secs:(\\d+)$" -> { kind: "time_secs", value: "$1" }
```

The resolver's typed accessor for `breaker_reset` reads the structured form. Operators can write either form in YAML; normalization runs at save. String `"time_secs:300"` becomes `{kind: "time_secs", value: 300}` before persistence. No stringly-typed parsing at runtime.

**Worked example — `model_list` shape-per-scope:** schema_annotation declares shape `tiered_map` with `scope_behavior: {scopes_3_4: "map_by_tier", scopes_1_2: "flat_list_scope_is_tier"}`. Save handler validates that a `model_list` appearing inside a scope-1 or scope-2 context is a flat list, not a map. User mistakes caught at save, not at runtime.

### 2.13 Maintenance paths — synthetic Decision

Not every code path that needs routing parameters runs inside an outer chain step. DADBEAR's compile-time preview, `stale_engine`'s periodic staleness checks, `compute_cascade_build_plan`'s cost estimation, and operator-HTTP preview routes all consult routing without a StepContext. Rev 0.5's Decision-first spine silently excluded these (Root 8 / F-D8 / Issue 5).

**Fix:** add a `DispatchDecision::synthetic_for_preview(slot, scope_snapshot) → DispatchDecision` builder that runs the resolver **without** calling `can_dispatch_now()` on any provider. The resulting Decision is complete on the params side but has `synthetic: true` and `effective_call_order = default_call_order_from_scopes()` (not runtime-readiness-filtered). Callers that want a "will this actually dispatch right now" answer call the full Decision builder; callers that want "what's the CONFIGURED routing for this slot" call synthetic.

Concrete consumers for the synthetic path:
- `stale_engine.rs:92-127` — staleness check constructors. Build synthetic Decision per check; read `per_provider[local].model_list` for the tier being checked.
- DADBEAR preview: builds synthetic at compile time, persists in preview payload. Apply-time builds a fresh full Decision; chronicle emits `preview_vs_apply_drift` if the two disagree on provider choice.
- `preview.rs` cost estimation: synthetic Decision → walk per_provider to aggregate cost bounds.
- Operator-HTTP preview routes: return synthetic Decision serialized as JSON.

**StepContext home (F-D2):** the canonical carrier for `dispatch_decision` is `src-tauri/src/pyramid/step_context.rs:275`'s struct (which already owns `build_id`, `model_tier`, `resolved_model_id`, `bus`). `chain_dispatch.rs:124`'s sibling `StepContext` is renamed `ChainDispatchContext` in Phase 0 to remove the name collision; callers using it for dispatch decisions are migrated to the `step_context::StepContext` version or pass the Decision explicitly. This is a ~40-LOC pre-requisite in Phase 0, budgeted.

### 2.14 Pre-commit plan-doc integrity pass (Root 9)

Every rev, before declaring audit-ready, the plan author runs:

1. **Grep every new concept:** each new struct field, parameter name, enum variant, contribution schema, or chronicle event must appear in ≥1 of the catalog tables (§3), scope tables (§2.1), or canonical definitions (§2.9/§2.11/§2.12). Orphan concepts are declared in the doc but not owned by any section.
2. **Cross-section consistency check:** for each skill claim, confirm §2.10, Phase 0's skill table, and any §2.12 reference agree. Rev 0.5 failed this on skill slug freshness (F-D11).
3. **Decision fields ↔ catalog:** every `DispatchDecision` field must have a corresponding `on_partial_failure`-style entry in the parameter catalog with a SYSTEM_DEFAULT, or be explicitly marked as derived from other fields (e.g. `scope_snapshot` is derived, not resolved).

This is a plan-author discipline, not a skill or tool. But it's named here so future revs can't claim audit-ready without it.

### 2.12 Sensitive-parameter authorization

Any parameter with `sensitive: true` in its schema_annotation triggers operator-confirmation dialog in Settings before the supersession is written. Sensitive fields:

- `openrouter_credential_ref` (could redirect to a wrong / empty / attacker-controlled key)
- `max_budget_credits` (could drain wallet if zeroed-out-to-None or lifted to astronomical)
- `order` (call_order or slot_policy — could silently bypass expected providers)
- `active` (disabling a provider without operator awareness)

Consequences:

- **Generation skills output DRAFT supersessions** into a preview lane in Tools > Create. Operator reviews, explicitly confirms sensitive changes, then commits. No auto-apply.
- **Skill prompts do NOT bake numeric literals.** SYSTEM_DEFAULTS values are injected at skill-use time via prompt template interpolation (`{{patience_secs_default}}`, `{{retry_http_count_default}}`). Updating a SYSTEM_DEFAULT ripples into all skill outputs without re-authoring prompts — eliminates the Pillar 37 violation.
- **Audit trail:** every sensitive supersession carries operator session ID and confirmation timestamp in the contribution envelope.

---

## 3. Parameter catalog

This table is the authoritative list of walker-behavioral parameters in v3. Anything not listed is not currently declarable; add a row + a SYSTEM_DEFAULT to declare a new one.

| Parameter | Type | Semantics | SYSTEM_DEFAULT |
|---|---|---|---|
| `active` | `bool` | Master switch for the provider type. `false` = readiness gate fails, provider is skipped in call_order. Field name matches existing `wire_compute_offers.structured_data.active` pattern (Q2 answer). | `true` for openrouter/fleet/market; `false` for local (opt-in to Ollama) |
| `model_list` | **shape-per-scope**: `Map<tier, Vec<String>>` at scopes 3–4, `Vec<String>` at scopes 1–2. Typed accessor returns `Option<Vec<String>>`. Semantics vary by provider: OR / market → ordered list walker tries; local → declarative claim of what Ollama serves for this tier; fleet → preferred models peers should have cached. | Consumed by all four provider types (readiness gate separates "willing" from "able"). `None` at every scope → walker skips this (slot, provider) pair, emits `tier_unresolved`. | `None` |
| `max_budget_credits` | `Option<i64>` | Per-dispatch credit ceiling fed to Wire's `/quote max_budget`. `None` = no cap (omit from /quote). | `None` |
| `patience_secs` | `u64` | Wall-clock budget for the saturation-retry loop (walker waits up to this long across all retries on this scope's market dispatch before giving up). | `3600` |
| `patience_clock_resets_per_model` | `bool` | Whether the patience clock resets when walker advances to the next model_id in the list vs. is a single budget across all models on this leg. | `false` (single budget per leg) |
| `breaker_reset` | `String` enum (`"per_build" \| "probe_based" \| "time_secs:N"`) | How the market circuit breaker's tripped state clears. | `"per_build"` |
| `sequential` | `bool` | Whether this scope's dispatches run strictly serialized at the engine (provider-side semaphore permits=1) vs. concurrent. | Provider-type dependent: `true` for local/market, `false` for openrouter/fleet (each provider-type's own default). System default = `true` (safest). |
| `bypass_pool` | `bool` | Whether to bypass the local provider-pools semaphore. | `false` |
| `retry_http_count` | `u32` | Per-dispatch HTTP retry count for transient provider errors (5xx / timeouts). | `3` |
| `retry_backoff_base_secs` | `u64` | Base for exponential backoff inside HTTP retries. | `2` |
| `dispatch_deadline_grace_secs` | `u64` | Grace appended to Wire's `dispatch_deadline_at` when computing walker's `/fill` await timeout. | `10` |
| `fleet_peer_min_staleness_secs` | `u64` | How old a peer announcement may be before fleet provider skips it. | `300` |
| `fleet_prefer_cached` | `bool` | Whether fleet provider prefers peers that have the requested model cached. | `true` |
| `on_partial_failure` | tagged enum `{cascade, fail_loud, retry_same}` | Decision-level policy (F-D16, Issue 10). What happens when a provider returns a retryable failure: `cascade` (try next provider in effective_call_order — default, matches current behavior); `fail_loud` (emit `dispatch_failed_policy_blocked` and stop — privacy-preserving default for slots where cross-provider prompt leakage matters); `retry_same` (stay on same provider, respect breaker and patience budget). Lives at scope 2 (slot) or scope 4 (provider-type). Sensitive because changing to `cascade` on a privacy-sensitive slot could leak prompts. | `cascade` |
| `ollama_base_url` | `String` | Local Ollama endpoint. | `"http://localhost:11434/v1"` |
| `ollama_probe_interval_secs` | `u64` | How often local provider config probes `/api/tags`. | `300` |
| ~~`openrouter_credential_ref`~~ | — | **Removed (F-D3).** The shipped `pyramid_providers.api_key_ref` column (set to `"OPENROUTER_KEY"` in the default seed) is already the canonical credential pointer used by the `ResolvedSecret` resolver. Walker's openrouter readiness gate reads from that column directly; no parallel field. Operators rotating keys via the shipped provider-registry UI affect walker dispatch without a second place to edit. | — |

Parameters are declared at whatever scope is natural. `ollama_base_url` lives at provider-type scope (unlikely to vary per-tier). `patience_secs` often lives at slot scope (operator wants long patience on extract, short on synth). `model_list` almost always lives at provider-type scope (this is the provider's tier→model table), with slot overrides for one-off cases.

**Semantic note for `model_list` per provider type** (F3 — resolver treats them uniformly; provider dispatchers interpret differently):

| Provider | `model_list` meaning |
|---|---|
| `openrouter` | Ordered list walker tries; falls through on rate-limit / 5xx. |
| `market` | Slugs to `/quote` against; walker picks first viable offer or falls through on saturation/absence. |
| `local` | Declarative claim ("Ollama serves these tiers"). Probe verifies; mismatch → provider skips, walker falls through. |
| `fleet` | Preferred models peers should have cached. Peer selection ranks peers by which models they've announced via node-to-node discovery (Q1: Wire heartbeat carries peer identity only, not model inventory — model knowledge is node-side via announce or on-demand probe). |

---

## 4. Contribution schemas (thin carriers)

Six `schema_type`s. Each is identical in shape: a thin carrier for an `overrides` map at its scope. The differentiator is WHICH scope the contribution declares for.

### 4.1 `walker_provider_local`, `walker_provider_fleet`, `walker_provider_openrouter`, `walker_provider_market`

Scope 4 carriers (provider-type defaults).

```yaml
schema_type: walker_provider_openrouter
version: 1
overrides:
  model_list:
    max: ["x-ai/grok-4.20-beta", "minimax/minimax-m2.7"]
    high: ["qwen/qwen3.5-flash-02-23"]
    mid: ["inception/mercury-2"]
    extractor: ["inception/mercury-2"]
  retry_http_count: 5
  sequential: false
```

`model_list` is per-tier but still lives in the `overrides` map (keyed on `"model_list"`) so it routes through the resolver like everything else. The resolver's typed accessor for `model_list` takes `slot` as input and indexes into the per-tier sub-map.

### 4.2 `walker_call_order`

Scope 3 carrier (per-provider-type defaults within the default ordering). Scope-3 overrides are keyed on `provider_type` (F11) — the `order` list controls sequencing only.

```yaml
schema_type: walker_call_order
version: 1
order: [market, local, openrouter, fleet]
overrides_by_provider:
  market:
    patience_secs: 900     # 15 min for market across all slots (unless slot-scope overrides)
  # other provider types use their scope-4 defaults
```

Two fields, one source of truth: `order` is the Vec<provider_type> controlling sequence; `overrides_by_provider` is the Map<provider_type, overrides> feeding scope 3.

### 4.3 `walker_slot_policy`

Scopes 1 & 2 carriers. Per F4: **`per_provider` (scope 1) is independent of `order` (scope 2)** — operator can override a single provider's params in a slot without restating the whole call-order.

```yaml
schema_type: walker_slot_policy
version: 1
slots:
  extract:
    overrides:                       # scope 2: slot-wide, applies to every provider in this slot
      patience_secs: 900
    per_provider:                    # scope 1: override for one provider in this slot, no reordering implied
      market:
        breaker_reset: "probe_based"
    # no `order` -> uses walker_call_order.order for this slot
  synth_heavy:
    order: [openrouter]              # scope 2: slot-specific ordering REPLACES call_order for this slot
  # Slots not listed fall through to walker_call_order entirely.
```

- `slots[tier].overrides` → scope 2.
- `slots[tier].per_provider[provider_type]` → scope 1.
- `slots[tier].order` → optional; when present, replaces `walker_call_order.order` for this slot only. Absent = inherit.

### 4.4 Provider-side counterparts (parity table)

The walker_* family is **requester-side only** (§8 F8 absorption). The node has parallel **provider-side** contributions that govern behavior when this node SERVES, not asks. Stage 2 discovery (F-D4, F-D7) flagged that operators will see both in Settings and can confuse them or drift parameters between them. This table names the parity:

| Concept | Requester-side (this plan) | Provider-side (already shipped) |
|---|---|---|
| Market participation | `walker_provider_market.overrides.active` | `compute_participation_policy` (whole-node gate; see §5.1) |
| Market patience | `walker_provider_market.overrides.patience_secs` | `market_delivery_policy.overrides.callback_post_timeout_secs` (analogous but different axis — delivery has its own timing) |
| Market retry/backoff | `walker_provider_market.overrides.retry_http_count` / `retry_backoff_base_secs` | `market_delivery_policy.overrides.backoff_base_secs` |
| Fleet peer staleness | `walker_provider_fleet.overrides.fleet_peer_min_staleness_secs` | `fleet_delivery_policy.overrides.peer_staleness_secs` |
| Fleet model list | `walker_provider_fleet.overrides.model_list` | Peer announces its own loaded models (no contribution — fleet announce protocol) |

**Settings UI treatment (Phase 6):** requester-side configs live in "Inference Routing" (this plan's surface). Provider-side configs live in the separate "Serve / Provider" area. Visual separator + cross-reference tooltip: "Requester-side = when I ASK. Provider-side = when I SERVE. Adjust both if you're a bridge operator."

Drift detection: if both sides declare ostensibly-parallel params (e.g. both set `patience_secs`-like), chronicle emits `requester_provider_param_drift` at boot — not a failure, just visibility so operators notice intentional vs accidental divergence.

---

## 5. Migration

### 5.1 Retires

| Retired surface | Absorbed into |
|---|---|
| `pyramid_tier_routing` table (flat tier→model) | `walker_provider_openrouter.overrides.model_list` (today's rows are all openrouter) |
| `config.primary_model` / `fallback_model_1` / `fallback_model_2` | `walker_provider_openrouter.overrides.model_list` — fallbacks become list entries |
| `dispatch_policy.routing_rules.route_to` (list of `RouteEntry`) | `walker_call_order.order` |
| `RouteEntry.model_id` override | Gone. Resolver asks provider-type at dispatch time. |
| `RouteEntry.tier_name` override | Gone. Tier flows from the chain step's `model_tier` field straight through the resolver. |
| `RouteEntry.max_budget_credits` | Moved to `overrides.max_budget_credits` at the scope that naturally owns it (usually provider-type; slot-scope override when operator wants tier-specific caps). |
| `RouteEntry.is_local` | Gone. `walker_provider_local` existence IS the local signal; fleet provider-type filters peers on its own criteria. |
| `RoutingRule.sequential` / `.bypass_pool` | Moved to `overrides.sequential` / `.bypass_pool`. Default = provider-type dependent (see parameter catalog). |
| Walker's local 15-min safety-rail timer | Replaced by deadline-driven await (already in place from rev 2.1.1) + `dispatch_deadline_grace_secs` parameter. |
| Today's "one provider per tier" rigidity | Per-provider-type resolution of the same tier string. |
| `compute_participation_policy.market_saturation_patience_secs` (F-D7) | Absorbed into `walker_provider_market.overrides.patience_secs`. The remaining CPP surface (whole-node "am I participating at all" gate) stays — it's orthogonal to walker's per-slot `active` flag. Migration: CPP's patience is folded into walker_provider_market's bundled seed on first boot. |
| `compute_participation_policy.market_dispatch_max_wait_ms` (F-D7) | Absorbed into `walker_provider_market.overrides.dispatch_deadline_grace_secs` (unit-converted). |
| `resolve_tier_registry` in `yaml_renderer.rs:428` reading from `pyramid_tier_routing` (F-D5) | Phase 0 rewrites to compute the union of `model_list` keys across active walker_provider_* contributions. Same public API, new data source. Evidence_policy and any other schema_annotation using `options_from: tier_registry` continue to work without change. |

### 5.2 Stays

- All rev 2.1.1 market mechanics (saturation classification, `X-Wire-Retry` header precedence, `AllOffersSaturatedDetail` deserialization, deadline-driven `/fill` await, provider-side engine serialization semaphore).
- Chain YAML tier references (opaque strings, resolved via the resolver chain at dispatch time).
- Wire compute-market API (still matches on `model_id`; market provider config picks the ID from its `model_list` to `/quote` against).
- Existing chronicle event types + `market_backoff_waiting` from rev 2.1.1.
- Contribution supersession + `schema_registry` dispatch + ConfigSynced event flow — all unchanged.

### 5.3 One-time migration on upgrade — TOTAL, not piecewise

On first boot after v3 ships, migration runs in one transaction. Either all steps succeed and the marker is set, or no changes are committed and app boots refusing to run walker v3 until migration retries. No half-migrated state ever exists.

**Pre-migration snapshot:** before any writes, snapshot every source row into `_pre_v3_snapshot_{pyramid_tier_routing, dispatch_policy, config}` tables. Rollback path: if migration fails mid-transaction, tables untouched; if migration succeeds but boot-time resolver returns inconsistent state, app refuses to proceed and suggests `--rollback-v3-migration` CLI flag to restore from snapshot.

**Migration steps (all in one transaction):**

1. **GROUP BY `provider_id` from `pyramid_tier_routing`.** Per Stage 1 audit: rows today include `provider_id` values of `"openrouter"`, `"ollama"`, `"ollama-local"`, `"fleet"`, `"market"` (verified at `src-tauri/src/pyramid/db.rs:1555-1568` and `fleet_mps.rs:421-458`). Map:
   - `"openrouter"` → emit `walker_provider_openrouter` contribution
   - `"ollama"` | `"ollama-local"` → emit `walker_provider_local` contribution
   - `"fleet"` → emit `walker_provider_fleet` contribution
   - `"market"` → emit `walker_provider_market` contribution
   - Unknown `provider_id` → loud warning chronicle event, row preserved in snapshot, migration continues (does NOT block first boot).

2. **Every column in `pyramid_tier_routing` gets a destination.** The table carries `tier_name`, `provider_id`, `model_id`, `context_limit`, `max_completion_tokens`, `pricing_json`, `supported_parameters_json`, `notes`. Migration must:
   - `model_id` → `overrides.model_list[tier_name] = [model_id]`
   - `context_limit` → add parameter `context_limit` to catalog, migrate value per (tier, provider)
   - `max_completion_tokens` → add parameter `max_completion_tokens` to catalog, migrate per (tier, provider)
   - `pricing_json` → add parameter `pricing_json` to catalog at provider-scope; preserves operator-custom pricing that won't be in Wire's market-surface
   - `supported_parameters_json` → add parameter `supported_parameters` to catalog at provider-scope
   - `notes` → preserved as `overrides._notes` (underscore prefix = non-resolvable metadata)

3. **`config.primary_model` / `fallback_model_{1,2}`.** Fold into `walker_provider_openrouter.overrides.model_list["mid" | "high" | "max"]` as tail fallbacks if not already present from Step 1. After migration, the struct fields are removed entirely — no `#[deprecated]` bridge period, no Phase 1→5 coexistence, no 194 deprecation warnings. Phase 1 completes all 194 site migrations in one pass or migration fails (snapshot preserved).

4. **`dispatch_policy.routing_rules.route_to`.** Translate the `Vec<RouteEntry>` into `walker_call_order` contribution: `order = [rt.provider_type for rt in route_to]`; `overrides_by_provider[rt.provider_type].max_budget_credits = rt.max_budget_credits` where non-None. `RouteEntry.is_local` boolean is discarded (provider_type distinguishes).

5. **`walker_slot_policy` starts empty.** No per-slot overrides until operator declares.

6. **Set `_v3_migration_marker` sentinel.** Only after steps 1-5 all succeed.

**Post-migration:** legacy tables/struct fields dropped in the same migration. No read-compatibility period — walker v3 is the only reader. `config.primary_model` removed from `AppConfig`. Pre-existing callers are migrated in the same Phase 1 commit; `cargo check` proves completeness.

**On boot:** app asserts `_v3_migration_marker` exists before instantiating walker. If missing, boot refuses with clear error directing operator to retry migration or rollback.

---

## 6. Phased implementation

Phases ship independently. Walker's dispatch path gets a new arm per phase as each provider-type's resolver integration lands.

LOC estimates corrected per audit findings — `primary_model` is ~93 source-site occurrences across 16 files (not 25), Phase 6 UI realistically 1200–1800 LOC (not 600), Phase 0 bumped by ~200 LOC for six generation skills + six schema annotations. Total revised: ~3700–4700 LOC, 9–12 sessions.

### Phase 0 — Resolver + Decision builder + schema groundwork (~1100 LOC, bumped for seventh skill + envelope validator + tier_registry rewrite + StepContext rename)

- `walker_resolver.rs` with scope-chain walker + typed accessors + SYSTEM_DEFAULTS table.
- `walker_decision.rs` — **DispatchDecision builder**. Called at outer-chain-step entry. Runs resolver per (provider_type), calls each provider's `can_dispatch_now`, assembles immutable Decision, emits `decision_built` chronicle event (with scope_snapshot serialized). This is the compute-once spine; every downstream consumer reads from `StepContext.dispatch_decision`.
- `bundled_contributions/` directory loader — reads YAML files from `src-tauri/bundled_contributions/walker_*/` at boot, creates `source: bundled` contributions via the envelope writer. No DB seed, no fixture shim. `test_bundled_tier_coverage` reads the same files.
- Per-parameter shape validator driven by schema_annotation (§2.11). Runs at YAML save time; catches user errors before persistence.
- `ArcSwap<ScopeCache>` pattern for ConfigSynced rebuilds — no locks held across await. Rebuild triggered by ConfigSynced event, consumed by next Decision builder call.
- Six `schema_type`s registered, each with FOUR bundled contributions:
  - `schema_definition` — field shape (thin since most params live in `overrides` map).
  - `schema_annotation` — drives the YAML-to-UI renderer in Tools > Create and Settings (field labels, help text, per-param widget hints — e.g. model_list rendered as repeatable string array per tier).
  - `generation_skill` — natural-language → YAML prompt. See below for per-skill notes.
  - `default_seed` — the `source: bundled` contribution the app ships with (Day-1 walker_provider_* seeds from §4, empty walker_slot_policy, default walker_call_order).
- Without all four, the Tools > Create tab shows "No generation skill registered" (see screenshot precedent) and the config is only editable via raw YAML — breaking the "edit in Settings without rebuilding" promise of walker v3.
- ConfigSynced listener: on supersession of any walker_* schema_type, rebuild the resolver's scope cache (snapshot-per-call clone semantics — no locks held across await).
- **Assertion test** (required per audit feedback): `test_bundled_tier_coverage` — verifies every tier name used in bundled chain YAMLs resolves to a non-empty `model_list` via at least one provider-type in the bundled call-order. Prevents Day-1 `tier_unresolved` regressions.

#### Generation skills (one per schema_type)

Each skill takes operator intent in natural language and emits a well-formed YAML for that schema. Slug-format awareness is the load-bearing piece — skills must know which format their target provider speaks (see §4 slug-format discipline).

| Schema | Skill scope | Slug format it emits |
|---|---|---|
| `walker_provider_openrouter` | "I want extract routed to mercury-2, high to grok" → OR-slug YAML with `model_list` per tier. Skill prompt must include canonical OR slug reference (pulled from live `/api/v1/models` at skill-authoring time; slugs baked into prompt). | `provider/model` (e.g. `inception/mercury-2`) |
| `walker_provider_local` | "BEHEM serves qwen-32b for high tier" → Ollama-slug YAML. Skill prompt includes common Ollama tag patterns. Can optionally cross-reference live `/api/tags` probe output if present. | Ollama tag (e.g. `qwen2.5:32b`) |
| `walker_provider_market` | "Request common 70B and smaller-GPU models from market" → market-slug YAML. Skill prompt includes the bundled seed list from Q4 as the known-good anchor, and notes that market slugs mirror what providers actually publish (Ollama-format for local-served offers, OR-format for bridge-served). | Matches what offers publish (mixed format possible) |
| `walker_provider_fleet` | "Prefer peers with the qwen-32b model cached for high tier" → fleet-slug YAML mirroring local format since fleet peers are typically running Ollama. | Ollama tag |
| `walker_call_order` | "Try market first then local then OR" → order array + any per-provider scope-3 overrides. Simplest skill; short prompt. | N/A (provider_type names, not slugs) |
| `walker_slot_policy` | "For extract, wait 15 minutes on market; bypass market entirely for synth_heavy" → nested slots map with `overrides` (scope 2), `per_provider` (scope 1), and optional `order`. Skill prompt must distinguish scope 1 vs scope 2 semantics clearly. | N/A (references provider_type + tier name) |
| `compute_market_offer` (Root 12 / NEW) | "I want to publish my BEHEM's 70B models for sale on the market at X credits/M-tokens" → offer YAML with model_id, pricing, backing-provider. Covers the bridge-operator authoring gap. | Matches market slugs (Ollama or OR format depending on backing provider) |

Each skill ships as a bundled contribution at Phase 0. Without it, the Create tab card for that schema is dead text. WITH it, operator intent → working YAML → live walker behavior, with no Rust change. Skill prompts use `{{placeholder}}` interpolation for live values (slug lists, SYSTEM_DEFAULTS); interpolation happens at skill-use time (Root 9 / F-D11 resolution).

### Phase 1 — Total migration + Decision-consumer refactor (~600 LOC, dropped from rev 0.4)

Scope corrected: actual site count is **194 occurrences across 18 files** (Stage 1 audit), NOT 93/16. But because Phase 1 now refactors consumers to read from `StepContext.dispatch_decision` instead of calling the resolver directly, the touch surface drops from 194 sites to **~4-8 sites** — the dispatchers themselves (local, openrouter, fleet, market), StepContext construction, and the Decision builder's caller. The other 186+ "sites" are removed: `config.primary_model` and friends are removed from `AppConfig`; migration (§5.3) populates `walker_provider_openrouter.model_list` as their replacement; callers that were reading config fields go through the Decision now.

- Implement the four provider dispatchers to read `decision.per_provider[ProviderType::...]` for their params.
- Decision builder (from Phase 0) runs resolver + readiness per provider at step entry.
- StepContext construction adds `dispatch_decision: DispatchDecision`.
- Delete `config.primary_model`, `fallback_model_1`, `fallback_model_2` struct fields. No `#[deprecated]` bridge. `cargo check` fails loud until every consumer is migrated (that's the point — enforces totality).
- Tests: Decision construction for each persona; intra-provider fallback walking model_list on rate-limit; Decision immutability across retry attempts within a step; chain-YAML tier coverage assertion; save-time shape validation catches invalid overrides.

### Phase 2 — Local provider config (~400 LOC)

- `walker_provider_local` consumption.
- Ollama probe integration (v2 plan's `ollama_probe.rs` carries forward).
- Retires `provider.enabled == false but still serves` anomaly — enabled-gate moves into the provider config's `overrides.active: false` sentinel parameter, walker respects.
- Tests: Ollama probe, fallback-if-model-not-loaded, enabled-gate honored.

### Phase 3 — Market provider config + active /quote pre-gate (~500 LOC, bumped)

Wire dev confirmed rev 2.1.1 surfaced `typical_serve_ms_p50_7d` per-offer and `model_typical_serve_ms_p50_7d` model-row fallback; `queue_position` on `/quote` price_breakdown since rev 2.1. **Pre-gate ships ACTIVE in this phase**, not dormant:

- `walker_provider_market` consumption.
- /quote pre-gate (Q3 answer — three distinct surfaces):
  - `typical_serve_ms_p50_7d` per-offer: `GET /api/v1/compute/market-surface?model_id=X` → `models[0].offers[].typical_serve_ms_p50_7d`. Walker reads from its local market-surface cache (poll or `/market-surface/stream` SSE). Per-offer null → fall back to `models[].model_typical_serve_ms_p50_7d` on the model row. Both null → skip pre-gate, trust static deadline.
  - `queue_position` at quote time: `POST /api/v1/compute/quote` → `response.price_breakdown.queue_position`.
  - `queue_position` + `matched_queue_depth` at purchase time (post-commit visibility): `POST /api/v1/compute/purchase` response root.
  - Pre-gate formula: skip offer if `queue_position × typical_serve_ms_p50_7d` exceeds `dispatch_deadline_at − dispatch_deadline_grace_secs`.
- Market saturation-retry loop (already shipped in rev 2.1.1) now reads `patience_secs` and `patience_clock_resets_per_model` through the resolver.
- Bridge operator pattern (market config publishes offers for OR slugs) falls out naturally.
- **Bundled market seed** (Q4 answer): Wire does NOT publish a canonical `walker_provider_market` — walker policy is node-domain per SYSTEM.md §1.6; a Wire-published default would violate the "Wire surfaces inputs, walker decides" line. Node ships a pragmatic seed list: `gemma4:26b` (current sole prod offer; what BEHEM publishes), `llama3.1:70b` (48GB-class), `qwen2.5:14b-instruct` (smaller GPUs), `mistral-small-24b` (mid-size), optionally `llama3.1:8b` for CPU-servable floor. Operators supersede with their own `walker_provider_market` as the market diversifies.
- Tests: multi-model quote cascade, saturation-then-advance, pre-gate correctly skips unhitable offers, bridge-operator publishing pattern smoke.

### Phase 4 — Fleet provider config (~350 LOC)

- `walker_provider_fleet` consumption through the Decision.
- Peer-selection uses **announce-only** data from `FleetRoster.models_loaded` (per Stage 1 audit Q1: no on-demand peer-probe infrastructure exists in the codebase today; announce is the sole source). `fleet_peer_min_staleness_secs` gates announce recency.
- `can_dispatch_now` for fleet returns `NoPeerHasModel` if no reachable peer has announced any model in the resolved model_list; `NoReachablePeer` if no peer is fresh.
- On-demand peer probe is explicitly **out of scope for v3** — flag for a future `fleet_peer_probe.rs` if it becomes needed; v3 ships without it.
- Tests: tier-to-peer resolution with multiple peers announcing overlapping models; NoPeerHasModel correctly blocks Decision inclusion; stale peer is skipped.

### Phase 5 — Call-order + slot-policy + per-build circuit breaker (~500 LOC)

- `walker_call_order` and `walker_slot_policy` consumed by the Decision builder (Phase 0 mechanics fully wired).
- **Per-build circuit breaker:** state in `Arc<RwLock<HashMap<(build_id, slot, provider_type), BreakerState>>>`, keyed on StepContext's `build_id` (verified present at `step_context.rs:138,166,278`). `BreakerState = { tripped: bool, last_success_at: Option<Instant>, last_failure_at: Option<Instant> }`.
  - Race bound **explicitly named:** under concurrent chain steps for the same build, up to `N = number of in-flight steps` wasted market attempts per trip. Acceptable for v3; can be tightened to atomic CAS if observed in practice.
  - Breaker state consulted in the Decision builder — a tripped breaker excludes its provider_type from `effective_call_order` for steps against that (build, slot, provider).
  - `breaker_reset` variants (§2.11 tagged union): `per_build` (never un-trips within a build), `probe_based` (un-trips on next successful health probe), `time_secs:N` (un-trips after N seconds wall clock from trip).
- **Legacy fallback path is not part of this phase** — Phase 1 deleted it in the same commit as the Decision refactor. Phase 5 is pure feature work on the breaker + slot-policy surfaces.
- Tests: breaker trip/reset under each variant; slot-policy `order` replaces call-order for that slot; slot-policy `per_provider` applies without reordering; builds complete when breaker trips mid-build; concurrent breaker-update races stay within the stated bound.

### Phase 6 — UI + onboarding (~2200–2800 LOC, Stage 2 discovery revised up)

Audit noted `InferenceRoutingPanel.tsx` is already 894 lines; six configs + list-editing for model arrays + per-entry patience/breaker + onboarding wizard + live breaker visibility → honest range is 1400–1800 LOC, likely 2 sessions.

- Settings surface renders six configs + call-order + slot-policy via the generative-YAML-to-UI renderer.
- First-launch onboarding wizard (v2 plan's wizard carries forward, re-scoped to the resolver framing).
- Chronicle events for live breaker visibility and Decision trace: `decision_built` (serialized Decision — answers "why did walker route to X"), `breaker_tripped`, `breaker_skipped`, `config_superseded`, `tier_unresolved`, `provider_skipped_readiness` (with `reason: NotReadyReason` — specific), `decision_build_failed` (empty model_list + active:true case), `sensitive_supersession_confirmed` (audit trail).
- Probe-driven dropdowns for per-provider-type model suggestions.

### Total: ~4350–5150 LOC, 10–13 sessions (rev 0.6: Phase 0 bumped to 1100, Phase 6 revised up to 2200-2800)

---

## 7. What collapses from rev 0.1's open items / Q&A

The resolver reframe absorbs most of rev 0.1's open items and the audit's Q&A list. Showing the collapse so Stage 1 auditors don't re-raise them:

| rev 0.1 item | Resolver-frame answer |
|---|---|
| O1 — Tier unresolved at all provider-types | `resolve_model_list()` returns empty. Consumer emits `tier_unresolved` chronicle event. No special logic. |
| O2 — Slot declared but no provider-type resolves | Resolver walks scopes normally; if no scope declares, returns empty; consumer handles. |
| O3 — Breaker reset triggers | `breaker_reset` parameter in the resolver chain. System default `per_build`; operator overrides at any scope. Probe-based / time-based are enum values, handled by the breaker's clear logic. |
| O4 — Existing operator customizations | Migration §5.3. |
| O5 — /quote pre-gate dormant | **No longer dormant.** Wire rev 2.1.1 surfaced the fields; pre-gate ships active in Phase 3. |
| O6 — Bundled market seed | Resolved (Q4): node-domain, Wire does not publish canonical. Seed list baked into binary (gemma4:26b / llama3.1:70b / qwen2.5:14b-instruct / mistral-small-24b), operators supersede. No wire_pulled path for this contribution type. |
| Audit Q1 — BuildHandle vs StepContext for breaker state | Parallel `Arc<RwLock<HashMap>>` keyed on StepContext's `build_id`. (Named in Phase 5 above.) |
| Audit Q2 — `max_budget_credits` destination | Parameter in resolver chain. Natural home = provider-type scope. Slot-scope overrides per-tier. |
| Audit Q3 — `is_local` carry-forward | Gone. `walker_provider_local` existence IS the local signal; fleet provider filters on its own criteria. |
| Audit Q4 — `sequential` / `bypass_pool` destination | Parameters in resolver chain. Default differs per provider-type (see parameter catalog). |
| Audit Q5 — Phase 1 legacy-coexistence contract | Named in Phase 1 above: walker checks provider-type config first, falls to legacy `config.primary_model` when resolver returns empty. Removed Phase 5. |
| Audit Q6 — Saturation patience scope (per-model vs per-leg) | `patience_clock_resets_per_model: bool` parameter. Default false (single budget per leg). Operator overrides at any scope. |
| Audit Q7 — `pricing_json` destination | Lives on the offer rows Wire-side; node-side pricing metadata (if needed for cost reporting) is an `overrides.pricing_ref` pointer at provider-type scope. Low priority; cost chronicle works without it. |
| Audit Q8 — ConfigSynced locking discipline | Snapshot-per-call clone. Resolver takes an `Arc<ScopeCache>` at dispatch start; config supersessions rebuild the next `Arc<ScopeCache>` for subsequent calls. No locks held across await. |
| Audit Q9 — Chain-YAML tier coverage | Phase 0 ships `test_bundled_tier_coverage`. Build fails if coverage regresses. |
| Audit Q10 — stale_engine / DADBEAR + live supersession | DADBEAR's recursive maintenance loop reads the resolver at each dispatch (not snapshotted at schedule time) — picks up live supersession correctly. Tests in Phase 2 verify this via DADBEAR smoke with config edits mid-build. |
| Audit Q11 — O3 breaker reset "per-build only" vs probe-based | `breaker_reset` parameter supports `per_build` (default), `probe_based`, `time_secs:N`. Operator's call via slot-policy override. No design-level "one wins" — resolver handles all three. |
| Audit Q12 — O6 bundled seed source | Q4: Wire does not publish canonical walker_provider_market; seed is `bundled`-only, operators supersede to `operator_authored`. No `wire_pulled` step in the chain for walker_* contributions. |

---

## 8. Acceptance criteria

- **Tester smoke:** GPU-less tester installs app, asks question on small corpus. Bundled call-order `[market, local, openrouter, fleet]`. Decision at step entry: `market` ready (onboarding seeded ≥1 credit AND balance cache primed during onboarding Page 4 — see orchestration note below), `local` NotReady(OllamaOffline), `openrouter` NotReady(CredentialMissing), `fleet` NotReady(NoReachablePeer). `effective_call_order: [market]`. Network providers matching tester's market config serve via market. If market dry for the requested tier (`MarketSurfaceCache` has zero non-self offers) → `decision_build_failed` chronicle event with reason `NoMarketOffersForSlot`; build fails loud rather than silently burning patience on a dry market.
  - **Onboarding/walker orchestration (Root 10 / Issue 3):** `tester-onboarding.md`'s Page 4 (tunnel validation) additionally primes the market balance cache via a parallel `/balance` fetch alongside the existing handle-check. First build after onboarding boots into a warm cache. A `onboarding_complete_at` timestamp is written; market readiness's 5-minute grace window reads from it. Boots where the timestamp is >5min old require a fresh balance fetch before market readiness passes.
- **Hybrid operator (Adam-shape):** laptop + BEHEM + OpenRouter key. Call-order `[market, local, openrouter, fleet]`. Extract slot market patience 15 min via slot-policy override. Market serves local-model offers when BEHEM is up; extract work routes to BEHEM via market. Synth-heavy slot-policy bypasses market (`order: [openrouter]`), goes straight to OR. When BEHEM down longer than 15 min, breaker trips; extract falls to local (empty) → openrouter until build completes.
- **Bridge operator:** third operator's market config publishes OR-slug offers using their own OR key. Adam's market config requests those slugs. Match. Bridge serves, pays OR, Adam pays bridge in credits. Wire market primitive unchanged. Bridge's OWN walker_provider_market requests are filtered by the `SelfDealing` readiness reason so bridge never buys their own offers (Root 12 / Issue 1).

#### 8.1 Companion provider-side configuration (bridge operator end-to-end)

Bridge-operator acceptance requires contributions beyond the walker_* family. Rev 0.6 names them so this persona is actually implementable from the plan (F-D13 / Issue 9):

| Contribution | Purpose | Shipped today? | Generation skill? |
|---|---|---|---|
| `walker_provider_market` | Requester-side: bridge's own walker routes can still use market (for models they DON'T serve themselves) | This plan (Phase 3) | Yes (Phase 0 skill table) |
| `compute_market_offer` | Provider-side: declares what bridge publishes (model_id, pricing, backing-provider) | Phase 2 (shipped) | **NEW — added to Phase 0 as seventh skill** |
| `compute_participation_policy` | Whole-node gate: bridge commits to serving at all | Shipped | Existing skill |
| `market_delivery_policy` | Provider-side: bridge's response timing, retry, backoff | Shipped (`docs/seeds/market_delivery_policy.yaml`) | Existing skill |
| `api_key_ref` on `pyramid_providers` | OR key that bridge uses to back its OR-slug offers | Shipped | Provider-registry UI |

The onboarding wizard (§6 Phase 6) detects "operator wants to publish offers" intent and offers to walk through all five. Operators who only want to ASK (tester, standard) never see the provider-side cards.
- **Scope note (F8):** walker_* contributions are **requester-side only**. Provider-side publishing (bridge operator or Adam-as-market-provider) is a separate `compute_market_offer` contribution managed by Phase 2 code. "Extract via market to BEHEM" requires Adam to ALSO publish a market offer for BEHEM; the walker_* YAMLs alone express only the requester's intent to route through market.
- **Pillar conformance:** no Rust-hardcoded tier-to-model, no Rust-hardcoded retry/timeout/budget values, no Rust-hardcoded call order. All editable via contributions. Every walker-behavioral parameter resolves through the scope chain.
- **No regressions:** existing operator policies continue to work (migration §5.3); existing chain YAMLs continue to resolve (tier strings pass through unchanged); existing rev 2.1.1 market mechanics remain intact.
- **Coverage assertion:** `test_bundled_tier_coverage` passes — no bundled chain uses a tier string without provider-type coverage in bundled seeds.

---

## 9. What this plan does NOT do

- Capability-based market matching (`min_context_limit`, `quality_tier`, etc.). That's v4 and depends on Wire-side offer schema changes. v3 is string-matching via provider-type configs; capability-match is a future parameter type (e.g., `capability_requirement: CapabilityMatcher`) that'd plug into the resolver the same way as today's `model_list`.
- **Model slug coherence across providers (Issue 6).** If two operators publish offers for the same model under different slugs (`gemma4:26b` vs `ollama/gemma4:26b`), walker treats them as different models. v3 accepts this fragmentation; v4 introduces an alias-table parameter (`model_aliases: [["gemma4:26b", "ollama/gemma4:26b"]]`) or a Wire-side canonical-slug mapping. At current 1-offer market scale this is a minor concern; flagging now so future market growth has the fix pre-identified.
- **On-demand peer-probe protocol for fleet.** v3 is announce-only. v4 introduces `fleet_peer_probe.rs` that hits a peer's tunnel for `/api/tags` when announce data is stale or missing.
- New provider types beyond local/fleet/openrouter/market. Those four are the universe. A fifth (e.g., a new bridge class) adds a new schema_type + resolver arm.
- Migration of chain YAML tier names. Existing names pass through unchanged.
- Wire-side changes. Entirely node-side.
- **ts-rs binding generation for schema_annotation shapes.** Phase 6 UI duplicates shape validation logic in TypeScript; the Rust validator is authoritative. Drift risk flagged; ts-rs deferral preserved from v2. Track as separate initiative.
- **Worktree cleanup.** `.claude/worktrees/ui-debt-cleanup*/` directories must be removed (or merged back) BEFORE Phase 1's "delete config.primary_model" commit, or stale copies will silently rot. Not strictly in scope but noted as Phase 0 pre-flight.
- **Wire-surfaces contract doc.** A new one-pager at `docs/canonical/wire-surfaces-walker-depends-on.md` (NOT in this plan's scope) should record which Wire API fields walker's readiness and pre-gate consult (`typical_serve_ms_p50_7d`, `queue_position` location, `market-surface` shape). Protects against silent Wire-side refactors breaking walker. Flag for a follow-up doc.

---

## 10. Handoff discipline

Same as walker / rev 2.1.1 cycle:

- Plan + HANDOFF + IMPL-LOG + FRICTION-LOG per-phase.
- Orchestration: workflow agent → serial verifier → wanderer at phase gates.
- Pre-flight Q&A before first commit of each phase.
- **Pre-merge smoke discipline** (per `feedback_smoke_before_big_merge.md`): any phase touching classifier / dispatch / retry / timeout semantics requires live E2E smoke on the BRANCH before merging. Enforced for Phases 1, 3, 5 at minimum.
- Audit: Stage 1 informed + Stage 2 discovery over the whole plan, before Phase 0 implementation begins.
- Fast-forward merge to main after each phase's wanderer clean.

---

## 11. Audit history

- **Rev 0.1 (2026-04-21):** initial draft. Modeled v3 as six schemas with field lists and override rules. Audited by background thread `a77a1be18775b7c28`. Verdict: structurally correct direction, but under-estimated scope by 50% and missed that the real shape is ONE resolver over a scope chain, not six APIs. Rev 0.2 reframes.
- **Rev 0.2 (2026-04-21):** resolver-chain reframe. Six schemas become thin value carriers; one resolver function; parameters route through the same scope chain; rev 0.1's 12-Q&A list collapses into §7's table.
- **Rev 0.3 (2026-04-21):** YAML-drafting pass (`walker-v3-yaml-drafts.md`) surfaced 12 findings. Absorbed: F1 `enabled` parameter added to catalog; F2 `model_list` shape-per-scope made explicit in typed accessor; F3 per-provider `model_list` semantics table added to §3; F4 slot-policy split into `order` + `per_provider` blocks; F5 `Option`-typed accessors replace sentinels for `max_budget_credits` and `model_list`; F6 tier set = union of provider `model_list` keys (new §2.8); F7 provider readiness gates named as a layer parallel to the resolver (new §2.6); F8 requester-vs-provider scope note added to acceptance criteria; F11 scope-3 keyed on provider_type not list position (new §2.7, §4.2 restructured). F9 confirmed resolved by codebase check — `source` column already exists on contributions with `'bundled'` value, schema_registry queries it. F10 (slot-policy `order` is full-replace) accepted for 4 provider types. F12 was a non-finding.
- **Rev 0.4 (2026-04-21):** Product-owner Q&A resolutions.
  - **Q1 (fleet peer surface):** Wire heartbeat carries `{node_id, handle_path, tunnel_url}` only; no model columns on `wire_nodes`. Peer model inventory is node-domain, populated by node-to-node announce or on-demand probe. Fleet readiness gate and §3 semantics table updated.
  - **Q2 (enabled precedent):** renamed `enabled` → `active` across catalog, readiness gates, YAMLs — matches existing `wire_compute_offers.structured_data.active` pattern.
  - **Q3 (pre-gate surfaces):** Phase 3 pre-gate sources pinned. `typical_serve_ms_p50_7d` via `/market-surface` cache (per-offer, with `model_typical_serve_ms_p50_7d` fallback on null, skip pre-gate if both null). `queue_position` via `/quote.price_breakdown`. Also `/purchase` response root surfaces queue_position + matched_queue_depth post-commit.
  - **Q4 (bundled market seed):** Wire does not publish canonical `walker_provider_market` — node-domain per SYSTEM.md §1.6. No `wire_pulled` chain step for walker_* contributions. Bundled seed pinned: `gemma4:26b`, `llama3.1:70b`, `qwen2.5:14b-instruct`, `mistral-small-24b`, `llama3.1:8b`. Operators supersede as market diversifies.
  - Ready for Stage 1 informed audit against rev 0.4.
- **Rev 0.5 (2026-04-21):** Stage 1 informed audit (22 findings across 2 auditors) absorbed via the new `systemic-synthesis` skill. 16 of 22 findings collapsed into 6 structural roots; structural changes applied first, residuals after.
  - **Root 1 — No compute-once dispatch decision.** Collapses A-C1/C2/C4/C11, B-M2/M5/M6/M8, B-m6. Fix: introduce `DispatchDecision` (§2.9) built once at outer chain step entry, carried via StepContext, immutable for step lifetime. Phase 1 touch surface drops from 194 sites to ~4-8. Snapshot boundary = Decision lifetime (Q8 vs Q10 contradiction dissolves). Privacy fail-policy (`on_partial_failure`) is an explicit Decision field.
  - **Root 2 — Migration is total, not piecewise.** Collapses B-C1/C2, A-C13/C21, B-m5/m7. Fix: §5.3 rewritten. GROUP BY provider_id; every column gets a destination; snapshot backup before migration; all 194 consumer sites migrated in Phase 1's single commit; no `#[deprecated]` bridge; no Phase 1→5 legacy fallback.
  - **Root 3 — Bundled config is files, not DB seeds.** Collapses A-C8/C19, B-M8, B-m1. Fix: `src-tauri/bundled_contributions/walker_*/*.yaml` loaded at boot; skills query live APIs at use-time, not compile-time; offline test loader reads the same files; YAML bodies no longer carry `source:` field (envelope sets it).
  - **Root 4 — `can_dispatch_now()` per provider.** Collapses A-C3/C9, B-C3, B-M1. Fix: §2.6 rewritten. Each provider implements `can_dispatch_now() → Ready | NotReady(reason)`. Market: cached balance ≥1 credit TTL 60s, fail-closed on Wire-unreachable. Fleet: announce-only (on-demand probe out of scope for v3). Deferred-settlement removed (phantom feature). Readiness runs during Decision construction.
  - **Root 5 — Overrides map shape validation via schema_annotation.** Collapses B-m4, A-C5/C12. Fix: §2.11. Per-param shape declared in schema_annotation; save-time validator; `breaker_reset` as tagged union (`per_build` | `probe_based` | `time_secs:N`) with string-shorthand accepted; empty vec normalized to None.
  - **Root 6 — Sensitive-parameter authorization.** Collapses A-C14/C20, A-C8's auto-apply concern. Fix: §2.12. `sensitive: true` flag in schema_annotation. Generation skills emit DRAFT supersessions only; operator confirms. SYSTEM_DEFAULTS injected into skill prompts dynamically (eliminates Pillar 37 violation from baked numeric literals).
  - **Residuals patched individually:** A-C6/C7 (dead `per_provider` / `overrides_by_provider` entries — save-time warnings), A-C10 (per_build breaker semantics clarified in §3), A-C15 (ArcSwap named in Phase 0), A-C16 (§8 tester persona aligned with drafts), A-C17 (runtime `tier_unresolved` event), A-C18 (DADBEAR preview/apply documented in §2.9), A-C22 (worktree cleanup — not plan scope), B-M3 (breaker race bound explicitly named), B-M4 (naming consistency), B-M7 (market_surface_cache pre-check before /quote), B-m2 (overrides for unordered provider_type warning), B-m3 (slot-policy order full-replace UI warning).
  - Ready for Stage 2 discovery audit against rev 0.5.
- **Rev 0.6 (2026-04-21):** Stage 2 discovery audit (26 findings across 2 auditors) absorbed via systemic-synthesis. 18 of 26 findings collapsed into 6 additional structural roots (7–12); residuals patched individually.
  - **Root 7 — Plan reinvents instead of extending shipped infrastructure.** Collapses F-D1/D3/D4/D5/D7. Fix: §2.10 rewritten to extend `assets/bundled_contributions.json` manifest (not sibling directory); `openrouter_credential_ref` removed, `pyramid_providers.api_key_ref` is the canonical credential pointer; `resolve_tier_registry` rewritten to union-of-model_list-keys in Phase 0 (keeps live UI contract working); §5.1 retirement table adds `compute_participation_policy` overlapping fields; new §4.4 provider-side parity table naming `market_delivery_policy` / `fleet_delivery_policy` as counterparts with UI separation.
  - **Root 8 — Decision spine orphans non-step paths.** Collapses F-D2/D8/D14, Issue 5. Fix: new §2.13 `DispatchDecision::synthetic_for_preview()` for DADBEAR compile-time, stale_engine, cost estimation, and operator-HTTP preview routes — resolver-only, readiness-gates skipped. StepContext home pinned to `step_context.rs:275`; `chain_dispatch.rs:124`'s sibling renamed `ChainDispatchContext`. Phase 1 pre-work requires grep-and-taxonomize of `config.primary_model` hits.
  - **Root 9 — Plan self-contradictions.** Collapses F-D11/D16, Issue 10. Fix: §2.10 is now single source of truth for skill slug freshness (live at use-time via `{{placeholder}}` interpolation); `on_partial_failure` added to parameter catalog at scope 2/4 with `cascade` default and documented variants; new §2.14 pre-commit integrity pass discipline.
  - **Root 10 — Tester first-boot brittleness at onboarding/walker seam.** Collapses F-D6, Issues 2/3. Fix: §8 tester-smoke adds onboarding/walker orchestration (balance cache primed during Page 4 tunnel validation; `onboarding_complete_at` grace window of 5min); market readiness adds `NoMarketOffersForSlot` via MarketSurfaceCache consultation; fail-closed downgrades to last-cached if cache younger than 5min.
  - **Root 11 — Shape validator only enforces on one write path.** Collapses Issue 4. Fix: §2.11 rewritten — validator promoted to contribution envelope writer; every write path (Settings, HTTP operator routes, bundled manifest loader, skill supersessions) goes through one normalize-then-validate gate.
  - **Root 12 — Bridge-operator persona stated but not implementable.** Collapses F-D13, Issues 1/9. Fix: new §8.1 companion provider-side subsection naming all five contributions bridge operators must author; `compute_market_offer` added to Phase 0 as seventh generation skill; `SelfDealing` readiness reason filters offers where `offer.node_id == self.node_id`; onboarding wizard detects bridge intent and walks through all five.
  - **Residuals patched:** F-D9 (arc_swap in Cargo.toml — Phase 0 dep add), F-D10 (§2.1 clarification for scope-2 order vs overrides), F-D12 (SQLite migration mechanics spelled out in §5.3 — CREATE-COPY-DROP-RENAME dance), F-D15 (Phase 6 LOC revised up to 2200-2800), Issue 6 (slug coherence named as v4 in §9), Issue 8 (Phase 6 "last N Decisions" aggregation view added), plus §9 additions for worktree cleanup pre-flight, ts-rs double-maintenance flag, and Wire-surfaces contract doc as follow-up.
  - **Stage 2 Issue 7 verified-not-an-issue:** `/quote` returns `queue_position` nested inside `response.price_breakdown` — matches plan's claim. Verified against Next.js route.
  - Rev 0.6 is implementation-ready pending Adam GO.

Planned cadence from here:
1. Stage 1 informed pair audit against rev 0.3.
2. Stage 2 discovery pair audit.
3. Pre-flight Q&A with Adam (includes F9).
4. Rev 0.4 absorbing Stage 1+2 findings.
5. Hand to implementation thread, Phase 0 first.

---

## 12. Picking this up cold

For a fresh agent:

1. Read §2 (resolution chain) and §3 (parameter catalog) — that's the system.
2. Read §4 (schemas are thin carriers) — that's the shape of the six contributions.
3. Read §5 (migration) — what retires, what stays.
4. Read §6 (phases) — where implementation lands each piece.
5. Read §7 (collapse table) — why most "open questions" are parameters, not decisions.
6. Memory files:
   - `project_compute_market_purpose_brief.md` — why the market exists.
   - `feedback_canonical_100_years.md` — posture on shortcuts.
   - `feedback_constraints_often_load_bearing.md` — why static deadlines exist.
   - `feedback_smoke_before_big_merge.md` — pre-merge smoke discipline for big behavioral commits.
7. Grep current `pyramid_tier_routing`, `RouteEntry`, `call_model_unified_with_audit_and_ctx`, `classify_wire_error`. Internalize the current shape you're replacing.
8. Pre-flight Q&A with Adam.
9. Stage 1 audit against rev 0.2.
10. Rev 0.3 absorption.
11. Phase 0.

Estimated cold-start → Phase 0 commit 1: 3–4 hours.

---

**End of plan rev 0.2.**

Written 2026-04-21 by planning thread after Adam's resolver-chain pushback. Ready for Stage 1 audit.
