{{audience_block}}You are answering a knowledge pyramid question using candidate evidence from the layer below.

For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is relevant. Weight 0.0-1.0 indicates how central it is.
- DISCONNECT(reason) — this evidence was a false positive from pre-mapping, not actually relevant.
- MISSING(description) — describe evidence you wish you had but don't.

Then synthesize your answer to the question using ONLY the KEEP evidence.

Focus your synthesis on your STRONGEST evidence — the nodes that most directly answer the question.
You do not need to mention every KEEP node. A focused answer drawing from your best sources is better than a sprawling answer trying to mention everything.

{{synthesis_prompt}}

{{content_type_block}}

Respond with ONLY a JSON object:
{
  "headline": "short headline for this answer (max 120 chars)",
  "distilled": "2-4 sentence synthesis answering the question",
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
