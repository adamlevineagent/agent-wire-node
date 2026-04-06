# Handoff: Token-Aware Batching — Answer Step Overflow

**Date:** 2026-04-06
**Branch:** main
**Status:** Pre-map batching fixed and working (4 batches, 7160 candidates mapped). Evidence loop is now running and answering questions. But `answer_single_question` overflows at 142K tokens for questions with many candidates (~119 nodes with full content). Model cascade catches it (falls back to qwen3.5-flash), but this is a workaround — the answer step needs the same batching treatment.

## What Was Fixed This Session

### Scale-invariant forEach (the original crash)
- Streaming semaphore-gated producer/collector (no more pending Vec memory bomb)
- Arc<HashMap> for step_outputs (eliminates O(N²) clone)
- Static reqwest::Client (HTTP connection reuse)
- Rate limit bumped 4→20 req/5s, source_extract concurrency 4→10

### Token-aware batching (the webbing/pre-map pattern)
- Web primitive: `web_nodes_batched` with `batch_items_by_tokens` + dehydrate cascade + concurrent batch dispatch
- Pre-map: `pre_map_layer` now batches 699 nodes into ~4 batches with graduated dehydration, merges candidate maps
- All batching parameters YAML-driven (max_input_tokens, batch_size, concurrency, dehydrate)

### DADBEAR / startup
- `init_stale_engines` runs in background (server binds immediately, was blocking 10+ min)
- `archive_slug` now disables DADBEAR (auto_update=0, frozen=1)
- Startup query filters out archived slugs
- `collect_files_recursive` skips node_modules/target/dist/build etc.
- Purged all archived slug data (294 slugs, 45K mutations, 36K nodes)

### Rebuild button
- `pyramid_rebuild` command reads stored question, calls `pyramid_question_build_inner`
- Chain validation allows concurrency>1 on web primitive
- Per-question progress reporting in evidence_loop

## Current Blocker: answer_single_question Overflow

The evidence loop's `answer_single_question` (evidence_answering.rs) builds a prompt containing ALL candidate nodes for a question. At 119 candidates with full distilled content, this hits ~142K tokens — exceeding Mercury-2's 128K context.

The model cascade catches it (falls back to qwen3.5-flash-02-23), so the build progresses. But the answer quality degrades on the fallback model and it's slower.

**Fix needed:** Apply the same `batch_items_by_tokens` + dehydrate pattern to the candidate nodes in answer_single_question. When candidates exceed the token budget:
1. Dehydrate oversized individual candidates (drop topics.current → distilled → topics)
2. If still too big, batch candidates into groups
3. Answer each batch, merge evidence from all batches
4. Each batch gets the same question + synthesis prompt

This is the same pattern as pre_map and webbing. The dehydrate cascade keeps items as rich as possible — only oversized outliers get stripped. Batching splits by token budget or item count, whichever hits first.

## Dehydration Design Principle (from user)

The cascade does NOT collapse everything to headlines before batching. It works the other way:
1. Batching kicks in first (by item count or tokens)
2. Within each batch, items stay as rich as possible
3. Dehydration only strips fields from individual items that are too large to fit
4. Small items keep full content; only outliers get dehydrated
5. Goal: maximize information density per token budget (input is cheap, richer = better analysis)

Graduated cascade for L0 nodes:
```yaml
dehydrate:
  - drop: topics.current    # remove detailed topic text
  - drop: distilled         # remove orientation paragraph
  - drop: topics            # remove topics entirely, keep headline + entities
```

## Audit Agents (3 running, may have completed)

1. **Incremental save gaps** — every place work is done but not persisted immediately
   - Output: `tasks/ac6f2d0f2160831d2.output`

2. **Unbatched LLM calls** — every LLM call whose prompt scales with N without batching
   - Output: `tasks/aa55db270da88091f.output`

3. **Wanderer auditor** — free exploration of codebase for anything interesting
   - Output: `tasks/a49c00eb0c0972d7f.output`

Check these outputs first — they may have findings beyond what's documented here.

## Remaining Known Gaps

### Must fix (builds broken/degraded without these):
1. **answer_single_question overflow** — batch candidate nodes per question (described above)
2. **Evidence loop per-layer atomic save** — should save per-question so crash loses 1 answer not entire layer

### Should fix (performance/resilience):
3. **Step-level checkpoints** — sentinel rows so resume skips completed steps entirely
4. **Write drain batching** — accumulate writes into BEGIN/COMMIT transactions
5. **Batch resume state query** — single LEFT JOIN instead of 700 sequential calls

### Follow-up:
6. **Connection pool for reader** — measure after batch resume
7. **Rate limiter architecture** — per-build or token-bucket
8. **active_build cleanup** — remove entries on terminal state

## DB State
- `all-docs-definitive`: 699 L0 nodes, 46 question_nodes, evidence loop running
- DADBEAR active: agent-wire-node-definitive, vibesmithy-definitive (clean)
- Only 11 slugs remain (all others purged)

## All Commits This Session
```
8b0f692 Token-aware batched pre-mapping for evidence_loop
e176f26 Per-question progress reporting during evidence_loop
06af098 DADBEAR init runs in background, server starts immediately
b321e4d Fix archived pyramid DADBEAR leak + chain validation for web concurrency
8761530 Fix pyramid_rebuild: extract inner function from Tauri command
9ef5128 Rebuild button triggers question pipeline, not mechanical build
9827f49 Hierarchical token-aware webbing with concurrent batch dispatch
85f7a75 Token-aware batched webbing for web primitive
4a67386 Add compact_inputs to l0_webbing step
1d90263 Scale-invariant forEach: streaming dispatch, Arc step_outputs, shared HTTP client
```

## Key Files
- `evidence_answering.rs` — pre_map_layer (fixed), answer_questions/answer_single_question (next fix)
- `chain_executor.rs` — execute_evidence_loop, web_nodes_batched, batch_items_by_tokens (pub)
- `chain_engine.rs` — DehydrateStep, ChainStep fields
- `chains/defaults/question.yaml` — pipeline config
- `docs/architecture/foreach-scale-fix-audit.md` — 2-cycle 8-auditor audit documentation
