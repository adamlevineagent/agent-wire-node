# Handoff Addendum 01 — Pipeline B scope fix and Phase 0 expansion

**Date:** 2026-04-09 (later the same day as the original handoff)
**Supersedes portions of:** `handoff-2026-04-09-pyramid-folders-model-routing.md`
**Read order:** read the original handoff first, then this addendum. Specs and master plan have been updated in place — this file is the changelog explaining WHY.

---

## Why this addendum exists

After the original handoff went out, an implementer reviewing Phase 1 caught that the symptom attribution in the Phase 1 spec didn't match the current tree. The catch led to a full re-read of `dadbear_extend.rs`, `watcher.rs`, `stale_engine.rs`, and `stale_helpers_upper.rs`, which revealed that:

- **"DADBEAR" is actually two parallel pipelines, not one unified subsystem.** The original plan treated it as unified.
- **Pipeline B (`dadbear_extend.rs`) was shipped one day before the original handoff was written** as part of the episodic-memory vine work (commit `b78169e`, 2026-04-08), with its chain dispatch explicitly stubbed awaiting WS-EM-CHAIN that never landed.
- **The "200 → 528 L0 blowup" symptom I attributed to Phase 1 is actually caused by Phase 2's code.** Phase 1 couldn't fix that symptom even if it tried — wrong pipeline, wrong function.
- **Phase 17 (folder ingestion) implicitly assumed Pipeline B was live.** It isn't yet.

The fix is to complete Pipeline B up front, then the rest of the plan works as originally designed. This addendum documents the corrections that have been committed to the specs and master plan.

---

## The two pipelines (now documented explicitly in the specs)

| Pipeline | File | First committed | Responsibility | Current state |
|---|---|---|---|---|
| **A** | `watcher.rs` | 2026-03-23 (`aa4f12a` — original DADBEAR auto-stale system) | **Maintenance** of already-ingested files. fs-notify events on files in `pyramid_file_hashes` → writes `pyramid_pending_mutations` → `stale_engine.rs` polls + debounces → `stale_helpers_upper.rs::execute_supersession` creates new node versions | Live, in use, has its own guards (`start_timer` debounce at line 328, `check_runaway` breaker at line 612) |
| **B** | `dadbear_extend.rs` | 2026-04-08 (`b78169e` — "episodic-memory vine: Phase 2b complete") | **Creation/extension**. Polling scanner → detects new files → `pyramid_ingest_records` → `dispatch_pending_ingests` should run the content-type chain via `fire_ingest_chain` | **Dispatch is stubbed at lines 401-408**. Records are marked "complete" with a placeholder build_id; no chain runs; `pyramid_ingest_records` is effectively a tracking log nothing downstream reads |

The pipelines are **complementary by design**, not duplicates. Pipeline A handles "file I know about changed" (re-sync affected nodes). Pipeline B handles "file I don't know about appeared" (run an extraction chain for it). A newly-ingested file transitions from Pipeline B's domain to Pipeline A's domain the moment `fire_ingest_chain` completes and the file is recorded in `pyramid_file_hashes`.

The only reason Pipeline B looks dead is that the dispatch stub was never replaced. The entire rest of Pipeline B is built and correct: tick loop, config management, ingest record lifecycle, session timeout handling, event emission, LockManager integration. One function away from being live.

---

## What changed in the plan

### Phase 0 is now two parts (0a + 0b), not one

The original Phase 0 was just "commit the clippy cleanup." It's been expanded:

**Phase 0a — Commit clippy cleanup** (unchanged content, same 14 files listed in the original handoff)

**Phase 0b — Finish Pipeline B (`fire_ingest_chain` wiring)** — NEW

Replace the stub at `dadbear_extend.rs:401-408` with a real `fire_ingest_chain` helper that:

1. Resolves the active chain definition for the ingest record's `content_type` via the existing chain registry
2. Constructs a `ChainContext` with the new source file as the ingest input
3. Calls into `build_runner::run_build_from()` / `chain_executor::invoke_chain()` (whichever is the correct entry for firing an ingest chain against a single source file — read the current code before deciding)
4. Captures the returned `build_id` and returns it so the ingest record can be marked complete with the real build_id
5. On chain failure, returns an error that `dispatch_pending_ingests` translates into `mark_ingest_failed` + `IngestFailed` event emission (the existing code path handles this already)
6. Holds LockManager write locks correctly during dispatch — works together with Phase 1's in-flight flag as defense in depth

**Scope estimate:** localized to `dadbear_extend.rs` plus possibly a new helper function. Integration touchpoints (chain registry, `build_runner`, LockManager, event bus) all exist. Plan for 2-4 focused hours, verified by the test described in Phase 1's verification section below.

**What 0b does NOT do:**
- Does NOT obsolete Pipeline A. Pipeline A continues handling maintenance of already-ingested files via fs-notify → stale engine.
- Does NOT merge the two pipelines. They remain complementary.
- Does NOT change `execute_supersession` behavior (that's Phase 2's work, independent).

### Phase 1's symptom attribution has been corrected

The original Phase 1 spec claimed the in-flight lock fixes:
- Duplicate WAL entries in `pyramid_pending_mutations`
- Stacked stale checks
- Evidence loops running aggressively (200 files → 528 L0s)

**None of those symptoms are caused by Pipeline B tick re-entrancy.** They're caused by Pipeline A's `execute_supersession` inserting new nodes per stale check, which Phase 2 fixes. The in-flight lock has never been able to fix those symptoms — they live in a different pipeline.

**The corrected framing** (now in `evidence-triage-and-dadbear.md` Part 1):

Phase 1's lock guards a distinct race that becomes live once Phase 0b wires real chain dispatch. When `dispatch_pending_ingests` actually runs chain builds that can take minutes, the next 1-second tick would start a concurrent dispatch for the same config without this guard, racing on ingest record state transitions, LockManager acquisition, and event emission ordering. The lock prevents that race. It does NOT fix the L0 cascade.

**Verification for Phase 1** (also corrected):

Phase 1 cannot be verified against the current tree because `dispatch_pending_ingests` is stubbed and returns immediately. After Phase 0b wires real dispatch:

1. Mock or temporarily extend `fire_ingest_chain` to block on a `tokio::time::sleep(30s)` future (or use a slow test chain)
2. Enable DADBEAR on a test folder with `scan_interval_secs: 1`
3. Drop a new source file into the folder
4. Assert that subsequent 1-second ticks log `"DADBEAR: skipping tick, previous dispatch in-flight"` and do NOT launch a concurrent dispatch
5. When the slow chain completes, the next tick proceeds normally

Verification is NOT expected to observe any change in `pyramid_pending_mutations` row counts or L0 node counts — those metrics live in Pipeline A, which this lock does not touch.

### Phase 2's scope boundary is now explicit

The original Phase 2 spec said "change-manifests replace `supersede_nodes_above` + full rebuild" without specifying which callers of `supersede_nodes_above` change. An implementer reading Phase 2 loosely could have rewritten all three call sites, which would break two working systems.

`supersede_nodes_above()` has **three callers** in the current tree:

| Caller | Semantic | Phase 2 action |
|---|---|---|
| `stale_helpers_upper.rs::execute_supersession` (lines 1387-1700+) | Stale-update path: INSERTs new node at line 1671, sets `superseded_by` at line 1694, produces the viz orphaning bug | **MODIFIED by Phase 2** — rewrite body to use change-manifest in-place updates |
| `vine.rs:3381` (inside `handle_vine_rebuild_upper` or equivalent) | Explicit wholesale L2+ rebuild triggered by user/system action. Comment at line 3384: *"Superseded {nodes_superseded} nodes and cleared {steps_deleted} steps above L1"* | **NOT modified by Phase 2** — correct wholesale-rebuild semantics, leave alone |
| `chain_executor.rs:4821` (inside `build_lifecycle` fresh path) | Clears leftover L1+ overlay nodes from a prior build attempt. Comment at line 4815: *"Fresh path: supersede all prior L1+ overlay nodes"* | **NOT modified by Phase 2** — correct wholesale-rebuild semantics, leave alone |

The viz orphaning bug is specifically the stale-update path pattern: "insert a new node, leave all the old evidence links pointing at the old ID, hide the old node via the `live_pyramid_nodes` view filter." The two wholesale-rebuild sites don't produce orphaning because they create complete new upper trees with fresh evidence links — there's no half-updated state.

This is now documented in `change-manifest-supersession.md` → "Scope boundary: which call sites this phase touches."

### Phase 17 (folder ingestion) now explicitly depends on Phase 0b

The original Phase 17 spec said "each created pyramid gets a DADBEAR config" and implied that would drive ongoing updates. Before Phase 0b, creating a `pyramid_dadbear_config` row doesn't drive anything because `dispatch_pending_ingests` is stubbed.

The corrected framing (now in `vine-of-vines-and-folder-ingestion.md` → "DADBEAR Integration"):

- Phase 17 depends on Phase 0b having landed first
- For a folder-ingested pyramid, Pipeline B handles new-file ingestion (via `fire_ingest_chain`), and Pipeline A handles ongoing maintenance of files already in `pyramid_file_hashes` (via fs-notify → stale engine → change-manifest per Phase 2)
- The two pipelines coexist naturally because their triggers are disjoint: Pipeline B polls for files NOT yet tracked, Pipeline A watches files that ARE tracked

### Everything else in the plan is unchanged

The Wire contribution mapping, config contribution / Wire sharing, YAML-to-UI renderer, generative config, LLM output cache, credentials, provider registry, cost reconciliation with leak detection, build viz expansion, discovery ranking, and all other phases are unaffected. The corrections are localized to Phase 0, Phase 1, Phase 2 (scope boundary note only, no code scope change), and Phase 17 (dependency note only, no functional change).

---

## How to proceed

**Your first action as the implementer is now Phase 0b**, after the clippy commit (Phase 0a) is already in — which it is, as of this addendum:

```
28fb3e5 chore: clippy cleanup pre-pyramid-folders-model-routing
adc664b plan: pyramid folders + model routing + full-pipeline observability
```

Both are pushed to `origin/main`. Phase 0a is complete.

**Phase 0b begins with reading**, not writing:

1. `src-tauri/src/pyramid/dadbear_extend.rs` end-to-end (you'll be touching lines 401-408 and possibly adding a helper)
2. `src-tauri/src/pyramid/build_runner.rs` — specifically `run_build_from` and the entry points for single-file ingest
3. `src-tauri/src/pyramid/chain_executor.rs` — look at how existing chain invocations are constructed and fired
4. `src-tauri/src/pyramid/ingest.rs` — the existing ingest primitive, especially `cross_build_input` if that's how new files get injected
5. `src-tauri/src/pyramid/chain_engine.rs` — chain registry lookup by `content_type`
6. `src-tauri/src/pyramid/lock_manager.rs` — the lock semantics you'll need to respect

Once you've read those in full, write `fire_ingest_chain` as described in Phase 0b and replace the stub. Verify with a real file drop. Then proceed to Phase 1 (in-flight lock) per the corrected spec.

**Do not deviate silently.** The deviation protocol in the original handoff still applies. If anything here doesn't match reality — for example if `build_runner::run_build_from` turns out to have a different signature than I'm assuming — alert the planner via Adam before picking an alternative, because I specifically should not be trusted to have internalized every detail of the current `build_runner.rs` (I haven't read it in full either — that's next on the planner's reading queue if Phase 0b needs guidance).

**If Phase 0b turns out to be bigger than 2-4 hours** because the wiring requires touching more files than I've anticipated (e.g., new chain entry points, new event types, schema changes), stop and report back. That's a scope signal worth acting on.

---

## Retro note on the miss

This addendum exists because the original plan was written against pyramid drills and grep output, not against full file reads. The implementer's careful reading of `dadbear_extend.rs` lines 401-408 and `stale_helpers_upper.rs::execute_supersession` caught symptoms attributed to the wrong code. The lesson has been captured as a feedback memory extension: `feedback_read_canonical_in_full` now includes "this rule also applies to your own codebase — read functions you're planning to change in full before planning the change."

The plan catching a mistake during implementation review rather than during implementation itself is the system working as designed. But it happened because the implementer was willing to stop and ask specific questions with line numbers attached, which is exactly the deviation protocol the original handoff asked for. Keep doing that.

---

## Signatures

**Planner:** Claude (session partner to Adam). Corrections signed off on; specs and master plan updated in place; changes pushed to `origin/main`. Remaining available for questions via Adam.

**Product owner:** Adam Levine. Approved the direction: "finish Pipeline B as Phase 0b, don't renumber the rest, keep the plan together by making it match the live pipeline we're planning against."

**Git state at addendum time:**
```
adc664b plan: pyramid folders + model routing + full-pipeline observability
28fb3e5 chore: clippy cleanup pre-pyramid-folders-model-routing
74796d4 docs: session 3 handoff — three-gap fix, evidence mode, fast mode shelved  (prior state)
```

Both new commits pushed to `origin/main`.
