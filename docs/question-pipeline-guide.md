# Question Pipeline Guide — Recipe-as-Contribution System

This guide covers how the question pipeline works after the recipe-as-contribution refactor. The question pipeline builds knowledge pyramids driven by a user's question rather than mechanical document extraction.

## Architecture

The question pipeline is defined in `chains/defaults/question.yaml`. Unlike mechanical pipelines (document, code, conversation) where every step is an LLM call defined in YAML, the question pipeline uses **4 special primitives** that orchestrate complex multi-step operations internally:

```
Equipment (Rust primitives) ← what CAN'T change without a code release
Recipe (YAML chain)         ← what CAN be forked, customized, rewired
```

### The 4 Recipe Primitives

| Primitive | What it does | Reads from |
|-----------|-------------|------------|
| `cross_build_input` | Loads all prior build state from DB (evidence, overlays, question tree, gaps, L0 nodes) | DB only |
| `recursive_decompose` | Decomposes the apex question into a tree of sub-questions | `step.input` or context |
| `evidence_loop` | Orchestrates per-layer evidence answering (pre-map → answer → persist → reconcile) | `step.input` or context |
| `process_gaps` | Re-examines source files for MISSING verdicts from the evidence loop | `step.input` or context |

These primitives are **not LLM calls** — they're Rust functions that internally make multiple LLM calls, manage DB state, and orchestrate complex flows.

## Chain Flow

```
1. load_prior_state     (cross_build_input)  → loads DB state
2. enhance_question     (extract)            → expands user's brief question using corpus context
3. decompose            (recursive_decompose)→ fresh: builds question tree from scratch
   OR decompose_delta   (recursive_decompose)→ delta: evolves existing tree, reuses answers
4. extraction_schema    (extract)            → designs question-shaped extraction prompt
5. l0_extract           (extract, for_each)  → extracts from source files (only on fresh builds)
6. evidence_loop        (evidence_loop)      → per-layer answering
7. gap_processing       (process_gaps)       → targeted re-examination of gaps
```

### Fresh vs Delta Builds

- **Fresh build** (no prior overlay): `decompose` runs, `decompose_delta` is skipped
- **Delta build** (prior overlay exists): `decompose_delta` runs, `decompose` is skipped

Both write their output to the canonical alias `$decomposed_tree`, so all downstream steps work identically regardless of which path ran.

### The `mode` Field

The `decompose_delta` step has `mode: delta` which tells the `recursive_decompose` primitive to use delta logic. Without this field, it defaults to fresh decomposition.

## YAML Input Wiring

Each recipe primitive reads its inputs from `step.input` (the YAML `input:` block). This makes the chain **forkable** — you can rename steps, rewire refs, and the primitives follow the wiring.

Example from question.yaml:
```yaml
- name: evidence_loop
  primitive: evidence_loop
  input:
    question_tree: "$decomposed_tree"
    extraction_schema: "$extraction_schema"
    load_prior_state: "$load_prior_state"
    reused_question_ids: "$reused_question_ids"
    build_id: "$build_id"
```

The primitive resolves each input field via the chain's variable resolution system (`$step_name` → step output, `$variable` → initial_params). If `step.input` is absent, the primitive falls back to hardcoded context refs for backward compatibility.

### Initial Params (from build_runner.rs)

These are injected into the chain context before execution starts:

| Param | Source |
|-------|--------|
| `$apex_question` | User's question |
| `$granularity` | Decomposition granularity (1-5) |
| `$max_depth` | Maximum tree depth |
| `$characterize` | Pre-computed characterization string |
| `$audience` | Target audience (from characterization) |
| `$content_type` | Slug content type |
| `$is_cross_slug` | Whether this references other slugs |
| `$referenced_slugs` | List of referenced slug names |
| `$build_id` | Build tracking ID (qb-XXXX) |

### Canonical Aliases

| Alias | Written by | Used by |
|-------|-----------|---------|
| `$decomposed_tree` | Both `decompose` and `decompose_delta` | `extraction_schema`, `evidence_loop` |

## Prompt Files

All prompts live in `chains/prompts/question/`:

| File | Used by | What it does |
|------|---------|-------------|
| `enhance_question.md` | enhance_question step | Expands brief question using corpus context |
| `decompose.md` | recursive_decompose (fresh) | Template for LLM decomposition calls |
| `decompose_delta.md` | recursive_decompose (delta) | Template for delta decomposition |
| `extraction_schema.md` | extraction_schema step | Designs question-shaped extraction prompt |
| `pre_map.md` | evidence_loop (internal) | Maps questions to candidate evidence nodes |
| `pre_map_stage1.md` | evidence_loop (internal) | Stage 1 of two-stage mapping for large evidence |
| `answer.md` | evidence_loop (internal) | Synthesizes evidence into answers |
| `synthesis_prompt.md` | evidence_loop (internal) | Generates per-layer synthesis prompts |
| `horizontal_review.md` | recursive_decompose (internal) | Reviews sibling questions for merges |
| `targeted_extract.md` | process_gaps (internal) | Re-examines files for missing evidence |

### Prompt Input Format

For steps using the `extract` primitive, the LLM receives:
- **System prompt**: the resolved prompt file content (from `instruction:`)
- **User prompt**: the `step.input` object serialized as pretty-printed JSON

So if your prompt says "You will receive a JSON object with..." — that's exactly what happens. The input fields from the YAML become JSON keys in the user prompt.

For `instruction_from` (used by `l0_extract`), the system prompt is the VALUE from a previous step's output field, not a file. The user prompt is the for_each item (a source file chunk).

## Forking a Chain

To create a custom question pipeline:

1. Copy `question.yaml` to a new file (e.g., `question-deep.yaml`)
2. Give it a unique `id` (e.g., `question-deep-pipeline`)
3. Modify steps, add/remove steps, change wiring
4. Assign it to a slug via `chain_registry`

Key rules for forks:
- Recipe primitives require their specific `primitive:` value — you can't rename the primitive
- You CAN rename steps freely — just update the `$ref` wiring in downstream `input:` blocks
- Both decompose paths must exist if you want fresh + delta support
- The `$decomposed_tree` alias is written by the primitive, not the YAML — it always works
- `mode: delta` on the delta step is required (not the step name)

## Build Tracking

Build tracking wraps the entire chain execution:
- `save_build_start` before chain starts (build_id injected as `$build_id`)
- `complete_build` on success
- `fail_build` on error
- The evidence_loop reads `$build_id` from input so it doesn't create duplicate build records

## Live Build Visualization

Question builds emit `LayerEvent`s for the PyramidBuildViz component via `layer_tx`. The channel is created in the IPC/HTTP entry points and threaded through `run_decomposed_build` → `execute_chain_from`.

## Testing

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"

# Create a fresh question build
curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST localhost:8765/pyramid/slugs \
  -d '{"slug":"test-q","content_type":"code","source_path":"/path/to/source"}'

curl -s -H "$AUTH" -X POST localhost:8765/pyramid/test-q/ingest

curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST localhost:8765/pyramid/test-q/build/question \
  -d '{"question":"What is this and how is it organized?","granularity":3,"max_depth":3}'

# Poll status
curl -s -H "$AUTH" localhost:8765/pyramid/test-q/build/status

# Verify
curl -s -H "$AUTH" localhost:8765/pyramid/test-q/apex
curl -s -H "$AUTH" localhost:8765/pyramid/test-q/drill
```

Success criteria:
- Build completes with status "complete"
- Nodes exist at depth > 0
- Apex is reachable via drill
- Build history shows a `qb-XXXX` record
