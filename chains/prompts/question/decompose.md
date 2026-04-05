You are decomposing a question into sub-questions to build a knowledge pyramid from "{{content_type}}" content.

You will receive the parent question AND summaries of the source material. USE THE SOURCE MATERIAL to understand what the system or corpus IS and DOES — not to derive a list of files or folders.

WHAT GOOD DECOMPOSITION LOOKS LIKE:
Ask about what the system DOES, how it WORKS, why it EXISTS, what someone would NEED TO UNDERSTAND to grasp it fully. Good sub-questions address purpose, behavior, architecture, capabilities, data flow, and user experience.

BAD decomposition:
- "What files are in the X folder?"
- "What is the top-level directory layout?"
- "What does the config file do?"
These tell someone WHERE things are, not what they mean. Sub-questions ask about knowledge areas, not about files or file locations.

GOOD decomposition for code:
- "What is this software and what problem does it solve?"
- "What are the core user-facing capabilities?"
- "How does the [major system] work?"
- "What are the key data models and how does data flow?"
- "How are [major feature areas] implemented?"

GOOD decomposition for documents:
- "What is the central argument or purpose of this body of work?"
- "What are the key mechanisms, policies, or decisions described?"
- "What are the implications or outcomes?"

GRANULARITY GUIDANCE:
The `granularity` parameter is a scale from 1 (highly focused — only the most essential questions) to 5 (comprehensive — all meaningful sub-questions). At granularity 3, aim for a balanced decomposition: enough sub-questions to cover the major areas without excessive detail. Lean toward merging overlapping concerns rather than splitting everything out.

HOW TO DECOMPOSE:
1. Read the source material summaries. What does this system or corpus actually DO? What is its purpose?
2. What would someone need to understand to fully grasp this? List the major conceptual areas.
3. For each area: what question would an intelligent person ask to understand it?
4. Check: does each sub-question address a genuinely different concern? If two questions would draw from mostly the same documents, merge them. Be aggressive about merging — cover all real dimensions while keeping the set tight.

BRANCH vs LEAF:
- A BRANCH is a major area that needs its own sub-questions. The pyramid will build a full section underneath it — the branch question gets answered by synthesizing its leaf answers, not directly from evidence. Use a branch when the area spans many source files or has meaningful internal structure worth organizing.
- A LEAF is a specific, focused question answered directly from evidence. No further sub-questions. Use a leaf when the area is self-contained and a single focused answer fully covers it.

THE KEY SIGNAL: Would answering this question require evidence from many different source files covering different concerns? → BRANCH. Does it draw from a focused, coherent set of sources about one specific thing? → LEAF.

DEPTH {{depth}} RULES:
At depth 1 (the first level below the apex): these are the major sections of the entire pyramid. Conceptual areas — purpose, architecture, user experience, data models, capabilities — are almost always BRANCHES because they span many source files and have meaningful internal structure. A leaf at depth 1 collapses an entire topic to a single answer with no sub-organization. Only use a leaf at depth 1 for areas so narrow and atomic that no sub-question would add value.

At depth 2 and beyond: your sub-questions MUST be specific aspects of the PARENT QUESTION. You are breaking down ONE topic, not re-surveying the whole corpus. If the parent asks about user-facing capabilities, your sub-questions must be about specific capabilities. If the parent asks about architecture, your sub-questions must be about specific architectural concerns. Do NOT drift into filesystem layout or unrelated topics just because the source material contains those things.

{{audience_block}}

Respond with a JSON array of objects, each with:
  "question": string,
  "prompt_hint": string (guidance for what to focus on when gathering evidence),
  "is_leaf": boolean

Return ONLY the JSON array.

/no_think
