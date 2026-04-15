# Rust Handoff: Proportional batch splitting

## Status
**IMPLEMENTED** — compiled, built into Wire Node v0.2.0. This is now the default behavior when `batch_size` is set without `batch_max_tokens`. See `handoff-batch-size-for-each.md` for unified docs.

## Summary
Change `batch_size` semantics from "max items per chunk" to "target items per chunk with proportional distribution."

## Current behavior
```
127 items, batch_size: 100 → [100, 27]  (lopsided)
```

## Desired behavior
```
127 items, batch_size: 100 → [64, 63]  (balanced)
```

The logic: `num_batches = ceil(items.len() / batch_size)`, then split items evenly across that many batches.

## The Change

In `chain_executor.rs`, replace:
```rust
items.chunks(bs)
    .map(|chunk| Value::Array(chunk.to_vec()))
    .collect::<Vec<Value>>()
```

With:
```rust
let num_batches = (items.len() + bs - 1) / bs;  // ceil division
let base_size = items.len() / num_batches;
let remainder = items.len() % num_batches;
let mut result = Vec::with_capacity(num_batches);
let mut offset = 0;
for i in 0..num_batches {
    let size = base_size + if i < remainder { 1 } else { 0 };
    result.push(Value::Array(items[offset..offset + size].to_vec()));
    offset += size;
}
result
```

First `remainder` batches get `base_size + 1`, the rest get `base_size`. All batches within 1 item of each other.

## Why
Lopsided batches produce lopsided clustering results. A 100-doc batch produces rich, well-structured threads. A 27-doc batch produces thin, incomplete threads. The merge step then has to reconcile fundamentally different quality levels. Balanced batches = balanced clustering = cleaner merge.

## Files
- `src-tauri/src/pyramid/chain_executor.rs` — the chunking logic in `execute_for_each`
