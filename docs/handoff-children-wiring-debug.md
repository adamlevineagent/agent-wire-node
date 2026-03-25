# Children Wiring Debug — Handoff

## Date: 2026-03-24

## The Problem

L1 (thread narrative) nodes save with empty `children` arrays instead of L0 node IDs. The pyramid visualization shows L1 as leaf nodes with no drill-down.

## What Was Tried

### Original state (before my changes)

The `chain_executor.rs` forEach loop at ~line 808 had logic that checked whether `build_node_from_output()` returned valid children (IDs containing `-L`). If invalid (headlines), it tried extracting from `item.get("assignments")`.

On a previous build, this DID work for some nodes — logs showed `"overriding 0 LLM children with 13 assignment IDs"`. But it only fired when `has_valid_children` was false. When the LLM returned strings containing `-L` (even wrong ones), the override was skipped.

### My change

I changed the logic to **always prefer assignment IDs over LLM source_nodes** regardless of whether LLM children look valid. The new code at ~line 808-855 in `chain_executor.rs`:

1. First checks `item.get("assignments")` — extracts `source_node` from each
2. If no assignments, checks `item.get("node_ids")`
3. Only falls back to LLM children if neither authoritative source exists

### Current symptom

The logs now show: `"no authoritative children and LLM children invalid"` — meaning BOTH extraction paths found nothing. The `item.get("assignments")` returns `None` or an empty array.

## What I Don't Know

- Whether the `item` variable at this point in execution actually contains the thread object with `assignments`, or whether it's been transformed/flattened by the reference resolver
- Whether my restructuring of the if/else blocks changed which variable `item` points to
- Whether the previous build that successfully extracted was using a different code path (the old `!has_valid_children` gate meant this code only ran for some nodes, so the successful extractions might have been from a different branch)

## Key Context

- The forEach iterates over `$thread_clustering.threads` — each item should be a thread object from the clustering step
- The thread clustering LLM returns objects with `name`, `description`, and `assignments[]`
- Each assignment has `source_node` (L0 ID), `topic_index`, and `topic_name`
- The `build_node_from_output()` function at `chain_dispatch.rs:401-409` reads `output.get("source_nodes")` for children — this is from the LLM's synthesis response, NOT from the clustering assignments
- The clustering assignments are the authoritative source because they came from the step that decided which L0 nodes belong to which thread

## Files

- `src-tauri/src/pyramid/chain_executor.rs` — lines ~803-860 (the forEach node save block)
- `src-tauri/src/pyramid/chain_dispatch.rs` — lines 262-409 (`build_node_from_output`)
- `src-tauri/src/pyramid/chain_resolve.rs` — reference resolver (transforms `$ref` expressions into values)
- `chains/defaults/code.yaml` — the chain definition, specifically the `l1_code_group_synthesis` and `thread_narratives` steps

## Suggested Debug Approach

Log the actual contents of `item` right before the assignment extraction:

```rust
info!("[CHAIN] [{}] {node_id}: item keys: {:?}, has assignments: {}",
    step.name,
    item.as_object().map(|o| o.keys().collect::<Vec<_>>()),
    item.get("assignments").is_some()
);
```

This will show whether `assignments` exists on the item at all, and what keys the item actually has.
