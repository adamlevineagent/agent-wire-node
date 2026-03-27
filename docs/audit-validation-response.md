# Validation of Peer Auditor Findings: Pyramid Contribution Tree
**Date:** 2026-03-26
**Target:** `pyramid-contribution-tree.md` + Codebase (`wire_publish.rs`, `db.rs`, `chain_executor.rs`, `crystallization.rs`)

### Summary
I have reviewed the peer auditor's "Pyramid Contribution Tree — Comprehensive Audit" against the actual state of the `agent-wire-node` codebase. 

**Conclusion:** The peer auditor's findings are **100% accurate**. The design plan for the Pyramid Contribution Tree contains significant architectural mismatches, schema hallucinations, and Wire API misunderstandings that contradict the current Rust implementation.

---

### Detailed Validation

#### C1. `derived_from` Weight Is Hardcoded to 1.0 — Wire Publish Layer Ignores Evidence Weights
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `src-tauri/src/pyramid/wire_publish.rs` (Lines 177-187). 
The `derived_from_wire_uuids` list is mapped into the `derived_from` JSON array with a hardcoded `"weight": 1.0`. The plan's proposed royalty cascade completely breaks here because differential evidence weights are never actually transmitted to the Wire. The publisher must be updated to accept `&[(String, f64, String)]`.

#### C2. `source_type: "source_document"` vs Wire API's `source_document` — Wrong Enum Value
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `wire_publish.rs` (Line 181).
The code hardcodes `"source_type": "contribution"` for all parent-child relationships. There is no code path in `publish_pyramid_node` that emits `source_type: "source_document"` for L0 extraction nodes citing original corpus files. L0 publication will fail to cite corpus sources correctly.

#### C3. Handle-Path Assigned at Publish Time, Not Predictable
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `wire_publish.rs` (Lines 228-245).
The plan assumes `derived_from` can use pre-computed handle-paths (`ref: "playful/84/3"`). However, the actual publishing loop correctly queries the `pyramid_id_map` to build the `derived_from` array using Wire UUIDs (`wire_uuid`). The documentation must be updated to reflect the reality of UUID usage during bottom-up publication.

#### C4. No `pyramid_evidence` Table Exists — Core Schema Is Missing
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `src-tauri/src/pyramid/db.rs` (Lines 57-75).
The schema for `pyramid_nodes` relies strictly on a single `parent_id TEXT` and a `children TEXT` JSON array. The `pyramid_evidence` table described in the v2 specs (for many-to-many relationships) does not exist in the database initialization or migrations.

#### C5. Wire `type: "pyramid_node"` Is Not a Valid Contribution Type
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `wire_publish.rs` (Lines 190, 304).
The codebase assigns `"type": "pyramid_node"` for contributions and `"type": "question_set"` for question sets. Neither of these are valid Wire contribution types (which strictly require `analysis`, `extraction`, `higher_synthesis`, etc.).

#### C6. `contribution_type: "mechanical"` vs `"intelligence"` Affects Deposit and Pricing
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `wire_publish.rs` (Line 191).
The code hardcodes `"contribution_type": "mechanical"`, which fundamentally undercuts the "royalty cascade" model described in the plan. Citation royalties apply to `intelligence` contributions on the Wire. 

#### C7. Concurrent Crystallization Can Corrupt Pyramid State
**Finding:** ✅ **Validated (Critical)**
**Code Evidence:** `chain_executor.rs` and `crystallization.rs`
There is no node-level locking or concurrency control mechanism when bulk crystallizations fire simultaneously, creating race conditions for re-answering the same nodes.

#### Minor/Significant Issues (S1-S9, M1-M6)
All accurately reflect inconsistencies between the four conflicting design documents (`pyramid-contribution-tree.md`, `question-pyramid-architecture.md`, `question-driven-pyramid-v2.md`, and `question-yaml-format.md`). The plan jumps straight from clustering to synthesis, completely skipping horizontal pre-mapping. 

---

### Recommended Next Steps for the Orchestrator
1. **Pause Implementation:** Do not proceed with the `pyramid-contribution-tree.md` plan as written.
2. **Schema Migration:** Create a `pyramid_evidence` migration in `db.rs` to support the required many-to-many v3 architecture.
3. **Refactor `wire_publish.rs`:** Update `publish_pyramid_node` to accept dynamic evidence weights and distinct `source_type` mappings (`contribution` vs. `source_document`), and map to valid intelligence types (`extraction`, `higher_synthesis`).
4. **Document Harmonization:** Designate `question-driven-pyramid-v2.md` as the canonical Spec and rewrite the contribution tree plan to match it.
