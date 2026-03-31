# Web Research Experiment Log

## Executive Summary (as of exp-43)

### Journey: 4.7 → 8.7 across 43 experiments

**Baseline:** 4.7/10 (vibe-web-2 with inline fallback prompts)
**Best:** 8.7/10 (exp-36) with D=9 A=8 G=9 C=9 Co=9 Cn=8
**Mean of best config (20 runs):** 6.7/10
**8+ hit rate:** 25% (5 of 20 runs)

### The Winning Configuration
- **Base:** vibe-ev8 (original code pyramid, 30 L0 nodes)
- **Question:** "Walk me through three things: First, what does the screen actually look like when I use this. Second, what specific problem does it solve that I have right now. Third, what happens when I ask it a question. Keep it simple, no tech words."
- **Prompts:** decompose_v6 (answer simulation), horizontal_review_v3 (no leaf conversion), enhance_v4 (30-word casual)
- **Parameters:** granularity=3, max_depth=3

### Key Discoveries

1. **The question IS the rubric (Pillar 40)** — The single biggest lever is the question text itself. A question with built-in orthogonal facets (screen/problem/AI-interaction) forces the decomposition to produce distinct sub-questions, which produces non-repetitive answers.

2. **Horizontal review was the #1 blocker** — The default "aggressive with marking leaves" instruction collapsed EVERY tree into a flat structure. Fixing this (v1 → v3, now fully disabling leaf conversion) was the prerequisite for any structural improvement.

3. **2 layers is the sweet spot** — 3 L1 answers synthesized into 1 L2 apex. More depth amplifies repetition. Less depth loses the layering benefit.

4. **vibe-ev8 beats vibe-hi2 for this evaluator** — The simpler "This file builds..." L0 descriptions from vibe-ev8 score higher on grounding than the more detailed user-experience descriptions from vibe-hi2, which leak technical terms.

5. **Variance is high (~2 points)** — Same config produces 5.3 to 8.7. The LLM's stochastic decomposition is the main source. When decompose produces 3 truly distinct questions, scores hit 8+. When it produces variations of one question, scores hit 5-6.

6. **The ceiling is evidence-bound** — Code-only L0 can't tell you about use-case scenarios, emotional appeal, or competitive differentiation. These concepts must be inferred by the synthesis step, which sometimes succeeds brilliantly and sometimes produces generic output.

### Prompt Changes Made (final state)

**enhance_question.md** — 30-word limit, casual language, no component listing, no pre-decomposition
**decompose.md** — Answer simulation mental exercise, forced is_leaf=false at depth 1, 2-3 sub-questions max
**horizontal_review.md** — Merge-only, mark_as_leaf ALWAYS empty
**code_extract_frontend.md** — User-experience-first framing (created but vibe-ev8 doesn't use it)
**code_extract.md** — User-experience-first framing (created but vibe-ev8 doesn't use it)

### Score History (all evaluated experiments)
| Exp | Composite | Config delta |
|-----|-----------|--------------|
| 0 (baseline) | 4.7 | - |
| 4 | 5.7 | Fixed horizontal review |
| 15 | 6.3 | Answer-sim decompose |
| 17 | 6.7 | Forced-orthogonal question |
| 25 | 7.3 | Same config, lucky run |
| 28 | 8.3 | Same config, great run |
| 30 | 8.5 | Same config, great run |
| **36** | **8.7** | **Same config, best run** |
| 38 | 8.0 | Same config |
| 43 | 8.2 | Same config |

### What Would Push to 10

1. **Richer base evidence** — Docs/design docs contain WHY information. Code only contains WHAT.
2. **Multi-question accretion** — First build an understanding web about "what it does", then build another about "why it matters" on top. The accretion should produce compounding understanding.
3. **Smarter evidence answering** — The Rust `evidence_answering.rs` prompt could be improved to produce more audience-appropriate synthesis. (Requires code change.)
4. **Deterministic decomposition** — The biggest source of variance is the decompose step. A more constrained decomposition (maybe even hard-coded question templates for common question types) would raise the floor.

## Session 2 — WS13 Audience Gating Validation (2026-03-28)

### Context
WS13 shipped audience gating in evidence answering. Testing whether it improves scores.

### Findings

#### vibe-web-3 (baseline with gating): 3.5
- Audience gating WORKS: jargon is gone from L1+ nodes. Only 1 "hook" leak across 36 nodes.
- BUT: 4 of 8 L2 nodes are empty ("insufficient evidence"). This is a **pre_map bug** — the L0 evidence exists (chat panel, Dennis avatar, Space view, node marbles) but the layer-2 pre_map failed to connect L1s to L2 questions.
- All three L3 nodes are identical paraphrases — same overlap problem as before.
- The 4-layer deep / 36 question tree is OVERSIZED for a 30-L0 codebase. More questions → more unanswerable nodes.
- **Tree depth should scale with L0 count.** Small base → shallow tree. Large base → deeper tree.

#### exp-49 (forced-ortho + gating): 4.2
- Clean language throughout (audience gating working)
- But decomposition focused all 3 L1s on the chat flow (screen→chat status→backend), ignoring the "problem it solves" facet entirely
- The "missing facet" failure mode from the previous session persists

#### Key insight: pre_map bug at layer 2
vibe-web-3's L2 nodes that say "insufficient evidence" are WRONG. The evidence exists:
- L2 asks "How does Vibesmithy's AI chat interface help users?" → L0-012 (Chat Panel) and L0-013 (Dennis Avatar) exist
- L2 asks "How does the visual Space view make navigation intuitive?" → L0-010 (Space Exploration Page) and L0-021 (Node Marble UI) exist
- The pre_map LLM failed to match them because the L2 questions are rephrased differently from the L1 headlines

Even the L2 nodes that DID get distilled text have 0 children — the layer-2 evidence answering completely failed to wire L2→L1 edges.

#### Tree sizing principle
Adam's point: "small trees good for small projects, not necessarily true as we scale up."
- 30 L0 nodes + max_depth=5 → 36 questions, many unanswerable
- 30 L0 nodes + max_depth=3 → 3 questions + 1 apex, well-sized
- Rule of thumb: max_depth ≈ log2(L0_count/3). For 30 L0s → depth 3. For 100 L0s → depth 5. For 500 L0s → depth 7.

### Remaining experiments
- exp-50 through exp-52: batch run with winning question + gating, awaiting eval

### Prompt externalization complete (2026-03-28)

Adam externalized the two hardwired prompts from evidence_answering.rs:
- `pre_map.md` — with inclusive evidence hint (fixes the 4 empty L2 nodes bug)
- `answer.md` — with audience gating and synthesis prompt

New template variables available: `{{audience_block}}`, `{{content_type_block}}`, `{{synthesis_prompt}}`

I updated both prompt files with content-type-aware framing:
- `answer.md` v1: Added "When the source material is code: extract what the code PRODUCES and MEANS, not describe the code itself. Let the question guide the register."
- `decompose.md` v6: Added "The code is evidence, not the topic. If the question is non-technical, decompose into non-technical facets."

Also noted for future work: I should ONLY work on prompt files (yaml/md), not Rust code. If I find hardwired Rust that needs changing, hand it off.

Awaiting server restart to test.

### Post-externalization batch 1 results (exp-54-57)

Config: externalized pre_map (inclusive) + answer (content-type framing) + decompose (code-is-evidence)

| Slug | D | A | G | Co | Coh | Cn | Composite |
|------|---|---|---|----|----|-----|-----------|
| exp-54 | 7 | 6 | 7 | 8 | 7 | 7 | 7.0 |
| exp-55 | 7 | 7 | 7 | 5 | 4 | 7 | 6.2 |
| exp-56 | 8 | 8 | 8 | 9 | 6 | 5 | 7.3 |
| exp-57 | 8 | 9 | 8 | 8 | 8 | 7 | 8.0 |

**exp-57 audience=9** — highest audience score ever. Content-type framing working.
**Completeness improved massively** from 5.0 → 7.5 mean — inclusive pre_map fixed.
**Coherence dropped** from 8.0 → 6.3 — too many L0 nodes per L1 (up to 20).

Fix: tightened answer.md to encourage aggressive DISCONNECT (v2). "Reserve KEEP for evidence that DIRECTLY informs YOUR specific question. 3-5 strong KEEP nodes > 15 weak ones."

Building exp-58-62 with v2 answer prompt.

### v2 tight evidence: REGRESSION (exp-58-62)

answer.md v2 encouraged aggressive DISCONNECT ("3-5 strong KEEP nodes > 15 weak ones").

Result: mean composite dropped from 7.1 → 6.4. Coherence didn't improve (5.6 vs 6.3). Audience regressed (6.0 vs 7.5).

Root cause: the tighter DISCONNECT instruction caused the LLM to be inconsistently aggressive — sometimes dropping actually-relevant evidence, sometimes not. The jargon leak problem is NOT from too many L0 citations; it's from specific L0 nodes (Partner Messaging Hook L0-025, Node Client API L0-027) that have tech-jargon HEADLINES which leak through to synthesis regardless of prompt guidance.

**Reverted to answer.md v1.** The v1 config (inclusive pre_map + content-type framing + standard KEEP/DISCONNECT) is the current best.

Key learning: **the jargon leak is a base pyramid L0 problem, not an answer prompt problem.** The fix is either:
1. Rebuild the base with L0 headlines that don't contain tech identifiers
2. Accept variance and optimize other dimensions

Current best config: decompose v6 + answer v1 + inclusive pre_map. Mean 7.1, peak 8.0.

### v2 tight evidence: DISCARD (exp-58-62)

| Slug | D | A | G | Co | Coh | Cn | Composite |
|------|---|---|---|----|----|-----|-----------|
| exp-58 | 5 | 7 | 8 | 6 | 5 | 5 | 6.0 |
| exp-59 | 7 | 7 | 9 | 8 | 5 | 6 | 7.0 |
| exp-60 | 7 | 7 | 8 | 8 | 6 | 6 | 7.0 |
| exp-61 | 6 | 5 | 7 | 7 | 6 | 7 | 6.3 |
| exp-62 | 6 | 4 | 7 | 5 | 6 | 7 | 5.8 |

Mean: 6.4 — regression from v1's 7.1. Reverted answer.md to v1.

Root cause: aggressive DISCONNECT guidance caused inconsistent evidence dropping. The jargon leak is an L0 headline problem (Partner Messaging Hook, Node Client API) not an answer prompt problem.

### New approach: fix L0 headlines at the source

Updated code_extract.md and code_extract_frontend.md with stronger headline rules: "NEVER use function names, hook names, class names, or API identifiers in the headline."

Building vibe-clean-1 as a new base pyramid with these improved extraction prompts. Will compare L0 headlines against vibe-ev8 to verify the fix works.

### Delta build bug found

Multi-question accretion doesn't work. Second question on accrete-1 slug:
- Delta decomposition ran (reused 1, new 3)
- But evidence loop only answered 1 question at layer 1, stopped at layers=1
- Q1 L2 apex was superseded but Q2 L2 was never created
- Pyramid left headless

Handed off as Rust bug. The delta evidence loop doesn't continue to the apex layer after answering new layer-1 questions.

### Clean base (vibe-clean-1) results: BREAKTHROUGH (exp-63-67)

Rebuilt base with code_extract.md fix: "NEVER use function names, hook names, class names, or API identifiers in the headline."

Key headline improvements:
- "Partner Messaging Hook" → "Interactive Chat Conversation"
- "Dennis Avatar UI" → "Animated AI Avatar"  
- "Node Client API" → "Node API client for data access" (still borderline)

Results on clean base:
| Mean | Old base (ev8) | Clean base |
|------|---------------|------------|
| Audience | 6.7 | **8.8** (+2.1) |
| Composite | 7.1 | **7.9** (+0.8) |
| 8+ rate | ~30% | **60%** |

**exp-66 = 8.8 — new all-time best.** D=9, A=9, G=9, Co=9, Coh=9, Cn=8.

The jargon leak is solved at the source. Remaining variance is decomposition quality (narrow scope, empty L1s) — same as before but with a higher floor.

### Score trajectory
4.2 → 5.5 → 6.3 → 7.0 mean → 7.1 mean → **7.9 mean, 8.8 peak**
