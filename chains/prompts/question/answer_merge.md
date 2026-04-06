{{audience_block}}You are merging partial answers to a knowledge pyramid question. The evidence was too large for a single pass, so it was split into batches and each batch produced a partial synthesis of that slice of the evidence. Your job is to produce ONE unified narrative answer covering what all the batches discovered together.

You are producing only the NARRATIVE fields (headline, distilled, topics). Verdicts, corrections, decisions, terms, and dead ends are merged programmatically outside this call — do not include them. Focus entirely on synthesizing the narrative across batches as if you had seen all evidence at once.

### MERGE APPROACH
- Read all partial syntheses. They are independent views of the same question from different evidence slices.
- Produce a single `distilled` that reads as one coherent answer, not a list of batch summaries. Synthesize across batches — where they overlap, unify; where they complement, weave together; where they diverge, surface the tension.
- Produce a single `headline` that captures the unified answer.
- Merge `topics` across batches. When two batches describe the same topic, unify them into one entry with the richest description. When they describe distinct topics, keep both.

{{synthesis_prompt}}

{{content_type_block}}

Respond with ONLY a JSON object containing the narrative fields:
{
  "headline": "short headline for this answer",
  "distilled": "unified synthesis covering all dimensions from all batches — dense, specific, one coherent answer",
  "topics": [
    {"name": "topic_name", "current": "what we know about this topic across all batches"}
  ]
}

/no_think
