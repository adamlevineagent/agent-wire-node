# Audit Handback to Tester — Chain `for_each` Concurrency Pass

## Date

2026-03-24

## Goal of this pass

Remove the biggest avoidable performance bottleneck in the chain-engine pyramid build: fully sequential `for_each` execution for independent LLM calls.

This pass adds configurable concurrency to `for_each` steps while intentionally preserving serial behavior for steps that rely on sequential accumulators.

## What changed

### 1. Added `concurrency` to `ChainStep`

File:

- `src-tauri/src/pyramid/chain_engine.rs`

- Added a new `concurrency` field to `ChainStep`.
- Default is `1`, so existing chains remain backward-compatible and keep current behavior unless they opt in.

Validation added:

- `concurrency` must be `>= 1`
- `concurrency > 1` requires `for_each`
- `sequential: true` cannot be combined with `concurrency > 1`

### 2. `for_each` now has a real concurrent execution path

File:

- `src-tauri/src/pyramid/chain_executor.rs`

Behavior:

- sequential steps still use the old serial path
- non-sequential `for_each` steps with `concurrency > 1` now:
  - precompute per-item inputs/resume state serially
  - preserve `$item` / `$index` resolution correctness
  - dispatch LLM work in parallel behind a semaphore
  - keep DB writes serialized through the existing `writer_tx` channel
  - preserve output ordering by writing each completed item back into its original slot

Important design choice:

- accumulator-dependent steps remain serial on purpose
- only independent steps fan out

### 3. Chain-context and dispatch-context cloning support were added

Files:

- `src-tauri/src/pyramid/chain_resolve.rs`
- `src-tauri/src/pyramid/chain_dispatch.rs`

- `ChainContext` is now cloneable so each concurrent work item can resolve its own `$item` / `$index` safely.
- `StepContext` is now cloneable so each worker can dispatch independently without sharing mutable executor state.

### 4. The code pipeline now opts into concurrency where it matters

File:

- `chains/defaults/code.yaml`

Updated:

- `l0_code_extract`
  - `concurrency: 8`
- `thread_narrative`
  - `concurrency: 5`

These are the two heavy `for_each` phases that were previously one-at-a-time.

### 5. Sequential accumulator behavior is explicitly protected

Files:

- `src-tauri/src/pyramid/chain_engine.rs`
- `src-tauri/src/pyramid/chain_executor.rs`

- The executor does not run concurrent fan-out for `sequential: true` steps.
- Validation now rejects `sequential + concurrency > 1` so this cannot silently misbehave in YAML.

## Expected runtime effect

Approximate effect for the code chain:

- L0 extraction: instead of 112 serial calls, up to 8 can run at once
- L1 thread synthesis: instead of 10-ish serial thread calls, up to 5 can run at once

This should materially reduce wall-clock build time, especially on larger code slugs.

## Verification run

Ran successfully:

- `cargo fmt --manifest-path src-tauri/Cargo.toml`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_executor::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_engine::tests -- --nocapture`

Status:

- `chain_executor::tests`: 10 passed
- `chain_engine::tests`: 12 passed

Known unrelated warnings during test runs:

- `src-tauri/src/pyramid/vine.rs`: `true_orphans` assigned but never read
- `src-tauri/src/pyramid/vine.rs`: unused variable `llm`

I did not change those warnings in this pass.

## What still needs live confirmation

I could not run the networked LLM build from here, so the remaining check is a real app/runtime build.

Recommended live check:

1. Restart/rebuild the app/backend so the Rust executor changes are loaded.
2. Run a real code-pyramid build.
3. Confirm logs show multiple L0 and L1 items completing out of order instead of strictly one-by-one.
4. Confirm total node count and child wiring still match expected output.
5. Watch for provider-side rate limiting or timeout changes at the new concurrency levels.

## Net result

This pass removes the main executor-side serialization bottleneck without changing chain semantics:

- default behavior is still safe
- sequential accumulator steps still stay sequential
- independent `for_each` LLM calls can now run in parallel
- the code chain is configured to actually use the new capability
