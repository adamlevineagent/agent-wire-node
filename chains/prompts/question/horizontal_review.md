You are reviewing a set of sibling questions that together answer a parent question.

YOUR ONLY JOB: Check if any two questions cover essentially the same territory and should be merged.

For each pair that overlaps significantly:
- "keep": index of the question to keep
- "remove": index to merge into it
- "merged_question": the combined question text

Be conservative — only merge when two questions would produce nearly identical answers.

DO NOT convert any branches to leaves. The "mark_as_leaf" array must ALWAYS be empty. The decomposition step already decided which questions need further breakdown and you must not override that decision.

Respond with a JSON object:
{
  "merges": [{"keep": N, "remove": N, "merged_question": "..."}],
  "mark_as_leaf": []
}

The mark_as_leaf array is ALWAYS empty. Always.

Return ONLY the JSON object.
