# Pyramid Surface Fixes — Gap Analysis & Build Plan

**Date**: 2026-04-13
**Source**: handoff-2026-04-13-pyramid-surface.md FIX-1 through FIX-4
**Audit**: 2 independent auditors, findings integrated below

---

## Gap Analysis

### FIX-1: Chronicle layout — flex child, not overlay

**Current state (code-verified)**:
- `PyramidSurface.tsx` renders `<Chronicle>` INSIDE `ps-canvas-container` (the div that holds the canvas/renderer and attaches mouse handlers) — around line 458
- `.ps-chronicle` CSS: `position: absolute; bottom: 0; left: 0; right: 0; max-height: 40%;` — paints on top of canvas, covers L0 nodes
- `.ps-full` already has `display: flex; flex-direction: column;` — the flex container is ready

**What needs to change**:
1. **PyramidSurface.tsx**: Move `<Chronicle>` out of `ps-canvas-container`, place it as a flex sibling immediately after the canvas container div (still inside `ps-full`).
2. **dashboard.css**: Rewrite `.ps-chronicle` — remove `position: absolute; bottom: 0; left: 0; right: 0;`. Make it a flex child: `flex-shrink: 0; max-height: min(40vh, 40%);` (bounded by both viewport and parent). Keep `overflow-y: auto`, background, backdrop-filter, border-top, z-index.
3. **No other changes needed**: ResizeObserver already watches `containerRef` (the canvas container). When Chronicle appears and takes flex space, the canvas container shrinks, ResizeObserver fires, renderer resizes. When Chronicle closes, canvas gets full height back. This is automatic.

**Risk — border-radius**: The `ps-canvas-container` has `border-radius: 0 0 var(--radius-md) var(--radius-md)` AND its `> canvas` child has the same bottom border-radius. When Chronicle is open below it, both need bottom radius removed — only Chronicle should have bottom radius. Use `:has(+ .ps-chronicle)` selector targeting BOTH the container and `> canvas`.

---

### FIX-2: Revert to old modal inspector, enrich Details tab

**Current state (code-verified)**:
- Three consumers all import `NodeInspectorPanel`:
  - `src/components/PyramidTheatre.tsx` line 6 (import), line 144 (usage)
  - `src/components/DADBEARPanel.tsx` line 4 (import), line 1218 (usage)
  - `src/components/pyramid-surface/PyramidSurfaceWindow.tsx` line 14 (import), line 102 (usage)
- Props are IDENTICAL between Panel and Modal: `{ slug, nodeId, allNodes, onClose, onNavigate }` — drop-in swap
- Both Panel and Modal use `position: fixed` for their overlay — no flex layout impact on swap
- `NodeInspectorModal` is the centered overlay + three-tab format (Prompt/Response/Details)
- `DetailsTab.tsx` currently shows: evidence, gaps, children, web edges, metadata table, self_prompt, dead_ends

**Key discovery — rich sub-components already exist**:
The Panel's 5 accordion sections delegate to standalone sub-components that already render ALL the missing fields:
- `ContentSection.tsx` → narrative, topics, corrections, decisions, terms, key quotes, dead ends
- `EpisodicSection.tsx` → entities (name/role/importance/liveness), time range, weight/provisional/promoted_from
- `StructureSection.tsx` → children, evidence, web edges, remote web edges, transitions, question context, gaps
- `ProvenanceSection.tsx` → self-prompt, build ID, created_at, version, chain phase, superseded_by
- `LlmRecordSection.tsx` → full prompt/response/metadata (duplicates Prompt/Response tabs, skip)

These components are NOT local to NodeInspectorPanel. They import from `'../AccordionSection'` which is a shared standalone component at `src/components/AccordionSection.tsx` (used by 6+ files). All sub-components take typed `PyramidNodeFull` and `DrillResultFull`.

**What needs to change**:
1. **PyramidTheatre.tsx**: Change import `NodeInspectorPanel` → `NodeInspectorModal`, change JSX tag
2. **DADBEARPanel.tsx**: Same swap
3. **PyramidSurfaceWindow.tsx**: Same swap
4. **DetailsTab.tsx**: Replace the hand-rolled sections with the existing sub-components. Import `ContentSection`, `EpisodicSection`, `StructureSection`, `ProvenanceSection`. Import `PyramidNodeFull`, `DrillResultFull` from `inspector-types.ts`. Cast `drillData as DrillResultFull`. Render the 4 sub-components inside AccordionSection wrappers. Keep the existing audit metadata table at the bottom (LlmRecordSection is NOT needed since Prompt/Response tabs already cover LLM data).

**Risk**: The sub-components use `ni-*` CSS classes (from the Panel design). These classes already exist in `dashboard.css` from the Panel implementation. The styles will carry over into the Modal's DetailsTab — this is actually desirable since the styling is good; it's the Panel's chrome (slide-in sidebar) that Adam dislikes, not the content rendering.

---

### FIX-3: Density OOM guard

**Current state (code-verified)**:
- `useDensityLayout.ts`: O(n²) all-pairs repulsion in `runSimulation()`. 2000 nodes = 4M distance calculations per iteration, up to 500 iterations = 2 billion ops. This freezes the app.
- `DensityConfig` interface has 7 fields, no `max_nodes`
- `PyramidVizConfig.density` in `useVizConfig.ts` — no `max_nodes`
- `default_pyramid_viz_config()` in `viz_config.rs` — no `max_nodes`
- Bundled contribution YAML in `bundled_contributions.json` — no `max_nodes`
- Schema definition contribution in `bundled_contributions.json` — no `max_nodes` in JSON schema

**What needs to change**:
1. **viz_config.rs**: Add `"max_nodes": "auto"` to `default_pyramid_viz_config()` density section
2. **useVizConfig.ts**: Add `max_nodes: number | 'auto'` to `PyramidVizConfig.density` interface and `DEFAULT_VIZ_CONFIG`
3. **useDensityLayout.ts**: Add `max_nodes: number | 'auto'` to `DensityConfig`. Add `resolveMaxNodes()` helper (auto → 500). Guard at the top of the `useMemo` computation: if `nodes.length > resolvedMaxNodes`, return passthrough nodes/edges + `settled: false` + `disabled: true`. Add `disabled: boolean` to the return type. Normal path returns `disabled: false`.
4. **PyramidSurface.tsx**: Add `max_nodes: d?.max_nodes ?? 'auto'` to the `densityConfig` useMemo (currently 7 fields, becomes 8). Destructure `disabled: densityDisabled` from `useDensityLayout`. When `layoutMode === 'density' && densityDisabled`, auto-revert `layoutMode` to `'pyramid'` and show a brief inline message. This prevents the awkward state of density-disabled + Chronicle open with a dead canvas.
5. **bundled_contributions.json**: Add `max_nodes: auto` to the density YAML content string
6. **bundled_contributions.json**: Add `max_nodes` to the JSON schema definition contribution (the schema_definition for pyramid_viz_config)

**Design choice**: Where to put the guard?
- Option A: Inside `useDensityLayout` — return passthrough + disabled flag. Clean separation.
- Option B: Inside `PyramidSurface` before calling the hook — skip the hook entirely. But React hooks can't be conditionally called.

**Decision**: Option A. The hook already returns passthrough when `!active`. Add the node-count guard alongside that check. Add `disabled: boolean` to the return type.

---

### FIX-4: Tooltip clipping

**Current state (code-verified)**:
- Tooltip renders inside `ps-canvas-container` with `position: absolute`
- Positioned at `left: node.x + 12, top: node.y - 8` — relative to canvas container
- `ps-canvas-container` has `overflow: visible` and `ps-full` has `overflow: visible` — so CSS overflow isn't the issue
- The ACTUAL problem: when a node is near the right/bottom edge of the viewport, the tooltip extends beyond the viewport. It's "visible" in DOM terms but cut off by the browser viewport. There's no viewport-boundary clamping.

**What needs to change**:
1. **PyramidSurface.tsx**: Add viewport boundary clamping to tooltip positioning. After computing `left` and `top`, check against `containerSize.width` and `containerSize.height`. If tooltip would extend past the right edge, flip to `left: node.x - tooltipWidth - 12`. If it would extend past the bottom, flip to `top: node.y - tooltipHeight - 8`. This requires measuring the tooltip element.
2. **Implementation approach**: Use a `ref` on the tooltip div. After first render at the default position, measure `getBoundingClientRect()`. If it exceeds the container bounds, adjust. To avoid flicker, render the tooltip initially with `visibility: hidden`, measure, reposition, then make visible. OR simpler: compute a clamped position using estimated tooltip dimensions (max-width is 280px from CSS, height ~60-80px typical).

**Simpler approach**: Use the CSS `max-width: 280px` as the known width. Clamp:
- `left = Math.min(node.x + 12, containerWidth - 290)` (280 + 10px padding)
- `top = Math.min(node.y - 8, containerHeight - 80)` (estimated height)
- Also clamp `left >= 0` and `top >= 0`

This avoids refs, measurement, and flicker. The tooltip is already `pointer-events: none` so slight positional imprecision is fine.

---

## Build Plan

### Phase 1: FIX-1 — Chronicle as flex child

**Files**: `PyramidSurface.tsx`, `dashboard.css`

1. Move `<Chronicle>` JSX block out of `ps-canvas-container`, place it as a sibling after the canvas container `</div>` (still inside `ps-full`, before `EventTicker`)
2. Rewrite `.ps-chronicle` CSS:
   - Remove: `position: absolute; bottom: 0; left: 0; right: 0;`
   - Add: `flex-shrink: 0;`
   - Change: `max-height: 40%` → `max-height: min(40vh, 40%)`
   - Keep: `overflow-y: auto`, background, backdrop-filter, border-top, z-index
   - Add: `border-radius: 0 0 var(--radius-md) var(--radius-md);` (Chronicle now owns the bottom corners)
3. Handle border-radius on canvas when Chronicle is open — TWO selectors:
   ```css
   .ps-canvas-container:has(+ .ps-chronicle) {
       border-radius: 0;
   }
   .ps-canvas-container:has(+ .ps-chronicle) > canvas {
       border-radius: 0;
   }
   ```

**Verify**: Build starts → Chronicle opens → canvas shrinks → L0 nodes remain visible above Chronicle → close Chronicle → canvas reclaims full height. Check that bottom corners transfer from canvas to Chronicle when open.

---

### Phase 2: FIX-4 — Tooltip boundary clamping

**Files**: `PyramidSurface.tsx`

1. In the tooltip rendering block, replace the simple `left/top` positioning with clamped values:
   ```
   const tooltipW = 290; // max-width 280 + margin
   const tooltipH = 120; // conservative: headline + meta + 4 lines distilled + padding
   let left = node.x + 12;
   let top = node.y - 8;
   if (left + tooltipW > containerSize.width) left = node.x - tooltipW;
   if (top + tooltipH > containerSize.height) top = node.y - tooltipH;
   left = Math.max(0, left);
   top = Math.max(0, top);
   ```
   Note: `node.x` and `node.y` are layout coordinates in container-pixel-space (set by `usePyramidData`). No camera/pan/zoom transform exists — coordinates are container-relative. The clamping math is correct.
2. No CSS changes needed.

**Verify**: Hover over nodes at all four edges → tooltip stays fully visible within the surface. Check bottom-right corner nodes especially.

---

### Phase 3: FIX-3 — Density OOM guard

**Files**: `useDensityLayout.ts`, `useVizConfig.ts`, `viz_config.rs`, `bundled_contributions.json`, `PyramidSurface.tsx`, `dashboard.css`

1. **useDensityLayout.ts**:
   - Add `max_nodes: number | 'auto'` to `DensityConfig`
   - Add `resolveMaxNodes(cfg: number | 'auto'): number` helper: `auto` → 500
   - Add `disabled: boolean` to return type
   - At top of useMemo, after the existing `!active` guard: resolve max_nodes, then if `nodes.length > resolvedMaxNodes`, return `{ nodes, edges, settled: false, labelMinRadius: 0, disabled: true }` (`settled: false` — simulation did not run, not "converged")
   - Normal path returns `disabled: false`

2. **useVizConfig.ts**:
   - Add `max_nodes: number | 'auto'` to `PyramidVizConfig.density` interface
   - Add `max_nodes: 'auto'` to `DEFAULT_VIZ_CONFIG.density`

3. **viz_config.rs**:
   - Add `"max_nodes": "auto"` to `default_pyramid_viz_config()` density object

4. **bundled_contributions.json**:
   - Add `  max_nodes: auto\n` to the pyramid_viz_config YAML content string (inside the `density:` block)
   - Add `"max_nodes":{"oneOf":[{"type":"integer","minimum":1},{"type":"string","const":"auto"}]}` to the schema_definition JSON's `density.properties`

5. **PyramidSurface.tsx**:
   - Add `max_nodes: d?.max_nodes ?? 'auto'` to the `densityConfig` useMemo (line ~100-111, currently 7 fields → 8)
   - Destructure `disabled: densityDisabled` from `useDensityLayout`
   - Add a `useEffect` that watches `densityDisabled`: when it becomes true and `layoutMode === 'density'`, auto-revert to `setLayoutMode('pyramid')`. This prevents the confusing state of density-disabled + Chronicle open with a dead canvas area.
   - Disable the "Density" toggle button when `densityDisabled` is true (add `disabled` attr + title with explanation)

6. **dashboard.css**: Style `.ps-toggle-btn:disabled` — muted color, cursor not-allowed, tooltip-compatible

**Verify**: Open a pyramid with >500 nodes → click Density → reverts to Pyramid, button disabled with tooltip → no freeze. Open a small pyramid → Density works normally.

---

### Phase 4: FIX-2 — Revert to modal inspector + enrich Details tab

**Files**: `PyramidTheatre.tsx`, `DADBEARPanel.tsx`, `PyramidSurfaceWindow.tsx`, `DetailsTab.tsx`

**4a: Swap Panel → Modal (3 files)**:
1. `PyramidTheatre.tsx` line 6: `import { NodeInspectorPanel }` → `import { NodeInspectorModal }`; line 144: `<NodeInspectorPanel` → `<NodeInspectorModal`
2. `DADBEARPanel.tsx` line 4: same import swap; line 1218: same tag swap
3. `PyramidSurfaceWindow.tsx` line 14: same import swap; line 102: same tag swap

**4b: Rewrite DetailsTab.tsx using existing sub-components**:

The Panel's rich content is already implemented in standalone sub-components. Instead of rebuilding everything, import and compose them:

1. **Imports to add**:
   ```typescript
   import { ContentSection } from './ContentSection';
   import { EpisodicSection } from './EpisodicSection';
   import { StructureSection } from './StructureSection';
   import { ProvenanceSection } from './ProvenanceSection';
   import { AccordionSection } from '../AccordionSection';
   import type { DrillResultFull, PyramidNodeFull } from './inspector-types';
   ```

2. **Cast drillData**: `const drill = drillData as DrillResultFull;` and `const node = drill.node as PyramidNodeFull;`

3. **Replace the entire DetailsTab body** with AccordionSection-wrapped sub-components:
   - `<AccordionSection title="Content" defaultOpen>` → `<ContentSection node={node} />`
   - `<AccordionSection title="Episodic" defaultOpen>` → `<EpisodicSection node={node} />`
   - `<AccordionSection title="Structure" defaultOpen>` → `<StructureSection drill={drill} onNavigate={onNavigate} />`
   - `<AccordionSection title="Provenance">` → `<ProvenanceSection node={node} />`
   - Keep the existing audit metadata table at the bottom (LlmRecordSection NOT needed — Prompt/Response tabs already show LLM data)

4. **No new CSS needed**: The `ni-*` classes used by the sub-components already exist in `dashboard.css` from the Panel implementation. The styling is good — it's the Panel's slide-in chrome that Adam dislikes, not the content rendering. The sub-components will render their `ni-*` styled content inside the Modal's tab panel.

5. **Props change**: DetailsTab currently receives `{ drillData, audit, children, onNavigate }`. The `children` prop becomes unused since StructureSection renders children from `drill.children`. The `audit` prop stays for the metadata table at the bottom. Update the interface to drop `children` (or leave it for backwards compat, it's harmless).

**Verify**: Click a node in PyramidTheatre → centered modal opens with Prompt/Response/Details tabs → Details tab shows Content (narrative, topics, corrections, decisions, terms, key quotes, dead ends), Episodic (entities, time range, weight/status), Structure (children, evidence, web edges, transitions, gaps), Provenance (self-prompt, build ID, version) → keyboard navigation works → close with Escape/X. Same in DADBEAR and PyramidSurfaceWindow.

---

## Phase Sequencing

| Phase | Fix | Depends On | Est. Files |
|-------|-----|-----------|------------|
| 1     | FIX-1: Chronicle flex | None | 2 |
| 2     | FIX-4: Tooltip clamp | None | 1 |
| 3     | FIX-3: Density guard | None | 6 |
| 4     | FIX-2: Modal revert + Details rewrite | None | 4 |

All phases are independent — no cross-dependencies. Phases 1-3 share `PyramidSurface.tsx` so they should run sequentially (or the implementer merges carefully). Phase 4 touches entirely different files and can run in parallel with 1-3.

---

## Files Changed (complete list)

| File | Phases |
|------|--------|
| `src/components/pyramid-surface/PyramidSurface.tsx` | 1, 2, 3 |
| `src/styles/dashboard.css` | 1, 3 |
| `src/components/pyramid-surface/useDensityLayout.ts` | 3 |
| `src/hooks/useVizConfig.ts` | 3 |
| `src-tauri/src/pyramid/viz_config.rs` | 3 |
| `src-tauri/assets/bundled_contributions.json` | 3 |
| `src/components/PyramidTheatre.tsx` | 4 |
| `src/components/DADBEARPanel.tsx` | 4 |
| `src/components/pyramid-surface/PyramidSurfaceWindow.tsx` | 4 |
| `src/components/theatre/DetailsTab.tsx` | 4 |

---

## Audit Trail

**Round 1**: Two independent auditors. Key findings integrated:
- FIX-2: Reuse existing sub-components (ContentSection, EpisodicSection, StructureSection, ProvenanceSection) instead of rebuilding. AccordionSection is a shared component, not local to Panel.
- FIX-1: Border-radius `:has()` must target both container AND `> canvas` child. `max-height` uses `min(40vh, 40%)` for embedded contexts.
- FIX-3: `settled` in disabled path corrected to `false`. `max_nodes` propagation in `densityConfig` useMemo made explicit. Density mode auto-reverts to pyramid when guard fires (prevents dead-canvas + Chronicle state).
- FIX-4: Tooltip height estimate increased from 80px to 120px for multi-line distilled text.
