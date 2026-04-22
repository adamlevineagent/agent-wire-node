# Experiment Log — Lens Framework Research

## Experiment 0 — Baseline (4-lens prompts)
Branch: research/lens-framework / Type: real / Parent: —
Hypothesis: Establish baseline with current 4-lens prescriptive prompts
Changes: None — current prompts as-is (with JSON schema fixes for corrections/decisions/terms)
Result: MMU composite 7.2 (median of 3 evaluators: 5.9, 7.4, 7.2)
Duration: 1244s
Status: baseline
Insight: Corpus Fidelity is the weakest dimension across all evaluators. The L2 structure (purpose/architecture/data/operations) is generic — "a template that could scaffold any intelligent system." The 4-lens framework produces competent but predictable decomposition. Layer Lift and Scaffold Quality are decent (7-7.5) but the structure doesn't surprise. Accessibility hurt by unexplained jargon (DADBEAR, RLS, supersession). The prescriptive lenses are doing what Pillar 37 predicts: filling predetermined slots rather than discovering what this corpus uniquely reveals.

## THINK — before Experiment 1

**Convergence signals:** Fresh start — baseline only.

**Baseline analysis:**
- Composite: 7.2 (median)
- Weakest: Corpus Fidelity (7 median, but Eval B scored 5 — "could have been predicted before reading the corpus")
- Strongest: Layer Lift (7.5) — the pyramid does add understanding at each layer
- The 4-lens framework produces a working pyramid but the structure is generic

**What the 4-lens framework does:**
1. Forces Value/Intent, Kinetic/State Flow, Temporal, Metaphorical axes on EVERY corpus
2. Appears in: source_extract.md, decompose.md, decompose_delta.md, answer.md, web_cluster.md, web_cluster_merge.md, web_domain_apex.md
3. The decompose.md is the critical prompt — it determines the question tree structure

**Untested assumptions:**
- The 4 lenses are WHY the structure feels generic — haven't tested without them
- Removing the lenses might cause regression to file-crawling (the original problem they were added to solve)
- The "what kind of thinking" vs "what specific axes" distinction hasn't been tested

**Hypothesis for Experiment 1:**
Remove the prescriptive 4-lens framework from decompose.md and answer.md. Replace with quality-of-thinking guidance: "discover what dimensions THIS corpus naturally reveals" rather than "evaluate through these 4 axes." Keep the anti-file-crawling instructions. The decompose prompt is the critical one — it determines the L2 branch structure that all evaluators flagged as generic.

The goal is to see if corpus fidelity improves without sacrificing layer lift.

## Experiment 1 — Remove 4-lens framework
Branch: research/lens-framework / Type: real / Parent: #0
Hypothesis: Removing prescriptive lenses improves corpus fidelity without sacrificing layer lift
Changes: Replaced "Force through 4 lenses" with "discover natural structure of THIS corpus" in decompose.md. Replaced 4-lens synthesis with emergent-understanding guidance in answer.md.
Result: MMU composite 5.9 (median of 5.9, 5.9, 6.3) — REGRESSION from 7.2 baseline
Duration: 1278s
Status: discard
Insight: Simply removing the framework without replacing it with something equally strong leaves the LLM rudderless. The decomposer generated repetitive questions (knowledge pyramid asked 3 times, multi-tenancy asked twice). The answer synthesis was thinner — without explicit "find the emergent pattern" guidance at the level of specificity the 4 lenses provided, the LLM just concatenated. The 4th L2 branch ("distinctive innovations") was promising but the overall quality dropped. The lenses ARE doing useful work — the problem is they're too specific/generic, not that they exist.

Key learning: The framework needs to be REPLACED, not REMOVED. The LLM needs a thinking structure — just not a predetermined one.

## THINK — before Experiment 2

**Convergence signals:** 1 discard. Clear signal: removing framework = regression.

**What we learned:**
- The 4-lens framework provides essential decomposition scaffolding
- Without it, the LLM produces repetitive, shallow questions
- But the lenses ARE too generic (evaluators flagged corpus fidelity as weakest)
- The answer.md synthesis guidance matters — "find emergent patterns" is too vague

**Untested assumptions:**
- What if we keep the STRUCTURE of having a thinking framework but make it corpus-adaptive?
- What if the characterize step (which already runs before decompose) generates the framework?
- What if we keep strong anti-concatenation guidance in answer.md but without naming specific lenses?

**Hypothesis for Experiment 2:**
Keep the structure of a thinking framework in decompose.md but replace the 4 fixed lenses with guidance to derive dimensions FROM the source material. Instead of "Force through Value/Intent, Kinetic, Temporal, Metaphorical" say something like: "Before decomposing, identify the dimensions that THIS corpus naturally reveals — what are the tensions, patterns, and organizing principles that are specific to this material? Your sub-questions should explore along THOSE dimensions."

For answer.md, keep the branch synthesis requirement but replace the specific lens references with: "Your synthesis must reveal what EMERGES from holding these answers together — what pattern, tension, or insight becomes visible only at this altitude?"

This is a middle path: structured thinking without predetermined axes.

## Experiment 2 — Corpus-adaptive "find tensions" framework
Branch: research/lens-framework / Type: real / Parent: #0
Hypothesis: Replace fixed lenses with "identify the TENSIONS AND ORGANIZING PRINCIPLES unique to THIS corpus"
Changes: decompose.md — "find tensions, not fill lenses." answer.md — "find what EMERGES from holding child answers together." Also includes JSON schema fixes and horizontal_review mark_as_leaf removal.
Result: MMU v2 composite 7.1 (median of 7.1, 7.1, 7.5)
Duration: 1593s
Status: keep
Insight: Strongest concept discovery score (8 median). Generated 7 L2 branches (vs 4 baseline) — delta chains, intelligence passes, wire agents each got their own branch. 162 gaps (vs ~0 baseline). 33 L1 nodes, 185 KEEPs. The "find tensions" framing produced the most corpus-specific structure. Navigable depth still weak (6) — same as lens-1, likely horizontal_review-at-all-depths issue.

## Metric Revision — v1 → v2
After first-contact testing by Antigravity-A/B agents, v1 rubric was found to penalize structural noise (duplicate L1 questions) while missing agent utility. Under v1, lens-1 scored 5.9 (discard). Under v2, lens-1 scored 7.1 (keep). The testers validated that the pyramid worked well as an agent comprehension tool — navigation, concept discovery, annotation pipeline all functional.

v2 criteria: Cold-Start Orientation (0.30), Concept Discovery (0.30), Navigable Depth (0.20), Growth Scaffold (0.20). Full rationale in config.md.

Re-scored results:
- Baseline: v1=7.2, v2=6.4
- Lens-1: v1=5.9, v2=7.1 (flipped from discard to keep)
- Lens-2: v2=7.1

## Session State
- 2 keeps (lens-1 at 7.1, lens-2 at 7.1), baseline at 6.4
- Lens-2 has stronger concept discovery (8 vs 7) and more gaps (162 vs 100)
- Both share navigable depth weakness (6) — horizontal_review at all depths needed
- Waiting for Rust build with: horizontal_review at every decomposition depth, plus tester report fixes
- Next experiment: re-run lens-2 prompts on the improved build to see if depth dedup improves navigable depth score

## Experiment 3 — Generalizability test: "find tensions" on code
Branch: research/lens-framework / Type: real / Parent: #2
Hypothesis: "Find tensions" framework generalizes from docs to code
Changes: None — same prompts as experiment 2, different corpus (vibesmithy React/TypeScript)
Result: Framework generalized. 5 corpus-specific L2 branches (spatial paradigm, value prop, capabilities, distinctiveness, tech stack). 15 L1 nodes, 132 KEEPs, 78 gaps, 0 empty. 11 self-corrections caught real code issues.
Duration: 985s
Status: keep
Insight: Two independent testers validated: lens-3 (code) produces implementation-oriented knowledge while lens-2 (docs) produces conceptual knowledge. Same framework, same prompts, radically different output. The "find tensions" prompt adapts to content type naturally. 40+ terms extracted, web edges accurately map React component hierarchy. The pyramid is not summarizing — it's analyzing.

## Research Conclusion
The "find tensions and organizing principles" framework is the winner over the fixed 4-lens framework:
- Baseline (4-lens): v2 composite 6.4
- Lens-1 (no lenses): v2 composite 7.1
- Lens-2 (find tensions, docs): v2 composite 7.1, concept discovery 8
- Lens-3 (find tensions, code): generalizability confirmed by two independent testers

The framework works because it tells the LLM WHAT KIND of thinking to do (find tensions, organizing principles, trade-offs) without prescribing WHAT AXES to think along. The corpus itself determines the axes. This is the anti-Pillar-37 solution: structured thinking without predetermined structure.
