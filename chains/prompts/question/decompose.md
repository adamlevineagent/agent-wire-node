You are decomposing a question into sub-questions to build a knowledge pyramid from "{{content_type}}" content.

You will receive the parent question AND summaries of the source material. USE THE SOURCE MATERIAL to understand what the system or corpus IS and DOES — not to derive a list of files or folders.

WHAT GOOD DECOMPOSITION LOOKS LIKE:
Ask about what the system DOES, how it WORKS, why it EXISTS, and what makes it distinctive. Do NOT split by structural paths, file layout, or generic categories.

BEFORE writing any sub-questions, read the source material and identify the TENSIONS AND ORGANIZING PRINCIPLES unique to THIS corpus. Every body of knowledge has its own natural fault lines — the dimensions along which it divides into meaningfully different concerns. Your job is to FIND those dimensions, not impose predetermined ones.

Ask yourself: "What are the fundamental tensions in this material? What trade-offs does it navigate? What are the distinct mechanisms that can't be understood through each other?" Your sub-questions should explore along THOSE dimensions.

BAD decomposition:
- "What files are in the X folder?" (file layout)
- "What does the config file do?" (implementation detail)
- "What is the UI made of?" (structural inventory)
- Generic questions that could apply to ANY corpus without reading it (purpose/architecture/data/operations)

GOOD decomposition asks questions that could ONLY emerge from reading THIS specific material — questions that reveal what makes this corpus distinctive, what tensions it contains, what problems it uniquely solves, what trade-offs it navigates.

WHEN THE CORPUS IS A SEQUENTIAL TRANSCRIPT (conversation, session, meeting, interview, journal, exchange):
The transcript is the PRIMARY SOURCE, not the subject. Do NOT decompose into questions ABOUT the transcript-as-artifact — that produces meta-questions that the contents cannot answer:
- "What is the purpose of this conversation?" — bad. The conversation's purpose is what the speakers were doing inside it, not metadata about the file.
- "What is the value of this session?" — bad. Same problem.
- "Who is the audience for this chat?" — bad.
- "What was the goal of this meeting?" — only good if the speakers themselves explicitly stated and revisited a goal inside the transcript.

Instead, decompose into questions about WHAT WAS ACTUALLY DISCUSSED, ATTEMPTED, FELT, DECIDED, OR CHANGED inside the transcript. The speakers themselves are the system; their exchange is the activity. Sub-questions must point at things that exist between the lines of the transcript, not at the transcript's existence.

If the apex question is itself temporal/narrative ("tell the story of...", "what was the arc of...", "what was true at the start that wasn't true at the end"), your sub-questions should preserve that temporal frame. Decompose by phases of the session, by threads that ran through it, by turning points, by what changed — not by static categories.

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
