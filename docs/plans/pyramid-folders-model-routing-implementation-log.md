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
