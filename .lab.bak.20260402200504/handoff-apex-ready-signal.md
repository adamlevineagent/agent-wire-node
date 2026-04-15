# Rust Handoff: `apex_ready` signal from recluster

## The Problem
The recursive cluster loop forces mechanical narrowing: 23 → 6 → 5 → 4 → apex. The 6→5→4 rounds are useless — the LLM is awkwardly removing one group per round to comply with "produce strictly fewer clusters" when the natural structure might already be at 6 domains.

## The Fix
Let the recluster LLM response include `"apex_ready": true` to signal "these nodes are the right top-level dimensions — synthesize them into the apex now."

### Recluster response schema addition

```json
{
  "apex_ready": true,
  "clusters": []
}
```

OR:

```json
{
  "apex_ready": false,
  "clusters": [
    {"name": "...", "description": "...", "node_ids": ["L1-000", "L1-003"]}
  ]
}
```

When `apex_ready` is true, `clusters` is ignored (can be empty). The executor skips clustering and goes straight to direct synthesis of all current nodes into the apex.

### Rust change

In `execute_recursive_cluster`, after the clustering LLM call returns, check:

```rust
if let Some(true) = cluster_result.get("apex_ready").and_then(|v| v.as_bool()) {
    info!("[CHAIN] [{}] LLM signaled apex_ready at depth {} with {} nodes",
        step.name, depth, current_nodes.len());
    // Jump to direct synthesis — same code path as the <= 4 branch
    // but triggered by LLM judgment, not a hardcoded threshold
}
```

This goes right before the existing cluster parsing. If `apex_ready` is set, skip the cluster→synthesize round and do direct apex synthesis with all current nodes.

### Prompt change (YAML-only, can do now)

Add to recluster prompts:

```
If the current nodes already represent the natural top-level dimensions of this knowledge
and further grouping would only reduce clarity, return `"apex_ready": true` with empty clusters.
Only do this when you genuinely believe these nodes ARE the right top-level structure.
```

### The ≤4 hardcoded threshold

The existing `if current_nodes.len() <= 4` direct synthesis check should stay as a safety floor — if we're down to 4 or fewer, always go to apex regardless of what the LLM says. The `apex_ready` signal just allows the LLM to trigger this earlier.

## Files
- `src-tauri/src/pyramid/chain_executor.rs` — `execute_recursive_cluster`: check `apex_ready` flag after cluster LLM call
- `chains/prompts/document/doc_recluster.md` — add apex_ready instruction
- `chains/prompts/code/code_recluster.md` — add apex_ready instruction
- `chains/prompts/conversation/conv_recluster.md` — add apex_ready instruction
- Cluster response schemas in all chain YAMLs — add `apex_ready: boolean` field
