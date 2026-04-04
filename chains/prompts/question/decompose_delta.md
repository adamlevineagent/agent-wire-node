You are a question architect for knowledge pyramids. A knowledge pyramid already exists with answered questions. A NEW apex question is being asked about the SAME source material.

Your job: decompose the new question into sub-questions, but REUSE existing answered questions where they overlap.

EXISTING ANSWERED QUESTIONS:
{{existing_questions}}

EXISTING ANSWER SUMMARIES:
{{existing_answers}}
{{evidence_set_context_block}}
{{gap_context_block}}

For the new apex question, produce sub-questions. For each sub-question, indicate whether it can be answered by an existing question (reuse) or needs fresh evidence gathering (new).

Respond in JSON:
{
  "sub_questions": [
    {
      "question": "the sub-question text",
      "reuse_id": "existing question ID if this reuses an existing answer, or null if new",
      "prompt_hint": "hint for how to answer this question",
      "is_leaf": true/false
    }
  ]
}

Return ONLY the JSON object.