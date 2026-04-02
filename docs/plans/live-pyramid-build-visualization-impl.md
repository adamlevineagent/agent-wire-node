# Live Pyramid Build Visualization — Implementation Plan

## Context
The build screen currently shows a flat progress bar (done/total) that overfills and communicates nothing about structure. The design doc (`docs/live-pyramid-build-visualization.md`) — which passed a full two-stage audit — specifies replacing it with a live pyramid visualization where layers appear and fill in real-time. This plan implements that design.

## Phase 1: Rust Types + LayerEvent Channel

### 1a. New types in `src-tauri/src/pyramid/types.rs`
Add after existing `BuildProgress` / `BuildStatus`:
- `BuildProgressV2` — snapshot returned by the v2 polling command
- `LayerProgress` — per-layer state (depth, step_name, estimated/completed/failed, nodes)
- `NodeStatus` — per-node detail for small layers (node_id, status, label/headline)
- `LogEntry` — timestamped log line
- `BuildLayerState` — the shared state (layers vec + current_step + log ring buffer)
- `LayerEvent` enum — Discovered, NodeCompleted, NodeFailed, LayerCompleted, StepStarted, Log

### 1b. Add `layer_state` to `BuildHandle` in `src-tauri/src/pyramid/mod.rs`
Add field: `pub layer_state: Arc<tokio::sync::RwLock<BuildLayerState>>`

### 1c. Update all 4 BuildHandle construction sites
Each needs `layer_state: Arc::new(tokio::sync::RwLock::new(BuildLayerState::default()))`:
- `src-tauri/src/main.rs` ~line 3489 (pyramid_build)
- `src-tauri/src/main.rs` ~line 4200 (pyramid_question_build)
- `src-tauri/src/pyramid/routes.rs` ~line 2335 (handle_build)
- `src-tauri/src/pyramid/routes.rs` ~line 4409 (handle_question_build)

## Phase 2: Layer Event Channel + Drain Task

### 2a. Create channel in `pyramid_build` (main.rs)
After the existing `progress_tx/progress_rx` channel creation (~line 3572):
- Create `let (layer_tx, layer_rx) = mpsc::channel::<LayerEvent>(256);`
- Spawn drain task that reads `layer_rx` and updates `layer_state` (same pattern as progress drain)
- Use `try_send` instead of `send().await` to prevent backpressure from blocking the executor
- Ring buffer the log entries (cap at 200)

### 2b. Same channel setup in routes.rs handle_build (~line 2406)

### 2c. For question build sites (main.rs pyramid_question_build, routes.rs handle_question_build)
Initialize `layer_state` with default empty state but DON'T create layer channel — question builds don't go through the chain executor.

## Phase 3: Thread `layer_tx` Through the Executor

### 3a. Change `execute_chain_from` signature
`src-tauri/src/pyramid/chain_executor.rs` line 2576:
Add parameter: `layer_tx: Option<mpsc::Sender<LayerEvent>>`

### 3b. Update `build_runner.rs` call site
`src-tauri/src/pyramid/build_runner.rs` line 494:
Pass `layer_tx` through `run_chain_build` → `run_build_from` → `execute_chain_from`.
Add `layer_tx: Option<mpsc::Sender<LayerEvent>>` to `run_build`, `run_build_from`, and `run_chain_build` signatures.

### 3c. Change all step executor signatures
Add `layer_tx: &Option<mpsc::Sender<LayerEvent>>` to:
- `execute_for_each` (line 2923)
- `execute_for_each_concurrent` (line 3260)
- `execute_for_each_work_item` (line 3474) — gets a clone
- `execute_pair_adjacent` (line 3667)
- `execute_recursive_pair` (line 3950)
- `execute_recursive_cluster` (line 4154)
- `execute_single` (line 4966)

### 3d. Change `total` to `&mut i64` in recursive functions
- `execute_recursive_pair`: `total: i64` → `total: &mut i64`
- `execute_recursive_cluster`: `total: i64` → `total: &mut i64`
- Update all `send_progress` calls inside these to use `*total`
- Update call sites in the main dispatch loop (~lines 2729-2770) to pass `&mut total`

### 3e. Helper function for sending layer events
```rust
fn try_send_layer_event(layer_tx: &Option<mpsc::Sender<LayerEvent>>, event: LayerEvent) {
    if let Some(ref tx) = layer_tx {
        let _ = tx.try_send(event); // non-blocking, drops if full
    }
}
```

## Phase 4: Emit Events at Natural Boundaries

### 4a. Step start (main dispatch loop, ~line 2705)
Emit `LayerEvent::StepStarted { step_name }` before each step dispatches.

### 4b. `execute_for_each` (line 2956 area, after items resolved)
Emit `LayerEvent::Discovered { depth, step_name, estimated_nodes: items.len() }` when `saves_node` is true.
On each node completion (sequential: ~line 3099, concurrent: collector loop ~line 3436):
Emit `NodeCompleted { depth, step_name, node_id, label }` where label comes from the output's headline field.
For concurrent path: clone `layer_tx` into each spawned task, emit inside `execute_for_each_work_item`.

### 4c. `execute_pair_adjacent` (~line 3686)
Emit `Discovered` at start with `estimated_nodes = (source_nodes.len() + 1) / 2`.
Emit `NodeCompleted` after each pair is synthesized (~line 3838).

### 4d. `execute_recursive_pair` layer loop (~line 3970-4144)
At top of each iteration: emit `Discovered { depth: target_depth, estimated_nodes }`.
On resume/skip (~line 3996): emit `Discovered` + immediate `LayerCompleted`.
After each pair completes (~line 4047): emit `NodeCompleted`.
At end of layer (~line 4144 before `depth = target_depth`): emit `LayerCompleted`.
**Re-estimation:** After layer completes, `*total = *done + estimate_recursive_pair_nodes(actual_count)`.

### 4e. `execute_recursive_cluster` layer loop (~line 4181-4681)
Same pattern as recursive_pair.
Direct-synthesis fast path (≤4 nodes, ~line 4247): emit `Discovered(est=1)` + `NodeCompleted` + `LayerCompleted`.
Resume/skip (~line 4222): emit `Discovered` + `LayerCompleted`.
**Fix pre-existing bug:** Add `*done += existing` to resume path (~line 4231).
**Re-estimation:** Same pattern as recursive_pair using `estimate_recursive_cluster_nodes`.

### 4f. Node failures
Wherever `failures += 1` after a skip strategy, emit `NodeFailed { depth, step_name, node_id }`.

### 4g. Resume paths in for_each
When `ResumeState::Complete` is hit (~line 3025), emit `NodeCompleted` with label from `load_prior_step_output`.

## Phase 5: New Tauri Command

### 5a. Add `pyramid_build_progress_v2` command in `main.rs`
Register in the `.invoke_handler(tauri::generate_handler![...])` list.
Returns `BuildProgressV2` by reading both `handle.status` and `handle.layer_state`.

## Phase 6: Frontend — PyramidBuildViz Component

### 6a. New file: `src/components/PyramidBuildViz.tsx`
- Polls `pyramid_build_progress_v2` every 2s (500ms when finalizing)
- Renders layers bottom-up as rows
- >50 nodes: density bar (CSS width transition)
- 4-50 nodes: individual cells grid, hover shows label
- 1 node (apex): diamond shape
- Completed layers dim slightly
- Log panel at bottom with auto-scroll

### 6b. Update `src/components/BuildProgress.tsx`
- Try v2 endpoint first; fall back to v1 if not available
- Or: replace entirely with PyramidBuildViz since we're shipping both together

## Phase 7: Verify
- `cargo check` — compilation
- Build a pyramid on a multi-doc corpus and watch the viz
- Verify existing pyramids remain queryable during the build (lockup fix)
- Check resumed builds show pre-completed layers correctly

## Execution Strategy
This is a large change touching ~10 files. Split into parallel workstreams:
- **Workstream A (Rust):** Phases 1-5 — types, channel, threading, events, Tauri command
- **Workstream B (Frontend):** Phase 6 — PyramidBuildViz component

Workstream A first (frontend needs the backend types to poll against), then B.
Within Workstream A, the order is strict: types → BuildHandle → channel → signatures → events → command.
