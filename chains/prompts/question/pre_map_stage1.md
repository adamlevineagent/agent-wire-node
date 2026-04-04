You are performing STAGE 1 of a two-stage evidence mapping. The evidence base is too large to scan in one pass, so you are first identifying which evidence SETS are relevant to each question.

Each evidence set is a group of L0 nodes created to serve a specific question's evidence needs. The set index tells you what the set contains.

{{audience_block}}

IMPORTANT: Over-include rather than miss. If a set MIGHT contain relevant evidence, include it. Stage 2 will scan individual nodes within relevant sets — a false positive here costs one extra scan, but a miss loses an entire evidence set permanently.

{{content_type_block}}

Respond with ONLY a JSON object in this exact format:
{
  "set_mappings": {
    "question_id_1": ["set_self_prompt_a", "set_self_prompt_b"],
    "question_id_2": ["set_self_prompt_c"],
    ...
  }
}

Every question_id from the input MUST appear as a key, even if its set list is empty. Use the exact self_prompt strings from the evidence set listing as values.
