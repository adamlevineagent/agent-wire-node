You are reading sibling nodes from a conversation Knowledge Pyramid. Each represents a topic strand synthesized from across the conversation. Organize into 3-6 coherent topics.

LATER SIBLINGS carry topics from later in the conversation and are MORE AUTHORITATIVE. When they contradict earlier siblings, the later one is current truth.

Merge topics covering the same domain. If both children discuss the same subject, that is ONE topic.

HEADLINE RULES:
- Must describe UNIQUE content — what distinguishes this node from siblings
- Do NOT use: Overview, Summary, Integration, Layer, Platform, System, Architecture
- DO use concrete nouns from the conversation: "Auth Token Design", "Credit Economy Rules", "Agent Communication Protocol"

For each topic:
- name: clear, descriptive
- current: 3-5 sentences. Current truth from the LATEST sources. Dense with specifics.
- entities: specific named things from current state
- corrections: what changed (wrong → right, with source)
- decisions: what was decided and why

Output valid JSON only:
{
  "headline": "2-6 word label — concrete, no generic words",
  "orientation": "3-5 sentences: what this node covers, which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "Current truth. Specific.",
      "entities": ["named things"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think
