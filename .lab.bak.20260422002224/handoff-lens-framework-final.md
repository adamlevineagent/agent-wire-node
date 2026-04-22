# Handoff: Lens Framework Research — Final

## What We Accomplished

### 1. Found the generalized prompt framework
**"Find tensions and organizing principles" replaces the fixed 4-lens framework.**

The winning prompt tells the LLM WHAT KIND of thinking to do without prescribing WHAT AXES to think along. Tested on:
- Architecture docs (34 files) → 7 corpus-specific L2 branches
- Vibesmithy code (34 files) → 5 corpus-specific L2 branches
- Both validated by independent agent testers

| Experiment | Framework | v2 Score | Nodes | Gaps |
|------------|-----------|----------|-------|------|
| 0 (baseline) | 4-lens | 6.4 | 60 | ? |
| 1 | No lenses | 7.1 | 58 | 100 |
| 2 | **Find tensions** (docs) | **7.1** | 75 | 162 |
| 3 | **Find tensions** (code) | tester-validated | 55 | 78 |

### 2. Fixed critical bugs
- **JSON schema** — answer.md corrections/decisions/terms had no object examples → 24/25 questions failed parsing. Fixed with example objects.
- **horizontal_review mark_as_leaf** — prompt included the field, LLM populated it, Rust honored it, branches collapsed. Removed from prompt output.
- **Auth token** — app overwrites config on shutdown. Must set via UI, not disk.

### 3. Revised evaluation rubric (v1 → v2)
v1 penalized structural noise (duplicate L1 questions) while missing agent utility. After first-contact testing by two agents, revised to measure Cold-Start Orientation, Concept Discovery, Navigable Depth, Growth Scaffold.

### 4. Tested delta pyramid expansion
Second question ("How did ideas develop over time?") on lens-2:
- `source_extract` and `l0_webbing` correctly SKIPPED
- `decompose_delta` ran instead of `decompose`
- 11 new L1 temporal questions answered from existing L0 evidence
- 1 new L2 branch synthesizing architectural evolution
- **BUG: apex superseded but not rebuilt** — pyramid has no live apex after delta build

## Prompt Changes (committed on research/lens-framework)

### decompose.md
**Before:** "Force through 4 lenses: Value/Intent, Kinetic/State Flow, Temporal, Metaphorical"
**After:** "Identify the TENSIONS AND ORGANIZING PRINCIPLES unique to THIS corpus. Every body of knowledge has its own natural fault lines. Your job is to FIND those dimensions, not impose predetermined ones."

### answer.md
**Before:** "Synthesize by evaluating across the 4 Lenses"
**After:** "What TENSION, PATTERN, or INSIGHT only becomes visible when you hold A-B-C together? Name the underlying dynamic."

### horizontal_review.md
Removed `mark_as_leaf` from JSON output template.

### source_extract.md + answer.md
Added JSON schema examples for corrections (`{wrong, right, who}`), decisions (`{decided, why}`), terms (`{term, definition}`).

### Still has 4-lens references (lower priority)
- `web_cluster.md` — references multi-lens framework for domain grouping
- `web_cluster_merge.md` — references preserving multi-lens abstraction
- `web_domain_apex.md` — references multi-lens for domain synthesis
- `source_extract.md` — still has Value/Intent and Kinetic/Ecosystem lens references in extraction guidance
- `decompose_delta.md` — still has 4-lens framework for new sub-questions

These are in the webbing/extraction phases, not the question decomposition. Less critical but should be updated for consistency.

## Rust Bugs / Fixes Needed

### 1. Delta apex not rebuilt (BLOCKER for expansion)
After delta build, old apex is superseded (`superseded_by: qb-xxx`) but no new apex created. The evidence loop's layer iteration may not reach max_layer in delta mode, or the apex synthesis step is skipped.
- Where: `chain_executor.rs` `execute_evidence_loop` around line 4560 (layer iteration loop)
- Evidence: lens-2 after delta build has `L3-429916d3` with `superseded_by: qb-f9ef5565` and no replacement

### 2. Horizontal review at all depths (quality improvement)
`horizontal_review_siblings` only runs on `apex_children` (depth 1). Depth 2+ siblings never get deduplicated → repetitive L1 questions.
- Where: `question_decomposition.rs` line 748
- Fix: Call at every recursion depth, not just top level
- Also update inline fallback prompt (lines 1887-1908) to match the .md version

### 3. Tester report fixes (from Antigravity-A/B first-contact test)
- **F11: Annotations in drill** — add `annotations` field to DrillResult (low effort, high impact)
- **F15: Breadcrumb path** — walk parent_id chain server-side in drill response (low effort)
- **F12: Search↔FAQ cross-referral** — suggest FAQ on 0 search results, suggest keywords on 0 FAQ results
- **F10: Intra-depth search ranking** — BM25/TF-IDF instead of flat depth-based scoring

## Lab State
- Branch: `research/lens-framework`
- `.lab/` has config (v2 rubric), results.tsv, log.md, summary.md
- Slugs in DB: lens-0 (deleted), lens-1 (keep), lens-2 (keep + delta), lens-3 (keep)
- Next experiment: 4, next slug: lens-4
- Prompts synced to runtime

## What's Next
1. Fix delta apex bug → re-run delta expansion to verify
2. Fix horizontal review at all depths → re-run to verify navigable depth improves
3. Update remaining 4-lens prompt files (web_cluster, web_domain_apex, source_extract, decompose_delta)
4. Test on mixed corpus (docs + code together)
5. Consider whether `characterize` step should dynamically generate the framework per-corpus instead of the static prompt text
