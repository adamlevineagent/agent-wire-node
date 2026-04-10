# Pyramid Folders + Model Routing — Friction Log

**Plan:** `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md`
**Implementation log:** `docs/plans/pyramid-folders-model-routing-implementation-log.md`

---

## Purpose

Running log of friction points encountered during implementation of this 17-phase initiative. Keep this updated live so that if we need to hard-reset a phase (or the whole cycle), the surprises, dead ends, and lessons are all in one place. This is scoped to THIS plan — cross-session friction lives in the `project_friction_log` memory.

Each entry should have:
- **Date**
- **Phase / workstream**
- **What hit friction** (2-3 sentences)
- **Root cause** (if known)
- **What we did about it** (correction, deviation protocol, workaround, etc.)
- **Lesson for future phases** (if any)

---

## Entries

### 2026-04-09 — Planning: original handoff attributed Pipeline A symptoms to Pipeline B

**Phase / workstream:** Phase 1 (pre-implementation review)

**What hit friction:** The original handoff's Phase 1 spec attributed three symptoms (duplicate WAL entries in `pyramid_pending_mutations`, stacked stale checks, 200→528 L0 blowup) to `dadbear_extend.rs` tick re-entrancy. An implementer review against the current tree caught that those symptoms live in a different pipeline (`stale_helpers_upper.rs::execute_supersession`, Phase 2's target), and that `dadbear_extend.rs`'s dispatch was stubbed at lines 401-408 awaiting WS-EM-CHAIN. Phase 1 couldn't fix the symptoms it claimed to fix.

**Root cause:** The original plan was written from pyramid drills and grep output, not from full reads of the functions being changed. The two-pipeline architecture ("A = maintenance, B = creation/extension") was not recognized as two-pipeline until full-file reads happened.

**What we did about it:** Planner issued addendum-01 documenting the two pipelines explicitly, expanding Phase 0 into 0a (clippy) + 0b (finish Pipeline B), correcting Phase 1's symptom attribution, adding a Phase 2 scope boundary (only `stale_helpers_upper.rs::execute_supersession` is rewritten, not the two wholesale-rebuild callers), and noting Phase 17's dependency on 0b. Specs and master plan updated in place (commit `adc664b` / `ce7b62b`).

**Lesson for future phases:** Read every function you plan to change in full before planning the change. Pyramid drills and grep output are fine for navigation, not for architectural decisions. The `feedback_read_canonical_in_full` memory has been updated to include codebase reads, not just canonical spec reads.

---

### 2026-04-09 — Phase 0b wanderer caught a chunk-collision bug on second dispatch

**Phase / workstream:** Phase 0b (wanderer pass on `fire_ingest_chain`)

**What hit friction:** The implementer and verifier both signed off on `fire_ingest_chain` with tests that only exercised the FIRST dispatch. The wanderer caught that `ingest_conversation` always inserts chunks with `chunk_index` starting at 0, and `pyramid_chunks` has a `UNIQUE(slug, chunk_index)` constraint (`db.rs:107`). On the SECOND dispatch for the same slug (e.g., the next file drop after the initial build populated chunks), the chunking step would fail with `UNIQUE constraint failed: pyramid_chunks.slug, pyramid_chunks.chunk_index`. The ingest record would be marked `failed` and the build would never fire. Pipeline B was effectively broken end-to-end on any slug that already had chunks from a prior build.

**Root cause:** The punch-list verification checked lock ordering, variant coverage, error propagation, and the `chain_executor.rs:3804` zero-chunks guard, but the success path of the existing test (`test_fire_ingest_chain_chunks_conversation_before_dispatch`) only calls `fire_ingest_chain` once. The equivalent wizard path at `routes.rs:3431` does an explicit `db::clear_chunks(&conn, slug)?` before re-ingesting for exactly this reason ("repeated ingest calls append duplicate copies of the same source files") — but that pattern did not get copied into `fire_ingest_chain`. The six-spec punch list had no "idempotency under re-dispatch" check, and the wizard's defensive clear wasn't mentioned in the phase-0b prompt or addendum.

**What we did about it:** Wanderer added `test_fire_ingest_chain_second_dispatch_collision_repro` which calls `fire_ingest_chain` twice in a row on the same slug+file, asserts the second call does not surface a `UNIQUE` constraint error. That test failed against the committed implementer code, confirming the bug. Fix: added `db::clear_chunks(&conn, slug_owned)?` inside the chunking `spawn_blocking` block, before the `for path_str in &paths_owned` loop, mirroring `routes.rs:3431`. All 11 dadbear_extend tests now pass. Committed on branch `phase-0b-pipeline-b-dispatch`.

**Lesson for future phases:** (1) When the wizard/legacy path has a defensive operation before re-ingest (clear_chunks, delete_existing, truncate, etc.), the equivalent DADBEAR Pipeline B path needs the same operation. The "canonical build dispatch block" at `main.rs:3566-3730` that Phase 0b was patterned after is a FIRST-time build code path — it does not exercise the second-dispatch lifecycle that Pipeline B specifically fires. (2) Wanderer-style "does this work end-to-end, including the second and third time" testing catches things that punch-list verification misses. (3) For any future Pipeline B work, the test skeleton should include N>=2 sequential dispatches, not just one. Schema UNIQUE constraints across `chunk_index`, `batch_id`, and similar file-lifecycle keys are the first thing to check.

---
