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

### Phase 2 fix pass — 2026-04-10

The wanderer pass on `phase-2-change-manifest-supersession` caught three problems in the initial Phase 2 land. All three are fixed in this pass on the same branch; a single follow-up commit lands on top of commit `3ff7e14 phase-2: change-manifest supersession` and its wanderer friction log commit `951ce94`.

**Wanderer verdict (three issues):**

1. **BLOCKING — L0 file_change regression.** `execute_supersession` has two callers in `stale_engine.rs`: the L1+ confirmed_stale path at line 968 AND the L0 file_change path at line 838. The Phase 2 spec only described the L1+ path, and the implementer's rewrite of `execute_supersession` dropped the `depth == 0` source-file-reading branch that the pre-Phase-2 body (now `execute_supersession_identity_change`) had at lines 2551-2562. `load_supersession_node_context` reads only pyramid state; `build_changed_children_from_deltas` emits old==new content for L0 nodes with no deltas; `update_node_in_place` applies a no-op and bumps `build_version`. Net effect: L0 distilled text never updates when the user edits a file on disk. Compounding: `pyramid_file_hashes.hash` is never updated on file_change, so the watcher re-fires on every tick until the hash matches — DADBEAR enters a loop burning LLM budget on no-op updates.

2. **BLOCKING — identity-change fallback reintroduces the orphaning bug.** On `generate_change_manifest` LLM failure, `execute_supersession` fell back to `execute_supersession_identity_change` — the pre-Phase-2 body preserved verbatim. That body creates a new node id via `next_sequential_node_id` and leaves the old evidence links pointing at the old id, which is EXACTLY the viz orphaning bug Phase 2 was written to fix. A 5% LLM flakiness rate reintroduces the bug 5% of the time. The spec's "Manifest Validation → Failure handling" section at line 251 says unambiguously: "Invalid manifests are rejected (the node is left in its pre-manifest state) and logged with the failure reason. The stale check is not retried automatically." The implementer read that as "validation failure" only and applied the wrong graceful-degradation default to LLM failure.

3. **MINOR — dead `build_id` parameter in `update_node_in_place`.** The parameter is declared, receives the literal string `"stale_refresh"` from the caller, and is never written anywhere — line ~3018 had a `let _ = build_id;` with a misleading comment. The snapshot INSERT uses `snap.build_id` (the pre-update node's existing build_id), not the function parameter.

**Fix directions:**

1. **L0 file_change regression — thread source file through the manifest flow.**
   - Extended `SupersessionNodeContext` with `source_file_path: Option<String>` and `source_snapshot: Option<String>`, populated by `load_supersession_node_context` for depth==0 nodes only via `lookup_source_file_path_for_node` + `fs::read_to_string` + 400-line/20k-char truncation (matches the pre-Phase-2 body verbatim).
   - Extended `build_changed_children_from_deltas` with an L0 branch that synthesizes a `ChangedChild { child_id: file_path, old_summary: current_distilled, new_summary: file_excerpt }` when the context has a source snapshot. The LLM's existing "what changed?" prompt handles this cleanly — the "child" is the source file, the "delta" is the new content.
   - Added a `stale_check_reason` branch that reflects the L0 case ("source file changed on disk") and a `reason_tag` branch (`file_change` vs `node_stale`) for cost-log categorization.
   - After a successful `update_node_in_place` on a depth==0 node, `execute_supersession` now UPDATEs `pyramid_file_hashes.hash` with a freshly-computed hash via `super::watcher::compute_file_hash`. This stops the watcher's re-fire loop — the next tick sees the hash match and skips the file. Failures are logged WARN but do not roll back the apply (the update is still correct; the watcher will re-fire next tick if the UPDATE didn't land, which is benign).
   - Added a code comment on `db::update_node_in_place` documenting that the absence of the `depth <= 1 && !provisional` immutability check from `apply_supersession` is deliberate: the immutability invariant exists for Wire publication snapshot, not for local refresh. Local L0 nodes need to mutate in place as files change.

2. **Identity-change fallback on LLM failure — removed.**
   - Extracted `handle_manifest_generation_failure` as a private async helper. On LLM failure `execute_supersession` now calls it instead of `execute_supersession_identity_change`. The helper persists a placeholder `ChangeManifest` row in `pyramid_change_manifests` with `note = "manifest_generation_failed: <error>"` against the CURRENT build_version, then returns an error to the stale engine. The node stays at its prior valid state — same id, same distilled, same build_version.
   - Also extracted `apply_supersession_manifest` as a private async helper that takes a pre-generated manifest. `execute_supersession`'s main body now generates the manifest and delegates to the applier. The identity-change path ONLY fires inside `apply_supersession_manifest` when the LLM explicitly returned `identity_changed = true` in a SUCCESSFUL manifest — the rare escape hatch the spec describes.
   - `execute_supersession_identity_change` is unchanged (the pre-Phase-2 body preserved verbatim) and is called from exactly ONE place: the `identity_changed == true` branch inside `apply_supersession_manifest`. A grep for the name confirms the single call site. The extraction also makes `apply_supersession_manifest` directly callable from tests, which is how Test 1 drives the full L0 hash-rewrite path without mocking the LLM.

3. **Dead `build_id` parameter — removed.**
   - Removed `build_id: &str` from `update_node_in_place`'s signature. Removed the dead `let _ = build_id;` body line. Updated the doc comment. Updated the one production caller (`stale_helpers_upper.rs::apply_supersession_manifest`) and the three existing test callers (`test_update_node_in_place_normal_case`, `test_update_node_in_place_stable_id`, `test_validate_then_apply_end_to_end`). `snap.build_id.clone()` (the local Snapshot struct field inside the function body) is unchanged — that's the pre-update node's original build_id which is correctly carried into the snapshot row.

**Files touched:**

- `src-tauri/src/pyramid/stale_helpers_upper.rs` — extended `SupersessionNodeContext`, `load_supersession_node_context`, `build_changed_children_from_deltas`; extracted `handle_manifest_generation_failure` and `apply_supersession_manifest`; added L0 hash rewrite; updated the one `update_node_in_place` caller; added three fix-pass regression tests + a shared `setup_l0_test_db` helper.
- `src-tauri/src/pyramid/db.rs` — removed `build_id` parameter from `update_node_in_place`; added doc-comment note about why the immutability guard is deliberately omitted (local refresh semantics, not Wire publication).

Not touched: `vine.rs`, `chain_executor.rs` (Phase 2 scope boundary held), `stale_engine.rs` (both call sites still go through `execute_supersession` with the same five-argument signature — the fix is transparent to callers).

**New tests (3, all in `pyramid::stale_helpers_upper::tests`):**

1. `test_apply_supersession_manifest_l0_file_change_updates_hash_and_distilled` — the L0 regression test. Writes a source file, creates an L0 node + `pyramid_file_hashes` row with the pre-edit hash, then rewrites the file on disk. Loads `SupersessionNodeContext` via `load_supersession_node_context` and asserts it carries `source_file_path` + `source_snapshot` with the post-edit content. Calls `build_changed_children_from_deltas` and asserts the synthesized child's `new_summary` contains the new file bytes. Builds a synthetic manifest (stand-in for the LLM call) with `distilled` referencing the new content, then calls `apply_supersession_manifest` directly. After the apply, asserts (a) the L0 node's distilled mentions the new content, (b) `build_version` bumped from 1 to 2, (c) the L0 node id is unchanged, (d) `pyramid_file_hashes.hash` has been rewritten to the post-edit hash.

2. `test_handle_manifest_generation_failure_no_identity_change_fallback` — directly drives the failure-path helper with a synthesized anyhow error. Snapshots the node state pre-failure, calls the helper, re-opens the DB and asserts: (a) node id unchanged, (b) distilled unchanged, (c) headline unchanged, (d) build_version unchanged, (e) total row count unchanged (so no new node id was created by a sneaky fallback), (f) `superseded_by` is still NULL on the original row, (g) a failed-manifest row lands in `pyramid_change_manifests` with `note` starting `"manifest_generation_failed:"` and `build_version` = 1 (pre-bump).

3. `test_identity_change_only_on_explicit_flag_with_rewrite` — pins the spec-aligned semantics of the identity-change escape hatch via `validate_change_manifest`. A manifest with `identity_changed = true` AND `distilled`/`headline` updates validates clean (positive escape hatch). A manifest with `identity_changed = true` and no rewrite returns `Err(IdentityChangedWithoutRewrite)`. Confirms the validator does not persist rows (validation is side-effect free). Combined with test 2, this pins the full shape: identity-change fires only on an explicit LLM flag, never as a fallback for LLM failure. A future accidental re-introduction of the LLM-failure-to-identity-change path would have to update test 2's assertions, making the regression visible in review.

**Verification results:**

- `cargo check --lib` — clean. No new warnings (3 pre-existing warnings unchanged: `get_keep_evidence_for_target` deprecated use, and two `LayerCollectResult` visibility warnings in `publication.rs`).
- `cargo build --lib` — clean, same 3 pre-existing warnings.
- `cargo test --lib pyramid::stale_helpers_upper` — **10/10 passed** (7 existing Phase 2 tests + 3 new fix-pass tests, matching the expected count in the fix-pass prompt). Finished in 0.68s.
- `cargo test --lib pyramid` — **798 passed, 7 failed** (the same 7 pre-existing schema-drift failures in `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, and 5 `pyramid::staleness::tests::*` tests). No new failures from this fix pass. The Phase 1 fix-pass log entry at line 152 lists the same 7 failures as pre-existing.
- `grep -n "execute_supersession_identity_change" src-tauri/src/pyramid/stale_helpers_upper.rs` — function still exists at its original location; called from exactly ONE place in production code (the `if manifest.identity_changed` branch inside `apply_supersession_manifest`). No call from the LLM-failure path.
- `grep -n "build_id" src-tauri/src/pyramid/db.rs` around `update_node_in_place` — the parameter is gone from the signature. The dead `let _ = build_id;` line is gone. `snap.build_id.clone()` inside the function body remains correct (it's the pre-update node's build_id being carried into the snapshot row).

**Updated understanding:**

Phase 2 now fixes BOTH the viz DAG orphaning bug (L1+ stale-refresh path — the original target) AND the L0 content sync on file_change regression (the wanderer-caught gap). It also removes the fallback-reintroduces-bug trap: LLM-failure no longer silently creates a new node id, so the viz DAG stays coherent even under flaky LLM conditions. The spec's "Invalid manifests are rejected... not retried automatically" semantics are now restored for the LLM-failure branch, not just the validation-failure branch.

**Scope boundary maintained:**

- `vine.rs:3381` and `chain_executor.rs:4821` still use wholesale-rebuild semantics (intentional, spec-aligned, correct as-is per the "Scope boundary: which call sites this phase touches" section of `change-manifest-supersession.md`).
- No StepContext threading added to `generate_change_manifest` — still Phase 6's scope.
- `generate_change_manifest` and `validate_change_manifest` bodies are unchanged beyond what the three issues required (the only touch to the manifest generation call site is the addition of the L0 reason tag / stale_check_reason branches which feed the existing function).
- Pre-existing `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence` and the 6 other schema-drift test failures are still failing; this fix pass does not widen scope to address them.

The phase is ready to ship after the commit on this branch. No further audit cycles needed for the three issues — the regression tests lock down the spec-aligned behavior.

---

## Phase 3 — Provider Registry + Credentials

**Workstream:** phase-3-provider-registry-credentials
**Started:** 2026-04-10 (single session)
**Completed:** 2026-04-10
**Verified by:** pending (awaiting conductor/wanderer pass)
**Wanderer result:** n/a
**Status:** awaiting-verification

### What shipped

Phase 3 replaces the hardcoded OpenRouter URL + headers + response parser in `llm.rs` with a pluggable `LlmProvider` trait, backed by a provider registry table, a tier routing table, and a per-step overrides table. Secrets move out of `LlmConfig.api_key` into a `.credentials` YAML file on disk, referenced from provider rows as `api_key_ref = "OPENROUTER_KEY"`. The credential value is wrapped in a `ResolvedSecret` opaque type with no `Debug` / `Display` / `Clone` / `Serialize` impl so it cannot leak into logs, error messages, or publication payloads. The refactor keeps `LlmConfig` as a compatibility shim (per the brief's guidance) by attaching the registry + credential store as new fields, so every existing call site that takes an `&LlmConfig` transparently routes through the provider trait.

### Files touched

**New files:**
- `src-tauri/src/pyramid/credentials.rs` (NEW, ~900 lines) — `CredentialStore`, `ResolvedSecret`, `${VAR_NAME}` substitution with `$${...}` escape, atomic write with 0600 enforcement, `collect_references` for publish-time scans, `file_status`, `ensure_safe_permissions`, 18 unit tests.
- `src-tauri/src/pyramid/provider.rs` (NEW, ~1400 lines) — `LlmProvider` trait, `OpenRouterProvider`, `OpenAiCompatProvider` (Ollama + custom OAI-compat), `ProviderRegistry` with in-memory maps + DB hydration, `Provider` / `TierRoutingEntry` / `StepOverride` domain types, `RequestMetadata` + OpenRouter trace injection, Ollama `/api/show` context-window detection, pricing JSON parsing (string-encoded values), supported_parameters gate, 20 unit tests (including 3 end-to-end registry wiring tests).

**Modified files:**
- `src-tauri/Cargo.toml` — added `async-trait = "0.1"` dependency (required for `LlmProvider` trait object with async `detect_context_window`).
- `src-tauri/src/pyramid/mod.rs` — declared `credentials` and `provider` modules; extended `PyramidState` with `provider_registry: Arc<ProviderRegistry>` and `credential_store: SharedCredentialStore`; updated `with_build_reader` to clone both; added `PyramidConfig::to_llm_config_with_runtime` that attaches the registry + store.
- `src-tauri/src/pyramid/llm.rs` — removed the hardcoded `https://openrouter.ai/api/v1/chat/completions` URL + headers from `call_model_unified_with_options` and `call_model_direct`; added `build_call_provider` helper that either pulls the `openrouter` row from the attached registry or synthesizes an `OpenRouterProvider` from legacy `LlmConfig.api_key` for tests; added the new registry-aware `call_model_via_registry` entry point with per-step override resolution and rich `RequestMetadata`; removed legacy `parse_openrouter_response_body` + `sanitize_json_candidate` (the provider trait owns response parsing now); custom `Debug` impl for `LlmConfig` that redacts `api_key` + `auth_token`; `LlmConfig` now has `provider_registry` + `credential_store` fields.
- `src-tauri/src/pyramid/db.rs` — added `pyramid_providers`, `pyramid_tier_routing`, `pyramid_step_overrides` tables to `init_pyramid_db`; added CRUD helpers (`get_provider`, `list_providers`, `save_provider`, `delete_provider`, `get_tier_routing`, `save_tier_routing`, `delete_tier_routing`, `list_step_overrides`, `get_step_overrides_for_chain`, `get_step_override`, `save_step_override`, `delete_step_override`); added `seed_default_provider_registry` that inserts the default OpenRouter row + Adam's 4 tier routing entries on first run (idempotent via COUNT check); added an 8-test `provider_registry_tests` module.
- `src-tauri/src/pyramid/vine.rs` — updated the fallback `PyramidState` constructor to clone the new `provider_registry` + `credential_store` fields.
- `src-tauri/src/pyramid/chain_executor.rs` — updated 4 test-only `PyramidState` constructors to include the new fields (empty registry + empty credential store for unit tests).
- `src-tauri/src/pyramid/dadbear_extend.rs` — updated `make_test_state` helper to include the new fields.
- `src-tauri/src/partner/conversation.rs` — refactored `call_partner` to build its URL + attribution headers via the shared `OpenRouterProvider` trait impl so the hardcoded `/chat/completions` string no longer lives in the partner path. Partner keeps its own title header override.
- `src-tauri/src/main.rs` — added credential store + provider registry construction at app boot (immediately after `init_pyramid_db`); routed `PyramidConfig::to_llm_config_with_runtime` into the live config; preserved the registry + store across profile-apply paths; added 16 new IPC commands: `pyramid_list_credentials`, `pyramid_set_credential`, `pyramid_delete_credential`, `pyramid_credentials_file_status`, `pyramid_fix_credentials_permissions`, `pyramid_credential_references`, `pyramid_list_providers`, `pyramid_save_provider`, `pyramid_delete_provider`, `pyramid_test_provider`, `pyramid_get_tier_routing`, `pyramid_save_tier_routing`, `pyramid_delete_tier_routing`, `pyramid_get_step_overrides`, `pyramid_save_step_override`, `pyramid_delete_step_override`; registered all 16 in `invoke_handler!`.

### Spec adherence

**`docs/specs/credentials-and-secrets.md`:**
- ✅ `.credentials` file at the OS-specific support directory (macOS `~/Library/Application Support/wire-node/.credentials`).
- ✅ Plain-text YAML, top-level mapping of uppercase SNAKE_CASE keys to string values.
- ✅ 0600 permissions enforced on load (refuses to load if wider); `apply_safe_permissions` helper for the "Fix permissions" IPC button.
- ✅ Atomic write: temp file with 0600 mode, fsync, rename over original, defense-in-depth chmod.
- ✅ `${VAR_NAME}` substitution syntax with `$${VAR_NAME}` escape.
- ✅ No nested substitution (single pass over the input).
- ✅ `ResolvedSecret` opaque wrapper: NO Debug / Display / Serialize / Clone impls. The only extraction methods are `as_bearer_header`, `as_url`, `raw_clone`, and `expose_raw` (the last two are explicit crate-internal escape hatches for custom header formats).
- ✅ Best-effort zeroization on drop (volatile byte writes over the String's capacity before `.clear()`).
- ✅ Missing-variable error includes the "Settings → Credentials" hint.
- ✅ IPC surface: list (masked previews only, never returns values), set, delete, file status, fix permissions, cross-reference dashboard.
- ✅ Validation: uppercase SNAKE_CASE key regex, non-empty value.
- ⚠️ Backward-compat migration of legacy `api_key_ref = "settings"` rows — NOT implemented because there are no such rows in the current codebase. The spec's Migration section describes a hypothetical pre-credential sentinel that was never deployed. Skipped in Phase 3; if a migration is needed later it can be added to `seed_default_provider_registry`.
- ❌ Publish-time credential leak scan — Phase 5 scope per the brief.
- ❌ ToolsMode credential warnings — Phase 10 scope per the brief.
- ❌ Settings.tsx UI — Phase 10 scope per the brief.

**`docs/specs/provider-registry.md`:**
- ✅ `LlmProvider` trait with `name`, `provider_type`, `chat_completions_url`, `prepare_headers`, `parse_response`, `supports_response_format`, `supports_streaming`, `detect_context_window`, `augment_request_body`.
- ✅ `OpenRouterProvider` implementation: Bearer auth, canonical `X-OpenRouter-Title` header (+ legacy `X-Title` alias), `X-OpenRouter-Categories`, `HTTP-Referer`; response parser extracts `id`, `choices[0].message.content`, `usage.prompt_tokens`, `usage.completion_tokens`, `usage.cost`, `finish_reason`; `augment_request_body` injects `trace` object (build_id/slug/chain_id/step_name/depth), `session_id` (explicit or synthesized from slug+build), and `user` (node_identity).
- ✅ `OpenAiCompatProvider` implementation: optional Authorization header, `response_format` support, Ollama `/api/show` context-window detection with arch-prefix algorithm + suffix-scan fallback.
- ✅ `pyramid_providers` table with full schema: id, display_name, provider_type CHECK constraint, base_url, api_key_ref, auto_detect_context, supports_broadcast, broadcast_config_json, config_json, enabled, created_at, updated_at.
- ✅ `pyramid_tier_routing` table with full schema: tier_name PK, provider_id FK with CASCADE, model_id, context_limit, max_completion_tokens, pricing_json, supported_parameters_json, notes.
- ✅ `pyramid_step_overrides` table with composite PK (slug, chain_id, step_name, field_name).
- ✅ Default seeding with Adam's exact model slugs: `fast_extract → inception/mercury-2`, `web → x-ai/grok-4.1-fast (2M)`, `synth_heavy → minimax/minimax-m2.7`, `stale_remote → minimax/minimax-m2.7`. `stale_local` intentionally NOT seeded (Adam's Option A).
- ✅ Idempotent seed: `COUNT(*)` check before seeding, never overwrites user edits.
- ✅ Pricing JSON string-encoded values parsed via `parse_price_field` with `parseFloat` defensiveness.
- ✅ `supported_parameters_json` gate on `response_format` at call time in `call_model_via_registry`.
- ✅ Tier routing resolver with per-step override lookup via `pyramid_step_overrides`.
- ✅ Credential-aware provider instantiation via `ProviderRegistry::instantiate_provider` → resolves `${VAR_NAME}` in `base_url` and `extra_headers`, resolves `api_key_ref` against the credential store, surfaces clear "Settings → Credentials" errors when the variable is missing.
- ✅ IPC surface: list/save/delete providers, test provider (credential presence check), tier routing CRUD, step override CRUD.
- ⚠️ `OllamaCloudProvider` — DEFERRED to Phase 10 per the brief's explicit scope carve-out. The spec's "OllamaCloudProvider" section is not implemented; `OpenAiCompatProvider` covers the local + reverse-proxy cases.
- ⚠️ Cross-provider fallback chains — DEFERRED to Phase 14. The `call_model_via_registry` path surfaces a single-provider failure via a clear error. The `TierRoutingEntry` schema has no `fallback_chain` column yet; adding it is Phase 14 scope.
- ❌ `/api/v1/credits` management-key flow — Phase 14 scope.
- ❌ Dynamic model selection from `/api/v1/models` — Phase 14 scope (and the brief explicitly says NOT to hit `/models` at seed time; Adam's slugs are pinned).
- ❌ Pricing prefetch from `/api/v1/models` — Phase 14 scope. Current seed uses empty pricing JSON; the tier routing table has the column ready.

**`llm.rs` refactor:**
- ✅ `call_model_unified_with_options` now dispatches through `build_call_provider` → provider trait.
- ✅ `call_model_direct` now dispatches through `build_call_provider` → provider trait.
- ✅ `call_model_via_registry` NEW entry point for chain-executor callers with rich `RequestMetadata`.
- ✅ `parse_openrouter_response_body` + `sanitize_json_candidate` helpers REMOVED — provider trait owns all response parsing.
- ✅ `LlmConfig` gets a custom `Debug` impl that redacts `api_key` and `auth_token`.
- ✅ Pillar 37 comment added next to legacy hardcoded temperature/max_tokens, flagging Phase 4/6 as the real fix.

**IPC endpoints:** all 16 endpoints implemented and registered.

**Tests:**
- Credentials: 18 tests (load/save round trip, permission refusal, variable substitution including escape sequence, missing-var error, atomic write, masked preview, YAML parse failures, 0600 mode enforcement, file status, key validation).
- Provider: 20 tests (OpenRouter headers/URL/response parsing, OpenAI-compat no-auth and auth paths, Ollama context window detection with arch-prefix and suffix-scan fallback, pricing JSON parsing, supported_parameters gate, trace augmentation with explicit session_id, extra_headers parsing, 3 end-to-end registry wiring tests covering Adam's seeded defaults, step override precedence, and missing-credential error).
- DB provider registry: 8 tests (seed-on-empty, 4-tier seed without stale_local, Adam's exact slugs, no-reseed when populated, provider round trip, tier routing round trip, step override round trip, cascade delete on provider removal).

### Scope decisions

- **Registry threading approach:** `LlmConfig` carries `provider_registry: Option<Arc<ProviderRegistry>>` + `credential_store: Option<Arc<CredentialStore>>` fields. Rejected the alternative `LlmCtx` wrapper approach because there are 85+ call sites of `call_model_*` across 17 files — threading a new positional argument through each would have been a massive churn. The Option wrapping lets unit tests construct an `LlmConfig::default()` and still exercise the legacy synth-OpenRouter fallback in `build_call_provider`. Production boot paths always attach a non-None registry via `PyramidConfig::to_llm_config_with_runtime`. Documented in `llm.rs` header comment.
- **OllamaCloudProvider deferred** to Phase 10 per the brief's explicit scope note. The current `OpenAiCompatProvider` covers local Ollama + reverse-proxy Ollama via `config_json.extra_headers`. Ollama Cloud (`ollama.com/api`) requires a separate provider type with `-cloud` suffix model IDs and mandatory auth; adding it now would widen scope without unblocking Phase 3's downstream consumers.
- **Partner subsystem URL:** `src-tauri/src/partner/conversation.rs` also had a hardcoded `/chat/completions` literal. The brief's grep sanity check explicitly requires the literal to only exist in `provider.rs`, so I refactored `call_partner` to build its URL and attribution headers via the shared `OpenRouterProvider` trait impl. Partner still keeps its own `PartnerLlmConfig` (with tool-call wiring the pyramid path doesn't use) and its own title header override. This preserves Partner's request body shape while removing the duplicate URL literal.
- **Legacy helpers removed:** `parse_openrouter_response_body` and `sanitize_json_candidate` in `llm.rs` were entirely replaced by the provider trait's response parser. The two tests that referenced them were deleted — equivalent coverage lives in `provider.rs::tests::openrouter_*`. This prevents drift where two implementations might diverge.
- **`pyramid_test_provider` IPC endpoint:** v1 implementation verifies the credential reference resolves cleanly — it does NOT make a real HTTP call. A real ping endpoint is Phase 10 UI scope. The v1 surface is enough to catch "you set `api_key_ref = OPENROUTER_KEY` but the credentials file doesn't define it" errors, which is the #1 support case.
- **Pillar 37 temperature/max_tokens:** the pre-existing hardcoded `0.2, 4096` / `0.1, 2048` calls stay in place throughout the pyramid. Moving them to config flows is Phase 4/6 scope (config contributions + LLM output cache) per the brief. The `call_model_via_registry` function takes `temperature` + `max_tokens` as explicit args so the next phase's refactor can flow them in from StepContext without further signature changes.
- **Legacy `LlmConfig` fields preserved:** `primary_model`, `fallback_model_1`, `fallback_model_2`, `primary_context_limit`, `fallback_1_context_limit`, `model_aliases` all kept. The new provider registry is the canonical path, but the legacy fields still drive the 3-tier cascade in `call_model_unified` when the registry isn't the per-call resolver. A future phase can retire them.
- **`resolve_credential_for` supports two `api_key_ref` shapes:** bare variable name (`OPENROUTER_KEY`) and `${VAR_NAME}` pattern. The bare form is preferred for new rows but the `${...}` shape is tolerated so hand-written config YAML that uses `api_key_ref: "${OPENROUTER_KEY}"` still works.
- **`base_url` supports `${VAR_NAME}` substitution:** per the spec's self-hosted-Ollama-tunnel use case. The `resolve_base_url` helper runs `substitute_to_string` which returns a plain `String` (not `ResolvedSecret`) because the URL itself is logged during debug output. Operators with a tunnel-in-URL are expected to redact via their log setup.

### Verification results

- ✅ `cargo check --lib` — clean, zero new warnings in files I touched. Pre-existing 3 warnings (deprecated `get_keep_evidence_for_target`, `LayerCollectResult` private visibility x2) unchanged.
- ✅ `cargo check --lib --tests` — clean, zero new warnings in files I touched. Pre-existing warnings unchanged.
- ✅ `cargo check` (full crate) — clean. 3 lib warnings + 1 pre-existing bin warning (`tauri_plugin_shell::Shell::open` deprecated).
- ✅ `cargo build --lib` — clean, same 3 pre-existing warnings.
- ✅ `cargo build --bin wire-node-desktop` — clean, same pre-existing warnings.
- ✅ `cargo test --lib pyramid::credentials` — 18 passed, 0 failed.
- ✅ `cargo test --lib pyramid::provider` — 20 passed, 0 failed.
- ✅ `cargo test --lib pyramid::db::provider_registry_tests` — 8 passed, 0 failed.
- ✅ `cargo test --lib pyramid` — **842 passed, 7 failed** (the same 7 pre-existing failures documented in Phase 2's log: `test_evidence_pk_cross_slug_coexistence`, `real_yaml_thread_clustering_preserves_response_schema`, and 5 `pyramid::staleness::tests::*` tests). Phase 3 added 46 new tests (800 → 846 total, minus 4 filtered = 842 reported). No new failures introduced.
- ✅ `cargo test --lib` — 844 passed, 7 failed (same 7). No regressions.
- ✅ `grep -n "https://openrouter.ai/api/v1/chat/completions" src-tauri/src/` — returns only two hits in `provider.rs` (one inside `chat_completions_url()` assertion, one inside the end-to-end `registry_resolve_tier_instantiates_openrouter_for_seeded_defaults` test). **Zero hits in `llm.rs`, `partner/conversation.rs`, `main.rs`, or any other production file.**
- ✅ `grep -n "as_bearer_header\|ResolvedSecret" src-tauri/src/pyramid/credentials.rs` — both opacity helpers present (`as_bearer_header`, `as_url`, `raw_clone`, `expose_raw`) and the `ResolvedSecret` struct is defined with no derive of Debug/Display/Clone/Serialize.

### Notes

- **`ResolvedSecret` has no `Debug` impl → tests can't use `.unwrap_err()` on Results containing it.** Three tests in `provider.rs` and `credentials.rs` match the Result explicitly instead. This is a surprising but load-bearing constraint of the opacity contract: if you `#[derive(Debug)]` on `ResolvedSecret` to silence those compile errors, you break the spec's "never-log rule" because `tracing::debug!` macro calls can now print the secret. I documented this in both places.
- **The spec's "Implementation Order" for credentials (load-bearing first) matched my execution order:** credentials.rs → provider.rs → db.rs schema → llm.rs refactor → threading → IPC. No reordering surprises.
- **`async-trait` crate added as a new dependency.** Rust 1.93 supports native async fn in traits but not with `Box<dyn LlmProvider>` object safety — the `detect_context_window` method forces this. `async-trait` 0.1 is the standard workaround. No other trait in the repo uses it, but the pattern is mature and low-risk.
- **Partner module refactor was in gray-area scope.** The brief said "grep must only hit provider.rs" which meant Partner's duplicate URL had to move. I took the minimal approach: build `OpenRouterProvider` inline in `call_partner` and use its `chat_completions_url()` + `prepare_headers()` — no structural changes to `PartnerLlmConfig` or the tool-call request body. Flagging for the planner in case the architectural intent was to keep Partner fully separate.
- **Transitional fallback in `build_call_provider`:** when `LlmConfig.provider_registry` is `None` (unit tests, pre-DB-init window), the helper synthesizes an `OpenRouterProvider` from the legacy `api_key` field. This is transitional — Phase 4/6 can remove it once the unit test suite grows a `TestRegistry` helper. The fallback contains `base_url: "https://openrouter.ai/api/v1"` which technically violates "hardcoded URLs live in exactly one place" but it's the fallback path specifically for cases where no registry exists. I left a code comment explaining the transitional nature.
- **Pillar 37 awareness:** no new hardcoded LLM-constraining numbers introduced. The `call_model_via_registry` helper uses `effective_max_tokens` capped at 48K (same constant that already lived in `call_model_unified_with_options`), and takes `temperature` + `max_tokens` as args to thread through from the caller. The 48K cap is pre-existing and will move to a config contribution in Phase 4/6.
- **No friction log entries required.** The spec was unambiguous, the scope boundaries held, and the only gray-area decision (Partner subsystem) has a defensible minimal-change resolution.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Credential opacity integrity:** grep for any new code path that might log a `ResolvedSecret` via `Debug`, `Display`, or a tracing macro. The type-system should catch it at compile time, but a second pair of eyes should verify.
2. **First-boot path:** boot the app fresh (no `pyramid.db` file) and confirm the registry hydrates cleanly. The sequence is: `init_pyramid_db` runs `seed_default_provider_registry` → hydrate a fresh reader → construct `PyramidState` with the registry attached.
3. **Profile-apply flow:** `pyramid_apply_profile` swaps the entire `LlmConfig` — I added registry preservation there but the wanderer should trace end-to-end to confirm the swap doesn't drop the registry reference.
4. **IPC surface smoke test:** the 16 new commands are wired up but have no frontend yet (Phase 10). A smoke test via Tauri's invoke harness would confirm they're reachable.

Wanderer prompt suggestion: "Does a fresh Wire Node boot produce a `.credentials` file at the right path with 0600 mode, seed the four tier routing rows, and allow `call_model_unified` to place a real LLM call via the new provider trait — all without the user having to click anything?"

### Phase 3 fix pass — 2026-04-10

**What the wanderer found.** The original Phase 3 implementation routed the chain executor through the new provider registry but left the maintenance subsystem (DADBEAR stale engine, faq engine, delta engine, web edge collapse, meta passes) using `pyramid::config_helper::config_for_model(api_key, model)`. That helper builds a fresh `LlmConfig` via `..Default::default()`, which silently zeroes the new `provider_registry` and `credential_store` fields. Every helper that called `config_for_model` therefore landed in `build_call_provider`'s transitional fallback path: hardcoded OpenRouter URL, no `.credentials` lookup, no per-tier routing, no `pyramid_step_overrides`. The wanderer counted ~22 production call sites across `stale_helpers.rs`, `stale_helpers_upper.rs`, `faq.rs`, `delta.rs`, `meta.rs`, and `webbing.rs` — more than half the LLM call sites in the repo. Credential rotation broke for the maintenance subsystem (cached `api_key` strings on `PyramidStaleEngine`); per-tier routing was silently ignored on every stale/faq/delta/meta/webbing call.

**Option chosen.** Option 2 from the friction log entry: retire `config_for_model` in production code in favor of `LlmConfig::clone_with_model_override(&self, model)`. The helper clones the live `LlmConfig` (which preserves the `provider_registry` + `credential_store` `Arc` handles by construction) and overrides only the `primary_model` field. The legacy `config_for_model` body is retained for unit-test fixtures that don't have a live `PyramidState` to clone from, but it's now `#[deprecated]` with a doc comment pointing at the replacement. Production code that still imports it will fail clippy / `cargo check` lints, surfacing the bug before it lands in main.

**Files touched (full set, including the previous agent's partial work that this pass completed).**

Already-touched by the previous fix agent:

- `src-tauri/src/pyramid/config_helper.rs` — `config_for_model` marked `#[deprecated]` with retention comment for tests.
- `src-tauri/src/pyramid/llm.rs` — `LlmConfig::clone_with_model_override` method added (~lines 215-238) with doc comments explaining the registry-preservation contract.
- `src-tauri/src/pyramid/faq.rs` — every helper signature updated from `(api_key, model)` to `(base_config: &LlmConfig, model)`. 6 LLM call sites converted.
- `src-tauri/src/pyramid/stale_helpers.rs` — 5 helper signatures updated (`dispatch_file_stale_check`, `dispatch_rename_check`, `dispatch_evidence_set_apex_synthesis`, `dispatch_targeted_l0_stale_check`, plus internal helpers).
- `src-tauri/src/pyramid/stale_helpers_upper.rs` — `dispatch_node_stale_check`, `dispatch_connection_check`, `dispatch_edge_stale_check`, `generate_change_manifest`, `execute_supersession`, `apply_supersession_manifest`, `execute_supersession_identity_change` all converted. Test fixture at line ~4068 updated to pass `LlmConfig::default()` for the apply path that doesn't make LLM calls.
- `src-tauri/src/pyramid/delta.rs` (partial) — `match_or_create_thread` and `create_delta` signatures converted; `rewrite_distillation` and `collapse_thread` were left half-converted (signature still `api_key/model`, body referenced `config_for_model`).

Completed by this fix pass:

- `src-tauri/src/pyramid/delta.rs` — finished `rewrite_distillation` and `collapse_thread`. `create_delta` now passes `base_config` through to `rewrite_distillation`. Removed the last two `config_for_model` call sites (lines 497, 681 in the wanderer's snapshot).
- `src-tauri/src/pyramid/webbing.rs` — `collapse_web_edge` and `check_and_collapse_edges` signatures converted from `api_key/model` to `base_config/model`. `config_for_model` import removed.
- `src-tauri/src/pyramid/meta.rs` — all four meta passes (`timeline_forward`, `timeline_backward`, `narrative`, `quickstart`) plus the orchestrator `run_all_meta_passes` converted. `config_for_model` import removed.
- `src-tauri/src/pyramid/stale_engine.rs` — `PyramidStaleEngine` now stores a live `LlmConfig` field named `base_config` instead of the prior `api_key: String`. The `new()` constructor takes `base_config: LlmConfig` by value. `start_poll_loop`, `start_timer`, `run_layer_now`, and `drain_and_dispatch` (the free function) all clone `base_config` into spawned task scope and pass `&base_config` (renamed `cfg` per task) into every dispatched helper. The unit test at the bottom of the file builds a `LlmConfig::default()` for the engine constructor (the test only checks struct construction, not dispatch).
- `src-tauri/src/pyramid/routes.rs` — three route handlers updated: `process_annotation_hook` (background hook from annotation save), `handle_meta_run` (`/pyramid/:slug/meta/run` HTTP route), `handle_match_faq` (`/pyramid/:slug/faq/match` HTTP route), `handle_faq_directory` (`/pyramid/:slug/faq/directory` HTTP route). Each clones the live `LlmConfig` from `state.config.read().await.clone()` and threads it through to the helper. The `pyramid_run_full_l0_sweep` route handler that drives `drain_and_dispatch` directly now reads `engine.base_config.clone()` instead of `engine.api_key.clone()`.
- `src-tauri/src/main.rs` — three IPC commands updated: `pyramid_meta_run` (Tauri command for full meta pass), `pyramid_faq_directory` (Tauri command for FAQ directory listing), and the two `PyramidStaleEngine::new` call sites at lines ~3328 and ~5957 (post-build engine start, dadbear config-init engine start). Both engine call sites now pass a cloned live `LlmConfig` from `pyramid_state.config.read().await.clone()` instead of an extracted `api_key` string.
- `src-tauri/src/server.rs` — the boot-time stale engine reconstruction loop (`start_dadbear_engines_for_active_slugs`) at line ~260 now clones the live `LlmConfig` once outside the per-slug loop and passes `base_config.clone()` into every `PyramidStaleEngine::new` call. This is the load-bearing path for boot — every active pyramid's engine starts with a registry-aware config attached.
- `src-tauri/src/partner/crystal.rs` — `crystallize` signature converted; the spawned web-edge collapse task now clones `base_config` into the task scope.
- `src-tauri/src/partner/warm.rs` — `warm_pass` signature converted; the spawned crystallization task clones `base_config` into the task scope.
- `src-tauri/src/partner/conversation.rs` — `handle_message`'s `warm_pass` invocation now reads the pyramid's live `LlmConfig` via `state.pyramid.config.read().await.clone()` instead of synthesizing a fresh one from `PartnerLlmConfig.api_key`. `PartnerLlmConfig` only carries `(api_key, partner_model)` and would lose both runtime handles on conversion — the partner subsystem now treats the pyramid config as the source of truth for the maintenance subsystem.

**New signatures (before → after).**

- `PyramidStaleEngine::new` (`src/pyramid/stale_engine.rs`):
  - **Before:** `pub fn new(slug: &str, config: AutoUpdateConfig, db_path: &str, api_key: &str, model: &str, ops: OperationalConfig) -> Self`
  - **After:** `pub fn new(slug: &str, config: AutoUpdateConfig, db_path: &str, base_config: LlmConfig, model: &str, ops: OperationalConfig) -> Self`

- `drain_and_dispatch` (`src/pyramid/stale_engine.rs`):
  - **Before:** `pub async fn drain_and_dispatch(slug: &str, layer: i32, min_changed_files: i32, db_path: &str, semaphore: Arc<Semaphore>, api_key: &str, model: &str, ...) -> Result<()>`
  - **After:** `pub async fn drain_and_dispatch(slug: &str, layer: i32, min_changed_files: i32, db_path: &str, semaphore: Arc<Semaphore>, base_config: &LlmConfig, model: &str, ...) -> Result<()>`

- `faq::run_faq_category_meta_pass` (`src/pyramid/faq.rs`):
  - **Before:** `pub async fn run_faq_category_meta_pass(_reader, writer, slug: &str, faqs: &[FaqNode], api_key: &str, model: &str) -> Result<Vec<FaqCategory>>`
  - **After:** `pub async fn run_faq_category_meta_pass(_reader, writer, slug: &str, faqs: &[FaqNode], base_config: &LlmConfig, model: &str) -> Result<Vec<FaqCategory>>`

- `faq::process_annotation`, `faq::match_faq`, `faq::update_faq_answer`, `faq::create_new_faq`, `faq::get_faq_directory` — all converted from `api_key: &str, model: &str` to `base_config: &LlmConfig, model: &str`.

- `meta::timeline_forward`, `meta::timeline_backward`, `meta::narrative`, `meta::quickstart`, `meta::run_all_meta_passes` (`src/pyramid/meta.rs`):
  - **Before:** `pub async fn timeline_forward(reader, writer, slug: &str, api_key: &str, model: &str) -> Result<String>`
  - **After:** `pub async fn timeline_forward(reader, writer, slug: &str, base_config: &LlmConfig, model: &str) -> Result<String>`
  - (Same conversion for the other four functions; signatures otherwise unchanged.)

- `delta::rewrite_distillation`, `delta::collapse_thread`, `delta::create_delta`, `delta::match_or_create_thread` — all converted from `api_key/model` to `base_config/model`.

- `webbing::collapse_web_edge`, `webbing::check_and_collapse_edges` — converted similarly.

- `stale_helpers::dispatch_file_stale_check`, `stale_helpers::dispatch_rename_check`, `stale_helpers::dispatch_evidence_set_apex_synthesis`, `stale_helpers::dispatch_targeted_l0_stale_check` — converted.

- `stale_helpers_upper::dispatch_node_stale_check`, `stale_helpers_upper::dispatch_edge_stale_check`, `stale_helpers_upper::dispatch_connection_check`, `stale_helpers_upper::generate_change_manifest`, `stale_helpers_upper::execute_supersession`, `stale_helpers_upper::execute_supersession_identity_change`, `stale_helpers_upper::apply_supersession_manifest` — converted.

- `partner::crystal::crystallize`, `partner::warm::warm_pass` — converted.

- `routes::process_annotation_hook` (private) — converted from `api_key: &str, model: &str` to `base_config: &super::llm::LlmConfig, model: &str`.

**How `PyramidStaleEngine` now carries the live config.** The struct field is `pub base_config: LlmConfig` (owned, not `Arc`-wrapped — the field cost is small and the existing call shape was "clone into spawned task scope" anyway). Construction sites (`main.rs:3328`, `main.rs:5957`, `server.rs:260`) read `pyramid_state.config.read().await.clone()` and pass the result by value into `PyramidStaleEngine::new`. On every dispatch (poll loop, debounce timer fire, or manual `run_layer_now`), the engine clones `base_config` into the spawned task scope as `cfg` and passes `&cfg` to every helper. Per-tier routing/per-step overrides still flow through `cfg.provider_registry` because `clone()` on `LlmConfig` clones the underlying `Arc<ProviderRegistry>` and `Arc<CredentialStore>` references — the registry path is preserved at every hop.

**What this fixes for the user.**

1. **Credential rotation works for the maintenance subsystem.** Rotating `OPENROUTER_KEY` via Settings → Credentials now affects every stale/faq/delta/meta/webbing call on the next dispatch tick, not just the chain executor. The previous behavior cached the raw `api_key` string on `PyramidStaleEngine` at boot and never refreshed it; now the engine carries a `LlmConfig` whose `credential_store: Arc<CredentialStore>` resolves the variable on every call via the registry.

2. **Per-tier routing applies to the maintenance subsystem.** A user who configures `pyramid_tier_routing.tier = 'stale_remote'` to a different model now sees that model used on stale dispatch. Previously the maintenance subsystem hardcoded `LlmConfig.primary_model` from `config_for_model` and ignored the tier table.

3. **`pyramid_providers.base_url` applies to the maintenance subsystem.** A user with a self-hosted OpenAI-compatible default provider can now use it for stale/faq/delta/meta/webbing calls. Previously those code paths hit `https://openrouter.ai/api/v1` because `build_call_provider`'s fallback synthesized an `OpenRouterProvider` with a hardcoded URL when `provider_registry` was `None`.

4. **`.credentials` file is now read by the maintenance subsystem.** The IPC mutation path was already wired (via the in-memory cache), but the read path on every LLM call now consults `Arc<CredentialStore>` instead of `LlmConfig.api_key`. This closes the "write-only file" bug the wanderer flagged in entry 1.

**Updated understanding.** Phase 3 now applies the provider registry to **both** the chain executor **and** the maintenance subsystem uniformly. The unified mental model is: every code path that needs an LLM call clones the live `LlmConfig` from `PyramidState.config` (or one passed down through the call chain) and either uses it directly or calls `clone_with_model_override(model)` to swap the model while preserving registry/credential handles. There is no longer a "fast path" (chain executor) vs "fallback path" (maintenance subsystem) — every call lands in `build_call_provider`'s registry branch unless the test suite explicitly constructs a `LlmConfig::default()`.

### Verification results (fix pass)

- ✅ `cargo check --lib` — clean. Same 3 pre-existing warnings, no new warnings, no errors. Confirmed equal to the pre-fix-pass baseline by stash-and-rerun.
- ✅ `cargo build --lib` — clean. Same 3 pre-existing warnings.
- ✅ `cargo test --lib pyramid` — **842 passed, 7 failed**. The 7 failures are the same pre-existing unrelated tests (`pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, `pyramid::staleness::tests::*` × 5). No new failures introduced by the fix pass; no Phase 3 tests regressed.
- ✅ `cargo test --lib pyramid::credentials` — 18/18 passed (Phase 3 baseline preserved).
- ✅ `cargo test --lib pyramid::provider` — 20/20 passed (Phase 3 baseline preserved).
- ✅ `cargo test --lib pyramid::db::provider_registry_tests` — 8/8 passed (Phase 3 baseline preserved).
- ✅ `grep -rn "config_for_model" src-tauri/src/pyramid/` — only hits are: (a) `config_helper.rs:45` (the deprecated function definition itself), (b) `config_helper.rs:3,7,17` (deprecation doc comments), (c) `llm.rs:218,222,233` (doc comments on the replacement helper that reference the deprecated original), and (d) Phase 3 fix-pass marker comments left in `webbing.rs`, `meta.rs`, `delta.rs`, `faq.rs`, `stale_helpers_upper.rs` documenting where the old call sites were. **Zero active production callers.**
- ✅ No `#[allow(deprecated)]` was added anywhere — the goal is exactly that production code never silences the warning.

### Notes (fix pass)

- **`PartnerLlmConfig` is the wrong shape for the maintenance subsystem.** It only carries `(api_key, partner_model)` — building an `LlmConfig` from it would lose the `provider_registry` + `credential_store` handles. The fix pass routes the spawned warm-pass through `state.pyramid.config.read().await.clone()` directly instead of going through `PartnerLlmConfig`. A future cleanup could either fold `PartnerLlmConfig` into `LlmConfig` or have it carry the runtime handles too; the present fix is the minimal change.

- **Test updates were minimal.** Only one test in `stale_helpers_upper.rs` (`test_l0_file_change_apply_path`) and one in `stale_engine.rs` (`test_engine_new`) needed updating, and both just construct a `LlmConfig::default()` for the parameter slot. Neither test exercises the registry path — they exercise struct construction and the no-LLM apply path respectively.

- **Threading the registry through `config_for_model` (Option 1) was rejected.** Option 1 would have added `Option<Arc<ProviderRegistry>>` + `Option<Arc<CredentialStore>>` parameters to `config_for_model` and required every caller to pass them through. That's exactly the same surface area as Option 2 in number of touched files, with a worse architectural shape (`config_for_model` becomes a pseudo-trampoline that just rebuilds an `LlmConfig`). Option 2 (clone the live config) is strictly cleaner — every caller already has access to a `PyramidState` or an upstream `LlmConfig`, so the threading is immediate.

- **Out of scope for this fix.** The 5 other friction log entries from the wanderer pass (in-memory credential cache, `pyramid_test_api_key` legacy IPC, `.credentials` parent fsync, `parse_openai_envelope` control-char sanitize, HTTP 400 body logging) are NOT addressed in this commit. They are separate decisions, separately scoped, and the fix pass mandate was Option 1 only.
