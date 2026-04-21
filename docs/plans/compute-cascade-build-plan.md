# Unified Dispatch Walker + Inference Routing Panel — Build Plan

**Date:** 2026-04-20
**Author:** Claude (agent-wire-node upstairs mac)
**Status:** 🛑 **SUPERSEDED** — Wire-side quote primitive (rev 2.1, `GoodNewsEveryone/docs/plans/compute-market-quote-primitive-spec-2026-04-20.md`) shipped a substantially bigger cross-repo resolution. The walker-does-all-the-work architecture below is no longer the path. A new node-side plan will land against the Wire rev 2.1 shape once Wire ship-notice arrives.
**Rev:** 0.5 (Path A: walker fold with concurrency uniformity; scope honest after 5 audit passes)

> **Design-history preservation note.** This plan went through 5 revisions + multiple audit cycles as the correct architectural shape evolved. It is kept in the repo as a design artifact rather than the live build spec. The key lessons landing in the post-ship retro: (a) audit hygiene — never taint auditor prompts with prior-stage summaries or verdict nudges; (b) minimalism — find the existing primitive and extend one enum value rather than inventing a new one; (c) scope estimation — actual source-line counts of the blocks being consolidated matter and vary ~3-5× from function-signature reads; (d) cross-repo symptom-to-systemic — when node-side is fabricating signals the other repo has direct access to, the right fix is usually the other-repo API enhancement, not more node-side derivation. The Wire rev 2.1 spec dissolves ~60% of what this plan tries to do on the node side.

---

## One-paragraph statement

Today's dispatcher has three hardcoded phases (fleet pre-loop → market pre-loop → pool permit-acquisition) that each handle capacity and failure differently. This plan replaces the phase model with a single walker over `route.providers` where every entry obeys the same contract: (a) check runtime gates, (b) try to acquire capacity, (c) on saturation emit a chronicle event and advance, (d) on success dispatch and return, (e) on retryable failure advance, (f) on terminal failure return. Fleet, market, OpenRouter, and local Ollama all become route entries in one ordered list. Operator-facing: a Settings panel over `dispatch_policy.routing_rules[0].route_to` with drag-reorder + enable/disable + per-route config. Deployment: fresh-install only (single operator wiping the DB); no rev-0.6.1 migration preservation needed.

---

## What the walker does (concrete)

Given `ResolvedRoute { providers: Vec<RouteEntry>, matched_rule_name, ... }` from `dispatch_policy::resolve_route`:

```
// Pseudocode — actual Rust in src-tauri/src/pyramid/llm.rs
for entry in route.providers {
    // 1. Runtime gates (tunnel/policy/ctx for fleet+market; credentials for cloud)
    if !route_runtime_gates_pass(&entry, &ctx) {
        emit_chronicle("network_route_skipped", { reason: "runtime_gate" });
        continue;
    }

    // 2. Capacity acquire (per-entry, type-specific)
    let permit = match try_acquire_capacity(&entry, &ctx).await {
        Ok(p) => p,
        Err(Saturated) => {
            emit_chronicle("network_route_saturated", { entry });
            continue;
        }
        Err(Unavailable) => {
            emit_chronicle("network_route_unavailable", { entry, reason });
            continue;
        }
    };

    // 3. Dispatch via type-specific path (fleet/market/http)
    match dispatch_entry(&entry, permit, &ctx, &messages).await {
        Ok(response) => {
            cache_write(canonical_model, &ctx.prompt_hash, &response).await;
            emit_chronicle("cascade_resolved", { entry, latency_ms: response.latency });
            return Ok(response);
        }
        Err(Retryable(reason)) => {
            emit_chronicle("network_route_retryable_fail", { entry, reason });
            continue;  // advance to next entry
        }
        Err(Terminal(reason)) => {
            emit_chronicle("network_route_terminal_fail", { entry, reason });
            return Err(reason);  // don't advance — terminal means the call itself is doomed
        }
    }
}
return Err(NoViableRoute);
```

Every entry type implements the same contract:

- **Pool provider** (openrouter, ollama-local, any registered provider row):
  - gates: provider exists in registry + has credentials resolved
  - acquire: existing `pools.acquire(&provider_id)` semaphore
  - dispatch: existing HTTP call with retry loop (moved inside this branch)
- **Fleet** (`provider_id == "fleet"`):
  - gates: `policy.allow_fleet_dispatch` + fleet_dispatch_context present + fleet_roster nonempty + tunnel connected
  - acquire: `fleet_roster.find_peer_for_rule(route.matched_rule_name)` — returns `None` if all peers at max queue depth → saturated
  - dispatch: existing fleet-JWT-fetch + fleet_dispatch_by_rule + oneshot await (moved inside this branch)
- **Market** (`provider_id == "market"`):
  - gates: `policy.allow_market_dispatch` + compute_market_context present + tunnel connected + model_tier_eligible
  - acquire: read MarketSurfaceCache for this model; if `active_offers == 0` → unavailable; if every offer's `queue.depth >= queue.max_depth` → saturated; otherwise return a logical permit
  - dispatch: premium filter computes `max_budget` from cache's min rates; existing `compute_requester::dispatch_market` with computed budget; pending_jobs oneshot (unchanged)

**What advance-on-saturation delivers:**

`for_each_concurrent(8)` at the chain level → 8 parallel walker invocations → each tries `route_to[0]` first. If 8 calls all find market-offer full, all 8 advance to `route_to[1]` (openrouter), acquire 8 openrouter semaphore permits in parallel, dispatch. Works exactly like operator expects. No phase-level bottleneck.

---

## Verified structure of today's code (from audit)

Line references from actual source read during rev-0.4 audit:

- `src-tauri/src/pyramid/llm.rs::call_model_unified_with_audit_and_ctx` is the entry point (~line 1123).
- **Phase A fleet block**: ~lines 1213-1620 (~475 LOC). Reads `route.matched_rule_name`, fleet_roster lookup, JWT fetch, `fleet_dispatch_by_rule`, awaits oneshot callback with timeout.
- **`fleet_filter`**: line 1694. Strips `provider_id == "fleet"` entries from `resolved_route.providers` before the permit-acquisition loop (prevents the loop from trying to acquire a pool permit for the fleet sentinel).
- **Phase B market block**: ~lines 1697-2047 (~350 LOC). Gated by `should_try_market(...)` at line 167. Calls `compute_requester::dispatch_market` with `MarketInferenceRequest { max_budget: (1i64 << 53) - 1, input_tokens: 0, ... }` — today sends JS-safe sentinel as budget, zero input tokens.
- **compute_queue enqueue**: ~lines 2049-2163. Fires if `route.providers.iter().any(|entry| entry.is_local)` and we're not already in a replay context.
- **Permit-acquisition loop**: ~lines 2208-2234. Iterates `route.providers`, calls `pools.acquire(&entry.provider_id)`, breaks on first success. No `dispatch_through_pool` function — that was a fabrication in rev 0.4.
- **Single HTTP retry block**: ~lines 2312-2740 (~430 LOC). Runs ONCE against the acquired-permit provider. Has its own retry loop with exponential backoff, context-exceeded cascade, model override, escalation at line 2249-2253.

What the walker rewrites: all five blocks above collapse into ONE iteration body where each arm (fleet, market, pool-provider) is a branch.

What survives unchanged: fleet's peer-lookup + JWT + oneshot mechanism; market's `dispatch_market` + pending_jobs; the HTTP retry block (moved inside the pool-provider branch of the walker); audit row lifecycle; cache probe.

What gets removed: the `fleet_filter`, the standalone `should_try_market` as a gate (its checks move into the market-entry runtime gate), the hardcoded phase ordering.

---

## Wire market-surface response shape (verified)

From `GoodNewsEveryone/src/lib/server/market-surface-cache.ts:134-157`. Node-side Rust types to author:

```rust
// src-tauri/src/pyramid/market_surface_types.rs (NEW file)

#[derive(Debug, Clone, Deserialize)]
pub struct MarketSurfaceResponse {
    pub models: Vec<ModelSurface>,
    pub generated_at: String,   // ISO 8601
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSurface {
    pub model_id: String,
    pub active_offers: u32,
    pub price: PriceAggregate,
    pub queue: QueueAggregate,
    pub providers: Vec<ProviderSummary>,   // anonymized
    pub performance: PerformanceAggregate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceAggregate {
    pub rate_per_m_input: MinMedianMax,
    pub rate_per_m_output: MinMedianMax,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MinMedianMax {
    pub min: u64,
    pub median: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueueAggregate {
    pub depth: MinMedianMax,      // per-offer queue depth
    pub max_depth: MinMedianMax,  // per-offer queue capacity
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSummary { /* anonymized per D6; not used by dispatcher */ }

#[derive(Debug, Clone, Deserialize)]
pub struct PerformanceAggregate { /* TPS etc; not used by v1 dispatcher */ }
```

Field paths the walker cares about: `models[].price.rate_per_m_input.min`, `models[].price.rate_per_m_output.min`, `models[].queue.depth.min` (lowest-depth offer available), `models[].active_offers` (zero → market unavailable for this model).

**Units reality check:** rates are credits-per-million-tokens. Wire's `match_compute_job` expects `max_budget` in total-call-credits. Premium filter math:

```rust
let min_rate_in_per_m  = surface.price.rate_per_m_input.min;   // credits/M
let min_rate_out_per_m = surface.price.rate_per_m_output.min;  // credits/M
let est_out_tokens     = effective_max_tokens.min(8192);       // node-side cap; can revisit
let est_in_tokens      = ctx.input_tokens_estimate;            // already computed at llm.rs:2277
let floor_cost         =
    (est_in_tokens * min_rate_in_per_m / 1_000_000)
    + (est_out_tokens * min_rate_out_per_m / 1_000_000);
let max_budget         = floor_cost * (100 + premium_pct) / 100;
```

Rate-cap approximation, not total-cost cap. Wire's per-offer `reservation_fee` isn't in public `/market-surface`, so if matched offer's reservation_fee pushes total past `max_budget`, Wire's own `match_compute_job` check rejects with 409 → walker advances. Documented tradeoff.

---

## Scope

### In scope

**Wave 0 — prerequisites (~300 LOC, 1 agent)**

1. **Bundle `dispatch_policy-default-v1` contribution** into `src-tauri/assets/bundled_contributions.json`. Fresh-install seed. Initial `routing_rules[0]`: `{ name: "default", match_config: {}, route_to: [{provider_id: "market"}, {provider_id: "fleet"}, {provider_id: "openrouter"}, {provider_id: "ollama-local", is_local: true}] }`. Matches current behavior (fleet + market both tried, then cloud + local as pool fallbacks).
2. **Write `sync_dispatch_policy_to_operational`** in `config_contributions.rs` (new fn). Mirrors `sync_chain_defaults_to_operational` pattern at `wire_migration.rs:1439-1440`. Direct call to `db::upsert_dispatch_policy` — bypasses the `ConfigSynced` event bus which isn't wired during boot migration (per existing comment at `wire_migration.rs:1419-1440`). Invoked from `walk_bundled_contributions_manifest` post-bundled-insert.
3. **Wire boot hydration**: in `main.rs` startup, after bundled sync completes, read `pyramid_dispatch_policy` table and populate `LlmConfig.dispatch_policy`. Otherwise `cfg.dispatch_policy = None` until first user save, and the walker has nothing to iterate.
4. **Fix Settings.tsx `ComputeParticipationPolicy` TS interface**. Current (line 47-59 of Settings.tsx) omits the three `market_dispatch_{threshold_queue_depth, max_wait_ms, eager}` fields — round-trip silently defaults them on every save. Adding `market_dispatch_premium_pct` without fixing this compounds the latent bug.
5. **Delete `call_model_via_registry`** at `llm.rs:3487-3660`. Zero prod callers verified; its doc comment ("preferred entry point for chain-executor callers") is a trap for any fresh agent.
6. **Grep audit for dispatch bypass paths**: `rg 'pools\.acquire\(' src-tauri/src/` + `rg 'client\.post' src-tauri/src/pyramid/` confirms `call_model_unified_with_audit_and_ctx` is the single gate. Document findings as inline comment.

**Wave 1A — Walker shell + pool entries (~600 LOC, 1 agent)**

7. **Author `MarketSurfaceResponse` types** in new `src-tauri/src/pyramid/market_surface_types.rs` per section above.
8. **Implement `MarketSurfaceCache`** with `Arc<RwLock<CacheData>>` on `PyramidState`. 60s TTL, `refresh_if_stale()` async method. Boot-warm via post-tunnel-connect hook in `main.rs`. Public-unauthed `/market-surface` endpoint — no auth token required (verify by reading Wire's route handler); if auth-required, strip auth and use public path.
9. **Introduce walker skeleton** in `llm.rs`. Replace the permit-acquisition loop (lines 2208-2234) with a full walker loop that iterates `route.providers` and dispatches per entry. For THIS wave, keep Phase A fleet and Phase B market as pre-loop blocks UNCHANGED — walker handles only pool-provider entries. This is a compile-compatible intermediate state where walker exists but only covers today's pool path. Audit + verifier on this commit.
10. **Move HTTP retry block (lines 2312-2740) into per-pool-entry dispatch**. Each pool entry now runs its own retry loop. Cache probe stays outside the walker loop (once per call, not per-entry). Audit row lifecycle: `insert_llm_audit_pending` stays at today's line 2172; exit updates with the winning entry's `provider_id`.
11. **Introduce `try_acquire_capacity(&entry)` abstraction.** For pool entries it wraps `pools.acquire(&entry.provider_id)`. Returns `Ok(Permit)` or `Err(AcquireError::{Saturated, NotRegistered, CredentialsMissing})`. Saturation emits `network_route_saturated` and advances.

**Wave 1B — Inline fleet into walker (~600 LOC, 1 agent, after 1A passes verifier)**

12. **Extract fleet dispatch (lines 1213-1620) into `dispatch_fleet_entry()`** new function. Takes the RouteEntry + CallCtx + resolved peer + permit. Returns `Result<Response, EntryError>`. Internal logic unchanged — JWT fetch, fleet_dispatch_by_rule, oneshot await with timeout, error classification.
13. **Walker branch**: when `entry.provider_id == "fleet"`:
    - Runtime gate: `policy.allow_fleet_dispatch && ctx.fleet_dispatch_present && ctx.tunnel_snap.connected && !route.matched_rule_name.is_empty()`
    - Acquire: `fleet_roster.find_peer_for_rule(&route.matched_rule_name)`. None → `Saturated` (all peers at max queue). Some(peer) → logical permit = peer handle.
    - Dispatch: `dispatch_fleet_entry(entry, ctx, peer, permit)`.
14. **Remove the `fleet_filter` at line 1694** and the entire pre-loop Phase A block (lines 1213-1620). Fleet is now an in-walker branch.
15. **Handle `skip_fleet_dispatch` recursion guard**: today at llm.rs:2087 the queue replay clones config without fleet state. Walker must respect this — if `ctx.skip_fleet_dispatch` is true, skip fleet entries (emit `network_route_skipped` reason=`fleet_replay_guard`).
16. **`resolve_local_for_rule` at `dispatch_policy.rs:238-253`** — filter out `provider_id == "fleet"` OR `"market"` (both now walker-sentinels, not real provider rows) when determining the local handler for incoming fleet jobs.

**Wave 1C — Inline market into walker + premium filter + MarketSurfaceCache integration (~800 LOC, 1 agent, after 1B passes verifier)**

17. **Extract market dispatch (lines 1697-2047) into `dispatch_market_entry()`** new function. Takes the RouteEntry + CallCtx + ResolvedMarketCapacity (from cache) + premium-capped max_budget. Internal: existing pending_jobs register + `compute_requester::dispatch_market` + oneshot await. Replace today's sentinel `max_budget: (1i64 << 53) - 1` at line 1834 with computed `max_budget` from premium filter. Replace today's `input_tokens: 0` at line 1835 with real `est_input_tokens` (already computed at line 2277).
18. **Walker branch**: when `entry.provider_id == "market"`:
    - Runtime gate: merges today's `should_try_market` at line 167-210. All 6 checks preserved.
    - Acquire: `MarketSurfaceCache::try_acquire_for_model(&tier_model)`. Returns:
      - `Ok(MarketCapacity { cheapest_offer_rates, min_queue_depth })` if any offer has `active_offers > 0 && queue.depth.min < queue.max_depth.min`.
      - `Err(Unavailable)` if `active_offers == 0` (no offers for this model).
      - `Err(Saturated)` if every offer is at queue capacity.
      - `Err(CacheCold)` on first boot call — falls back to sentinel max_budget + emits `network_rate_cap_unavailable`.
    - Dispatch: compute `max_budget` per premium filter, call `dispatch_market_entry`.
19. **Premium filter logic** per the unit-reality-check section above. If `floor_cost * (1 + premium_pct/100) < reservation_fee_min` (which we don't know client-side), Wire rejects; walker catches the 409 response and advances. Chronicle emits `network_rate_above_cap` or `network_rate_above_wire_budget` depending on which side rejected.
20. **Remove the pre-loop Phase B block (lines 1697-2047)** and the standalone `should_try_market` function if all its gates are now inside the walker's market runtime-gate check. Market is now an in-walker branch.
21. **Deprecate `market_dispatch_eager` + `market_dispatch_threshold_queue_depth`**: these fields exist because the pre-loop Phase B market had no per-call saturation signal and used LOCAL queue depth as a proxy. With walker + per-entry saturation via cache, both fields become dead code. Keep them on the struct for one rev (serde-compatible) but ignore in walker. Document deprecation in Wave 3 cleanup.

**Wave 2 — Policy field + Settings panel (~600 LOC, 1 agent, after Wave 1 passes wanderer)**

22. **Add `market_dispatch_premium_pct: u32` to `ComputeParticipationPolicy`** (default 50, via `#[serde(default)]`). Add to `EffectiveParticipationPolicy` projection. Update bundled schema_definition + schema_annotation + default seed rows in `bundled_contributions.json`.
23. **New component `src/components/settings/InferenceRoutingPanel.tsx`** (~500 LOC). Inserted above Ollama section in Settings.tsx.
    - **Routes subpanel**: edits active dispatch_policy's `routing_rules[0].route_to` via `pyramid_active_config_contribution` + `pyramid_supersede_config`. Up/down reorder buttons (drag-reorder deferred as polish). Each row: enable/disable + entry name + expandable sub-config.
    - **Market sub-config**: premium_pct slider (0-500, default 50) writes to `compute_participation_policy` via `pyramid_set_compute_participation_policy`. `max_wait_ms` readonly display.
    - **Tiers subpanel**: per-tier model editor. Autocomplete from MarketSurfaceCache (via new IPC `pyramid_market_models`), Ollama probe's `available_models`, OpenRouter catalog. Picker shows source → auto-fills `provider_id`. Warning banner if selected model's provider_id isn't in current route_to.
    - **Discovery subpanel**: read-only. Shows new `(provider_node_id, model_id)` tuples in MarketSurfaceCache since `localStorage.cascade_last_reviewed_at`.
24. **Local-Mode preset interaction**: when `compute_participation_policy.mode == "local"` (or whichever check today's Local Mode uses), Routes + Tiers subpanels render read-only with banner "Local Mode is active. Turn off Local Mode to edit routing." Premium_pct stays editable.
25. **Invisibility discipline UI copy**: "Max rate above cheapest (%)", "New network models since last review", never "offers" / "premium" / "market" in operator-facing labels.
26. **Debounce saves**: drag-reorder triggers save only on Apply button or mouse-up; tier-edit triggers save on blur. Prevents supersession flood on dispatch_policy which triggers provider_pools rebuild.

**Wave 3 — Cleanup + deprecation (~150 LOC, 1 agent)**

27. **Remove `market_dispatch_eager` + `market_dispatch_threshold_queue_depth`** from struct + schema_definition + annotation + seed. Wave 1C deprecated; Wave 3 removes. Any remaining reads emit a compile error, forcing cleanup of stragglers.
28. **Audit all string-match sites for `"fleet"` + add parallel `"market"` branches**: per audit, sites include `dispatch_policy.rs:238-253` (resolve_local_for_rule), `fleet_mps.rs:319` (serving-rule derivation), and a few others. Verified 6 sites total. Ensure each handles `"market"` correctly (usually filter it out the same way `"fleet"` is filtered).
29. **Chronicle event deprecation**: legacy event names that the walker replaces (if any) get marked deprecated in `compute_chronicle.rs`. Operational views continue to read from live events only.
30. **Pool permit release on walker-advance**: verify that when walker advances from one entry to next after a permit was acquired and dispatch failed, the permit releases cleanly (drop-guard). Write a test.

### Explicitly NOT in scope

- New `compute_cascade` schema_type or contribution family
- Replacing `resolve_tier` / `ResolvedTier` (walker is a peer of resolve_tier, not a replacement — resolve_tier still returns model name for a tier; walker uses it)
- Modifying `pyramid_tier_routing` or `pyramid_local_mode_state` schemas
- `min_reputation` filter (needs Wire-side reputation surface — separate plan)
- Canary-as-market protocol (separate plan)
- Per-tier route_order (single global route_to per rule in v1)
- `*market` wildcard (needs Wire-side "any-model-for-tag" endpoint)
- Tagged-enum `Mechanism::Fleet | Mechanism::Market | Provider(id)` refactor (flat-string debt acknowledged, deferred to future simplification pass)
- Wire-side contract changes (`/match` body shape identical to rev 0.6.1; `max_budget` carries premium-capped value)
- JSON Schema validator wiring (stays stubbed)
- `ui_preferences` new schema_type (localStorage is fine for banner / last-reviewed-at)
- Per-entry timeout overrides (walker uses today's global timeouts)
- Drag-reorder native HTML5 (up/down buttons for v1)
- Rev-0.6.1 migration preservation (DB is wiped before rollout; single operator)

---

## Deployment — fresh install, single operator

Adam wipes the DB; rollout is effectively a new install. No migration from rev-0.6.1 state. Bundled `dispatch_policy-default-v1` is the only dispatch_policy that exists post-wipe. No operator-state matrix, no banner, no "custom dispatch_policy preservation."

---

## Known tradeoffs (documented, accepted)

1. **Rate-cap approximation, not total-cost cap.** `reservation_fee` isn't on public `/market-surface`; walker's premium filter is a rate-only pre-filter, and Wire's own `match_compute_job` catches reservation_fee outliers via 409. Operator occasionally sees `network_rate_above_wire_budget` chronicle when a high-reservation-fee offer would have been the match.

2. **Flat-string `"fleet"` / `"market"` as `provider_id` sentinel values**. Extends existing overload rather than introducing a tagged enum. 6 string-match sites audited + updated in Wave 3. Future plan can refactor to tagged enum when scope allows.

3. **Output-token estimate `min(effective_max_tokens, 8192)`**. Under-conservative for chains that use <2K output (filter too permissive); over-conservative for chains that use >8K (filter may reject fine offers). Operator dials `premium_pct` based on observed behavior.

4. **Cold-start first market call** emits `network_rate_cap_unavailable` once (cache empty). Subsequent calls warm. Acceptable.

5. **Local-Mode active → routes/tiers subpanels read-only**. Prevents the "panel edits lost on Local Mode disable" bug (existing restore-pointer restores enable-time state verbatim). Operator must disable Local Mode to edit routes. Banner surfaces this.

6. **Up/down buttons instead of native drag-drop for v1**. ~150 LOC savings. Native drag-drop is a follow-up polish.

7. **Per-entry capacity signals are type-specific**: pool = semaphore; fleet = peer queue depth from roster; market = offer queue depth from cache. Different acquisition primitives, same contract (`Result<Permit, AcquireError>`). The walker doesn't care what kind of capacity it's checking.

8. **`for_each_concurrent(N)` at chain level + walker** = N parallel walkers, each trying route_to[0] first. If a route entry's capacity is K < N, K parallel calls succeed at that entry; the other N-K advance to route_to[1]. Expected and correct.

---

## Chronicle event vocabulary

New node-side events (`SOURCE_NETWORK`, no Wire-side CHECK migration needed):

| Event | Fires when | Metadata |
|---|---|---|
| `network_route_skipped` | Runtime gate rejected entry before acquire | `{entry, reason: "policy_disabled" \| "fleet_replay_guard" \| "tunnel_down" \| "tier_ineligible" \| ...}` |
| `network_route_saturated` | Capacity acquire returned saturated | `{entry, capacity_kind: "pool_semaphore" \| "fleet_peer_queue" \| "market_offer_queue"}` |
| `network_route_unavailable` | Acquire returned unavailable (e.g., no offers, cache cold, credentials missing) | `{entry, reason}` |
| `network_route_retryable_fail` | Dispatch failed with retryable classification; walker advanced | `{entry, reason, status_code?}` |
| `network_route_terminal_fail` | Dispatch failed with terminal classification; walker returned error | `{entry, reason}` |
| `cascade_resolved` | First-success dispatch completed | `{entry, latency_ms}` |
| `network_rate_above_cap` | Market entry: all offers exceed computed max_budget (premium filter rejected pre-Wire) | `{model_id, premium_pct, min_rate_in, min_rate_out, max_budget, filter_source: "cache"}` |
| `network_rate_above_wire_budget` | Market entry: Wire 409 rejected on budget (reservation_fee outlier) | `{model_id, wire_reason}` |
| `network_rate_cap_unavailable` | Cache cold or model not offered; fallback to sentinel | `{model_id, reason: "cache_cold" \| "model_not_offered"}` |

Existing events (`network_helped_build`, `network_result_returned`, `network_fell_back_local`, `cloud_returned`, `local_ollama_returned`) continue to fire unchanged. Cascade events wrap them as outer brackets.

---

## Build order recap

- **Wave 0** (serial, 1 agent, ~300 LOC): prereqs — bundled seed + sync wiring + boot hydration + TS interface fix + delete via-registry + bypass-path audit. Verifier at end.
- **Wave 1A** (serial, 1 agent, ~600 LOC): walker shell + pool-entry branch + HTTP retry moved inside + MarketSurfaceCache types + cache impl. Verifier.
- **Wave 1B** (serial, 1 agent, ~600 LOC): inline fleet into walker, remove Phase A pre-loop, remove fleet_filter, `resolve_local_for_rule` updated. Verifier.
- **Wave 1C** (serial, 1 agent, ~800 LOC): inline market into walker, premium filter, remove Phase B pre-loop, deprecate eager/threshold fields. Verifier + wanderer.
- **Wave 2** (serial, 1 agent, ~600 LOC): policy field + Settings panel + Local-Mode interaction + tier autocomplete + discovery. Verifier.
- **Wave 3** (serial, 1 agent, ~150 LOC): remove deprecated fields + string-match site audit + chronicle cleanup + permit-release test. Verifier.

**Total: ~3050 LOC across 6 sub-waves. Realistic estimate: 3-4 sessions including verify/wander cycles.** Single operator wiping — no migration cost.

---

## Acceptance

- Walker replaces Phase A pre-loop + Phase B pre-loop + permit-acquisition loop with one iteration
- `route.providers` is THE cascade; entries include `"fleet"`, `"market"`, and any provider_id from `pyramid_providers`
- `for_each_concurrent(N)` fires N parallel walkers, each respecting per-entry capacity
- Fleet saturation (all peers at max depth) → advance to next route entry
- Market saturation (all offers at queue capacity) → advance to next route entry
- Pool saturation (semaphore full + timeout) → advance to next route entry
- Premium filter rejects pre-Wire when all offers exceed cap; Wire rejects via 409 when reservation_fee pushes total over budget; both advance correctly
- Fresh install reads bundled `dispatch_policy-default-v1`, runs a build, chronicle shows walker-driven dispatch
- Settings panel reorders/toggles routes; saves supersede contribution; walker picks up new order
- Local Mode preset still toggles atomically via existing IPCs; routes/tiers panel read-only during enabled
- `cargo check` (default target) green; `cargo test --lib llm dispatch_policy compute_requester market_surface_cache` green; `npm run build` green
- No `dispatch_through_pool` function introduced (was a rev-0.4 fabrication); walker is the only dispatch gate
- Grep: zero production callers of `call_model_via_registry` (deleted in Wave 0)

---

## Anti-scope-creep list

- ❌ New contribution schema_type
- ❌ New IPC surface (existing `pyramid_create/supersede/active_config` used)
- ❌ Walker replaces `resolve_tier` (resolve_tier still resolves tier → model; walker dispatches)
- ❌ Wire-side `/match` body changes (existing `max_budget` field carries premium-capped value)
- ❌ JSON Schema validator wiring
- ❌ `min_reputation` / reputation surface
- ❌ Canary protocol
- ❌ Tagged-enum routing types
- ❌ Per-tier route_order
- ❌ `*market` wildcard
- ❌ `ui_preferences` schema
- ❌ Native HTML5 drag-reorder (v1 uses up/down buttons)
- ❌ Per-entry timeout overrides
- ❌ Rev-0.6.1 migration preservation (wiped DB)
- ❌ Settings.tsx Local Mode toggle removal (demoted to preset, not deleted)
- ❌ `pyramid_tier_routing` / `pyramid_local_mode_state` schema changes

---

## Audit history

- rev 0.1 — `compute_cascade` new primitive. Stage 1 informed pair → REVISE.
- rev 0.2 — split Phase A + Phase B plan. Stage 2 discovery pair → REVISE with 6 new CRITICALs.
- rev 0.3 — dissolved new primitive; "teach walker about market." Two audits → REVISE; walker scope was 3-5× my estimate (fabricated `dispatch_through_pool`).
- rev 0.4 — added Wave 0 prereqs + rate-cap premium + Local-Mode read-only. Clean blind audit → REVISE; walker fold scope still under-estimated, MarketSurfaceResponse types fabricated.
- rev 0.5 (this rev) — walker fold accepted at honest scope (1500-2200 LOC for Wave 1A+B+C). Concurrency folded via per-entry `try_acquire_capacity`. Market-surface types specified from verified TS shape. Per-entry saturation = advance-to-next-entry cascade. All rev-0.4 criticals addressed.

---

## Retro items for post-ship

1. **Audit hygiene**: don't taint prompts with prior-stage summaries or verdict nudges. Target doc + stated purpose + audit scope only.
2. **Maximal elegance, minimum new elements**: the pattern is "find the existing primitive, extend ONE enum value or add ONE semantic" — not "invent a new primitive." Rev 0.1→0.5 trajectory is this lesson in five parts.
3. **Walker scope estimation**: reading just the function signatures of the target code isn't enough. Actual source-line counts of the blocks being consolidated matter. Rev 0.3→0.4 walker estimate was off by 3-5× because I didn't verify the ~475 LOC Phase A + ~350 LOC Phase B + ~430 LOC HTTP retry numbers until the audit forced me to.
4. **Concurrency is an orthogonal concern that needs to ride the same refactor**: fleet/market serializing was a symptom of the same hardcoded-hierarchy problem. Fixing it separately would have been another 3-session cycle on the same code paths.

---

## Handoff

After clean blind audit + Adam GO:
- Wave 0 serial (~300 LOC).
- Wave 1A serial (~600 LOC). Verifier.
- Wave 1B serial (~600 LOC). Verifier.
- Wave 1C serial (~800 LOC). Verifier + wanderer.
- Wave 2 serial (~600 LOC). Verifier.
- Wave 3 serial (~150 LOC). Verifier.
- Final full-feature wanderer.
- Ship.

Estimated total: 3-4 sessions, ~3050 LOC. No cross-repo coordination. No migration. Single operator.
