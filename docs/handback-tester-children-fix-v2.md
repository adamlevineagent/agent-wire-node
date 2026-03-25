# Audit Handback to Tester — Pyramid Reliability Pass

## Date

2026-03-24

## Goal of this pass

Take the code pyramid pipeline from "mostly fixed" to "fail-loud and recoverable":

- recover L0 child IDs even when thread clustering emits headlines instead of `C-L0-XXX`
- make `from_depth=1` rebuilds actually reuse lower-layer outputs instead of starving clustering
- stop silent partial pyramids from being reported as success
- push the prompts/schemas harder so the LLM uses exact IDs and produces more distinct upper-layer names

## What changed

### 1. Headline-to-ID recovery is now implemented in the executor

File: `src-tauri/src/pyramid/chain_executor.rs`

- Added decorated step outputs so saved/resumed outputs consistently carry:
  - `node_id`
  - `source_node`
  - `chunk_index`
- Added context-side recovery helpers that resolve assignment entries back to real node IDs using:
  - exact IDs if present
  - `topic_index`
  - matching `headline` / `topic_name`
- Added DB fallback recovery when context resolution is insufficient:
  - depth-0 lookup by `chunk_index`
  - depth-0 lookup by `headline`
- Result: if clustering returns
  - `{"source_node":"MCP Server Package Config","topic_index":0,...}`
  the executor can still recover `C-L0-000` and wire the L1 node’s `children` correctly.

### 2. `from_depth=1` rebuilds now hydrate skipped lower-layer outputs

File: `src-tauri/src/pyramid/chain_executor.rs`

- When lower extract/compress/fuse steps are skipped because `from_depth > 0`, the executor now reloads their exact saved JSON into `ctx.step_outputs` instead of just `continue`-ing.
- This fixes the hidden rebuild bug where `thread_clustering` could run without `$l0_code_extract` actually being present in memory.
- Rehydrated outputs are decorated the same way as fresh outputs, so the clustering step sees the exact same shape during rebuilds as during a cold build.

### 3. Fresh, resumed, and upper-layer outputs now all use one consistent saved shape

File: `src-tauri/src/pyramid/chain_executor.rs`

- `execute_for_each`, `execute_single`, `dispatch_pair`, and `dispatch_group` now all:
  - validate step output
  - save decorated output
  - return decorated output
- Resume paths now decorate loaded outputs too, so old saved rows still work with the new child-resolution logic.

### 4. Child wiring now prefers authoritative sources end-to-end

File: `src-tauri/src/pyramid/chain_executor.rs`

- L1 child wiring now tries, in order:
  1. exact assignment IDs
  2. assignment recovery from `topic_index` / headline
  3. DB fallback by chunk index / headline
  4. only then LLM `source_nodes`
- If no authoritative children can be recovered, the existing debug log of the first assignment object remains in place.

### 5. Thread clustering can no longer quietly produce an empty upper tree

Files:

- `src-tauri/src/pyramid/chain_executor.rs`
- `chains/defaults/code.yaml`

- Added runtime validation for schema-backed outputs with `minItems`.
- Added `minItems: 1` to `thread_clustering.threads`.
- Added `minItems: 1` to each thread’s `assignments`.
- Added explicit empty-`threads` / empty-`clusters` rejection in the executor.
- Result: if clustering returns an empty structured response, the build fails instead of silently creating zero L1 nodes.

### 6. L0 extraction no longer silently skips failed files

File: `chains/defaults/code.yaml`

- Changed `l0_code_extract.on_error` from `skip` to `retry(3)`.
- Because exhausted `retry(n)` now aborts in the executor, a failed file extract causes a real build failure instead of a partial pyramid with missing source coverage.

### 7. Prompt/schema pressure on exact IDs is stronger now

Files:

- `chains/prompts/code/code_cluster.md`
- `chains/defaults/code.yaml`

- The clustering prompt now explicitly says:
  - each input topic includes exact `node_id` / `source_node`
  - `assignments[].source_node` must copy the exact `C-L0-XXX`
  - the human-readable title belongs in `topic_name`, not `source_node`
- The schema description for `source_node` now explicitly says "never put the headline text here."

### 8. L2/L3 naming guidance got one more push

Files:

- `chains/prompts/code/code_distill.md`
- `chains/prompts/code/code_recluster.md`

- Synthesis already had sibling-cluster context added in the previous pass.
- This pass also strengthened the prompt language so cluster names are compared side-by-side and renamed if they still read like overlapping "project overview" labels.

### 9. DB helpers added for exact resume + fallback lookup

File: `src-tauri/src/pyramid/db.rs`

- Added `get_step_output_exact(...)`
- Added `get_node_id_by_depth_and_chunk_index(...)`
- Added `get_node_id_by_depth_and_headline(...)`

These are used to:

- reload the exact saved step row during resume/hydration
- recover real L0 node IDs from clustering assignments that contain headlines

### 10. Build-progress node counts now track nodes instead of generic work items

Files:

- `src-tauri/src/pyramid/chain_executor.rs`
- `src/components/BuildProgress.tsx`

- Root cause: the chain runtime was filling `BuildProgress.total` from a generic work-item estimator, but the UI labeled it as node count.
- Biggest inflation source: `thread_narrative` was counted as one item per source chunk instead of one item per actual clustered thread.
- Fixed by:
  - switching progress estimation to node-oriented totals only for `save_as: node` steps
  - dynamically recomputing totals after step outputs become available
  - resolving `for_each` counts from actual refs like `$thread_clustering.threads` instead of defaulting to chunk count
  - updating the frontend copy to say `estimated nodes` while the build is running
- Result: predicted node totals should now stay close to actual pyramid node counts instead of overshooting badly on code builds.

## Verification run

Ran successfully:

- `cargo fmt --manifest-path src-tauri/Cargo.toml`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_executor::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml db::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_dispatch::tests -- --nocapture`
- `npm run build`

Status:

- `chain_executor::tests`: 8 passed
- `db::tests`: 11 passed
- `chain_dispatch::tests`: 12 passed
- frontend production build: passed

Known unrelated warnings during test runs:

- `src-tauri/src/pyramid/vine.rs`: `true_orphans` assigned but never read
- `src-tauri/src/pyramid/vine.rs`: unused variable `llm`

I did not change those warnings in this pass.

## What still needs live confirmation

I could not run the full networked LLM build loop from here, so the last remaining proof step is an actual pyramid rebuild in the app/runtime.

Recommended live check:

1. Run a rebuild from depth 1 on a code slug.
2. Confirm `thread_clustering` receives decorated L0 inputs that include `node_id` / `source_node`.
3. Confirm L1 nodes persist `children` as real L0 IDs like `C-L0-070`.
4. Confirm there is no silent empty-L1 outcome if clustering fails or returns empty.
5. Confirm L2 headlines are materially distinct, not three variations of the project name.

## Expected log signatures

Healthy child recovery:

- `[CHAIN] [thread_narrative] L1-000: using N authoritative child IDs`

If clustering still emits headlines, but recovery works:

- no failure, and saved `children` still contain real `C-L0-XXX` IDs

If something is still wrong:

- `[CHAIN] ... assignments present but no child IDs extracted; first_assignment=...`
- or a hard build failure from empty `threads` / missing skipped-step output

## SQL spot-check

```sql
SELECT id, depth, children
FROM pyramid_nodes
WHERE slug = ? AND depth = 1
ORDER BY id
LIMIT 5;
```

Expected:

- `children` contains real L0 IDs such as `["C-L0-000","C-L0-001"]`
- not headlines like `["MCP Server Package Config", ...]`

## Net result

This pass addresses both sides of the root cause:

- prevention: make the clustering model much more likely to emit exact IDs
- recovery: even if it still emits headlines, the executor now resolves them back to authoritative L0 IDs

It also closes the main silent-failure paths that were undermining reliability:

- skipped lower-layer rebuild starvation
- empty clustering outputs
- partial L0 extraction via `skip`
