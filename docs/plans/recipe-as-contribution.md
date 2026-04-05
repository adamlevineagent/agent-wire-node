# Recipe-as-Contribution: Question Pipeline Refactor

**Date:** 2026-04-04
**Status:** IMPLEMENTED. All 4 phases built, serial-verified, post-completion audit in progress.
**Pillars:** 2 (contributions all the way down), 18 (any number of chain definitions, one IR, one executor), 26 (extraction is question-shaped), 28 (recipe is itself a contribution), 37 (never prescribe outputs to intelligence)

## Problem

`build_runner.rs:run_decomposed_build()` is ~1200 lines of recipe frozen in Rust. The step sequence (characterize, enhance, decompose, extraction_schema, evidence loop, gap processing), input wiring, flow control, and conditional logic are intelligence decisions that should be a forkable YAML chain definition. An agent who discovers a better decomposition strategy or evidence-gathering sequence cannot contribute that improvement — the recipe is compiled Rust.

The mechanical fallback (`run_build()` at line 888) crashes on fresh slugs ("batch_cluster returned 0 threads") and is architecturally backwards — the question pipeline should never invoke the mechanical pipeline. Extraction is question-shaped (Pillar 26): the decomposed sub-questions determine what L0 looks for in each source file.

## Architecture

**Equipment (stays in Rust):** The executor runtime, step dispatch, concurrency (semaphore, parallel spawn), LLM API client, SQLite read/write, error handling, file watching, timer management. Kitchen appliances — nobody forks a semaphore.

**Recipe (moves to YAML):** The step sequence, which steps exist, what order they run, what inputs each step receives, the "for each layer" iteration, the "if gaps exist, re-examine" conditional, the "decompose recursively until leaves" control flow. Intelligence decisions about the best way to build a pyramid from a question.

---

## Phase 1: Quarantine (remove run_build fallback)

### 1.1 Reorder: extraction_schema before L0 check

**`build_runner.rs`** — Current order: characterize → ensure L0 (calls run_build) → enhance → decompose → extraction_schema. New order: characterize → enhance → decompose → extraction_schema → ensure L0 (direct extraction).

- Move L0 existence check to AFTER extraction_schema generation
- `decomp_context` (lines 909-933): for fresh slugs, fall back to characterization text (material_profile + audience + tone) instead of L0 summaries
- `enhance_question` (lines 960-978): also reads `base_l0_for_context` for sample headlines. For fresh slugs, pass characterization summary instead of L0 headlines. The enhancement still works — it gets corpus-level context from characterization instead of node-level from L0.
- `decompose`: uses decomp_context — works with characterization fallback
- `extraction_schema`: needs leaf_questions + characterization only — no L0 dependency

### 1.2 Replace run_build() with chain executor extraction

Delete the run_build call + write_tx hack (lines 880-899). Build a synthetic `ChainDefinition` with one `extract` step.

**Key finding from audit:** The executor's `resolve_instruction()` at `chain_executor.rs:955-969` already accepts raw text — it falls back to the instruction string when template file resolution fails. So the `instruction` field can be set directly to `ext_schema.extraction_prompt` (no `instruction_from` needed for Phase 1).

Synthetic chain configuration:
- `primitive: "extract"`
- `instruction`: set to `ext_schema.extraction_prompt` (raw text, not a file path)
- `for_each: "$chunks"`
- `content_type`: set to the slug's ACTUAL content_type (code/document/conversation), NOT "question"
- `node_id_pattern: "Q-L0-{index:03}"`
- `depth: 0`, `save_as: "node"`
- `concurrency`: from `ops.tier1.extraction_concurrency` (not hardcoded — Pillar 37)
- `max_input_tokens`: from `ops.tier2.pre_map_prompt_budget` or similar config (not hardcoded — Pillar 37)
- `split_strategy: "sections"`, `split_merge: true`
- `on_parse_error: "heal"` with heal_instruction from `$prompts/shared/heal_json.md`

Execute via `chain_executor::execute_chain_from()`. Reuses the entire executor: resume, error strategies, progress, node saving, split/merge.

**Precondition:** Chunks must exist in `pyramid_chunks` from prior `pyramid_ingest` call. The chain executor aborts if `count_chunks() == 0`. The UI calls `pyramid_ingest` before `pyramid_question_build`, so this is satisfied. Add a guard with clear error message if chunks are missing.

### 1.3 Q-L0 + existing L0 coexistence

When a slug already has mechanical L0 (C-L0, D-L0) and gets question-shaped L0 (Q-L0), both coexist at depth 0. The evidence loop loads ALL depth-0 nodes via `get_nodes_at_depth(slug, 0)` — both prefixes are included. The pre-mapper sees all L0 as candidates. No conflict.

For the `is_targeted_l0_id()` filter: Q-L0-001 starts with "Q-", not "L0-", so it's correctly classified as canonical (not targeted). No change needed.

For the apex finder: Q-L0 nodes are saved with `build_id LIKE 'qb-%'` — the overlay detection from the earlier understanding web build handles them correctly.

### 1.4 Prevent future fallback

Assert L0 nodes exist after extraction. Log warning if any code path attempts mechanical invocation from question pipeline.

### Phase 1 verify

- Fresh slug question build produces Q-L0 nodes with question-shaped extraction
- Existing slugs with mechanical L0 still work (evidence loop uses all L0)
- Cross-slug builds unaffected
- Delta decomposition still works
- `run_build()` never called from question pipeline

---

## Phase 2: New executor primitives

### 2.1 `instruction_from` (dynamic instruction from prior step output)

**`chain_engine.rs`** — Add `instruction_from: Option<String>` to `ChainStep`. Serde default None.

**`chain_executor.rs`** — In the main step loop, BEFORE `build_system_prompt()` is called, resolve `instruction_from` if present. Access `ctx.step_outputs` via `ctx.resolve_ref()`. If resolved to a string, override the step's `instruction` field with the resolved value. Falls through to existing instruction/instruction_map if not set or resolution fails.

**Precedence:** `instruction_from` > `instruction_map` > `instruction`. If `instruction_from` resolves, the others are not consulted.

This is needed for Phase 3 YAML where `extraction_schema` generates a prompt and `l0_extract` uses it.

### 2.2 `recursive_decompose` primitive — FIRST-CLASS EXECUTOR PRIMITIVE

**`chain_engine.rs`** — Add `"recursive_decompose"` to VALID_PRIMITIVES.

**`chain_executor.rs`** — Add dispatch branch. Decomposition is NOT a single LLM call — it's recursive (multiple LLM calls building a tree, with horizontal review between levels). The `extract` primitive does "call LLM once, parse result" and cannot express this.

For fresh decomposition: routes to `question_decomposition::decompose_question_incremental()`. For delta decomposition: routes to `question_decomposition::decompose_question_delta()`. The `when` condition determines which path, and the `input` block provides existing state for delta.

Receives `&PyramidState` for: writer lock (incremental tree persistence), LLM config, operational config (tier1/tier2 for granularity ranges), chains_dir (prompt loading). Reads `apex_question`, `granularity`, `max_depth` from `ctx.initial_params`.

Returns the `QuestionTree` as JSON step output, accessible as `$decompose` or `$decompose_delta` by downstream steps.

### 2.3 `evidence_loop` primitive — FIRST-CLASS EXECUTOR PRIMITIVE

**`chain_engine.rs`** — Add `"evidence_loop"` to VALID_PRIMITIVES.

**`chain_executor.rs`** — Add dispatch branch at line ~3810 (before the final `else`):
```rust
else if step.primitive.as_deref() == Some("evidence_loop") {
    execute_evidence_loop(state, &step, &mut ctx, slug, cancel, progress_tx, layer_tx).await?
}
```

This is a **first-class async primitive**, NOT mechanical dispatch. It receives `&PyramidState` directly from `execute_chain_from()` (which already has `state: &PyramidState` as its first parameter). This gives it access to: writer lock, reader lock, operational config, chains_dir, LLM config — everything the evidence loop needs.

**`execute_evidence_loop()`** (~400-500 lines). Entry point deserializes typed args from `ctx.step_outputs` via `serde_json::from_value()`:
- `$question_tree` → `QuestionTree`
- `$layer_questions` → `HashMap<u32, Vec<LayerQuestion>>`
- `$synth_prompts` → `SynthesisPrompts`
- `$l0_nodes` → loaded from DB (not from context — always fresh)
- `$reused_ids` → `Vec<String>`
- `$evidence_sets` → loaded from DB

Then calls existing functions: `pre_map_layer()`, `answer_questions()`, `reconcile_layer()`. Handles per-layer iteration, reused question skipping, evidence + gap persistence in transactions, progress reporting, cancellation.

No duplication of evidence_answering.rs functions — the primitive is orchestration only.

### 2.4 `cross_build_input` primitive — FIRST-CLASS EXECUTOR PRIMITIVE

**`chain_engine.rs`** — Add `"cross_build_input"` to VALID_PRIMITIVES.

**`chain_executor.rs`** — Add dispatch branch (same pattern as 2.2):
```rust
else if step.primitive.as_deref() == Some("cross_build_input") {
    execute_cross_build_input(state, &step, &mut ctx, slug).await?
}
```

First-class async primitive (NOT mechanical dispatch). Needs async DB access via `state.reader.lock().await`. Loads:
- `evidence_sets` via `db::get_evidence_sets()`
- `overlay_answers` via `db::get_existing_overlay_answers()`
- `question_tree` via `db::get_question_tree()`
- `unresolved_gaps` via `db::get_unresolved_gaps_for_slug()`
- `l0_count` via `db::count_nodes_at_depth()`
- `l0_summary` via `evidence_answering::build_l0_summary()`
- `has_overlay` via `db::has_existing_question_overlay()`
- `is_cross_slug` + `referenced_slugs` via `db::get_slug_references()`

Returns JSON object stored as step output, accessible as `$load_prior_state.*`.

### 2.5 `process_gaps` primitive — FIRST-CLASS EXECUTOR PRIMITIVE

**`chain_engine.rs`** — Add `"process_gaps"` to VALID_PRIMITIVES.

**`chain_executor.rs`** — Add dispatch branch. First-class async primitive wrapping the gap processing logic from build_runner.rs lines 1616-1846. Needs PyramidState for: writer lock, reader lock, chains_dir, operational config, LLM config, audience.

NOT mechanical dispatch — the current `dispatch_mechanical` contract (sync, StepContext only) cannot provide cancellation tokens, chains_dir, build_id, audience, cross-slug refs, or async DB access.

### 2.6 `when` on all steps — ALREADY IMPLEMENTED (zero work)

`chain_executor.rs:3583-3584` already evaluates `when` on every step in the main loop. Removed from scope.

**Fix needed:** When `evaluate_when()` encounters an unresolved `$ref`, it should return `false` (skip step) + log warning, NOT evaluate the unresolved value. Currently unresolved refs may produce `false` which then matches `"false"` in comparisons like `$x == false`. Fix: add an explicit unresolved-ref check at the top of `evaluate_when()`.

### 2.7 Add "question" to VALID_CONTENT_TYPES

**`chain_engine.rs:338`** — Add `"question"` to the array:
```rust
const VALID_CONTENT_TYPES: &[&str] = &["conversation", "code", "document", "question"];
```

### 2.8 Add "question" to chain_registry routing

**`chain_registry.rs`** — Add mapping in `default_chain_id()`:
```rust
"question" => "question-pipeline"
```

### 2.9 ChainContext initial parameters

**`chain_resolve.rs`** — Add `initial_params: HashMap<String, Value>` to `ChainContext` (after `has_prior_build` at line ~44). In `resolve_ref()`, check `initial_params` as fallback after `step_outputs`.

**`chain_executor.rs`** — Add `initial_context: Option<HashMap<String, Value>>` parameter to `execute_chain_from()`. Populate `ctx.initial_params` before execution. All existing callers pass `None`.

### 2.10 Relax chunk requirement for conditional extraction

**`chain_executor.rs:3467-3469`** — Change the hard abort on zero chunks to a warning:
```rust
if num_chunks == 0 {
    warn!(slug, "No chunks found — steps requiring $chunks will be skipped or fail");
    // Do NOT return Err — allow the chain to proceed
    // Steps with for_each: "$chunks" will get an empty array and produce no nodes
}
```

This is critical for cross-slug builds which have no own-chunks but should still run characterize → decompose → evidence_loop. The `l0_extract` step has `when: "$load_prior_state.l0_count == 0"` — for cross-slug builds, `l0_count > 0` (loaded from referenced slugs), so extraction is skipped and no chunks are needed.

---

## Phase 3: Write question.yaml

**New file: `chains/defaults/question.yaml`**

```yaml
schema_version: 1
id: question-pipeline
name: Question Pipeline
description: "Question-driven knowledge pyramid build. Forkable recipe — Pillar 28."
content_type: question
version: "1.0.0"
author: "wire-node"

defaults:
  model_tier: mid
  temperature: 0.3
  on_error: "retry(2)"

steps:
  - name: load_prior_state
    primitive: cross_build_input
    save_as: step_only

  - name: characterize
    primitive: extract
    instruction: "$prompts/question/characterize.md"
    save_as: step_only

  - name: enhance_question
    primitive: extract
    instruction: "$prompts/question/enhance_question.md"
    input:
      corpus_context: "$load_prior_state.l0_summary"
      characterization: "$characterize"
    save_as: step_only

  - name: decompose
    primitive: recursive_decompose
    instruction: "$prompts/question/decompose.md"
    when: "$load_prior_state.has_overlay == false"
    save_as: step_only

  - name: decompose_delta
    primitive: recursive_decompose
    instruction: "$prompts/question/decompose_delta.md"
    when: "$load_prior_state.has_overlay == true"
    input:
      existing_tree: "$load_prior_state.question_tree"
      existing_answers: "$load_prior_state.overlay_answers"
      evidence_sets: "$load_prior_state.evidence_sets"
      gaps: "$load_prior_state.unresolved_gaps"
    save_as: step_only

  - name: extraction_schema
    primitive: extract
    instruction: "$prompts/question/extraction_schema.md"
    save_as: step_only

  - name: synthesis_prompts
    primitive: extract
    instruction: "$prompts/question/synthesis_prompt.md"
    input:
      question_tree: "$decompose"
      l0_summary: "$load_prior_state.l0_summary"
      extraction_schema: "$extraction_schema"
    save_as: step_only

  - name: l0_extract
    primitive: extract
    instruction_from: "$extraction_schema.extraction_prompt"
    for_each: "$chunks"
    when: "$load_prior_state.l0_count == 0"
    node_id_pattern: "Q-L0-{index:03}"
    depth: 0
    save_as: node

  - name: evidence_loop
    primitive: evidence_loop
    save_as: step_only

  - name: gap_processing
    primitive: process_gaps
    save_as: step_only
```

~50 lines replacing ~1200 lines of Rust recipe. Forkable, improvable, publishable as a Wire contribution.

---

## Phase 4: Wire it in

**`build_runner.rs`** — Replace `run_decomposed_build()` body (~1200 lines) with ~100-150 lines:
1. Load `chains/defaults/question.yaml` via `chain_loader::load_chain()`
2. Build initial_context HashMap: apex_question, granularity, max_depth, from_depth, content_type, audience
3. Handle cross-slug reference resolution (load referenced slugs, set is_cross_slug flag — goes into initial_context)
4. Register build start via `local_store::save_build_start()`
5. Call `execute_chain_from(state, &chain, slug, from_depth, None, None, cancel, progress_tx, layer_tx)`
6. Handle build finalization (complete/fail), slug stats update
7. Return result

The function signature remains identical for backward compatibility with callers.

---

## Files Modified

| Phase | Files | Scope |
|-------|-------|-------|
| 1 (Quarantine) | build_runner.rs | Reorder + replace run_build with chain executor |
| 2 (Primitives) | chain_engine.rs, chain_executor.rs, chain_resolve.rs, chain_registry.rs | 4 first-class primitives (recursive_decompose, evidence_loop, cross_build_input, process_gaps) + instruction_from + content_type + initial_params + chunk relaxation + when fix |
| 3 (YAML) | chains/defaults/question.yaml | New chain definition |
| 4 (Wire in) | build_runner.rs | Replace 1200-line body with ~100-150-line chain loader |

## Sequencing

Phase 1 → Phase 2 → Phase 3 → Phase 4. Each independently testable. Phase 1 ships as a standalone fix (fresh slugs work). Phases 2-4 are the architectural convergence.

---

## Audit Trail

**Informed audit (2 auditors):** 3 critical, 5 major found. All resolved:
- enhance_question L0 dependency → characterization fallback added to Phase 1
- $prior_state → $load_prior_state throughout YAML
- content_type "question" not in VALID_CONTENT_TYPES → added to Phase 2.6
- `when` already generalized → removed from scope
- instruction resolution accepts raw text → simplifies Phase 1
- Remaining items (estimates, input wiring, process_gaps callout) → corrected

**Discovery audit (2 auditors):** 7 critical, 12 major found. All resolved:
- Cross-slug zero-chunks abort → chunk requirement relaxed (Phase 2.9)
- Synthesis prompt step missing → added to YAML
- YAML missing required fields → added (description, author, defaults)
- evidence_loop/process_gaps/cross_build_input → first-class async primitives (not mechanical dispatch)
- JSON vs typed boundary → serde_json::from_value at primitive entry
- chain_registry routing → added (Phase 2.7)
- when failure defaults → unresolved refs return false + warn (Phase 2.5 fix)
