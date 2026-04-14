# Handoff: Pyramid Surface — 2026-04-13

## What Was Done

Two full sprints + polish fixes shipped the Pyramid Surface visualization system. ~13,000 lines of new code across 30+ commits.

### Sprint 1 (7 phases): Component Architecture
- **Phase 1**: Node Inspector rewrite — five-section scrollable panel replacing three-tab modal
- **Phase 2a**: `pyramid_viz_config` contribution type (Tools tab, ConfigSynced live reload)
- **Phase 2b**: PyramidSurface shell + CanvasRenderer + GpuRenderer (WebGL2) + DomRenderer + MiniaturePyramid
- **Phase 2c**: Unified data hook (static tree + build progress + event bus)
- **Phase 3a**: Viz-from-YAML — chain YAML drives visualization via primitives (node_fill, edge_draw, verdict_mark, cluster_form, progress_only)
- **Phase 3b**: Three-axis visual encoding (brightness = citation, saturation = propagated importance, border = connectivity)
- **Phase 4**: Chronicle + Event Ticker + Minimap
- **Phase 5**: Grid View (mission control) — all pyramids as miniature cards
- **Phase 6**: Multi-window — popup Tauri windows, nested mode
- **Phase 7**: GpuRenderer (WebGL2 instanced rendering, bloom pipeline)
- **Wiring pass**: PyramidSurface replaces old viz on Dashboard, Builds tab, DADBEAR

### Sprint 2 (6 phases): Close All Gaps
- **S2-1**: Viz-from-YAML actually wired (useVizMapping called, activeVizPrimitive drives renderer)
- **S2-2**: EdgeCreated events emitted from both web execution paths + link intensity pipeline verified
- **S2-3**: Inspector on DADBEAR + useRenderTier cleanup
- **S2-4**: Reconciliation persistence as contributions
- **S2-5**: Chronicle post-build review (historical data from audit tables)
- **S2-6**: Relationship density layout (force simulation, auto-fit, configurable via viz config)

### Polish Fixes (A-D)
- **Fix A**: Chain-driven expected depths — max_depth from build config, L0 stays near bottom during builds
- **Fix B**: Chronicle as overlay (newest first, close button)
- **Fix C**: Wizard collapse — AddWorkspace/AskQuestion collapse chrome when build starts
- **Fix D**: Event content enrichment — NodeProduced event with headline, VerdictProduced/EdgeCreated enriched with headlines. Chronicle shows "Extracted: **Wire Game Design Kit**" not "LLM: gemma4:26b 5562tok"

---

## What Needs Fixing (from latest testing)

### FIX-1: Chronicle layout — flex child, not overlay

**Current state**: Chronicle is `position: absolute; bottom: 0` overlaying the canvas. Covers L0 nodes.

**Desired**: Chronicle is a vertical flex child BELOW the canvas. When open, the canvas container shrinks and the pyramid auto-adjusts via ResizeObserver. When closed, canvas gets full height back.

**Files**: `src/components/pyramid-surface/PyramidSurface.tsx` (move Chronicle out of canvas container, back to flex sibling), `src/styles/dashboard.css` (revert `.ps-chronicle` from absolute to flex child)

### FIX-2: Revert to old modal inspector format

**Current state**: NodeInspectorPanel is a slide-in right panel with five accordion sections. Adam finds this worse than the old centered modal with three tabs.

**Desired**: Use the OLD `NodeInspectorModal` (centered modal, Prompt/Response/Details tabs) everywhere. Add the missing rich data to the Details tab:
- Narrative (multi-zoom levels)
- Entities (name, role, importance, liveness)
- Key Quotes (text, speaker, importance)
- Corrections (wrong → right → who)
- Decisions (decided, why, stance badge, importance)
- Terms (glossary)
- Transitions (prior/next question)
- Weight, provisional, promoted_from
- Version history (current_version, chain_phase)
- Time range

**Approach**: 
1. Revert `PyramidTheatre.tsx` to import `NodeInspectorModal` instead of `NodeInspectorPanel`
2. Revert `DADBEARPanel.tsx` to use `NodeInspectorModal`
3. Revert `PyramidSurfaceWindow.tsx` to use `NodeInspectorModal`
4. Enhance `DetailsTab.tsx` with the additional fields (add collapsible sections using AccordionSection)
5. Update the TypeScript `DrillResult` type reference (already done in `inspector-types.ts`)

**Files**: `PyramidTheatre.tsx`, `DADBEARPanel.tsx`, `PyramidSurfaceWindow.tsx`, `src/components/theatre/DetailsTab.tsx`

### FIX-3: Density OOM guard

**Current state**: Density layout runs O(n²) force simulation. On pyramids with 2000+ nodes, this freezes/OOMs the app.

**Desired**: Disable density above a configurable node count threshold (from `pyramid_viz_config.density.max_nodes`). Show "Density view available for smaller pyramids" message. Default threshold from viz config seed (not hardcoded).

**Files**: `src/components/pyramid-surface/PyramidSurface.tsx` (guard before calling useDensityLayout), `src-tauri/src/pyramid/viz_config.rs` (add `max_nodes` to density config), `src/hooks/useVizConfig.ts` (add to TypeScript interface), bundled_contributions.json (add to seed + schema)

### FIX-4: Canvas/tooltip clipping

**Current state**: Tooltip is constrained inside the canvas container div, clips at edges.

**Desired**: Tooltip renders outside the canvas container (portal or parent-level positioning) so it's always fully visible.

**Files**: `src/components/pyramid-surface/PyramidSurface.tsx` (move tooltip rendering or use portal)

---

## Key Architecture Decisions

- **Viz-from-YAML (AD-1)**: Chain primitives drive visualization. `useVizMapping` loads chain definition, maps step_name → viz primitive. Renderer switches draw mode per step. New chain types from Wire market auto-visualize.
- **Three-axis encoding**: Brightness = direct citation, Saturation = propagated importance (BFS from apex), Border thickness = web edge count. Power curve normalization. All from `pyramid_viz_config` contribution.
- **Event content enrichment**: `NodeProduced`, enriched `VerdictProduced`/`EdgeCreated` carry headlines at emission time. Chronicle renders content, not just metadata.
- **Viz config is a contribution**: `pyramid_viz_config` schema type, supersedable, per-pyramid overridable, editable in Tools tab.

## Key Files

| File | What |
|------|------|
| `src/components/pyramid-surface/PyramidSurface.tsx` | Main component — full/nested/ticker modes |
| `src/components/pyramid-surface/CanvasRenderer.ts` | Canvas 2D renderer with viz overlays |
| `src/components/pyramid-surface/GpuRenderer.ts` | WebGL2 renderer with bloom |
| `src/components/pyramid-surface/usePyramidData.ts` | Unified data hook (tree + build + events) |
| `src/components/pyramid-surface/useVizMapping.ts` | Chain YAML → viz primitive mapping |
| `src/components/pyramid-surface/useVisualEncoding.ts` | Three-axis encoding + BFS propagation |
| `src/components/pyramid-surface/useChronicleStream.ts` | Event stream → chronicle entries |
| `src/components/pyramid-surface/useDensityLayout.ts` | Force simulation layout |
| `src/components/pyramid-surface/Chronicle.tsx` | Chronicle panel (newest first) |
| `src/components/pyramid-surface/GridView.tsx` | All-pyramids mission control |
| `src/components/pyramid-surface/PyramidSurfaceWindow.tsx` | Popup window wrapper |
| `src/components/theatre/NodeInspectorModal.tsx` | OLD modal inspector (preferred format) |
| `src/components/theatre/NodeInspectorPanel.tsx` | NEW panel inspector (deprecated — revert) |
| `src-tauri/src/pyramid/event_bus.rs` | TaggedKind variants including NodeProduced |
| `src-tauri/src/pyramid/viz_config.rs` | Viz config contribution get/set |
| `src/hooks/useVizConfig.ts` | Frontend viz config with ConfigSynced reload |

## Plan Documents

- `docs/plans/pyramid-surface.md` — Sprint 1 plan (audit-clean, 3 cycles)
- `docs/plans/pyramid-surface-visual-encoding.md` — Three-axis encoding spec
- `docs/plans/pyramid-surface-gap-analysis.md` — Gap analysis between plan and implementation
- `docs/plans/pyramid-surface-sprint2.md` — Sprint 2 plan (audit-clean, 2 cycles)
- `docs/plans/pyramid-surface-polish.md` — Polish fixes plan (audit-clean, 3 cycles)

## Process Learnings

1. **Wanderers must verify plan delivery, not just code quality.** Sprint 1 wanderers confirmed "does this compile" but not "does this feature work end-to-end." Sprint 2 fixed this.
2. **Verifier first, then wanderer, always sequential.** The wanderer's value is finding bugs that survive systematic verification. Running them in parallel wastes that capability.
3. **Every intelligence event should carry its content.** IDs that need frontend resolution create timing gaps and concurrent attribution problems. The event stream should BE the content stream.
4. **The PRIMITIVE_TO_VIZ map must use actual chain YAML primitives**, not Rust executor internals. `extract` not `for_each`.
5. **Numbers that look like "reasonable defaults" in rendering/simulation MUST come from viz config contribution.** Even if they're rendering parameters, route them through the contribution for consistency.
