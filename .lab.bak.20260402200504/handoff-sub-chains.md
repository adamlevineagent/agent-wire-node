# Rust Handoff: Sub-chains — composable step nesting

## Principle

Every time we need a new execution pattern (batch+merge, split+merge, overflow handling, sequential accumulation within a thread), we currently need a new Rust code path. This is the opposite of the everything-is-YAML architecture. The chain executor should support ONE generic primitive — steps that contain steps — and every pattern becomes YAML composition.

## The Problem

Current special-case Rust code paths:
- `recursive_cluster` — hardcoded cluster→synthesize→repeat loop
- `split_merge` / `max_input_tokens` — hardcoded split→extract→merge
- `batch_size` + merge step — requires two separate top-level steps that the user must wire together
- `sequential` + `accumulate` — hardcoded accumulator pattern
- Thread narrative overflow — NO solution exists; oversized threads cascade to Qwen

Each of these is a pattern that should be expressible in YAML but instead requires Rust implementation. Every new pattern means another Rust pass.

## The Fix: `steps` field on ChainStep

A step can contain sub-steps. When `steps` is present, the executor runs the sub-chain instead of dispatching to the LLM directly.

### New field on ChainStep

```rust
#[serde(default)]
pub steps: Option<Vec<ChainStep>>,
```

### Execution rules

1. If a step has `steps`, it's a **container step**. It does NOT make an LLM call itself.
2. The container step's `for_each` provides `$item` to the inner scope.
3. Inner steps execute sequentially within the container.
4. Inner step outputs are scoped — `$step_name` references resolve within the container first, then fall back to the outer chain context.
5. The container step's output is the last inner step's output.
6. The container step's `save_as`, `node_id_pattern`, `depth` apply to the final output.

### YAML Examples

#### Thread narrative with overflow handling

```yaml
- name: thread_narrative
  for_each: $thread_clustering.threads
  concurrency: 5
  node_id_pattern: "L1-{index:03}"
  depth: 1
  save_as: node
  steps:
    - name: batch_synthesize
      primitive: synthesize
      instruction: "$prompts/document/doc_thread.md"
      for_each: $item.assigned_docs
      batch_max_tokens: 60000
      concurrency: 3
      model_tier: mid
    - name: merge_thread
      primitive: synthesize
      instruction: "$prompts/document/doc_thread_merge.md"
      input:
        parts: $batch_synthesize
      model_tier: mid
```

The outer `for_each` iterates threads. For each thread, the inner chain runs: batch the thread's assigned docs into sub-groups, synthesize each sub-group, then merge into the final thread node. Every call fits Mercury 2. No Qwen cascades.

#### Oversized document splitting (replaces max_input_tokens)

```yaml
- name: l0_doc_extract
  for_each: $chunks
  dispatch_order: "largest_first"
  concurrency: 12
  node_id_pattern: "D-L0-{index:03}"
  depth: 0
  save_as: node
  steps:
    - name: split_extract
      primitive: extract
      instruction: "$prompts/document/doc_extract.md"
      for_each: $item.sections
      batch_max_tokens: 60000
      model_tier: mid
    - name: merge_extractions
      primitive: synthesize
      instruction: "$prompts/shared/merge_sub_chunks.md"
      input:
        parts: $split_extract
      model_tier: mid
```

Wait — this requires the input to already be split into sections. The splitting logic itself needs to live somewhere. Two options:

**Option A:** A `split` primitive that runs in Rust without an LLM call:
```yaml
- name: split_doc
  primitive: split
  split_strategy: "sections"
  max_tokens: 60000
  overlap_tokens: 500
```

**Option B:** The `for_each` on the inner step handles it — if `$item` is a string (raw content) and `batch_max_tokens` is set, the executor splits the string into text chunks before dispatching.

Option A is cleaner — it makes splitting explicit and YAML-controlled. The `split` primitive is a Rust utility (no LLM), but its parameters are all YAML.

#### Revised oversized document splitting

```yaml
- name: l0_doc_extract
  for_each: $chunks
  dispatch_order: "largest_first"
  concurrency: 12
  node_id_pattern: "D-L0-{index:03}"
  depth: 0
  save_as: node
  steps:
    - name: split_if_needed
      primitive: split
      input: $item.content
      max_tokens: 60000
      strategy: "sections"
      overlap_tokens: 500
      # Output: array of text chunks (1 element if no split needed)
    - name: extract_parts
      primitive: extract
      instruction: "$prompts/document/doc_extract.md"
      for_each: $split_if_needed
      concurrency: 3
      model_tier: mid
    - name: merge_if_split
      primitive: synthesize
      instruction: "$prompts/shared/merge_sub_chunks.md"
      input:
        parts: $extract_parts
      # Only runs if there were multiple parts; passthrough if single
      when: "count($extract_parts) > 1"
      model_tier: mid
```

#### Batched clustering as a sub-chain (replaces separate batch+merge steps)

```yaml
- name: thread_clustering
  steps:
    - name: batch_cluster
      primitive: classify
      instruction: "$prompts/document/doc_cluster.md"
      for_each: $l0_doc_extract
      item_fields: ["node_id", "headline", "orientation", "topics.name"]
      batch_max_tokens: 80000
      concurrency: 3
      model_tier: mid
    - name: merge_clusters
      primitive: classify
      instruction: "$prompts/document/doc_cluster_merge.md"
      input:
        batch_results: $batch_cluster
      model_tier: mid
```

Now it's one logical step instead of two top-level steps. The merge is always coupled to the batch.

#### Recursive convergence as a sub-chain (replaces recursive_cluster flag)

```yaml
- name: upper_layers
  primitive: loop
  until: "count($current_nodes) <= 1"
  steps:
    - name: recluster
      primitive: classify
      instruction: "$prompts/document/doc_recluster.md"
      input:
        nodes: $current_nodes
      cluster_item_fields: ["node_id", "headline", "orientation"]
      model_tier: mid
    - name: check_apex
      primitive: gate
      when: "$recluster.apex_ready == true"
      break: true  # exit the loop
    - name: synthesize_clusters
      primitive: synthesize
      instruction: "$prompts/document/doc_distill.md"
      for_each: $recluster.clusters
      node_id_pattern: "L{depth}-{index:03}"
      save_as: node
      model_tier: mid
```

This replaces the entire `execute_recursive_cluster` function with YAML. The loop structure, the apex_ready check, the convergence behavior — all in YAML. Rust just executes the loop and checks the `until` and `when` conditions.

## New Rust Primitives Needed

### 1. Container steps (sub-chains)
`steps: Vec<ChainStep>` on ChainStep. Sequential execution, scoped outputs.

### 2. `split` primitive (no LLM)
Takes text content, splits by strategy (sections/lines/tokens), returns array of chunks. Parameters: `max_tokens`, `strategy`, `overlap_tokens`.

### 3. `loop` primitive
Repeats its sub-steps until a condition is met. Parameters: `until` (expression that reads step outputs).

### 4. `gate` primitive (no LLM)
Evaluates a `when` condition. If `break: true` and condition is met, exits the enclosing loop. If condition is not met, continues to next step.

### 5. Expression evaluation
Simple expression language for `when`, `until`, and conditional fields:
- `count($step_name)` — array length
- `$step_name.field` — field access
- `== true`, `> N`, `<= N` — comparison
- `AND`, `OR` — logical

This does NOT need to be a full programming language. It's a predicate evaluator for flow control. Keep it minimal.

## What this replaces

| Current Rust special-case | Replaced by |
|---|---|
| `recursive_cluster` flag + `execute_recursive_cluster()` | `loop` + sub-chain |
| `max_input_tokens` + `split_strategy` + `split_merge` | `split` primitive + sub-chain |
| Separate batch + merge top-level steps | Sub-chain within one logical step |
| `sequential` + `accumulate` | Could be expressed as a loop (stretch, current works fine) |
| Hardcoded convergence fallback logic | `gate` + conditional re-call in YAML |

## What stays in Rust

- `for_each` dispatch (concurrency pool, progress tracking)
- `batch_size` / `batch_max_tokens` (batching is mechanical, not a quality decision)
- `item_fields` projection (mechanical data shaping)
- `dispatch_order` sorting (mechanical)
- DB read/write, node persistence, resume/replay
- LLM dispatch (HTTP, retry, JSON parse)
- Expression evaluation engine (the evaluator itself is Rust; the expressions are YAML)

## Implementation scope

All four primitives ship together:

1. **Container steps** (`steps` field) — sub-chain execution with scoped outputs
2. **`split` primitive** — YAML-controlled text splitting (no LLM)
3. **`loop` + `gate` primitives** — YAML-controlled iteration and conditional flow
4. **Expression evaluator** — predicate engine for `when`/`until` conditions

Without all four, `recursive_cluster` stays hardcoded in Rust and we're back to writing Rust for every new execution pattern.

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add `steps: Option<Vec<ChainStep>>` to ChainStep, add `split`/`loop`/`gate` primitives
- `src-tauri/src/pyramid/chain_executor.rs` — container step execution (recurse into sub-steps), split primitive implementation, loop primitive, gate/expression evaluator
- Chain YAMLs — restructure to use sub-chains (after Rust ships)
