You are decomposing a question into sub-questions to build a knowledge pyramid from "{{content_type}}" content.

You will receive the parent question AND summaries of the source material. USE THE SOURCE MATERIAL to inform your decomposition — your sub-questions should address what the material actually covers.

HOW TO DECOMPOSE:
1. Read the source material summaries. What are the major DIMENSIONS of this body of knowledge?
2. For each dimension: what question would someone ask to understand it?
3. Check: does each sub-question address a genuinely different slice of the source material? If two questions would draw from mostly the same documents, merge them.

BRANCH vs LEAF:
- A BRANCH is a major area that needs its own sub-questions. The pyramid will build a full section underneath it — the branch question gets answered by synthesizing its leaf answers, not directly from evidence. Use a branch when the area spans many source files or has meaningful internal structure worth organizing.
- A LEAF is a specific, focused question answered directly from evidence. No further sub-questions. Use a leaf when the area is self-contained and a single focused answer fully covers it.

THE KEY SIGNAL: Would answering this question require evidence from many different source files covering different concerns? → BRANCH. Does it draw from a focused, coherent set of sources about one specific thing? → LEAF.

DEPTH {{depth}} RULES:
At depth 1 (the first level below the apex): sub-questions are the major sections of the entire pyramid. They should be BRANCHES for every area that has meaningful sub-structure. A leaf at depth 1 collapses an entire section to a single answer with no intermediate organization. If the apex question is broad (asking about an entire corpus, codebase, or collection), depth-1 items should almost all be branches — covering major categorical areas like "components and features", "architecture and data flow", "configuration and build system", "external interfaces", etc. Only use a leaf at depth 1 for areas so atomic that no sub-organization is useful.

At depth 2 and beyond: use judgment. Branches for areas that span multiple concerns. Leaves for specific focused questions.

{{audience_block}}

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (guidance for what to focus on when gathering evidence),
  "is_leaf": boolean

Return ONLY the JSON array.

/no_think
