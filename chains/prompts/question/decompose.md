You are decomposing a question into sub-questions to build a knowledge pyramid from "{{content_type}}" content.

You will receive the parent question AND summaries of the source material. USE THE SOURCE MATERIAL to understand what the system or corpus IS and DOES — not to derive a list of files or folders.

WHAT GOOD DECOMPOSITION LOOKS LIKE:
Ask about what the system DOES, how it WORKS, why it EXISTS, and how it behaves across time and space. You MUST use a MULTI-LENS ABSTRACTION FRAMEWORK to break down the question, rather than just splitting it by structural paths.

Force your sub-questions to view the corpus through these lenses:
1. **The Value/Intent Lens**: "What human or business value does this enable? Why was it built?"
2. **The Kinetic/State Flow Lens**: "How do data, leverage, and events move through this space? How is state mutated?"
3. **The Temporal Lens**: "How do the components of the corpus relate to time relative to each other? (e.g., Pre-flight builds vs Active lifecycles vs Historical state)"
4. **The Metaphorical Lens**: "What underlying physical or societal mechanism does this system emulate conceptually?"

BAD decomposition:
- "What files are in the X folder?"
- "What does the config file do?"
- "What is the UI made of?"
These tell someone WHERE things are or what they are made of mechanically, not what they mean conceptually. Sub-questions must evaluate profound systemic abstraction.

GOOD decomposition:
- "What is the overarching value and intent of this corpus?"
- "How does the system mutate and distribute state across its kinetic flow?"
- "Where do these capabilities sit across the temporal lifecycle (build vs runtime)?"
- "What core conceptual mechanisms and policies define the architecture?"

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
