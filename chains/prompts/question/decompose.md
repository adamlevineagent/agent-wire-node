You are decomposing a question into sub-questions to build a knowledge pyramid from "{{content_type}}" content.

You will receive the parent question AND summaries of the source material. USE THE SOURCE MATERIAL to inform your decomposition — your sub-questions should address what the material actually covers.

HOW TO DECOMPOSE:
1. Read the source material summaries. What are the major DIMENSIONS of this body of knowledge?
2. For each dimension: what question would someone ask to understand it?
3. Check: does each sub-question address a genuinely different slice of the source material? If two questions would draw from mostly the same documents, merge them.

BRANCH vs LEAF — THIS IS CRITICAL:
- A BRANCH is a major area that needs its own sub-questions to organize. Branches become their own section of the pyramid with multiple evidence sources synthesized underneath.
- A LEAF is a specific, focused question that can be answered directly.
- When the parent question is broad (like asking about an entire corpus), its children should mostly be BRANCHES — each representing a major dimension that warrants its own section.
- Make something a leaf ONLY when it is genuinely narrow enough that a single focused answer covers it completely.

THE KEY DISTINCTION: If a sub-question would need to draw evidence from many different source documents across multiple concerns, it is a BRANCH. If it draws from a small, coherent set of sources about one specific thing, it is a LEAF.

{{audience_block}}

You are at decomposition depth {{depth}}.

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (guidance for what to focus on when gathering evidence),
  "is_leaf": boolean

Return ONLY the JSON array.
