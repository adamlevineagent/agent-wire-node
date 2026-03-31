You are decomposing a question into sub-questions to build a knowledge pyramid.

WHAT YOU ARE DOING:
You are helping build a layered understanding of a topic. The source material is "{{content_type}}" content. Your sub-questions will either be answered directly from source material (leaves) or further decomposed (branches).

THE is_leaf DECISION:
This is the single most important decision you make. Getting it wrong collapses the entire pyramid into a flat list.

At depth {{depth}}:
- If depth is 1: set is_leaf to FALSE for ALL sub-questions. No exceptions. Questions about an entire application or system always need further decomposition to produce layered understanding. Setting is_leaf to true at depth 1 produces a single flat answer with no pyramid structure.
- If depth is 2: set is_leaf to FALSE unless the question is truly narrow (about a single feature or component).
- If depth is 3 or higher: set is_leaf to TRUE for most questions, as they should be specific enough to answer from evidence.

ORTHOGONALITY:
The strongest decompositions produce sub-questions where knowing the answer to one tells you NOTHING about the answer to another. Before finalizing, simulate answering each question from the same source files. Would the paragraphs be substantially different?

If two questions both lead to "it has a visual map, an AI helper, and organized collections" — they're the same question. Cut one.

HOW TO DECOMPOSE:
- 2-3 sub-questions is usually right
- Each should guarantee a different answer when confronted with the same evidence
- Prefer questions seeking different TYPES of understanding over different PARTS of the same type

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (what to focus on when answering),
  "is_leaf": boolean

Return ONLY the JSON array.
