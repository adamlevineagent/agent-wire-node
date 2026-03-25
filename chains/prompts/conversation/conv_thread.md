You are given all the L0 topics from a single THREAD — a coherent subject strand pulled from across a conversation. These topics come from different chunks (different moments in the conversation) but all relate to the same subject.

Your job: synthesize this thread into the CURRENT TRUTH. What does the conversation NOW KNOW about this subject?

CRITICAL TEMPORAL RULES:
- Topics are ordered by chunk position. Higher position = later in conversation = MORE AUTHORITATIVE.
- When a later topic contradicts an earlier one, the LATER topic IS the current truth. The earlier one becomes a correction (wrong → right).
- Do NOT present early ideas as current when they were later overridden.
- Dead ends from earlier in the conversation get ONE sentence ("Also explored X but abandoned in favor of Y at chunk N").
- The LAST 30% of topics in this thread represent the conversation's settled position. Weight them heavily.

ORIENTATION — write a COMPREHENSIVE synthesis (8-15 sentences):
- What is this subject? Define the concept in the conversation's own terms.
- What is the CURRENT STATE? The final decision/architecture/approach.
- How did understanding EVOLVE? "Initially proposed X (chunk 3), discovered problem Y (chunk 7), redesigned as Z (chunk 12)."
- What CORRECTIONS happened? These are the most valuable content.
- What was DECIDED and what's still OPEN?
- What DEAD ENDS were explored and abandoned?

Then organize into 3-8 sub-topics. For each:
- name: a clear aspect of this subject
- current: 4-8 sentences. The CURRENT TRUTH per the latest/highest-order topics. Dense with specifics.
- entities: every named thing from the CURRENT state (not superseded names)
- corrections: wrong → right → who, with source chunk
- decisions: what was decided, what was rejected, why

Output valid JSON only:
{
  "headline": "2-6 word thread label — concrete, human-friendly",
  "orientation": "8-15 sentences: complete synthesis of this subject. Current state, evolution, corrections, decisions, open items.",
  "source_nodes": ["L0-000", "L0-007", "L0-012"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "4-8 sentences. Current truth only. What this IS right now per the LATEST topics.",
      "entities": ["named thing 1", "named thing 2"],
      "corrections": [{"wrong": "early idea", "right": "final approach", "who": "chunk N"}],
      "decisions": [{"decided": "what was chosen", "why": "reasoning", "rejected": "what was not chosen"}]
    }
  ]
}

/no_think
