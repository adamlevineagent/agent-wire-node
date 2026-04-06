# Handoff: Token-Aware Batching Everywhere

**Date:** 2026-04-06
**Branch:** main
**Status:** 699-chunk crash fixed. DADBEAR startup fixed. Rebuild button fixed. But builds produce flat pyramids because pre_map_layer and other evidence_answering calls overflow without batching.

## Current Blocker

`all-docs-definitive` (699 chunks) build "completes" but produces 699 L0 nodes and zero L1+ nodes. The evidence loop's `pre_map_layer` call sends all 699 node summaries in one LLM prompt, exceeds context, times out after 300s × 3 retries. With no candidate mapping, evidence_loop produces zero overlay nodes and reports success.

## The Fix

Apply the token-aware `batch_items_by_tokens` + `dehydrate` pattern uniformly to ALL LLM calls that scale with corpus size N. We already fixed this for the `web` primitive — now it needs to happen everywhere.

**User's stated principle:** "Adopt the automatic token aware dehydration pattern uniformly everywhere."

**User correction on dehydration cascade:** The cascade has TWO intermediate levels between full and headlines-only — not a direct jump from full to headlines. The dehydration should be graduated:
1. Full content (distilled + topics + entities)
2. Drop distilled/orientation (keep topics + entities)
3. Drop topics (keep entities + headline)
4. Headlines only
5. If still too big → batch into groups and process each batch

## Known Gaps (confirmed)

### Unbatched LLM calls that scale with N:
1. **pre_map_layer** (evidence_answering.rs:79) — sends all N node summaries in one call. Has a crude budget guard that truncates (loses coverage) instead of batching.
2. **pre_map_layer_two_stage** (evidence_answering.rs:~325) — stage 2 may also overflow
3. **answer_questions candidate nodes** — each question could get 200+ candidate nodes in its prompt

### Incremental save gaps:
4. **Evidence loop saves per-layer** in atomic BEGIN/COMMIT transaction — should save per-question so crash loses at most 1 answer, not an entire layer
5. **No step-level checkpoints** — restart re-verifies all items instead of skipping completed steps
6. **Build status only updated at end** — not between steps

## Audit Agents (may have completed)
- Incremental save gaps: `tasks/ac6f2d0f2160831d2.output`
- Unbatched LLM calls: `tasks/aa55db270da88091f.output`
Check these for complete findings before starting work.

## Architecture: The Pattern to Apply Everywhere

Every LLM call that receives N-scaled input should:
1. Build full payload
2. Estimate tokens
3. If fits → single dispatch (no change)
4. If doesn't fit → `batch_items_by_tokens(items, max_tokens, batch_size, dehydrate_cascade)`
5. Dehydrate cascade is graduated (4 levels, NOT binary compact/full)
6. All parameters from YAML config, nothing hardcoded
7. Each batch dispatched, results merged
8. Cross-batch merge pass if needed (like webbing)

Working reference implementation: `web_nodes_batched` in chain_executor.rs

## What Was Fixed This Session

| Commit | What |
|--------|------|
| e176f26 | Per-question progress reporting during evidence_loop |
| 06af098 | DADBEAR init runs in background, server starts immediately |
| b321e4d | Fix archived pyramid DADBEAR leak + chain validation for web concurrency |
| 8761530 | Fix pyramid_rebuild: extract inner function from Tauri command |
| 9ef5128 | Rebuild button triggers question pipeline, not mechanical build |
| 9827f49 | Hierarchical token-aware webbing with concurrent batch dispatch |
| 85f7a75 | Token-aware batched webbing for web primitive |
| 4a67386 | Add compact_inputs to l0_webbing step |
| 1d90263 | Scale-invariant forEach: streaming dispatch, Arc step_outputs, shared HTTP client |

## DB State
- `all-docs-definitive`: 699 L0 nodes, 46 question_nodes, 0 overlay/answer nodes
- DADBEAR active on: agent-wire-node-definitive, vibesmithy-definitive (both clean, 0 pending)
- All archived slug data purged, DB vacuumed to 112MB

## Key Files
- `evidence_answering.rs` — pre_map_layer (line 79), answer_questions (line 564)
- `chain_executor.rs` — execute_evidence_loop (~line 4650), web_nodes_batched (working pattern)
- `chains/defaults/question.yaml` — pipeline config
- `docs/architecture/foreach-scale-fix-audit.md` — audit documentation
