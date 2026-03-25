# Audit Handback to Tester — Cross-Layer Webbing Pass

## Date

2026-03-24

## Goal of this pass

Implement the first real cross-layer webbing pass for the chain-engine code pyramid so sibling nodes at the same depth can be linked by semantic "see also" edges.

This pass also closes the runtime gaps that would have prevented webbing from working reliably in the current chain-engine build:

- add a first-class `web` primitive to the chain runtime
- persist web edges into `pyramid_web_edges`
- support both L1 and L2 webbing in the code pipeline
- make node-ID extraction and thread-target resolution robust for chain-engine IDs like `L1-000`

## What changed

### 1. Added a real `web` primitive to the chain engine

Files:

- `src-tauri/src/pyramid/chain_engine.rs`
- `src-tauri/src/pyramid/chain_executor.rs`

- Registered `web` as a valid primitive in chain validation.
- Added executor support for `primitive: web`.
- The executor now:
  - resolves the step input
  - gathers the sibling nodes for the target depth
  - builds a structured LLM payload containing:
    - `node_id`
    - `headline`
    - `orientation`
    - `topics`
    - deduped entity lists
  - parses the returned `edges`
  - normalizes source/target endpoints
  - persists the result into `pyramid_web_edges`

### 2. Web-edge parsing is resilient to headline-vs-ID drift

File:

- `src-tauri/src/pyramid/chain_executor.rs`

- The parser accepts edge endpoints as either:
  - exact node IDs like `L1-000`
  - unique sibling headlines like `Build Engine`
- Duplicate A↔B / B↔A edges are deduped to a single normalized pair.
- Self-edges are discarded.
- `shared_resources` are folded into the saved relationship text so the DB/UI keep the concrete detail even though `pyramid_web_edges` only stores one relationship field.

### 3. Depth-scoped web-edge refresh is now explicit

File:

- `src-tauri/src/pyramid/db.rs`

- Added `delete_web_edges_for_depth(...)`.
- Each successful webbing pass now replaces only the edges whose two endpoints both live at that depth.
- Result:
  - L1 webbing refreshes L1↔L1 edges without touching L2↔L2 edges
  - L2 webbing refreshes L2↔L2 edges without touching L1↔L1 edges

### 4. Thread-target resolution now works for chain-engine L1 IDs

File:

- `src-tauri/src/pyramid/stale_helpers_upper.rs`

- The self-heal path previously only auto-created depth-1 thread targets for legacy IDs like `C-L1-*`.
- Chain-engine builds use `L1-*`.
- Updated the self-heal check so both `C-L1-*` and `L1-*` can become thread targets when missing.
- This matters because `pyramid_web_edges` stores thread IDs, not raw node IDs.

### 5. Node-ID recognition was widened to support normal chain-engine IDs

File:

- `src-tauri/src/pyramid/chain_executor.rs`

- The generic node-ID matcher previously favored IDs containing `-L`, which worked for `C-L0-000` but missed ordinary IDs like `L1-000` and `L2-001`.
- It now accepts both styles.
- This was necessary for:
  - web-step input node extraction
  - web-edge endpoint normalization
  - general chain-engine compatibility with upper-layer IDs

### 6. Added L1 and L2 webbing steps to the code pipeline

File:

- `chains/defaults/code.yaml`

Added:

- `l1_webbing`
  - `primitive: web`
  - input nodes from `$thread_narrative`
  - `depth: 1`
  - `save_as: web_edges`
  - `on_error: skip`

- `l2_webbing`
  - `primitive: web`
  - `depth: 2`
  - `save_as: web_edges`
  - `on_error: skip`

Both steps now have structured response schemas requiring:

- `source`
- `target`
- `relationship`
- `shared_resources`
- `strength`

### 7. Strengthened the webbing prompt to force exact node IDs

File:

- `chains/prompts/code/code_web.md`

The prompt now explicitly says:

- `source` and `target` must be the exact `node_id` strings from the provided node list
- do not emit both A→B and B→A
- do not emit self-edges

This is the same reliability pattern used in the children-wiring fixes: pressure the prompt harder, but keep the Rust side robust anyway.

### 8. Added regression tests for the new behavior

Files:

- `src-tauri/src/pyramid/chain_executor.rs`
- `src-tauri/src/pyramid/db.rs`

Added tests covering:

- extraction of explicit web-step node IDs without accidentally pulling child `source_nodes`
- edge parsing from sibling headlines back to normalized node IDs
- pair deduplication
- depth-scoped edge deletion in the DB helper

## Verification run

Ran successfully:

- `cargo fmt --manifest-path src-tauri/Cargo.toml`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_executor::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml db::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml webbing::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_engine::tests -- --nocapture`
- `cargo test --manifest-path src-tauri/Cargo.toml chain_loader::tests -- --nocapture`

Status:

- `chain_executor::tests`: 10 passed
- `db::tests`: 12 passed
- `webbing::tests`: 3 passed
- `chain_engine::tests`: 10 passed
- `chain_loader::tests`: 0 tests, command passed

Known unrelated warnings during test runs:

- `src-tauri/src/pyramid/vine.rs`: `true_orphans` assigned but never read
- `src-tauri/src/pyramid/vine.rs`: unused variable `llm`

I did not change those warnings in this pass.

## What still needs live confirmation

I could not run the networked LLM pyramid build from here, so the remaining proof is a real build in the app/runtime.

Recommended live check:

1. Let the current rebuild finish and confirm `l1_webbing` executes after `thread_narrative`.
2. Inspect `pyramid_web_edges` and confirm L1 edges exist between real sibling threads.
3. Confirm `l2_webbing` runs after upper-layer synthesis and writes only depth-2 peer edges.
4. In the UI, verify the tree still drills normally and that same-depth "see also" lines now appear between related siblings.
5. Spot-check that saved web-edge relationships mention concrete shared resources rather than generic "both are part of the system" phrasing.

## SQL spot-checks

```sql
SELECT thread_a_id, thread_b_id, relationship, relevance
FROM pyramid_web_edges
WHERE slug = ?
ORDER BY relevance DESC
LIMIT 20;
```

Expected:

- real sibling thread IDs like `L1-000`, `L1-003`, `L2-000`, `L2-001`
- relationship text mentioning concrete resources, functions, endpoints, tables, IPC channels, or types

To verify depth-scoped refresh:

```sql
SELECT pt_a.depth AS depth_a,
       pt_b.depth AS depth_b,
       pwe.thread_a_id,
       pwe.thread_b_id,
       pwe.relationship
FROM pyramid_web_edges pwe
JOIN pyramid_threads pt_a
  ON pt_a.slug = pwe.slug AND pt_a.thread_id = pwe.thread_a_id
JOIN pyramid_threads pt_b
  ON pt_b.slug = pwe.slug AND pt_b.thread_id = pwe.thread_b_id
WHERE pwe.slug = ?
ORDER BY pt_a.depth, pwe.relevance DESC;
```

Expected:

- L1 webbing rows have both endpoints at depth 1
- L2 webbing rows have both endpoints at depth 2
- no cross-depth rows from this pass

## Net result

This pass moves cross-layer webbing from design-doc status into the actual chain-engine runtime:

- the code pipeline can now ask for webbing explicitly
- the executor can run it and persist the result
- the DB refreshes only the intended layer
- chain-engine IDs like `L1-000` / `L2-001` are now handled correctly throughout the path

The remaining confirmation is live-build behavior and UI rendering, not missing Rust-side implementation.
