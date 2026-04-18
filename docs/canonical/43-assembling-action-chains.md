# Assembling action chains

One chain runs one build. When a build needs to orchestrate multiple phases — decompose the question, extract evidence, answer, synthesize, reconcile — the orchestration happens inside a single chain using **recipe primitives** and the `container` primitive.

This doc covers how compositional chains are built in the shipped executor, the recipe primitives that drive specialized phases, and how vines compose builds across multiple pyramids. It also notes where planned-but-not-yet-shipped composition features (notably `invoke_chain`) are headed.

---

## Current state of composition

**What's shipped:**

- Recipe primitives (`cross_build_input`, `recursive_decompose`, `build_lifecycle`, `evidence_loop`, `process_gaps`) that trigger specialized executor paths within one chain.
- The `container` primitive for grouping a sub-sequence of steps into one logical unit.
- **`invoke_chain`** as a step field that calls another named chain with scoped inputs and merges outputs back — a chain can cleanly delegate to another chain by id.
- Cross-pyramid composition via vine chains — a vine pyramid's build triggers child pyramid builds.
- Cross-build input via `cross_build_input` — a chain can load prior build state (nodes, evidence, question tree) from the same slug and decide fresh-vs-delta behavior.

The shipped composition vocabulary is rich: one chain with recipe primitives for specialized phases plus `invoke_chain` for delegating to other chains. The question-pipeline chain leans on recipe primitives; larger compositional pipelines (vines, custom workflows) use `invoke_chain` to compose smaller chains.

---

## Recipe primitives: composition inside a chain

Recipe primitives don't take an `instruction` — they trigger executor paths. Each one handles a named phase of the build. Together they are the composition vocabulary of the shipped chain executor.

**A note on current state:** recipe primitives are **implemented in Rust**. A chain invokes them by name and passes inputs via the `input` map, but the primitive's internals aren't expressible in YAML. Moving them into chains you can edit — so that e.g. the evidence loop could itself be a published chain variant — is on the near-term roadmap. Until it lands, treat recipe primitives as built-in phases you call into, and shape their behavior by editing the prompts they reference.

### `cross_build_input`

Loads prior build state into `$load_prior_state.*`. This is the gating mechanism for fresh-vs-delta builds.

```yaml
- name: load_prior_state
  primitive: cross_build_input
  save_as: step_only
```

After this step, later steps can gate on things like:

```yaml
- name: source_extract
  when: "$load_prior_state.l0_count < $load_prior_state.source_count"
  # ...
```

There's nothing magic about `cross_build_input` — it's a recipe that reads prior state into context, exposed through `$load_prior_state.*`. But it's the canonical "do we need to do fresh extraction or incremental work" decision point in a chain.

### `recursive_decompose`

Runs question decomposition against an apex question, producing `$decomposed_tree`. Has two modes:

```yaml
- name: decompose
  primitive: recursive_decompose
  instruction: "$prompts/question/decompose.md"
  when: "$load_prior_state.has_overlay == false"
  input:
    apex_question: "$apex_question"
    granularity: "$granularity"
    max_depth: "$max_depth"
    characterize: "$characterize"
    audience: "$audience"
    l0_summary: "$refresh_state.l0_summary"
  save_as: step_only

- name: decompose_delta
  primitive: recursive_decompose
  mode: delta
  instruction: "$prompts/question/decompose_delta.md"
  when: "$load_prior_state.has_overlay == true"
  input:
    # ... includes existing_tree, existing_answers, evidence_sets, gaps
  save_as: step_only
```

Mode `delta` runs against an existing question tree rather than from scratch. Both paths produce `$decomposed_tree` as output.

### `build_lifecycle`

Lifecycle management — overlay cleanup, supersession bookkeeping, build state transitions. Runs after decomposition.

```yaml
- name: build_lifecycle
  primitive: build_lifecycle
  input:
    build_id: "$build_id"
    load_prior_state: "$refresh_state"
  save_as: step_only
```

### `evidence_loop`

Runs the evidence answering cycle: pre-map candidate L0 nodes for each leaf question, answer with KEEP/DISCONNECT/MISSING verdicts, weight each KEEP, record reasons.

```yaml
- name: evidence_loop
  primitive: evidence_loop
  input:
    question_tree: "$decomposed_tree"
    extraction_schema: "$extraction_schema"
    load_prior_state: "$refresh_state"
    reused_question_ids: "$reused_question_ids"
    build_id: "$build_id"
  save_as: step_only
```

### `process_gaps`

Handles MISSING verdicts from `evidence_loop`. Records demand signals, optionally triggers gap-filling targeted extractions.

```yaml
- name: gap_processing
  primitive: process_gaps
  input:
    evidence_loop: "$evidence_loop"
    load_prior_state: "$refresh_state"
  save_as: step_only
```

### `container`

Groups a sub-sequence of steps inside one logical unit. Useful when a conceptual "phase" has multiple steps that should be treated as one:

```yaml
- name: thread_clustering
  primitive: container
  steps:
    - name: batch_cluster
      primitive: classify
      # ...
    - name: merge_clusters
      primitive: synthesize
      # ...
```

From outside the container, you reference `$thread_clustering.<sub-step>` to access sub-step outputs.

---

## The question-pipeline chain as a worked example

The shipped `question.yaml` is a compositional chain with ~15 phases. Top-level shape:

```
1. load_prior_state        (cross_build_input)    — "what state exists?"
2. source_extract          (extract, for_each chunks)   — L0 extraction for first question
3. l0_webbing              (web)                   — lateral L0 connections
4. refresh_state           (cross_build_input)    — re-read state after extraction
5. enhance_question        (extract)               — improve apex question given corpus
6. decompose               (recursive_decompose, when: no overlay) — decompose from scratch
7. decompose_delta         (recursive_decompose, mode: delta, when: overlay exists)
8. extraction_schema       (extract)               — what should L0 look like?
9. build_lifecycle         (build_lifecycle)      — overlay cleanup
10. evidence_loop          (evidence_loop)         — answer leaf questions
11. gap_processing         (process_gaps)          — MISSING verdict handling
12. l1_webbing             (web)                   — lateral L1 connections
13. synthesis              (synthesize, recursive_cluster) — build answer hierarchy up to apex
14. l2_webbing             (web)                   — lateral connections at L2
15. horizontal_review      (evaluate)              — final consistency pass
```

Every step is in one file. The composition is linear with `when` conditions gating phases. Recipe primitives handle the specialized phases (cross_build_input, decomposition, evidence_loop, process_gaps, build_lifecycle); regular primitives (extract, classify, synthesize, web) handle the straightforward LLM work.

This is the pattern to emulate when authoring custom chains: linear step sequence, recipe primitives for specialized work, regular primitives for the rest, `when` conditions to gate phases.

---

## Cross-build composition: vines

A **vine** pyramid is composed of other pyramids (its bedrock children). Vine chains orchestrate builds across those children. The key pattern:

```yaml
- name: build_bedrocks
  for_each: $bedrock_children
  primitive: # ... vine-specific primitive
  input:
    child_slug: $item.slug
    child_path: $item.path
    child_chain_id: $item.chain_id
  # ...
```

The vine chain iterates over child metadata and dispatches child builds. Each child build is a separate pyramid build, with its own chain, running through the full executor. When children complete, the vine chain synthesizes a cross-cutting view at the vine's apex level.

This is how cross-pyramid composition works **today**: a vine chain triggers independent child builds, each of which is a standalone build using its own chain. No `invoke_chain`-style composition inside a single chain — instead, the vine's top level orchestrates separate builds.

---

## Variable flow across phases

Step outputs accumulate in the chain context and are referenceable by later steps:

- `$step_name` — the step's output object.
- `$step_name.nodes` — array of produced nodes (for `save_as: node` steps).
- `$step_name.output.distilled` — specific fields on output.

Special global variables (populated by recipe primitives or executor):

- `$load_prior_state.*` — from `cross_build_input`.
- `$decomposed_tree` — canonical alias for either `decompose` or `decompose_delta` output.
- `$extraction_schema` — from the `extraction_schema` step.
- `$evidence_loop`, `$gap_processing` — from their respective recipe primitives.

Later steps conditionally execute based on these via `when:` expressions. This is how fresh-vs-delta, with-vs-without overlay, and similar forks are expressed.

---

## When to author your own chain vs tune the shipped one

**Tune the shipped chain** when:

- You want different extraction behavior (edit the prompts).
- You want different tier routing per step (edit `model_tier` values).
- You want different batching, concurrency, dehydration policies (edit step fields).
- You want an extra webbing pass, or to skip a phase entirely (add/remove/gate steps).

**Author a new chain** when:

- You need a fundamentally different phase structure.
- You're building a specialized pipeline for a specific content type that doesn't fit question-pipeline.
- You want a published chain others can pull that's meaningfully different from the default.

In practice, most customization is tuning the shipped chain. Authoring entirely new chains is rare and should follow the question-pipeline's compositional shape.

---

## `invoke_chain` in practice

`invoke_chain` is a step field that calls another chain by id with scoped inputs and merges outputs back:

```yaml
- name: extract_and_cluster
  invoke_chain: extraction-and-clustering-v2
  inputs:
    chunks: "$chunks"
    tier: extractor
  save_as: step_only
```

The invoked chain runs in its own context — its variables are scoped to itself. Inputs are the only thing that crosses the boundary from caller; outputs come back through the step's `save_as`. Nesting depth is tracked so recursive invocations can't run away.

You author small, reusable chains (e.g. `recursive-cluster-synthesis`, `cross-corpus-glossary`) and compose them from a thin orchestrator chain. Published chains can invoke other published chains by handle-path, making chain composition a first-class sharable primitive on the Wire.

---

## Where to go next

- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — the step vocabulary in detail.
- [`42-editing-prompts.md`](42-editing-prompts.md) — the prompts your steps reference.
- [`chains/CHAIN-DEVELOPER-GUIDE.md`](../../chains/CHAIN-DEVELOPER-GUIDE.md) — authoritative quick reference.
- [`47-schema-types.md`](47-schema-types.md) — when you really do need something outside the chain system.
