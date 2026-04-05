# Question Pipeline v2: Extract-First Architecture

## Problem Statement

The current question pipeline runs `decompose` before `l0_extract`. The decomposer sees only `$characterize` (a one-paragraph corpus description) and generates a question tree based on general knowledge of "what this type of corpus typically contains." This produces speculative questions about topics not in the corpus (testing, deployment, CI/CD, specific doc files), resulting in 24-30% permanent empty/no-evidence nodes.

Document.yaml solves this by extracting generically first, then clustering from real content. Themes emerge bottom-up from actual data. The question pipeline needs the same pattern: extract first, then decompose from real content.

## Evidence

Six experiments (tune-0 through tune-6) on the vibesmithy codebase (34 source files):

| Experiment | Util% | Layers | Empty nodes | Key finding |
|------------|-------|--------|-------------|-------------|
| tune-0 | 61.7% | 2 | 13 untouched | All-leaf flat tree, no branches |
| tune-1 | 100% | 3 | 0 | Branch guidance fixed structure, but 53 leaves (over-decomposed) |
| tune-2 | 100% | 3 | 0 | Granularity guidance cut leaves to 11, build time halved |
| tune-3 | 100% | 2 | 0 | Post-Rust-fixes re-baseline, layer regression |
| tune-4 | 94% | 4 | 6/7 L2 "no evidence" | Conceptual framing improved apex but extraction missed branches |
| tune-5 | 94% | 4 | 20/76 (26%) | extraction_schema blew up (47k tokens), heavy empty nodes |
| tune-6 | 97% | 4 | 13/47 (28%) | Best apex quality, still 28% empty from speculative questions |

Two blind Haiku assessors on tune-6: CONDITIONAL PASS / FAIL. Both identified the same root causes — speculative questions about absent topics, and branches repeating the apex without cross-cutting connections.

The prompt mitigations applied ("stay within corpus", "no document-specific questions", "calibrate to complexity") help at the margin but cannot solve a data-availability problem. The decomposer fundamentally cannot know what doesn't exist when it can't see the content.

## Architecture

### Current flow
```
load_prior_state → enhance → decompose → extraction_schema → l0_extract → evidence_loop → gap_processing → l1_webbing
```

### v2 flow
```
load_prior_state → source_extract → l0_webbing → refresh_state → enhance → decompose → extraction_schema → evidence_loop → gap_processing → l1_webbing → l2_webbing
```

### What changed and why

| Step | Change | Rationale |
|------|--------|-----------|
| `source_extract` | NEW — generic L0 extraction before decompose | Decompose sees real content, can't ask about absent topics |
| `l0_webbing` | NEW — web L0 nodes with compact_inputs | Corpus structure map informs decompose about thematic boundaries |
| `refresh_state` | NEW — re-run cross_build_input after extraction | Enhance and decompose see populated L0 summary |
| `l0_extract` | REMOVED — replaced by source_extract | Generic extraction is the foundation; question-shaped extraction only happens via gap_processing |
| `extraction_schema` | KEPT — used by evidence_loop internals + gap targeting | Still generates question-shaped directives for targeted re-extraction |
| `l2_webbing` | NEW — web branch answers | Cross-cutting connections between branches that tree hierarchy misses |

## Detailed Step Specifications

### Step: source_extract

Generic, content-type-neutral L0 extraction. Identical mechanical config to the current `l0_extract` but uses a generic prompt instead of a question-shaped one.

```yaml
- name: source_extract
  primitive: extract
  instruction: "$prompts/question/source_extract.md"
  for_each: "$chunks"
  when: "$load_prior_state.l0_count == 0"
  dispatch_order: "largest_first"
  concurrency: 4
  node_id_pattern: "Q-L0-{index:03}"
  depth: 0
  save_as: node
  max_input_tokens: 80000
  split_strategy: "sections"
  split_overlap_tokens: 500
  split_merge: true
  merge_instruction: "$prompts/shared/merge_sub_chunks.md"
  on_error: "retry(3)"
  on_parse_error: "heal"
  heal_instruction: "$prompts/shared/heal_json.md"
```

**Changes from current l0_extract:**
- `instruction:` points to `source_extract.md` (generic prompt) instead of `instruction_from: "$extraction_schema.extraction_prompt"` (question-shaped)
- Added `dispatch_order: "largest_first"` — largest files process first, reducing tail latency
- Everything else identical (same split/merge/heal config, same concurrency, same node_id_pattern)

### Step: l0_webbing

Web the generic L0 nodes to build a corpus structure map. Uses `compact_inputs` since webbing only needs headline + entities, not full topic content.

```yaml
- name: l0_webbing
  primitive: web
  instruction: "$prompts/question/question_web.md"
  input:
    nodes: "$source_extract"
  response_schema:
    type: object
    properties:
      edges:
        type: array
        items:
          type: object
          properties:
            source:
              type: string
            target:
              type: string
            relationship:
              type: string
            shared_resources:
              type: array
              items:
                type: string
            strength:
              type: number
          required: ["source", "target", "relationship", "shared_resources", "strength"]
          additionalProperties: false
    required: ["edges"]
    additionalProperties: false
  depth: 0
  save_as: web_edges
  compact_inputs: true
  model_tier: mid
  temperature: 0.2
  on_error: "skip"
  when: "$load_prior_state.l0_count == 0"
```

**Notes:**
- `compact_inputs: true` — strips to headline + entities for the webbing call. Full content stays in DB for evidence_loop.
- `when` guard matches source_extract — only webs on fresh builds. Subsequent questions reuse existing edges.
- Uses same `question_web.md` prompt as l1_webbing (content-neutral once Pillar 37 is fixed).

### Step: refresh_state

Re-runs `cross_build_input` to pick up the L0 nodes and web edges created by source_extract + l0_webbing. Provides populated `l0_summary` for enhance and decompose.

```yaml
- name: refresh_state
  primitive: cross_build_input
  save_as: step_only
```

**Notes:**
- Always runs (no `when` guard). On fresh builds, captures new L0 content. On rebuilds, captures current state (redundant with load_prior_state but harmless — DB read, not LLM call).
- Downstream steps reference `$refresh_state.l0_summary` instead of `$load_prior_state.l0_summary`.

### Step: enhance_question (updated input wiring)

```yaml
- name: enhance_question
  primitive: extract
  instruction: "$prompts/question/enhance_question.md"
  input:
    apex_question: "$apex_question"
    corpus_context: "$refresh_state.l0_summary"
    characterization: "$characterize"
  save_as: step_only
```

**Change:** `corpus_context` now points to `$refresh_state.l0_summary` instead of `$load_prior_state.l0_summary`. On fresh builds, this contains real extracted content instead of empty string.

### Step: decompose (updated input wiring)

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
```

**Change:** Added `l0_summary: "$refresh_state.l0_summary"` to input. The `recursive_decompose` primitive already reads L0 summaries from DB as fallback context for the decompose prompt's "source material summaries." With this explicit wiring, the input is guaranteed to contain real content.

**Note:** The `recursive_decompose` primitive also reads L0 directly from DB when building "source material summaries" for each recursion level. With source_extract having populated the DB, the primitive should see real content regardless of the input wiring. The explicit input wiring is belt-and-suspenders.

### Step: extraction_schema (unchanged)

```yaml
- name: extraction_schema
  primitive: extract
  instruction: "$prompts/question/extraction_schema.md"
  input:
    question_tree: "$decomposed_tree"
    characterize: "$characterize"
    audience: "$audience"
  save_as: step_only
```

**Note:** Still runs. The evidence_loop reads `$extraction_schema` for internal synthesis prompt generation. The extraction_schema also provides question-shaped directives that gap_processing uses for targeted re-extraction. It no longer drives the primary L0 extraction.

### Step: l0_extract (REMOVED)

The current `l0_extract` step is removed entirely. Its work is now done by `source_extract` (generic extraction) and `gap_processing` (targeted re-extraction for MISSING verdicts).

### Step: l2_webbing (NEW)

Web branch answer nodes for cross-cutting connections that the question tree hierarchy misses.

```yaml
- name: l2_webbing
  primitive: web
  instruction: "$prompts/question/question_web.md"
  response_schema:
    type: object
    properties:
      edges:
        type: array
        items:
          type: object
          properties:
            source:
              type: string
            target:
              type: string
            relationship:
              type: string
            shared_resources:
              type: array
              items:
                type: string
            strength:
              type: number
          required: ["source", "target", "relationship", "shared_resources", "strength"]
          additionalProperties: false
    required: ["edges"]
    additionalProperties: false
  depth: 2
  save_as: web_edges
  model_tier: mid
  temperature: 0.2
  on_error: "skip"
```

## New Prompt: source_extract.md

Adapted from `doc_extract.md`. Content-type neutral. Same dehydration-friendly schema.

```markdown
You are distilling a single source into a reference card. Not summarizing — distilling. Keep what someone MUST understand, discard what they don't.

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE. A few hundred words total. If your extraction approaches the length of the original source, you are rewriting it, not distilling it. Stop and cut.

TOPICS ARE DIMENSIONS OF UNDERSTANDING, NOT SECTIONS.
- If the source has 5 examples illustrating one concept, that is ONE topic (the concept), not five.
- If three sections describe one system from different angles, that is ONE topic (the system).
- Ask: "Would removing this topic leave a gap in understanding?" If no, it doesn't deserve to be a topic.

HOW TO DISTILL:
1. Read the whole source. What does it DO or SAY? Not describe — DO or SAY.
2. What are the key CAPABILITIES, DECISIONS, MECHANISMS, or FINDINGS?
3. Group into the natural dimensions of understanding. Most sources have 2-4.
4. Write each topic as a dense sentence or two. Specific names, terms, identifiers. No filler.

RULES:
- Be concrete: actual names, terms, references from the source
- Preserve temporal context where present: when written, what state things were in
- Do NOT editorialize
- Topic names are used for clustering — name concepts specifically. "Spatial Canvas Renderer" not "Rendering."
- The `summary` field is a single-sentence distillation used when even the `current` field can't fit downstream. Make it count.
- Entities: cross-references to other sources, systems, people, decisions

Output valid JSON only:
{
  "headline": "2-6 word source label",
  "orientation": "2-3 sentences. What this source is, what it does or concludes, what to take away.",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "One sentence: the key point of this topic.",
      "current": "One to three sentences. The specific capability, decision, or finding. Names, identifiers, specifics.",
      "entities": ["system: Pyramid Engine", "component: CanvasRenderer", "decision: switched from REST to IPC"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
```

### Dehydration cascade

The schema supports the same cascade as document.yaml:

```yaml
dehydrate:
  - drop: "topics.current"     # verbose detail first
  - drop: "topics.entities"    # cross-refs next
  - drop: "topics.summary"    # one-liners next
  - drop: "topics"            # whole array next
  - drop: "orientation"       # node summary last
```

At maximum dehydration: `headline` only. At moderate pressure: `headline` + `orientation` + `topics[].summary`. This is the same multi-level structure proven in the document pipeline.

## Prompt Updates

### decompose.md

The following prompt mitigations can be REMOVED once the pipeline is reordered (they compensate for decompose not seeing real content):

- "STAY WITHIN THE CORPUS" section — unnecessary when decompose sees actual L0 content
- "BAD decomposition — never ask about individual documents" examples about CLAUDE.md, README.md — unnecessary when decompose sees what files actually exist
- "CALIBRATE TO ACTUAL COMPLEXITY" — less critical when decompose sees the actual scale

The following should be KEPT (they address question quality independent of data availability):

- "WHAT GOOD DECOMPOSITION LOOKS LIKE" — conceptual framing (purpose, behavior, architecture)
- "BRANCH vs LEAF" — structural guidance
- "DEPTH RULES" — depth-1 branches, depth-2 parent anchoring
- "GRANULARITY GUIDANCE" — scale parameter semantics
- "HOW TO DECOMPOSE" — step-by-step process

### question_web.md

Remove Pillar 37 violation: "Aim for 3-15 edges depending on node count." Replace with: "Quality over quantity — only report connections that a reader would genuinely benefit from knowing."

### extraction_schema.md

The current consolidated directives approach ("4-8 themes") is correct for its new role — generating directives for gap_processing's targeted re-extraction. No changes needed beyond what was already applied.

## Full question.yaml v2

```yaml
schema_version: 1
id: question-pipeline
name: Question Pipeline
description: "Extract-first question-driven knowledge pyramid. Generic L0 → web → decompose → evidence → gaps."
content_type: question
version: "2.0.0"
author: "wire-node"

defaults:
  model_tier: mid
  temperature: 0.3
  on_error: "retry(2)"

steps:
  # ── Phase 0: Load prior state ───────────────────────────────────────
  - name: load_prior_state
    primitive: cross_build_input
    save_as: step_only

  # ── Phase 1: Generic L0 extraction (first question only) ───────────
  - name: source_extract
    primitive: extract
    instruction: "$prompts/question/source_extract.md"
    for_each: "$chunks"
    when: "$load_prior_state.l0_count == 0"
    dispatch_order: "largest_first"
    concurrency: 4
    node_id_pattern: "Q-L0-{index:03}"
    depth: 0
    save_as: node
    max_input_tokens: 80000
    split_strategy: "sections"
    split_overlap_tokens: 500
    split_merge: true
    merge_instruction: "$prompts/shared/merge_sub_chunks.md"
    on_error: "retry(3)"
    on_parse_error: "heal"
    heal_instruction: "$prompts/shared/heal_json.md"

  # ── Phase 2: L0 webbing (first question only) ──────────────────────
  - name: l0_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    input:
      nodes: "$source_extract"
    response_schema:
      type: object
      properties:
        edges:
          type: array
          items:
            type: object
            properties:
              source:
                type: string
              target:
                type: string
              relationship:
                type: string
              shared_resources:
                type: array
                items:
                  type: string
              strength:
                type: number
            required: ["source", "target", "relationship", "shared_resources", "strength"]
            additionalProperties: false
      required: ["edges"]
      additionalProperties: false
    depth: 0
    save_as: web_edges
    compact_inputs: true
    model_tier: mid
    temperature: 0.2
    on_error: "skip"
    when: "$load_prior_state.l0_count == 0"

  # ── Phase 3: Refresh state after extraction ─────────────────────────
  - name: refresh_state
    primitive: cross_build_input
    save_as: step_only

  # ── Phase 4: Question-driven decomposition ──────────────────────────

  - name: enhance_question
    primitive: extract
    instruction: "$prompts/question/enhance_question.md"
    input:
      apex_question: "$apex_question"
      corpus_context: "$refresh_state.l0_summary"
      characterization: "$characterize"
    save_as: step_only

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
      apex_question: "$apex_question"
      granularity: "$granularity"
      max_depth: "$max_depth"
      characterize: "$characterize"
      audience: "$audience"
      existing_tree: "$load_prior_state.question_tree"
      existing_answers: "$load_prior_state.overlay_answers"
      evidence_sets: "$load_prior_state.evidence_sets"
      gaps: "$load_prior_state.unresolved_gaps"
      l0_summary: "$refresh_state.l0_summary"
    save_as: step_only

  # Both decompose and decompose_delta write to $decomposed_tree (canonical alias).

  - name: extraction_schema
    primitive: extract
    instruction: "$prompts/question/extraction_schema.md"
    input:
      question_tree: "$decomposed_tree"
      characterize: "$characterize"
      audience: "$audience"
    save_as: step_only

  # ── Phase 5: Evidence answering ─────────────────────────────────────
  # No separate l0_extract step. evidence_loop works from generic L0 nodes.
  # extraction_schema is used internally for synthesis prompts and gap targeting.

  - name: evidence_loop
    primitive: evidence_loop
    input:
      question_tree: "$decomposed_tree"
      extraction_schema: "$extraction_schema"
      load_prior_state: "$refresh_state"
      reused_question_ids: "$reused_question_ids"
      build_id: "$build_id"
    save_as: step_only

  - name: gap_processing
    primitive: process_gaps
    input:
      evidence_loop: "$evidence_loop"
      load_prior_state: "$refresh_state"
    save_as: step_only

  # ── Phase 6: Cross-cutting webbing ──────────────────────────────────

  - name: l1_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    response_schema:
      type: object
      properties:
        edges:
          type: array
          items:
            type: object
            properties:
              source:
                type: string
              target:
                type: string
              relationship:
                type: string
              shared_resources:
                type: array
                items:
                  type: string
              strength:
                type: number
            required: ["source", "target", "relationship", "shared_resources", "strength"]
            additionalProperties: false
      required: ["edges"]
      additionalProperties: false
    depth: 1
    save_as: web_edges
    compact_inputs: true
    model_tier: mid
    temperature: 0.2
    on_error: "skip"

  - name: l2_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    response_schema:
      type: object
      properties:
        edges:
          type: array
          items:
            type: object
            properties:
              source:
                type: string
              target:
                type: string
              relationship:
                type: string
              shared_resources:
                type: array
                items:
                  type: string
              strength:
                type: number
            required: ["source", "target", "relationship", "shared_resources", "strength"]
            additionalProperties: false
      required: ["edges"]
      additionalProperties: false
    depth: 2
    save_as: web_edges
    compact_inputs: true
    model_tier: mid
    temperature: 0.2
    on_error: "skip"

post_build: []
```

## Verification Checklist

After implementation, verify:

1. **Fresh build on vibesmithy (34 files):**
   - [ ] source_extract creates 34 Q-L0-* nodes with generic content
   - [ ] l0_webbing creates web edges between related L0 nodes
   - [ ] decompose sees real L0 content (check server logs with llm_debug_logging)
   - [ ] No questions about absent topics (testing, deployment, CLAUDE.md)
   - [ ] Empty/no-evidence node count significantly reduced from 28% baseline
   - [ ] Apex names what the software IS and DOES
   - [ ] 3-4 layers

2. **Fresh build on Core Selected Docs (127 files):**
   - [ ] source_extract handles 127 chunks with rate limiting
   - [ ] l0_webbing uses compact_inputs at scale
   - [ ] Dehydration doesn't lose critical content
   - [ ] Build completes within reasonable time

3. **Second question on same slug (delta build):**
   - [ ] source_extract skips (when guard: l0_count != 0)
   - [ ] l0_webbing skips
   - [ ] decompose_delta runs, reuses existing L0
   - [ ] evidence_loop reuses existing answers where questions overlap

4. **Haiku blind assessment:**
   - [ ] Two assessors score the vibesmithy pyramid
   - [ ] Target: both PASS (currently CONDITIONAL PASS / FAIL)
   - [ ] Empty node rate under 5%
   - [ ] Apex quality 4+/5

## Migration

This is a non-breaking change. The existing `question.yaml` can be renamed to `question-v1.yaml` as a reference. The new pipeline uses the same primitives, same DB schema, same node ID patterns. Existing slugs built with v1 remain valid — they just have question-shaped L0 nodes instead of generic ones. Rebuilds on existing slugs will detect L0 already exists and skip source_extract.

## Cost Analysis

**First question (fresh build, 34 files):**
- v1: extraction_schema (1 call) + l0_extract (34 calls) + evidence_loop + gap_processing
- v2: source_extract (34 calls) + l0_webbing (1 call) + evidence_loop + gap_processing
- **Delta: +1 LLM call (webbing), -1 LLM call (extraction_schema no longer drives L0)**
- Net: approximately equivalent for first question

**Second question (delta build):**
- v1: extraction_schema (1 call) + l0_extract (34 calls, if L0 doesn't exist) + evidence_loop + gap_processing
- v2: evidence_loop + gap_processing (source_extract + webbing skip)
- **Delta: -35 LLM calls on second question** (no re-extraction needed)
- Net: significantly cheaper for subsequent questions

**127-doc corpus:**
- Webbing at L0 with compact_inputs: ~1 call, dehydrated
- Worth the cost for the decompose quality improvement
