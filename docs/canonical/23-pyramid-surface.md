# Pyramid Surface and node inspector

The Pyramid Surface is the visualization at the heart of Wire Node. Every built pyramid can be opened as a surface — a spatial rendering of the nodes, edges, and the ongoing build activity. The surface is where you get a sense of the shape of the pyramid; the node inspector is where you pull any specific node apart.

This doc covers both.

---

## Opening a surface

Three ways:

1. **From the Dashboard**, click a pyramid row, then click **Open surface** in the detail drawer.
2. **From the Grid tab**, click any pyramid card.
3. **During a build**, the surface opens automatically. If you closed it, click the running build in the Builds tab to reopen.

Surfaces open in a dedicated window (separate from the main app window). You can have multiple surface windows open at once — useful for comparing two pyramids side by side.

---

## The layout

The surface window shows:

- **Main visualization area** (most of the window) — the pyramid itself, rendered as nodes and edges.
- **Chronicle** (collapsible right panel) — live event stream of build and DADBEAR activity for this pyramid.
- **Minimap** (bottom-right corner) — a zoomed-out reference with a rectangle showing your current viewport.
- **Event ticker** (bottom edge) — the last few notable events, scrolling.
- **Overlay controls** (top toolbar) — toggles for which overlays render.
- **Layout toggle** (top toolbar) — pyramid / grid / density layout.

You can pan with click-drag and zoom with the mouse wheel or pinch gesture. The minimap lets you jump around fast.

## Nodes

Each node is a dot or circle. Properties:

- **Color** — by default, layer depth (L0 a muted blue, L1 darker, L2 darker still, apex brightest). You can switch this via overlays.
- **Size** — by node importance (inbound edges, evidence weight).
- **Position** — layered vertically, with children below their parents. `pyramid` layout preserves the tree shape; `density` layout packs more nodes and is better for very large pyramids; `grid` layout is a pure grid, useful for scanning.

Hovering a node shows a tooltip: headline, depth, link count. Clicking opens the **node inspector**.

## Edges

Edges connect nodes. By default you only see **structural edges** (parent-to-child). Toggle overlays to see:

- **Web edges** — cross-cutting connections between siblings (shared systems, shared decisions). These are the lateral relationships that make the pyramid a DAG rather than a tree.
- **Evidence edges** — KEEP/DISCONNECT links at specific weights. DISCONNECT edges appear as thin gray lines (these are noise that got filtered); KEEP edges appear thicker, with weight encoded in width.
- **Staleness edges** — red edges between stale nodes and their propagation targets. Should be empty on a fresh build; populated during DADBEAR activity.

## Overlays

The overlay toggles in the top toolbar control what's visible:

| Overlay | What it shows | When to use |
|---|---|---|
| **Structure** | Parent-child edges. | Always on by default. |
| **Web edges** | Sibling-to-sibling lateral connections. | When you want to see cross-cutting themes. |
| **Staleness** | Nodes currently flagged as stale. | When monitoring DADBEAR activity. |
| **Provenance** | Each node colored by which build created it. | When looking at evolution over time. |
| **Build** | During a live build, highlights the phase each node came from. | During a running build. |
| **Weight intensity** | Edge thickness encodes evidence weight. | When you want to see which evidence is central. |

Overlays stack — you can have several on at once. Popular combo for a first look: Structure + Weight intensity.

## Layout modes

- **Pyramid** (default) — classic tree-ish layout, apex at top, L0 at bottom. Best for small pyramids (under a few hundred nodes).
- **Grid** — nodes packed into a regular grid, colored by depth. Best for pyramids where you want a scannable overview rather than a topology view.
- **Density** — a density-aware packing that shows hot regions. Best for very large pyramids where topology is overwhelming.

Switch in the top toolbar. The layout takes effect immediately; your camera position is preserved.

---

## The node inspector

Clicking any node opens the **node inspector** as a modal overlay. The inspector has:

### Header

- **Headline** — the node's title.
- **Depth badge** — L0, L1, L2, apex.
- **Node ID** — copyable.

### Navigation controls

Arrow buttons (or arrow keys on the keyboard):

- **Left / right** — previous/next sibling at the same depth.
- **Up** — parent.
- **Down** — first child.

You can walk the whole pyramid this way without closing and reopening the inspector. Useful for scanning.

### Tabs

**Details**

- The node's **self_prompt** (what question this node answers).
- The **distilled** answer.
- The **topics** breakdown.
- Evidence links (KEEP / DISCONNECT / MISSING summary with counts and weights).
- Parent and children IDs (clickable).
- Inbound web edges (other nodes that connect laterally to this one).

**Prompt**

- The full prompt that was sent to the LLM to produce this node, including the resolved template slots. Useful for debugging why an answer came out the way it did, or for learning how the chain's prompts work.

**Response**

- The raw LLM response that produced this node, before post-processing. You can see the full extracted topics, any JSON structure, any reasoning the model did.

**Audit trail** (available for nodes that have been superseded or rerolled)

- The history of this node's lineage. Shows prior versions, when they were superseded, who triggered the supersession, and why. Walk history to see how answers evolved.

### Actions

- **Reroll** — re-generate this specific node. Opens a dialog where you can add a note to steer the regeneration (e.g. "focus on error handling"). Rerolls are tracked; you can compare old and new versions in the audit trail.
- **Annotate** — leave an annotation on this node. Annotations with question context create FAQ entries.
- **Copy link** — copy a URL that opens this specific node. Useful for referencing in conversations.
- **Open in agent** — emit a prompt that asks an agent to walk this node specifically.

Close the inspector with Escape or by clicking outside.

---

## The chronicle panel

Toggle the chronicle panel (right edge) to see a live stream of events for this pyramid:

- Build events (phase transitions, node creations, cache hits).
- DADBEAR events (mutations queued, staleness checks run, supersessions applied).
- Compute market events (if applicable — market calls invoked by this pyramid).
- Cost events (LLM call completed, cost attributed).

Each event has a timestamp, a type, and a short description. Click an event to jump to the affected node on the surface (for node-level events).

The chronicle is filterable — narrow to just DADBEAR events, or just one specific node's activity.

---

## Searching within a pyramid

Press `/` (or click the search input in the toolbar). Search matches against:

- Node headlines and distilled answers.
- Topic names.
- Annotations.
- FAQ entries attached to this pyramid.

Results appear as a list; clicking a result opens the node inspector on that node.

If FTS finds nothing, search has a `--semantic` fallback that rewrites your query via an LLM call and tries again. This costs one LLM call per fallback, so use it when plain search is coming up empty.

---

## Multiple windows

Each surface window is independent — you can:

- Have two pyramids open side by side.
- Have the same pyramid open twice (useful for watching a live build in one and inspecting specific nodes in the other).
- Close a window without affecting the pyramid or other windows.

Windows remember their last layout and camera position.

---

## Performance with large pyramids

Very large pyramids (tens of thousands of nodes) can be heavy to render. A few things to know:

- **Density layout** is much faster than pyramid layout for large counts.
- **Turn off overlays you aren't using.** Web edges and provenance are the most expensive.
- **Use the minimap** to navigate — panning with the mouse gets tedious at scale.
- **Search** is faster than visual scanning for finding a specific node.

If the surface stutters persistently, the pyramid may be large enough that the visualization isn't practical. Use `pyramid-cli` or the MCP server to query instead — those are not visual-size-bounded.

---

## Keyboard shortcuts

Inside a surface window:

- `/` — focus search.
- `esc` — close inspector or modal.
- Arrow keys (in inspector) — navigate siblings / parent / first child.
- `f` — fit whole pyramid to viewport.
- `g` — toggle grid layout.
- `d` — toggle density layout.
- `p` — toggle pyramid layout.
- `space` — pause/resume live build animation (during a running build).

Full shortcut reference in [`Z1-quick-reference.md`](Z1-quick-reference.md).

---

## Where to go next

- [`20-pyramids.md`](20-pyramids.md) — the mode that contains the surface.
- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — see DADBEAR activity that drives chronicle events.
- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — leave annotations from the inspector.
