# Resume-Here — Node Walker Re-Plan Against Wire Rev 2.1

**Date:** 2026-04-20
**Purpose:** Comprehensive resumption brief for post-compact future-me (or a fresh agent) picking up the node-side walker refactor now that Wire rev 2.1 has shipped.

---

## TL;DR

The Wire quote primitive + maximal `/market-surface` shipped at commit **`1adb3f20`** in the GoodNewsEveryone repo. Node-side contracts crate bumped in commit **`116d87a`**. Walker re-plan has not started. The old `docs/plans/compute-cascade-build-plan.md` (rev 0.5) is marked **SUPERSEDED** at the top — do not implement that plan.

**Next action:** Write a fresh node-side walker plan against Wire rev 2.1's shape. Then one clean blind audit. Then implement. Retro after ship.

---

## Where to read first (in order)

1. **Wire rev 2.1 spec (authoritative):** `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/plans/compute-market-quote-primitive-spec-2026-04-20.md` (930 lines). Freeze state at commit 1adb3f20.
2. **Superseded node plan (history only):** `/Users/adamlevine/AI Project Files/agent-wire-node/docs/plans/compute-cascade-build-plan.md`. Marked SUPERSEDED at the top — gives 5 revs of design evolution + retro items. Architectural approach there is obsolete; retro lessons are still valid.
3. **This repo's recent commits (walker-relevant):**
   - `116d87a` — contracts bump to Wire 1adb3f20
   - `240e119` — superseded plan doc committed as design record
   - `3bb86b8` — Settings.tsx `ComputeParticipationPolicy` TS interface round-trip fix
   - `0c4afd7` — **`DispatchOrigin` enum on `LlmCallOptions`** — chronicle source-label fix (market jobs now label `market_received`, not `fleet_received`). Carries forward into walker as the mechanism that distinguishes per-entry dispatch origin.
   - `b7ad65f` — deleted dead `call_model_via_registry` (~420 LOC). Bypass trap eliminated.
4. **Rev-0.6.1 P2P delivery** is live + smoke-validated on `f6317ed` and prior. Unchanged by this work.

---

## The shape of the walker re-plan

Wire rev 2.1's quote primitive collapses most of what the superseded rev-0.5 plan was trying to do on the node side. The walker becomes dramatically simpler.

### Per-entry dispatch algorithm

For each entry in `dispatch_policy.routing_rules[0].route_to`, in order:

**If `provider_id == "market"`:**
```
quote = POST /api/v1/compute/quote
  body: { model_id, input_tokens_est, max_tokens, latency_preference, max_budget, requester_node_id }
  responses:
    200 { quote_jwt, quote_id, expires_at, price_breakdown }
    404 no_offer_for_model      → advance silently (no chronicle per §2.1 scoping)
    409 budget_exceeded         → advance (PremiumFiltered; operator-actionable chronicle fires)
    409 insufficient_balance    → TERMINAL, bubble
    503 platform_unavailable    → Retryable, honor X-Wire-Retry + Retry-After
    400/401                     → TERMINAL

purchase = POST /api/v1/compute/purchase
  body: { quote_jwt, trigger: "immediate", idempotency_key }
  responses:
    200 { job_id, request_id, dispatch_deadline_at }
    409 quote_no_longer_winning         → advance (race lost; cheaper/retracted/full)
    409 quote_already_purchased + idem  → use cached response, proceed
    401 quote_jwt_expired               → advance or re-quote same entry
    403 quote_operator_mismatch         → TERMINAL (config)
    400 quote_jwt_invalid               → TERMINAL (config)
    409 insufficient_balance            → TERMINAL (balance race)

fill = POST /api/v1/compute/fill
  body: { job_id, callback_url, requester_delivery_jwt, max_tokens (≤ quoted, optional), payload... }
  responses:
    200 dispatch ACK (rev-2.0 oneshot await follows as before)
    409 dispatch_deadline_exceeded         → advance (lost the slot)
    503 provider_depth_exceeded            → advance w/ X-Wire-Retry backoff
    503 provider_dispatch_conflict         → advance w/ X-Wire-Retry backoff
    400 max_tokens_exceeds_quote           → TERMINAL (walker bug; should never happen if walker honors quoted)
```

**If `provider_id == "fleet"`:**
- Runtime gates: `policy.allow_fleet_dispatch && ctx.fleet_dispatch_present && tunnel_snap.connected && route.matched_rule_name nonempty`.
- Acquire: `fleet_roster.find_peer_for_rule(route.matched_rule_name, staleness_secs)` — returns lowest-queue-depth peer OR None on no-fresh-peer.
- Dispatch: inline the existing Phase-A fleet block (fleet-JWT + fleet_dispatch_by_rule + oneshot). On Retryable failure → advance. On Terminal (AuthFailed/ConfigError) → bubble.

**If `provider_id` is a real provider row (openrouter / ollama-local / custom):**
- Runtime gates: provider exists in registry, credentials resolved.
- Acquire: **add `ProviderPools::try_acquire_owned`** (non-blocking variant) that returns `Err(Saturated)` immediately when the semaphore is full. Today's `acquire()` is await-blocking via `timeout(30s, ...)`, which would kill the walker's advance-on-saturation semantic. This new method is a Wave 1 prereq.
- Dispatch: inline the existing HTTP retry loop (moved into the per-entry branch; today it lives once post-permit-acquisition at llm.rs:2312+).

### Concurrency (uniform across route types)

`for_each_concurrent(N)` at the chain level → N parallel walkers → each tries `route_to[0]` first. Per-entry `try_acquire_capacity` returns `Saturated` immediately, walker advances. The walker never blocks waiting on capacity — advance is the response.

### Cache probe semantics

Cache lookup happens ONCE, at entry, before the walker loop. Cache key uses the CANONICAL model name (the tier's configured model), not the alias-translated name. So market + openrouter + local Ollama share cache hits when the canonical name is the same.

### Audit row lifecycle

`insert_llm_audit_pending` runs once at walker entry (after cache probe miss). On exit, update with the winning entry's `provider_id` — which now needs to accept `"market"` and `"fleet"` as valid values alongside real provider rows (minor DB schema check).

### MarketSurfaceCache

SSE stream subscription + 60s-TTL snapshot fallback. Re-GET-then-subscribe on reconnect (no Last-Event-ID replay). Rate-limit: 30 concurrent streams per IP (rev 2.1 bumped from 10). Walker reads cache synchronously for discovery but the walker's `/quote` call is the authoritative viability check — cache is advisory.

---

## 10 verifier+wanderer notes from Wire side (affect walker design)

Directly copied from Wire dev's ship-notice. These are the known nuances to respect in the walker implementation:

1. **`/fill` rematch retired.** Old Wire-side rematch is deleted (match_compute_job gone). `/fill` now returns typed 503 `provider_depth_exceeded` / `provider_dispatch_conflict` with `retry_after_seconds` + `X-Wire-Retry: backoff`. Walker sees the typed slug → advance (or re-quote via `/quote` + `/purchase` for a fresh match). Chronicle `compute_fill_rematch_exhausted` fires with `second_reason: wire_rematch_disabled_rev_2_1` for traceability.
2. **`/purchase` orphan recovery.** Retry with same quote_jwt that hit a mid-commit DB hiccup is auto-recovered server-side. Walker sees normal 200. Chronicle `recovered_from_orphan: true` on committed event. If recovery itself fails, `compute_rpc_error` fires separately for ops.
3. **`queue_position` is 0-indexed pre-increment.** Cheapest offer's `price_breakdown.queue_position == 0` means "immediate dispatch slot."
4. **Whole-market scope unfiltered.** `/market-surface?model_id=X` still computes the top-level `market` block from ALL offers, not the filtered slice. Walker trusts `market.*` for global state, `models[].*` for per-route filtering.
5. **Filter placeholders (`min_reputation`, `max_tokens_min`) inert in v1.** Accepted but don't filter. Walker can pass speculatively for forward-compat without 400s.
6. **Depth bucket edges per-axis.** `economic_parameter['market_depth_rate_buckets']` has separate `input` and `output` arrays. Legacy flat-`edges` shape accepted via fallback.
7. **`/market-surface/history`** merges whole-market + per-model per bucket. Walker rarely reads this; operator UI.
8. **SSE is process-local EventEmitter.** Single Wire process today (Temps). Horizontal scale = NOTIFY/LISTEN migration later. Walker's reconnect via re-GET-then-subscribe.
9. **`velocity_1h.rate_changes` inflated** by queue-mirror pushes bumping `offers.updated_at`. Informational; walker doesn't consult.
10. **Chronicle scoping** — `/quote` 404 `no_offer_for_model` and generic 400s emit NOTHING on Wire side (walker-advance noise avoided). Only operator-actionable economic states emit `compute_quote_rejected` (insufficient_balance, budget_exceeded, 503 platform_unavailable, 503 economic_parameter_missing). Walker's speculation pattern won't flood wire_chronicle.

---

## Node-side call-site integration

The walker replaces the hardcoded Phase-A fleet / Phase-B market / pool-escalation hierarchy in `src-tauri/src/pyramid/llm.rs::call_model_unified_with_audit_and_ctx`. Approximate line ranges (post-b7ad65f deletion):

- **Phase A fleet** (~1200-1690) — becomes `dispatch_fleet_entry()` called from walker's fleet branch.
- **Inline `route.providers.retain(|e| e.provider_id != "fleet")`** at ~line 1694 — **deleted** (fleet is a real walker entry now, not filtered).
- **Phase B market** (~1697-2047) — becomes `dispatch_market_entry()` called from walker's market branch. Today's `should_try_market` gate function's 6 checks move INTO the walker's per-entry runtime gate.
- **compute_queue enqueue** (~2049-2163) — **stays where it is**, runs BEFORE the walker. Local-GPU scheduling primitive that the walker doesn't replace.
- **Permit-acquisition loop** (~2198-2243) — **replaced** by walker iteration over `route.providers`.
- **HTTP retry loop** (~2312-2740, ~430 LOC) — becomes per-entry logic inside the pool-provider branch of the walker.

`resolve_tier()` at `provider.rs:911` is NOT replaced — it still resolves `tier_name → (provider_id, model_id)`. The walker uses the resolved model_id for its `/quote` body and for cache key.

**DispatchOrigin enum** (shipped in commit 0c4afd7) is the existing mechanism for labeling which origin a dispatch came from — walker preserves this at per-entry spawn time for local-execution queue replays.

---

## Other Wire rev-2.1 integration points

- **`/match` → 410 Gone** with Sunset + X-Wire-Reason: use_quote_purchase + Link: successor-version. Any straggler node that still hits /match gets a parseable deprecation signal. Rev-0.6.1 node code path uses /match; walker re-plan removes those call sites.
- **`compute_purchase_expired_unloaded` chronicle** fires via `purchase_expiry` cron (60s) when a reserved job isn't filled within `dispatch_deadline_at`. Walker must fill promptly after purchase OR accept the reservation forfeit.
- **`dispatch_deadline_at` default = 60s** via `economic_parameter['compute_purchase_dispatch_window_s']`. Walker's eager path (quote → purchase → fill back-to-back) fits comfortably. Deferred pattern (purchase now, fill later) is possible but forfeits N-1 reservation fees when speculating across routes; documented design tradeoff.
- **`max_budget` f64-safe ceiling** is `Number.MAX_SAFE_INTEGER` (2^53-1). Rev-0.6.1 `f6317ed` already shipped this sentinel on the node side.

---

## Wave structure (proposed — needs to be written into the new plan doc)

Rough wave sketch to feed into the new plan draft:

- **Wave 0 — prereqs:** Bundle `dispatch_policy-default-v1` seed. Write `sync_dispatch_policy_to_operational` helper. Verify boot hydration at main.rs:11824+ picks up the bundled seed correctly. Add `ProviderPools::try_acquire_owned` non-blocking variant.
- **Wave 1A — walker shell:** Iteration-over-route-entries scaffolding. Pool-provider branch with try_acquire + HTTP retry moved inside. MarketSurfaceCache types from rev 2.1 contracts.
- **Wave 1B — fleet inline:** Extract Phase A into `dispatch_fleet_entry`. Walker fleet branch. Remove inline retain.
- **Wave 1C — market inline:** `dispatch_market_entry` calling the 3 RPCs (/quote → /purchase → /fill). Premium filter is now Wire-side via `max_rate_per_m_input/output` params on /market-surface or /quote body — scope to match rev 2.1's exact semantics. Remove Phase B pre-loop.
- **Wave 2 — SSE cache + Settings panel:** MarketSurfaceCache subscribes to SSE. Inference Routing panel edits `dispatch_policy.routing_rules[0].route_to`.
- **Wave 3 — cleanup:** Remove deprecated `market_dispatch_eager` + `market_dispatch_threshold_queue_depth` (walker saturation replaces their role).

Scope estimate: ~1500-2200 LOC node-side (walker was audit-sized at ~4500-6000 in the pre-rev-2.1 plan; Wire's shipping dissolved ~60% of that). 2-3 sessions realistic.

---

## First action on resumption

1. Read this brief.
2. Read the Wire rev 2.1 spec end-to-end (930 lines; don't summarize, actually read — audit history shows summary-reading bit us multiple times).
3. Verify `cargo check` in `src-tauri/` is clean against contracts rev 1adb3f20. (Was clean at commit 116d87a; should still be clean unless something drifted.)
4. Read `src-tauri/src/pyramid/llm.rs::call_model_unified_with_audit_and_ctx` (the ~1600-line dispatcher) — not skim, actually read — to ground truth the line ranges above.
5. Draft `docs/plans/walker-re-plan-wire-2.1.md` (fresh name, not another rev of the superseded doc).
6. Run ONE clean blind audit (target + purpose + scope; NO prior-stage summaries, NO verdict nudges — audit hygiene lesson from prior session).
7. Apply findings, re-audit if needed.
8. Ask Adam for GO before implementation.
9. Implement per wave structure.
10. Retro after ship — include the four lessons from the superseded plan's audit history (hygiene, minimalism, scope, cross-repo).

---

## Who owns what

- **Wire side (GoodNewsEveryone):** rev 2.1 shipped + frozen. Any further tweaks ride through the contracts package. Wire dev available if walker build surfaces non-obvious behavior.
- **Node side (agent-wire-node):** walker re-plan + implementation. This brief is for future-me / the next agent.
- **Adam:** orchestrates; does not hand-edit code. Manages moltbot cron enables (per ship-notice).

---

## Open items / backlog

- **Post-ship retro** capturing: audit hygiene (don't taint prompts), minimalism (find existing primitive, extend one enum value), scope estimation (actual source-line counts matter), cross-repo symptom-to-systemic (when node fabricates signals the other repo has direct access to, fix the API).
- **Moltbot cron timers** Adam is enabling: `wire-compute-tick@purchase_expiry.timer` (60s), `wire-compute-tick@market_snapshot.timer` (300s), `wire-compute-tick@market_snapshot_retention.timer` (daily).
- **Stale pre-rev-2.1 reserved row** in wire_compute_jobs is backfilled + will be swept by the first purchase_expiry tick.
- **BEHEM rebuild** to pick up the chronicle source-label fix (0c4afd7) is orthogonal — no urgency, but fixes the "market jobs show as fleet_received" display anomaly on its Chronicle tab.
