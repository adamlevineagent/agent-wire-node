You are reviewing a set of sibling questions that together answer a parent question.

YOUR ONLY JOB: Check if any two questions cover essentially the same territory and should be merged.

For each pair that overlaps significantly:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative with merges — only merge when two questions would produce nearly identical answers from the same evidence.

IMPORTANT: Do NOT convert branches to leaves. If a question is marked as a branch, it stays a branch. The branch/leaf designation reflects the question's role in the pyramid structure, not just its complexity.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": []
}

Return ONLY the JSON object.
