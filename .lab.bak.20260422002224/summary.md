# Research Summary: Lens Framework

## Objective
Find a generalized prompt framework for question pyramids that maximizes marginal usefulness across any corpus type.

## Result
**"Find tensions and organizing principles" replaces the fixed 4-lens framework.**

The winning prompt tells the LLM WHAT KIND of thinking to do (find tensions, trade-offs, organizing principles) without prescribing WHAT AXES to think along. The corpus determines the axes.

## Experiments

| # | Slug | Framework | v2 Score | Key Finding |
|---|------|-----------|----------|-------------|
| 0 | lens-0 | 4-lens (baseline) | 6.4 | Generic structure, weak growth scaffold |
| 1 | lens-1 | No prescribed lenses | 7.1 | Better cold-start, concept discovery; needs dedup |
| 2 | lens-2 | **Find tensions** (docs) | **7.1** | Best concept discovery (8), 162 gaps, 7 L2 branches |
| 3 | lens-3 | **Find tensions** (code) | — | Generalizability confirmed. Tester-validated. |

## What Changed

### decompose.md (the critical prompt)
**Before:** "Force your sub-questions through these 4 lenses: Value/Intent, Kinetic/State Flow, Temporal, Metaphorical"

**After:** "Before decomposing, identify the TENSIONS AND ORGANIZING PRINCIPLES unique to THIS corpus. Every body of knowledge has its own natural fault lines — the dimensions along which it divides into meaningfully different concerns. Your job is to FIND those dimensions, not impose predetermined ones."

### answer.md (synthesis guidance)
**Before:** "YOU MUST synthesize by evaluating across the 4 Lenses: Value/Intent, Kinetic/State Flow, Temporal Mapping, and Metaphorical Organ"

**After:** "If lower nodes describe A, B, and C, ask: what TENSION, PATTERN, or INSIGHT only becomes visible when you hold A-B-C together? Your synthesis should name the underlying dynamic."

### Also fixed
- `horizontal_review.md` — removed `mark_as_leaf` from JSON output (was fighting decomposer)
- `source_extract.md` + `answer.md` — added JSON schema examples for corrections/decisions/terms (was causing 24/25 parse failures)

## Why It Works
1. The LLM needs a thinking framework — removing it entirely (exp 1 under v1) causes regression
2. But the framework must be ADAPTIVE — fixed lenses produce generic structure regardless of corpus
3. "Find tensions" gives the LLM a METHOD (identify fault lines) without predetermining the RESULT
4. On docs: produced 7 branches (delta chains, intelligence passes, wire agents, staleness, multi-tenancy, subsystem interconnection, purpose)
5. On code: produced 5 branches (spatial paradigm, value prop, capabilities, distinctiveness, tech stack)
6. Neither could have been predicted — the corpus determined them

## Remaining Work
1. **Horizontal review at all depths** — Rust fix needed. Currently only dedup at depth 1, causing repetitive L1 questions
2. **Navigable depth** — scored 6 across both keeps. Likely improves with the horizontal review fix
3. **Test on mixed corpus** (code + docs together)
4. **Propagate to other prompt files** — web_cluster.md, web_domain_apex.md still reference the 4-lens framework. These are less critical (webbing phase, not question decomposition) but should be updated for consistency
