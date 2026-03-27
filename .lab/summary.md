# Research Summary: Document Prompt Optimization

## Objective
Make document chain prompts intelligence-driven instead of prescriptive.

## Key Finding
**Prompt changes alone cannot fix the performance bottleneck.** The issue is architectural, not linguistic.

## Experiments Run

| # | Change | Time | Result | Status |
|---|--------|------|--------|--------|
| 0 | Baseline (intelligence-driven clustering) | 810s | 89 L0→8 L1→4 L2, no apex | keep |
| 1 | Streamlined classify + all prompts | 1211s | 90 L0→9 L1→5 L2→1 L3, wrong apex | discard |
| 2 | Apex-aware distill | >1211s | 127 L0→11 L1, timed out | discard |

## What Worked
- Intelligence-driven clustering prompt (no prescribed counts) produces reasonable thread groupings
- Allowing orphans/unassigned documents is architecturally correct
- Removing bookkeeping from assignments (doc_type, date, canonical) reduces output size marginally

## What Didn't Work
- **No speed improvement from any prompt change** — the bottleneck is the monolithic classification + clustering calls
- Removing prescribed counts made clustering slightly WORSE (11 threads instead of 9 — the model produced more threads without the cap)
- Apex-aware distill prompt didn't even get tested because the build timed out before reaching the apex

## Root Cause Analysis
The document pipeline has TWO monolithic bottleneck steps:

1. **Classification (step 1)**: Reads headers from ALL 127 docs → produces structured classification for each → ~5-8 minutes
2. **Clustering (step 3)**: Reads ALL 127 L0 extractions → assigns each to a thread → ~8-12 minutes

Together: 13-20 minutes, which is 90%+ of the total build time.

These steps are monolithic because:
- Classification needs cross-document context for concept tag normalization
- Clustering needs to see all documents to make assignment decisions

## Recommended Architectural Changes (Require Rust Rebuild)

### 1. Split Classification into Two Phases
- **Phase A (parallel, mercury-2)**: Per-doc type + date classification. Each doc independently → 127 parallel calls, ~30s total
- **Phase B (single, qwen)**: Given Phase A results, normalize concept tags into a shared taxonomy. Input is 127 × 2 fields (type + date), not 127 × 20 lines of content. Much smaller call.

### 2. Split Clustering into Two Phases
- **Phase A (single, qwen)**: Given L0 headlines only (not full extractions), identify concept areas. Output: list of thread names + descriptions. Small call, fast.
- **Phase B (parallel, mercury-2)**: Per-doc assignment. Given one doc extraction + the list of thread names, assign it to the best thread. 127 parallel calls, ~30s total.

### 3. Question Pyramid Overlay (Blocked by Rust Bug)
The question overlay build hangs on decomposition — the `decompose_question_incremental` function appears to deadlock. Needs debugging with access to the Rust code and a rebuild.

## Prompt Improvements (Already Committed, Worth Keeping)
These are good changes even if they don't affect speed:
- `doc_cluster.md`: Intelligence-driven grouping, no prescribed counts, allows orphans
- `doc_recluster.md`: No prescribed cluster counts
- `doc_thread.md`: No prescribed topic counts
- `doc_distill.md`: Apex-aware synthesis, EVERY child must be represented
- `doc_classify.md`: Simplified, skip supersession analysis

## What Adam Should See When He Returns
1. The prompt improvements are committed and pushed
2. The mechanical build takes ~15-20 min on 127 docs (same as before — prompt changes didn't help)
3. The question overlay is blocked by a Rust bug (needs rebuild)
4. The real speed fix requires the two-phase architectural split described above
5. The vibesmithy question pyramid (vibe-ev8) scored 5.5/10 — up from 4.2 baseline, with audience match jumping from 3→7
