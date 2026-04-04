{{audience_block}}You are answering a knowledge pyramid question using candidate evidence from the layer below.

For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is relevant. Weight 0.0-1.0 indicates how central it is.
- DISCONNECT(reason) — this evidence was a false positive from pre-mapping, not actually relevant.
- MISSING(description) — describe evidence you wish you had but don't.

Then synthesize your answer to the question using ONLY the KEEP evidence.

Every KEEP candidate that represents a genuinely distinct dimension of the answer should be reflected in your synthesis. If the question asks about an entire system and you have evidence about architecture, economics, legal, and operations — all of those belong in the answer. Don't drop dimensions just because some are more central than others.

Your synthesis should be dense and specific — names, decisions, relationships from the evidence. Not a vague overview.

{{synthesis_prompt}}

{{content_type_block}}

Respond with ONLY a JSON object:
{
  "headline": "short headline for this answer",
  "distilled": "synthesis answering the question — dense, specific, covering all major dimensions from the evidence",
  "topics": [
    {"name": "topic_name", "current": "what we know about this topic"}
  ],
  "verdicts": [
    {"node_id": "...", "verdict": "KEEP", "weight": 0.85, "reason": "..."},
    {"node_id": "...", "verdict": "DISCONNECT", "reason": "..."},
    {"node_id": "...", "verdict": "KEEP", "weight": 0.3, "reason": "..."}
  ],
  "missing": [
    "description of evidence we wish we had"
  ],
  "corrections": [],
  "decisions": [],
  "terms": [],
  "dead_ends": []
}
