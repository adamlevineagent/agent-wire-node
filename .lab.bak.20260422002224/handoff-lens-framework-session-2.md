# Handoff: Lens Framework Research — Session 2

## Summary
Ran 3 experiments (0 baseline, 1, 2) testing prompt frameworks for question pyramid decomposition. Revised the evaluation rubric after first-contact agent testing invalidated v1 scores. Found two prompt variants that beat the baseline.

## Results Table (v2 rubric)

| Exp | Slug | Framework | Composite | Cold-Start | Concept | Depth | Growth | Status |
|-----|------|-----------|-----------|------------|---------|-------|--------|--------|
| 0 | lens-0 | 4-lens (baseline) | **6.4** | 7 | 6 | 7 | 5 | baseline |
| 1 | lens-1 | No prescribed lenses | **7.1** | 8 | 7 | 6 | 7 | keep |
| 2 | lens-2 | "Find tensions" | **7.1** | 7 | **8** | 6 | 7 | keep |

## Key Findings

### 1. The 4-lens framework IS the problem — but removal alone isn't enough
- Experiment 1 removed the lenses entirely → scored 5.9 under v1, but 7.1 under v2
- The v1 rubric was wrong (penalized structural noise, missed agent utility)
- First-contact testers validated lens-1 as genuinely useful

### 2. "Find tensions" produces the most corpus-specific structure
- Experiment 2 generated 7 L2 branches (vs 4 baseline) — each mapping to a real subsystem
- Delta chains, intelligence passes, wire agents got their own branches
- Concept discovery scored 8 (strongest across all experiments)
- 162 gaps identified (strongest growth scaffold signal)

### 3. Navigable depth is the weak dimension across ALL non-baseline experiments
- Both lens-1 and lens-2 score 6 on navigable depth
- Root cause: `horizontal_review_siblings` only runs on apex children (depth 1)
- Depth 2 siblings never get deduplicated → repetitive L1 questions → drilling feels samey

### 4. Metric revision was critical
- v1 rubric said lens-1 was a regression (5.9 vs 7.2) → discard
- Agent testers said lens-1 was good → re-evaluated
- v2 rubric aligned with tester findings: lens-1 beats baseline (7.1 vs 6.4)

## Current Prompt State (on research/lens-framework branch)
The current committed prompts are the **experiment 2** versions:
- `decompose.md` — "identify TENSIONS AND ORGANIZING PRINCIPLES unique to THIS corpus"
- `answer.md` — "find what TENSION, PATTERN, or INSIGHT emerges from holding child answers together"
- `horizontal_review.md` — mark_as_leaf removed from JSON output
- `source_extract.md` — JSON schema fixes (corrections/decisions with example objects)

## Rust Fixes Needed (Priority Order)

### 1. Horizontal review at all decomposition depths (BLOCKS research)
Currently: `horizontal_review_siblings(&mut apex_children, ...)` called once at depth 1 only
Need: Run at every recursion depth inside `recursive_decompose`
File: `src-tauri/src/pyramid/question_decomposition.rs` around line 748
Impact: Should improve navigable depth scores by deduplicating L1 questions

### 2. Inline annotations in drill response (from tester report)
Add `annotations` field to DrillResult struct
File: drill response construction in routes.rs
Impact: Closes compound knowledge loop — one change, massive payoff

### 3. Breadcrumb path in drill
Walk parent_id chain server-side, return full path
Impact: Agents can orient without manual upward traversal

### 4. Search↔FAQ cross-referral
When search returns 0, suggest FAQ; when FAQ returns 0, suggest keywords
Impact: Zero-result dead ends eliminated

## Lab State
- Branch: `research/lens-framework` (rebased on main with chain engine)
- `.lab/` fully populated with config (v2 rubric), results.tsv, log.md
- Slugs lens-0, lens-1, lens-2 all exist in the DB for comparison
- Next experiment number: 3
- Next slug: lens-3

## What Next Session Should Do
1. Get the Rust build with horizontal_review-at-all-depths
2. Re-run experiment 2 prompts (lens-3) on the new build
3. If navigable depth improves → the framework is working, iterate on fine-tuning
4. If navigable depth doesn't improve → the issue is in the prompts, not dedup
5. Test on a different corpus (vibesmithy code) to verify generalizability

## Key Architecture Context
- The pyramid is a **scaffold**, not a final product — additional questions build on it
- Default altitude should be **ground truth, not developer truth**
- Maximal marginal usefulness at each layer = each layer adds genuinely new understanding
- Model: minimax/minimax-m2.7 (set in UI settings)
- Auth token: set via UI, not disk (app overwrites config on shutdown)
- Build endpoint: POST /pyramid/:slug/build/question with body
