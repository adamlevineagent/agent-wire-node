# Live Pyramid Build Visualization

## Status
Design — not yet implemented. Audit cycle 1 complete.

## Problem
The build screen shows a progress bar with two numbers: `done` and `total`. The bar fills linearly, frequently overfills (because `total` is a pre-build estimate that never re-adjusts), and communicates nothing about the structure being built or what the system is actually doing. You stare at a bar for 12 minutes and hope.

## MPS: The build screen IS the pyramid

Each layer materializes as a row of cells. Cells light up as nodes complete. When a layer finishes, the system discovers how many nodes go above it, a new narrower row appears, and those cells start filling. You watch the pyramid grow upward until the apex lights up. A log stream below shows real-time activity.

## Scope

This design covers **chain executor builds** (`execute_chain_from` path). The following are explicitly out of scope and retain the existing flat progress bar:

- **Vine builds** — structurally different (bunch-based, not layer-based). `VineBuildProgress.tsx` already has its own bunch-level visualization.
- **Question/decomposed builds** (`run_decomposed_build`) — different phase structure (characterization, decomposition, evidence answering), not layer-by-layer.
- **IR executor builds** (`execute_plan` path) — separate progress tracking at `chain_executor.rs:6408`. Can be instrumented later if the IR path becomes primary.

## Current State (what exists)

### Backend
- `BuildProgress { done: i64, total: i64 }` — two flat numbers
- Sent via `mpsc::channel<BuildProgress>` from executor to a drain task
- Drain task writes into `Arc<tokio::sync::RwLock<BuildStatus>>`
- `total` is estimated once before any steps run via `estimate_total()`, never updated downward
- The executor already knows depth, node_id, step name, and node count at every point — none of it is surfaced

### Frontend
- `BuildProgress.tsx` polls `pyramid_build_status` every 2s (500ms when finalizing)
- Renders a progress bar: `width = (done / total) * 100%`
- Shows: percentage, "N/M estimated nodes", elapsed time, status badge
- No layer awareness, no step names, no structure

### Why `total` overfills
`total` IS re-estimated between top-level steps (`chain_executor.rs:2837`: `total = estimate_total(chain, &ctx, num_chunks).max(done)`). But the real problem: `total` is passed **by value** (`total: i64`) into `execute_recursive_pair` and `execute_recursive_cluster`. Within those recursive loops — which iterate over multiple layers in a single step call — `total` is immutable. The per-layer node count discovered during recursion never feeds back into `total`. Additionally, `.max(done)` prevents `total` from decreasing below `done` even when the real node count is lower.

Result: progress shows "127/127" when actual is "127/115" — the bar overflows.

### Pre-existing bug: recursive_cluster resume doesn't update `done`
In `execute_recursive_cluster` (~line 4229), when a layer is detected as already complete during resume, the code increments `depth` but does NOT increment `*done`. Compare with `execute_recursive_pair` (~line 4004) which correctly does `*done += existing`. This causes underreporting on resumed recursive_cluster builds. **Must be fixed as a prerequisite.**

## Design

### New Backend Types

In `src-tauri/src/pyramid/types.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProgressV2 {
    pub done: i64,
    pub total: i64,
    pub layers: Vec<LayerProgress>,
    pub current_step: Option<String>,
    pub log: Vec<LogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerProgress {
    pub depth: i64,
    pub step_name: String,
    pub estimated_nodes: i64,
    pub completed_nodes: i64,
    pub failed_nodes: i64,
    /// "pending" | "active" | "complete"
    pub status: String,
    /// Per-node detail for small layers (<=50 nodes).
    /// Large layers (L0 with 600 chunks) get None — frontend shows a density bar instead.
    /// Only contains completed/failed nodes — pending count is inferred by frontend
    /// as (estimated_nodes - completed_nodes - failed_nodes).
    pub nodes: Option<Vec<NodeStatus>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    /// "complete" | "failed"
    pub status: String,
    /// Headline from the PyramidNode, shown on hover in the UI.
    /// Extracted from the node save data (the `headline` field on PyramidNode).
    /// The NodeCompleted event fires after the node is saved, so the headline is available.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub elapsed_secs: f64,
    pub message: String,
}
```

Note: `NodeStatus.status` only has `"complete"` and `"failed"` variants. There is no `"pending"` — pending nodes are inferred by the frontend from the counts. The `nodes` vec is append-only.

### Layer State Tracking

A `BuildLayerState` lives alongside the existing `BuildStatus` in the build handle, behind its own `Arc<tokio::sync::RwLock<...>>` (using tokio's async RwLock, consistent with the existing `BuildStatus` pattern):

```rust
pub struct BuildHandle {
    pub slug: String,
    pub cancel: CancellationToken,
    pub status: Arc<tokio::sync::RwLock<BuildStatus>>,
    pub layer_state: Arc<tokio::sync::RwLock<BuildLayerState>>,  // NEW
    pub started_at: Instant,
}

pub struct BuildLayerState {
    pub layers: Vec<LayerProgress>,
    pub current_step: Option<String>,
    pub log: VecDeque<LogEntry>,  // ring buffer, last ~200 entries
}
```

### Event Emission Points

The executor already hits natural boundaries where layer events should fire. No new control flow — just surfacing what it already knows.

| Executor location | Event | Data |
|---|---|---|
| `execute_for_each` start | `LayerDiscovered` | depth, step_name, estimated_nodes from items.len() |
| Each node save in `for_each` / `work_item` | `NodeCompleted` | depth, step_name, node_id, label from output title |
| `execute_pair_adjacent` start (~line 3686) | `LayerDiscovered` | depth, step_name, estimated_nodes from source_nodes.len()/2 |
| Each pair save in `pair_adjacent` | `NodeCompleted` | depth, step_name, node_id, label |
| `recursive_pair` layer transition (~line 4144) | `LayerCompleted` + `LayerDiscovered` | completed depth + actual count, new depth + estimated count |
| `recursive_pair` resume/skip (~line 3996) | `LayerDiscovered` + `LayerCompleted` (immediate) | the already-complete layer's depth and real node count |
| `recursive_cluster` layer transition (~line 4681) | `LayerCompleted` + `LayerDiscovered` | same as recursive_pair |
| `recursive_cluster` direct-synthesis (≤4 nodes, ~line 4247) | `LayerDiscovered(depth=target, est=1)` + `NodeCompleted` + `LayerCompleted` | fast path to apex — skips cluster step |
| `recursive_cluster` resume/skip (~line 4222) | `LayerDiscovered` + `LayerCompleted` (immediate) | same |
| Step start (~line 2705) | `StepStarted` | step.name |
| Node failure (error_strategy skip) | `NodeFailed` | depth, step_name, node_id |

### Event Communication: Channel Pattern (not direct RwLock writes)

Layer events use the same `mpsc` channel pattern as the existing progress system, not direct `RwLock` writes. This avoids the `blocking_write()` problem (async executor can't use `blocking_write()` on `tokio::sync::RwLock` without risking panics) and is consistent with the existing architecture.

```rust
// New event channel alongside progress channel
let (layer_tx, mut layer_rx) = mpsc::channel::<LayerEvent>(128);

// Drain task updates BuildLayerState (same pattern as progress drain)
let layer_handle = tokio::spawn(async move {
    while let Some(event) = layer_rx.recv().await {
        let mut state = layer_state.write().await;
        match event {
            LayerEvent::Discovered { depth, step_name, estimated_nodes } => {
                state.layers.push(LayerProgress {
                    depth,
                    step_name,
                    estimated_nodes,
                    completed_nodes: 0,
                    failed_nodes: 0,
                    status: "pending".into(),
                    nodes: if estimated_nodes <= 50 {
                        Some(Vec::new())
                    } else {
                        None
                    },
                });
            }
            LayerEvent::NodeCompleted { depth, step_name, node_id, label } => {
                // Compound lookup by depth + step_name to handle multiple steps at same depth
                if let Some(layer) = state.layers.iter_mut()
                    .find(|l| l.depth == depth && l.step_name == step_name)
                {
                    layer.completed_nodes += 1;
                    layer.status = "active".into();
                    if let Some(ref mut nodes) = layer.nodes {
                        nodes.push(NodeStatus {
                            node_id,
                            status: "complete".into(),
                            label,
                        });
                    }
                }
            }
            LayerEvent::LayerCompleted { depth, step_name } => {
                if let Some(layer) = state.layers.iter_mut()
                    .find(|l| l.depth == depth && l.step_name == step_name)
                {
                    layer.status = "complete".into();
                }
            }
            LayerEvent::NodeFailed { depth, step_name, node_id } => {
                if let Some(layer) = state.layers.iter_mut()
                    .find(|l| l.depth == depth && l.step_name == step_name)
                {
                    layer.failed_nodes += 1;
                    if let Some(ref mut nodes) = layer.nodes {
                        nodes.push(NodeStatus {
                            node_id,
                            status: "failed".into(),
                            label: None,
                        });
                    }
                }
            }
            LayerEvent::StepStarted { step_name } => {
                state.current_step = Some(step_name);
            }
            LayerEvent::Log { message, elapsed_secs } => {
                state.log.push_back(LogEntry { elapsed_secs, message });
                if state.log.len() > 200 { state.log.pop_front(); }
            }
        }
    }
});
```

The `layer_tx` sender is passed through the executor alongside `progress_tx`. The executor sends events via `layer_tx.send(...).await` at each boundary.

### Re-estimation (kills the overfill bug)

**Key constraint:** `total` is currently passed by value (`total: i64`) into `execute_recursive_pair`, `execute_recursive_cluster`, `execute_for_each`, `execute_pair_adjacent`, and `execute_single`. None of these functions can mutate the caller's `total`.

**Fix:** Change the signature of `execute_recursive_pair` and `execute_recursive_cluster` to accept `total: &mut i64`. These are the only functions where mid-step re-estimation matters (they loop over multiple layers within a single step call). `execute_for_each` and `execute_pair_adjacent` don't need it — their item count is known upfront.

When a layer completes inside the recursive loop, recalculate:

```rust
// In recursive_pair layer loop body, after all pairs at target_depth are built:
let actual_at_this_depth = count_nodes_at_depth(conn, slug, target_depth);
let remaining = estimate_recursive_pair_nodes(actual_at_this_depth);
*total = *done + remaining;
send_progress(progress_tx, *done, *total).await;
```

Same pattern in `recursive_cluster`:
```rust
let actual_at_this_depth = count_nodes_at_depth(conn, slug, target_depth);
let remaining = estimate_recursive_cluster_nodes(actual_at_this_depth);
*total = *done + remaining;
send_progress(progress_tx, *done, *total).await;
```

This means `total` adjusts at every layer boundary as real data replaces estimates. The progress bar and pyramid viz both benefit.

### New Tauri Command

```rust
#[tauri::command]
async fn pyramid_build_progress_v2(
    state: tauri::State<'_, SharedState>,
    slug: String,
) -> Result<BuildProgressV2, String> {
    let active = state.pyramid.active_build.read().await;
    if let Some(handle) = active.get(&slug) {
        let status = handle.status.read().await;
        let layer_state = handle.layer_state.read().await;
        Ok(BuildProgressV2 {
            done: status.progress.done,
            total: status.progress.total,
            layers: layer_state.layers.clone(),
            current_step: layer_state.current_step.clone(),
            log: layer_state.log.iter().cloned().collect(),
        })
    } else {
        Ok(BuildProgressV2 {
            done: 0, total: 0,
            layers: vec![],
            current_step: None,
            log: vec![],
        })
    }
}
```

The existing `pyramid_build_status` command stays unchanged for backward compatibility.

Note: The pyramid visualization is only available during and immediately after a build, not after app restart. `BuildLayerState` is in-memory only, same as the current progress. This matches existing behavior.

### Frontend: PyramidBuildViz Component

Replaces `BuildProgress.tsx` when `progress_v2` is available.

#### Layout

```
┌─────────────────────────────────────────────┐
│  Building: core-selected-docs               │
│                                             │
│                 ┌───┐                       │
│             L3  │ ◆ │  apex                │
│                 └───┘                       │
│              ┌──┬──┬──┐                     │
│          L2  │■ │■ │□ │  2/3               │
│              └──┴──┴──┘                     │
│         ┌──┬──┬──┬──┬──┬──┬──┐             │
│     L1  │■ │■ │■ │■ │■ │▪ │░ │  5/7       │
│         └──┴──┴──┴──┴──┴──┴──┘             │
│  ┌─────────────────────────────────────┐    │
│  │████████████████████░░░░░            │    │
│  └─────────────────────────────────────┘    │
│  L0  87/112 nodes                           │
│                                             │
│  12m 34s elapsed  ·  94/122 nodes           │
│                                             │
│  ┌─ Activity ─────────────────────────────┐ │
│  │ 12:34  L0 extract: 87/112 complete     │ │
│  │ 12:35  L1 thread clustering: 7 threads │ │
│  │ 12:36  L1 synthesis: 5/7 done          │ │
│  │ 12:37  L2 discovered: 3 nodes          │ │
│  └────────────────────────────────────────┘ │
│                                             │
│  [Cancel Build]                             │
└─────────────────────────────────────────────┘
```

#### Rendering Rules

| Layer size | Rendering |
|---|---|
| >50 nodes (L0 typically) | Density bar — filled portion proportional to `completed/estimated`. No individual cells. |
| 4-50 nodes (L1, L2) | Grid of individual cells. Pending count = `estimated - completed - failed`. Filled = complete, red = failed. Hover shows `label` (node title). |
| 1 node (apex) | Diamond shape, lights up on completion. |
| Not yet discovered | Not rendered — layer appears only when `LayerDiscovered` fires. |

Note: Node completions within a layer may arrive out of order due to concurrent `for_each` execution. The snapshot polling model handles this naturally — the frontend always renders the latest state, not an event stream.

#### Animation

- New layers slide in from below (or fade in above the current top)
- Cells transition from empty → filled with a brief glow
- Completed layers dim slightly so visual focus stays on the active layer
- The density bar for L0 fills smoothly between polls (CSS transition on width)

#### Polling

Keep the existing 2s poll interval. Return the full `BuildProgressV2` snapshot. The frontend diffs against previous state and animates deltas.

2s polling is appropriate because:
- Each LLM call takes 3-10s, so individual node completions are slower than the poll rate
- Upper layers with few nodes: each completion is visible on the next poll
- L0 with many nodes: the density bar advances in visible chunks every poll

No WebSocket/SSE needed. If we later want sub-second smoothness for upper-layer nodes, Tauri push events (`app_handle.emit_all`) are available without architectural changes.

### Files to Modify

#### Rust (backend)
| File | Change |
|---|---|
| `src-tauri/src/pyramid/types.rs` | Add `BuildProgressV2`, `LayerProgress`, `NodeStatus`, `LogEntry`, `BuildLayerState`, `LayerEvent` types |
| `src-tauri/src/pyramid/mod.rs` | Add `layer_state: Arc<tokio::sync::RwLock<BuildLayerState>>` to `BuildHandle` |
| `src-tauri/src/pyramid/chain_executor.rs` | (1) Change `execute_recursive_pair` and `execute_recursive_cluster` signatures to take `total: &mut i64`. (2) Emit layer events at natural boundaries via `layer_tx`. (3) Re-estimate `total` on layer complete. (4) Fix pre-existing bug: add `*done += existing` to recursive_cluster resume path. (5) Emit `LayerDiscovered`+`LayerCompleted` for resumed/skipped layers. (6) Handle recursive_cluster direct-synthesis fast path (≤4 nodes). (7) For concurrent `for_each`: clone `layer_tx` into each spawned task (same pattern as `writer_tx.clone()`), fire `NodeCompleted` inside `execute_for_each_work_item` after node save. |
| `src-tauri/src/pyramid/build_runner.rs` | Pass `layer_tx` through `run_chain_build` / `run_build_from` to `execute_chain_from`. |
| `src-tauri/src/main.rs` | Add `pyramid_build_progress_v2` command. Create `layer_tx`/`layer_rx` channel and drain task. Initialize `layer_state` in `BuildHandle` at ALL construction sites: `pyramid_build` (~line 3489) and `pyramid_question_build` (~line 4200). For question builds, `layer_state` can be initialized with an empty default since the viz only covers chain executor builds. |
| `src-tauri/src/pyramid/routes.rs` | Add `layer_state` initialization to BuildHandle construction at ALL sites: HTTP build endpoint (~line 2335) and HTTP question build endpoint (~line 4409). Create `layer_tx`/`layer_rx` channel and drain task for the standard build path. |

#### Frontend (React)
| File | Change |
|---|---|
| `src/components/PyramidBuildViz.tsx` | **New.** Pyramid visualization component with layer rows, cells, density bars, log panel. |
| `src/components/BuildProgress.tsx` | Swap to use `PyramidBuildViz` when v2 endpoint available. Keep as fallback. |

### What This Does NOT Need
- WebSocket/SSE — polling at 2s is sufficient given LLM call latency
- Database schema changes — all layer state is in-memory during build, same as current progress
- Chain YAML changes — purely observability, no chain format impact
- New dependencies — standard Tauri + React, existing tech stack
- Vine/question/IR build coverage — out of scope (see Scope section above)

### Implementation Sequence
1. Fix pre-existing bug: `recursive_cluster` resume not updating `done`
2. Rust types + `BuildLayerState` + `LayerEvent` types
3. Change `execute_recursive_pair`/`execute_recursive_cluster` signatures to `total: &mut i64`
4. Wire layer event channel into executor; emit at all boundaries (including pair_adjacent + resume paths)
5. Re-estimation on layer complete inside recursive loops
6. `pyramid_build_progress_v2` Tauri command + drain task
7. `PyramidBuildViz` React component (layers + cells + density bar)
8. Log panel (ring buffer backend + scrolling frontend)
9. Swap `BuildProgress.tsx` to use new component

## Audit Log

### Cycle 1 — Stage 1 (Informed Pair)
Findings applied:
- **CRITICAL (fixed):** `total` passed by value into recursive functions — re-estimation couldn't work. Fixed: design now specifies `total: &mut i64` for recursive_pair/recursive_cluster.
- **MAJOR (fixed):** `pair_adjacent` steps omitted from event emission. Fixed: added to emission table.
- **MAJOR (fixed):** Vine builds unaddressed. Fixed: added explicit Scope section marking vine/question/IR as out of scope.
- **MAJOR (fixed):** `blocking_write()` incompatible with async context. Fixed: switched to mpsc channel drain pattern (consistent with existing progress architecture).
- **MAJOR (fixed):** Resume paths skip LayerDiscovered events. Fixed: resume/skip paths now emit LayerDiscovered + immediate LayerCompleted.
- **MAJOR (fixed):** `recursive_cluster` resume doesn't update `done` (pre-existing bug). Fixed: noted as prerequisite fix in implementation sequence.
- **MINOR (fixed):** Line number references corrected (~4144, ~4681, ~2705).
- **MINOR (fixed):** "pending" NodeStatus variant removed — frontend infers pending from counts.
- **MINOR (fixed):** Depth collision in LayerProgress lookup — compound lookup by `(depth, step_name)`.
- **MINOR (noted):** Concurrent for_each out-of-order completions — noted in rendering rules.
- **MINOR (fixed):** Ambiguous RwLock type — explicitly `tokio::sync::RwLock` throughout.
- **MINOR (noted):** No persistence across restart — noted in Tauri command section.
- **MINOR (fixed):** Estimation formula description corrected to `ceil(n/2)` / `ceil(n/5) + 1 apex`.

### Cycle 1 — Stage 1 (Auditor B)
Cross-agreement with Auditor A (6 findings independently confirmed):
- blocking_write, total by value, pair_adjacent, vine builds, line refs, depth collision, resume paths

New findings applied:
- **MAJOR (fixed):** "never updated" claim about total is inaccurate — total IS re-estimated between top-level steps, but not within recursive fns. Problem statement rewritten.
- **MAJOR (fixed):** `routes.rs` also constructs BuildHandle — needs `layer_state`. Added to files-to-modify.
- **MINOR (fixed):** `recursive_cluster` direct-synthesis fast path (≤4 nodes) needs its own events. Added to emission table.
- **MINOR (fixed):** `label` source should be `headline` field from `PyramidNode`. Clarified in type definition.
- **MINOR (noted):** Snapshot consistency between `status` and `layer_state` reads — benign, next poll corrects.
- **MINOR (noted):** `from_depth` skip hydration — already covered by resume path handling.

### Cycle 1 — Stage 2 (Discovery Pair)
Two blind auditors with only a purpose statement and known issues list.

Cross-agreement (3 findings independently confirmed by both):
- Vine builds missing `with_build_reader()` — already fixed in code
- Question build HTTP route missing `with_build_reader()` — already fixed in code
- `busy_timeout` inconsistency — already fixed in code

New findings applied:
- **MAJOR (fixed in code):** `pyramid_question_build` Tauri command uses `state.pyramid.clone()`, sharing global reader. Fixed: now calls `with_build_reader()`.
- **MAJOR (fixed in code):** TOCTOU race in `pyramid_build` — read lock then write lock allows double-start. Fixed: atomic check-and-set with single write lock.
- **MAJOR (fixed in doc):** `build_runner.rs` missing from files-to-modify. Added.
- **MAJOR (fixed in doc):** Concurrent `for_each` event emission — `layer_tx` must be cloned into spawned tasks. Specified in files-to-modify.
- **MINOR (fixed in doc):** Four BuildHandle construction sites, not two. All four enumerated.
- **MINOR (fixed in code):** Frontend progress bar doesn't clamp to 100%. Fixed with `Math.min`.
- **MINOR (noted in code):** `AtomicBool` snapshot copies in `with_build_reader()`. Comment added.
- **MINOR (noted):** Design doc LayerProgress.status could be a Rust enum — accepted as matching existing pattern for now.
- **MINOR (noted):** `layer_tx` buffer (128) should use `try_send` to avoid backpressure blocking executor — deferred to implementation.
- **MINOR (noted):** Re-estimation should call `flush_writes` before `count_nodes_at_depth` — deferred to implementation.
- **MINOR (noted):** Non-atomic read of status + layer_state (benign, next poll corrects).
- **MINOR (noted):** Unused external writer drain in main.rs for chain engine path — pre-existing, out of scope.
