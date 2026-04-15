# chain-binding-v2.6 — Conversation-aware question pipeline + token-aware chunker

> **Status:** v2.6 plan, written 2026-04-08 after a full source-read pass on the related code and existing artifacts. Supersedes the design-spec at `chains/questions/conversation-chronological.yaml` (which is in the v3 DSL and not loaded by production).
>
> Lineage: chain-binding-v2.5 shipped + audited. v2.5's "Chronological" wizard option dispatched to the legacy `build_conversation` Rust intrinsic, which is its own pipeline that bypasses the question pipeline entirely. **That was the wrong target.** v2.6 delivers what was actually wanted: a new chain YAML in the legacy ChainStep DSL that runs the question pipeline shape (decompose → evidence → gaps → web → answer-the-apex) but with an L0 extract step expanded into forward + reverse + combine multi-pass over conversation chunks, plus a token-aware chunker so chunks are coherent units of conversation context instead of fragmented mid-content cutoffs.
>
> **Single-session shipping convention:** all phases of this plan ship in one session. No rollback plans, no migration guards.

---

## Section 0 — Verified facts (from source reads, post-v2.5)

Every claim in this plan is grounded in a file path + line range that I read end-to-end. Re-verify by reading the cited sites.

### 0.1 Existing assets

**Pre-drafted prompts that already exist (unwired today):**
- `chains/prompts/conversation-chronological/forward.md` — drafted in commit `a7d8a50`. Conversation-aware, content-neutral ("session, meeting, interview, journal"), tracks decisions/questions/feelings/running_context. **Not currently loaded by anything.**
- `chains/prompts/conversation-chronological/reverse.md` — same vintage, same shape. Tracks turning_points/later_revised/dead_ends/running_context.
- `chains/prompts/conversation-chronological/combine.md` — same vintage. Fuses forward + reverse views into a single L0 record. **Has Pillar 37 violations:** "4-12 word" headline length, "10-20%" target length, "1-3 sentences" running_context length. Must be scrubbed before shipping.

**Pre-existing question-conversation prompt fork:**
- `chains/prompts/question-conversation/` — 18 files, mirrors `chains/prompts/question/` (also 18 files). Forked in commit `e9c9c7f` as part of the chain-binding-v2 era as work toward conversation-aware extraction. Audit each file before reuse — they may be content-tuned variants of the question/ prompts, OR they may be identical, OR they may be stale.

**Existing v3 design-spec:**
- `chains/questions/conversation-chronological.yaml` — 130-line v3 DSL design-spec from the chain-binding-and-triple-pass.md era. Status: `design-spec` per its own header. References primitives that were claimed not to exist at the time but actually DO exist now (`save_as: step_only`, `zip_steps`, sequential accumulators). The shape is correct; it's in the wrong DSL. **Use as a reference for the v2.6 chain YAML structure but rewrite into the legacy ChainStep DSL.**
- `chains/questions/conversation.yaml` and `chains/questions/conversation.questionpipeline-v1.yaml.bak` — siblings from the same era, also v3 DSL, also not loaded by production.
- `chains/defaults/conversationarchived.yaml` — backup of the original conversation-default chain. Mentioned in handoffs as "deprecated."

### 0.2 Canonical question.yaml structure

`chains/defaults/question.yaml` (233 lines) is the legacy-DSL chain that production runs for question pyramids. Verified step layout:

| Step | Primitive | Notes |
|---|---|---|
| `load_prior_state` | `cross_build_input` | save_as: step_only |
| `source_extract` | `extract` | for_each: $chunks, when: l0_count==0, save_as: node, max_input_tokens: 80000, split_strategy/split_overlap_tokens/split_merge already wired |
| `l0_webbing` | `web` | input: $source_extract, save_as: web_edges |
| `refresh_state` | `cross_build_input` | save_as: step_only |
| `enhance_question` | `extract` | save_as: step_only |
| `decompose` | `recursive_decompose` | save_as: step_only |
| `decompose_delta` | `recursive_decompose` | mode: delta, save_as: step_only |
| `extraction_schema` | `extract` | save_as: step_only |
| `evidence_loop` | `evidence_loop` | save_as: step_only |
| `gap_processing` | `process_gaps` | save_as: step_only |
| `l1_webbing` | `web` | depth: 1, save_as: web_edges |
| `l2_webbing` | `web` | depth: 2, save_as: web_edges |

**v2.6 replaces `source_extract` with three steps (forward, reverse, combine) and copies the rest unchanged.**

### 0.3 Chain executor primitives that already work

**`save_as: step_only`** — verified at `chain_executor.rs:1997+` and `execution_plan.rs:367` (`StorageKind::StepOnly`). Used by `question.yaml` for `load_prior_state`, `refresh_state`, `enhance_question`, `decompose`, `decompose_delta`, `extraction_schema`, `evidence_loop`, `gap_processing`. Step output is held in `ctx.step_outputs` for downstream reference but never written as a `pyramid_nodes` row.

**`zip_steps` with `reverse: true`** — verified at `chain_executor.rs:1997-2073`. Per-iteration injection of two prior steps' outputs into the current step's input payload. Code:

```rust
// chain_executor.rs:2046-2061
for entry in &zip_entries {
    let item_output = ctx.step_outputs.get(&entry.step_name)
        .map(|out| {
            if let Some(arr) = out.as_array() {
                let resolved_idx = if entry.reverse {
                    arr.len().saturating_sub(1 + index)
                } else {
                    index
                };
                arr.get(resolved_idx).cloned().unwrap_or(Value::Null)
            } else {
                out.clone()
            }
        })
        .unwrap_or(Value::Null);
```

The `reverse: true` flag inverts the lookup index: `arr[total_len - 1 - index]`. This is exactly the trick the combine step needs to pair up forward[i] (forward iteration) with reverse[N-1-i] (since reverse pass writes its outputs in reverse order).

**`sequential: true` + `accumulate` config** — verified at `chain_executor.rs:5652-5660` (init), `:5739` (resume update), `:5996/6140` (per-iteration update), `:6949-7004` (`update_accumulators` function). Accumulator config shape (per ChainStep):

```yaml
accumulate:
  running_context:
    init: "Beginning of session."
    from: "$item.output.running_context"
    max_chars: 1500
```

`from` reads a path from the per-iteration LLM output (post-prefixed with `$item.output.`). `max_chars` truncates (Phase 0.1 fix: char-aware via char_indices). The accumulator is keyed by name and stored in `ctx.accumulators: HashMap<String, String>`.

Accumulators reach the LLM via the input expression environment at `chain_executor.rs:5776` ("handles $item, $index, $running_context, etc.") and `:9081-9117` (env merger). So a step input like `running_context: "$running_context"` resolves to the current accumulator value, which the prompt template can reference via `{{running_context}}`.

**Sequential vs concurrent dispatch** — `chain_executor.rs:5666-5690`. If `step.sequential: false` AND `step.concurrency > 1`, dispatches to `execute_for_each_concurrent`. Otherwise iterates sequentially via the `for (index, item) in items.iter().enumerate()` loop at `:5692`.

### 0.4 What does NOT work yet

**`dispatch_order`** is a no-op. `chain_executor.rs:5662-5663`:

```rust
if let Some(ref order) = step.dispatch_order {
    warn!("[CHAIN] [{}] dispatch_order '{}' specified but not yet implemented — using insertion order", step.name, order);
}
```

Documented purpose was "largest_first" load balancing. v2.6 needs **reverse iteration** for the reverse pass — that's a different concern. Two options:

- **(a)** Implement `dispatch_order: "reverse"` — overload the existing field.
- **(b)** Add a new sibling field `for_each_reverse: bool` purpose-built for this case.

**v2.6 picks (b).** Cleaner naming, no conflict with the existing "largest_first" semantic, additive change.

### 0.5 Chunker + tokenizer state

**`chunk_transcript`** at `ingest.rs:262-300`. Line-based:
- `chunk_target_lines()` returns `Tier2Config::default().chunk_target_lines` (default: 100)
- soft_threshold = 70% of target (default: 70 lines)
- hard_limit = 130% of target (default: 130 lines)
- Boundary trigger: `is_speaker_boundary(line) && current_count >= soft_threshold` (Phase 0.4 fix: speaker label must be `--- [A-Z]...`)
- If hard_limit hits without a boundary, flushes mid-content (the failure mode that produced fragmented L0s in the v2.5 baseline)

**`is_speaker_boundary`** at `ingest.rs:247-255`:

```rust
fn is_speaker_boundary(line: &str) -> bool {
    if !line.starts_with("--- ") { return false; }
    line.as_bytes().get(4).map(|c| c.is_ascii_uppercase()).unwrap_or(false)
}
```

**`ingest_conversation`** at `ingest.rs:367-395`. Reads .jsonl, parses messages via `parse_conversation_messages` (`:171-238`), joins them into a transcript, calls `chunk_transcript`, persists each chunk via `db::insert_chunk`. The labels emitted are `PLAYFUL` (user) and `CONDUCTOR` (assistant) per `:224-228`.

**`tiktoken-rs`** is in `src-tauri/Cargo.toml` (`tiktoken-rs = "0.6"`). Used at `llm.rs:158-181` via a private async wrapper:

```rust
async fn estimate_tokens_llm(system_prompt: &str, user_prompt: &str) -> usize {
    // ... spawn_blocking ...
    static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
    let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok());
    // ... encode_with_special_tokens ...
}
```

The BPE encoder is a static OnceLock — initialized once, reused. v2.6 needs a **synchronous** public helper for the chunker (which runs on the ingest thread, not the async runtime):

```rust
pub fn count_tokens_sync(text: &str) -> usize {
    use std::sync::OnceLock;
    static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
    let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok());
    match bpe {
        Some(encoder) => encoder.encode_with_special_tokens(text).len(),
        None => text.len() / 4,
    }
}
```

**The recursive-fancy-regex stack overflow concern** at `llm.rs:161` ("tiktoken's fancy-regex engine is recursive and overflows the 2MB async worker thread stack on large inputs (observed at 699+ doc prompts)") **only applies on the async runtime's 2MB stack**. The chunker runs on the ingest thread which is invoked from `spawn_blocking` in routes.rs:2380 (the HTTP ingest path) — that's a blocking thread with an 8MB stack. So the sync helper is safe to call from the chunker context.

### 0.6 Tier2Config shape

`mod.rs:338` has `pub chunk_target_lines: usize` with default `100` at `:383`. v2.6 adds two more fields to Tier2Config:

```rust
pub chunk_target_tokens: usize,    // default: 28000
pub chunk_overlap_tokens: usize,   // default: 6000
```

Together with `chunk_target_lines`, operators can choose between line-based chunking (legacy, code/document) and token-based chunking (new, conversation). The conversation ingest path uses the token-based one.

### 0.7 chain_engine validator content_type check

`chain_engine.rs:356`:

```rust
const VALID_CONTENT_TYPES: &[&str] = &["conversation", "code", "document", "question"];
```

A chain with `content_type: conversation` passes validation. Note: `vine` is NOT in the list. v2.6 declares `content_type: conversation`, not `vine`, because conversation pyramids (not vine bunches) are the target.

### 0.8 Chain assignment dispatch path (post-v2.5)

`build_runner.rs:880-902` (the part of `run_decomposed_build` that loads the chain YAML):

```rust
let resolved_chain_id = {
    let conn = state.reader.lock().await;
    chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str)?
};
if resolved_chain_id == chain_registry::CHRONOLOGICAL_CHAIN_ID {
    return Err(anyhow!(
        "chain '{}' is only supported for Conversation content_type; \
         route through run_build_from's Conversation dispatch",
        resolved_chain_id
    ));
}
let all_chains = chain_loader::discover_chains(&chains_dir)?;
let meta = all_chains.iter().find(|m| m.id == resolved_chain_id).ok_or_else(...)?;
let chain = chain_loader::load_chain(yaml_path, &chains_dir)?;
// ... pass chain to chain_executor ...
```

If the wizard binds a slug to chain id `conversation-chronological` (the new v2.6 chain id, NOT v2.5's `conversation-legacy-chronological`), the resolver returns that id, the defense-in-depth guard against the LEGACY chronological intrinsic does not fire (different id), `discover_chains` finds the new chain in `chains/defaults/conversation-chronological.yaml`, and the chain executor runs it. **The whole v2.6 chain ships entirely as YAML — no new Rust dispatch code.**

`spawn_question_build` (the WS-C wiring fix from v2.5, at `question_build.rs:226+`) ALSO checks `resolved_chain_id == CHRONOLOGICAL_CHAIN_ID` and dispatches to `vine::run_build_pipeline → build_conversation`. **That dispatch stays as-is** — it still works for the legacy intrinsic (used by vine bunches and any explicit `conversation-legacy-chronological` assignment). v2.6's new chain id is a different string, so the legacy dispatch doesn't fire.

### 0.9 What this plan does NOT touch

- v2.5's `CHRONOLOGICAL_CHAIN_ID` constant + `build_conversation` intrinsic + spawn_question_build dispatch — kept as a separate vine-bunch path.
- The `conversation-legacy-chronological` chain id — still resolves to `build_conversation` for any slug bound to it. Legacy compat.
- Code / document chunking (still uses line-based `chunk_transcript` for code? Actually code uses different ingest paths — verify before claiming).
- The recursive-vine-v2 work (still queued, separate prep doc).

---

## Section 1 — Premise (corrected from v2.5)

The wizard's "Chronological" option in v2.5 dispatched to `build_conversation`, the legacy Rust intrinsic. That intrinsic does:

- Forward pass → Reverse pass → Combine → L0 nodes
- L1 positional pairing (NOT topic clustering)
- L2 thread clustering
- L3+ upper-layer pairing until apex

**Zero pieces of the question pipeline are involved.** No question decomposition, no evidence verdicts, no MISSING/KEEP/DISCONNECT, no gap re-examination, no FAQ generation, no question-shape web edges, no abstain rule, no answering the apex question. The "apex" is the top of the L3+ pairing tree — a bottom-up synthesis that doesn't target the user's apex question.

**What the user actually wanted:** the question pipeline architecture (decompose → evidence loop → answer with verdicts → FAQ → web edges → answer the apex), with the L0 extract step replaced by a conversation-aware multi-pass extractor that uses running context across chunks. The triple-pass forward/reverse/combine value is the **L0 extraction quality**, not the pipeline shape.

v2.6 delivers exactly this:

1. New chain YAML at `chains/defaults/conversation-chronological.yaml` (legacy ChainStep DSL, content_type: conversation).
2. Same step layout as `question.yaml`, with one change: `source_extract` is replaced by three new steps (`forward_pass`, `reverse_pass`, `combine_l0`) using the existing zip_steps + accumulate + save_as: step_only primitives plus a new `for_each_reverse: bool` field.
3. A token-aware chunker that produces ~40k-token chunks with 6k overlap on each side (15% / 70% / 15% layout), so each L0 is a coherent context window instead of a hard-cut fragment.
4. Wizard "Chronological" option redirects from the legacy intrinsic id to the new chain id.

---

## Section 2 — Path A: chain-binding-v2.6 chain YAML

### 2.1 New `for_each_reverse` field on ChainStep

**File:** `src-tauri/src/pyramid/chain_engine.rs:122-263` (`ChainStep` struct)

Add field with `#[serde(default)]`:

```rust
/// chain-binding-v2.6: iterate the for_each items in REVERSE order. Used by
/// the reverse pass of conversation-chronological so that running_context
/// accumulates "what comes after" instead of "what came before". Each item's
/// inner `index` field is preserved (not the iteration position).
#[serde(default)]
pub for_each_reverse: bool,
```

Add to the `Default` impl in `chain_engine.rs:265-325`:

```rust
for_each_reverse: false,
```

**File:** `src-tauri/src/pyramid/chain_executor.rs:5640-5666` (the for_each entry point in execute_for_each)

Add the reverse logic right before the sequential vs concurrent split:

```rust
// chain-binding-v2.6: reverse iteration order if requested. Each item's
// `index` field is its position in the ORIGINAL array — preserved through
// the reverse so chunk_index labels remain stable.
let items: Vec<Value> = if step.for_each_reverse {
    info!("[CHAIN] [{}] forEach: reversing iteration order (for_each_reverse: true)", step.name);
    items.into_iter().rev().collect()
} else {
    items
};
```

The existing `dispatch_order` warning at `:5662-5663` stays as-is (different concern, still a no-op for the legitimate `largest_first` use case).

**Note on parallel path:** the concurrent dispatch at `execute_for_each_concurrent` doesn't iterate in any guaranteed order anyway (it uses tokio task spawn). The reverse field is only meaningful with `sequential: true`. The validator should reject `for_each_reverse: true` combined with `concurrency > 1` AND not `sequential: true`. Add to `chain_engine.rs::validate_chain` (around line 487 where the existing `sequential requires for_each` check lives):

```rust
if step.for_each_reverse && !step.sequential {
    errors.push(format!("{}: for_each_reverse requires sequential: true", prefix));
}
```

### 2.2 The chain YAML

**File:** `chains/defaults/conversation-chronological.yaml`

```yaml
schema_version: 1
id: conversation-chronological
name: "Conversation — Chronological L0 + Question Pipeline"
description: "Question pipeline with L0 extraction expanded into forward + reverse + combine multi-pass for chronological conversation sources."
content_type: conversation
version: "1.0.0"
author: "wire-node"

defaults:
  model_tier: synth_heavy
  temperature: 0.3
  on_error: "retry(2)"

steps:
  # ── Phase 0: Load prior state ───────────────────────────────────────
  - name: load_prior_state
    primitive: cross_build_input
    save_as: step_only

  # ── Phase 1a: Forward pass ──────────────────────────────────────────
  # Earliest-to-latest. Maintains a running summary of "what has happened
  # in the session so far" via the accumulator. Each chunk's analysis
  # references the running context to avoid re-explaining established
  # ground. Output is held in step_outputs for the combine step; not
  # persisted as a node.
  - name: forward_pass
    primitive: extract
    instruction: "$prompts/conversation-chronological/forward.md"
    for_each: "$chunks"
    when: "$load_prior_state.l0_count == 0"
    sequential: true
    accumulate:
      running_context:
        init: "Beginning of session."
        from: "$item.output.running_context"
        max_chars: 4000
    input:
      chunk: "$item.content"
      chunk_index: "$item.index"
      total_chunks: "$chunks.length"
      running_context: "$running_context"
    save_as: step_only
    on_error: "retry(3)"
    on_parse_error: "heal"
    heal_instruction: "$prompts/shared/heal_json.md"
    model_tier: extractor

  # ── Phase 1b: Reverse pass ──────────────────────────────────────────
  # Latest-to-earliest. Same shape as forward but iterates in reverse and
  # accumulates "what comes after this chunk in the session". Annotates
  # each chunk with hindsight. Output is held in step_outputs in REVERSE
  # CHRONOLOGICAL ORDER — the combine step reverses the index lookup
  # via zip_steps reverse: true.
  - name: reverse_pass
    primitive: extract
    instruction: "$prompts/conversation-chronological/reverse.md"
    for_each: "$chunks"
    when: "$load_prior_state.l0_count == 0"
    sequential: true
    for_each_reverse: true   # NEW (v2.6)
    accumulate:
      running_context:
        init: "End of session."
        from: "$item.output.running_context"
        max_chars: 4000
    input:
      chunk: "$item.content"
      chunk_index: "$item.index"
      total_chunks: "$chunks.length"
      running_context: "$running_context"
    save_as: step_only
    on_error: "retry(3)"
    on_parse_error: "heal"
    heal_instruction: "$prompts/shared/heal_json.md"
    model_tier: extractor

  # ── Phase 1c: Combine forward + reverse → L0 nodes ──────────────────
  # Per-chunk fusion of the two passes into a single L0 record. zip_steps
  # injects forward_pass[index] and reverse_pass[total - 1 - index] into
  # the input payload, available in the prompt template as
  # {{forward_pass_output}} and {{reverse_pass_output}}. Output is
  # persisted as L0 nodes.
  - name: combine_l0
    primitive: extract
    instruction: "$prompts/conversation-chronological/combine.md"
    for_each: "$chunks"
    when: "$load_prior_state.l0_count == 0"
    concurrency: 10
    node_id_pattern: "C-L0-{index:03}"
    depth: 0
    save_as: node
    input:
      chunk: "$item.content"
      chunk_index: "$item.index"
      total_chunks: "$chunks.length"
      zip_steps:
        - forward_pass
        - step: reverse_pass
          reverse: true
    max_input_tokens: 80000
    split_strategy: "sections"
    split_overlap_tokens: 500
    split_merge: true
    merge_instruction: "$prompts/shared/merge_sub_chunks.md"
    on_error: "retry(3)"
    on_parse_error: "heal"
    heal_instruction: "$prompts/shared/heal_json.md"
    model_tier: extractor

  # ── Phase 2: L0 webbing — copied from question.yaml unchanged ───────
  - name: l0_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    input:
      nodes: "$combine_l0"
    max_input_tokens: 80000
    batch_size: 50
    concurrency: 4
    dehydrate:
      - drop: orientation
      - drop: topics
      - drop: entities
    compact_inputs: true
    response_schema: &web_schema
      type: object
      properties:
        edges:
          type: array
          items:
            type: object
            properties:
              source: { type: string }
              target: { type: string }
              relationship: { type: string }
              shared_resources:
                type: array
                items: { type: string }
              strength: { type: number }
            required: ["source", "target", "relationship", "shared_resources", "strength"]
            additionalProperties: false
      required: ["edges"]
      additionalProperties: false
    depth: 0
    save_as: web_edges
    model_tier: web
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
    max_input_tokens: 80000
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
    max_input_tokens: 80000
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
    max_input_tokens: 80000
    dehydrate:
      - drop: evidence_sets
      - drop: gaps
      - drop: existing_answers
    save_as: step_only

  - name: extraction_schema
    primitive: extract
    instruction: "$prompts/question/extraction_schema.md"
    input:
      question_tree: "$decomposed_tree"
      characterize: "$characterize"
      audience: "$audience"
    max_input_tokens: 80000
    save_as: step_only

  # ── Phase 5: Evidence answering ─────────────────────────────────────
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
    max_input_tokens: 80000
    batch_size: 50
    concurrency: 4
    dehydrate:
      - drop: orientation
      - drop: topics
      - drop: entities
    compact_inputs: true
    response_schema: *web_schema
    depth: 1
    save_as: web_edges
    model_tier: web
    temperature: 0.2
    on_error: "skip"

  - name: l2_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    max_input_tokens: 80000
    batch_size: 50
    concurrency: 4
    dehydrate:
      - drop: orientation
      - drop: topics
      - drop: entities
    compact_inputs: true
    response_schema: *web_schema
    depth: 2
    save_as: web_edges
    model_tier: web
    temperature: 0.2
    on_error: "skip"

post_build: []
```

### 2.3 Prompts

**Reuse the existing `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` drafts.** Verified content above; they're well-written and content-neutral.

**Pillar 37 sweep required before shipping:**

| File | Line | Violation | Replacement |
|---|---|---|---|
| `combine.md` | "headline" field | `"4-12 word recognizable name..."` | `"A vivid recognizable name for this chunk, drawn from its actual content (not a category like 'discussion' or 'planning')"` |
| `forward.md` | "distilled" field | `"Target: 10-20% of input length"` | Remove sizing, replace with `"Dense, faithful record of what happened in this chunk. Preserve every concrete detail."` |
| `forward.md` | "running_context" field | `"1-3 sentences:"` | Remove sentence count, keep the rest |
| `reverse.md` | "running_context" field | `"1-3 sentences:"` | Same |

The forward/reverse/combine prompts already accept the input shape v2.6 sends: `chunk`, `chunk_index`, `total_chunks`, `running_context` (forward and reverse) or `forward_pass_output` + `reverse_pass_output` (combine via zip_steps). Verify each prompt's template renders the right fields by reading them after the Pillar 37 sweep.

### 2.4 Wizard wiring

**File:** `src/components/AddWorkspace.tsx`

Update the `conversationChain` state type and dropdown to use the new chain id:

```typescript
const [conversationChain, setConversationChain] = useState<
    'question-pipeline' | 'conversation-chronological'
>('question-pipeline');
```

```jsx
<select value={conversationChain} onChange={(e) => setConversationChain(e.target.value as any)}>
    <option value="question-pipeline">Question pipeline (default)</option>
    <option value="conversation-chronological">
        Chronological (forward + reverse + combine)
    </option>
</select>
```

The IPC call (`pyramid_assign_chain_to_slug`) is unchanged — it still passes whatever `conversationChain` holds.

**Backend IPC validation** at `main.rs::pyramid_assign_chain_to_slug` already accepts any chain id that `discover_chains` finds in the chains directory. After v2.6's new YAML lands, `conversation-chronological` is a valid chain id and the IPC accepts it without modification.

### 2.5 What happens to v2.5's `conversation-legacy-chronological` chain id

**Stays as-is.** It's still a valid magic chain id that dispatches to `build_conversation` (the legacy intrinsic) via `spawn_question_build`'s WS-C dispatch fix. Vine bunches that go through `vine::run_build_pipeline` continue to use `build_conversation` directly. Anyone who manually assigns `conversation-legacy-chronological` to a slug via the IPC still gets the legacy intrinsic dispatch.

The wizard dropdown no longer offers it. If we want to keep it accessible for testing, add a third dropdown option `"Chronological intrinsic (legacy, no question pipeline)"` — recommend NOT doing this in v2.6 to avoid UX confusion. The legacy id is reachable via direct IPC call if needed.

### 2.6 Resume + idempotency

The new chain steps inherit the existing `pyramid_pipeline_steps` resume mechanism. Each iteration of `forward_pass` writes a row keyed on `(slug, "forward_pass", chunk_index, depth=0, "")` (or whatever the resume helper computes from step.depth and node_id_pattern). Re-runs of the build resume from the latest completed iteration. Same pattern as `question.yaml`'s source_extract step today.

The `combine_l0` step writes L0 nodes to `pyramid_nodes` with the `C-L0-{index:03}` ID pattern. If the build is interrupted mid-combine, resume picks up from the next missing L0 node. Forward and reverse passes are step_only, so their resume state lives in `pyramid_pipeline_steps`.

### 2.7 Path A done criteria

- [ ] `for_each_reverse: bool` field on ChainStep + Default impl + validator check
- [ ] `chain_executor.rs::execute_for_each` reverses the items vector when `for_each_reverse: true`
- [ ] `chains/defaults/conversation-chronological.yaml` exists and validates
- [ ] Pillar 37 sweep done on the 3 conversation-chronological prompts
- [ ] Wizard dropdown updated; new chain id flows through `pyramid_assign_chain_to_slug` → `pyramid_chain_assignments`
- [ ] `cargo check` clean
- [ ] Test build: same `.jsonl` as the question-pipeline run, same apex question, new chain id assigned. L0 nodes have the C-L0-NNN pattern, evidence verdicts appear, the apex answers the apex question.
- [ ] Existing question pyramids unaffected
- [ ] vine bunches still build via the legacy intrinsic path

---

## Section 3 — Path B: Token-aware chunker

### 3.1 Public token counter helper

**File:** `src-tauri/src/pyramid/llm.rs`

Add a synchronous public helper alongside the existing async one:

```rust
/// Synchronous tiktoken token counter for cl100k_base. Safe to call from
/// blocking thread contexts (8MB stack). DO NOT call from async runtime
/// worker threads (2MB stack) — tiktoken's fancy-regex engine is recursive
/// and overflows on large inputs (verified at 699+ doc prompts). Use the
/// async wrapper estimate_tokens_llm if calling from async context.
///
/// Falls back to len/4 estimation if the BPE encoder fails to initialize.
pub fn count_tokens_sync(text: &str) -> usize {
    use std::sync::OnceLock;
    static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
    let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok());
    match bpe {
        Some(encoder) => encoder.encode_with_special_tokens(text).len(),
        None => text.len() / 4,
    }
}
```

The chunker calls this from inside `chunk_transcript_tokens` running on the ingest thread (which is a blocking thread via `spawn_blocking` in routes.rs). Stack-safe.

### 3.2 New Tier2Config fields

**File:** `src-tauri/src/pyramid/mod.rs:338, 383`

Add to `Tier2Config`:

```rust
/// chain-binding-v2.6: target token count for the unique-content portion
/// of each conversation chunk (the 70% middle band of the 15/70/15 layout).
/// Default 28000 = 70% of a 40k chunk window.
#[serde(default = "default_chunk_target_tokens")]
pub chunk_target_tokens: usize,

/// chain-binding-v2.6: token overlap with each adjacent chunk (the 15%
/// bands on either side of the 70% middle band). Default 6000 = 15% of
/// a 40k chunk window.
#[serde(default = "default_chunk_overlap_tokens")]
pub chunk_overlap_tokens: usize,
```

```rust
fn default_chunk_target_tokens() -> usize { 28000 }
fn default_chunk_overlap_tokens() -> usize { 6000 }
```

Add the literals to the `Tier2Config::default()` impl. Existing `chunk_target_lines: 100` stays for code/document chunkers that aren't changing.

### 3.3 New `chunk_transcript_tokens` function

**File:** `src-tauri/src/pyramid/ingest.rs`

Add alongside the existing `chunk_transcript`:

```rust
/// chain-binding-v2.6: token-aware chunker for conversation transcripts.
/// Produces chunks of `target_tokens + 2*overlap_tokens` total size, where:
///   - The middle 70% (target_tokens) is unique forward content
///   - The leading 15% (overlap_tokens) is the trailing tail of the previous chunk
///   - The trailing 15% (overlap_tokens) is the leading head of the next chunk
///
/// First chunk has no leading overlap; last chunk has no trailing overlap.
/// Token boundaries don't align with line boundaries — the chunker walks
/// the encoded token stream and slices on token positions.
fn chunk_transcript_tokens(
    transcript: &str,
    target_tokens: usize,
    overlap_tokens: usize,
) -> Vec<String> {
    use tiktoken_rs::cl100k_base;

    let bpe = match cl100k_base() {
        Ok(b) => b,
        Err(_) => {
            // Fallback to legacy line-based chunking if tokenizer fails
            tracing::warn!("tiktoken cl100k_base init failed; falling back to line chunker");
            return chunk_transcript(transcript);
        }
    };

    let tokens = bpe.encode_with_special_tokens(transcript);
    let total_tokens = tokens.len();

    if total_tokens <= target_tokens {
        // Whole transcript fits in one chunk; no chunking needed.
        return vec![transcript.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start: usize = 0;

    while start < total_tokens {
        // Compute the unique content range [start, mid_end)
        let mid_end = (start + target_tokens).min(total_tokens);

        // Compute the chunk's actual token range with overlap brackets
        let chunk_start = start.saturating_sub(overlap_tokens);
        let chunk_end = (mid_end + overlap_tokens).min(total_tokens);

        let chunk_token_slice = &tokens[chunk_start..chunk_end];
        match bpe.decode(chunk_token_slice.to_vec()) {
            Ok(chunk_text) => chunks.push(chunk_text),
            Err(e) => {
                tracing::warn!("tiktoken decode failed at chunk boundary: {e}; skipping");
            }
        }

        // Advance by target_tokens (the unique-content stride). Overlap is
        // implicit in the next chunk_start computation.
        start += target_tokens;
    }

    chunks
}
```

**Edge cases:**
- **Tokens that span character boundaries.** The cl100k_base BPE encoder is text-in / text-out and the decode round-trips correctly — no character corruption.
- **Single chunk smaller than target_tokens.** Returns one chunk with the whole transcript.
- **Last chunk overlap.** The last chunk has no trailing overlap because chunk_end is capped at total_tokens.
- **First chunk overlap.** First chunk's chunk_start is 0 (saturating_sub clamps), so no leading overlap.

### 3.4 Wire `ingest_conversation` to use the new chunker

**File:** `src-tauri/src/pyramid/ingest.rs:367-395`

Replace the call to `chunk_transcript(&transcript)` with a call to `chunk_transcript_tokens` using the Tier2Config defaults. Similarly for `ingest_continuation` at `:401-444`.

Need access to the Tier2Config — the ingest functions don't currently take a config parameter. Two options:

- **(a)** Add a `tier2: &Tier2Config` parameter to `ingest_conversation` and `ingest_continuation`. Update call sites in `routes.rs:2380+`, `main.rs:3848+`, etc.
- **(b)** Use `Tier2Config::default()` inline like the existing `chunk_target_lines()` helper does. Loses operator override capability but requires zero call-site changes.

**v2.6 picks (b)** for the smallest possible diff. If operators need to tune chunk size later, that's a follow-up.

```rust
fn chunk_target_tokens() -> usize {
    super::Tier2Config::default().chunk_target_tokens
}
fn chunk_overlap_tokens_cfg() -> usize {
    super::Tier2Config::default().chunk_overlap_tokens
}

// In ingest_conversation:
let chunks = chunk_transcript_tokens(
    &transcript,
    chunk_target_tokens(),
    chunk_overlap_tokens_cfg(),
);
```

The legacy `chunk_transcript` line-based function stays in place — code/document chunkers and any callers that haven't been migrated keep using it.

### 3.5 Backward compatibility

Existing slugs that were chunked with the old line-based chunker keep their existing chunks (data preservation — `pyramid_chunks` rows are unchanged). Re-ingest of those slugs (via `pyramid_ingest`) will use the new token-aware chunker and overwrite the old chunks via the existing `clear_chunks + reinsert` flow in `routes.rs:2380+` (already wired with the Phase 3.4 hash-mismatch invalidation from chain-binding-v2.5).

**Slugs that were partially built** with the old chunks have pipeline_steps rows tied to the old chunk indices. After re-ingest with the new chunker, those steps will be invalidated by the hash-mismatch logic and the build resumes from scratch (which is what you want — old chunks are different content).

### 3.6 Path B done criteria

- [ ] `count_tokens_sync` public helper in `llm.rs`
- [ ] `Tier2Config` has `chunk_target_tokens` (28000) + `chunk_overlap_tokens` (6000)
- [ ] `chunk_transcript_tokens` function in `ingest.rs` with the 15/70/15 layout
- [ ] `ingest_conversation` and `ingest_continuation` use the new chunker
- [ ] Legacy `chunk_transcript` stays for code/document
- [ ] Test ingest: a 200KB Claude Code .jsonl produces ~10 chunks of ~40k tokens each, with 6k overlap on each side. Verify by reading chunks 0, 1, 2 and confirming chunk[1]'s first 6k tokens overlap with chunk[0]'s last 6k tokens.

---

## Section 4 — Combined sequencing

```
Path A — chain YAML
   2.1 for_each_reverse field on ChainStep + validator check
   2.2 chain_executor reverse handling
   2.3 chains/defaults/conversation-chronological.yaml
   2.4 Pillar 37 sweep on conversation-chronological prompts
   2.5 Wizard dropdown new option
   │
Path B — token-aware chunker
   3.1 count_tokens_sync helper
   3.2 Tier2Config new fields
   3.3 chunk_transcript_tokens function
   3.4 ingest_conversation wiring
   │
Build + reinstall + test
```

Path A and Path B are independent in the source — A touches chain_engine.rs / chain_executor.rs / a new YAML / wizard / prompts; B touches llm.rs / mod.rs / ingest.rs. Can ship in either order. **Recommend Path A first** so we can test the chain dispatch with the OLD chunker (regression check), then ship Path B and re-test with bigger chunks (quality check).

---

## Section 5 — Risks

1. **`for_each_reverse` interaction with resume.** When the build resumes mid-reverse-pass, the resume helper at `chain_executor.rs:5710-5720` keys on `chunk_index` not iteration position. So resume of chunk 47 of 112 in reverse mode looks up the row for `chunk_index=47`, which is the 65th iteration in reverse order. Verify the resume logic handles this correctly — it should, because `chunk_index` is read from `item.get("index")` not from the iteration counter, and items still carry their original index after `into_iter().rev()`.

2. **`zip_steps reverse: true` index alignment.** The combine step iterates `$chunks` in normal forward order. forward_pass stored its outputs in normal order (`forward[i] = analysis of chunk i`). reverse_pass stored its outputs in reverse order (`reverse[0] = analysis of chunk N-1, reverse[1] = analysis of chunk N-2, ..., reverse[N-1] = analysis of chunk 0`). For combine[i] (working on chunk i), it needs `forward[i]` and the reverse output that corresponds to chunk i, which is at position `N - 1 - i` in the reverse_pass output array. The `reverse: true` flag in zip_steps does exactly this lookup (`arr[total_len - 1 - index]`). **Verify by walking a tiny example mentally:** N=3 chunks. forward = [F0, F1, F2]. reverse_pass iterates [chunk2, chunk1, chunk0] and writes outputs [R2, R1, R0]. combine[0] (chunk 0) wants F0 + R0. zip_steps reverse:true at index=0 looks up `reverse[3-1-0] = reverse[2] = R0`. ✓

3. **`accumulate` running_context truncation.** The forward/reverse pass accumulates `running_context` from each chunk's output. With `max_chars: 4000`, the running context is char-truncated at 4000 chars after each iteration. For long sessions (100+ chunks), the early context gets dropped from the running window. **This is the same trade-off the existing `build_conversation` legacy intrinsic makes.** Acceptable for v2.6.

4. **Token boundaries don't align with line boundaries.** A chunk may start mid-word or mid-line. The LLM handles this fine (it's not pretending to parse the input as code), but it may look ugly when displayed in the drill view. Worth flagging in the prompt: "the chunk may begin or end mid-sentence; that's expected, the surrounding context covers it."

5. **`forward_pass` model tier vs combine_l0 model tier.** Both set to `model_tier: extractor`. If the extractor tier is too small a model, the running context summarization may lose detail. Worth verifying which model the `extractor` tier resolves to in operational config.

6. **Pillar 37 sweep on combine.md MUST happen before shipping.** The drafted prompts have multiple sizing prescriptions. Without the sweep, the chain produces output that violates Adam's writing rules.

7. **Backward-compat with existing slugs.** A slug that was built with `question.yaml` and is now reassigned to `conversation-chronological` will skip the L0 phase due to `when: "$load_prior_state.l0_count == 0"` and try to run `refresh_state` and onward against the old L0 nodes. This may or may not work depending on the L0 node shape. **Recommend: only assign the new chain id at slug creation time, not as a re-assignment on existing slugs.** Add this constraint to the wizard documentation.

8. **The `dispatch_order` warning at chain_executor.rs:5662** still fires for any chain that uses it. v2.6 doesn't use it. The warning is unchanged.

9. **`count_tokens_sync` from async context.** If anyone calls `count_tokens_sync` from an async runtime worker thread (instead of from blocking context), they'll hit the 2MB stack overflow. The doc comment warns about this. v2.6 only calls it from `chunk_transcript_tokens` which runs on `spawn_blocking` thread. Verify no other caller pulls it into async.

10. **Re-ingest of a partially-built slug** after switching to the new chunker. The hash-mismatch invalidation flow from chain-binding-v2.5 Phase 3.4 handles this — old pipeline_steps for chunks whose content_hash changed get invalidated and the build resumes from the new chunks. Worth a manual smoke test.

---

## Section 6 — Audit cycle plan

Same pattern as chain-binding-v2.5:

1. **Stage 1 informed audit** (auditors O, P) on this v2.6 plan with full context. Verify every claim in Section 0 against actual source. Find any claims that don't match.
2. **Stage 2 discovery audit** (auditors Q, R) blind to Stage 1. Verify the plan independently.
3. Apply findings.
4. Implement against the corrected plan.
5. Cargo check at every phase boundary.
6. Manual smoke test.
7. Hand off.

---

## Section 7 — What this plan does NOT do (and tracked elsewhere)

- **recursive-vine-v2 Phase 2 (recursive ask escalation)** — prep doc at `recursive-vine-v2-phase-2-and-4-prep-v2.md`, audited, ready to implement next.
- **recursive-vine-v2 Phase 4-local (cross-operator vines, no payment)** — same prep doc.
- **recursive-vine-v2 Phase 4-paid** — blocked on WS-ONLINE-H landing on the GoodNewsEveryone repo.
- **Wizard "Domain Vine" UI** — backend ready via `pyramid_create_slug` accepting `referenced_slugs`; frontend follow-up.
- **chain-binding-v2.5 Phase 5 documentation tree** at `docs/chain-development/` — deferred from v2.5 ship.
- **The `db.rs:856 updated_at` ALTER latent bug** — pre-existing, worked around by chain-binding-v2.5's migration introspection. Permanent fix is to drop the line or change `DEFAULT (datetime('now'))` to `DEFAULT CURRENT_TIMESTAMP`.
- **The `chunk_transcript` legacy line-based chunker** stays for code/document. Only conversation switches to the token-aware version. Code/document chunking improvements are out of scope.
- **Operator-tunable chunk size** — Tier2Config defaults are the only knob. UI for tuning is a follow-up.
- **Question pyramid building from a non-conversation source** with the new chain — the chain declares `content_type: conversation`, so the validator only allows conversation slugs to use it.

---

## Section 8 — Stage 1 Audit Corrections (AUTHORITATIVE)

Stage 1 informed audits by O and P returned 2 CRITs and 9 HIGHs. Where this section conflicts with anything earlier in the plan, **this section wins**.

### 8.1 — DROP `for_each_reverse` field entirely
`$chunks_reversed` already exists at `chain_resolve.rs:102-106` as a built-in context scalar that returns the chunk stub array reversed with original `index` field preserved. Comments at `chain_executor.rs:252` and `:2009` describe this as the intended pattern.

**Effect:**
- Section 2.1 (add `for_each_reverse: bool` field) — **DELETED**.
- Section 2.2 reverse_pass uses `for_each: "$chunks_reversed"`.
- No Rust executor change for reverse iteration. No new validator rule. No new test.
- Risk #1 (resume + reverse iteration) becomes irrelevant — `$chunks_reversed` preserves `index`, so resume keys remain stable.

### 8.2 — Rewrite all three prompts against actual template variable names
The drafted prompts at `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` do NOT match the template variables the executor injects. **Not** a Pillar 37 polish task — they need a real rewrite.

Facts:
- `zip_steps` at `chain_executor.rs:2066-2070` injects `{step_name}_output` and `{step_name}_output_pretty`. combine.md must reference `{{forward_pass_output_pretty}}` and `{{reverse_pass_output_pretty}}` (current draft uses `forward_view` / `reverse_view`).
- forward.md and reverse.md must declare `{{chunk}}`, `{{chunk_index}}`, `{{total_chunks}}`, `{{running_context}}` placeholders. Current drafts have none.
- combine.md must declare `{{chunk}}`, `{{chunk_index}}`, `{{total_chunks}}`, plus the two zip_steps payload keys.
- **Verify before implementation**: arbitrary `input:` keys on an `extract` primitive's `for_each` step actually flow into the prompt template namespace (not just LLM payload). Existing `source_extract` in `question.yaml` uses no explicit `input:` block. If keys don't propagate to template, change strategy: use a custom intrinsic step OR restructure so chunk content arrives via the default path.

Pillar 37 sweep, **expanded**:
- forward.md:15 — "Target: 10-20% of input length" — REMOVE
- forward.md:19 — "1-3 sentences" — REMOVE
- reverse.md:20 — "1-3 sentences" — REMOVE
- combine.md:20 — "4-12 word recognizable name" (JSON template) — REMOVE
- **combine.md:16** — "a vivid 4-12 word phrase from the actual content" (RULES section, missed by §2.3) — REMOVE
- All three files end with `/no_think` — **verify against the configured extractor model_tier**. If not Qwen, delete.

### 8.3 — `combine_l0` MUST be sequential, OR match by chunk_index
`zip_steps` reads `ctx.current_index` (`chain_executor.rs:2040`). The sequential `for_each` path at `:5774` sets it. **The concurrent path's per-task `current_index` propagation is NOT verified.** If it doesn't propagate, parallel `combine_l0` reads stale `current_index` and zips wrong forward/reverse pairs. Silent failure.

**Pick ONE before implementation:**
- (a) **Set `combine_l0` to `sequential: true`** (or `concurrency: 1`). Recommended — combine is the cheap step.
- (b) Verify `execute_for_each_concurrent` propagates `current_index` per task. Document the line if so.

**Independently:** zip_steps lookup-by-position misaligns under `on_error: skip`. Plan uses `on_error: retry(3)` which aborts on unhealable failures — fine, but **document explicitly**: "If forward_pass or reverse_pass fails any chunk after retries, the entire build aborts. Partial completion is not supported in v2.6." Future enhancement: `zip_steps.match_by: "index"`.

### 8.4 — Verify `$chunks.length` accessor exists
Plan uses `total_chunks: "$chunks.length"` in three places. `chain_resolve.rs` was not confirmed to support `.length` on arrays. **Before implementation, grep `chain_resolve.rs` for length accessor.** If absent: (a) add a `.length` accessor for arrays, (b) drop `total_chunks` from the YAML, or (c) precompute as a separate context scalar (e.g. `$chunks_total`).

### 8.5 — Default `node_id` under reverse iteration uses iteration position
`chain_executor.rs:5704-5708` builds default node_id as `format!("L{depth}-{index:03}")` where `index` is the iteration counter, not `chunk_index`. With `$chunks_reversed`, iteration index 0 corresponds to chunk_index N-1. Resume keys still match (same step replays same direction), but forensic queries joining `forward_pass`/`reverse_pass` rows by node_id silently misalign.

**Fix during implementation**: set `node_id_pattern: "L{depth}-{chunk_index:03}"` explicitly on `forward_pass`, `reverse_pass`, and `combine_l0`. Verify `node_id_pattern` supports `{chunk_index}` interpolation; if not, add it.

### 8.6 — Add `max_input_tokens` + split fields to forward_pass and reverse_pass
Plan currently sets `max_input_tokens: 80000` only on `combine_l0`. If the new chunker's fallback ever returns a single whole-transcript chunk (e.g. on tiktoken init failure), forward_pass and reverse_pass will dispatch a multi-million-token prompt with no split.

**Add to both `forward_pass` and `reverse_pass`:**
```yaml
max_input_tokens: 80000
split_strategy: "sections"
split_overlap_tokens: 500
split_merge: true
```

**And fix the chunker fallback** (Section 3.3): on tiktoken init failure, return `chunk_transcript(transcript)` (the line-based legacy chunker) — never return the whole transcript as one chunk.

### 8.7 — Token chunker pathological tail + speaker boundary snapping
Two related fixes to `chunk_transcript_tokens`:

**(a) Tail merge:** after computing `mid_end`, if `mid_end == total_tokens && (mid_end - start) < target_tokens / 4`, extend the previous chunk's end and break instead of emitting a near-empty trailing chunk.

**(b) Speaker boundary snap:** raw cl100k token slicing splits mid-word and mid-speaker-label. After decoding the token slice, snap chunk_start forward to the next `\n--- [A-Z]` boundary in the decoded text and back-trim chunk_end at the prior boundary (using `is_speaker_boundary` from `ingest.rs:247`). Alternative cleaner approach: pre-tokenize per speaker turn and pack turns into target_tokens budget greedily, mirroring `chunk_transcript`'s line-granular packing.

### 8.8 — Conversation slug must carry `apex_question`; wizard guards by content_type
**(a) `apex_question` on conversation slugs.** The chain copies refresh_state, enhance_question, decompose, etc. from `question.yaml`. These reference `$apex_question` from the slug record. **Verify** `pyramid_create_slug` for conversations stores an apex_question (via the v2.5 wizard's "what is the question" field). If not: (i) add an apex_question field to conversation slug creation, or (ii) wizard gates the chronological dropdown behind "you must enter a question first."

**(b) Wizard guard.** `chain_engine.rs:395-398` validates `def.content_type` against `VALID_CONTENT_TYPES` but does NOT enforce `chain.content_type == slug.content_type` at assignment time. Add BOTH:
- A check inside `pyramid_assign_chain_to_slug` that the chain's `content_type` matches the slug's `content_type`.
- The wizard restricts the "Chronological" dropdown so it only appears for Conversation slugs.

### 8.9 — Chunk-count shrink leaves orphaned `pyramid_pipeline_steps` rows
When a slug is rebuilt with a different `chunk_target_tokens` (30 chunks → 8 chunks), hash-based invalidation only nukes rows where the per-chunk hash changed. Rows for `chunk_index >= new_chunk_count` are orphaned forever.

**Fix:** when ingest produces a smaller chunk count than the previous build, also delete `pyramid_pipeline_steps` rows where `chunk_index >= new_chunk_count`. Add this to the chunk-invalidation helper introduced in v2.5 Phase 3.4 (`invalidate_pipeline_steps_for_changed_chunks` in `db.rs`).

### 8.10 — `count_tokens_sync` must be the chunker's only tokenizer path
Plan §3.1 adds a public `count_tokens_sync` helper but §3.3 instantiates `cl100k_base()` directly. Dead helper. **Refactor**: chunker uses a `static OnceLock<CoreBPE>` (or imports the same OnceLock the helper wraps). Chunker calls the helper for counting AND uses the same BPE handle for `encode`/`decode`. One init point.

### 8.11 — Verify `routes.rs:2380` spawn_blocking claim across all `ingest_conversation` callers
Plan §0.5 cites `routes.rs:2380` for the HTTP ingest path's `spawn_blocking`. `ingest_conversation` is also called from CLI and Tauri command paths. **Before implementation**, grep all callers and confirm each is inside `spawn_blocking` or running on a thread with adequate stack. If any caller invokes it directly on an async worker, the 2MB tiktoken stack overflow risk returns and the chunker must move into `spawn_blocking` itself, OR the BPE handle must be initialized at startup off the hot path.

### 8.12 — Verify `conversationChain` symbol in `AddWorkspace.tsx`
Plan §2.4 says "Update the conversationChain state type." Confirm the symbol exists before diffing. Read `src/components/AddWorkspace.tsx` first, find the actual state variable name introduced in v2.5, adjust accordingly.

### 8.13 — Memory footprint disclosure (informational, no code change)
For a 100-chunk × 40k-token transcript: `step_outputs` for forward_pass + reverse_pass live until `combine_l0` finishes (~2MB JSON). `outputs: Vec<Value>` accumulator (~1MB). `ctx.chunks` hydrated content (~15MB raw text — verify lazy-chunk-loading status; cited only in `docs/plans/lazy-chunk-loading.md` as a plan).

### 8.14 — Section 0 NIT corrections
- `VALID_CONTENT_TYPES` is at `chain_engine.rs:364`, not `:356` as plan claims.
- `save_as: step_only` storage handling is NOT at `chain_executor.rs:1997` (that's zip_steps); StorageKind::StepOnly lives in `execution_plan.rs` around `:367`. Correct line numbers during implementation.

### 8.15 — Stage 2 still owed
Stage 2 discovery audit (Q + R, blind to Stage 1) is the next gate. Stage 1 found 11 substantive issues; Stage 2 will likely find more in spaces Stage 1 didn't probe (concurrent code paths, db migrations, prompt-engine template resolution, AddWorkspace.tsx wiring). Do NOT skip.

---

## Section 9 — Stage 2 Audit Corrections (AUTHORITATIVE, supersedes Section 8 where in conflict)

Stage 2 discovery audits Q and R returned independently of Stage 1. They confirmed many Stage 1 findings AND surfaced new ones. The most important new findings:

### 9.1 — [CRIT, NEW] combine.md output schema does not match question pipeline L0 contract

combine.md emits `{headline, distilled, decisions, questions_raised, feelings_or_reactions, turning_points, dead_ends}`. The downstream question pipeline (`l0_webbing`, `decompose.l0_summary`, `evidence_loop`, `evidence_answering::build_l0_summary`) requires a `topics` array with `headline`, `current`, `summary`, and `entities` per topic. `chain_dispatch.rs::build_node_from_output` (around `:364`) and `parse_topics_with_required_fields` walk this shape. Without `topics`, L0 nodes are topic-less, `l0_summary` produces null, and the entire pipeline degrades.

**Fix:** combine.md must emit the question-pipeline L0 shape. Concretely, look at the existing `chains/prompts/question/source_extract.md` (or whatever the question pipeline's L0 prompt is) and mirror its output JSON schema. The chronological framing (forward/reverse + turning_points + dead_ends) should be folded into the topics' `summary` and `current` fields, not be top-level keys. This is the largest semantic gap in the v2.6 plan and was missed by Section 0 entirely.

**During implementation**: read `chains/prompts/question/source_extract.md` first, copy its JSON output schema verbatim, then add chronological-aware language to the prompt body. The output schema is the contract — do not innovate on it.

### 9.2 — [CRIT, NEW] `when: l0_count == 0` on forward_pass and reverse_pass breaks resume-after-partial-combine

`forward_pass` and `reverse_pass` are `save_as: step_only`, meaning their outputs live only in `ctx.step_outputs` for the duration of the run. They do NOT persist across builds. The plan gates them with `when: $load_prior_state.l0_count == 0`.

**Failure scenario:**
1. Run 1: forward_pass completes, reverse_pass completes, combine_l0 starts and crashes mid-way. l0_count is still 0 (no L0 nodes saved yet).
2. Run 2 (resume): `when` gate evaluates `l0_count == 0` → true → forward_pass and reverse_pass would re-run. **But** Phase 3.4 hash-invalidation hasn't run because chunks are unchanged, so the resume helper sees prior `pyramid_pipeline_steps` rows for forward_pass and skips them. Result: `ctx.step_outputs["forward_pass"]` is **empty** (resume skipping does not rehydrate step_only outputs into ctx). combine_l0 then runs zip_steps lookup against empty arrays → `Value::Null` for both views → garbage L0.

**Pick ONE:**
- (a) **Drop the `when` gate from forward_pass and reverse_pass entirely.** They always re-run unless their pipeline_steps row exists with the same chunk hash. Cost: extra LLM calls on resume even when not strictly needed. Recommended.
- (b) Add a "rehydrate step_only outputs from pipeline_steps row on skip" code path in the executor. More invasive, harder to verify.
- (c) Make forward_pass and reverse_pass `save_as: node` so their outputs persist as L-1 nodes (using a sub-depth namespace). Most architecturally clean but requires more thought about node_id collision.

Recommended: **(a)** for v2.6. Document the cost in Section 5 risks.

**Independently:** the same gate on combine_l0 IS correct (combine writes L0 nodes; if L0 exists, skip combine). Keep it there.

### 9.3 — [HIGH, NEW] `running_context` accumulator is REPLACE-on-update, not append

`chain_executor.rs:6998` does `accumulators.insert(name.clone(), truncated)` — each iteration **overwrites** the prior accumulator value with whatever the LLM emitted in `output.running_context` for THIS chunk. This is not "accumulation" in the additive sense; it's a one-chunk-memory rolling pointer.

The drafted forward.md does NOT instruct the LLM to integrate the prior `running_context` it was given with the current chunk's events. Result: each forward_pass step sees only the immediately prior chunk's running_context, with no rolling memory of earlier chunks.

**Fix during prompt rewrite (combined with 8.2 / 9.1):** forward.md and reverse.md must explicitly say:
> "You will be given a `running_context` summarizing the conversation so far. Rewrite the running_context as a single field that integrates the prior context with what just happened in this chunk. Do not drop earlier context. Trim only what is genuinely superseded."

Truncation behavior at `update_accumulators` (`max_chars: 4000`) still applies — the LLM-rewritten string gets truncated at the head if it exceeds 4000 chars. Document this in Section 5 risks: "running_context can drift if an unhealable LLM output truncates earlier context that the next chunk needed."

### 9.4 — [MED, NEW] combine_l0 max_input_tokens too tight at 80k

combine_l0 receives a 40k-token chunk PLUS forward_pass output for that chunk PLUS reverse_pass output for the corresponding chunk. With JSON envelope overhead, easily 50-60k. With the system prompt + pipeline framing, 80000 is tight enough that any expansion (e.g. richer forward_pass output) will overflow into the splitter, which makes no sense for a combine step.

**Fix:** bump combine_l0 `max_input_tokens` to **120000**. Verify the configured combine model_tier supports 128k context.

### 9.5 — [MED, NEW] Chunker decode error path drops chunks silently

Plan §3.3 lines 736-738: on `bpe.decode` error, log warn and skip the chunk. **For a 100-chunk transcript, losing chunk 47 means forward_pass jumps from chunk 46 to chunk 48 with no notice.** Ordering is broken silently; downstream synthesis is wrong but not flagged.

**Fix:** on ANY decode error in `chunk_transcript_tokens`, fall back to the legacy `chunk_transcript` (line-based) for the entire transcript. Do not partial-recover. Log a warning that conversation chunking degraded to legacy mode.

### 9.6 — [MED, NEW] Single-giant-chunk fast path interacts badly with hash invalidation

Plan §3.3 lines 717-720: if `total_tokens <= target_tokens`, return `vec![transcript.to_string()]`. For a small transcript, that's one giant chunk. **Re-ingestion after appending one new message flips the whole-file content_hash**, and Phase 3.4's `invalidate_pipeline_steps_for_changed_chunks` nukes the entire prior build. With the line-based chunker, earlier chunks were stable across appends.

**Fix:** acknowledge this tradeoff in Section 5 risks. Optional: in the fast path, still split into 2-3 chunks at speaker boundaries to preserve earlier-chunk hash stability across small appends. Not blocking for v2.6 ship.

### 9.7 — [MED, NEW] Tokenizer init happens per-call in chunker

Plan §3.3 calls `cl100k_base()` fresh per chunker invocation. Each init is ~10ms and allocates several MB. `llm.rs:158-181` already uses a `OnceLock` to cache the BPE encoder.

**Fix:** chunker uses a `static OnceLock<CoreBPE>` (or re-uses the one in `llm.rs` via the `count_tokens_sync` helper from §3.1). One init point. This is the same fix as 8.10 but framed around init cost rather than dead helper code.

### 9.8 — [LOW, NEW] Tier2Config new fields are dead weight if chunker hardcodes constants

Plan §3.2 adds `chunk_target_tokens: 28000` and `chunk_overlap_tokens: 6000` to Tier2Config. But §3.3 chunker reads from `Tier2Config::default()` inline, never from a runtime config instance. The config fields are written-only.

**Fix:** either (a) wire the chunker to read from the slug's actual Tier2Config instance (passed through ingest call sites) so future operator-tunable chunk size works, or (b) hardcode the constants in the chunker and remove the Tier2Config fields. Recommended (a) — it's a couple of parameter passes and unblocks the operator-tuning follow-up.

### 9.9 — [LOW, NEW] `split_strategy: "sections"` inappropriate for conversation

Conversation source has no markdown sections. The splitter would fall back to character-window splitting. For forward_pass and reverse_pass (which now have `max_input_tokens` per 8.6), use `split_strategy: "lines"` (which respects newline boundaries — closer to speaker turns) or `"none"` (let an oversize chunk error out, since the chunker should have prevented oversize chunks in the first place).

**Fix:** set `split_strategy: "lines"` on forward_pass and reverse_pass (revising 8.6).

### 9.10 — [LOW, NEW] Wizard dropdown literal value at AddWorkspace.tsx:779

Plan §2.4 only shows the TypeScript type update. The JSX `<option value="conversation-legacy-chronological">` at `src/components/AddWorkspace.tsx:779` ALSO needs updating to `"conversation-chronological"`. Don't miss this in the diff.

### 9.11 — [LOW, NEW] Invariant comment at chain_executor.rs:4072

`chain_executor.rs:4072` asserts "outputs[i] corresponds to chunk index i (guaranteed for forward $chunks iteration)". With $chunks_reversed (8.1), reverse_pass iteration index 0 corresponds to chunk_index N-1. Today this is safe because reverse_pass is `step_only` and the file-hash UpdateFileHash loop only runs on `saves_node=true` steps. But the invariant is load-bearing and a future reverse-iterating step that saves nodes would silently corrupt file→node mapping.

**Fix:** update the invariant comment at `:4072` to say "guaranteed for forward $chunks iteration; for $chunks_reversed iteration, use `output.chunk_index` instead of array position." Optional defensive fix: change the loop to dereference `output.get("chunk_index")` unconditionally — `:5698-5701` already sets it from `item.get("index")`.

### 9.12 — [INFO, NEW] spawn_question_build vs conversation build dispatcher

Q raised concern that `spawn_question_build` is question-content-type-specific, and the new chain has `content_type: conversation`, so it goes through a different dispatcher entirely. **Verify before implementation:** trace the conversation build route. Find where conversation slugs invoke `run_decomposed_build` (vs. the legacy `vine::run_build_pipeline` path). If it's a different entry function, the v2.5 dispatch hijack (`spawn_question_build` checking `CHRONOLOGICAL_CHAIN_ID`) is irrelevant for the new chain id but **the new entry function must also load YAML chains**, not assume hardcoded steps.

If the conversation route currently always dispatches to `vine::run_build_pipeline` (legacy intrinsic) for ALL conversation slugs, then v2.6 needs to add chain-id awareness there: if the assigned chain id is `conversation-chronological`, route through `run_decomposed_build` instead. This is a load-bearing wiring point the plan glosses.

### 9.13 — Stage 2 confirmations of Stage 1 findings

Confirmed by both Q and R independently:
- ✓ `$chunks_reversed` exists; `for_each_reverse` field is unnecessary (Stage 1 §8.1)
- ✓ combine.md uses wrong field names (`forward_view` vs `forward_pass_output`) (Stage 1 §8.2)
- ✓ Pillar 37 sweep missed combine.md:16 RULES section (Stage 1 §8.2)
- ✓ `$chunks.length` accessor doesn't exist; will fail at build time (Stage 1 §8.4)
- ✓ Orphaned pipeline_steps rows on chunk count shrink (Stage 1 §8.9)
- ✓ Tail merge / speaker boundary snap needed in chunker (Stage 1 §8.7)
- ✓ count_tokens_sync helper is dead code as proposed (Stage 1 §8.10)
- ✓ Wizard `conversationChain` symbol verification (Stage 1 §8.12)

Cleared by Stage 2 (no fix needed):
- ✓ Accumulator path resolution (`update_accumulators`) works as plan claims for `$item.output.running_context`
- ✓ zip_steps under for_each concurrency: combine_l0 reads a snapshot of step_outputs taken before the for_each starts; forward_pass/reverse_pass are sequential steps that complete first, so the snapshot is consistent. **But** 8.3 still applies: if you allow `concurrency > 1` on combine_l0, current_index propagation must be verified. Stage 2 found no evidence this is broken in the current code path; Stage 1 found no evidence it works. **Final call: set combine_l0 sequential for safety, document why.**
- ✓ Hash invalidation wiring (Phase 3.4) for re-ingest with same chunker config
- ✓ Reverse iteration + resume semantics (under `$chunks_reversed`)
- ✓ Wizard IPC path through `pyramid_assign_chain_to_slug`
- ✓ Legacy v2.5 chain id co-existence (existing slugs bound to `conversation-legacy-chronological` continue to dispatch to the intrinsic)

### 9.14 — Final implementation gate

After Section 9 corrections are applied to the chain YAML drafts, prompt drafts, and Rust implementation plans, **no further audit cycles are needed**. Implementation can begin. Implementer should treat Sections 8 and 9 as authoritative, read source files to verify claims marked "verify before implementation," and ship.

Implementation order (revised):
1. Read `chains/prompts/question/source_extract.md` to capture the L0 output schema (9.1).
2. Rewrite forward.md / reverse.md / combine.md against actual template variables AND the question-pipeline L0 schema (8.2 + 9.1 + 9.3).
3. Pillar 37 sweep (8.2 expanded).
4. Verify `$chunks.length` and decide: add accessor or precompute or drop (8.4).
5. Verify `extract` primitive `input:` keys reach prompt template namespace (8.2 dependency).
6. Trace conversation slug build dispatcher; confirm it loads YAML chains for non-legacy chain ids (9.12).
7. Verify all `ingest_conversation` callers run inside `spawn_blocking` (8.11).
8. Verify `pyramid_create_slug` for conversations carries `apex_question` (8.8).
9. Add `count_tokens_sync` helper to `llm.rs` (8.10) and use it from the chunker.
10. Add Tier2Config fields wired through ingest call sites (9.8).
11. Implement `chunk_transcript_tokens` with tail merge, speaker boundary snap, OnceLock cache, hard-error fallback (8.6 / 8.7 / 9.5 / 9.7).
12. Wire `chunk_transcript_tokens` into `ingest_conversation`.
13. Write `chains/defaults/conversation-chronological.yaml` with: `for_each: $chunks_reversed` for reverse_pass, `node_id_pattern: "L{depth}-{chunk_index:03}"` on all three new steps, `sequential: true` on combine_l0, `max_input_tokens: 80000` + `split_strategy: "lines"` on forward/reverse, `max_input_tokens: 120000` on combine_l0, NO `when` gate on forward/reverse (drop `l0_count == 0`), `when` gate ON combine_l0 only.
14. Add chunk-count-shrink cleanup to `invalidate_pipeline_steps_for_changed_chunks` (8.9).
15. Add `pyramid_assign_chain_to_slug` content_type check (8.8).
16. Wizard: update `conversationChain` state type AND the JSX `<option value=...>` literal at `AddWorkspace.tsx:779` (8.12 + 9.10).
17. Update invariant comment at `chain_executor.rs:4072` (9.11).
18. Cargo check at every meaningful boundary.
19. Smoke test: create conversation slug with question, pick Chronological, ingest a real multi-conversation transcript, verify forward/reverse/combine run sequentially, verify L0 nodes have `topics` array, verify rest of question pipeline runs to completion, verify chunks are ~40k tokens with overlap.
