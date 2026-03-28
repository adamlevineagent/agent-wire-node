# Wire Node Chain/Pyramid System -- Practitioner Reference

This guide covers everything you need to read, modify, and extend the chain-driven pyramid build system. After reading it, you should be able to add a new step to an existing chain within 15 minutes.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Chain YAML Format](#2-chain-yaml-format)
3. [Prompt .md Files](#3-prompt-md-files)
4. [How to Add a New Chain Step](#4-how-to-add-a-new-chain-step)
5. [The Three Compilation Paths](#5-the-three-compilation-paths)
6. [Model Tier System](#6-model-tier-system)
7. [Content Type Patterns](#7-content-type-patterns)
8. [Question Pyramid Prompts](#8-question-pyramid-prompts)
9. [Reference: All Prompt Files](#9-reference-all-prompt-files)

---

## 1. Architecture Overview

A **chain** is a YAML file that declares an ordered sequence of LLM-powered steps. Each step takes input (raw chunks or output from prior steps), sends it to an LLM with a prompt, and stores the result as pyramid nodes or edges.

The build pipeline works in three phases:

```
YAML chain file
      |
      v
  Compiler (defaults_adapter / question_compiler / wire_compiler)
      |
      v
  Intermediate Representation (IR) -- ExecutionPlan with Step objects
      |
      v
  Chain Executor -- runs each IR step, handles concurrency, resume, errors
      |
      v
  SQLite -- pyramid_nodes, pyramid_pipeline_steps, web_edges
```

Key source files:

| File | Role |
|------|------|
| `chains/defaults/code.yaml` | Code pipeline chain definition |
| `chains/defaults/document.yaml` | Document pipeline chain definition |
| `chains/prompts/{type}/*.md` | LLM prompt templates |
| `src-tauri/src/pyramid/chain_loader.rs` | Loads YAML, resolves `$prompts/` refs |
| `src-tauri/src/pyramid/defaults_adapter.rs` | Compiles YAML chains to IR |
| `src-tauri/src/pyramid/chain_executor.rs` | Executes the IR plan |
| `src-tauri/src/pyramid/chain_dispatch.rs` | LLM dispatch, model resolution |
| `src-tauri/src/pyramid/llm.rs` | OpenRouter API client with cascade |

---

## 2. Chain YAML Format

### Top-Level Fields

```yaml
schema_version: 1              # Always 1
id: code-default               # Unique identifier for this chain
name: Code Pyramid             # Human-readable name
description: >                 # Multi-line description
  What this chain does.
content_type: code             # "code" | "document" | "conversation"
version: "2.0.0"              # Semver string
author: agent-wire             # Who wrote this chain

defaults:                      # Inherited by all steps unless overridden
  model_tier: mid              # Default model tier for steps
  temperature: 0.3             # Default temperature
  on_error: "retry(2)"         # Default error strategy

steps: [...]                   # Ordered list of step definitions
post_build: []                 # Post-build hooks (currently unused)
```

### Step Definition Fields

Every step is an object in the `steps` array. Here is every field a step can have:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Unique step identifier. Used as the key in `$variable` references. |
| `primitive` | string | yes | Step type: `extract`, `classify`, `synthesize`, `web`, `compress` |
| `instruction` | string | yes* | Prompt text or `$prompts/...` file reference. |
| `instruction_map` | map | no | Variant prompts keyed by content traits (see below). |
| `input` | map | no | Explicit input bindings. Keys are variable names, values are `$step_name` references. |
| `context` | map | no | Additional context passed alongside input. Same `$ref` syntax. |
| `for_each` | string | no | Iterate over an array: `$chunks` (raw input) or `$step_name.field`. |
| `concurrency` | int | no | Max parallel LLM calls when using `for_each`. Default 1. |
| `depth` | int | no | Pyramid layer this step produces nodes at (0 = base, 1 = L1, etc.). |
| `node_id_pattern` | string | no | Pattern for generated node IDs. Supports `{index:03}` and `{depth}`. |
| `save_as` | string | no | What to persist: `node` (pyramid node) or `web_edges` (cross-layer edges). |
| `response_schema` | object | no | JSON Schema for structured output. Forces the LLM to conform. |
| `model_tier` | string | no | Override model tier: `mid`, `high`, `max`. |
| `model` | string | no | Direct model override (e.g., `qwen/qwen3.5-flash-02-23`). Bypasses tier. |
| `temperature` | float | no | Override temperature for this step. |
| `on_error` | string | no | Error strategy: `retry(N)`, `skip`, `abort`, `carry_left`, `carry_up`. |
| `compact_inputs` | bool | no | When true, strip full content from input nodes, keeping only headlines/IDs. Reduces token count for classification steps. |
| `recursive_cluster` | bool | no | When true, this step runs in a loop: cluster the current layer's nodes, synthesize each cluster, repeat until <= threshold nodes remain. Produces all upper layers through to the apex. |
| `cluster_instruction` | string | no | Prompt for the clustering sub-step of `recursive_cluster`. |
| `cluster_model` | string | no | Model override for clustering sub-step. |
| `cluster_response_schema` | object | no | JSON Schema for clustering output. |
| `merge_instruction` | string | no | Prompt for merge sub-steps. |
| `max_thread_size` | int | no | Upper bound on items per group (used by classify steps). |
| `header_lines` | int | no | Truncate each chunk's content to the first N lines. Set in `input` map, not at step level. |

### How `$variable` References Work

Any string value starting with `$` is a reference resolved at runtime.

- **`$chunks`** -- The raw input documents/files. Available to any step.
- **`$step_name`** -- The output of a previous step (by its `name` field). Returns the full output array.
- **`$step_name.field`** -- Dot-access into the output. For example, `$thread_clustering.threads` returns the `threads` array from the clustering step's JSON output.
- **`$item`** -- Inside a `for_each` loop, refers to the current iteration item.
- **`$index`** -- Inside a `for_each` loop, the current iteration index (0-based).

Examples from the code chain:

```yaml
# Reference raw input chunks
for_each: $chunks

# Reference output of a previous step
input:
  nodes: $l0_code_extract

# Dot into a field of a previous step's output
for_each: $thread_clustering.threads

# Reference current item in a for_each loop
input:
  doc: $item
  header_lines: 20
```

### How `for_each` + `concurrency` Enables Parallel Execution

When a step has `for_each: $some_array`, the executor iterates over every element, running the LLM call once per element. The `concurrency` field controls how many of these calls run in parallel:

```yaml
- name: l0_code_extract
  primitive: extract
  instruction: "$prompts/code/code_extract.md"
  for_each: $chunks          # One call per source file
  concurrency: 8             # Up to 8 LLM calls at once
```

Without `for_each`, the step runs exactly once with all its input as a single batch.

### How `storage_directive` Controls Persistence

The `save_as` and `depth` fields together control what gets written to SQLite:

- **`save_as: node`** + **`depth: 0`** -- Each output becomes a pyramid node at L0. The `node_id_pattern` generates the ID.
- **`save_as: node`** + **`depth: 1`** -- Nodes at L1 (synthesis layer).
- **`save_as: web_edges`** -- Output is an `edges` array stored as cross-layer connections.
- **No `save_as`** -- Step output is kept in memory for downstream steps but not persisted as nodes.

The `node_id_pattern` field uses `{index:03}` for zero-padded index and `{depth}` for the current layer:

```yaml
node_id_pattern: "C-L0-{index:03}"   # Produces C-L0-000, C-L0-001, ...
node_id_pattern: "L1-{index:03}"      # Produces L1-000, L1-001, ...
node_id_pattern: "L{depth}-{index:03}" # Produces L2-000, L3-000, ... (for recursive steps)
```

### How `header_lines` Truncates Input

Set `header_lines` as a key inside `input` to truncate chunk content to the first N lines. This is a resolver directive -- it is stripped from the input before reaching the LLM.

```yaml
input:
  doc: $item
  header_lines: 20    # Only pass first 20 lines of each chunk
```

Useful for classification steps that only need the top of each document to determine its type.

### `instruction_map` -- Variant Prompt Dispatch

When a step needs different prompts for different kinds of input, use `instruction_map`:

```yaml
instruction: "$prompts/code/code_extract.md"          # Default
instruction_map:
  type:config: "$prompts/code/config_extract.md"       # Config files
  extension:.tsx: "$prompts/code/code_extract_frontend.md"  # TSX files
  extension:.jsx: "$prompts/code/code_extract_frontend.md"  # JSX files
  type:frontend: "$prompts/code/code_extract_frontend.md"   # Frontend type
```

At runtime, the executor reads headers from the chunk content (`## TYPE: ...`, `## LANGUAGE: ...`) and dispatches to the matching variant prompt. If no variant matches, the default `instruction` is used.

---

## 3. Prompt .md Files

### Where They Live

```
chains/
  prompts/
    code/                    # Prompts for code chains
      code_extract.md
      code_extract_frontend.md
      config_extract.md
      code_cluster.md
      code_thread.md
      code_thread_split.md
      code_web.md
      code_distill.md
      code_recluster.md
      code_group.md
    document/                # Prompts for document chains
      doc_classify_perdoc.md
      doc_taxonomy.md
      doc_extract.md
      doc_concept_areas.md
      doc_assign.md
      doc_thread.md
      doc_web.md
      doc_distill.md
      doc_recluster.md
      doc_classify.md
      doc_cluster.md
      doc_group.md
    question/                # Prompts for question pyramids
      enhance_question.md
      decompose.md
      horizontal_review.md
      pre_map.md
      answer.md
```

### Template Variable Convention

Prompts use **double-brace** placeholders: `{{variable_name}}`. These are replaced at runtime by `render_prompt_template()`, which does simple string substitution.

```
{{content_type}}       -- "code" or "document"
{{audience_block}}     -- Audience description paragraph, or empty string
{{content_type_block}} -- Content type guidance paragraph, or empty string
{{synthesis_prompt}}   -- User-provided synthesis guidance, or empty string
{{depth}}              -- Current decomposition depth
```

Single braces `{index:03}` are used only in `node_id_pattern` (not in prompts).

### Runtime Loading

The chain loader (`chain_loader.rs`) resolves prompt references at load time:

1. If a step's `instruction` starts with `$prompts/`, the loader reads the file from `{chains_dir}/prompts/{rest_of_path}`.
2. The file contents replace the `$prompts/...` string in the step definition.
3. If the file doesn't exist, loading fails with an error.

For question pyramid prompts, the evidence answering and pre-mapping code loads prompts at call time:

1. Try to read `{chains_dir}/prompts/question/{file}.md`.
2. If the file exists, use it as a template with `render_prompt_template()`.
3. If the file is missing, fall back to an inline Rust string constant.

### How to Edit Prompts

1. Open the `.md` file in `chains/prompts/{type}/`.
2. Edit the prompt text. Keep the `{{variable}}` placeholders intact.
3. Rebuild the pyramid. The new prompt takes effect immediately -- no Rust recompile needed, because `chain_loader.rs` reads from the filesystem at chain load time.
4. If `chains_dir` points to the source tree (dev mode), changes apply without any build step at all.

---

## 4. How to Add a New Chain Step

This walkthrough adds a hypothetical "complexity scoring" step to the code chain that scores each L0 node's complexity before clustering.

### Step 1: Create the Prompt File

Create `chains/prompts/code/code_complexity.md`:

```markdown
You are scoring the complexity of a code extraction. Rate the following
aspects on a 1-5 scale:

- Cyclomatic complexity (branching, nesting)
- Integration complexity (external dependencies, API calls)
- Domain complexity (business logic density)

Output valid JSON only:
{
  "complexity_score": 3.2,
  "breakdown": {
    "cyclomatic": 4,
    "integration": 2,
    "domain": 3
  },
  "rationale": "Brief explanation"
}

/no_think
```

### Step 2: Define the Step in the YAML

Open `chains/defaults/code.yaml` and add the step after `l0_code_extract` (step ordering matters -- you can only reference steps that come before):

```yaml
  - name: complexity_scoring
    primitive: classify
    instruction: "$prompts/code/code_complexity.md"
    for_each: $l0_code_extract
    concurrency: 8
    response_schema:
      type: object
      properties:
        complexity_score:
          type: number
        breakdown:
          type: object
          properties:
            cyclomatic:
              type: integer
            integration:
              type: integer
            domain:
              type: integer
          required: ["cyclomatic", "integration", "domain"]
          additionalProperties: false
        rationale:
          type: string
      required: ["complexity_score", "breakdown", "rationale"]
      additionalProperties: false
    model_tier: mid
    temperature: 0.2
    on_error: "retry(2)"
```

### Step 3: Set Up Input References

The step above uses `for_each: $l0_code_extract`, which iterates over each L0 extraction result. Inside the prompt, the LLM receives each extraction as user input automatically.

If you needed to pass additional context from another step:

```yaml
    context:
      taxonomy: $doc_taxonomy
```

### Step 4: Choose the Model Tier

- **`mid`** (default) -- Good for simple classification, extraction, small inputs. Maps to mercury-2.
- **`high`** -- For cross-document reasoning or inputs > 120K tokens. Maps to qwen.
- **`max`** -- For frontier reasoning or inputs > 900K tokens. Maps to grok.

For a per-node scoring step with small input, `mid` is correct.

### Step 5: Set `save_as` If the Step Produces Nodes

This step only produces metadata for downstream consumption -- it does NOT create pyramid nodes. So we omit `save_as`, `depth`, and `node_id_pattern`. The output lives in memory as `$complexity_scoring` for subsequent steps to reference.

If the step DID produce nodes, you would add:

```yaml
    depth: 0
    save_as: node
    node_id_pattern: "C-L0-{index:03}"
```

### Step 6: Reference From Downstream Steps

Other steps can now use `$complexity_scoring` as input or context:

```yaml
  - name: thread_clustering
    primitive: classify
    instruction: "$prompts/code/code_cluster.md"
    input:
      topics: $l0_code_extract
      complexity: $complexity_scoring   # New: pass complexity scores
```

### Step 7: Test

1. Run a pyramid build on a small source set.
2. Check the pipeline steps table for `complexity_scoring` entries.
3. Verify the JSON output matches your schema.
4. If using `on_error: "retry(2)"`, watch logs for retry attempts that might indicate prompt issues.

---

## 5. The Three Compilation Paths

All chains compile down to the same **Intermediate Representation (IR)** -- an `ExecutionPlan` containing flat `Step` objects. The executor only sees IR, never raw YAML or question trees.

### Path 1: Mechanical (YAML defaults)

```
chains/defaults/code.yaml  or  document.yaml
        |
        v
  chain_loader::load_chain()       -- parse YAML, resolve $prompts/ refs
        |
        v
  defaults_adapter::compile_defaults()  -- translate to IR
        |
        v
  ExecutionPlan { steps: [...] }
```

Key translations by `defaults_adapter`:
- `recursive_cluster: true` becomes a converge block (loop of cluster + synthesize steps)
- `instruction_map` becomes variant dispatch logic in the IR step
- `compact_inputs: true` adds a transform that strips content, keeping only IDs and headlines
- `for_each` + `concurrency` becomes `IterationMode::ForEach` with a semaphore

### Path 2: Question (question tree)

```
User's apex question + DecompositionConfig
        |
        v
  question_decomposition::decompose_question()  -- LLM decomposes into tree
        |
        v
  QuestionSet (YAML-like structure)
        |
        v
  question_compiler::compile_question_set()  -- translate to IR
        |
        v
  ExecutionPlan { steps: [...] }
```

The question decomposition is itself LLM-powered, using prompts from `chains/prompts/question/`. The resulting question tree is then compiled into an IR plan where each question becomes steps for evidence gathering and answering.

### Path 3: Wire (future)

```
Wire action chain JSON (from the marketplace)
        |
        v
  wire_compiler::compile_wire()  -- translate to IR
        |
        v
  ExecutionPlan { steps: [...] }
```

This path will allow Wire marketplace action chains to compile into the same IR, enabling remote agents to drive pyramid builds using the Wire protocol. Not yet implemented.

---

## 6. Model Tier System

### The Three Tiers

| Tier | Model | Context Limit | Use Case |
|------|-------|---------------|----------|
| `mid` (default) | `inception/mercury-2` | 120K tokens | Per-file extraction, classification, small synthesis. Fast and cheap. |
| `high` | `qwen/qwen3.5-flash-02-23` | 900K tokens | Cross-document reasoning, taxonomy normalization, large codebases. |
| `max` | `x-ai/grok-4.20-beta` | >900K tokens | Frontier reasoning, largest context window. Expensive. |

The `low` tier is an alias for `mid` -- both map to mercury-2.

### How Tiers Resolve

Resolution priority (from `chain_dispatch.rs`):

1. **Step-level `model`** -- Direct model string (e.g., `model: "qwen/qwen3.5-flash-02-23"`). Highest priority.
2. **Step-level `model_tier`** -- Mapped through the tier table above.
3. **Defaults-level `model`** -- Only used if the step has no `model_tier`.
4. **Defaults-level `model_tier`** -- Lowest priority fallback.
5. **Unknown tier** -- Falls back to primary model (mercury-2) with a warning log.

### Context Cascade (Safety Net)

Even when a step declares `model_tier: mid`, the LLM client in `llm.rs` has a safety net: if the assembled prompt exceeds the primary model's 120K context limit, the client automatically cascades to the next model:

```
mercury-2 (120K) --[too big]--> qwen (900K) --[too big]--> grok (unlimited)
```

This cascade is triggered by HTTP 400 responses from OpenRouter, which indicate the input exceeded the model's capacity. The cascade is transparent to the chain definition.

### Choosing a Tier for Your Step

- Start with `mid` (the default). It handles most single-document steps.
- Use `high` when your step processes many documents at once (taxonomy normalization, concept area identification) or when compacted input might still exceed 120K.
- Use `max` only for apex-level synthesis of very large pyramids.
- Prefer `model_tier` over `model` -- it lets the config change models without editing every chain.

---

## 7. Content Type Patterns

### Code Chain Pipeline

```
l0_code_extract          extract     Per-file LLM analysis (one node per source file)
    |                                Variants: code_extract.md / code_extract_frontend.md / config_extract.md
    v
l0_webbing               web         Cross-file edge discovery at L0
    |
    v
thread_clustering        classify    Group ALL L0 topics into 10-18 semantic threads
    |
    v
thread_narrative         synthesize  Per-thread synthesis into L1 nodes (for_each thread)
    |
    v
l1_webbing               web         Cross-thread edge discovery at L1
    |
    v
upper_layer_synthesis    synthesize  Recursive clustering: L1 -> L2 -> ... -> apex
    |                                Uses code_recluster.md (cluster) + code_distill.md (synthesize)
    v
l2_webbing               web         Cross-domain edge discovery at L2
```

### Document Chain Pipeline

```
doc_classify_perdoc      classify    Per-doc type/date/keywords (parallel, header_lines: 20)
    |
    v
doc_taxonomy             classify    Normalize raw keywords into shared concept taxonomy
    |
    v
l0_doc_extract           extract     Per-doc extraction with taxonomy context
    |
    v
doc_concept_areas        classify    Identify conceptual thread definitions (compact_inputs)
    |
    v
doc_assign               classify    Assign each doc to a thread (parallel)
    |
    v
thread_narrative         synthesize  Per-thread temporally-ordered synthesis
    |
    v
l1_webbing               web         Cross-thread edge discovery
    |
    v
upper_layer_synthesis    synthesize  Recursive clustering to apex
    |
    v
l2_webbing               web         Cross-domain edge discovery
```

### Question Chain Pipeline

```
enhance_question                     Expand user's short question (max 30 words)
    |
    v
decompose                           Break into sub-questions (leaf/branch tree)
    |
    v
horizontal_review                    Merge overlaps, convert branches to leaves
    |
    v
[IR executor for L0]                Standard extraction runs against source material
    |
    v
pre_map                             Map questions to candidate evidence nodes
    |
    v
answer (per layer)                   Synthesize answers with KEEP/DISCONNECT verdicts
```

---

## 8. Question Pyramid Prompts

The question pyramid uses a different flow than mechanical chains. Instead of YAML steps, the decomposition is driven by `question_decomposition.rs` and `evidence_answering.rs`, which load prompts from `chains/prompts/question/` at runtime.

### `enhance_question.md`

**Purpose:** Expands a user's terse question into a clear, focused apex question.

**Template variables:** None (receives user question as user prompt).

**Output:** Plain text -- the expanded question (max 30 words).

**Key rules in the prompt:**
- Maximum 30 words
- No specific feature/component names from source material
- Casual language, no jargon
- Must be ONE question, not compound

### `decompose.md`

**Purpose:** Breaks a question into sub-questions for the knowledge pyramid tree.

**Template variables:**
- `{{content_type}}` -- "code" or "document"
- `{{audience_block}}` -- Audience description paragraph (or empty)

**Output:** JSON array of `{ question, prompt_hint, is_leaf }` objects.

**Key behavior:**
- Each sub-question must have a genuinely different imagined answer
- `is_leaf: true` = answerable directly from source material
- `is_leaf: false` = needs further decomposition (branch)
- Minimum viable count -- no padding

### `horizontal_review.md`

**Purpose:** Post-processing pass that merges overlapping sibling questions and converts branches to leaves when possible.

**Template variables:** None (receives question list as user prompt).

**Output:** JSON object with `merges` array and `mark_as_leaf` array.

### `pre_map.md`

**Purpose:** Maps each question to candidate evidence nodes from the layer below. Over-includes to avoid missing evidence.

**Template variables:**
- `{{audience_block}}` -- Audience context
- `{{content_type_block}}` -- Content type guidance

**Output:** JSON object with `mappings` dict: `{ question_id: [node_id, ...] }`.

**Key rule:** Over-include candidates. False positives are cheap; missed evidence is permanent.

### `answer.md`

**Purpose:** Synthesizes an answer to a question using candidate evidence, producing KEEP/DISCONNECT verdicts for each candidate.

**Template variables:**
- `{{audience_block}}` -- Audience context
- `{{synthesis_prompt}}` -- User-provided synthesis guidance
- `{{content_type_block}}` -- Content type guidance

**Output:** JSON object with:
- `headline` -- Short answer headline (max 120 chars)
- `distilled` -- 2-4 sentence synthesis
- `topics` -- Array of `{ name, current }` topic objects
- `verdicts` -- Array of `{ node_id, verdict, weight, reason }` for each candidate
- `missing` -- Evidence gaps
- `corrections`, `decisions`, `terms`, `dead_ends` -- Additional metadata arrays

---

## 9. Reference: All Prompt Files

### Code Prompts (`chains/prompts/code/`)

| File | Used By Step | Purpose |
|------|-------------|---------|
| `code_extract.md` | `l0_code_extract` | Default per-file code extraction. 2-5 topics per file. User-facing + system-facing categories. |
| `code_extract_frontend.md` | `l0_code_extract` (variant) | Frontend file extraction. Leads with user experience. |
| `config_extract.md` | `l0_code_extract` (variant) | Config file extraction. Build scripts, dependencies, platform settings. |
| `code_web.md` | `l0_webbing`, `l1_webbing`, `l2_webbing` | Cross-cutting connection discovery. Shared tables, endpoints, types. |
| `code_cluster.md` | `thread_clustering` | Groups L0 topics into 10-18 semantic threads. Max 12 files per thread. |
| `code_thread.md` | `thread_narrative` | Per-thread synthesis into a coherent L1 node. 5-10 sentence orientation. |
| `code_thread_split.md` | (internal) | Splits oversized threads. Compiled into Rust via `include_str!`. |
| `code_distill.md` | `upper_layer_synthesis` | Synthesizes sibling nodes into parent. Density scales with merge level. |
| `code_recluster.md` | `upper_layer_synthesis` (cluster sub-step) | Groups nodes into 3-5 architectural domain clusters. |
| `code_group.md` | (legacy) | Older grouping prompt, superseded by cluster. |

### Document Prompts (`chains/prompts/document/`)

| File | Used By Step | Purpose |
|------|-------------|---------|
| `doc_classify_perdoc.md` | `doc_classify_perdoc` | Per-doc type/date/keyword classification. |
| `doc_taxonomy.md` | `doc_taxonomy` | Normalize keywords into shared concept taxonomy. |
| `doc_extract.md` | `l0_doc_extract` | Per-doc extraction with taxonomy context. |
| `doc_concept_areas.md` | `doc_concept_areas` | Identify conceptual thread definitions. |
| `doc_assign.md` | `doc_assign` | Assign each doc to a concept thread. |
| `doc_thread.md` | `thread_narrative` | Temporally-ordered, supersession-aware synthesis. |
| `doc_web.md` | `l1_webbing`, `l2_webbing` | Cross-thread edge discovery for documents. |
| `doc_distill.md` | `upper_layer_synthesis` | Parent node synthesis for document pyramids. |
| `doc_recluster.md` | `upper_layer_synthesis` (cluster sub-step) | Cluster document L1 nodes into domains. |
| `doc_classify.md` | (legacy) | Older batch classification. |
| `doc_cluster.md` | (legacy) | Older clustering prompt. |
| `doc_group.md` | (legacy) | Older grouping prompt. |

### Question Prompts (`chains/prompts/question/`)

| File | Used By | Variables |
|------|---------|-----------|
| `enhance_question.md` | `question_decomposition.rs` | (none) |
| `decompose.md` | `question_decomposition.rs` | `{{content_type}}`, `{{audience_block}}` |
| `horizontal_review.md` | `question_decomposition.rs` | (none) |
| `pre_map.md` | `evidence_answering.rs` | `{{audience_block}}`, `{{content_type_block}}` |
| `answer.md` | `evidence_answering.rs` | `{{audience_block}}`, `{{synthesis_prompt}}`, `{{content_type_block}}` |

---

## Quick Reference: Adding a Step Checklist

1. [ ] Create prompt file: `chains/prompts/{content_type}/{step_name}.md`
2. [ ] Add step to `chains/defaults/{content_type}.yaml` in the correct position
3. [ ] Set `instruction: "$prompts/{content_type}/{step_name}.md"`
4. [ ] Wire `input` or `for_each` to reference prior step outputs
5. [ ] Choose `model_tier` (start with `mid`)
6. [ ] Add `response_schema` if you need structured JSON output
7. [ ] Set `save_as: node` + `depth` + `node_id_pattern` if the step produces pyramid nodes
8. [ ] Set `on_error` strategy (default: `retry(2)`)
9. [ ] Test with a small source set and check pipeline step records
