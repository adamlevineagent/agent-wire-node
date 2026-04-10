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

### 2026-04-10 — Pre-existing: release-mode chain bootstrap gap (conversation-episodic not embedded)

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace (pre-existing, NOT a Phase 0b regression)

**What hit friction:** `chain_loader::ensure_default_chains` (~lines 241-247) embeds only 5 fallback chain YAML files via `include_str!`: `conversation.yaml` (placeholder with `id: conversation-default`), `code.yaml`, `document.yaml`, `question.yaml`, `extract-only.yaml`. The bootstrap path is taken ONLY when `env!("CARGO_MANIFEST_DIR").join("../chains")` does not exist at runtime. Since `CARGO_MANIFEST_DIR` is resolved at compile time, the path only exists on the build machine's filesystem. The `default_chain_id_for_mode` now routes conversations to `conversation-episodic`, which is NOT in the embedded list and has no embedded fallback. `tauri.conf.json` `bundle` section has no `resources` array, so chains aren't packed into the `.app`.

**Impact:** If this app is ever shipped to a user whose filesystem doesn't have `/Users/adamlevine/AI Project Files/agent-wire-node/chains`, conversation (and code and doc) builds will fail with "chain not found in chains directory". For Adam's personal build-and-run workflow this is fine; for any distribution (dogfood, alpha, etc.) it's a ticking bomb.

**Status:** Not blocking Phase 0b (Adam's dev workflow works because the chains dir exists on disk). Needs a dedicated phase or workstream before any distribution milestone. Candidate approach: embed the full `chains/**/*.yaml` tree via `rust-embed` or a build script, OR add a Tauri `resources` entry to `tauri.conf.json` so the chains directory is packaged alongside the app.

**Lesson:** Any content-type routing change that adds a new chain ID must verify the chain is reachable in both dev-build and distribution scenarios.

---

### 2026-04-10 — Pre-existing: DADBEAR config CHECK constraint excludes `vine` but `main.rs:3249` tries to create one

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace (pre-existing, NOT a Phase 0b regression)

**What hit friction:** The `pyramid_dadbear_config` CHECK constraint at `db.rs:1085` allows only `('code', 'conversation', 'document')` for `content_type`. But `main.rs:3249` does `matches!(content_type, ContentType::Conversation | ContentType::Vine)` and attempts to save a `pyramid_dadbear_config` row with `content_type = "vine"`. Every vine-slug auto-DADBEAR attempt fails the CHECK silently (logged as a warning, not propagated). `fire_ingest_chain`'s `ContentType::Vine | ContentType::Question` guard arm would never actually be reachable in production because the config write upstream fails first.

**Impact:** No user-visible effect today because vine DADBEAR configs never get created. Future phases that want vine-level DADBEAR (e.g., Phase 17 recursive folder ingestion where a folder = a vine) need to either widen the CHECK constraint or change the main.rs:3249 match.

**Status:** Not blocking Phase 0b (vine DADBEAR was never functional). Fix when Phase 17 gets to vine folder ingestion — that phase will need to widen the CHECK anyway.

---

### 2026-04-10 — Latent in Phase 0b: multi-file batch chunk collision when `batch_size > 1`

**Phase / workstream:** Phase 0b wanderer flagged (NEW code path introduced by Phase 0b's claim-once batch dispatch)

**What hit friction:** Phase 0b's `fire_ingest_chain` clears chunks ONCE before iterating over claimed source files. For `batch_size = 1` (the default in `main.rs:3269`) this is correct — one file per dispatch, one `ingest_conversation` call, no collision. For `batch_size > 1`, the for-loop calls `ingest_conversation` N times; each call starts chunk_index at 0; the second file collides with the first file on the `UNIQUE(slug, chunk_index)` constraint. `ingest_conversation` does not take a chunk_offset parameter the way `ingest_code` effectively does.

**Impact:** Latent until a user manually sets `batch_size > 1` in their DADBEAR config. Default path is safe. The wanderer noted this exists in the existing wizard path too (`routes.rs:3435` / `main.rs:3901`) so it's not exclusively a Phase 0b regression — but Phase 0b's claim-once batch dispatch is the first code path that CAN exercise it via the batch_size config.

**Status:** Not blocking Phase 0b ship. Proper fix requires either:
- (a) Extending `ingest_conversation` to accept a `chunk_offset: i64` parameter and computing it from `db::count_chunks(conn, slug)` before each call, mirroring `ingest_code`'s implicit offset pattern
- (b) Keeping chunks cleared once and only allowing `batch_size = 1` for Pipeline B with a hard error otherwise
- (c) Refactoring `ingest_conversation` and `ingest_code` to share a common chunk-offset-aware primitive

Phase 17 (folder ingestion) is the right phase to resolve this because it introduces multi-file ingestion as a first-class concern. Until then, the default `batch_size = 1` keeps it dormant.

---

### 2026-04-10 — Observation: `fire_ingest_chain` writer drain task is dead code on the conversation path

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace

**What hit friction:** `fire_ingest_chain` (dadbear_extend.rs:641-695) spawns a full writer drain task covering every `WriteOp` variant. The implementer mirrored the canonical block at `main.rs:3585-3631` exactly. But that canonical block feeds the **legacy build path** — the conversation path goes through `run_build_from` → `run_decomposed_build` → `execute_chain_from`, and `execute_chain_from` spawns its OWN internal write drain via `spawn_write_drain(state.writer.clone())` at `chain_executor.rs:3949`. Every `WriteOp` actually produced by the chain goes through the internal drain; the drain task in `fire_ingest_chain` sits idle until `write_tx` is dropped.

**Impact:** No defect — all writes still happen correctly via the internal drain. But ~50 lines of well-commented code in `fire_ingest_chain` is dead on the conversation path. On a future legacy-build code path (if one ever routes through `fire_ingest_chain`), the drain would become live again. Not worth tearing out pre-ship.

**Status:** Informational. Not blocking. Could be cleaned up in a future refactoring phase if Pipeline B remains chain-only. Documented here so future readers don't chase the drain looking for why it appears unused.

---
