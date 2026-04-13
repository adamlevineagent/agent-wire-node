# Pyramid Surface — Visual Encoding System

**Date:** 2026-04-13
**Parent plan:** `pyramid-surface.md`
**Scope:** Defines the three-axis node encoding, link importance propagation, and aggregation rules for the Pyramid Surface renderer.

---

## Three-Axis Node Encoding

Every node in the pyramid carries three independent visual signals, each encoding a different dimension of importance. All three are continuous (power curve ramp), not categorical.

### Axis 1: Brightness — "How much am I cited?"

**Data source:** Aggregate KEEP weight from the reconciliation weight map. Sum of all KEEP verdict weights where this node is the source.

**What it answers:** How much does the pyramid's own reasoning rely on this node?

**Visual:** Node fill luminance. Dim = barely cited. Bright = heavily cited. Power curve ramp — most visual range in the top quartile, bottom 75% has subtle gradient.

**Example:** An L0 node cited by 4 questions with weights [0.9, 0.7, 0.6, 0.3] has aggregate weight 2.5. An L0 node cited once with weight 0.2 has aggregate 0.2. The first is dramatically brighter.

### Axis 2: Color Saturation — "How much importance flows through me?"

**Data source:** Propagated importance from upstream nodes. Computed by walking DOWN from the apex through KEEP evidence links, attenuating by link weight at each hop.

**What it answers:** How close am I to the things that matter most? A node might only be cited once, but if the node citing it is the apex, that one citation carries enormous weight.

**Visual:** Color saturation. Desaturated = peripheral, far from high-importance nodes. Vivid = on the critical path from apex to source material. Power curve ramp.

**Propagation formula:**
```
node.propagated_importance = sum(
    upstream_link.weight × upstream_node.propagated_importance
    for each incoming KEEP evidence link
)
apex.propagated_importance = 1.0
```

**Example:** A node with one KEEP link (weight 0.8) from a node with propagated importance 0.9 gets: 0.8 × 0.9 = 0.72. A node with one KEEP link (weight 0.8) from a node with propagated importance 0.3 gets: 0.8 × 0.3 = 0.24. Same raw link weight, very different saturation.

### Axis 3: Border Thickness (Inward) — "How connected am I to peers?"

**Data source:** Web edge count — number of same-layer relationship edges this node participates in.

**What it answers:** How much does this node knit the knowledge together laterally? A web hub that connects many sibling concepts is structurally important even if it's not heavily cited as evidence.

**Visual:** Border thickness, growing inward (node outer diameter stays constant). No border = isolated node. Thick border = connective hub. Power curve ramp.

**Why inward:** Node size stays constant across the layer, preserving the pyramid shape and spatial layout. The border consumes internal space rather than expanding outward.

### Three Axes Are Independent

A node can be any combination:

| Bright | Vivid | Thick Border | Meaning |
|--------|-------|-------------|---------|
| Yes | Yes | Yes | Central, on critical path, and a web hub. The most important nodes in the pyramid. |
| Yes | No | No | Heavily cited directly but by peripheral questions. An island of importance. |
| No | Yes | No | Not cited much directly, but what cites it is important. A quiet node on a critical path. |
| No | No | Yes | Not cited, not on a critical path, but connects many peers. A bridge node. |
| No | No | No | Peripheral. Exists in the pyramid but doesn't contribute much to the current structure. |

---

## Link Importance Propagation

Evidence links (KEEP verdicts) don't just have their own weight — they carry the importance of the upstream node.

### Visual Intensity of Links

```
link_visual_intensity = link_weight × upstream_node.propagated_importance
```

A KEEP link with weight 0.8 from a central, high-importance node renders as a thick bright line. The same weight 0.8 from a peripheral node renders as a thin faint line.

### Propagation Example

```
Apex (propagated: 1.0)
  │
  ├─ KEEP 0.9 ──→ L2-001 (propagated: 0.9)
  │   link intensity: 0.9 × 1.0 = 0.9          ← thick, bright
  │    │
  │    ├─ KEEP 0.8 ──→ L1-007 (propagated: 0.72)
  │    │   link intensity: 0.8 × 0.9 = 0.72     ← thick, bright
  │    │
  │    └─ KEEP 0.3 ──→ L1-012 (propagated: 0.27)
  │        link intensity: 0.3 × 0.9 = 0.27     ← medium, dimmer
  │
  └─ KEEP 0.3 ──→ L2-003 (propagated: 0.3)
      link intensity: 0.3 × 1.0 = 0.3           ← thin, dim
       │
       └─ KEEP 0.8 ──→ L1-042 (propagated: 0.24)
           link intensity: 0.8 × 0.3 = 0.24     ← thin despite high raw weight
```

L1-007 and L1-042 both have raw KEEP weight 0.8 from their parent. But L1-007's link is 3× more prominent because it carries importance from a more central upstream node. The user sees "rivers of importance" flowing from the apex down through the critical evidence paths.

### Multiple Incoming Links

When a node receives KEEP links from multiple upstream nodes, each link renders with its own intensity. The node's saturation (Axis 2) is the aggregate of all incoming propagated importance:

```
L1-007 receives:
  ├─ KEEP 0.8 from L2-001 (propagated 0.9) → link intensity 0.72
  └─ KEEP 0.4 from L2-002 (propagated 0.6) → link intensity 0.24

L1-007.propagated_importance = 0.72 + 0.24 = 0.96  → very vivid saturation
```

---

## Aggregation Rules

When individual links are too numerous to show distinctly, they aggregate into node effects.

### When to aggregate

- **Full view, zoomed in, below a per-node link threshold (tunable rendering heuristic, not hardcoded):** Show individual links with per-link intensity.
- **Full view, zoomed out, many links:** Aggregate into node saturation (Axis 2). Individual links visible on hover.
- **Miniature rendering (Grid View, Ticker, Minimap):** Always aggregated. No individual links. Node intensity carries the summary signal.
- **Relationship density view:** No explicit links by default. Proximity encodes relationship. Hover reveals individual connections.

### Web edge aggregation

Web edges (same-layer) aggregate into border thickness (Axis 3). Individual web edges are not drawn in the structural pyramid view — they'd create horizontal noise. They're visible in:
- The relationship density view (as proximity and optional faint lines for the strongest relationships — threshold tunable via viz config, not hardcoded)
- The node inspector (listed with relationship type and strength)
- On hover (highlight direct web neighbors)

### Evidence link aggregation

Evidence links (cross-layer KEEP/DISCONNECT) are drawn as individual lines when zoomed in. When zoomed out or in miniature, they aggregate into node brightness (Axis 1) and saturation (Axis 2).

DISCONNECT links are not drawn in the structural view (they represent rejected evidence — visual clutter). Available in the inspector and chronicle.

---

## Rendering Tier Adaptation

### Minimal (DOM/CSS)
- Brightness: CSS opacity on node element
- Saturation: CSS filter saturate() on node element
- Border: CSS border-width with box-sizing: border-box (inward growth)
- Links: Not rendered. Importance conveyed entirely through node effects.

### Standard (Canvas 2D)
- Brightness: Fill alpha + slight radius increase (1-2px max)
- Saturation: HSL color space manipulation on fill
- Border: strokeWidth on arc, drawn inside clipping region
- Links: Bezier curves with strokeStyle alpha proportional to link intensity
- Glow: ctx.shadowBlur on high-importance nodes

### Rich (WebGPU / WebGL2)
- Brightness: HDR bloom shader on node geometry
- Saturation: Fragment shader color grading
- Border: SDF (signed distance field) rendering for crisp inward borders at any zoom
- Links: Instanced line geometry with per-instance intensity attribute
- Glow: Post-processing bloom pass — central nodes literally radiate light
- Animation: Links pulse briefly when new evidence arrives during build

---

## Computation

### When to compute

- **During build:** Propagated importance recalculated each time a layer completes (new evidence links arrive). Renders update progressively — early layers are dim, importance "lights up" as higher layers cite them.
- **After build:** Computed once on load from persisted weight maps and evidence links. Static until next build or DADBEAR update.
- **During DADBEAR:** If a stale check rewrites a node, the propagated importance recalculates for the affected subgraph. The visual update propagates — if a central node gets rewritten, everything downstream shifts.

### Data sources (no new tables needed)

| Signal | Source | Already Persisted? |
|--------|--------|-------------------|
| Aggregate KEEP weight (Axis 1) | `pyramid_evidence` table, aggregate by source_node_id | Yes |
| Propagated importance (Axis 2) | Computed from evidence links + apex importance | Computed at render time |
| Web edge count (Axis 3) | `pyramid_web_edges` table, count by node_id | Yes |
| Per-link intensity | `pyramid_evidence.weight` × upstream propagated importance | Computed at render time |

### Algorithm

Propagation uses **BFS from apex(es) in reverse-depth order** (highest depth first, working down to L0). This handles the DAG structure correctly — evidence links in question pyramids create DAGs where a source node can be cited by multiple question nodes.

**Multi-apex pyramids:** When multiple questions are asked of the same source material, the pyramid has multiple apex nodes. Each apex starts at `propagated_importance = 1.0`. For nodes cited by multiple apex paths, propagated importance accumulates (can exceed 1.0). The renderer normalizes: clamp to [0, 1] after power curve, or use log-scale if the range is extreme. This means a node on the critical path of ALL questions in the pyramid will be maximally vivid.

**Topological ordering:** BFS from apexes ensures each node's propagated importance is fully computed before its children are processed. Standard reverse-depth BFS — O(nodes + edges), same complexity as rendering the tree.

### Performance

BFS propagation is O(nodes + edges) — same complexity as rendering the tree. Profile during implementation against real pyramid data to determine whether caching is needed. Do not assume timing — measure it.

---

## Relationship Density View Encoding

The relationship density view uses the same three-axis encoding but maps it to a different layout:

- **Node size** (replaces brightness): Larger = more cited. Central nodes are the biggest.
- **Color saturation**: Same as structural view — propagated importance.
- **Border thickness**: Same as structural view — web edge density.
- **Position**: Determined by relationship strength. Strongly related nodes are close, weakly related are far.
- **Labels**: Auto-appear above a size threshold (driven by centrality). Small nodes are unlabeled until hover or zoom.
- **Cluster proximity**: Nodes with many mutual web edges naturally cluster. The force simulation's attraction is proportional to relationship strength.

This means the same data drives both views — switching between structural pyramid and relationship density preserves the visual encoding, just changes the spatial layout.
