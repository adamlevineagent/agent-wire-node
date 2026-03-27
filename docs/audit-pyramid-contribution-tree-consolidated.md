# Consolidated Audit: Pyramid Contribution Tree Plan

> **Status:** CANONICAL — Two independent audits, cross-validated  
> **Date:** 2026-03-26  
> **Target:** [pyramid-contribution-tree.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/pyramid-contribution-tree.md)  
> **Supporting docs:** question-pyramid-architecture.md, question-driven-pyramid-v2.md, progressive-crystallization-v2.md, question-yaml-format.md  
> **Codebase verified:** wire_publish.rs, db.rs, crystallization.rs, chain_executor.rs, rotator-arm.ts (GNE)  
> **Directive:** DO NOT implement the current plan as-written. Use this audit to rewrite it.

---

## Reading Guide for the Planner

This document is organized into three sections:

1. **Architectural Misalignments** — the plan is structurally incompatible with the specs it claims to implement
2. **Wire API Faults** — the plan/code will produce rejected or broken Wire contributions
3. **Missing Infrastructure** — capabilities the plan assumes exist but don't

Each finding includes: the problem, code evidence, the canonical spec that defines the correct behavior, and a directive for the fix. The planner should treat each directive as a hard constraint on the rewritten plan.

---

## Section 1: Architectural Misalignments

These are design contradictions where the contribution tree plan diverges from the architectural specs it is supposed to implement.

### 1.1 Actions 5-6 Use Deprecated v2 Clustering Instead of Question Pre-Mapping

**The problem:** Action 5 (`generate-grouping-schema`) and Action 6 (`cluster-topics`) describe bottom-up topic clustering: "Groups L0 topics into threads" via a `clustering_prompt`. Action 10 repeats this pattern at each upper layer.

**The canonical spec:** [question-driven-pyramid-v2.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/question-driven-pyramid-v2.md) lines 149-199 explicitly replaces clustering with a three-step loop per layer:
- **Step A — Horizontal Pre-Mapping:** Single LLM call reads ALL questions at this layer + ALL nodes from below → produces candidate connections with initial weights (intentionally over-includes)
- **Step B — Vertical Answering:** Each question answered using its pre-mapped candidates → evidence confirmed/disconnected with weights and justifications
- **Step C — Post-Answering Reconciliation:** Collect gaps, orphans, weight maps

**Why it matters:** Clustering produces groups without justification ("the algorithm put them together"). Pre-mapping + answering produces justified, weighted evidence chains. The entire evidence model depends on this.

> [!CAUTION]
> **Directive:** Replace Actions 5-6 entirely. The new plan must implement Pre-Mapping → Vertical Answering → Reconciliation as the core build loop for every layer above L0. No clustering step should exist.

---

### 1.2 Crystallization Section Omits Belief Supersession Tracing (Dual-Channel Propagation)

**The problem:** The plan's crystallization section (lines 248-255) describes only weight-based upward propagation: "which L1 nodes cited the changed L0 with high weight?"

**The canonical spec:** [progressive-crystallization-v2.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/progressive-crystallization-v2.md) defines two distinct propagation channels:

| | Staleness | Supersession |
|---|----------|-------------|
| Signal | "Evidence changed" | "A specific belief is now false" |
| Propagation | Through evidence weights (attenuates) | Through belief dependency / entity matching (does NOT attenuate) |
| Threshold | Configurable (0.5 re-answer, 0.2 flag) | None — always mandatory |
| Dismissable | Yes | No |

**Why it matters:** An L2 node with staleness score 0.05 (normally ignored) that contains the claim "auth uses session-based validation" — where sessions are now replaced by JWT — will remain WRONG indefinitely. Only the belief trace catches this.

> [!CAUTION]
> **Directive:** The rewritten plan must implement both propagation channels. The crystallization procedure must include: (a) weight-based staleness scoring with configurable thresholds, AND (b) entity/claim-based belief trace that bypasses all thresholds when a specific claim is contradicted by evidence.

---

### 1.3 Web Edge Mutation Violates Wire Contribution Immutability

**The problem:** Action 9 (`web-layer`) runs AFTER Action 7 (`synthesize-threads`), which publishes L1 nodes to the Wire. Action 9 then mutates the published nodes' `structured_data.web_edges`. But Wire contributions are immutable once published.

**The publication sequence as written:**
1. Action 7: Synthesize + publish L1 nodes (get Wire UUIDs) ← *immutable after this*
2. Action 9: Generate web edges → write to L1 nodes' `structured_data` ← *can't modify published contributions*

**Why it matters:** Either web edges are silently lost (local-only, never on Wire), or every node gets superseded just to add web edges (doubles publication cost and clutters audit trail).

> [!IMPORTANT]
> **Directive:** Restructure the per-layer build loop so web edge generation completes BEFORE Wire publication. The correct order per layer is: Pre-Map → Answer → Reconcile → Web Edges → Publish. A single publication point per layer, after all enrichment is complete.

---

### 1.4 Orphan L0 Nodes Waste Wire Credits with No Recovery

**The problem:** Action 4 publishes ALL L0 nodes to the Wire (50 credits each). Action 8 (reconciliation) then identifies orphans — L0 nodes not claimed by any question. But those orphans are already published and credits are already spent.

**Scale:** A 400-file codebase where 50 files are irrelevant = 2,500 credits wasted on orphan contributions.

> **Directive:** L0 publication must be deferred until after reconciliation. Build loop: Extract (local) → Pre-Map → Answer → Reconcile → Publish only non-orphan L0s + their consuming L1s. Orphan L0s remain local workspace artifacts, not Wire contributions.

---

### 1.5 Cost Estimation Ignores L0 File Count

**The problem:** The permission manifest (line 28) estimates node count as `question tree depth × breadth`. But L0 node count = number of source files, which is independent of the question tree.

**Example:** Question tree has 12 leaf questions (depth 3 × breadth 4 = 12). But the folder contains 350 files. L0 alone produces 350 nodes. Total is ~400+, not 12.

> **Directive:** Cost estimation formula must be: `L0_count(= source_file_count) + Σ(upper_layer_nodes from question_tree) + web_edge_passes + reconciliation_overhead`. The folder map from Action 1 provides the L0 floor.

---

### 1.6 Five Documents Describe Three Incompatible Models

The plan references four supporting docs but is not consistent with any of them:

| Topic | contribution-tree (current) | question-driven-v2 (canonical) | yaml-format (legacy) |
|-------|:---:|:---:|:---:|
| Grouping | LLM clustering | Pre-mapping + answering | LLM clustering |
| Evidence | `derived_from` | Evidence table w/ KEEP/DISCONNECT | Implicit `children` |
| Schema generation | Dynamic (Actions 3,5) | Canonical schema from decomposition | Fixed per content type |
| Multi-parent | Via `derived_from` on Wire | Via `pyramid_evidence` table locally | Single `parent_id` |

> **Directive:** Designate [question-driven-pyramid-v2.md](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/docs/question-driven-pyramid-v2.md) as the canonical architecture. The contribution tree plan must implement its model faithfully. Update question-yaml-format.md to v3.1 that reflects the question-driven model. Deprecate the clustering vocabulary.

---

## Section 2: Wire API Faults

These are issues where the plan or existing code will produce Wire contributions that are rejected, broken, or economically wrong.

### 2.1 `derived_from` Weights Hardcoded to 1.0

**Code:** [wire_publish.rs:177-187](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L177-L187)
```rust
"weight": 1.0,  // hardcoded — ignores evidence weights
```

**Impact:** Rotator arm allocates slots uniformly instead of by evidence weight. A source cited with weight 0.95 gets the same royalty share as one cited with weight 0.10.

> **Directive:** `publish_pyramid_node()` signature must change to accept `&[(String, f64, String)]` — (wire_uuid, weight, justification). Evidence weights from the pre-mapping/answering phase must flow through to `derived_from`.

---

### 2.2 `type: "pyramid_node"` Is Not a Valid Wire Contribution Type

**Code:** [wire_publish.rs:190](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L190)

**Valid Wire types:** `analysis`, `assessment`, `rebuttal`, `extraction`, `document_recon`, `higher_synthesis`, `corpus_recon`, `sequence`

> **Directive:** Map pyramid types to valid Wire types:
> - L0 extraction nodes → `extraction`
> - L1+ synthesis/answer nodes → `higher_synthesis`
> - Question sets → `corpus_recon`

---

### 2.3 All Nodes Published as `contribution_type: "mechanical"`

**Code:** [wire_publish.rs:191](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L191)

**Impact:** The royalty cascade model depends on citation royalties flowing through `derived_from`. If `mechanical` contributions don't participate in the rotator arm the same way as `intelligence` contributions, the entire economic model breaks.

> **Directive:** Verify Wire server behavior for `mechanical` vs `intelligence` royalty treatment. If `mechanical` contributions have reduced or no citation royalties, switch pyramid nodes to `intelligence`. L0 nodes (original extraction work) are arguably `intelligence`; upper-layer synthesis is definitely `intelligence`.

---

### 2.4 No `source_document` Citation Path for L0 Nodes

**Code:** [wire_publish.rs:181](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L181) — only sends `source_type: "contribution"`

**The problem:** L0 nodes cite source files (corpus documents), not other contributions. The `derived_from` should use `source_type: "source_document"` with a corpus document UUID. Currently, L0 nodes either have empty `derived_from` or incorrectly cite source files as contributions.

> **Directive:** L0 publication must: (1) resolve source file path → corpus document UUID via a lookup, (2) emit `source_type: "source_document"` with that UUID. This requires the corpus to be synced to the Wire first.

---

### 2.5 Handle-Paths Can't Be Pre-Referenced

**Design claim:** `{ ref: "playful/84/3" }` in `derived_from` examples  
**Reality:** Handle-paths are assigned by the Wire server at publish time. The code correctly uses Wire UUIDs.

> **Directive:** Remove all handle-path references from `derived_from` examples in the plan. Use Wire UUIDs. Handle-paths are for human navigation, not for programmatic inter-node references during publication.

---

### 2.6 No Publication Idempotency

**Code:** [wire_publish.rs:211-271](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/wire_publish.rs#L211-L271) — `publish_pyramid()` never checks `pyramid_id_map` before publishing.

**Impact:** If publication fails mid-pyramid (after some L0s published, before L1s), re-running creates duplicate L0 contributions on the Wire. No dedup.

> **Directive:** Before publishing each node, check `pyramid_id_map` for an existing wire_uuid. If found, skip (or supersede if content changed). Publication must be resumable.

---

## Section 3: Missing Infrastructure

### 3.1 Local Schema Missing 7 Tables

The plan assumes but doesn't specify migration for these tables (defined across supporting docs):

| Table | Source Doc | Purpose |
|-------|-----------|---------|
| `pyramid_evidence` | question-driven-v2 | Many-to-many weighted evidence links |
| `pyramid_question_tree` | question-driven-v2 | Question decomposition tree |
| `pyramid_gaps` | question-driven-v2 | Missing evidence flagged by answering |
| `pyramid_deltas` | crystallization-v2 | Change log per L0 node |
| `pyramid_supersessions` | crystallization-v2 | Belief correction audit trail |
| `pyramid_staleness_queue` | crystallization-v2 | Pending re-answer work items |
| `pyramid_crystallization_log` | crystallization-v2 | Completed re-answer audit |

**Current state:** `db.rs` has only `pyramid_nodes` (single `parent_id`) and `pyramid_web_edges`. The `pyramid_id_map` table exists in `wire_publish.rs`.

> **Directive:** The rewritten plan must include a schema migration section with: (1) CREATE TABLE statements for all 7 tables, (2) migration from `parent_id`/`children` to evidence table, (3) backfill strategy for existing pyramids, (4) version numbering.

---

### 3.2 No `converge` Block Specification for Recursive Upward Synthesis

Action 10 references a `converge` block with "compile-time expansion" but this is unspecified:
- Convergence condition (when to stop)
- Maximum iteration count
- Handling of layers that don't reduce node count
- The `converge_metadata` field on Step structs is unused

> **Directive:** Specify the upward recursion as an explicit loop with: (1) stop condition = single node at current layer, (2) max_iterations guard (default 10), (3) error if two consecutive iterations produce the same node count (infinite loop).

---

### 3.3 No Concurrency Control for Crystallization

[crystallization.rs](file:///Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/crystallization.rs) implements event-chain templates but has no node-level locking.

> **Directive:** Add either: (a) per-node mutex during re-synthesis, or (b) serialize all re-answers to the same node through a queue. The staleness queue table can serve as the serialization point if re-answers are dequeued sequentially per `question_id`.

---

## Canonical Build Loop (What the Rewritten Plan Must Implement)

Based on the validated specs, the correct build loop per layer is:

```
Phase 1: Architecture (top-down, no source material)
  Action 1: Characterize material (folder map + question) → user checkpoint
  Action 2: Decompose question recursively → question tree + canonical schema
  Action 3: Generate extraction schema from leaf questions

Phase 2: L0 Extraction
  Action 4: Extract per file (parallel) → L0 nodes (LOCAL ONLY, not published)

Phase 3: Bottom-Up Answering (repeat per layer, deepest first)
  Action 5: Horizontal Pre-Mapping (single LLM call, all questions × all nodes below)
  Action 6: Vertical Answering (parallel, one per question, with evidence KEEP/DISCONNECT)
  Action 7: Reconciliation (mechanical: orphans, gaps, weight maps)
  Action 8: Web Edge Generation (cross-references between siblings)
  Action 9: PUBLISH this layer to Wire (non-orphan nodes only, with evidence weights)

Phase 4: Apex
  Action 10: Synthesize apex from top-layer nodes using original question as prompt
  Action 11: Publish apex

Phase 5: Crystallization (on source change)
  Step 1: Delta extraction (classify ADDITION/MODIFICATION/SUPERSESSION)
  Step 2: Update L0 with supersession audit trail
  Step 3: Dual-channel trace (weight-based staleness + belief-based supersession)
  Step 4: Classify impact per affected node
  Step 5: Re-answer mandatory (supersession) nodes with correction directives
  Step 6: Cascade check (did re-answers produce new supersessions?)
  Step 7: Re-answer staleness-triggered nodes
  Step 8: Republish affected nodes (supersede old Wire contributions)
```

---

## Priority Order for the Planner

1. **Restructure the build loop** — Replace Actions 5-10 with the canonical Pre-Map → Answer → Reconcile → Web → Publish sequence
2. **Fix wire_publish.rs** — Dynamic weights, valid types, source_document citations, idempotency
3. **Design schema migration** — 7 new tables + evidence model migration
4. **Rewrite crystallization section** — Dual-channel propagation with belief trace
5. **Fix cost estimation** — Include L0 = file count in permission manifest
6. **Defer L0 publication** — Publish after reconciliation, not before
7. **Harmonize all 5 documents** — question-driven-pyramid-v2.md is canonical
