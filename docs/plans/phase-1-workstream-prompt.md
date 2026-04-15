# Workstream: Phase 1 — DADBEAR In-Flight Lock

## Who you are

You are an implementer joining an active 17-phase initiative to turn Wire Node into a Wire-native application. Phase 0a (clippy cleanup) and Phase 0b (wire `fire_ingest_chain` to actually dispatch chains from Pipeline B) are already shipped. You are the implementer of Phase 1.

Phase 1 is small. The spec calls for ~20 lines of code plus a test. Do not inflate the scope.

## Context: why Phase 1 exists now (and not before 0b)

Phase 0b wired the previously-stubbed `dispatch_pending_ingests` in `src-tauri/src/pyramid/dadbear_extend.rs` to actually fire chain builds via `fire_ingest_chain` → `build_runner::run_build_from`. Until Phase 0b landed, the tick loop's per-config work was fast enough that re-entrancy was not a real risk. After Phase 0b, a single tick can take minutes because it now runs a real chain build. The 1-second base tick will fire again while the previous tick is still inside `run_build_from`, and without a guard the next tick will start a concurrent dispatch for the same config — racing on ingest record state transitions, `LockManager` acquisition, and event emission ordering.

Phase 1 adds a per-config in-flight flag that causes the tick loop to skip any config whose previous dispatch has not yet returned. This is the corrected framing of the original addendum's spec: **the in-flight lock does NOT fix the 200→528 L0 cascade** (that lives in Pipeline A's `stale_helpers_upper.rs::execute_supersession` and is Phase 2's scope). It guards a DIFFERENT race that is now live.

## Required reading (in order, in full unless noted)

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md` — read end-to-end. The "Phase 1's symptom attribution has been corrected" section is your primary framing. The lock guards tick re-entrancy, not cascade propagation.
2. `docs/specs/evidence-triage-and-dadbear.md` **Part 1 only** — the corrected Phase 1 spec. This is your implementation contract.
3. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — read the Phase 0b entry in full. The implementer log describes what Phase 0b shipped and the lock ordering contract you must continue to respect.
4. `src-tauri/src/pyramid/dadbear_extend.rs` — the whole file. Your change sits inside `start_dadbear_extend_loop` (currently around lines 76-149) and the tick loop inside it. Pay attention to `ConfigTicker` (line 58-62), the tick loop's `for config in &configs` block, and how `run_tick_for_config` is invoked from inside that loop. Phase 0b added `Arc<PyramidState>` threading — your change must continue to thread state correctly.
5. `src-tauri/src/pyramid/lock_manager.rs` doc comment only (lines 1-95). You need to understand why the spec says "use an AtomicBool per config.id, not a LockManager write lock" — the LockManager write lock is per-slug and would block queries; the in-flight flag is per-config and only prevents re-entrant ticks.

You do not need to re-read `build_runner.rs`, `chain_executor.rs`, `ingest.rs`, `chain_registry.rs`, or `main.rs` for this phase. Phase 0b already wired those and Phase 1 does not touch them.

## What to build

### The flag

Add a `HashMap<i64, Arc<AtomicBool>>` to the tick loop state (inside the `tokio::spawn(async move { ... })` closure in `start_dadbear_extend_loop`), keyed by `config.id`. The entry is lazily created the first time a config is seen, same lifecycle as the existing `tickers: HashMap<i64, ConfigTicker>`.

### The check

Before running `run_tick_for_config` for a config, check the flag. If already set, log `debug!(slug = %config.slug, "DADBEAR: skipping tick, previous dispatch in-flight")` and `continue` without running the tick. If not set, set it, then run the tick, then clear it.

### Panic safety (critical)

**The flag MUST be cleared on every exit path from `run_tick_for_config`, including panics.** The spec's inline sketch uses a simple `flag_clone.store(false, Ordering::Relaxed)` after the match arm. That is not panic-safe: if `run_tick_for_config` panics, the store is never reached, and the flag stays stuck at `true` forever, skipping all future ticks for that config until the process restarts. This is a real risk — the chain path can panic on LLM parse failures, DB corruption, etc.

Use an RAII guard struct with `impl Drop` that clears the flag on drop. Construct it AFTER setting the flag and BEFORE calling `run_tick_for_config`. The guard's `Drop::drop` runs regardless of panic or normal return.

```rust
struct InFlightGuard(Arc<AtomicBool>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
```

### Memory hygiene

When a config is removed (the tick loop already has `tickers.retain(|id, _| configs.iter().any(|c| c.id == *id))` around line 111), the in-flight flag entry for that config should also be removed. Add an equivalent `.retain()` call for the flag HashMap.

### Test

Add a test that exercises the in-flight skip path. The test should:

1. Create a test DB (reuse the existing `test_db()` helper)
2. Simulate a long-running `run_tick_for_config` via an `Arc<AtomicBool>` that the test controls
3. Verify that if the flag is set, a second tick attempt observes it set and skips (the log message can be asserted via `tracing-test` if available, or just assert the side effect — e.g., the second tick does not double-process a pending ingest record)

If mocking `run_tick_for_config` is complex, a lighter test is acceptable: construct the `HashMap<i64, Arc<AtomicBool>>` manually, set a flag, simulate a single loop iteration's flag-check logic, assert the skip. The point is to exercise the skip decision, not necessarily the full tick path.

## Scope boundaries

**In scope:**
- Adding the in-flight `HashMap<i64, Arc<AtomicBool>>` to `start_dadbear_extend_loop`'s tick loop
- The RAII guard struct that clears the flag on drop
- The skip-with-debug-log path when the flag is already set
- The retain-cleanup when a config is removed
- At least one test verifying the skip decision

**Out of scope:**
- Any changes to `fire_ingest_chain`, `dispatch_pending_ingests`, or `run_tick_for_config` bodies. The flag wraps the tick invocation, not its internals.
- Any changes to Pipeline A (`watcher.rs`, `stale_engine.rs`, `stale_helpers_upper.rs`). Pipeline A has its own guards (`start_timer` debounce, `check_runaway` breaker). Do not touch.
- The stale-check cascade fix (Phase 2's scope — `change-manifest-supersession.md`).
- New `TaggedKind` variants.
- Per-slug config (LockManager) changes.

## Verification criteria

1. **`cargo check` and `cargo build`** from `src-tauri/` — clean, zero new warnings in files you touched.
2. **`cargo test --lib pyramid::dadbear_extend`** — all existing tests still pass PLUS your new test(s). Post-Phase-0b there are currently 11 tests in this module.
3. **Human-verification checklist** (add to the implementation log as a pending checklist item): long DADBEAR dispatch scenario — start the app with a DADBEAR-enabled conversation pyramid, drop a file, observe the first dispatch running. Before it completes, observe subsequent 1-second ticks emitting the `"DADBEAR: skipping tick, previous dispatch in-flight"` debug log for the same config. When the dispatch completes, observe the next tick proceeds normally.

## Deviation protocol

Same as every phase: append to `docs/plans/pyramid-folders-model-routing-friction-log.md` AND include a clearly-framed question at the top of your final summary. Do not deviate silently. You are empowered to correct the plan if it gets details wrong, but NOT to change the end state: the tick loop has a per-config in-flight guard that skips re-entrant ticks and is panic-safe.

## Implementation log protocol

Append / update the Phase 1 entry in `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Use the format defined at the top of that file. Minimum fields:

- **Started / Completed** ISO timestamps
- **Files touched:** every file with a brief change description
- **Spec adherence:** one line per spec element (the flag, the check, the skip, the RAII guard, the retain cleanup, the test)
- **Verification results:** cargo check/build/test results, note that human verification is pending
- **Status:** `awaiting-verification`

Do NOT write `verified` in your own entry — that's reserved for a later pass.

## Mandate

- **Correct before fast.** Do not skip the panic-safe RAII guard. The naive `store(false)` pattern is a stuck-flag bug waiting to happen.
- **Right before complete.** If you find the spec's inline sketch doesn't actually work (e.g., a type issue with the guard closing over `Arc<AtomicBool>`), fix it properly and note the correction in the log.
- **No speculative abstractions.** One guard struct. One HashMap. No "future-proofing" generics or trait definitions.
- **Pillar 37:** watch for any magic number that constrains LLM output — Phase 1 shouldn't introduce any, but the `Ordering::Relaxed` choice is a defensible atomics decision, not a Pillar 37 concern.
- **Fix all bugs found.** If you spot an adjacent bug in `dadbear_extend.rs` while working, fix it and note in the friction log.
- **Commit when done.** Single commit with message `phase-1: dadbear in-flight lock`. Body should summarize the flag lifecycle and the panic-safety decision. Do not amend; create a fresh commit. Do not push.

## End state

Phase 1 is complete when:

1. `start_dadbear_extend_loop`'s tick loop has a `HashMap<i64, Arc<AtomicBool>>` of in-flight flags keyed by `config.id`, lifecycle-matched to the existing `tickers` HashMap.
2. Before running `run_tick_for_config` for a config, the flag is checked; if set, the tick is skipped with a `debug!` log.
3. The flag is set via an RAII `InFlightGuard` struct with `impl Drop` that clears the flag on drop — panic-safe.
4. When a config is removed, its flag entry is removed from the HashMap.
5. At least one new test verifies the skip decision.
6. `cargo check`, `cargo build`, `cargo test --lib pyramid::dadbear_extend` all pass, with 11 pre-existing tests + the new Phase 1 test(s) all green.
7. The implementation log Phase 1 entry is filled in.
8. A single commit lands cleanly on branch `phase-1-dadbear-inflight-lock`.

Begin with the reading. This phase is small; the reading should be fast.

Good luck. Build carefully.
