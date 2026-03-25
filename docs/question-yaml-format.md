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
      min_groups: 8
      max_groups: 15
      max_items_per_group: 12
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
| `sequential_context` | no | For ordered processing. See "Sequential Context" below. |
| `preview_lines` | no | Number of lines to send per item instead of full content. Used for cheap pre-classification. Default: full content. |

### Scope values (`about:`)

| Value | Meaning | Engine maps to |
|-------|---------|---------------|
| `each file individually` | One LLM call per source file, full content | parallel forEach over chunks |
| `each chunk individually` | One LLM call per conversation chunk, full content | parallel forEach over chunks |
| `the first N lines of each file` | One LLM call per file, truncated to N lines | parallel forEach, header_lines: N |
| `all L0 nodes at once` | Single LLM call with all L0 content | holistic classify/web |
| `all L0 topics at once` | Single call with topic summaries only (compact) | compact classify |
| `each L1 thread's assigned L0 nodes` | One call per L1, with its assigned children | parallel forEach over threads |
| `each L1 thread's assigned L0 nodes, ordered chronologically` | Same as above, but children sorted by date or position | parallel forEach, sorted |
| `all L1 nodes at once` | Single call with all L1 content | holistic classify/web |
| `all L2 nodes at once` | Single call with all L2 content | holistic classify/web |
| `all top-level nodes at once` | Whatever the highest non-apex layer is | apex synthesis |

### Output types (`creates:`)

| Value | Meaning | Engine maps to |
|-------|---------|---------------|
| `L0 nodes` | One node per input file at depth 0 | save_as: node, depth: 0 |
| `L0 classification tags` | Per-item metadata (type, subject, date, canonical status) | save_as: metadata, depth: 0 |
| `L1 topic assignments` | Grouping of L0 topics into conceptual threads | save_as: assignments |
| `L1 thread assignments` | Grouping of L0s into threads (alias for topic assignments) | save_as: assignments |
| `L1 nodes` | One node per thread/cluster at depth 1 | save_as: node, depth: 1 |
| `L2 nodes` | One node per domain at depth 2 | save_as: node, depth: 2 |
| `apex` | Single apex node | save_as: node, depth: max+1 |
| `web edges between L0 nodes` | Cross-references at L0 | save_as: web_edges, depth: 0 |
| `web edges between L1 nodes` | Cross-references at L1 | save_as: web_edges, depth: 1 |
| `web edges between L2 nodes` | Cross-references at L2 | save_as: web_edges, depth: 2 |

### Context references

| Reference | Meaning |
|-----------|---------|
| `L0 classification tags` | Metadata from a classification question (type, date, canonical) |
| `L0 web edges` | Web edges produced by an earlier L0 webbing question |
| `L1 web edges` | Web edges produced by an earlier L1 webbing question |
| `L2 web edges` | Web edges produced by an earlier L2 webbing question |
| `sibling headlines` | Headlines of other nodes at the same depth (for uniqueness) |

### Sequential Context

For content types where order matters (conversations, time-series documents), the `sequential_context` field enables ordered processing with a running summary carried forward:

```yaml
sequential_context:
  mode: accumulate        # carry forward a running context
  max_chars: 8000         # maximum size of accumulated context
  carry: summary of prior chunks so far  # what to carry (natural language description)
```

When `sequential_context` is present:
- Items are processed in order (parallel is ignored)
- Each LLM call receives the accumulated context from prior items
- The engine maintains a running summary that grows up to `max_chars`
- This ensures chunk 5 knows what was discussed in chunks 1-4

### Preview Lines

For cheap pre-classification without sending full document content:

```yaml
- ask: "What type of document is this?"
  about: each file individually
  preview_lines: 20                  # send only first 20 lines per file
  creates: L0 classification tags
  prompt: prompts/doc/classify.md
```

The `preview_lines` value is configurable per question. The engine truncates each item's content to N lines before sending to the LLM. Use this for fast metadata extraction where headers, titles, and dates are sufficient.

### Variants

Different item types may need different extraction prompts:

```yaml
variants:
  config files: prompts/code/config_extract.md
  frontend (.tsx, .jsx): prompts/code/frontend_extract.md
```

The engine matches items against variant keys using file extension, chunk type header, or content heuristics. Items not matching any variant use the default `prompt`. Variant keys are intentionally human-readable — the engine resolves "config files" to `type == 'config'` and "frontend (.tsx, .jsx)" to `extension in ['.tsx', '.jsx']` internally.

### Constraints

Guardrails that the engine enforces on question outputs:

```yaml
constraints:
  min_groups: 8           # minimum number of clusters/threads
  max_groups: 15          # maximum number of clusters/threads
  max_items_per_group: 12 # maximum items assigned to any single group
```

If the LLM output violates constraints, the engine can:
- Re-prompt with a correction hint (for min/max violations)
- Post-process to split oversized groups (for max_items_per_group)
- Fail and retry (respecting the `retry` count)

## Example: Document Pyramid

```yaml
type: document
version: 3.0

defaults:
  model: inception/mercury-2
  retry: 2

questions:

  - ask: "What type of document is this, what subject does it cover, when was it written, and is it still current?"
    about: each file individually
    preview_lines: 20
    creates: L0 classification tags
    prompt: prompts/doc/classify.md
    parallel: 8

  - ask: "What are the key claims, decisions, and entities in this document?"
    about: each file individually
    creates: L0 nodes
    context:
      - L0 classification tags
    prompt: prompts/doc/extract.md
    parallel: 8
    retry: 3

  - ask: "What are the 6-12 distinct conceptual topics across all documents?"
    about: all L0 topics at once
    creates: L1 topic assignments
    context:
      - L0 classification tags
    prompt: prompts/doc/cluster.md
    model: qwen/qwen3.5-flash-02-23

  - ask: "What is the current state of this topic, incorporating all relevant documents in temporal order?"
    about: each L1 thread's assigned L0 nodes, ordered chronologically
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

## Example: Conversation Pyramid

```yaml
type: conversation
version: 3.0

defaults:
  model: inception/mercury-2
  retry: 2

questions:

  - ask: "What topics were discussed in this chunk? What claims, decisions, and questions emerged?"
    about: each chunk individually
    creates: L0 nodes
    prompt: prompts/conversation/extract.md
    sequential_context:
      mode: accumulate
      max_chars: 8000
      carry: summary of prior chunks so far
    retry: 3

  - ask: "What are the 4-12 distinct topics discussed throughout this conversation?"
    about: all L0 topics at once
    creates: L1 thread assignments
    prompt: prompts/conversation/cluster.md
    model: qwen/qwen3.5-flash-02-23

  - ask: "What is the full arc of this topic — starting position, what changed, and final conclusion?"
    about: each L1 thread's assigned L0 nodes, ordered chronologically
    creates: L1 nodes
    prompt: prompts/conversation/thread.md
    parallel: 5

  - ask: "Which topics influenced each other? Where are there unresolved tensions?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/conversation/web.md

  - ask: "What are the 2-4 major themes of this conversation?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges
      - sibling headlines
    prompt: prompts/conversation/recluster.md
    model: qwen/qwen3.5-flash-02-23

  - ask: "What was this conversation about, what was decided, what changed, and what's the final state?"
    about: all top-level nodes at once
    creates: apex
    prompt: prompts/conversation/distill.md
```

## Migration from v2.0

The engine maintains backward compatibility. v2.0 YAML files (with `primitive:`, `for_each:`, `save_as:`) continue to work. The engine can read either format. New question sequences should use v3.0 format.

The v3.0 format compiles to the same internal step representation. `about: each file individually` becomes `for_each: $chunks`. `creates: L1 nodes` becomes `save_as: node, depth: 1`. The engine sees the same primitives either way.

### Compilation mapping

| v3.0 field | v2.0 equivalent |
|-----------|-----------------|
| `about: each file individually` | `primitive: extract, for_each: $chunks` |
| `about: all L0 nodes at once` | `primitive: classify` or `primitive: web` |
| `about: each L1 thread's assigned L0 nodes` | `primitive: synthesize, for_each: $clustering.threads` |
| `creates: L0 nodes` | `save_as: node, depth: 0` |
| `creates: web edges between L1 nodes` | `save_as: web_edges, depth: 1` |
| `creates: L1 topic assignments` | `save_as: assignments` |
| `creates: apex` | `save_as: node, depth: max+1` |
| `parallel: 8` | `concurrency: 8` |
| `preview_lines: 20` | `input: { header_lines: 20 }` |
| `context: [L0 web edges]` | `context: { edges: $l0_webbing }` |
| `variants: { config files: ... }` | `instruction_map: { type:config: ... }` |
| `sequential_context: { mode: accumulate }` | `accumulate: { max_chars: 8000 }` |
| `constraints: { min_groups: 8 }` | `min_thread_size: 8` (or enforced in prompt) |
