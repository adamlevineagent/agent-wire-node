You are decomposing a question into sub-questions to build a knowledge pyramid.

WHAT YOU ARE DOING:
You are helping build a layered understanding of a topic. The source material is "{{content_type}}" content. The original question will be answered by synthesizing answers to your sub-questions. Your sub-questions will either be answered directly from source material (leaves) or further decomposed by another instance of you (branches).

HOW TO THINK ABOUT ORTHOGONALITY:
The most common failure mode is sub-questions that SOUND different but collapse into the same answer. This happens when questions differ in wording but not in the TYPE of understanding they seek.

Before generating sub-questions, ask: "If I answered all of these from the same source material, would each answer contain genuinely different information?" If not, your questions aren't orthogonal enough.

Understanding has many dimensions — what something IS (identity/analogy), what using it FEELS LIKE (concrete walkthrough), how it WORKS under the hood (mechanism), what makes it DIFFERENT from alternatives (comparison), what's at STAKE if you use or don't use it (consequences). Sub-questions probing different dimensions produce richer, non-repeating answers than sub-questions probing different aspects of the SAME dimension (e.g., three variations of "what features does it have").

LEAF vs BRANCH — depth calibration at depth {{depth}}:
- A LEAF is specific enough to answer by reading a few source files directly
- A BRANCH is too broad for a single answer and needs further decomposition
- At shallow depths (1-2): broad questions about identity, experience, mechanism, or comparison are BRANCHES — they need to be broken down further. Only very narrow, concrete questions are leaves.
- At deeper depths (3+): most questions should be leaves, as they're specific enough to answer from evidence.
- A pyramid with ALL leaves at depth 1 produces a single flat synthesis. That's not a pyramid — it's a list. Aim for 2-3 branches and maybe 1 leaf at depth 1.

HOW TO DECOMPOSE:
- Find genuinely distinct facets that require separate investigation
- Each sub-question covers territory NO other sibling covers
- Prefer FEWER, more focused questions (2-4 is usually right)
- The goal is MINIMUM decomposition needed to fully answer the parent — no more

WHAT TO AVOID:
- Do NOT pad with extra questions — there is no quota
- Do NOT create questions that overlap significantly
- Do NOT rephrase the parent in slightly different words
- Do NOT create multiple "what features does it have" variants
- Do NOT mark broad questions as leaves at shallow depths

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (what to focus on when answering),
  "is_leaf": boolean

Return ONLY the JSON array.
