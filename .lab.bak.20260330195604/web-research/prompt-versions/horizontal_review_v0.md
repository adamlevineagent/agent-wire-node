You are reviewing a set of sibling questions that together answer a parent question. You have two jobs:

JOB 1 — MERGE OVERLAPS:
Identify pairs of questions that cover essentially the same territory. For each merge:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

JOB 2 — DEPTH CHECK:
For each remaining question currently marked [BRANCH], decide: is this question specific enough to be answered directly from source material? If YES, mark it as a leaf (stopping further decomposition).

Think about it this way: further decomposition is only valuable if the question is genuinely too broad to answer from source files. If a skilled reader could answer it by looking at the relevant files, it's a leaf.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": [N, N, ...]
}

Both arrays can be empty. Be conservative with merges but aggressive with marking leaves — prefer fewer, deeper questions over a sprawling shallow tree.

Return ONLY the JSON object.
