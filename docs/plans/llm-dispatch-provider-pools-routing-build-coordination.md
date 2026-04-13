# LLM Dispatch: Provider Pools, Routing Policy, Build Coordination

## Context

**Problem:** No coordinated LLM dispatch. Every subsystem directly calls `call_model_*`. A global `Semaphore(1)` for Ollama and a sliding window for OpenRouter are the only coordination. Result: folder builds fire concurrently overwhelming GPU, maintenance competes with builds, no routing to different providers by work type, no escalation, and the tier routing table is dead infrastructure (bypassed by a hardcoded cascade).

**Root cause:** The system has no concept of coordinated work. Each subsystem is independently autonomous.

**Pre-existing bugs (fix alongside):**
- `call_model_via_registry` has no 400/context-exceeded cascade handling
- In-flight LLM calls not aborted on build cancellation (cost leak)
- Navigate endpoint uses legacy LLM path with no cache/audit/priority
- Stale engine cost logging bypasses reconciliation pipeline (9 raw SQL sites)

---

## The Fix: Three Composable Mechanisms

Not a central dispatcher. Three independent, composable pieces:

1. **Provider Pools** — per-provider concurrency at the same code points as the current semaphore
2. **Routing Policy** — contribution-governed rules resolved at function entry
3. **Build Coordination** — sequential folder builds + maintenance deferral

**Why not a central dispatcher:** Four auditors found a central dispatcher conflicts at every seam — three separate HTTP paths, deeply coupled retry/cache/audit logic, five existing concurrency layers, and a latency-sensitive interactive path (navigate) that can't tolerate queue delay.

---

## Dispatch Policy YAML Schema

New contribution schema_type: `dispatch_policy`. Editable in Tools section.

```yaml
version: 1

# Per-provider concurrency pools. IDs match pyramid_providers rows.
provider_pools:
  ollama-local:
    concurrency: 1
  remote-5090:
    concurrency: 2
  openrouter:
    concurrency: 20
    rate_limit:
      max_requests: 20
      window_secs: 5.0

# First matching rule wins. Provider preference chain tried in order.
routing_rules:
  - name: interactive
    match:
      work_type: interactive
    bypass_pool: true
    route_to:
      - provider_id: ollama-local
      - provider_id: openrouter

  - name: stale-local
    match:
      work_type: maintenance
    sequential: true
    route_to:
      - provider_id: ollama-local
        model_id: "qwen3:a3b"

  - name: synth-heavy
    match:
      work_type: build
      min_depth: 2
    route_to:
      - provider_id: remote-5090
        model_id: "qwen3:32b"
        tier_name: synth_heavy     # optional: get context_limit/pricing from tier routing
      - provider_id: openrouter
        model_id: "qwen/qwen3.5-coder-32b"

  - name: webbing-frontier
    match:
      work_type: build
      step_pattern: "web*"
    route_to:
      - provider_id: openrouter
        model_id: "anthropic/claude-sonnet-4"

  - name: build-default
    match:
      work_type: build
    route_to:
      - provider_id: ollama-local

  - name: catch-all
    match: {}
    route_to:
      - provider_id: ollama-local
      - provider_id: openrouter

escalation:
  wait_timeout_secs: 30
  max_wait_secs: 300           # hard cap for callers without CancellationToken

build_coordination:
  folder_builds_sequential: true
  defer_maintenance_during_build: true
  defer_dadbear_during_build: true
```

### Rule semantics

- `bypass_pool: true` — skip pool acquire entirely. For rare, user-interactive calls. Ollama queues concurrent requests internally; interactive caller waits at most one background call's duration, not the entire queue. Does NOT preempt Ollama's internal queue.
- `sequential: true` — per-rule `Semaphore(1)` acquired BEFORE provider pool. Ensures all matching work is serialized. With escalation: if pool blocks, timeout releases; sequential semaphore is held throughout (this is correct — one sequential item at a time including its escalation).
- `route_to[].tier_name` — optional. When present, `registry.resolve_tier(tier_name)` provides `context_limit`, `max_completion_tokens`, `pricing_json` for that entry. When absent, provider defaults are used.
- `match.min_depth` — matches when work item `depth >= value`. Depth is `Option<i64>`; `None` never matches min_depth rules.
- `match.step_pattern` — glob on step name. Non-chain callers have empty step_name, which matches only `{}` catch-all.
- `max_wait_secs` — hard timeout for callers without a CancellationToken (stale engine, evidence, FAQ, navigate). Prevents indefinite blocking on saturated providers.

### Two-layer model selection (routing vs cascade)

Routing and model cascade are SEPARATE concerns:
1. **Routing** picks the PROVIDER and optionally overrides the model
2. **Cascade** selects model SIZE within that provider based on input token count

Flow: `resolve_route()` → `(provider_id, optional model_override)` → existing cascade logic (lines 683-691 of llm.rs) runs AFTER that, using the selected provider's context limits. If routing specifies a `model_id`, the cascade uses that model's context limit for its thresholds.

`clone_with_model_override` stays as-is — the all-slot pinning is intentional across 27 call sites. The routing policy's `route_to` preference chain IS the new cascade for routed calls; the three-model cascade becomes irrelevant for those paths.

---

## Architecture

### 1. Provider Pools

New `ProviderPools` struct on `PyramidState`:

```rust
pub struct ProviderPools {
    pools: HashMap<String, ProviderPool>,
    rule_sequencers: HashMap<String, Arc<Semaphore>>,
}

pub struct ProviderPool {
    pub provider_id: String,
    pub semaphore: Arc<Semaphore>,
    pub rate_limiter: Option<SlidingWindowLimiter>,  // replaces global RATE_LIMITER
}
```

Replaces: global `LOCAL_PROVIDER_SEMAPHORE` (llm.rs:51), global `RATE_LIMITER` (llm.rs:43).

**Critical: keep global semaphore as fallback** for `dispatch_policy: None` paths (tests, pre-init).

Pool acquire happens at the same three sites inside the retry loop (matching current behavior):

```rust
// Before (inside retry loop at llm.rs:767):
let _permit = if provider_type == ProviderType::OpenaiCompat {
    Some(LOCAL_PROVIDER_SEMAPHORE.acquire().await?)
} else { None };

// After (inside retry loop):
let _permit = if let Some(pools) = &config.provider_pools {
    pools.acquire_for_provider(&provider_id, &route).await?
} else if provider_type == ProviderType::OpenaiCompat {
    Some(LOCAL_PROVIDER_SEMAPHORE.acquire().await?)  // fallback
} else { None };
```

The acquire stays INSIDE the retry loop. Per-attempt escalation: if timeout fires, the `Acquire` future is dropped (cancellation-safe per tokio docs), semaphore stays clean, caller tries next provider.

### Hot-reload

When a `dispatch_policy` contribution is superseded:
1. Dispatcher writes new YAML to `pyramid_dispatch_policy` table
2. Dispatcher fires `ConfigSynced { schema_type: "dispatch_policy", ... }` on event bus
3. Listener in server.rs (which has `PyramidState` access) receives the event
4. Listener rebuilds `ProviderPools` from the new YAML, swaps the `Arc`
5. In-flight work on old `Arc<Semaphore>` completes normally (permits hold old Arc alive)
6. Transient over-subscription during swap window is acceptable (Ollama serializes internally)

This follows the same event-driven pattern needed for `tier_routing` and `step_overrides` reload.

### 2. Routing Policy

New `DispatchPolicy` struct, loaded from contribution, held on `LlmConfig`:

```rust
// dispatch_policy.rs
pub fn resolve_route(
    &self,
    work_type: WorkType,
    tier: &str,
    step_name: &str,
    depth: Option<i64>,
) -> ResolvedRoute
```

Returns `ResolvedRoute { providers: Vec<RouteEntry>, bypass_pool, sequential_semaphore, escalation_timeout, max_wait_secs }`.

**Each LLM path maps this to its own provider instantiation:**

- `call_model_unified_with_audit_and_ctx`: routing resolve at entry → `provider_id` → `build_call_provider()` (modified to accept provider_id instead of auto-detecting) → existing cascade + retry logic
- `call_model_via_registry`: routing resolve at entry → if route overrides provider/model, use that; otherwise fall through to existing `resolve_tier()` → add 400/cascade handling (bug fix)
- `call_model_direct`: routing resolve at entry → catch-all rule → existing logic (single call site: ASCII art)

**`build_call_provider` modification**: currently returns `(Box<dyn LlmProvider>, Option<ResolvedSecret>, ProviderType)`. Add `provider_id: String` to the return tuple. When dispatch_policy is present, accept the provider_id from routing instead of calling `active_provider_id()`.

### 3. Build Coordination

Separate from LLM dispatch.

**Sequential folder builds:** `spawn_question_build` (question_build.rs) currently spawns a `tokio::spawn` and returns immediately. Modify to return a `oneshot::Receiver<Result<()>>` completion signal. The build task sends on the oneshot when done. `spawn_initial_builds` in folder_ingestion.rs awaits the receiver before spawning the next build (when `folder_builds_sequential: true`).

**Maintenance deferral:** Add `active_build: Arc<tokio::sync::RwLock<HashMap<String, BuildHandle>>>` as a field on `PyramidStaleEngine` (cloned from `PyramidState.active_build` at construction). The poll loop checks `active_build.read().await.is_empty()` before dispatching drain_and_dispatch. Same for DADBEAR: thread `active_build` into `start_dadbear_extend_loop` and check before `run_tick_for_config`.

---

## Pre-Existing Bug Fixes

### Fix 1: 400/context-exceeded cascade in `call_model_via_registry`

Currently `call_model_via_registry` has no 400 handling — builds fail instead of cascading. Add the same 400-body parsing and model cascade logic that exists in `call_model_unified_with_audit_and_ctx` (lines 829-849). When context-exceeded detected, cascade to next provider in the routing rule's `route_to` chain (if routing is active) or to the tier's fallback (if using legacy tier resolution).

### Fix 2: CancellationToken in LLM HTTP calls

Thread `CancellationToken` into all three LLM entry points as `Option<&CancellationToken>`. Use `tokio::select!` around the HTTP send:

```rust
tokio::select! {
    result = request.json(&body).send() => { /* handle */ }
    _ = cancel_or_max_wait(cancel, max_wait_secs) => {
        return Err(anyhow!("cancelled or max wait exceeded"))
    }
}
```

`cancel_or_max_wait`: if cancel is Some, race against `cancel.cancelled()`. If None, race against `tokio::time::sleep(max_wait_secs)`. Non-build callers (stale engine, evidence, FAQ, navigate) pass `cancel: None` and get the `max_wait_secs` hard cap from escalation config.

### Fix 3: Navigate endpoint retrofit

Add synthetic StepContext to `/navigate` handler:
- `build_id`: `"navigate-{slug}-{timestamp}"` (synthetic, not a real build)
- `step_name`: `"navigate"`
- `work_type`: `WorkType::Interactive` (added to dispatch params)
- `cache_access`: constructed with navigate-specific scope (not the global config's cache_access)
- API key guard: change from `config.api_key.is_empty()` to check for any configured provider

Route with `bypass_pool: true` so never blocked by background work.

### Fix 4: Stale engine cost logging migration

9 sites in stale_helpers.rs and stale_helpers_upper.rs use raw INSERT with NULL generation_id/provider_id. The underlying issue: `call_model_with_usage_and_ctx` returns `(String, TokenUsage)`, stripping `generation_id` and `provider_id`.

Fix: change the 9 stale helper call sites from `call_model_with_usage_and_ctx` to `call_model_unified_and_ctx` (which returns the full `LlmResponse`). Destructure at the call site to get `generation_id` and `provider_id`. Then pass them to `insert_cost_log_synchronous`.

---

## Files to Create

| File | Purpose |
|------|---------|
| `src-tauri/src/pyramid/provider_pools.rs` | ProviderPools, ProviderPool, SlidingWindowLimiter, acquire logic |
| `src-tauri/src/pyramid/dispatch_policy.rs` | DispatchPolicy, RoutingRule, WorkType, ResolvedRoute, YAML parsing |

## Files to Modify

| File | Change |
|------|--------|
| `pyramid/mod.rs` | Add modules, add `provider_pools` + `dispatch_policy` to PyramidState |
| `pyramid/llm.rs` | Add `dispatch_policy: Option<Arc<DispatchPolicy>>`, `provider_pools: Option<Arc<ProviderPools>>` to LlmConfig. Routing resolve at entry of all three paths. Keep global semaphore as fallback. Pool acquire at same three sites. Thread CancellationToken. Retire global RATE_LIMITER. Modify `build_call_provider` to return provider_id and accept optional provider_id override. Add 400/cascade to `call_model_via_registry`. |
| `pyramid/config_contributions.rs` | Add `dispatch_policy` branch to dispatcher (write DB + fire bus event) |
| `pyramid/db.rs` | Add `DispatchPolicyYaml` struct, `pyramid_dispatch_policy` table, upsert/read helpers |
| `pyramid/wire_native_metadata.rs` | Add `dispatch_policy` to resolve_wire_type |
| `pyramid/server.rs` | Initialize pools + policy at boot, thread into LlmConfig via `to_llm_config_with_runtime`. Add ConfigSynced listener for hot-reload. |
| `pyramid/stale_engine.rs` | Add `active_build` field. Check in tick loop for deferral. |
| `pyramid/stale_helpers.rs` | Change 4 sites from `call_model_with_usage_and_ctx` to `call_model_unified_and_ctx`. Migrate cost logging. |
| `pyramid/stale_helpers_upper.rs` | Change 5 sites same way. Migrate cost logging. |
| `pyramid/evidence_answering.rs` | Tag calls with WorkType::Evidence |
| `pyramid/faq.rs` | Tag calls with WorkType::Faq |
| `pyramid/routes.rs` | Retrofit navigate endpoint with StepContext + WorkType::Interactive |
| `pyramid/folder_ingestion.rs` | Sequential build dispatch when policy says so |
| `pyramid/question_build.rs` | Return oneshot completion signal from spawn_question_build |
| `pyramid/dadbear_extend.rs` | Thread active_build, check for deferral in tick loop |
| `pyramid/main.rs` | Thread pools + policy through to LlmConfig |

---

## Implementation Phases

### Phase A: Provider Pools + Routing Foundation

Goal: Per-provider concurrency, routing policy contribution, all three LLM paths intercepted. **Behavior identical to today with default policy.**

1. Create `provider_pools.rs` — per-provider Semaphore + per-pool rate limiter
2. Create `dispatch_policy.rs` — types, YAML parsing, resolve_route
3. Add fields to LlmConfig (5 construction sites: struct def, Default, to_llm_config, Debug, to_llm_config_with_runtime)
4. Add routing resolve at entry of all three LLM paths
5. Modify `build_call_provider` to return provider_id and accept override
6. Replace semaphore acquire at all three sites with pool acquire (keep global as fallback)
7. Retire global RATE_LIMITER (per-pool rate limiting replaces it)
8. Add `dispatch_policy` contribution schema type
9. Seed DEFAULT policy: `folder_builds_sequential: false`, `defer_maintenance: false`, catch-all rule routing to `[active_provider, openrouter]`, pools matching current semaphore (Ollama=1, OpenRouter=20)
10. Initialize pools + policy at boot via `to_llm_config_with_runtime`
11. Hot-reload listener on ConfigSynced bus event

**Default policy (Phase A — matches current behavior exactly):**
```yaml
version: 1
provider_pools:
  ollama-local:
    concurrency: 1
  openrouter:
    concurrency: 20
    rate_limit:
      max_requests: 20
      window_secs: 5.0
routing_rules:
  - name: catch-all
    match: {}
    route_to:
      - provider_id: ollama-local
      - provider_id: openrouter
escalation:
  wait_timeout_secs: 300
  max_wait_secs: 600
build_coordination:
  folder_builds_sequential: false
  defer_maintenance_during_build: false
  defer_dadbear_during_build: false
```

### Phase B: Work Type Tagging + Build Coordination

1. Add `work_type` as parameter to `dispatch_with_routing` (not on StepContext — avoids changing 92 call sites)
2. Tag five subsystem categories at the three LLM entry points
3. Modify `spawn_question_build` to return oneshot completion signal
4. Folder ingestion: await completion signal before spawning next build
5. Thread `active_build` into PyramidStaleEngine + DADBEAR, check in tick loops
6. Implement `sequential: true` (per-rule Semaphore(1))
7. Implement `bypass_pool: true` (skip pool acquire)

### Phase C: Pre-Existing Bug Fixes

1. Add 400/context-exceeded cascade to `call_model_via_registry`
2. Thread CancellationToken into LLM HTTP calls with `max_wait_secs` fallback
3. Retrofit navigate endpoint with StepContext + WorkType::Interactive
4. Migrate 9 stale helper cost logging sites (change wrapper, add generation_id/provider_id)

### Phase D: Escalation

1. Per-provider timeout on pool acquire
2. Escalation to next provider in preference chain
3. Last provider: wait up to `max_wait_secs` (with cancellation if available)

---

## Verification

1. **Phase A:** Build a pyramid with Ollama. Verify per-provider pool acquire in logs (not global semaphore). Edit dispatch_policy in Tools — verify hot-reload rebuilds pools.
2. **Phase B:** Build 3 pyramids from folder ingestion — verify sequential (each completes before next starts). Start maintenance on one pyramid during another's build — verify deferred. Verify `sequential: true` serializes stale checks.
3. **Phase C:** Trigger context-exceeded on a chain step — verify cascade. Cancel a build — verify HTTP calls abort. Navigate twice — verify cache hit. Check stale cost logs — verify generation_id populated.
4. **Phase D:** Saturate Ollama. Verify timeout triggers escalation to next provider. Verify interactive calls bypass pool.
