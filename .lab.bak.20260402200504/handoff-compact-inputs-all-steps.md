# Rust Handoff: `compact_inputs` for all step types

## Summary
`compact_inputs: true` currently only works for web steps and IR steps. It should work for ALL step types — especially `for_each` classify steps where batched clustering sends full L0 extractions when it only needs headlines.

## The Problem
`thread_clustering_batch` does `for_each: $l0_doc_extract` with `batch_size: 100`. Each batch sends 100 full L0 extractions — headline, orientation, all topics (with full `current` text), all entities, corrections, decisions. That's ~50-100K tokens per batch.

The clustering prompt only needs `node_id` + `headline` + `orientation` to group documents. That's ~200 tokens per doc × 100 = ~20K tokens. A 5x reduction.

## The Fix
In `execute_for_each` (chain_executor.rs), after resolving the items array and before dispatching to LLM calls, apply the same compaction logic that `build_webbing_input` uses when `step.compact_inputs` is true.

For each item in the batch/for_each array, strip it down to:
- `node_id` / `source_node`
- `headline`
- `orientation` (truncated to ~200 chars)
- `entities` (top 16)

The compaction function `compact_ir_inventory_payload` already exists and does roughly this for IR steps. Apply it to the resolved items in `execute_for_each` and `execute_single` when `compact_inputs` is true.

## YAML usage
```yaml
- name: thread_clustering_batch
  primitive: classify
  for_each: $l0_doc_extract
  batch_size: 100
  compact_inputs: true    # <-- this now works
  concurrency: 3
```

## Files
- `src-tauri/src/pyramid/chain_executor.rs` — `execute_for_each` and `execute_single`: apply compaction to resolved input when `compact_inputs` is true
