# Feature Request: Bounded Builds (stop_after)

## What we need

A `stop_after` parameter on pyramid builds that halts the pipeline after a named step completes. Combined with the existing step-output caching in `pyramid_pipeline_steps`, this enables an incremental layer-by-layer build workflow where each layer is inspected and verified before the next one runs.

## The workflow it enables

```
# 1. Extract only — inspect L0 quality, fix prompt, re-run if needed
POST /pyramid/my-slug/build?stop_after=l0_doc_extract

# 2. Add webbing — extract is reused from step cache, only webbing runs
POST /pyramid/my-slug/build?stop_after=l0_webbing

# 3. Add clustering — extract + webbing reused, clustering runs fresh
POST /pyramid/my-slug/build?stop_after=thread_clustering

# 4. Add thread narratives — everything above reused, narratives run
POST /pyramid/my-slug/build?stop_after=thread_narrative

# 5. Add upper layers — full pipeline
POST /pyramid/my-slug/build
```

Each build reuses completed step outputs from the previous build (already tracked in `pyramid_pipeline_steps` with `build_id`). The operator inspects results at each checkpoint before proceeding.

## What exists today

| Capability | Status |
|---|---|
| `from_depth` parameter (skip extraction below depth N, re-run synthesis above) | Implemented — HTTP only, not CLI |
| Step output caching in `pyramid_pipeline_steps` | Implemented |
| Resume state detection (`step_exists`, `get_step_output`) | Implemented |
| Step output hydration for skipped steps | Implemented in executor |
| Per-step stop (`stop_after`) | **Not implemented** |
| CLI `from_depth` | **Not implemented** (HTTP-only) |

## What `from_depth` cannot do

`from_depth` is depth-based, not step-based. Multiple steps live at the same depth:

- **Depth 0**: `l0_doc_extract`, `l0_webbing` (can't separate these)
- **Depth 1**: `thread_clustering` (no depth, in-memory only), `thread_narrative`, `l1_webbing`
- **Depth 2+**: `upper_layer_synthesis`, `l2_webbing`

The bounded build workflow requires stopping between steps at the same depth (e.g., after extraction but before webbing). `from_depth` can't express this.

## Requirements

### R1: `stop_after` parameter on build endpoint

Accept an optional step name. The executor runs the pipeline in normal order and halts cleanly after the named step completes. All steps up to and including the named step run; all steps after it do not.

**Valid step names** are the `name` fields from the chain YAML. For the document pipeline: `l0_doc_extract`, `l0_webbing`, `thread_clustering`, `thread_narrative`, `l1_webbing`, `upper_layer_synthesis`, `l2_webbing`.

### R2: Step reuse across bounded builds

When a build runs and a prior build already completed a step (same slug, same step name, matching chunk indices), the executor should reuse that step's output instead of re-running the LLM call. This is the existing resume/hydration logic — the requirement is that it works correctly across sequential bounded builds on the same slug.

**Important**: Step reuse should be opt-in or at least overridable. If the operator changed the prompt between runs, they need to force re-extraction. A `force_from` parameter (step name) that invalidates that step and all downstream cached outputs would handle this.

### R3: Build status reports which steps ran vs. reused vs. skipped

The build status response should distinguish:
- **Ran**: Step executed fresh (LLM called)
- **Reused**: Step output loaded from prior build cache
- **Stopped**: Step was not reached due to `stop_after`

This is essential for the inspection workflow — the operator needs to know what actually happened.

### R4: HTTP and CLI parity

Both the HTTP endpoint and the CLI should accept `stop_after` and `force_from`. Currently `from_depth` is HTTP-only; don't repeat that gap.

### R5: Container step support

`thread_clustering` is a `container` step with two inner sub-steps (`batch_cluster` and `merge_clusters`). `stop_after=thread_clustering` should mean the entire container completes (both sub-steps), not that it stops after the first inner step.

## Non-requirements

- **No changes to chain YAML format** — this is purely executor/API behavior.
- **No changes to prompts** — this is infrastructure, not intelligence.
- **No new database tables** — `pyramid_pipeline_steps` already has everything needed.
- **No step-level granularity inside containers** — stopping between `batch_cluster` and `merge_clusters` is not needed.

## Edge cases to handle

1. **Invalid step name**: Return a clear error listing valid step names for the chain.
2. **`stop_after` + `from_depth` together**: `from_depth` controls where re-extraction starts; `stop_after` controls where the pipeline halts. They compose — `from_depth=1&stop_after=thread_narrative` means "reuse L0, re-run clustering and narratives, stop before webbing."
3. **Prompt changed but cache exists**: `force_from=l0_doc_extract` should invalidate the extract step and everything downstream, then re-run up to `stop_after`.
4. **Build already running**: Same behavior as today — reject with "build in progress."
5. **Single-doc slug**: Should work identically. Clustering with 1 doc produces 1 thread, narrative synthesizes it, upper layers may produce a trivial apex. All steps should still run and produce inspectable output.
