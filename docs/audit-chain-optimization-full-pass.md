# Pyramid Chain Optimization — Full Audit Pass
## Conductor Two-Stage Blind Audit Report

**Date:** 2026-03-25
**Slug audited:** `opt-025`
**Auditors:** informed-auditor-A, informed-auditor-B (Stage 1), discovery-auditor-C, discovery-auditor-D (Stage 2)
**Scope:** 8-part optimization pass of the Pyramid Build Pipeline (agent-wire-node)

---

## Executive Summary

All 8 proposed changes are **unimplemented** — the codebase is the pre-optimization baseline. The audit found **5 critical issues**, **26 major issues**, and **12 minor issues** across the proposed design, the existing engine, and infrastructure that will be affected by the changes.

The two most important findings from Stage 2 (not anticipated by Stage 1):

1. **Security:** `source_path` from the API is stored and used for filesystem traversal without validation — an authenticated caller can ingest arbitrary filesystem paths.
2. **Runtime panic:** Clustering distilled text is truncated at byte offset 500 with a raw byte slice — will panic on any non-ASCII content at that boundary.

Both stages independently flagged the FK-constraint teardown without a transaction in `cleanup_from_depth_sync`, the 120s fixed timeout in `call_model_with_usage`, and the single-slot `active_build` guard. These triple-verified findings should be treated as high-confidence.

**Recommendation:** Address all Criticals and the security findings before any implementation work begins. The transaction and indexing gaps are pre-existing bugs that the optimization pass will make significantly worse by touching those code paths more frequently.

---

## Implementer Checklist

### CRITICAL — Stop-ships (must fix before any new code lands)

| ID | Finding | Location |
|----|---------|----------|
| **CR-1** | Missing indexes on `pyramid_web_edges` for OR predicate — every drill/apex query will full-table-scan once web edges are surfaced | `db.rs` schema init block |
| **CR-2** | `ConnectedWebEdge` struct does not exist; `DrillResult` has no `web_edges` field — nothing from optimization pass 1 is implemented | `types.rs` |
| **CR-3** | `max_thread_size` field not in `ChainStep`; split logic not in executor — serde silently discards the YAML field, no splitting occurs | `chain_engine.rs`, `chain_executor.rs` |
| **CR-4** | Thread split partial-state corruption: retry re-issues clustering LLM (non-deterministic), new assignments corrupt `parent_id` wiring of already-persisted nodes | `chain_executor.rs` lines 3362, 3530 |
| **CR-5** | **SECURITY:** `source_path` from `POST /pyramid/slugs` stored verbatim and passed to `std::fs::read_dir` — authenticated caller can ingest any filesystem path (e.g. `~/.ssh`, `/etc`) | `routes.rs:67`, `ingest.rs:282` |

**CR-1 fix:** Add two indexes:
```sql
CREATE INDEX IF NOT EXISTS idx_web_edges_slug_a ON pyramid_web_edges(slug, thread_a_id);
CREATE INDEX IF NOT EXISTS idx_web_edges_slug_b ON pyramid_web_edges(slug, thread_b_id);
```
Or rewrite the lookup as a UNION to use the existing UNIQUE index prefix.

**CR-2 fix:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedWebEdge {
    pub opposite_node_id: String,
    pub opposite_headline: String,
    pub relationship: String,   // maps from pyramid_web_edges.relationship (NOT "reason" — spec is wrong)
    pub strength: f64,          // maps from pyramid_web_edges.relevance
}
```
Add `#[serde(default)] pub web_edges: Vec<ConnectedWebEdge>` to `DrillResult`. Use a single JOIN query — see CR-1 join template below.

**CR-3 fix:** Add `#[serde(default)] pub max_thread_size: Option<usize>` to `ChainStep`. Add post-processing in the executor after `thread_clustering` output is received: if `assignments.len() > max_thread_size.unwrap_or(12)`, split positionally. This must land before the `max_thread_size: 12` YAML key is added.

**CR-4 fix:** Persist the clustering LLM output as a pipeline step (step_type `cluster_assignment`) before beginning synthesis. On resume, load the saved assignment instead of re-calling the LLM. This makes cluster boundaries stable across retries.

**CR-5 fix:** At ingest time, validate that each resolved `source_path` is under an allowed base directory (configurable at startup or derived from `data_dir`). Reject with HTTP 400 for paths outside allowed roots before passing to `walk_dir`.

---

### MAJOR — Must fix before shipping

These are grouped by subsystem for easier implementation sequencing.

#### Database & Query Layer

| ID | Finding | Location | Dual? |
|----|---------|----------|-------|
| **M-01** | N+1 query for `ConnectedWebEdge.opposite_headline` — naive per-edge fetch causes up to 50 sequential DB lookups per drill/apex call | `query.rs`, proposed `drill()` extension | — |
| **M-02** | No transaction wrapping delete+insert in `persist_web_edges_for_depth` — crash between delete and first insert permanently loses web edges for that depth | `chain_executor.rs` lines 1228–1301 | ✓ (A+B) |
| **M-03** | `cleanup_from_depth_sync` disables FK constraints without a surrounding transaction — a panic between any of the 8 DELETE statements leaves FK enforcement permanently off on the shared writer connection | `chain_executor.rs:154` | ✓✓ (C+D) |
| **M-04** | `cleanup_from_depth_sync` nulls `parent_id` on lower nodes but does not clear stale `children` arrays on the base layer — `drill()` silently returns empty children after a layered rebuild | `chain_executor.rs:157`, `query.rs:265` | ✓ (C+D) |
| **M-05** | `cleanup_from_depth_sync` does not delete web edges for threads that were split — original thread_id survives, stale edges accumulate | `chain_executor.rs`, `db.rs` |  — |
| **M-06** | Test `test_delete_web_edges_for_depth_only_clears_target_layer` validates the **wrong** behavior of `delete_web_edges_for_depth` — any correct fix for cross-depth edge cleanup will break this test | `db.rs:2835` | — |
| **M-07** | Web step saves resume marker (`db::save_step`) **after** persisting edges — crash after persist but before save_step means the next run re-deletes and re-generates edges non-deterministically | `chain_executor.rs:3825` | — |

**M-01 fix:** Single SQL query joining `pyramid_threads → pyramid_web_edges → opposite pyramid_threads → pyramid_nodes` — do not loop over edges in Rust.

**M-02/M-03 fix:** Wrap the delete+insert sequence and the full `cleanup_from_depth_sync` body in explicit `BEGIN`/`COMMIT` transactions. Use the `migrate_slugs_check_constraint` pattern as the template for FK-off mutations.

**M-04 fix:** Add to `cleanup_from_depth_sync` before deleting upper nodes:
```sql
UPDATE pyramid_nodes SET children = '[]' WHERE slug = ?1 AND depth = ?2 - 1
```

**M-07 fix:** Save the step record (`db::save_step`) **before** calling `persist_web_edges_for_depth`. Add a repair path if the step record exists but edges are absent.

#### Read-Time API / Type Safety

| ID | Finding | Location |
|----|---------|----------|
| **M-08** | `DrillResult` mutation (adding `web_edges`) may break typed Tauri IPC and MCP clients using strict deserialization — audit `mcp-server/src/tools.ts` and any TypeScript Zod schemas before shipping | `types.rs:118`, `mcp-server/` |
| **M-09** | Reader lock (`state.reader.lock().await`) held for the entire duration of the web-edge join — serializes all concurrent API reads; adding the 3-table join increases hold time | `routes.rs:780–890` |
| **M-10** | Stale or zero web edges returned if an apex query arrives during the delete-then-insert window in `persist_web_edges_for_depth` — mid-build queries see empty edge set | `routes.rs:780`, `chain_executor.rs` |

**M-08 fix:** Use `#[serde(skip_serializing_if = "Vec::is_empty")]` on `web_edges` so the field is omitted when empty (additive-safe for lenient clients). Audit TypeScript consumers before shipping.

**M-09 fix:** Ensure the web-edge JOIN uses a single prepared statement with the indexes from CR-1. Consider a second reader connection in WAL mode for concurrent reads.

#### Engine & Executor Reliability

| ID | Finding | Location | Dual? |
|----|---------|----------|-------|
| **M-11** | UTF-8 panic: clustering step truncates `distilled` text at raw byte offset 500 — panics on non-ASCII content at the boundary | `chain_executor.rs:3338` | — |
| **M-12** | `validate_step_output` errors escape the retry loop immediately — a structurally-valid but semantically-empty LLM response (e.g. `{"threads": []}`) aborts or skips regardless of `on_error: retry(3)` | `chain_executor.rs:2124` | — |
| **M-13** | `load_nodes_for_webbing` polls with 5×100ms sleeps — under load the drain may lag >500ms, silently omitting nodes from webbing inputs | `chain_executor.rs:1043` | ✓ (C+D) |
| **M-14** | Partial cluster failure silently switches from semantic to positional grouping with no DB flag or log distinguishing the two paths — resulting nodes are structurally valid but semantically degraded | `chain_executor.rs:3415` | — |
| **M-15** | No test coverage for the proposed `ConnectedWebEdge` query path — implementation review criteria explicitly require `test_` functions for the new mapping | `query.rs` (missing test) | — |
| **M-16** | Mixed `lock().await` / `blocking_lock()` on shared reader mutex — latent Tokio thread-pool starvation risk under concurrent load when web edge reads are added to the `for_each` loop | `chain_executor.rs` | — |

**M-11 fix:** `n.distilled.chars().take(500).collect::<String>()` — use char boundary, not byte offset.

**M-12 fix:** Move `validate_step_output` inside the retry loop within `dispatch_with_retry` so semantic validation failures consume the retry budget.

**M-13 fix:** Replace the polling loop with an explicit drain barrier (await the writer handle or send a `WriteOp::Flush` sentinel) before the web step reads nodes.

**M-14 fix:** Save the cluster assignment JSON as a `cluster_assignment` pipeline step before synthesis. Log a distinguishable event when positional fallback activates.

#### LLM / Prompt Pipeline

| ID | Finding | Location |
|----|---------|----------|
| **M-17** | `call_model_with_usage` hardcodes 120s timeout; `call_model` dynamically scales to 600s — stale-check batches systematically time out on large inputs | `llm.rs:276` | ✓ (C+D) |
| **M-18** | `code_extract_frontend.md` does not exist; no YAML routing mechanism for extension-based dispatch — recommend `instruction_map` field in ChainStep (Option A) or inline conditional in `code_extract.md` (Option B) | `chain_engine.rs`, `chains/prompts/code/` | — |
| **M-19** | L0 webbing step missing from `code.yaml` entirely | `chains/defaults/code.yaml` | — |
| **M-20** | L0 webbing 100+ node payload will use qwen (1M ctx) due to size — `compact_inputs` mode must be specified explicitly in YAML; `model_tier: mid` would route to mercury-2 and fail | `chain_executor.rs`, `code.yaml` | — |
| **M-21** | Web edge injection into `thread_narrative` risks token clipping on mercury-2 — cap injected context to top 5–10 edges by strength, `strength + opposite_headline` only, never raw relationship text | `chains/prompts/code/code_thread.md` | — |
| **M-22** | `code_cluster.md` thread count range conflict: line 4 says "8–14 threads," line 13 says "10–18 threads" — model sees conflicting constraints | `chains/prompts/code/code_cluster.md:4,13` | — |
| **M-23** | Semantic split prompt risks file ID hallucination — must cross-validate every output `source_node` against DB before accepting | `chain_executor.rs` (proposed split logic) | — |
| **M-24** | Human-authored annotations permanently deleted by `cleanup_from_depth_sync` during layered rebuild; FAQ provenance broken (FAQ nodes reference deleted annotation IDs) | `chain_executor.rs:162`, `db.rs` | — |

**M-17 fix:** Extract a shared `compute_timeout(prompt_len: usize) -> Duration` helper used by both `call_model_inner` and `call_model_with_usage`.

**M-18 recommended path (Option A):** Add `instruction_map: Option<HashMap<String, String>>` to `ChainStep`, resolve by `chunk.file_type` at dispatch time in `execute_for_each_work_item`.

**M-22 fix:** Pick one range. If max_thread_size is 12, the directive should be `ceiling(total_files / 10) ± 2` threads to stay self-consistent across codebase sizes.

**M-24 fix:** Before deleting annotations, set `node_id = NULL` (detach) for annotations where `author != 'system'`. Re-attach after rebuild by matching `node_id` pattern.

#### Security & Auth

| ID | Finding | Location |
|----|---------|----------|
| **M-25** | `handle_config` POST allows an authenticated caller to replace `auth_token` in-flight — a single compromised token can permanently lock the operator out; token replacement is persisted to disk | `routes.rs:1302` |
| **M-26** | `active_build` is a single global `Option<BuildHandle>` — blocks all builds for slug B when slug A is building; error message leaks the active slug name | `mod.rs:158`, `routes.rs:1024` | ✓ (C+D) |

**M-25 fix:** Remove `auth_token` from the API-accessible `ConfigBody`. Require auth token changes via config file or environment variable only.

**M-26 fix:** Change `active_build` to `Arc<Mutex<HashMap<String, BuildHandle>>>`, matching the `vine_builds` pattern. Remove slug name from the 409 error message.

---

### MINOR — Should fix (not blocking)

| ID | Finding | Location |
|----|---------|----------|
| **m-01** | `node_id_matches_depth` uses `contains("L0")` for depth 0 — false match for L10, L20, etc.; should be `starts_with("L0-")` | `chain_executor.rs:1383` |
| **m-02** | `annotate` HTTP route silently coerces unknown `annotation_type` to `Observation` — asymmetric with MCP layer which validates | `routes.rs:1438` |
| **m-03** | `get_apex` fallback scan re-prepares SQL on every loop iteration — prepare once before the loop | `query.rs:112` |
| **m-04** | `active_build` not set to `None` after completion — stale handle persists, confuses multi-slug status responses | `routes.rs:1046` |
| **m-05** | `accumulate` config silently ignored when `concurrency > 1` — `validate_chain` should emit an error, not silence | `chain_engine.rs:308` |
| **m-06** | `extract_json` heuristic picks inner nested object when model preamble contains a code block — strip content between triple-backtick pairs, not just fence lines | `llm.rs:372` |
| **m-07** | `decay_web_edges` applied unconditionally after every collapse — quiescent but valid edges decay to zero in ~20 cycles; add `last_confirmed_at` guard | `webbing.rs:266` |
| **m-08** | `delete_web_edges_for_depth` AND predicate — cross-depth edges never deleted; document the same-depth invariant explicitly | `db.rs:1377` |
| **m-09** | `execute_recursive_cluster` does not store apex_node_id in `ctx.step_outputs` — post-build steps can't reference `$upper_layer_synthesis` | `chain_executor.rs:1797` |
| **m-10** | MCP `pyramid_annotate` tool has no `node_id` format hint — hallucinated IDs produce silent 404; add regex pattern to Zod schema | MCP tool definition |
| **m-11** | `code_cluster.md` thread count upper bound is too low for 200+ file codebases — make dynamic: `ceiling(total_files / 10) ± 2` | `code_cluster.md:17` |
| **m-12** | `ConnectedWebEdge` maps `strength` from DB column `relevance` (not `strength`) — document this mapping explicitly to prevent implementer confusion | `types.rs` (proposed) |

---

## Spec Corrections Required Before Implementation

These are errors in the handoff documentation that will cause incorrect implementations if followed verbatim:

1. **`reason` vs `relationship`:** The audit handoff example states "DB stores it as `reason`. Needs mapping." The actual `pyramid_web_edges` schema uses `relationship`. No mapping is needed — `relationship → relationship` is identity. The `reason` column appears in `pyramid_stale_check_log` and `pyramid_connection_check_log`, not in `pyramid_web_edges`.

2. **`strength` vs `relevance`:** `ConnectedWebEdge.strength` maps from DB column `relevance`. Make sure implementers use `relevance` in SQL, not `strength`.

3. **`maxItems` schema enforcement:** The spec notes "Max 12 files per thread — hard limit" but `code.yaml` has no `maxItems` on assignments. Add `maxItems: 12` to the `assignments` array in the `thread_clustering` response schema as a complement to the Rust-level enforcement.

---

## Implementation Order Recommendation

The optimization pass changes are all additive, but several pre-existing bugs will be made significantly worse by touching the affected code paths. Sequence matters.

**Phase 0 — Pre-existing bug fixes (do first, independent of the optimization pass):**
1. CR-5: Security — validate source_path (no new deps)
2. M-11: UTF-8 panic in clustering — fix the byte slice
3. M-03: FK teardown transaction in cleanup_from_depth_sync
4. M-02: Transaction in persist_web_edges_for_depth
5. M-04: Clear stale children arrays in cleanup_from_depth_sync
6. M-17: Dynamic timeout in call_model_with_usage
7. M-12: validate_step_output inside retry loop
8. M-25: Remove auth_token from ConfigBody API
9. M-26: active_build → per-slug HashMap

**Phase 1 — Data structure foundations (must land before any query work):**
10. CR-1: Add indexes on pyramid_web_edges
11. CR-2: Define ConnectedWebEdge struct in types.rs
12. M-08: Audit TypeScript consumers, add #[serde(skip_serializing_if)] to DrillResult

**Phase 2 — Optimization pass implementation:**
13. CR-3: max_thread_size in ChainStep + split post-processing (Rust first, then YAML)
14. CR-4: Persist cluster_assignment pipeline step before synthesis
15. M-07: Save step record before persist_web_edges, not after
16. M-01: ConnectedWebEdge single-JOIN query in drill() and get_apex()
17. M-19/M-20: Add l0_webbing step to code.yaml with compact_inputs + explicit model
18. M-18: code_extract_frontend.md + routing mechanism
19. M-14: Save cluster assignment as a pipeline step; log positional fallback
20. M-21: Cap web edge injection in thread_narrative (top-N by strength only)
21. M-22: Resolve code_cluster.md thread count range conflict

**Phase 3 — Verification:**
22. M-15: Write test for ConnectedWebEdge drill() path
23. M-06: Update test_delete_web_edges_for_depth to assert correct behavior
24. Run dry-run on slug < 50 files at depth=0 rebuild — verify no thread-size panics

---

## Stage 2 Discovery — What Stage 1 Missed

Stage 2 auditors (no plan context) found the following classes of issue that Stage 1's framing did not surface:

| Category | Findings |
|----------|---------|
| **Security** | source_path filesystem traversal (CR-5), auth_token replacement via API (M-25) |
| **Runtime panic** | UTF-8 byte slice panic in clustering (M-11) |
| **Idempotency inversion** | Web step saves resume marker after DB effect — retry makes state worse (M-07) |
| **Silent mode degradation** | Cluster fallback to positional grouping leaves no DB trace (M-14) |
| **Provenance breakage** | FAQ nodes retain references to deleted annotations after rebuild (M-24) |
| **Async hazard** | validate_step_output errors escape retry loop (M-12) |
| **Test validates wrong behavior** | test_delete_web_edges_for_depth locks in the bug (M-06) |

This means the original plan framing had blind spots in security, data integrity edge cases, and idempotency. All Stage 2 findings should be treated as incremental to — not substitutes for — the Stage 1 findings.

---

## Pyramid Annotations

The following annotations were written to `opt-025` during this audit for future reference:

| Annotation ID | Node | Author | Summary |
|--------------|------|--------|---------|
| 170 | C-L0-098 | informed-auditor-B | ConnectedWebEdge not implemented; spec terminology mismatch (reason vs relationship) |
| 171 | C-L0-079 | informed-auditor-B | max_thread_size missing; safe defaulting with #[serde(default)] |
| 172 | C-L0-079 | informed-auditor-B | Thread split partial-state: re-issued clustering LLM corrupts parent_id on retry |
| 173 | C-L0-079 | informed-auditor-B | code_extract_frontend.md missing; YAML routing mechanism doesn't exist |
| (C/D annotations) | C-L0-084, C-L0-079, C-L0-102, L1-010 | informed-auditor-A | index gap, N+1, transaction gap, DrillResult type break |

---

*This document was produced by a four-agent blind audit. Stage 1 auditors had full plan context; Stage 2 auditors had only the system purpose statement. Dual-verified findings (independently found by two or more auditors) are marked ✓.*
