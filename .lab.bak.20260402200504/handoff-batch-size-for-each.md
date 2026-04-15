# Rust Handoff: `for_each` batching & projection primitives

## Status
**IMPLEMENTED** — compiled, built into Wire Node v0.2.0.

## Summary
Three composable YAML primitives that give full control over what data travels to the LLM and how batches are sized. Intelligence stays in YAML/prompts — Rust just projects, measures, and chunks.

## Primitives

### 1. `item_fields` — field-level projection

```yaml
item_fields: ["node_id", "headline", "orientation"]
```

Projects each item to only the listed top-level fields before dispatch. Applied FIRST (before token estimation and batching), so all three primitives compose correctly.

Given an L0 extraction item with `node_id`, `headline`, `orientation`, `topics` (huge), `entities`, etc. — setting `item_fields: ["node_id", "headline", "orientation"]` strips everything else. A 92K-token clustering call drops to ~20K.

**Applies to:** `for_each` steps (each item or each item within a batch). Does NOT apply to `execute_web_step` (webbing has its own compaction via `compact_inputs`).

### 2. `batch_size` — proportional count-based batching

```yaml
batch_size: 100
```

Chunks items into proportionally balanced groups. 127 items with `batch_size: 100` → `[64, 63]` (not `[100, 27]`). Each batch is passed as a JSON array in `$item`.

### 3. `batch_max_tokens` — token-aware greedy batching

```yaml
batch_max_tokens: 80000
```

Fills each batch greedily until the token limit would be exceeded. Token estimation: `json_string.len() / 4` (same heuristic as `build.rs`). A single oversized item always gets its own batch (never dropped).

### Composition

When multiple primitives are set, the pipeline is:

```
items → project(item_fields) → batch(batch_max_tokens OR batch_size) → dispatch
```

| Fields set | Behavior |
|---|---|
| `item_fields` only | Project, no batching (one item per call) |
| `batch_size` only | Proportional splitting, no projection |
| `batch_max_tokens` only | Token-greedy batching, no projection |
| `item_fields` + `batch_size` | Project → proportional split |
| `item_fields` + `batch_max_tokens` | Project → token-greedy fill |
| `item_fields` + `batch_size` + `batch_max_tokens` | Project → greedy fill respecting both limits |
| `batch_size` + `batch_max_tokens` | Greedy fill respecting both limits, no projection |

### YAML Usage

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

# Merge: combine batch results into final threads
- name: thread_clustering_merge
  primitive: classify
  instruction: "$prompts/document/doc_cluster_merge.md"
  input:
    batch_results: $thread_clustering_batch

# Thread synthesis: full items, no batching
- name: thread_narrative
  primitive: synthesize
  instruction: "$prompts/document/doc_thread.md"
  for_each: $thread_clustering.threads
  concurrency: 5
  model_tier: mid
```

The prompt receives `$item` as a JSON array when batched. The prompt handles the array. A separate merge step combines batch results. This keeps intelligence in YAML/prompts, not Rust.

### What about `batch_threshold` and `merge_instruction`?

Dead code on `ChainStep`. They put token estimation and merge logic in Rust. These new primitives replace them — the YAML decides what to project, what token budget to use, and how to merge.

### What about `compact_inputs`?

Still works for webbing steps via `build_webbing_input`. For non-web steps, `item_fields` is the replacement. They don't conflict.

## Files Modified
- `chain_engine.rs` — `item_fields: Option<Vec<String>>`, `batch_max_tokens: Option<usize>` on ChainStep
- `chain_executor.rs` — `project_item()`, `batch_items_by_tokens()`, wired into `execute_for_each` pipeline
- `defaults_adapter.rs` — IR metadata passthrough

## Also Shipped in Same Session

### Live Pyramid Build Visualization
Build progress replaced with live pyramid. Layers appear as rows of cells filling in real-time. Design doc: `docs/live-pyramid-build-visualization.md`.

### Server Lockup Fix
Builds get their own SQLite reader connection (`with_build_reader()`) so existing pyramids remain queryable during builds.

### Chain Auto-Sync (Two-Tier)
`ensure_default_chains()` now syncs from the source tree when present (dev mode), bootstraps from embedded defaults otherwise. No more manual rsync after prompt changes.
