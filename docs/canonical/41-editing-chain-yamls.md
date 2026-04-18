# Editing chain YAMLs

A **chain** is a YAML file that defines how a pyramid gets built. It lists the steps, the order, the primitives, the prompts, the iteration modes, and the model tier per step. Every pyramid build runs a chain; editing a chain changes how future builds behave.

Chains are the primary way you customize Wire Node's build behavior. The binary is a dumb executor; the intelligence is in the YAML + markdown prompt files.

This doc reflects the **shipped state as of 2026-04** — specifically what the chain executor currently runs on. Where something is planned-but-not-shipped, it's marked.

---

## Current state of the chains

Since 2026-04-07, **all content types route through `question.yaml`** (the `question-pipeline` chain). The earlier per-content-type defaults (`code.yaml`, `document.yaml`, `conversation.yaml`) are deprecated but kept for parity testing. If you open `chains/defaults/code.yaml` the first thing you'll see is a `⚠ DEPRECATED — DO NOT USE FOR NEW BUILDS ⚠` banner.

Practically that means:

- Fresh builds of any content type use `question-pipeline`.
- Customizing build behavior means customizing `question.yaml` (or a variant of it).
- The legacy content-type chains still work if you explicitly opt in via `pyramid_chain_assignments`, but that's primarily for regression testing.

There is also a feature flag — `use_chain_engine` in `pyramid_config.json`. When false (the historical default on fresh installs), builds go through legacy hardcoded pipelines in `build.rs` instead of the chain executor. **Planned but not enabled by default:** `use_chain_engine: true` as the default on fresh installs. For now, if you want chain-based builds on a fresh install, set the flag yourself:

```bash
CONFIG="$HOME/Library/Application Support/wire-node/pyramid_config.json"
python3 -c "import json; c=json.load(open('$CONFIG')); c['use_chain_engine']=True; json.dump(c, open('$CONFIG','w'), indent=2)"
# Then restart Wire Node.
```

---

## Where chains live

```
chains/
├── CHAIN-DEVELOPER-GUIDE.md      — the authoritative quick reference. Read it.
├── defaults/
│   ├── question.yaml             — canonical chain for ALL content types
│   ├── code.yaml                 — deprecated (kept for parity testing)
│   ├── document.yaml             — deprecated
│   ├── conversation.yaml         — deprecated
│   ├── topical-vine.yaml         — vine pyramid orchestrator
│   ├── extract-only.yaml         — extract-only variant (no synthesis)
│   └── ...
├── variants/                     — your variants
└── prompts/
    ├── question/                 — prompts used by question.yaml
    ├── code/, document/, conversation/, shared/, vine/, generation/, migration/, planner/
    └── ...
```

Chains are loaded from the source tree (`agent-wire-node/chains/`) into the runtime data directory on app restart (dev mode) or on update (production). Editing the source tree and restarting picks up your changes.

A **chain variant** is a copy of a default with your edits. Variants can be assigned per-pyramid or globally.

---

## The anatomy of a chain

Top-level fields:

```yaml
schema_version: 1
id: question-pipeline                 # unique chain ID (referenced by pyramid_chain_assignments)
name: Question Pipeline
description: "..."
content_type: question                # content type this chain handles
version: "2.0.0"
author: wire-node

defaults:
  model_tier: synth_heavy
  temperature: 0.3
  on_error: "retry(2)"

steps:
  - name: step_name
    primitive: extract
    instruction: "$prompts/question/source_extract.md"
    # ... step-specific fields
```

The `defaults` block applies to every step unless overridden. The `steps` block is the ordered list.

---

## Step fields

Every step has a `name` and a `primitive`. Most have an `instruction` (path to a prompt file). Beyond that, a rich vocabulary of fields controls execution, batching, model selection, and output shape.

### Core fields

| Field | Purpose |
|---|---|
| `name` | Unique within the chain. Used in references (`$step_name`). |
| `primitive` | Semantic intent (see below). |
| `instruction` | Path to the prompt markdown, prefixed with `$prompts/...`. |
| `input` | Map of named values to pass to the prompt template. |
| `save_as` | `node` (saves as pyramid node), `web_edges` (saves as edge list), `step_only` (internal — referenceable but not a node). |
| `when` | Conditional expression — skip step if falsy. |

### Iteration

| Field | Purpose |
|---|---|
| `for_each` | Iterate over an array. E.g. `for_each: "$chunks"`. |
| `sequential` | Bool — run iterations serially (default is parallel up to `concurrency`). |
| `concurrency` | Max parallel LLM calls per step. Default 1. |
| `dispatch_order` | Ordering hint for parallel dispatch: `"largest_first"`, `"smallest_first"`. |
| `on_error` | `"retry(N)"`, `"skip"`, `"abort"`. |
| `on_parse_error` | `"heal"` (call a healing prompt) or absent. |
| `heal_instruction` | Prompt path for healing on parse error. |

### Input shaping

| Field | Purpose |
|---|---|
| `input` | Map of variables to pass into the prompt. Values can be `$variable` references. |
| `item_fields` | **Field projection.** Only send these fields per item to the LLM. Dramatically reduces token cost when sources are large. |
| `compact_inputs` | Webbing-specific compaction — strip to node_id, headline, entities. |
| `dehydrate` | List of `{drop: field_name}` ops — drops named fields from inputs when budget is tight. |
| `header_lines` | For extract steps, only send first N lines of source. |

### Batching

| Field | Purpose |
|---|---|
| `batch_size` | Target items per batch. Balanced batching — 127 items with batch_size 100 becomes 2 batches of 64+63, not 100+27. |
| `batch_max_tokens` | Token-aware greedy batching — fill batches up to this many tokens (estimated as `json_len / 4`). |
| `max_input_tokens` | Hard cap on inputs per call. Used with `split_strategy` for oversize handling. |
| `split_strategy` | `"lines"`, `"sections"`, or absent. How to split oversized inputs. |
| `split_overlap_tokens` | Overlap between splits. |
| `split_merge` | Bool — whether to merge split results back into one node. Default `false` in newer chains. |
| `merge_instruction` | Prompt for merging split results if `split_merge: true`. |

### Model selection

| Field | Purpose |
|---|---|
| `model_tier` | A tier name that maps via pyramid config (see [`50-model-routing.md`](50-model-routing.md)). |
| `model` | Explicit model override, e.g. `"inception/mercury-2"`. |
| `temperature` | 0.0–1.0. |

Tier names are **flexible strings** — not a fixed set. Common tiers in current chains: `extractor`, `synth_heavy`, `web`, `mid`, `low`, `high`, `max`. The mapping from tier name to `(provider, model)` lives in the tier routing table.

### Node output

| Field | Purpose |
|---|---|
| `save_as: node` | Save result as a pyramid node. |
| `save_as: web_edges` | Save result as lateral edges between nodes. |
| `save_as: step_only` | Don't save as a node; reference by name in later steps. |
| `node_id_pattern` | ID template, e.g. `"Q-L0-{index:03}"`, `"C-L0-{index:03}"`, `"L{depth}-{index:03}"`. |
| `depth` | Integer — pyramid layer (0 = L0 evidence, 1+ = understanding). |

### Response schema

| Field | Purpose |
|---|---|
| `response_schema` | JSON Schema the LLM must conform to. If set, the request enforces structured output. |
| `cluster_response_schema` | Separate schema for the clustering sub-call inside `recursive_cluster`. Must include `apex_ready: boolean`. |

### Recursive clustering (the main convergence mode)

This is how pyramids reach apex — an LLM-driven loop where the model says `apex_ready: true` when further grouping would only hurt clarity.

| Field | Purpose |
|---|---|
| `recursive_cluster` | `true` enables the loop. |
| `cluster_instruction` | Prompt for the clustering sub-call. |
| `cluster_item_fields` | Field projection for clustering. |
| `cluster_response_schema` | Schema for cluster response (must include `apex_ready`). |
| `direct_synthesis_threshold` | Skip clustering when node count ≤ this. `null` = trust `apex_ready` only. |
| `convergence_fallback` | `"retry"`, `"force_merge"`, `"abort"` — what to do if clusters don't converge. |
| `cluster_on_error` | Error strategy for the cluster sub-call. |
| `cluster_fallback_size` | Chunk size for `force_merge` fallback. |

### Instruction variants

| Field | Purpose |
|---|---|
| `instruction_map` | Override prompt by file type / extension / content type. Keys like `"type:config"`, `"extension:.tsx"`, `"content_type:conversation"`. |

### Accumulation (for sequential `for_each`)

| Field | Purpose |
|---|---|
| `accumulate.field` | Name of accumulator variable. |
| `accumulate.init` | Initial value. |
| `accumulate.max_chars` | Trim when exceeding. |
| `accumulate.trim_to` | Trim down to this. |
| `accumulate.trim_side` | `"start"` or `"end"`. |

---

## Primitives

A primitive declares what a step is doing. Two classes:

### Core primitives

| Primitive | Purpose | Input | Output |
|---|---|---|---|
| `extract` | Analyze a single item, produce structured node data. | One item (chunk, doc, message) | JSON with headline, orientation, topics |
| `classify` | Group or categorize items. | Array of items | JSON with threads/clusters/categories |
| `synthesize` | Merge multiple nodes into a higher-level node. | Array of nodes | JSON with headline, orientation, topics |
| `web` | Find cross-references between sibling nodes. | Array of nodes | List of edges with `source`, `target`, `relationship`, `strength` |
| `compress` | Sequential compression with running context. | One chunk + accumulator | Compressed representation |
| `fuse` | Merge two step outputs (e.g. forward + reverse). | Paired items | Fused node |

### Recipe primitives (orchestration — do not take an `instruction`)

These trigger specialized executor paths rather than direct LLM calls. They are shipped and used throughout `question.yaml`.

| Recipe primitive | What it does |
|---|---|
| `cross_build_input` | Loads prior build state into `$load_prior_state.*` — the gating mechanism for fresh-vs-delta builds. |
| `recursive_decompose` | Runs question decomposition against the apex question. `mode: delta` variant runs delta decomposition against an existing tree. |
| `build_lifecycle` | Build-wide lifecycle management (overlay cleanup, etc.). |
| `evidence_loop` | Runs the evidence answering cycle: pre-map candidates → answer with KEEP/DISCONNECT/MISSING. |
| `process_gaps` | Handles MISSING verdicts from evidence_loop (demand signal recording, optional gap-filling). |
| `container` | Composes a sub-sequence of steps inside one logical unit. |

**A note on current state:** these recipe primitives are **implemented in Rust** and invoked by name from the chain. They behave like built-ins you call but cannot rewrite in YAML. Moving them into expressible YAML (so that e.g. the evidence loop could itself be a chain you edit) is on the near-term roadmap but hasn't landed yet. Until it does, if you need to change how decomposition or evidence answering works, you change the prompts they reference (`$prompts/question/decompose.md`, `$prompts/question/pre_map.md`, `$prompts/question/answer.md`) — not the primitives themselves.

---

## Variables

**Built-in scalars:**

- `$chunks` — all content chunks.
- `$chunks_reversed` — chunks in reverse order.
- `$slug` — current pyramid slug.
- `$content_type` — `code` / `document` / `conversation` / `question` / `vine`.
- `$build_id` — the current build's unique ID.

**Characterization + question build params:**

- `$characterize` — content characterization result (audience, tone, type).
- `$audience` — audience framing derived from characterization.
- `$apex_question` — the apex question driving the build.
- `$granularity`, `$max_depth`, `$from_depth`, `$evidence_mode` — question build parameters.

**Prior build state** (populated by `cross_build_input`):

- `$load_prior_state.l0_count`, `.has_overlay`, `.overlay_answers`, `.question_tree`, `.unresolved_gaps`, `.l0_summary`, `.is_cross_slug`, `.referenced_slugs`, `.evidence_sets`, `.source_count`.

**Step output references:**

- `$step_name` — the step's output object.
- `$step_name.nodes` — array of produced nodes.
- `$step_name.output.distilled` — a specific field.

**Loop variables:**

- `$item` — current item in a `for_each` loop (or current batch when batched).
- `$index` — zero-based iteration index.

**Template resolution in prompts:** prompts use `{{variable}}` syntax. Values come from the step's `input` map after `$variable` resolution.

---

## A real example

Here's a step from the current `question.yaml` with most features in play:

```yaml
- name: source_extract
  primitive: extract
  instruction: "$prompts/question/source_extract.md"
  instruction_map:
    content_type:conversation: "$prompts/conversation/source_extract_v2.md"
  for_each: "$chunks"
  when: "$load_prior_state.l0_count < $load_prior_state.source_count"
  dispatch_order: "largest_first"
  concurrency: 10
  node_id_pattern: "Q-L0-{index:03}"
  depth: 0
  save_as: node
  max_input_tokens: 80000
  split_strategy: "sections"
  split_overlap_tokens: 500
  split_merge: false
  on_error: "retry(3)"
  on_parse_error: "heal"
  heal_instruction: "$prompts/shared/heal_json.md"
  model_tier: extractor
```

This step:

1. Only runs if not all chunks have been extracted yet (the `when` condition).
2. Dispatches chunks to the extractor tier in parallel (concurrency 10), largest first.
3. Uses an alternate prompt for conversation content type.
4. Oversized chunks get split by sections with 500-token overlap, no merge.
5. On parse error, invokes a healing prompt.
6. Produces `Q-L0-000`, `Q-L0-001`, ... nodes at depth 0.

---

## Prompt rules

Prompts are markdown in `chains/prompts/*/`. Referenced in YAML as `$prompts/question/source_extract.md`. Key conventions (from `CHAIN-DEVELOPER-GUIDE.md`):

1. **End with `/no_think`** — suppresses extended chain-of-thought, gets straight to the JSON output.
2. **Specify the exact JSON output format** with an example object.
3. **Never prescribe counts or ranges** — "produce 3-5 clusters" is a violation. Say "let the material decide" and constrain only with "fewer groups than inputs."
4. **Cluster prompts must teach `apex_ready`** — the LLM should return `apex_ready: true` when further grouping would hurt clarity rather than help it.
5. **Work with whatever fields are projected.** If `item_fields` limits the input, the prompt should reflect the actual projected fields, not the full shape.

See [`42-editing-prompts.md`](42-editing-prompts.md) for detailed prompt authoring.

---

## Authoring workflow

1. Copy the default you want to change:
   ```bash
   cd ~/Library/Application\ Support/wire-node/chains/
   cp defaults/question.yaml variants/my-question.yaml
   ```
2. Edit the variant. Change `id:` to something unique (e.g. `my-question-pipeline-v1`).
3. Assign to a test pyramid. Either:
   - From the pyramid's detail drawer, pick your chain in the chain assignment field; or
   - Directly in the DB via `pyramid_chain_assignments(slug, chain_id)`.
4. Trigger a fresh build on the test pyramid.
5. Watch the Pyramid Surface. Drill nodes to inspect prompt/response in the node inspector.
6. Iterate. If the model's output is wrong, tune the prompt first; if the structure is wrong, tune the chain.
7. When it's good, publish via Tools → Publish. Others can pull it.

---

## Resume and idempotency

If a build crashes mid-way, the next invocation resumes. For each iteration the executor dual-checks: does the `pipeline_steps` row exist, and (if `save_as: node`) does the `pyramid_nodes` row exist? Three states:

- Both exist → skip.
- Step row exists, node missing → rebuild.
- No step row → execute normally.

For `recursive_cluster`, whole depth levels get skipped if they already have the expected count. Restarting a failed build is cheap.

---

## Gotchas

- **Unresolved variables are runtime errors, not warnings.** Test on a small pyramid first.
- **Deprecated chains still work** if explicitly assigned, but they haven't been updated for recent improvements. Prefer `question.yaml` as your starting base.
- **`use_chain_engine` defaults to `false`** on fresh installs (as of this writing). If you're editing chains and not seeing any effect, check the flag.
- **Tier names aren't validated.** A typo in `model_tier` silently falls back to the chain default. Watch the cost log for unexpected models.
- **Splitting with `split_merge: true` doubles your LLM cost** on oversized chunks. Newer chains prefer `split_merge: false` — each split becomes its own node.

---

## Where to go next

- [`chains/CHAIN-DEVELOPER-GUIDE.md`](../../chains/CHAIN-DEVELOPER-GUIDE.md) — the authoritative source-of-truth reference, ships with the app.
- [`42-editing-prompts.md`](42-editing-prompts.md) — the markdown prompts your chain references.
- [`43-assembling-action-chains.md`](43-assembling-action-chains.md) — composing chains via recipe primitives.
- [`50-model-routing.md`](50-model-routing.md) — how tier names resolve to real models.
- [`28-tools-mode.md`](28-tools-mode.md) — the UI for authoring and publishing chain variants.
