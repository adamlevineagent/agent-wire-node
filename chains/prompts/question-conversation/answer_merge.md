{{audience_block}}You are merging partial answers to a knowledge pyramid question. The evidence was too large for a single pass, so it was split into batches. Each batch produced verdicts and a partial synthesis based on its slice of the evidence. Your job is to produce a SINGLE unified answer that reconciles across batches.

### MERGE RULES
1. VERDICTS: Combine all verdicts across batches. When the same node_id appears in multiple batches with different verdicts or different weights, use judgment to determine the correct final call — consider which batch had more relevant context, whether the reasons given are compatible or contradictory, and what the unified evidence set says about that node. When verdicts agree, keep one. The verdicts in your output ARE the final verdicts for this answer.
2. SYNTHESIS: Read all partial syntheses and produce ONE unified synthesis that covers all dimensions. This is NOT a list of batch summaries — synthesize across batches as if you had seen all evidence at once. Where batches overlap, unify; where they complement, weave together; where they diverge, surface the tension.
3. TOPICS: Merge topics from all batches. Deduplicate by name, keeping the richest "current" text. When two batches describe what is meaningfully the same topic with different framings, unify them.
4. MISSING/CORRECTIONS/DECISIONS/TERMS/DEAD_ENDS: Union all entries. Deduplicate by meaning, not just exact string match — two corrections phrased differently that point at the same error are one correction.

{{synthesis_prompt}}

{{content_type_block}}

Respond with ONLY a JSON object:
{
  "headline": "short headline for this answer",
  "distilled": "unified synthesis answering the question — dense, specific, covering all dimensions from ALL batches",
  "topics": [
    {"name": "topic_name", "current": "what we know about this topic"}
  ],
  "verdicts": [
    {"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."},
    {"node_id": "...", "verdict": "DISCONNECT", "reason": "..."}
  ],
  "missing": [],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}

/no_think
