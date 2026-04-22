# Walker v3 — YAML drafts + findings

**Synced-to-rev:** 0.8 (integrity-pass-required banner per §2.13 item 7 of the plan doc).

**Purpose:** concrete seeds for the six walker_* contributions. Originally authored against rev 0.2; this file has been updated through successive revs alongside the plan. Findings section at the bottom is historical (what the drafting pass surfaced at rev 0.2); the plan's §11 audit history is the authoritative record of resolutions.

**Rev 0.6+ deltas applied to seeds in §2:**
- Removed `openrouter_credential_ref` from bundled `walker_provider_openrouter` — credential resolution is via shipped `pyramid_providers.api_key_ref` column (see plan §3 catalog struck-through row). Do NOT re-add this field in new seed authoring.
- Removed `OPENROUTER_CREDENTIAL_REF_DEFAULT` const from the SYSTEM_DEFAULTS block.
- Removed `source: bundled` from YAML bodies — the `source` column is set by the envelope writer at contribution creation time, NOT in the YAML body. Existing seeds in §2 that still show `source: bundled` are stale; strip on next regeneration.
- `walker_provider_market.overrides.active` now defaults `false` in bundled (Root 17 / A-F6); onboarding Page 4 flip-to-true is the consent record, not the bundled default.

**Three personas** the seeds have to cover:

- **Tester:** no GPU, no OR key, no Ollama. Only viable path = market → someone else's 5090.
- **Hybrid (Adam):** laptop + BEHEM + OR key. Extract via market (to BEHEM). Synth_heavy direct to OR.
- **Standard:** Ollama local + OR key, no market participation. Classic hybrid.

Tier strings used throughout: `extractor`, `mid`, `high`, `max` (chain YAMLs reference these).

---

## 1. SYSTEM_DEFAULTS (Rust — scope 5)

Authoritative floor. Every typed accessor names its default here.

```rust
// walker_resolver/defaults.rs
pub const PATIENCE_SECS_DEFAULT: u64 = 3600;
pub const PATIENCE_CLOCK_RESETS_PER_MODEL_DEFAULT: bool = false;
pub const BREAKER_RESET_DEFAULT: BreakerReset = BreakerReset::PerBuild;
pub const SEQUENTIAL_DEFAULT: bool = true;           // safest floor; provider-types override
pub const BYPASS_POOL_DEFAULT: bool = false;
pub const RETRY_HTTP_COUNT_DEFAULT: u32 = 3;
pub const RETRY_BACKOFF_BASE_SECS_DEFAULT: u64 = 2;
pub const DISPATCH_DEADLINE_GRACE_SECS_DEFAULT: u64 = 10;
pub const FLEET_PEER_MIN_STALENESS_SECS_DEFAULT: u64 = 300;
pub const FLEET_PREFER_CACHED_DEFAULT: bool = true;
pub const OLLAMA_BASE_URL_DEFAULT: &str = "http://localhost:11434/v1";
pub const OLLAMA_PROBE_INTERVAL_SECS_DEFAULT: u64 = 300;
// openrouter_credential_ref removed rev 0.6 — credential lookup uses pyramid_providers.api_key_ref

// max_budget_credits intentionally returns Option<i64> — see Finding F6.
// model_list intentionally returns Option<Vec<String>> — see Finding F5.
```

---

## 2. Bundled seeds (ship with the app, Day 1)

These are the `source: bundled` contributions every install boots with, until operator-authored or wire-pulled supersedes.

### 2.1 `walker_provider_openrouter` (bundled)

```yaml
schema_type: walker_provider_openrouter
version: 1
source: bundled
overrides:
  model_list:
    extractor: ["inception/mercury-2"]
    mid: ["inception/mercury-2"]
    high: ["qwen/qwen3.5-flash-02-23", "inception/mercury-2"]
    max: ["x-ai/grok-4.20-beta", "qwen/qwen3.5-flash-02-23"]
  sequential: false
  retry_http_count: 5
  # openrouter_credential_ref removed rev 0.6 — walker reads pyramid_providers.api_key_ref directly
```

> Note: per `project_model_defaults.md` the confirmed OpenRouter fallback is `inception/mercury-2`. Other slugs above are placeholders pending Adam confirmation in pre-flight Q&A.

### 2.2 `walker_provider_local` (bundled — declarative-disabled)

```yaml
schema_type: walker_provider_local
version: 1
source: bundled
overrides:
  ollama_base_url: "http://localhost:11434/v1"
  ollama_probe_interval_secs: 300
  sequential: true
  active: false   # <-- SEE FINDING F1: 'enabled' is not in the §3 parameter catalog
  model_list:
    extractor: []
    mid: []
    high: []
    max: []
```

Left empty on purpose: bundled install has no claim about what Ollama serves. The probe fills `model_list` **or** the operator does via the Settings UI. Either way, the fact of a `walker_provider_local` contribution existing signals "local path is visible in call_order"; `active: false` mutes it until explicitly turned on. (Presence-as-signal alone is not enough — see F1.)

### 2.3 `walker_provider_fleet` (bundled)

```yaml
schema_type: walker_provider_fleet
version: 1
source: bundled
overrides:
  fleet_peer_min_staleness_secs: 300
  fleet_prefer_cached: true
  model_list:
    # SEE FINDING F2: fleet model_list semantics differ from openrouter's.
    # Here it's "tier -> list of models we prefer peers to already have cached."
    extractor: ["llama3.2:3b", "inception/mercury-2"]
    mid: ["llama3.2:3b", "inception/mercury-2"]
    high: ["qwen2.5:32b", "qwen/qwen3.5-flash-02-23"]
    max: ["x-ai/grok-4.20-beta"]
```

### 2.4 `walker_provider_market` (bundled — pragmatic Day-1 seed per Q4)

```yaml
schema_type: walker_provider_market
version: 1
# source: bundled — set by envelope writer, NOT in YAML body (rev 0.6)
overrides:
  active: false  # Root 17 — onboarding Page 4 flip-to-true is the operator consent record
  model_list:
    # Q4 answer: prod today has 1 active offer (gemma4:26b). Seed is pragmatic — not empirical.
    # Covers common GPU classes; operators supersede as market diversifies.
    extractor: ["gemma4:26b", "llama3.1:8b"]
    mid: ["gemma4:26b", "qwen2.5:14b-instruct"]
    high: ["llama3.1:70b", "mistral-small-24b"]
    max: ["llama3.1:70b"]
  patience_secs: 900
  patience_clock_resets_per_model: false
  breaker_reset: "per_build"
  dispatch_deadline_grace_secs: 10
```

> Wire does NOT publish a canonical `walker_provider_market` — walker policy is node-domain per SYSTEM.md §1.6. No `wire_pulled` supersession path for this contribution type; the chain is `bundled` → `operator_authored` only.

### 2.5 `walker_call_order` (bundled — rev 0.3 shape)

```yaml
schema_type: walker_call_order
version: 1
source: bundled
order: [market, local, openrouter, fleet]
overrides_by_provider: {}
```

### 2.6 `walker_slot_policy` (bundled — empty)

```yaml
schema_type: walker_slot_policy
version: 1
source: bundled
slots: {}
```

Empty by design. Operator populates when they want slot-specific routing.

---

## 3. Persona overlays (operator-authored supersessions)

### 3.1 Tester (GPU-less, no OR key)

**No overlay needed.** Bundled seeds work as-is. Market is first in call_order; `walker_provider_market` has models; local has `active: false`; openrouter has no credential → skip on dispatch; fleet has no peers → skip.

The only operator action in onboarding: confirm handle + accept default market participation. Walker routes everything through market to whoever's publishing offers.

**F-finding:** "openrouter skips because credential_ref resolves to missing key" is not actually specified in the plan. The provider dispatch code has to KNOW to skip when the credential is absent. That's a provider-implementation detail, not a resolver detail — but it's behavior the YAML drafts implicitly depend on. See F7.

### 3.2 Hybrid (Adam — BEHEM + OR key + market)

**`walker_provider_local` supersession:**

```yaml
schema_type: walker_provider_local
version: 1
source: operator_authored
supersedes: <bundled uuid>
overrides:
  ollama_base_url: "http://behem.local:11434/v1"
  sequential: true
  active: true
  model_list:
    extractor: ["llama3.2:3b"]
    mid: ["qwen2.5:14b"]
    high: ["qwen2.5:32b"]
    max: ["qwen2.5:72b"]
```

**`walker_slot_policy` supersession (rev 0.3 shape):**

```yaml
schema_type: walker_slot_policy
version: 1
source: operator_authored
supersedes: <bundled uuid>
slots:
  extract:
    overrides:
      patience_secs: 900          # scope 2: applies to every provider in this slot
    per_provider:                  # scope 1: no reordering implied
      market:
        breaker_reset: "probe_based"
  synth_heavy:
    order: [openrouter]            # scope 2: bypass market+local+fleet for synth_heavy
```

Walker behavior that falls out:
- `extract` slot: market first (patience 900s to find BEHEM via market), then local (direct BEHEM if market idle), then OR, then fleet.
- `synth_heavy` slot: OR only. Market never touched.
- All other slots: bundled call_order.

**F-finding:** Adam's "extract via market to BEHEM" only works if Adam himself is running a market provider publishing BEHEM's capabilities. That's a whole separate contribution (the provider-side `compute_market_offer` contribution), not a walker-side thing. Worth calling out in acceptance criteria that these YAMLs alone don't wire up the bridge — F8.

### 3.3 Standard (Ollama + OR, no market)

**`walker_call_order` supersession (rev 0.3 shape):**

```yaml
schema_type: walker_call_order
version: 1
source: operator_authored
supersedes: <bundled uuid>
order: [local, openrouter, fleet]
overrides_by_provider: {}
```

**`walker_provider_local` supersession:** enabled + populated (same shape as §3.2, different models).

Standard operator doesn't interact with market at all. Market provider config sits bundled but unreferenced.

---

## 4. Findings — what the drafting surfaced

### F1. `enabled` is a parameter but not in the §3 catalog.

Plan §6 Phase 2 casually introduces `overrides.active: false`. It's not in the parameter catalog. Either add it with `SYSTEM_DEFAULT = true` (or `false` for local, to force opt-in), or replace it with "absence of contribution = disabled." But absence loses config you want to preserve for later re-enabling. → **Add `enabled: bool` to the catalog. Default `true` for OR/market/fleet, `false` for local.**

### F2. `model_list` has scope-dependent shape.

At scope 4 (provider-type), `model_list` is `{tier: [models]}` — a map. At scopes 1–2 (slot or slot×entry), the enclosing scope IS the tier, so `model_list` would naturally be just `[models]`. The plan pretends `model_list` is one parameter routed through one resolver, but the shape differs by scope.

Options:
- **(a)** Typed accessor handles both: checks scopes 1–2 for a flat list first, then scopes 3–4 for a tiered map and indexes `[slot]`. Cost: the accessor knows about the shape split, not the resolver.
- **(b)** Split into two keys: `model_list` (flat, scopes 1–2) and `model_list_by_tier` (map, scope 4). Cost: operators have to know which to use.
- **(c)** Always a map, slot-scope map just has one key (the slot). Cost: redundant.

Recommend (a). It keeps the resolver uniform and the typed accessor is the only place that knows about the split — which is honest, because model_list is intrinsically tier-aware while other params aren't.

### F3. Fleet's `model_list` semantics differ from OR's.

For OpenRouter the list is "ordered models walker will try." For fleet it's "models we'd prefer peers to already have cached." The resolver treats them the same way (returns the list); the provider dispatch code interprets differently. That's fine and probably correct, but the plan doesn't call out the semantic divergence, and an auditor will. → **Add a "semantics per provider type" table to §3 or §4 making this explicit.**

### F4. Scope-1 overrides force operator to re-declare the full order.

Plan §4.3 puts per-(slot×entry) overrides inside `slots[tier].order[N].overrides`. Meaning: if an operator wants to tweak one param for market-in-extract-slot without reordering, they must declare the whole `order` array for that slot. That's awkward — it conflates "I'm reordering providers for this slot" with "I'm tweaking a param for one provider in this slot."

Cleaner shape:

```yaml
slots:
  extract:
    overrides:             # scope 2 — applies to all providers in extract
      patience_secs: 900
    per_provider:          # scope 1 — no reordering implied
      market:
        overrides:
          breaker_reset: "probe_based"
    order:                 # optional — only present when operator wants slot-specific ordering
      - provider_type: market
      - provider_type: local
```

This separates "reorder for this slot" (`order`) from "override scoped to one provider in this slot" (`per_provider`). Rev 0.3 should adopt this.

### F5. `max_budget_credits` sentinel is a smell.

Plan §3 uses `(1<<53)-1` as NO_BUDGET_CAP sentinel. Sentinels leak a None-ish value into the typed domain — every consumer has to check for the sentinel AND the parameter. Cleaner: typed accessor returns `Option<i64>`, `None` means "no cap," consumers branch on `Option`. Same pattern for `model_list` — `None` = "this provider isn't claiming this tier" vs `Some([])` = "declared empty (skip)."

### F6. Tier names are implicit-typed strings with no declaration site.

Chain YAMLs reference `mid`, `high`, etc. Provider configs declare `model_list.mid`. There's no canonical list of tiers. Phase 0's `test_bundled_tier_coverage` catches bundled regressions but operator-edited chains can reference typos silently. Three options:

- **(a)** A `walker_tiers` contribution enumerates known tier strings; Settings UI validates chain YAMLs against it; runtime `tier_unresolved` chronicle event on unknown references.
- **(b)** No enumeration, purely structural (operator's problem if they typo). Runtime chronicle event is the only feedback.
- **(c)** Tiers emerge from the union of all provider configs' `model_list` keys (self-documenting). UI reads that union as its autocomplete source.

Recommend (c). It composes naturally — each provider declares what tiers it covers, and the "known tiers" set is the union. No new schema. No extra declaration site. The `tier_unresolved` event still fires at runtime for typos in chain YAMLs.

### F7. "Skip if credential absent" is an unspecified provider-dispatch behavior.

The GPU-less tester persona depends on `walker_provider_openrouter` skipping itself when `OPENROUTER_KEY` isn't in the credential store. The plan doesn't specify this anywhere — it assumes it. Every provider needs a "can I actually dispatch right now?" gate that runs BEFORE the resolver starts looking up params. Candidates:

- local: Ollama probe says online + `enabled: true`
- openrouter: credential resolvable
- market: has credits OR has participation policy allowing deferred settlement
- fleet: at least one peer meets staleness cutoff

→ **Add a "provider readiness gate" section to §2 or §6, parallel to the resolver. Walker iterates call_order, skipping providers whose gate is false, before running the resolver on the survivors.**

### F8. Market-requester YAMLs don't wire the bridge-operator path.

For Adam's hybrid setup to route extract-to-BEHEM via market, Adam ALSO has to be a market PROVIDER publishing BEHEM's offers. That's a `compute_market_offer` contribution, not a walker_* contribution. The walker-side YAMLs above describe ONLY the requester side. The acceptance criterion in plan §8 ("Adam's config requests those slugs. Match.") glosses this. → **Add an explicit note: walker_* configs are requester-side only; provider-side offers are separate contributions managed by Phase 2-shipped code.**

### F9. Source precedence (`bundled < wire_pulled < operator_authored`) needs to be explicit on every contribution.

The drafts include `source: bundled` / `operator_authored` fields. The plan §7 mentions the source chain (in Audit Q12) but doesn't say it's a field on the contribution or how it's set. Is it part of the existing contribution envelope (implied by supersession chain depth) or a new field? → **Confirm whether existing contribution schema already carries `source` or whether walker_* needs it added.**

### F10. Slot-policy `order` fully replaces call-order; no partial override.

Plan §4.3 says slot-policy's order, if present, replaces call-order entirely. So "try market first for synth_heavy but otherwise keep the default order" isn't expressible without restating the whole list. For 4 provider types it's not a burden, but if the space grows it becomes one. Probably fine for now — flag for future.

### F11. Resolver ignores the call-order position semantics except for scope-3 lookup.

Scope 3 (`walker_call_order.entries[N].overrides`) keys overrides by position in the call-order. But position has no meaning beyond "nth in the default ordering" — if operator swaps market and local in call_order, the scope-3 overrides travel with their provider_type, not their position. The plan's `scope_call_order_entry(call_order_idx)` function signature suggests position-keyed, but the actual semantic is provider_type-keyed within call-order. Minor, but the naming will confuse implementers. → **Key scope-3 overrides by `provider_type`, not by position. The YAML in §4.2 of rev 0.2 already does this; the resolver signature in §2.2 should match.**

### F12. No "break out of call_order early on success" parameter — but also no need for one?

Walker semantics implied throughout the plan: iterate call_order, first provider that successfully dispatches wins, done. There's no parameter for "after success, retry another provider for cross-check" — which is fine, that's a v5 concern (quality challenges). Flagging only because I was expecting one and didn't find it. Not an issue; just confirming the absence is intentional.

---

## 5. Net effect on rev 0.3

Incorporating F1–F11 into rev 0.3:

- **§3 catalog:** add `enabled`, clarify `model_list` shape-per-scope, clarify fleet vs OR semantics, switch `max_budget_credits` and `model_list` to `Option`-typed.
- **§4.3 slot-policy shape:** adopt `per_provider` block separate from `order`.
- **§2.1 scope 3 keying:** by provider_type, not position.
- **New §2.6:** "Provider readiness gates" — what each provider checks before resolver engages.
- **New §3 sub-note:** tier names are self-documenting via the union of provider `model_list` keys; no separate declaration.
- **§5.3 migration:** confirm `source` field provenance on existing contribution envelope.
- **§8 acceptance criteria:** explicit note that walker_* is requester-side; bridge-operator setup is provider-side contributions.

None of these overturn the resolver-chain reframe. They clarify and tighten it. Draft should absorb these and then Stage 1 audit can hit a more-settled target.

---

**End of drafts. Written 2026-04-21 by YAML-drafting pass against rev 0.2 before Stage 1 audit.**
