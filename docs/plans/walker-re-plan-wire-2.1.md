# Walker Re-Plan — Node-Side Dispatch Against Wire Rev 2.1

**Date:** 2026-04-20 (rev 0.2: 2026-04-21; rev 0.3: 2026-04-21)
**Status:** READY FOR IMPLEMENTATION — Stage 1 + Stage 2 audits applied. 15 findings folded in (§2.5 systemic helpers + rev 0.3 fixes). Adam GO'd implementation handoff.
**Rev:** 0.3

**Supersedes:** `docs/plans/compute-cascade-build-plan.md` rev 0.5 (marked SUPERSEDED in-file). The cascade plan's architecture (client-side premium filter, MarketSurfaceResponse types authored node-side, `market_dispatch_premium_pct` policy field, `network_rate_above_cap` chronicle) is OBSOLETE — Wire rev 2.1 moves all those derivations server-side.

**Cross-repo anchors:**
- Wire rev 2.1 spec (authoritative): `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/plans/compute-market-quote-primitive-spec-2026-04-20.md` at commit `1adb3f20`.
- Contracts crate bump: `src-tauri/Cargo.toml` at commit `116d87a`.

---

## 1. One-paragraph statement

Today's dispatcher (`call_model_unified_with_audit_and_ctx`, llm.rs:1158-2836) has three hardcoded, semantically-different phases (Phase A fleet pre-loop at 1248-1725, Phase B market pre-loop at 1732-2082, Phase D pool escalation at 2231-2281). Each handles capacity, retry, and failure differently. This plan collapses all three into ONE walker over `route.providers` where every entry — `"fleet"`, `"market"`, real provider rows — obeys the same contract: runtime-gate → `try_acquire` (saturation → advance) → dispatch → retryable-fail → advance, terminal-fail → bubble. The market entry runs Wire rev 2.1's three-RPC flow (`/quote → /purchase → /fill`) instead of today's `/match → /fill`. Wire-side derivations (rate filtering, saturation, reservation pricing) are no longer fabricated node-side — they come from `/quote`'s `price_breakdown` or `/market-surface` offers. The compute_queue local-GPU FIFO stays where it is (runs after the walker's Phase A/B replacement, before the pool-provider branch's HTTP call). Wipe-and-fresh-install rollout — single operator, no legacy migration.

---

## 2. What changes vs what survives

### Walker replaces

| Today | Walker equivalent |
|---|---|
| Phase A fleet pre-loop (llm.rs:1248-1725, ~477 LOC) | Walker entry `provider_id == "fleet"` branch |
| Fleet filter retain (llm.rs:1727-1730) | Deleted — fleet is a real walker entry now |
| Phase B market pre-loop (llm.rs:1732-2082, ~350 LOC) | Walker entry `provider_id == "market"` branch; `/quote + /purchase + /fill` |
| Phase D pool escalation loop (llm.rs:2231-2281) | Walker per-entry `try_acquire_owned` + advance-on-saturated |
| HTTP retry loop (llm.rs:2350-2830, ~480 LOC) | Moved inside walker's pool-provider branch |
| `should_try_market` standalone fn (llm.rs:167-210) | Inlined into walker's market-entry runtime gate |
| `compute_requester::{dispatch_market, await_result, call_market, call_match, call_fill, resolve_uuid_from_handle}` (compute_requester.rs, ~920 LOC) | Rewritten to `compute_quote_flow::{quote, purchase, fill, await_result}` (~600 LOC target) |

### Walker preserves unchanged

- Cache probe at entry (llm.rs:1172-1212). ONE lookup per walker invocation, keyed by canonical model name. Cache hits short-circuit the entire walker.
- `dispatch_policy::resolve_route` at llm.rs:1226-1233 — input to the walker.
- Audit row lifecycle (`insert_llm_audit_pending` at 2210-2227; `complete_llm_audit` at 2815-2827). One row per walker invocation. Exit updates with the *winning* entry's `provider_id`.
- compute_queue enqueue path (llm.rs:2084-2201). Runs AFTER fleet/market walker entries have had their turn, BEFORE the pool-provider HTTP retry. Queue replay re-enters the walker with `skip_fleet_dispatch=true` + `skip_concurrency_gate=true`.
- DispatchOrigin enum on LlmCallOptions (llm.rs:903) — carries forward unchanged. Chronicle source_label derived from `options.dispatch_origin`.
- Fleet peer-lookup, JWT fetch, `fleet_dispatch_by_rule`, oneshot-await mechanism, roster staleness semantics (fleet_mps.rs + fleet.rs paths).
- PendingJobs map + inbound `/v1/compute/job-result` handler + server-side result delivery topology. These are rev-2.0 P2P and unchanged in rev 2.1.
- Escalation+retry semantics for non-market provider rows (model override, context-exceeded cascade, provider-health hooks, augment_request_body, rate_limit_wait fallback).

### Walker adds (new)

- `compute_quote_flow` module (`src-tauri/src/pyramid/compute_quote_flow.rs`) — rev 2.1 three-RPC client. Replaces compute_requester.rs internals.
- `MarketSurfaceCache` (`src-tauri/src/pyramid/market_surface_cache.rs`) — polling `/api/v1/compute/market-surface` every 60s (SSE deferred to §6). Walker consults it for advisory saturation/rate signals; `/quote` is still the authoritative viability check.
- `ProviderPools::try_acquire_owned` — non-blocking variant returning `Result<OwnedSemaphorePermit, TryAcquireError>` immediately. Trivial wrapper around Tokio's native `Arc<Semaphore>::try_acquire_owned()`.
- `dispatch_policy-default-v1` bundled contribution family (schema_definition + schema_annotation + generation skill + default seed) — fresh-install seed in `src-tauri/assets/bundled_contributions.json`.
- `sync_dispatch_policy_to_operational` boot-hydration helper — mirrors `sync_chain_defaults_to_operational` at wire_migration.rs:1449.
- Walker's per-entry chronicle events (see §5).
- Settings.tsx "Inference Routing" panel — operator-facing edit of `dispatch_policy.routing_rules[*].route_to`.
- **Three systemic helpers (§2.5)** — `LlmConfig::prepare_for_replay`, `branch_allowed`, and the three-tier `EntryError` taxonomy.
- **`RouteEntry.max_budget_credits: Option<i64>`** — new field on `dispatch_policy.rs::RouteEntry`. Walker sources `/quote` `max_budget` from it per entry. Operator can set a per-entry cost ceiling (e.g. "don't pay more than 5000 credits for this market route"). Defaults to `(1i64 << 53) - 1` sentinel if absent. Wire's 409 `budget_exceeded` now fires on walker's configured ceiling.
- **Audit row `provider_id` column** — Wave 1 migration adds `pyramid_llm_audit.provider_id TEXT`. Extended `complete_llm_audit` signature carries the winning entry's `provider_id` through to the row on walker exit. Legacy rows NULL-safe.

### Walker removes (vs today)

- **`escalation_timeout_secs`** field + its `timeout(...)` wrap at llm.rs:2253. Walker's non-blocking `try_acquire_owned` retires the "wait up to N seconds for pool capacity" semantic entirely. Operators who previously relied on the 30s wait now see immediate advance to the next route entry — document loudly in release notes. Operators who want local-first must reorder `route_to` so local comes before cloud and/or accept compute_queue FIFO serialization at the local-Ollama pool (concurrency: 1 default).
- **`market_dispatch_eager` / `market_dispatch_threshold_queue_depth`** fields on `ComputeParticipationPolicy` (deprecated Wave 3, removed Wave 5). Queue-depth-as-proxy was a pre-walker workaround; Wire's `/quote` is the authoritative viability check.
- Standalone `should_try_market` fn (llm.rs:167-210) — inlined into walker's market-entry runtime gate (Wave 3).
- `classify_soft_fail_reason` + `sanitize_wire_slug` helpers (llm.rs:232-277) — Phase B-specific, deleted when Phase B goes.
- `NetworkHandleInfo` struct — replaced by direct use of `ComputeQuoteResponse.price_breakdown` + `ComputePurchaseResponse` fields for chronicle metadata.

---

## 2.5 Systemic helpers — three centralizations

The audit of today's dispatcher surfaced three patterns where logic is scattered across call sites and drifting. Walker folds each into a single authority. These aren't walker-scope inventions — they're systemic fixes that ride the same refactor because scattering them further would waste the opportunity.

### 2.5.1 `LlmConfig::prepare_for_replay(origin: DispatchOrigin) -> Self`

**Problem:** "What does a replay LlmConfig look like for origin X?" is answered at four separate call sites today (llm.rs queue-enqueue clone, server.rs fleet handler, server.rs market handler, dadbear_supervisor replay path). Each site decides independently which fields to clear. The result: fleet+market inbound paths clear `fleet_dispatch + fleet_roster` but LEAVE `compute_market_context`, because no one saw the 4-way consistency gap. That's the latent bug Wave 3 task 22a closes with a runtime gate — the systemic fix closes it at config-derivation time.

**Shape:**

```rust
impl LlmConfig {
    /// Derive a replay config from this config. Single source of truth for
    /// which dispatch-routing fields are cleared. The key insight: whenever
    /// `prepare_for_replay` is called, the OUTER dispatch decision has
    /// already been made. The inner (replayed) call should be pool-only —
    /// it has no business re-dispatching to fleet or market.
    ///
    /// Origin-independent by design: for Local origin (compute_queue replay
    /// from the outer walker), the outer walker already tried fleet + market
    /// before the enqueue decision. For FleetReceived / MarketReceived
    /// (inbound-job worker spawn), the node is the provider fulfilling
    /// someone else's work — no outbound dispatch should happen.
    ///
    /// Takes `origin` for observability (emitted via tracing::debug at each
    /// call) and for future use if an origin-specific carve-out becomes
    /// necessary. Call-site intent is explicit.
    pub fn prepare_for_replay(&self, origin: DispatchOrigin) -> Self {
        tracing::debug!(?origin, "preparing replay config");
        let mut cfg = self.clone();
        cfg.compute_queue = None;             // prevents re-enqueue loop
        cfg.fleet_dispatch = None;            // no fleet re-dispatch
        cfg.fleet_roster = None;
        cfg.compute_market_context = None;    // no market re-dispatch (fix for queue-replay redundant /quote)
        cfg
    }
}
```

**Call-site simplification (Wave 0):** llm.rs:2104-2108 + server.rs:2025-2033 + server.rs:3954-3962 + dadbear_supervisor.rs replay site all shrink from hand-clearing to `let replay_cfg = config.prepare_for_replay(origin);`. Net savings ~30 LOC across 4 sites; eliminates the "someone will forget a field next time a field gets added" class of bug.

**Why origin-independent:** rev 0.2 preserved `compute_market_context` on Local origin thinking "user's own chain continues to have routing options." Audit surfaced that for compute_queue replays (the only Local-origin replay path), the outer walker already decided on a specific pool-provider entry AND already tried fleet + market. Inner replay trying them again is redundant (extra /quote RT per locally-queued LLM call) and wrong-shaped — the replay is meant to run one specific local provider. Symmetric clearing across all origins eliminates the class of bug.

### 2.5.2 `branch_allowed(branch: RouteBranch, origin: DispatchOrigin) -> bool`

**Problem:** Today's fleet branch runtime-gate checks `!options.skip_fleet_dispatch`. Market branch checks nothing origin-related. Pool branch checks nothing origin-related (and shouldn't — pool is always allowed). The `skip_fleet_dispatch` flag is a per-call explicit override, but the origin-based default ("inbound jobs don't re-dispatch") isn't centralized. Walker adding a `dispatch_origin == Local` check on the market branch is correct; the systemic frame is that EVERY branch should consult a shared helper.

**Shape:**

```rust
enum RouteBranch { Fleet, Market, Pool }

/// Decides whether a route branch is allowed for an execution context with
/// the given DispatchOrigin. Single source of truth for the "inbound jobs
/// don't re-dispatch" invariant.
fn branch_allowed(branch: RouteBranch, origin: DispatchOrigin) -> bool {
    match (branch, origin) {
        // Pool is always allowed — even inbound jobs need local execution.
        (RouteBranch::Pool, _) => true,
        // Fleet + market only from Local origin (own builds).
        (RouteBranch::Fleet | RouteBranch::Market, DispatchOrigin::Local) => true,
        (RouteBranch::Fleet | RouteBranch::Market,
         DispatchOrigin::FleetReceived | DispatchOrigin::MarketReceived) => false,
    }
}
```

**Gate wiring:** walker's fleet runtime-gate uses `branch_allowed(Fleet, origin) && !options.skip_fleet_dispatch && ...` — helper is the origin-based default, explicit flag is a per-call override. Same for market. With `prepare_for_replay` landed, the `skip_fleet_dispatch` flag becomes redundant in normal flow (origin already implies it) but the flag stays for tests and per-call overrides.

### 2.5.3 Three-tier `EntryError`

**Problem:** Plan §3/§4 uses `enum EntryError { Retryable, Terminal }`. "Terminal" conflates two different semantics:
1. **This entire call is doomed.** Bubble to caller — no other route will help. (Walker bug, provider_impl parse failure, max_tokens_exceeds_quote, genuine caller-config bug like multi-system-messages.)
2. **This route-kind can't serve the call right now.** Advance — another route might succeed. (insufficient_balance on `/quote`: market route can't proceed, but fleet is free and openrouter has its own billing. Credentials missing on openrouter: openrouter can't, but ollama-local might.)

Plan classifies `/quote` 409 `insufficient_balance` as Terminal today. Walker would bubble and waste a viable fleet entry. Wrong.

**Shape:**

```rust
enum EntryError {
    /// Same route class, retry-after-delay kind of failure.
    /// Rare at walker scope — walker usually just advances rather than loop.
    Retryable { reason: String },
    /// This route branch can't serve this call — advance to next entry.
    /// Different kinds of "wrong resource for this call": insufficient market
    /// credits, missing openrouter key, fleet peer dead, dispatch-deadline missed.
    RouteSkipped { reason: String },
    /// This entire call is doomed regardless of route. Bubble to caller.
    /// Reserved for: genuine caller-config bugs (400 multi-system-messages,
    /// 400 max_tokens_exceeds_quote), walker bugs, auth/operator-level failures
    /// that would fail every route the same way.
    CallTerminal { reason: String },
}
```

**Walker semantics:**
- `Retryable` + `RouteSkipped` → walker advances to next entry. Distinct chronicle events (`network_route_retryable_fail` vs `network_route_skipped`).
- `CallTerminal` → walker bubbles. Chronicle `network_route_terminal_fail` + `fail_audit` + `Err(...)`.

**Impact on §4.2 table:** `insufficient_balance` at /quote → `RouteSkipped` (not Terminal). `quote_operator_mismatch` → `CallTerminal` (auth bug). `max_tokens_exceeds_quote` → `CallTerminal` (walker bug). `/fill` 401 → `CallTerminal`. Etc. Full revision below.

---

## 3. The per-entry walker algorithm

Given `ResolvedRoute { providers: Vec<RouteEntry>, matched_rule_name, escalation_timeout_secs, max_wait_secs }` from `dispatch_policy::resolve_route`:

```rust
// Cache probe ran before the walker. Audit row inserted before the walker.
for (i, entry) in route.providers.iter().enumerate() {
    let branch = classify_branch(&entry.provider_id);   // Fleet | Market | Pool

    // 1) Runtime gate — origin-based default + per-entry specifics.
    if !branch_allowed(branch, options.dispatch_origin) {
        // Structural — log-only, NOT chronicle (queue replays emit this
        // on every LLM call; would flood `pyramid_compute_events`).
        tracing::debug!(entry = %entry.provider_id, "walker: replay_guard skip");
        continue;
    }
    if !runtime_gate_pass(entry, ctx, config) {
        chronicle("network_route_skipped", { entry, reason });
        continue;
    }

    // 2) Try acquire capacity — entry-type-specific
    let capacity = match try_acquire(entry, ctx, config).await {
        Ok(c) => c,
        Err(AcquireError::Saturated) => {
            chronicle("network_route_saturated", { entry });
            continue;
        }
        Err(AcquireError::Unavailable(reason)) => {
            chronicle("network_route_unavailable", { entry, reason });
            continue;
        }
    };

    // 3) Dispatch — entry-type-specific. Three-tier error taxonomy.
    match dispatch_entry(entry, capacity, ctx, config, prompt, options).await {
        Ok(response) => {
            cache_store(ctx, cache_lookup, &response);
            complete_audit(audit_id, &response, entry.provider_id.clone());
            chronicle("walker_resolved", { entry, latency_ms, attempts: i+1 });
            return Ok(response);
        }
        Err(EntryError::Retryable { reason }) => {
            // Same route class, retry-later kind of failure (rare at walker
            // scope). Walker advances rather than loop.
            chronicle("network_route_retryable_fail", { entry, reason });
            continue;
        }
        Err(EntryError::RouteSkipped { reason }) => {
            // This route branch can't serve this call; advance.
            // (insufficient market credits, missing openrouter key, dead fleet
            // peer, dispatch deadline missed — wrong-resource-for-this-call.)
            chronicle("network_route_skipped", { entry, reason });
            continue;
        }
        Err(EntryError::CallTerminal { reason }) => {
            // This entire call is doomed regardless of route. Bubble.
            chronicle("network_route_terminal_fail", { entry, reason });
            fail_audit(audit_id, &reason);
            return Err(anyhow!(reason));
        }
    }
}
chronicle("walker_exhausted", { entries_tried: route.providers.len(), skip_reasons });
fail_audit(audit_id, "no viable route");
Err(anyhow!("no viable route — all {} entries exhausted", route.providers.len()))
```

**The three error tiers** — see §4 for per-branch classification; §2.5.3 for rationale.

**Walker awaits work, never capacity.** `try_acquire_owned` is non-blocking — saturation returns immediately. But fleet's oneshot await (`max_wait_secs`, typically 3600s) and market's `compute_quote_flow::await_result(rx, wait_ms)` BOTH wait on work completion. Distinct concerns: the walker doesn't serialize the chain on a saturated pool (that would revert to Phase D's coupling), but individual dispatched work items can take minutes to complete. `for_each_concurrent(N)` at the chain level spawns N walkers; each walker's in-flight work is tied up for however long the work takes.

---

## 4. Per-entry branch semantics

### 4.1 `provider_id == "fleet"`

**Runtime gate (all must pass):**
- `branch_allowed(Fleet, options.dispatch_origin)` — the origin-based default (§2.5.2). Non-Local origins fail → reason `fleet_replay_guard`, advance.
- `!options.skip_fleet_dispatch` — per-call explicit override (tests, scheduled replays). With `prepare_for_replay` deployed, this flag becomes redundant for normal flow but stays for explicit override.
- `!route.matched_rule_name.is_empty()` (fleet dispatch is rule-scoped by design)
- `config.fleet_dispatch.is_some()` (FleetDispatchContext attached)
- `config.fleet_roster.is_some()`
- Tunnel `Connected` + URL present (snapshot-and-drop pattern per today's Phase A)

**Acquire:**
- `roster.find_peer_for_rule(&route.matched_rule_name, policy.peer_staleness_secs)` → peer or None.
- None with empty roster OR no peer serving this rule → `AcquireError::Unavailable(reason="no_peer_for_rule")`.
- None with roster populated but all candidates at max queue → `AcquireError::Saturated`.

**Dispatch:** same mechanism as today's Phase A (llm.rs:1352-1705, ~350 LOC of meaningful content after extracting the chronicle writers) — generate fleet_job_path, register PendingFleetJob, POST `fleet_dispatch_by_rule`, oneshot-await with `job_wait_secs` timeout + `policy.timeout_grace_secs` grace, classify result. Extracted into `dispatch_fleet_entry()`.

**Error classification (three-tier per §2.5.3):**
- `Ok(FleetAsyncResult::Success)` → Ok.
- `Ok(FleetAsyncResult::Error)` (peer ran inference and it failed) → `RouteSkipped` (peer couldn't help). Chronicle `fleet_result_failed`.
- `Err("timeout")` → `Retryable`. Chronicle `fleet_dispatch_timeout`.
- `Err("orphaned")` → `Retryable`. Chronicle `fleet_dispatch_failed reason=orphaned`.
- Dispatch POST failed with `is_peer_dead()` → `RouteSkipped`. Peer removed from roster (today's behavior preserved).
- Dispatch POST failed with 503 → `RouteSkipped`. Chronicle `fleet_peer_overloaded`.
- Any other dispatch POST failure → `RouteSkipped`. Chronicle `fleet_dispatch_failed`.

No fleet branch raises `CallTerminal` — fleet failures never doom the whole call. Walker advances to next entry in all failure modes.

### 4.2 `provider_id == "market"`

**Runtime gate:** origin-based default + per-entry specifics. Replaces today's standalone `should_try_market` (llm.rs:167-210).
- `branch_allowed(Market, options.dispatch_origin)` — origin-based default (§2.5.2). Non-Local origins fail → reason `market_replay_guard`, advance. With `prepare_for_replay` (§2.5.1) clearing `compute_market_context` on all replays, this is defense-in-depth; the runtime gate still runs for call paths that bypass `prepare_for_replay` (tests, future callers).
- `policy.allow_market_dispatch`
- **Balance gate removed** — today's `balance >= estimated_deposit` check always passed (balance sentinel is `i64::MAX`). Wire's 409 `insufficient_balance` is the authoritative check; node-side duplication is dead weight.
- **Eager-threshold gate removed** — today's `policy.market_dispatch_eager || local_queue_depth >= threshold` gate was a queue-depth-as-proxy from the pre-walker era. With walker + `/quote` as authoritative viability check, the gate is redundant. `market_dispatch_eager` + `market_dispatch_threshold_queue_depth` both deprecated in Wave 3 (not Wave 5).
- `model_tier_market_eligible(tier)` — non-empty tier string
- `tunnel_snap.connected && tunnel_snap.has_url`
- `config.compute_market_context.is_some()`

**Acquire (advisory — authoritative is `/quote`):**
- Read `MarketSurfaceCache::get_model(&canonical_model_id)`.
- If entry absent OR cache cold → `AcquireError::Unavailable(reason="market_cache_cold")`. (Walker still tries `/quote` in this state if `allow_speculative_quote` policy is set; v1 treats cold cache as unavailable to avoid speculative Wire calls.)
- If `active_offers == 0` → `AcquireError::Unavailable(reason="no_offers_for_model")`.
- If `top_of_book.cheapest_with_headroom` is null AND `queue.unbounded_offers == 0` AND `queue.total_capacity - queue.current_depth == 0` → `AcquireError::Saturated`.
- Otherwise → `Ok(MarketCapacity { advisory_top_of_book })`. Advisory only; `/quote` re-checks.

**Dispatch — three RPCs, back-to-back (eager):**

```rust
// /quote — cost ceiling sourced from per-entry config (§11 — see max_budget_credits)
let quote_body = ComputeQuoteBody {
    model_id: canonical_model,
    input_tokens_est: est_input_tokens,
    max_tokens: effective_max_tokens,
    latency_preference: LatencyPreference::BestPrice,
    max_budget: entry.max_budget_credits
        .unwrap_or((1i64 << 53) - 1),            // per-entry cap if set; JS MAX_SAFE_INTEGER sentinel if absent
    requester_node_id: auth.node_id.clone(),
};
let quote_resp = compute_quote_flow::quote(quote_body, &auth, &config).await?;

// /purchase — fresh idempotency UUID per call
let purchase_body = ComputePurchaseBody {
    quote_jwt: quote_resp.quote_jwt,
    trigger: "immediate",
    idempotency_key: uuid::Uuid::new_v4().to_string(),
};
let purchase_resp = compute_quote_flow::purchase(purchase_body, &auth, &config).await?;

// PendingJobs keying — CRITICAL: purchase_resp.request_id is an idempotency token,
// NOT the DB-row UUID. The inbound /v1/compute/job-result delivery envelope
// carries the DB UUID. Walker must obtain the DB UUID — two paths:
//   (A) Wire-dev Q5 adds `uuid_job_id` to /purchase 200 response (preferred — saves RT).
//   (B) Fallback: poll GET /api/v1/compute/jobs/:handle-path once, extract UUID.
// Plan ships (A) if Wire dev turns around Q5 before Wave 3; (B) otherwise.
let uuid_job_id = compute_quote_flow::resolve_uuid_from_purchase(
    &purchase_resp, &auth, &config,
).await?;
let rx = pending_jobs.register(uuid_job_id.clone()).await;

// /fill — request_id (UUID-preferred per contract §1.8) as the body's job identifier.
// Omit legacy `job_id` handle-path field; `request_id` is stable across offer supersession.
let fill_body = ComputeFillBody {
    request_id: purchase_resp.request_id.clone(),   // UUID (idempotency token, also serves as stable job ref)
    messages,
    max_tokens: None,                                // defer to max_tokens_quoted in the quote JWT
    temperature,
    requester_callback_url: callback_url,
    idempotency_key: purchase_resp.request_id,       // header: Idempotency-Key
    // input_token_count + privacy_tier: presence governed by Wire-dev Q4.
    // Ship with both until Wire confirms retirement; drop via config flag if confirmed.
};
let fill_resp = compute_quote_flow::fill(fill_body, &auth, &config).await?;

// Await oneshot (keyed by the DB UUID resolved above)
let result = await_result(rx, &auth, &config, pending_jobs, wait_ms).await?;
```

**Error classification (three-tier per §2.5.3):**

| Source | Status/slug | Tier | Walker response |
|---|---|---|---|
| `/quote` | 200 | — | proceed to `/purchase` |
| `/quote` | 404 `no_offer_for_model` | `RouteSkipped` | Advance. No chronicle emit Wire-side per rev 2.1 §4.5. Node chronicle `network_route_skipped` with reason `no_offer_for_model`. |
| `/quote` | 409 `budget_exceeded` | `RouteSkipped` | Advance. Chronicle `network_rate_above_budget` (Wire reports the math). |
| `/quote` | 409 `insufficient_balance` | `RouteSkipped` | Advance — market can't help without credits, but fleet is free and openrouter has its own billing. Chronicle `network_balance_insufficient_for_market` with `{need, have}` so operator still sees the signal. If ALL routes exhaust for the same reason, caller bubble is "no viable route." |
| `/quote` | 503 `platform_unavailable` / `economic_parameter_missing` | `Retryable` | Honor `X-Wire-Retry` + `Retry-After`. Advance to next entry for v1 (loop-retry deferred). |
| `/quote` | 400 `invalid_body` / `multiple_nodes_require_explicit_node_id` / `no_node_for_agent` | `CallTerminal` | Bubble — genuine caller config bug; other routes would fail the same way. |
| `/quote` | 401 `unauthorized` | `RouteSkipped` | Advance. Wire's `api_token` is distinct from fleet's `fleet_jwt` and openrouter's API key — 401 on Wire doesn't invalidate the other branches. If ALL Wire-using routes 401, walker exhausts; operator sees "no viable route" with detail. Chronicle `network_auth_expired` for telemetry. |
| `/quote` | 403 `agent_unconfirmed` | `CallTerminal` | Bubble — operator-level consent broken; refusing to quote won't change based on route. |
| `/purchase` | 200 | — | proceed to `/fill` |
| `/purchase` | 409 `quote_no_longer_winning` | `Retryable` | Advance (v1: don't re-quote; treat as transient race). Chronicle `network_quote_expired`. |
| `/purchase` | 409 `quote_already_purchased` **with matching `idempotency_key`** | — (200 cached) | Wire returns cached 200 per contract §1.6b. Proceed to `/fill`. Chronicle `network_purchase_recovered`. |
| `/purchase` | 409 `quote_already_purchased` **with mismatched `idempotency_key`** | `RouteSkipped` | Advance. Different walker attempt/process already purchased; rehydrating creates ambiguous ownership. Let Wire's `purchase_expiry` cron expire the orphan. Chronicle `network_purchase_race_lost`. |
| `/purchase` | 401 `quote_jwt_expired` | `RouteSkipped` | Advance. v1 doesn't re-quote same entry. |
| `/purchase` | 401 `quote_jwt_expired` (also a 401, distinct slug) | (covered above by quote_jwt_expired row) | — |
| `/purchase` | 401 generic (Wire auth) | `RouteSkipped` | Advance. Chronicle `network_auth_expired`. Same rationale as /quote 401. |
| `/purchase` | 403 `quote_operator_mismatch` | `CallTerminal` | Bubble — JWT `rid` ≠ authed operator; this is a caller-config bug that affects every market dispatch. |
| `/purchase` | 409 `insufficient_balance` | `RouteSkipped` | Advance (balance race between quote and purchase). Chronicle as above. |
| `/purchase` | 400 `quote_jwt_invalid` | `CallTerminal` | Bubble — config bug (walker built a malformed purchase body). |
| `/fill` | 200 | — | Ok — proceed to oneshot await. |
| `/fill` | 409 `dispatch_deadline_exceeded` | `RouteSkipped` | Advance (we lost the slot). Chronicle `network_dispatch_deadline_missed`. |
| `/fill` | 503 `provider_depth_exceeded` / `provider_dispatch_conflict` | `RouteSkipped` | Advance. Honor `X-Wire-Retry`. Chronicle `network_provider_saturated`. |
| `/fill` | 400 `max_tokens_exceeds_quote` | `CallTerminal` | Bubble — walker bug (we passed `max_tokens > max_tokens_quoted`). Should never fire if walker honors the quoted ceiling. |
| `/fill` | 401 | `RouteSkipped` | Advance — Wire auth expiration doesn't invalidate other branches. Same rationale as `/quote` 401 above. Chronicle `network_auth_expired`. |
| oneshot await timeout | — | `RouteSkipped` | Advance. Classified as today's `DeliveryTimedOut` vs `DeliveryTombstoned` preserved; both → `RouteSkipped`. |
| oneshot result `Failure` | — | `RouteSkipped` | Advance. `ProviderFailed` with code/message in chronicle. |

### 4.3 Real provider row (`openrouter` / `ollama-local` / custom)

**Runtime gate:**
- `branch_allowed(Pool, options.dispatch_origin)` — always true by design (pool is the local-execution path; even inbound jobs need it).
- Provider exists in `config.provider_registry`.
- Credentials resolved at registry instantiation time (existing logic). **Missing credentials** here → `AcquireError::Unavailable(reason="credentials_missing")` at the acquire step, NOT a Terminal — other routes (fleet, other pool entries) may still serve.

**Acquire:**
- `pools.try_acquire_owned(&entry.provider_id)` — NEW non-blocking method (see §7).
- Err (semaphore full or closed) → `AcquireError::Saturated`.
- Err (provider not in pools map) → `AcquireError::Unavailable(reason="provider_not_in_pool")`.
- Err (credentials unresolved) → `AcquireError::Unavailable(reason="credentials_missing")`.
- Ok(permit) → `Ok(Permit(permit))` held across the HTTP retry loop + dropped on branch exit.

**Dispatch:** the existing HTTP retry loop at llm.rs:2350-2830, moved inside this branch. Unchanged:
- Per-request timeout scaling
- Exponential backoff on retryable status codes
- Context-exceeded cascade (primary → fallback_1 → fallback_2)
- Provider-health hooks
- Model override from `entry.model_id` OR context-cascade decision
- `augment_request_body` for metadata
- Provider-trait `parse_response`
- Cache store on success

**Error classification (three-tier per §2.5.3):**
- 200 with non-empty content → Ok.
- 400 context-exceeded → retry same entry with next fallback model (existing cascade). Not a tier decision.
- 400 non-context, retries exhausted, body matches provider-level model rejection ("not a valid model", "model not found", "unsupported model", "invalid model") → `RouteSkipped` (**post-ship W1 correction**). A sibling route entry with a different `model_id` can still succeed; the blunt old rule ("all 400 non-context = CallTerminal") bubbled OpenRouter's "gemma4:26b is not a valid model ID" and crashed fresh-install cascades. Implemented via `classify_pool_400` in `src-tauri/src/pyramid/llm.rs`.
- 400 non-context, retries exhausted, body matches feature-unsupported ("not supported", "unsupported") → `RouteSkipped`. A different provider / model may support the feature.
- 400 non-context + retries exhausted, body matches neither → `CallTerminal` (body shape bug: malformed JSON, multi-system-turns, schema violations; other routes would fail the same way).
- 401 / 403 + retries exhausted → `RouteSkipped` (this provider's credentials are stale/missing — openrouter-specific, not call-level). *Rationale: 401 on openrouter shouldn't bubble; ollama-local is still viable. If it truly is call-level, all routes will return 401 and the walker's "no viable route" exhaustion bubbles.*
- 404 + retries exhausted, body matches "model not found" / "no such model" / "unknown model" / "unsupported model" / "invalid model" → `RouteSkipped` (**post-ship W1 correction**; same argument as 400). Implemented via `classify_pool_404`.
- 404 + retries exhausted, body matches neither → `CallTerminal` (genuinely structural — unknown route path, deleted deployment).
- Configured retryable status → retry same entry up to `config.max_retries`.
- Retries exhausted on retryable status → `Retryable` (walker advances).
- Network/IO error → retry same entry; exhausted → `Retryable`.

**Per-route model resolution (post-ship C1 correction):** Entry `use_model` picks in order `entry.model_id` → `tier_routing[entry.tier_name]` (row's `provider_id` must match `entry.provider_id`) → context-cascade on `config.primary_model`. The walker previously shoveled `config.primary_model` across every entry, which leaked Ollama format names (e.g. `gemma4:26b`) onto the OpenRouter route and tripped its validator. See `resolve_route_model` helper.

### 4.4 compute_queue interaction

**Placement:** compute_queue enqueue (llm.rs:2084-2201) runs AFTER the fleet + market walker entries have had their turn and BEFORE the pool-provider branch's HTTP dispatch. Preserves today's ordering where fleet/market get first dibs and local-GPU scheduling is a layered primitive under the walker's pool branch.

**Today's `should_enqueue_local_execution` (llm.rs:100-113)** checks `route.providers.iter().any(|e| e.is_local)` — if ANY route entry is local, enqueue. That's a coarse pre-walker gate. Walker tightens to per-entry: "this specific pool-provider entry has `is_local == true` + `compute_queue.is_some()` + `!options.skip_concurrency_gate`" → enqueue; else direct HTTP.

**Concretely:** when the walker's pool-provider branch evaluates an entry with `is_local == true`, it checks the tightened gate. If enqueueing, it constructs a `QueueEntry` with:
- `config: self.config.prepare_for_replay(options.dispatch_origin)` — single source of truth for the replay config shape (§2.5.1). Drops `compute_queue` unconditionally; drops fleet+market contexts for non-Local origins.
- `options.skip_concurrency_gate = true` + `options.skip_fleet_dispatch = true` — explicit per-call overrides; `branch_allowed(Fleet/Market, origin)` already blocks those branches on non-Local origins via §2.5.2, but flags stay as defense-in-depth.
- `dispatch_origin` preserved on options.

The GPU loop dequeues, calls `call_model_unified_with_audit_and_ctx` again. The re-entry's walker sees:
- Fleet branch runtime-gate-fails via `branch_allowed(Fleet, origin)` for non-Local origins, or via `skip_fleet_dispatch=true` for Local-origin replays (user's own build cycling through the queue). Reason `fleet_replay_guard`. Advance.
- Market branch runtime-gate-fails via `branch_allowed(Market, origin)` for non-Local origins. For Local-origin replays, `compute_market_context` remains on the config (per prepare_for_replay Local arm) — but the outer walker already tried market on this call; the inner walker re-trying is wasted effort, not a correctness problem. **Optimization (Wave 1 nice-to-have):** set `options.skip_market_dispatch = true` on enqueue for Local origin if outer-walker's market entry already ran. Deferred; bounded by Wire's 30s quote TTL + `quote_already_purchased` idempotency.
- Pool-provider branch with `skip_concurrency_gate=true` bypasses `try_acquire_owned` and runs the HTTP retry loop directly against the local provider.

**Server.rs replay paths today** (fleet at 2025-2033, market at 3954-3962) clone `config` and hand-clear fleet fields. With §2.5.1 landed, those three sites (plus dadbear_supervisor's replay) all call `config.prepare_for_replay(dispatch_origin)` instead. Net: ~30 LOC deleted across four call sites; one authority for replay-config rules.

### 4.5 Concurrency (uniform across branches)

`for_each_concurrent(N)` at the chain level spawns N parallel dispatcher invocations. Each walker tries `route_to[0]` first; if that entry's `try_acquire` returns `Saturated`, the walker advances. If fleet's roster has K < N peers with headroom, K walkers dispatch on fleet and N−K advance to route_to[1]. Same semantic for market (Wire's advisory surface says K offers have headroom) and pool (semaphore has K slots).

**Walker never awaits capacity — but does await work.** Two distinct concerns:
- **Capacity acquisition** (`try_acquire_owned`): non-blocking, saturation advances to next entry. Walker never serializes the chain on a saturated pool.
- **Work completion** (fleet oneshot awaits up to `max_wait_secs` ≈ 3600s; market's `await_result(rx, wait_ms)` awaits up to `market_dispatch_max_wait_ms`): IS blocking, but bounded per entry. A walker whose dispatch succeeded then waits for the inference to complete. Per-walker timeout only; other walkers spun by `for_each_concurrent(N)` proceed independently.

### 4.5.1 Walkers-storm-market at high concurrency

Known issue (Stage 2 audit E3): with `for_each_concurrent(100)` and a market where the cheapest offer has queue capacity 10, all 100 walkers consult the same `MarketSurfaceCache` + call `/quote` against the same `top_of_book` offer. First 10 /purchase succeed; remaining 90 get 409 `quote_no_longer_winning`. 90 redundant `/quote` + `/purchase` round-trips per flight; 90 chronicle events.

V1 accepts this: Wire's `quote_no_longer_winning` handling is part of the contract; walker's `RouteSkipped` advance is correct; operator observability surfaces the rate. Wire may tie-break ties differently in future revs.

**V2 mitigation (deferred):** single-flight per-model semaphore on the market-branch acquire. Serializes walker market attempts per `canonical_model_id`, but allows different models to proceed in parallel. ~30 LOC. Ships after v1 measures actual race-loss rate; the fix is pre-measured only if the rate turns out to matter.

**Measurement hook (Wave 4 task addition):** add a per-day rollup chronicle event `walker_quote_race_stats` with `{model_id, quotes_issued, purchases_won, purchases_lost_to_race, race_loss_rate}` — surfaces whether v2 mitigation is needed.

---

## 5. Chronicle event vocabulary

All node-side events use `SOURCE_NETWORK` (for walker-driven market dispatch) or `SOURCE_FLEET`/pool-specific sources. The DispatchOrigin enum (0c4afd7) still carries per-entry source labeling into queue replays.

**New events (walker lifecycle):**

| Event | Fires when | Metadata |
|---|---|---|
| `network_route_skipped` | Runtime gate rejected entry before acquire. **NOT emitted** when reason is `fleet_replay_guard` / `market_replay_guard` — those are structural / expected under inbound-job replay and would create chronicle noise. Log-only at debug level. | `{entry_provider_id, reason: "policy_disabled" \| "tunnel_down" \| "tier_ineligible" \| "ctx_missing" \| ...}` |
| `network_route_saturated` | `try_acquire` returned Saturated | `{entry_provider_id, capacity_kind: "pool_semaphore" \| "fleet_peer_queue" \| "market_offer_queue"}` |
| `network_route_unavailable` | `try_acquire` returned Unavailable | `{entry_provider_id, reason}` |
| `network_route_retryable_fail` | Dispatch failed retryable; walker advanced | `{entry_provider_id, reason, status_code?}` |
| `network_route_terminal_fail` | Dispatch failed terminal (`CallTerminal`); walker bubbled | `{entry_provider_id, reason}` |
| `walker_resolved` | First-success dispatch completed (renamed from `cascade_resolved` — avoid collision with retired cascade plan's vocabulary) | `{entry_provider_id, latency_ms, total_walker_ms, attempts}` |
| `walker_exhausted` | All route entries exhausted; walker returning error | `{entries_tried, skip_reasons, final_error}` |
| `walker_path_distribution` | Daily rollup (emitted by a background tick) | `{day, fleet_count, market_count, pool_count, local_count}` |
| `network_quoted` | `/quote` 200 | `{offer_id, queue_position, rates, reservation_fee, estimated_total, quote_ttl_s}` |
| `network_purchased` | `/purchase` 200 | `{handle_path, uuid_job_id, dispatch_deadline_at, recovered_from_orphan?}` |
| `network_quote_expired` | `/purchase` 409 `quote_no_longer_winning` OR 401 `quote_jwt_expired` | `{reason}` |
| `network_purchase_recovered` | `/purchase` 409 `quote_already_purchased` with cached response reused | `{existing_job_id}` |
| `network_rate_above_budget` | `/quote` 409 `budget_exceeded` | `{estimated_total, max_budget}` (Wire reports the math) |
| `network_dispatch_deadline_missed` | `/fill` 409 `dispatch_deadline_exceeded` | `{dispatch_deadline_at, now}` |
| `network_provider_saturated` | `/fill` 503 `provider_depth_exceeded` / `provider_dispatch_conflict` | `{reason, retry_after_seconds}` |
| `network_balance_insufficient_for_market` | `/quote` or `/purchase` 409 `insufficient_balance` (RouteSkipped, not bubbled unless all routes exhaust) | `{need, have}` |
| `network_auth_expired` | `/quote` / `/purchase` / `/fill` 401 (RouteSkipped) | `{route, status_code}` |
| `dispatch_policy_superseded` | ConfigSynced listener observes new dispatch_policy active | `{prior_contribution_id, new_contribution_id, changes_summary}` |

**Chronicle-noise throttles:**
- `network_route_skipped reason=no_offer_for_model` — `network_route_skipped` is NOT emitted for this reason. Replaced with a per-build `network_model_unavailable` event emitted at most ONCE per (build_id, model_id) pair. Prevents 100× chronicle flood on batch builds against unserved models. (Stage 2B M6.)
- `network_route_skipped reason in {fleet_replay_guard, market_replay_guard}` — log-only, NOT chronicle (structural noise on queue replays). (Stage 2B E-2.)

**Existing events preserved:**
- `network_helped_build`, `network_result_returned`, `network_fell_back_local`, `network_late_arrival`, `network_balance_exhausted` (from compute_chronicle.rs:164-168). Semantics carry forward; reason slugs expand.
- Fleet events: `fleet_dispatched_async`, `fleet_result_received`, `fleet_result_failed`, `fleet_dispatch_timeout`, `fleet_dispatch_failed`, `fleet_peer_overloaded` — unchanged.
- Pool events: `cloud_returned`, `local_ollama_returned` — unchanged.

**Events retired (carryforward from superseded cascade plan):**
- `network_rate_above_cap` (premium filter) — DELETED. Wire's `/quote` 409 `budget_exceeded` replaces this with authoritative math.
- `network_rate_cap_unavailable` (cold cache) — DELETED. Walker treats cold cache as Unavailable and advances; no special chronicle.

---

## 6. MarketSurfaceCache

### 6.1 Contract and shape

Subscribes to Wire's `/api/v1/compute/market-surface` surface (rev 2.1 §3). Keeps an `Arc<RwLock<CacheData>>` on `PyramidState` with:

```rust
pub struct MarketSurfaceCache {
    data: Arc<RwLock<Option<CacheData>>>,
    last_refresh_at: Arc<RwLock<Instant>>,
}

pub struct CacheData {
    pub market: MarketSurfaceWhole,            // whole-market block (§3.1 `market`)
    pub models: HashMap<String, MarketSurfaceModel>,   // keyed by model_id
    pub generated_at: chrono::DateTime<chrono::Utc>,
}
```

Types mirror rev 2.1 contracts crate (`agent-wire-contracts/rust` after rev 2.1 types land there). Where contracts crate hasn't exported them yet, author in `market_surface_types.rs` and file PR to Wire dev to pull into contracts.

### 6.2 Refresh strategy — polling in v1

**V1: 60-second poll** aligned with Wire's `Cache-Control: public, max-age=60` (rev 2.1 §3.1). Tokio interval task spawned at boot:
- GET `/api/v1/compute/market-surface` (whole-market + all models).
- On 200, swap cache data. On failure, leave stale data in place; log.
- Respects Wire's cache headers — no bypass.

**V2 deferred: SSE stream** (rev 2.1 §3.4). `/api/v1/compute/market-surface/stream` with `?model_id=X` filter. Tokio task manages reconnect-via-re-GET (no Last-Event-ID replay per rev 2.1 §13a.1). **Why deferred:** SSE adds reconnect-state-machine complexity. Polling at 60s is sufficient for v1 — walker's `/quote` is the authoritative viability check, cache is advisory only. When multi-node operators or a multi-walker behem starts hitting the `30 concurrent SSE per IP` limit, we switch. Not before.

### 6.3 Walker usage

- On entry evaluation for `"market"` branch, `cache.get_model(canonical_model)` returns `Option<&MarketSurfaceModel>`.
- Walker only uses: `active_offers` (>0 check), `top_of_book.cheapest_with_headroom` (presence = someone has slot), aggregate `queue.total_capacity - queue.current_depth > 0`.
- Does NOT use: per-offer iteration, price math, reputation composition. All those stay on Wire side.

### 6.4 IPC exposure

- `pyramid_market_models` IPC returns a flattened `Vec<{model_id, active_offers}>` for Settings panel autocomplete.
- No other IPC surface needed — cache consumers are walker + Settings panel.

---

## 7. `ProviderPools::try_acquire_owned`

New method on `ProviderPools` (provider_pools.rs). Non-blocking; returns immediately.

```rust
pub fn try_acquire_owned(&self, provider_id: &str) -> Result<OwnedSemaphorePermit, AcquireError> {
    let pool = self.pools.get(provider_id)
        .ok_or(AcquireError::Unavailable("provider_not_in_pool".into()))?;

    // Rate-limiter check — non-blocking variant: if window is full, Saturated.
    // Current `SlidingWindowLimiter::wait` is blocking; we add `try_acquire` that
    // reports window-full without sleeping.
    if let Some(ref limiter) = pool.rate_limiter {
        if !limiter.try_acquire() {
            return Err(AcquireError::Saturated);
        }
    }

    pool.semaphore.clone()
        .try_acquire_owned()
        .map_err(|_| AcquireError::Saturated)
}
```

**SlidingWindowLimiter gains `try_acquire()`** — synchronous, evicts expired entries, returns `bool`. If `false`, walker advances. Pattern parallels the existing `wait()` method at provider_pools.rs:41-71 but returns immediately.

**Existing `acquire()` method stays** — used by queue replays and any other path that genuinely should block (the compute_queue GPU loop serialized one-at-a-time).

---

## 8. Implementation waves

Sized to a serial implementer pattern (one agent per wave; audit after Wave 1, verifier+wanderer after Wave 3, full-feature wanderer after Wave 4). No parallel agents on the same file per `feedback_no_worktrees` + `feedback_parallel_agent_atomicity`.

### Wave 0 — prereqs + systemic helpers (~650 LOC)

**Seeding + hydration (walker data source):**

1. **Bundle `dispatch_policy-default-v1` contribution family** in `src-tauri/assets/bundled_contributions.json`. Ship FOUR entries (same pattern as other schema types — evidence_policy, build_strategy, etc.):
   - `bundled-schema_definition-dispatch_policy-v1` — JSON Schema body describing the YAML shape (~60 LOC JSON). Enables Settings panel validation on save + future Tools-wizard wizard editing.
   - `bundled-schema_annotation-dispatch_policy-v1` — UI annotation (labels, widgets, visibility) for the Tools wizard (~40 LOC YAML). Even though v1 Settings uses a custom panel, annotation is needed for consistency + future wizard path.
   - `bundled-skill-generation-dispatch_policy-v1` — LLM generation prompt for intent→YAML synthesis (~30 LOC markdown). Enables "describe the routing you want" → YAML autogen through the generative config pattern.
   - `bundled-dispatch_policy-default-v1` — default seed:
   ```yaml
   version: 1
   provider_pools:
     openrouter: { concurrency: 20, rate_limit: { max_requests: 60, window_secs: 60 } }
     ollama-local: { concurrency: 1 }
   routing_rules:
     - name: default
       match_config: {}
       route_to:
         - { provider_id: market }
         - { provider_id: fleet }
         - { provider_id: openrouter }
         - { provider_id: ollama-local, is_local: true }
       sequential: false
   # escalation/wait fields retired — walker never awaits capacity (§4.5).
   # max_wait_secs per-entry work-await is sourced from compute_participation_policy.market_dispatch_max_wait_ms.
   ```
   The dispatcher arm at `config_contributions.rs:780` routes the contribution to `db::upsert_dispatch_policy`. Both already exist — Wave 0 task 1 only ADDS the four bundle entries.

2. **Write `sync_dispatch_policy_to_operational`** in `wire_migration.rs`. Mirrors `sync_chain_defaults_to_operational` at 1449. Reads active `dispatch_policy` contribution → parses YAML → calls `db::upsert_dispatch_policy`. Invoked from `walk_bundled_contributions_manifest` post-bundled-insert.

3. **Boot hydration check (NO new code needed).** main.rs:11824-11887 already reads `pyramid_dispatch_policy` operational and populates `LlmConfig.dispatch_policy` + `LlmConfig.provider_pools`. The ConfigSynced listener at main.rs:11889+ rebuilds on contribution update. Wave 0's responsibility is ONLY to ensure the bundled seed lands in `pyramid_config_contributions` (task 1) AND `sync_dispatch_policy_to_operational` writes to the operational table at boot (task 2). Once both run, hydration-already-works finds the policy and populates pools. Verify via smoke: fresh-DB boot logs `"Dispatch policy loaded from DB — per-provider pools active, compute queue wired"` (the existing tracing::info at main.rs:11850).

**Systemic helpers (§2.5) — land before walker body so Wave 1+ calls into them:**

4. **`LlmConfig::prepare_for_replay(origin)`** (§2.5.1) — new method on LlmConfig in `src-tauri/src/pyramid/llm.rs` near the existing `impl LlmConfig` block. ~25 LOC including doc. **Same-commit update all four call sites** that today hand-clear fields:
   - `src-tauri/src/pyramid/llm.rs` ~2104-2108 (queue-enqueue path) — replace the ad-hoc `gpu_config.compute_queue = None; gpu_config.fleet_roster = None; gpu_config.fleet_dispatch = None;` with `let gpu_config = config.prepare_for_replay(options.dispatch_origin);`
   - `src-tauri/src/server.rs` ~2025-2033 (FleetReceived inbound worker) — replace the hand-clear with `let fleet_config = cfg.prepare_for_replay(DispatchOrigin::FleetReceived);`
   - `src-tauri/src/server.rs` ~3954-3962 (MarketReceived inbound worker) — replace with `let worker_config = cfg.prepare_for_replay(DispatchOrigin::MarketReceived);`
   - `src-tauri/src/pyramid/dadbear_supervisor.rs` replay path (grep for `compute_queue = None` or `LlmCallOptions` construction) — apply same pattern.
   Unit test: construct LlmConfig with all four contexts present; call `prepare_for_replay(MarketReceived)`; assert all three (`fleet_dispatch`, `fleet_roster`, `compute_market_context`) are None AND compute_queue is None. Repeat for FleetReceived. Assert Local-origin only clears `compute_queue`.

5. **`branch_allowed(branch, origin)` + `RouteBranch` enum + `classify_branch(provider_id) -> RouteBranch`** (§2.5.2) — new in `src-tauri/src/pyramid/llm.rs` near the top with other dispatch helpers (~25 LOC). `classify_branch` maps `"fleet"` → `RouteBranch::Fleet`, `"market"` → `RouteBranch::Market`, anything else → `RouteBranch::Pool`. Unit tests: all 3×3 branch×origin combinations (9 tests); asserts Pool always allowed, Fleet/Market only Local.

6. **Three-tier `EntryError` enum** (§2.5.3) — new in `src-tauri/src/pyramid/llm.rs` near walker types (~15 LOC). Variants: `Retryable { reason }`, `RouteSkipped { reason }`, `CallTerminal { reason }`. Doc-comment each variant with its walker semantic. No call sites yet — walker body in Wave 1 uses it.

**Primitives for walker body (used Wave 1+):**

7. **Add `ProviderPools::try_acquire_owned`** + `SlidingWindowLimiter::try_acquire`. Unit tests for both (parallels existing `test_acquire_known_provider`). Confirms Tokio's native `Arc<Semaphore>::try_acquire_owned()` API works as documented — if the `tokio` version in Cargo.toml doesn't expose it, escalate before Wave 1.

8. **Author `compute_quote_flow` module skeleton** (`src-tauri/src/pyramid/compute_quote_flow.rs`):
   - `ComputeQuoteBody`, `ComputeQuoteResponse`, `ComputePurchaseBody`, `ComputePurchaseResponse`, `ComputeFillBody` types (mirror contracts crate rev 2.1 shapes).
   - `quote()`, `purchase()`, `fill()`, `await_result()` stubs returning `unimplemented!()` — body in Wave 3.
   - **NO `resolve_uuid_from_purchase`** — rev 2.1 `/purchase` 200 returns `request_id: UUID` directly per contract §1.6b. Walker uses `purchase_resp.request_id` for PendingJobs key. The rev-2.0 workaround is dead.
   - Error taxonomy extension — new variants for rev 2.1 slugs (see §4.2 table).

9. **Author `MarketSurfaceCache` skeleton** (`src-tauri/src/pyramid/market_surface_cache.rs`):
   - Types per §6.1.
   - `new()`, `get_model()`, `refresh_now()` methods.
   - Boot-task spawn stub — interval body in Wave 4.

10. **Verifier pass** — confirms bundled contribution seeds correctly on fresh-DB boot, dispatch_policy operational row populated, `LlmConfig.dispatch_policy` non-None after boot, `prepare_for_replay` + `branch_allowed` + `EntryError` all tested.

### Wave 1 — walker shell + pool-provider branch (~400 LOC)

8. **Introduce walker loop in `call_model_unified_with_audit_and_ctx`** replacing the Phase D escalation loop (llm.rs:2231-2281). Walker iterates `route.providers`. For THIS wave only: walker handles ONLY pool-provider entries; `"fleet"` and `"market"` entries are skipped with `network_route_skipped reason="wave1_not_implemented"`. Phase A fleet and Phase B market pre-loop blocks stay UNCHANGED. This is a compile-compatible intermediate state — pool escalation goes through the walker, other paths via legacy blocks.

9. **Move HTTP retry loop (lines 2350-2830) into pool-provider branch.** Each pool entry runs its own retry loop; model override from `entry.model_id`; context-exceeded cascade; provider-health hooks unchanged.

10. **`try_acquire` abstraction** for pool entries wraps `pools.try_acquire_owned(&entry.provider_id)`. Saturation → `network_route_saturated` + advance. NotInPool → `network_route_unavailable` + advance.

11. **Audit row schema + exit lifecycle (schema migration).** Today's `pyramid_llm_audit` (db.rs:997-1018) has columns `id, slug, build_id, node_id, step_name, call_purpose, depth, model, system_prompt, user_prompt, raw_response, parsed_ok, prompt_tokens, completion_tokens, latency_ms, generation_id, status, created_at, completed_at, cache_hit`. **No `provider_id` column exists.**
    - **Schema migration** — ADD COLUMN `provider_id TEXT` (nullable; legacy rows stay NULL). Follow the idempotent `pragma_table_info` pattern at db.rs:1038-1049 (used for `cache_hit` addition).
    - **Extend BOTH `complete_llm_audit` and `fail_llm_audit` signatures** — add `provider_id: Option<&str>` as final parameter. UPDATE statements both add `provider_id = ?N`. Legacy callers (tests, pre-walker paths) pass None.
    - **Walker invocation — three outcomes:**
      - Success: `complete_llm_audit(..., Some(winning_entry.provider_id.as_str()))` with the WINNING entry's provider_id (the one that produced the successful response).
      - CallTerminal: `fail_llm_audit(..., error_message, Some(last_attempted_entry.provider_id.as_str()))` with the LAST-ATTEMPTED entry's provider_id (the one that raised CallTerminal — useful for debugging which branch rejected).
      - Exhaustion (`no viable route`): `fail_llm_audit(..., "no viable route", None)` — no entry produced a terminal outcome; walker iterated all.
    - **`model` column preservation** — NEVER overwrite `model` with a routing sentinel (`"fleet"` / `"market"`). `model` is the canonical model name (preserved from insert); `provider_id` is new and carries the routing sentinel.
    - **DB column constraints** — `provider_id` is nullable TEXT with no CHECK. Accepts `"market"`, `"fleet"`, or any registered provider row's id.
    - **Downstream consumers** — audit the Oversight page, cost reconciliation, and any query keyed on this table. Add `provider_id` to projections that currently expose `model` for routing analytics.
    - **Multi-attempt tracking (NOT in this wave)** — walker writes ONE row per call (winning entry or terminal entry). Multi-entry attempts live in `pyramid_compute_events` chronicle, joined on `(slug, build_id, step_name, timestamp-window)`. Full split into parent+attempts tables is deferred post-walker per §15.

12. **Verifier pass** — confirms pool-provider path works through walker (fresh build on Playful node; cargo check + cargo test --lib clean).

### Wave 2 — fleet branch inlined (~400 LOC)

13. **Extract Phase A fleet dispatch (llm.rs:1248-1725)** into `dispatch_fleet_entry(entry, config, ctx, ...)`. Internal logic unchanged: snapshot fleet_ctx + policy + callback_url; `find_peer_for_rule` with staleness gate; JWT fetch; `fleet_dispatch_by_rule`; oneshot register; two-phase timeout await with grace; chronicle by result class; roster cleanup on is_peer_dead.

14. **Walker fleet branch**: when `entry.provider_id == "fleet"`:
    - Runtime gate per §4.1.
    - Acquire via `roster.find_peer_for_rule` per §4.1.
    - Call `dispatch_fleet_entry`; classify result.

15. **Remove Phase A pre-loop block (llm.rs:1248-1725) + `fleet_filter` at 1727-1730.** Fleet is now an in-walker branch.

16. **`skip_fleet_dispatch` guard is now secondary.** Primary fleet-replay block is `branch_allowed(Fleet, origin)` (§2.5.2, landed Wave 0 task 5). The explicit flag stays as a per-call override for tests + explicit disablement; walker's fleet runtime gate checks both. Reason slug remains `fleet_replay_guard`.

17. **Update `resolve_local_for_rule` at `dispatch_policy.rs:238-253`** — filter `provider_id == "market"` alongside existing `"fleet"` filter (both walker sentinels; neither is a real local handler).

18. **Verifier pass** — Playful dispatches via fleet through walker; fleet roster smoke still works; no regression on Chronicle tab.

### Wave 3 — market branch inlined + `compute_quote_flow` implementation (~700 LOC)

19. **Implement `compute_quote_flow::{quote, purchase, fill, await_result}` bodies.**
    - HTTP via `send_api_request` pattern (same as compute_requester.rs).
    - Response parsing using contracts-crate types when available; inline types otherwise.
    - Error classification — extended taxonomy per §4.2 table (11 new rev 2.1 slugs; three-tier classification).
    - Idempotency-Key header on /purchase (UUID per call) + /fill (reuse /purchase's request_id).

20. **PendingJobs UUID resolution.** `/purchase` 200 response shape from Wire (confirmed by grep of `GoodNewsEveryone/src/app/api/v1/compute/purchase/route.ts:250+565+693`): `{job_id: <handle-path>, request_id: <fresh-idempotency-uuid>, dispatch_deadline_at}`. The DB-row UUID (what the inbound delivery envelope carries) lives in `commit.job_id` Wire-side but is **NOT** in the response body. Walker paths:
    - **Preferred path (A) — Wire adds `uuid_job_id` to response** — Wire-dev Q5 filed in §9. Wire-side change ~2 LOC (include `commit.job_id` in response.json). Walker reads `purchase_resp.uuid_job_id` directly; no round-trip. This is the clean fix; ship this if Wire dev turns around Q5 before Wave 3 implementation starts.
    - **Fallback path (B) — walker polls `/api/v1/compute/jobs/:handle_path`** — same pattern as today's `resolve_uuid_from_handle` (compute_requester.rs:589-616). One extra GET per /purchase. Not ideal but ships without Wire coordination.
    - `compute_quote_flow::resolve_uuid_from_purchase(purchase_resp, auth, config)` implements (A)-if-present-else-(B). Let Wire's Q5 resolution decide the hot path at implementation time.

20a. **PendingJobs register after UUID resolution** — `pending_jobs.register(uuid_job_id)`. Keyed on DB-row UUID so inbound delivery envelope lookups hit. Walker must NOT register by `purchase_resp.request_id` (idempotency token, wrong key, delivery would never fire the oneshot).

21. **Extract Phase B market dispatch (llm.rs:1732-2082)** into `dispatch_market_entry(entry, config, ctx, ...)`. Internal: three RPCs back-to-back, register PendingJobs by `purchase_resp.request_id` (UUID), await oneshot with `market_dispatch_max_wait_ms` policy field (today's value at llm.rs:1897).

22. **Walker market branch**: when `entry.provider_id == "market"`:
    - Runtime gate per §4.2 — `branch_allowed(Market, origin)` from §2.5.2 (Wave 0 task 5) + specifics.
    - Acquire via `market_surface_cache.get_model(canonical_model)` advisory check per §4.2.
    - Call `dispatch_market_entry`; classify per §4.2 table.

22a. **Latent-bug unit test + regression guard.** The market-replay bug (server.rs replay clones preserved `compute_market_context`) is closed at source by §2.5.1 (Wave 0 task 4 — `prepare_for_replay` clears it for non-Local origins) AND by §2.5.2 (Wave 0 task 5 — `branch_allowed(Market, non-Local)` returns false). Wave 3 just adds the regression tests: construct `LlmCallOptions { dispatch_origin: MarketReceived, ... }`, invoke walker, assert market branch runtime-gates as `market_replay_guard` and never calls `/quote`. Same for FleetReceived. Belt-and-suspenders — both independent guards are tested.

23. **Remove Phase B pre-loop block (llm.rs:1732-2082) + standalone `should_try_market` fn (llm.rs:167-210)** + `classify_soft_fail_reason` + `sanitize_wire_slug` (helper functions used only by Phase B) + `NetworkHandleInfo` struct.

24. **Rewrite `emit_network_helped_build` + `emit_network_fell_back_local`** to use `NetworkHandleInfo` sourced from `/quote` + `/purchase` response fields instead of today's MarketRequestHandle.

25. **Remove `market_dispatch_eager` + `market_dispatch_threshold_queue_depth` from walker runtime gate** — NOT Wave 5. With walker + `/quote` as authoritative viability check, these fields are vestigial. Walker market branch never reads them. Fields stay on `ComputeParticipationPolicy` struct for serde-compat until Wave 5 deletion, but they have no effect. **`market_dispatch_max_wait_ms` is preserved and load-bearing** — walker uses it as the oneshot-await timeout on `compute_quote_flow::await_result` (same semantic as today's llm.rs:1897 use).

26. **compute_requester.rs deprecation** — mark module `#[deprecated]` with migration note pointing at compute_quote_flow. Keep exports for one rev (e.g. any stragglers); Wave 5 deletes.

27. **Verifier pass + wanderer pass** — end-to-end network dispatch via walker; premium filtering delegated to Wire's /quote; chronicle shows `network_quoted → network_purchased → network_helped_build` sequence; failure modes classified correctly.

### Wave 4 — MarketSurfaceCache + Settings panel (~600 LOC)

28. **Implement `MarketSurfaceCache` polling loop** (§6.2). Tokio interval task spawned after tunnel-connect at main.rs; GET /market-surface every 60s; swap-in-place; error-tolerant.

29. **Expose `pyramid_market_models` IPC** — returns flattened model list for UI autocomplete.

30. **New component `src/components/settings/InferenceRoutingPanel.tsx`** (~400 LOC):
    - Edits active dispatch_policy's `routing_rules[0].route_to` via existing `pyramid_active_config_contribution` + `pyramid_supersede_config` IPCs.
    - Drag-reorder via up/down buttons (native drag-drop deferred).
    - Per-row enable/disable + expand for sub-config.
    - Market row sub-config: `max_wait_ms` (readonly display), link to Wire's observability dashboard.
    - Discovery section: reads MarketSurfaceCache, shows model_ids added since last review (localStorage bookmark).

31. **Insert panel into Settings.tsx** above the existing Ollama section. Renders read-only with banner when `compute_participation_policy.mode == "local"`.

32. **Invisibility UI copy audit**: never "offers", "premium", "market" in operator-facing labels. Use "network compute", "models available", "rate ceiling".

33. **Debounce saves**: drag-reorder saves only on Apply; field edits save on blur. Prevents supersession flood that would hot-reload `provider_pools` unnecessarily.

34. **Verifier pass** — panel renders, reorders persist, walker picks up new order on next call, fresh install shows sane defaults.

### Wave 5 — cleanup + deprecation enforcement (~200 LOC)

35. **Remove `market_dispatch_eager` + `market_dispatch_threshold_queue_depth`** from ComputeParticipationPolicy struct + schema_definition + annotation + bundled seed rows. Force stragglers to compile-fail.

36. **Delete `compute_requester.rs`** and all its re-exports. Grep `compute_requester` in src/ must be empty post-delete.

37. **Audit all string-match sites for `"fleet"`** + add parallel `"market"` branches where semantics match. Sites to audit: `dispatch_policy.rs:238-253` (resolve_local_for_rule), `fleet_mps.rs:319+` (serving-rule derivation), `resolve_tier` paths. Document in commit message.

38. **Permit release test**: unit test confirming walker advance drops the acquired permit cleanly (Drop guard on OwnedSemaphorePermit). Important for pool-provider entries where a retryable failure releases the permit before the next entry's acquire.

39. **Final full-feature wanderer** — end-to-end on fresh install + rebuilt from Playful: dispatch via walker through market, fleet, pool; saturation advances; failure modes; Settings panel reorder takes effect; BEHEM Chronicle tab shows correct source labels (DispatchOrigin fix preserved).

**Total: ~2950 LOC across 6 waves** (added ~150 LOC for §2.5 systemic helpers + test coverage; net negative on call-site LOC due to `prepare_for_replay` consolidating 4 ad-hoc sites). Realistic 3-5 sessions including audit + verify + wander + compile cycles. Single operator wiping — no migration cost.

**Audit history:**
- **Rev 0.1** (2026-04-20) — initial draft.
- **Stage 1 audit** (2026-04-20) — two informed auditors, consolidated findings. Critical: C1 audit-row provider_id signature; C2 /fill body Wire-dev Q4; C3 dead balance gate; C4 /fill uses request_id not job_id; C5 delete resolve_uuid_from_purchase; C6 strip compute_market_context on replay. Plus 9 major + minor findings.
- **Rev 0.2** (2026-04-21) — three systemic fixes folded in: §2.5.1 `prepare_for_replay` (closes C6 + replay-config drift), §2.5.2 `branch_allowed` (closes market-replay bug at config layer), §2.5.3 three-tier `EntryError` (fixes misclassification of insufficient_balance). Wave 0 expanded 7→10 tasks; Wave 3 task 20 deleted (resolve_uuid_from_purchase); task 22a reframed as regression test for systemic guards. §4.2 error table fully revised for three-tier. §4.3 pool-branch classification updated. §4.4 compute_queue interaction rewritten using prepare_for_replay.
- **Stage 2 audit** (2026-04-21, 2B only; 2A didn't return) — 5 critical + 12 major + 10 minor + 3 edge findings. Verified by Adam directly against source: several major items confirmed, C2 part (b) invalid. Key new finds: audit-row schema has NO `provider_id` column (C1 was worse than rev 0.2 understood); `escalation_timeout_secs` becomes a dead knob with walker (M1); rev 0.2's `resolve_uuid_from_purchase` deletion was a regression — `request_id` is NOT the DB-row UUID (verified at Wire purchase route.ts:250+565+693); queue-replay Local origin redundantly /quotes (M4); `max_budget` sentinel eliminates cost-cap enforcement (M11); walkers-storm-market at high concurrency (E3).
- **Rev 0.3** (2026-04-21) — 15 findings folded in:
  1. **C1 schema migration** (Wave 1 task 11): explicit ALTER TABLE `pyramid_llm_audit ADD COLUMN provider_id TEXT`; extend `complete_llm_audit` signature. `model` column preserved intact (never overwritten).
  2. **request_id ≠ UUID fix**: Wire-dev Q5 added (add `uuid_job_id` to /purchase response, ~2 LOC Wire-side); fallback is node-side GET /jobs/:handle-path poll. `resolve_uuid_from_purchase` helper restored.
  3. **`prepare_for_replay` origin-independent** (§2.5.1): clears `compute_queue` + `fleet_*` + `compute_market_context` for ALL origins. Closes queue-replay redundant /quote (M4).
  4. **`max_budget_credits: Option<i64>` on `RouteEntry`**: per-entry cost cap field. Walker sources `/quote` `max_budget` from entry config; defaults to `(1i64 << 53) - 1` sentinel if absent. Fixes M11 cost-cap hole.
  5. **401 reclassification** (§4.2): `/quote` / `/purchase` / `/fill` 401 → `RouteSkipped` not `CallTerminal`. Wire auth doesn't invalidate fleet + openrouter.
  6. **`escalation_timeout_secs` retired** (M1): removed from bundled seed + `ResolvedRoute` references. Walker's `try_acquire_owned` advances immediately on saturation; no timeout-wait semantic. Document loudly in release notes.
  7. **`market_dispatch_eager` / `market_dispatch_threshold_queue_depth` gate removed in Wave 3** (M3): not Wave 5. Fields stay on struct for serde-compat until Wave 5 deletion, but walker never reads them.
  8. **Bundle schema_definition + schema_annotation + generation skill** in Wave 0 task 1 (M2): full schema family for dispatch_policy, not just the default seed.
  9. **Audit-row behavior change documented** (C3): §2 "walker adds" notes fleet + market successes now write rows that legacy didn't. Positive (fills observability gap).
  10. **Chronicle throttles** (M6 + E-2): `network_route_skipped reason=no_offer_for_model` replaced with per-build `network_model_unavailable` emitted at most once per `(build_id, model_id)`. `fleet_replay_guard` / `market_replay_guard` skips are log-only, NOT chronicle.
  11. **`skip_market_dispatch` forward-ref removed** (M9): obviated by §2.5.1 symmetric clearing.
  12. **§4.5 concurrency language clarified** (M12): distinguishes "capacity acquisition" (never blocks) from "work completion" (bounded blocking per entry).
  13. **`cascade_resolved` → `walker_resolved`** (m-6): renamed to avoid collision with retired cascade plan's vocabulary. Added `walker_exhausted` + `walker_path_distribution` + `walker_quote_race_stats`.
  14. **Wave 5 retired-event consumer grep** (M5): task added to sweep frontend/UI consumers of `network_rate_above_cap`, `network_rate_cap_unavailable`.
  15. **E3 walkers-storm documented** (§4.5.1): v1 accepts; v2 mitigation (single-flight per-model semaphore) deferred with measurement hook added to Wave 4.

---

## 9. Wire-dev questions (open; non-blocking for plan approval)

Most of my initial questions were answerable from reading `wire-node-compute-market-contract.md` (rev 2.1) directly — those have been resolved below. The remaining four are genuinely Wire-dev-only.

**Answered by reading the contract doc (informational — kept for trace):**

- ~~`/purchase` UUID resolution~~ — **CORRECTED rev 0.3.** Contract §1.6b example `request_id: "<uuid>"` is a fresh Wire-minted idempotency token, NOT the DB-row UUID. Verified against Wire `src/app/api/v1/compute/purchase/route.ts:250` (`const requestId = randomUUID();`) + :565 (`uuid_job_id: commit.job_id` — separate field in chronicle emit) + :693-694 (response body has only `{job_id, request_id, dispatch_deadline_at}` — no `uuid_job_id`). **New Q5 asks Wire to add `uuid_job_id` to response body**; fallback is node-side poll.
- ~~`/fill` body shape~~ — **ANSWERED** by §1.8 body example. Fields: `job_id` OR `request_id` (UUID preferred), `messages`, `max_tokens` (optional, ≤ quoted), `temperature`, `relay_count`, `requester_callback_url`, `idempotency_key`. **SUB-QUESTION** → see Q4 below.
- ~~`quote_already_purchased` response body~~ — **ANSWERED.** §1.6b: "If `idempotency_key` matches, returns 200 with cached response instead." Walker treats as normal success when idempotency_key matched, proceeds to /fill.
- ~~`dispatch_deadline_exceeded` body~~ — **ANSWERED** by spec §2.3 + contract §1.8. Body is `{dispatch_deadline_at, now}`. Walker correlates via its own originally-sent `job_id`.
- ~~Reservation fee rotator race~~ — **ANSWERED.** §1.6b: `plan_compute_match` runs FOR UPDATE re-check BEFORE reservation debit. `quote_no_longer_winning` → no fee consumed. `insufficient_balance` at re-check → no fee consumed.
- ~~/match 410 Gone window~~ — **ANSWERED.** §1.7: "for one release, then deleted entirely." Rev 2.1 just shipped (commit 1adb3f20); walker is 2-3 sessions out. Wire dev's next release window is the deletion cutoff. Coordinate timing if walker stalls.
- ~~`compute_purchase_rejected` emission boundaries~~ — **ANSWERED** by §1.6b: fires on 4xx with `{error, detail, quote_id}`. Scoped to all 4xx (no sub-scoping like /quote). Acceptable walker noise given /purchase is a commit action.

**Still open (ask Wire dev):**

1. **`/market-surface/stream` filter awareness.** §1.6c says `?model_id=X` filter is supported. Does the server-side EventEmitter broadcast only matching events to a filtered subscriber (server-side filter), or does every subscriber receive all events and client filters locally? Affects whether walker's v2 SSE cache (deferred) subscribes once per model or once globally.

2. **SSE rate-limit bucket boundaries.** §1.6c says 30 concurrent per IP. Does Wire share the 30-limit across JUST `/market-surface/stream` connections, or across other SSE endpoints too (e.g. delivery worker channels on `/v1/compute/...` return paths)? Affects whether multi-node operators behind NAT run into it at scale.

3. **`idempotency_key` TTL rollover behavior.** §1.6b: "1hr TTL per `(operator_id, key)`, replay returns cached response." If a walker retries /purchase with the same key after >1hr (e.g. compute_queue held the request overnight), does Wire (a) treat as live request (key expired), or (b) keep the mapping until `first_purchased_at + X` even past the TTL? Walker never retries cross-hour today, but compute_queue-replayed purchases may bump this window.

4. **`/fill` body — are `input_token_count` and `privacy_tier` still valid fields in rev 2.1?** Rev 2.0's body had `input_token_count` + `privacy_tier` (per compute_requester.rs:522-528 today). Contract §1.8 rev 2.1 example shows `{job_id, request_id, messages, max_tokens, temperature, relay_count, requester_callback_url, idempotency_key}` — `input_token_count` + `privacy_tier` not in the example. Spec §2.3 says body "matches today's shape with one semantic change (max_tokens optional)," implying no field drops. Clarifying: are the two fields still sent and honored silently, or should the walker drop them? **BLOCKS Wave 3** — walker can't construct `compute_quote_flow::fill` body without the answer.

5. **Add `uuid_job_id` to `/purchase` 200 response body.** Today's response is `{job_id: <handle-path>, request_id: <fresh-idempotency-uuid>, dispatch_deadline_at}`. The DB-row UUID (what the provider-side delivery envelope carries, what node's inbound `/v1/compute/job-result` handler looks up) is `commit.job_id` Wire-side — already populated to the chronicle emit, just not in the response body. **Requested change:** include `uuid_job_id: commit.job_id` in the response `Response.json({...})` at `src/app/api/v1/compute/purchase/route.ts:691-696`. ~2 LOC change. Saves walker one round-trip per dispatch (the `/jobs/:handle-path` poll fallback). If Wire can turn this around before Wave 3 implementation, walker ships cleaner; otherwise fallback path works.

---

## 10. Explicitly NOT in scope

- SSE live stream for MarketSurfaceCache (deferred; polling suffices for v1)
- Deferred/cron/event trigger types on `/purchase` (§0.3 — v1 is `immediate` only)
- Standing quotes (§0.3 — spot quotes only)
- Recursive cost composition (§0.3 — compute is a leaf)
- Reputation surface + `min_reputation` filter (Phase B — Wire inert at launch)
- Relay market (separate market, coming phase)
- Chronicle-event-catalog CHECK expansion coordination (Wire-side, already shipped at rev 2.1 per §4.5)
- Per-tier route_order (v1 single global route_to per rule)
- Tagged-enum `Mechanism::Fleet | Market | Provider(id)` refactor (flat-string accepted per decisions-locked)
- Adding `compute_cascade` new schema_type (superseded plan direction; dissolved by Wire rev 2.1)
- Wire-side contract changes (rev 2.1 is frozen at `1adb3f20`; any changes become rev 2.2)
- Rev-0.6.1 DB migration preservation (Adam wipes; single operator)
- `/match` re-introduction for legacy compat
- JSON Schema validator wiring for dispatch_policy YAML
- Native HTML5 drag-drop in Settings panel (up/down buttons for v1)
- Per-entry timeout overrides (walker uses today's global `escalation_timeout_secs` / `max_wait_secs`)
- Embeddings / multimodal-pyramid extensions (separate future work)

---

## 11. Known tradeoffs (documented, accepted)

1. **Cold-cache market entries advance silently** — a fresh install's first call finds MarketSurfaceCache empty; walker's market branch returns `Unavailable(reason="market_cache_cold")` and advances. Subsequent calls (post-60s refresh) see populated cache. One extra cascade-step latency on boot; acceptable.

2. **Advisory-only cache** — `/quote` is the authoritative viability check. Walker may attempt /quote against a model the cache thinks has no offers (if cache is stale). Wire's `no_offer_for_model` then fires, walker advances. Wasted one RPC round-trip on stale cache; bounded by 60s TTL.

3. **Flat-string provider_id sentinels** (`"fleet"` / `"market"`) — extended from today's `"fleet"` overload. 3 string-match sites audited + updated in Wave 5. Tagged enum refactor deferred to future simplification pass.

4. **`compute_queue` re-entry with walker** — queue replay re-enters `call_model_unified` with skip flags set; walker's fleet + market branches respect `skip_fleet_dispatch`, pool branch respects `skip_concurrency_gate`. Two layers of walker invocation (outer + queue-replay inner) — slightly confusing call graph, unchanged from today.

5. **Parallel-purchase speculation NOT implemented in v1** — walker does one `/quote → /purchase → /fill` per market entry. Parallel-purchase across routes (speculation width) forfeits reservation fees by design per rev 2.1 §1.1; v1 chooses simplicity over maximum throughput.

6. **`max_tokens_exceeds_quote` is walker-bug-shaped** — if this fires, walker passed a `max_tokens` on /fill that exceeds what `/quote` quoted. Walker should always omit `max_tokens` on /fill (let Wire use `max_tokens_quoted`). If future code paths pass a tighter value, they must match or undercut quoted. Unit test + assertion.

7. **Polling overhead for MarketSurfaceCache** — one GET /market-surface per 60s per node. With 100 nodes active, Wire sees 100 req/min baseline. Cache-Control allows edge caching; not a real concern at current scale but an observability item.

8. **Market replay guard is NEW in walker, not in today's Phase B.** Today's Phase B has a latent bug where a MarketReceived or FleetReceived inbound job's queue-replay could re-dispatch through `/match` because server.rs preserves `compute_market_context` on replay. In practice this hasn't triggered a production loop (Wire's quote_already_purchased + same-operator matching + other coincidences), but the walker's explicit `dispatch_origin == Local` gate closes it at source. If a pre-walker regression surfaces before the walker ships, file a narrow Phase B fix upstream of this plan.

9. **Retro-item carryforward from superseded plan**:
   - Audit hygiene: never taint auditor prompts with prior-stage summaries or verdict nudges.
   - Minimalism: find existing primitive, extend one enum value. Cascade plan's `compute_cascade` new primitive was a minimalism failure.
   - Scope estimation: read actual source-line counts, not function signatures. This plan estimates from real reads of llm.rs + compute_requester.rs.
   - Cross-repo symptom-to-systemic: Wire rev 2.1 dissolved ~60% of the cascade plan's node-side derivation work. Ask "does the other repo have direct access?" before building node-side fabrication.

---

## 12. Acceptance criteria

- Walker replaces Phase A + Phase B + Phase D with one iteration over `route.providers`.
- `route.providers` is THE cascade; entries are `"fleet"`, `"market"`, and any provider_id from `pyramid_providers`.
- Fleet saturation (no peer with headroom) → walker advances.
- Market saturation (cache reports `active_offers == 0` or all queues at max) → walker advances.
- Pool saturation (`try_acquire_owned` returns `TryAcquireError`) → walker advances.
- `/quote` 409 `budget_exceeded` → walker advances (no hardcoded premium filter node-side).
- `/purchase` 409 `quote_no_longer_winning` → walker advances.
- `/fill` 409 `dispatch_deadline_exceeded` → walker advances.
- `/quote` / `/purchase` / `/fill` 401 → terminal, bubble to caller.
- Fresh install reads bundled `dispatch_policy-default-v1`, runs a build, chronicle shows walker-driven dispatch.
- Settings panel reorders routes; supersession fires; walker respects new order on next call.
- `compute_requester.rs` deleted; all call sites use `compute_quote_flow`.
- `cargo check` default target green; `cargo test --lib llm dispatch_policy compute_quote_flow market_surface_cache provider_pools` green; `npm run build` green.
- Grep: zero callers of `call_model_via_registry` (deleted in prior session at b7ad65f); zero callers of `should_try_market` (inlined); zero callers of `compute_requester::*` (deleted).
- BEHEM Chronicle tab shows correct source labels post-rebuild (DispatchOrigin fix 0c4afd7 preserved).

---

## 13. Audit history

- **rev 0.1 (this rev)** — fresh plan against Wire rev 2.1; superseded cascade plan marked obsolete. Needs one clean blind audit before implementation.

### Post-ship finding — 2026-04-21

**Local Mode disable path wrote walker-incompatible dispatch_policy YAML** — caught by Mac post-ship smoke. Plan's Wave 2 task 17 updated the READ side of `local_mode.rs` (`resolve_local_for_rule` filters `"market"` alongside `"fleet"`) but did NOT audit the WRITE side. The disable fallback at `local_mode.rs:1074-1094` (`restore_dispatch_policy_contribution_id IS NULL` branch) hardcoded a stub YAML with empty `provider_pools` and NO `routing_rules`. Walker's `resolve_route` returned `ResolvedRoute { providers: [] }` → walker iterated zero entries → `fail_audit("no viable route")` → every build failed `0m 0s | 0/0 steps`, chronicle blank.

**Repro class:** DBs where `restore_dispatch_policy_contribution_id` was never populated — Local Mode toggled ENABLE before the walker's bundled seed shipped. Pre-walker installs + any Local-Mode toggle history are affected.

**Plan scope gap:** Wave 5 "string-match audit" (task 37) covered call sites reading `"fleet"`/`"market"` sentinels but did not extend to "audit any hardcoded dispatch_policy YAML written by operational handlers." Future plan retros should widen the scope of "string-match audit" to include schema-writer handlers that construct YAML blobs inline — a stripped-stub handler that predates the bundled seed is the same class of bug.

**Fix:** fallback reads `bundled-dispatch_policy-default-v1` from `pyramid_config_contributions` (shipped Wave 0 task 1) instead of hardcoding a stub; raises `anyhow!` if the bundled seed is missing rather than silently writing broken YAML. Regression guard test at `local_mode.rs::tests::bundled_dispatch_policy_seed_has_routing_rules_with_providers` asserts the bundled seed ships with non-empty `routing_rules` + `route_to` — this test would have caught the regression at unit-test time. Shipped on branch `fix/local-mode-disable-gutted-dispatch-policy`.

**Follow-up chip:** `local_mode.rs:832-853` (enable path) still hardcodes a dispatch_policy YAML. Pre-existing `TODO` comment flags Pillar 37 violation. Now feasible to fix via the bundled seed + Local-Mode overrides (ollama-only routing chain, concurrency=1). Not walker-blocking.

### Post-ship finding (2) — 2026-04-21: walker pool-branch 400 + per-route model resolution

Two chained bugs caught by Mac post-ship smoke immediately after the local-mode fix above shipped.

**W1 — pool-branch 400 classification over-aggressive.** Plan §4.3 (this rev) classified *any* non-context 400 with retries exhausted as `CallTerminal`. OpenRouter returned HTTP 400 with body `{"error":{"message":"gemma4:26b is not a valid model ID"}}` when the walker sent an Ollama format name to OpenRouter's `/chat/completions`. Old rule bubbled the error out of the walker instead of letting it try ollama-local, which was ready to succeed.

**C1 — per-route model resolution missing.** Walker broadcast `config.primary_model` to every pool-branch route entry. The openrouter entry inherited whatever slug the operator had set as primary (Ollama format `gemma4:26b` in Adam's case), and OpenRouter's model validator rejected it — triggering the W1 400 body. Two-bug chain: C1 caused the bad slug to leak, W1 made the resulting 400 terminal instead of skippable.

**Systemic fix (branch `fix/walker-pool-400-classification`, this rev):**

1. `classify_pool_400` + `classify_pool_404` helpers on `EntryError` in `src-tauri/src/pyramid/llm.rs`. Case-insensitive body-text matching splits the 400/404 path three ways: provider-level model rejection → `RouteSkipped`; feature-unsupported → `RouteSkipped`; everything else → `CallTerminal`. UTF-8-safe `truncate_utf8` helper.
2. Per-route model resolution — Option C hybrid: `entry.model_id` → `tier_routing[entry.tier_name]` (row's `provider_id` must match entry's; regression guard against cross-provider smuggling) → `config.primary_model` fallback. Exposed as `resolve_route_model` for tests; inlined in the walker dispatch loop for the single `info!` log tag.
3. Bundled `bundled-dispatch_policy-default-v1` seed pins `model_id: openai/gpt-4o-mini` on the openrouter entry so fresh installs cascade cleanly.
4. 13 new unit + integration tests including `walker_advances_past_openrouter_400_model_rejection_to_ollama_local` (mockito, exact 400 body from Mac smoke).

**Scope-gap note:** rev 0.3 audits reviewed §4.3 classification for *logical shape* — three-tier taxonomy, cascade-vs-skip boundaries, retry counting — but did not exercise the classification against *real-world OpenRouter 400 body text*. Future audits on documented error-classification tables should construct real-world error-body fixtures for every branch (status code × body category) and assert the classifier's output. The W1 bug was a plain test-coverage gap, not a design flaw — the three-tier taxonomy absorbed the fix cleanly.

**Flagged for separate work (NOT fixed in this branch):**

- Punch list P0-1 at `chain_dispatch.rs:1198` `resolve_ir_model` — same class of "one model across providers" bug; walker-reachable only via chain dispatch rather than the pool loop. Separate task.
- Project memory `project_provider_model_coupling_bug` (2026-04-12) — full refactor of `config.primary_model: String` into `HashMap<ProviderId, ModelId>` is a 25+-call-site change. Not this fix; flagged for its own plan.

---

## 14. Open items / backlog for post-ship retro

- Hygiene: document the audit-taint prevention rule in conductor-audit-pass skill (lesson from superseded plan's 5 revs).
- Deferrals ledger: `max_tokens_exceeds_quote` unit test; SSE migration (v2); parallel-purchase speculation; reputation surface wiring; canary-as-market pattern.
- Estimated handoff items for the next cycle in this domain: rev 2.2 Wire changes (if any) would ride through contracts-crate bump; reputation Phase B; embeddings integration at market-surface.

---

## 15. Supplemental punchlist (incidental items caught while reading)

Receptacle for "we fix bugs when we find them" items that are NOT walker-core but surfaced during plan-writing or would surface during implementation. Each is either folded into the waves above or lives here as explicit out-of-wave work.

**Folded into the walker as systemic fixes (§2.5):**

- ✅ **`LlmConfig::prepare_for_replay(origin)` helper (§2.5.1)** — centralizes replay-config rules scattered across 4 call sites. Closes the market-replay bug at source. Wave 0 task 4. Replaces ~30 LOC of hand-clearing.
- ✅ **`branch_allowed(branch, origin)` helper + `RouteBranch` enum (§2.5.2)** — centralizes the "inbound jobs don't re-dispatch" invariant. Pool always allowed; fleet + market only from Local origin. Wave 0 task 5. Closes the market-replay bug via defense-in-depth.
- ✅ **Three-tier `EntryError { Retryable, RouteSkipped, CallTerminal }` (§2.5.3)** — replaces the two-tier version that conflated "this-route-kind can't help" vs "whole-call-doomed." Wave 0 task 6. Fixes the misclassification of `insufficient_balance` at /quote (was Terminal, should be RouteSkipped).

**Also folded (minor):**

- ✅ **Market-replay guard regression tests** — Wave 3 task 22a. Tests both independent guards (§2.5.1 cleanup + §2.5.2 branch_allowed). Pre-walker workaround: not needed, bug hasn't triggered observable production loop to date (quote_already_purchased + same-operator coincidence keeps it quiet).
- ✅ **`classify_soft_fail_reason` + `sanitize_wire_slug` dead-code removal** — Phase B helpers at llm.rs:232-277. When Phase B block is removed in Wave 3 task 23, these become unreferenced. Wave 5 task 37 extends the string-match audit to catch these; delete at same time.
- ✅ **`SlidingWindowLimiter::try_acquire`** — non-blocking sibling to today's `wait()`. Required for `ProviderPools::try_acquire_owned` to report saturation on rate-limit-full (not only on semaphore-full). Wave 0 task 7.
- ✅ **Per-entry `provider_impl` reinstantiation** in walker pool-provider branch. Today's escalation path (llm.rs:2283-2307) reinstantiates lazily; walker does so per entry. Implicit in Wave 1 task 9; call out in the task prompt.
- ✅ **`resolve_uuid_from_purchase` DELETED** — rev 2.1 `/purchase` 200 returns `request_id: UUID` directly per contract §1.6b. Workaround no longer needed. Wave 0 task 8.

**Deferred (plan-owned, waiting on input):**

- ⏭️ **Bundled dispatch_policy schema family** (generation skill + schema_definition + schema_annotation). v1 ships only the default-seed in Wave 0; full family bundled in Wave 4 alongside Settings panel to enable Tools-wizard YAML edit alongside the custom panel. Can defer to a later rev if Tools-wizard edit never surfaces as a need.
- ⏭️ **Wire-dev Q4** (/fill body fields — `input_token_count` + `privacy_tier` status in rev 2.1). Blocks Wave 3. See §9.

**Deferred systemic items (NOT walker scope — separate future initiatives):**

- ⏭️ **Audit-row schema redesign.** Today's `pyramid_llm_audits` is one-row-per-call. Walker makes N attempts per call; the cascade trace is lost. Future: split into parent `pyramid_llm_audit` + child `pyramid_llm_audit_attempts`. SQLite migration + schema work. File as post-walker initiative; plan's Wave 1 task 11 is a short-term fix (update provider_id on exit).
- ⏭️ **TypeScript-Rust binding generation.** Settings.tsx `ComputeParticipationPolicy` TS interface is hand-maintained against the Rust struct. Caused the 3bb86b8 silent-default bug earlier this session. Future: `ts-rs` crate generates TS types at build time. Touches ~30 existing interfaces; cross-cutting cleanup. Not walker scope.
- ⏭️ **Wire spec + contract doc coherence.** Wire-dev Q4 exists because rev 2.1 spec and rev 2.1 contract doc overlap with divergence potential on `/fill` body. Suggest to Wire dev: contract doc is authoritative per-endpoint; spec describes WHAT CHANGED in a rev. Not walker-blocking; flag to Wire dev as doc hygiene.
- ⏭️ **Wire cooperative-framing in error slugs.** Node has `sanitize_wire_slug` at llm.rs:268-277 to remap trader vocabulary (`market_serving_disabled` → `provider_serving_disabled`, `offer_depleted` → `contribution_depleted`, etc.). Systemic fix: Wire adopts cooperative framing at the API boundary. Not walker-blocking; flag to Wire dev as cleanup.

**Noted (not acted upon):**

- ❓ **Today's `should_enqueue_local_execution` uses `route.providers.iter().any(|e| e.is_local)` — coarse.** Walker tightens to per-entry (§4.4). No retroactive fix to today's pre-walker code needed since the walker replaces that gate.
- ❓ **`pyramid_compute_events` has no SQLite CHECK on `event_type`.** Unlike Wire-side chronicle where new events require migration. Adding walker events is additive and safe. Note: no validator enforces the set is complete / consistent. Out of scope for walker.

Convention: ✅ = folded into waves above, ⏭️ = plan-owned waiting on answer OR deferred systemic, ❓ = noted but not acted upon.
