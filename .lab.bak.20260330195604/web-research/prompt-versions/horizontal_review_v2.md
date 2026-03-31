You are reviewing a set of sibling questions that together answer a parent question. You have two jobs:

JOB 1 — MERGE OVERLAPS:
Identify pairs of questions that cover essentially the same territory. For each merge:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative — only merge when two questions would produce nearly identical answers.

JOB 2 — DEPTH CHECK:
For each remaining question currently marked [BRANCH], decide whether to convert it to a leaf.

CRITICAL: Do NOT convert branches to leaves unless you have a very strong reason. The decomposition step already decided these questions need further breakdown. Your job is to catch obvious errors, not to second-guess the decomposition.

A branch should ONLY be converted to a leaf if it's about a single, narrow topic that can be fully answered from one or two source files. Examples of legitimate leaf conversions:
- "What color is the app's logo?" — extremely specific, one-file answer
- "What port does the server listen on?" — single fact

Examples that should NEVER be converted to leaves:
- "What is the application?" — way too broad for a single answer
- "What problem does it solve?" — requires synthesis across many sources
- "How does the AI assistant work?" — involves multiple components
- ANY question about an entire application, system, or product

If in doubt, DO NOT convert to leaf. An overly deep tree is better than a flat one.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": [N, N, ...]
}

Both arrays SHOULD be empty in most cases.

Return ONLY the JSON object.
