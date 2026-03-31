# Web Research Summary

## Objective
Score 10/10 on the understanding web quality rubric for question pyramids built on the vibesmithy codebase.

## Results
- **Starting score:** 4.7/10 (baseline vibe-web-2)
- **Final best score:** 8.7/10 (exp-36)
- **Mean of best config (25 runs):** 7.0/10
- **8+ hit rate:** 36% (9 of 25 runs)
- **Score range:** 5.3 – 8.7
- **Total experiments:** 48 builds, ~30 evaluations

## What Changed (from baseline to 8.7)

### Prompt Changes
1. **enhance_question.md** — 30-word limit, casual language, no component listing, no pre-decomposition
2. **decompose.md** — Answer-simulation mental exercise before generating sub-questions; forced is_leaf=false at depth 1; 2-3 sub-questions max
3. **horizontal_review.md** — Disabled leaf conversion entirely (mark_as_leaf always empty); merge-only mode
4. **code_extract_frontend.md** — User-experience-first framing (for future base rebuilds)
5. **code_extract.md** — User-experience-first framing (for future base rebuilds)

### Question Design
The single biggest improvement came from changing the question text:
- **Before:** "What is this and why do I care?"
- **After:** "Walk me through three things: First, what does the screen actually look like when I use this. Second, what specific problem does it solve that I have right now. Third, what happens when I ask it a question. Keep it simple, no tech words."

The forced-orthogonal structure (screen/problem/AI-interaction) guarantees that the decomposition produces distinct sub-questions, which is the #1 determinant of pyramid quality.

## Key Findings

### What Drives Quality (ranked by impact)
1. **Question structure** — Built-in orthogonal facets → +2.0 points
2. **Horizontal review fix** — Stopped killing branches → +1.0 point (prerequisite for any structure)
3. **Decompose prompt** — Answer simulation → +0.5 points
4. **Base evidence** — vibe-ev8 (simpler) beats vibe-hi2 (detailed but leaky)
5. **Tree depth** — 2 layers (3 L1 + 1 L2) is the sweet spot

### What Doesn't Matter
- Granularity parameter (doesn't control sub-question count)
- Enhance prompt (mostly overridden by question text)
- More tree depth (amplifies repetition)
- Human-interest base (vibe-hi2 leaks jargon, worse audience scores)

### Remaining Bottlenecks
1. **LLM variance** — Same config produces 5.3 to 8.7. Decomposition quality is non-deterministic.
2. **Evidence ceiling** — Code-only L0 can't provide use-case scenarios, emotional hooks, or competitive differentiation. These must be inferred.
3. **Audience match** — L0 descriptions still lean technical, which sometimes leaks through to higher layers.

## Next Steps (not implemented)
1. **Multi-question accretion** — Build understanding webs with multiple questions on the same slug to create compounding understanding
2. **Docs + code combined base** — Use core-selected-docs for WHY information + vibe-ev8 for WHAT information
3. **Deterministic decomposition** — For common question types, use a fixed decomposition template instead of LLM-generated
4. **Evidence answering improvements** — The Rust-side synthesis prompt could be improved for audience match (requires code change)

## Files
- Prompt versions: `.lab/web-research/prompt-versions/`
- Results: `.lab/web-research/results.tsv`
- Detailed log: `.lab/web-research/log.md`
- Best pyramids: exp-36 (8.7), exp-30 (8.5), exp-28 (8.3), exp-38 (8.0), exp-48 (8.3), exp-43 (8.2)
