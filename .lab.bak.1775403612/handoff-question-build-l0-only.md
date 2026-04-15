# Handoff: Question Pipeline Owns Its Own L0 Extraction

## The fundamental issue

`run_decomposed_build()` delegates L0 extraction to the mechanical pipeline by calling `run_build()`. This is architecturally wrong. The mechanical pipeline is a preset question — it should not be a dependency of the question pipeline. The question pipeline should be self-sufficient: chunks in, pyramid out. No fallback, no delegation, no separate pipeline involved.

The understanding web architecture doc is explicit: "There is no separate 'mechanical pipeline' and 'question pipeline.' There is one system: questions drive everything." The current code contradicts this by making the question pipeline dependent on the mechanical pipeline for its foundation.

## What happens today

```
run_decomposed_build()
  → "No L0 nodes? Call run_build() to create them"
    → run_build() loads document.yaml (7 steps)
    → Runs L0 extraction ✓
    → Runs L0 webbing (unnecessary)
    → Runs clustering (crashes: "batch_cluster returned 0 threads")
    → Question build never executes
```

## What should happen

```
run_decomposed_build()
  → "No L0 nodes? Extract them."
  → Load chunks from pyramid_chunks
  → For each chunk: call LLM with doc_extract.md prompt, save as C-L0-{index:03}
  → Continue to characterize → enhance → decompose → evidence loop
```

The question pipeline extracts L0 directly. It does not load document.yaml. It does not invoke the chain executor. It does not run clustering, webbing, threading, or any mechanical step. It calls the extraction prompt on each chunk and saves the results as canonical L0 nodes. That's it.

## Why this matters

1. **No crash path**: The mechanical clustering crash blocks all question builds on fresh slugs. This isn't a clustering bug to fix — it's a dependency that shouldn't exist.

2. **No wasted work**: The mechanical pipeline runs webbing, clustering, threading, upper synthesis — all of which the question pipeline replaces. Running them to "get L0" wastes tokens and time.

3. **Self-sufficiency**: The question pipeline must work without the mechanical pipeline existing at all. If someone deletes document.yaml and code.yaml, question builds should still work.

4. **Future: question-shaped L0**: The extraction_schema.rs module already generates question-shaped extraction prompts. When we wire that in, the question pipeline's L0 extraction will be informed by the decomposed questions. That's impossible if L0 extraction is delegated to the mechanical pipeline.

## The extraction step

The L0 extraction the question pipeline needs is simple:
- Input: chunks from `pyramid_chunks` table
- Prompt: `chains/prompts/document/doc_extract.md` (for documents) or `chains/prompts/code/code_extract.md` (for code) — the same prompts the mechanical pipeline uses, loaded from the filesystem
- Model: Mercury 2 (mid tier)
- Concurrency: same as mechanical (12 for docs, 8 for code)
- Output: one `C-L0-{index:03}` node per chunk, saved to `pyramid_nodes`
- Error handling: retry(3), on_parse_error: heal (same as mechanical)
- Content type dispatch: use `content_type` from the slug to pick the right prompt

This is the same extraction the mechanical pipeline does in its `l0_doc_extract` / `l0_code_extract` step — but called directly, not through the chain executor.

## What to remove

The block in `run_decomposed_build()` that says "if no L0 nodes exist, call run_build()" should be replaced with direct L0 extraction. The `run_build()` call (and all its mechanical pipeline machinery) should never be invoked from the question path.
