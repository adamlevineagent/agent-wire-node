You are reading sibling nodes from a document Knowledge Pyramid. Each node represents a topic area synthesized from multiple documents. Your job is to synthesize them into a higher-level understanding.

PURPOSE: A reader at this level wants to understand the big picture — how these topic areas relate, what the key themes are, and what matters most. Each topic you create should be a coherent bundle of related findings, decisions, and open items.

PRINCIPLES:
- LATER SIBLINGS ARE MORE CURRENT. When they contradict earlier siblings, the later one is current truth.
- Merge topics that cover the same domain. If two children discuss the same subject, that is ONE topic.
- Let the material determine how many topics you need. A reader should scan your topic names and immediately know which thread to pull for what they care about.

HEADLINE:
- Must describe this node's UNIQUE CONTENT — what distinguishes it from siblings
- Use concrete nouns: "Auth & Token Design", "Build Pipeline Decisions", "Bug Triage Results"
- Avoid generic words: Overview, Summary, Integration, Layer, Platform, System, Architecture

For each topic:
- name: clear, descriptive
- current: What the CURRENT STATE is — latest findings, final decisions, resolved status. Dense with specifics.
- entities: every specific name, system, person, metric, document referenced
- corrections: what changed (wrong → right, with source)
- decisions: what was decided and why

Output valid JSON only:
{
  "headline": "2-6 word label — concrete, no generic words",
  "orientation": "What this node covers, which children to drill for which topics, and what the key takeaway is.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "Current truth. Specific findings, final decisions, resolved items.",
      "entities": ["specific names and references"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think
