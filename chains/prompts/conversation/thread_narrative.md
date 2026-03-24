<!--
  User prompt template (constructed at call site via format!()):

  ## THREAD: {{thread_name}}

  ## TOPICS (chronological — higher order = later = more authoritative)
  {{assigned_topics_json}}

  Each topic in the array has an "order" field (1-based, higher = later)
  and topics in the top 30% have "temporal_authority": "LATE — AUTHORITATIVE".
-->
You are given all the topics from a single THREAD — a coherent narrative strand pulled from across a knowledge pyramid. These topics come from different L1 nodes (different parts of the conversation) but all relate to the same subject.

Your job: synthesize this thread into coherent sub-topics. What is the CURRENT TRUTH? Organize by sub-theme, not by source.

CRITICAL TEMPORAL RULE:
Each topic has an "order" number. Higher order = later in the conversation = MORE AUTHORITATIVE.
When a high-order topic contradicts a low-order topic, the HIGH-ORDER topic IS the current truth and the low-order topic IS the old/superseded state. Record the superseded state as a correction (wrong → right).
DO NOT present early ideas as current when they were later overridden.
Topics marked [LATE — AUTHORITATIVE] represent the final state of the conversation and ALWAYS override earlier topics on the same subject.

For each sub-topic:
- name: a clear aspect of this thread
- current: what this aspect IS RIGHT NOW per the latest/highest-order topics (1-2 sentences)
- entities: specific named things from the CURRENT state
- corrections: wrong/right/who for things that changed, with source node
- decisions: what was decided and why, with source node (prefer late decisions)
- headline: a 2-6 word label for the thread node itself. Concrete and human-friendly. No "This thread..."

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "1-2 sentences: what this thread covers. Which source nodes to drill for which sub-topics.",
  "source_nodes": ["L1-000", "L1-003"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "What this sub-topic IS right now per the LATEST topics.",
      "entities": ["named thing 1", "named thing 2"],
      "corrections": [{"wrong": "...", "right": "...", "who": "...", "source": "L1-XXX"}],
      "decisions": [{"decided": "...", "why": "...", "source": "L1-XXX"}]
    }
  ]
}

/no_think