# Prompt Catalog

Quick reference for all runtime-editable prompt files. Edit these to change pyramid behavior without rebuilding the app.

**Location:** `chains/prompts/` in the source tree (dev mode) or `~/Library/Application Support/wire-node/chains/prompts/` (release mode).

**Template convention:** `{{variable_name}}` — double braces, replaced at runtime. Unknown variables render as empty string.

---

## Question Pyramid Prompts

These shape how understanding webs are built on top of mechanical pyramids.

### `question/enhance_question.md`
**Purpose:** Expands the user's short question into a comprehensive apex question before decomposition.
**Template variables:** None (receives user question + characterization as the user prompt, not the system prompt).
**Key behavior:** Should preserve the user's intent while expanding implied scope. Default to non-technical framing unless the user explicitly asks for technical details.
**Called from:** `build_runner.rs` — runs once per build, before decomposition.

### `question/decompose.md`
**Purpose:** System prompt for the LLM that breaks questions into sub-questions.
**Template variables:**
- `{{content_type}}` — "code", "document", etc. The type of source material.
- `{{depth}}` — Current decomposition depth (1 = first level below apex).
- `{{audience_block}}` — When audience is set: "The person asking this question is {audience}..." When absent: empty string.
**Key behavior:** Produces a JSON array of `{question, prompt_hint, is_leaf}` objects. Should produce the MINIMUM number needed — no quotas, no prescribed counts (Pillar 37). Leaf = answerable directly from source; branch = needs further decomposition.
**Called from:** `question_decomposition.rs::call_decomposition_llm` — runs once per branch question during decomposition.

### `question/horizontal_review.md`
**Purpose:** Reviews sibling questions for overlap and decides which branches are specific enough to be leaves.
**Template variables:** None (siblings passed as user prompt).
**Key behavior:** One job: merge overlapping sibling questions. The `mark_as_leaf` array is forced empty — the decomposition step decides leaf vs branch, and horizontal review must not override that. Returns JSON with `merges` and `mark_as_leaf` (always empty) arrays.
**Called from:** `question_decomposition.rs::horizontal_review_siblings` — runs once per depth level after all siblings are decomposed.

### `question/pre_map.md`
**Purpose:** Maps questions to candidate evidence nodes from the layer below.
**Template variables:**
- `{{audience_block}}` — When audience is set: inclusive hint about the questioner. When absent: empty string.
- `{{content_type_block}}` — When source content type is known: "The source material is '{type}' content." When absent: empty string.
**Key behavior:** OVER-INCLUDE. If a node MIGHT be relevant, include it. The answering step handles pruning. ALL evidence is potentially relevant regardless of vocabulary — do not exclude based on technicality. Returns JSON `{mappings: {question_id: [node_ids]}}`.
**Called from:** `evidence_answering.rs::pre_map_layer` — runs once per layer during the evidence loop.

### `question/answer.md`
**Purpose:** Synthesizes an answer from candidate evidence with KEEP/DISCONNECT/MISSING verdicts.
**Template variables:**
- `{{audience_block}}` — When audience is set: strong jargon-gating instructions (translate ALL technical terms, never expose framework/file/function names). When absent: empty string.
- `{{synthesis_prompt}}` — Additional synthesis guidance generated from the question tree + L0 results.
- `{{content_type_block}}` — When source content type is known. When absent: empty string.
**Key behavior:** For each candidate, report a verdict. Then synthesize using ONLY KEEP evidence. Focus on strongest evidence — don't try to mention everything. Returns JSON with headline, distilled, topics, verdicts, missing, corrections, decisions, terms, dead_ends.
**Called from:** `evidence_answering.rs::answer_single_question` — runs once per question per layer (parallel, up to 5 concurrent).

---

## Code Chain Prompts

These shape how source code files are extracted into mechanical pyramid L0 nodes.

### `code/code_extract.md`
**Purpose:** System prompt for extracting a single source code file into a structured L0 node.
**Template variables:** None (file content passed as user prompt).
**Key behavior:** Has a HUMAN-INTEREST FRAMING block at the top. Describes what the code DOES for users, not just how it's built. When `{audience}` is available, shapes for that audience. Returns JSON with headline, distilled, topics, corrections, decisions, terms, dead_ends.
**Called from:** IR executor via chain YAML `instruction` reference.

### `code/code_distill.md`
**Purpose:** Distills/summarizes an already-extracted L0 node into a more concise form.
**Template variables:** None.
**Key behavior:** Same human-interest framing. Significance before mechanics.
**Called from:** IR executor via chain YAML.

### `code/code_extract_frontend.md`
**Purpose:** Variant extraction prompt specifically for frontend/UI code.
**Template variables:** None.
**Called from:** IR executor via `instruction_map` dispatch (when the classifier tags a file as frontend).

---

## Document Chain Prompts

These shape how documents are processed into mechanical pyramid nodes.

### `document/doc_extract.md`
**Purpose:** Extracts a single document into a structured L0 node.
**Template variables:** None.
**Key behavior:** Human-interest framing. What the document means and why it matters, not just what it contains.
**Called from:** IR executor via chain YAML.

### `document/doc_distill.md`
**Purpose:** Distills an extracted document node.
**Template variables:** None.
**Called from:** IR executor via chain YAML.

### `document/doc_classify_perdoc.md`
**Purpose:** Per-document classification (type, date, title, raw keywords).
**Template variables:** None (document header passed as user prompt).
**Called from:** IR executor via chain YAML `doc_classify_perdoc` step (parallel, concurrency 8).

### `document/doc_taxonomy.md`
**Purpose:** Normalizes raw keywords from per-doc classification into a shared concept taxonomy.
**Template variables:** None (classification results passed as user prompt).
**Called from:** IR executor via chain YAML `doc_taxonomy` step (single call).

### `document/doc_concept_areas.md`
**Purpose:** Identifies natural conceptual groupings from L0 headlines + concept tags.
**Template variables:** None.
**Called from:** IR executor via chain YAML `doc_concept_areas` step (single call).

### `document/doc_assign.md`
**Purpose:** Per-document thread assignment against the concept areas.
**Template variables:** None.
**Called from:** IR executor via chain YAML `doc_assign` step (parallel, concurrency 8).

### `document/doc_cluster.md`
**Purpose:** Legacy monolithic clustering prompt (replaced by concept_areas + assign in v4.0).
**Template variables:** None.
**Status:** May still be referenced by older chain versions.

---

## Editing Tips

1. **Changes take effect immediately** in dev mode (chains_dir points to source tree). In release mode, copy to `~/Library/Application Support/wire-node/chains/prompts/`.

2. **Test one change at a time.** Build a question pyramid, evaluate, change one prompt, rebuild, evaluate again.

3. **The most impactful prompts** for understanding web quality are (in order):
   - `decompose.md` — determines question structure, which determines everything
   - `answer.md` — determines synthesis quality and jargon filtering
   - `pre_map.md` — determines which evidence reaches the answering step
   - `enhance_question.md` — determines how the user's question is expanded

4. **Pillar 37:** Never prescribe output counts or structure. Describe goals and thinking frameworks. Let the intelligence decide.

5. **Audience flows through `{{audience_block}}`** in decompose.md, pre_map.md, and answer.md. If the user specified an audience, it appears in all three. If not, the blocks render as empty strings.
