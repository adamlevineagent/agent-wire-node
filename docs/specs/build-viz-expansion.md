# Build Visualization Expansion Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** LLM output cache (for cache hit detection + cost data), live pyramid build visualization (for existing LayerEvent + PyramidBuildViz infrastructure)
**Unblocks:** Full execution trace visibility, cost observability per build, cache effectiveness monitoring
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

The live pyramid build visualization (existing spec) shows node-level progress: layers appear, cells fill in, the pyramid grows. But it tells you nothing about what the system is doing inside each step. Which model is running? Was it a cache hit? How much did it cost? Did it retry?

The vision doc says every step should emit its own event type and the viz should show the complete execution trace. This spec extends the existing `TaggedKind` enum and `PyramidBuildViz.tsx` to expose step-level detail: LLM calls, cache hits, web edge generation, evidence answering, gap processing, cluster assignment, triage decisions, and errors/retries.

---

## Current State

### Backend

| Component | What It Provides | What's Missing |
|---|---|---|
| `TaggedKind` enum (event_bus.rs) | `ChainStepStarted`, `ChainStepFinished`, `Progress`, `V2Snapshot`, `CostUpdate` | No per-LLM-call events, no cache hit events, no sub-step events |
| `BuildEventBus` (event_bus.rs) | broadcast::channel(4096), 60ms coalescing for Progress/V2Snapshot, discrete events bypass coalesce | Infrastructure ready for new event types |
| `LayerEvent` channel (types.rs) | `Discovered`, `NodeCompleted`, `LayerCompleted`, `NodeFailed`, `StepStarted`, `Log` | Node-level only, no step-execution detail |
| `pyramid_llm_audit` (db.rs) | Every LLM call logged: prompts, responses, tokens, latency, cost | Write-only log, not surfaced in viz |
| `pyramid_step_cache` (LLM output cache spec) | Cache key, output, cost, latency per step | Not yet built; when built, provides cache hit detection |
| `pyramid_cost_log` (db.rs) | Cost breakdown by operation, estimated + actual | Not surfaced in build viz |

### Frontend

| Component | What It Shows | What's Missing |
|---|---|---|
| `PyramidBuildViz.tsx` | Layer rows, cells, density bars, activity log | No step timeline, no cost, no cache indicators, no trace detail |
| `BuildProgress.tsx` | Legacy progress bar (fallback) | No step awareness at all |

---

## New Event Types

### TaggedKind Extensions

Add these variants to the existing `TaggedKind` enum in `event_bus.rs`:

```rust
// ── Build viz expansion: step-level introspection events ──

/// An LLM call has been dispatched (before HTTP request).
LlmCallStarted {
    step_name: String,
    primitive: String,
    model_tier: String,
    model_id: String,          // resolved model name from tier routing
    cache_key: String,         // for correlation with cache hits
    depth: i64,
    chunk_index: Option<i64>,
},

/// An LLM call completed successfully.
LlmCallCompleted {
    step_name: String,
    cache_key: String,
    tokens_prompt: i64,
    tokens_completion: i64,
    cost_usd: f64,            // estimated cost
    latency_ms: i64,
    model_id: String,
},

/// A step output was served from the LLM output cache (no LLM call made).
CacheHit {
    step_name: String,
    cache_key: String,
    original_model_id: String, // model that produced the cached output
    original_cost_usd: f64,   // what it cost originally (savings = this amount)
    depth: i64,
    chunk_index: Option<i64>,
},

/// Web edge generation started for a set of nodes.
WebEdgeStarted {
    step_name: String,
    source_node_count: i64,
},

/// Web edge generation completed.
WebEdgeCompleted {
    step_name: String,
    edges_created: i64,
    latency_ms: i64,
},

/// Evidence answering: a question is being triaged or answered.
EvidenceProcessing {
    step_name: String,
    question_count: i64,
    action: String,            // "triage", "answer", "defer", "skip"
    model_tier: String,
},

/// Gap analysis: processing gaps for a layer.
GapProcessing {
    step_name: String,
    depth: i64,
    gap_count: i64,
    action: String,            // "identify", "fill", "defer"
},

/// Cluster assignment: nodes assigned to clusters for synthesis.
ClusterAssignment {
    step_name: String,
    depth: i64,
    node_count: i64,
    cluster_count: i64,
},

/// Triage decision made (evidence or gap triage).
TriageDecision {
    step_name: String,
    item_id: String,
    decision: String,          // "answer", "defer", "skip"
    reason: String,
},

/// An LLM call failed and is being retried.
StepRetry {
    step_name: String,
    attempt: i64,
    max_attempts: i64,
    error: String,
    backoff_ms: i64,
},

/// An LLM call failed permanently (all retries exhausted).
StepError {
    step_name: String,
    error: String,
    depth: i64,
    chunk_index: Option<i64>,
},
```

### Discrete vs Coalesced

All new event types are discrete (low-frequency, each one matters). They bypass the 60ms coalesce buffer. The existing `is_discrete()` method on `TaggedKind` already returns `true` for anything that isn't `Progress` or `V2Snapshot` — no change needed.

### Event Emission Points

| Event | Emitted By | File:Function |
|-------|-----------|---------------|
| LlmCallStarted | call_model_unified (before HTTP request) | `llm.rs:call_model_unified()` |
| LlmCallCompleted | call_model_unified (after successful response) | `llm.rs:call_model_unified()` |
| LlmCallFailed | call_model_unified (after retries exhausted) | `llm.rs:call_model_unified()` |
| CacheHit | call_model_unified (after cache lookup match) | `llm.rs:call_model_unified()` |
| CacheMiss | call_model_unified (after cache lookup no match) | `llm.rs:call_model_unified()` |
| WebEdgeStarted | execute_webbing (start of web call) | `chain_executor.rs:execute_webbing()` |
| WebEdgeCompleted | execute_webbing (after web call completes) | `chain_executor.rs:execute_webbing()` |
| EvidenceTriageStarted | triage_evidence_question (per question) | `triage.rs:triage_evidence_question()` |
| TriageDecision | triage_evidence_question (after triage LLM returns) | `triage.rs:triage_evidence_question()` |
| EvidenceAnsweringStarted | answer_question (per question) | `evidence_answering.rs:answer_question()` |
| EvidenceAnsweringCompleted | answer_question (after answer synthesis) | `evidence_answering.rs:answer_question()` |
| GapProcessingStarted | process_gaps (start) | `chain_executor.rs:process_gaps()` |
| GapProcessingCompleted | process_gaps (end) | `chain_executor.rs:process_gaps()` |
| ClusterAssignment | recursive_cluster (per cluster decision) | `chain_executor.rs:recursive_cluster()` |
| NodeRerolled | pyramid_reroll_node IPC handler | `routes.rs:handle_reroll_node()` |
| ManifestGenerated | generate_change_manifest | `stale_helpers_upper.rs:generate_change_manifest()` |
| CostUpdate | pyramid_cost_log INSERT/UPDATE | Emitted from any function that writes to pyramid_cost_log; centralized in a `log_cost()` helper |

All events receive the `bus: &Arc<BuildEventBus>` via StepContext (see StepContext coordination fix above) or via explicit parameter for functions outside step dispatch.

### Threading the Event Bus

`call_model_unified()` in `llm.rs` receives `LlmConfig` + prompts + a `StepContext`. The `StepContext` struct is the single context object threaded through all LLM-calling code paths and is **defined in `llm-output-cache.md` (see "Threading the Cache Context")**. This spec consumes the `bus: Arc<BuildEventBus>` field for event emission.

The canonical `StepContext` definition combines cache, event bus, and step metadata in one struct — do not define a parallel struct here. If an LLM call is non-observable (config generation, stale checks outside a build), construct a `StepContext` whose `bus` is a no-op/detached bus, OR skip the event emission branch in `call_model_unified()` when the context indicates a non-build path. Event emission is transparent to callers: every build-path LLM call emits; non-build paths do not.

See `llm-output-cache.md` for the full `StepContext` struct definition (build metadata, cache fields, event bus, and model resolution fields).

---

## Frontend: Step Timeline

### Layout

`PyramidBuildViz.tsx` gains a step timeline panel below (or alongside) the pyramid visualization:

```
┌─────────────────────────────────────────────────────────────────────┐
│  [Pyramid visualization — existing layers/cells/density bars]      │
│                                                                     │
│  Cost: $0.47 est / $0.42 actual    Cache hits: 12/45 (27%)         │
│                                                                     │
│  ┌─ Step Timeline ───────────────────────────────────────────────┐  │
│  │                                                               │  │
│  │  extract_chunks        ████████████████████  87/112  $0.31   │  │
│  │  thread_clustering     ████████████         7/7     $0.04    │  │
│  │  synthesize_threads    ██████████████       5/7     $0.08    │  │
│  │  web_edges            ██████████████████    done    $0.02    │  │
│  │  recursive_pair       ███████              2/3     $0.02    │  │
│  │  apex_synthesis       ░░░░░░░░░░░░░░░░░░   pending  —       │  │
│  │                                                               │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ┌─ Step Detail: synthesize_threads ─────────────────────────────┐  │
│  │  Primitive: for_each          Model: claude-sonnet-4-20250514       │  │
│  │  Tier: synth_heavy            Cache: 2/7 hits                │  │
│  │  Tokens: 24,312 in / 8,441 out                               │  │
│  │  Cost: $0.08 est              Latency: avg 4.2s              │  │
│  │                                                               │  │
│  │  ┌─ Call 1 ─────────────────────────────────────────────────┐ │  │
│  │  │ cached  model: claude-sonnet-4-20250514  $0.00  0ms           │ │  │
│  │  └──────────────────────────────────────────────────────────┘ │  │
│  │  ┌─ Call 2 ─────────────────────────────────────────────────┐ │  │
│  │  │ done  model: claude-sonnet-4-20250514  $0.012  3.8s  1.2k/420 │ │  │
│  │  └──────────────────────────────────────────────────────────┘ │  │
│  │  ┌─ Call 3 ─────────────────────────────────────────────────┐ │  │
│  │  │ running  model: claude-sonnet-4-20250514  ...                 │ │  │
│  │  └──────────────────────────────────────────────────────────┘ │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### Step Row States

Each chain YAML step is a row in the timeline. The step's state is derived from events received:

| State | Badge | Bar Color | Trigger |
|---|---|---|---|
| pending | gray "pending" | empty | Step exists in chain YAML but no `ChainStepStarted` received |
| running | blue "running" | blue, animated | `ChainStepStarted` received |
| completed | green "done" | solid green | `ChainStepFinished` with status = "ok" |
| cached | green "cached" | green with flash | All LLM calls for this step were `CacheHit` events |
| partial_cache | green "done" + cache badge | green | Mix of `CacheHit` and `LlmCallCompleted` |
| failed | red "failed" | solid red | `ChainStepFinished` with status = "error" |
| retrying | orange "retry N/M" | orange pulse | `StepRetry` received |

### Cache Hit Display

When a step is a cache hit:
- The step row shows instantly as green with a "cached" badge
- Cost shows "$0.00" (no LLM call made)
- Latency shows "<1ms"
- The step detail view shows "Served from cache" with the original model and cost

Visual treatment: cache hits use a brief green flash animation on appearance (distinct from the steady fill of a running step). This makes cache effectiveness visible at a glance.

### Cost Accumulator

A running cost display at the top of the viz, updated with each `LlmCallCompleted` or `CostUpdate` event:

```
Cost: $0.47 est / $0.42 actual    Cache savings: $0.12
```

- **Estimated**: Sum of `cost_usd` from `LlmCallCompleted` events
- **Actual**: From `CostUpdate` events (fed by OpenRouter Broadcast webhook when available)
- **Cache savings**: Sum of `original_cost_usd` from `CacheHit` events (what would have been spent without the cache)

The accumulator resets at build start and persists for the build's lifetime.

### Full Trace View

Clicking a step row expands it to show per-call detail. Each LLM call within the step is a sub-row:

| Field | Source |
|---|---|
| Status | `LlmCallStarted` -> "running", `LlmCallCompleted` -> "done", `CacheHit` -> "cached", `StepError` -> "failed" |
| Model | `model_id` from `LlmCallStarted` or `LlmCallCompleted` |
| Tokens | `tokens_prompt` / `tokens_completion` from `LlmCallCompleted` |
| Cost | `cost_usd` from `LlmCallCompleted`, "$0.00" for cache hits |
| Latency | `latency_ms` from `LlmCallCompleted`, "<1ms" for cache hits |
| Cache key | `cache_key` from any event (for debugging) |

The trace view does NOT show prompt/response content. That data is in `pyramid_llm_audit` and `pyramid_step_cache` and can be accessed via a "View full output" link that opens the step's cached output in a detail panel (future work, not in this spec).

---

## Event Schema (Wire Format)

Events are serialized as JSON over the WebSocket. The existing `TaggedBuildEvent` wrapper is preserved:

```json
{
  "slug": "my-pyramid",
  "kind": {
    "type": "llm_call_completed",
    "step_name": "extract_chunks",
    "cache_key": "sha256:abc123...",
    "tokens_prompt": 1200,
    "tokens_completion": 420,
    "cost_usd": 0.012,
    "latency_ms": 3800,
    "model_id": "claude-sonnet-4-20250514"
  }
}
```

The `#[serde(tag = "type", rename_all = "snake_case")]` on `TaggedKind` handles serialization. New variants are automatically snake_cased (`LlmCallStarted` -> `"llm_call_started"`).

---

## Integration with LLM Output Cache

The LLM output cache spec defines `pyramid_step_cache` with `cache_key`, `output_json`, `cost_usd`, `latency_ms`. The build viz expansion uses this data in two ways:

1. **Cache hit events**: When `call_model_unified()` finds a cache hit, it emits a `CacheHit` event with the cached entry's metadata (model, cost, latency from the original call).

2. **Step status pre-population**: When a build resumes after a crash, completed steps have cached outputs. The viz can query `pyramid_step_cache` for the current build's completed steps and pre-populate the step timeline with "cached" status, even though no live events were received.

### Pre-population Query

```sql
SELECT step_name, model_id, cost_usd, latency_ms, cache_key
FROM pyramid_step_cache
WHERE slug = ?1 AND build_id = ?2
ORDER BY created_at ASC;
```

This is queried once when the viz component mounts (or when polling starts) and used to seed the initial step timeline state.

---

## Frontend State Management

### StepTimelineState

```typescript
interface StepCall {
  cacheKey: string;
  status: 'running' | 'completed' | 'cached' | 'failed' | 'retrying';
  modelId: string;
  tokensPrompt?: number;
  tokensCompletion?: number;
  costUsd?: number;
  latencyMs?: number;
  attempt?: number;
  maxAttempts?: number;
  error?: string;
}

interface StepState {
  stepName: string;
  primitive: string;
  modelTier: string;
  status: 'pending' | 'running' | 'completed' | 'cached' | 'partial_cache' | 'failed' | 'retrying';
  calls: StepCall[];
  totalCostUsd: number;
  totalTokensPrompt: number;
  totalTokensCompletion: number;
  cacheHits: number;
  cacheMisses: number;
  depth: number;
}

interface CostAccumulator {
  estimatedUsd: number;
  actualUsd: number | null;  // null until first CostUpdate from Broadcast
  cacheSavingsUsd: number;
}

interface StepTimelineState {
  steps: StepState[];
  cost: CostAccumulator;
  expandedStep: string | null;  // step_name of expanded detail view
}
```

### Event Reduction

Events from the WebSocket are reduced into `StepTimelineState`:

```typescript
function reduceEvent(state: StepTimelineState, event: TaggedKind): StepTimelineState {
  switch (event.type) {
    case 'chain_step_started':
      // Find or create step entry, set status to 'running'
    case 'llm_call_started':
      // Add a new StepCall with status 'running' to the step's calls array
    case 'llm_call_completed':
      // Update the matching StepCall, add to cost accumulator
    case 'cache_hit':
      // Add a StepCall with status 'cached', add to cache savings
    case 'chain_step_finished':
      // Set step status to 'completed' or 'failed'
    case 'step_retry':
      // Update matching StepCall with retry info
    case 'step_error':
      // Update matching StepCall with error
    case 'cost_update':
      // Update actual cost in accumulator
    // ... other event types update their respective step states
  }
}
```

### Polling vs WebSocket

The existing build viz uses 2s polling. The step timeline can use either:

- **Polling path**: The `pyramid_build_progress_v2` endpoint is extended with a `step_timeline` field. Backend accumulates events into a `StepTimelineState` alongside `BuildLayerState`. Frontend diffs on each poll.

- **WebSocket path**: Events flow directly via the existing `BuildEventBus` WebSocket at `/p/{slug}/_ws`. The frontend reduces events into local state. No polling needed for step-level data.

Recommend: **WebSocket for step events, polling for layer state**. The step events are discrete and each one matters (you want to see a cache hit the instant it happens, not 2s later). The layer state (node counts, layer status) is fine with polling since individual node completions are slower than the poll rate.

The WebSocket connection already exists for the public web surface. For the Tauri desktop app, the same `BuildEventBus` is accessible via Tauri's `app_handle.emit_all()` — no new WebSocket needed for the desktop path.

---

## WebSocket Integration

### Desktop (Tauri)

Events are emitted via `app_handle.emit_all("build-event", &tagged_event)`. The frontend listens with `listen("build-event", handler)`. This is the existing pattern for Tauri events.

### Web (Public Surface)

Events flow via the existing WebSocket at `/p/{slug}/_ws` with 60ms coalescing for Progress/V2Snapshot. New step-level events are discrete and bypass coalescing (they already pass `is_discrete() == true`).

### Event Volume

Concern: a build with 112 L0 nodes, each with 3-5 LLM calls, produces 336-560 `LlmCallStarted`/`LlmCallCompleted` event pairs. At 60ms coalescing for Progress events and discrete delivery for step events, this is manageable:

- Each LLM call takes 3-10s, so events arrive at ~1-2 per second during concurrent for_each
- With concurrency 10, peak rate is ~20 events/second (start + complete for 10 concurrent calls)
- The broadcast channel (4096 capacity) handles this easily
- The WebSocket coalesce buffer only applies to Progress/V2Snapshot; step events pass through immediately but are small JSON payloads

No batching or sampling needed. Every event is delivered.

---

## Files Modified

| Phase | Files |
|---|---|
| TaggedKind extension | `event_bus.rs` — add new variants to `TaggedKind` enum |
| StepContext | Defined in the LLM output cache spec; implementation lives in `step_context.rs` or `types.rs` — this spec consumes the `bus` field |
| LLM call events | `llm.rs` — emit `LlmCallStarted`, `LlmCallCompleted`, `CacheHit`, `StepRetry`, `StepError` in `call_model_unified()` |
| Webbing events | `chain_executor.rs` — emit `WebEdgeStarted`, `WebEdgeCompleted` in `execute_webbing()` |
| Evidence events | `evidence_answering.rs` — emit `EvidenceProcessing`, `TriageDecision` |
| Gap events | Gap analysis steps — emit `GapProcessing` |
| Cluster events | `chain_executor.rs` — emit `ClusterAssignment` in `recursive_cluster` |
| Frontend timeline | `PyramidBuildViz.tsx` — add step timeline panel, cost accumulator, cache indicators |
| Frontend state | New `useStepTimeline.ts` hook — event reduction, state management |
| Frontend detail | New `StepDetailPanel.tsx` — expandable per-call trace view |
| Reroll IPC | `routes.rs` — extend `handle_reroll_node()` to accept optional `cache_key` parameter for intermediate-output reroll |
| Reroll downstream | `chain_executor.rs` or new `cache_invalidation.rs` — walk forward from a rerolled cache entry, mark dependents stale |
| Cross-pyramid view | New `CrossPyramidTimeline.tsx` — composes per-pyramid rows, subscribes to all slugs (see `cross-pyramid-observability.md`) |
| Cross-pyramid row | New `ActiveBuildRow.tsx` — compact per-slug row in the cross-pyramid timeline |
| Shared row state | New `useBuildRowState.ts` hook — factored out of `PyramidBuildViz.tsx` for reuse in cross-pyramid view |

---

## Implementation Order

1. **TaggedKind extension** — add all new variants to the enum (backend compiles, no emitters yet)
2. **StepContext** — create the struct, thread it through `call_model_unified()`
3. **LLM call events** — emit Started/Completed/CacheHit/Retry/Error in `call_model_unified()`
4. **Frontend step timeline** — basic timeline with step rows and status badges (consumes ChainStepStarted/Finished + new LLM events)
5. **Cost accumulator** — running total in the UI, fed by LlmCallCompleted events
6. **Cache hit display** — green flash, $0.00 cost, "cached" badge
7. **Full trace view** — expandable per-call detail panel
8. **Sub-step events** — WebEdge, Evidence, Gap, Cluster events (lower priority, adds richness)

Steps 1-6 deliver the core value: you can see what every step is doing, what it costs, and whether it hit cache. Steps 7-8 add depth for debugging and optimization.

---

## Node Reroll & Notes

The vision insists that rerolling any node must be a feedback-bearing action, not a slot-machine lever pull. This section owns the reroll UI and the IPC command that drives it. Storage lives in the LLM output cache spec; audit trail lives in the change manifest spec; the UI and the IPC contract live here.

### IPC Contract

```
POST pyramid_reroll_node
  Input: {
    slug: String,
    node_id: String,
    note: String,                  -- user's note (strongly encouraged, not required)
    force_fresh: bool              -- always true for reroll (bypasses cache)
  }
  Output: {
    new_cache_entry_id: i64,       -- new entry in pyramid_step_cache
    manifest_id: i64,              -- new entry in pyramid_change_manifests
    new_content: Value             -- the new node content
  }
```

### Data Flow

```
User clicks "Reroll" on a node in the build viz
  → UI opens a modal with the current node content + note textarea
  → User provides note (or leaves blank — discouraged but allowed)
  → IPC: pyramid_reroll_node(slug, node_id, note, force_fresh=true)
  → Backend:
      1. Load the original LLM call context from pyramid_step_cache by step_output linkage
      2. Construct reroll prompt: original system prompt + "The user requested a different output. Their feedback: {note}. The current output you should address: {current_content}. Produce an improved version that incorporates their concern."
      3. Call LLM with force_fresh=true (bypass cache)
      4. Store new result in pyramid_step_cache with supersedes_cache_id = original, note field populated
      5. Write change manifest to pyramid_change_manifests with note field populated (not NULL)
      6. Emit NodeRerolled event via build viz event bus
  → UI receives the event, updates the node display
```

### UI Ownership

The reroll button lives in `PyramidBuildViz.tsx` (per this spec's timeline view). Clicking a node in the timeline opens a detail panel with:
- Current output preview
- "Reroll" button
- Note textarea (labeled "Why reroll? (strongly encouraged)")
- Submit button (disabled if note is empty AND user hasn't confirmed a blank note via a second "Really reroll without feedback?" prompt)

### Where Notes Are Stored

Two places for redundancy and queryability:
1. `pyramid_change_manifests.note` — the authoritative user text
2. `pyramid_step_cache.note` (add this column if not present) — for cache-scoped lookup

This spec takes ownership of the reroll-with-notes UI. The llm-output-cache spec provides the storage layer; the change-manifest spec provides the audit trail; this spec provides the UI and IPC command.

### Anti-Slot-Machine Enforcement

The vision says "never create interfaces that encourage pulling a slot machine lever". Enforcement mechanism:
- Empty notes trigger a confirmation prompt: "Rerolling without feedback will just re-run the LLM with different randomness. Continue anyway?"
- Repeated rerolls (more than 3 in 10 minutes for the same node) show a warning: "You've rerolled this node 4 times. Providing specific feedback usually produces better results than additional attempts."

This is NOT a hard cap — users can always proceed. But the UI actively discourages mindless rerolls.

### Reroll for Intermediate Outputs

Reroll extends beyond node-creating steps to ANY LLM call that has a cache entry. This fully honors the vision's "The notes workflow applies everywhere a user might want to change LLM-generated output."

Any step whose output is stored in `pyramid_step_cache` is rerollable — not just the final content-producing steps. This includes:

- **Clustering decisions** — cluster assignment LLM calls that decide which nodes group together
- **Web edge generation** — LLM-generated edges between nodes (both the decision to create an edge and the edge's relationship type)
- **Evidence triage decisions** — the LLM call that decides whether a question is answered, deferred, or skipped
- **Evidence answering** — the synthesized answer to an evidence question
- **Gap processing** — the LLM call that identifies which gaps exist for a layer
- **Merge operations** — the LLM call that decides how to merge conflicting outputs during reconciliation

Each of these has a cache entry keyed by the same content-addressable key `hash(inputs_content_hash, prompt_hash, model_id)`. Structurally they are no different from a node-creating step — they're just LLM calls whose output lives in the cache.

#### IPC Extension

The `pyramid_reroll_node` IPC accepts an optional `cache_key` parameter as an alternative to `node_id`:

```
POST pyramid_reroll_node
  Input: {
    slug: String,
    node_id: String?,              -- for node-creating reroll (existing)
    cache_key: String?,             -- for intermediate-output reroll (new)
    note: String,
    force_fresh: bool              -- always true for reroll
  }
```

Exactly one of `node_id` or `cache_key` must be provided. If `cache_key` is provided, the backend:

1. Looks up the cache entry by `(slug, cache_key)` in `pyramid_step_cache`
2. Loads the original prompt template + inputs from the linked `pyramid_llm_audit` row
3. Constructs the reroll prompt: original system prompt + "The user requested a different output. Their feedback: {note}. The current output you should address: {cached_output}. Produce an improved version that incorporates their concern."
4. Calls LLM with `force_fresh = true` (bypass cache)
5. Stores new result in `pyramid_step_cache` with `supersedes_cache_id = original`, note field populated
6. Writes a change manifest entry with `target_type = "cache_entry"` and `target_id = cache_key`
7. Emits `NodeRerolled` event (the event name is kept for consistency even though it may apply to a non-node output) with the cache_key included

The note flows to the LLM the same way as a node reroll. The result supersedes the cache entry. Upstream steps that consumed the old output are marked for re-run if they were cache-hit — walk the evidence graph forward from the rerolled step and invalidate any cache entries whose `inputs_content_hash` depends on the rerolled output.

#### UI Surface

In the step timeline, each LLM call sub-row (see "Full Trace View" above) gains a "Reroll" button alongside the existing status/model/cost/latency fields. Clicking it opens the same reroll modal as the node reroll, preset with the cached output as context. The modal is unchanged — it's the same component; only the input payload differs (cache_key instead of node_id).

For cluster-assignment reroll specifically, the UI also surfaces a "Reroll this cluster decision" button on the cluster-assignment event row in the step timeline. This makes the intermediate-output reroll discoverable without requiring the user to navigate to the cached entry.

#### Downstream Invalidation

Rerolling an intermediate output may invalidate downstream cache entries whose inputs depended on the old output. The invalidation walk is identical to the DADBEAR dependency propagation:

```
invalidate_downstream(rerolled_cache_key):
  # Find cache entries whose inputs_content_hash references the old output
  affected = query entries where inputs_content_hash contains rerolled_cache_key's previous output
  for entry in affected:
    mark entry stale (do not delete — the old entry remains for history)
    emit CacheInvalidated event for the entry
  # Recursively walk forward to entries whose inputs depended on the now-stale entries
```

Marking stale means setting a flag that the cache lookup respects — on the next build, the stale entry is treated as a miss and recomputed. The old entry remains in the table for history and provenance, same as any superseded entry.

This fully honors the vision: every LLM-generated output is rerollable, every reroll carries a note, every reroll supersedes the prior output, and downstream consequences propagate cleanly.

---

## Cross-Pyramid Timeline

The per-pyramid `PyramidBuildViz.tsx` component defined by this spec is composed into a new cross-pyramid view component that subscribes to all slugs simultaneously. This enables:

- **Cross-pyramid build timeline** — see all concurrent builds at once, with compact rows per slug and drill-down to the detailed per-pyramid view
- **Cross-pyramid cost rollup** — aggregate cost display across all pyramids, added to the DADBEAR Oversight page
- **Cross-pyramid pause-all DADBEAR** — one click to pause DADBEAR on every pyramid (or scoped to a folder/circle)
- **Cross-pyramid reroll** — reroll a step in any pyramid from the cross-pyramid timeline, using the same reroll IPC defined above

The cross-pyramid view is a new parent component (`CrossPyramidTimeline.tsx`) that composes per-pyramid row components (`ActiveBuildRow.tsx`). The per-pyramid `PyramidBuildViz.tsx` is unchanged — it remains the detailed drill-down view, opened in a modal or sidebar panel when the user clicks a row in the cross-pyramid timeline.

See `cross-pyramid-observability.md` for the authoritative spec on:
- The `CrossPyramidEventRouter` backend fan-out from per-slug buses
- The `pyramid_active_builds`, `pyramid_cost_rollup`, `pyramid_pause_dadbear_all`, and `pyramid_resume_dadbear_all` IPC contracts
- The shared `useBuildRowState.ts` hook factored out of `PyramidBuildViz.tsx` for reuse
- The DADBEAR Oversight page integration for cost rollup display

---

## Open Questions

1. **Step name source**: The chain YAML defines `step.name` for each step. Are these guaranteed unique within a chain? If two steps share a name (unlikely but possible), the timeline lookup breaks. Recommend: enforce unique step names in chain validation. If duplicates exist, append a suffix (e.g., `extract_chunks_2`).

2. **Historical trace persistence**: Should the step timeline be queryable after a build completes (not just during)? The data exists in `pyramid_llm_audit` and `pyramid_step_cache`. Recommend: yes, but as a post-build report (query existing tables), not a persisted timeline structure. The live timeline is ephemeral (in-memory during build, same as `BuildLayerState`). A "build report" feature can reconstruct it from audit tables later.

3. **Event filtering**: Should the WebSocket support client-side event type filtering (e.g., "only send me LlmCallCompleted, not LlmCallStarted")? Recommend: no filtering in v1. All events are small. If bandwidth becomes an issue on slow connections, add server-side filtering later.

4. **Cost display precision**: `cost_usd` is an estimate based on token counts and published pricing. Actual cost (from OpenRouter Broadcast) may differ. Should the UI show a confidence indicator? Recommend: show "est" label until actual cost arrives, then show both. The DADBEAR oversight page (separate spec) already handles cost reconciliation display — reuse the same pattern.
