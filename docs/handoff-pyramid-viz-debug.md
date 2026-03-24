# Handoff: Pyramid Visualization Debug

## Problem
Canvas-based pyramid visualization in the DADBEAR panel renders an empty canvas. Debug border visible, container sized correctly, but **zero nodes appear**. The empty-state triangle outline and "Build a pyramid to see it here" text ARE visible, confirming `stateNodes.length === 0`.

## Root Cause Analysis

The data pipeline is:
```
invoke('pyramid_tree', { slug })
  → normalizeTreeData(raw)
  → flattenTree(treeData)
  → usePyramidLayout(flatNodes, width, height)
  → draw(stateNodes)
```

### What's confirmed working
- Rust `pyramid_tree` command returns correct `Vec<TreeNode>` (verified via curl)
- Canvas element exists at correct dimensions (debug border visible)
- `TreeNode` shape matches between Rust and TypeScript: `{ id: String, depth: i64, distilled: String, children: Vec<TreeNode> }`
- `flattenTree()` logic is correct for the expected shape
- `usePyramidLayout()` correctly guards on `width === 0 || height === 0`
- `draw()` function correctly renders nodes when `stateNodes.length > 0`
- DPI scaling in `useCanvasSetup` is correct (sets transform, returns CSS pixels)

### Where it breaks (ranked by likelihood)

**1. (MOST LIKELY) The `invoke` call fails silently.** The `.catch()` handler at line ~241 previously swallowed errors with no logging. I've added `console.error` — check console for `PyramidViz: pyramid_tree FAILED:`. Common Tauri IPC failures: command not registered, serialization error, database lock.

**2. `normalizeTreeData` discards valid data.** The function at line 99 handles `Array`, `{ roots: [...] }`, and single-object shapes. If Tauri wraps the response differently (e.g., `{ data: [...] }` or double-serializes), it falls through to `console.warn` and returns `[]`.

**3. Canvas dimensions are 0 on first meaningful render.** `usePyramidLayout` returns `{ nodes: [], edges: [] }` when `width === 0 || height === 0`. If the canvas container hasn't been measured by the ResizeObserver when `treeData` first populates, layout produces nothing. Subsequent re-renders should fix this, but if `width`/`height` state updates don't trigger a re-computation of the layout memo, nodes stay empty.

## Diagnostics Already Added

I've added `console.log` at every pipeline stage in `PyramidVisualization.tsx`:

```
PyramidViz: invoking pyramid_tree for slug='...'
PyramidViz: pyramid_tree raw response: {...}        ← or FAILED: ...
PyramidViz: normalized N root(s)
PyramidViz flatten: treeData.length=N, flatNodes=M
PyramidViz layout: flatNodes=M, layoutNodes=L, edges=E, canvas=WxH
PyramidViz: canvas=WxH, nodes=L, edges=E            ← existing log in draw()
```

**Run `cargo tauri dev` and open WebView devtools (Cmd+Option+I) to see these logs.** The first log line that shows `0` or `FAILED` is the bug.

## Key Files

| File | What it does |
|------|-------------|
| `src/components/PyramidVisualization.tsx` | Main component (~700 lines). Invoke, normalize, flatten, draw. **Already has diagnostics added.** |
| `src/components/pyramid-viz/useCanvasSetup.ts` | DPI/resize hook. Sets `canvas.width = w * dpr`, applies `ctx.setTransform(dpr, ...)`. Returns CSS pixel dimensions. |
| `src/components/pyramid-viz/usePyramidLayout.ts` | Layout algorithm. Groups by depth, positions in trapezoid bands. Returns `[]` if `width=0 || height=0`. |
| `src/components/pyramid-viz/types.ts` | `TreeNode`, `FlatNode`, `LayoutNode`, `LayoutEdge`, `NodeState` types. |
| `src/components/DADBEARPanel.tsx` | Parent component. Passes `slug`, `staleLog`, `status` as props. |
| `src-tauri/src/pyramid/types.rs:100-106` | Rust `TreeNode` struct definition. |
| `src-tauri/src/pyramid/query.rs:145-196` | Rust `get_tree()` — loads all nodes, builds tree from apex down. |
| `src-tauri/src/main.rs:2158-2164` | Tauri command handler — delegates to `get_tree()`. |

## Fix Strategy

### If invoke fails (scenario 1)
Check if `pyramid_tree` is registered in the Tauri command list at `src-tauri/src/main.rs:4032`. Verify the slug `agent-wire-nodepostdadbear` exists in the DB. Test with: `cargo run -- pyramid_tree agent-wire-nodepostdadbear` or equivalent.

### If normalize discards data (scenario 2)
Log the raw response shape. If Tauri wraps it (e.g., `Ok(data)` becomes `{ status: "ok", data: [...] }`), add that case to `normalizeTreeData`.

### If canvas=0x0 (scenario 3)
The layout memo depends on `[flatNodes, width, height]`. If `width`/`height` update BEFORE `flatNodes` populates, layout computes with empty nodes. If they update AFTER, layout should recompute. But if the `useMemo` for `flatNodes` and the layout `useMemo` resolve in the same render cycle where `width` is still 0, layout returns empty. Fix: add a `useEffect` that forces a re-render after both `flatNodes.length > 0` and `width > 0`.

## Pyramid Slug
```
agent-wire-nodepostdadbear
```

## Verification
After fixing, the DADBEAR panel should show ~124 nodes arranged in a pyramid shape (1 apex at depth 5, branching down to ~100+ L0 nodes at the base). Nodes should be cyan circles with hover tooltips and click popovers.
