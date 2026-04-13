# Pyramid Surface — Sprint 2: Close All Gaps

**Date:** 2026-04-13
**Context:** Sprint 1 shipped the component architecture. Sprint 2 connects everything end-to-end.
**Gap analysis:** `pyramid-surface-gap-analysis.md`

---

## Phase Order

Dependency-driven sequence. GAP-4 (density layout) is last — everything else ships first.

```
S2-1 (Viz-from-YAML wiring)     GAP-1 + GAP-8
    ↓
S2-2 (Edge events + link intensity)  GAP-2 + GAP-3
    ↓
S2-3 (Inspector on DADBEAR + cleanup)  GAP-6 + GAP-7
    ↓
S2-4 (Reconciliation persistence)  GAP-5
    ↓
S2-5 (Chronicle post-build)  GAP-9
    ↓
S2-6 (Relationship density layout)  GAP-4
```

S2-1 through S2-3 are wiring — connecting existing code. S2-4 and S2-5 are small backend + frontend additions. S2-6 is the algorithm work.

---

## S2-1: Viz-from-YAML Wiring (GAP-1 + GAP-8)

**Goal:** The renderer changes behavior based on which build step is running. The viz-from-YAML architecture actually works.

**What already exists:**
- `useVizMapping.ts` — loads chain, maps step→viz primitive
- `pyramid_get_build_chain` IPC — returns chain definition
- `useChronicleStream.ts` — already receives `chain_step_started` events with `step_name`
- `useBuildRowState.ts` — tracks `currentStep`

**What to build:**

1. **PyramidSurface: call useVizMapping**
   - Import and call `useVizMapping(slug, isBuilding)`
   - Track `activeVizPrimitive` state, updated when `chain_step_started` events arrive
   - Pass `activeVizPrimitive` to the renderer on each frame

2. **PyramidRenderer interface: add viz primitive awareness**
   - New method: `setActiveVizPrimitive(primitive: VizPrimitive | null)`
   - Renderer uses this to switch draw modes during build steps

3. **CanvasRenderer: implement viz-primitive-specific rendering**
   - `node_fill` (default): dots appearing in layer bands — already works
   - `edge_draw`: when active, draw edges from `WebEdgeCompleted` data between existing nodes. Accumulate edge positions from build events and render as animated lines.
   - `verdict_mark`: when active, overlay KEEP (green ring), DISCONNECT (orange ring), MISSING (yellow pulse) indicators on source nodes based on `VerdictProduced` events
   - `cluster_form`: when active, highlight cluster member nodes with a shared tint, then show parent node appearing above
   - `progress_only`: show step name text label in canvas center — already works via build status bar

4. **GpuRenderer: same viz primitive modes** (mirror CanvasRenderer changes)

5. **Build event → visual state mapping**
   - `VerdictProduced` events → store per-node verdict state (KEEP/DISCONNECT/MISSING) in a Map
   - `ClusterAssignment` events → store cluster membership per node
   - `WebEdgeCompleted` events → store newly created edge positions
   - These maps feed into the renderer's per-frame draw based on active viz primitive

**Files:**
- `src/components/pyramid-surface/PyramidSurface.tsx` — import useVizMapping, track activeVizPrimitive, pass to renderer
- `src/components/pyramid-surface/PyramidRenderer.ts` — add setActiveVizPrimitive
- `src/components/pyramid-surface/CanvasRenderer.ts` — implement per-primitive draw modes
- `src/components/pyramid-surface/GpuRenderer.ts` — mirror per-primitive draw modes
- `src/components/pyramid-surface/useBuildVizState.ts` — NEW: accumulates verdict/cluster/edge events into renderable state

**Acceptance:**
- During source_extract: nodes appear in L0 band (node_fill) ✓
- During l0_webbing: edges animate between L0 nodes (edge_draw)
- During evidence_loop: KEEP/DISCONNECT indicators appear on cited nodes (verdict_mark)
- During thread_clustering: nodes highlight with shared cluster tint (cluster_form)
- During gap_processing: status text only (progress_only)
- Switching between steps visually changes what the renderer draws
- A Wire market chain with a custom viz section would render correctly

---

## S2-2: Edge Events + Link Intensity (GAP-2 + GAP-3)

**Goal:** Evidence links show "rivers of importance." EdgeCreated events fire during webbing.

**What to build:**

1. **Emit EdgeCreated from Rust**
   - In `chain_executor.rs` `execute_web_step`, after edges are persisted, emit `EdgeCreated` per edge
   - If the step's chain YAML has `viz.edge_batch_size`, batch accordingly
   - If absent, emit all edges (the frontend accumulates per-frame anyway)

2. **PyramidRenderer interface: add link intensities**
   - New method: `setLinkIntensities(intensities: Map<string, number>)`
   - The key format matches useVisualEncoding: `"source_id→target_id"`

3. **CanvasRenderer/GpuRenderer: render link intensities**
   - In `drawEdges`, look up each edge's intensity
   - Modulate stroke alpha and width: high intensity = thick bright, low = thin faint
   - Only when `overlays.weightIntensity` is true

4. **PyramidSurface: pass link intensities to renderer**
   - Read `linkIntensities` from `useVisualEncoding` (already returned, just not consumed)
   - Call `renderer.setLinkIntensities(linkIntensities)` in a useEffect

**Files:**
- `src-tauri/src/pyramid/chain_executor.rs` — emit EdgeCreated in execute_web_step
- `src/components/pyramid-surface/PyramidRenderer.ts` — add setLinkIntensities
- `src/components/pyramid-surface/CanvasRenderer.ts` — use link intensities in drawEdges
- `src/components/pyramid-surface/GpuRenderer.ts` — same
- `src/components/pyramid-surface/PyramidSurface.tsx` — pass linkIntensities to renderer

**Acceptance:**
- Evidence links from apex to L0 show visual gradient: thick/bright near apex, thin/faint at periphery
- Same raw link weight renders differently based on upstream importance
- Toggling Weight overlay on/off switches between flat edges and importance-weighted edges
- EdgeCreated events appear in the chronicle during webbing

---

## S2-3: Inspector on DADBEAR + Cleanup (GAP-6 + GAP-7)

**Goal:** Click any node on DADBEAR to open the inspector. Clean up dead code.

**What to build:**

1. **DADBEARPanel: add inspector**
   - Add `inspectedNodeId` state
   - Pass `onNodeClick` to PyramidSurface
   - Render `NodeInspectorPanel` when inspectedNodeId is set
   - Need `allNodes` for inspector navigation — fetch via `pyramid_build_live_nodes` or `pyramid_tree` flattened

2. **Replace inline tier detection with useRenderTier**
   - Import `useRenderTier` in PyramidSurface
   - Use its output instead of the inline WebGL2 probe
   - Delete the inline detection code

**Files:**
- `src/components/DADBEARPanel.tsx` — add inspector state + NodeInspectorPanel
- `src/components/pyramid-surface/PyramidSurface.tsx` — use useRenderTier hook

**Acceptance:**
- Click a node on DADBEAR → inspector opens with full five-section panel
- Arrow key navigation works in DADBEAR inspector
- Render tier detection uses the hook, not inline code

---

## S2-4: Reconciliation Persistence (GAP-5)

**Goal:** Reconciliation summaries persist as contributions, queryable post-build.

**What to build:**

1. **Register reconciliation_result schema type**
   - Add dispatcher branch in config_contributions.rs (no-op, same as pyramid_viz_config)
   - Schema definition + annotation contributions (embedded in Rust, same pattern as Phase 2a)
   - Display name + description in schema_registry.rs

2. **Persist after reconcile_layer**
   - In chain_executor.rs, after `reconcile_layer` returns, create a `reconciliation_result` contribution
   - Include: slug, build_id, orphan_ids, central_node_ids, weight_map as YAML
   - Use `create_config_contribution` with `source: "build"` and build_id in triggering_note

**Files:**
- `src-tauri/src/pyramid/config_contributions.rs` — dispatcher branch
- `src-tauri/src/pyramid/schema_registry.rs` — display name + description
- `src-tauri/src/pyramid/chain_executor.rs` — persist reconciliation result as contribution

**Acceptance:**
- After a build with evidence, `reconciliation_result` contribution exists in the store
- Visible in Tools tab
- Contains orphan IDs, central node IDs, weight map

---

## S2-5: Chronicle Post-Build Review (GAP-9)

**Goal:** After a build completes, the chronicle shows what happened (loaded from persisted data).

**What to build:**

1. **New IPC: `pyramid_get_build_chronicle`**
   - Takes slug + build_id
   - Queries: `pyramid_llm_audit` (LLM calls), `pyramid_evidence` (verdicts), `pyramid_gaps` (gap reports), `pyramid_deferred_questions` (triage deferrals), `reconciliation_result` contributions (from S2-4)
   - Returns a chronologically sorted array of operation records

2. **useChronicleStream: load historical data**
   - When no build is active and a slug has a `last_build_id`, call `pyramid_get_build_chronicle`
   - Convert historical records to `ChronicleEntry` format (same as live events)
   - Show in Chronicle panel instead of "Awaiting events..."

**Files:**
- `src-tauri/src/main.rs` — new IPC
- `src-tauri/src/pyramid/query.rs` — query function joining audit tables
- `src/components/pyramid-surface/useChronicleStream.ts` — load historical data on mount

**Acceptance:**
- Open a completed pyramid → Chronicle shows all operations from the last build
- Decision entries (verdicts, triage, reconciliation) are expandable
- Mechanical entries (LLM calls, cache hits) shown when show_mechanical_ops is true

---

## S2-6: Relationship Density Layout (GAP-4)

**Goal:** The "Density" toggle shows a force-directed layout where proximity encodes relationship strength.

**What to build:**

1. **New layout hook: `useDensityLayout`**
   - Force simulation: nodes repel, web edges attract proportional to strength
   - Node mass proportional to aggregate weight (central nodes are gravitational anchors)
   - Simulation runs for N iterations to settle, then becomes static in Standard tier
   - In Rich tier: live simulation via requestAnimationFrame

2. **Wire layoutMode in PyramidSurface**
   - When `layoutMode === 'density'`, use `useDensityLayout` instead of `useUnifiedLayout`
   - Pass the resulting nodes/edges to the renderer
   - Node sizing driven by weight maps (central = larger)

3. **Labels appear based on size**
   - Nodes above a size threshold show headline labels
   - Smaller nodes are unlabeled until hover

**Files:**
- `src/components/pyramid-surface/useDensityLayout.ts` — NEW: force simulation
- `src/components/pyramid-surface/PyramidSurface.tsx` — wire layoutMode to layout selection

**Acceptance:**
- Click "Density" toggle → nodes reposition based on relationship strength
- Strongly related nodes cluster together
- Central nodes are larger and act as visual anchors
- Click "Pyramid" → returns to trapezoid band layout
- Labels auto-appear on central nodes

---

## Dependency Graph

```
S2-1 (Viz-from-YAML)
    ↓
S2-2 (Edge events + link intensity)
    ↓
S2-3 (Inspector DADBEAR + cleanup)
    ↓
S2-4 (Reconciliation persistence)
    ↓
S2-5 (Chronicle post-build)
    ↓
S2-6 (Density layout)
```

S2-1 is the critical path — it's the core innovation. S2-2 builds on the viz primitive infrastructure. S2-3-5 are independent but sequenced for clean commits. S2-6 is last per Adam's direction.

---

## Method

Same as Sprint 1: implement → serial verifier → wanderer → commit. But with one change: **the wanderer's brief must include "does this feature work end-to-end as described in the plan?" not just "does this code compile and make internal sense?"** The Sprint 1 gap happened because wanderers verified code quality but not plan delivery.
