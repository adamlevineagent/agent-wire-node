# Chain System Reference

Updated 2026-04-02. Reflects the current state after the everything-to-YAML refactor,
sub-chain primitives, adaptive dehydration, build visualization, rate limiting,
direct Inception API, self-healing parse failures, and tiktoken tokenization.

---

## Overview

The pyramid build pipeline is a **YAML-driven chain executor**. You define
what you want in a YAML file; Rust executes it. The executor is a dumb engine
that reads config and does what it says. Every decision that shapes pyramid
quality lives in YAML/prompts — improvable by any agent through the Wire's
contribution model.

**Files that matter:**
```
chains/defaults/         ← YAML chain definitions (code.yaml, document.yaml, conversation.yaml)
chains/prompts/          ← Markdown prompt files referenced from YAML
chains/questions/        ← Question set YAML files
src-tauri/src/pyramid/
  chain_engine.rs        ← ChainStep struct, DehydrateStep, YAML deserialization, validation
  chain_executor.rs      ← Main execution loop, all primitives, batching, splitting, sub-chains
  chain_dispatch.rs      ← LLM calls, model resolution, StepContext
  chain_resolve.rs       ← ChainContext, $ref resolution, {{template}} substitution
  llm.rs                 ← LLM client, model cascade, tiktoken tokenizer, JSON extraction
  build_runner.rs        ← Build entry point, rate limiting
  mod.rs                 ← PyramidState, OperationalConfig (Tier1/2/3), PyramidConfig
```

---

## Chain Sync (how YAML gets to the app)

Two-tier strategy in `chain_loader.rs`:

| Scenario | Behavior |
|----------|----------|
| Dev mode (source tree exists) | Source tree `chains/` → data dir, always overwrite |
| Release, first run | Embedded defaults → data dir (bootstrap) |
| Release, subsequent runs | Keep existing runtime files |

In dev mode, `chains_dir` points directly to the source tree — prompts are read live.
No manual rsync needed after prompt changes.

---

## Primitives

### LLM Primitives (make model calls)

| Primitive | Typical dispatch | Description |
|-----------|-----------------|-------------|
| `extract` | `execute_for_each` | Per-item LLM call, typically saves as node |
| `compress` | `execute_for_each` | Sequential with accumulate |
| `fuse` | `execute_for_each` | Zips outputs from prior steps |
| `classify` | `execute_for_each` or `execute_single` | Categorization / clustering |
| `synthesize` | `execute_for_each` or `dispatch_group` | Distillation / narrative synthesis |
| `web` | `execute_web_step` | Cross-reference edges between sibling nodes |

### Flow Control Primitives (no LLM calls)

| Primitive | Description |
|-----------|-------------|
| `container` | Groups inner steps into a sub-chain. `steps:` field required. |
| `split` | Splits text into chunks by sections/lines/tokens. No LLM call. |
| `loop` | Repeats inner steps until `until:` condition is met. |
| `gate` | Evaluates `when:` condition. Sets `break: true` to exit enclosing loop. |

### Execution Modes (flags on any step)

| Flag | Behavior |
|------|----------|
| `for_each: $ref` | Iterate items, dispatch per item (with concurrency) |
| `recursive_cluster: true` | Cluster → synthesize → repeat until apex |
| `recursive_pair: true` | Pair adjacent → repeat until apex (legacy) |
| `pair_adjacent: true` | Single pair pass, one layer |
| `mechanical: true` | Dispatch to `rust_function`, no LLM |

---

## Batching & Projection

Three composable primitives that control what data goes to the LLM and how batches are sized:

### `item_fields` — uniform field projection
```yaml
item_fields: ["node_id", "headline", "orientation"]
```
Projects every item to only these fields. Supports dot-notation:
- `"topics.name"` → extract `name` from each element in `topics` array
- `"topics.name,entities"` → extract multiple sub-fields

### `dehydrate` — adaptive per-item projection
```yaml
dehydrate:
  - drop: "topics.current"
  - drop: "topics.entities"
  - drop: "topics.summary"
  - drop: "topics"
  - drop: "orientation"
```
Progressively strips fields from each item until it fits the batch token budget.
Small items stay fully hydrated. Large items get dehydrated. Items in the same
batch can have different hydration levels. Mutually exclusive with `item_fields`.

`drop_field` supports recursive dot-notation at any depth. Array parents get
each element processed; object parents get the child field removed directly.

### `batch_size` — count-based batching
```yaml
batch_size: 100
```
Proportionally balanced: 127 items / batch_size=100 → [64, 63] not [100, 27].

### `batch_max_tokens` — token-aware batching
```yaml
batch_max_tokens: 80000
```
Greedy fill until token limit. Uses tiktoken cl100k_base for accurate estimation.

### Composition order
```
items → project(item_fields OR dehydrate) → batch(batch_max_tokens OR batch_size) → dispatch
```

When `dehydrate` is set, it handles both projection AND batching (adaptive).
When `item_fields` is set, projection happens first, then batching separately.

---

## Sub-Chains

Steps can contain steps. The `steps:` field makes a step a container.

### Container (no for_each)
```yaml
- name: thread_clustering
  primitive: container
  steps:
    - name: batch_cluster
      primitive: classify
      for_each: $l0_doc_extract
      batch_max_tokens: 80000
    - name: merge_clusters
      primitive: classify
      input:
        batch_results: $batch_cluster
```
Output: last inner step's output, stored under the container's name.
`$thread_clustering.threads` resolves to the merge step's threads.

### Container (with for_each)
```yaml
- name: thread_narrative
  for_each: $thread_clustering.threads
  concurrency: 5
  save_as: node
  depth: 1
  steps:
    - name: batch_synth
      primitive: synthesize
      for_each: $item.assigned_docs
      batch_max_tokens: 60000
    - name: merge_thread
      primitive: synthesize
      input:
        parts: $batch_synth
```
Iterates items. For each item, runs the sub-chain. The container's `save_as`,
`node_id_pattern`, `depth` apply to each iteration's final output.

### Loop
```yaml
- name: upper_layers
  primitive: loop
  until: "count($current_nodes) <= 1"
  steps:
    - name: recluster
      primitive: classify
    - name: check_apex
      primitive: gate
      when: "$recluster.apex_ready == true"
      break: true
    - name: synthesize_clusters
      primitive: synthesize
      for_each: $recluster.clusters
      save_as: node
```
Repeats until condition met or gate breaks. Max 100 iterations (safety cap).

### Split (no LLM)
```yaml
- name: split_if_needed
  primitive: split
  input: $item.content
  max_input_tokens: 60000
  split_strategy: "sections"
  split_overlap_tokens: 500
```
Returns array of text chunks. Strategies: `sections` (markdown headers),
`lines`, `tokens`.

### Gate (no LLM)
```yaml
- name: check_apex
  primitive: gate
  when: "$recluster.apex_ready == true"
  break: true
```
Evaluates condition. `break: true` exits the enclosing loop.

---

## Oversized Chunk Splitting

For steps that process large documents:
```yaml
max_input_tokens: 80000
split_strategy: "sections"
split_overlap_tokens: 500
split_merge: true
```
If an item exceeds `max_input_tokens`, it's split by the strategy.
Sub-chunks get their own extraction calls. If `split_merge: true` (default),
results are merged back into one L0 node via a merge LLM call.

---

## Convergence Controls (recursive_cluster)

All formerly hardcoded decisions are now YAML-controlled:

| Field | Default | Description |
|-------|---------|-------------|
| `direct_synthesis_threshold` | None | When set, skip clustering if node count ≤ threshold. When None, rely on `apex_ready` signal only. |
| `convergence_fallback` | "retry" | What to do when clustering doesn't converge: "retry" (re-call LLM), "force_merge", "abort" |
| `cluster_on_error` | "positional(3)" | What to do when cluster LLM fails: "positional(N)", "retry(N)", "abort" |
| `cluster_fallback_size` | 3 | Group size for positional fallback |
| `cluster_item_fields` | None | Field projection for the clustering sub-call. When None, uses legacy hardcoded projection. |

The `apex_ready` signal: after each clustering call, if the response includes
`"apex_ready": true`, the executor skips further clustering and synthesizes
directly to apex.

---

## Expression Language (for `when` and `until`)

Simple predicate evaluator for flow control:

| Expression | Meaning |
|-----------|---------|
| `$ref` | Truthy check (non-null, non-empty, non-false) |
| `$ref > N` | Numeric comparison |
| `$ref == N` | Numeric equality |
| `$ref == true` | Boolean comparison |
| `count($ref)` | Array length |
| `count($ref) > 1` | Array length comparison |

Comparison operators: `>`, `>=`, `<`, `<=`, `==`, `!=`

---

## Variable Resolution (`chain_resolve.rs`)

### `$ref` resolution — YAML step inputs and context

| Pattern | Resolves to |
|---------|-------------|
| `$chunks` | Vec of all content chunks |
| `$slug` | Pyramid slug string |
| `$content_type` | "code", "document", "conversation" |
| `$item` | Current forEach item |
| `$index` | Current forEach item index |
| `$step_name` | Full step output |
| `$step_name.field.nested` | Dot-path into step output |
| `$step_name.arr[0]` | Array index (literal) |
| `$step_name.arr[$index]` | Array index from forEach index |
| `$running_context` | Current accumulator value |

Inside sub-chains: `$step_name` resolves within the child context first,
then falls back to the parent context.

### `{{variable}}` resolution — markdown prompt files
Reads from `resolved_input` (the JSON produced after step input resolution).
Dot-path supported: `{{data.summary}}`.

---

## LLM Client (`llm.rs`)

### Model cascade
Pre-flight token estimation (tiktoken cl100k_base) determines starting model:
- ≤ `primary_context_limit` → primary model (Mercury 2)
- ≤ `fallback_1_context_limit` → fallback 1 (Qwen)
- Above that → fallback 2 (Grok)

### Dynamic max_tokens
`max_tokens = model_context_limit - estimated_input_tokens`, capped at 48K, floored at 1024.
Works around OpenRouter counting max_tokens as reserved space.

### HTTP 400 handling
Only cascades to fallback model on context-exceeded 400s (body contains "context", "too many tokens", "token limit"). All other 400s retry on the same model with backoff.

### JSON extraction
Proper depth-tracking boundary finder: walks forward from first `{`, tracks depth,
skips everything inside quoted strings. Handles trailing commentary, nested braces
in string values, etc. Debug logging shows exact parse error location on failure.

### Direct Inception API
When `inception_api_key` is set in `pyramid_config.json`, Mercury 2 calls route
directly to `api.inceptionlabs.ai` instead of through OpenRouter. 100 RPM free tier,
10M tokens. Cascades back to OpenRouter for Qwen/Grok when context is exceeded.
Re-evaluated on each retry loop iteration so the URL/auth switch correctly on cascade.

### Rate Limiting
Shared token bucket rate limiter across all LLM dispatch. Configured via:
- `rate_limit_requests_per_minute` (default 30)
- `rate_limit_burst` (default 6)
- `rate_limit_jitter_ms` (default 500)

Prevents Cloudflare 403s from burst patterns. Jitter on backoff desynchronizes
retry storms. Acquired before each API call, released after response.

### Self-Healing Parse Failures
When an LLM returns malformed JSON, instead of retrying the full call:
1. Classify failure type: `Truncated`, `MarkdownWrapped`, `MalformedValue`, `NoJsonFound`
2. Send a small fast healing call to fix the broken output
3. If healing succeeds, use the fixed result
4. If healing fails after `heal_max_retries`, fall back to full retry

YAML control:
```yaml
on_parse_error: "heal"       # "heal" | "retry" | "extract_partial"
heal_model_tier: mid          # model for healing calls
heal_max_retries: 2           # attempts before full retry
```

### Config-driven (from `pyramid_config.json`)
All timeouts, retries, retryable status codes, model names, context limits
read from OperationalConfig. No hardcoded values in Rust.

---

## Build Visualization

The build screen shows a live pyramid:
- L0 at the bottom, upper layers appear above as discovered
- Individual cells light up as nodes complete
- Step indicator between layers ("Clustering documents...", "Cross-referencing...")
- Badge shows current step name or "Finishing L{n}"
- Log panel with auto-scroll

Backend: `LayerEvent` enum sent via mpsc channel to a drain task.
Frontend: `PyramidBuildViz.tsx` polls `pyramid_build_progress_v2` every 2s.

---

## Server Availability During Builds

Builds use their own SQLite reader connection (`with_build_reader()`).
The shared reader stays available for CLI/frontend queries.
All 5 build entry points (pyramid_build, pyramid_question_build,
handle_build, handle_question_build, vine bunch builds) use build-scoped readers.

---

## Operational Config

All tunable parameters live in `pyramid_config.json` under `operational`:

**Tier 1** (LLM): context limits, max tokens, temperatures, retries, timeouts,
retryable status codes, retry sleep, timeout formula constants

**Tier 2** (Pipeline): staleness threshold, token budgets, chunk target lines,
headline limits, watcher exclusion patterns, rename thresholds, dequeue caps,
rate limit windows, phase display duration

**Tier 3** (Delta/Collapse): collapse threshold, propagation depth, edge parameters,
supersession, staleness batching caps

---

## Adding New Capabilities

### New ChainStep field
1. Add to `ChainStep` struct in `chain_engine.rs` with `#[serde(default)]`
2. Update ALL test constructors (chain_engine, chain_dispatch, chain_executor, defaults_adapter)
3. Add IR metadata passthrough in `defaults_adapter.rs`
4. Add validation in `validate_chain` if the field has constraints
5. Add to `VALID_PRIMITIVES` if it's a new primitive name

### New enrichment
1. Add string to `enrichments:` list in step YAML
2. In `enrich_for_each_step_input()` or `enrich_group_extra_input()`, add handler
3. No step-name hardcoding — YAML declaration controls when it runs

### New config field
1. Add to appropriate tier in `mod.rs` with `#[serde(default = "...")]`
2. Add default function
3. Update `Default` impl
4. If it goes on `LlmConfig`, also update `to_llm_config()` in `PyramidConfig`
