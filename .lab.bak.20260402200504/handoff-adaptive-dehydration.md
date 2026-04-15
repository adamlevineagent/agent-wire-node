# Rust Handoff: Adaptive per-item dehydration

## Summary
Per-item token-aware field stripping. Small items stay fully hydrated. Large items get progressively dehydrated. Items in the same batch can have different hydration levels. The YAML defines the dehydration cascade — what to strip and in what order.

## The Problem
Currently `item_fields` is uniform — every item gets the same projection. This forces a choice: project aggressively (lose signal from small items that could afford full detail) or keep everything (large items blow the context). We need both: rich data for small items, compact data for large items, in the same batch.

## New YAML field: `dehydrate`

Replaces `item_fields` for steps that need adaptive projection. Defines an ordered cascade of fields to drop when an item is too large for the batch budget.

```yaml
dehydrate:
  - drop: "topics.current"      # first cut: strip full topic text, keep summary
  - drop: "topics.entities"     # second cut: strip entity lists
  - drop: "topics.summary"      # third cut: strip summaries, keep just topic names
  - drop: "topics"              # fourth cut: strip all topic data
  - drop: "orientation"         # last resort: just node_id + headline
```

## How it works

In `execute_for_each`, after resolving items and before batching:

### Step 1: Measure each item fully hydrated
```rust
for item in &items {
    item.estimated_tokens = estimate_tokens(&item);
}
```

### Step 2: Build batches with adaptive dehydration
```rust
fn build_adaptive_batch(
    items: &mut [Item],
    max_tokens: usize,
    dehydrate_cascade: &[DehydrateStep],
) -> Vec<Value> {
    let mut batch = Vec::new();
    let mut batch_tokens = 0;

    for item in items {
        let mut item_value = item.full_value.clone();
        let mut item_tokens = item.estimated_tokens;

        // Try to fit the item, dehydrating progressively if needed
        for step in dehydrate_cascade {
            if batch_tokens + item_tokens <= max_tokens {
                break; // fits at current hydration level
            }
            // Apply this dehydration step
            item_value = drop_field(&item_value, &step.field);
            item_tokens = estimate_tokens(&item_value);
        }

        // If still doesn't fit after full dehydration, start a new batch
        if batch_tokens + item_tokens > max_tokens && !batch.is_empty() {
            // Close current batch, start new one
            yield_batch(batch);
            batch = Vec::new();
            batch_tokens = 0;
        }

        batch_tokens += item_tokens;
        batch.push(item_value);
    }

    yield_batch(batch);
}
```

### Step 3: Each item carries its hydration level
The dehydrated item goes to the LLM as-is. Small items have full `topics.current`, large items have only `topics.summary` or just `topics.name`. The LLM prompt handles mixed hydration (already updated).

## Field dropping with dot-notation

`drop: "topics.current"` means: for each element in the `topics` array, remove the `current` field. The `topics` array stays, each topic keeps its other fields (`name`, `summary`, `entities`, etc.), only `current` is removed.

```rust
fn drop_field(value: &Value, field_path: &str) -> Value {
    if let Some((parent, child)) = field_path.split_once('.') {
        // Dot-notation: drop child field from each element in parent array
        let mut v = value.clone();
        if let Some(Value::Array(arr)) = v.get_mut(parent) {
            for item in arr.iter_mut() {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove(child);
                }
            }
        }
        v
    } else {
        // Top-level field: remove it
        let mut v = value.clone();
        if let Some(obj) = v.as_object_mut() {
            obj.remove(field_path);
        }
        v
    }
}
```

## Interaction with other primitives

- `dehydrate` replaces `item_fields` on the step — they're mutually exclusive. `item_fields` is uniform projection, `dehydrate` is adaptive.
- `batch_max_tokens` still controls the batch token budget. `dehydrate` works within that budget.
- `batch_size` still caps item count per batch.
- `dispatch_order: "largest_first"` composes well — large items get dehydrated first, small items fill remaining space fully hydrated.

## New fields on ChainStep

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DehydrateStep {
    pub drop: String,  // field path to drop (supports dot-notation)
}

// On ChainStep:
#[serde(default)]
pub dehydrate: Option<Vec<DehydrateStep>>,
```

## The `topics.summary` field

L0 extraction prompts now produce `topics.summary` — a 10-15 word distillation per topic alongside the full `current` text. This gives the dehydration cascade a high-signal middle step between full `current` (50+ words) and just `name` (2-6 words).

Cost: ~20 extra tokens per topic at extraction time. Payoff: the clustering step can dehydrate large docs from `current` to `summary` instead of jumping straight to `name`, preserving much more signal for grouping.

## YAML usage

```yaml
- name: batch_cluster
  primitive: classify
  instruction: "$prompts/document/doc_cluster.md"
  for_each: $l0_doc_extract
  dehydrate:
    - drop: "topics.current"
    - drop: "topics.entities"
    - drop: "topics.summary"
    - drop: "topics"
    - drop: "orientation"
  batch_size: 150
  batch_max_tokens: 80000
  concurrency: 3
```

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add `DehydrateStep` struct and `dehydrate` field on ChainStep
- `src-tauri/src/pyramid/chain_executor.rs` — adaptive dehydration logic in `execute_for_each`, `drop_field()` function with dot-notation support
