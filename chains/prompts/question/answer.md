{{audience_block}}You are answering a knowledge pyramid question using candidate evidence from the layer below.

### 0. LAYER ROUTING — read this BEFORE anything else

Look at the input nodes. Are they raw extracted sources (concrete claims pulled from chunks of files or documents), or are they themselves synthesized answers from a lower pyramid layer (each one already a "headline + distilled" pair)?

**If the inputs are RAW SOURCES (you are extracting from primary evidence):** extract specific ground-truth details. The static rules below apply normally. Skip to section 1.

**If the inputs are ANSWERS from a lower layer (you are summarizing summaries):** the rules change. You are not extracting — you are zooming out. Read the next block carefully and treat it as overriding any conflicting guidance in `{{synthesis_prompt}}` further down (which was written for raw-source extraction and does not apply to you).

#### ABSTRACTION CONTRACT (when inputs are themselves answers)

Whatever level of abstraction the items you're looking at sit at, your job is to be one logical zoom-level pulled back. The children have already covered their own ground at their granularity — restating them at the same granularity is failure. A reader who has seen the children should learn something new from your answer or your answer should not exist.

Don't repeat the structure or framing of the row below. Collapse the children into their next logical buckets and name what those buckets actually are. The right grain is: "if these N children each describe a discrete subsystem, what is the *kind of system* they collectively constitute?" or "if these children describe behaviors, what is the *pattern* they participate in?" or "if they describe components, what is the *architecture* that organizes them?"

Honest signal beats manufactured insight: if the children genuinely don't share a cross-cutting pattern, say that plainly — "these are independent concerns held together only by the platform that hosts them" — and that meta-statement IS your answer. Don't invent fake unifying mechanisms. Don't relabel either.

The `topics` field at higher layers should not re-list child topics. It should name themes that only emerge when the children are held together — patterns the eye only catches with the wider view.

A useful test: cover your headline and read your distilled. Could a reader predict your headline from any single child's headline? If yes, you are not pulled back far enough.

### 1. EVIDENCE TRIAGE (Verdicts)
For each candidate node, you MUST report a verdict:
- KEEP(weight, reason) — this evidence is factually relevant to the question. **KEEP is NOT a zero-sum game or a threshold for profoundness.** If the evidence adds any relevant detail, KEEP it and use the `weight` (0.0-1.0) to signify its centrality. A core architectural pattern might be 0.9, while an additive styling detail might be 0.3. Keep all additive details!
- DISCONNECT(reason) — this evidence is a false positive and completely irrelevant to the question.
- MISSING(description) — describe evidence you wish you had but don't.

### 2. SYNTHESIS RULES
Then synthesize your answer to the question using ONLY the KEEP evidence.
Your synthesis should be dense and specific — names, decisions, relationships from the evidence. Not a vague overview.

#### ABSTAIN WHEN EVIDENCE IS EMPTY
If every verdict you assign is DISCONNECT — meaning none of the candidate evidence actually addresses the question — you have nothing grounded to answer with. Set `"abstain": true` at the top level of your response and leave `headline`, `distilled`, `topics`, `corrections`, `decisions`, `terms`, and `dead_ends` empty (or omit them). Still report your `verdicts` and `missing` so the upper layer can see the disconnect and reconsider clustering. Synthesizing an answer over zero relevant evidence is fabrication, not synthesis. Refusing is the right move — this node simply should not exist as written, and abstaining lets the system route its evidence elsewhere.

If this is a LEAF node (synthesizing raw sources), focus entirely on extracting specific, ground-truth details from the evidence.
If this is a BRANCH node (synthesizing leaf answers or lower branch answers), the ABSTRACTION CONTRACT in section 0 governs. The static "do not concatenate" rules and the dynamic `{{synthesis_prompt}}` below are SECONDARY to that contract.

The dynamic prompt below was generated at build start to guide L0 evidence extraction. If you are at depth ≥ 2, treat it as background context only — your contract is in section 0.

{{synthesis_prompt}}

{{content_type_block}}

Respond with ONLY a JSON object. When abstaining, set `"abstain": true` and leave the answer fields empty/omitted; when answering normally, omit `abstain` or set it to false:
{
  "abstain": false,
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
