You are building a question DAG for a knowledge pyramid from "{{content_type}}" content.

You will receive a whole frontier layer of parent questions plus summaries of the source material. Produce the canonical next layer of child questions for the entire frontier at once.

WHAT GOOD FRONTIER DECOMPOSITION LOOKS LIKE:
- Decompose from the higher-order question layer toward source-answerable questions.
- Treat the parent frontier as one planning surface, not as isolated branches.
- Create one canonical child question when the same child helps answer multiple parents.
- Attach that child to every relevant parent with `parent_ids`.
- Keep child questions across the whole layer non-overlapping.
- Let the material decide the shape. Prefer the smallest focused set that covers the real conceptual work. Do not pad.

BEFORE writing child questions, read the source material and identify the tensions, mechanisms, trade-offs, and organizing principles that cut across the parent frontier. Shared sub-questions are valuable when they reveal a real shared dependency between multiple higher-order questions.

BAD decomposition:
- Creating duplicate child questions under different parents with slightly different wording.
- Splitting by file layout or generic component categories.
- Re-surveying the whole corpus for every parent.
- Creating meta-questions about the existence of the transcript, file, or build artifact.

GOOD decomposition:
- Produces child questions that could only emerge from this corpus.
- Preserves cross-parent overlap as multiple edges into one canonical question.
- Moves one conceptual layer closer to evidence, while leaving another layer below when synthesis is still needed.

BRANCH vs LEAF:
- A BRANCH is a child question that still needs another layer of sub-questions before it can be answered well.
- A LEAF is a focused question answerable directly from source evidence.
- If a question spans multiple distinct mechanisms or concerns, set `is_leaf` false.
- If a question can be answered from a coherent evidence set without further decomposition, set `is_leaf` true.

DEPTH {{depth}} RULES:
At the first frontier below the apex, child questions are major sections of the pyramid and are often branches.
At deeper frontiers, child questions must be specific aspects of the parent frontier. Do not drift back to top-level categories unless multiple parents genuinely share that concern.

GRANULARITY GUIDANCE:
The configured breadth hint for this run is {{min_subs}} to {{max_subs}} children per parent, but this is only a hint. Prefer semantic completeness and deduplication over hitting a count.

{{audience_block}}

Respond with JSON only:
[
  {
    "question": "canonical child question",
    "prompt_hint": "what answering should focus on",
    "is_leaf": true,
    "parent_ids": ["parent question id", "..."]
  }
]

Return ONLY the JSON array.

/no_think
