# Handoff: Question Pyramid UI Surface — MPS Required

## The situation

Question pyramids are working. `core-selected-docs` has a 4-layer question-driven pyramid: L0:127 → L1:36 → L2:5 → L3:1 apex. The data is in SQLite. The API partially works (nodes are queryable by ID). But the UI can't display it — the pyramid visualization, apex lookup, and navigation all assume mechanical pipeline structure.

## What's broken

### 1. Apex endpoint doesn't find the apex

`GET /pyramid/core-selected-docs/apex` returns:
```json
{"error": "No valid apex for slug 'core-selected-docs': multiple nodes at every depth (max depth 0, 127 nodes)"}
```

The apex finder looks for a single node at the maximum depth using sequential IDs (L1-000, L2-000). Question pyramid nodes use UUID IDs (L3-522437ff-efd7-4cdc-9329-c28a58e9622c). The depth query or the "single node at max depth" logic is failing.

**Root cause likely in:** the apex query filters on `superseded_by IS NULL` but may also be filtering on node ID format, or the max-depth calculation is wrong when question pyramid nodes coexist with superseded mechanical nodes.

### 2. Pyramid visualization shows the wrong thing

The screenshot shows the apex tooltip rendering the raw enhanced question text (the long "What is the Wire platform..." string) rather than the apex node's `headline` or `distilled`. The visualization appears to show nodes but the tree structure isn't navigable.

### 3. Node IDs are UUIDs, not sequential

Question pyramid nodes use `L1-{uuid}`, `L2-{uuid}`, `L3-{uuid}` format. The UI, drill endpoints, and visualization may expect `L1-000`, `L2-001` sequential format. Anywhere that parses node IDs by splitting on `-` and expecting a numeric suffix will break.

### 4. Question context not surfaced

Each question pyramid node has a `self_prompt` field containing the question it answers. This is the most important navigational signal — "What are the core system-level architecture components?" tells the user exactly what this node is about. The UI doesn't display `self_prompt` anywhere.

### 5. Evidence links not navigable

The `pyramid_evidence` table has KEEP/DISCONNECT verdicts with weights and reasons connecting each answer node to its evidence sources. This is the drill-down path (click an L2 node → see its L1 evidence → see the L0 sources). The UI doesn't know about evidence links — it uses `children` arrays from mechanical builds.

## What the MPS looks like

The maximal potential solution is: **a question pyramid is navigable the same way a mechanical pyramid is, but with the question as the primary organizing signal instead of topic names.**

Specifically:

**Apex works:** `GET /apex` returns the L3 node regardless of ID format. The apex finder should look for the single node at the highest depth with `superseded_by IS NULL`, period. No assumptions about ID format.

**Drill works:** `GET /drill/{node_id}` returns the node + its children. For question pyramids, "children" means nodes from the layer below that have KEEP evidence links to this node. The evidence table IS the children relationship.

**Node display shows the question:** Every node rendered in the UI shows its `self_prompt` (the question it answers) as the primary label, with `headline` as secondary. Users navigate by question: "What is the credit economy?" → "How does the purchase mechanism work?" → source doc about credit purchases.

**Evidence is visible:** When drilling into a node, the user sees which source nodes were KEEP'd (and at what weight), which were DISCONNECT'd, and what's MISSING. This is the provenance chain — "I believe X because of these sources."

**The pyramid visualization renders the question tree:** The triangle/graph shows the question decomposition structure. Apex at top, branches fan out, leaves at the bottom touching L0. Each node labeled by its question.

## What this is NOT

- Not a new UI framework — it's making the existing pyramid visualization, drill, and apex routes work with question pyramid data
- Not a new data model — the data is already there (pyramid_nodes with self_prompt, pyramid_evidence with verdicts)
- Not a prompt change — this is pure Rust + frontend

## Files likely involved

**Rust (API):**
- `routes.rs` — apex endpoint logic, drill endpoint logic
- `db.rs` — node queries, evidence queries
- Wherever `children` is populated for drill responses — needs to use evidence links for question pyramid nodes

**Frontend (React):**
- Pyramid visualization component — render question tree structure
- Node detail/drill views — show `self_prompt`, evidence verdicts
- Apex display — use `headline`/`distilled` not the raw question text

## Test data

Slug `core-selected-docs` has a live question pyramid:
```
L0: 127 nodes (D-L0-000 through D-L0-126)
L1: 36 nodes (L1-{uuid} format, each answering a leaf question)
L2: 5 nodes (L2-{uuid}, each answering a branch question)
L3: 1 node (L3-522437ff-efd7-4cdc-9329-c28a58e9622c, the apex)
```

Evidence links in `pyramid_evidence` connect L1→L0, L2→L1, L3→L2 with KEEP/DISCONNECT verdicts.
