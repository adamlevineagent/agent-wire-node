# Action Chain System

Schema version: 1 | Runtime: Rust (Tauri backend) | Definition format: YAML + Markdown prompts

---

## 1. Overview

The Action Chain system replaces hardcoded Rust build pipelines with data-driven, YAML-defined execution chains. Each chain declares an ordered sequence of steps that transform raw content chunks into a hierarchical pyramid structure (nodes at increasing depths of abstraction, terminating at a single apex node).

**Problem solved:** The original build pipelines (`build_conversation`, `build_code`, `build_docs`) were ~8,000 lines of Rust with substantial duplication. Prompt text, model selection, error handling, and step ordering were all compiled into the binary. Changing a prompt or reordering steps required recompilation.

**What the chain system provides:**

- Pipelines as data: YAML chain definitions + Markdown prompt files, editable without recompilation.
- Per-step model selection, temperature, and error handling.
- Resume support: interrupted builds restart from the exact point of failure.
- Variant chains: users can fork a default chain, modify steps/prompts, and assign variants per-pyramid.
- Observability: per-step timing, token counts, and cost logged to `pyramid_cost_log`.
- A path to agent-driven chain authoring: chains are structured data that agents can read, modify, and export.

**Scope (v1):** Conversation, code, and document pipelines only. Vine, delta, and meta chains are wave 2.

---

## 2. Architecture

Six Rust modules in `src-tauri/src/pyramid/` compose the chain runtime:

```
chain_engine.rs    Schema structs + validation
chain_resolve.rs   $ref resolution + {{template}} prompt resolution
chain_dispatch.rs  LLM and mechanical step dispatch + node construction
chain_executor.rs  Main execution loop (forEach, pair, recursive_pair, resume)
chain_loader.rs    YAML loading, $prompts/ file refs, directory scanning
chain_registry.rs  SQLite assignment table, chain<->slug mapping
```

### Execution flow

```
run_chain_build(slug)
  |
  +-- chain_registry: resolve chain_id for slug (or use default for content_type)
  +-- chain_loader: load YAML, resolve $prompts/ refs, validate via chain_engine
  |
  +-- chain_executor::execute_chain(state, chain, slug, cancel, progress_tx)
        |
        +-- Build ChainContext (chain_resolve) with chunks, slug, content_type
        +-- Build StepContext (chain_dispatch) with DB connections, LLM config
        +-- Spawn writer drain task (async channel -> SQLite writes)
        |
        +-- For each step in chain.steps:
              |
              +-- Evaluate `when` condition
              +-- Determine step mode: mechanical | recursive_pair | pair_adjacent | forEach | single
              +-- For each iteration within the mode:
                    |
                    +-- Check resume state (step output + node existence)
                    +-- chain_resolve: resolve $refs in step.input
                    +-- chain_resolve: resolve {{vars}} in prompt template
                    +-- chain_dispatch: dispatch to LLM or mechanical function
                    +-- Persist step output + node (if save_as: node)
                    +-- Update accumulators (if sequential)
                    +-- Report progress
              |
              +-- Store step outputs in ChainContext for downstream $refs
        |
        +-- Return (apex_node_id, failure_count)
```

### Data flow between modules

- **chain_engine** defines the types (`ChainDefinition`, `ChainStep`, `ChainDefaults`) and validates structural correctness.
- **chain_resolve** owns runtime variable state (`ChainContext`) and provides two resolvers: `resolve_ref` for `$variable.path` in YAML inputs, and `resolve_prompt_template` for `{{variable}}` in Markdown prompts.
- **chain_dispatch** routes steps to either the LLM (via OpenRouter) or named Rust mechanical functions. Also provides `build_node_from_output` (LLM JSON -> `PyramidNode`) and `generate_node_id` (pattern-based ID generation).
- **chain_executor** orchestrates the full execution: step iteration, mode dispatch, resume checks, error strategy application, cancellation, progress reporting, and the writer drain channel.

---

## 3. Chain Definition Schema

### Top-level fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema_version` | `u32` | yes | Must be `1`. Validated at load time. |
| `id` | `String` | yes | Immutable identity for assignments. Referenced by `pyramid_chain_assignments.chain_id`. |
| `name` | `String` | yes | Human-readable name. |
| `description` | `String` | yes | What this chain does. |
| `content_type` | `String` | yes | One of `"conversation"`, `"code"`, `"document"`. |
| `version` | `String` | yes | Semver string for the chain definition itself. |
| `author` | `String` | yes | Who authored this chain. |
| `defaults` | `ChainDefaults` | yes | Default model_tier, model, temperature, on_error for all steps. |
| `steps` | `Vec<ChainStep>` | yes | Ordered list of steps. At least one required. |
| `post_build` | `Vec<PostBuildRef>` | no | Wave 2: references to delta/meta chains to run after the main build. |

### ChainDefaults

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model_tier` | `String` | `"mid"` | Default tier: `low`, `mid`, `high`, `max`. |
| `model` | `Option<String>` | `null` | Direct OpenRouter model slug override. Takes precedence over tier. |
| `temperature` | `f32` | `0.3` | Default LLM temperature. |
| `on_error` | `String` | `"retry(2)"` | Default error strategy for all steps. |

### ChainStep fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `String` | yes | Unique within the chain. Used as key in `step_outputs`. |
| `primitive` | `String` | yes | One of 28 Wire synthesis primitives (see section 12) or `"custom"`. |
| `instruction` | `Option<String>` | LLM steps | Prompt file reference (`$prompts/conversation/forward.md`) or inline text. Required for non-mechanical steps. |
| `mechanical` | `bool` | no | If `true`, dispatches to a named Rust function instead of the LLM. |
| `rust_function` | `Option<String>` | mechanical only | Name of the Rust function to call. Required when `mechanical: true`. |
| `input` | `Option<Value>` | no | JSON object with `$ref` expressions. Resolved against `ChainContext` before dispatch. |
| `output_schema` | `Option<Value>` | no | Expected output shape (informational, not enforced at runtime in v1). |
| `model_tier` | `Option<String>` | no | Override default model tier for this step. |
| `model` | `Option<String>` | no | Direct model override. Highest precedence. |
| `temperature` | `Option<f32>` | no | Override default temperature. |
| `sequential` | `bool` | no | If `true`, forEach processes in order with state accumulation. Requires `for_each`. |
| `accumulate` | `Option<Value>` | no | Accumulator configuration for sequential forEach (see section 6). |
| `for_each` | `Option<String>` | no | Loop expression. Resolves to an array via `ChainContext`. |
| `pair_adjacent` | `bool` | no | Pair adjacent nodes from a source depth, producing nodes at depth+1. |
| `recursive_pair` | `bool` | no | Repeat adjacent pairing from a starting depth until a single apex remains. Mutually exclusive with `pair_adjacent`. |
| `batch_threshold` | `Option<usize>` | no | Estimated token limit. When exceeded, input is batched and results merged. |
| `merge_instruction` | `Option<String>` | no | Prompt for merging batched results. |
| `when` | `Option<String>` | no | Conditional expression. Step is skipped if this evaluates to false. |
| `on_error` | `Option<String>` | no | Error strategy override: `abort`, `skip`, `retry(N)`, `carry_left`, `carry_up`. |
| `save_as` | `Option<String>` | no | `"node"` to persist as a `PyramidNode`. `"step_only"` to persist step output without a node. |
| `node_id_pattern` | `Option<String>` | no | Template for generated node IDs: `"L0-{index:03}"`, `"L{depth}-{index:03}"`. |
| `depth` | `Option<i64>` | no | Depth value for generated nodes. |

### Validation rules

The `validate_chain` function enforces:

- `schema_version` must equal 1.
- `id` and `name` must be non-empty.
- `content_type` must be one of the three valid types.
- At least one step is required.
- Step names must be unique within the chain.
- Each step's `primitive` must be in the valid primitives list.
- Mechanical steps must declare `rust_function`.
- LLM steps must declare `instruction`.
- `on_error` values must be `abort`, `skip`, `retry(N)` where N is 1-10, `carry_left`, or `carry_up`.
- `recursive_pair` and `pair_adjacent` are mutually exclusive.
- `sequential` requires `for_each`.

Non-standard `model_tier` values produce warnings (not errors).

---

## 4. Variable Resolution

Two resolution systems operate at different layers.

### YAML input resolution: `$variable.path`

The `ChainContext` struct holds all runtime state. When a step's `input` JSON is resolved, every string value starting with `$` is looked up against the context.

**Built-in scalars:**

| Reference | Type | Description |
|-----------|------|-------------|
| `$chunks` | `Array` | All content chunks for this pyramid. |
| `$chunks_reversed` | `Array` | Chunks in reverse order. |
| `$slug` | `String` | Pyramid slug being built. |
| `$content_type` | `String` | `"conversation"`, `"code"`, or `"document"`. |
| `$has_prior_build` | `Bool` | True if nodes already exist for this slug. |

**forEach loop variables (only available inside a forEach step):**

| Reference | Type | Description |
|-----------|------|-------------|
| `$item` | `Value` | Current iteration item. |
| `$index` | `Number` | Current iteration index (0-based). |

**recursive_pair variables (only available inside pair steps):**

| Reference | Type | Description |
|-----------|------|-------------|
| `$pair.left` | `Value` | Left node in the current pair. |
| `$pair.right` | `Value` | Right node (null if odd carry). |
| `$pair.depth` | `Number` | Depth being constructed. |
| `$pair.index` | `Number` | Pair index within current depth. |
| `$pair.is_carry` | `Bool` | True if this is an odd node being carried up. |

**Step output references:**

Outputs from prior steps are namespaced by step name:

- `$forward_pass` -- full output of the `forward_pass` step
- `$forward_pass.output.distilled` -- dot-path navigation into the output
- `$forward_pass.nodes[0]` -- array index (literal integer)
- `$forward_pass.nodes[$index]` -- array index using forEach index
- `$forward_pass.nodes[i]` -- pair mode: `pair_index * 2`
- `$forward_pass.nodes[i+1]` -- pair mode: `pair_index * 2 + 1`

**Accumulator references:**

Named accumulators are accessible as top-level `$ref` values (e.g., `$running_context`).

**Resolution behavior:**

- A string that is entirely a `$ref` resolves to the referenced value's native type (preserving arrays, objects, numbers, bools).
- A string containing embedded `$ref` patterns interpolates them as strings: `"Chunk $index of conversation"` becomes `"Chunk 3 of conversation"`.
- Objects and arrays are walked recursively.
- Non-string primitives (numbers, bools, null) pass through unchanged.
- **Undefined required references are runtime errors**, not warnings. The step fails immediately.

### Prompt template resolution: `{{variable}}`

Prompt `.md` files use double-brace syntax. The engine resolves `{{variable}}` by looking up the key in the step's already-resolved input map.

```markdown
You are a distillation engine.

## SIBLING A (earlier)
{{left}}

## SIBLING B (later)
{{right}}
```

Given a resolved input of `{"left": "...", "right": "..."}`, the engine substitutes both slots. Nested paths work: `{{data.summary}}` navigates into the input object.

- Unresolved `{{ref}}` is a runtime error.
- Values are stringified for interpolation: strings as-is, numbers/bools as their string representation, objects/arrays as compact JSON.

---

## 5. Step Modes

Every step executes in exactly one of five modes, determined by its configuration flags.

### `forEach`

Iterates over an array resolved from the `for_each` field. Each iteration receives `$item` and `$index` in the context.

```yaml
- name: "forward_pass"
  primitive: "compress"
  instruction: "$prompts/conversation/forward.md"
  for_each: "chunks"
  sequential: true
  accumulate:
    field: "running_context"
    init: "Beginning of conversation."
    max_chars: 1500
    trim_to: 1200
    trim_side: "start"
```

If `sequential: true`, iterations execute in order and accumulators carry state forward. If `sequential: false` (default), iterations could conceptually run concurrently (v1 executes sequentially regardless but does not enforce accumulator semantics).

Step outputs are collected into a `Vec<Value>` indexed by iteration order and stored in `ChainContext.step_outputs` under the step name.

### `pair_adjacent`

Pairs nodes at a source depth into nodes at depth+1. Reads all nodes at the source depth from the database, then iterates in pairs of two.

```yaml
- name: "l1_pairing"
  primitive: "synthesize"
  instruction: "$prompts/conversation/distill.md"
  pair_adjacent: true
  depth: 1
  on_error: "carry_left"
```

The source depth is derived from the `depth` field (target depth = source depth from input config, actual node depth = source + 1). Each pair dispatches through the LLM. An odd trailing node is carried up without an LLM call. On failure with `carry_left`, the left node is promoted to the target depth.

### `recursive_pair`

Repeats adjacent pairing from a starting depth until only one node (the apex) remains. Each round reads nodes at the current depth, pairs them, writes nodes at depth+1, then advances.

```yaml
- name: "upper_layers"
  primitive: "synthesize"
  instruction: "$prompts/conversation/distill.md"
  recursive_pair: true
  depth: 2
  node_id_pattern: "L{depth}-{index:03}"
  on_error: "carry_left"
```

The loop terminates when a depth has 0 or 1 nodes. If a depth is already fully populated (resume case), it is skipped entirely.

### `single`

A step with no `for_each`, `pair_adjacent`, or `recursive_pair` executes once. Used for batch operations like thread clustering where one LLM call processes all data.

```yaml
- name: "thread_clustering"
  primitive: "classify"
  instruction: "$prompts/conversation/thread_cluster.md"
  batch_threshold: 30000
  merge_instruction: "$prompts/conversation/merge_batches.md"
```

### `mechanical`

Dispatches to a named Rust function instead of the LLM. No instruction, model_tier, or temperature applies.

```yaml
- name: "mechanical_metadata"
  primitive: "detect"
  mechanical: true
  rust_function: "extract_import_graph"
  on_error: "abort"
```

Known v1 mechanical functions: `extract_import_graph`, `extract_mechanical_metadata`, `cluster_by_imports`, `cluster_by_entity_overlap`.

---

## 6. Sequential Accumulation

For steps that must process items in order while carrying forward a running summary (e.g., the conversation forward/reverse passes), the `accumulate` config declares state that persists across iterations.

### Configuration

```yaml
accumulate:
  field: "running_context"
  init: "Beginning of conversation."
  max_chars: 1500
  trim_to: 1200
  trim_side: "start"
```

| Field | Description |
|-------|-------------|
| `field` | Name of the accumulator field in the LLM output to extract. |
| `init` | Initial value before the first iteration. |
| `max_chars` | Maximum character length before truncation. |
| `trim_to` | Target length when truncation triggers. |
| `trim_side` | Which end to trim from: `"start"` (keep end) or `"end"` (keep start). |

### How it works

1. Before the forEach loop begins, the accumulator is initialized from `init` and stored in `ChainContext.accumulators`.
2. The accumulator value is accessible as `$running_context` (or whatever the field name is) in step input resolution.
3. After each iteration's LLM response, the engine extracts the named field from the output and updates the accumulator.
4. If the new value exceeds `max_chars`, it is truncated to `trim_to` characters from the specified side.

### Resume semantics

On resume, the engine replays all completed prior iterations' stored outputs to reconstruct the accumulator value before continuing from the first incomplete iteration. This is implemented in `execute_for_each`: when `ResumeState::Complete` is detected for a sequential step, the prior output is loaded from the database, and `update_accumulators` is called to replay the accumulator state.

---

## 7. Resume Contract

The chain executor supports full resume after interruption (crash, cancellation, or error). Resume correctness depends on a dual-check system.

### Per-iteration resume check

For each iteration of a step, `get_resume_state` checks:

1. **Step output existence:** Does a `pipeline_steps` row exist for this (slug, step_name, chunk_index, depth, node_id)?
2. **Node existence (if `save_as: "node"`):** Does a `pyramid_nodes` row exist for this node_id?

Three possible states:

| State | Meaning | Action |
|-------|---------|--------|
| `Complete` | Step output AND node (if applicable) both exist. | Skip this iteration. |
| `StaleStep` | Step output exists but node is missing. | Rebuild (step was saved but node write failed). |
| `Missing` | No step output. | Execute normally. |

### Why dual check matters

A step that saves both a pipeline_steps record and a node could fail between the two writes. If the executor only checked step output, it would skip the iteration, leaving a missing node. The dual check catches this: `StaleStep` means the step ran but the node was lost, so it must be re-executed.

### Accumulator replay

For sequential forEach steps, resume must also reconstruct accumulator state. When a completed iteration is encountered during resume, its stored output is loaded from the database and passed through `update_accumulators` to rebuild the running state.

### recursive_pair depth-level resume

For recursive_pair, each entire depth level is checked: if the target depth already has the expected number of nodes (`ceil(source_count / 2)`), that depth is skipped entirely. Within a partially-complete depth, individual pairs are checked via the standard dual-check.

---

## 8. Error Handling

### The five strategies

| Strategy | Behavior |
|----------|----------|
| `abort` | Fail the entire chain immediately. No recovery. |
| `skip` | Log the failure, record it as a failure count, continue to the next iteration. |
| `retry(N)` | Retry up to N times (1-10) with exponential backoff: 2s, 4s, 8s, etc. If all retries fail, fall through to the step's secondary behavior (skip for forEach, carry for pairs). |
| `carry_left` | On pair failure, promote the left node to the target depth without an LLM call. The left node's content is copied; its ID, depth, and children are updated. |
| `carry_up` | Synonym for `carry_left` in the current implementation. |

### Resolution order

1. Step-level `on_error` (if specified).
2. Chain-level `defaults.on_error` (fallback).

### Retry with exponential backoff

```rust
// Backoff: 2^(attempt+1) seconds
let delay = Duration::from_secs(2u64.pow(attempt + 1));
```

Attempt 0 fails -> wait 2s -> attempt 1 fails -> wait 4s -> attempt 2 fails -> wait 8s -> ...

### JSON-retry guarantee

All LLM steps have an automatic JSON parse retry built into the dispatch layer, independent of the step-level error strategy. If the LLM response fails JSON extraction:

1. The step is retried once at temperature 0.1 (low temperature for more deterministic output).
2. If the retry also fails JSON extraction, the error propagates to the step-level error strategy.

This is implemented in `chain_dispatch::dispatch_llm` and applies to every LLM call without configuration.

---

## 9. Prompt Templates

### File structure

```
{data_dir}/chains/
  prompts/
    conversation/           # 7 prompt files
      forward.md
      reverse.md
      combine.md
      distill.md
      thread_cluster.md
      thread_narrative.md
      merge_batches.md
    code/                   # 3 prompt files
      code_extract.md
      config_extract.md
      code_group.md
    document/               # 2 prompt files
      doc_extract.md
      doc_group.md
```

### Reference syntax

In YAML chain definitions, prompt files are referenced with the `$prompts/` prefix:

```yaml
instruction: "$prompts/conversation/forward.md"
```

The chain loader resolves this to the full filesystem path and loads the file contents.

### Template slots

Prompts use `{{variable}}` syntax. Variables are resolved from the step's resolved input map (after `$ref` resolution has already run).

Example from a distillation prompt:

```markdown
You are a distillation engine. Compress these two sibling nodes...

## SIBLING A (earlier)
{{left}}

## SIBLING B (later)
{{right}}
```

The step's `input` block resolves the `$ref` values:

```yaml
input:
  left: "$pair.left"
  right: "$pair.right"
```

After `$ref` resolution, the input map contains `{"left": "...", "right": "..."}`, which is then used to fill the `{{left}}` and `{{right}}` slots in the prompt.

### Variant prompts

Default chains reference shared prompt files. Variant chains (in `chains/variants/`) inline their prompts directly in the YAML `instruction` field. Editing a variant never modifies shared prompt files.

---

## 10. Model Configuration

### Tier system

Four tiers map to configured model slugs:

| Tier | Maps to | Typical use |
|------|---------|-------------|
| `low` | `config.primary_model` | Lightweight extraction, metadata. |
| `mid` | `config.primary_model` | Standard distillation, synthesis. Default. |
| `high` | `config.fallback_model_1` | Complex reasoning, thread clustering. |
| `max` | `config.fallback_model_2` | Apex synthesis, critical steps. |

The actual model slugs are defined in `LlmConfig` and point to OpenRouter endpoints.

### Resolution precedence

1. **Step `model` field** (direct slug, e.g., `"inception/mercury-2"`) -- highest precedence.
2. **Defaults `model` field** (if step has no `model_tier` override).
3. **Step `model_tier`** mapped through the tier system.
4. **Defaults `model_tier`** mapped through the tier system -- lowest precedence.

```yaml
# Direct override on a step:
- name: "apex_synthesis"
  model: "anthropic/claude-sonnet-4"
  # Ignores tier entirely

# Tier override on a step:
- name: "thread_clustering"
  model_tier: "high"
  # Uses config.fallback_model_1
```

### Temperature

Resolved per-step: `step.temperature` overrides `defaults.temperature`. Default is `0.3`.

---

## 11. Creating Custom Chains

### Chain storage

```
{data_dir}/chains/
  defaults/                 # Shipped with the application. Do not modify.
    conversation.yaml
    code.yaml
    document.yaml
  variants/                 # User-created chains.
    {name}.yaml
```

### Chain assignment

Chains are assigned to pyramids via SQLite:

```sql
CREATE TABLE IF NOT EXISTS pyramid_chain_assignments (
    slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    chain_id TEXT NOT NULL,
    chain_file TEXT,
    assigned_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

`chain_id` is authoritative (matches the YAML `id` field). `chain_file` is a cached hint for fast lookup, re-resolved by directory scan on startup.

### Creating a variant

1. Copy a default chain YAML to `chains/variants/my-variant.yaml`.
2. Change the `id` field to a unique value.
3. Modify steps: remove steps, reorder, change model tiers, inline custom prompts.
4. Assign the variant to a pyramid slug.

### Example: conversation chain without reverse pass

```yaml
schema_version: 1
id: "conv-no-reverse"
name: "Conversation (No Reverse)"
description: "Simplified conversation pipeline without reverse pass."
content_type: "conversation"
version: "1.0.0"
author: "user"

defaults:
  model_tier: "mid"
  temperature: 0.3
  on_error: "retry(2)"

steps:
  - name: "forward_pass"
    primitive: "compress"
    instruction: "$prompts/conversation/forward.md"
    for_each: "chunks"
    sequential: true
    accumulate:
      field: "running_context"
      init: "Beginning of conversation."
      max_chars: 1500
      trim_to: 1200
      trim_side: "start"
    save_as: "node"
    node_id_pattern: "L0-{index:03}"
    depth: 0

  # No reverse_pass, no combine — forward directly produces L0 nodes.

  - name: "upper_layers"
    primitive: "synthesize"
    instruction: "$prompts/conversation/distill.md"
    recursive_pair: true
    depth: 0
    node_id_pattern: "L{depth}-{index:03}"
    on_error: "carry_left"

post_build: []
```

This produces a shorter pipeline with different topology (no combined forward+reverse L0 nodes, fewer steps, faster execution).

---

## 12. Wire Synthesis Primitives

Every chain step declares a `primitive` from the following 28 categories. The primitive is a semantic label indicating the step's intent -- the actual behavior comes from the instruction prompt and model. Primitives are validated at chain load time.

### Perception

| Primitive | Description |
|-----------|-------------|
| `ingest` | Accept and normalize raw input data. |
| `extract` | Pull structured information from unstructured content. |
| `classify` | Assign categories, labels, or types to items. |
| `detect` | Identify patterns, anomalies, or specific features. |

### Judgment

| Primitive | Description |
|-----------|-------------|
| `evaluate` | Assess quality, relevance, or fitness of content. |
| `compare` | Contrast two or more items along specified dimensions. |
| `verify` | Confirm factual accuracy or internal consistency. |
| `calibrate` | Adjust confidence levels or scoring thresholds. |
| `interrogate` | Generate probing questions to expose gaps or assumptions. |

### Synthesis

| Primitive | Description |
|-----------|-------------|
| `pitch` | Generate a concise persuasive summary or proposal. |
| `draft` | Produce a first-pass written artifact. |
| `synthesize` | Combine multiple inputs into a unified output. |
| `translate` | Convert content between formats, languages, or registers. |
| `analogize` | Map concepts from one domain to another. |
| `compress` | Reduce content while preserving essential meaning. |
| `fuse` | Merge two complementary analyses into one. |

### Adversarial

| Primitive | Description |
|-----------|-------------|
| `review` | Critique content for weaknesses or improvements. |
| `fact_check` | Verify claims against known evidence. |
| `rebut` | Construct counterarguments to a position. |
| `steelman` | Strengthen the best version of an argument. |
| `strawman` | Identify the weakest form of an argument. |

### Temporal

| Primitive | Description |
|-----------|-------------|
| `timeline` | Order events or changes chronologically. |
| `monitor` | Track state changes over time. |
| `decay` | Model how information relevance diminishes. |
| `diff` | Identify what changed between two states. |

### Relational

| Primitive | Description |
|-----------|-------------|
| `relate` | Establish relationships between entities or concepts. |
| `cross_reference` | Link information across multiple sources. |
| `map` | Build a structural representation of connections. |

### Meta

| Primitive | Description |
|-----------|-------------|
| `price` | Estimate cost, effort, or resource requirements. |
| `metabolize` | Process and integrate feedback or corrections. |
| `embody` | Adopt a persona or perspective for generation. |

### Escape hatch

| Primitive | Description |
|-----------|-------------|
| `custom` | Any operation not covered by the standard set. |

---

## 13. Default Chains

### conversation.yaml

Seven-step pipeline:

1. **forward_pass** (`compress`, forEach chunks, sequential) -- Forward chronological distillation with running context accumulation.
2. **reverse_pass** (`compress`, forEach chunks_reversed, sequential) -- Reverse chronological distillation with future context accumulation.
3. **combine_l0** (`fuse`, forEach chunks) -- Combines forward+reverse outputs ("stone + water") into L0 nodes.
4. **l1_pairing** (`synthesize`, pair_adjacent) -- Pairs adjacent L0 nodes into L1 via distillation.
5. **thread_clustering** (`classify`, single, batched) -- Groups L1 topics into 6-12 thematic threads.
6. **thread_narratives** (`synthesize`, forEach threads) -- Synthesizes each thread's topics into L2 nodes with temporal authority ordering.
7. **upper_layers** (`synthesize`, recursive_pair) -- Recursive pairing from L2 to apex.

### code.yaml

Seven-step pipeline:

1. **mechanical_metadata** (`detect`, mechanical) -- Extracts import graph, IPC bindings, spawn counts, string resources, complexity metrics.
2. **l0_code_extract** (`extract`, forEach chunks) -- Per-file LLM code analysis.
3. **l0_config_extract** (`extract`, forEach chunks, conditional) -- Config file variant with separate prompt.
4. **import_graph_clustering** (`classify`, mechanical) -- BFS connected components on the import graph, max cluster size 8.
5. **l1_code_group_synthesis** (`synthesize`, forEach clusters) -- Per-cluster synthesis with import graph and IPC context.
6. **l2_thread_clustering** (`classify`, single, batched) -- Groups L1 topics into threads.
7. **upper_layer_synthesis** (`synthesize`, recursive_pair) -- Recursive pairing to apex.

### document.yaml

Six-step pipeline:

1. **l0_doc_extract** (`extract`, forEach chunks) -- Per-document LLM extraction.
2. **entity_overlap_clustering** (`classify`, mechanical) -- Greedy entity overlap clustering.
3. **l1_doc_group_synthesis** (`synthesize`, forEach clusters) -- Per-cluster synthesis.
4. **l2_thread_clustering** (`classify`, single, batched) -- Thread clustering.
5. **l2_thread_narrative** (`synthesize`, forEach threads) -- Thread narrative synthesis.
6. **upper_layer_synthesis** (`synthesize`, recursive_pair) -- Recursive pairing to apex.

All three chains share the thread clustering, thread narrative, and upper layers steps (same prompts, same logic). The differentiation is in L0 extraction and L1 grouping strategy.

---

## 14. Future: Vine + Delta + Meta (Wave 2)

### Vine

The vine is a universal meta-pyramid that nests existing pyramids together. A vine's L0 is assembled from the apex + penultimate layer of each source pyramid, then clustered and synthesized upward.

Key differences from standard chains:
- No chunk ingestion. Source data comes from existing pyramids.
- Source pyramids are registered via `vine_sources` (vine_slug -> source_slug[]).
- Adding/removing a source pyramid triggers incremental rebuild.
- Vine-specific intelligence passes: ERAs, entity resolution, decisions, thread continuity, corrections.

`vine.yaml` will be a chain template with vine-specific lifecycle extensions.

### Delta

Delta chains handle incremental updates: when new chunks are added to an existing pyramid, the delta chain rebuilds only the affected nodes rather than the entire pyramid. Requires:
- Change detection (which chunks are new/modified).
- Surgical node replacement without full rebuild.
- Thread/delta/distillation side effects.

### Meta

Meta chains emit `META-*` nodes: metadata overlays on existing pyramids (e.g., entity directories, decision logs, correction tracking). These run after the main build and read the completed pyramid to produce supplementary structures.

---

## 15. Future: Wire Sync

Chains will synchronize to and from the Wire network:

- **Upload:** A local chain definition (YAML + prompts) is serialized and published to the Wire for other agents to discover and use.
- **Download:** An agent retrieves a chain from the Wire network, validates it locally, and registers it as a variant.
- **Versioning:** Chain versions track compatibility. The Wire enforces schema_version compatibility.

This enables distributed chain authoring: one agent designs a pipeline, publishes it, and other agents adopt it for their pyramids.

---

## 16. Future: Frontend (Wave 3)

### Phase 6: Chain Manager

List all available chains (defaults + variants). Assign chains to pyramids. Duplicate a default to create a variant.

### Phase 7: Chain Editor

Two-panel step editor: left panel shows the step list with drag-to-reorder; right panel shows the selected step's configuration (primitive, instruction, model, error handling). Prompt files are editable inline.

### Phase 8: Schema Export/Import

The agent handoff UX:

- **"Copy Schema for Agent"** -- serializes the chain definition + schema documentation into a markdown block that an LLM agent can parse and modify.
- **"Paste Chain from Agent"** -- textarea accepting YAML with real-time validation feedback.
- Named variants are saved and assignable per-pyramid.
