# Pyramid Surface — Gap Analysis

**Date:** 2026-04-13
**Context:** Sprint 1 shipped 7 phases + wiring pass (~11,000 lines). This document catalogs what was promised in the plan docs vs what actually works end-to-end.

---

## What Works

| Feature | Status | Notes |
|---------|--------|-------|
| Node Inspector (5-section panel) | Working | All contexts: build view, popup window. Accordion persistence works. |
| Viz Config Contribution | Working | Registered, seeded, Tools tab, ConfigSynced live reload |
| PyramidSurface shell | Working | Canvas2D and WebGL2 renderers, overlay toggles, tooltip, hit testing |
| Build-time node rendering | Working | Live nodes appear via polling during builds |
| Chronicle panel | Working | Event stream mapped, decision/mechanical categorization, auto-open during builds |
| Event Ticker | Working | Shows latest event headline, expands after 2s idle |
| Grid View | Working | All pyramids as cards, sorting, activity glow, responsive grid |
| Multi-window popup | Working | Opens from Grid card click, inspector works in popup |
| GPU Renderer | Working | WebGL2 auto-detection, instanced nodes, batched edges, bloom, Canvas2D fallback |
| Visual Encoding (nodes) | Working | Three-axis (brightness/saturation/border) computed via BFS and applied to nodes |
| Wiring pass | Working | PyramidSurface replaces old viz on Dashboard, Builds tab, DADBEAR |
| Stale detection colors | Working | StaleLogEntry flows through, node states derived correctly |
| Bedrock layer | Working | Source files rendered below L0 |
| Minimap | Working | Self-adjusting dot renderer in top-right of canvas |

---

## Gaps: Scaffolding That's Not Connected

### GAP-1: Viz-from-YAML not wired (AD-1 — the core architectural innovation)

**Plan says:** Chain YAML drives visualization. Each step's primitive type maps to a viz primitive (node_fill, edge_draw, cluster_form, verdict_mark, progress_only). The renderer changes behavior based on which step is running.

**What exists:**
- `useVizMapping.ts` — complete hook, loads chain via `pyramid_get_build_chain` IPC, builds step→viz map
- `pyramid_get_build_chain` Rust IPC — resolves chain YAML by slug, returns parsed JSON
- `PRIMITIVE_TO_VIZ` mapping table — covers all known primitives
- `VizPrimitive` type — defined with all 5 values

**What's missing:**
- PyramidSurface never imports or calls `useVizMapping`
- No code reads the current step's viz primitive and switches rendering behavior
- During webbing: should show edges drawing between nodes (edge_draw), but shows nothing
- During evidence loop: should show verdict indicators on nodes (verdict_mark), but shows nothing
- During clustering: should show nodes grouping (cluster_form), but shows nothing
- All steps render identically as node_fill

**Fix scope:** Import `useVizMapping` in PyramidSurface. Subscribe to `ChainStepStarted` events (already in the event bus). When a step starts, look up its viz primitive. Pass the active viz primitive to the renderer. The CanvasRenderer and GpuRenderer need a `setActiveVizPrimitive(primitive)` method that changes what they draw during the current step. For `edge_draw`: animate edges as they're created. For `verdict_mark`: show KEEP/DISCONNECT indicators on source nodes. For `cluster_form`: visually group nodes before parent appears.

### GAP-2: Per-link visual intensity not rendered (from visual encoding spec)

**Plan says:** Evidence links show "rivers of importance" — `link_visual_intensity = link_weight × upstream_node.propagated_importance`. Thick bright links from apex path, thin faint links from periphery.

**What exists:**
- `useVisualEncoding.ts` computes `linkIntensities: Map<string, number>` correctly
- The map is keyed by `"source_id→target_id"` with the correct intensity formula

**What's missing:**
- `useVisualEncoding` returns `linkIntensities` but PyramidSurface never reads it
- Neither CanvasRenderer nor GpuRenderer accept link intensity data
- The PyramidRenderer interface has no method for setting link intensities
- Edge drawing uses flat colors regardless of importance

**Fix scope:** Add `setLinkIntensities(intensities: Map<string, number>)` to the PyramidRenderer interface. Both CanvasRenderer and GpuRenderer implement it. PyramidSurface reads `linkIntensities` from `useVisualEncoding` and passes to renderer. Edge drawing modulates stroke alpha/width based on intensity.

### GAP-3: EdgeCreated event never emitted

**Plan says:** `EdgeCreated` fires per-edge during webbing (or in batches). Frontend accumulates and renders in next animation frame.

**What exists:**
- `TaggedKind::EdgeCreated` variant defined in event_bus.rs with correct fields
- Frontend `useBuildRowState` handles `edge_created` events
- `KnownTaggedKind` union includes the type

**What's missing:**
- chain_executor.rs `execute_web_step` never calls `emit_chain_event` with `EdgeCreated`
- The webbing step completes silently between `WebEdgeStarted` and `WebEdgeCompleted`

**Fix scope:** In `execute_web_step`, after edges are persisted, iterate the edge collection and emit `EdgeCreated` events. Respect the chain YAML `viz.edge_batch_size` parameter (or skip if absent, per the plan).

### GAP-4: Relationship density layout is dead state

**Plan says (RD-3):** "Relationship density view available on any pyramid type. More like a weighted word cloud. Bond tightness weighted by actual relationship strength. Node sizing driven by weight maps."

**What exists:**
- Pyramid/Density toggle buttons in PyramidSurface toolbar
- `layoutMode` state ('pyramid' | 'density')

**What's missing:**
- `layoutMode` is never passed to any layout engine or renderer
- No force-directed layout algorithm exists
- No relationship-based positioning code
- Clicking "Density" does nothing

**Fix scope:** Implement a force-directed layout in `useUnifiedLayout` (or a new `useDensityLayout` hook). When `layoutMode === 'density'`: position nodes by relationship strength (close = strongly related), size by weight map centrality. This is a significant algorithm — force simulation with attraction proportional to web edge relevance. In Standard tier: pre-computed static layout. In Rich tier: GPU-accelerated live simulation.

### GAP-5: reconciliation_result contribution never created

**Plan says (Phase 4 persistence):** "Reconciliation summaries persist as contributions (schema_type: reconciliation_result, build_id scoped)."

**What exists:**
- `ReconciliationEmitted` event fires with orphan_count and central_count
- The event is handled in `useBuildRowState` (shows in activity log)

**What's missing:**
- No `reconciliation_result` schema registered in config_contributions.rs
- No schema_definition or schema_annotation for this type
- Reconciliation data is still computed and discarded after event emission
- No persistence to contribution store

**Fix scope:** Register `reconciliation_result` schema type (same pattern as `pyramid_viz_config` in Phase 2a). In chain_executor.rs after `reconcile_layer`, persist the result as a contribution. Add the dispatcher branch. This enables post-build queryability in the Chronicle.

### GAP-6: Inspector not available on DADBEAR

**Plan says:** Node Inspector available in ALL contexts.

**What exists:**
- Inspector works in PyramidTheatre (build view) and PyramidSurfaceWindow (popup)
- DADBEARPanel renders PyramidSurface but passes no `onNodeClick`

**What's missing:**
- Clicking nodes on DADBEAR's PyramidSurface does nothing
- No `inspectedNodeId` state or `NodeInspectorPanel` in DADBEARPanel

**Fix scope:** Add `inspectedNodeId` state + `NodeInspectorPanel` to DADBEARPanel, same pattern as PyramidTheatre.

### GAP-7: useRenderTier hook unused

**Plan says (AD-5):** "useRenderTier — detection + preference hook"

**What exists:**
- `useRenderTier.ts` — complete hook with WebGPU/WebGL2 detection

**What's missing:**
- Never imported. PyramidSurface does inline tier detection.

**Fix scope:** Import and use `useRenderTier` in PyramidSurface instead of inline detection. Minor cleanup — functional behavior is identical.

---

## Gaps: Features That Need Design Work

### GAP-8: Viz primitives need renderer support

Even after GAP-1 is fixed (wiring useVizMapping), the renderers need to know HOW to render each viz primitive during a build step:

- **edge_draw**: Animate edges appearing between existing nodes. Requires knowing which edges were just created (from `EdgeCreated` events or `WebEdgeCompleted` with edge data).
- **verdict_mark**: Show KEEP/DISCONNECT/MISSING indicators on source nodes. Requires knowing which nodes received which verdict (from `VerdictProduced` events).
- **cluster_form**: Show nodes visually grouping before parent appears. Requires cluster membership data (from `ClusterAssignment` events).
- **progress_only**: Text status indicator — already works via build status bar.
- **node_fill**: Nodes appearing in a layer — already works.

Each of these needs both data flow (events → state) and rendering (state → visual). The data flow exists via the event bus. The rendering needs new draw modes in CanvasRenderer/GpuRenderer.

### GAP-9: Chronicle post-build review

**Plan says:** "Post-build: chronicle can be reviewed (loads from persisted data)."

**What exists:** Chronicle shows live events during builds. "Awaiting events..." when idle.

**What's missing:** After a build completes, the chronicle is empty. No code loads historical events from `pyramid_llm_audit`, `pyramid_evidence`, etc. for post-build review.

**Fix scope:** New hook or extension to `useChronicleStream` that loads historical data from existing audit tables when no build is active. Needs a new IPC or multiple existing IPCs combined.

---

## Root Cause

The implementation focused on creating components that compile and pass type checks, but didn't close the loop on cross-component wiring. Each phase was verified internally (files exist, types match, imports resolve) but not verified against the plan's promised end-to-end behavior.

The viz-from-YAML gap (GAP-1) is the most significant because it's the core architectural innovation — the thing that makes the system composable rather than hardcoded. The hook is 100% complete. The IPC is 100% complete. The connection between them is 0%.
