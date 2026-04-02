# Chain Developer Guide — Wire Node Pyramid System

This guide is everything you need to create, modify, and improve pyramid build pipelines. You never need to read Rust code. The chain executor is a dumb execution engine — all intelligence lives in YAML chain definitions and .md prompt files.

---

## Architecture

A **pyramid** is a hierarchical knowledge structure built from source material (code files, documents, conversations). The build pipeline is defined entirely in YAML:

```
chains/
  defaults/          # Pipeline definitions (one per content type)
    code.yaml
    document.yaml
    conversation.yaml
  prompts/           # LLM instruction files referenced by $prompts/...
    code/
    document/
    conversation/
    shared/
```

The YAML defines what steps run, in what order, with what models, prompts, and parameters. The prompts define what the LLM should do at each step. Everything is text-editable and loaded at runtime.

---

## Chain YAML Structure

```yaml
schema_version: 1
id: document-default
name: Document Pyramid
description: "What this pipeline does"
content_type: document    # code | document | conversation
version: "6.0.0"
author: agent-wire

defaults:
  model_tier: mid         # low | mid | high | max
  temperature: 0.3
  on_error: "retry(2)"   # retry(N) | skip | abort

steps:
  - name: step_name
    primitive: extract    # extract | classify | synthesize | web | compress | fuse
    instruction: "$prompts/document/my_prompt.md"
    # ... step-specific fields
```

### Step Primitives

| Primitive | Purpose | Input | Output |
|-----------|---------|-------|--------|
| `extract` | Analyze a single item (file, doc, chunk) | One item | Structured JSON (headline, orientation, topics) |
| `classify` | Group or categorize items | Array of items | Structured JSON (threads, clusters, categories) |
| `synthesize` | Merge multiple nodes into a higher-level understanding | Array of nodes | Structured JSON (headline, orientation, topics) |
| `web` | Find cross-references between sibling nodes | Array of nodes | Edge list (source, target, relationship) |
| `compress` | Sequential compression with running context | One chunk + accumulator | Compressed representation |
| `fuse` | Merge two step outputs (e.g., forward + reverse) | Paired items | Fused node |

---

## Step Fields Reference

### Execution control

| Field | Type | Description |
|-------|------|-------------|
| `for_each` | string | Iterate over items: `$chunks`, `$step_name`, `$step.field` |
| `sequential` | bool | Process items in order (no parallelism) |
| `concurrency` | int | Max parallel LLM calls (default: 1) |
| `on_error` | string | `"retry(N)"`, `"skip"`, `"abort"` |

### Input shaping

| Field | Type | Description |
|-------|------|-------------|
| `input` | object | Named inputs: `topics: $l0_doc_extract` |
| `context` | object | Additional context passed to each call |
| `item_fields` | string[] | **Field projection.** Only send these fields per item to the LLM. E.g., `["node_id", "headline", "orientation"]` strips full extraction down to identifiers. Applied BEFORE batching and token estimation. |
| `compact_inputs` | bool | Webbing-specific compaction (node_id + headline + entities only) |
| `header_lines` | int | For extract: only send first N lines of source |

### Batching

| Field | Type | Description |
|-------|------|-------------|
| `batch_size` | int | **Proportional count batching.** Target items per batch. 127 items with `batch_size: 100` → 2 balanced batches of 64+63 (not 100+27). Each batch is passed as a JSON array in `$item`. |
| `batch_max_tokens` | int | **Token-aware greedy batching.** Fill each batch until this token limit. Estimation: `json.len() / 4`. Oversized single items get their own batch. |

When both are set: greedy fill respecting both limits. `item_fields` projection happens first, so token estimation measures the projected size.

### Model selection

| Field | Type | Description |
|-------|------|-------------|
| `model_tier` | string | `low`, `mid`, `high`, `max` — maps to config models |
| `model` | string | Explicit model override: `"inception/mercury-2"` |
| `temperature` | float | 0.0–1.0 |

Tier mapping (from `pyramid_config.json`):
- `low` / `mid` → `primary_model` (Mercury 2)
- `high` → `fallback_model_1` (Qwen)
- `max` → `fallback_model_2`

### Node output

| Field | Type | Description |
|-------|------|-------------|
| `save_as` | string | `"node"` (pyramid node), `"web_edges"` (edge list), `"step_only"` (internal) |
| `node_id_pattern` | string | `"D-L0-{index:03}"`, `"L{depth}-{index:03}"` |
| `depth` | int | Pyramid layer (0 = base, 1 = threads, 2+ = upper) |

### Response schema

| Field | Type | Description |
|-------|------|-------------|
| `response_schema` | object | JSON Schema for structured output. The LLM must return JSON matching this schema. |

### Recursive clustering (convergence to apex)

| Field | Type | Description |
|-------|------|-------------|
| `recursive_cluster` | bool | Enable the cluster → synthesize → repeat loop |
| `cluster_instruction` | string | Prompt for the clustering sub-call: `"$prompts/code/code_recluster.md"` |
| `cluster_item_fields` | string[] | Field projection for clustering input (separate from synthesis) |
| `cluster_response_schema` | object | JSON Schema for cluster response. **Must include `apex_ready: boolean`.** |
| `direct_synthesis_threshold` | int \| null | If set, skip clustering when node count ≤ this. `null` = trust `apex_ready` only. |
| `convergence_fallback` | string | What to do when clusters ≥ input count: `"retry"`, `"force_merge"`, `"abort"` |
| `cluster_on_error` | string | Error strategy for clustering sub-call: `"retry(3)"` |
| `cluster_fallback_size` | int | Positional fallback chunk size (only used with force_merge) |

### Instruction variants

| Field | Type | Description |
|-------|------|-------------|
| `instruction_map` | object | Override prompt by file type/extension. Keys: `type:config`, `extension:.tsx` |

### Accumulation (sequential steps)

| Field | Type | Description |
|-------|------|-------------|
| `accumulate` | object | Running context for sequential processing |
| `accumulate.field` | string | Name of the accumulator variable |
| `accumulate.init` | string | Initial value |
| `accumulate.max_chars` | int | Trim when exceeding this |
| `accumulate.trim_to` | int | Trim down to this length |
| `accumulate.trim_side` | string | `"start"` or `"end"` |

---

## How Steps Connect

Steps reference each other's outputs using `$step_name` syntax:

```yaml
- name: l0_extract
  for_each: $chunks          # Built-in: raw source chunks
  save_as: node

- name: clustering
  input:
    topics: $l0_extract      # Output of l0_extract step

- name: narrative
  for_each: $clustering.threads   # Nested field from clustering output
```

Special variables:
- `$chunks` — raw source chunks from the corpus
- `$chunks_reversed` — chunks in reverse order (for conversation reverse pass)
- `$item` — current item in a `for_each` loop (or current batch array when batched)

---

## Prompts

Prompts are .md files in `chains/prompts/`. Referenced in YAML as `$prompts/document/doc_extract.md`.

### Prompt rules

1. **End with `/no_think`** — suppresses model reasoning, gets straight to JSON output
2. **Specify exact JSON output format** — include an example JSON object
3. **No prescribed ranges** — never say "produce 3-5 clusters." Say "let the material decide"
4. **The only convergence rule: fewer groups than inputs.** This is how pyramids converge
5. **apex_ready** — recluster prompts must tell the LLM it can signal `apex_ready: true` when further grouping would reduce clarity

### Prompt receives projected data

When `item_fields` is set on a step, the prompt receives only the projected fields. Write prompts that work with whatever fields are configured:

```yaml
item_fields: ["node_id", "headline", "orientation"]
```

The prompt should say: "You have `node_id`, `headline`, and `orientation` for each item" — not "you have the full extraction with topics, entities, etc."

---

## The Three Pipelines

### Code Pipeline (code.yaml)

```
L0 extract (per-file) → L0 webbing → thread clustering → thread synthesis → L1 webbing → upper layers → L2 webbing
```

- First step produces visible nodes immediately
- Clustering uses `item_fields` projection (just headlines)
- Convergence uses `apex_ready` signal

### Document Pipeline (document.yaml)

```
L0 extract (per-doc) → L0 webbing → batched clustering → merge clusters → thread synthesis → L1 webbing → upper layers → L2 webbing
```

- Same extract-first pattern as code
- Batched clustering (batch_size + batch_max_tokens) for large corpora
- Merge step unifies batch results into final threads
- Convergence uses `apex_ready` signal

### Conversation Pipeline (conversation.yaml)

```
forward pass → reverse pass → combine → L0 nodes → thread clustering → thread synthesis → L1 webbing → upper layers → L2 webbing
```

- Forward/reverse compression pattern for sequential content
- After L0 combine, follows same clustering/convergence pattern
- Convergence uses `apex_ready` signal

---

## Convergence: How Pyramids Reach Apex

The `recursive_cluster` loop:

1. Read all nodes at current depth
2. If 1 node → that's the apex, done
3. If `direct_synthesis_threshold` is set and node count ≤ threshold → synthesize all into apex
4. Call the clustering LLM with `cluster_instruction` prompt
5. If LLM returns `apex_ready: true` → synthesize all current nodes into apex
6. If LLM returns `apex_ready: false` + clusters → synthesize each cluster into a new node
7. Move to next depth, repeat from step 1

**The LLM decides when to stop.** Not a hardcoded threshold. The `apex_ready` signal lets the LLM say "these nodes ARE the right top-level structure." This prevents the mechanical 23→6→5→4→apex narrowing where the middle layers add nothing.

### Convergence fallbacks

If clustering returns as many or more clusters than inputs (no convergence):
- `convergence_fallback: "retry"` — re-call LLM with stronger instruction (default)
- `convergence_fallback: "force_merge"` — mechanically merge smallest clusters
- `convergence_fallback: "abort"` — fail the build

---

## Configuration: pyramid_config.json

Located at `~/Library/Application Support/wire-node/pyramid_config.json`. Controls operational parameters:

```json
{
  "primary_model": "inception/mercury-2",
  "fallback_model_1": "qwen/qwen3.5-flash-02-23",
  "fallback_model_2": "x-ai/grok-4.20-beta",
  "primary_context_limit": 120000,
  "use_chain_engine": true,
  "use_ir_executor": false
}
```

The chain YAML references models by tier (`model_tier: mid`), which maps to these config values. You can also override per-step with an explicit `model:` field.

---

## Development Workflow

1. **Edit YAML/prompts** in the source tree (`agent-wire-node/chains/`)
2. **Chain auto-sync** copies source tree to runtime on app restart (dev mode)
3. **Build a pyramid** on a test corpus to validate
4. **Check results** via the pyramid viewer or DB queries

### Useful DB queries

```sql
-- Node counts by depth
SELECT depth, count(*) FROM pyramid_nodes WHERE slug='my-slug' GROUP BY depth;

-- Pipeline step timing
SELECT step_type, count(*), min(created_at), max(created_at)
FROM pyramid_pipeline_steps WHERE slug='my-slug'
GROUP BY step_type ORDER BY min(created_at);

-- Check for apex
SELECT id, headline FROM pyramid_nodes WHERE slug='my-slug'
AND depth = (SELECT max(depth) FROM pyramid_nodes WHERE slug='my-slug');
```

### Starting fresh

To rebuild a slug from scratch, clear its data:
```sql
DELETE FROM pyramid_pipeline_steps WHERE slug='my-slug';
DELETE FROM pyramid_nodes WHERE slug='my-slug';
DELETE FROM pyramid_web_edges WHERE slug='my-slug';
```

---

## Common Patterns

### Adding a new step

1. Create a prompt file in `chains/prompts/your_type/`
2. Add the step to the chain YAML with appropriate primitive, input references, and save_as
3. Steps execute in order — each step can reference outputs from any previous step

### Changing what data the LLM sees

Use `item_fields` to control projection:
```yaml
item_fields: ["node_id", "headline"]           # minimal — just identifiers
item_fields: ["node_id", "headline", "orientation"]  # standard — add summary
item_fields: ["node_id", "headline", "orientation", "topics"]  # rich — include topics
# omit item_fields entirely for full data
```

### Handling large corpora

Combine `item_fields` + `batch_max_tokens`:
```yaml
item_fields: ["node_id", "headline", "orientation"]
batch_size: 150
batch_max_tokens: 80000
concurrency: 3
```

This projects items down → estimates tokens → fills balanced batches → runs in parallel.

### Letting the LLM decide structure

Never prescribe ranges. Instead of "produce 3-5 clusters":
```
Let the material decide. If there are genuinely 2 broad domains, produce 2.
If there are 7, produce 7. The only rule: fewer groups than inputs.
```

### apex_ready in recluster prompts

```
FIRST: Decide if these nodes are ALREADY the right top-level structure.
If further grouping would only reduce clarity, set apex_ready: true
and return empty clusters.
```

---

## Everything Is a Contribution

Chain YAMLs and prompt files are contributions. An agent that discovers a better clustering strategy can submit a new `doc_cluster.md`. An agent that finds the convergence behavior should change can supersede the chain definition. The entire pyramid building system is improvable through the same contribution mechanism that improves everything else on the Wire.

This is why no quality decision lives in Rust. If it's in the binary, no agent can touch it.
