# How the Chain System Works

This document is the complete reference for building and modifying knowledge pyramids via the chain system. It is written for someone who will only ever edit YAML chain definitions and MD prompt files, and needs a full mental model of the machinery those files drive, the design theory behind it, and the practical lessons learned so far.

Read `docs/architecture/understanding-web.md` for the design philosophy. Read `docs/question-pipeline-guide.md` for question pipeline specifics (canonical aliases, initial params, forking rules). This document covers the machinery, the theory, and the current state of the art.

---

## Part 1: The Big Picture

### What this system is

A chain is a YAML file that declares steps. Each step either calls an LLM or orchestrates a complex operation. The chain IS the recipe for building a knowledge pyramid — it can be forked, improved, and published as a Wire contribution (Pillar 28).

The system has two layers:
- **Equipment (Rust):** The executor runtime — concurrency, LLM client, SQLite access, error handling. This is the kitchen. Nobody forks a semaphore.
- **Recipe (YAML + prompts):** The step sequence, input wiring, and prompt content. These are intelligence decisions about how to build a pyramid. If an agent could improve it, it belongs here, not in Rust.

### The key design insights

These emerged from building and testing the system. They are not obvious from the code alone.

**Questions drive everything.** There is no "generic extraction." Every L0 extraction is shaped by the questions being asked. The decomposition runs first, the sub-questions are examined holistically, and the resulting extraction prompt determines what L0 looks for in each source file. Different questions on the same corpus produce different extractions. "What is this codebase?" extracts architecture. "What are the security vulnerabilities?" extracts auth flows. The extraction prompt is generated, not static (Pillar 26).

**The evidence base accretes.** The first question on a corpus is the most expensive — full decomposition, full extraction, full evidence gathering. The second question inherits the existing evidence and only does new work for genuine gaps. The tenth question is nearly free. Each question makes the evidence base richer for all future questions without redundancy.

**The underlying structure is a DAG, not a tree.** The same L0 evidence node can be KEEP'd by multiple questions at different weights. A node about authentication appearing in an architecture question (weight 0.9) and a security question (weight 0.8) and an operations question (weight 0.3) tells you that node is central to architecture and security, peripheral to operations. Redundant copies of evidence would destroy this signal.

**MISSING verdicts are demand signals, not creation orders.** When the answer step reports "I wish I had evidence about X," that's a signal — not a command. The gap processing step tries to fill it. But the demand existing even when unfilled is valuable information: it tells you what the understanding web lacks.

**Branches become answer nodes. Leaves are decomposition guidance.** In the question tree, branch questions get their own answer nodes in the pyramid (they're the L1, L2, etc. layers). Leaf questions guide what the branch answer covers but don't produce their own nodes. This means: if you want intermediate layers in the pyramid, the decomposition must produce branches, not all leaves. This is a consequence of the layer numbering system, explained next.

### How layer numbering works

The question tree has a depth (root at top, leaves at bottom). The pyramid has layers (L0 at bottom, apex at top). These map to each other via `layer = max_tree_depth - current_level + 1`.

The `+1` offset is critical: it ensures the lowest question layer is always L1 (above L0 extraction nodes). Without it, leaf questions would land at L0 — the same layer as extraction nodes — and the evidence loop would never answer them.

Concrete example with a depth-2 tree:
```
apex question          → tree level 0 → layer 3 (answered at L3 from L2 evidence)
├── branch question A  → tree level 1 → layer 2 (answered at L2 from L1 evidence)
│   ├── leaf question  → tree level 2 → layer 1 (answered at L1 from L0 evidence)
│   └── leaf question  → tree level 2 → layer 1
├── branch question B  → tree level 1 → layer 2
│   └── leaf question  → tree level 2 → layer 1
```

The evidence loop starts at layer 1 and works up:
1. Layer 1: answer all leaf questions using L0 evidence nodes → creates L1 answer nodes
2. Layer 2: answer all branch questions using L1 answer nodes → creates L2 answer nodes
3. Layer 3: answer the apex using L2 answer nodes → creates the apex

With a flat tree (all leaves, no branches):
```
apex question          → tree level 0 → layer 2
├── leaf question      → tree level 1 → layer 1 (answered from L0)
├── leaf question      → tree level 1 → layer 1
└── leaf question      → tree level 1 → layer 1
```

This still works — leaves at layer 1 get answered, apex at layer 2 synthesizes from them. Two-layer pyramid (L1 + apex).

**The practical implication:** More branches = more pyramid layers = richer intermediate structure. All leaves = two layers (leaf answers + apex). The decomposition prompt controls how deep the pyramid gets by choosing what's a branch and what's a leaf.

### Cross-slug builds

A question pyramid can reference other slugs — asking questions about evidence that lives in a different pyramid. In cross-slug builds:

- The current slug has NO source files and NO chunks. The `l0_extract` step is skipped because `$load_prior_state.l0_count > 0` (L0 nodes are counted from REFERENCED slugs, not the current slug).
- The evidence loop loads L0 nodes from the referenced slugs instead of the current slug.
- The zero-chunks requirement is relaxed for question pipelines — the chain doesn't abort on empty chunks because L0 extraction may be skipped entirely.
- Everything else runs identically: decomposition, extraction_schema, evidence loop, gap processing.

Cross-slug builds are how you ask questions that span multiple corpora: "How does authentication flow between the node and the Wire?" can reference both the `agent-wire-node` code pyramid and the `wire-platform` docs pyramid.

---

## Part 2: How Data Flows

### The execution model

```
build_runner.rs                    chain_executor.rs
     |                                  |
  1. Load question.yaml              4. For each step:
  2. Characterize corpus (LLM)          a. Resolve $refs in input block
  3. Build initial_params map           b. Build system prompt (from instruction file)
     {apex_question, audience,          c. Build user prompt (input → pretty JSON)
      build_id, ...}                    d. Call LLM (or first-class primitive)
     |                                  e. Parse JSON response
     +--→ execute_chain_from() ----→    f. Store output in step_outputs
                                        g. Optionally save as pyramid node
                                     5. Build complete
```

### How variables resolve

Every `$ref` in YAML resolves against three sources, checked in order:

1. **step_outputs** — output of a completed step. `$enhance_question` → the enhance step's JSON output. `$enhance_question.enhanced_question` → the `enhanced_question` field of that output. Dot-access works to any depth.

2. **initial_params** — values injected by build_runner before the chain starts. `$apex_question` → the user's question. `$build_id` → the generated tracking ID.

3. **canonical aliases** — special keys written by certain primitives. `$decomposed_tree` is written by both the `decompose` and `decompose_delta` steps, so downstream steps reference the question tree without caring which path ran.

**When resolution fails:**
- In a `when` expression → evaluates to `false`, step is silently skipped
- In an `input` block → chain aborts with error naming the unresolved reference
- In `instruction` or `instruction_from` → chain aborts with error

This cascading behavior is intentional. If `decompose` runs and `decompose_delta` is skipped, anything downstream that references `$decompose_delta` via a `when` condition will also be skipped (safe). But anything that references `$decompose_delta` in an `input` block without a guarding `when` will crash (bug in the YAML wiring).

### How the LLM receives your prompt

For steps using the `extract`, `classify`, or `synthesize` primitive:

**System prompt** = the resolved content of the `instruction` file. If `instruction_from` is set and resolves, that value is used instead (takes absolute precedence).

**User prompt** = the step's `input` block, with all `$ref` expressions resolved, serialized as **pretty-printed JSON**. This is done by `serde_json::to_string_pretty(resolved_input)`.

Concrete example. If your YAML says:
```yaml
input:
  apex_question: "$apex_question"
  corpus_context: "$load_prior_state.l0_summary"
```

The LLM receives this user prompt:
```json
{
  "apex_question": "What is this codebase and how is it organized?",
  "corpus_context": "- Q-L0-000: Next.js Config — Configures image optimization...\n- Q-L0-001: Package JSON — Scripts, dependencies..."
}
```

Your prompt file (the system prompt) should tell the LLM what these fields are: "You will receive a JSON object with `apex_question` (the question to expand) and `corpus_context` (summaries of the source material)."

**For `for_each` steps:** The system prompt stays the same across all items. The user prompt is each individual ITEM from the array, serialized as JSON. For `for_each: "$chunks"`, each item is a source file chunk — the LLM sees one file at a time.

**All responses must be valid JSON.** The executor parses every response with `serde_json`. If the LLM returns prose, markdown, or malformed JSON, the step fails — UNLESS `on_parse_error: "heal"` is configured.

**How healing works:** When a step has `on_parse_error: "heal"` and `heal_instruction: "$prompts/shared/heal_json.md"`, a parse failure triggers a recovery call:
- System prompt: the heal instruction file content
- User prompt: the broken LLM response (raw text)
- The healer's job: find valid JSON inside the broken response, extract it, return it

The healer sees whatever Mercury produced — truncated JSON, JSON wrapped in markdown fences, prose with embedded JSON fragments — and tries to recover the valid structure. This is the primary recovery path for Mercury 2 runaway responses, where the model produces 47K tokens of mostly-valid JSON that gets truncated mid-field. The healer can often extract the valid prefix.

If healing also fails, the executor retries the original call at temperature 0.1 (lower = more likely to produce valid JSON). If that fails too, the step fails per its `on_error` strategy.

**Practical rule:** Set `on_parse_error: "heal"` and `heal_instruction` on any step that processes large or unpredictable inputs. All prompts must still include JSON output format instructions — healing is a safety net, not a substitute.

### Template variables in prompts

Some prompts use double-brace `{{variable}}` placeholders. These are replaced by the executor BEFORE the prompt is sent to the LLM, using simple string substitution.

| Variable | Replaced with | Used in |
|----------|--------------|---------|
| `{{content_type}}` | `"code"`, `"document"`, `"conversation"` | decompose.md |
| `{{depth}}` | Current decomposition depth as string (`"1"`, `"2"`, ...) | decompose.md |
| `{{audience_block}}` | Audience description paragraph, or empty string if no audience | decompose.md, pre_map.md, answer.md |
| `{{content_type_block}}` | Content-type-specific guidance paragraph, or empty string | pre_map.md, answer.md |
| `{{synthesis_prompt}}` | User-provided synthesis guidance, or empty string | answer.md |

Template variables use double braces (`{{x}}`) to avoid conflicts with JSON braces. They are a DIFFERENT mechanism from `$ref` resolution — `$refs` resolve in YAML input blocks, `{{vars}}` resolve inside prompt file text.

### How outputs become pyramid nodes

When a step has `save_as: node`, the executor maps the LLM's JSON output to `pyramid_nodes` columns:

| LLM JSON field | Node column | Fallback chain |
|---------------|------------|----------------|
| `headline` | `headline` | Required — parsing utility extracts from multiple possible locations |
| `orientation` | `distilled` | First choice. Falls back to `distilled`, then `purpose` |
| `orientation` | `self_prompt` | First choice. Falls back to `self_prompt` field if present |
| `topics` | `topics` | Deserialized from JSON array into topic objects |
| `corrections` | `corrections` | Extracted from top-level AND from within each topic |
| `decisions` | `decisions` | Extracted from top-level AND from within each topic |
| `terms` | `terms` | Deserialized from JSON array |
| `dead_ends` | `dead_ends` | Extracted as string array |
| `source_nodes` | `children` | Normalized to canonical node ID format |

**Critical behavior:** `orientation` takes precedence over `distilled` for the `distilled` column. If your LLM returns both fields, `orientation` wins. This means extraction prompts should use `orientation` as the primary summary field.

The `node_id_pattern` generates the node's ID:
- `{index:03}` → zero-padded index (000, 001, 002...)
- `{depth}` → current pyramid layer number

The `depth` field sets which pyramid layer the node belongs to (0 = L0, 1 = L1, etc.).

---

## Part 3: Each Step in Detail

### Characterization (pre-chain)

Happens in `build_runner.rs` BEFORE the chain starts. One LLM call using `characterize.md` prompt. Uses the "max" tier model (frontier reasoning) because this is a judgment call that shapes the entire build.

**What it does:** Analyzes a sample of the source material and produces:
- `material_profile` — what kind of content this is ("a Next.js TypeScript codebase with 34 source files covering UI components, configuration, and API client code")
- `interpreted_question` — the user's question restated precisely
- `audience` — who will consume this pyramid ("a curious, intelligent non-developer" or "a senior software engineer")
- `tone` — how to write answers (technical, conversational, executive)

**Why it matters:** The audience flows into EVERY subsequent LLM call. A pyramid built for a high school student extracts and synthesizes differently than one built for a senior engineer. If characterization gets the audience wrong, everything downstream is misframed.

**Fallback:** If source files aren't available (e.g., cross-slug builds), characterization uses L0 pyramid summaries instead.

The characterization result is injected as `$characterize` (the material_profile string) and `$audience` in initial_params.

### Step 1: load_prior_state (cross_build_input primitive)

**Zero LLM calls.** Reads SQLite to load the full state from prior builds.

**Output fields:**

| Field | What it contains | Why it matters |
|-------|-----------------|---------------|
| `l0_count` | Number of L0 nodes | Controls whether L0 extraction runs (`when: "$load_prior_state.l0_count == 0"`) |
| `l0_summary` | Headlines + truncated distilled text of all L0 nodes | Context for the enhancer and decomposer — tells them what the corpus contains |
| `has_overlay` | Whether a question overlay exists from a prior build | Controls fresh vs delta decomposition path |
| `question_tree` | Persisted question tree from last build | Input to delta decomposition — the existing structure to evolve |
| `overlay_answers` | Existing L1+ answer nodes | Delta decomposition can cross-link to these instead of re-answering |
| `evidence_sets` | Grouped targeted re-examination L0 nodes | Context for delta decomposition — what evidence already exists |
| `unresolved_gaps` | MISSING verdicts not yet resolved | Delta decomposition can prioritize filling these |
| `is_cross_slug` | Whether this slug references other slugs | Controls how L0 evidence is loaded (own vs referenced) |
| `referenced_slugs` | List of referenced slug names | Evidence loop loads L0 from these slugs for cross-slug builds |

**Design theory:** Loading all prior state upfront (instead of querying per-step) ensures every subsequent step has the same view of the world. No TOCTOU races between steps.

### Step 2: enhance_question (extract primitive)

**One LLM call** with `enhance_question.md`.

**What the LLM receives:**
- System prompt: enhance_question.md content
- User prompt: `{"apex_question": "...", "corpus_context": "...", "characterization": "..."}`

**What it returns:** `{"enhanced_question": "..."}`

**Design theory:** Users ask brief questions. "What is this?" doesn't give the decomposer enough to work with. The enhancer sees sample headlines from the corpus and expands the question to name the actual territory. If the headlines show architecture docs, economic design, legal structure — the enhanced question acknowledges those dimensions.

**What we learned:** The enhancer must NOT list individual document names (produces overly verbose questions). It should identify DIMENSIONS visible in the headlines. It must return JSON (not raw text) because the extract primitive parses all responses as JSON.

### Step 3: decompose / decompose_delta (recursive_decompose primitive)

**Multiple LLM calls.** Recursively decomposes the enhanced question into a tree.

**How recursion works:**
1. First call: decomposes the apex question into L1 sub-questions using `decompose.md`
2. For each L1 sub-question marked as a BRANCH: another call decomposes it into L2 sub-questions
3. After each level: one call with `horizontal_review.md` checks for overlapping siblings and merges them
4. Repeats until all branches are resolved to leaves or max_depth is reached

**Template variables in decompose.md:**
- `{{content_type}}` → replaced with "code", "document", etc.
- `{{depth}}` → replaced with the current decomposition depth ("1", "2", etc.)
- `{{audience_block}}` → replaced with audience description or empty string

**The user prompt** at each recursion level contains the parent question and the source material summaries (L0 headlines + truncated distilled text for all source files).

**Design theory — branches vs leaves:**

This is the most important concept for prompt authors to understand.

BRANCHES become their own answer nodes in the pyramid. They are the L1, L2, etc. layers. When the evidence loop runs, it answers each branch question by synthesizing evidence from the layer below.

LEAVES are decomposition guidance. They tell the branch what sub-aspects to cover, but they don't produce their own nodes. They exist at the lowest layer of the question tree alongside the L0 evidence nodes.

**Practical implication:** If the decomposer marks everything as a leaf, the pyramid collapses to a single answer node (the apex answers directly from L0, no intermediate layers). For a useful multi-layer pyramid, the decomposer must produce branches for major areas. The decompose.md prompt must make this distinction clear.

**What we learned:**
- If horizontal_review.md converts branches to leaves, it destroys tree depth. The review should merge overlapping questions but NEVER change a branch to a leaf.
- The decomposer should use the source material summaries to identify the actual dimensions of the corpus, not invent categories from the question alone.
- For broad questions ("What is this corpus?"), top-level children should almost always be branches.

**Delta mode (`mode: delta`):**
On second+ question builds, the decomposer receives the existing question tree, existing answers, evidence sets, and unresolved gaps. It decomposes the new question and diffs against existing structure — sub-questions already answered become cross-links (no new work), gaps become priorities.

Both paths write to the canonical alias `$decomposed_tree`.

### Step 4: extraction_schema (extract primitive)

**One LLM call** with `extraction_schema.md`.

**What the LLM receives:**
- System prompt: extraction_schema.md content
- User prompt: `{"question_tree": <the full decomposed tree>, "characterize": "...", "audience": "..."}`

**What it returns:**
```json
{
  "extraction_prompt": "For each file, extract: (1) ... (2) ... Output valid JSON: {...} /no_think",
  "topic_schema": [{"name": "field_name", "description": "...", "required": true}],
  "orientation_guidance": "How detailed to be, what tone to use"
}
```

**Design theory:** This is where questions shape extraction. The extraction_schema step looks at ALL the decomposed sub-questions holistically and designs an extraction prompt tailored to what those questions need. "What are the security vulnerabilities?" produces an extraction prompt focused on auth flows, input validation, error handling. "What is this and how is it organized?" produces a broader prompt covering architecture, structure, relationships.

The `extraction_prompt` field IS the system prompt that L0 extraction will use (via `instruction_from`). The extraction_schema step is a meta-prompt — a prompt that generates a prompt.

**What we learned:**
- The generated extraction_prompt MUST include JSON output format instructions and `/no_think`. If it doesn't, Mercury produces conversational markdown instead of structured JSON, and L0 extraction fails with "No JSON found."
- The extraction_schema.md prompt must explicitly tell the LLM that its generated prompt will be used verbatim as a system prompt for another LLM call. The generated prompt must be self-contained.

### Step 5: l0_extract (extract primitive, for_each)

**Parallel LLM calls** — one per source file chunk, concurrency controlled.

**What the LLM receives:**
- System prompt: the VALUE of `$extraction_schema.extraction_prompt` (via `instruction_from` — this is the generated prompt, not a file)
- User prompt: one source file chunk (the raw file content)

**What it returns:** Extraction JSON matching the schema designed by step 4 (headline, orientation, topics with schema-specific fields, entities).

**Conditional:** Only runs when `$load_prior_state.l0_count == 0` (no existing L0 nodes). On subsequent question builds against the same corpus, L0 already exists and this step is skipped.

**Node IDs:** `Q-L0-{index:03}` (Q prefix distinguishes question-shaped extraction from mechanical C-L0/D-L0 nodes).

**What we learned:**
- Mercury 2 occasionally enters runaway generation mode on small inputs, producing 47K+ tokens. This is a model behavior issue, not a prompt issue. Structured JSON output (via the generated prompt's format instructions) and `/no_think` reduce the frequency.
- The `orientation` field in extraction output maps to the node's `distilled` column. Extraction prompts should make `orientation` the primary summary field.

### Step 6: evidence_loop (evidence_loop primitive)

**Multiple LLM calls.** Orchestrates per-layer evidence answering from leaves to apex.

**For each layer (bottom-up):**

**6a. Pre-map** — One LLM call with `pre_map.md`. Receives all questions at this layer + all node summaries from the layer below. Returns a candidate map: `{question_id: [candidate_node_ids]}`.

The pre-mapper's job is to OVER-INCLUDE. False positives are cheap (the answer step prunes them via DISCONNECT). Missed evidence is permanent — if the pre-mapper doesn't connect a question to a relevant L0 node, that evidence is invisible to the answer step.

For large evidence bases (when total node summaries exceed the token budget), two-stage mapping activates: Stage 1 (`pre_map_stage1.md`) scans evidence set indexes to identify relevant sets, then Stage 2 (`pre_map.md`) scans only nodes within those sets.

**6b. Answer** — Parallel LLM calls (up to 5 concurrent) with `answer.md`. One call per question. Each receives the question + its candidate evidence nodes.

The answer step must produce:
- **KEEP** (weight 0.0-1.0, reason) — this evidence supports the answer. Weight indicates centrality.
- **DISCONNECT** (reason) — false positive from pre-mapping. Not actually relevant.
- **MISSING** (description) — evidence the question needed but couldn't find.
- Synthesized answer using only KEEP evidence (headline, distilled, topics).

**6c. Persist** — Each answered question becomes a pyramid node. Evidence links saved to `pyramid_evidence`, scoped by build_id.

**6d. Reconcile** — Cross-references between sibling answer nodes identified and saved as web edges.

**Design theory — evidence utilization:**

The metric that matters most is: what fraction of L0 nodes were touched by the evidence loop? If only 8/34 L0 nodes are KEEP'd by any question, 26 nodes of extracted evidence are invisible. This means either:
- The extraction produced evidence the questions don't need (extraction_schema misaligned with decomposition)
- The pre-mapper can't find the connection between evidence and questions (pre_map prompt too narrow)
- The decomposition doesn't cover enough of the corpus (decompose prompt too shallow)

Low evidence utilization is the primary signal that something upstream is wrong.

**What we learned:**
- The answer prompt originally said "focus on your STRONGEST evidence" — this caused the apex to mention only 3 of 7 dimensions. Changed to "every KEEP candidate that represents a genuinely distinct dimension should be reflected." Dimension coverage beats depth on any single dimension.
- Evidence weights are not just scores — they're the graph signal. A node KEEP'd by 5 different questions at varying weights is the load-bearing fact of the corpus.

### Step 7: gap_processing (process_gaps primitive)

**Zero or more LLM calls** depending on whether gaps resolve to source files.

1. Reads all MISSING verdicts from the evidence loop
2. For each gap: extracts keywords, scores them against L0 node headlines + distilled text + topics
3. If matching source files found: calls LLM with `targeted_extract.md` to re-examine those files focused on the gap's question
4. Saves new L0 nodes with `L0-{uuid}` IDs and the gap question in `self_prompt`
5. Marks gaps as resolved

**What we learned:**
- The original gap-to-file resolver only matched against file paths. Gap descriptions use content terms ("JSONB field names", "foreign-key columns") that only appear in L0 node topics, not file paths. The resolver now searches headlines + distilled + topics.
- Even when gaps can't be resolved (no matching source files), recording them as resolved prevents re-processing on subsequent builds.

---

## Part 4: YAML Step Fields — Complete Reference

### Core fields

| Field | Type | What it does |
|-------|------|-------------|
| `name` | string | Unique step identifier. Referenced as `$name` by downstream steps. |
| `primitive` | string | What kind of operation. See below. |
| `instruction` | string | Prompt text or `$prompts/path` file reference. Resolved at chain load time. |
| `instruction_from` | string | Dynamic prompt — resolves a `$ref` and uses the VALUE as the system prompt. Takes absolute precedence over `instruction`. |
| `input` | map | Input bindings. Keys become JSON fields in the user prompt. Values are `$ref` expressions resolved at runtime. |
| `save_as` | string | `node` → persist as pyramid node in SQLite. `web_edges` → persist as cross-layer edges. `step_only` → in-memory only (available as `$step_name` for downstream steps but NOT persisted to SQLite — lost if the build crashes mid-flight and not available for resume). |
| `when` | string | Conditional execution. Expression like `"$ref == value"`. If false or unresolvable, step is skipped. |
| `mode` | string | Modifier for primitive behavior. Currently only `delta` (for recursive_decompose). |

### Iteration and concurrency

| Field | Type | What it does |
|-------|------|-------------|
| `for_each` | string | Iterate over an array. `$chunks` for source files, `$step_name.field` for prior output. Each item becomes the user prompt. |
| `concurrency` | int | Max parallel LLM calls during `for_each`. Default 1. |
| `dispatch_order` | string | **Not yet implemented.** When implemented, `"largest_first"` will sort items by size before dispatching for better parallelization. Currently items process in insertion order. |

### Node output

| Field | Type | What it does |
|-------|------|-------------|
| `depth` | int | Pyramid layer (0 = L0, 1 = L1, etc.). Required when `save_as: node`. |
| `node_id_pattern` | string | ID generation pattern. `{index:03}` → zero-padded index. `{depth}` → layer number. |

### Model control

| Field | Type | What it does |
|-------|------|-------------|
| `model_tier` | string | `mid` (Mercury 2, 120K context), `high` (Qwen, 900K), `max` (Grok, >900K). |
| `model` | string | Direct model override (e.g., `qwen/qwen3.5-flash-02-23`). Bypasses tier system. |
| `temperature` | float | Override for this step. Lower = more deterministic. |

### Error handling

| Field | Type | What it does |
|-------|------|-------------|
| `on_error` | string | `retry(N)` → retry N times with reducing temperature. `skip` → log error, produce null, continue. `abort` → stop the entire chain. |
| `on_parse_error` | string | `heal` → on JSON parse failure, make a second LLM call with the heal_instruction prompt to repair the output. |
| `heal_instruction` | string | Prompt file for JSON healing (e.g., `$prompts/shared/heal_json.md`). |

### Structured output

| Field | Type | What it does |
|-------|------|-------------|
| `response_schema` | object | JSON Schema object. When present, uses OpenRouter's structured output feature (`strict: true`) to guarantee the response matches the schema. |

**How response_schema changes behavior:**
- **Without:** The LLM generates free-form text. The executor extracts JSON from the response (finding the first `{...}` block). On parse failure, retries at temperature 0.1.
- **With:** The LLM is constrained by OpenRouter to produce JSON matching the schema exactly. No post-parse extraction needed. But: structured output is significantly slower on Mercury 2 (30-60x for extraction tasks). This is why L0 extraction does NOT use response_schema — free-form JSON + heal is faster.

Structured output is used for clustering and webbing steps where the output schema is critical (thread assignments must be valid node IDs, edge lists must have required fields).

### Document splitting (for oversized source files)

These fields apply to any `for_each` extraction step — both question pipeline (`l0_extract`) and mechanical presets. They handle source files that exceed the LLM's context window.

| Field | Type | What it does |
|-------|------|-------------|
| `max_input_tokens` | int | Maximum estimated tokens for a single chunk. Chunks exceeding this are split. |
| `split_strategy` | string | `"sections"` (split by markdown headers, default) or `"lines"` (fallback for sections that individually exceed the limit). |
| `split_overlap_tokens` | int | Overlap between split parts (for context continuity). |
| `split_merge` | bool | When true, split parts are extracted independently, then merged into one node. |
| `merge_instruction` | string | Prompt file for the merge step (e.g., `$prompts/shared/merge_sub_chunks.md`). Default: "Combine these extractions, deduplicate topics, preserve all entities/decisions." |

**How splitting works:**
1. If a chunk exceeds `max_input_tokens`, it's split by markdown headers into sub-chunks
2. Sub-chunks are grouped until adding the next would exceed the token budget, with overlap
3. Each sub-chunk is extracted independently (separate LLM call)
4. If `split_merge: true`, the extractions are merged via a merge LLM call into one final node
5. If a single section exceeds the limit, it falls through to line-based splitting

**Used in question pipeline:** `l0_extract` should have `max_input_tokens: 80000`, `split_strategy: "sections"`, `split_merge: true`, `merge_instruction` set. Without these, oversized documents will exceed Mercury's 120K context and either crash or truncate.

### Batching (for steps processing many items at once)

These fields control how items are grouped when a step processes an array of nodes in a single LLM call (as opposed to `for_each` which processes one item per call).

| Field | Type | What it does |
|-------|------|-------------|
| `batch_size` | int | Maximum items per batch. Items are split proportionally (127 items with batch_size=100 → two batches of 64 and 63). |
| `batch_max_tokens` | int | Maximum estimated tokens per batch. Items are added greedily until the next item would exceed the budget. Composes with batch_size. |

When both are set, `batch_max_tokens` takes priority (token-aware batching) with `batch_size` as a cap on items per batch.

### Dehydration (progressive field dropping for oversized batches)

When items need to fit in a token budget and they're too large, dehydration progressively drops fields to make them smaller.

| Field | Type | What it does |
|-------|------|-------------|
| `dehydrate` | array | Cascade of `{drop: "field.path"}` entries. When an item doesn't fit in a batch, fields are dropped in order until it fits. |

Example from document.yaml's clustering step:
```yaml
dehydrate:
  - drop: "topics.current"      # First: drop the verbose current field from each topic
  - drop: "topics.entities"     # Then: drop entities
  - drop: "topics"              # Then: drop all topics
  - drop: "orientation"         # Finally: drop orientation (keep only headline)
```

**How it works:** When batch_max_tokens is set and an item would exceed the budget, the executor applies drops in order. After each drop, it re-estimates the token count. It stops when either the item fits or no more drops remain.

**A single oversized item is never dropped entirely.** If all dehydration drops are exhausted and the item still exceeds the budget, it gets its own batch and is sent as-is. Evidence is never silently lost.

**The dehydration order matters.** The cascade is designed to drop the least valuable content first (verbose topic text) and the most valuable last (headline). The order encodes a judgment about which information is most dispensable under token pressure.

### Compact inputs (for webbing steps)

| Field | Type | What it does |
|-------|------|-------------|
| `compact_inputs` | bool | When true, strips full topic payloads and keeps only: node_id, headline, source_path, entities (max 16, deduplicated). ~60-70% smaller payload. |

Used for cross-layer webbing where the LLM only needs to know what each node IS (headline + entities), not its full content.

### Recursive clustering (for mechanical upper-layer convergence)

These fields are used by mechanical preset chains (document.yaml, code.yaml) for their upper-layer synthesis. The question pipeline does NOT use recursive clustering — it uses the evidence_loop primitive instead, which answers questions from evidence rather than clustering by similarity.

These fields are documented here because the mechanical YAMLs remain as reference implementations for the batching, dehydration, and convergence strategies they encode. The logic in these presets was validated through extensive tuning and represents proven approaches to organizing large corpora.

| Field | Type | What it does |
|-------|------|-------------|
| `recursive_cluster` | bool | Enables convergence loop: cluster → synthesize → repeat until single node remains. |
| `cluster_instruction` | string | Prompt for the clustering sub-step. |
| `cluster_item_fields` | array | Which fields from each node to include in clustering input. |
| `cluster_response_schema` | object | JSON Schema for clustering output (must include `clusters` array, may include `apex_ready` boolean). |
| `convergence_fallback` | string | What to do if clustering doesn't converge. `"retry"` re-clusters. |
| `cluster_fallback_size` | int | Fallback cluster size if convergence fails. |
| `direct_synthesis_threshold` | int or null | If set and node count ≤ this value, skip clustering and synthesize directly. |

**How the convergence loop works:**
1. Takes current nodes at depth D
2. Clusters them into semantic groups via LLM (using cluster_instruction)
3. Synthesizes each group into a parent node at depth D+1
4. If only one node remains → it's the apex, loop ends
5. If `apex_ready: true` in cluster response → synthesize remaining nodes directly into apex
6. Otherwise → repeat with the new parent nodes

**Safety net:** If clustering produces more clusters than source nodes (degenerate case), the executor force-merges the smallest clusters until count < node count.

---

## Part 5: Prompt Engineering — What We Learned

### Mercury 2 behavior

Mercury 2 is a diffusion-based LLM. Unlike autoregressive models, it sometimes commits to a very long output during its denoising pass and generates until hitting the 48K token ceiling. This manifests as:
- A 5K-token prompt producing a 47K-token response
- Usually happens on freeform string fields with no structural constraint
- Structured JSON output naturally bounds this (the model has to produce valid JSON, which limits runaway)
- `/no_think` suppresses reasoning tokens and reduces the frequency

**Practical rule:** Every prompt must end with `/no_think`. Every prompt must request structured JSON output with explicit format instructions. These two practices together prevent most Mercury runaway issues.

### Pillar 37 — no numbers in prompts

Never put word counts, sentence counts, topic counts, or length limits in a prompt. "Write a dense paragraph" is fine. "Write 2-4 sentences" is a Pillar 37 violation. "Most documents have 2-4 topics" is a Pillar 37 violation. The LLM decides the shape of its output based on the content, not based on a number.

Every number constraining LLM output seems defensible in context — "2-4 sentences is just a guideline!" That's the trap. The number becomes a ceiling that prevents the LLM from producing the right answer when the right answer needs 6 sentences or 1 sentence.

### Evidence utilization is the key metric

The single most important quality signal is: what fraction of L0 nodes were KEEP'd by at least one question? If 30% of extracted evidence is never referenced, something upstream is misaligned:
- extraction_schema generated an extraction prompt that captured things no question asks about
- decompose produced questions that don't cover the corpus's actual dimensions
- pre_map can't find the connection between evidence and questions (too narrow)

### The horizontal review trap

The horizontal_review step merges overlapping sibling questions. It was originally also converting branches to leaves ("this question is specific enough to answer directly"). Converting branches to leaves destroys tree depth — the pyramid collapses to fewer layers. The review must ONLY merge overlapping questions, never change branch/leaf designation.

### Dimension coverage vs depth

When the answer step synthesizes from evidence, it's tempted to focus on the "strongest" evidence and produce a deep answer about a few things. But for apex and branch questions, coverage matters more than depth — the answer should acknowledge ALL major dimensions visible in the evidence, not just the top 3. This was a specific prompt fix: "every KEEP candidate that represents a genuinely distinct dimension should be reflected in your synthesis."

---

## Part 6: Files and Deployment

### Prompt files (what you edit)

| File | Step | Input fields | Expected output |
|------|------|-------------|----------------|
| `chains/prompts/question/characterize.md` | Pre-chain | Source material sample | `{material_profile, interpreted_question, audience, tone}` |
| `chains/prompts/question/enhance_question.md` | enhance_question | `{apex_question, corpus_context, characterization}` | `{enhanced_question}` |
| `chains/prompts/question/decompose.md` | decompose | Parent question + source material (via template vars `{{content_type}}`, `{{depth}}`, `{{audience_block}}`) | JSON array of `{question, prompt_hint, is_leaf}` |
| `chains/prompts/question/decompose_delta.md` | decompose_delta | Same as decompose + existing tree/answers/gaps (via template vars) | `{sub_questions, reused_question_ids}` |
| `chains/prompts/question/horizontal_review.md` | Inside recursive_decompose | List of sibling questions | `{merges: [...], mark_as_leaf: []}` (mark_as_leaf should always be empty) |
| `chains/prompts/question/extraction_schema.md` | extraction_schema | `{question_tree, characterize, audience}` | `{extraction_prompt, topic_schema, orientation_guidance}` |
| `chains/prompts/question/pre_map.md` | Inside evidence_loop | All questions at layer + all node summaries below | `{mappings: {question_id: [node_ids]}}` |
| `chains/prompts/question/pre_map_stage1.md` | Inside evidence_loop (large) | All questions + evidence set indexes | `{relevant_sets: [set_ids]}` |
| `chains/prompts/question/answer.md` | Inside evidence_loop | Question + candidate evidence nodes | `{headline, distilled, topics, verdicts, missing, ...}` |
| `chains/prompts/question/targeted_extract.md` | Inside process_gaps | Source file + gap question | Extraction JSON matching the original schema |
| `chains/prompts/question/synthesis_prompt.md` | Inside evidence_loop (Rust) | Question tree + L0 summary + extraction schema | `{pre_mapping_prompt, answering_prompt, web_edge_prompt}` |

**Note on synthesis_prompt.md:** This prompt is called internally by the evidence_loop primitive via `extraction_schema::generate_synthesis_prompts()` in Rust. It generates per-layer synthesis instructions that the evidence loop uses for pre-mapping and answering. It is NOT a YAML step — there is no `synthesis_prompts` step in question.yaml. Editing this prompt affects how the evidence loop frames its internal LLM calls.

### Chain files (what you edit)

| File | What it is |
|------|-----------|
| `chains/defaults/question.yaml` | **The canonical question pipeline recipe.** This is the active build path. |

### Deprecated mechanical presets (reference only)

These YAMLs are deprecated as build paths — the question pipeline replaces them. They remain in the repo as **reference implementations** for the extraction, batching, dehydration, and convergence strategies they encode. The logic in these presets was validated through extensive tuning and represents proven approaches. When tuning the question pipeline, consult these for how problems were solved before.

| File | What it encodes |
|------|----------------|
| `chains/defaults/document.yaml` | Dehydration cascade order (topics.current → entities → topics → orientation). Token-aware batching at 50K with 150-item cap. Section-based splitting at 80K tokens with merge. Container sub-chain for batch+merge clustering. Recursive clustering with apex_ready signal. |
| `chains/defaults/code.yaml` | Content-type variant dispatch (frontend files get different prompts, config files get different prompts). Thread clustering with max items per thread. Code-specific topic naming conventions. |
| `chains/defaults/conversation.yaml` | Temporal ordering in thread synthesis. Batch+merge clustering for large conversation sets. |

### Deployment

Prompts and YAML are read from the runtime location: `~/Library/Application Support/wire-node/chains/`

The Tier 2 bootstrap syncs from the repo **on app startup only**. If you edit files while the app is running, sync manually:

```bash
SRC="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DST=~/Library/Application\ Support/wire-node/chains
cp "$SRC/defaults/question.yaml" "$DST/defaults/question.yaml"
for f in "$SRC/prompts/question/"*.md; do cp "$f" "$DST/prompts/question/$(basename "$f")"; done
```

Or restart the app.

### Equipment files (do not edit — for understanding only)

| File | What it does |
|------|-------------|
| `src-tauri/src/pyramid/chain_executor.rs` | Runs steps, dispatches primitives, manages for_each/concurrency, handles errors |
| `src-tauri/src/pyramid/chain_dispatch.rs` | Builds system/user prompts from step config, calls OpenRouter, parses responses, maps output to nodes |
| `src-tauri/src/pyramid/chain_resolve.rs` | Resolves `$ref` expressions against step_outputs, initial_params, and canonical aliases |
| `src-tauri/src/pyramid/chain_loader.rs` | Loads YAML, resolves `$prompts/` to file contents, validates structure |
| `src-tauri/src/pyramid/chain_engine.rs` | ChainStep struct, VALID_PRIMITIVES list, validation rules |
| `src-tauri/src/pyramid/build_runner.rs` | Entry point: characterizes, loads chain, injects initial_params, calls executor |
| `src-tauri/src/pyramid/llm.rs` | OpenRouter HTTP client, retry logic, model cascade (mid → high → max) |
| `src-tauri/src/pyramid/evidence_answering.rs` | Pre-map and answer logic (called internally by evidence_loop primitive) |
| `src-tauri/src/pyramid/question_decomposition.rs` | Recursive decomposition and horizontal review logic (called by recursive_decompose primitive) |
