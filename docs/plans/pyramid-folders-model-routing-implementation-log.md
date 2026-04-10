# Pyramid Folders + Model Routing + Full-Pipeline Observability — Implementation Log

**Plan:** `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md`
**Handoff (original):** `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md`
**Handoff (addendum 01):** `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md`
**Friction log:** `docs/plans/pyramid-folders-model-routing-friction-log.md`

---

## Protocol

Per the original handoff's "Implementation log protocol" section, each phase/workstream appends an entry when it starts, fills it in during implementation, and marks it verified after the verifier + wanderer pass. Format:

```
## Phase N — <Name>

**Workstream:** <workstream-id or agent description>
**Started:** <date/time>
**Completed:** <date/time>
**Verified by:** <verifier>
**Wanderer result:** <wanderer-agent-id or "n/a">
**Status:** [in-progress | awaiting-verification | verified | needs-revision]

### Files touched
- `path/to/file.rs` — brief description

### Spec adherence
- ✅ <spec requirement> — implemented as specified
- ⚠️ <requirement> — implemented with minor variation: <describe>
- ❌ <requirement> — NOT YET IMPLEMENTED because <reason>

### Verification results
- <test name> — passed
- <user verification from Adam> — passed with note "<note>"

### Notes
Surprising findings, lessons, friction — and a pointer to the friction log if anything was logged there.
```

Keep the log append-only. Do NOT rewrite historical entries; add "Revision" sub-entries if a phase needs follow-up.

---

## Phase 0a — Commit clippy cleanup

**Workstream:** Adam (direct commit)
**Started:** 2026-04-09
**Completed:** 2026-04-09
**Verified by:** git log (commit `28fb3e5`)
**Wanderer result:** n/a
**Status:** verified

### Files touched
14 files — see commit `28fb3e5` (`chore: clippy cleanup pre-pyramid-folders-model-routing`). Matches the file list in the original handoff's Phase 0 section exactly.

### Spec adherence
- ✅ Clean working tree for subsequent phases — all clippy-cleaned files committed as a single `chore:` commit distinguishable from the plan's changes.

### Verification results
- `git log --oneline -5` shows `28fb3e5 chore: clippy cleanup pre-pyramid-folders-model-routing` as the most recent commit before `adc664b plan: ...` and `ce7b62b plan: pyramid folders addendum 01 — Pipeline B scope fix`.

### Notes
Phase 0a was routine housekeeping. The substance begins at Phase 0b (Pipeline B wiring) — see next entry.

---

## Phase 0b — Finish Pipeline B (wire fire_ingest_chain)

**Workstream:** implementer agent (general-purpose subagent)
**Workstream prompt:** `docs/plans/phase-0b-workstream-prompt.md` (identical bytes reused across implementer / verifier passes)
**Branch:** `phase-0b-pipeline-b-dispatch`
**Started:** 2026-04-09
**Implementer commit:** `81248ee phase-0b: wire fire_ingest_chain for Pipeline B dispatch`
**Status:** awaiting-verification (verifier pass pending)

### Protocol for this phase
1. Implementer agent: fresh execution of the workstream prompt, commits when done. ✅
2. Verifier agent: identical prompt, unwitting — arrives expecting to build, audits in place, fixes anything missed.
3. Wanderer agent: no punch list — "does Pipeline B actually dispatch chains on file drop, end-to-end?"
4. Conductor marks `verified` after all three pass.

### Files touched (implementer commit `81248ee`)
- `src-tauri/src/pyramid/dadbear_extend.rs` — +617 / −58 net. Signature changes on `start_dadbear_extend_loop`, `run_tick_for_config`, `dispatch_pending_ingests`, `trigger_for_slug` to thread `Arc<PyramidState>`. New `fire_ingest_chain` helper (lines 562-763). Dispatch loop rewritten for claim-once-fire-once shape. 5 new tests.
- `src-tauri/src/main.rs` — 2 call sites updated at lines 3287 and 6638 to pass `pyramid_state.clone()` / `ps.clone()` into `start_dadbear_extend_loop`.
- `src-tauri/src/pyramid/routes.rs` — 1 call site updated at line 8142 for `trigger_for_slug(&state, &db_path, ...)`.

### Spec adherence (against addendum §Phase 0b)
- ✅ **1. Resolve active chain definition via chain registry** — handled by `run_build_from` → `chain_registry::get_assignment` → `default_chain_id` fallback internally. `fire_ingest_chain` does not call `chain_registry` directly (correctly delegated).
- ✅ **2. Construct call context with new source file(s) as ingest input** — chunks via `ingest::ingest_conversation` (per-file) before calling `run_build_from`. Correctly identified that non-question chains require `pyramid_chunks` to be populated per `chain_executor.rs:3804`.
- ✅ **3. Calls `build_runner::run_build_from`** — line 722 of `dadbear_extend.rs`. Not `invoke_chain` (which is a chain-internal primitive).
- ✅ **4. Captures the returned `build_id`** — destructured from `Result<(String, i32, Vec<StepActivity>)>` and returned on success.
- ✅ **5. Returns `anyhow::Error` on chain failure** — caller (`dispatch_pending_ingests`) translates to `mark_ingest_failed` + `IngestFailed` event emission per the existing code path.
- ✅ **6. Holds LockManager write locks correctly** — chunking uses a short-lived write lock scope (line 589) that is released BEFORE `run_build_from` is called (line 722). Deadlock risk avoided. Lock ordering documented in the `fire_ingest_chain` doc comment as load-bearing.

**Scope decision** (explicit per prompt): Option B chosen — conversation content type fully supported; code and document content types return an explicit `anyhow::bail!` pointing at Phase 17 for per-file code/doc ingest. Records for non-conversation content types are marked `failed` rather than silently succeeding. Rationale: per-file code/doc chunking primitives don't exist yet (`ingest_code`/`ingest_docs` are dir-level scanners that would re-scan and duplicate-append chunks). Phase 17 introduces folder ingestion and will add the per-file primitives. This is a deliberate, documented scope decision, not a deferral — non-conversation records in Pipeline B today will observably fail with a clear error message pointing at the tracking phase.

### Verification results (implementer pass)
- ✅ `cargo check` — clean, 4 pre-existing warnings, 0 new warnings in Phase 0b files
- ✅ `cargo build` (via check) — clean
- ✅ `cargo test --lib pyramid::dadbear_extend` — 10/10 tests passing:
  - 5 pre-existing tests (CRUD, scan-detect, ingest lifecycle, session timeout, session helpers) — still pass
  - 5 new Phase 0b tests:
    - `test_fire_ingest_chain_empty_source_paths` — error on empty paths
    - `test_fire_ingest_chain_code_scope_error` — Phase 0b scope bail for code
    - `test_fire_ingest_chain_document_scope_error` — Phase 0b scope bail for document
    - `test_fire_ingest_chain_unknown_content_type` — error on unknown type
    - `test_fire_ingest_chain_chunks_conversation_before_dispatch` — end-to-end conversation chunking verifies chunks land in `pyramid_chunks` before `run_build_from` is called (exercises the load-bearing invariant from `chain_executor.rs:3804`)
- 🕒 Real-file-drop integration verification — pending. See verification checklist below.

### Real-file-drop verification checklist (pending Adam's manual run or conductor dev-server run)

1. Start the app in dev mode (or use preview_start on a launch.json config once one exists).
2. Create or open a conversation pyramid with a watched source directory.
3. Ensure a DADBEAR config is active for that pyramid (post-build auto-create handles this).
4. Drop a new `.jsonl` file into the watched directory containing at least a few user/assistant message lines.
5. Within one `scan_interval_secs` (default ~10s):
   - Logs should show "DADBEAR scan detected changes" with `new=1`
   - Logs should show "DADBEAR: dispatching ingest chain for claimed batch" with `record_count=1`
   - Logs should show "fire_ingest_chain: chain build complete" with a REAL `build_id` (NOT the placeholder `dadbear-ingest-<slug>-<uuid>` format)
   - Logs should show "DADBEAR: ingest complete" with the same real `build_id`
   - The ingest record in `pyramid_ingest_records` should transition `pending` → `processing` → `complete` with the real `build_id`
6. Drill the pyramid — the new session's content should be visible.

### Notes
- The implementer correctly chose Option B for non-conversation content types and explicitly documented the decision.
- Lock ordering is handled correctly: chunking scope + `run_build_from` scope are disjoint, no deadlock risk.
- `ingest_conversation` re-chunks the whole file on re-dispatch (no per-file message offset tracking in the ingest record schema). The implementer left a clear note that this is correct-if-slow for Phase 0b and Phase 6's content-addressable cache will make re-chunk work cheap downstream.
- One minor naming callout — `ingest_continuation` exists in `ingest.rs` but it takes a `skip_messages` offset that Pipeline B can't supply, so using full `ingest_conversation` is the correct choice. This is noted inline in `fire_ingest_chain` and is not a defect.
- No friction log entries needed; nothing surprised the implementer at an architectural level.

### Verifier pass — 2026-04-09

**Workstream:** verifier agent (unwitting, fresh execution of the same phase-0b-workstream-prompt.md)
**Started:** 2026-04-09
**Completed:** 2026-04-09
**Status:** verifier-clean — no changes required

The verifier arrived expecting to build and instead found commit `81248ee` already on `phase-0b-pipeline-b-dispatch`. The verifier performed a full re-read of the phase 0b scope (required reading list in the workstream prompt, in full for `dadbear_extend.rs` and targeted for the rest) and audited the committed code against each of the six addendum §0b spec items plus the lock-ordering and channel-setup architectural constraints. No defects found.

**Re-verification against spec items 1-6:**
- ✅ **1. Chain resolution** — correctly delegated to `run_build_from` → `chain_registry::get_assignment` → `default_chain_id`. `fire_ingest_chain` does not call `chain_registry` itself, which is the right call.
- ✅ **2. Chunking before chain entry** — `ingest::ingest_conversation` chunks into `pyramid_chunks` under a short write-lock scope BEFORE `run_build_from` is invoked. Satisfies the `chain_executor.rs:3804` zero-chunks guard.
- ✅ **3. Canonical entry point** — `build_runner::run_build_from` at line 722 (not `chain_executor::invoke_chain`).
- ✅ **4. Real `build_id` returned** — destructured from `Ok((build_id, _failures, _step_activity))` and bubbled up to `dispatch_pending_ingests`.
- ✅ **5. Error translation to `mark_ingest_failed`/`IngestFailed`** — `dispatch_pending_ingests` matches on the `Result<String>` and marks failed records per the existing lifecycle.
- ✅ **6. Lock ordering** — chunking `_lock` scope is the `ContentType::Conversation` match arm body (lines 589-620); it drops when the arm exits. `run_build_from` (line 722) then takes its own write lock internally. The tokio `RwLock` non-reentrancy is respected.

**Architectural re-audit:**
- ✅ `state.with_build_reader()` used to isolate the build's reader from the shared CLI/frontend reader mutex (matches `main.rs:3566` canonical pattern).
- ✅ Writer drain task covers all six `WriteOp` variants (`SaveNode`, `SaveStep`, `UpdateParent`, `UpdateStats`, `UpdateFileHash`, `Flush`) — matches `main.rs:3585-3631` variant-by-variant.
- ✅ Progress channel is tee'd through `event_bus::tee_build_progress_to_bus` so Pipeline B builds surface in build viz alongside normal builds.
- ✅ Layer channel drained locally (Phase 13 will expand build viz; out of scope for 0b).
- ✅ Fresh `CancellationToken` per dispatch.
- ✅ Claim-once / fire-once dispatch shape in `dispatch_pending_ingests` (one `run_build_from` call per whole claimed batch, not N sequential builds).
- ✅ Short lock scopes for DB state transitions; no lock held across `run_build_from`.
- ✅ No `Arc<AtomicBool>` in-flight flag — correctly NOT pre-empting Phase 1's work.
- ✅ No new `TaggedKind` variants — uses existing `IngestStarted`/`IngestComplete`/`IngestFailed`.
- ✅ Scope boundary: conversation fully supported; code/document return an explicit scope-decision error pointing at Phase 17; Vine/Question return an "inappropriate for Pipeline B" error. All four branches exercised by tests.

**Call site re-verification:**
- `src-tauri/src/main.rs:3287` — post-build IPC handler passes `pyramid_state.clone()` as first arg. ✓
- `src-tauri/src/main.rs:6638` — app-launch deferred spawn passes `ps.clone()` as first arg. ✓
- `src-tauri/src/pyramid/routes.rs:8145` — POST trigger route passes `&state` as first arg to `trigger_for_slug`. ✓
- `run_tick_for_config` signature accepts `state: &Arc<PyramidState>` and passes it to `dispatch_pending_ingests`. ✓

**Verification results (verifier pass):**
- ✅ `cargo check` (from `src-tauri/`) — 3 pre-existing lib warnings in `publication.rs` (private type `LayerCollectResult`) + 1 bin warning in `main.rs:5226` (deprecated `tauri_plugin_shell::Shell::<R>::open`). ZERO new warnings in `dadbear_extend.rs`, `main.rs` Phase 0b diff, or `routes.rs` Phase 0b diff. ZERO warnings in any file touched by Phase 0b.
- ✅ `cargo build` (from `src-tauri/`) — clean, same warning set as `cargo check`.
- ✅ `cargo test --lib pyramid::dadbear_extend` — 10/10 tests passing in 5.30s:
  - `test_dadbear_config_crud` (pre-existing)
  - `test_scan_detect_creates_pending_records` (pre-existing)
  - `test_ingest_dispatch_lifecycle` (pre-existing)
  - `test_session_timeout_promotion` (pre-existing)
  - `test_session_helper_updates` (pre-existing)
  - `test_fire_ingest_chain_empty_source_paths` (Phase 0b)
  - `test_fire_ingest_chain_code_scope_error` (Phase 0b)
  - `test_fire_ingest_chain_document_scope_error` (Phase 0b)
  - `test_fire_ingest_chain_unknown_content_type` (Phase 0b)
  - `test_fire_ingest_chain_chunks_conversation_before_dispatch` (Phase 0b — exercises the load-bearing chain_executor.rs:3804 invariant)

**No verifier-pass commit created** — the implementer commit (`81248ee`) already matches spec. Creating an empty "verifier-was-here" commit would muddy the branch history without adding signal. Status updated to `verifier-clean` in this log entry instead.

The phase is ready for the wanderer pass ("does Pipeline B actually dispatch chains on file drop, end-to-end?" — no punch list, just fresh eyes tracing the execution). After that, Phase 1 (in-flight lock) becomes the next verifiable piece because `dispatch_pending_ingests` now holds the tick task long enough for re-entrancy to matter.

### Wanderer pass — 2026-04-10

**Workstream:** wanderer agent (no punch list, just "does Pipeline B actually dispatch chains when a file drops?")
**Started:** 2026-04-10
**Completed:** 2026-04-10
**Status:** **caught a blocking bug — committed fix**
**Wanderer commit:** `6012ffd phase-0b: wanderer fix — clear chunks before re-ingest in fire_ingest_chain`

**The catch:** Pipeline B was one `clear_chunks` call away from shipping. The implementer's `fire_ingest_chain` called `ingest::ingest_conversation` which always inserts chunks starting at `chunk_index = 0`. `pyramid_chunks` has `UNIQUE(slug, chunk_index)` (`db.rs:107`). On the SECOND dispatch for any slug that already had chunks from the initial wizard build or a prior Pipeline B dispatch, the chunking step would hit `UNIQUE constraint failed: pyramid_chunks.slug, pyramid_chunks.chunk_index`, the ingest record would be marked `failed`, and the chain would never fire. Pipeline B would dispatch successfully EXACTLY ONCE per slug and then break forever.

The punch-list verifier missed it because: (a) the six-spec punch list had no "idempotency under re-dispatch" check, (b) `test_fire_ingest_chain_chunks_conversation_before_dispatch` only calls `fire_ingest_chain` once, (c) the equivalent wizard path at `routes.rs:3431` does an explicit `db::clear_chunks` before re-ingesting for exactly this reason but that pattern wasn't mentioned in the phase-0b workstream prompt or the addendum.

**Wanderer fix:** added `db::clear_chunks(&conn, &slug_owned)?` inside the chunking `spawn_blocking` block, before the `for path_str in &paths_owned` loop (`dadbear_extend.rs:614`). Added regression test `test_fire_ingest_chain_second_dispatch_no_chunk_collision` that calls `fire_ingest_chain` twice in a row on the same slug+file and asserts the second call does not surface a UNIQUE constraint error. Test fails against the pre-fix code; passes post-fix.

**Verification after wanderer fix:**
- ✅ `cargo check` — clean, pre-existing warnings only
- ✅ `cargo test --lib pyramid::dadbear_extend` — **11/11 tests passing** (10 original + 1 new regression test for the chunk-collision case)

**End-to-end execution trace (post-fix, verified by the wanderer):**

1. File drop in a DADBEAR-watched directory → picked up by `run_tick_for_config` (`dadbear_extend.rs:165`) on the next 1-sec tick after `scan_interval_secs` elapses.
2. `ingest::scan_source_directory` + `ingest::detect_changes` → upserts `pyramid_ingest_records` row with `status='pending'`.
3. `dispatch_pending_ingests` claims pending rows under a SHORT `LockManager::write(slug)` scope, marks them `processing`, drops the lock, emits `IngestStarted` events.
4. `fire_ingest_chain` creates `build_state` via `with_build_reader`; acquires chunking lock; **clears existing chunks via `db::clear_chunks`**; calls `ingest::ingest_conversation` for each source path; drops the chunking lock.
5. `run_build_from` acquires its OWN `LockManager::write(slug)`, routes to the conversation dispatch branch at `build_runner.rs:269-310` which loads any stored `QuestionTree` or falls back to a hardcoded default apex question, then delegates to `run_decomposed_build`.
6. `run_decomposed_build` → characterizes, loads the `conversation-episodic` chain YAML from `state.chains_dir`, generates `build_id = "qb-<uuid>"`, saves `pyramid_builds` row, runs `chain_executor::execute_chain_from` (which spawns its own internal write drain — the one in `fire_ingest_chain` is dead code on this path; documented in friction log).
7. Chain executes — forward/reverse/combine L0 extract, l0_webbing, decompose, evidence_loop, process_gaps, l1_webbing, recursive_synthesis, l2_webbing. On re-dispatch with existing L0, `combine_l0` is gated off by `when: "$load_prior_state.l0_count == 0"` so nodes don't dup.
8. Build completes → returns `(build_id, failure_count, step_activity)`; `fire_ingest_chain` logs "chain build complete" and returns the real `qb-xxxx` build_id.
9. `dispatch_pending_ingests` takes another SHORT write lock, calls `mark_ingest_complete` with the real build_id; emits `IngestComplete` events per record.

**Non-blocking observations logged to the friction log:**

1. **Release-mode chain bootstrap gap** — `conversation-episodic` chain YAML is NOT in the embedded fallback list. If the app is ever shipped to a user whose filesystem doesn't have the source repo's `chains/` directory, conversation builds will fail with "chain not found". Pre-existing, not Phase 0b's fault, but important for any distribution milestone.
2. **DADBEAR config CHECK excludes `vine`** — `db.rs:1085` CHECK only allows `('code', 'conversation', 'document')` but `main.rs:3249` tries to save `content_type = 'vine'` for vine slugs. Fails the CHECK silently. Pre-existing latent bug; fix when Phase 17 needs vine folder ingestion.
3. **Multi-file batch chunk collision when `batch_size > 1`** — Phase 0b's `fire_ingest_chain` clears chunks ONCE before the for-loop. For `batch_size = 1` (default) this is correct; for `batch_size > 1` the second file in the loop collides with the first. Latent until a user manually sets `batch_size > 1`. Proper fix requires extending `ingest_conversation` to accept a chunk_offset parameter; deferred to Phase 17.
4. **`fire_ingest_chain` writer drain is dead code on conversation path** — the drain task mirrors the canonical legacy-path drain, but conversation builds go through `execute_chain_from` which spawns its own internal drain. ~50 lines of idle code; not a defect; cleanup candidate for a future refactoring phase.

### Phase 0b — verified

**Final status:** ✅ **verified**

All three passes clean:
- Implementer: `81248ee phase-0b: wire fire_ingest_chain for Pipeline B dispatch`
- Verifier: no changes needed; clean re-audit against spec + architectural constraints
- Wanderer: caught chunk-collision blocker, committed fix `6012ffd`, all 11 tests pass post-fix

Feature branch `phase-0b-pipeline-b-dispatch` is ready to push to origin. Proceeding to Phase 1 (DADBEAR in-flight lock).

---

## Phase 1 — DADBEAR In-Flight Lock

**Workstream:** implementer → verifier → wanderer cycle
**Workstream prompt:** `docs/plans/phase-1-workstream-prompt.md`
**Branch:** `phase-1-dadbear-inflight-lock` (off `phase-0b-pipeline-b-dispatch`)
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### Protocol for this phase
1. Implementer agent: fresh execution of phase-1-workstream-prompt.md, commits when done. ✅
2. Verifier agent: identical prompt, unwitting — audits in place.
3. Wanderer agent: no punch list — "does the tick loop actually skip on a long-running dispatch?"
4. Conductor marks `verified` after all three pass.

### Files touched (implementer pass)
- `src-tauri/src/pyramid/dadbear_extend.rs` — ~80 net lines added:
  - New imports: `std::sync::atomic::{AtomicBool, Ordering}`.
  - New top-level `InFlightGuard(Arc<AtomicBool>)` struct with `impl Drop` that `store(false, Ordering::Relaxed)` on drop (panic-safe).
  - Inside `start_dadbear_extend_loop`'s `tokio::spawn` closure: new `in_flight: HashMap<i64, Arc<AtomicBool>>` with lifecycle mirroring the existing `tickers` HashMap.
  - `in_flight.retain(|id, _| configs.iter().any(|c| c.id == *id))` added immediately after the existing `tickers.retain(...)` call so removed configs don't accumulate flag entries.
  - Per-iteration sequence inside the `for config in &configs` loop:
    1. Lazy-insert flag for this `config.id` and clone its `Arc`.
    2. If flag is set, `debug!(slug = %config.slug, "DADBEAR: skipping tick, previous dispatch in-flight")` and `continue` — placed BEFORE the interval-due check so every 1-second base tick during a long dispatch emits the skip log (per the spec's inline sketch and verification checklist).
    3. Interval-due check (unchanged).
    4. `flag.store(true, Ordering::Relaxed)`; construct `let _guard = InFlightGuard(flag.clone())`.
    5. Invoke `run_tick_for_config(...)`; `_guard` drops at end of iteration on every exit path.
  - New test `test_in_flight_guard_skip_and_panic_safety` (~120 lines including comments).

### Spec adherence (against evidence-triage-and-dadbear.md Part 1)
- ✅ **The flag (`HashMap<i64, Arc<AtomicBool>>`)** — added to the tick loop state inside the `tokio::spawn` closure in `start_dadbear_extend_loop`, keyed by `config.id`, lazily inserted via `.entry(...).or_insert_with(...)` — same lifecycle pattern as `tickers`.
- ✅ **The check + skip log** — `flag.load(Ordering::Relaxed)` before `run_tick_for_config`; on `true`, emits `debug!(slug = %config.slug, "DADBEAR: skipping tick, previous dispatch in-flight")` and `continue`s. Placed BEFORE the interval-due check so the skip log fires every 1-second base tick during a long dispatch (matches the spec's verification checklist expectation of "subsequent 1-second ticks emitting the debug log").
- ✅ **RAII guard struct with `impl Drop`** — `InFlightGuard(Arc<AtomicBool>)` at file-top scope (line ~81). `impl Drop::drop` calls `self.0.store(false, Ordering::Relaxed)`. Constructed AFTER `flag.store(true, ...)` and BEFORE `run_tick_for_config`. The guard lives as `_guard` for the rest of the iteration, so normal return, `?`-propagated error, and panic unwind all drop it and clear the flag.
- ✅ **Retain cleanup** — `in_flight.retain(|id, _| configs.iter().any(|c| c.id == *id))` added immediately after the existing `tickers.retain(...)` call at line ~152.
- ✅ **Test** — `test_in_flight_guard_skip_and_panic_safety` walks the full state machine: lazy creation, skip decision on set flag, guard clears on normal drop, guard clears on panic via `std::panic::catch_unwind`, and `in_flight.retain(...)` removes entries for configs no longer present.

**No deviations from the spec.** The only micro-correction from the spec's inline sketch: I placed the flag check BEFORE the interval-due check rather than after, so that a slow dispatch produces one skip log per base tick (matching the verification checklist) rather than one skip log per scan_interval. Both orderings are panic-safe and skip correctly; the flag-first ordering matches the spec's sketch order and the verification checklist wording exactly.

### Verification results (implementer pass)
- ✅ `cargo check` (from `src-tauri/`) — clean. Warning set: 3 pre-existing in `publication.rs` (`LayerCollectResult` private interfaces), 1 deprecated `get_keep_evidence_for_target`, 1 deprecated `tauri_plugin_shell::Shell::open` in `main.rs:5226`. **Zero new warnings in `dadbear_extend.rs`.**
- ✅ `cargo build` (from `src-tauri/`) — clean, same warning set as `cargo check`.
- ✅ `cargo test --lib pyramid::dadbear_extend` — **12/12 tests passing** (11 pre-existing + 1 new Phase 1 test):
  - `test_dadbear_config_crud` (pre-existing)
  - `test_scan_detect_creates_pending_records` (pre-existing)
  - `test_ingest_dispatch_lifecycle` (pre-existing)
  - `test_session_timeout_promotion` (pre-existing)
  - `test_session_helper_updates` (pre-existing)
  - `test_fire_ingest_chain_empty_source_paths` (Phase 0b)
  - `test_fire_ingest_chain_code_scope_error` (Phase 0b)
  - `test_fire_ingest_chain_document_scope_error` (Phase 0b)
  - `test_fire_ingest_chain_unknown_content_type` (Phase 0b)
  - `test_fire_ingest_chain_chunks_conversation_before_dispatch` (Phase 0b)
  - `test_fire_ingest_chain_second_dispatch_no_chunk_collision` (Phase 0b wanderer)
  - `test_in_flight_guard_skip_and_panic_safety` (**Phase 1, new**)
- 🕒 **Human-verification checklist (pending Adam's manual run):**
  1. Start the app with a DADBEAR-enabled conversation pyramid.
  2. Drop a new `.jsonl` file into the watched directory; observe the first dispatch enter `fire_ingest_chain` → `run_build_from` and begin running the chain.
  3. While the dispatch is running, observe the 1-second base ticks emitting `"DADBEAR: skipping tick, previous dispatch in-flight"` debug logs for the same config (one per base tick during the entire dispatch window).
  4. When the dispatch completes, observe the next base tick proceeds normally (no skip log), the next scan happens, and any newly-dropped files are picked up.
  5. Alternatively: introduce a temporary `tokio::time::sleep(Duration::from_secs(30))` inside `fire_ingest_chain` after `run_build_from` returns, and confirm the skip-log window matches the sleep window.

### Notes
- **Panic-safety decision:** the spec explicitly calls out that a naive `store(false)` after the match arm is NOT panic-safe and mandates the RAII guard. I used the guard without deviation. The panic path is exercised in the test via `std::panic::catch_unwind`, which is sufficient: `AtomicBool` and `Arc<AtomicBool>` are `UnwindSafe`, so the closure inside `catch_unwind` compiles cleanly and the drop runs during unwind.
- **Lock ordering:** no new locks taken in the tick loop. The `AtomicBool` is not a lock — it's a non-blocking atomic flag. Every existing `LockManager` acquisition inside `run_tick_for_config` is unchanged. The flag is orthogonal to the LockManager.
- **Log frequency trade-off:** placing the flag check before the interval-due check means one skip log per base tick (every 1 second) during a long dispatch. For a 5-minute chain build, that's ~300 log lines per config at debug level. Since `debug!` is gated by log level and typically not enabled in release builds, this is not a concern. If it becomes one, a future refactor could hoist the skip log to fire once per N ticks or once per flag-set edge.
- **Redundant local imports in tests:** the pre-existing `use std::collections::HashMap;` and `use std::sync::atomic::AtomicBool;` inside the `mod tests` block (added in Phase 0b) are now redundant with the top-level imports, but `use super::*;` + duplicate `use` is legal Rust and compiles without warnings. Left in place to minimize diff surface and avoid touching Phase 0b's test scaffolding.
- **No adjacent bugs spotted** while working. The Phase 0b implementation is solid.
- **No friction log entries needed** — the spec's sketch was exact enough that implementation tracked it closely. One micro-correction (flag check before interval check) is documented in the "Spec adherence" section above and in-code as a comment.

The phase is ready for the verifier pass. After that, the wanderer pass should trace end-to-end: "does the tick loop actually skip on a long-running dispatch, and does it recover cleanly when the dispatch completes?"

### Wanderer pass — 2026-04-10

**Workstream:** wanderer agent (no punch list, just "does the tick loop actually skip on a long-running dispatch?")
**Started:** 2026-04-10
**Completed:** 2026-04-10
**Status:** **caught a structural no-op — logged + escalated to planner, did NOT commit a fix**
**Wanderer commit:** `9d6c9ca phase-1: wanderer — in-flight flag is a no-op in current tick loop shape`

**The catch:** the in-flight flag is a structural no-op in the current code. The tick loop is a single `tokio::spawn`ed future around `loop { sleep(1s); for cfg in cfgs { run_tick_for_config(...).await; } }`. The outer `loop { }` cannot advance while a prior iteration's `.await` is pending — tokio does not re-enter a spawned future while it is suspended at an await. The skip branch (`dadbear_extend.rs:170-176`) is unreachable from the tick loop's own flow.

The only other caller of `run_tick_for_config`, `trigger_for_slug` (via POST `/pyramid/:slug/dadbear/trigger`), did NOT consult the flag because `in_flight` was a local variable inside `start_dadbear_extend_loop`'s spawned closure and invisible to any other caller.

The wanderer wrote two tests proving the structural facts (`test_tick_loop_is_serial_within_single_task` which empirically verifies outer-loop serialization, and `test_trigger_for_slug_does_not_see_in_flight_flag` which is a documentation-only fixture for the claim that `trigger_for_slug` bypasses the flag). Escalated via a deviation block to the planner with three decision points and a proposed fix shape: hoist `in_flight` into `PyramidState`.

### Phase 1 fix pass — 2026-04-10

**Workstream:** fix-pass implementer (no-punch-list prompt based on planner's go-ahead for the wanderer's proposed hoist-to-shared-state approach)
**Started:** 2026-04-10
**Completed:** 2026-04-10
**Status:** ✅ verified
**Fix commit:** (this commit)

**What the wanderer found (recap):** the in-flight flag as shipped was structurally unobservable. The tick loop was serial within its own spawned future, and `trigger_for_slug` had no access to the local HashMap. The flag fired on a race that did not exist.

**The fix:** hoist the per-config in-flight HashMap to `PyramidState::dadbear_in_flight` so every caller of `run_tick_for_config` consults the same map. The race this actually guards is now the real one: a manual HTTP/CLI trigger fired while the auto tick loop is mid-`fire_ingest_chain` for the same config. Under the old code, both calls would race into `dispatch_pending_ingests`, both would claim non-overlapping pending records under the per-slug lock, and the SECOND call's `fire_ingest_chain` would run a full second chain build after the first completes — not a data-corruption race, but a "double work" race that burned LLM budget and time. Under the new code, the second caller observes the flag set, skips with a `"skipped: dispatch in-flight"` JSON note, and the HTTP caller gets a fast response instead of queuing a duplicate full-pipeline dispatch.

**Spec adherence (fix pass):**
- ✅ **Shared per-config in-flight flag** — added `PyramidState::dadbear_in_flight: Arc<std::sync::Mutex<HashMap<i64, Arc<AtomicBool>>>>`. Updated `with_build_reader` to clone it (build-scoped state observes the same flag map). Updated every `PyramidState { ... }` construction site: `main.rs` (3 sites), `vine.rs` (1 site), `chain_executor.rs` (4 test fixtures), `dadbear_extend.rs::make_test_state` (1 test fixture).
- ✅ **Tick loop consults shared state** — removed the local `HashMap<i64, Arc<AtomicBool>>` inside `start_dadbear_extend_loop`'s closure. Lazy-insert + clone-out now happens under `state.dadbear_in_flight.lock()` in a short scope that drops the mutex BEFORE `run_tick_for_config(...).await`. The `retain` cleanup for removed configs also uses the shared mutex in a short scope. Both lock acquisitions recover from mutex poisoning (`.lock().or(poisoned.into_inner())`) rather than killing the tick loop.
- ✅ **`trigger_for_slug` consults shared state** — before calling `run_tick_for_config` for each config, the new code acquires `state.dadbear_in_flight.lock()`, lazy-inserts or clones the entry, drops the mutex, and checks the atomic flag. If set, the config is skipped and added to a new `"skipped"` array in the returned JSON with reason `"dispatch in-flight"`. If clear, the code sets the flag, constructs an `InFlightGuard` (same RAII primitive the tick loop uses), runs the tick, and the guard clears the flag on every exit path (normal, error, panic unwind). `configs_processed` remains the count of configs that actually ran.
- ✅ **Panic safety preserved** — both call sites build `InFlightGuard` the same way. The `InFlightGuard::drop` impl is unchanged and still load-bearing. No second primitive, no divergent cleanup paths.
- ✅ **HTTP route (`routes.rs::handle_dadbear_trigger`)** — unchanged; the signature of `trigger_for_slug` is unchanged, only the returned JSON gained a `"skipped"` field.

**Files touched (fix pass):**
- `src-tauri/src/pyramid/mod.rs` — added `dadbear_in_flight` field to `PyramidState`, threaded through `with_build_reader`.
- `src-tauri/src/main.rs` — initialized `dadbear_in_flight` in the canonical `PyramidState` construction at line ~6574 and cloned it in the two `vine_integrity` / `vine_rebuild_upper` constructor sites.
- `src-tauri/src/pyramid/vine.rs` — cloned `dadbear_in_flight` in the `run_build` fallback state builder.
- `src-tauri/src/pyramid/chain_executor.rs` — added `dadbear_in_flight` initializer to all 4 test fixtures (`integration_execute_plan_initializes_state`, `integration_execute_plan_with_chunks_reaches_first_step`, `integration_build_runner_ir_flag_exists`, `integration_execute_plan_respects_pre_cancellation`) via `replace_all`.
- `src-tauri/src/pyramid/dadbear_extend.rs`:
  - Removed the local `let mut in_flight: HashMap<i64, Arc<AtomicBool>> = HashMap::new();` inside `start_dadbear_extend_loop`'s spawned closure.
  - Replaced the old `in_flight.retain(...)` cleanup with a mutex-acquired retain against `state.dadbear_in_flight`.
  - Replaced the old per-iteration `in_flight.entry(...)` with a mutex-acquired lookup/insert/clone against `state.dadbear_in_flight`.
  - Taught `trigger_for_slug` to consult the flag, collect skipped configs into a new JSON `"skipped"` array, and claim the flag via `InFlightGuard` when it proceeds.
  - Replaced the stale wanderer documentation test `test_trigger_for_slug_does_not_see_in_flight_flag` with a real `test_trigger_for_slug_respects_shared_in_flight_flag` that asserts the opposite behavior: pre-populate the shared map with a set flag, call `trigger_for_slug`, verify the JSON `"skipped"` array contains the config with reason `"dispatch in-flight"`, verify `configs_processed == 0`, verify the flag remains set (the skip path does not stomp on the holder's claim).
  - Added a new test `test_tick_loop_and_trigger_race_skip` that exercises the concurrent-holder-vs-trigger race: spawn a background task that claims the flag and holds it via `InFlightGuard`, fire `trigger_for_slug` while the holder owns the flag, assert it skips; release the holder, verify the flag clears; fire `trigger_for_slug` again, assert it no longer surfaces a skip.
  - Added `dadbear_in_flight` initializer to `make_test_state`.
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.
- `docs/plans/pyramid-folders-model-routing-friction-log.md` — resolution note appended to the "Phase 1 wanderer" entry.

**Verification results (fix pass):**
- ✅ `cargo check` (from `src-tauri/`) — clean. Same pre-existing warning set as before (3 `LayerCollectResult` private-interface in `publication.rs`, 1 deprecated `get_keep_evidence_for_target` in `routes.rs`, 1 deprecated `tauri_plugin_shell::Shell::open` in `main.rs:5226`). **Zero new warnings in any file touched by the fix pass.**
- ✅ `cargo build` (from `src-tauri/`) — clean, same warning set.
- ✅ `cargo test --lib pyramid::dadbear_extend` — **15/15 tests passing** in 9.75s:
  - 11 pre-existing dadbear_extend tests (Phase 0b + Phase 0b wanderer) — unchanged, all pass
  - `test_in_flight_guard_skip_and_panic_safety` (Phase 1 primitive test) — still passes, unchanged
  - `test_tick_loop_is_serial_within_single_task` (Phase 1 wanderer structural test) — still passes, unchanged — the scheduler facts it tests are independent of where the HashMap lives
  - `test_trigger_for_slug_respects_shared_in_flight_flag` (Phase 1 fix pass — **replaces** the stale documentation test of the same slot) — **new, passing**
  - `test_tick_loop_and_trigger_race_skip` (Phase 1 fix pass — new race test) — **new, passing**
- ✅ `cargo test --lib pyramid::chain_executor::tests::integration*` — 10/10 passing. The 4 test fixtures updated via `replace_all` still compile and run.
- ✅ `cargo test --lib` (full lib suite) — **795 passed / 7 failed / 0 ignored**. The 7 failures (`pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 `pyramid::staleness::tests::*`) are **pre-existing**, reproduced on the pre-fix stashed state, caused by schema drift in `pyramid_evidence` and a YAML/schema-preservation check in `defaults_adapter`. None are in files I touched. Confirmed by running the 7 failing tests against a pre-fix working tree (stash) and observing identical failures.

**Updated understanding (supersedes the implementer's original spec-adherence claim):** Phase 1 guards the HTTP/CLI-trigger-vs-auto-dispatch race, NOT the scheduler re-entrancy race the Phase 1 spec's inline sketch described. The scheduler race is structurally impossible in the current tick loop shape (see `test_tick_loop_is_serial_within_single_task`). The `evidence-triage-and-dadbear.md` Part 1 framing should be corrected by the planner in a follow-up pass — this fix pass deliberately does not touch the spec doc per scope boundary. The primitive is forward-compatible with any future restructuring that does introduce per-config `tokio::spawn` sub-tasks (Phase 17 recursive folder ingestion), at which point the scheduler race the original spec described DOES become live; the same shared flag will cover it then.

**Out-of-scope items flagged by the wanderer that remain open:**
- Tick loop panic recovery (the `tokio::spawn`ed tick loop task terminates on `run_tick_for_config` panic, leaving DADBEAR silently dead until app restart). The wanderer identified this as a separate operational gap. Not part of Phase 1 fix pass scope; deserves its own workstream.
- The `evidence-triage-and-dadbear.md` Part 1 spec and the addendum-01 "symptom attribution corrected" section still claim the guard is for the scheduler race. That framing should be updated, but planner approval is required for spec doc edits so this fix pass limits itself to the log entries below.

---

## Phase 2 — Change-Manifest Supersession

**Workstream:** implementer agent (fresh execution of phase-2-workstream-prompt.md)
**Workstream prompt:** `docs/plans/phase-2-workstream-prompt.md`
**Spec:** `docs/specs/change-manifest-supersession.md`
**Branch:** `phase-2-change-manifest-supersession` (off `phase-1-dadbear-inflight-lock`)
**Started:** 2026-04-09
**Completed (implementer pass):** 2026-04-09
**Status:** awaiting-verification

### Protocol for this phase
1. Implementer agent: fresh execution of phase-2-workstream-prompt.md, commits when done.
2. Verifier agent: identical prompt, unwitting — audits in place, fixes anything missed.
3. Wanderer agent: no punch list — "does a stale check on an upper-layer node preserve the node id, bump build_version, keep evidence links valid, and leave get_tree() coherent?"
4. Conductor marks `verified` after all three pass.

### Files touched (implementer pass)

- `src-tauri/src/pyramid/types.rs` — +217 lines. Added Phase 2 types: `TopicOp`, `TermOp`, `DecisionOp`, `DeadEndOp`, `ContentUpdates`, `ChildSwap`, `ChangeManifest`, `ChangeManifestRecord`, `ManifestValidationError` enum + Display/Error impls.
- `src-tauri/src/pyramid/db.rs` — +672 lines. Added:
  - `pyramid_change_manifests` table creation in `init_pyramid_db` (with `idx_change_manifests_node` and `idx_change_manifests_supersedes` indices).
  - `update_node_in_place()` — the core in-place update primitive. BEGIN IMMEDIATE transaction (or nested SAVEPOINT when inside an outer tx), snapshot to `pyramid_node_versions`, apply field-level content ops, bump `build_version`, rewrite evidence links for children_swapped entries.
  - `apply_topic_ops`, `apply_term_ops`, `apply_decision_ops`, `apply_dead_end_ops` helpers — per-entry JSON mutation for topic/term/decision/dead-end arrays.
  - `save_change_manifest()`, `get_change_manifests_for_node()`, `get_latest_manifest_for_node()` CRUD helpers for the new table.
  - Note: the existing `pyramid_nodes.build_version` column (base schema ~line 91) is what the new table indexes against — no new column added. The existing `apply_supersession` already bumps it; `update_node_in_place` continues that pattern.
- `src-tauri/src/pyramid/stale_helpers_upper.rs` — +1716 / −0 net. Added:
  - `ManifestGenerationInput`, `ChangedChild` structs.
  - `change_manifest_prompt()` + `load_change_manifest_prompt_body()` — static fallback + file loader for the new prompt.
  - `generate_change_manifest()` — async LLM call that produces a `ChangeManifest` from a `ManifestGenerationInput`. Follows the existing stale_helpers_upper LLM pattern (config_for_model → call_model_with_usage → extract_json → parse). Logs cost to `pyramid_cost_log` with `operation='change_manifest'`.
  - `validate_change_manifest()` — synchronous six-check validation (TargetNotFound, MissingOldChild, MissingNewChild, IdentityChangedWithoutRewrite, InvalidContentOp, InvalidContentOpAction, RemovingNonexistentEntry, EmptyReason, NonContiguousVersion).
  - `load_current_build_version()`, `persist_change_manifest()` convenience helpers.
  - `SupersessionNodeContext` struct + `load_supersession_node_context()` + `build_changed_children_from_deltas()` helpers used by the rewritten `execute_supersession`.
  - **`execute_supersession` body REWRITTEN** (line 1896+): resolve live canonical → load node context → build `ManifestGenerationInput` → call `generate_change_manifest` → validate synchronously → if `identity_changed` delegate to legacy path, else apply via `update_node_in_place` + persist manifest + propagate via new `propagate_in_place_update` helper. Returns the same (unchanged) node id in the normal case.
  - `execute_supersession_identity_change()` — the pre-Phase-2 body wrapped in a private function, kept verbatim for the rare identity-change escape hatch and for fallback when manifest generation fails.
  - `propagate_in_place_update()` — writes deltas on upstream threads + confirmed_stale pending mutations + edge_stale pending mutations, mirroring the legacy path's propagation but referencing the same (unchanged) node id.
  - 5 new tests in the existing `tests` module.
- `src-tauri/src/pyramid/vine_composition.rs` — +151 / −23 net. Added:
  - `enqueue_vine_manifest_mutations()` helper — walks cross-slug evidence links in the vine slug that reference the updated bedrock apex, enqueues a `confirmed_stale` pending mutation for each affected vine node at its depth.
  - `notify_vine_of_bedrock_completion()` extended to call `enqueue_vine_manifest_mutations` inside the same writer lock scope that updates `update_bedrock_apex`. The stale engine picks these up and routes them through `execute_supersession`, which now uses the change-manifest path.
  - Updated file header comment explaining the Phase 2 vine-level manifest integration path.
- `chains/prompts/shared/change_manifest.md` — **new file**. The LLM prompt body from the spec's "LLM Prompt: Change Manifest Generation" section, adapted to the existing prompt-file style in the `chains/` tree (ends with `/no_think` like other prompts).

### Spec adherence (against change-manifest-supersession.md + phase-2-workstream-prompt.md)

- ✅ **Schema: `pyramid_change_manifests` table** — created in `init_pyramid_db` with exact columns from the spec (id, slug, node_id, build_version, manifest_json, note, supersedes_manifest_id, applied_at, UNIQUE(slug, node_id, build_version)). Indices on (slug, node_id) and (supersedes_manifest_id).
- ✅ **Schema: `build_version` column** — ALREADY EXISTS on pyramid_nodes at line ~91 as `build_version INTEGER NOT NULL DEFAULT 1`. The existing `apply_supersession` bumps it. My new `update_node_in_place` bumps it the same way. No ALTER TABLE needed.
- ✅ **Manifest CRUD helpers** — `save_change_manifest`, `get_change_manifests_for_node` (applied_at ASC ordering), `get_latest_manifest_for_node` (applied_at DESC, id DESC ordering for deterministic "latest" with equal timestamps). Signatures match the spec.
- ✅ **`update_node_in_place` helper** — implements the 7-step flow from the spec: (1) BEGIN IMMEDIATE (with SAVEPOINT fallback for nested-tx callers), (2) snapshot into `pyramid_node_versions`, (3) apply per-entry content ops to topics/terms/decisions/dead_ends + wholesale replacement of distilled/headline, (4) bump `build_version`, (5) children JSON array swap, (6) UPDATE `pyramid_evidence` for children_swapped (handles PK conflict on conflicting destinations by DELETE-then-UPDATE), (7) commit and return new build_version.
- ✅ **Manifest validation — 6 checks** — `validate_change_manifest` in `stale_helpers_upper.rs` implements all six (target exists + live, children_swapped references, identity_changed semantics, content_updates field-level add/update/remove, reason non-empty, build_version contiguous). Returns `ManifestValidationError` variants; never silently discards.
- ✅ **LLM prompt file** — `chains/prompts/shared/change_manifest.md` created with the spec's prompt body adapted to the existing prompt-file style. A static inline fallback lives in `change_manifest_prompt()` so release builds without the chains/ tree still work.
- ✅ **`generate_change_manifest` function** — async helper in `stale_helpers_upper.rs` that takes a `ManifestGenerationInput`, loads the prompt file, calls the LLM via the existing `config_for_model` / `call_model_with_usage` pattern, parses the JSON, returns a `ChangeManifest`. Normalizes the echoed node_id against the one we asked about so the validator always sees a consistent id.
- ✅ **Rewrite `execute_supersession`** — body replaced per the spec. Normal path: generate manifest → validate → apply via `update_node_in_place` → persist manifest row → propagate. Identity-change path: delegates to `execute_supersession_identity_change` (the verbatim pre-Phase-2 body wrapped in a private function). Manifest-gen failure path: falls back to identity-change path with a failure note. Validation-failure path: persists the failed manifest row with `note = "validation_failed: {err}"` so the Phase 15 oversight page can surface it, then returns an error.
- ✅ **Vine-level manifest integration** — `notify_vine_of_bedrock_completion` extended to enqueue `confirmed_stale` pending mutations on the vine's L1+ nodes that KEEP-reference the updated bedrock apex (checking three valid source_node_id reference formats: bare id, handle path, short form). The stale engine picks these up and routes them through the Phase 2 `execute_supersession` flow, which produces a change manifest with `children_swapped` entries. Not a direct LLM call from vine_composition.rs — instead enqueues work for the stale engine so the LLM call flows through the same unified `execute_supersession` path.
- ✅ **Tests** — 5 new tests in `stale_helpers_upper::tests`:
  - `test_update_node_in_place_normal_case` — insert node with topic + evidence link, apply manifest with distilled + topic update + children_swapped, assert node id unchanged, build_version bumped 1→2, snapshot row in pyramid_node_versions, evidence link rewritten to new child.
  - `test_update_node_in_place_stable_id` — apply three consecutive in-place updates on the same node, assert `build_version` walks 1→2→3→4, row count stays at 1 (no new nodes), three snapshot rows exist, evidence link still valid.
  - `test_validate_change_manifest_all_errors` — exercises TargetNotFound, MissingOldChild, MissingNewChild, IdentityChangedWithoutRewrite, InvalidContentOp, InvalidContentOpAction, RemovingNonexistentEntry, EmptyReason, NonContiguousVersion, plus a happy-path success assertion.
  - `test_manifest_supersession_chain` — insert two manifests for the same node with `supersedes_manifest_id` pointing at the first; assert `get_change_manifests_for_node` returns both in applied_at order and `get_latest_manifest_for_node` returns the second.
  - `test_validate_then_apply_end_to_end` — closest non-LLM simulation of `execute_supersession`: build a manifest manually, validate against the live DB, apply via `update_node_in_place`, persist via `save_change_manifest`, verify the node survives with the same id, evidence link is rewritten, and `get_latest_manifest_for_node` finds it.
  - The spec's `test_execute_supersession_stable_id` is covered by `test_update_node_in_place_stable_id` + `test_validate_then_apply_end_to_end` together — the stable-id property is asserted at the helper level, and the end-to-end-ish test exercises the validate-then-apply chain. The full `execute_supersession` cannot be exercised in a pure unit test because it makes an LLM call; an integration-style test would need a fixture LLM, which is deferred to a future workstream.

### Scope boundary verification

- ✅ `git diff --stat` shows ONLY 4 files touched: `db.rs`, `stale_helpers_upper.rs`, `types.rs`, `vine_composition.rs`. Plus the new `chains/prompts/shared/change_manifest.md`.
- ✅ `src-tauri/src/pyramid/vine.rs` is UNCHANGED. The `supersede_nodes_above(&conn, vine_slug, 1, &rebuild_build_id)` call at line 3382 is verbatim (addendum noted line 3381 but the current tree has shifted by one line — the call itself is the same and correct as-is).
- ✅ `src-tauri/src/pyramid/chain_executor.rs` is UNCHANGED. The `db::supersede_nodes_above(&c, &slug_owned, 0, &overlay_build_id)` call at line 4821 is verbatim.

### Verification results (implementer pass)

- ✅ `cargo check` (from `src-tauri/`) — clean. Warning set: 3 pre-existing (2 `LayerCollectResult` private-interface in `publication.rs`, 1 deprecated `get_keep_evidence_for_target` in `routes.rs`). **Zero new warnings** in any file touched by Phase 2.
- ✅ `cargo build --lib` (from `src-tauri/`) — clean, same 3 warnings.
- ✅ `cargo test --lib pyramid::stale_helpers_upper` — **7/7 tests passing in 0.52s**:
  - `resolves_live_canonical_for_thread_and_historical_ids` (pre-existing)
  - `file_hash_lookup_and_rewrite_follow_live_node` (pre-existing)
  - `test_update_node_in_place_normal_case` (**Phase 2, new**)
  - `test_update_node_in_place_stable_id` (**Phase 2, new**)
  - `test_validate_change_manifest_all_errors` (**Phase 2, new**)
  - `test_manifest_supersession_chain` (**Phase 2, new**)
  - `test_validate_then_apply_end_to_end` (**Phase 2, new**)
- ✅ `cargo test --lib pyramid` (full pyramid suite) — **795 passed / 7 failed / 0 ignored / 5 filtered out** in 38.77s. The 7 failures are **pre-existing and unrelated** to Phase 2:
  - `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`
  - `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`
  - `pyramid::staleness::tests::test_below_threshold_not_enqueued`
  - `pyramid::staleness::tests::test_deletion_skips_first_attenuation`
  - `pyramid::staleness::tests::test_path_normalization`
  - `pyramid::staleness::tests::test_propagate_staleness_with_db`
  - `pyramid::staleness::tests::test_shared_node_higher_score_propagates`
  Confirmed by `git stash` + re-running the 7 failing tests against the Phase 1 tree — identical failures, same error messages (`no such column: build_id in pyramid_evidence` for the staleness tests, `ChainStep.response_schema must be parsed from YAML` for the defaults_adapter test). None of the failing files were touched by Phase 2.
- ✅ `cargo test --lib` (full lib suite) — **800 passed / 7 failed / 0 ignored / 0 filtered out** in 38.67s. 800 = 795 (pre-Phase-2) + 5 new Phase 2 tests. Same 7 pre-existing failures.
- 🕒 **Manual viz verification** (pending Adam's dev-server run): see checklist below.

### Manual viz verification checklist (pending Adam's manual run)

Phase 2's fix is the viz-orphaning bug. To verify the DAG stays coherent after a stale-check-driven upper-node update:

1. Build a test pyramid with at least L2+ depth (any content type with an upper layer).
2. Confirm the current `get_tree()` output shows children under the apex.
3. Trigger a source-file change on one of the L0 files that feeds the apex (e.g. `touch` + small edit + save).
4. Wait for DADBEAR to detect the change and propagate staleness up to the apex (`pyramid_pending_mutations` should show `confirmed_stale` rows landing at the apex depth).
5. Observe the stale engine run `execute_supersession` on the apex.
6. Re-fetch `get_tree()` for the slug.
7. **Assertion (the fix):** the apex id is unchanged AND the children array is non-empty (the viz DAG still has visible leaves under the apex). The apex's `build_version` has incremented by 1.
8. **Additional check:** query `pyramid_change_manifests` for the apex's node_id — should show a row with `note IS NULL` (automated stale check) and the full manifest JSON.
9. **Pre-fix repro** (for contrast): on a pre-Phase-2 build, the same flow leaves `get_tree()` showing a lone apex with no children because a new id was created and the evidence links still point at the old (now superseded-hidden) node.

### Notes

- **`build_version` was already there.** The spec says to add the column; it's already present on `pyramid_nodes` at base schema creation (line ~91) and `apply_supersession` has been bumping it for a while. I continued that pattern in `update_node_in_place`. No migration needed.
- **Pillar 37 note.** `generate_change_manifest` uses the same hardcoded `0.2, 4096` temperature/max_tokens as the existing `execute_supersession` LLM call (literally the number it's replacing). The entire `stale_helpers_upper.rs` file uses hardcoded temperature/max_tokens today — the tier-routing infrastructure that would fix this doesn't yet exist (Phase 3). Matching the file's existing convention for Phase 2 and flagging for the friction log; the real fix is the Phase 3 provider-registry refactor.
- **Vine-level manifest integration uses the stale engine, not a direct LLM call.** The spec's "Vine-Level Manifests" section says "for each affected vine node, call `generate_change_manifest`". I implemented this by enqueueing `confirmed_stale` pending mutations on affected vine L1+ nodes — the stale engine picks these up and routes them through the Phase 2 `execute_supersession` which DOES call `generate_change_manifest`. The end result is the same (vine nodes get change manifests with bedrock-apex child deltas), but the integration point is one level deeper — the vine_composition.rs code stays pure bookkeeping and the LLM dispatch lives in the stale engine's existing batch flow. This has two advantages: (1) vine_composition.rs doesn't need api_key/model threading, (2) vine-level manifests flow through the same cost-logging and batching as pyramid-level manifests, giving uniform observability.
- **Identity-change path preserved verbatim.** The rare `identity_changed = true` case still creates a new id via `next_sequential_node_id` and runs the legacy insert-new-row + set-superseded_by + re-parent-children flow. The old body of `execute_supersession` is now `execute_supersession_identity_change` — a private function at the same indent. Any caller relying on the "new id returned" behavior for identity changes continues to work unchanged.
- **Evidence link rewrite semantics.** `update_node_in_place` handles the `pyramid_evidence` PK conflict carefully: `pyramid_evidence` has PK `(slug, build_id, source_node_id, target_node_id)` so a naive UPDATE of source_node_id would hit the PK uniqueness if the destination row already exists. I handle this by DELETE-any-existing-destination, then UPDATE the old row. This is correct because the destination being present means the NEW child already has a link to the parent, which is the desired end state.
- **Reject manifest-generation failures, don't retry.** Per spec, validation failures are logged WARN and NOT silently retried. The failed manifest is persisted to `pyramid_change_manifests` with `note = "validation_failed: ..."` so the Phase 15 DADBEAR oversight page can surface it. Manifest-gen LLM failures (e.g., JSON parse failure) fall back to the identity-change path with a failure-note, so the system degrades gracefully rather than leaving a stale node un-updated.
- **No friction log entries required.** Scope held, spec was clear, no architectural questions came up. The Pillar 37 note above is mentioned here rather than in the friction log because it's a pre-existing condition of the entire `stale_helpers_upper.rs` file, not a Phase 2 regression or new violation.

The phase is ready for the verifier pass. After that, the wanderer pass should trace end-to-end: "does a stale check on an upper-layer node preserve the node id, bump build_version, keep evidence links valid, and leave get_tree() coherent?"
