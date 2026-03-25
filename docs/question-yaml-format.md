# Question YAML Format — v3.0

## Design Principles

1. **Every field means what it says.** No `primitive: classify` — instead `creates: L1 thread assignments`. No `for_each: $chunks` — instead `about: each file individually`.
2. **The question IS the documentation.** Reading the YAML tells you what the pyramid does, in order, without needing to understand the engine.
3. **Execution details are explicit but secondary.** Model, concurrency, retry are visible but don't obscure the question structure.
4. **The engine resolves implementation.** `about: each file individually` maps to a parallel forEach. `about: all L1 nodes at once` maps to a single holistic call. The YAML author doesn't need to know the primitive types.

## Format

```yaml
type: code                    # content type this question sequence is designed for
version: 3.0

defaults:
  model: inception/mercury-2  # default model for all questions
  retry: 2                    # default retry count
  temperature: 0.3

questions:

  # ── Per-file extraction ──────────────────────────────────────────
  - ask: "What does this file do, what are its exports, and how does it connect to other files?"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
    parallel: 8
    retry: 3
    variants:                          # different prompts for different file types
      config files: prompts/code/config_extract.md
      frontend (.tsx, .jsx): prompts/code/frontend_extract.md

  # ── Cross-file connections ───────────────────────────────────────
  - ask: "What resources, tables, endpoints, or types are shared between files?"
    about: all L0 nodes at once
    creates: web edges between L0 nodes
    prompt: prompts/code/web.md
    model: qwen/qwen3.5-flash-02-23   # large context needed
    optional: true                     # pyramid works without this, just less rich

  # ── Semantic grouping ────────────────────────────────────────────
  - ask: "What are the 8-15 distinct subsystems in this codebase?"
    about: all L0 topics at once
    creates: L1 thread assignments
    prompt: prompts/code/cluster.md
    model: qwen/qwen3.5-flash-02-23
    constraints:
      min_threads: 8
      max_threads: 15
      max_per_thread: 12
    retry: 3

  # ── Thread synthesis ─────────────────────────────────────────────
  - ask: "Synthesize this subsystem into a comprehensive developer briefing"
    about: each L1 thread's assigned L0 nodes
    creates: L1 nodes
    context:
      - L0 web edges                  # so synthesis knows cross-cutting connections
    prompt: prompts/code/thread.md
    parallel: 5
    retry: 2

  # ── Subsystem connections ────────────────────────────────────────
  - ask: "What do these subsystems share — tables, APIs, auth flows, IPC channels?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/code/web.md

  # ── Domain grouping ──────────────────────────────────────────────
  - ask: "What are the 3-5 major architectural domains?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges                  # so grouping respects cross-cutting connections
      - sibling headlines             # so each domain gets a unique name
    prompt: prompts/code/recluster.md
    model: qwen/qwen3.5-flash-02-23
    retry: 3

  # ── Domain connections ───────────────────────────────────────────
  - ask: "What connects these architectural domains?"
    about: all L2 nodes at once
    creates: web edges between L2 nodes
    prompt: prompts/code/web.md
    optional: true

  # ── Apex ─────────────────────────────────────────────────────────
  - ask: "What is the unified system overview?"
    about: all top-level nodes at once
    creates: apex
    context:
      - L2 web edges
    prompt: prompts/code/distill.md
```

## Field Reference

### Question fields

| Field | Required | Meaning |
|-------|----------|---------|
| `ask` | yes | The question in natural language. Also serves as documentation. |
| `about` | yes | What this question is asked of. See "Scope" below. |
| `creates` | yes | What the answer produces. See "Output types" below. |
| `prompt` | yes | Path to the detailed instruction file for the LLM. |
| `context` | no | Prior answers to include as additional input. List of references. |
| `model` | no | Override the default model for this question. |
| `parallel` | no | How many concurrent LLM calls for "each" scoped questions. Default 1. |
| `retry` | no | How many times to retry on failure. Default from `defaults:`. |
| `optional` | no | If true, failure skips this question instead of aborting. Default false. |
| `constraints` | no | Guardrails for the answer (min/max counts, size limits). |
| `variants` | no | Alternative prompts for specific file/doc types. |
| `temperature` | no | Override default temperature. |

### Scope values (`about:`)

| Value | Meaning | Engine maps to |
|-------|---------|---------------|
| `each file individually` | One LLM call per source file | parallel forEach over chunks |
| `all L0 nodes at once` | Single LLM call with all L0 content | holistic classify/web |
| `all L0 topics at once` | Single call with topic summaries only | compact classify |
| `each L1 thread's assigned L0 nodes` | One call per L1, with its children | parallel forEach over threads |
| `all L1 nodes at once` | Single call with all L1 content | holistic classify/web |
| `all L2 nodes at once` | Single call with all L2 content | holistic classify/web |
| `all top-level nodes at once` | Whatever the highest non-apex layer is | apex synthesis |

### Output types (`creates:`)

| Value | Meaning | Engine maps to |
|-------|---------|---------------|
| `L0 nodes` | One node per input file at depth 0 | save_as: node, depth: 0 |
| `L1 nodes` | One node per thread/cluster at depth 1 | save_as: node, depth: 1 |
| `L2 nodes` | One node per domain at depth 2 | save_as: node, depth: 2 |
| `apex` | Single apex node | save_as: node, depth: max+1 |
| `L1 thread assignments` | Grouping of L0s into threads | save_as: assignments |
| `web edges between L0 nodes` | Cross-references at L0 | save_as: web_edges, depth: 0 |
| `web edges between L1 nodes` | Cross-references at L1 | save_as: web_edges, depth: 1 |
| `web edges between L2 nodes` | Cross-references at L2 | save_as: web_edges, depth: 2 |

### Context references

| Reference | Meaning |
|-----------|---------|
| `L0 web edges` | Web edges produced by an earlier L0 webbing question |
| `L1 web edges` | Web edges produced by an earlier L1 webbing question |
| `L2 web edges` | Web edges produced by an earlier L2 webbing question |
| `sibling headlines` | Headlines of other nodes at the same depth (for uniqueness) |

## Example: Document Pyramid

```yaml
type: document
version: 3.0

defaults:
  model: inception/mercury-2
  retry: 2

questions:

  - ask: "What type of document is this, what subject does it cover, when was it written, and is it still current?"
    about: the first 20 lines of each file
    creates: L0 classification metadata
    prompt: prompts/doc/classify.md

  - ask: "What are the key claims, decisions, and entities in this document?"
    about: each file individually
    creates: L0 nodes
    context:
      - L0 classification metadata
    prompt: prompts/doc/extract.md
    parallel: 8
    retry: 3

  - ask: "What are the 6-12 distinct conceptual topics across all documents?"
    about: all L0 topics at once
    creates: L1 thread assignments
    context:
      - L0 classification metadata    # so clustering respects type and timeline
    prompt: prompts/doc/cluster.md
    model: qwen/qwen3.5-flash-02-23

  - ask: "What is the current state of this topic, incorporating all relevant documents in temporal order?"
    about: each L1 thread's assigned L0 nodes
    creates: L1 nodes
    prompt: prompts/doc/thread.md
    parallel: 5

  - ask: "What connects these topics — shared entities, contradictions, dependencies?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/doc/web.md

  - ask: "What are the 3-5 major domains these topics fall into?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges
      - sibling headlines
    prompt: prompts/doc/recluster.md
    model: qwen/qwen3.5-flash-02-23

  - ask: "What is this corpus about, what's the current state, and what's still evolving?"
    about: all top-level nodes at once
    creates: apex
    prompt: prompts/doc/distill.md
```

## Migration from v2.0

The engine maintains backward compatibility. v2.0 YAML files (with `primitive:`, `for_each:`, `save_as:`) continue to work. The engine can read either format. New question sequences should use v3.0 format.

The v3.0 format compiles to the same internal step representation. `about: each file individually` becomes `for_each: $chunks`. `creates: L1 nodes` becomes `save_as: node, depth: 1`. The engine sees the same primitives either way.
