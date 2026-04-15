# Rust Handoff: Largest-first ordering for `for_each` steps

## The Problem
`for_each` dispatches items in array order (directory walk). Large files sit at the end of the queue. With concurrency 12, a 100KB doc at position 125 doesn't start until slot opens after position 113+. It finishes last, holding up the entire layer for 5-10s while all other slots are idle.

## The Fix
New YAML field: `dispatch_order` on any `for_each` step.

```yaml
- name: l0_doc_extract
  for_each: $chunks
  dispatch_order: "largest_first"   # "largest_first" | "smallest_first" | "original" (default)
  concurrency: 12
```

### Implementation
In `execute_for_each`, after resolving and projecting items, before dispatching:

```rust
if let Some(order) = &step.dispatch_order {
    match order.as_str() {
        "largest_first" => {
            // Sort by serialized JSON length descending, preserving original indices
            items.sort_by(|a, b| {
                let a_len = serde_json::to_string(a).map(|s| s.len()).unwrap_or(0);
                let b_len = serde_json::to_string(b).map(|s| s.len()).unwrap_or(0);
                b_len.cmp(&a_len)
            });
        }
        "smallest_first" => {
            items.sort_by(|a, b| {
                let a_len = serde_json::to_string(a).map(|s| s.len()).unwrap_or(0);
                let b_len = serde_json::to_string(b).map(|s| s.len()).unwrap_or(0);
                a_len.cmp(&b_len)
            });
        }
        _ => {} // "original" or unknown: preserve array order
    }
}
```

The sort must preserve the mapping from sorted position back to original index for `node_id_pattern` generation. The node ID should still reflect the original chunk index (D-L0-042 is chunk 42 regardless of dispatch order), not the sorted position.

### Why `largest_first`
With a concurrency pool of 12:
- **Original order:** Small docs 1-125 churn fast, large docs 125-127 enter pool last, finish last. All 12 slots idle waiting on the 3 stragglers.
- **Largest first:** Large docs start immediately in 3 of 12 slots. Small docs fill the other 9 slots and keep cycling. By the time the large docs finish, most small docs are done too. Wall time ≈ max(single largest doc).

### New field on ChainStep
```rust
#[serde(default)]
pub dispatch_order: Option<String>,  // "largest_first" | "smallest_first" | "original"
```

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add `dispatch_order` to ChainStep
- `src-tauri/src/pyramid/chain_executor.rs` — sort items before dispatch loop in `execute_for_each`
