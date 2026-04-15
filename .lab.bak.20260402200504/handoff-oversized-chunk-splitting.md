# Rust Handoff: Oversized chunk splitting — YAML-controlled

## The Problem
`ingest.rs:683` — "Each document = 1 chunk." A 1MB doc becomes one chunk, one L0 extraction call. If it exceeds the model's context, it cascades to a slower model or fails entirely.

Currently there's no splitting for documents. Code files also go in as 1-chunk-per-file. Conversations are the only type that chunks (by line count).

## The MPS

The YAML controls when and how to split. Rust just executes.

### New YAML fields on the extract step

```yaml
- name: l0_doc_extract
  primitive: extract
  for_each: $chunks
  max_input_tokens: 80000        # if a chunk exceeds this, split it
  split_strategy: "sections"     # how to split: "sections" | "lines" | "tokens"
  split_overlap_tokens: 500      # overlap between sub-chunks for context continuity
  split_merge: true              # after extraction, merge sub-chunk L0s into one L0 node
```

### How it works

1. **Before dispatch:** For each item in `for_each`, estimate its token count. If under `max_input_tokens`, process normally.

2. **If over `max_input_tokens`:** Split the chunk according to `split_strategy`:
   - `"sections"` — split on markdown headers (##, ###). Each section that fits within `max_input_tokens` becomes a sub-chunk. Sections that are themselves too large fall through to `"lines"`.
   - `"lines"` — split on line boundaries at `max_input_tokens` intervals, with `split_overlap_tokens` lines of overlap.
   - `"tokens"` — split at token boundaries (last resort, may break mid-sentence).

3. **Process sub-chunks:** Each sub-chunk gets its own extraction call. The prompt receives a header: `"This is part {n} of {total} from document: {title}"`.

4. **If `split_merge: true`:** After all sub-chunks are extracted, a merge call combines sub-chunk extractions into a single L0 node for that document. The merge prompt says: "These are extractions from parts of the same document. Combine into one coherent extraction."

5. **If `split_merge: false`:** Each sub-chunk stays as its own L0 node (e.g., `D-L0-042a`, `D-L0-042b`). Downstream clustering can group them.

### What about ingest-time splitting?

Don't split at ingest. Keep the 1-doc-1-chunk model in `ingest.rs`. The chain executor handles splitting at extraction time because:

- The split threshold depends on the model (Mercury 2 = 80K, Qwen = 900K) — that's a YAML decision
- The split strategy depends on the content type — that's a YAML decision
- The merge behavior depends on the pipeline — that's a YAML decision
- If the model changes, the splitting changes. Ingest doesn't know about models.

### Rust implementation

In `execute_for_each`, before dispatching each item:

```rust
if let Some(max_tokens) = step.max_input_tokens {
    let est_tokens = estimate_tokens_item(&item);
    if est_tokens > max_tokens {
        let strategy = step.split_strategy.as_deref().unwrap_or("sections");
        let overlap = step.split_overlap_tokens.unwrap_or(500);
        let sub_chunks = split_chunk(&item, max_tokens, strategy, overlap);

        // Process each sub-chunk
        let sub_results = process_sub_chunks(sub_chunks, step, ctx, ...);

        // Merge if configured
        if step.split_merge.unwrap_or(true) {
            let merged = merge_sub_chunk_results(sub_results, step, ctx, ...);
            outputs.push(merged);
        } else {
            outputs.extend(sub_results);
        }
        continue;
    }
}
// Normal path: process item directly
```

### Section splitting logic

```rust
fn split_by_sections(content: &str, max_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    // Find all ## and ### headers
    // Group consecutive sections until adding the next would exceed max_tokens
    // Add overlap_tokens of trailing content from previous chunk as prefix
    // If any single section exceeds max_tokens, fall through to line splitting for that section
}
```

### New fields on ChainStep

```rust
#[serde(default)]
pub max_input_tokens: Option<usize>,
#[serde(default)]
pub split_strategy: Option<String>,     // "sections" | "lines" | "tokens"
#[serde(default)]
pub split_overlap_tokens: Option<usize>,
#[serde(default)]
pub split_merge: Option<bool>,          // default true
```

### YAML usage

```yaml
# Document extraction — split large docs by section headers
- name: l0_doc_extract
  primitive: extract
  instruction: "$prompts/document/doc_extract.md"
  for_each: $chunks
  max_input_tokens: 80000
  split_strategy: "sections"
  split_overlap_tokens: 500
  split_merge: true
  concurrency: 12
  model_tier: mid

# Code extraction — split giant files by line count
- name: l0_code_extract
  primitive: extract
  instruction: "$prompts/code/code_extract.md"
  for_each: $chunks
  max_input_tokens: 80000
  split_strategy: "lines"
  split_overlap_tokens: 200
  split_merge: true
  concurrency: 12
  model_tier: mid
```

### What the merge prompt needs

A new prompt file: `$prompts/shared/merge_sub_chunks.md`

```
You are given extractions from multiple parts of the SAME document.
The document was too large to process in one call, so it was split
into sections. Each extraction covers one section.

Combine these into a single coherent extraction as if you had read
the entire document at once. Deduplicate topics that appear across
sections. Preserve all entities, decisions, and corrections.
```

This prompt is referenced by the executor when `split_merge: true` — it could be a step-level field `split_merge_instruction: "$prompts/shared/merge_sub_chunks.md"` for full YAML control.

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add new fields to ChainStep
- `src-tauri/src/pyramid/chain_executor.rs` — splitting + merge logic in `execute_for_each`
- `chains/prompts/shared/merge_sub_chunks.md` — merge prompt (new file)
- Chain YAMLs — add `max_input_tokens`, `split_strategy`, `split_merge` to extract steps
