# Walker Provider Configs + Slot Policy (v3)

**Date:** 2026-04-21
**Status:** DRAFT (rev 1.0.1) — Cycle 4 Stage 1 residuals absorbed. No new structural roots; both auditors confirmed convergence. Pending Cycle 4 Stage 2.
**Rev:** 1.0.1
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
| 1 | **Slot × Provider entry** | `walker_slot_policy.slots[tier].per_provider[provider_type]` | Operator, per-tier-per-provider |
| 2 | **Slot** | `walker_slot_policy.slots[tier].overrides` | Operator, per-tier |
| 3 | **Call-order × Provider type** | `walker_call_order.overrides_by_provider[provider_type]` | Operator, per-type in default order (keyed on provider_type, not list position — see §2.7) |
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

Each scope's `overrides` is a `Map<String, serde_json::Value>` (or a typed enum with a serde-heterogeneous variant). Keeps schemas static — **redeclaring** an existing parameter at a new scope requires no schema migration; operator just writes the key in YAML and the resolver picks it up. **Genuinely-new parameter keys** require code (annotation + catalog + SYSTEM_DEFAULT + accessor); see §2.14.3.

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
    NetworkUnreachable {          // openrouter/market: network back-off active (§2.16.5, Root 20)
        consecutive_failures: u32,
        last_success_at: Option<SystemTime>,
    },
    NoMarketOffersForSlot,        // market: MarketSurfaceCache shows 0 offers matching any slug in resolved model_list
    SelfDealing,                  // market: only available offers come from this node's own publisher OR from this node's node_identity_history (§2.16.7)
    NoReachablePeer,              // fleet: no peer younger than staleness cutoff
    NoPeerHasModel,               // fleet: announce shows no peer has listed model in resolved model_list
    PeerIsV1Announcer,            // fleet: peer's announce_protocol_version < 2, strict mode refuses dispatch (§5.5.2)
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
    // ...provider-specific fields (ollama_base_url, ollama_probe_interval_secs, fleet_peer_min_staleness_secs, fleet_prefer_cached, etc.)
}
```

**Why this is the spine, not the resolver:**

- The Decision is built ONCE per step. All 194 existing callers of `config.primary_model` / `fallback_model_1` / `RouteEntry` conceptually collapse onto the Decision spine, but the implementation touch surface is NOT assumed to be "~4-8 sites". Phase 0a-1's consumer inventory is authoritative: Stage 2 already found 55+ hits in core files, so Phase 1 planning and LOC sizing must track the inventory artifact rather than a hand-wavy small-site estimate.
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

**Bundled loader skip-and-log mode (A-C4 / Root 22).** The bundled manifest loader at boot routes through the envelope writer (single choke point per Root 11). But a malformed bundled YAML (author error, or operator-authored-then-bundled drift from a build-time manifest-generator bug) that fails validation must NOT brick the install — other bundled contributions should still load. Phase 0a envelope writer has a `mode: WriteMode::{Strict, BundledBootSkipOnFail}` parameter. `Strict` (default, for runtime writes) returns error on validation failure. `BundledBootSkipOnFail` logs a `bundled_contribution_validation_failed` chronicle event (new; add to §5.4.6 local-only list) with the contribution and validation error, skips that row, continues loading. App boots; operator sees the error in chronicle.

**Placeholder-interpolated content injection escaping (A-C8 / Root 22).** Generation skills inject `{{market_surface_slugs}}` from Wire-controlled data — adversarial slug strings could contain YAML control characters (`:`, `\n`, quotes) that break shape in post-interpolation output. Placeholder engine v2 (Phase 0a) MUST (a) YAML-safe-quote every interpolated value (round-trip through a YAML string-encoder), (b) reject values containing null bytes or non-printable control chars at injection time, (c) validate post-substitution output is still well-formed YAML at skill-generation time (not just at draft-apply). Shape validator at the envelope writer catches only shape violations — semantic injection (right shape, adversarial intent) is a separate safety layer that lives at the placeholder engine.

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

### 2.12 Maintenance paths — synthetic Decision

Not every code path that needs routing parameters runs inside an outer chain step. DADBEAR's compile-time preview, `stale_engine`'s periodic staleness checks, `compute_cascade_build_plan`'s cost estimation, and operator-HTTP preview routes all consult routing without a StepContext. Rev 0.5's Decision-first spine silently excluded these (Root 8 / F-D8 / Issue 5).

**Fix:** add a `DispatchDecision::synthetic_for_preview(slot, scope_snapshot) → DispatchDecision` builder that runs the resolver **without** calling `can_dispatch_now()` on any provider. The resulting Decision is complete on the params side but has `synthetic: true` and `effective_call_order = default_call_order_from_scopes()` (not runtime-readiness-filtered). Callers that want a "will this actually dispatch right now" answer call the full Decision builder; callers that want "what's the CONFIGURED routing for this slot" call synthetic.

Concrete consumers for the synthetic path:
- `stale_engine.rs:92-127` — staleness check constructors. Build synthetic Decision per check; read `per_provider[local].model_list` for the tier being checked.
- DADBEAR preview: builds synthetic at compile time, persists in preview payload. Apply-time builds a fresh full Decision; chronicle emits `preview_vs_apply_drift` if the two disagree on provider choice.
- `preview.rs` cost estimation: synthetic Decision → walk per_provider to aggregate cost bounds.
- Operator-HTTP preview routes: return synthetic Decision serialized as JSON. Bearer-gated same as all `routes_operator.rs`. Emits `decision_previewed` (NOT `decision_built` — separate event, B-F5) so Builds-tab observability doesn't show phantom dispatches when operators click "preview routing" in Settings. No `can_dispatch_now` calls in synthetic mode, no outbound HTTP/DB write reachable from the preview path, no billing events emitted.
- Cost estimation across all tiers (A-F10): caller computes `tier_set_for_build(chain_id) -> Vec<String>` (union of `model_tier` strings across chain steps), then builds one `synthetic_for_preview(tier, scope_snapshot)` per tier, aggregates cost bounds. Synthetic builds skip breaker state (build_id may not exist yet — cost estimation runs pre-build) and readiness gates; they reflect **configured** routing only, not runtime availability.

**StepContext home (F-D2):** the canonical carrier for `dispatch_decision` is `src-tauri/src/pyramid/step_context.rs:275`'s struct (which already owns `build_id`, `model_tier`, `resolved_model_id`, `bus`). `chain_dispatch.rs:124`'s sibling `StepContext` is renamed `ChainDispatchContext` in Phase 0 to remove the name collision; callers using it for dispatch decisions are migrated to the `step_context::StepContext` version or pass the Decision explicitly. This is a ~40-LOC pre-requisite in Phase 0, budgeted.

### 2.13 Plan-doc integrity via `plan-integrity` skill (rev 0.8 — honest)

Rev 0.7 described this as a CI-gated script at `docs/tools/plan-integrity.sh`. Cycle 2 Stage 2 audit caught that agent-wire-node has no CI infrastructure (no `.github/`, no `justfile`, no `docs/tools/`), so the "mechanized" framing was a second recursion of the same "promised infrastructure without wiring" pattern rev 0.7 diagnosed.

**The honest mechanism: the `plan-integrity` skill** — a Claude-run discipline invoked between audit rounds by the `conductor-audit-pass` skill. Claude reads the plan + companions end-to-end, runs the 8 consistency checks below, auto-fixes safe drift, and surfaces judgment calls to the operator before launching the next audit round. Skill lives at `~/.claude/skills/plan-integrity/SKILL.md`.

This is the same enforcement architecture as `systemic-synthesis`: a required stage in the audit cycle, enforced by skill-discipline not CI.

The 8 checks:

1. **Placeholder resolution** — every `{{placeholder}}` in the plan must appear in a placeholder-registry section naming the resolver.
2. **Chronicle event registry** — every chronicle event name must appear in a single event-list section AND have a named emission location (existing file or Phase N).
3. **Struct field ↔ catalog** — every field in `DispatchDecision` / `ResolvedProviderParams` must appear in the §3 parameter catalog or be flagged `derived`.
4. **Absorbed-finding claims ↔ section content** — every §11 audit-history claim of form "X absorbed in §Y" must grep-verify in §Y.
5. **Section numbering** — monotonic.
6. **Cross-referenced state fields** — any field referenced as a state read must have a named write-site.
7. **Companion drafts doc rev-match** — `walker-v3-yaml-drafts.md` must carry a "synced to rev X" banner matching the plan's current rev.
8. **Sensitive-field catalog parity** — the sensitive-fields list (§2.15) must match exactly the set of parameters with `sensitive: true`.
9. **Count-assertion parity (rev 1.0 fixed)** — any prose claim of form "adds N events" / "N scopes" / "N schemas" must equal the **actual enumerated list cardinality** (not eyeballed). The skill counts enumerations by regex-matching list items, not by trusting prose. Rev 0.9 added this as a syntactic check but missed the 18-vs-20 drift because the implementation trusted the prose number; rev 1.0 inverts — count the list, verify prose matches.
10. **Enum variant coverage** — every `enum` variant referenced in prose (e.g. `NetworkUnreachable`, `PeerIsV1Announcer`) must appear in the corresponding Rust-shape definition in the plan.
11. **Audit-evidence artifact** — `docs/plans/history/plan-integrity-rev{N-1}-to-rev{N}.md` must exist and list what was caught.
12. **Invariant-tag coverage (rev 1.0 / B-F5)** — sections declaring single-writer / single-reader / single-gate invariants use an explicit marker (e.g. `{invariant: scope_cache_single_writer}`). Check 12 greps every prose mention of the named resource and verifies no section contradicts the invariant. Prevents a future rev from writing "ConfigSynced handler directly writes ScopeCache" alongside §2.16.2's one-writer invariant without the skill catching it.
13. **Transaction-boundary compatibility (rev 1.0 / B-F5)** — any section that opens a SQL transaction on a given table must be cross-checked against every other section that opens a transaction on the same table. Catches contradictions like "`BEGIN IMMEDIATE` around supersede" + "`BEGIN IMMEDIATE TRANSACTION` wrapping boot" that could deadlock or be semantically incompatible.
14. **Contribution field-list parity** — for every contribution schema introduced or revised in the plan, grep every prose field-list / state-table entry / lifecycle note and verify they describe the same fields. Prevents semantic drift like `onboarding_state` listing two fields in §2.18 while later sections rely on `migration_acks`, `chain_engine_enable_ack`, or `re_onboarding_required`.

Plan-integrity skill at `~/.claude/skills/plan-integrity/SKILL.md` is updated to include checks 9-14. Check 9's implementation is fixed (count the enumeration, not the prose).

When the agent-wire-node repo gets CI (separate initiative), a subset of these checks can be ported to a shell/python script at `docs/tools/plan-integrity.sh` for redundant mechanical enforcement. Until then: Claude + skill-discipline is the enforcement. Rev 0.7's CI-script framing is explicitly walked back.

### 2.14 Growth and failure modes (Root 15 — rev 0.7 new)

Cycle 2 surfaced three cases rev 0.6 left unspecified. All are now named here.

**2.14.1 Cascade exhaustion** (B-F3). When `on_partial_failure: cascade` walks off the end of `effective_call_order` without any provider succeeding, walker emits `dispatch_exhausted` chronicle event with `{tried: [provider_type, NotReadyReason|DispatchError], decision_id, duration_ms}`, then returns `StepFailure::DispatchExhausted` to the chain executor. **One-pass guarantee:** cascade walks each provider at most once per Decision; saturation-retry inside a provider counts as `retry_same`, not `cascade`. No infinite loop possible.

Tester first-build path with `effective_call_order: [market]`: if market's first dispatch returns a retryable failure AND market's own patience budget is exhausted, cascade has no next provider, `dispatch_exhausted` fires, step fails loudly. No silent hang.

**2.14.2 Multi-node-per-operator SelfDealing** (A-F5). The SelfDealing readiness check is **node-local**: filters offers where `offer.node_id == self.node_id`. Operators running multiple nodes under a single identity (e.g. laptop + BEHEM, both publishing to market) can have Node A buy Node B's offer — a round-trip via Wire fees for nothing. v3 ships with this limitation explicitly named; v4 introduces operator-scoped sibling filtering. v3 workaround: operators who run multi-node setups configure `walker_provider_market.overrides.active = false` on nodes they intend as serve-only or ask-only. See §9.

**2.14.3 Schema evolution** (B-F10). Rev 0.6 overclaimed "adding a parameter requires no schema change." Corrected semantics:

- **Redeclaring an existing parameter at a new scope** (operator adds `patience_secs` override to walker_slot_policy when it was previously only in walker_provider_*) — no code change. Pure contribution supersession.
- **Adding a genuinely-new parameter key** (e.g. `temperature_cap`) — requires (a) annotation-first supersession declaring the shape, (b) Rust SYSTEM_DEFAULT entry, (c) parameter catalog row, (d) typed accessor. Annotation ships before Rust binary so old nodes booting with the new annotation don't fail validation on unknown keys (the old validator grandfathers unrecognized keys as "declared but not consumed"; new binary recognizes them).
- **Retroactive `sensitive: true`** — when an annotation supersession marks a previously-non-sensitive parameter as sensitive, existing contributions are grandfathered (not rejected) but Settings flags them with a "re-confirm this sensitive field" banner. Operators can re-supersede to explicitly acknowledge.

Update §2.3's "adding a parameter = no schema change" wording accordingly — it's only true for scope redeclaration.

### 2.15 Sensitive-parameter authorization

Any parameter with `sensitive: true` in its schema_annotation triggers operator-confirmation dialog in Settings before the supersession is written. Sensitive fields:

- ~~`openrouter_credential_ref`~~ — removed in rev 0.6; credential rotation now happens in `pyramid_providers` provider-registry UI which has its own confirmation surface. Walker-side sensitivity is on the provider-registry, not in walker_*.
- `max_budget_credits` (could drain wallet if zeroed-out-to-None or lifted to astronomical)
- `order` (call_order or slot_policy — could silently bypass expected providers)
- `active` (disabling a provider without operator awareness, OR enabling market without consent on first build — see tester-onboarding Page 4 flip-to-true consent record per Root 17)
- `on_partial_failure` — **directional sensitivity (Root 16 / A-F3):** confirmation fires only on transitions TO `cascade` (operator relaxing privacy). `fail_loud → cascade` on a privacy-sensitive slot is the dangerous direction; `cascade → fail_loud` is strictly tightening privacy and does not require confirmation.

Consequences:

- **Generation skills output DRAFT supersessions** into a preview lane in Tools > Create. Operator reviews, explicitly confirms sensitive changes, then commits. No auto-apply.
- **Skill prompts do NOT bake numeric literals.** SYSTEM_DEFAULTS values are injected at skill-use time via prompt template interpolation (`{{patience_secs_default}}`, `{{retry_http_count_default}}`). Updating a SYSTEM_DEFAULT ripples into all skill outputs without re-authoring prompts — eliminates the Pillar 37 violation.
- **Audit trail:** every sensitive supersession carries operator session ID and confirmation timestamp in the contribution envelope.

**Draft-time parent reconciliation (rev 1.0 / B-F8):** DRAFT supersessions in the Tools > Create preview lane do NOT bind to a `supersedes_id` until commit time. If a ConfigSynced pull applies a Wire-side supersession on the SAME `schema_type` during the draft's lifetime, the draft is flagged `parent_changed: true` and the UI shows "Base changed while drafting — [Review merge] [Discard draft]." On commit of an unreconciled draft, the envelope writer detects `supersedes_id` mismatch with the current active row and emits `config_supersession_conflict`; operator re-reviews. This prevents the stale-draft-on-committed-parent silent-reparent hazard.

### 2.16 Concurrency and lifecycle invariants (Root 20 — rev 0.8)

Cycle 2 Stage 2 surfaced a cluster of concurrency and lifecycle edge cases. This section names them with concrete invariants; Phase 0a builds the infrastructure.

**2.16.1 Single-active-contribution invariant** `{invariant: config_contrib_active_unique}` `{txn: pyramid_config_contributions}` **(B-I5).** Today the `pyramid_config_contributions` table has no unique constraint enforcing one active contribution per logical `(scope, schema_type)`. Concurrent supersession (Settings save + ConfigSynced pull applying a Wire-side supersession on the same schema) can produce two rows with `status='active'` — resolver picks whichever `id DESC` returns, supersession chain is broken.

**Fix (Phase 0a):** normalize the contribution scope key so GLOBAL schemas do not rely on `slug IS NULL` uniqueness semantics. SQLite unique indexes treat `NULL` values as distinct, so `UNIQUE(slug, schema_type)` would still allow multiple active global rows. Canonical fix: introduce a normalized scope expression `COALESCE(slug, '__global__')` (or an equivalent explicit `scope_key` column) and enforce `CREATE UNIQUE INDEX uq_config_contrib_active ON pyramid_config_contributions(COALESCE(slug, '__global__'), schema_type) WHERE status='active'`. Read paths (`load_active_config_contribution`, scope-cache rebuild, migration-marker lookup) and write paths (`create_*`, `supersede_*`, bundled manifest load) must use the SAME normalization rule. Wrap `supersede_config_contribution` in `BEGIN IMMEDIATE TRANSACTION` to serialize on write intent. Second concurrent supersession fails with SQLITE_CONSTRAINT; caller retries or surfaces conflict to operator.

**Transaction-mode parameter (rev 1.0.1 — cycle 4 A-F3 fix):** `supersede_config_contribution` accepts `mode: TransactionMode::{OwnTransaction, JoinAmbient}`. `OwnTransaction` (default for runtime writes) opens `BEGIN IMMEDIATE TRANSACTION`. `JoinAmbient` — used by callers ALREADY inside a transaction (§5.3 migration step 6) — does the INSERT + UPDATE with no BEGIN, relying on the caller's outer transaction for atomicity. Nested `BEGIN IMMEDIATE` inside an ambient transaction would error with `SQLite: cannot start a transaction within a transaction` and crash first-boot migration. `{txn: pyramid_config_contributions, mode: OwnTransaction}` is the invariant tag Check 13 watches for runtime-path supersessions; migration path is explicitly `JoinAmbient`.

**2.16.2 ArcSwap listener supervision** `{invariant: scope_cache_single_writer}` **(A-M4).** `ArcSwap<ScopeCache>` is the rebuild primitive but rev 0.7 didn't name the supervisor. If a ConfigSynced handler panics (e.g. shape validation surprise in a superseded contribution), ArcSwap never gets the rebuild and all subsequent Decisions use the stale cache.

**Fix (Phase 0a):** main.rs spawns a single named task `scope_cache_reloader` that holds the ArcSwap writer. Task supervision: on panic, restart and emit `scope_cache_listener_restarted` chronicle event. Rebuild debounced 250ms (coalesces rapid operator edits into one rebuild). Dead-listener detection: integrity-pass check 2 (chronicle events) additionally verifies `scope_cache_listener_restarted` has an emission site.

**2.16.3 Placeholder interpolation engine cost model (B-I6).** `{{openrouter_live_slugs}}` fires a live OR API call. Without cost model, opening Tools > Create and rendering 7 skill cards triggers 7 concurrent ~80KB fetches.

**Fix (Phase 0a placeholder engine v2):** per-placeholder TTL (60s OR, 30s Ollama, 60s market-surface); single-flight deduplication (concurrent resolution of same placeholder blocks on one in-flight fetch); offline-safe stale fallback (use last successful value, return with `stale: true` flag so UI can render offline badge); circuit breaker (back off 5min after 3 consecutive failures). Named as Phase 0a acceptance criteria.

**2.16.4 In-flight builds at v3 upgrade (B-I7).** Mid-build binary swap: operator has build with `status='running'` when they quit to upgrade. Migration drops `pyramid_tier_routing`, removes `config.primary_model`. On v3 boot, chain executor resumes via replay — but it reads retired routing state. Silent re-route mid-build.

**Fix (§5.3 pre-migration check):** migration refuses to run if any row in `pyramid_builds` has `status IN ('running','paused_for_resume')`. Boot-time modal surfaces: "Upgrade to v3 requires in-progress builds to finish or be marked failed. [Resume] [Mark failed] [Rollback to v2]". No silent data-corruption path.

**BreakerState rehydrate at boot (rev 1.0.1 / A-F7):** BreakerState HashMap is ephemeral (§2.18 — in-memory only, not persisted across process restart). On cold boot, DADBEAR resumes with an empty map; first post-boot dispatch naturally probes. §5.5.1's bucket-rotation carry-forward (`last_failure_at` / `consecutive_failure_count`) applies only WITHIN a process lifetime. This is the correct default — cold boot is a natural probe opportunity — but stating it explicitly prevents implementer confusion when §5.5.1's carry-forward language is read in isolation.

**2.16.5 Offline-aware readiness (A-C6).** Openrouter's `can_dispatch_now` today checks only local credential — returns Ready on a network-partitioned laptop, then dispatch fails with 5xx/timeout, burns `retry_http_count × backoff`, cascades to fleet (NotReady), emits `dispatch_exhausted`. DADBEAR keeps scheduling the loop → battery/resource drain.

**Fix (Phase 0a ProviderReadiness):** each network provider tracks a `last_success_at` and a `consecutive_failure_count`. `can_dispatch_now` returns `NotReady { NetworkUnreachable }` when `consecutive_failure_count >= 3` AND `last_success_at` > 5 minutes. Resets on any success. DADBEAR additionally applies a back-off bucket: after N `dispatch_exhausted` fires within T seconds, stale_engine suppresses its next scheduled dispatch until T elapses.

**2.16.6 Per-build breaker semantics for DADBEAR (A-M1).** `breaker_reset: per_build` permanently trips for DADBEAR's long-lived maintenance build. One transient market flake → provider bypassed until app restart.

**Fix (Phase 5):** DADBEAR's maintenance build_id uses time-bucketed sub-build-ids (`{parent_build_id}:bucket_{epoch_hour}`). The effective breaker key becomes `(parent_build_id:bucket, slot, provider_type)`; per-hour bucket rotation gives `per_build` meaningful granularity for long-lived builds. Operators running short builds see no change; DADBEAR gets a natural reset cadence.

**2.16.7 Node identity rotation + SelfDealing (brain-dump Q10).** `self.node_id` changes on re-onboarding (new pin → new node_id). Old self-published offers still sit on market under the previous node_id. Post-rotation walker sees `offer.node_id != current_self_node_id` → passes SelfDealing → buys own historic offers.

**Fix (rev 0.9):** walker's SelfDealing check consults `node_identity_history` — **stored as a contribution** (per Root 23 "everything is a contribution"). Schema: `node_identity_history` with `overrides.history: Vec<{node_id, rotated_at, reason}>` and `local_only: true` + `sensitive: true` schema_annotation flags. Survives reinstall IF Wire sync restores it. Operators SHOULD retract their own market offers before re-onboarding; onboarding wizard surfaces this as a pre-rotation checklist. On rotation, the wizard appends to the history contribution in the same commit that sets the new node_id.

### 2.17 Boot and init order (Root 24 — rev 1.0 sequential-startup rewrite)

Rev 0.9's §2.17 proposed a transactional gate on `app_mode` in `pyramid_config`. Cycle 3 Stage 2 caught that (a) `pyramid_config` is a JSON file on disk, not a SQL table, so `BEGIN IMMEDIATE` doesn't serialize it; (b) the gate was fighting symptoms — there's no concurrent writer to serialize against if the code that starts builds hasn't been spawned yet.

**The canonical boot sequence (rev 1.0 — sequential startup, no transactional gate needed):**

```
1. open DB
2. load bundled_contributions.json manifest through envelope writer in
   BundledBootSkipOnFail mode (§2.11)
3. build initial ScopeCache from active contributions → ArcSwap::store
4. migration phase (only if migration_marker contribution says v2 is active):
   a. SQL transaction BEGIN
   b. refuse-to-migrate if any `pyramid_builds` row has status IN ('running','paused_for_resume')
      (no race possible — step 8 hasn't started listeners yet)
   c. run v3 migration DDL (§5.3 CREATE-COPY-DROP-RENAME)
   d. supersede migration_marker contribution: v2 → v3-db-migrated-config-pending
      (atomic with DDL; final `v3` lands only after the config-file rewrite)
   e. COMMIT
5. rebuild ScopeCache from POST-migration active contributions → ArcSwap::store
   (mandatory even if step 3 ran; first Decision read must never see pre-migration cache)
6. spawn scope_cache_reloader task with quarantine supervisor (§2.17.2)
7. wire ConfigSynced listener — ready to handle supersession events
8. stale_engine rehydrate — reads via synthetic Decision builder (§2.12)
9. AppState::transition_to(AppMode::Ready) — in-memory state change
10. routes_operator.rs + HTTP listeners come up — accepts traffic
11. chain executor + DADBEAR scheduler spawned — can now start builds
```

**Why sequential replaces transactional:** steps 1-8 run on a single thread at boot before any code that can start a new build has been spawned. There is no concurrent writer. `AppMode::Ready` is an in-memory gate checked by build-starter code paths, and those code paths aren't running yet. The F-C3-2 CRITICAL (builds entering `running` during migration) is solved by not starting the listeners until after migration AND by requiring every build-starting entry point (HTTP routes, Tauri IPC, folder-ingestion background spawns, question-build spawns, DADBEAR manual triggers, stale-engine startup hooks) to funnel through one shared `guard_app_ready_then_start_*` helper beneath the public trigger surface. This is simpler than a transactional gate and doesn't depend on pyramid_config being a SQL table (which it isn't).

**2.17.1 `AppMode` is an in-memory state machine** `{invariant: app_mode_single_writer}`. `enum AppMode { Booting, Migrating, Ready, Quarantined, ShuttingDown }` lives in `AppState` (the Tauri-managed struct already threaded through IPC handlers) as `tokio::sync::RwLock<AppMode>`. Transitions are single-writer from the boot coordinator in main.rs; readers check on entry. Not persisted — if the app crashes, next boot starts fresh at `Booting`. Build-starter code paths check `app_state.app_mode.read().await == AppMode::Ready` on entry, fail fast with `AppNotReady` error otherwise. This is NOT a best-effort convention: every current starter (HTTP build routes, Tauri `pyramid_build`, question-build spawn, folder-ingestion initial-build spawn, DADBEAR manual trigger, stale-engine startup reconciliation, and any future `spawn_*build*` helper) must route through the same guard helper so boot ordering and runtime gating cannot drift apart. The boot-state-machine is ephemeral by design; persisting it would create recovery ambiguity (what does "Migrating" mean on a fresh-start after a crash mid-migration?).

**2.17.2 ArcSwap reloader supervisor — quarantine on persistent panic.** Restart-budget of 3 within 60s. On 4th panic within that window: supervisor holds LAST-KNOWN-GOOD `Arc<ScopeCache>` (resolver keeps serving reads from stale cache), marks the triggering `contribution_id` as `status='quarantined'` (new contribution-status value), emits `scope_cache_quarantined` chronicle event, transitions `AppMode` to `Quarantined`. Operator must retract or fix the contribution; next restart re-runs the boot sequence and quarantined rows are skipped at step 2.

**2.17.3 Boot aborts to known states.** If step 1 fails (DB corrupt): app refuses to boot, surfaces recovery modal. If step 4 fails (migration DDL or migration_marker supersession): transaction rollback, `_pre_v3_snapshot_*` tables preserved, recovery modal. If step 2 produces a quarantined bundled row: boot continues (SkipOnFail), quarantined row surfaced in chronicle. If steps 5-7 fail: `AppMode::Quarantined`, listeners don't come up, operator banner. No step can "partially succeed" without an explicit AppMode transition.

### 2.18 Internal state — contribution-native by default (Root 23 — rev 1.0 reframe)

Rev 0.9 invented new non-contribution storage (`pyramid_config` "field", sentinel rows) for state that should be contribution-backed. Cycle 3 Stage 2 caught that this was defaulting to hardcoding. Rev 1.0 converts runtime state to contributions wherever the semantic fits, and keeps the non-contribution cases strictly to runtime-ephemeral state that shouldn't be persisted.

| State | Rev 1.0 decision | Rationale |
|---|---|---|
| **Migration marker** | **Contribution** (`migration_marker` schema). Bundled default declares `v2`. v3 migration uses a two-step body progression: `v2` → `v3-db-migrated-config-pending` inside the SQL transaction, then `v3` after the config-file rewrite + post-rewrite cache rebuild. Future migrations supersede further (`v4`, etc.) using the same explicit staged model when crossing storage boundaries. | Natural supersession model; no new infrastructure; schema_version is just the body of the active contribution, and staged marker values make cross-store progress resumable instead of ambiguous. |
| **`onboarding_state`** | **Contribution** (`onboarding_state` schema). Holds `onboarding_complete_at`, `completed_pages`, `migration_acks`, `chain_engine_enable_ack`, `re_onboarding_required`. Written by the onboarding save path / Page 4 consent path as operator-authored supersession. | Envelope-validated; surfaces in Settings as authoritative source; survives Wire sync if operator_private; Check 6 writer-site verifiable. |
| **`node_identity_history`** | **Contribution** (`node_identity_history` schema). Holds `current: NodeId`, `history: Vec<{node_id, rotated_at, reason}>`. On re-onboarding, the rotation flow supersedes with current+previous appended. | SelfDealing check reads from the active contribution; historic node_ids survive reinstall via Wire's `operator_private` sync (see §5.4.3 — the op-private flag is distinct from `local_only`). |
| **`AppMode`** | **In-memory only** (`AppState::app_mode: tokio::sync::RwLock<AppMode>`). Not persisted. | Boot-state-machine is meaningful only within a single process lifetime; crash-restart starts fresh. Persisting it creates recovery ambiguity. |
| **BreakerState HashMap** | **In-memory only** (Phase 5). | Ephemeral per-build runtime state. |
| **ScopeCache** | **In-memory only** (ArcSwap). | Derived from active contributions, rebuilt on demand. |
| **MarketSurfaceCache** | **In-memory + disk cache** (Wire-derived). | Derived from Wire `/market-surface`, not operator config. |
| **`_pre_v3_snapshot_*` tables** | **SQL tables, non-contribution.** Auto-pruned 30d post-migration (§5.5.9). | Migration forensics; intentionally decoupled from contribution graph to preserve the snapshot against subsequent contribution edits. |

**Three new contribution schema_types** land in rev 1.0: `migration_marker`, `onboarding_state`, `node_identity_history`. Each ships with schema_definition + schema_annotation + generation_skill + default_seed per the four-part pattern. Phase 0b LOC absorbs these (~150 LOC for all three).

**Why this is different from rev 0.9:** rev 0.9 treated "this needs transactional coordination with the migration DDL" as a reason to invent new hardcoded SQL state. Rev 1.0 recognizes that migration_marker IS the schema-version tracker, naturally expressed as a superseding contribution, atomically committed alongside the DDL in the same SQL transaction on `pyramid_config_contributions`. No new sentinel field, no pyramid_config storage-type confusion, no transactional gate on a JSON file.

---

## 3. Parameter catalog

This table is the authoritative list of walker-behavioral parameters in v3. Anything not listed is not currently declarable; add a row + a SYSTEM_DEFAULT to declare a new one.

| Parameter | Type | Semantics | SYSTEM_DEFAULT |
|---|---|---|---|
| `active` | `bool` | Master switch for the provider type. `false` = readiness gate fails, provider is skipped in call_order. Field name follows the `structured_data.active` pattern used by shipped contributions (Q2). | `true` for openrouter/fleet; `false` for local AND market (opt-in — see Root 17 / A-F6: tester's onboarding Page 4 flip-to-true is the consent record for first market spend, not a silent bundled default) |
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
| `network_failure_backoff_threshold` | `u32` | Consecutive-failure count before readiness returns `NetworkUnreachable` (§2.16.5, §5.5.8). Applies to openrouter + market. | `3` |
| `network_failure_backoff_secs` | `u64` | Duration in `NetworkUnreachable` state before readiness retries (§2.16.5, §5.5.8). | `300` |
| `on_partial_failure` | tagged enum `{cascade, fail_loud, retry_same}` | Decision-level policy. What happens when a provider returns a retryable failure: `cascade` (try next provider in effective_call_order — default, matches current behavior); `fail_loud` (emit `dispatch_failed_policy_blocked` and stop — privacy-preserving posture for slots where cross-provider prompt leakage matters); `retry_same` (stay on same provider, respect breaker and patience budget). **Scope 2 ONLY (slot-level).** Not per-provider, because at Decision-level there's exactly one policy per step; allowing scope 4 declarations across four providers creates ambiguity about which wins (Root 16 / A-F12). Sensitive **directionally** (Root 16 / A-F3): `new == cascade AND old != cascade` triggers confirmation; other transitions don't. | `cascade` |
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

### 5.3 One-time migration on upgrade — TOTAL at the routing layer, explicit across DB + config JSON

On first boot after v3 ships, migration is atomic for the SQLite routing state and explicit for the `pyramid_config.json` rewrite. The database changes (new walker contributions, migration marker supersession, de-dup, unique index, retired routing tables) commit in one SQL transaction. The config-file rewrite is a second durable step using temp-file + atomic rename. Either both complete and walker v3 proceeds, or boot remains in a recoverable blocked state with a precise marker showing which phase failed. No silent half-migrated routing state is allowed.

**Pre-migration snapshot:** before any writes, snapshot every source row into `_pre_v3_snapshot_{pyramid_tier_routing, dispatch_policy, config}` tables. Rollback path: if the SQL transaction fails, tables untouched; if SQL succeeds but the config-file rewrite or post-rewrite cache rebuild fails, app refuses to proceed and suggests `--rollback-v3-migration` CLI flag to restore from snapshot. The snapshot is authoritative for both the DB rows and the JSON-file payload.

**Migration steps (Phase A = one SQL transaction, Phase B = atomic file rewrite):**

**Phase A — SQL transaction**

1. **GROUP BY `provider_id` from `pyramid_tier_routing`.** Per Stage 1 audit: rows today include `provider_id` values of `"openrouter"`, `"ollama"`, `"ollama-local"`, `"fleet"`, `"market"` (verified at `src-tauri/src/pyramid/db.rs:1555-1568` and `fleet_mps.rs:421-458`). Map:
   - `"openrouter"` → emit `walker_provider_openrouter` contribution
   - `"ollama"` | `"ollama-local"` → emit `walker_provider_local` contribution
   - `"fleet"` → emit `walker_provider_fleet` contribution
   - `"market"` → emit `walker_provider_market` contribution
   - Unknown `provider_id` → hard migration failure by default. Row preserved in snapshot; recovery modal offers "[Proceed — these tiers will need reconfiguring]" only through the explicit unknown-provider acknowledgement flow from §5.5.4. There is no silent continue-with-warning path.

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

6. **Supersede `migration_marker` contribution from `v2` to `v3-db-migrated-config-pending`** (rev 1.0.2 tightening). This is a standard contribution supersession in the same SQL transaction as the DDL — no separate sentinel field. The active `migration_marker` contribution IS the schema-version tracker. This intermediate value means: DB migration committed; config-file rewrite still pending.
7. **Pre-index de-duplication (rev 1.0 / B-F3).** Before CREATE UNIQUE INDEX `uq_config_contrib_active`, run: for every `(slug, schema_type)` with >1 `status='active'` row in `pyramid_config_contributions`, deactivate all but the `id DESC` row; record as snapshot under `_pre_v3_dedup_snapshot`. Existing dev DBs with pre-enforcement duplicates will otherwise fail the CREATE INDEX. Then create index inside the same transaction.

**Phase B — config-file rewrite**

8. Rewrite `pyramid_config.json` using temp-file + `rename(2)` atomic replacement: remove `primary_model`, `fallback_model_1`, `fallback_model_2`, and apply the `use_chain_engine` handling from §5.6.3.
9. Rebuild ScopeCache from the POST-migration contribution graph.
10. Supersede `migration_marker` contribution from `v3-db-migrated-config-pending` to `v3`.

**Post-migration:** legacy tables/struct fields dropped in the same migration. No read-compatibility period — walker v3 is the only reader. `config.primary_model` removed from `AppConfig`. Pre-existing callers are migrated in the same Phase 1 commit; `cargo check` proves completeness. If boot crashes after Phase A but before Phase B completes, the marker body `v3-db-migrated-config-pending` forces the next boot into the recovery/rewrite path rather than pretending migration fully completed.

**On boot:** boot sequence (§2.17) reads active `migration_marker` contribution. If body is `v2`, step 4 runs migration. If body is `v3-db-migrated-config-pending`, skip the SQL DDL and resume at Phase B's config-file rewrite + cache rebuild. If `v3` (or higher), skip migration and proceed. Missing marker = treat as v2 (first-ever boot of v3 binary on an unmigrated DB). No separate sentinel needed — the contribution IS the marker.

**SQLite mechanics for column removal (B-F9 / rev 0.7 — spelling out what rev 0.6 over-claimed):** SQLite versions < 3.35 cannot `ALTER TABLE DROP COLUMN`; `db.rs:2446` notes "SQLite DROP COLUMN is expensive." The migration removes these columns via CREATE-COPY-DROP-RENAME:

```sql
BEGIN TRANSACTION;

-- Example: removing legacy pyramid_tier_routing after consolidation
CREATE TABLE pyramid_tier_routing__new (
  -- post-migration shape: none needed; the table is fully retired
  -- (all data now lives in walker_provider_* contributions)
  placeholder INTEGER  -- or drop the table entirely if no FK refs remain
);

INSERT INTO pyramid_tier_routing__new SELECT ...;  -- no rows needed if retiring
DROP TABLE pyramid_tier_routing;
ALTER TABLE pyramid_tier_routing__new RENAME TO pyramid_tier_routing;
-- Or simply: DROP TABLE pyramid_tier_routing; if no FK references remain.

-- AppConfig struct fields (primary_model, fallback_model_1, fallback_model_2)
-- are NOT database columns — they live in config JSON serialized per-operator.
-- Migration rewrites that JSON: read the old shape, write the new shape without
-- those fields, persist. Rust struct definition loses the fields in the same
-- Phase 1 commit; `cargo check` enforces no stale readers.

COMMIT;
```

Disable foreign-key enforcement during the rebuild if `pyramid_tier_routing` has FK references: `PRAGMA defer_foreign_keys = ON;` at transaction start, re-enable after commit. Indexes on the old table must be recreated on the new one; `schema_version` is bumped in the same transaction.

### 5.4 Cross-boundary wire-format contracts (Root 19 — rev 0.8)

Cycle 2 Stage 2 caught that walker v3 silently changes the semantic weight of several cross-boundary contracts (fleet announce, fleet dispatch, chronicle serialization, retraction). Silently = without version bumps or explicit compatibility handling. This section names every cross-boundary contract walker v3 touches and the compatibility strategy.

**5.4.1 Fleet dispatch wire format — `rule_name` vs `model_id` (B-I2, CRITICAL).** Today `FleetDispatchRequest` (at `src-tauri/src/fleet.rs:264`) sends a `rule_name` string; the peer resolves rule → local model via its own routing rules. Walker v3 resolves a model_list per provider but never translates into `rule_name`. Without a translation layer, walker v3's fleet arm cannot dispatch.

**Fix (Phase 4):** extend `FleetDispatchRequest` with `model_id: Option<String>` (additive, backward-compatible). Peer prefers `model_id` when present; falls back to `rule_name` otherwise. Walker v3 fleet dispatcher populates `model_id` from `decision.per_provider[fleet].model_list[0]` and sets `rule_name` from the chain step's `model_tier` for old-peer compatibility. Tests: mixed-cluster dispatch (v3 requester, v2.1.1 peer with only rule_name awareness).

**5.4.2 Fleet announce protocol versioning (B-I1).** `FleetAnnouncement.models_loaded` was observability-only in v2.1.1 (`fleet.rs:236` comment: "kept for observability"). Walker v3 promotes it to gating — readiness reads it as authoritative. Mixed-version clusters: v3 node skips a v2.1.1 peer that actually has the model but populated `models_loaded` differently.

**Fix (Phase 4):** add `announce_protocol_version: u8` field to `FleetAnnouncement` with **`#[serde(default = "announce_protocol_version_default")]` returning `0`** (pre-versioning value, per rev 1.0 / B-F7). Version mapping: `0` = pre-v3 peer (field was absent in the deserialized announce), `1` = v2.1.1 with explicit versioning, `2` = v3. Walker's fleet readiness on a v<2 announcer returns `PeerIsV1Announcer` NotReady per §5.5.2 strict mode — refuses to dispatch. Additionally emits `fleet_peer_version_skew` chronicle event (in §5.4.6 — Infrastructure category) on first-seen v<2 peer per boot, so operator UI surfaces "N of M peers are pre-v3; upgrade to enable fleet dispatch." Sunset horizon: v3 ships with v<2 strict-refuse; no deprecation window — fleet just doesn't work for mixed clusters until peers upgrade. Tests: mixed-version roster with explicit 0/1/2 peers; version-skew event firing; NoPeerHasModel vs PeerIsV1Announcer distinction surfaced correctly.

**5.4.3 Chronicle scope_snapshot leak (B-I3).** `DispatchDecision.scope_snapshot` serialized into `decision_built` chronicle payload. ScopeCache carries operator-local config — LAN URLs (`ollama_base_url: "http://behem.lan:11434"`), budget caps, closed-beta slugs. If chronicle syncs cross-node (Phase 6 "last N Decisions aggregation" is one path; Wire-side observability is another latent possibility), this leaks.

**Fix (Phase 0a + §2.11):** add `local_only: bool` to schema_annotation fields. Default `true` for `ollama_base_url`, default `false` for tier names and public params. `ScopeSnapshot::redacted_for_chronicle() -> SerializableView` strips any parameter with `sensitive: true` OR `local_only: true`. The `decision_built` event serializes the redacted view. Phase 0a envelope writer validates schema_annotations declare `local_only` for every parameter explicitly (no default implicit).

**Type-level guard (Root 27 / F-C3-6, rev 0.9):** Convention-based redaction will regress. The first well-meaning dev who adds a chronicle event by calling `serde_json::to_value(&decision.scope_snapshot)` leaks LAN URLs. Rev 0.9 requires a type-level enforcement:

- Remove `#[derive(Serialize)]` from the raw `ScopeSnapshot` type.
- `ScopeSnapshot` exposes two views: `redacted_for_chronicle() -> RedactedSnapshot` (distinct type that DOES `impl Serialize`) and `for_dispatch_internal() -> &InternalView` (NOT Serialize-able — dispatchers consume by field access, not serialization).
- `cargo check` fails if anyone writes `serde_json::to_value(&scope_snapshot)`. Redaction becomes a type property, not a convention.
- Same pattern applies to `DispatchDecision`: Decision is not Serialize; chronicle serializes a `DecisionChronicleView` built via `decision.for_chronicle()` which reaches through the Decision's scope_snapshot's `redacted_for_chronicle()`.

**5.4.4 Retraction semantics (B-I4).** `wire-contribute retract` is first-class in the Wire skill but the node side has zero handler — grep `retract|is_retracted|retraction` in `src-tauri/src/pyramid/` returns nothing. An operator retracting their `walker_provider_market` on Wire sees no effect locally. Silent state divergence.

**Fix (Phase 0a):** implement `retract_config_contribution(prior_id, triggering_note) -> Result<()>` that (a) marks the row `status='retracted'`, (b) reactivates the `supersedes_id` ancestor if it's bundled-or-operator-authored, (c) emits `config_retracted` chronicle event (new — add to const registry), (d) triggers ArcSwap rebuild. Refuses to retract a bundled-only row (bundled is the floor — nothing to revert to). Wire-originating retractions (pulled via sync) hit the same code path.

**5.4.5 Unknown provider_id migration outcome (B-I8).** Rev 0.7 §5.3 step 1 treated unknown `provider_id` rows (non-openrouter/ollama/fleet/market) as "continue with warning." Operators who customized their tier routing with tier names backed by unknown providers silently drop those tiers — then their chain YAMLs reference tiers that no walker_provider_* declares → runtime `tier_unresolved`.

**Fix (§5.3 step 1 revision):** unknown `provider_id` → **hard migration failure** by default. Operator opts in via `--allow-unknown-providers` CLI flag with an explicit "I understand these tiers will stop working" confirmation. Plus a Phase 0b boot check: enumerate all tier names referenced in chain YAMLs; cross-reference against post-migration `model_list` keys union; mismatches surface in Settings as a "Chain tiers not backed by any provider" banner, not just chronicle.

**5.4.6 Two chronicle event categories.** The chronicle is the cross-subsystem boundary. Walker v3 adds **22 events** (rev 1.0.1 authoritative count — cycle 4 auditors caught `dispatch_failed_policy_blocked` missing from rev 1.0's registry despite being referenced in §3 + Phase 1 tests). They divide into two categories:
- **Local-only events** (informational, node-internal, never cross-node-synced):
  - Decision lifecycle (4): `decision_built`, `decision_previewed`, `decision_build_failed`, `dispatch_failed_policy_blocked`
  - Readiness & breaker (4): `provider_skipped_readiness`, `breaker_tripped`, `breaker_skipped`, `dispatch_exhausted`
  - Config lifecycle (6): `config_superseded`, `config_retracted`, `config_retracted_to_bundled`, `retraction_walked_deep`, `sensitive_supersession_confirmed`, `config_supersession_conflict`
  - Plan integrity / drift (3): `tier_unresolved`, `preview_vs_apply_drift`, `requester_provider_param_drift`
  - Infrastructure (5): `scope_cache_listener_restarted`, `scope_cache_quarantined`, `bundled_contribution_validation_failed`, `v3_migration_snapshots_pruned`, `fleet_peer_version_skew`

Total 4+4+6+3+5 = **22**. §5.4.6 is the single source of truth; Phase 0a-1 body defers here, Phase 0a-1 exit criteria defers here. Plan-integrity Check 9 counts this list and verifies every "adds N events" / "all N new events" in the plan matches 22.
- **Wire-visible events** (none in walker v3). Walker v3 does NOT emit any event that crosses to Wire. If a future phase introduces cross-node observability, the redaction from §5.4.3 applies.

This boundary is load-bearing: chronicle can be arbitrarily verbose locally; nothing leaks unless an explicit cross-node sync is built, at which point §5.4.3's redaction is the required filter.

### 5.5 Cross-subsystem cascade discipline (Root 26 — rev 0.9 new)

Cycle 3 Stage 1 surfaced several behaviors where rev 0.8 fixes cascaded into untracked second-order effects. This section names each and the resolution:

**5.5.1 DADBEAR bucket boundary breaker state carry-forward (F-C3-3).** Rev 0.8's §2.16.6 time-bucketed build_id naively resets breaker state at every epoch-hour boundary — meaning a real market outage at 23:59 is invisible to the 00:00 dispatch regardless of `breaker_reset` variant semantics. **Rev 0.9 fix:** bucket rotation ONLY applies to `breaker_reset: per_build`; for `probe_based` and `time_secs:N`, breaker state carries forward from the prior bucket's tripped state. Concretely: on bucket transition, copy `last_failure_at` + `consecutive_failure_count` from the outgoing bucket entry if its `tripped: true`. Map eviction (§2.16.6) drops entries with no activity for 48h, bounding growth.

**5.5.2 Fleet dispatch to v1-announcer peer — strict (F-C3-5).** Rev 0.8's §5.4.1 said v3 requester sends BOTH `rule_name` + `model_id`; v2.1.1 peer ignores `model_id` and resolves `rule_name=model_tier` through its own routing rules, which may map to a different model than v3 intended. **Rev 0.9 fix:** before dispatching, walker consults `FleetAnnouncement.announce_protocol_version` from §5.4.2. If peer is v1-announcer, walker returns `NotReady { PeerIsV1Announcer }` for that peer on that dispatch (strict mode — refuses mixed-version dispatch). `NoPeerHasModel` remains reserved for same-protocol peers whose announce is current but whose declared `models_loaded` set does not contain any requested model. v3↔v3 peers exchange `FleetDispatchRequest` with `#[serde(deny_unknown_fields)]` to lock the contract.

**5.5.3 Retract depth ceiling + cycle detection (F-C3-7).** Rev 0.8's §5.4.4 said retraction "reactivates the `supersedes_id` ancestor" without handling the case where the ancestor is also retracted. **Rev 0.9 fix:** walker walks `supersedes_id` chain backwards, skipping retracted ancestors, depth ceiling 16, visited-set cycle detection. Emits `retraction_walked_deep` chronicle event when >1 hop (add to §5.4.6 registry). On cycle detection or depth exhaustion: retraction fails loud with `RetractionChainCorrupt { contribution_id }`. If all ancestors retracted and depth not exhausted, reactivates the bundled floor with `config_retracted_to_bundled` event.

**5.5.4 Unknown provider_id during migration — in-UI modal, not CLI flag (F-C3-13).** Rev 0.8's §5.4.5 required `--allow-unknown-providers` CLI flag. Tauri desktop apps don't have a natural CLI path. **Rev 0.9 fix:** §2.17's boot-time in-flight-builds modal extends to also surface the unknown-provider case. If migration detects unknown `provider_id` rows, modal shows: "[N] tier routings use providers Walker v3 doesn't recognize. Details: [expand]. [Proceed — these tiers will need reconfiguring] [Rollback to v2]." Operator's click-through is recorded on `onboarding_state.migration_acks` with the provider list; there is no standalone `migration_unknown_providers_ack` schema anymore. CLI flag remains as headless-mode escape for advanced users.

**5.5.5 Path-aware shape validator (F-C3-15).** Rev 0.8's §2.11 said schema_annotation carries `scope_behavior: {scopes_3_4, scopes_1_2}` for `model_list`, but didn't spec HOW the validator knows which sub-path of a `walker_slot_policy` YAML is scope 1 vs scope 2. **Rev 0.9 fix:** schema_annotation for `walker_slot_policy` declares path-rules explicitly:
- `slots[*].overrides.*` → scope 2 (flat list for `model_list`)
- `slots[*].per_provider.*.*` → scope 1 (flat list for `model_list`) — note: per §4.3, `per_provider.market: {breaker_reset: "probe_based"}` is flat, NOT nested under `.overrides`
- `slots[*].order` → scope-2 ordering (validated as `Vec<ProviderType>`)

The validator's walk is schema-type-driven: `walker_slot_policy` YAML paths match the declared path-rules; `walker_provider_*` YAML paths default to scope-4. Plan-integrity skill's Check 3 strengthened to grep the schema_annotation for path-rules when a schema has multi-scope semantics.

**5.5.6 Dispatch Decisions aggregation surface (F-C3-8).** Rev 0.8's Phase 6 "Dispatch Decisions tab" didn't specify where aggregation runs. **Rev 0.9 fix:** add `GET /api/operator/builds/{build_id}/dispatch-decisions?limit=N&before=T` to `routes_operator.rs`, returning redacted Decision summaries with pagination. Server-side aggregation; Phase 6 client UI consumes. LOC: ~100 on top of Phase 6 UI.

**5.5.7 Tester stuck-state banner (B-F7).** When `decision_build_failed` fires AND no prior successful dispatch exists for this install, Settings and Builds tab surface a banner: "Market has no offers for tier X yet. Your build can't run until offers appear. [Wait] [Open help] [Check market status]." Wired to balance-cache + MarketSurfaceCache state.

**5.5.8 Offline back-off as resolver-chain parameters (F-C3-9 / Pillar 37).** Rev 0.8's §2.16.5 used unbounded literals (`N consecutive failures`, `T seconds`). Pillar 37 violation. **Rev 0.9 fix:** add to §3 parameter catalog:
- `network_failure_backoff_threshold: u32` — SYSTEM_DEFAULT `3` — number of consecutive failures before readiness returns `NetworkUnreachable`.
- `network_failure_backoff_secs: u64` — SYSTEM_DEFAULT `300` (5 min) — how long to remain `NetworkUnreachable` before retrying.

Both operator-overrideable via scope chain. Back-off logic lives in `ProviderReadiness::can_dispatch_now` (not in stale_engine — single home). stale_engine simply sees `NotReady` from the gate and skips, same as other NotReady reasons.

**5.5.9 `_pre_v3_snapshot_*` retention (F-C3-14).** 30-day auto-prune. On any boot where the active `migration_marker` contribution is `v3+` AND its creation timestamp is older than 30 days, drop the `_pre_v3_snapshot_*` + `_pre_v3_dedup_snapshot` tables and emit `v3_migration_snapshots_pruned`. Operator can also manually clear via Settings "Clear migration snapshots" button.

### 5.6 Lifecycle semantics for walker state (Root 31 — rev 1.0 new)

Rev 0.9 added state to the contribution graph without specifying how it behaves across rollback, backup/restore, and default-flag changes. This section names each.

**5.6.1 Rollback from v3 to v2.** Triggered either via the boot-time recovery modal's "Rollback" option (§2.17.3) or the `--rollback-v3-migration` CLI flag. Steps:

1. Restore `pyramid_tier_routing`, `dispatch_policy`, `config` (JSON) from `_pre_v3_snapshot_*` tables.
2. Purge rev-0.9-introduced contribution rows: `migration_marker` (all rows — no longer meaningful), `walker_provider_*`, `walker_call_order`, `walker_slot_policy`, `onboarding_state`, `node_identity_history`. Retract (not hard-delete) so they remain visible in chronicle for forensics; v2 code ignores unknown `schema_type` rows.
3. Snapshot of `pyramid_config_contributions` PRE-v3 migration is captured as part of step 1's `_pre_v3_snapshot_config` — walker v3 migration extends §5.3 step 1 to include a pre-migration row-dump of the contributions table.
4. The boot-time modal surfaces rollback as a first-class button (not CLI-flag, per §5.5.4).

**5.6.2 Backup/restore.** Each contribution schema declares its backup semantics in `schema_annotation`:

- `migration_marker`: backed up as-is. On restore, if the binary's embedded `schema_version > marker_body`, run migration (normal path — restored state is just another "needs migration" trigger). This prevents a restored v2-marker from short-circuiting a later-version migration.
- `onboarding_state`: backed up as-is. On restore, validate `onboarding_complete_at.node_identity` matches current `node_identity_history.current`; if mismatch, mark state `re_onboarding_required: true` so Settings surfaces the prompt.
- `node_identity_history`: backed up as-is with `operator_private: true` flag — encrypted to operator keypair for Wire sync; on OS-level backup, stored unencrypted per disk-backup security model.
- `_pre_v3_snapshot_*` tables: backed up as tables, not contributions. If restored inside the 30-day window, they remain available for rollback; if older, they're pruned automatically on first boot.

**5.6.3 `use_chain_engine` current reality + migration policy — PUNCHLIST P0-2 resolution.** The current codebase already defaults `PyramidConfig.use_chain_engine` to `true` (`src-tauri/src/pyramid/mod.rs`, `impl Default for PyramidConfig`). So walker v3 does NOT need a fresh-install default flip anymore, and the older "default false" migration premise is stale.

**Canonical handling going forward:**

- **Fresh installs / untouched defaults:** no migration work. They already boot with `use_chain_engine: true`.
- **Existing config explicitly set to `true`:** no-op, pass through.
- **Configs that currently deserialize to `false`:** this remains a real compatibility branch because it bypasses the Decision spine and keeps the legacy `build.rs` route alive. **Current code reality matters here:** `PyramidConfig.use_chain_engine` is declared `#[serde(default)] pub use_chain_engine: bool` in `src-tauri/src/pyramid/mod.rs`, so a pre-field legacy `pyramid_config.json` with the key absent also loads as `false` (`bool::default()`), which is operationally indistinguishable from an operator-explicit `false` in today's persisted shape. Therefore rev 1.0.1 can only safely gate on the loaded value: if config loads as `false`, boot surfaces the modal *"[Enable chain engine + continue with walker v3] [Rollback to v2]"* and records operator confirmation as `chain_engine_enable_ack` on `onboarding_state`. If a future rev wants to distinguish "explicit false" from "field absent on old install", it must first add a provenance bit (for example `user_set_use_chain_engine`) and migrate to it explicitly.

This keeps P0-1 + P0-2 tied to the same Phase 1 outcome without inventing migration/UI work for non-problem installs: legacy `dispatch_policy` routing path retires, chain engine remains the only supported dispatch path, and only installs whose config currently loads as `use_chain_engine: false` get an intervention.

**5.6.4 Retraction asymmetry in walker state.** §5.4.4's retract handler reactivates the `supersedes_id` ancestor. For rev 1.0's three new schemas (`migration_marker`, `onboarding_state`, `node_identity_history`): retraction reactivates the prior contribution normally. For `migration_marker` specifically, operator retraction is **refused** outside of rollback context — downgrading the schema-version marker via normal retraction would leave the DB in v3 DDL shape with a v2 marker. Rollback is the only valid downgrade path.

---

## 6. Phased implementation

Phases ship independently. Walker's dispatch path gets a new arm per phase as each provider-type's resolver integration lands.

LOC estimates are cumulative across audit rounds. Rev 0.7 verified actuals: `config.primary_model` spans ~194 source-site occurrences across 18 files (not the earlier 93/16 estimate); Phase 6 revised up to 2200-2800 after Stage 2 cycle-2 found UI scope underestimated; Phase 0 split into 0a (infrastructure, 1000-1200) + 0b (schema landing, 1200-1600). Current total: ~4900–5850 LOC, 11–14 sessions. See §6 section header of each phase for the current number; this paragraph is an index, not authoritative.

### Phase 0a — Infrastructure extraction (Root 14 — rev 0.7, ~1000–1200 LOC)

Rev 0.6 bundled all of Phase 0 into a single "groundwork" bucket and cycle 2 Stage 1 demonstrated that the Phase 0 promises rested on infrastructure that didn't exist. Phase 0a is infrastructure-creation — the scaffolding every later phase depends on — and must finish clean before 0b starts.

- **Envelope writer extraction** (~300–500 LOC, B-F1 / Root 13 resolution). Extract `write_contribution_envelope(schema_type, body, source, extras) -> Result<ContributionId>` as the sole `INSERT INTO pyramid_config_contributions` site. Refactor the ~35 existing INSERT call sites across 9 files (`migration_config.rs`, `config_contributions.rs`, `wire_migration.rs`, `demand_signal.rs`, `evidence_answering.rs`, `wire_pull.rs`, `prompt_cache.rs`, `generative_config.rs`, `db.rs`) to call it. Add a clippy deny-rule regex blocking raw `INSERT INTO pyramid_config_contributions` outside the envelope writer.
- **Chronicle event const declarations** (~70 LOC). Declare all new events in `compute_chronicle.rs` per §5.4.6's authoritative list (count = 22 as of rev 1.0.1). Emission sites use the consts, not string literals. Phase 0a-1 body does NOT restate the count here — §5.4.6 is the single source of truth; plan-integrity Check 9 counts §5.4.6 and verifies every other count assertion matches.
- **Placeholder interpolation engine v2** (~250 LOC, A-F1 / Root 13). Current `generative_config.rs:substitute_prompt` is single-brace literal replacer over 4 tokens. Extend (or add `substitute_prompt_v2`) to support double-brace `{{placeholder}}` with an async resolver context carrying handles to OllamaProbe, OpenRouter client (`/api/v1/models`), MarketSurfaceCache, and a SYSTEM_DEFAULTS borrow. Resolve at skill-invocation time. Named placeholders for v3: `{{openrouter_live_slugs}}`, `{{ollama_available_models}}`, `{{market_surface_slugs}}`, `{{patience_secs_default}}`, `{{retry_http_count_default}}`, `{{max_budget_credits_default}}`. Registered in the integrity-pass placeholder table (§2.13 item 1).
- **ProviderReadiness trait + four impls** (~200 LOC). Trait def per §2.6. Impls in `local_mode.rs`, `fleet_mps.rs` or `fleet.rs`, `provider.rs` (openrouter), `compute_market_*.rs` (market). Each impl ~30–80 LOC carrying the reason enum.
- **`ChainDispatchContext` rename** (~40 LOC, F-D2). Rename `chain_dispatch.rs:124`'s `StepContext` to `ChainDispatchContext`. Pin `step_context.rs:275` as the Decision home.
- **`arc_swap` dep add** to `Cargo.toml` (Root 13 / F-D9).

**Phase 0a exit criteria (rev 1.0.1):** envelope writer is the sole INSERT site (lint-enforced — see below); all chronicle events from §5.4.6 (22 at rev 1.0.1) exist as consts AND grep-hit at least one emission site (plan-integrity Check 9 enforces); placeholder engine resolves all 6 named placeholders against live sources in tests; ProviderReadiness trait compiles with four impls returning Ready stubs; `ChainDispatchContext` rename complete; **§2.17 sequential boot sequence test** exercises all 11 canonical steps; build-starter code paths fail-fast with `AppNotReady` when invoked before step 9; `scope_cache_reloader` supervisor's restart-budget tested via injected panic; `ScopeSnapshot` does NOT derive `Serialize` (cargo-check enforced — Root 27 type-guard).

**Phase 0a LOC (rev 1.0):** honest range ~1700-2100 per cycle-2 audit. Split into 0a-1 + 0a-2 with explicit commit order; **§11 B-F9's alternative commit list is retired — this body is the canonical sequence**.

**Phase 0a-1 pre-flight (rev 1.0 / Root 29):** before any Phase 0a-1 commit, produce a **consumer inventory artifact** at `docs/plans/history/walker-v3-consumer-inventory.md` that greps every site reading `config.primary_model`, `config.fallback_model_{1,2}`, `pyramid_tier_routing`, `RouteEntry`, `resolve_ir_model`. Map each site to one of: `reads Decision` (migrates in Phase 1), `reads synthetic Decision` (preview path — DADBEAR, cost estimation, operator-HTTP preview), `retires` (legacy build.rs pipeline — retired once Phase 1 removes the legacy dispatch path and any `use_chain_engine: false` config is either operator-enabled or rolled back), or `test fixture`. Phase 1 LOC locks against the inventory. Cycle 3 Stage 2 found 55+ hits in `llm.rs`/`chain_executor.rs`/`dadbear_preview.rs` that rev 0.9's "~4-8 sites" didn't acknowledge — inventory surfaces these before Phase 1 commits scope.

**Phase 0a-1 canonical commit order (~800 LOC, reordered rev 1.0.1 per cycle-4 A-F4 / B-F5):**
1. `arc_swap` Cargo dep add + chronicle const declarations (count per §5.4.6) — trivial, low-risk baseline commit.
2. `ChainDispatchContext` rename (mechanical, ~40 LOC).
3. `ProviderReadiness` trait definition + four stub impls returning `Ready` — enables Phase 0a-2 fills.
4. **Envelope writer introduced as pass-through shim** — new `write_contribution_envelope()` function with signature matching future final form, body = existing INSERT logic (no validation, no BEGIN IMMEDIATE yet). Refactor all ~35 raw INSERT sites to call the shim. Grep-based CI check / cargo-deny lint asserts zero raw INSERTs outside the writer + test allowances. (Cycle-4 reorder: shim lands BEFORE serialization/index so the refactor itself doesn't collide with the new constraint.)
5. **Activate envelope writer's full body** — replace shim body with (a) normalize-then-validate via schema_annotation shape, (b) `BEGIN IMMEDIATE TRANSACTION` wrap on supersessions (`TransactionMode::OwnTransaction`; migration path uses `JoinAmbient` per §2.16.1), (c) `config_supersession_conflict` event on SQLITE_CONSTRAINT. Pre-index de-dup step (§5.3 step 7). Then partial unique index `uq_config_contrib_active` migration. Validation, serialization, and the index become active atomically; no intermediate window where legacy raw INSERTs can violate the index.
6. Worktree cleanup: `git worktree list` shows main only; `.claude/worktrees/*` removed or merged back per §9.

**Phase 0a-2 canonical commit order (~1000 LOC):**
1. `migration_marker` + `onboarding_state` + `node_identity_history` contribution schemas registered with four-part bundles.
2. Placeholder interpolation engine v2 with TTL + single-flight + circuit breaker + YAML injection escaping.
3. `scope_cache_reloader` task with restart-budget supervisor + 250ms debounce.
4. `ScopeSnapshot` type-guard: remove `Serialize` derive from raw type; add `redacted_for_chronicle() -> RedactedSnapshot`.
5. `retract_config_contribution` handler + depth-ceiling/cycle-detection (§5.5.3).
6. Boot sequence wiring per §2.17 — main.rs orchestrates the 10-step startup; `AppMode` state machine in `AppState`.
7. Sequential integration test: full boot → migration → ready, with injected panic on reloader to verify quarantine.

Phase 0a-1 gates Phase 0b (schemas can land); Phase 0a-2 gates Phase 1 (total migration requires readiness trait).

**Clippy deny-rule, honestly (F-C3-12 / D-M3):** "clippy deny-rule regex blocking raw INSERT" from rev 0.7 is imprecise — clippy is AST-aware, not regex. Phase 0a ships this as a grep-based CI check (`scripts/check-insert-sites.sh`) OR a Rust custom lint via `dylint`. Allow-list: the envelope writer itself, test utilities (flagged with `#[cfg(test)]`). Either implementation acceptable; Phase 0a picks one.

**Worktree pre-flight (F-C3-11):** Phase 0a-1 exit criteria explicitly requires `git worktree list` shows main only — `.claude/worktrees/*` directories removed or merged back. Stale worktree copies of `config_contributions.rs` silently rot otherwise.

### Phase 0b — Schema landing + Decision builder (~1200–1600 LOC, Root 14)

- `walker_resolver.rs` with scope-chain walker + typed accessors + SYSTEM_DEFAULTS table.
- `walker_decision.rs` — **DispatchDecision builder**. Called at outer-chain-step entry. Runs resolver per (provider_type), calls each provider's `can_dispatch_now`, assembles immutable Decision, emits `decision_built` chronicle event (with scope_snapshot serialized). This is the compute-once spine; every downstream consumer reads from `StepContext.dispatch_decision`.
- Extend the existing compile-embedded `src-tauri/assets/bundled_contributions.json` manifest with the walker_* entries and keep `walk_bundled_contributions_manifest` as the ONLY runtime loader. If repo-local YAML files under `src-tauri/bundled_contributions/walker_*/` are preferred for authoring, they are build-time inputs to a manifest-generator step only; runtime still reads one manifest, and `test_bundled_tier_coverage` validates the generated manifest artifact that ships.
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
| `walker_provider_openrouter` | "I want extract routed to mercury-2, high to grok" → OR-slug YAML with `model_list` per tier. Skill prompt body stays authoring-time text, but the canonical OR slug reference is injected LIVE at skill-use time via placeholder interpolation from `/api/v1/models` (per §2.10); slugs are not baked into the authored prompt body. | `provider/model` (e.g. `inception/mercury-2`) |
| `walker_provider_local` | "BEHEM serves qwen-32b for high tier" → Ollama-slug YAML. Skill prompt includes common Ollama tag patterns. Can optionally cross-reference live `/api/tags` probe output if present. | Ollama tag (e.g. `qwen2.5:32b`) |
| `walker_provider_market` | "Request common 70B and smaller-GPU models from market" → market-slug YAML. Skill prompt includes the bundled seed list from Q4 as the known-good anchor, and notes that market slugs mirror what providers actually publish (Ollama-format for local-served offers, OR-format for bridge-served). | Matches what offers publish (mixed format possible) |
| `walker_provider_fleet` | "Prefer peers with the qwen-32b model cached for high tier" → fleet-slug YAML mirroring local format since fleet peers are typically running Ollama. | Ollama tag |
| `walker_call_order` | "Try market first then local then OR" → order array + any per-provider scope-3 overrides. Simplest skill; short prompt. | N/A (provider_type names, not slugs) |
| `walker_slot_policy` | "For extract, wait 15 minutes on market; bypass market entirely for synth_heavy" → nested slots map with `overrides` (scope 2), `per_provider` (scope 1), and optional `order`. Skill prompt must distinguish scope 1 vs scope 2 semantics clearly. | N/A (references provider_type + tier name) |
| `compute_market_offer` (Root 12 / NEW) | "I want to publish my BEHEM's 70B models for sale on the market at X credits/M-tokens" → offer YAML with model_id, pricing, backing-provider. Covers the bridge-operator authoring gap. | Matches market slugs (Ollama or OR format depending on backing provider) |

Each skill ships as a bundled contribution at Phase 0. Without it, the Create tab card for that schema is dead text. WITH it, operator intent → working YAML → live walker behavior, with no Rust change. Skill prompts use `{{placeholder}}` interpolation for live values (slug lists, SYSTEM_DEFAULTS); interpolation happens at skill-use time (Root 9 / F-D11 resolution).

### Phase 1 — Total migration + Decision-consumer refactor (~600 LOC, dropped from rev 0.4)

Scope corrected: actual site count is **194 occurrences across 18 files** (Stage 1 audit), NOT 93/16. The architectural goal is still to converge those reads onto `StepContext.dispatch_decision`, but implementation sizing must NOT assume the refactor reduces to **~4-8 sites**. The authoritative surface is the consumer inventory artifact from Phase 0a-1, which already found 55+ hits in core files before broader pass completion. Phase 1 planning, staffing, and LOC estimates should key off that inventory, not the old optimistic shorthand.

- Implement the four provider dispatchers to read `decision.per_provider[ProviderType::...]` for their params.
- Decision builder (from Phase 0) runs resolver + readiness per provider at step entry.
- StepContext construction adds `dispatch_decision: DispatchDecision`.
- Delete `config.primary_model`, `fallback_model_1`, `fallback_model_2` struct fields. No `#[deprecated]` bridge. `cargo check` fails loud until every consumer is migrated (that's the point — enforces totality).
- Tests (rev 1.0 expanded per B-F4):
  - Decision construction for each persona (tester / hybrid / standard).
  - Intra-provider fallback walking model_list on rate-limit.
  - Decision immutability across retry attempts within a step.
  - Chain-YAML tier coverage assertion (bundled chains resolve).
  - Save-time shape validation catches invalid overrides.
  - **DADBEAR bucket rotation + breaker carry-forward** for `probe_based` and `time_secs` variants (§5.5.1) — asserts 23:59→00:00 rotation preserves tripped state.
  - **Retraction depth-chain** (§5.5.3) — ≤16-hop walk with cycle detection; `RetractionChainCorrupt` on pathological input; `config_retracted_to_bundled` when all ancestors retracted.
  - **v1-peer strict refusal** (§5.5.2) — mixed-version roster test asserts `PeerIsV1Announcer` returned for v<2 peers; `fleet_peer_version_skew` chronicle event fires first-seen-per-boot.
  - **ArcSwap reloader quarantine** (§2.17.2) — injected-panic test asserts restart-budget (3/60s), LKG cache preserved on 4th panic, `scope_cache_quarantined` event, AppMode::Quarantined.
  - **`on_partial_failure: fail_loud` privacy path** — slot-scope override asserted to emit `dispatch_failed_policy_blocked` instead of cascading to next provider.
  - **Synthetic Decision vs live Decision drift** (§Phase 6 `preview_vs_apply_drift` predicate) — DADBEAR compile+apply mismatch triggers event.
  - **Envelope writer `BundledBootSkipOnFail` vs `Strict`** — malformed bundled row logs and boots; malformed operator-authored row returns error.
  - **`use_chain_engine` current-default + intervention path** (§5.6.3) — fresh-install completes an Ollama build end-to-end with the already-true default; a config that loads as `use_chain_engine: false` hits the intervention modal and only proceeds after operator enablement or rollback.
  - **Multi-writer ScopeCache debounce** (F-C3S2-7) — two supersessions of different walker_* schemas within 250ms, asserts both reflected in post-debounce cache.

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

### Phase 6 — UI + onboarding (~2500–3100 LOC, rev 0.8 revised up for Root 21)

`InferenceRoutingPanel.tsx` is already 894 lines. Rev 0.8 scope includes:

- Six provider-type config renderers (~400 LOC)
- Slot-policy grid (tier × provider with per_provider overrides) (~300 LOC)
- Call-order drag-reorder (~150 LOC)
- Onboarding wizard (Root 10 orchestration + `active: false → true` consent flip + balance cache prime + `onboarding_complete_at` write) (~400 LOC)
- Sensitive-supersession confirmation modals + directional guard for `on_partial_failure` (~200 LOC)
- Seven generation-skill drafts + DRAFT preview lane in Tools > Create (~300 LOC)
- Schema-annotation-driven client-side shape hints (not a replacement for server validator; UX pre-check) (~200 LOC)
- **Chronicle color-map extension (Root 21 / A-C3):** `ComputeChronicle.tsx` currently colors ~35 fleet/market/network events; walker v3 adds 22 local-only events (per §5.4.6). Add color entries, category = "Dispatch Decisions" (~100 LOC).
- **"Dispatch Decisions" aggregation tab (Root 21 / A-C3, Issue 8):** filter preset over Decision lineage for a build. Shows per-step `decision_built` payload (redacted per §5.4.3), `effective_call_order`, winner provider, per-provider NotReady reasons. Click-through to raw chronicle. Primary surface for operator's "why did walker route to X" question (~400 LOC).
- **Bundled-vs-operator-authored source indicator** in Settings (B-D brain-dump item): each config surface displays `source: bundled | operator_authored` with a "Reset to bundled" affordance (retraction per §5.4.4 reactivates the bundled ancestor) (~150 LOC).

**Drift comparator predicates (Root 21 / B-F11 / A-M2, rev 0.8 closes the residual):**
- `preview_vs_apply_drift`: emit when `decision_synthetic.effective_call_order != decision_apply.effective_call_order` OR `decision_synthetic.per_provider[winner].model_list[0] != decision_apply.per_provider[winner].model_list[0]`. Not every field — just order + winner's primary model. Avoids flood-fire from patience/budget params cycling.
- `requester_provider_param_drift`: emit at boot if `walker_provider_market.overrides.patience_secs` differs from `market_delivery_policy.overrides.callback_post_timeout_secs` by >2× in either direction, OR if `walker_provider_fleet.overrides.fleet_peer_min_staleness_secs` differs from `fleet_delivery_policy.overrides.peer_staleness_secs` by any amount (different axes — any divergence is notable). One event per boot, not per step.

**Emission-site integrity check (Root 21 / A-M2):** plan-integrity skill's Check 2 strengthened — every `EVENT_*` declared in §5.4.6 must grep-hit at least one non-test Rust file as an emission site. Unused events caught before ship.

- Settings surface renders six configs + call-order + slot-policy via the generative-YAML-to-UI renderer.
- First-launch onboarding wizard (v2 plan's wizard carries forward, re-scoped to the resolver framing).
- Chronicle events for live breaker visibility and Decision trace: `decision_built` (serialized Decision — answers "why did walker route to X"), `breaker_tripped`, `breaker_skipped`, `config_superseded`, `tier_unresolved`, `provider_skipped_readiness` (with `reason: NotReadyReason` — specific), `decision_build_failed` (empty model_list + active:true case), `sensitive_supersession_confirmed` (audit trail).
- Probe-driven dropdowns for per-provider-type model suggestions.

### Total: ~5600–6600 LOC, 13–16 sessions (rev 0.9: Phase 0a revised up to 1700-2100 and split into 0a-1 + 0a-2 per F-C3-12; new §2.17 boot-order + §2.18 state audit + §5.5 cascade discipline absorbed into existing phase budgets; new contribution schemas `onboarding_state` + `node_identity_history` add ~100 LOC to Phase 0b)

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
| Audit Q5 — Phase 1 legacy-coexistence contract | Removed in rev 0.5 via total-migration approach (Root 2). No `#[deprecated]` bridge, no Phase 1→5 coexistence. Phase 1 deletes `config.primary_model` struct fields in one commit; migration §5.3 populates walker_provider_openrouter.model_list. `cargo check` enforces no stale readers. |
| Audit Q6 — Saturation patience scope (per-model vs per-leg) | `patience_clock_resets_per_model: bool` parameter. Default false (single budget per leg). Operator overrides at any scope. |
| Audit Q7 — `pricing_json` destination | Lives on the offer rows Wire-side; node-side pricing metadata (if needed for cost reporting) is an `overrides.pricing_ref` pointer at provider-type scope. Low priority; cost chronicle works without it. |
| Audit Q8 — ConfigSynced locking discipline | Snapshot-per-call clone. Resolver takes an `Arc<ScopeCache>` at dispatch start; config supersessions rebuild the next `Arc<ScopeCache>` for subsequent calls. No locks held across await. |
| Audit Q9 — Chain-YAML tier coverage | Phase 0 ships `test_bundled_tier_coverage`. Build fails if coverage regresses. |
| Audit Q10 — stale_engine / DADBEAR + live supersession | DADBEAR's recursive maintenance loop reads the resolver at each dispatch (not snapshotted at schedule time) — picks up live supersession correctly. Tests in Phase 2 verify this via DADBEAR smoke with config edits mid-build. |
| Audit Q11 — O3 breaker reset "per-build only" vs probe-based | `breaker_reset` parameter supports `per_build` (default), `probe_based`, `time_secs:N`. Operator's call via slot-policy override. No design-level "one wins" — resolver handles all three. |
| Audit Q12 — O6 bundled seed source | Q4: Wire does not publish canonical walker_provider_market; seed is `bundled`-only, operators supersede to `operator_authored`. No `wire_pulled` step in the chain for walker_* contributions. |
| **PUNCHLIST P0-1** (Ollama local builds fail immediately) | Resolved by Phase 1 deletion of `config.primary_model` + migration §5.3 populating `walker_provider_openrouter.model_list`, with §5.6.3 ensuring the chain engine path is the only supported dispatch path after migration. Root cause (resolve_ir_model returns primary_model) eliminated when primary_model ceases to exist. |
| **PUNCHLIST P0-2** (legacy installs with `use_chain_engine: false` bypass the Decision spine) | Resolved by §5.6.3. Jointly closed with P0-1 as a single Phase 1 outcome. Fresh installs already default to `true`; any config that currently loads as `false` must hit the intervention modal because today's serde shape cannot distinguish "old missing field" from explicit `false`. |
| **PUNCHLIST P1-5** (Provider health monitoring) | Delivered by §2.6 `ProviderReadiness::can_dispatch_now` trait + four impls. Phase 0a-2 lands the trait; Phase 1 wires into Decision builder. |
| **PUNCHLIST P2-8** (Breaker trip + recovery semantics) | Delivered by §3 `breaker_reset` tagged union + Phase 5 `per_build` / `probe_based` / `time_secs:N` variants. |

---

## 8. Acceptance criteria

- **Tester smoke:** GPU-less tester installs app, asks question on small corpus. Bundled call-order `[market, local, openrouter, fleet]`. Decision at step entry: `market` ready (onboarding seeded ≥1 credit AND balance cache primed during onboarding Page 4 — see orchestration note below), `local` NotReady(OllamaOffline), `openrouter` NotReady(CredentialMissing), `fleet` NotReady(NoReachablePeer). `effective_call_order: [market]`. Network providers matching tester's market config serve via market. If market dry for the requested tier (`MarketSurfaceCache` has zero non-self offers) → `decision_build_failed` chronicle event with reason `NoMarketOffersForSlot`; build fails loud rather than silently burning patience on a dry market.
  - **Onboarding/walker orchestration (Root 10 / Issue 3, Root 13 F-D2, Root 17):** `tester-onboarding.md`'s Page 4 (tunnel validation) additionally: (a) primes the market balance cache via parallel `/balance` fetch alongside the existing handle-check, (b) flips `walker_provider_market.overrides.active` from the bundled `false` to `true` as the explicit consent supersession (operator-authored, chronicle-logged — this IS the market-participation consent record), (c) writes `onboarding_complete_at` timestamp to an `onboarding_state` contribution (rev 1.0 §2.18 converged here — the rev-0.9 "pyramid_config.json" residual was struck in rev 1.0.1 per B-F3). **Current verified owner:** the existing onboarding save path lives in `src-tauri/src/main.rs` as `save_onboarding(...)`; `src-tauri/src/pyramid/onboarding.rs` does not exist today. Therefore Phase 0a-2 MUST either extend `main.rs::save_onboarding` directly to write the contribution, or land a prior/shared extraction that creates `src-tauri/src/pyramid/onboarding.rs` and moves ownership there. Do not leave this as an unnamed "(or equivalent)" write site. Walker plan and tester-onboarding plan must be rev-aligned on this dependency — the integrity-pass script (§2.13 item 6) enforces that `onboarding_complete_at` has a named writer. Boots where the timestamp is >5min old require a fresh balance fetch before market readiness passes.
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
  - **Root 1 — No compute-once dispatch decision.** Collapses A-C1/C2/C4/C11, B-M2/M5/M6/M8, B-m6. Fix: introduce `DispatchDecision` (§2.9) built once at outer chain step entry, carried via StepContext, immutable for step lifetime. Snapshot boundary = Decision lifetime (Q8 vs Q10 contradiction dissolves). Privacy fail-policy (`on_partial_failure`) is an explicit Decision field. Implementation scope is controlled by the Phase 0a-1 consumer inventory, not by the retired "~4-8 sites" estimate.
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
- **Rev 0.7 (2026-04-21):** Cycle 2 Stage 1 informed audit (23 findings across 2 auditors) absorbed via systemic-synthesis. 5 structural roots (13–17). Cycle 2 surfaced a meta-pattern: rev 0.6 promised infrastructure and discipline that didn't actually exist in the codebase.
  - **Root 13 — Rev 0.6 added discipline and infrastructure-promises without wiring them.** Symptoms: envelope writer doesn't exist (35 raw INSERT sites); `{{placeholder}}` syntax doesn't exist in generative_config.rs engine; `onboarding_complete_at` referenced but undefined; 10 new chronicle events not in `compute_chronicle.rs` const registry; §11 over-claimed F-D12 SQLite mechanics; §2.12 still listed removed `openrouter_credential_ref`; section numbering ordered 2.11→2.13→2.14→2.12; companion drafts doc stale across 3 revs. **Fix:** §2.14 (now §2.13) upgraded from prose discipline to **mechanized `docs/tools/plan-integrity.sh`** script with 8 automated checks (placeholder resolution, chronicle event registry grep, struct↔catalog cross-check, absorbed-finding verification, section monotonicity, cross-ref field write-sites, companion rev-match, sensitive-field catalog parity). Runs in CI AND `just plan-integrity`. No rev is audit-ready without it.
  - **Root 14 — Phase 0 is infrastructure-creation, not groundwork.** Cycle 1 treated Phase 0 as "scaffolding"; cycle 2 showed it's ~2200-2800 LOC of actual infrastructure work. **Fix:** Phase 0 split into **Phase 0a (infrastructure extraction, ~1000-1200 LOC)** — envelope writer, chronicle consts, placeholder engine v2, ProviderReadiness trait + 4 impls, ChainDispatchContext rename, arc_swap dep — and **Phase 0b (schema landing, ~1200-1600 LOC)** — schemas, skills, shape validator, tier_registry rewrite, test_bundled_tier_coverage.
  - **Root 15 — Failure and growth paths unspecified.** New §2.14 "Growth and failure modes" covering: (2.14.1) cascade exhaustion → `dispatch_exhausted` event + `StepFailure::DispatchExhausted` + one-pass guarantee; (2.14.2) multi-node-per-operator SelfDealing is node-local only, v3 workaround is per-node `active: false`, v4 adds operator-scoped sibling filtering; (2.14.3) schema evolution — new parameter keys require annotation+catalog+SYSTEM_DEFAULT+accessor (Rust release), only redeclaration at new scopes is schema-change-free. §2.3's "no schema change" language corrected.
  - **Root 16 — `on_partial_failure` scope semantics.** Narrowed to scope 2 only (slot-level, not scope 4 per-provider — would create Decision-level ambiguity). Directional sensitivity: confirms only on `new == cascade AND old != cascade`. Added to §2.15's explicit sensitive-fields list.
  - **Root 17 — Drift comparators / market active default.** Bundled `walker_provider_market.active = false`; onboarding Page 4 flip-to-true is the consent record. (Drift comparators B-F11 remain as residual — see below.)
  - **Residuals patched:** A-F4 (§2.15 sensitive-fields list — removed `openrouter_credential_ref`, added `on_partial_failure`), A-F10 (§2.12 synthetic Decision adds per-tier iteration spec for cost estimation), A-F11 (§4.4 fleet drift pairing noted; drift comparators spec deferred — see below), B-F5 (§2.12 synthetic Decision emits `decision_previewed` not `decision_built`), B-F9 (§5.3 adds SQLite CREATE-COPY-DROP-RENAME recipe), A-F8 (Phase 1 framing clarification — behavioral vs mechanical touch surface), A-F7 + B-F7 + B-F11 (companion drafts doc synced to rev 0.7 with rev-match banner).
  - **Still open (B-F11 Root 17 drift comparator specs):** `preview_vs_apply_drift` and `requester_provider_param_drift` concrete predicate thresholds remain fuzzy. Named as implementation-time decision; integrity pass will enforce that whichever spec ships is documented before Phase 0a completes.
  - Ready for Cycle 2 Stage 2 discovery audit against rev 0.7.
- **Rev 0.8 (2026-04-21):** Cycle 2 Stage 2 discovery audit (24 findings across 2 auditors) absorbed via systemic-synthesis. 5 structural roots (18-22). Plan-integrity skill created and invoked between rev 0.7 corrections and rev 0.8 — caught + auto-fixed 8 drift items.
  - **Root 18 — Rev 0.7's "mechanization" depends on CI that doesn't exist.** Meta-recursion: rev 0.7 diagnosed "promised infrastructure without wiring" in rev 0.6, then did the same thing with §2.13's CI-gated script. **Fix:** §2.13 walked back from "CI-script + clippy deny-rule" to honest "plan-integrity skill in the audit cycle, Claude-run discipline." Skill built at `~/.claude/skills/plan-integrity/SKILL.md`; wired into conductor-audit-pass as required stage between audit rounds. CI port is a separate future initiative; plan doesn't depend on it.
  - **Root 19 — Walker v3 silently changes cross-boundary wire-format semantics.** Collapses B-I1, B-I2, B-I3, B-I4, B-I8, brain-dump Q10. **Fix:** new §5.4 "Cross-boundary wire-format contracts" names every contract walker v3 changes. Fleet dispatch gets additive `model_id` field (B-I2 critical — fleet arm couldn't dispatch without this). Fleet announce gets `announce_protocol_version` for mixed-version compatibility. Chronicle `scope_snapshot` gets `redacted_for_chronicle()` + `local_only: bool` schema_annotation flag. `retract_config_contribution` handler implemented (Phase 0a). Unknown migration provider_id changed from soft-fail-with-warning to hard-fail by default.
  - **Root 20 — Concurrency and lifecycle races unbounded.** Collapses B-I5, A-M4, B-I6, B-I7, A-C6, A-M1, brain-dump Q10 (SelfDealing node_id history). **Fix:** new §2.16 "Concurrency and lifecycle invariants" — partial unique index on `(slug, schema_type) WHERE status='active'` + `BEGIN IMMEDIATE` (B-I5 critical); named `scope_cache_reloader` task with supervision + 250ms debounce (A-M4); placeholder engine TTL + single-flight + circuit breaker (B-I6); pre-migration in-flight build check with UI recovery dialog (B-I7); `NetworkUnreachable` reason on consecutive-failure heuristic (A-C6); time-bucketed DADBEAR build_id for per_build breaker semantics (A-M1); node_identity_history for SelfDealing across re-onboardings (Q10).
  - **Root 21 — Chronicle viewer UI doesn't speak walker events.** Collapses A-C3, A-M2. **Fix:** Phase 6 LOC revised up to 2500-3100; adds chronicle color-map extension, "Dispatch Decisions" aggregation tab as primary observability surface, bundled-vs-operator-authored source indicator. Drift comparator predicates spec'd concretely (`preview_vs_apply_drift` = order + winner's primary model mismatch; `requester_provider_param_drift` = boot-time once, >2× deviation threshold). Plan-integrity Check 2 strengthened: every `EVENT_*` const must grep-hit an emission site.
  - **Root 22 — Envelope-writer validator scope/failure-mode underspecified.** Collapses A-C4, A-M3, A-C8. **Fix:** §2.11 adds `WriteMode::{Strict, BundledBootSkipOnFail}` — bundled loader logs and skips on validation failure rather than bricking boot. Placeholder engine v2 adds YAML-safe injection escaping + control-char rejection (prevents market-surface-slug adversarial content from rewriting draft YAML). Clippy deny-rule → honest grep-based CI check (or Rust custom lint via `dylint`) with named exempt list; not AST-level clippy.
  - **Residuals patched:** A-C1 / A-C2 / A-C5 (CI infrastructure — deferred via Root 18 walk-back; onboarding write-site ownership is now explicit in §8 via `main.rs::save_onboarding` or a named prior extraction), A-C7 (migration recovery dialog added per §2.16.4), D-M3 (clippy deny-rule scope per Root 22), D-m1 (bundled market model_list — stays pragmatic seed, onboarding wizard offers to expand per §6 Phase 3), D-m2 (test_bundled_tier_coverage gated on plan-integrity's emission-site check). Brain-dump items 1-12 noted for implementation-time decisions; none require rev 0.9 before Phase 0a.
  - **Plan-integrity skill caught** in its first run: Phase 0a event count "10" → "12", stale `openrouter_credential_ref` in struct comment, §2.3 schema-static overclaim needs forward-reference to §2.14.3, §6 header LOC paragraph from rev 0.3, §6 Phase 6 contradiction "1400-1800" vs header "2200-2800", §7 Q5 legacy-coexistence text superseded by Root 2, trailing "Planned cadence" block at rev 0.2, "End of plan rev 0.2" footer. All auto-fixed.
  - Rev 0.8 is implementation-ready pending Adam GO. No structural roots remain open; residual items are implementation-time decisions for Phase 0a workstream leads.
- **Rev 0.9 (2026-04-21):** Cycle 3 Stage 1 informed audit (28 findings across 2 auditors, including 1 CRITICAL) absorbed via systemic-synthesis. 5 new structural roots (23-27) PLUS meta-findings that strengthened plan-integrity skill itself. Both auditors independently noted cycle-3 findings are moving from architecture to seams — sign of convergence at architectural level, residual work at seam level.
  - **Root 23 — "Everything is a contribution" quietly violated by walker v3's internal state.** Collapses F-C3-4 and A-brain-dump-5 (`_v3_migration_marker`, `onboarding_complete_at`, `node_identity_history`, `app_mode`, breaker state all live outside the contribution pattern). **Fix:** new §2.18 "Internal state contribution audit" — per-state decision table. `onboarding_state` and `node_identity_history` become new contribution schemas (rev 0.9 Phase 0b addition). Boot-flag / runtime-ephemeral / derived state justified as non-contribution with explicit rationale.
  - **Root 24 — Boot and init order unspecified.** Collapses F-C3-2 (CRITICAL: builds can enter `running` between migration check and commit → schema flip under live build), F-C3-1 (ArcSwap reloader panic recursion needs quarantine). **Fix:** new §2.17 "Boot and init order" naming the canonical sequence (app_mode flag set under BEGIN IMMEDIATE before builds-check; migration transaction; bundled_contributions load; ArcSwap reloader with restart-budget + quarantine supervisor; ConfigSynced listener; stale_engine rehydrate; app_mode='ready'; routes last). `app_mode` state added to `pyramid_config`. `scope_cache_reloader` gets 3-restarts-in-60s budget before quarantine mode (hold last-known-good cache, mark offending contribution_id).
  - **Root 25 — Rev 0.8's structural promises accumulated without Phase 0a LOC bump.** Collapses F-C3-12, B-F1 (event count drift: 10 vs 12 vs 14), B-F3 (`NetworkUnreachable` not in enum), B-F4 (companion doc stale with `source: bundled`). **Fix:** Phase 0a LOC revised to 1700-2100, split into Phase 0a-1 (~800 LOC — envelope writer + consts + ChainDispatchContext rename + unique index migration) and Phase 0a-2 (~1000 LOC — ProviderReadiness + placeholder engine + scope_cache_reloader + ScopeSnapshot type-guard + retract_config_contribution + §2.17 wiring). Event count fixed to 18 with full enumeration. `NetworkUnreachable` and `PeerIsV1Announcer` added to §2.6 enum. Companion YAMLs scrubbed.
  - **Root 26 — Cross-subsystem cascade effects not traced.** Collapses F-C3-3/5/7/13/15, B-F5/F7. **Fix:** new §5.5 "Cross-subsystem cascade discipline" with 9 sub-items. DADBEAR bucket rotation carries forward last_failure_at if prior bucket tripped (only for per_build; other variants self-contained). Fleet dispatch to v1-announcer peers: strict mode — refuse dispatch, mark `PeerIsV1Announcer`. Retract walks ≤16 hops with visited-set cycle detection; depth exhausted or cycle → `RetractionChainCorrupt` loud fail. Unknown-provider-id migration surfaced in §2.17's boot modal, not CLI flag. Path-aware validator spec (schema_annotation declares path-rules per schema). Dispatch Decisions aggregation gets server-side route + pagination. Tester stuck-state banner wired to `decision_build_failed` + no-prior-success. Offline backoff as resolver-chain parameters (eliminates Pillar 37 violation).
  - **Root 27 — Type-level guards.** Collapses F-C3-6 + A-brain-dump-2. **Fix:** §5.4.3 revised. Remove `Serialize` derive from raw `ScopeSnapshot` and `DispatchDecision`. Only `redacted_for_chronicle() → RedactedSnapshot` and `for_chronicle() → DecisionChronicleView` implement `Serialize`. Dispatchers consume via field access, not serialization. Cargo-check enforced. Rev 0.9 makes redaction a type property, not a convention.
  - **Residuals patched:** F-C3-8 (aggregation route in §5.5.6), F-C3-9 (backoff as resolver params + moved to ProviderReadiness — §5.5.8), F-C3-10 (audit-evidence artifact at `docs/plans/history/plan-integrity-rev0.7-to-rev0.8.md`, Check 11 added to skill), F-C3-11 (worktree pre-flight in Phase 0a-1 exit criteria), F-C3-13 (boot modal not CLI flag — §5.5.4), F-C3-14 (30d snapshot auto-prune — §5.5.9), F-C3-15 (path-aware validator — §5.5.5). B-F1/F2/F3/F4 (count drift / Check 9 / enum coverage / Check 10 / companion scrub). B-F6 (supersession conflict UX — new `config_supersession_conflict` event, 18th event). B-F8 (sub-numbering style consistent in rev 0.9). B-F9 (Phase 0a commit order: arc_swap + consts first, ChainDispatchContext rename, ProviderReadiness trait + stub impls, unique index migration, envelope writer + BEGIN IMMEDIATE + validator hookup, 35-site INSERT refactor, placeholder engine, scope_cache_reloader, retract handler). B-F10 (deadline-grace asymmetric semantics note). B-F11 (onboarding_complete_at decided as contribution per §2.18 — resolved). B-F13 (six schemas vs seven skills framing: rev 0.9 §2.18 adds two more schemas, so it's now eight schemas + seven walker skills + one compute_market_offer skill).
  - **Plan-integrity skill upgraded:** Check 9 (count-assertion parity), Check 10 (enum variant coverage), Check 11 (audit-evidence artifact persistence). Skill at `~/.claude/skills/plan-integrity/SKILL.md` updated. First persisted artifact at `docs/plans/history/plan-integrity-rev0.7-to-rev0.8.md`.
  - **Convergence indicator:** findings/root ratio has dropped to ~2-3 in cycle 3 (was 4-5 in cycles 1-2); remaining structural findings are at seams between rev 0.8 additions. Both auditors flagged that further cycles hit diminishing returns on paper — wanderers on built systems per `feedback_wanderers_on_built_systems.md` catch more residuals per hour than another paper audit round.
  - Ready for Cycle 3 Stage 2 discovery audit against rev 0.9.
- **Rev 1.0 (2026-04-21):** Cycle 3 Stage 2 discovery audit (21 findings across 2 auditors, including 1 CRITICAL that reopened a prior CRITICAL) absorbed via systemic-synthesis. 6 new structural roots (28-33). Adam caught the architectural regression: rev 0.9 had defaulted to hardcoding more state instead of following "everything is a contribution." Rev 1.0 converts runtime state to contribution-native wherever the semantic fits.
  - **Root 28 — Rev 0.9's §2.17/§2.18 built on false storage premise.** `pyramid_config` is a JSON file, not a SQL table; `BEGIN IMMEDIATE` around `app_mode` in it is physically impossible. No migration framework exists (`wire_migration.rs` is contribution-rewrite; no `schema_version` table). **Fix:** §2.17 rewritten to sequential startup — main.rs doesn't spawn listeners until after migration + `AppMode::Ready` transition, so there's no concurrent writer to serialize against. `BEGIN IMMEDIATE` only applies where it actually works (SQL transactions on `pyramid_config_contributions`). `migration_marker` becomes a contribution; the supersession IS the schema-version bump.
  - **Root 29 — Phase scope enumeration impressionistic.** 55+ hits in `llm.rs`/`chain_executor.rs`/`dadbear_preview.rs` never named in plan; the old "~4-8 sites" shorthand was wrong and is now retired everywhere in this doc. **Fix:** Phase 0a-1 pre-flight REQUIRES authoritative consumer inventory artifact at `docs/plans/history/walker-v3-consumer-inventory.md` before any commit. Phase 1 LOC locks against inventory. §11 B-F9's alternative commit list retired; §6 body is canonical sequence. De-dup step added before CREATE INDEX (§5.3 step 7).
  - **Root 30 — "Everything is a contribution" still quietly violated for runtime state.** **Fix:** §2.18 rewritten. `app_mode` is in-memory (RwLock in AppState); `migration_marker` / `onboarding_state` / `node_identity_history` are contributions. `local_only` vs `operator_private` distinction named in §5.4.3: former never crosses node boundary; latter syncs to Wire authenticated-private.
  - **Root 31 — Lifecycle semantics for new state unmodeled.** New §5.6 covers rollback (snapshot includes `pyramid_config_contributions`; walker-introduced rows retracted on rollback), backup/restore (per-schema declared semantics), `use_chain_engine` current-default + intervention policy as the Phase 1 exit criterion (resolves PUNCHLIST P0-1 + P0-2 jointly), retraction asymmetry for `migration_marker` (downgrade refused outside rollback context).
  - **Root 32 — Plan-integrity skill failed flagship Check 9 on first use.** B-F10: "18 events" prose alongside 20-item enumeration. Check 9 implementation trusted the prose number. **Fix:** Check 9 rewritten — skill now counts enumeration cardinality first, then verifies prose matches. Checks 12 (invariant-tag coverage) + 13 (transaction-boundary compat) added per B-F5 to catch semantic contradictions the syntactic checks miss; Check 14 now extends this to contribution field-list parity so semantic drifts like `onboarding_state` cannot hide in later prose.
  - **Root 33 — Backward-compat + operator-UX.** `announce_protocol_version` gets `#[serde(default)]` returning 0 (pre-versioning). New `fleet_peer_version_skew` chronicle event surfaces mixed-version clusters. Settings DRAFT lane doesn't bind `supersedes_id` until commit time; ConfigSynced during draft lifetime marks `parent_changed: true` and shows merge prompt. §12 rewritten for rev 1.0 current state.
  - **Chronicle event registry authoritative at §5.4.6** — cycle-4 caught `dispatch_failed_policy_blocked` missing from the rev-1.0 list; rev 1.0.1 adds it → 22 events total. Plan-integrity Check 9 counts §5.4.6's list and verifies all prose "N events" claims match.
  - **Architectural convergence:** all accumulated runtime state is now contribution-native (3 new schemas) or in-memory (AppMode, BreakerState, ScopeCache, MarketSurfaceCache) with explicit justification per §2.18. No net-new SQL storage outside of existing migration patterns.
  - **Plan-integrity artifact** at `docs/plans/history/plan-integrity-rev0.9-to-rev1.0.md` — first artifact written BEFORE audit-history claim, per Check 11 discipline.
  - Ready for Cycle 4 audit (cycle 3 was not clean — rev 1.0's structural changes are substantial and warrant verification).
- **Rev 1.0.1 (2026-04-21):** Cycle 4 Stage 1 residuals absorbed (19 findings across 2 auditors). Both auditors independently confirmed residuals-only — no new structural roots. Meta-finding: prior plan-integrity skill runs were prose-level assertions, not structural execution — rev 1.0 shipped with 3 count-drift locations that Check 9 specifically existed to catch. Rev 1.0.1 fixes drift AND wires the infrastructure (invariant tags) Checks 12-13 need to actually fire.
  - Count drift (both A-F1 / B-F1 CRITICAL): 22 events authoritative in §5.4.6 (added `dispatch_failed_policy_blocked` per A-F2 / B-F2). Phase 0a-1 body defers; Phase 0a exit criteria defers; Phase 6 color-map ref updated.
  - Nested BEGIN IMMEDIATE (A-F3): `TransactionMode::{OwnTransaction, JoinAmbient}` on `supersede_config_contribution`; migration path uses `JoinAmbient`.
  - Phase 0a-1 commit reorder (A-F4 / B-F5): envelope writer as shim in commit 4 before activation; commit 5 activates validation + BEGIN IMMEDIATE + unique index atomically.
  - `use_chain_engine` explicit-false (A-F5): boot modal + `chain_engine_enable_ack` field on `onboarding_state`.
  - §8 onboarding residual (B-F3): struck "or pyramid_config.json".
  - Phase 0a exit criteria language (B-F4): updated to §2.17 sequential-boot.
  - Invariant tags (B-F6): `{invariant: config_contrib_active_unique}` + `{txn: pyramid_config_contributions}` at §2.16.1; `{invariant: scope_cache_single_writer}` at §2.16.2; `{invariant: app_mode_single_writer}` at §2.17.1. Checks 12-13 now have something to grep.
  - PUNCHLIST credits (B-F8): §7 adds P0-1/P0-2/P1-5/P2-8 rows.
  - `migration_unknown_providers_ack` (A-F6 / B-F7): folded into `onboarding_state.migration_acks`.
  - BreakerState rehydrate note (A-F7): §2.16.4 explicit.
  - Rollback vs migration_marker (A-F8): §5.6.1 re-supersedes to v2 via rollback-context bypass, not retraction.
  - §2.18 table gaps (B-F10) / generation-skill async (B-F9): residuals noted for Phase 0a-2 implementation.
  - Meta: I didn't run the skill literally before rev 1.0. Rev 1.0.1 artifact at `plan-integrity-rev1.0-to-rev1.0.1.md` is produced from actual list-counting and tag-grepping.
  - **Ready for:** Codex fresh-eyes audit (Adam's request) — independent third-party review of the full rev 1.0.1 plan + companion + integrity artifacts.

Planned cadence from here:
1. Rev 0.8 absorbs Cycle 2 Stage 2 findings (Roots 18-22).
2. Plan-integrity skill runs between every rev.
3. Optional Cycle 3 audit if Stage 2 Roots 19 (cross-boundary wire contracts) surface more substantial issues than rev 0.8 absorbs.
4. Adam GO → hand to implementation thread, Phase 0a first.

---

## 12. Picking this up cold (rewritten rev 1.0 — B-F9)

For a fresh agent picking up at rev 1.0:

1. **Read §Status + §11 rev-1.0 audit history entry FIRST.** The plan has been through 3 full audit cycles (6 rounds, ~125 findings, 33 structural roots absorbed). §11's most recent entry tells you what converged.
2. **Read §2 — focus on the compute-once spine:**
   - §2.1-2.8: resolution chain and scopes (the mechanism).
   - §2.9: DispatchDecision — the spine. Every downstream consumer reads from this.
   - §2.17-2.18: boot and state — sequential startup, in-memory AppMode, contribution-native for everything else.
3. **Read §3 parameter catalog** — the declarable surface.
4. **Read §5.3 + §5.4 + §5.5 + §5.6** — migration, cross-boundary wire contracts, cross-subsystem cascades, lifecycle semantics.
5. **Phase 0a-1 pre-flight:** produce the consumer inventory artifact at `docs/plans/history/walker-v3-consumer-inventory.md` before any commit (Root 29 requirement).
6. **Memory files to read:**
   - `project_compute_market_purpose_brief.md` — why the market exists.
   - `feedback_everything_is_contribution.md` — default architectural move.
   - `feedback_smoke_before_big_merge.md` — pre-merge smoke discipline.
   - `feedback_systemic_before_fix.md` — root-cause posture.
7. **Grep the code:** `config.primary_model`, `pyramid_tier_routing`, `RouteEntry`, `call_model_unified_with_audit_and_ctx`, `resolve_ir_model`, `FleetDispatchRequest`. Internalize what's retiring.
8. **Integrity artifacts:** skim `docs/plans/history/plan-integrity-*.md` — these show what drift the plan has experienced and what the skill catches.
9. **Phase 0a-1 canonical commit order:** §6 Phase 0a-1 body. Start at commit 1 (arc_swap + chronicle consts).

Estimated cold-start → Phase 0a-1 commit 1: 3-5 hours (read + consumer inventory + first trivial commit).

---

**End of plan — current rev at top (§Status).**

Originally written 2026-04-21. Cumulatively audited through 2 full cycles (4 audit rounds, 71+ findings, 17+ structural roots). Plan-integrity skill enforces self-consistency between revs.
