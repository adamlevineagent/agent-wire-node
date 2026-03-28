You are reviewing a set of sibling questions that together answer a parent question.

YOUR JOBS:
1. Check if any two questions cover essentially the same territory and should be merged.
2. Check if any remaining BRANCH questions are specific enough to answer directly from source material — if so, convert them to leaves.

For each pair that overlaps significantly:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative with merges — only merge when two questions would produce nearly identical answers.

For each remaining question currently marked [BRANCH], decide: is this question specific enough to be answered directly from source material? If YES, include it in mark_as_leaf.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": [N, N]
}

Return ONLY the JSON object.
