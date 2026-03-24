# Handoff: Pyramid Upper Layer Build Bug

## The Symptom

Pyramid builds routinely fail to generate L4/L5 apex layers on the first attempt. They appear to succeed on retry 2-3, but the underlying cause is not transient LLM failure — it's a logic bug in the resume path.

**User experience:** "I usually have to restart a build 3-5 times to get through the apex layers. Long ones routinely error after ~400 calls. But it always works after 2-3 retries."

**OpenRouter logs show:** Zero new API calls on retry. The LLM is never reached. The build finishes instantly with the same error.

## The Root Cause

`build_upper_layers()` in `build.rs` (line ~2020) has an expected-count check that assumes positional pairing at every depth:

```rust
depth += 1;
let expected = (current_nodes.len() + 1) / 2;
let existing = count_nodes_at_depth(conn, slug, depth)?;
if existing >= expected {
    // "already complete", skip this depth
    continue;
}
```

**The problem:** L2 nodes come from **semantic thread clustering** (via `THREAD_CLUSTER_PROMPT`), NOT positional pairing. Thread clustering typically produces fewer nodes than `(L1_count + 1) / 2`.

Example from a real build (bunch-002, 84 chunks):
```
depth 2: L1_count=42, expected=21, actual_L2=12  → MISMATCH (12 < 21)
depth 3: L2_count=12, expected=6,  actual_L3=6   → OK
depth 4: L3_count=6,  expected=3,  actual_L4=2   → MISMATCH (2 < 3)
depth 5: L4_count=2,  expected=1,  actual_L5=0   → Would build, but never reached
```

When the build re-runs:
1. Forward/reverse/combine: all `step_exists` → skip (fast, correct)
2. L1 pairing: all `step_exists` → skip (fast, correct)
3. Thread clustering: `thread_cluster` step exists → skip (correct)
4. **L2 check:** 12 existing < 21 expected → tries to build more L2 nodes
5. But there's nothing to build — the thread clustering already ran and produced 12
6. The build enters the pair-and-distill loop for depth 2, but `step_exists` is true for all 12 existing nodes
7. `pair_idx` advances to 12, `i` advances to 24, which exceeds `current_nodes.len()` (42 L1 nodes at this point, but pairing produces 21 pairs, only 12 of which exist)
8. The loop tries to build nodes 12-20 from L1 pairs 12-20, but these are **thread narratives that don't pair** — they were created by semantic clustering, not pairing

The result depends on exact state: sometimes it silently produces garbage nodes, sometimes it errors, sometimes it appears to work because enough retry cycles push through. The L4 mismatch (2 < 3) has the same issue — thread clustering produced 2 nodes at L4, but the carry-up logic expected 3.

## Why Retries Appear to Work

When the user manually triggers a rebuild via the UI:
1. The UI calls `clear-above` which deletes L2+ nodes and steps
2. Fresh build from L1 runs thread clustering again
3. If the LLM returns slightly different clusters, the count may happen to match the expected count
4. Or the fresh build doesn't hit the stale-count check because there are zero existing nodes

The "2-3 retries" success is actually "delete everything above L1, rebuild from scratch, and get lucky on the count alignment."

## The Evidence

### Database state for test-vine--bunch-002:
```
depth 2: prev_count=42 expected=21 existing=12 skip=NO ← BUG: tries to build 9 more L2 nodes
depth 3: prev_count=12 expected=6  existing=6  skip=YES
depth 4: prev_count=6  expected=3  existing=2  skip=NO ← BUG: tries to build 1 more L4 node
depth 5: prev_count=2  expected=1  existing=0  skip=NO ← Never reached
```

### Pipeline steps saved:
```
forward: 84, reverse: 84, combine: 84, synth: 60, thread_cluster: 1
```

### Node depths:
```
L0: 84, L1: 42, L2: 12, L3: 6, L4: 2, L5: 0 (never built)
```

### OpenRouter logs:
Zero new calls on retry — confirms the LLM is never reached. The build fails in Rust logic before making any API call.

## Where to Look

### Primary file: `src-tauri/src/pyramid/build.rs`

**`build_upper_layers()`** (~line 1995-2090):
- Line 2022: `let expected = (current_nodes.len() + 1) / 2;` — this assumes positional pairing at ALL depths, but depth 2 uses semantic clustering
- Line 2024: `if existing >= expected` — the skip check that breaks on count mismatch

**`build_threads_layer()`** (~line 1660-1990):
- This produces L2 nodes via `THREAD_CLUSTER_PROMPT` + `THREAD_NARRATIVE_PROMPT`
- The L2 count is determined by the LLM's clustering decision, not by `L1_count / 2`

**`build_conversation()`** (~line 612-870):
- Line 866: calls `build_upper_layers(db, writer_tx, llm_config, slug, 2, ...)` — starts upper pairing FROM depth 2
- The `start_depth=2` means it re-evaluates depth 2 nodes even though they came from thread clustering

### Key question: What does `build_upper_layers` actually do when `existing < expected` at depth 2?

The inner loop (lines 2033-2089) iterates L1 nodes in pairs and tries to create L2 nodes. But the existing L2 nodes were created by thread clustering with different IDs (they're thread narrative nodes, not pair synthesis nodes). The `step_exists` check uses node IDs like `L2-000`, `L2-001` etc. — if the thread clustering used those same IDs, the steps exist and the loop skips them. If the IDs don't match (e.g., thread clustering uses `T2-000` format), the loop tries to create new nodes alongside the existing ones.

**Check the node ID format:** Do thread-clustered L2 nodes use the same `L{depth}-{idx:03}` format as pair-synthesis nodes?

## Proposed Fix Options

### Option A: Skip expected-count check for threaded depths
If `thread_cluster` step exists for a slug, the L2 nodes came from clustering. Skip the `expected` check for depth 2 — just use whatever exists. The upper layers (L3+) can still use positional pairing since they're built from L2 nodes, not from threads.

```rust
// In build_upper_layers, before the expected check:
let thread_clustered = step_exists(conn, slug, "thread_cluster", -1, -1, "")?;
if thread_clustered && depth == start_depth {
    // L2 was built by thread clustering, not pairing. Skip count validation.
    continue;
}
```

### Option B: Use actual node count as expected
Replace `(current_nodes.len() + 1) / 2` with a query that checks what the PREVIOUS build step actually produced, rather than predicting from the parent count.

### Option C: Track expected counts during build
When `build_threads_layer` finishes, store the actual L2 count somewhere (e.g., in a pipeline step). `build_upper_layers` reads this instead of computing from parent count.

### Recommendation
**Option A** is simplest and most targeted. The only depth where the count mismatch occurs is depth 2 (thread clustering). All other depths use positional pairing where `expected = (n+1)/2` is correct.

## How to Verify the Fix

1. Build a pyramid with 40+ chunks (any conversation JSONL with 4000+ lines)
2. Let it complete through L1
3. Check: does L2 have fewer nodes than `L1_count / 2`?
4. Let it continue to L3+: does it reach the apex without error?
5. Delete L3+ and rebuild: does it still work on the first try?

## Related Context

- The vine conversation system triggers this bug because it builds bunch pyramids programmatically and retries automatically. The retry doesn't help because the logic bug persists across retries.
- The existing UI workflow works around this because manual "Rebuild" clears L2+ before retrying, giving thread clustering a fresh start.
- The pyramid knowledge base at slug `agent-wire-nodecanonical` has annotations from auditors documenting this pattern (annotations #127-#165 on various nodes).

## Files

| File | Lines | What to look at |
|------|-------|----------------|
| `src-tauri/src/pyramid/build.rs` | 2020-2090 | `build_upper_layers()` expected-count logic |
| `src-tauri/src/pyramid/build.rs` | 1660-1990 | `build_threads_layer()` L2 creation |
| `src-tauri/src/pyramid/build.rs` | 612-870 | `build_conversation()` orchestration |
| `src-tauri/src/pyramid/db.rs` | ~780 | `get_nodes_at_depth()` query |
| `src-tauri/src/pyramid/db.rs` | ~800 | `count_nodes_at_depth()` query |
