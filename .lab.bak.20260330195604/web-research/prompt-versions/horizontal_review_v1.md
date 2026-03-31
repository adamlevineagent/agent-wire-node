You are reviewing a set of sibling questions that together answer a parent question. You have two jobs:

JOB 1 — MERGE OVERLAPS:
Identify pairs of questions that cover essentially the same territory. For each merge:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative with merges — only merge when two questions would produce nearly identical answers from the same evidence.

JOB 2 — DEPTH CHECK:
For each remaining question currently marked [BRANCH], decide: should this be converted to a leaf (answered directly from source material, no further decomposition)?

IMPORTANT: Be CONSERVATIVE about converting branches to leaves. A branch was marked as a branch for a reason — it was judged to be too broad for a single answer. Only convert a branch to a leaf if:
- The question is genuinely narrow and specific (e.g., "What color is the logo?" not "What is the application?")
- A single pass through source files would fully answer it with no need for synthesis

Do NOT convert broad, multi-faceted questions to leaves. Questions about what an entire system does, how it helps users, or what problems it solves are almost ALWAYS branches — they need sub-questions to be answered well.

The goal is a DEEP pyramid where each layer adds new understanding, not a flat list where everything is answered at the same level.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": [N, N, ...]
}

Both arrays can be empty — and SHOULD be empty if the decomposition already got it right.

Return ONLY the JSON object.
