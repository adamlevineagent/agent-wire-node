# Workstream: Phase 0b — Finish Pipeline B (wire `fire_ingest_chain`)

## Who you are

You are an implementer joining an active 17-phase initiative to turn Wire Node into a Wire-native application. The initiative adds pyramid folders, per-step model routing, universal LLM output caching, full-pipeline observability, cost integrity, recursive folder ingestion, and YAML-driven generative configuration. Planning is done; implementation just started with a clippy cleanup (Phase 0a, already committed as `28fb3e5`). You are the implementer of Phase 0b.

Your task is to complete Pipeline B — the "creation/extension" half of DADBEAR — by wiring its stubbed chain dispatch to real chain builds via the existing build_runner.

## Context: the two pipelines

"DADBEAR" in this codebase is actually **two parallel pipelines** that collaborate:

| Pipeline | File | Responsibility | Current state |
|---|---|---|---|
| **A** | `watcher.rs` → `stale_engine.rs` → `stale_helpers_upper.rs` | **Maintenance** of files already tracked in `pyramid_file_hashes`. fs-notify events, writes `pyramid_pending_mutations`, polls + debounces, runs LLM-powered stale checks | Live, in active use, has its own guards (`start_timer` debounce at stale_engine.rs:328, `check_runaway` breaker at stale_engine.rs:612) |
| **B** | `dadbear_extend.rs` | **Creation / extension**. Periodic scanner → detects files NOT yet tracked → writes `pyramid_ingest_records` → `dispatch_pending_ingests` should run the content-type chain via a helper named `fire_ingest_chain` | **Dispatch is stubbed at lines 401-408.** Records marked "complete" with placeholder build_id of form `dadbear-ingest-<slug>-<uuid>`; no chain actually runs; `pyramid_ingest_records` is effectively a tracking log nothing downstream reads |

Pipeline B was shipped one day before the initiative plan was written (commit `b78169e`, 2026-04-08), with dispatch explicitly stubbed awaiting "WS-EM-CHAIN / WS-VINE-UNIFY that never landed." Everything else about Pipeline B is correct and live: tick loop, config management, ingest record lifecycle, session timeout detection, event emission, LockManager integration. **One function away from being live.**

The two pipelines are complementary. A file transitions from Pipeline B's domain to Pipeline A's domain the moment `fire_ingest_chain` completes and the file is recorded in `pyramid_file_hashes` (which the build's `WriteOp::UpdateFileHash` handler does as a side effect).

## Why Phase 0b exists, and what it unblocks

- **Phase 1** (DADBEAR in-flight lock) cannot be verified against the current tree — with dispatch stubbed, `dispatch_pending_ingests` is fast and never holds the tick loop long enough for re-entrancy to matter. After Phase 0b, the tick will actually run chain builds, re-entrancy races become real, and Phase 1's `Arc<AtomicBool>` becomes verifiable.
- **Phase 17** (recursive folder ingestion) implicitly assumed Pipeline B was live. Without 0b, creating a `pyramid_dadbear_config` row doesn't drive anything.
- **Conversation-focused DADBEAR** (today's primary use case) depends on Pipeline B to pick up new conversation session files dropped into a watched directory.

## Required reading (in order, in full unless noted)

**Read every function you plan to change in full.** Do not rely on grep snippets or drill summaries to make architectural decisions. Historical miss in this initiative: the planner wrote Phase 1 against grep output and attributed Pipeline A symptoms to Pipeline B. The addendum issued to correct that is your primary context — do not repeat the mistake.

### Handoff docs (read first)
1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — original handoff (deviation protocol, implementation log protocol, pace/quality framing)
2. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md` — Pipeline A vs B story, Phase 0 split into 0a/0b, Phase 1 correction, Phase 2 scope boundary. **Read this second — it corrects portions of (1).**

### Master plan
3. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — specifically the Phase 0b scope as updated by the addendum, and the parallelism map

### Spec
4. `docs/specs/evidence-triage-and-dadbear.md` Part 1 only (the corrected DADBEAR in-flight lock spec, for context on what Phase 1 will add on top of your work)

### Code reading — read in full unless marked "targeted"
5. `src-tauri/src/pyramid/dadbear_extend.rs` (779 lines) — the whole file. Lines 401-408 are the stub you replace. Pay attention to `run_tick_for_config` (line 153), `dispatch_pending_ingests` (line 357), and `start_dadbear_extend_loop` (line 65).
6. `src-tauri/src/pyramid/build_runner.rs` (1055 lines) — in full. `run_build_from` at line 171 and `run_build_from_with_evidence_mode` at line 191 are your main targets. Note the `LockManager::global().write(slug)` at line 208 — this matters for lock ordering.
7. `src-tauri/src/pyramid/chain_registry.rs` (111 lines) — short file, read in full.
8. `src-tauri/src/pyramid/lock_manager.rs` (623 lines) — read the doc comment (lines 1-95) and the public API (through line 298). Understand why `run_build_from` holding a single-slug write lock internally means your wrapper must NOT hold one across the `run_build_from` call (same-task re-acquisition of an exclusive write lock deadlocks).
9. `src-tauri/src/pyramid/ingest.rs` (1386 lines) — focus on `ingest_conversation` (line 350), `ingest_continuation` (line 384), `ingest_code` (line 436), `ingest_docs` (line 596), and the WS-INGEST-PRIMITIVE section starting at line 712. Understand that the chain path (`run_build_from` → `execute_chain_from`) REQUIRES `pyramid_chunks` to already be populated for non-question content types.
10. **Targeted** — `src-tauri/src/pyramid/chain_executor.rs` is 14,947 lines. Do NOT read in full. Read around `execute_chain_from` (starts at line 3782) and the chunk-count guard at line 3804 (`"No chunks found for slug '{}' — cannot run non-question pipeline with zero chunks"`). That's enough context about what the chain executor expects.
11. `src-tauri/src/pyramid/mod.rs` around line 739 — `PyramidState::with_build_reader()` creates a build-scoped state with an isolated reader connection. You will use this.
12. **Targeted** — `src-tauri/src/main.rs` is huge. Read:
    - Around line 3566-3730 — the canonical build dispatch block showing the `write_tx` / `progress_tx` / `layer_tx` channel setup + drain tasks + `run_build_from` call pattern. This is your template for the channel setup inside `fire_ingest_chain`.
    - Around line 3260-3296 — the post-build IPC handler that auto-creates a DADBEAR config and calls `start_dadbear_extend_loop` at line 3287. Call site #1 to update.
    - Around line 6625-6646 — the app-launch deferred spawn that calls `start_dadbear_extend_loop` at line 6638 if existing configs are found. Call site #2 to update.

## What to build

### 1. Thread `Arc<PyramidState>` into `start_dadbear_extend_loop`

Current signature:
```rust
pub fn start_dadbear_extend_loop(
    db_path: String,
    event_bus: Arc<BuildEventBus>,
) -> DadbearExtendHandle
```

New signature (add `state` as a parameter — keep `db_path` for short-lived DB operations that shouldn't contend on `state.reader`):
```rust
pub fn start_dadbear_extend_loop(
    state: Arc<PyramidState>,
    db_path: String,
    event_bus: Arc<BuildEventBus>,
) -> DadbearExtendHandle
```

Propagate `state` into the tick task so `run_tick_for_config` and `dispatch_pending_ingests` can access it. Update the two call sites in `main.rs` to pass `pyramid_state.clone()` / `ps.clone()` as appropriate for the surrounding context.

### 2. Write a new `fire_ingest_chain` helper

Per the addendum's Phase 0b spec requirements:

1. **Resolves the active chain definition for the ingest record's `content_type`.** Note: `run_build_from` handles `chain_registry::get_assignment` + `default_chain_id` fallback internally. You probably don't need to call `chain_registry` directly from `fire_ingest_chain` — confirm this in your reading and act accordingly.
2. **Constructs the call context with the new source file(s) as the ingest input.** For the chain path, this means ensuring `pyramid_chunks` is populated for the new files BEFORE calling `run_build_from`. The chunking primitives are in `ingest.rs`: `ingest_conversation(conn, slug, jsonl_path)` for jsonl (one file at a time — ideal for per-file dispatch), and `ingest_code` / `ingest_docs` for dirs (currently re-scan the whole dir; see scope note below).
3. **Calls into `build_runner::run_build_from()`** as the canonical entry. (Not `chain_executor::invoke_chain` — that's an in-chain child-invocation primitive, not an external entry point. Confirm by reading.)
4. **Captures the returned `build_id`** (first element of `run_build_from`'s `Result<(String, i32, Vec<StepActivity>)>`) and returns it so the ingest records can be marked complete with the real build_id.
5. **On chain failure, returns an `anyhow::Error`** that the caller (`dispatch_pending_ingests`) translates into `mark_ingest_failed` + `TaggedKind::IngestFailed` event emission. The existing failure code path already handles this — your helper just needs to return `Result<String>`.
6. **Holds LockManager write locks correctly.** `run_build_from` takes its own `LockManager::global().write(slug)` at `build_runner.rs:208`. Your helper MUST NOT be holding that lock when it calls `run_build_from`, or you deadlock on the same task. Keep short-lived write-lock scopes only for DB chunking writes.

**Additional architectural constraints:**

- Use `state.with_build_reader()` at the top of `fire_ingest_chain` to get a build-scoped `Arc<PyramidState>` with an isolated reader, matching the pattern in `main.rs:3566`. This prevents the build's reader from contending with CLI/frontend queries for the shared reader Mutex.
- Create ephemeral mpsc channels for `write_tx`, `progress_tx`, `layer_tx` inside `fire_ingest_chain`. Spawn a writer drain task (see `main.rs:3575-3638` for the canonical pattern that handles every `WriteOp` variant). For `progress_tx` and `layer_tx`: either spawn no-op drain tasks, or pipe them through the existing `event_bus` so Pipeline B builds become visible in build viz like any other build. The latter is preferable — Phase 13 expands build viz and the existing viz already consumes these.
- Create a fresh `CancellationToken` per dispatch. Pipeline B dispatch is not currently cancellable from the outside — a future phase can add cancellation, but do NOT invent a cancellation mechanism now.

### 3. Replace the stub in `dispatch_pending_ingests`

Current shape (lines 357-453): iterates pending records one at a time, marks each processing, stub-"completes" each with a placeholder build_id, emits events per record.

**The iteration shape is wrong for pyramids.** A pyramid build processes the whole slug, not one file at a time. Firing `run_build_from` N sequential times for N pending records would do N full builds where one suffices.

**Correct shape:**

1. Get pending records (respect `batch_size` as a cap on how many to CLAIM per dispatch — not how many sequential builds to run)
2. Under a short `LockManager::global().write(slug)` scope, mark all claimed records as `processing` in a single DB pass
3. Release the write lock (so `run_build_from` can acquire its own)
4. Emit `IngestStarted` events for each claimed record
5. Call `fire_ingest_chain(&state, slug, content_type, &pending_source_paths, ...)` ONCE, which:
   a. Chunks any new source files via the appropriate `ingest::ingest_*` primitive under its own short write-lock scope
   b. Fires `run_build_from` (which takes its own write lock for the full build duration)
   c. Returns the real `build_id` on success or an error
6. Under a short write-lock scope, mark all claimed records as `complete` with the real build_id on success, or `failed` with the error message on failure
7. Emit `IngestComplete` / `IngestFailed` events for each record

Use existing `TaggedKind::IngestStarted`, `TaggedKind::IngestComplete`, `TaggedKind::IngestFailed` variants. Do not invent new event types for Phase 0b — Phase 13 expands the event vocabulary.

### 4. Scope boundaries (what's in, what's out)

**In scope:**
- Conversation content type (the primary use case). `ingest_conversation` is a per-file chunker and maps cleanly to the per-file ingest-record model.
- Code and document content types IF the per-file chunking story is clean. Reading `ingest.rs`, `ingest_code`/`ingest_docs` take a directory and re-scan everything. For Phase 0b you may EITHER:
  - **(a)** Extend them with a per-file sibling (e.g., `ingest_single_code_file`) that chunks one file and appends it at the current `chunk_offset`. Real implementation, not a TODO.
  - **(b)** Return an explicit `anyhow::bail!("Phase 0b: content_type X is not yet supported by Pipeline B ingest; lands in Phase 17 folder ingestion")` for non-conversation records, so they enter `failed` status rather than silently succeed. Record the decision explicitly in the implementation log.
  
  Either choice is defensible. Pick one, document it in the implementation log under "Spec adherence" as an explicit scope decision, move on.

**Out of scope:**
- The in-flight `Arc<AtomicBool>` flag (Phase 1's work). Do not pre-empt Phase 1.
- Changing the scan/detect logic in `run_tick_for_config` (lines 164-247). Pipeline B's scan loop is correct.
- Session timeout / promotion logic (`check_session_timeouts`, `promote_session`). Correct and working.
- Rewriting the ingest record schema or lifecycle states. Use the existing `pending` / `processing` / `complete` / `failed` states.
- Cost tracking (Phase 11), build viz expansion (Phase 13), LLM output cache (Phase 6). Just make the chain fire.

## Verification criteria

**Your work is not done until all three pass:**

1. **`cargo check` and `cargo build` pass** from `src-tauri/` with no new warnings in files you touched. If you encounter pre-existing warnings in other files, leave them alone. Note any warnings you introduce as friction entries — then fix them.
2. **Existing tests in `dadbear_extend.rs` tests module (lines 527-778) still pass.** Run the equivalent of `cargo test -p wire-node-lib --lib pyramid::dadbear_extend`. Update test fixtures minimally if your signature changes require it. Add at least one new test for `fire_ingest_chain` that exercises the success path with an in-memory DB and a minimal chain (see `chain_executor.rs` tests around line 14604 for PyramidState test fixture patterns).
3. **Real file drop verification steps** — describe the exact steps for a human to verify end-to-end, and write them into the implementation log as a pending verification checklist:
   - Start the app with a DADBEAR-enabled conversation pyramid pointed at a test directory
   - Drop a test jsonl file into the directory
   - Within one tick interval, observe in the logs: scan picks up the file → ingest record enters `pending` → dispatch fires → log shows "DADBEAR: dispatching ingest" with a real build_id (NOT the placeholder format `dadbear-ingest-<slug>-<uuid>`) → build runs → log shows "DADBEAR: ingest complete" → ingest record enters `complete` with the real build_id
   - The pyramid viz should reflect the new content

## Deviation protocol

**If you find a reason the spec or this prompt needs to change, STOP and surface it.** Do not deviate silently.

Legitimate reasons to deviate:
- A spec detail contradicts what the code actually does (the code wins; the spec needs correcting)
- Two specs contradict each other at an integration point
- A performance or semantic constraint makes a specified approach impractical
- You find a bug in adjacent code that blocks Phase 0b (fix it per the repo's "fix all bugs when found" convention, and note it in the friction log)

**How to surface:** append an entry to `docs/plans/pyramid-folders-model-routing-friction-log.md` AND include a clearly-framed question at the top of your final summary in the format:

```
> [For the planner]
>
> Context: Phase 0b, file X, line Y.
>
> Question: <direct question>
>
> What I found: <brief>
>
> What I did: <brief — either worked around, or stopped and flagged>
>
> Impact: <brief>
```

**You are empowered to correct the plan if it gets details wrong, but NOT to change the end state** that Phase 0b is supposed to create: Pipeline B actually runs chain builds on new source files via a `fire_ingest_chain` helper that respects lock ordering and integrates cleanly with `run_build_from`.

## Implementation log protocol

Append / update the Phase 0b entry in `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Use the format defined at the top of that file. Minimum fields:

- **Started:** ISO timestamp when you began
- **Files touched:** every file with a brief change description
- **Spec adherence:** one line per addendum §0b requirement (1-6) plus the scope-narrowing decision for code/doc
- **Verification results:** `cargo check` / `cargo build` / test results, plus a bullet noting that real-file-drop verification is pending a human's manual run (with a reference to the verification steps you wrote)
- **Completed:** ISO timestamp when you finished
- **Status:** `awaiting-verification`

Do NOT write `verified` in your own entry — that's reserved for a later pass.

Update the friction log if anything surprised you, dead-ended, or contradicted the spec.

## Mandate

- **Correct before fast.** Never skip verification to move faster.
- **Right before complete.** A phase that's 95% done and correct is preferable to 100% done and subtly wrong. If something feels off, stop and flag it.
- **No shortcuts, no simplifications, no deferrals without explicit approval.** If you want to narrow scope beyond what this prompt allows, flag it first.
- **No speculative abstractions or helpers for one-time operations.** Keep `fire_ingest_chain` a single function. Extract helpers only if the same pattern repeats within the Phase 0b change set.
- **Pillar 37:** every number that constrains LLM output is a violation of the "everything flows from config" principle. Phase 0b shouldn't introduce any such numbers (it's wiring existing pieces, not new LLM calls), but watch for them.
- **Fix all bugs found.** If you encounter a bug in adjacent code that isn't blocking Phase 0b, still fix it and note in the friction log — the repo convention is fix-on-sight.
- **Commit when done.** Single commit with a clear message. Suggested format: `phase-0b: wire fire_ingest_chain for Pipeline B dispatch`. Include a 2-3 line body summarizing the change. Do not amend; create a fresh commit. Do not push unless explicitly told.

## End state

Phase 0b is complete when:

1. `start_dadbear_extend_loop` accepts an `Arc<PyramidState>` and both main.rs call sites pass it.
2. A new `fire_ingest_chain` helper exists in `dadbear_extend.rs` (or a dedicated sibling module if you prefer — flag the choice in the implementation log) that chunks new source files via the appropriate ingest primitive and fires `run_build_from`, returning the real `build_id`.
3. `dispatch_pending_ingests` claims all pending records under a short lock, fires `fire_ingest_chain` once (not N times), and marks records complete/failed with the real build_id. No more placeholder `dadbear-ingest-<slug>-<uuid>` build_ids.
4. `cargo check`, `cargo build`, existing + new tests all pass locally.
5. The implementation log entry is filled in with spec adherence, files touched, cargo results, and a verification checklist for the human to run.
6. The friction log is updated with anything surprising, if anything was surprising.
7. A single commit lands cleanly on the current branch.

Begin with the reading. Do not write any code until you've read every file on the required reading list in full (or to the targeted sections for chain_executor.rs and main.rs). When in doubt, read more before writing.

Good luck. Build carefully. Build right. Take the time you need.
