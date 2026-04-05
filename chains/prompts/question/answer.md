{{audience_block}}You are answering a knowledge pyramid question using candidate evidence from the layer below.

### 1. EVIDENCE TRIAGE (Verdicts)
For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is factually relevant to the question. **KEEP is NOT a zero-sum game or a threshold for profoundness.** If the evidence adds any relevant detail, KEEP it and use the `weight` (0.0-1.0) to signify its centrality. A core architectural pattern might be 0.9, while an additive styling detail might be 0.3. Keep all additive details!
- DISCONNECT(reason) — this evidence is a false positive and completely irrelevant to the question.
- MISSING(description) — describe evidence you wish you had but don't.

### 2. SYNTHESIS RULES
Then synthesize your answer to the question using ONLY the KEEP evidence.
Your synthesis should be dense and specific — names, decisions, relationships from the evidence. Not a vague overview.

If this is a LEAF node (synthesizing raw sources), focus entirely on extracting specific, ground-truth details from the evidence.
If this is a BRANCH node (synthesizing leaf answers or lower branch answers), YOU MUST ADD SYNTHESIS VALUE:
- DO NOT just concatenate or mechanically rephrase the lower-level answers into a broader list.
- DO NOT list components or technical dependencies as the primary answer.
- If lower nodes describe A, B, and C, ask: what TENSION, PATTERN, or INSIGHT only becomes visible when you hold A-B-C together? What does their combination reveal that none shows individually?
- Your synthesis should name the underlying dynamic — the trade-off being navigated, the mechanism that connects them, or the emergent property they collectively produce.

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
  "corrections": [
    {"wrong": "incorrect claim from evidence", "right": "what is actually true"}
  ],
  "decisions": [
    {"decided": "what was decided", "why": "rationale"}
  ],
  "terms": [
    {"term": "domain term", "definition": "what it means in this context"}
  ],
  "dead_ends": []
}

/no_think
