# Walker Provider Configs + Slot Policy (v3)

**Date:** 2026-04-21
**Status:** DRAFT (rev 0.4) — product-owner Q&A absorbed (Q1–Q4). Pending Stage 1 informed audit, Stage 2 discovery audit, Adam GO.
**Rev:** 0.4
**Supersedes:** `inference-routing-v2-model-aware-config.md` (retired); rev 0.1 (six-API model, obsolete); rev 0.2 (resolver-chain reframe, this doc continues it).
**Author context:** planning thread, 2026-04-21. Rev 0.1 modeled this as six schemas with field lists. Rev 0.2 reframed as ONE resolver over a scope chain with schemas as thin value carriers. Rev 0.3 absorbs findings F1–F11 from `walker-v3-yaml-drafts.md` — shape-per-scope for `model_list`, `Option`-typed accessors replacing sentinels, `per_provider` block on slot-policy, provider readiness gates as a named layer parallel to the resolver, tier-set as union of provider `model_list` keys.

---

## 1. TL;DR

All walker-behavioral parameters resolve through a single chain:

```
Slot × Provider-entry → Slot → Call-order × Provider-entry →
    Provider-type → System default (bundled floor)
```

Most-specific → least-specific. First non-None wins. Any parameter — `max_budget_credits`, `patience_secs`, `breaker_reset`, `model_list`, `retry_count`, `sequential`, `bypass_pool`, anything future — resolves the same way. No special cases.

The schemas are value carriers at each scope, not API contracts with field lists. Adding a parameter = declare it at whatever scope, no schema change. Operator mental model: "declare where it matters; everything else cascades." Implementer mental model: one resolver function; every lookup routes through it.

Walker becomes a pure consumer. Tier names are arbitrary strings. All rev 2.1.1 market mechanics (saturation classification, deadline-driven await, provider serialization, X-Wire-Retry precedence) are preserved unchanged — they now read their parameters through the resolver instead of from hardcoded Rust.

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

### 2.6 Provider readiness gates — layer parallel to the resolver

Per F7: not every provider in call_order can dispatch at every moment. Before the resolver runs on a (slot, provider_type) pair, walker consults a **readiness gate** per provider_type. Gates are pure checks, no side effects, and live in the same module as the provider dispatcher they gate.

| Provider type | Readiness gate |
|---|---|
| `local` | `overrides.active == true` AND last Ollama probe within `ollama_probe_interval_secs × 2` AND probe reported online |
| `openrouter` | Credential `overrides.openrouter_credential_ref` resolves in the credential store AND `overrides.active != false` |
| `market` | Operator has ≥1 credit OR market contribution declares deferred-settlement participation AND `overrides.active != false` |
| `fleet` | At least one peer in `FleetRoster` with `last_seen_at` younger than `fleet_peer_min_staleness_secs` AND `overrides.active != false` AND (if tier resolved a model_list) a peer probe or last-announce confirms at least one listed model — **peer model inventory is node-domain (Q1), populated by node-to-node announce or on-demand probe; Wire's heartbeat carries only `{node_id, handle_path, tunnel_url}`** |

Walker's outer loop over call_order:

```
for provider_type in effective_call_order(slot):
    if !readiness_gate(provider_type): emit `provider_not_ready`; continue
    model_list = resolve_model_list(slot, provider_type)
    if model_list.is_none(): emit `tier_unresolved`; continue
    dispatch(...)   // success -> break; retryable failure -> continue
```

Gates are NOT resolver parameters; they read provider-specific state (credential store, probe cache, peer roster, credit balance). Putting them in the resolver would conflate static policy with live runtime state.

### 2.7 Scope-3 keying — by provider_type, not position

Per F11: `walker_call_order.order` is a list for ordering, but per-entry overrides are keyed on `provider_type`, not list index. If the operator reorders market and local, their scope-3 overrides travel with the provider_type. The resolver's `scope_call_order_provider(provider_type)` reflects this.

Implementation-wise: load `walker_call_order.order` as a `Vec<OrderEntry>` (preserving order) AND build a `HashMap<provider_type, OrderEntry>` (for scope lookup). Two views, one source of truth.

### 2.8 Tier names are self-documenting

Per F6: there is no `walker_tiers` contribution and no canonical tier enumeration. The set of known tiers IS the union of `model_list` keys across all active provider configs (scope 3 and scope 4). The Settings UI reads that union as its autocomplete / validation source for chain-YAML tier references. Runtime `tier_unresolved` chronicle event fires if a chain references a tier no provider declares.

Consequence: operators can introduce new tier strings by editing any provider's `model_list` — no schema change, no registry. Typos remain a runtime concern; Phase 0's `test_bundled_tier_coverage` catches bundled regressions, and Settings UI catches operator typos before save.

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
| `ollama_base_url` | `String` | Local Ollama endpoint. | `"http://localhost:11434/v1"` |
| `ollama_probe_interval_secs` | `u64` | How often local provider config probes `/api/tags`. | `300` |
| `openrouter_credential_ref` | `String` | Credential store key for OR API key. | `"OPENROUTER_KEY"` |

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

### 5.2 Stays

- All rev 2.1.1 market mechanics (saturation classification, `X-Wire-Retry` header precedence, `AllOffersSaturatedDetail` deserialization, deadline-driven `/fill` await, provider-side engine serialization semaphore).
- Chain YAML tier references (opaque strings, resolved via the resolver chain at dispatch time).
- Wire compute-market API (still matches on `model_id`; market provider config picks the ID from its `model_list` to `/quote` against).
- Existing chronicle event types + `market_backoff_waiting` from rev 2.1.1.
- Contribution supersession + `schema_registry` dispatch + ConfigSynced event flow — all unchanged.

### 5.3 One-time migration on upgrade

On first boot after v3 ships, a migration pass runs once:

1. Read `pyramid_tier_routing` rows. Every row has `provider_id = 'openrouter'` (verified on Adam's DB; operators with other rows are zero today). Translate into a single `walker_provider_openrouter` contribution with `overrides.model_list = {tier: [model]}` per row. Mark migration complete in a `_migration_marker` sentinel.
2. Read `config.primary_model` / `fallback_model_{1,2}`. Fold into that contribution's `model_list["mid"]` / `["high"]` / `["max"]` as tail fallbacks if not already present.
3. Read `dispatch_policy.routing_rules.route_to`. Translate into a single `walker_call_order` contribution with the type sequence. Carry forward `max_budget_credits` as scope-3 overrides for each entry.
4. `pricing_json` from tier_routing → lives on offer rows on the Wire side; node-side has `walker_provider_openrouter.overrides.pricing_ref` pointing at the credential / pricing contribution if the operator needs it for cost reporting. (Minor — covered in the `pricing_json` Q&A item resolution below.)
5. `walker_slot_policy` starts empty (no overrides until operator declares). Resolver cleanly falls through to call-order for all tiers.

Legacy tables stay readable (not dropped) for one rev so old logs and debugging still work; dropped in a follow-up.

---

## 6. Phased implementation

Phases ship independently. Walker's dispatch path gets a new arm per phase as each provider-type's resolver integration lands.

LOC estimates corrected per audit findings — `primary_model` is ~93 source-site occurrences across 16 files (not 25), Phase 6 UI realistically 1200–1800 LOC (not 600), Phase 0 bumped by ~200 LOC for six generation skills + six schema annotations. Total revised: ~3700–4700 LOC, 9–12 sessions.

### Phase 0 — Resolver + schema groundwork (~600 LOC, bumped)

- `walker_resolver.rs` with scope-chain walker + typed accessors + SYSTEM_DEFAULTS table.
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

Each skill ships as a bundled contribution at Phase 0. Without it, the Create tab card for that schema is dead text. WITH it, operator intent → working YAML → live walker behavior, with no Rust change.

### Phase 1 — Openrouter provider config + primary_model refactor (~700–900 LOC, bumped from rev 0.1)

- Implement `walker_provider_openrouter` consumption in the walker's dispatch path.
- Migrate ~93 source-site occurrences of `primary_model` / `fallback_model_{1,2}` / related. Each site now reads through the typed accessor `resolve_model_list(slot, "openrouter")`.
- `config.primary_model` stays as struct field with `#[deprecated]` warning. Struct removal in a follow-up.
- Bundled seed: Adam's canonical distribution (mercury-2 / grok-4.20-beta / qwen-flash / minimax-m2.7 / grok-4.1-fast).
- Legacy-coexistence contract: walker reads `walker_provider_openrouter` first; if the active contribution is absent (fresh install pre-migration, zero state), falls back to legacy `config.primary_model`. Removed in Phase 5.
- Tests: tier resolution, intra-type fallback on rate-limit (step through model_list), deprecated-field warning emission, chain-YAML tier coverage assertion.

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

### Phase 4 — Fleet provider config (~300 LOC)

- `walker_provider_fleet` consumption.
- Peer-selection parameters (`fleet_peer_min_staleness_secs`, `fleet_prefer_cached`, tier→expected-model-loaded).
- Integrates with existing FleetRoster + fleet dispatch code; config layer informs peer selection.
- Tests: tier-to-peer resolution with multiple peers.

### Phase 5 — Call-order + slot-policy + per-build circuit breaker (~500 LOC)

- `walker_call_order` consumption (walker's route-entry loop walks call-order's `order` list).
- `walker_slot_policy` consumption (slot.order overrides call_order.order for that slot; slot.overrides shadows call-order's scope-3 values).
- **Per-build circuit breaker** — named implementation choice per audit feedback: `BuildHandle` does NOT have `build_id` today, so the breaker state lives in a parallel `Arc<RwLock<HashMap<(build_id, slot, provider_type), BreakerState>>>` (keyed on StepContext's `build_id`, which is the canonical per-call build identifier). `BreakerState = { tripped: bool, last_success_at: Option<Instant>, last_failure_at: Option<Instant> }`.
  - Check-then-act is non-atomic (one wasted market attempt per build trip under race is acceptable; documented).
  - `breaker_reset` resolution chain determines whether the breaker clears per-build, on probe, or after time elapsed.
- Remove Phase 1's legacy-coexistence fallback path (`config.primary_model` read removed from walker).
- Tests: breaker trip/reset under each `breaker_reset` mode; slot-policy overrides call-order; builds complete when breaker trips mid-build; concurrent breaker-update races don't corrupt state.

### Phase 6 — UI + onboarding (~1400–1800 LOC, realistic)

Audit noted `InferenceRoutingPanel.tsx` is already 894 lines; six configs + list-editing for model arrays + per-entry patience/breaker + onboarding wizard + live breaker visibility → honest range is 1400–1800 LOC, likely 2 sessions.

- Settings surface renders six configs + call-order + slot-policy via the generative-YAML-to-UI renderer.
- First-launch onboarding wizard (v2 plan's wizard carries forward, re-scoped to the resolver framing).
- Chronicle events for live breaker visibility (new: `breaker_tripped`, `breaker_skipped`, `config_superseded`, `tier_unresolved`).
- Probe-driven dropdowns for per-provider-type model suggestions.

### Total: ~3900–4700 LOC, 9–12 sessions

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

- **Tester smoke:** GPU-less tester installs app, asks question on small corpus. Bundled call-order `[market, local, openrouter, fleet]`. Market fires first. Network providers matching tester's market config serve via market; else local is empty → openrouter serves. Chronicle shows provider for each step.
- **Hybrid operator (Adam-shape):** laptop + BEHEM + OpenRouter key. Call-order `[market, local, openrouter, fleet]`. Extract slot market patience 15 min via slot-policy override. Market serves local-model offers when BEHEM is up; extract work routes to BEHEM via market. Synth-heavy slot-policy bypasses market (`order: [openrouter]`), goes straight to OR. When BEHEM down longer than 15 min, breaker trips; extract falls to local (empty) → openrouter until build completes.
- **Bridge operator:** third operator's market config publishes OR-slug offers using their own OR key. Adam's market config requests those slugs. Match. Bridge serves, pays OR, Adam pays bridge in credits. Wire market primitive unchanged.
- **Scope note (F8):** walker_* contributions are **requester-side only**. Provider-side publishing (bridge operator or Adam-as-market-provider) is a separate `compute_market_offer` contribution managed by Phase 2 code. "Extract via market to BEHEM" requires Adam to ALSO publish a market offer for BEHEM; the walker_* YAMLs alone express only the requester's intent to route through market.
- **Pillar conformance:** no Rust-hardcoded tier-to-model, no Rust-hardcoded retry/timeout/budget values, no Rust-hardcoded call order. All editable via contributions. Every walker-behavioral parameter resolves through the scope chain.
- **No regressions:** existing operator policies continue to work (migration §5.3); existing chain YAMLs continue to resolve (tier strings pass through unchanged); existing rev 2.1.1 market mechanics remain intact.
- **Coverage assertion:** `test_bundled_tier_coverage` passes — no bundled chain uses a tier string without provider-type coverage in bundled seeds.

---

## 9. What this plan does NOT do

- Capability-based market matching (`min_context_limit`, `quality_tier`, etc.). That's v4 and depends on Wire-side offer schema changes. v3 is string-matching via provider-type configs; capability-match is a future parameter type (e.g., `capability_requirement: CapabilityMatcher`) that'd plug into the resolver the same way as today's `model_list`.
- New provider types beyond local/fleet/openrouter/market. Those four are the universe. A fifth (e.g., a new bridge class) adds a new schema_type + resolver arm.
- Migration of chain YAML tier names. Existing names pass through unchanged.
- Wire-side changes. Entirely node-side.

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
