# Handoff: Layer-by-Layer Pyramid Rebuild

## Problem
Every experiment rebuilds ALL layers from scratch — including L0 extraction (112 files × mercury-2 = ~5 minutes). But L0 nodes are stable between prompt experiments that only change L1+ prompts (thread clustering, thread synthesis, distill, recluster). Rebuilding L0 wastes time and credits.

## What We Need
A way to rebuild from a specific depth upward, reusing existing nodes below that depth.

## Proposed API

### Option A: Query parameter on build endpoint
```
POST /pyramid/{slug}/build?from_depth=1
```
- Keeps all nodes at depth < `from_depth`
- Deletes all nodes at depth >= `from_depth`
- Deletes corresponding `pyramid_pipeline_steps` entries for steps that produce depth >= `from_depth`
- Runs the chain from the first step that produces output at `from_depth` or higher
- The chain executor needs to know which steps to skip vs run

### Option B: Separate rebuild endpoint
```
POST /pyramid/{slug}/rebuild
Body: { "from_depth": 1 }
```
Same behavior, cleaner separation.

## What Needs to Change

### 1. `chain_executor.rs` — Skip completed steps

The executor's main loop iterates over `steps` and checks `pyramid_pipeline_steps` for resume state. We need it to:
1. Accept a `from_depth` parameter
2. Before executing, delete all nodes at `depth >= from_depth` and their pipeline_step entries
3. Skip steps whose `depth` field is below `from_depth` (they already have valid output)
4. Run steps at `from_depth` and above normally

Key consideration: Steps that **read** from lower depths (e.g., thread_clustering reads L0 topics) need those L0 nodes to exist. Since we're keeping nodes below `from_depth`, this should work automatically.

### 2. `routes.rs` — Accept from_depth parameter

In `handle_build()`:
```rust
// Parse optional from_depth from query params or body
let from_depth = body.from_depth.unwrap_or(0); // 0 = full rebuild

if from_depth > 0 {
    // Delete nodes at depth >= from_depth
    conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ? AND depth >= ?",
        params![slug, from_depth]
    )?;
    // Delete pipeline steps for those depths
    conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ? AND step_name IN (
            SELECT step_name FROM pyramid_pipeline_steps
            WHERE slug = ? AND depth >= ?
        )",
        params![slug, slug, from_depth]
    )?;
}
```

### 3. Run script update

```bash
# Rebuild from L1 only (keep L0 nodes)
curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST "$BASE/$SLUG/build" \
  -d '{"from_depth": 1}'
```

## Step-to-Depth Mapping (code.yaml)

| Step | Depth | Skip if from_depth=1? |
|------|-------|----------------------|
| l0_code_extract | 0 | YES — keep existing L0 |
| thread_clustering | 1 | NO — needs to re-run |
| thread_narrative | 1 | NO — produces L1 nodes |
| upper_layer_synthesis | 1+ | NO — produces L2+ nodes |

## Time Savings

| Build type | Duration | Cost |
|-----------|----------|------|
| Full rebuild (L0→apex) | ~12 min | 112 LLM calls for L0 + ~20 for L1+ |
| From L1 (skip L0) | ~3 min | ~20 LLM calls |

**4x faster iteration for prompt experiments that don't change L0 extraction.**

## Testing

After implementation:
1. Full build: `bash .lab/run-experiment.sh opt-full`
2. Modify thread_clustering prompt
3. Rebuild from L1: `curl ... -d '{"from_depth": 1}'`
4. Verify L0 nodes unchanged, L1+ rebuilt
5. Compare node counts and apex quality

## Files to Modify

| File | Change |
|------|--------|
| `src-tauri/src/pyramid/routes.rs` | Accept `from_depth` in build request body |
| `src-tauri/src/pyramid/chain_executor.rs` | Skip steps below `from_depth`, clean up nodes above |
| `.lab/run-experiment.sh` | Add `--from-depth` flag for quick iteration |
