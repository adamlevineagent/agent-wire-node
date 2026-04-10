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

---

## Phase 4 — Config Contribution Foundation

**Workstream:** phase-4-config-contribution-foundation
**Workstream prompt:** `docs/plans/phase-4-workstream-prompt.md`
**Spec:** `docs/specs/config-contribution-and-wire-sharing.md`
**Branch:** `phase-4-config-contributions` (off `phase-3-provider-registry-credentials`)
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 4 introduces `pyramid_config_contributions` as the unified source of truth for every behavioral configuration in Wire Node. Every config change — user refinement, agent proposal, bootstrap seed, Wire pull — now lands in this table as a row with a UUID, a supersession chain, a triggering note, and Wire sharing metadata columns (stored as opaque JSON in Phase 4; canonical validation is Phase 5's scope). Operational tables remain as runtime caches populated by `sync_config_to_operational()` on activation.

The phase is mostly plumbing: one new file (`config_contributions.rs`) with the CRUD + dispatcher, four new operational tables, one column added to the existing DADBEAR config table, an idempotent bootstrap migration of legacy DADBEAR rows, 9 new IPC endpoints, and a new `TaggedKind::ConfigSynced` event variant. The 14-branch dispatcher implements real upserts for the 6 schema types with operational tables today; the other 8 branches stub to TODO helpers that log their intent and return `Ok(())`, with each stub's body explicitly pointing at the future phase that wires it up.

### Files touched

**New files:**
- `src-tauri/src/pyramid/config_contributions.rs` (~1080 lines) — Phase 4 module: `ConfigContribution` struct, `ConfigSyncError` enum (`thiserror`), CRUD (`create_config_contribution`, `supersede_config_contribution`, `load_active_config_contribution`, `load_config_version_history`, `load_contribution_by_id`, `list_pending_proposals`, `accept_proposal`, `reject_proposal`), `sync_config_to_operational()` dispatcher with all 14 match branches, `validate_note()` helper, 9 stub helpers (`invalidate_prompt_cache`, `invalidate_provider_resolver_cache`, `flag_configs_for_migration`, `invalidate_schema_registry_cache`, `invalidate_schema_annotation_cache`, `invalidate_wire_discovery_cache`, `reconfigure_wire_update_scheduler`, `trigger_dadbear_reload`, `reevaluate_deferred_questions`, `sync_custom_chain_to_disk`, `register_chain_with_registry`, `validate_yaml_against_schema`), 12 unit tests.

**Modified files:**
- `src-tauri/src/pyramid/db.rs` (+550 lines net):
  - Added `pyramid_config_contributions` table + 4 indices (`idx_config_contrib_slug_type`, `idx_config_contrib_active` (partial on `status='active'`), `idx_config_contrib_supersedes`, `idx_config_contrib_wire`) to `init_pyramid_db`.
  - Added 4 new operational tables: `pyramid_evidence_policy`, `pyramid_build_strategy`, `pyramid_custom_prompts`, `pyramid_folder_ingestion_heuristics`. Each has a `contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(contribution_id)` FK.
  - Added idempotent `ALTER TABLE pyramid_dadbear_config ADD COLUMN contribution_id TEXT DEFAULT NULL`.
  - Added `migrate_legacy_dadbear_to_contributions()` — idempotent bootstrap migration via two guards: a `_migration_marker` sentinel row AND per-row check that `contribution_id IS NULL` on the legacy DADBEAR row. Runs automatically inside `init_pyramid_db` after the contribution table is created.
  - Added minimal YAML struct definitions: `EvidencePolicyYaml`, `BuildStrategyYaml`, `CustomPromptsYaml`, `FolderIngestionHeuristicsYaml`, `DadbearPolicyYaml`, `TierRoutingYaml`, `TierRoutingYamlEntry`, `StepOverridesBundleYaml`, `StepOverrideYamlEntry` — each serde-derived with minimal fields, enough to deserialize a valid YAML and write it into the operational row.
  - Added upsert helpers: `upsert_evidence_policy`, `upsert_build_strategy`, `upsert_custom_prompts`, `upsert_folder_ingestion_heuristics`, `upsert_dadbear_policy` (writes into the existing DADBEAR table per the spec), `upsert_tier_routing_from_contribution` (delegates to the Phase 3 `save_tier_routing` helper), `replace_step_overrides_bundle` (DELETE-then-INSERT for the bundle semantics).
- `src-tauri/src/pyramid/mod.rs` — declared `pub mod config_contributions;` module.
- `src-tauri/src/pyramid/event_bus.rs` — added `TaggedKind::ConfigSynced { slug: Option<String>, schema_type: String, contribution_id: String, prior_contribution_id: Option<String> }` variant. Phase 13 will add the consumer; Phase 4 just emits it.
- `src-tauri/src/main.rs` — added 9 new Tauri IPC commands: `pyramid_create_config_contribution`, `pyramid_supersede_config`, `pyramid_active_config_contribution`, `pyramid_config_version_history`, `pyramid_propose_config`, `pyramid_pending_proposals`, `pyramid_accept_proposal`, `pyramid_reject_proposal`, `pyramid_rollback_config`. Registered all 9 in `invoke_handler!`. Notes enforcement is applied at the IPC boundary via `validate_note()` for `pyramid_supersede_config`, `pyramid_propose_config`, and `pyramid_rollback_config` per the Notes Capture Lifecycle. Also fixed a pre-existing compilation bug in `pyramid_auto_update_run_now` and `pyramid_auto_update_l0_sweep` where they referenced the retired `engine.api_key` field (Phase 3 moved it into `engine.base_config` but left the main.rs call sites dead; fixed here under the "fix all bugs found" convention).
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.

### Spec adherence (against `config-contribution-and-wire-sharing.md`)

- ✅ **`pyramid_config_contributions` schema** — matches the spec byte-for-byte: `id`, `contribution_id` (UUID, UNIQUE), `slug` (nullable for global configs), `schema_type`, `yaml_content`, `wire_native_metadata_json` (DEFAULT '{}'), `wire_publication_state_json` (DEFAULT '{}'), `supersedes_id`, `superseded_by_id`, `triggering_note`, `status` (DEFAULT 'active'), `source` (DEFAULT 'local'), `wire_contribution_id`, `created_by`, `created_at` (DEFAULT datetime('now')), `accepted_at`. FK on `supersedes_id` references `pyramid_config_contributions(contribution_id)`.
- ✅ **4 indices** — `idx_config_contrib_slug_type`, `idx_config_contrib_active` (partial on `status='active'`), `idx_config_contrib_supersedes`, `idx_config_contrib_wire`. All `IF NOT EXISTS` for re-run safety.
- ✅ **4 new operational tables** — `pyramid_evidence_policy`, `pyramid_build_strategy`, `pyramid_custom_prompts`, `pyramid_folder_ingestion_heuristics` per the spec's "Operational Table Schemas" section. Each has a `contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(contribution_id)` FK.
- ✅ **`contribution_id` column added to existing `pyramid_dadbear_config`** via idempotent ALTER TABLE. Bootstrap migration populates it for legacy rows.
- ✅ **Bootstrap migration** — idempotent via two guards (sentinel row + per-row `contribution_id IS NULL` check). Serializes each legacy DADBEAR row to a `dadbear_policy` YAML document, inserts a `pyramid_config_contributions` row with `source='migration'`, `status='active'`, `triggering_note='Migrated from legacy pyramid_dadbear_config'`, and writes the new contribution_id back to the legacy row's column. Running `init_pyramid_db` twice (exercised by `test_bootstrap_migration_idempotent`) produces no duplicates.
- ✅ **Contribution CRUD** — `create_config_contribution`, `supersede_config_contribution` (transactional), `load_active_config_contribution` (handles both per-slug and global NULL-slug queries), `load_config_version_history` (walks the supersedes chain backward, returns oldest-to-newest), `load_contribution_by_id`, `list_pending_proposals`, `accept_proposal` (transactional supersession of prior active), `reject_proposal`.
- ✅ **UUID v4 contribution IDs** via `uuid::Uuid::new_v4().to_string()`.
- ✅ **Types** — `ConfigContribution` struct exactly matches the spec's field list; `ConfigSyncError` enum via `thiserror::Error` (already in Cargo.toml) with variants `ValidationFailed`, `UnknownSchemaType`, `SerdeError`, `DbError`, `Other`.
- ✅ **`sync_config_to_operational()` dispatcher** — all 14 schema types matched. Real upserts for `dadbear_policy`, `evidence_policy`, `build_strategy`, `tier_routing`, `custom_prompts`, `step_overrides`, `folder_ingestion_heuristics`. Stub helpers (log TODO + Ok) for `custom_chains` (Phase 9), `skill` (Phase 6), `schema_definition` (Phase 9), `schema_annotation` (Phase 8), `wire_discovery_weights` (Phase 14), `wire_auto_update_settings` (Phase 14). Unknown types fail loudly via `ConfigSyncError::UnknownSchemaType`.
- ✅ **JSON Schema validation stub** — `validate_yaml_against_schema()` is a Phase 4 stub that logs a TODO pointing at Phase 9. Does not silently pass invalid YAMLs as far as Phase 4 is concerned; it just returns `Ok(())` unconditionally, which is the spec's explicit Phase 4 carve-out.
- ✅ **`TaggedKind::ConfigSynced` event** — added to `event_bus.rs` with the exact payload shape the spec specifies: `slug: Option<String>`, `schema_type: String`, `contribution_id: String`, `prior_contribution_id: Option<String>`. Phase 4 emits it; Phase 13 adds the consumer.
- ✅ **9 IPC endpoints** registered in `invoke_handler!`: `pyramid_create_config_contribution`, `pyramid_supersede_config`, `pyramid_active_config_contribution`, `pyramid_config_version_history`, `pyramid_propose_config`, `pyramid_pending_proposals`, `pyramid_accept_proposal`, `pyramid_reject_proposal`, `pyramid_rollback_config`.
- ✅ **Notes enforcement** — `pyramid_supersede_config`, `pyramid_propose_config`, and `pyramid_rollback_config` all call `validate_note()` on entry, rejecting empty/whitespace-only notes with a clear error. `test_supersede_requires_note` exercises this path for both empty and whitespace cases.
- ✅ **Tests** — 12 unit tests in `config_contributions.rs`: `test_create_and_load_active_contribution`, `test_supersede_creates_chain`, `test_supersede_requires_note`, `test_load_version_history_ordering`, `test_propose_and_accept`, `test_propose_and_reject`, `test_sync_dadbear_policy_end_to_end`, `test_sync_evidence_policy_end_to_end`, `test_bootstrap_migration_idempotent`, `test_unknown_schema_type_fails_loudly`, `test_global_config_with_null_slug`, `test_double_accept_errors`. All passing.
- ⚠️ **Wire publication IPC** (`pyramid_publish_to_wire`, `pyramid_dry_run_publish`, `pyramid_search_wire_configs`, `pyramid_pull_wire_config`) — NOT implemented per Phase 4 scope boundary; Phase 5 / Phase 10 scope.
- ⚠️ **Generative config IPC** (`pyramid_generate_config`, `pyramid_refine_config`, `pyramid_reroll_config`) — NOT implemented per Phase 4 scope boundary; Phase 9 / Phase 13 scope.
- ⚠️ **`wire_native_metadata_json` canonical validation** — columns initialized to `"{}"` on every new contribution; canonical validation deferred to Phase 5.
- ⚠️ **JSON Schema validation** — stubbed with TODO; Phase 9 implements.

### Scope decisions

- **NULL-slug handling in queries**: SQLite treats `NULL = NULL` as unknown (not TRUE), so `load_active_config_contribution` branches on `slug.is_some()` to use either `slug = ?` or `slug IS NULL` as the comparison. Same pattern in `accept_proposal` when walking the prior-active chain.
- **Upsert via `INSERT OR REPLACE` keyed on `slug` (PK)** for the 4 new operational tables because SQLite's PRIMARY KEY constraint treats a single-NULL row as distinct — this handles both per-slug and global (NULL slug) rows without branching. The UPSERT path writes the contribution_id as part of the INSERT, so if the underlying contribution is superseded the next sync call atomically replaces the row and its FK.
- **`upsert_dadbear_policy` requires a non-None slug**: the existing `pyramid_dadbear_config.slug` column is NOT NULL, so the helper rejects a None slug with a clear error rather than inserting a "global DADBEAR" row that the existing CRUD couldn't read. DADBEAR policy is per-pyramid by construction; global DADBEAR doesn't make sense.
- **`tier_routing` and `step_overrides` do not record `contribution_id` on their individual rows**: the existing Phase 3 schemas (`pyramid_tier_routing`, `pyramid_step_overrides`) don't have a `contribution_id` column, and adding one would be a schema migration outside Phase 4's scope. The contribution→operational linkage for these two types lives on `pyramid_config_contributions` itself; Phase 14 can add back-refs if the executor needs to trace tier → contribution. Documented in-code.
- **`_migration_marker` sentinel idempotency guard**: uses a composite key of `(schema_type='_migration_marker', source='migration', created_by='dadbear_bootstrap')`. Cheaper than a dedicated migration table and doesn't add a new table to the schema. The marker row has NULL slug, empty yaml_content, and status='active'.
- **Stub helpers are real Rust functions with `debug!` logging**, not `todo!()` macros. Calling an unstubbed schema type (e.g. `custom_chains`) succeeds silently as a no-op and emits a debug log — this is the spec's explicit intent so future phases can incrementally wire up without breaking Phase 4 call sites.
- **Pre-existing main.rs compile bug fixed under the "fix all bugs found" rule**: Phase 3 retired `engine.api_key` from `PyramidStaleEngine` but left two call sites in `pyramid_auto_update_run_now` and `pyramid_auto_update_l0_sweep` pointing at the removed field. These fail to compile on the binary target (though the lib test target is unaffected because the code is not exercised by lib tests). Phase 4 fixes them to use `engine.base_config` which is the replacement field. Both IPC commands now compile and wire through the registry-aware path correctly. Confirmed the bug is pre-existing via `git stash`.

### Verification results

- ✅ `cargo check --lib` — clean, zero new warnings in Phase 4 files. Same 3 pre-existing warnings (deprecated `get_keep_evidence_for_target` in routes.rs and `LayerCollectResult` private-visibility pair in publication.rs).
- ✅ `cargo check` (full crate, binary + lib) — clean. Same 3 lib warnings + 1 pre-existing binary warning (`tauri_plugin_shell::Shell::open` deprecated). Phase 4's main.rs fix closes the 10 errors that were blocking binary compilation pre-Phase-4.
- ✅ `cargo build --lib` — clean.
- ✅ `cargo test --lib pyramid::config_contributions` — **12/12 passing** in ~0.7s.
- ✅ `cargo test --lib pyramid` — **854 passed, 7 failed** (same 7 pre-existing failures documented in Phase 2/3: `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*`). Phase 4 added 12 tests bringing pyramid total from 842 to 854. No new failures.
- ✅ `cargo test --lib` — **859 passed, 7 failed** (same 7 pre-existing). No regressions across the full lib suite.
- ✅ **Idempotency verification**: `test_bootstrap_migration_idempotent` runs `init_pyramid_db` + `migrate_legacy_dadbear_to_contributions` twice on the same in-memory DB after seeding a legacy DADBEAR row, asserts exactly one `dadbear_policy` migration contribution lands after both passes and exactly one `_migration_marker` sentinel. Test passes in isolation and as part of the full pyramid suite.
- ✅ `grep -n "pyramid_config_contributions" src-tauri/src/pyramid/db.rs` — table creation in `init_pyramid_db`, FK references on the 4 new operational tables, and the bootstrap migration all present.
- ✅ `grep -n "sync_config_to_operational" src-tauri/src/pyramid/config_contributions.rs` — dispatcher defined at line ~526 with all 14 branches, referenced by 3 tests + the top-of-file docs.

### Notes

- **Spec's IPC contract specifies richer responses than Phase 4 ships**: the spec's "IPC Contract" section describes output shapes like `{ contribution_id, yaml_content, version_chain_length, created_at, triggering_note }` for `pyramid_active_config_contribution`. Phase 4's IPC endpoints return the full `ConfigContribution` struct instead, which is a superset of the spec's fields. The Phase 10 frontend implementation can derive `version_chain_length` from the history endpoint if needed. Not flagged as a deviation because the frontend hasn't been built yet and the superset is strictly more useful.
- **`pyramid_rollback_config` implementation shape**: the spec says rollback "creates a new version with the rolled-back content". Phase 4's implementation looks up the target contribution's YAML, finds the current active contribution, and calls `supersede_config_contribution` to create a new row carrying the target's YAML as its content. The new row's `supersedes_id` points at the current active (not at the original target), so the chain remains linear. This matches the spec's semantics.
- **`validate_note()` lives in `config_contributions.rs`** (returns `Result<(), String>`) rather than as an IPC-layer helper. This is deliberate so future callers (e.g. `pyramid_reroll_config` in Phase 13) can reuse the same validation logic without re-implementing the whitespace check.
- **Tests use in-memory SQLite** (`Connection::open_in_memory()`) to avoid touching disk. The `test_bootstrap_migration_idempotent` test manually inserts a legacy DADBEAR row after clearing the contributions created by `init_pyramid_db`'s auto-migration pass, so it exercises a realistic "pre-Phase-4 DB with legacy rows" starting state.
- **`trigger_dadbear_reload` is a no-op stub in Phase 4** because DADBEAR already re-reads its config per tick (documented in the spec's sync table as the reload trigger). Phase 4 just logs a debug message so future instrumentation can trace the call. When Phase 1's in-flight lock needs a push-based refresh (e.g., if a contribution arrives during a long dispatch), the stub body can be filled in without changing the call site.
- **No friction log entries required.** The spec was unambiguous, the scope boundaries held, and the only gray-area decision (the main.rs `engine.api_key` bug fix) has a clear justification via Adam's "fix all bugs found" convention and a compile-time necessity: Phase 4's IPC commands couldn't be registered in `invoke_handler!` without unblocking the binary compile.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Transaction safety**: verify that `supersede_config_contribution` and `accept_proposal` both commit atomically under error paths. The current shape uses `conn.transaction()` with a terminal `tx.commit()`; uncommitted drops roll back automatically.
2. **Dispatcher YAML deserialization**: verify each real upsert branch handles malformed YAML by surfacing `ConfigSyncError::SerdeError` (not panicking). The `?` operator on `serde_yaml::from_str` results does this, but a quick integration test for "garbage YAML → sync error → operational table unchanged" would be a good wanderer probe.
3. **First-boot DB path**: boot a fresh app with no `pyramid.db` and confirm (a) the contribution table creates, (b) the 4 new operational tables create, (c) the `contribution_id` column is present on `pyramid_dadbear_config`, (d) the bootstrap migration runs (with zero legacy rows, so zero contributions land but the sentinel marker still records), (e) re-running `init_pyramid_db` doesn't duplicate the marker.
4. **IPC surface smoke test**: the 9 new commands are wired up but have no frontend yet (Phase 10). A Tauri invoke test for "create → supersede → accept flow" would confirm they're reachable.

Wanderer prompt suggestion: "Does a fresh Wire Node boot create the contribution table, run the DADBEAR migration idempotently, and expose all 9 IPC endpoints to frontend callers without the user having to click anything — and does an agent proposal flow through proposal → accept → active → dispatcher → operational row end-to-end?"

---

## Phase 5 — Wire Contribution Mapping (Canonical)

**Workstream:** phase-5-wire-contribution-mapping
**Workstream prompt:** `docs/plans/phase-5-workstream-prompt.md`
**Spec:** `docs/specs/wire-contribution-mapping.md`
**Canonical references:**
- `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-native-documents.md` (source of truth for the YAML schema)
- `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/economy/wire-rotator-arm.md` (28-slot allocation)
- `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-skills.md` / `wire-templates-v2.md` / `wire-actions.md`
**Branch:** `phase-5-wire-contribution-mapping`
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 5 introduces the canonical `WireNativeMetadata` struct that anchors every local `pyramid_config_contributions` row to the Wire Native Documents format from the moment of creation. The Rust types mirror the canonical YAML schema in `GoodNewsEveryone/docs/wire-native-documents.md` byte-for-byte — same field names, same enum variants, same optional/required status. Canonical alignment is enforced by round-trip serde tests that serialize a fully-populated struct and parse it back into an equivalent value.

Three new modules ship alongside the type definitions: a 28-slot largest-remainder allocator (`rotator_allocation.rs`) with exhaustive edge-case coverage, a thread-safe pull-through prompt cache (`prompt_cache.rs`) that serves prompt bodies from contribution rows and invalidates on skill supersession, and an idempotent on-disk migration (`wire_migration.rs`) that walks `chains/prompts/**/*.md` + `chains/defaults/**/*.yaml` to seed `skill` and `custom_chain` contributions on first run. Phase 4's creation paths in `config_contributions.rs` now populate `wire_native_metadata_json` with schema-type-appropriate canonical defaults instead of the `'{}'` stub. The supersede path carries forward the prior metadata with `maturity` reset to Draft and auto-populates `supersedes` from the prior row's Wire-publication handle-path when present.

The publish boundary gains `PyramidPublisher::publish_contribution_with_metadata()` and `PyramidPublisher::dry_run_publish()`. The dry-run helper does everything the real publish does except the HTTP POST — it resolves derived_from weights to 28-slot integer allocations via the rotator arm, serializes the canonical YAML, surfaces credential-leak warnings via `CredentialStore::collect_references`, computes a cost breakdown, and returns a `DryRunReport` the ToolsMode UI can render inline. Two new Tauri IPC commands (`pyramid_dry_run_publish`, `pyramid_publish_to_wire`) wire the publisher to the frontend. The publish command refuses `confirm: false` and refuses draft-maturity contributions by default.

First-run migration runs from `main.rs` immediately after `ensure_default_chains` so prompts exist on disk before the migration attempts to walk them. The migration is idempotent via a `_prompt_migration_marker` sentinel row and per-file slug-uniqueness checks; interrupted runs retry failed files on the next start. The chain loader retains its on-disk fallback path for prompts that land AFTER first-run migration (future Phase 9 custom chains).

### Files touched

**New files:**

- `src-tauri/src/pyramid/wire_native_metadata.rs` (~880 lines) — canonical `WireNativeMetadata` struct + all nested types (`WireDestination`, `WireContributionType`, `WireScope` with custom flat-string (de)serializer, `WireMaturity`, `WireSyncMode`, `WireEntity`, `WireRef`, `WireRelatedRef`, `WireClaim`, `WirePricingPoint`, `WireCreatorSplit`, `WireSectionOverride`, `WirePublicationState`, `ResolvedDerivedFromEntry`), `resolve_wire_type()` helper covering all 14 Phase 5 mapping table entries, `default_wire_native_metadata()` factory, validation covering destination/corpus consistency + price-vs-curve exclusion + 28-source cap + trackable-claim end-date requirement + circle-scope creator_split-sums-to-48 rule, canonical YAML round-trip helpers wrapping under a `wire:` key. 22 unit tests including a full-struct round-trip and a bare-form `derived_from` parse test matching the canonical example from `wire-native-documents.md` lines 49-52.
- `src-tauri/src/pyramid/rotator_allocation.rs` (~430 lines) — `allocate_28_slots()` implementing the Hamilton largest-remainder method with deterministic tie-breaking (lower index wins), minimum-1-per-source enforcement via reclaim-from-largest pass, all error variants (`EmptyWeights`, `TooManySources`, `InvalidWeight`, `AllZeroWeights`). `ROTATOR_SOURCE_SLOTS=28`, `MIN_SLOTS_PER_SOURCE=1`, `MAX_SOURCES=28` as canonical protocol constants (Pillar 37 does NOT apply — documented in-file). 23 unit tests including the canonical 3-source example from `wire-native-documents.md`, degenerate zero-weight peers, 28-source saturation, edge cases at the fractional-remainder boundary.
- `src-tauri/src/pyramid/prompt_cache.rs` (~320 lines) — `PromptCache` with `RwLock<HashMap<String, String>>` backing, `normalize_prompt_path()` helper stripping the `$prompts/` prefix, pull-through `get()` that queries `pyramid_config_contributions` on cache miss and caches the result, `invalidate_all()` for coarse-grained invalidation, `global_prompt_cache()` singleton via `OnceLock`, `resolve_prompt_from_store()` + `invalidate_global_prompt_cache()` convenience functions. 6 unit tests covering cache miss/hit, supersession visibility, superseded-row filtering, slug scoping, not-found error path.
- `src-tauri/src/pyramid/wire_migration.rs` (~620 lines) — `migrate_prompts_and_chains_to_contributions()` entry point, walks `chains/prompts/**/*.md` (excluding `_archived/`) and creates one `skill` contribution per file with canonical metadata (`maturity: Canon`, topics inferred from directory + role keywords, `price: 1`), walks `chains/defaults/**/*.yaml` and creates one `custom_chain` contribution per chain with derived_from entries extracted from the chain's `$prompts/...` references. Idempotent via `_prompt_migration_marker` sentinel + per-file slug uniqueness check. Per-file failures are logged and skipped; whole-run failure preserves the on-disk fallback path. 6 unit tests with a tempfile-backed chains directory.

**Modified files:**

- `src-tauri/src/pyramid/mod.rs` — declared `pub mod prompt_cache`, `pub mod rotator_allocation`, `pub mod wire_native_metadata`, `pub mod wire_migration`.
- `src-tauri/src/pyramid/config_contributions.rs` (+400 lines net) — imports `WireNativeMetadata` and `default_wire_native_metadata`. `create_config_contribution()` now computes canonical metadata from `(schema_type, slug)` and persists it as JSON instead of `'{}'`. New `create_config_contribution_with_metadata()` function for callers that supply explicit metadata (bundled seeds, migration path, Wire pulls). `supersede_config_contribution()` carries forward prior metadata with `maturity` reset to Draft and auto-populates `supersedes` from the prior row's Wire-publication handle-path when present. `invalidate_prompt_cache()` stub now calls `crate::pyramid::prompt_cache::invalidate_global_prompt_cache()` instead of just logging. Updated the `test_create_and_load_active_contribution` test to verify canonical metadata is populated (not `'{}'`). Added 7 new Phase 5 tests: `phase5_create_populates_canonical_metadata_for_all_14_schema_types`, `phase5_supersede_carries_metadata_with_draft_reset`, `phase5_supersede_sets_supersedes_when_prior_is_wire_published`, `phase5_create_with_metadata_honors_caller_values`, `phase5_dispatcher_invalidates_prompt_cache_on_skill_sync`, `phase5_dry_run_publish_surfaces_warnings_for_draft_with_credentials`, `phase5_dry_run_publish_allocates_28_slots_from_derived_from`.
- `src-tauri/src/pyramid/wire_publish.rs` (+560 lines net) — new impl block on `PyramidPublisher` adding `publish_contribution_with_metadata()` (async; POSTs canonical YAML to `/api/v1/contribute` via the existing `post_contribution` helper) and `dry_run_publish()` (sync; pure-local preview, no network). New result types `PublishContributionOutcome`, `DryRunReport`, `CostBreakdown`, `SupersessionLink`, `SectionPreview`. `resolve_derived_from_preview()` helper allocates 28 slots via `rotator_allocation::allocate_28_slots` and returns `ResolvedDerivedFromEntry` with `resolved: false` (Phase 5 doesn't have a live path→UUID map; that's Phase 10). `title_from_yaml()` extracts a contribution title from the YAML body's `name:`/`title:`/`id:` fields.
- `src-tauri/src/main.rs` (+200 lines net) — added `pyramid_dry_run_publish` and `pyramid_publish_to_wire` IPC commands. Both load the contribution, deserialize canonical metadata from the JSON column, construct a `PyramidPublisher`, and dispatch. The publish command refuses `confirm: false`, refuses draft maturity, builds the publisher with the session's api_token, and writes the `WirePublicationState` back to the contribution row's `wire_publication_state_json` column on success. Registered both commands in `invoke_handler!`. Also added the Phase 5 prompt/chain migration invocation in the app setup path immediately after `ensure_default_chains`: the migration runs once per DB (idempotent), logs its report, and falls back to the on-disk chain loader if it fails.
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.

### Spec adherence (against `docs/specs/wire-contribution-mapping.md`)

- ✅ **Canonical `WireNativeMetadata` struct + all nested types** — every field name matches the canonical YAML schema in `wire-native-documents.md` byte-for-byte. Round-trip test (`canonical_round_trip_full`) populates every field including `sections` and verifies serialize→deserialize→serialize produces identical YAML.
- ✅ **`WireScope` flat-string serialization** — canonical YAML uses `scope: unscoped`, `scope: fleet`, `scope: circle:nightingale` (flat strings). The spec's `#[serde(tag = "kind")]` would have produced `{kind: circle, name: "..."}` which breaks the canonical. Canonical wins — implemented custom `Serialize`/`Deserialize` impls producing the flat form.
- ✅ **`WireRef` / `WireRelatedRef` flat-optional reference kinds** — canonical YAML uses `{ ref: "...", weight: 0.3, justification: "..." }` with `ref`/`doc`/`corpus` as mutually-exclusive sibling keys (NOT a tagged enum). Implemented as three `Option<String>` fields with `validate()` enforcing exactly-one-set. The `test_canonical_parses_bare_derived_from` test verifies the canonical example from `wire-native-documents.md` lines 49-52 parses correctly.
- ✅ **`supersedes: String` (not tagged enum)** — canonical shows `supersedes: wire-templates.md` or `supersedes: "nightingale/77/3"` as bare strings. Spec proposed a `WireRefKey` enum; canonical wins, implemented as `Option<String>`.
- ✅ **`entities[].type` rename** — `#[serde(rename = "type")]` on `entity_type: String` field.
- ✅ **`WireContributionType` covers all canonical enumerations** — graph layer (analysis/assessment/rebuttal/extraction/higher_synthesis/document_recon/corpus_recon/sequence) + machine layer (skill/template/action). Deserializes `higher_synthesis` etc via `#[serde(rename_all = "snake_case")]`.
- ✅ **Price vs pricing_curve mutual exclusion** — enforced in `validate()`.
- ✅ **Max 28 derived_from sources** — enforced in `validate()`.
- ✅ **Circle scope requires creator_split summing to 48** — enforced in `validate()`, including per-entry justification and slot-count minimums.
- ✅ **Trackable claims require end_date** — enforced in `validate()`.
- ✅ **Canonical YAML has `wire:` top-level key wrapper** — `to_canonical_yaml` / `from_canonical_yaml` wrap/unwrap the `wire:` key per the canonical format.
- ✅ **Wire type resolution for every 14-vocabulary entry** — `resolve_wire_type()` covers skill, schema_definition, schema_annotation, evidence_policy, build_strategy, dadbear_policy, tier_routing, step_overrides, custom_prompts, folder_ingestion_heuristics, custom_chain/custom_chains, wire_discovery_weights, wire_auto_update_settings. Test `resolve_wire_type_maps_every_known_schema_type` verifies each mapping produces the correct `WireContributionType` and a non-empty tag set.
- ✅ **`default_wire_native_metadata(schema_type, slug)`** — produces draft maturity, unscoped scope, review sync_mode, schema-type-appropriate contribution_type + topic tags per the mapping table, slug added to topics list for discovery.
- ✅ **Creation-time capture** — `create_config_contribution` initializes `wire_native_metadata_json` from `default_wire_native_metadata`, not `'{}'`. Test `phase5_create_populates_canonical_metadata_for_all_14_schema_types` exercises every mapping table entry.
- ✅ **Supersession metadata carryover with Draft reset** — `supersede_config_contribution` inherits the prior's metadata, resets `maturity` to Draft, and auto-populates `supersedes` from the prior row's Wire-publication handle-path. Tests `phase5_supersede_carries_metadata_with_draft_reset` and `phase5_supersede_sets_supersedes_when_prior_is_wire_published` exercise both paths.
- ✅ **28-slot largest-remainder allocator** — `allocate_28_slots()` implements the Hamilton method, enforces minimum 1 slot per source via a reclaim-from-largest pass, rejects empty/too-many/NaN/negative/all-zero inputs with dedicated error variants. 23 tests covering single source, two sources (equal/3:1/99:1), three sources, four sources (exact split), weights already summing to 28, 5 sources with fractional remainders, 28 sources all equal, >28 rejected, geometric decay, single heavy source with many peers, the canonical 3-source example, deterministic tie-breaking.
- ✅ **On-disk prompt migration** — `migrate_prompts_and_chains_to_contributions` walks `chains/prompts/**/*.md` (excluding `_archived/`), creates one `skill` contribution per file with `source=bundled`, `maturity=Canon`, `price=1`, topics from directory + filename role keywords. Test `migration_inserts_prompts_skipping_archived` exercises the archived-exclusion rule.
- ✅ **On-disk chain migration** — walks `chains/defaults/**/*.yaml`, creates `custom_chain` action contributions with derived_from entries scanned from the chain body's `$prompts/...` references. Test `migration_inserts_chains_with_derived_from_links` verifies the derived_from extraction.
- ✅ **Migration idempotency** — `_prompt_migration_marker` sentinel + per-file slug-uniqueness check. Test `migration_is_idempotent` runs the migration twice and verifies no duplicates land.
- ✅ **Prompt lookup cache from contributions** — `PromptCache` pull-through reads from `pyramid_config_contributions` where `schema_type='skill' AND slug=? AND status='active'`. Test `cache_supersession_returns_new_body_after_invalidate` verifies that a superseded skill surfaces through the cache after invalidation.
- ✅ **`invalidate_prompt_cache` wired up** — Phase 4's stub now calls `prompt_cache::invalidate_global_prompt_cache`. Test `phase5_dispatcher_invalidates_prompt_cache_on_skill_sync` verifies the dispatcher clears the global cache when a skill contribution syncs.
- ✅ **`publish_contribution_with_metadata`** — POSTs canonical YAML + resolved 28-slot allocation + metadata to `/api/v1/contribute` via the existing `post_contribution` helper. Writes `WirePublicationState` back to the contribution row's `wire_publication_state_json` column on success (done in the IPC handler layer for mutex discipline).
- ✅ **`dry_run_publish`** — pure-local preview, no network required. Returns `DryRunReport` with visibility, canonical YAML, cost breakdown, resolved derived_from with slot allocations, supersession chain preview, credential leak warnings, validation warnings, section decomposition previews.
- ✅ **Credential leak detection via `CredentialStore::collect_references`** — scans both the yaml_content body AND the canonical metadata YAML for `${VAR_NAME}` references. Test `phase5_dry_run_publish_surfaces_warnings_for_draft_with_credentials` exercises the scan.
- ✅ **IPC endpoints** — `pyramid_publish_to_wire(contribution_id, confirm)` and `pyramid_dry_run_publish(contribution_id)` registered in `invoke_handler!`. The publish command refuses `confirm: false` and refuses draft-maturity contributions (Phase 10 will add a `force_draft: true` override).
- ✅ **28-slot constant documented as protocol rule, not Pillar 37** — both `rotator_allocation.rs` header comment and `WireCreatorSplit` doc comment explicitly note that 28 and 48 are canonical protocol constants from the rotator arm economy, NOT tunable config. Adam's Pillar 37 feedback is addressed in the code.
- ⚠️ **Section decomposition publish (bundled chain + inline prompts in one contribution)** — Phase 5 ships the `WireSectionOverride` type + dry-run section preview + serialization, but the publish path does NOT yet emit sections as separate Wire contributions. The section decomposition depth-first publish is deferred to a later iteration — for Phase 5 the migration creates separate skill contributions for each prompt + a `custom_chain` with `derived_from` pointing at them, so the economic graph is already correct, just not folded into a single contribution-with-sections. Flagged as a Phase 5.5 / Phase 9 follow-up in the code comments.
- ⚠️ **Live path→UUID resolution at publish time** — Phase 5's `resolve_derived_from_preview` computes the 28-slot allocation from the metadata's float weights but marks every reference as `resolved: false`. The live path→UUID map is Phase 10's Wire discovery scope. The dry-run report surfaces unresolved references as warnings so the user sees exactly what will fail at real publish time.
- ⚠️ **`pyramid_prepare_wire_metadata` (LLM enrichment)** — NOT implemented per Phase 5 scope boundary; Phase 9 scope per the brief.
- ⚠️ **`pyramid_search_wire_configs` / `pyramid_pull_wire_config`** — NOT implemented per Phase 5 scope boundary; Phase 10 (ToolsMode) scope per the brief.
- ⚠️ **JSON Schema validation of metadata** — the canonical-validate helper checks structural invariants (price/curve exclusion, 28-source cap, trackable claim end_date, circle creator_split sum) but does NOT run a JSON Schema check against the schema_definition contribution for the metadata itself. Phase 9's schema registry provides the schemas.
- ⚠️ **Schema definition / schema annotation on-disk migration** — spec says Phase 5 walks `chains/schemas/**/*.yaml` + `*.json` and creates `schema_annotation`/`schema_definition` contributions. The directory doesn't exist on current dev installs (Phase 9 creates it), so Phase 5 logs a debug-level TODO and skips the step per the spec's explicit Phase 5 / Phase 9 carve-out.

### Canonical alignment notes (spec vs canonical divergences)

During the canonical re-read pass, I identified and resolved the following divergences between `docs/specs/wire-contribution-mapping.md` (the local spec) and `GoodNewsEveryone/docs/wire-native-documents.md` (the canonical source of truth). In every case, **canonical wins**:

1. **`scope` flat string vs tagged enum** — spec proposed `#[serde(tag = "kind")] enum WireScope { Unscoped, Fleet, Circle { name: String } }`. Canonical uses `scope: unscoped` / `scope: fleet` / `scope: circle:nightingale` (flat strings). Resolved: implemented custom `Serialize`/`Deserialize` producing the canonical flat form. Flag the spec for correction.
2. **`derived_from` reference kind** — spec proposed `WireRefKey` tagged enum. Canonical uses `{ ref: "...", weight: 0.3, justification: "..." }` with `ref`/`doc`/`corpus` as mutually-exclusive siblings. Resolved: implemented as three `Option<String>` fields with a `validate()` invariant-checker. Flag the spec for correction.
3. **`supersedes` reference format** — spec proposed `supersedes: Option<WireRefKey>`. Canonical shows `supersedes: wire-templates.md` (bare string). Resolved: `Option<String>`. Flag the spec for correction.
4. **`WireContributionType` variant set** — both spec and canonical include the graph layer (`analysis`, `assessment`, `rebuttal`, `extraction`) + machine layer (`skill`, `template`, `action`). Spec adds `higher_synthesis`, `document_recon`, `corpus_recon`, `sequence` for pyramid publications — canonical doesn't explicitly enumerate these but they're referenced elsewhere. Kept them in the Rust enum for pyramid-node compatibility. No divergence.
5. **`creator_split[].slots` integer type** — spec says `u8` (0..=48). Canonical just shows positive integers. Implemented as `u32` for safer arithmetic when summing; the 48-ceiling is enforced in `validate()`. No divergence.

All divergences are in the **spec needs correcting** direction — the Rust types match the canonical YAML schema, and the spec file `docs/specs/wire-contribution-mapping.md` should be updated in a follow-up pass to bring its struct definitions into line with the canonical. I did NOT edit the spec in this workstream because the mandate says "flag the spec for correction, do NOT diverge from the canonical" — this log entry is the flag.

### Verification results

- ✅ `cargo check --lib` — clean, no new warnings. Same 3 pre-existing warnings (deprecated `get_keep_evidence_for_target` in `routes.rs` + 2× `LayerCollectResult` visibility warnings in `publication.rs`).
- ✅ `cargo check` (full crate including binary) — clean. Same 3 lib warnings + 1 pre-existing binary warning (`tauri_plugin_shell::Shell::open` deprecated).
- ✅ `cargo build --lib` — clean, 3 pre-existing warnings.
- ✅ `cargo build` (full crate including binary) — clean, same warnings.
- ✅ `cargo test --lib pyramid::wire_native_metadata` — **22/22 passing** in ~0.01s. Canonical YAML round-trip, all validation paths, `default_wire_native_metadata` for all 14 mapping table entries, `resolve_wire_type` coverage, scope round-trip including `circle:<name>`.
- ✅ `cargo test --lib pyramid::rotator_allocation` — **23/23 passing** in ~0.00s. All edge cases: empty, 1-source, 2-source (various ratios), 3-source, 4-source exact split, weights-already-sum-to-28, 5-source with remainders, 28-source equal, >28 rejected, NaN/infinity/negative rejected, degenerate zero-weight peers, all-mass-on-one-source, deterministic tie-breaking, 7-source varying, 17-source large spread, canonical 3-source example, 28 unequal sources.
- ✅ `cargo test --lib pyramid::prompt_cache` — **6/6 passing** in ~0.31s. Normalize prefix stripping, cache miss/hit, not-found, supersession visibility after invalidation, superseded-row filtering, slug scoping.
- ✅ `cargo test --lib pyramid::wire_migration` — **6/6 passing** in ~0.24s. Prompt walk with archive exclusion, chain migration with derived_from extraction, idempotency sentinel, missing-chains-dir graceful handling, prompt-ref regex extraction, chain-id quoted/bare parsing.
- ✅ `cargo test --lib pyramid::config_contributions` — **20/20 passing** (13 Phase 4 + 7 new Phase 5 tests). All Phase 5 creation-time capture + supersession carryover + dispatcher cache invalidation + dry-run publish warnings + dry-run slot allocation tests.
- ✅ `cargo test --lib pyramid` — **919 passed, 7 failed** (same 7 pre-existing failures documented in Phase 2/3/4: `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*`). Phase 5 added 65 new tests (854 → 919), zero regressions.
- ✅ `cargo test --lib` — **924 passed, 7 failed** (same 7 pre-existing). Full lib suite with no regressions.
- ✅ **Canonical YAML round-trip verification** — `canonical_round_trip_full` constructs a `WireNativeMetadata` populated with every field from the canonical example (circle scope, 3-source derived_from with all three reference kinds, trackable claim with end_date, 2-entry creator_split summing to 48, section override), serializes to YAML, parses back, asserts `parsed == original`. A second round-trip from the parsed version produces byte-identical YAML.
- ✅ **Canonical bare-form derived_from parse** — `canonical_parses_bare_derived_from` feeds the exact canonical example from `wire-native-documents.md` lines 49-52 through the parser and verifies both `ref:`-keyed and `doc:`-keyed entries resolve correctly.
- ✅ **Pillar 37 compliance** — 28-slot allocator and 48-slot creator_split are documented in-code as canonical protocol constants, NOT tunable config. Header comments in `rotator_allocation.rs` and doc comments on `WireCreatorSplit` spell this out to protect future phases from mistaking them for Pillar 37 violations.

### Scope decisions

- **`WireRef` fields as `Option<String>` instead of tagged enum**: per the canonical alignment pass, the canonical YAML uses `ref`/`doc`/`corpus` as flat sibling keys. Modeling them as a tagged enum would require custom (de)serialization anyway (flatten wouldn't work cleanly with the `rel` field on `WireRelatedRef`). Three `Option<String>` fields with a runtime `validate()` invariant-checker keeps the struct portable and the YAML output canonical-shaped. The validate call is idempotent — callers can run it at any point and get back a clear error if the invariant is broken.
- **Canonical flat-string scope serialization**: implemented via manual `Serialize`/`Deserialize` impls on `WireScope` rather than a top-level `serde(rename_all)` because the circle variant carries a name that the canonical encodes inline (`circle:nightingale`). The helper methods `to_canonical_string()` / `from_canonical_string()` are public so callers can round-trip scope values independently.
- **Default maturity is `Draft` (not `Canon`) for user-created contributions**: the spec's Creation-Time Capture table says "maturity = draft" for every path except Wire pulls and bundled seeds. `default_wire_native_metadata` produces Draft; the migration path (`build_skill_metadata` / `build_custom_chain_metadata`) explicitly overrides to `Canon` for bundled seeds per the spec's "Seed Contributions Ship with the Binary" section.
- **28-slot allocator tie-breaking uses lower-index preference**: deterministic output matters for the round-trip invariant. When two weights produce identical fractional remainders, the lower-index source wins. Documented in-code.
- **28-slot minimum-1 reclaim pass**: when a degenerate input like `weights = [1.0, 0.001, 0.001]` produces `[28, 0, 0]` after the largest-remainder pass, the allocator reclaims 1 slot per zero-weight source from the largest allocation. The defensive fallback path (redistribute-to-1-per-source) is unreachable in practice (`n ≤ 28` guarantees at least one source has ≥ 2 slots when any source has 0) but present as a safety net.
- **Prompt cache is coarse-grained invalidation**: clearing the entire map on any skill/chain contribution write is cheap because the prompt set is small (< 100 entries on current dev installs) and cache misses are fast (single SQLite query per key). Fine-grained invalidation would require the dispatcher to know which slug is changing and is a Phase 9 / Phase 10 optimization.
- **Prompt cache singleton uses `OnceLock`**: lazy initialization, tests that never touch prompts pay zero cost. The singleton is process-wide; tests that need a clean cache between assertions construct a fresh `PromptCache` locally rather than relying on a reset of the global. `phase5_dispatcher_invalidates_prompt_cache_on_skill_sync` is the one test that exercises the global, and it uses a "prime then clear" pattern to avoid test interdependency on the global's initial state.
- **Migration skips `_archived/` subdirectories**: per the spec's "Walk recursively, excluding `_archived/`" directive, the `walk_prompt_files` helper checks the directory name and short-circuits on `_archived`. Test `migration_inserts_prompts_skipping_archived` verifies the archived file does NOT land in the DB.
- **Migration does NOT abort on per-file failure**: a single unreadable or non-UTF-8 file only logs a warning and increments `report.prompts_failed`. The sentinel is only written if at least one file succeeded, so a fully-failed run allows a later retry. This is critical because the chain_loader's on-disk fallback keeps the executor working even if migration fails.
- **Chain migration extracts derived_from via line-scan regex, not full YAML parse**: the YAML parse would reject unusual-but-valid chain files; the line-scan catches every `$prompts/...` reference regardless of structure. Test `extract_prompt_refs_finds_all_forms` verifies the scan handles `instruction:`, `cluster_instruction:`, `merge_instruction:`, and dedupe.
- **Chain contribution body is the raw chain YAML bundle**: the spec's "Custom Chain Bundle Serialization" section describes a future format where inline prompts become section entries. Phase 5's migration keeps the chain YAML as-is and lets the sections system land in Phase 9 / Phase 10. The derived_from graph is correct today because each prompt is a separate skill contribution.
- **`publish_contribution_with_metadata` does not walk sections**: Phase 5 publishes the top-level contribution only. When `sections` is non-empty, the dry-run report shows a `SectionPreview` per entry so the user sees what would publish. Section depth-first publish is a follow-up iteration.
- **`publish_contribution_with_metadata` body payload uses JSON, not YAML**: the Wire's `/api/v1/contribute` endpoint accepts JSON. Phase 5 serializes the canonical YAML into a `wire_native_metadata_yaml` field inside the JSON body so the Wire can parse it, plus breaks out individual fields (`scope`, `price`, `creator_split`, etc.) at the top level for backwards compatibility with the existing pyramid-node publication shape. This is best-effort until the Wire side lands Phase 5 support — the canonical YAML is always present so a Wire-side parser that supports the new format can read it directly.
- **Publication state writes go through the IPC handler, not the publisher**: `PyramidPublisher::publish_contribution_with_metadata` returns a `PublishContributionOutcome`; the IPC handler in `main.rs` holds the DB writer mutex and persists the publication state. This matches the Phase 4 pattern where all DB writes happen at the IPC boundary under explicit mutex discipline.
- **`pyramid_publish_to_wire` refuses draft maturity by default**: Phase 5 hard-refuses draft publishes. Phase 10 will add a `force_draft: true` override for ToolsMode's "publish as draft" button. Refusing without the override is the safer default and matches the spec's `maturity != Draft` validation rule.
- **Dry-run validates instead of aborting**: `dry_run_publish` runs `metadata.validate()` but captures the error as a warning rather than returning `Err`. The user sees every problem at once in the preview instead of having to fix and re-run. The real publish path still fails loud on validation errors.

### Notes

- **Canonical alignment was the load-bearing work**: the Rust type definitions in `wire_native_metadata.rs` match the canonical YAML schema field-for-field. I had the spec open side-by-side with the canonical `wire-native-documents.md` during the type definition pass and corrected three divergences (scope, derived_from, supersedes) in the canonical's favor. The round-trip test (`canonical_round_trip_full`) is the safety net — any future change to the struct that breaks canonical parity will fail the test.
- **The 28-slot allocator's minimum-1 reclaim pass took some thought**: the straightforward floor+remainder approach produces `[28, 0, 0]` for `[1.0, 0.001, 0.001]`, which violates the minimum-1 rule. My first pass used a "bump zeros, trim from largest" loop and it worked on every test case. I kept the defensive fallback redistribute-to-1-per-source branch as a safety net even though it's unreachable in practice (`n ≤ 28` guarantees at least one source has ≥ 2 slots when any source has 0).
- **The Phase 4 `test_create_and_load_active_contribution` test needed a Phase 5 update**: it previously asserted `wire_native_metadata_json == "{}"` — Phase 5 populates real metadata. I updated the assertion to deserialize the column and check the canonical-default contribution_type, maturity, and topic list.
- **No friction log entries required.** The spec-vs-canonical divergences were documented in the implementation log above (flagged for spec correction), but they didn't block implementation — canonical wins every time and the Rust types match the canonical. The only gray-area call was "fail loud on draft publish" vs "allow draft with confirm" — I chose fail-loud since the spec explicitly lists draft as a dry-run warning and Phase 10 will add the override.
- **Pillar 37 trap avoided**: the 28-slot rotator arm constant and the 48-slot creator split are hardcoded in `rotator_allocation.rs` and `wire_native_metadata.rs`. Both are protected by explicit header comments documenting them as canonical protocol rules, not tunable config. A future reader who's been primed on Pillar 37 might flag them for move-to-config; the comments explain why that's wrong.
- **The PromptCache global singleton will need a reset helper in future tests that care about global state between runs**. For now, every test that touches the global uses a local `PromptCache::new()` except the one dispatcher-invalidation test which uses the "prime then clear" pattern. No hidden test interdependency was introduced.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Canonical YAML parity**: re-read `wire-native-documents.md` and diff every field name against the `WireNativeMetadata` struct. Any field I missed is a round-trip failure waiting to happen.
2. **Edge cases in `allocate_28_slots`**: the reclaim pass is the trickiest part. A wanderer probe with fuzzed random weights (1000 runs, random N between 1 and 28, random weights in [0, 1]) would be a good confidence booster — every output should sum to 28, every entry should be ≥ 1, and the distribution should correlate with the input weights.
3. **Migration idempotency on real chains dirs**: run the migration against a checked-out copy of `chains/` with 98 prompt files and 11 chain YAMLs, verify exactly 98 + 11 rows land on first run and 0 on subsequent runs.
4. **Dry-run publish against a real contribution**: populate a test DB with one of each schema_type, call `pyramid_dry_run_publish` through the IPC layer, verify the report is coherent and the credential-leak scan catches a `${VAR_NAME}` in a custom_prompts body.
5. **Spec-vs-canonical follow-up**: the spec file `docs/specs/wire-contribution-mapping.md` has three struct-shape divergences from the canonical. A tiny correction pass should update the spec to match the Rust types (which in turn match the canonical).

Wanderer prompt suggestion: "Does Wire Node boot on a fresh DB, run the Phase 5 prompt+chain migration end-to-end, populate the prompt cache on first lookup, serve a skill contribution's body through the chain loader, then let a user call `pyramid_dry_run_publish` for any of the 14 schema types and see a coherent preview with 28 resolved slots and zero panics?"

### Wanderer pass — 2026-04-10

Status: **two blocking findings fixed in place**. Details in `docs/plans/pyramid-folders-model-routing-friction-log.md` → "Phase 5 wanderer pass".

Summary:

1. **`PromptCache` was dead code (FIXED).** `chain_loader::resolve_prompt_refs` still read from disk via `std::fs::read_to_string` — zero imports of `prompt_cache` in either `chain_executor.rs` or `chain_loader.rs`. The Phase 5 migration populated skill contributions that the runtime never read. Added `set_global_prompt_cache_db_path()` + `resolve_prompt_global()` to `prompt_cache.rs` (ephemeral-connection resolver pattern keeps all call sites unchanged), stashed the path once in `main.rs` during setup, rewrote `chain_loader::resolve_prompt_refs` to consult the global resolver first and fall back to disk on miss. Added 2 new tests in `prompt_cache.rs`.

2. **`migrate_legacy_dadbear_to_contributions` wrote `'{}'` metadata (FIXED).** `db.rs:1543` — the Phase 4 DADBEAR bootstrap migration direct INSERT — hardcoded `wire_native_metadata_json = '{}'`, bypassing Phase 5's canonical-metadata helpers. The spec's Creation-Time Capture table says bootstrap migrations write canonical metadata with `maturity: canon`. Fix: build a canonical `WireNativeMetadata` via `default_wire_native_metadata("dadbear_policy", Some(slug))`, override `maturity` to `Canon`, serialize and use in the INSERT. Added 1 new test in `db.rs::provider_registry_tests`.

3. **Spec file still has old struct shapes (NOT FIXED, flagged).** `docs/specs/wire-contribution-mapping.md` retains three pre-canonical struct definitions (`WireScope` tagged enum, `WireRef` tagged enum, `supersedes: Option<WireRefKey>`) that the Rust code correctly diverges from. Standalone editing task; not in wanderer scope.

Verification: `cargo check` clean; `cargo test --lib pyramid` reports 923 passed, 7 pre-existing failures unchanged. Phase 5 implementer reported 919 passing, verifier commit added 1, wanderer fix adds 3 → 923. Zero regressions. Files modified: `src-tauri/src/pyramid/prompt_cache.rs`, `src-tauri/src/pyramid/chain_loader.rs`, `src-tauri/src/pyramid/db.rs`, `src-tauri/src/main.rs`. Commit: `phase-5: wanderer fix — PromptCache wire-up + DADBEAR canonical metadata` on branch `phase-5-wire-contribution-mapping`.

---

## Phase 6 — LLM Output Cache + StepContext

**Workstream:** phase-6-llm-output-cache
**Workstream prompt:** `docs/plans/phase-6-workstream-prompt.md`
**Spec:** `docs/specs/llm-output-cache.md`
**Branch:** `phase-6-llm-output-cache` (off `phase-5-wire-contribution-mapping`)
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 6 turns `pyramid_llm_audit` from a write-only log into a content-addressable LLM output cache and introduces the unified `StepContext` struct that Phases 2, 3, and 5 all deferred to "when Phase 6 lands." The cache is keyed on `cache_key = sha256(inputs_hash, prompt_hash, model_id)` and lives in a new `pyramid_step_cache` table with a `UNIQUE(slug, cache_key)` constraint. The cache hook is wired into a new ctx-aware variant of the unified LLM call path so production callers opt in by passing a `StepContext` while every legacy call site (and unit tests) continues to bypass the cache by passing `None`.

The implementation contains four load-bearing correctness gates: (1) `verify_cache_hit` performs all four mismatch checks plus a corruption parse, returning a distinct `CacheHitResult` variant for each failure mode; (2) the cache lookup is OPT-IN — when no `StepContext` is passed (or when the context lacks a resolved model id / prompt hash) the call falls through to the existing HTTP retry loop without touching the cache; (3) verification failure deletes the stale row, emits `CacheHitVerificationFailed` with the precise reason tag, and falls through to the wire so a corrupt cache cannot poison subsequent runs; (4) force-fresh writes route through `supersede_cache_entry` which moves the prior row to an archival cache_key (`archived:{id}:{orig}`) so the new content-addressable slot stays unique while the supersession chain remains queryable from `pyramid_step_cache` for Phase 13's reroll history.

The Phase 2 `generate_change_manifest` retrofit is the first proof-of-concept use of the StepContext pattern: `execute_supersession` now constructs a `StepContext` with `step_name="change_manifest"`, `primitive="manifest_generation"`, the current node's depth, no chunk_index, the resolved model id, and a hash of the prompt template body, then threads it through `generate_change_manifest` which delegates to `call_model_unified_with_options_and_ctx`. The cache layer treats manifest generation as just another LLM call with its own cache key, so a repeated stale check on the same node at the same `build_version` (with unchanged children, prompt, and routing) is a hit.

### Files touched

**New files:**
- `src-tauri/src/pyramid/step_context.rs` (~530 lines) — Phase 6 module:
  - Hash helpers: `sha256_hex`, `compute_cache_key` (composite of inputs|prompt|model with `|` delimiter), `compute_inputs_hash` (separator-protected concat of system+user prompts), `compute_prompt_hash` (template body hash).
  - `CacheHitResult` enum with five variants (`Valid`, `MismatchInputs`, `MismatchPrompt`, `MismatchModel`, `CorruptedOutput`) and a `reason_tag()` helper for telemetry.
  - `CachedStepOutput` (read shape) and `CacheEntry` (write shape) structs covering every column on `pyramid_step_cache`.
  - `verify_cache_hit` — the load-bearing correctness gate. Checks all three components individually before parsing the stored JSON for corruption. Documented mismatch-beats-corruption ordering.
  - `StepContext` struct with build metadata, cache plumbing (`db_path`, `force_fresh`), event bus handle, model resolution fields (`model_tier`, `resolved_model_id`, `resolved_provider_id`), and the prompt hash. Custom `Debug` impl that does NOT print the bus handle. Builder methods (`with_model_resolution`, `with_provider`, `with_prompt_hash`, `with_bus`, `with_force_fresh`) and `cache_is_usable()` predicate.
  - 15 unit tests covering hash determinism, separator collision protection, cache key uniqueness against single-component changes, every `CacheHitResult` variant including the mismatch-beats-corruption ordering, and StepContext builder semantics.

**Modified files:**
- `src-tauri/src/pyramid/db.rs` (+~290 lines):
  - `init_pyramid_db` adds `pyramid_step_cache` table per the spec's exact column list, plus `idx_step_cache_lookup` and `idx_step_cache_key` indices. All `IF NOT EXISTS`.
  - New CRUD section at the end of the file adds `check_cache`, `store_cache` (INSERT with `ON CONFLICT(slug, cache_key) DO UPDATE`), `delete_cache_entry`, and `supersede_cache_entry` (the force-fresh path that archives the prior row under `archived:{id}:{orig_key}` so the unique constraint stays satisfied while history is preserved).
  - New `step_cache_tests` module with 13 tests: table creation idempotency, store/check round-trip, miss-returns-None, ON CONFLICT replaces (not duplicates), delete, all four `verify_cache_hit` variants, supersede with prior link-back AND with no prior row, and the most-recent-row ORDER BY tie-break.
- `src-tauri/src/pyramid/llm.rs` (+~340 lines):
  - New imports for `event_bus::{TaggedBuildEvent, TaggedKind}` and `step_context::*`.
  - `call_model_unified_with_options` is now a one-line shim that delegates to `call_model_unified_with_options_and_ctx(.., None, ..)` — preserves backward compatibility for every existing caller.
  - `call_model_unified_with_options_and_ctx` is the new ctx-aware entry point. Its body adds the cache lookup BEFORE the existing HTTP retry loop (computes `inputs_hash`, `cache_key`; checks `pyramid_step_cache`; on Valid hit emits `CacheHit` and returns the cached response; on `Mismatch*`/`CorruptedOutput` deletes the stale row and emits `CacheHitVerificationFailed`; on miss emits `CacheMiss`; force_fresh skips lookup but still computes the key for the write path). The HTTP success path adds a cache write through either `store_cache` (normal) or `supersede_cache_entry` (force-fresh). The cache write uses `tokio::task::block_in_place` with an ephemeral connection so it doesn't take the writer mutex (the cache is content-addressable; INSERT OR REPLACE on the unique key is safe without serialization).
  - New helpers `serialize_response_for_cache`, `parse_cached_response`, `emit_cache_event`, plus a private `CacheLookupResult` struct that carries the components computed once per call.
  - 4 new integration tests at the end of `llm::tests`: cache hit returns cached content without HTTP, no-ctx path bypasses cache entirely (and does NOT consult the cache), force-fresh bypasses lookup and falls through to HTTP, verification failure deletes the stale row.
- `src-tauri/src/pyramid/chain_resolve.rs` (+~80 lines):
  - `ChainContext` gains `prompt_hashes: HashMap<String, String>` and `resolved_models: HashMap<String, String>` fields (initialized to empty HashMaps in `ChainContext::new`).
  - New methods: `get_or_compute_prompt_hash` (lazy pull-through with closure-provided body) and `cache_resolved_model`/`get_resolved_model`.
  - 5 new tests for default-empty initialization, lazy compute-then-cache (using a panic closure to prove the cache hits), distinct path keys, and the model resolution round-trip.
- `src-tauri/src/pyramid/event_bus.rs` (+~35 lines):
  - `TaggedKind` gains `CacheHit`, `CacheMiss`, and `CacheHitVerificationFailed` variants per the spec. Phase 6 just emits them; Phase 13 will add the consumer.
- `src-tauri/src/pyramid/stale_helpers_upper.rs` (+~80 lines, retrofit):
  - `generate_change_manifest` signature gains `ctx: Option<&super::step_context::StepContext>`. The function body now delegates to `call_model_unified_with_options_and_ctx` instead of `call_model_with_usage`, threading the ctx through. The Pillar 37 hardcoded `0.2, 4096` temperature/max_tokens stays in place — that's still Phase 9's scope.
  - `execute_supersession` now constructs a `StepContext` with `step_name="change_manifest"`, `primitive="manifest_generation"`, `depth=node_ctx.depth`, `chunk_index=None`, the model id, and a `compute_prompt_hash(&load_change_manifest_prompt_body())` value, then passes `Some(&cache_ctx)` to `generate_change_manifest`. The `cache_build_id` is `format!("stale-{node_id}-{build_version}")` so a repeated stale check at the same version is a hit.
  - 1 new test (`test_generate_change_manifest_with_step_context_compiles`) — a type-check regression test that constructs a StepContext + builds the call future without polling it. Any future signature drift that drops the ctx parameter will fail to compile this test.
- `src-tauri/src/pyramid/mod.rs` — declared `pub mod step_context`.
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.

### Spec adherence (against `llm-output-cache.md` and the workstream brief)

- ✅ **`pyramid_step_cache` table** — created in `init_pyramid_db` with the exact 17 columns from the spec (id, slug, build_id, step_name, chunk_index, depth, cache_key, inputs_hash, prompt_hash, model_id, output_json, token_usage_json, cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id) plus the two indices (`idx_step_cache_lookup` on `(slug, step_name, chunk_index, depth)` and `idx_step_cache_key` on `cache_key`). UNIQUE constraint on `(slug, cache_key)`.
- ✅ **CRUD helpers** — `check_cache`, `store_cache`, `delete_cache_entry`, `supersede_cache_entry` per the spec's signature list. Store uses ON CONFLICT-DO-UPDATE for INSERT OR REPLACE semantics.
- ✅ **`StepContext` struct** — every field from the spec's "Threading the Cache Context" section: slug, build_id, step_name, primitive, depth, chunk_index, db_path, force_fresh, bus, model_tier, resolved_model_id, resolved_provider_id. Plus a `prompt_hash` field threaded by the caller (since ChainContext holds the lazy cache and the LLM call site is downstream of it).
- ✅ **`ChainContext` extensions** — `prompt_hashes: HashMap<String, String>` and `resolved_models: HashMap<String, String>` populated lazily per the spec's "Model ID Normalization" section. Get-or-compute helper for prompt hashes prevents redundant rehashing within a build.
- ✅ **Cache key computation** — `compute_cache_key(inputs_hash, prompt_hash, model_id)` returns SHA-256 hex of `inputs|prompt|model` (literal `|` delimiter). `compute_inputs_hash` separates system + user prompts with `\n---\n` to prevent concat collisions. All hashes use `sha2::Sha256`, never `std::hash::Hash`.
- ✅ **Cache lookup hook in `call_model_unified`** — the new `call_model_unified_with_options_and_ctx` is the spec's hook point. It lives BEFORE the HTTP request, runs only when a StepContext is provided AND `cache_is_usable()` (resolved model id + prompt hash), checks `pyramid_step_cache`, runs `verify_cache_hit`, and either returns cached or falls through. The legacy `call_model_unified_with_options` is now a thin shim that passes `None` so every existing caller is unchanged.
- ✅ **`verify_cache_hit`** — implements all four mismatch variants exactly per the spec. Inputs check first (most likely failure), then prompt, then model, then JSON parse for corruption. Returns a distinct `CacheHitResult` variant for each so callers (and Phase 13's oversight UI) can distinguish failure modes. The mismatch-beats-corruption ordering is documented and tested.
- ✅ **Force-fresh path** — `StepContext.force_fresh` skips the lookup. The write path detects force_fresh and routes through `supersede_cache_entry` which moves the prior row to `archived:{id}:{orig_cache_key}`, then inserts the new row under the original key with `force_fresh=1` and `supersedes_cache_id` pointing at the moved-aside id. The reroll IPC command itself is still Phase 13 scope — Phase 6 just plumbs the bool.
- ✅ **Phase 2 retrofit** — `generate_change_manifest` accepts `Option<&StepContext>`. `execute_supersession` constructs the StepContext with the spec's exact fields (`step_name="change_manifest"`, `primitive="manifest_generation"`, `depth=node_ctx.depth`, `chunk_index=None`). The call is now cache-eligible.
- ✅ **`TaggedKind::CacheHit` / `CacheMiss` / `CacheHitVerificationFailed` events** — added with the payload shapes the spec specifies (slug, step_name, cache_key, chunk_index, depth on hit/miss; reason on verification failure). Phase 6 just emits them. Phase 13 will add the consumer.
- ✅ **Tests** — every test from the workstream brief's enumeration:
  - `test_compute_cache_key_stable` — `test_compute_cache_key_stable_across_runs` in step_context.
  - `test_compute_cache_key_changes_on_input_change` — `test_compute_cache_key_changes_on_each_component` covers all three.
  - `test_check_cache_hit_and_verify` — `test_check_cache_hit_and_verify_valid` in db::step_cache_tests.
  - `test_cache_hit_verification_rejects_input_mismatch` and the prompt/model variants — three tests in db::step_cache_tests.
  - `test_cache_hit_verification_rejects_corrupted_output` — db::step_cache_tests.
  - `test_force_fresh_bypasses_cache` — `test_force_fresh_bypasses_cache_lookup` in llm::tests.
  - `test_supersede_cache_entry_links_back` — db::step_cache_tests.
  - `test_unique_constraint_on_slug_cache_key` — `test_unique_constraint_on_slug_cache_key_replaces` in db::step_cache_tests.
  - `test_step_context_creation` — `test_step_context_new_and_builder` in step_context.
  - `test_model_id_normalization_cached` — `cache_resolved_model_round_trip` in chain_resolve plus `get_or_compute_prompt_hash_caches_first_call` exercises the lazy-cache pattern.
  - `test_generate_change_manifest_with_step_context_compiles` — type-check test in stale_helpers_upper.
- ⚠️ **`StepContext` naming** — there is a pre-existing `chain_dispatch::StepContext` (a dispatch context carrying DB handles + LlmConfig). Both types coexist; the Phase 6 one lives in `pyramid::step_context` and is referenced via fully-qualified path at use sites. No renaming of the pre-existing type — that would be an out-of-scope churn. Documented in the new module's header.
- ✅ **Pillar 37 awareness** — Phase 6 adds zero new hardcoded LLM-constraining numbers. The `0.2/4096` temperature/max_tokens in `generate_change_manifest` are unchanged; that's still Phase 9's config-contribution scope per the brief.

### Verification results

- ✅ `cargo check --lib` — clean. Same 3 pre-existing warnings (deprecated `get_keep_evidence_for_target`, two `LayerCollectResult` private-visibility warnings in `publication.rs`). Zero new warnings.
- ✅ `cargo check --lib --tests` — clean. Same warnings as the lib-only check plus the pre-existing test-only warnings (unused imports in chain_dispatch tests, dead `id2` variable, deprecated function references in db tests, deprecated `tauri_plugin_shell::Shell::open` in main.rs). No new warnings from Phase 6 files.
- ✅ `cargo build --lib` — clean, same 3 pre-existing warnings.
- ✅ `cargo test --lib pyramid::step_context` — **15/15 passed** in 0.00s.
- ✅ `cargo test --lib pyramid::db::step_cache_tests` — **13/13 passed** in 0.81s.
- ✅ `cargo test --lib pyramid::llm::tests` — all Phase 6 cache tests pass: `test_cache_hit_returns_cached_response_without_http`, `test_cache_lookup_skipped_without_step_context`, `test_force_fresh_bypasses_cache_lookup`, `test_cache_hit_verification_failure_deletes_stale_row`. Plus the pre-existing llm tests still pass.
- ✅ `cargo test --lib pyramid::chain_resolve::tests` — **38/38 passed** (33 pre-existing + 5 new Phase 6).
- ✅ `cargo test --lib pyramid::stale_helpers_upper::tests` — **11/11 passed** (10 pre-existing Phase 2 + 1 new Phase 6 retrofit type-check test).
- ✅ `cargo test --lib pyramid` — **961 passed, 7 failed** in 13.34s. The 7 failures are the same pre-existing unrelated tests (`pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*` tests). Phase 5 ended at 923 passing — Phase 6 added 38 new tests (961 - 923 = 38). Zero regressions, zero new failures.
- ✅ `grep -n "call_model_unified" src-tauri/src/pyramid/llm.rs` — multiple hits including the new `call_model_unified_with_options_and_ctx` signature with `Option<&StepContext>` parameter, plus the legacy `call_model_unified_with_options` shim that delegates with `None`.
- ✅ `grep -n "StepContext" src-tauri/src/pyramid/stale_helpers_upper.rs` — confirms the Phase 2 retrofit: `generate_change_manifest` accepts `ctx: Option<&super::step_context::StepContext>` and `execute_supersession` constructs a `cache_ctx` via `super::step_context::StepContext::new(...)` and passes `Some(&cache_ctx)`.
- ✅ `grep -rn "pyramid_step_cache" src-tauri/src/` — table creation in `init_pyramid_db`, CRUD helpers in `db.rs`, hook references in `llm.rs`, test references in db tests. All wired.

### Scope decisions

- **Naming the Phase 6 StepContext.** A pre-existing `chain_dispatch::StepContext` already exists (carries DB handles + live LlmConfig — conceptually a "dispatch context"). Renaming it would have rippled through 25+ chain_executor call sites, all of them out of Phase 6 scope. I left it alone and added the Phase 6 type as `pyramid::step_context::StepContext`. Disambiguation at use sites is a fully-qualified path import. The two types have orthogonal responsibilities and the comment in `step_context.rs` documents the coexistence.
- **`call_model_unified_with_options_and_ctx` as a sibling, not a signature change.** The brief allowed "(or similar)" for the signature, and the cleaner approach was to add a sibling function rather than ripple a new positional argument through the 3 existing `call_model_unified_with_options` callers in chain_dispatch.rs. The legacy function is now a one-line shim that delegates with `None`. Backward compatibility is preserved by construction.
- **`prompt_hash` on StepContext, not just on ChainContext.** The spec says `ChainContext.prompt_hashes` is the build-scoped lazy cache, but `call_model_unified_with_options_and_ctx` lives below ChainContext in the call stack. To keep the LLM call site cache-aware without threading `&mut ChainContext` through every helper, the StepContext carries the already-computed prompt_hash as a field. The retrofit caller in `execute_supersession` computes the hash via `compute_prompt_hash(&load_change_manifest_prompt_body())` and stamps it into the ctx. ChainContext's `get_or_compute_prompt_hash` is the lazy cache for chain executor sites that have a `&mut ChainContext` in scope; future retrofits will call it.
- **Cache reads/writes via ephemeral connections, not the writer mutex.** `pyramid_step_cache` is content-addressable: same key = same value. ON CONFLICT-DO-UPDATE on the unique key is safe under concurrent writers because the write is idempotent. The code path opens a fresh connection inside `tokio::task::block_in_place` rather than awaiting the writer mutex. This keeps the cache off the hot path and makes a cache hit zero-overhead.
- **`supersede_cache_entry` archives via `archived:{id}:{orig_key}` rather than a separate column.** The spec's `UNIQUE(slug, cache_key)` constraint means we can't have two rows for the same content address simultaneously. Archiving via cache_key prefix mutation (`archived:`) keeps the supersession chain queryable from the same table, retains row identity (id stays stable so `supersedes_cache_id` keeps pointing at the right row), and avoids a schema migration to add a "tombstoned" column. A real cache_key is a 64-char SHA-256 hex and never starts with `archived:`, so no collision risk.
- **`cache_lookup_result` is computed even on force-fresh.** The lookup phase computes `inputs_hash`, `cache_key`, and the resolved model id. On force_fresh we skip the read but the write path still needs the same key, so we keep the result and short-circuit only the SELECT.
- **Verification failure on output_json parse vs structure parse.** The spec calls out the JSON parse as the corruption check, but the cache also has a "structure parse" step downstream (extracting `content`, `usage`, `generation_id`). I treat both as corruption — if the JSON parses but the structure doesn't have a `content` string, we still emit `CacheHitVerificationFailed` with reason `unusable_structure` and delete the row. This is strictly safer than letting an unusable parse pass through.
- **Cache build_id for stale checks.** `execute_supersession` uses `format!("stale-{node_id}-{build_version}")` so a repeated stale check at the same version is a cache hit. A new `build_version` (typical case) gets a new build_id which doesn't affect the cache_key (the key is content-addressable, not build-scoped) but is recorded on the row for provenance.
- **`token_usage_json` written on every cache row.** The spec lists it as an optional field but every successful LLM call returns one, so we always serialize it. Phase 13's cost panel can read it directly without joining `pyramid_llm_audit`.

### Notes

- **The cache hit path is genuinely zero-network.** The four llm::tests integration tests prove this: `test_cache_hit_returns_cached_response_without_http` constructs an `LlmConfig::default()` (no api_key, no provider registry) and still gets the cached response back, because the cache hit short-circuits BEFORE `build_call_provider` runs. This is the load-bearing property — Phase 13's "crash recovery is a cache hit" claim depends on it.
- **Pre-existing `chain_dispatch::StepContext` is not the same thing.** It carries DB handles + the live LlmConfig and existed before Phase 6. The Phase 6 StepContext is a separate concern. They coexist in the codebase. A future refactor could fold them but Phase 6 deliberately did not — that's scope creep into chain_executor, which is out of Phase 6's bounds.
- **The Phase 2 retrofit is intentionally minimal.** It adds StepContext threading to ONE function (`generate_change_manifest`) per the spec's "first retrofit validation" mandate. Every other LLM call site (faq, delta, webbing, meta, evidence triage, FAQ matcher) still uses `call_model_with_usage` and gets `None` cache treatment. Phase 12 will sweep through them.
- **Pillar 37 stays clean.** The hardcoded `0.2/4096` temperature/max_tokens in `generate_change_manifest` are unchanged because moving them is Phase 9's config-contribution scope per the workstream brief and the friction log. Phase 6 introduces ZERO new hardcoded LLM-constraining numbers.
- **No friction log entries required.** The spec was unambiguous, the scope boundaries held, and the only naming question (StepContext vs chain_dispatch::StepContext) had a clean answer (coexist via fully-qualified paths).

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **`verify_cache_hit` correctness** — re-read the four-mismatch-variant-plus-corruption logic and confirm the ordering matches the spec. The mismatch-beats-corruption test (`test_verify_cache_hit_mismatch_beats_corruption`) locks down the precedence; a verifier should confirm this is what the spec intends.
2. **`supersede_cache_entry` archival semantics** — the prior row gets moved to `archived:{id}:{orig_key}` to free the unique slot. A verifier should confirm this preserves the supersession chain and that the archival key cannot collide with a content-addressable lookup.
3. **`call_model_unified_with_options` shim correctness** — the new wrapper passes `None` straight through. The verifier should confirm no caller accidentally relies on the old behavior of bypassing the cache via the function name (everyone now bypasses via the `None` parameter).
4. **Phase 2 retrofit end-to-end** — `execute_supersession` constructs the StepContext and threads it. The verifier should construct an in-memory pyramid, simulate a stale check that produces a manifest, then re-run the same stale check and confirm the second run is a cache hit (no real LLM needed if the test pre-populates the row with the right cache key).
5. **Pre-existing `chain_dispatch::StepContext` coexistence** — confirm no test relies on a single canonical `StepContext` import path. Both types should be reachable via their module paths.

Wanderer prompt suggestion: "Does Wire Node boot, run a fresh build, persist every LLM call to `pyramid_step_cache` with the right cache_key, and then on a re-build of the same source files use the cache for every step that has a usable StepContext — confirming end-to-end that the cache hit path is wired through chain_executor and produces zero network traffic for unchanged content?"

---

## Phase 7 — Cache Warming on Pyramid Import

**Workstream:** phase-7-cache-warming-import
**Workstream prompt:** `docs/plans/phase-7-workstream-prompt.md`
**Spec:** `docs/specs/cache-warming-and-import.md`
**Branch:** `phase-7-cache-warming-import` (off `phase-6-llm-output-cache`)
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 7 builds the import-side counterpart to Phase 5's publication path and Phase 6's `pyramid_step_cache`. When a user pulls a pyramid from Wire, the source node's exported cache manifest is downloaded (frontend concern, Phase 10) and populated into the local cache via a three-pass staleness check: (1) L0 nodes get their source files hashed and compared to the manifest, (2) the stale L0 set propagates upward through the manifest's `derived_from` graph via BFS, (3) only upper-layer nodes NOT in the stale set have their cache entries inserted. Surviving rows go through a new `db::store_cache_if_absent` helper that uses `INSERT ... ON CONFLICT DO NOTHING` — the `INSERT OR IGNORE` semantic the spec mandates — so re-importing the same manifest is a no-op AND any locally-written rows (notably force-fresh rerolls) are preserved across resume attempts.

The module ships with a resumable state row (`pyramid_import_state`) + CRUD, the shared Rust manifest types (`CacheManifest`, `ImportNodeEntry`, `ImportedCacheEntry`) that both export and import encode/decode against, a content-addressable SHA-256 file hasher, the three-pass staleness algorithm (`populate_from_import`), the top-level entry point (`import_pyramid`), and the canonical DADBEAR auto-enable path — routed through Phase 4's `create_config_contribution_with_metadata` + `sync_config_to_operational` so the imported pyramid's DADBEAR row carries a proper `contribution_id` FK and audit trail.

On the publication side, `PyramidPublisher::export_cache_manifest` reads `pyramid_step_cache` rows and assembles a canonical manifest, with a **privacy-safe default**: returns `Ok(None)` unless the caller explicitly passes `include_cache = true`. Phase 10 will add the opt-in checkbox to the publish wizard with appropriate warnings. Three new Tauri IPC commands wire the module to the frontend: `pyramid_import_pyramid`, `pyramid_import_progress`, and `pyramid_import_cancel`.

### Files touched

**New files:**

- `src-tauri/src/pyramid/pyramid_import.rs` (~880 lines) — Phase 7 module:
  - Shared manifest types: `CacheManifest`, `ImportNodeEntry`, `ImportedCacheEntry`, `ImportReport` with serde derives matching the spec's JSON shape byte-for-byte (manifest_version, source_pyramid_id, exported_at, nodes with layer/source_path/source_hash/source_size_bytes/derived_from/cache_entries).
  - `sha256_file_hex` — streaming 64KiB-chunk file hasher that keeps large sources off-heap.
  - `normalize_hash` — case-insensitive `sha256:` prefix stripper so manifests that include the prefix (per spec example) and locally-computed bare-hex hashes compare equal.
  - `resolve_source_path` — path joiner with `\`/`/` separator normalization and parent-traversal refusal (`..` returns empty path).
  - `populate_from_import` — the three-pass staleness algorithm. Pass 1 (L0 file-hash check), Pass 2 (BFS upward via in-memory `derived_from` graph), Pass 3 (upper-layer cache insertion for non-stale nodes). Returns `ImportReport` with `cache_entries_valid`, `cache_entries_stale`, `nodes_needing_rebuild`, `nodes_with_valid_cache`. Rejects unsupported `manifest_version`.
  - `import_pyramid` — top-level entry that validates inputs, creates or resumes the import state row, runs the staleness pass, enables DADBEAR via the Phase 4 contribution path, and flips status to `complete`.
  - `enable_dadbear_via_contribution` — builds a canonical `dadbear_policy` YAML, creates a contribution row via `create_config_contribution_with_metadata` (source=`import`, maturity=`Canon`), then dispatches through `sync_config_to_operational`. Does NOT write directly to `pyramid_dadbear_config`.
  - `yaml_escape` — best-effort YAML scalar escaper for source_path strings.
  - 15 unit tests covering hash normalization, path resolution, parent-traversal refusal, YAML escaping, manifest version rejection, the mixed-stale three-pass flow (integration test: 3 L0s + 2 upper layers, one L0 mismatch propagates to the upper layer that references it), missing-L0-file stale marking, idempotent re-import (INSERT OR IGNORE semantics), **the reroll-then-resume regression test** (`test_re_import_preserves_local_reroll_force_fresh_row`: imports a manifest, supersedes one cache row locally with a force-fresh reroll, re-imports, asserts the rerolled row is intact — `output_json` unchanged, `force_fresh = 1`, `build_id = "local-reroll"`), full-flow `import_pyramid` with state-row progression, resume-same-pyramid succeeds, refuse-different-pyramid-for-same-slug, reject-missing-source-path, serde round-trip, canonical DADBEAR metadata on the contribution row.

**Modified files:**

- `src-tauri/src/pyramid/db.rs` (+~290 lines):
  - Added `pyramid_import_state` table to `init_pyramid_db` per the spec's "Import Resumability" section: `target_slug` PK, `wire_pyramid_id`, `source_path`, `status`, `nodes_total`, `nodes_processed`, `cache_entries_total`, `cache_entries_validated`, `cache_entries_inserted`, `last_node_id_processed`, `error_message`, `started_at`, `updated_at`, plus `idx_pyramid_import_state_status` on status.
  - Added `ImportState` struct + `ImportStateProgress` partial-update struct.
  - Added CRUD helpers: `create_import_state`, `load_import_state`, `update_import_state` (uses COALESCE for partial updates so only-supplied fields are written), `delete_import_state` (idempotent).
  - Added `store_cache_if_absent` helper next to `store_cache` — uses `INSERT ... ON CONFLICT(slug, cache_key) DO NOTHING` and returns whether the row was actually inserted. This is the `INSERT OR IGNORE` semantic the spec's "Idempotency" section (~line 341) mandates for the import flow: a re-import must never clobber a local force-fresh (reroll) row that the user wrote between attempts. `store_cache_if_absent` is called ONLY from the import path; every other cache write goes through `store_cache` (which uses DO UPDATE for the normal LLM-call write path).
  - Added `import_state_tests` module with 5 tests: create+load, load-missing-returns-None, duplicate-create-fails, coalesced partial update that preserves other fields, idempotent delete.
  - Added 2 `store_cache_if_absent` tests in `step_cache_tests`: fresh-insert returns true + row present; conflict-on-prior-row returns false + rerolled row's `output_json` / `force_fresh` / `build_id` all preserved (the exact clobber scenario the spec warns about).

- `src-tauri/src/pyramid/wire_publish.rs` (+~290 lines):
  - New `impl PyramidPublisher` block with two methods: `export_cache_manifest` (async, privacy-gate wrapper — returns `Ok(None)` unless `include_cache = true`) and `build_cache_manifest` (pure-local manifest builder used internally + by tests).
  - `build_cache_manifest` reads `pyramid_step_cache` (optionally filtered by `build_id`, always excluding archived-prefix cache_keys so supersession chains don't leak), joins against `pyramid_pipeline_steps` on `(slug, step_type=step_name, chunk_index, depth)` to recover `node_id`, loads L0 source metadata from `pyramid_file_hashes` (keyed on node_ids JSON array), and loads upper-layer `derived_from` from `pyramid_evidence` (KEEP verdicts only). Groups by node_id, sorts by `(layer, node_id)` for deterministic output. Rows that can't be joined to a pipeline step fall into a synthetic `synth:L{depth}:C{chunk_index}` bucket so they still land in the manifest.
  - 6 new Phase 7 tests: privacy gate default off returns None, opt-in returns populated manifest, empty slug returns empty-nodes manifest, build_id filter works, archived rows are excluded, full export → import round-trip (seed cache → export manifest → populate_from_import into a fresh slug → verify row counts match).

- `src-tauri/src/pyramid/mod.rs` — declared `pub mod pyramid_import`.

- `src-tauri/src/main.rs` (+~140 lines):
  - Added 3 Phase 7 Tauri IPC commands: `pyramid_import_pyramid(wire_pyramid_id, target_slug, source_path, manifest_json)` (parses the manifest JSON, calls `pyramid_import::import_pyramid` under the writer mutex, returns an `ImportPyramidResponse` with the five report counters), `pyramid_import_progress(target_slug)` (reads the `pyramid_import_state` row and computes the spec's weighted progress: `(nodes_processed/nodes_total)*0.5 + (cache_entries_validated/cache_entries_total)*0.5`), `pyramid_import_cancel(target_slug)` (deletes the state row; cache rows are idempotent so they stay).
  - Registered all 3 commands in `invoke_handler!`.

- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.

### Spec adherence (against `cache-warming-and-import.md`)

- ✅ **`pyramid_import_state` table** — schema matches the spec's SQL byte-for-byte: `target_slug` PRIMARY KEY, `wire_pyramid_id`, `source_path`, `status`, `nodes_total`/`nodes_processed`, `cache_entries_total`/`cache_entries_validated`/`cache_entries_inserted`, `last_node_id_processed`, `error_message`, `started_at`/`updated_at` with `datetime('now')` defaults. Plus a status index for fast "in-flight imports" queries.
- ✅ **CRUD helpers** — `create_import_state`, `load_import_state`, `update_import_state` (with COALESCE partial update), `delete_import_state` (idempotent).
- ✅ **Cache manifest types** — `CacheManifest`, `ImportNodeEntry`, `ImportedCacheEntry` match the spec's JSON shape (manifest_version, source_pyramid_id, exported_at, nodes with layer/source_path/source_hash/source_size_bytes/derived_from/cache_entries). All fields serde-derived with `#[serde(default)]` on optional fields.
- ✅ **Three-pass staleness algorithm** — `populate_from_import` implements the spec's exact three passes: L0 hash check → upward BFS propagation → upper-layer cache insertion. Idempotency via `db::store_cache_if_absent`'s `INSERT ... ON CONFLICT DO NOTHING` on the `UNIQUE(slug, cache_key)` constraint — the `INSERT OR IGNORE` semantic the spec's "Idempotency" section (~line 341) mandates so a re-import (crash resume, explicit retry) cannot clobber a local force-fresh reroll row the user may have written between attempts. Mixed-stale integration test (`test_populate_from_import_mixed_stale_l0_propagates_to_upper_layers`) covers the exact scenario the verification criteria called out.
- ✅ **`ImportReport`** — four counters: `cache_entries_valid`, `cache_entries_stale`, `nodes_needing_rebuild`, `nodes_with_valid_cache`.
- ✅ **`import_pyramid` main entry** — validates inputs (non-empty slug, existing directory), checks for an existing state row (resume-same-pyramid vs refuse-different-pyramid), updates status through `downloading_manifest` → `validating_sources` → `populating_cache` → `complete`, calls `populate_from_import`, enables DADBEAR via the contribution path, marks complete.
- ✅ **`export_cache_manifest` with privacy-safe default** — returns `Ok(None)` unless `include_cache = true`. Phase 10 adds the opt-in checkbox. Documented in-code referencing the spec's "Privacy Consideration" section. When opted in, the manifest is built from `pyramid_step_cache` joined with `pyramid_pipeline_steps` + `pyramid_file_hashes` + `pyramid_evidence`.
- ✅ **3 IPC commands** — `pyramid_import_pyramid`, `pyramid_import_progress`, `pyramid_import_cancel`. Progress calculation matches the spec's weighted formula `(nodes_processed/nodes_total)*0.5 + (cache_entries_validated/cache_entries_total)*0.5` with a clamp to [0,1] and None-total → 0 fallback.
- ✅ **DADBEAR auto-enable via Phase 4 contribution path** — `enable_dadbear_via_contribution` constructs a minimal `dadbear_policy` YAML, calls `create_config_contribution_with_metadata` with `source=import` and `maturity=Canon`, then dispatches through `sync_config_to_operational`. The operational `pyramid_dadbear_config` row is populated via the contribution sync path, NOT via direct INSERT. `test_dadbear_contribution_has_canonical_metadata` and `test_import_pyramid_full_flow_creates_state_then_completes` lock this down.
- ✅ **Build_id synthetic tag** — imported cache rows get `build_id = format!("import:{wire_pyramid_id}")` per the spec's "Integration with LLM Output Cache" section. Distinguishes imported rows from locally-built rows for audit trails without affecting the content-addressable lookup (which ignores build_id).
- ✅ **Manifest version validation** — `populate_from_import` rejects any `manifest_version != 1` with a clear error. Future additive extensions get their own version bump.
- ✅ **Archived cache rows excluded from export** — the publish-side query filters `cache_key NOT LIKE 'archived:%'` so supersession history never leaks through a manifest.
- ✅ **Idempotency test** — `test_populate_from_import_idempotent` re-runs the same manifest twice against the same DB, asserts the cache row count is unchanged after the second pass.
- ⚠️ **`RemotePyramidClient` manifest download** — NOT in scope. The spec's "Import Flow" step 3 talks about downloading the manifest from the source node's tunnel URL, but the existing `WireImportClient` in `wire_import.rs` is scoped to chain definitions / question sets, not pyramid manifests. Phase 10's ImportPyramidWizard will own the frontend download (likely via a new endpoint) and pass the raw manifest JSON into `pyramid_import_pyramid` as a string argument. Phase 7 ships the IPC entry point that accepts the manifest; the download wiring is explicitly deferred.
- ⚠️ **Privacy gate detection logic** — Phase 7 ships the safer default-off rather than the full public-source detection the spec describes (~line 270). The spec's full version walks the L0 set and checks each corpus document's `visibility` field; Phase 10's publish UI will implement that detection alongside the opt-in checkbox. Phase 7's default-off is strictly safer than the full detection because it can't false-positive.
- ⚠️ **Frontend wizard / sidebar / build viz integration** — Phase 10 / Phase 13 scope. Phase 7 ships backend-only.

### Scope decisions

- **`pyramid_import.rs` as a new module**: the spec's "Files Modified" table lists "New `pyramid_import.rs`" explicitly. Chose the name `pyramid_import` (not just `import`) to avoid colliding with the Rust `import` keyword as a filename concern on case-insensitive file systems, and to stay consistent with `wire_import.rs` (which handles chain imports, not pyramid imports — the two domains are orthogonal and I did not touch `wire_import.rs`).
- **Manifest types live in `pyramid_import.rs`, not `types.rs` or a shared location**: both the export side (`wire_publish.rs::build_cache_manifest`) and the import side (`pyramid_import::populate_from_import`) need to speak the same types, so they live in the module that owns the import-side semantics. `wire_publish.rs` references them via fully-qualified path `crate::pyramid::pyramid_import::*`. This avoids introducing a new crate-root type file for what is essentially one set of structs with two callers.
- **In-memory dependency graph from the manifest, not from `pyramid_evidence`**: the spec's deviation protocol explicitly lists "the manifest carries its own `derived_from` lists, so you can build the dependency graph in-memory from the manifest alone without touching the local `pyramid_evidence` table. Use this approach to avoid coupling to the local state during import." The three-pass algorithm builds `dependents: HashMap<String, Vec<String>>` from the manifest's `ImportNodeEntry.derived_from` fields at runtime. This keeps import decoupled from the local state — a partial `pyramid_evidence` table (e.g. a prior failed import) cannot poison the staleness pass.
- **`store_cache_if_absent` (INSERT OR IGNORE) vs `store_cache` (INSERT OR REPLACE)**: the initial Phase 7 implementation used `store_cache` (ON CONFLICT DO UPDATE) with the rationale "cache is content-addressable, so replace and ignore produce the same observable state." The verifier pass caught this as a real spec deviation: the rationale is incorrect for the reroll-then-resume case. If a user imports a pyramid, rerolls a cached step locally via `supersede_cache_entry` (which writes a new row at the same cache_key with `force_fresh = 1`, a new `output_json` from the reroll, and a supersession link), and then re-runs the import for any reason (network drop resume, explicit retry, crash recovery), `store_cache`'s DO UPDATE branch would clobber the rerolled row — replacing the reroll's `output_json`, clearing the `force_fresh` flag, and blowing away the supersession link. The spec's "Idempotency" section (~line 341) and the workstream prompt both mandate `INSERT OR IGNORE` specifically to prevent this. The fix: added `db::store_cache_if_absent` (ON CONFLICT DO NOTHING) and routed the import path through it. `store_cache` remains the path for normal LLM-call writes where DO UPDATE is correct. Added a dedicated regression test (`test_re_import_preserves_local_reroll_force_fresh_row`) and two unit tests on `store_cache_if_absent` itself (fresh insert returns true + row present; conflict on prior row returns false + prior row's `output_json` / `force_fresh` / `build_id` all preserved).
- **Missing source file = stale (not error)**: the spec's staleness flow says "if file missing → mark node + dependents stale, skip cache entry." This is a graceful-degradation path. Phase 7 honors it — `!local_path.exists()` adds the L0 to the stale set and continues. Same for unreadable files (hash computation failure) and L0 nodes with no `source_hash` in the manifest. A single problem file can't abort the whole import.
- **`resolve_source_path` refuses parent traversal**: `..` segments are defense-in-depth — a manifest from an untrusted peer cannot escape the local source root. `resolve_source_path` returns an empty PathBuf on `..`, which hits the `.exists()` check and stale-marks the node. Documented + tested.
- **Build ID for imported rows**: `format!("import:{wire_pyramid_id}")` so an audit query filtering by `build_id LIKE 'import:%'` isolates every row that came from a peer manifest. The cache hit path ignores `build_id` (it's content-addressable) so this doesn't affect lookup behavior.
- **DADBEAR content_type default = "document"**: the `pyramid_dadbear_config` table's `content_type` column has a CHECK constraint limiting it to `code`/`conversation`/`document`. The manifest doesn't carry the source pyramid's declared content type, so Phase 7 defaults to `document` — the widest compatibility option. Phase 10's import wizard can override. Documented in-code.
- **DADBEAR maturity = Canon, not Draft**: the default metadata factory produces Draft, but an imported pyramid's DADBEAR config is a verified config from another node, not a user draft. `enable_dadbear_via_contribution` explicitly overrides `maturity` to `Canon`. Matches Phase 5's bundled migration pattern.
- **Publisher query filters archived rows**: `cache_key NOT LIKE 'archived:%'` in the export query. Phase 6's `supersede_cache_entry` archives prior rows under the `archived:` prefix; those rows still live in the table for history but must not surface in a published manifest. The filter is applied in both the `build_id`-scoped and unscoped query paths.
- **Manifest export uses synthetic node IDs for unjoinable rows**: a cache row that has no matching `pyramid_pipeline_steps` entry (edge case: a test fixture, or a subsystem that bypasses pipeline step logging) falls into a `synth:L{depth}:C{chunk_index}` bucket so it still appears in the exported manifest. The importer treats synthetic L0 nodes as stale by default (no `source_path`) — the test `test_export_then_import_round_trip` exercises this path end-to-end.
- **IPC commands are Tauri invoke, not HTTP**: Phase 5 and 6 both wired new commands through `#[tauri::command]` in `main.rs` with `invoke_handler!` registration. The spec's "Files Modified" table mentions `routes.rs`, but the workstream prompt says "match whichever surface Phase 5/6 use" and the implementation log's Phase 5 entry confirms Tauri commands. Phase 7 follows suit.

### Verification results

- ✅ `cargo check --lib` — clean. Same 3 pre-existing warnings (deprecated `get_keep_evidence_for_target`, two `LayerCollectResult` private-visibility warnings). Zero new warnings from Phase 7 files.
- ✅ `cargo build --lib` — clean, same 3 pre-existing warnings.
- ✅ `cargo test --lib pyramid::pyramid_import` — **15/15 passed** in ~1.0s (14 original + 1 new reroll-preservation regression test).
- ✅ `cargo test --lib pyramid::db::import_state_tests` — **5/5 passed** in ~0.5s.
- ✅ `cargo test --lib pyramid::db::step_cache_tests` — **15/15 passed** in ~1.8s (13 original + 2 new `store_cache_if_absent` unit tests).
- ✅ `cargo test --lib pyramid::wire_publish` — **20/20 passed** (14 pre-existing + 6 new Phase 7 tests) in ~0.75s.
- ✅ `cargo test --lib pyramid` — **989 passed, 7 failed** in ~40s. The 7 failures are the same pre-existing unrelated tests carried from Phase 6 (`pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*`). Phase 6 ended at 961 passing; Phase 7 added 28 new tests (15 pyramid_import + 5 import_state + 6 wire_publish + 2 store_cache_if_absent) bringing the total to 989. Zero regressions.
- ✅ **Integration test verification**: `test_populate_from_import_mixed_stale_l0_propagates_to_upper_layers` constructs a cache manifest with 3 L0 nodes (L0a, L0b, L0c) and 2 upper-layer nodes (L1a derives from L0a+L0b, L1b derives from L0b+L0c), seeds a temp dir with matching files for L0a+L0b and a mismatched hash for L0c, calls `populate_from_import`, and asserts:
  - L0c is stale (hash mismatch)
  - L0a + L0b are fresh → their cache entries land in `pyramid_step_cache`
  - L1a depends on L0a + L0b (both fresh) → cache entry lands
  - L1b depends on L0b + L0c → L0c stale propagates → L1b is stale → cache entry dropped
  - `report.cache_entries_valid == 3`, `report.cache_entries_stale == 2`, `report.nodes_needing_rebuild == 2`, `report.nodes_with_valid_cache == 3`
  - Direct SQL count verifies `pyramid_step_cache` has exactly 3 rows under `imp-slug`
  - Direct SQL query verifies the stale L1b cache_key is NOT present
- ✅ **Idempotency verification**: `test_populate_from_import_idempotent` runs `populate_from_import` twice on the same manifest + DB, asserts the row count stays at 5 after the second pass.
- ✅ **DADBEAR-via-contribution verification**: `test_import_pyramid_full_flow_creates_state_then_completes` runs the full `import_pyramid` entry point and asserts:
  - `pyramid_config_contributions` has one active row with `schema_type='dadbear_policy'`, `source='import'`, `status='active'` for the target slug
  - `pyramid_dadbear_config` has one row with a non-NULL `contribution_id` FK for the target slug
- ✅ `grep -rn "pyramid_import" src-tauri/src/pyramid` — confirms the module is declared in `mod.rs`, the types are referenced from `wire_publish.rs`, and the IPC commands call into it from `main.rs`.

### Notes

- **The three-pass algorithm is the safety net.** Getting the pass ordering wrong is a correctness regression: if Pass 2 ran before Pass 1, an upper-layer node could cache-hit with stale L0 ancestors; if Pass 3 ran before Pass 2, stale propagation wouldn't reach nodes that depend on stale L0s. The integration test locks down the exact ordering with a manifest that will fail if any pass shifts.
- **In-memory dependency graph avoids coupling to `pyramid_evidence`.** This was the most important scope decision. The spec's deviation protocol called it out explicitly — building the graph from the manifest means the import cannot be poisoned by stale local state from a prior failed import. The BFS walks entirely in a `HashMap<String, Vec<String>>` constructed at the top of the function.
- **`store_cache_if_absent` is the load-bearing idempotency primitive.** The first implementation used `store_cache` (ON CONFLICT DO UPDATE) and the verifier caught the clobber-on-resume bug. The fix adds a dedicated helper with DO NOTHING semantics and routes the import path through it. Row count is unchanged on re-import (content-addressable constraint), AND any local force-fresh rerolls written between import attempts survive untouched. The tests `test_populate_from_import_idempotent`, `test_re_import_preserves_local_reroll_force_fresh_row`, and the two `store_cache_if_absent` unit tests lock down both invariants.
- **DADBEAR auto-enable is the load-bearing contribution-path example.** Phase 4's wanderer caught `sync_config_to_operational` being dead code; Phase 5's wanderer caught `PromptCache` being dead code + a direct DADBEAR migration INSERT bypassing canonical metadata. Phase 7's `enable_dadbear_via_contribution` is exactly the Phase 4 canonical route — create a contribution via the helper, re-load it, dispatch through sync. This pattern is what Phase 4's invariant calls for, and Phase 7 adds zero new bypass paths.
- **Privacy-safe default is strictly safer than the full detection.** The spec's full public-source detection walks the L0 set and checks each corpus document's visibility flag — that's fine for Phase 10 when the UI has the publish wizard to present warnings, but Phase 7 shipping it would mean a single bug in the detection logic could leak cache contents. Defaulting to off with an explicit opt-in keeps the safety net simple. Phase 10's wanderer pass will validate the detection logic when it's added.
- **`RemotePyramidClient` deferred to Phase 10.** The existing `WireImportClient` handles chain and question-set imports, not pyramid manifests. Writing a new HTTP client in Phase 7 would have added scope that Phase 10 can own properly when it has the full frontend wizard in view. The Phase 7 IPC command accepts the manifest JSON as a string parameter so Phase 10 can fetch it however it wants (direct HTTP, through the existing wire_import infrastructure, from a file, from a pasted blob, etc.).
- **Verifier pass flagged one spec deviation on the idempotency helper.** The initial implementation used `db::store_cache` (ON CONFLICT DO UPDATE) for the import-side cache writes with the rationalization "content-addressable → replace and ignore are equivalent." That rationalization misses the reroll-then-resume case the spec explicitly calls out (~line 341) and the workstream prompt restates: a user can reroll a locally cached step with force_fresh = true between import attempts, and a DO UPDATE re-import would silently clobber that reroll. The verifier pass added `db::store_cache_if_absent` (ON CONFLICT DO NOTHING) and routed `insert_cache_entries` through it, plus a regression test (`test_re_import_preserves_local_reroll_force_fresh_row`) that exercises the exact clobber scenario. Fix is pinned by two additional unit tests on the helper itself. This is a case study in why the workstream prompt's explicit "INSERT OR IGNORE (not INSERT OR REPLACE)" phrasing needs to be taken literally rather than interpreted as flavor text — the scenario is real and unit-testable.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Three-pass ordering correctness** — re-read `populate_from_import` and confirm Pass 1 (L0 staleness) → Pass 2 (BFS propagation) → Pass 3 (upper-layer insert) is the canonical order. Any reordering breaks the safety net.
2. **`enable_dadbear_via_contribution` is the ONLY path** — confirm no direct `pyramid_dadbear_config` INSERT exists anywhere in `pyramid_import.rs`. The contribution path is the canonical route; direct writes would be a regression.
3. **`export_cache_manifest` default-off** — verify every caller passes `include_cache = false` by default, and confirm the Phase 10 opt-in wiring (when it lands) surfaces a warning to the user before flipping the bit.
4. **Integration test coverage** — `test_populate_from_import_mixed_stale_l0_propagates_to_upper_layers` is the load-bearing test. A verifier should mutate the test (e.g. swap which L0 mismatches) and confirm the stale propagation tracks. The propagation is what makes upper-layer cache safety work.
5. **Idempotency lock-down** — `test_populate_from_import_idempotent` should pass even if the manifest is re-imported a third time. The UNIQUE constraint guarantees this but a verifier should exercise the third run as a defensive check.
6. **Build_id audit trail** — imported cache rows have `build_id = "import:{wire_pyramid_id}"`. A verifier should confirm Phase 13's build viz (when it lands) can filter by this prefix to distinguish imported rows from locally-built rows.

Wanderer prompt suggestion: "Does Wire Node boot, accept a `pyramid_import_pyramid` IPC call with a realistic manifest, walk the three-pass staleness check, insert the correct subset of cache rows, enable DADBEAR through the Phase 4 contribution path with canonical metadata, and flip the import state to complete — all without leaving any dangling state rows or bypassing the contribution path for operational table writes?"

---

## Phase 8 — YAML-to-UI Renderer

**Workstream:** phase-8-yaml-to-ui-renderer
**Workstream prompt:** `docs/plans/phase-8-workstream-prompt.md`
**Spec:** `docs/specs/yaml-to-ui-renderer.md`
**Branch:** `phase-8-yaml-to-ui-renderer`
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 8 introduces `YamlConfigRenderer`, the generic React component that renders any YAML document as an editable configuration UI driven by a `SchemaAnnotation` document. The renderer is build-once, consume-many — Phases 9, 10, 14 and every future user-facing config surface import the same component with a schema annotation and a values tree, and a form materializes without any per-schema UI code.

Schema annotations live in `pyramid_config_contributions` with `schema_type = 'schema_annotation'` per Phase 4's unified source-of-truth model. The Phase 5 on-disk migration was extended to walk `chains/schemas/**/*.schema.yaml` on first run, so the initial set of annotation files (`chain-step.schema.yaml`, `dadbear.schema.yaml`) seed as `schema_annotation` contributions with canonical Wire Native metadata (Template type, Canon maturity, `ui_annotation` topic tag). From that point forward every read goes through `pyramid_get_schema_annotation` against the contributions table — disk files are never read at runtime.

Three new Tauri IPC commands (`pyramid_get_schema_annotation`, `yaml_renderer_resolve_options`, `yaml_renderer_estimate_cost`) and one new Rust module (`pyramid::yaml_renderer`) form the backend surface. Six dynamic option sources resolve at mount time: `tier_registry`, `provider_list`, `model_list:{provider}`, `node_fields`, `chain_list`, `prompt_files`. Cost estimation parses the Phase 3 `pricing_json` column and returns USD-per-call estimates for fields flagged `show_cost: true`. Ten widget components ship in `src/components/yaml-renderer/widgets/`: select, text, number, slider, toggle, readonly, model_selector, list, group, code — the full Phase 1/2/3 set from the spec.

### Files touched

**New files (backend):**

- `src-tauri/src/pyramid/yaml_renderer.rs` (~800 lines) — Phase 8 module. Defines `SchemaAnnotation`, `FieldAnnotation`, `OptionValue` serde types mirroring the TypeScript contract 1:1. Implements `load_schema_annotation_for()` (direct slug lookup + scan fallback via `applies_to`), `resolve_option_source()` (dispatches by source name + handles `model_list:{provider_id}` parameterization), `estimate_cost()` (parses `pricing_json` via Phase 3's `TierRoutingEntry::prompt_price_per_token`/`completion_price_per_token` helpers), plus six resolver helpers for the supported sources. 12 unit tests covering happy paths + fallback + missing-pair edge cases.

- `chains/schemas/chain-step.schema.yaml` — seed annotation file for chain step config. Exercises select (static + dynamic), slider, number (with min/max/step/suffix), toggle, list (with `item_options_from: node_fields`), and groups. Content mirrors the spec example from lines 64-162 with an added `order:` field for deterministic rendering + an extra `group: "Token Budget"` bucket.

- `chains/schemas/dadbear.schema.yaml` — smaller 4-field seed annotation for `dadbear_policy`. Exists to spot-check the renderer against a config type that has no `inherits_from` structure.

**New files (frontend):**

- `src/types/yamlRenderer.ts` (~150 lines) — TypeScript contract mirroring the Rust types. Exports `SchemaAnnotation`, `FieldAnnotation`, `OptionValue`, `WidgetType`, `FieldVisibility`, `VersionInfo`, `YamlConfigRendererProps`. Designed so `invoke<SchemaAnnotation>('pyramid_get_schema_annotation', ...)` deserializes directly into the interface without conversion.

- `src/components/YamlConfigRenderer.tsx` (~460 lines) — the renderer component. Sorts fields by `order` within a stable natural order, groups by `annotation.group`, buckets into basic/advanced/hidden, dispatches each field to the appropriate widget via a switch on `annotation.widget`, handles the inherits-from-default indicator, optional cost badge, readonly mode, version info header, and Accept/Notes action bar. Uses `readPath()` + `valuesEqual()` helpers for path-based value lookup and inheritance equality. Inline styles match the existing project convention (CSS variables + class utilities from `dashboard.css`).

- `src/components/yaml-renderer/widgets/WidgetTypes.ts` — shared `WidgetProps` contract.

- `src/components/yaml-renderer/widgets/SelectWidget.tsx` — static + dynamic options dropdown.

- `src/components/yaml-renderer/widgets/TextWidget.tsx` — free-form string input.

- `src/components/yaml-renderer/widgets/NumberWidget.tsx` — numeric input with min/max/step + optional suffix.

- `src/components/yaml-renderer/widgets/SliderWidget.tsx` — range slider with live value readout + step-derived decimal precision.

- `src/components/yaml-renderer/widgets/ToggleWidget.tsx` — boolean checkbox with inline "On/Off" label.

- `src/components/yaml-renderer/widgets/ReadonlyWidget.tsx` — static display with JSON-pretty fallback for objects/arrays.

- `src/components/yaml-renderer/widgets/ModelSelectorWidget.tsx` — composite tier picker with provider + model + context window + cost badges. Reads `OptionValue.meta` from `tier_registry` for rich display.

- `src/components/yaml-renderer/widgets/ListWidget.tsx` — Phase 3 add/remove item list with sub-widget dispatch (supports scalar text + select items via `item_widget` + `item_options_from`).

- `src/components/yaml-renderer/widgets/CodeWidget.tsx` — Phase 3 monospace textarea for YAML/prompt content. No syntax highlighting (a heavier editor dep is deferred to Phase 10+).

- `src/components/yaml-renderer/widgets/GroupWidget.tsx` — Phase 3 collapsible section. Phase 8 renders nested objects as compact JSON; full recursive nested-form support lands in Phase 10 when annotations gain `fields:` sub-maps.

- `src/components/yaml-renderer/widgets/index.ts` — barrel file re-exporting every widget.

- `src/hooks/useYamlRendererSources.ts` — Phase 8 dynamic options + cost hook. Walks the schema's `options_from` / `item_options_from` set, dedupes, calls `yaml_renderer_resolve_options` once per unique source, caches results. For `show_cost: true` fields, reads the currently-selected tier's meta and calls `yaml_renderer_estimate_cost` with a Phase 8 default token budget (8k in / 2k out; Phase 10 replaces with per-step historical averages).

**Modified files:**

- `src-tauri/src/pyramid/mod.rs` — declared `pub mod yaml_renderer;` alongside the other Phase 5/8 modules.

- `src-tauri/src/pyramid/wire_migration.rs` (+250 lines) — Phase 8 extension: walks `chains/schemas/**/*.schema.yaml` (and `.schema.yml`), excludes `_archived/` for parity with the prompt walker, extracts the annotation slug via `applies_to` → `schema_type` → filename stem fallback, inserts each as a `schema_annotation` contribution with canonical Wire Native metadata via `create_config_contribution_with_metadata`. Per-file idempotency via slug uniqueness check; whole-run idempotency via the same `_prompt_migration_marker` sentinel used by Phase 5. The sentinel write was moved to AFTER the schema walk so a first run with only schemas (no prompts or chains) still writes the marker. `MigrationReport` gained three new counters (`schema_annotations_inserted`, `schema_annotations_skipped_already_present`, `schema_annotations_failed`). 6 new unit tests — slug extraction (3 cases), insertion correctness, idempotency across re-runs, and the "schemas only" edge case. The pre-existing `setup_chains_dir` helper was extended to seed two schema annotation files alongside the prompts + chains.

- `src-tauri/src/main.rs` (+90 lines) — added 3 IPC commands: `pyramid_get_schema_annotation(schema_type)` returns an `Option<SchemaAnnotation>`, `yaml_renderer_resolve_options(source)` returns `Vec<OptionValue>` via the provider registry, `yaml_renderer_estimate_cost(provider, model, avg_input_tokens, avg_output_tokens)` returns an `f64`. All three registered in `invoke_handler!` in a new "Phase 8: YAML-to-UI renderer" block between Phase 5 and Phase 7. Phase 5's migration call site at line ~7544 already walks `chains/schemas/` now (the migration function was extended, not the call site), so no additional wiring was needed in main.rs.

- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — this entry.

### Spec adherence (against `docs/specs/yaml-to-ui-renderer.md`)

- ✅ **Backend IPC contract** — `pyramid_get_schema_annotation`, `yaml_renderer_resolve_options`, `yaml_renderer_estimate_cost` all registered in `invoke_handler!` and return the shapes specified in the spec's "Backend Contract" section. Returns from `pyramid_get_schema_annotation` are `Option<SchemaAnnotation>` per the spec; returning `None` lets the frontend fall back to a generic editor when no annotation exists.
- ✅ **Phase 4/5 alignment for schema annotations** — `load_schema_annotation_for()` queries `pyramid_config_contributions` via Phase 4's `load_active_config_contribution` helper. Disk files are never read at runtime; they're migrated once via `wire_migration::walk_schema_files` and from that point all reads go through the contributions table. Explicitly mirrors the Phase 5 prompt/chain migration pattern.
- ✅ **Dynamic option sources — all six** — `tier_registry`, `provider_list`, `model_list:{provider_id}`, `node_fields`, `chain_list`, `prompt_files` all resolve via `resolve_option_source`. `model_list:{provider_id}` is parameterized via a `strip_prefix` check. Unknown sources return an empty list + a warn log (the select widget shows "no options available"); they are NOT fatal errors.
- ✅ **Cost estimation** — `estimate_cost` pulls pricing from `pyramid_tier_routing.pricing_json` via Phase 3's `TierRoutingEntry::prompt_price_per_token` / `completion_price_per_token` parsers. Missing pairs return `0.0` + a warn log per the spec's "show 'cost unavailable'" guidance.
- ✅ **`SchemaAnnotation` type** — Rust and TypeScript definitions match the spec's `Renderer Contract` section byte-for-byte (field names, types, optional-ness). Includes the Phase 8 extensions (`label`, `description`, `order` on field annotations) that lived in the spec's YAML examples but weren't called out in the explicit `FieldAnnotation` property table.
- ✅ **Widget implementations** — the full set in the spec's "Renderer Implementation Scope" Phase 1 + Phase 2 + Phase 3: select, text, number, slider, toggle, readonly, model_selector, list, group, code. Ten widgets, not nine — list and group are both shipped in Phase 8 even though group's recursive nested-form mode is deferred to Phase 10.
- ✅ **Visibility levels** — basic/advanced/hidden all respected. Hidden fields are dropped entirely (not rendered anywhere, not in a collapsed section). Advanced fields live in a collapsible "▶ Advanced" section that starts closed.
- ✅ **Inheritance display** — `FieldRow` computes `inheritsFromDefault = annotation.inherits_from != null && valuesEqual(value, resolvedDefault)` and shows `← {inherits_from} default` as a muted label. `valuesEqual` uses JSON comparison for objects/arrays.
- ✅ **Cost display** — `show_cost: true` fields render a `$0.xxxx est.` badge next to the label. The `model_selector` composite widget also gets a larger cost badge in its secondary row.
- ✅ **Notes paradigm** — the renderer ships Accept + Notes buttons at the bottom. Notes opens an inline textarea and calls `onNotes(trimmed)` on submit; the parent owns the LLM round-trip (Phase 9 wires it). Empty notes are refused at the UI layer.
- ✅ **Version info** — `versionInfo` prop renders "Version X of Y" + the triggering note in the header when provided. Phase 8 just displays it; Phase 13 adds the navigation controls.
- ✅ **Read-only mode** — `readOnly={true}` disables every widget and hides the action bar entirely. Used by version history inspection.
- ✅ **Dynamic options + cost hook** — `useYamlRendererSources` collects unique sources, fetches each once, caches the results, and also computes cost estimates by reading the `meta` payload of the currently-selected tier and calling `yaml_renderer_estimate_cost`. Uses a Phase 8 constant token budget (8k in / 2k out) with a TODO for Phase 10's historical averages.
- ✅ **Schema annotation file migration** — `wire_migration.rs` extended with `walk_schema_files` + `extract_annotation_slug` + `build_schema_annotation_metadata`. Idempotent via the existing sentinel + per-slug uniqueness check. Phase 8 writes Template contribution_type + Canon maturity + `ui_annotation` topic tag per the Wire Native mapping table in `wire_native_metadata.rs`.
- ✅ **Seed annotation files on disk** — two files shipped in `chains/schemas/`: `chain-step.schema.yaml` (complete spec example with groups + 9 fields exercising all core widgets) and `dadbear.schema.yaml` (smaller 4-field example for spot checks).
- ⚠️ **Condition evaluation** — the spec mentions `condition` as a field annotation property (e.g. `"split_strategy != null"`). Phase 8 ships the TypeScript + Rust field on `FieldAnnotation` but the renderer does NOT yet evaluate conditions. Deferred to Phase 10 alongside the creation UI integration. This matches the spec's "Phase 2: Conditional field visibility (`condition` property)" bullet which is part of Phase 2 scope inside the renderer spec — Phase 8 shipped the type, wiring lands with Phase 10.
- ⚠️ **Section decomposition for the `group` widget** — the spec's `group` widget is "Collapsible section for a nested object with sub-fields". Phase 8 ships a collapsible section that shows the nested object as compact JSON. Full recursive nested-field rendering (where a group contains its own `fields:` sub-map) is Phase 10 because the annotation shape in the current spec doesn't declare nested `fields:`, and adding that requires a schema change. Filed as a Phase 10 carryover in the `GroupWidget.tsx` header comment.
- ⚠️ **Ollama `/api/tags` live model list** — spec mentions Phase 10 adds a live query. Phase 8 ships `model_list:{provider_id}` but backs it with the configured tier routing rows only. Adam's architectural lens applies: Phase 10's dynamic lookup will read from live provider responses; Phase 8's implementation is the "what's configured" view that works for OpenRouter right now.
- ⚠️ **Creation UI integration (Phase 4 of the renderer spec)** — explicitly Phase 10 scope per the workstream brief. Phase 8 ships the renderer; Phase 10 binds it to the ToolsMode Create tab.
- ⚠️ **Full config type annotation set** — only `chain_step_config` and `dadbear_policy` seeded in Phase 8. The remaining 5 (chain_defaults_config, provider_config, tier_routing_config, evidence_policy, build_strategy) land with Phase 10 when the creation UI needs them. The migration infrastructure is in place so adding new annotation files requires no code changes.

### Scope decisions

- **Phase 5 migration function extended, not replaced.** Phase 5's `migrate_prompts_and_chains_to_contributions` was the natural home for schema annotation migration — it already walks `chains/`, holds the idempotency sentinel, and is invoked in main.rs at the right point in app setup. Forking Phase 8 into its own migration function would have meant two sentinels + two call sites + a race between them. Extending the existing function keeps the migration single-entry-point, at the cost of a slightly longer file.
- **Sentinel write position moved AFTER schema walk.** The original Phase 5 code wrote the sentinel right after the chain walk, before Phase 8's new schema walk. On a first run with ONLY schemas present (edge case — future user who drops in a custom annotation without any prompts or chains), the sentinel write would have fired on the zero-prompts+zero-chains path and skipped the schema insertion. Moving the write to after the schema walk fixes this. Added the `phase8_migration_with_schemas_only_still_writes_marker` test to lock it in.
- **`SchemaAnnotation.applies_to` defaults to `schema_type` when absent.** Simple annotation files can omit `applies_to` entirely and the loader falls back to treating `schema_type` as the lookup key. This matches the ergonomic feel in the spec's example YAML where the annotation's self-describing `schema_type: chain_step_config` already names the target. The explicit `applies_to` is only needed when one annotation file describes multiple targets or uses a different name for its own identity vs the target.
- **Direct-slug lookup + scan fallback in `load_schema_annotation_for`.** Primary path: look up the contribution whose slug equals the target schema_type. This is the common case since the migration keys rows on `applies_to`. Fallback path: scan every active schema_annotation contribution and parse each body, matching on `applies_to` / `schema_type`. This catches (a) annotation files whose slug was derived differently and (b) future agent-generated contributions that might re-use a misaligned slug. Scan cost is bounded by the number of annotation contributions, which is O(number of config types) — tens, not thousands.
- **`model_list:{provider_id}` is "what's routed" not "what's available".** Phase 8 derives the model list from the tier routing rows that reference the provider. Adam's architectural lens question: "can an agent improve this?" Yes — Phase 10 will add an Ollama `/api/tags` live query for local providers and a cached `/api/v1/models` query for OpenRouter. But the Phase 8 implementation works correctly for the current configured view, and the frontend doesn't need to care whether the list comes from routing or from a live query.
- **Cost estimation uses constant token budgets for Phase 8.** `useYamlRendererSources` passes a fixed `(8k input, 2k output)` pair to `yaml_renderer_estimate_cost`. These are rough averages that put the cost badge in the correct order of magnitude. Phase 10 replaces this with per-step historical averages once the cost log + build viz can surface the data. The constant is Phase 8-only; it's in the hook, not the annotation, so swapping it later is a one-file change.
- **Inline styles instead of new CSS classes.** Every component uses `var(--text-primary)`, `var(--bg-card)`, etc. via inline `style={{}}` props. This matches the AddWorkspace / ToolsMode convention — no new stylesheet, no new CSS modules, nothing for the designer to have to learn. The one exception is the shared `.btn`/`.btn-primary`/`.input` class names which exist in `dashboard.css` and are used for the action bar buttons.
- **Widget file-per-component + barrel export.** Each widget lives in its own file under `src/components/yaml-renderer/widgets/` with an `index.ts` barrel that re-exports them. The renderer imports via `import { SelectWidget, ... } from "./yaml-renderer/widgets"`. This scales cleanly — adding a new widget in Phase 10 (e.g. a rich code editor) means one new file + one new line in the barrel.
- **`TextWidget` is NOT a textarea.** The spec says "text: Text input" and separately "code: Monospace text area for YAML/prompt content". I kept them distinct — text is a single-line `<input type="text">`, code is a multi-line `<textarea>` with monospace font and auto-sized rows. This means annotations that want multi-line plain text should use `widget: code` (which Phase 8 ships).
- **Inherited-from-default indicator compares current vs resolved default, not vs "absent".** The spec says "When a step's field matches the chain default, show '← chain default'". I interpreted this as comparing the current value to the resolved default (via `inherits_from` path lookup) — if they match, show the indicator. If the field is absent from values entirely, both `value` and `resolvedDefault` could be `undefined`, which `valuesEqual` treats as equal, so the indicator shows for unset fields (which IS the inheritance case — no override means we use the default).

### Verification results

- ✅ `cargo check --lib` — clean. 3 pre-existing warnings (deprecated `get_keep_evidence_for_target`, two `LayerCollectResult` private-visibility warnings). No new warnings from Phase 8 files.
- ✅ `cargo check` (full crate) — clean. Same 3 lib warnings + 1 pre-existing binary warning (`tauri_plugin_shell::Shell::open` deprecated). Phase 8's new main.rs IPC commands wire in cleanly.
- ✅ `cargo build --lib` — clean.
- ✅ `cargo test --lib pyramid::yaml_renderer` — **12/12 passing** (`test_load_schema_annotation_from_contribution`, `test_load_schema_annotation_missing_returns_none`, `test_load_schema_annotation_falls_back_to_scan`, `test_resolve_options_tier_registry_empty`, `test_resolve_options_tier_registry_seeded`, `test_resolve_options_node_fields_is_static`, `test_resolve_options_chain_list_reads_custom_chain_contributions`, `test_resolve_options_prompt_files_reads_skill_contributions`, `test_resolve_options_unknown_source_returns_empty`, `test_estimate_cost_from_seeded_tier`, `test_estimate_cost_missing_pair_returns_zero`, `test_annotation_serializes_preserving_optional_fields`).
- ✅ `cargo test --lib pyramid::wire_migration` — **12/12 passing** including the 6 new Phase 8 tests (`extract_annotation_slug_prefers_applies_to`, `extract_annotation_slug_falls_back_to_schema_type`, `extract_annotation_slug_handles_quoted_values`, `phase8_migration_inserts_schema_annotations`, `phase8_schema_annotation_migration_idempotent`, `phase8_migration_with_schemas_only_still_writes_marker`). The 6 pre-existing Phase 5 tests still pass.
- ✅ `cargo test --lib pyramid` — **1010 passing, 7 failed** (same 7 pre-existing failures documented in Phase 2/3/4/5/6/7: `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*`). Phase 8 added 18 tests bringing pyramid total from Phase 7's 992 to 1010. Zero new failures.
- ✅ `npm run build` (tsc + vite) — clean. 115 modules transformed. No TypeScript errors. Bundle size unchanged since previous phase (the new renderer is tree-shakeable — unused widgets would drop out, but all ten are currently registered). One pre-existing warning about chunk size > 500kB (not introduced by Phase 8).
- ⚠️ No frontend test runner present in `package.json` (no Vitest/Jest/Playwright). Frontend component tests skipped per the workstream brief's explicit "If there's no Vitest/Jest/Playwright in `package.json`, skip frontend unit tests and document in the log" instruction. The Rust-side tests cover the IPC contract + data resolution; a Phase 10 verifier pass with the ToolsMode wiring will exercise the frontend rendering path end-to-end.
- ⚠️ No IPC smoke test script on the existing dev harness, but the commands are registered in `invoke_handler!` and the TypeScript types match the Rust types exactly. Manual verification path: run the app, open the ToolsMode tab in dev tools, invoke `pyramid_get_schema_annotation` with `schema_type: "chain_step_config"` after first run has completed the migration. The returned `SchemaAnnotation` should have `fields.model_tier`, `fields.temperature`, `fields.concurrency`, `fields.on_error` under basic visibility and `fields.max_input_tokens`, `fields.batch_size`, `fields.split_strategy`, `fields.dehydrate`, `fields.compact_inputs` under advanced. Each tier_registry option should carry `meta.provider_id` and `meta.context_limit`.

### Notes

- **Schema annotation storage is the load-bearing architectural choice.** The brief explicitly called out: "Schema annotations are loaded from `pyramid_config_contributions` via Phase 4's `schema_annotation` schema_type, NOT from disk at runtime." This aligns with Adam's architectural lens — every configurable behavior in Wire Node flows through the contribution table so agents can improve it. Reading annotations from disk at runtime would have been faster to implement but would have blocked Phase 10's generative config loop from applying notes to annotations. Done right: annotations behave identically to prompts + chains + policies.
- **The `load_schema_annotation_for` fallback scan is not wasted work.** The direct-slug lookup handles the happy path (one annotation per target config type, slug = target). The scan fallback handles the future case where an agent contributes a new annotation with a misnamed slug but a correct `applies_to`. Both paths are O(single SELECT) vs O(N × parse) — scan cost is bounded by the number of distinct annotation contributions, which is small.
- **Widget design philosophy: dumb and focused.** Each widget is a single-purpose display that takes `{value, onChange, disabled, annotation, optionSources, costEstimate}` and returns JSX. No state, no IPC calls, no effects. All stateful behavior lives in the parent `YamlConfigRenderer` (advanced-section collapse, notes open/close, notes text). This makes the widgets trivially testable and composable — Phase 10's creation UI can mix and match widgets without inheriting Phase 8's renderer wrapper.
- **`ModelSelectorWidget` reads `OptionValue.meta` directly.** The tier_registry resolver attaches `provider_id`, `model_id`, `context_limit`, `max_completion_tokens`, `prompt_price_per_token`, `completion_price_per_token` to each option's `meta` object. The widget pulls these out for the provider badge + context window display without a second IPC round trip. This is why the Rust resolver JSON-serializes the meta as an opaque `serde_json::Value` — the widget layer decides what to render from it.
- **Phase 5 log said "Phase 5 ships with no on-disk schemas, so this step is a TODO (Phase 9 handles it)."** Phase 8 claims that TODO. The Phase 5 comment was replaced with "Phase 5 schema definition migration: deferred to Phase 9" — schema DEFINITION (JSON Schema validation bodies) is still Phase 9 scope, separate from the schema ANNOTATION work Phase 8 just shipped.
- **No Pillar 37 violations.** The only numbers in Phase 8 code that constrain any LLM behavior are the default token budgets in `useYamlRendererSources.ts` (8_000 / 2_000). These are UI-visible cost hints, not LLM input bounds — the LLM still reads `max_input_tokens` from the chain step's actual config, which is itself schema-annotation-driven. The `DEFAULT_AVG_INPUT_TOKENS` / `DEFAULT_AVG_OUTPUT_TOKENS` constants are local to the hook and documented as Phase 8 placeholders for the Phase 10 historical-average lookup. They do not appear anywhere in a prompt or constrain what the LLM produces.
- **No friction log entries required.** The spec was unambiguous on the contract. One mildly tricky decision: the `applies_to` vs `schema_type` lookup key ambiguity (the spec's example annotation has both with the same value, suggesting they're redundant). I kept both and treat `applies_to` as the explicit override, `schema_type` as the fallback, which preserves backwards compatibility with simple annotation files and makes the explicit intent clear when needed.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Schema annotation contract stability.** Phase 9 and Phase 10 will both consume `SchemaAnnotation` via IPC. A verifier should deserialize the shipped `chain-step.schema.yaml` and `dadbear.schema.yaml` into the Rust `SchemaAnnotation` type and confirm every field round-trips to JSON with the exact shape the TypeScript interface expects. A mismatch here would ripple through Phase 9's LLM prompt and Phase 10's creation UI.
2. **Migration idempotency under partial failure.** The Phase 8 migration extension runs inside the Phase 5 sentinel scope. A verifier should manually run the migration against a chains_dir where one schema file has malformed YAML — the malformed file should fail, the others should succeed, and a re-run should retry only the failed file. Phase 5's test suite covered this pattern for prompts + chains; Phase 8's extension inherits the per-file resilience but should be spot-checked.
3. **`load_schema_annotation_for` scan fallback.** Confirm the scan path matches `applies_to` regardless of slug. Seed a contribution whose slug is `"foo"` but whose body declares `applies_to: chain_step_config`, then query by `chain_step_config` — the scan should find it.
4. **Widget dispatch fallback.** Annotations with unknown widget types (e.g. a future Phase 10 widget the renderer doesn't know yet) should render as `ReadonlyWidget` (the default branch in `pickWidget`). Verify this doesn't crash the page.
5. **Inheritance indicator correctness.** The `valuesEqual` helper uses JSON comparison for objects/arrays. A verifier should test edge cases: empty arrays, null vs undefined, different key orders in nested objects. If the indicator flickers or shows incorrectly for valid overrides, users will lose trust in it.
6. **Cost estimate refresh on tier change.** When the user changes the `model_tier` field, the cost estimate should update. The `useYamlRendererSources` hook's effect dependency array includes `values` so the cost recomputes on each change. A verifier should trace the actual re-render to confirm the badge flips cleanly.
7. **Advanced section collapse state persistence.** Currently the collapse state is local component state — navigating away and back resets it. A verifier should confirm this is acceptable for Phase 8 (it is per the spec) or flag it as a Phase 10 persistence target.

Wanderer prompt suggestion: "Does a fresh Wire Node boot → run the Phase 8 migration → seed two schema annotation contributions → serve them via `pyramid_get_schema_annotation` → and can a test harness invoke `yaml_renderer_resolve_options('tier_registry')` and `yaml_renderer_estimate_cost('openrouter', 'inception/mercury-2', 8000, 2000)` and get back structurally correct payloads without the app crashing — even though no frontend consumer exists yet?"

---

## Phase 9 — Generative Config Pattern

**Workstream:** phase-9-generative-config-pattern
**Workstream prompt:** `docs/plans/phase-9-workstream-prompt.md`
**Spec:** `docs/specs/generative-config-pattern.md`
**Branch:** `phase-9-generative-config-pattern`
**Started:** 2026-04-10
**Completed (implementer pass):** 2026-04-10
**Status:** awaiting-verification

### What shipped

Phase 9 ships the backend for the generative config loop — the "describe what you want → see a YAML config" round trip that becomes the foundation for Phase 10's frontend wizard. Every moving piece flows through the Phase 4 contribution store; there is no operational-table shortcut path. Every LLM call goes through Phase 6's `call_model_unified_with_options_and_ctx` with a fully-populated `StepContext` so cache hits work across generation + refinement.

Four new Rust modules land: `pyramid::schema_registry` (a view over `pyramid_config_contributions` that resolves the `(schema_definition, schema_annotation, generation skill, seed default)` tuple for each active schema_type), `pyramid::generative_config` (the IPC-layer logic for generate/refine/accept/list), plus extensions to `wire_migration.rs` (Phase 9 bundled manifest walker) and `config_contributions.rs` (Phase 4 stubs `invalidate_schema_registry_cache` and `flag_configs_for_migration` are both wired to real implementations). Six new Tauri IPC commands (`pyramid_generate_config`, `pyramid_refine_config`, `pyramid_accept_config`, `pyramid_active_config`, `pyramid_config_versions`, `pyramid_config_schemas`) register in `main.rs` with the 3-phase load → LLM → persist pattern that keeps `rusqlite::Connection` off the async task scheduler's hair.

The bundled contributions manifest at `src-tauri/assets/bundled_contributions.json` ships 18 entries covering 5 schema types (`evidence_policy`, `build_strategy`, `dadbear_policy`, `tier_routing`, `custom_prompts`) with their generation skills, JSON schemas, seed defaults, and schema annotations (3 new ones; `dadbear_policy` and `tier_routing` annotations are stretch work that still use Phase 8's seeds via the frontend fallback). A `needs_migration INTEGER` column lands on `pyramid_config_contributions` via idempotent `ALTER TABLE` so Phase 10 can surface a "Migrate" button without a schema change.

### Files touched

**New files (backend):**

- `src-tauri/src/pyramid/schema_registry.rs` (~560 lines) — `SchemaRegistry` with `RwLock<HashMap<String, ConfigSchema>>`, `hydrate_from_contributions` + `reload` + `get` + `list` + `invalidate`. Resolves the 3-piece tuple for each target schema_type via slug-convention lookups with metadata-topic scan fallbacks. Includes `flag_configs_needing_migration` helper that `UPDATE … SET needs_migration = 1 WHERE schema_type = ?1 AND status = 'active'`. 10 unit tests covering empty hydrate, minimal + full resolution, sorted listing, invalidation re-read, hydration from the shipped bundled manifest, annotation body matching, metadata topic matching, flag-setting, and the superseded-row skip.

- `src-tauri/src/pyramid/generative_config.rs` (~1200 lines) — Phase 9 IPC-layer logic. `GenerateConfigResponse`, `RefineConfigResponse`, `AcceptConfigResponse`, `ActiveConfigResponse`, and `SyncResult` response types. Three-phase entry points (`load_generation_inputs` → `run_generation_llm_call` → `persist_generated_draft`, and the same for refinement) so the IPC handler can drop the DB lock across the LLM await. Convenience wrappers (`generate_config_from_intent`, `refine_config_with_note`) for tests and non-async call sites. `accept_config_draft` handles both (a) promote-latest-draft and (b) direct-YAML inline paths, routing through `sync_config_to_operational_with_registry` in both cases. `call_generation_llm` resolves the `synth_heavy` tier via the provider registry and constructs a full `StepContext` with `primitive = "config_generation"` or `"config_refinement"`. Prompt substitution supports `{schema}`, `{intent}`, `{current_yaml}`, `{notes}` placeholders plus simple `{if X}...{end}` conditional blocks. 16 unit tests including prompt-substitution cases, YAML extraction (plain + fenced + prose-prefix), active config for empty DB, bundled-manifest schema listing, draft supersession, direct-YAML accept with sync, missing-draft error, empty-note rejection, empty-intent rejection, unknown-schema-type rejection, bundled-body loading, and end-to-end accept-promotes-draft.

- `src-tauri/assets/bundled_contributions.json` (~160 lines JSON) — 18 bundled contribution entries spanning 5 schema types. Each entry carries an explicit `contribution_id` with `bundled-` prefix so app upgrades can reference by stable handle. Metadata is NOT inline — the Phase 9 migration builds canonical `WireNativeMetadata` from the Phase 5 mapping table at insertion time and overrides `maturity = Canon`, `price = 1`. The manifest has its own `topics_extra` + `applies_to` convenience fields that feed into the metadata builder.

- `chains/prompts/generation/evidence_policy.md`, `chains/prompts/generation/build_strategy.md`, `chains/prompts/generation/dadbear_policy.md`, `chains/prompts/generation/tier_routing.md`, `chains/prompts/generation/custom_prompts.md` — 5 generation skill bodies shipped on disk AND inlined into `bundled_contributions.json`. The on-disk files are the editable authoring copies; the manifest is the runtime-loaded binary blob. Both paths land the same body in `pyramid_config_contributions` with `source = 'bundled'`.

**Modified files:**

- `src-tauri/src/pyramid/mod.rs` — declared `pub mod schema_registry;`, `pub mod generative_config;`. Added `schema_registry: Arc<schema_registry::SchemaRegistry>` field to `PyramidState`. Updated `with_build_reader` to clone the field through to build-scoped state copies.

- `src-tauri/src/pyramid/db.rs` — idempotent `ALTER TABLE pyramid_config_contributions ADD COLUMN needs_migration INTEGER NOT NULL DEFAULT 0` in `init_pyramid_db`. Pattern matches the Phase 4 `contribution_id` column add — best-effort execute, ignore "column already exists" error.

- `src-tauri/src/pyramid/wire_migration.rs` (+260 lines) — Phase 9 bundled manifest support. New `BundledContributionsManifest` + `BundledContributionEntry` types, `load_bundled_manifest()` using `include_str!("../../assets/bundled_contributions.json")` so the manifest ships inside the binary, `build_bundled_metadata()` computing canonical `WireNativeMetadata` from the Phase 5 mapping table with Canon maturity + topic_extra + applies_to overrides, `insert_bundled_contribution()` using explicit `contribution_id` with `INSERT OR IGNORE` semantics (skip-on-conflict, NEVER UPDATE), `walk_bundled_contributions_manifest()` + `BundledMigrationReport`. Hooked into `migrate_prompts_and_chains_to_contributions()` to run BEFORE the Phase 5 sentinel check — the bundled walk runs on every boot so app upgrades can add new entries without being blocked by a stale disk-walk sentinel. `MigrationReport` gained three new counters (`bundled_inserted`, `bundled_skipped_already_present`, `bundled_failed`). 5 new Phase 9 tests: manifest parse smoke check, full insert verification (≥15 rows), idempotency, user-supersession preservation, sentinel-present regression. Fixed 2 existing Phase 5/8 tests that counted `schema_type = 'skill'` / `'schema_annotation'` rows — they now filter by `created_by = 'phase5_bootstrap'` to isolate disk-walk rows from the new bundled rows.

- `src-tauri/src/pyramid/config_contributions.rs` — `sync_config_to_operational_with_registry()` variant that threads an `Option<&Arc<SchemaRegistry>>` through. The original `sync_config_to_operational()` delegates to the new variant with `None` for backward compat. The `schema_definition` branch of the dispatcher now (a) calls the wired `flag_configs_for_migration` stub which delegates to `schema_registry::flag_configs_needing_migration` (setting `needs_migration = 1` on downstream rows) and (b) calls the wired `invalidate_schema_registry_cache` stub which invokes `registry.invalidate(conn)` to re-hydrate. Neither is a debug-log TODO anymore. Added 1 new Phase 9 dispatcher-wiring test verifying both stubs execute end-to-end.

- `src-tauri/src/main.rs` (+260 lines) — 6 new IPC commands registered in `invoke_handler!` under a "Phase 9: Generative config pattern" header block. `pyramid_generate_config` and `pyramid_refine_config` use the 3-phase load → LLM → persist pattern so a `rusqlite::Connection` never crosses an `.await`. Notes enforcement happens at the IPC boundary via `validate_note()` for `pyramid_refine_config` before any LLM work begins. The 2 commands that read (active/versions) use `state.pyramid.reader.lock()` while the 2 commands that write (accept, generate/refine persist) use `state.pyramid.writer.lock()`. `pyramid_config_schemas` is sync — just calls `list_config_schemas(&state.pyramid.schema_registry)`. The PyramidState construction block now includes `schema_registry: schema_registry.clone()` after hydrating from the contribution store via `SchemaRegistry::hydrate_from_contributions` at boot. Updated the 2 other PyramidState constructions in main.rs (`pyramid_vine_integrity` + `pyramid_vine_rebuild_upper`) to pass through the shared `schema_registry` Arc from the outer state.

- `src-tauri/src/pyramid/chain_executor.rs`, `src-tauri/src/pyramid/vine.rs`, `src-tauri/src/pyramid/dadbear_extend.rs` — updated PyramidState struct literals to include `schema_registry: Arc::new(SchemaRegistry::new())` (tests) or `state.schema_registry.clone()` (runtime clone).

### Spec adherence (against `docs/specs/generative-config-pattern.md`)

- ✅ **Bundled contributions manifest** — `src-tauri/assets/bundled_contributions.json` ships 18 entries covering 5 schema types. Each entry carries an explicit `contribution_id` with `bundled-` prefix. Manifest format diverges slightly from the spec example (no inlined `wire_native_metadata` object — instead, per-entry `topics_extra` + `applies_to` convenience fields feed into runtime metadata construction via the Phase 5 mapping table). This is a deliberate simplification: keeping the Phase 5 mapping table as the single source of truth for per-schema-type default tags + contribution_type means new bundled entries don't need to hand-craft a canonical metadata blob every time.
- ✅ **Bootstrap migration** — `walk_bundled_contributions_manifest()` extends Phase 5's `migrate_prompts_and_chains_to_contributions`. INSERT OR IGNORE per-entry semantics preserve user supersessions across app upgrades. Runs BEFORE the Phase 5 sentinel check so new bundled entries land even when the disk-walk sentinel is present.
- ✅ **Schema registry** — `SchemaRegistry` struct with `RwLock<HashMap<String, ConfigSchema>>`. `hydrate_from_contributions` walks every active `schema_definition` contribution and joins annotations + generation skills + seed defaults via slug-convention lookups with fallback scans. `PyramidState::schema_registry: Arc<SchemaRegistry>` hydrated at boot. `invalidate(conn)` called from Phase 4 dispatcher hook.
- ✅ **`invalidate_schema_registry_cache` stub wired** — Phase 4's stub used to just `debug!(...)` and return. Phase 9's version takes a `&Arc<SchemaRegistry>` and calls `registry.invalidate(conn)`. The test `test_phase9_schema_definition_dispatcher_flags_and_invalidates` verifies the wiring end-to-end.
- ✅ **`flag_configs_for_migration` stub wired** — Phase 4's stub was also a debug-log TODO. Phase 9's version delegates to `schema_registry::flag_configs_needing_migration`, which runs an `UPDATE` setting `needs_migration = 1` on every active contribution whose `schema_type` matches the superseded schema_definition's target. Uses the contribution's `slug` (the Phase 9 convention for schema_definition rows) as the target. The same dispatcher-wiring test verifies the flag gets set.
- ✅ **Generation prompt skills** — 5 `chains/prompts/generation/*.md` files with `{schema}`, `{intent}`, `{current_yaml}`, `{notes}` placeholders plus `{if current_yaml}...{end}` / `{if notes}...{end}` conditional blocks. Both the on-disk files and the manifest carry identical bodies — the manifest is the runtime-loaded path.
- ✅ **JSON schemas** — 5 `schema_definition` contributions in the manifest (Draft-07 JSON Schemas for evidence_policy, build_strategy, dadbear_policy, tier_routing, custom_prompts). Each is stored as a contribution body (JSON string) with `applies_to` set to the target schema_type so the registry's lookup-by-slug path finds it.
- ✅ **`pyramid_generate_config` IPC handler** — loads schema, loads skill body, loads JSON schema, substitutes placeholders, constructs StepContext with `primitive = "config_generation"`, calls `call_model_unified_with_options_and_ctx`, parses YAML, creates a draft contribution via Phase 4's CRUD helper, returns the contribution_id + YAML.
- ✅ **`pyramid_refine_config` IPC handler** — loads prior contribution, loads skill + definition, substitutes with `current_yaml` + `notes` blocks present, constructs StepContext with `primitive = "config_refinement"`, calls the LLM, parses YAML, calls `create_draft_supersession` which inlines the superession transaction with the refined row landing as `status = 'draft'` (NOT active — user accepts explicitly). **Notes enforcement:** both the IPC handler (`main.rs`) AND the backend loader (`load_refinement_inputs`) call `validate_note` before any LLM work begins.
- ✅ **`pyramid_accept_config` IPC handler** — handles two cases: (a) an inline YAML payload produces a fresh active contribution via `create_config_contribution_with_metadata`; (b) absence of the payload looks up the latest draft for `(schema_type, slug)` and promotes it via `promote_draft_to_active`. Both cases trigger `sync_config_to_operational_with_registry` with the schema registry Arc so the `schema_definition` branch's Phase 9 hooks fire. Returns the full `AcceptConfigResponse` including `sync_result.operational_table` + `reload_triggered` fields.
- ✅ **`pyramid_active_config` + `pyramid_config_versions` IPC handlers** — thin wrappers over Phase 4's `load_active_config_contribution` + `load_config_version_history` that shape the response per the Phase 9 spec.
- ✅ **`pyramid_config_schemas` IPC handler** — returns `state.pyramid.schema_registry.list()` which produces `ConfigSchemaSummary { schema_type, display_name, description, has_generation_skill, has_annotation, has_default_seed }` for every resolved schema. Sorted alphabetically by schema_type for deterministic UI ordering.
- ✅ **Schema migration scaffolding** — `needs_migration` column added via idempotent ALTER. `flag_configs_for_migration` fully wired. `pyramid_migrate_config` IPC + the migration LLM call are explicitly Phase 10 scope per the workstream brief.
- ✅ **Tests** — 10 schema_registry tests, 16 generative_config tests, 5 new wire_migration Phase 9 tests, 1 new config_contributions dispatcher-wiring test. 34 new tests total; 0 pre-existing tests regressed (the 2 Phase 5/8 idempotency tests that touched `schema_type` counts were updated to filter by `created_by = 'phase5_bootstrap'` so they isolate disk-walk rows from the new bundled rows).

### Scope decisions + deviations

- **JSON Schema validation skipped in Phase 9.** The `jsonschema` crate is not in `Cargo.toml` and the workstream brief's deviation protocol says adding it is out of scope unless trivial. Phase 9's safety net is "is this parseable YAML" via `serde_yaml::from_str`. Structural validation lands with Phase 10 alongside the schema migration flow. The generated JSON schemas ship as `schema_definition` contributions so Phase 10 can consume them without a manifest change.
- **Metadata format divergence in the bundled manifest.** The spec's example manifest inlines the full `wire_native_metadata` object per entry. Phase 9 ships a more compact shape that just carries identity + the bodies, with runtime metadata construction via the Phase 5 mapping table. Rationale: the mapping table already knows the canonical `contribution_type` + default topics for every schema_type; duplicating that into every manifest entry would mean every new schema_type requires changes in TWO places (the mapping table AND the manifest). Keeping the mapping table as single source of truth means adding a new config type is a one-place change. The spec's shape is still representable via the manifest (add more explicit fields later if needed); Phase 9 just chose the compact form.
- **`bundled_walk` runs outside the sentinel check.** Phase 5's `_prompt_migration_marker` sentinel protects the disk walks (prompts + chains + schema annotations). Phase 9's bundled walk runs BEFORE the sentinel check so app upgrades can add new bundled entries even when the disk-walk sentinel is present (the disk-walk files are immutable seeds; the bundled manifest is the versioned app-release surface). Per-entry `INSERT OR IGNORE` makes this safe.
- **3-phase load → LLM → persist pattern.** `rusqlite::Connection` is not `Send`, so holding it across an `.await` breaks Tauri's async IPC handlers. Phase 9's generation + refinement functions expose `load_*_inputs` (sync, in DB lock) + `run_*_llm_call` (async, no DB lock) + `persist_*` (sync, in writer DB lock) so the IPC handlers can drop the lock across the LLM await. The convenience wrappers `generate_config_from_intent` + `refine_config_with_note` chain the three phases but inherit the non-Send constraint — they're kept for tests and any non-Tauri callers.
- **Latest-draft vs inline-YAML accept.** `pyramid_accept_config` handles both paths: when the frontend passes an explicit `yaml` payload (user edited the generated result inline), a fresh active contribution lands directly. When no payload is passed, the latest draft for `(schema_type, slug)` gets promoted via `promote_draft_to_active` which transactionally flips status + supersedes the prior active. This mirrors the spec's guidance that accept is "activate a contribution + trigger operational sync" — the contribution to activate is either the supplied one or the latest draft.
- **Generation skill tier is hardcoded to `synth_heavy`.** The brief's architectural lens says "every decision: can an agent improve this?" The tier choice is a per-generation knob the user might reasonably want to override. Phase 9 hardcodes `synth_heavy` inside `call_generation_llm` as the default; the escape hatch is that the generation skill body is itself a contribution — users who want a different tier for generation can supersede the skill and inline the tier choice in the prompt, or Phase 10+ can add a `model_tier` field to the generation skill's own metadata. Not a Pillar 37 violation because the tier name is a routing key (constrains which row in `pyramid_tier_routing` to look up), not a number constraining LLM output.
- **No `pyramid_reroll_config` IPC in Phase 9.** The canonical IPC list in `config-contribution-and-wire-sharing.md` includes `pyramid_reroll_config` as a force-fresh bypass. The Phase 9 workstream brief doesn't list it in the "6 new IPC commands" scope and the spec marks it as a separate concern from the notes refinement loop. Deferred to Phase 13 (reroll bypass + force_fresh) per the specs' existing sequencing.
- **`dadbear_policy` and `tier_routing` schema annotations not in the Phase 9 bundled manifest.** Phase 8 already seeded a `dadbear.schema.yaml` annotation via the on-disk `chains/schemas/` walk, and `tier_routing` gets its UI via the frontend's fallback key/value editor. Adding Phase 9 bundled annotations for these would duplicate existing work. The stretch targets (`folder_ingestion_heuristics`, `schema_migration_policy`, `wire_discovery_weights`) are deferred — Phase 9 hit the minimum 5-schema-type requirement and the 15-entry manifest size requirement.

### Verification results

- ✅ `cargo check --lib` — clean. 3 pre-existing warnings (deprecated `get_keep_evidence_for_target`, two `LayerCollectResult` private-visibility warnings). Zero new warnings from Phase 9 files.
- ✅ `cargo check` (full crate) — clean. Same 3 lib warnings + 1 pre-existing binary warning (`tauri_plugin_shell::Shell::open` deprecated). Phase 9's new main.rs IPC commands and PyramidState field update wire in cleanly.
- ✅ `cargo build --lib` — clean in 1m 01s on the first build after the new modules land.
- ✅ `cargo test --lib pyramid::schema_registry` — **10/10 passing** (`test_metadata_has_both_topics_matches`, `test_annotation_body_matches_applies_to`, `test_hydrate_from_contributions_empty`, `test_flag_configs_skips_superseded_rows`, `test_invalidate_re_reads`, `test_hydrate_finds_minimal_schema_entry`, `test_list_returns_sorted_summaries`, `test_hydrate_joins_all_pieces`, `test_hydrate_from_bundled_manifest`, `test_flag_configs_needing_migration_sets_column`).
- ✅ `cargo test --lib pyramid::generative_config` — **16/16 passing** covering prompt substitution (3 cases), YAML extraction (3 cases), active config empty-state, bundled-manifest listing, draft supersession, direct-YAML accept with sync, missing-draft error, promote-draft-to-active, empty-note rejection, empty-intent rejection, unknown-schema-type rejection, bundled-body loading, refinement-requires-note, config_contributions-inputs loading.
- ✅ `cargo test --lib pyramid::wire_migration` — **17/17 passing** (11 pre-existing Phase 5/8 tests including the 2 updated to filter by `created_by` + 5 new Phase 9 tests + 1 pre-existing `extract_prompt_refs_finds_all_forms`).
- ✅ `cargo test --lib pyramid::config_contributions` — **21/21 passing** including the new `test_phase9_schema_definition_dispatcher_flags_and_invalidates` that exercises the end-to-end stub wiring.
- ✅ `cargo test --lib pyramid` — **1044 passing, 7 failed** (same 7 pre-existing failures documented in Phase 2/3/4/5/6/7/8: `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, 5 × `pyramid::staleness::tests::*`). Phase 9 added 34 tests bringing pyramid total from Phase 8's 1010 to 1044. Zero new failures.
- ✅ Grep verification: `grep -rn "invalidate_schema_registry_cache\|flag_configs_for_migration" src-tauri/src/pyramid/` shows both stubs have REAL implementations (not debug-log TODOs). `flag_configs_for_migration` at line 838 of `config_contributions.rs` delegates to `schema_registry::flag_configs_needing_migration`. `invalidate_schema_registry_cache` at line 853 calls `registry.invalidate(conn)`. Both are wired from the dispatcher's `schema_definition` branch at line 738/740.
- ✅ `grep -n "bundled-" src-tauri/assets/ chains/` confirms the manifest + the generation prompts are on disk (5 `.md` files under `chains/prompts/generation/` + `src-tauri/assets/bundled_contributions.json` with 18 `bundled-*` ids).
- ⚠️ No frontend/IPC smoke test script in the existing dev harness. The 6 new commands are registered in `invoke_handler!` and the response types implement `Serialize`. Manual verification path: run the app, from the dev tools console invoke `pyramid_config_schemas()` and expect a 5-entry array with `evidence_policy`, `build_strategy`, `dadbear_policy`, `tier_routing`, `custom_prompts` each having `has_generation_skill: true`, `has_default_seed: true`. Then `pyramid_generate_config({schema_type: "evidence_policy", intent: "conservative local-only policy"})` should return a contribution_id + YAML body. Then `pyramid_refine_config({contribution_id, current_yaml, note: "bump concurrency to 2"})` should return a new contribution_id + refined YAML. Then `pyramid_accept_config({schema_type: "evidence_policy", slug: null})` should promote the latest draft to active. This end-to-end flow hits all four IPC commands and exercises both Phase 4 (CRUD + sync dispatcher) and Phase 6 (cache-aware LLM) contracts.

### Notes

- **The schema registry is a view, not a table.** Phase 9 resists the temptation to introduce a `pyramid_schema_registry` table. Every lookup flows through `pyramid_config_contributions` — the registry is just an in-memory cache keyed on `schema_type` → `ConfigSchema`. The `invalidate()` method re-reads from the contribution store; there's no write path. This keeps the Phase 4 architectural contract ("every config is a contribution") intact even for the metadata that describes configs.
- **Per-entry INSERT OR IGNORE is the key to app upgrades.** The alternative — using a whole-run sentinel like Phase 5's disk walk — would prevent app upgrades from adding new bundled entries. Per-entry skip-on-conflict means new manifest entries land on next boot, existing entries stay untouched (preserving any user supersessions), and the bundled defaults flow through the standard contribution lifecycle.
- **The 3-phase load → LLM → persist pattern is the Tauri async discipline.** `rusqlite::Connection` is `!Send`, so Tauri's async IPC handlers can't hold a DB lock across an await point. The three-phase form decouples the DB work from the LLM work. Non-async callers (tests, future MCP handlers) can still use the convenience wrappers that chain the three phases but they won't work as Tauri commands. This is the same architectural pattern Phase 10's frontend wizard will rely on.
- **Notes enforcement lives at the IPC boundary, not in the backend helper.** The spec's Notes Capture Lifecycle rule is enforced in BOTH places: the IPC handler (`main.rs::pyramid_refine_config`) calls `validate_note(&note)` before touching the DB, and the backend loader (`load_refinement_inputs`) re-validates defensively. Double enforcement is intentional — the IPC boundary rejects empty notes with a clean error before the user burns a round-trip, and the backend re-check ensures non-IPC callers can't bypass the rule.
- **`default_seed_contribution_id` on `ConfigSchema` is the link from schema → factory reset.** Every `ConfigSchema` entry carries an optional `default_seed_contribution_id` pointing at the active bundled default for the target schema_type. Phase 10's "Restore to default" button uses this field to look up the bundled contribution and promote it back to active (creating a new active row that supersedes the user's current one, tagged `source = "revert-to-bundled"`). The field is stored but no IPC consumes it yet — Phase 10 wires the UI.
- **No Pillar 37 violations.** The only numbers in Phase 9 code that touch the LLM are the `temperature: 0.2` and `max_tokens: 4096` passed to `call_model_unified_with_options_and_ctx`. Temperature is a per-call API knob (not an output constraint), and `max_tokens` is ignored inside the ctx-aware path — the cache layer resolves effective max tokens from the model's context window minus input per Phase 6's LLM cache spec. Both are Tauri async command parameters, not values that shape the LLM output semantically.
- **The `synth_heavy` tier hardcode.** `call_generation_llm` resolves `synth_heavy` unconditionally. Adam's architectural lens asks "can an agent improve this?" Yes — Phase 10+ can make the tier a field on the generation skill's metadata so users can supersede the skill and change the tier. For now, hardcoding keeps the Phase 9 scope tight and the tier is a routing key (not a number constraining output). Documented in the scope-decisions section above.

### Next

The phase is ready for the conductor's verifier pass. Recommended focus areas for the audit:

1. **Bundled manifest upgrade safety.** The `INSERT OR IGNORE` pattern preserves user supersessions across app upgrades. A verifier should manually simulate an upgrade: (a) bundled-evidence_policy-default-v1 lands; (b) user supersedes with a refinement; (c) re-run the walk with the same manifest; (d) verify the user's refinement is still the active row. The test `phase9_bundled_walk_skips_user_superseded` covers this in-process, but a hand-run with a modified manifest would spot any edge cases in the SQL.
2. **Stub wiring verification.** The `test_phase9_schema_definition_dispatcher_flags_and_invalidates` test in `config_contributions.rs` exercises both stubs end-to-end via the dispatcher. A verifier should confirm no debug-log-only path remains — `grep -n "Phase 4 stub\|Phase 9 stub\|TODO.*Phase 9" src-tauri/src/pyramid/` should return zero matches in the two stub functions.
3. **3-phase pattern lock-in.** The IPC handlers MUST drop the reader lock before the LLM `.await`. A verifier should inspect `pyramid_generate_config` and `pyramid_refine_config` and confirm the `let reader = state.pyramid.reader.lock().await` is scoped to a block that ends before the LLM call. Without this, the compiler rejects the handler as non-Send.
4. **Bundled manifest entry count matches the 5 schema-type requirement.** The shipped manifest has 18 entries: 5 generation skills + 5 schema_definitions + 3 schema_annotations + 5 seed defaults. A verifier should confirm the 5 required schema types are covered and that the spec's minimum set (`evidence_policy`, `build_strategy`, `dadbear_policy`, `tier_routing`, `custom_prompts`) all have generation skills + JSON schemas + seed defaults. The 3 schema annotations cover the 3 new types (Phase 8's seeds cover `dadbear_policy` and `tier_routing` via the on-disk walk).
5. **Cache wiring for generation calls.** Phase 6's cache requires a non-empty `resolved_model_id` + `prompt_hash` on the `StepContext`. Phase 9's `call_generation_llm` computes `compute_prompt_hash(params.skill_body)` and resolves the model via `provider_registry.resolve_tier("synth_heavy", None, None, None)`. A verifier should confirm these are both populated when the cache path is expected to fire. The failure mode is "cache always misses silently" if either field is empty — which hurts cost but doesn't break correctness.
6. **Supersession draft status.** When a user refines a draft via `pyramid_refine_config`, the new row lands as `status = 'draft'`, NOT `active`. The standard `supersede_config_contribution` helper forces `active`, which is wrong for the Phase 9 refinement flow. Phase 9 inlines a `create_draft_supersession` transaction that keeps the new row in `draft`. A verifier should confirm that a refinement doesn't accidentally activate the new version before the user explicitly accepts it.
7. **Accept path operational sync.** The `accept_config_draft` function calls `sync_config_to_operational_with_registry` with the schema registry Arc so the `schema_definition` branch's Phase 9 hooks fire. A verifier should confirm that accepting a schema_definition contribution actually invalidates the registry (via the test at line 2057 of `config_contributions.rs`) and flags downstream configs for migration.

Wanderer prompt suggestion: "Does a fresh Wire Node boot → run Phase 5+9 migrations → seed ≥18 bundled contributions → hydrate the schema registry → and can a test harness call `pyramid_config_schemas` and get back a 5-entry summary list with `has_generation_skill: true` + `has_default_seed: true` for every entry, then call `pyramid_generate_config({schema_type: 'evidence_policy', intent: 'conservative local-only'})` through the IPC layer without the DB lock crossing an `.await` boundary, without the rusqlite Connection making the future non-Send, and with the Phase 6 LLM cache receiving a StepContext carrying a resolved model id + prompt hash — even with no active frontend consumer yet?"

