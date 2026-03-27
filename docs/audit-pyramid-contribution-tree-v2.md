# Consolidated Audit v2: Pyramid Contribution Tree Plan

> **Status:** CANONICAL — Two independent audits, cross-validated, reframed  
> **Date:** 2026-03-26  
> **Supersedes:** audit-pyramid-contribution-tree-consolidated.md  
> **Target:** [pyramid-contribution-tree.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/pyramid-contribution-tree.md)  
> **Supporting specs:** question-driven-pyramid-v2.md, progressive-crystallization-v2.md, question-pyramid-architecture.md, question-yaml-format.md  
> **Codebase verified:** wire_publish.rs, db.rs, crystallization.rs, chain_executor.rs, rotator-arm.ts (GNE)

---

## Framing

The pyramid builder is the **Wire's first real action-chain consumer**. It doesn't just conform to the Wire — it defines what the Wire needs to support. Findings are categorized into three buckets:

1. **Plan Fixes** — the plan contradicts its own canonical specs or has internal logic errors. Fix in the plan.
2. **Wire Enhancements** — the Wire doesn't yet support what the plan correctly requires. Add to the Wire.
3. **Code Fixes** — the existing publisher code doesn't implement what the plan or Wire already specify. Fix the code.

---

## Part 1: Plan Fixes

These are internal contradictions or architectural omissions within the plan itself, independent of the Wire's capabilities.

### PF-1. Actions 5-6 Use Deprecated v2 Clustering Instead of Question Pre-Mapping

Action 5 (`generate-grouping-schema`) and Action 6 (`cluster-topics`) describe bottom-up topic clustering — "Groups L0 topics into threads." This is the v2 model that [question-driven-pyramid-v2.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/question-driven-pyramid-v2.md) explicitly replaces.

The canonical spec defines a per-layer loop:
- **Step A — Horizontal Pre-Mapping:** Single LLM call maps candidate evidence to each question (over-includes)
- **Step B — Vertical Answering:** Each question answered against its candidates with KEEP/DISCONNECT/weight
- **Step C — Reconciliation:** Orphans, gaps, weight maps

> **Directive:** Replace Actions 5-6 entirely with Pre-Mapping → Vertical Answering → Reconciliation.

---

### PF-2. Crystallization Section Omits Belief Supersession Tracing

The plan's crystallization section (lines 248-255) describes only weight-based propagation. [progressive-crystallization-v2.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/progressive-crystallization-v2.md) defines dual-channel propagation:

| Channel | Signal | Attenuates? | Dismissable? |
|---------|--------|:-----------:|:------------:|
| Staleness | "Evidence changed" | Yes | Yes |
| Supersession | "This specific claim is now false" | No | No |

Without belief tracing, an L2 node with a false claim but low evidence weight stays wrong forever.

> **Directive:** Implement both propagation channels. Weight-based staleness with configurable thresholds AND entity/claim-based belief trace that bypasses all thresholds.

---

### PF-3. Orphan L0 Nodes Published Before Reconciliation

Action 4 publishes ALL L0 nodes to the Wire (50 credits each). Action 8 then identifies orphans. Credits already spent on nodes nobody will cite.

> **Directive:** Defer L0 publication until after reconciliation. Extract locally → Pre-Map → Answer → Reconcile → Publish only confirmed (non-orphan) nodes.

---

### PF-4. Cost Estimation Ignores L0 File Count

The permission manifest estimates `question tree depth × breadth = node_count`. But L0 = number of source files, which is independent of the question tree. 12 leaf questions × 350 files ≠ 12.

> **Directive:** Formula: `source_file_count + Σ(upper_layer_nodes) + overhead`. Folder map from Action 1 provides the L0 floor.

---

### PF-5. Web Edge Generation Runs After Layer Publication

Action 9 (web edges) runs after Action 7 publishes L1 nodes. You can't enrich a published contribution's `structured_data` without superseding it.

> **Directive (plan-side):** Restructure the per-layer loop so web edge generation completes BEFORE Wire publication. Per-layer order: Pre-Map → Answer → Reconcile → Web Edges → Publish.

> **Directive (Wire-side):** See WE-2 below — the Wire may also want to support lightweight annotation without full supersession.

---

### PF-6. `converge` Block Unspecified

Action 10's recursive upward synthesis references a `converge` block with no specification of: convergence condition, maximum iterations, or handling of non-reducing layers.

> **Directive:** Specify as explicit loop: stop when single node remains, max 10 iterations, error if consecutive iterations don't reduce count.

---

### PF-7. Five Documents Describe Three Incompatible Models

| Topic | contribution-tree | question-driven-v2 | yaml-format |
|-------|:-:|:-:|:-:|
| Grouping | LLM clustering | Pre-mapping + answering | LLM clustering |
| Evidence | `derived_from` | Evidence table KEEP/DISCONNECT | Implicit `children` |
| Schema | Dynamic gen | Canonical from decomposition | Fixed per type |

> **Directive:** Designate question-driven-pyramid-v2.md as canonical. Rewrite contribution tree plan and update yaml-format to match.

---

## Part 2: Wire Enhancements

The pyramid builder is the Wire's first multi-contribution action chain. These are capabilities the Wire should gain, surfaced by this build.

### WE-1. `pyramid_node` as a First-Class Contribution Type

The Wire's `type` enum currently allows: `analysis`, `assessment`, `rebuttal`, `extraction`, `document_recon`, `higher_synthesis`, `corpus_recon`, `sequence`. The plan uses `type: "pyramid_node"`.

Rather than cramming pyramid nodes into `extraction` or `higher_synthesis`, the Wire should support pyramid contributions natively. This is a real, distinct category of structured knowledge artifact.

> **Directive (Wire server):** Add `pyramid_node` to the contribution type enum. Consider also `question_set` for published question trees.
>
> **Scope:** Wire server `/api/v1/contribute` route validation, database enum, query filtering.

---

### WE-2. Contribution Annotation Without Full Supersession

Web edges (Action 9) enrich published contributions with cross-references. Currently this requires superseding the entire contribution. The Wire should support **additive structured metadata** — lightweight annotations that extend a contribution's `structured_data` without creating a new contribution.

Use case: After publishing L1-003, a web-edge pass discovers L1-003 connects to L1-007 via a shared database table. Both contributions should gain a `web_edges` entry without supersession.

> **Directive (Wire server):** Design a contribution annotation mechanism. Candidates:
> - `PATCH /api/v1/contributions/:id/structured_data` — additive-only merge
> - A separate `annotation` contribution type that attaches to a parent
> - An `enrichment` field on contributions that accepts post-publish additions
>
> **Scope:** New Wire API endpoint or contribution field. Immutability of the core `body` is preserved; only `structured_data` sub-fields are enrichable.

---

### WE-3. Batch Publication with Pre-Allocated Handle-Paths

The plan publishes contributions bottom-up: L0 first, then L1 citing L0 UUIDs. Currently, handle-paths are assigned by the Wire server per-contribution. For a 350-node pyramid, this means 350 sequential publishes.

The Wire should support batch contribution submission with pre-allocated or predictable handle-paths, enabling the pyramid builder to reference sibling nodes by handle-path during construction.

> **Directive (Wire server):** Design a batch publish endpoint:
> ```
> POST /api/v1/contribute/batch
> { contributions: [...], preserve_ordering: true }
> → { results: [{ id, handle_path }, ...] }
> ```
> Or a handle-path reservation mechanism:
> ```
> POST /api/v1/handle-paths/reserve
> { count: 350 }
> → { reserved: ["playful/91/1", ..., "playful/91/350"] }
> ```
>
> **Scope:** Wire server API, handle-path allocation logic.

---

### WE-4. `mechanical` Contribution Royalty Semantics

The pyramid builder publishes all nodes as `contribution_type: "mechanical"`. The economic model depends on citation royalties propagating through `derived_from` chains. The Wire needs clearly defined mechanics for how `mechanical` contributions participate in the rotator arm.

Options:
1. **Mechanical = full royalty participation** — same as intelligence, just flagged for provenance
2. **Mechanical = reduced share** — e.g., 30% creator instead of 60%
3. **Mechanical = no citation royalties** — only direct access revenue

The pyramid builder needs option 1 or 2. If the Wire currently implements option 3, it needs to change.

> **Directive (Wire server):** Clarify and document `mechanical` vs `intelligence` royalty treatment. If mechanical doesn't participate in the rotator arm cascade, either change the behavior or add a third type (`structured_intelligence`?) for machine-built knowledge artifacts that DO earn citation royalties.
>
> **Scope:** Wire server rotator-arm.ts, contribution route validation, documentation.

---

### WE-5. Corpus Document Citation as First-Class `derived_from` Path

L0 nodes cite source files from a corpus. The `derived_from` should use `source_type: "source_document"` with a corpus document UUID. The plan correctly specifies this, but the end-to-end path needs to work:

1. Corpus synced to Wire via `wire_sync`
2. Source files have stable document UUIDs
3. `derived_from` with `source_type: "source_document"` and document UUID is accepted
4. Rotator arm correctly routes royalties to corpus contributors

> **Directive (Wire server):** Verify end-to-end: corpus sync → document UUID stability → `derived_from` acceptance → rotator arm routing. If any step fails, fix it. This is the foundation of the pyramid's provenance chain.
>
> **Scope:** Wire sync, contribute route, rotator arm.

---

## Part 3: Code Fixes

Existing code that doesn't match what the plan and Wire already specify.

### CF-1. `derived_from` Weights Hardcoded to 1.0

**File:** [wire_publish.rs:177-187](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L177-L187)

```rust
"weight": 1.0,  // ← hardcoded, ignores evidence weights
```

The Wire already accepts weights. The code just doesn't send them.

> **Fix:** Change `publish_pyramid_node()` to accept `&[(String, f64, String)]` — (wire_uuid, weight, justification). Pass actual evidence weights from the answering phase.

---

### CF-2. No `source_document` Citation for L0 Nodes

**File:** [wire_publish.rs:181](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L181) — hardcodes `source_type: "contribution"` for everything.

> **Fix:** L0 publication must emit `source_type: "source_document"` with corpus document UUID. Requires a corpus→document UUID lookup function.

---

### CF-3. No Publication Idempotency

**File:** [wire_publish.rs:211-271](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L211-L271) — `publish_pyramid()` never checks `pyramid_id_map` before publishing.

If publication fails mid-pyramid and is re-run, it creates duplicate contributions.

> **Fix:** Check `pyramid_id_map` for existing wire_uuid before each publish. Skip if already published (or supersede if content changed). Publication must be resumable.

---

### CF-4. No Concurrency Control for Crystallization

**File:** [crystallization.rs](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/crystallization.rs) — event-chain templates have no node-level locking.

Two deltas targeting the same node → concurrent re-synthesis → last writer wins, one correction dropped.

> **Fix:** Per-node mutex during re-synthesis, or serialize re-answers through the staleness queue by `question_id`.

---

## Part 4: Local Schema Requirements

Seven tables defined across supporting specs, none exist in `db.rs`:

| Table | Purpose | Defined in |
|-------|---------|-----------|
| `pyramid_evidence` | Many-to-many weighted evidence links | question-driven-v2 |
| `pyramid_question_tree` | Question decomposition tree | question-driven-v2 |
| `pyramid_gaps` | Missing evidence from answering | question-driven-v2 |
| `pyramid_deltas` | Per-L0 change log | crystallization-v2 |
| `pyramid_supersessions` | Belief correction audit trail | crystallization-v2 |
| `pyramid_staleness_queue` | Pending re-answer work items | crystallization-v2 |
| `pyramid_crystallization_log` | Completed re-answer audit | crystallization-v2 |

Current schema: `pyramid_nodes` (single `parent_id`, `children` JSON array), `pyramid_web_edges`, `pyramid_id_map`.

> **Directive:** Migration plan must include: CREATE statements, backfill from `children` → evidence table, version numbering, backward compatibility for existing pyramids.

---

## Canonical Build Loop

```
Phase 1: Architecture (top-down, no source material)
  1. Characterize material (folder map + question) → user checkpoint
  2. Decompose question recursively → question tree + canonical schema
  3. Generate extraction schema from leaf questions

Phase 2: L0 Extraction (local only)
  4. Extract per file (parallel) → L0 nodes saved LOCALLY

Phase 3: Bottom-Up Answering (per layer, deepest first)
  5. Horizontal Pre-Mapping (single LLM, all questions × all nodes below)
  6. Vertical Answering (parallel per question, KEEP/DISCONNECT with weights)
  7. Reconciliation (mechanical: orphans, gaps, weight maps)
  8. Web Edges (cross-references between siblings at this layer)
  9. PUBLISH confirmed nodes to Wire (with evidence weights, correct types)

Phase 4: Apex
  10. Synthesize apex from top-layer nodes
  11. Publish apex

Phase 5: Crystallization (on source change)
  12. Delta extraction (ADDITION / MODIFICATION / SUPERSESSION)
  13. Update L0 with supersession audit trail
  14. Dual-channel trace:
      a. Weight-based staleness (attenuates, configurable threshold)
      b. Belief-based supersession (does NOT attenuate, mandatory)
  15. Re-answer affected nodes with correction directives
  16. Cascade check → repeat until resolved
  17. Republish (supersede old Wire contributions)
```

---

## Priority Order for the Planner

| # | Item | Type | Why first |
|---|------|------|-----------|
| 1 | Restructure build loop (PF-1, PF-3, PF-5) | Plan | Everything downstream depends on the correct action sequence |
| 2 | Fix wire_publish.rs (CF-1, CF-2, CF-3) | Code | Unblocks any Wire publication testing |
| 3 | Clarify mechanical royalty semantics (WE-4) | Wire | Economic model depends on the answer |
| 4 | Add `pyramid_node` type to Wire (WE-1) | Wire | Needed before first real publication |
| 5 | Design schema migration (Part 4) | Code | Evidence table is prerequisite for pre-mapping |
| 6 | Rewrite crystallization section (PF-2) | Plan | Dual-channel propagation |
| 7 | Design annotation mechanism (WE-2) | Wire | Web edges without supersession |
| 8 | Batch publish / handle-path reservation (WE-3) | Wire | Performance optimization, can ship later |
| 9 | Verify corpus citation e2e (WE-5) | Wire | L0 provenance chain |
| 10 | Harmonize all docs (PF-7) | Plan | Cleanup, can happen in parallel |
