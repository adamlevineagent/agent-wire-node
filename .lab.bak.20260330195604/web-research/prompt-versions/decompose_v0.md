You are decomposing a question into sub-questions to build a knowledge pyramid.

WHAT YOU ARE DOING:
You are helping build a layered understanding of a topic. The source material is "{{content_type}}" content. The original question will be answered by synthesizing answers to your sub-questions. Your sub-questions will either be answered directly from source material (leaves) or further decomposed by another instance of you (branches).

HOW TO THINK ABOUT IT:
- Ask yourself: "What are the genuinely distinct facets of this question that require separate investigation?"
- Each sub-question should cover territory that NO other sibling covers
- If a question can be answered by reading source files directly, it is a leaf — do not decompose further
- If a question requires combining insights from multiple sources, it is a branch
- Prefer FEWER, more focused questions over many overlapping ones
- It is completely fine to produce just 1 or 2 sub-questions if that is what the question needs
- It is also fine to say this question is already specific enough and return zero sub-questions (empty array)
- The goal is the MINIMUM decomposition needed to fully answer the parent question — no more

WHAT TO AVOID:
- Do NOT pad with extra questions just to fill a quota — there is no quota
- Do NOT create questions that overlap significantly with each other
- Do NOT create questions that rephrase the parent in slightly different words
- Do NOT decompose a question that is already answerable from source material

You are at decomposition depth {{depth}}. Deeper depth means the questions should be MORE specific and MORE likely to be leaves.

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (what to focus on when answering),
  "is_leaf": boolean

Return ONLY the JSON array. An empty array [] is valid if the parent question needs no decomposition.
