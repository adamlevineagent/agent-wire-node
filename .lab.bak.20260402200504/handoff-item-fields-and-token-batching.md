# Rust Handoff: `item_fields` + `batch_max_tokens`

## Status
**IMPLEMENTED** — compiled, built into Wire Node v0.2.0. See `handoff-batch-size-for-each.md` for the unified documentation of all three batching/projection primitives.

**Supersedes:** `handoff-compact-inputs-all-steps.md`

## Summary
Two composable primitives that give YAML full control over what data travels to the LLM and how batches are sized.

## 1. `item_fields` — field-level hydration

New field on `ChainStep`:
```rust
#[serde(default)]
pub item_fields: Option<Vec<String>>,
```

When set, each item in a `for_each` (or each element in a batched array, or the input to a `single` step) is projected down to only the listed fields before being sent to the LLM.

### Behavior

Given an L0 extraction item:
```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "orientation": "This document covers the token rotation...",
  "topics": [
    {
      "name": "Token Rotation",
      "current": "The system uses refresh tokens with 24h expiry...",
      "entities": ["system: Supabase Auth", "decision: 24h expiry"],
      "corrections": [...],
      "decisions": [...]
    }
  ]
}
```

With `item_fields: ["node_id", "headline", "orientation"]`:
```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "orientation": "This document covers the token rotation..."
}
```

### Nested field paths (stretch goal)

Support dot-notation for partial hydration of nested objects:

`item_fields: ["node_id", "headline", "topics.name", "topics.entities"]`

```json
{
  "node_id": "D-L0-042",
  "headline": "Auth Token Design",
  "topics": [
    {
      "name": "Token Rotation",
      "entities": ["system: Supabase Auth", "decision: 24h expiry"]
    }
  ]
}
```

If nested paths are complex to implement in v1, flat top-level fields are sufficient — we can add dot-notation later.

### Where it applies

Apply the projection in these execution paths:
- `execute_for_each` — project each item (or each item within a batch) before sending
- `execute_single` — project each element in arrays within the resolved input
- Does NOT apply to `execute_web_step` (webbing has its own compaction via `build_webbing_input`)

### Implementation

```rust
fn project_item(item: &Value, fields: &[String]) -> Value {
    let Some(obj) = item.as_object() else { return item.clone() };
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = obj.get(field.as_str()) {
            projected.insert(field.clone(), value.clone());
        }
    }
    Value::Object(projected)
}
```

Apply this after resolving `$item` but before building the system prompt / dispatching to LLM.

## 2. `batch_max_tokens` — token-aware batch sizing

New field on `ChainStep`:
```rust
#[serde(default)]
pub batch_max_tokens: Option<usize>,
```

When set alongside `batch_size` (or alone), the executor fills each batch greedily until EITHER limit is hit.

### Behavior

```
Items: [a(500tok), b(800tok), c(300tok), d(1200tok), e(400tok)]
batch_size: 100, batch_max_tokens: 1500

Batch 1: [a, b, c] = 1600 tokens → c pushes over, but a+b = 1300 fits, add c = 1600 > 1500
```

Algorithm: accumulate items into the current batch. Before adding an item, check if `running_total + item_tokens > batch_max_tokens`. If yes, close the current batch and start a new one. Exception: if the current batch is empty, always add the item (a single item that exceeds the token limit gets its own batch rather than being dropped).

Token estimation: `serde_json::to_string(&item).len() / 4` — same heuristic used in `build.rs:2290`.

**Important:** `item_fields` projection happens BEFORE token estimation. You measure the projected item size, not the full item size. This means the two primitives compose correctly — compact items → measure tokens → fill batches.

### Proportional distribution

When ONLY `batch_size` is set (no `batch_max_tokens`), use proportional splitting as specified in `handoff-proportional-batch-split.md`.

When `batch_max_tokens` is set, use greedy filling instead — proportional splitting doesn't apply because items may have different sizes.

When both are set, greedy fill respects both limits: close the batch when either `batch_max_tokens` or `batch_size` would be exceeded.

### Implementation sketch

```rust
fn batch_items_by_tokens(
    items: Vec<Value>,
    max_tokens: usize,
    max_items: Option<usize>,
) -> Vec<Value> {
    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_tokens = 0usize;

    for item in items {
        let item_tokens = serde_json::to_string(&item)
            .map(|s| s.len() / 4)
            .unwrap_or(0);

        let would_exceed_tokens = current_tokens + item_tokens > max_tokens && !current_batch.is_empty();
        let would_exceed_items = max_items.map_or(false, |max| current_batch.len() >= max);

        if would_exceed_tokens || would_exceed_items {
            batches.push(Value::Array(current_batch));
            current_batch = Vec::new();
            current_tokens = 0;
        }

        current_tokens += item_tokens;
        current_batch.push(item);
    }

    if !current_batch.is_empty() {
        batches.push(Value::Array(current_batch));
    }

    batches
}
```

## YAML Usage

```yaml
# Clustering: lightweight projection, token-aware batching
- name: thread_clustering_batch
  primitive: classify
  instruction: "$prompts/document/doc_cluster.md"
  for_each: $l0_doc_extract
  item_fields: ["node_id", "headline", "orientation"]
  batch_size: 150
  batch_max_tokens: 80000
  concurrency: 3
  model_tier: mid

# Thread synthesis: full items, no batching
- name: thread_narrative
  primitive: synthesize
  instruction: "$prompts/document/doc_thread.md"
  for_each: $thread_clustering.threads
  # no item_fields = full hydration
  concurrency: 5
  model_tier: mid

# Webbing: just headlines + entities for cross-referencing
- name: l0_webbing
  primitive: web
  instruction: "$prompts/document/doc_web.md"
  item_fields: ["node_id", "headline", "entities"]
  compact_inputs: true  # webbing still uses its own path
  model_tier: mid
```

## Fields on ChainStep

```rust
pub item_fields: Option<Vec<String>>,     // field projection
pub batch_max_tokens: Option<usize>,       // token-aware batching
pub batch_size: Option<usize>,             // item count batching (already exists)
```

## What about `compact_inputs`?

Leave it. It still works for webbing steps via `build_webbing_input`. For non-web steps, `item_fields` is the replacement. They don't conflict — `compact_inputs` operates on the webbing-specific node loading path, `item_fields` operates on the generic for_each/single item path.

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add `item_fields` and `batch_max_tokens` to `ChainStep`
- `src-tauri/src/pyramid/chain_executor.rs`:
  - `execute_for_each` — apply `project_item` before dispatch, use `batch_items_by_tokens` when `batch_max_tokens` is set
  - `execute_single` — apply `project_item` to array elements in resolved input
- `src-tauri/src/pyramid/defaults_adapter.rs` — pass through new fields to IR step metadata

## Test
Build a 127-doc pyramid with `item_fields: ["node_id", "headline", "orientation"]` on the clustering step. The Qwen calls (92K tokens) should drop to ~20K tokens on Mercury 2. Build should complete in under 5 minutes total.
