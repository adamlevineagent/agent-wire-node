# Phase 6: Market Intelligence via DADBEAR

**What ships:** Autonomous market intelligence for compute, storage, and relay markets. Three DADBEAR slugs (`market:compute`, `market:storage`, `market:relay`) with market-specific observation sources, compiler mappings, and result application paths. The steward is the DADBEAR compiler. The sentinel is a periodic observation source. The daemon is the result application path. No new architectural patterns — extensions of existing DADBEAR infrastructure.

**Prerequisites:** Phases 2-5 (working market with quality enforcement), DADBEAR canonical architecture (shipped)

**Collapses original Phases 6-9.** See `audit-2026-04-15-compute-market-phases-2-9.md` Theme 7 for rationale.

---

## I. Why This Is One Phase, Not Four

The audit (two independent auditors, converging conclusion) found that DADBEAR's canonical architecture already provides every structural component Phases 6-9 intended to build separately:

| Original Phase | What It Described | What Already Exists |
|---|---|---|
| Phase 6: Daemon Intelligence | Watch demand signals, decide model portfolio, adjust rates | DADBEAR compiler: observation events trigger work items whose results are applied as contribution supersessions |
| Phase 7: Sentinel | 2b model periodic health checks, auto-adjust, escalate | DADBEAR observation source (periodic function writing events) + compiler auto-commit work items |
| Phase 8: Smart Steward | Observe → hypothesize → change → measure → keep/revert | DADBEAR's observe → compile → preview → dispatch → apply lifecycle. Measurement = hold-then-check. Keep/revert = apply_result superseding or reverting the contribution. |
| Phase 9: Steward Chains | Methodology as contributable chains, publication, meta-coordination | Action chains processed by existing `chain_executor.rs`. Publication as a new contribution type. |

The three daemons described in Addendum C become three DADBEAR slugs, not three daemon instances. The supervisor (`dadbear_supervisor.rs`) already processes work per-slug via `gather_dispatchable_items` → slug-grouped `HashMap<String, Vec<WorkItem>>`. Holds are per-slug via `dadbear_holds_projection`. The compiler reads `WHERE slug = ?`. All three requirements from Addendum C — independent experimentation, blast radius isolation, and independent sentinel chains — are delivered by slug scoping for free.

**What is actually new work:**
1. Three observation sources (heartbeat demand extractor, chronicle health monitor, network config fetcher)
2. Compiler mappings (market event types → work item primitives)
3. Result application paths (Ollama control plane calls, contribution supersession, Wire publication)
4. Market chain YAML (existing chain format, new mechanical step registrations)
5. Wire-side additions (publication contribution type, config recommendation RPC, subscription mechanism)
6. Frontend (market intelligence dashboard, model portfolio view, pricing optimizer, experiment log, sentinel activity log)
7. GPU access design for management LLM calls
8. Model loading state machine
9. Experiment lifecycle with statistical validity
10. Cross-market resource management
11. Publication privacy

**What does NOT need to be built:**
- No new reconciliation loop (`dadbear_supervisor.rs` tick loop: 5s interval, JoinSet-based dispatch, crash recovery)
- No new event tables (`dadbear_observation_events`, `dadbear_hold_events` — both exist)
- No new hold system (`auto_update_ops.rs`: `place_hold`, `clear_hold`, `has_hold`, `is_held` — all generic, any hold name)
- No new work item lifecycle (`dadbear_work_items`: compiled → previewed → dispatched → completed → applied, with CAS transitions)
- No new crash recovery (supervisor Phase A: scan in-flight items, timeout stale attempts, re-dispatch)
- No new chain executor (`chain_executor.rs` with `ChainStep.mechanical` + `rust_function` dispatch)
- No separate daemon/sentinel/steward processes — they are observation sources, compiler mappings, and application paths within the existing supervisor

---

## II. Architecture: DADBEAR Market Extension

### Three DADBEAR Slugs

`market:compute`, `market:storage`, `market:relay`. Each slug gets:

- **Independent observation events** — the compiler reads `WHERE slug = ?1 AND id > last_compiled_observation_id` (see `compile_observations` in `dadbear_compiler.rs:331`), so market:compute observations never trigger market:storage work items
- **Independent compiler mappings** — `map_event_to_primitive` is extended with market event types, returning slug-appropriate (primitive, step_name, model_tier) tuples
- **Independent holds** — `auto_update_ops::place_hold(conn, bus, "market:compute", "measurement", Some("pricing experiment in progress"))` freezes compute experiments while storage and relay continue. The `gather_dispatchable_items` function already checks `is_held(conn, slug)` per-slug and blocks held items.
- **Independent epochs** — `get_or_create_epoch` is per-slug, so recipe/norms changes in one market don't rotate epochs in another
- **Shared supervisor infrastructure** — same 5-second tick loop, same `JoinSet<CompletedItem>` dispatch, same crash recovery scan. The supervisor processes all slugs each tick via the `for (slug, items) in &slug_work` loop.

Why slugs, not instances: DADBEAR's infrastructure is inherently slug-scoped. Every database query, every hold check, every epoch rotation, every compilation pass filters by slug. Three slugs in one supervisor give the plan's stated benefits (independent experimentation, blast radius isolation, per-daemon sentinel chains) with zero new coordination infrastructure.

### DADBEAR Mapping: Plan Concepts to DADBEAR Primitives

| Plan Concept | DADBEAR Primitive | Existing Infrastructure | New Work |
|---|---|---|---|
| Daemon "watch demand signals" | Observation event source | `observation_events::write_observation_event` | New periodic function: heartbeat demand extractor |
| Daemon "decide which models to load" | Compiler: event → work item | `dadbear_compiler::map_event_to_primitive` | New mapping: `demand_signal` → `model_portfolio_eval` |
| Daemon "adjust rates" | Result application path | `DadbearSupervisor::apply_result` | New branch: supersede `compute_pricing` contribution |
| Sentinel "health check" | Observation source (periodic) | `observation_events::write_observation_event` | New periodic function: chronicle health monitor |
| Sentinel "auto-adjust" | Compiler auto-commit work item | Preview gate `BudgetDecision::AutoCommit` | New mappings with `auto_commit: true` flag |
| Sentinel "escalate" | Hold event | `auto_update_ops::place_hold` | New hold type: `"escalation"` |
| Steward "experiment loop" | Work item with measurement window | Work item lifecycle + holds | New hold type: `"measurement"` placed during experiment window |
| Steward "contribution publishing" | Result application: create Wire contribution | `apply_result` dispatching on primitive type | New branch: POST to Wire contribution API |
| Steward chains | Action chain YAML | `chain_executor.rs` + `ChainStep { mechanical: true, rust_function }` | New chain YAML files + mechanical step registrations |
| Three daemons | Three slugs | Slug-scoped compilation, holds, work items | Slug creation + observation source wiring |

---

## III. New Observation Sources

### Source 1: Heartbeat Demand Signal Extractor

Reads the heartbeat response that already contains market surface data. Writes observation events to the appropriate market slug.

**Events produced:**

| Event Type | Slug | Description |
|---|---|---|
| `demand_signal` | `market:compute` | Unfilled bids for model X — demand exists with no supplier |
| `model_popularity` | `market:compute` | Which models have the most market activity |
| `pricing_trend` | `market:compute` | How market rates are moving for models this node serves |
| `fleet_utilization` | `market:compute` | How busy fleet peers are |

**Trigger:** On every heartbeat response. The heartbeat is already periodic (interval is an `economic_parameter` contribution, not hardcoded). The extractor runs as a post-heartbeat hook — after the heartbeat response is parsed and applied, before the next heartbeat cycle.

**Event schema written to `dadbear_observation_events`:**
```
slug: "market:compute"
source: "heartbeat"
event_type: "demand_signal" | "model_popularity" | "pricing_trend" | "fleet_utilization"
target_node_id: model_id (for model-specific signals) or NULL (for aggregate signals)
metadata_json: { "signal_value": <numeric>, "model_id": "<model>", "unfilled_count": <n>, ... }
```

**Implementation:** New function `extract_market_signals(heartbeat_response: &HeartbeatResponse, conn: &Connection)` called from the heartbeat handler. Uses `observation_events::write_observation_event` for each extracted signal.

### Source 2: Chronicle Health Monitor

Queries `pyramid_compute_events` (local SQLite, no Wire calls) for this node's performance trends. Writes observation events for anomalies and drift.

**Events produced:**

| Event Type | Slug | Description |
|---|---|---|
| `throughput_drift` | `market:compute` | Tokens/sec trending down (hardware degradation, thermal throttling) |
| `queue_utilization` | `market:compute` | Average queue depth over window — too deep = underpriced, too shallow = overpriced |
| `error_rate` | `market:compute` | Failure/timeout rate trending up |
| `latency_drift` | `market:compute` | p95 latency trending up |

**Trigger:** Periodic, every N minutes. N is an `economic_parameter` contribution (`chronicle_health_interval_minutes`). No hardcoded default — seed the contribution with an initial value during market activation.

**Data source:** `pyramid_compute_events` table (shipped with Compute Chronicle). The monitor queries for events within the trailing window and computes statistical aggregates.

**Implementation:** New periodic task spawned alongside the DADBEAR extend loop. Each tick: open ephemeral DB connection, compute aggregates from chronicle events, compare against thresholds (also `economic_parameter` contributions), write observation events for detected anomalies.

**Window and threshold sourcing:**
- Trend window: `economic_parameter` contribution (`chronicle_trend_window_minutes`)
- Throughput degradation threshold: `economic_parameter` contribution (`throughput_degradation_pct`) — percentage decline triggering a `throughput_drift` event
- Queue utilization bands: `economic_parameter` contributions (`queue_depth_high`, `queue_depth_low`) — thresholds for "too deep" and "too shallow"
- Error rate threshold: `economic_parameter` contribution (`error_rate_threshold_pct`)

### Source 3: Network Config Fetcher

Queries the Wire for configurations used by nodes with similar hardware profiles. Writes observation events with network-level baselines and recommendations.

**Events produced:**

| Event Type | Slug | Description |
|---|---|---|
| `network_recommendation` | `market:compute` | Best pricing/capacity configs for this hardware class |
| `network_baseline` | `market:compute` | Median performance for this model on similar hardware |

**Trigger:** Periodic, every N minutes. N is an `economic_parameter` contribution (`network_fetch_interval_minutes`). Deliberately slower than chronicle health (network data changes slowly).

**Wire API:** `GET /api/v1/compute/config-recommendations?hardware_class=<class>&model_family=<family>` (new endpoint, see Section XI).

**Cold start fallback:** If no network data is available (new market, no similar nodes), the fetcher writes no observation events. The steward operates from local experiments only. When network data becomes available, it starts flowing in as `network_recommendation` events and the compiler creates `config_baseline` work items.

**Proto-steward bridge:** Before the network has enough data, DD-10's generative config UI serves as the initial configuration source. The network config fetcher supersedes it naturally — as real data arrives, `network_recommendation` events trigger compiler work items that evaluate and potentially adopt network-recommended configs.

---

## IV. New Compiler Mappings

Extend `map_event_to_primitive` in `dadbear_compiler.rs` for market event types.

**P3 signature change (from 2026-04-15 audit):** The existing signature `(event_type: &str) -> Option<(&str, &str, &str)>` cannot distinguish `demand_signal` variants (high-demand-no-offer vs low-demand-active-offer) without reading the observation event's `metadata_json`. Phase 6 changes the signature to accept the full event struct:

```rust
// Before (pre-Phase-6):
fn map_event_to_primitive(event_type: &str) -> Option<(&str, &str, &str)>

// After (Phase 6):
fn map_event_to_primitive(event: &ObservationEvent) -> Option<(&'static str, &'static str, &'static str)>
```

The function reads `event.event_type` and — where needed — `event.metadata_json.signal_variant` to select the primitive. Existing callers of the `&str` variant must be migrated. Known call site: `dadbear_compiler.rs:362`. A grep pass (`rg 'map_event_to_primitive' src-tauri/src`) should turn up any others in the same pass so none are left on the old signature after migration.

The return type continues to be `(primitive, step_name, model_tier)`.

New mappings:

| Event Type | Primitive | Step Name | Model Tier | Auto-Commit? |
|---|---|---|---|---|
| `demand_signal` (high demand, no offer) | `model_portfolio_eval` | `market_load_eval` | `management` | No — requires LLM evaluation |
| `demand_signal` (low demand, offer active) | `model_unload_eval` | `market_unload_eval` | `management` | No — requires LLM evaluation |
| `queue_utilization` (depth > `queue_depth_high`) | `pricing_increase` | `sentinel_price_bump` | N/A | Yes (sentinel auto-adjust) |
| `queue_utilization` (depth < `queue_depth_low`) | `pricing_decrease` | `sentinel_price_drop` | N/A | Yes (sentinel auto-adjust) |
| `throughput_drift` (degrading) | `capacity_reduction` | `sentinel_capacity_cut` | N/A | Yes (sentinel auto-adjust) |
| `error_rate` (above threshold) | `offer_suspension` | `sentinel_offer_suspend` | N/A | Yes (sentinel auto-adjust) |
| `latency_drift` (p95 above threshold) | `depth_reduction` | `sentinel_depth_cut` | N/A | Yes (sentinel auto-adjust) |
| `network_recommendation` | `config_baseline` | `steward_network_eval` | `management` | No — requires preview |
| `network_baseline` | `baseline_comparison` | `steward_baseline_compare` | `management` | No — requires LLM evaluation |
| `pricing_trend` (significantly underpriced) | `pricing_eval` | `steward_pricing_strategy` | `management` | No — requires LLM evaluation |
| `model_popularity` (trending up, not loaded) | `model_demand_eval` | `steward_demand_assess` | `management` | No — requires LLM evaluation |
| `fleet_utilization` (all peers saturated) | `capacity_expansion_eval` | `steward_capacity_eval` | `management` | No — requires LLM evaluation |

**Auto-commit vs. steward behavior:**
- Auto-commit items are the **sentinel** — routine fixes that don't need judgment. The observation source writes the event with enough context in `metadata_json` for the compiler to produce a deterministic work item. The `apply_result` path makes the change directly (supersede contribution, update offer).
- Non-auto-commit items are the **steward** — decisions that need LLM evaluation through the preview gate. These work items go through the full dispatch lifecycle: compile → preview → (budget check → auto-commit or requires-approval) → dispatch to compute queue → LLM evaluates → apply_result.

**Distinguishing demand signal variants:** The event_type is always `demand_signal`, but the `metadata_json` carries a `signal_variant` field: `"high_demand_no_offer"`, `"low_demand_active_offer"`, etc. The extended `map_event_to_primitive` parses `metadata_json` to select the correct primitive. This follows the existing pattern where `derive_target_id` already parses `metadata_json` for rename events.

**Sentinel auto-commit implementation:** Auto-commit work items skip the LLM dispatch path entirely. The compiler creates them in `compiled` state. The supervisor's preview step marks them as auto-commit (existing `BudgetDecision::AutoCommit` path). The `apply_result` path applies the change directly — no LLM response to parse, the work item's `metadata_json` contains the deterministic action (e.g., `{"action": "increase_pricing", "delta_basis_points": 50, "model_id": "llama3:8b"}`).

---

## V. New Result Application Paths

Extend `DadbearSupervisor::apply_result` (currently in `dadbear_supervisor.rs:931`) with new primitive-type branches for market work items. The existing pattern dispatches on `primitive.as_str()` — market primitives add new match arms.

**P3 acknowledgment (from 2026-04-15 audit):** `apply_result` currently has no market dispatch surface. Phase 6 adds this as **new construction**, not a modification of existing branches. The five new match arms below each call a distinct subsystem — they do not share a common "apply market change" code path, because the subsystems being called (Ollama control-plane IPC, local config contribution supersession, Wire contribution API publication, local offer-state RPC, cross-slug hold placement) have nothing structurally in common beyond the shape of the match arm itself. Implementers should resist the instinct to build a shared abstraction during Phase 6 — the concrete repetition is deliberately preserved so each primitive's behavior is locally readable.

### Model Loading/Unloading

When `apply_result` processes a `model_portfolio_eval` or `model_unload_eval` work item whose LLM evaluation recommends action:

**Load path:**
1. Check experimental territory: `get_experimental_territory(conn)` — if `model_selection.status == "locked"`, skip. The territory contribution already has a `model_selection` dimension (see `default_experimental_territory` in `local_mode.rs:1717`).
2. Call existing Ollama control plane: `pyramid_ollama_pull_model` (IPC command, already handles concurrent pull guard and cancellation).
3. Wait for model to reach `ready` state (see Section VII state machine).
4. Create Wire offer via market RPC (only after `ready`).
5. Write chronicle event: `model_loaded` with `work_item_id` correlation.

**Unload path:**
1. Check experimental territory: same gate as load.
2. Transition model to `draining` state (see Section VII).
3. Suspend Wire offer (deactivate, don't delete — preserves performance history).
4. Wait for queue to drain (existing entries processed, new entries rejected).
5. Call existing Ollama control plane: `pyramid_ollama_delete_model` (IPC command, already refuses to delete the active model — the model selection logic handles switching first).
6. Write chronicle event: `model_unloaded`.

### Pricing Adjustment

When `apply_result` processes a `pricing_increase`, `pricing_decrease`, or `pricing_eval` work item:

1. Read current `compute_pricing` contribution for the model.
2. Compute new rate. For sentinel auto-adjustments: deterministic delta from `metadata_json`. For steward evaluations: rate recommended in LLM response.
3. Supersede via `supersede_config_contribution` (existing function in `config_contributions.rs`). The supersession pattern handles deactivating the old contribution and activating the new one atomically.
4. Update Wire offer with new effective rate (market RPC).
5. Write chronicle event: `pricing_adjusted` with old rate, new rate, and triggering `work_item_id`.

### Capacity Adjustment

When `apply_result` processes a `capacity_reduction` or `depth_reduction` work item:

1. Supersede `compute_capacity` contribution with new `max_market_depth`.
2. Update Wire offer.
3. Write chronicle event: `capacity_adjusted`.

### Offer Suspension

When `apply_result` processes an `offer_suspension` work item:

1. Deactivate Wire offer (suspend, not delete).
2. Place a `"suspended"` hold on the market:compute slug with reason from the work item.
3. Write chronicle event: `offer_suspended`.
4. The hold prevents the compiler from creating new work items for this model until the issue is resolved (operator clears the hold or a subsequent health check shows recovery).

### Experiment Publication

When `apply_result` processes a work item whose outcome is marked for publication:

1. Check experimental territory: `publication` dimension must be `"unlocked"` (see Section X).
2. Run redaction pass: strip absolute revenue numbers, retain only relative deltas (see Section X).
3. POST to Wire contribution API with `schema_type: steward_publication`.
4. Write chronicle event: `experiment_published`.

---

## VI. GPU Access for Management LLM Calls

### The Problem

Sentinel and steward LLM calls go through the compute queue (`compute_queue.rs`), which is the sole serializer for local GPU access. Per-model FIFO queues mean a 2b model sentinel check could sit behind a queue of 70b model market jobs. The 2b job waits for all ahead-of-queue 70b jobs to finish before the GPU becomes available for the small model.

### Why This Is Overstated

The compute queue is **per-model**. `ComputeQueueManager.queues` is `HashMap<String, ModelQueue>`. The GPU loop drains round-robin across models via `dequeue_next()` (line 115). So:

- If the management model (e.g., `qwen2.5:3b`) is **different** from the market model (e.g., `llama3:70b-q4`): they have separate per-model queues. The round-robin ensures the management model gets its turn every cycle. A single management check runs in seconds. There is **no contention** — the management model's queue is typically 0-1 items deep while the market model's queue has the production load.
- If the management model is the **same** as a market model: management calls enter the same FIFO queue. They wait behind existing entries but are never rejected. The management call latency equals its queue position * average job duration.

### Design: Separate Management Model

The sentinel and steward use a small model for management decisions. The model is specified via a contribution — `economic_parameter` with key `management_model_id`. No hardcoded default (Pillar 37). The seed contribution is set during market activation, not at build time.

**Why a small model:**
- Sentinel checks ("is queue utilization too high?") are classification tasks, not generation tasks. A 2b-3b model is sufficient.
- Steward evaluations ("should we change pricing?") need slightly more reasoning but still work well with 3b-7b models — they receive structured market data, not open-ended prompts.
- A small model fits alongside larger models in VRAM. On a 24GB GPU running a 70b quantized model (~20GB VRAM), a 3b model adds ~2GB — within budget.

**VRAM exhaustion fallback:** If VRAM is fully allocated and the management model cannot be loaded alongside the market model, management calls use the already-loaded market model. This is slower (the management call enters the market model's queue) but functional. The `model_tier: "management"` in the compiler mapping resolves to the management model via the AI Registry's tier routing. If the management model is unavailable, the registry falls back to the next available model in the tier. No dedicated VRAM reservation mechanism is needed — the per-model queue's FIFO naturally handles contention.

**Implementation:** The `management` model tier is added to the AI Registry's tier configuration (a contribution, not hardcoded). The compiler emits work items with `model_tier: "management"`. The supervisor's prompt materializer resolves the tier to a concrete model ID. The compute queue routes the call to that model's per-model queue. Round-robin ensures it drains.

---

## VII. Model Loading State Machine

### States

```
deciding → downloading → loading_vram → warming_up → ready → draining → unloaded
```

A model begins in `deciding` when a `model_portfolio_eval` work item's LLM response recommends loading it. It progresses through download and load, warms up with a small test inference, reaches `ready` (production traffic accepted), and eventually enters `draining` when an unload evaluation recommends removal.

### State Transitions

| From | To | Trigger | Side Effects |
|---|---|---|---|
| (none) | `deciding` | `model_portfolio_eval` work item recommends load | Work item created, awaiting dispatch |
| `deciding` | `downloading` | LLM evaluation confirms load, `apply_result` initiates pull | `pyramid_ollama_pull_model` IPC called |
| `downloading` | `loading_vram` | Pull completes, Ollama loads model to GPU | Monitored via pull progress events on `build_event_bus` |
| `loading_vram` | `warming_up` | Ollama reports model available | Per-model queue created (empty). Warmup inference dispatched. |
| `warming_up` | `ready` | Warmup inference succeeds, baseline metrics recorded | Wire offer created. Queue accepts market jobs. |
| `ready` | `draining` | `model_unload_eval` work item recommends unload | Wire offer suspended. Queue stops accepting new entries. |
| `draining` | `unloaded` | Queue empty (all in-flight jobs completed) | `pyramid_ollama_delete_model` IPC called. Queue removed. |

### State to Market Interaction

| State | Queue Exists? | Offer Active? | Accepting Market Jobs? |
|---|---|---|---|
| `deciding` | No | No | No |
| `downloading` | No | No | No |
| `loading_vram` | No | No | No |
| `warming_up` | Yes (empty) | No | No |
| `ready` | Yes | Yes | Yes |
| `draining` | Yes (draining) | No (suspended) | No |
| `unloaded` | No | No | No |

**Key invariant:** Wire offers are created ONLY when the model reaches `ready`. The queue mirror reflects actual model state. No offer exists for a model that is still downloading or loading.

**Draining protocol:** When entering `draining`, the queue is marked as non-accepting: `enqueue_local` returns an error for the draining model (the queue manager checks a `draining: bool` flag on the `ModelQueue`). Existing entries continue processing through the GPU loop's round-robin. When `entries.is_empty()` and no in-flight dispatch exists for that model, the drain is complete.

**State persistence:** Model states are stored in a `market_model_states` table (new, SQLite local). The table has columns: `model_id TEXT PRIMARY KEY, state TEXT NOT NULL, entered_at TEXT NOT NULL, metadata_json TEXT`. On crash recovery (supervisor Phase A), the supervisor reads this table and resumes from the persisted state. A model stuck in `downloading` for longer than the pull timeout is transitioned to `unloaded`. A model stuck in `draining` has its queue checked — if empty, complete the unload; if not, continue draining.

**Observation source integration:** State transitions write observation events to `market:compute`:
- `model_state_changed` with `metadata_json: { "model_id": "...", "from_state": "...", "to_state": "..." }`
- The compiler can react to these (e.g., a model stuck in `warming_up` for too long triggers an escalation hold).

---

## VIII. Experiment Lifecycle

### Experiment as DADBEAR Work Item

The steward's experiment loop maps directly to the DADBEAR work item lifecycle with one addition: a measurement hold placed during the observation window.

```
Observation event (pricing_trend, network_recommendation, etc.)
    → Compiler creates work item (primitive: pricing_eval, config_baseline, etc.)
    → Preview gate evaluates cost and experimental territory bounds
    → Dispatch: LLM evaluates and recommends a configuration change
    → apply_result: applies the change (supersedes contribution)
    → MEASUREMENT HOLD placed on slug ("measurement", reason: "pricing experiment {wi_id}")
    → Measurement window: observation sources continue writing events, but the compiler
      does NOT create new work items for the held slug (existing hold-aware behavior)
    → After window: clear the hold, compiler processes accumulated observations
    → Evaluation work item: compiler creates a follow-up work item that compares
      pre-experiment and post-experiment chronicle data
    → apply_result: keep (do nothing, new config stands) or revert (supersede back to prior config)
```

### Measurement Window Management

The measurement hold uses the existing `auto_update_ops::place_hold` with hold name `"measurement"`:

```rust
place_hold(conn, bus, "market:compute", "measurement",
    Some(&format!("pricing experiment {} — window until {}", wi_id, window_end)));
```

Window duration is an `economic_parameter` contribution (`experiment_measurement_window_minutes`). The hold carries the window end time in its reason string. A periodic check in the supervisor tick loop (new branch after the retention pass) scans for expired measurement holds and clears them:

```rust
// In supervisor tick loop, after retention check:
clear_expired_measurement_holds(conn, bus)?;
```

When the measurement hold clears, the slug becomes dispatchable again. The accumulated observation events (written during the measurement window but not compiled because the slug was held) are now compiled in the next pass, including the experiment evaluation event.

### Statistical Validity

Experiments must reach a minimum sample size before conclusions are drawn. This prevents thrashing on zero-data experiments (a critical gap identified in the audit).

**Minimum sample size:** `economic_parameter` contribution (`experiment_min_sample_size`). Seeded during market activation. This is the minimum number of completed market jobs during the measurement window required for a valid conclusion.

**Maximum window extension:** `economic_parameter` contribution (`experiment_max_window_minutes`). If the minimum sample size is not reached within the initial measurement window, the window extends (hold remains in place) up to this maximum.

**Decision logic (in the evaluation work item's `apply_result`):**

| Condition | Action |
|---|---|
| Sample size >= minimum, metric improved | Keep new config (do nothing) |
| Sample size >= minimum, metric unchanged or degraded | Revert to prior config (supersede back) |
| Sample size < minimum, window < max | Extend window (hold stays) |
| Sample size < minimum, window >= max | Revert to prior config (insufficient data for conclusion) |

**Low-traffic degraded mode:** When a node consistently cannot reach minimum sample sizes (low-traffic node), the steward falls back to network-published configs (`network_recommendation` events from Source 3) instead of running local experiments. The compiler detects this pattern: if the last N experiment evaluations all resulted in "insufficient data" reverts (count from `dadbear_work_items WHERE primitive = 'experiment_eval' AND result_json LIKE '%insufficient%'`), it stops creating local experiment work items for that slug and switches to `config_baseline` work items from network data.

### Concurrent Experiment Isolation

**Same-slug isolation:** The `"measurement"` hold prevents two experiments on the same slug simultaneously. If a new experiment-triggering observation arrives while a measurement hold is active, it accumulates in `dadbear_observation_events` but is not compiled (the slug is held, so `gather_dispatchable_items` calls `block_held_items` and skips it). After the hold clears, the compiler processes the accumulated observations and may start a new experiment.

**Cross-slug independence:** Different slugs CAN experiment concurrently. `market:compute` pricing experiment + `market:storage` pricing experiment = both proceed independently. Each slug has its own holds projection row.

**Cross-market resource experiments (VRAM allocation):** Require a cross-slug hold. The `apply_result` path for `budget_rebalance` work items places holds on ALL three slugs simultaneously before applying the allocation change:

```rust
place_hold(conn, bus, "market:compute", "measurement", Some("cross-market budget rebalance"));
place_hold(conn, bus, "market:storage", "measurement", Some("cross-market budget rebalance"));
place_hold(conn, bus, "market:relay", "measurement", Some("cross-market budget rebalance"));
```

This prevents the serial-experiment failure mode where compute "wins" all the VRAM by experimenting first.

---

## IX. Cross-Market Resource Management

### The Problem

On unified memory architectures (Apple M-series, integrated GPUs), VRAM is zero-sum. Compute models and storage cache both draw from the same memory pool. If the compute market slug experiments with loading larger models, it can exhaust memory available for storage caching, degrading storage market performance. Serial per-slug experiments cannot discover joint optima.

### Resource Budget Design

**Budget contribution:** A new `resource_budget` configuration contribution defines the allocation split:

```yaml
schema_type: resource_budget
allocations:
  compute_vram_pct: 70    # percentage of detected VRAM for compute models
  storage_cache_pct: 20   # percentage for filesystem cache
  relay_buffer_pct: 5     # percentage for relay packet buffers
  system_reserve_pct: 5   # minimum reserve (never allocated to markets)
```

**Hardware detection:** Total VRAM budget is read from existing hardware detection (already ships — the Ollama probe returns available memory). The budget contribution's percentages are applied against this total.

### Implementation

**New observation source: resource utilization monitor**

Writes to all three market slugs:

| Event Type | Description |
|---|---|
| `resource_pressure` | VRAM used per market exceeds its allocation |
| `resource_underutilized` | A market is significantly below its allocation |

Trigger: periodic, same interval as chronicle health monitor. Reads VRAM usage from Ollama API (model sizes) and filesystem cache metrics.

**Compiler mapping:**

| Event Type | Primitive | Auto-Commit? |
|---|---|---|
| `resource_pressure` (single market over budget) | `capacity_reduction` | Yes — sentinel reduces the over-budget market's allocation |
| `resource_pressure` (multiple markets over budget) | `budget_rebalance` | No — steward evaluates the joint allocation |
| `resource_underutilized` | `budget_expansion_eval` | No — steward evaluates whether the unused allocation should be offered to other markets |

**Budget rebalance lifecycle:**
1. Observation: resource utilization monitor detects cross-market pressure
2. Compiler: creates `budget_rebalance` work item
3. LLM evaluation: steward examines demand signals, utilization, and revenue per market to recommend a new split
4. Apply: supersede `resource_budget` contribution + place cross-slug measurement holds (see Section VIII)
5. Measure: wait for all three markets to show stable performance under new allocation
6. Keep/revert: evaluate per the standard experiment lifecycle

---

## X. Publication Privacy

### The Problem

Steward analysis contains competitively sensitive information. A raw experiment report reveals pricing strategy, revenue figures, hardware capabilities, and demand patterns. Publishing this to the Wire without redaction leaks competitive intelligence.

### Publishable vs. Private Fields

| Field | Publishable? | Rationale |
|---|---|---|
| Relative performance delta | Yes | "+15% throughput after config change" reveals improvement, not absolutes |
| Absolute revenue numbers | **No** | Direct competitive intelligence |
| Pricing strategy details | **No** | Strategy type publishable ("dynamic queue-based"), parameters not |
| Hardware class | Yes | Enables network matching — already visible on Wire offers |
| Model + config combination | Yes | Enables network learning — the whole point of publication |
| Queue utilization patterns | **No** | Reveals demand volume |
| Experiment methodology | Yes | Chain YAML hash — enables methodology sharing |
| Raw performance metrics | **No** | Combined with pricing, reveals revenue |
| Relative performance vs. network baseline | Yes | "12% above median" — enables benchmarking |

### Publication Opt-In

The experimental territory contribution gains a new dimension: `publication`:

```yaml
schema_type: experimental_territory
dimensions:
  model_selection: { status: "unlocked" }
  context_limit: { status: "locked" }
  concurrency: { status: "locked" }
  publication: { status: "locked" }     # <-- new dimension
```

Default: `locked`. The operator must explicitly unlock publication to participate in network learning. This is surfaced in the existing experimental territory UI.

### Redaction Pass

Before any Wire contribution is created from experiment results, a redaction function strips private fields:

```rust
fn redact_for_publication(experiment_result: &Value) -> Value {
    // Keep: relative_delta, hardware_class, model_id, config_yaml_hash,
    //       experiment_type, methodology_chain_hash, relative_vs_baseline
    // Strip: absolute_revenue, pricing_parameters, queue_depth_raw,
    //        raw_throughput, raw_latency, raw_error_count
}
```

The redaction function is deterministic and auditable. It does not use LLM judgment — it is a hard-coded field allowlist applied mechanically.

---

## XI. Wire Workstream

### Publication Contribution Type

New `schema_type: steward_publication` for Wire contributions:

```yaml
schema_type: steward_publication
hardware_class: "apple_m2_pro_16gb"
model_id: "llama3:8b-q4_K_M"
experiment_type: "pricing_optimization"
relative_delta:
  throughput_change_pct: 15.2
  revenue_change_pct: 8.7      # relative to pre-experiment, not absolute
  quality_change_pct: -0.3
methodology_chain_hash: "a1b2c3d4"
config_snapshot:                 # the config that produced the improvement
  rate_per_m_input: 150          # basis points, not dollars
  rate_per_m_output: 200
  max_market_depth: 4
network_baseline_comparison:
  vs_median_throughput_pct: 12.1
  vs_median_latency_pct: -8.4   # negative = better
```

The contribution follows the standard Wire contribution lifecycle: creation, pricing (set by publisher), citation via `derived_from` chains, retraction.

### Config Recommendation RPC

New Wire endpoint:

```
GET /api/v1/compute/config-recommendations
Parameters:
  hardware_class: string (required) — e.g., "apple_m2_pro_16gb"
  model_family: string (optional) — e.g., "llama3"
Returns:
  recommendations: [
    {
      model_id: string,
      config: { rate_per_m_input, rate_per_m_output, max_market_depth, ... },
      sample_size: integer,           # how many nodes contributed to this median
      performance_baseline: {
        median_throughput_tps: float,
        median_latency_p95_ms: float,
      }
    }
  ]
```

The endpoint aggregates data from `steward_publication` contributions. It does NOT return individual node data — only statistical aggregates (medians) across nodes with matching hardware class.

**Implementation:** Wire-side SQL view or materialized query over `steward_publication` contributions, grouped by `(hardware_class, model_id)`, computing median config values and performance baselines.

### Subscription Mechanism

Operators subscribe to steward publications for specific hardware classes and/or model families. The subscription is a standard Wire list (existing `wire_lists` infrastructure) with criteria type `steward_publication` and filter parameters.

**Delivery:** New publications matching a subscription appear in the heartbeat response's `new_contributions_since_last` field. The heartbeat demand signal extractor (Source 1) reads these and writes them as `network_recommendation` observation events, completing the feedback loop.

---

## XII. Market Chain YAML

Steward chains use the **existing** chain format (`ChainDefinition` in `chain_engine.rs`) with new mechanical step types registered in `chain_dispatch.rs`'s `dispatch_mechanical` function.

### Example: Pricing Evaluation Chain

```yaml
name: market_pricing_eval
content_type: market_intelligence
defaults:
  model_tier: management

steps:
  - name: gather_market_data
    mechanical: true
    rust_function: query_market_surface
    input:
      model_id: "{{model_id}}"

  - name: gather_local_data
    mechanical: true
    rust_function: query_chronicle_stats
    input:
      model_id: "{{model_id}}"
      window_hours: "{{trend_window_hours}}"

  - name: evaluate_pricing
    instruction: prompts/market/pricing_eval.md
    model_tier: management
    input:
      market_data: "$gather_market_data"
      local_data: "$gather_local_data"
      current_pricing: "{{current_pricing_yaml}}"
    response_schema:
      type: object
      properties:
        recommendation:
          type: string
          enum: [increase, decrease, hold]
        new_rate_per_m_input:
          type: integer
        new_rate_per_m_output:
          type: integer
        reasoning:
          type: string

  - name: apply_if_approved
    mechanical: true
    rust_function: supersede_pricing_contribution
    input:
      model_id: "{{model_id}}"
      recommendation: "$evaluate_pricing.recommendation"
      new_rate_per_m_input: "$evaluate_pricing.new_rate_per_m_input"
      new_rate_per_m_output: "$evaluate_pricing.new_rate_per_m_output"
```

### New Mechanical Steps

Registered in `dispatch_mechanical` (in `chain_dispatch.rs:579`). Each new function follows the existing pattern: `match function_name { ... }`.

| Function Name | Description | Input | Output |
|---|---|---|---|
| `query_market_surface` | Read current market data for a model from Wire cache / last heartbeat | `{ model_id }` | `{ unfilled_bids, competing_offers, price_range, demand_trend }` |
| `query_chronicle_stats` | Aggregate local chronicle events for a model over a time window | `{ model_id, window_hours }` | `{ throughput_avg, latency_p95, error_rate, queue_depth_avg, job_count }` |
| `supersede_pricing_contribution` | Supersede the `compute_pricing` contribution for a model | `{ model_id, new_rate_per_m_input, new_rate_per_m_output }` | `{ contribution_id, old_rate, new_rate }` |
| `supersede_capacity_contribution` | Supersede the `compute_capacity` contribution | `{ max_market_depth }` | `{ contribution_id, old_depth, new_depth }` |
| `publish_to_wire` | Create a `steward_publication` contribution on the Wire (with redaction) | `{ experiment_result, hardware_class, model_id }` | `{ contribution_id, published }` |
| `query_resource_utilization` | Read current VRAM/disk allocation per market | `{}` | `{ compute_vram_used_mb, storage_cache_used_mb, total_vram_mb }` |
| `initiate_model_load` | Begin the model loading state machine | `{ model_id }` | `{ state: "downloading", model_id }` |
| `initiate_model_drain` | Begin the model draining/unload sequence | `{ model_id }` | `{ state: "draining", model_id }` |

### Chain as Contribution

Each market chain is a YAML file in the `chains/market/` directory, shipped as a bundled contribution (same as existing pyramid build chains). Operators can supersede with custom chains from the Wire or local modifications. The chain registry (`chain_registry.rs`) resolves the active chain via the standard tier system: per-slug override > content-type default > bundled default.

---

## XIII. Frontend Workstream

### Market Intelligence Dashboard

The primary market overview. Shows:

- **Demand signals:** unfilled bids by model, trending models, pricing trends. Source: observation events with `source: "heartbeat"`.
- **Revenue breakdown:** per-model, per-market. Source: chronicle compute events (local only — never published). Displayed as relative trends (up/down/flat), not absolute numbers.
- **Network position:** this node's performance vs. network baseline. Source: `network_baseline` observation events.

### Model Portfolio View

Per-model management interface. Shows:

- **Loaded models:** current list with state machine visualization (the state from Section VII rendered as a horizontal pipeline: deciding → downloading → ... → ready).
- **Demand per model:** from heartbeat demand signals.
- **Revenue per model:** from chronicle events.
- **Load/unload actions:** manual trigger that creates observation events (same path as autonomous — no special manual code path).

### Pricing Optimizer

Per-model pricing interface. Shows:

- **Current pricing curve:** from active `compute_pricing` contribution.
- **Market comparison:** this node's rates vs. network median (from `network_baseline` events).
- **Suggested adjustments:** most recent steward pricing evaluation (from work item results).
- **Experiment preview:** before launching a pricing experiment, shows expected measurement window duration and minimum sample requirements.

### Experiment Log

History of all steward experiments. Shows:

- **Running experiments:** which slugs have `"measurement"` holds, time remaining in window, samples collected so far vs. minimum.
- **Completed experiments:** outcome (keep/revert), metric deltas, sample size, window duration.
- **Published experiments:** which results were published to Wire, contribution IDs.
- **Failed experiments:** insufficient samples, reverts, error conditions.

Data source: `dadbear_work_items` filtered by market primitives + `dadbear_hold_events` filtered by `"measurement"` holds.

### Sentinel Activity Log

Auto-adjustment history. Shows:

- **Recent auto-adjustments:** sentinel work items that were auto-committed. What changed, when, trigger event.
- **Escalations:** holds placed by the sentinel for steward review. Current escalation holds and their reasons.
- **Health check history:** observation events from the chronicle health monitor. Trend lines for throughput, queue depth, error rate.

Data source: `dadbear_work_items` filtered by sentinel step names (`sentinel_price_bump`, `sentinel_price_drop`, etc.) + observation events with `source: "chronicle_health"`.

---

## XIV. Verification Criteria

Each criterion traces an end-to-end path through the system:

1. **Demand signal to model load:** Heartbeat response contains unfilled bids for model X → heartbeat extractor writes `demand_signal` observation event → compiler creates `model_portfolio_eval` work item → supervisor dispatches to management model → LLM recommends loading → `apply_result` calls `pyramid_ollama_pull_model` → model reaches `ready` → Wire offer created.

2. **Queue depth to pricing adjustment:** Chronicle health monitor detects queue depth above `queue_depth_high` → writes `queue_utilization` observation event → compiler creates `pricing_increase` work item (auto-commit) → supervisor auto-commits → `apply_result` supersedes `compute_pricing` contribution → Wire offer updated → queue depth stabilizes over subsequent monitoring windows.

3. **Experiment lifecycle:** `pricing_trend` event shows significant underpricing → compiler creates `pricing_eval` work item → steward chain executes (gather market data, gather local data, LLM evaluate) → `apply_result` supersedes pricing contribution → `"measurement"` hold placed → measurement window elapses → hold cleared → evaluation work item created → sufficient samples: metric improved → keep new pricing.

4. **Experiment revert on insufficient data:** Same as #3 but the node has low traffic → measurement window extends to max → still below minimum sample size → revert to prior config → after N consecutive reverts, steward switches to network-published configs.

5. **Cross-market resource management:** Resource utilization monitor detects compute VRAM exceeding budget while storage cache is under-allocated → `resource_pressure` event → compiler creates `budget_rebalance` work item → steward evaluates joint allocation → cross-slug measurement holds placed on all three market slugs → new allocation applied → measurement window → all three markets stable → keep.

6. **Publication with privacy:** Experiment completes with positive result → check `publication` dimension in experimental territory → unlocked → run redaction pass (strip absolute revenue, queue depth, raw metrics) → POST `steward_publication` contribution to Wire → another node receives via subscription in heartbeat → writes `network_recommendation` observation event → adopts config.

7. **Sentinel auto-adjustment:** Throughput degradation detected by chronicle health monitor → `throughput_drift` observation event → compiler creates `capacity_reduction` work item (auto-commit) → supervisor auto-commits → `apply_result` supersedes `compute_capacity` contribution with reduced `max_market_depth` → Wire offer updated.

8. **Sentinel escalation:** Error rate above threshold AND throughput degrading simultaneously → sentinel creates `offer_suspension` work item (auto-commit) → offer deactivated → `"suspended"` hold placed → operator sees hold in UI → operator investigates, resolves issue, clears hold → slug becomes dispatchable → health monitor confirms recovery → sentinel creates `offer_resumption` work item.

9. **Management LLM access under load:** Market model queue has 10 entries → sentinel health check fires → management model (different from market model) has empty queue → round-robin dispatches management check immediately → check completes in seconds, independent of market queue depth.

10. **Model state machine crash recovery:** Node crashes during model download → supervisor restart → Phase A crash recovery reads `market_model_states` table → model in `downloading` state for longer than pull timeout → transition to `unloaded` → no stale offer on Wire (offer was never created — only created at `ready`).

---

## XV. Handoff

Phase 6 is the final compute market phase in this build plan. After this phase ships, the compute market is operational with:
- Job matching, settlement, and quality enforcement (Phases 2-5)
- Autonomous pricing, capacity, and model portfolio management (Phase 6)
- Network learning via steward publication and subscription
- Operator controls via experimental territory and holds

**Future work (separate build plans, not phases of this plan):**
- Relay market intelligence (same DADBEAR pattern, `market:relay` slug already created in this phase, observation sources and compiler mappings are relay-market-specific)
- Storage market intelligence (same pattern, `market:storage` slug)
- Clean Room / Vault privacy tiers (referenced in known issue #1)
- Compute contracts (long-term committed capacity, referenced in known issue #4)
- Advanced fleet intelligence (fleet routing optimization based on market signals)

**What the next implementer needs to know:**
- The three DADBEAR slugs are created in this phase but only `market:compute` has full observation sources and compiler mappings. `market:storage` and `market:relay` are structural placeholders — their slugs exist, their holds work, but their observation sources are stubs until the storage and relay market build plans ship.
- Market chain YAML files are in `chains/market/` and follow the standard chain contribution lifecycle.
- All numeric thresholds are `economic_parameter` contributions, never hardcoded values.
- The model loading state machine's `market_model_states` table is local SQLite only — it does not sync to Wire.

---

## XVI. Audit Corrections Applied

| Audit Finding | How Addressed | Section |
|---|---|---|
| Theme 7: Phases 6-9 should collapse into one phase | Single phase doc replaces four. Justification in Section I. | I |
| Theme 1: DADBEAR absent from plan | Every component maps to DADBEAR primitives. Mapping table in Section II. | II |
| Auditor B: GPU access for management calls unspecified | Per-model queue separation + management model tier. No dedicated VRAM reservation needed. | VI |
| Auditor B: Model loading state machine undefined | 7-state machine with offer lifecycle tied to state. Crash recovery specified. | VII |
| Auditor B: Cross-market VRAM conflict | Resource budget contribution + cross-slug measurement holds. | IX |
| Auditor B: Experiment statistical validity | Minimum sample size, max window extension, low-traffic degraded mode. | VIII |
| Auditor B: Publication privacy leaks competitive intelligence | Field-level publishable/private distinction, opt-in via experimental territory, deterministic redaction. | X |
| Addendum C: Three daemons, not one | Three DADBEAR slugs, not three daemon instances. Same supervisor. | II |
| DD-10: Steward-mediated operation | Steward = DADBEAR compiler + observation sources. Operator sets experimental territory boundaries, steward operates within them autonomously. | II, V |
| Known issue #5: Experimental territory | Existing territory contribution with `model_selection` dimension already integrated. New `publication` dimension added. | V, X |
| Pillar 37: No hardcoded numbers | All thresholds, intervals, model selections, and window durations are `economic_parameter` contributions with seed values set during market activation, not build-time defaults. | III, IV, VI, VII, VIII |
