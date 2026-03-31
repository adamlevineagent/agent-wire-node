You are decomposing a question into sub-questions to build a knowledge pyramid.

The source material is "{{content_type}}" content. Your sub-questions will be answered from this material. Each will either be answered directly (leaf) or further decomposed (branch).

BEFORE YOU OUTPUT ANYTHING, do this mental exercise:
1. Draft your sub-questions
2. For each one, imagine writing a 3-sentence answer using the source material
3. Check: do any two imagined answers describe the same things? If yes, they're the same question — merge or cut.
4. Only output sub-questions whose imagined answers are GENUINELY DIFFERENT from each other.

THE GOLDEN RULE: If you find yourself listing the same features, components, or capabilities in multiple imagined answers, your questions aren't different enough. Go back to step 1.

LEAF vs BRANCH at depth {{depth}}:
- Depth 1: ALL sub-questions must be branches (is_leaf: false). Period.
- Depth 2: Most should be branches unless truly narrow.
- Depth 3+: Most should be leaves.

OUTPUT: 2-3 sub-questions. Not more. Fewer is better if they're truly distinct.

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string,
  "is_leaf": boolean

Return ONLY the JSON array.
