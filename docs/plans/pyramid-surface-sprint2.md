# Pyramid Surface — Sprint 2: Close All Gaps

**Date:** 2026-04-13
**Context:** Sprint 1 shipped the component architecture. Sprint 2 connects everything end-to-end.
**Gap analysis:** `pyramid-surface-gap-analysis.md`
**Audit:** Cycle 1 applied 2026-04-13. 3 critical, 5 major corrected inline.

---

## Phase Order

Dependency-driven sequence. GAP-4 (density layout) is last — everything else ships first.

```
S2-1 (Viz-from-YAML wiring)     GAP-1 + GAP-8
    ↓
S2-2 (Edge events + link intensity)  GAP-2 + GAP-3
    ↓
S2-3 through S2-5 are independent (sequenced for clean commits):
  S2-3 (Inspector on DADBEAR + cleanup)  GAP-6 + GAP-7
  S2-4 (Reconciliation persistence)  GAP-5
  S2-5 (Chronicle post-build)  GAP-9
    ↓
S2-6 (Relationship density layout)  GAP-4
```

Hard dependencies: S2-2 depends on S2-1 (viz primitives). S2-5 depends on S2-4 (reconciliation contributions needed for chronicle). S2-6 depends on everything else. S2-3 is independent of S2-4/S2-5.

---

## S2-1: Viz-from-YAML Wiring (GAP-1 + GAP-8)

**Goal:** The renderer changes behavior based on which build step is running. The viz-from-YAML architecture actually works.

**What already exists:**
- `useVizMapping.ts` — loads chain, maps step→viz primitive. Already imported in PyramidSurface (line 7) but never called.
- `pyramid_get_build_chain` IPC — returns chain definition
- `useChronicleStream.ts` — already receives `chain_step_started` events with `step_name`
- `useBuildRowState.ts` — tracks `currentStep`, handles `verdict_produced`, `cluster_assignment`, `edge_created` events

**What to build:**

1. **PyramidSurface: call useVizMapping and track active viz primitive**
   - Call `useVizMapping(slug, isBuilding)` (import already exists)
   - Subscribe to `cross-build-event` for `chain_step_started` events (or derive from `usePyramidData.currentStep`)
   - Look up current step's viz primitive via `vizMapping.getVizPrimitive(currentStep)`
   - Track `activeVizPrimitive` state, pass to renderer

2. **PyramidRenderer interface: add viz primitive awareness**
   - New method: `setActiveVizPrimitive(primitive: VizPrimitive | null)`
   - All three renderers (CanvasRenderer, GpuRenderer, DomRenderer) implement it. DomRenderer can be a no-op.

3. **Build viz state: derive from existing event handlers, don't duplicate subscriptions**
   - Do NOT create a new `useBuildVizState.ts` hook with its own event subscription (would be a third subscriber to `cross-build-event` alongside `useChronicleStream` and `useBuildRowState`)
   - Instead: extend `usePyramidData` to accumulate viz-relevant state from the events it already processes:
     - `verdictsByNode: Map<string, 'KEEP' | 'DISCONNECT' | 'MISSING'>` — from `VerdictProduced` events
     - `clusterMembers: Map<string, string[]>` — from `ClusterAssignment` events
     - `newEdges: Array<{sourceId, targetId}>` — from `EdgeCreated` events
   - Pass this state to the renderer alongside nodes/edges

4. **CanvasRenderer: implement viz-primitive-specific rendering**
   - `node_fill` (default): dots appearing in layer bands — already works
   - `edge_draw`: draw edges from `newEdges` state between existing nodes. Look up source/target node positions from the SurfaceNode array by ID. Render as animated lines fading in.
   - `verdict_mark`: overlay KEEP (green ring), DISCONNECT (orange ring), MISSING (yellow pulse) indicators on nodes using `verdictsByNode` map
   - `cluster_form`: highlight nodes sharing a cluster with a shared tint using `clusterMembers` map, then show parent node appearing above
   - `progress_only`: text status indicator — already works via build status bar

5. **GpuRenderer: same viz primitive modes** (mirror CanvasRenderer changes)

**Files:**
- `src/components/pyramid-surface/PyramidSurface.tsx` — call useVizMapping, derive activeVizPrimitive from currentStep, pass to renderer + pass build viz state
- `src/components/pyramid-surface/PyramidRenderer.ts` — add setActiveVizPrimitive
- `src/components/pyramid-surface/CanvasRenderer.ts` — implement per-primitive draw modes
- `src/components/pyramid-surface/GpuRenderer.ts` — mirror per-primitive draw modes
- `src/components/pyramid-surface/DomRenderer.ts` — no-op setActiveVizPrimitive
- `src/components/pyramid-surface/usePyramidData.ts` — accumulate verdict/cluster/edge events into viz state (extend existing event handler, no new subscription)

**Acceptance:**
- During source_extract: nodes appear in L0 band (node_fill)
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

1. **Emit EdgeCreated from Rust — BOTH execution paths**
   - In `chain_executor.rs` `execute_web_step` (~line 9527), after edges are persisted, emit `EdgeCreated` per edge
   - **ALSO** in `execute_ir_web_edges` (~line 12259) — the IR compiled path, which is the production execution path for compiled chains. This function mirrors execute_web_step but currently emits no edge events at all.
   - If the step's chain YAML has `viz.edge_batch_size`, batch accordingly
   - If absent, emit all edges (the frontend accumulates per-frame anyway)

2. **PyramidRenderer interface: add link intensities**
   - New method: `setLinkIntensities(intensities: Map<string, number>)`
   - The key format matches useVisualEncoding: `"source_id→target_id"`

3. **CanvasRenderer/GpuRenderer: render link intensities**
   - In `drawEdges`, for each edge, build the lookup key from `edge.fromId→edge.toId` and look up intensity
   - Modulate stroke alpha and width: high intensity = thick bright, low = thin faint
   - Only when `overlays.weightIntensity` is true

4. **PyramidSurface: pass link intensities to renderer**
   - Read `linkIntensities` from `useVisualEncoding` (already returned, just not consumed)
   - Call `renderer.setLinkIntensities(linkIntensities)` in a useEffect

5. **Edge position resolution for edge_draw viz primitive**
   - `EdgeCreated` events carry `source_id` and `target_id` (node IDs), NOT positions
   - The renderer resolves positions by looking up source/target in the current SurfaceNode array
   - If a node hasn't been positioned yet (still building), the edge is queued and drawn on the next frame when both nodes exist

**Files:**
- `src-tauri/src/pyramid/chain_executor.rs` — emit EdgeCreated in BOTH execute_web_step AND execute_ir_web_edges
- `src/components/pyramid-surface/PyramidRenderer.ts` — add setLinkIntensities
- `src/components/pyramid-surface/CanvasRenderer.ts` — use link intensities in drawEdges
- `src/components/pyramid-surface/GpuRenderer.ts` — same
- `src/components/pyramid-surface/DomRenderer.ts` — no-op setLinkIntensities
- `src/components/pyramid-surface/PyramidSurface.tsx` — pass linkIntensities to renderer

**Acceptance:**
- Evidence links from apex to L0 show visual gradient: thick/bright near apex, thin/faint at periphery
- Same raw link weight renders differently based on upstream importance
- Toggling Weight overlay on/off switches between flat edges and importance-weighted edges
- EdgeCreated events fire during webbing (both IR and non-IR paths)
- EdgeCreated events appear in the chronicle during webbing

---

## S2-3: Inspector on DADBEAR + Cleanup (GAP-6 + GAP-7)

**Goal:** Click any node on DADBEAR to open the inspector. Clean up dead code.

**What to build:**

1. **DADBEARPanel: add inspector**
   - Add `inspectedNodeId` state
   - Pass `onNodeClick` to PyramidSurface
   - Render `NodeInspectorPanel` when inspectedNodeId is set
   - For `allNodes`: add a `useEffect` that calls `pyramid_tree` IPC on mount, flattens the tree response into a `LiveNodeInfo[]`-compatible array for inspector navigation. Same pattern as PyramidSurfaceWindow.tsx.

2. **Replace inline tier detection with useRenderTier**
   - Import `useRenderTier` in PyramidSurface (call before the renderer lifecycle effect)
   - Use `tierInfo.tier` instead of the inline WebGL2 probe + `config.rendering.tier` detection
   - Delete the inline `testCanvas.getContext('webgl2')` detection code

3. **Note: ComposedView.tsx is now dead code**
   - Not imported anywhere after the Sprint 1 wiring pass
   - Mark for deletion in a future cleanup pass (not deleting now — parallel running period)

**Files:**
- `src/components/DADBEARPanel.tsx` — add inspector state + NodeInspectorPanel + allNodes fetch
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

2. **Persist after reconcile_layer — full ReconciliationResult**
   - In chain_executor.rs, after `reconcile_layer` returns (BEFORE the ReconciliationEmitted event emission), persist the FULL result as a contribution
   - Include ALL fields: orphan IDs (`recon_result.orphans`), central node IDs (`recon_result.central_nodes`), weight map (`recon_result.weight_map`), AND gaps (`recon_result.gaps`) — the plan originally dropped the gaps field
   - YAML format with slug, build_id, layer depth
   - Use `create_config_contribution` with `source: "build"` and build_id in triggering_note
   - **Check both execution paths:** the current ReconciliationEmitted emission is in the evidence_loop path. Verify whether reconcile_layer is also called from the IR executor path and add persistence there too if so.

**Files:**
- `src-tauri/src/pyramid/config_contributions.rs` — dispatcher branch
- `src-tauri/src/pyramid/schema_registry.rs` — display name + description
- `src-tauri/src/pyramid/chain_executor.rs` — persist reconciliation result as contribution (both execution paths if applicable)

**Acceptance:**
- After a build with evidence, `reconciliation_result` contribution exists in the store
- Visible in Tools tab
- Contains orphan IDs, central node IDs, weight map, AND gaps
- Persisted before ReconciliationEmitted event fires

---

## S2-5: Chronicle Post-Build Review (GAP-9)

**Goal:** After a build completes, the chronicle shows what happened (loaded from persisted data).

**What to build:**

1. **Resolve build_id for a slug**
   - The frontend has no `last_build_id` for a slug. Two options:
     - (a) New IPC: `pyramid_latest_build_id(slug)` — returns the most recent build_id from `pyramid_llm_audit` or the slug's metadata
     - (b) Extend `BuildStatus` to include `last_build_id` from the slug row
   - Option (a) is simpler — one focused IPC

2. **New IPC: `pyramid_get_build_chronicle(slug, build_id)`**
   - Queries existing tables (no new tables):
     - `pyramid_llm_audit` WHERE slug AND build_id — LLM calls with prompts, model, tokens, latency
     - `pyramid_evidence` WHERE slug AND build_id — KEEP/DISCONNECT verdicts with weights
     - `pyramid_gaps` WHERE slug — gap reports (filtered by layer/build context)
     - `pyramid_deferred_questions` WHERE slug — triage deferrals
     - `reconciliation_result` contributions WHERE slug AND build_id (from S2-4)
   - Returns chronologically sorted array by `created_at`
   - **Index check:** verify compound indices exist on (slug, build_id) for pyramid_llm_audit and pyramid_evidence. Add if missing.

3. **useChronicleStream: load historical data**
   - On mount, if no build is active: call `pyramid_latest_build_id(slug)` then `pyramid_get_build_chronicle(slug, buildId)`
   - Convert historical records to `ChronicleEntry` format (same as live events)
   - Show in Chronicle panel instead of "Awaiting events..."

**Files:**
- `src-tauri/src/main.rs` — two new IPCs (latest_build_id + get_build_chronicle)
- `src-tauri/src/pyramid/query.rs` — query functions, index verification
- `src/components/pyramid-surface/useChronicleStream.ts` — load historical data on mount

**Acceptance:**
- Open a completed pyramid → Chronicle shows all operations from the last build
- Decision entries (verdicts, triage, reconciliation) are expandable
- Mechanical entries (LLM calls, cache hits) shown when show_mechanical_ops is true
- No "Awaiting events..." for pyramids that have build history

---

## S2-6: Relationship Density Layout (GAP-4)

**Goal:** The "Density" toggle shows a force-directed layout where proximity encodes relationship strength.

**What to build:**

1. **New layout hook: `useDensityLayout`**
   - Force simulation: nodes repel, web edges attract proportional to strength
   - Node mass proportional to aggregate weight (central nodes are gravitational anchors)
   - All simulation parameters (repulsion coefficient, attraction multiplier, damping, settling threshold) come from `pyramid_viz_config` contribution — NOT hardcoded constants (Pillar 37). Seed defaults in the viz config YAML under a `density:` section.
   - Standard tier: simulation runs to convergence, result is static
   - Rich tier: live simulation via requestAnimationFrame with interactive dragging

2. **Extend pyramid_viz_config seed with density parameters**
   - Add `density:` section to the viz config YAML seed:
     ```yaml
     density:
       repulsion: auto
       attraction: auto
       damping: auto
       settle_threshold: auto
     ```
   - All `auto` — the simulation determines its own parameters based on node count and edge density. Explicit values override for users who want to tune.

3. **Wire layoutMode in PyramidSurface**
   - When `layoutMode === 'density'`, use `useDensityLayout` instead of `useUnifiedLayout`
   - Pass the resulting nodes/edges to the renderer
   - Node sizing driven by weight maps (central = larger)

4. **Labels appear based on size**
   - Nodes above a size threshold (configurable in viz config, not hardcoded) show headline labels
   - Smaller nodes are unlabeled until hover

5. **Performance budget**
   - Standard tier: simulation settles in <500ms for typical pyramids (<5000 nodes)
   - Rich tier: live simulation at 60fps
   - Profile during implementation — do not assume timing

**Files:**
- `src/components/pyramid-surface/useDensityLayout.ts` — NEW: force simulation
- `src/components/pyramid-surface/PyramidSurface.tsx` — wire layoutMode to layout selection
- `src-tauri/src/pyramid/viz_config.rs` — extend default config with density section
- `src-tauri/assets/bundled_contributions.json` — update seed YAML

**Acceptance:**
- Click "Density" toggle → nodes reposition based on relationship strength
- Strongly related nodes cluster together
- Central nodes are larger and act as visual anchors
- Click "Pyramid" → returns to trapezoid band layout
- Labels auto-appear on central nodes
- All simulation parameters come from viz config contribution, not hardcoded

---

## Method

Same as Sprint 1: implement → serial verifier → wanderer → commit. With two changes:

1. **Wanderer brief includes plan verification:** "Does this feature work end-to-end as described in the Sprint 2 plan? Check the specific acceptance criteria."

2. **No new event subscriptions unless justified:** Do not create additional `cross-build-event` listeners. Extend existing hooks that already subscribe.
