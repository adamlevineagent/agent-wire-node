# Handoff: Wire Market System — Overnight Build

**Date:** 2026-04-13
**Author:** Design session with Adam. Three audit rounds (7 agents) + unified cross-doc audit + final coherence wanderers.
**Scope:** Build Phase 1 of the Wire Compute Market. Shared infrastructure for all three markets.

---

## What This Is

A decentralized inference exchange where Wire nodes buy and sell LLM compute, storage, and relay bandwidth using credits. The Wire is pure control plane (matching, settlement, routing). All data flows node-to-node through a relay network. Three orthogonal privacy mechanisms (variable relay count, distributional opacity, tunnel rotation).

## Documents (read in this order)

1. **`docs/architecture/wire-market-privacy-tiers.md`** — Privacy architecture. Read first for the mental model.
2. **`docs/plans/wire-compute-market-build-plan.md`** — Compute market: 9 phases + addendum. The most detailed plan. Contains shared infrastructure (rotator arm, atomic RPCs, queue system) used by all three markets.
3. **`docs/plans/storage-market-conversion-plan.md`** — Storage market: 3 phases. Converts existing flat-rate storage to market pricing.
4. **`docs/plans/relay-market-plan.md`** — Relay market: 2 phases. Simplest market. Streaming relay for privacy.
5. **`docs/plans/market-seed-contributions.md`** — All v1 seed YAMLs, schema reflection, new mechanical recipes.

## What to Build Tonight: Phase 1

**Phase 1 = Compute Queue + Shared Infrastructure.** This is the foundation everything else builds on.

### Wire Workstream (GoodNewsEveryone)

**Prerequisites migration** (compute plan Section II, "Prerequisites Migration"):
1. Extend `wire_graph_fund.source_type` CHECK to include ALL market values: `'compute_service'`, `'compute_reservation'`, `'storage_serve'`, `'hosting_grant'`, `'relay_hop'`
2. Create `wire_market_rotator` table (shared rotator arm state)
3. Create `advance_market_rotator(node_id, market_type, scope_id, rotator_type)` function
4. Create `market_rotator_recipient(position)` function (reads slot counts from `economic_parameter` contribution)
5. Seed `market_rotator_config` economic parameter contribution (80 total, 76 provider, 2 Wire, 2 Graph Fund)
6. Create Wire platform operator entity (handle: `agentwireplatform`)
7. Create Graph Fund entity (handle: `agentwiregraphfund`)

**Compute market tables** (compute plan Section II, "New Tables"):
- `wire_compute_offers`
- `wire_compute_jobs`
- `wire_compute_observations`
- `wire_compute_queue_state`
- All with RLS enabled + GRANT to service_role

**Compute market RPCs** (compute plan Section II):
- `settle_compute_job` — settlement with rotator arm
- `match_compute_job` — exchange matching with FOR UPDATE locking
- `fill_compute_job` — purely financial (no prompt data)
- `fail_compute_job` — failure handling with deposit refund
- `void_compute_job` — unfilled reservation resolution
- `sweep_timed_out_compute_jobs` — timeout sweep
- `deactivate_stale_compute_offers` — heartbeat liveness
- `compute_queue_multiplier_bps` — queue discount interpolation (integer basis points)

### Node Workstream (agent-wire-node)

**The big change: semaphore removal.**

This is a flag-day migration. The `LOCAL_PROVIDER_SEMAPHORE` in `llm.rs` (line ~51) and the `ProviderPools` Ollama pool MUST be replaced by the `ComputeQueueManager`. They cannot coexist (deadlock risk).

Steps:
1. Create `src-tauri/src/compute_queue.rs` — `ComputeQueueManager` with per-model `ModelQueue`, FIFO ordering
2. Create `src-tauri/src/compute_market.rs` — `ComputeMarketState`, load/save to disk
3. **Remove semaphore acquisition** from all LLM call sites in `llm.rs`. There are THREE sites (search for `LOCAL_PROVIDER_SEMAPHORE` and `provider_pools`):
   - Main path: `call_model_unified_with_audit_and_ctx` (~line 878)
   - Registry path: `call_model_via_registry` (~line 2055)
   - Direct path: (~line 2587)
4. **Stale engine** (`stale_engine.rs`): must submit LLM calls through `enqueue_local`, not directly through the LLM path
5. **GPU processing loop** in `main.rs`: new background task that pulls from queue, executes via existing LLM path (which no longer acquires any semaphore)
6. **`enqueue_local` blocks (async wait)** — never returns QueueError. Local work is never rate-limited.
7. Add `ComputeQueueManager` to `AppState` in `lib.rs` (same `Arc<RwLock<>>` pattern as other state)

**Frontend:**
- `QueueLiveView.tsx` — real-time queue visualization
- Shell `MarketDashboard.tsx` with enable/disable toggle

### Verification

After Phase 1 ships: local pyramid builds work exactly as before but through the queue. The queue view shows build steps processing serially. No regressions. No performance change (the queue just replaces the semaphore).

---

## Critical Rules

Read BEFORE implementing:
- `/Users/adamlevine/AI Project Files/agent-wire-node/docs/SYSTEM.md` — The Five Laws
- `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-pillars.md` — The 44 Pillars
- Especially: Law 1 (one executor), Law 3 (one contribution store), Law 4 (every LLM call gets StepContext), Pillar 9 (integer economics), Pillar 37 (never prescribe outputs to intelligence)

## Build Pattern

Per phase:
1. **Implementer** — one agent per workstream (Wire, Node, Frontend). Follow the plan exactly.
2. **Serial verifier+fixer** — same instructions as implementer. Arrives expecting to build, audits instead, fixes in place.
3. **Wanderer** — feature name only + "does this actually work?" Traces end-to-end execution.

## What NOT to Build Tonight

- Phases 2-9 of compute market (exchange, settlement, bridge, quality, daemon, sentinel, steward)
- Storage conversion (depends on Phase 1)
- Relay market (depends on Phase 1)
- Any privacy/relay features (Phase R1+)
- Competitive auto-pricing (Phase 2+)
- Steward-mediated anything (Phase 7+)

Phase 1 is purely: queue replaces semaphore + shared infrastructure (rotator arm, tables, RPCs) ready for Phases 2+.

## Critical Implementation Details (from implementer-perspective audit)

### Semaphore Removal: What Actually Needs to Happen

**The static semaphore** is at `llm.rs:51`:
```rust
static LOCAL_PROVIDER_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(1));
```

**Exactly three acquire sites** (plan is correct):
1. `call_model_unified_with_options_and_ctx` (llm.rs:879)
2. `call_model_via_registry` (llm.rs:2056)
3. `call_model_direct` (llm.rs:2589)

All three share the same guard: try `pools.acquire()` first, fall back to `LOCAL_PROVIDER_SEMAPHORE` when pools are None and provider is OpenaiCompat.

**Approach: Set semaphore to `usize::MAX`, not delete.** This preserves test compilation (tests don't construct ProviderPools and fall through to the semaphore). With `usize::MAX` permits, the semaphore is effectively a no-op. The queue IS the real serializer.

**How the GPU loop calls LLM without deadlock:** Add `skip_concurrency_gate: bool` to `LlmCallOptions`. The GPU processing loop sets this to `true`. The three acquire sites check it: if true, skip semaphore/pool acquisition entirely. This is the simplest approach — one new field, three guard checks.

### Stale Engine: 10+ Call Sites, Not One

The plan says "stale engine must submit through enqueue_local." In reality, there are **10+ distinct `call_model_unified_and_ctx` calls** across `stale_helpers.rs` and `stale_helpers_upper.rs`:

- `stale_helpers.rs`: lines ~306, ~827, ~1246, ~1529
- `stale_helpers_upper.rs`: lines ~609, ~940, ~1303, ~1373, ~1676, ~3089

Each must be converted from direct LLM call to queue submission. The pattern:
```rust
// Before (direct call):
let result = call_model_unified_and_ctx(&config, Some(&ctx), ...).await?;

// After (queue submission):
let (tx, rx) = oneshot::channel();
queue.enqueue_local(model_id, QueueEntry { payload: Ready(request), result_tx: tx, ... })?;
let result = rx.await??;
```

**Do NOT try to rewire these one by one.** Instead: create a wrapper function `call_model_via_queue(queue, model_id, config, ctx, ...)` that encapsulates the enqueue + await pattern. Then find-and-replace the 10+ call sites to use the wrapper. The wrapper has the same return type as `call_model_unified_and_ctx`.

### Boot Sequence Ordering

The GPU processing loop MUST start consuming BEFORE any producer tries to enqueue:

```
1. Construct ComputeQueueManager (empty queues)
2. Add to AppState
3. Start GPU processing loop (begins consuming from queues)
4. Start stale engine (may try to enqueue stale checks)
5. Ready for build commands (may try to enqueue build steps)
```

If stale engine init (server.rs:444) runs before the GPU loop, stale tasks block forever on `enqueue_local` with no consumer.

### for_each Concurrency Under the Queue Model

Currently `for_each` spawns tasks bounded by `concurrency_cap` (defaults to 1 for Ollama). With the queue model:
- `for_each` spawns N tasks (up to `concurrency_cap`)
- Each task calls `call_model_via_queue()` which calls `enqueue_local()`
- `enqueue_local` **blocks** (async wait) until the GPU loop processes the item
- The GPU loop processes one item at a time per model queue

So `for_each` with `concurrency_cap: 200` spawns 200 tasks. All 200 enqueue. They wait in the FIFO queue. The GPU processes them one at a time. The `concurrency_cap` just controls how many are prepared simultaneously — the queue serializes execution. No uncontrolled parallel access.

### Model ID Resolution for enqueue_local

The caller resolves model_id BEFORE enqueue. The current LLM path already resolves the model from dispatch_policy/tier_routing before the HTTP call. The queue just needs the resolved model_id to route to the right per-model queue. For stale engine: model resolved at `stale_engine.rs:741-745` via `registry.resolve_tier("stale_remote", ...)`. For chain executor: resolved via dispatch policy routing rules.

### ComputeQueueManager Placement in State

Put it as a **top-level field on AppState**, NOT nested inside `ComputeMarketState`:

```rust
pub struct AppState {
    // ... existing fields ...
    pub compute_queue: Arc<tokio::sync::Mutex<ComputeQueueManager>>,
}
```

Use `tokio::sync::Mutex` (not `RwLock`) because most access is mutable (enqueue, advance). Top-level because it's accessed from everywhere (chain executor, stale engine, GPU loop, IPC commands, server endpoints).

**Phase 1 simplification:** For Phase 1 (local builds only), the `ComputeQueueManager` doesn't need `result_channels` (that's Phase 3 for webhook delivery). Strip the `HashMap<String, oneshot::Sender>` field from the Phase 1 implementation. Add it in Phase 3.

### Node-Side Schema

No migration system — all tables are `CREATE TABLE IF NOT EXISTS` in `db.rs:init_pyramid_db`. Add new tables there. No migration numbering needed for SQLite.

### Wire-Side Migration Numbering

Convention: `YYYYMMDD######_description.sql`. Use today's date:
- `20260413100000_market_prerequisites.sql` — rotator arm, CHECK extension, economic parameter seeds
- `20260413200000_compute_market_tables.sql` — offers, jobs, observations, queue state

## Known Gotchas

1. **`stale_engine.rs` has its own `concurrent_helpers` semaphore.** This is separate from the LLM semaphore. It controls how many stale-check batches run in parallel. KEEP it — but each batch's LLM calls must go through the queue.
2. **The `wire_compute_jobs` table has NO `messages` column.** The Wire never has prompts. This is the relay-first model. Don't add a messages column.
3. **All credit amounts are integer (`i64` in Rust, `INTEGER`/`BIGINT` in SQL).** No `f64` anywhere in financial paths. Queue discount multipliers are integer basis points (10000 = 1.0x).
4. **The `matched_multiplier_bps` column (not `matched_multiplier`).** The `_bps` suffix is load-bearing — it signals integer basis points, not a float multiplier.
5. **`market_rotator_recipient` takes 1 param (position).** It reads slot counts from the economic_parameter contribution internally. Don't pass slot counts as arguments.
