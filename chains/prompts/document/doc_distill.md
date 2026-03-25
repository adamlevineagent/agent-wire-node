You are reading sibling nodes from a document Knowledge Pyramid. Each node represents a topic area synthesized from multiple documents. Organize everything into coherent TOPICS.

A topic is a bundle: a named subject grouping all related findings, decisions, and open items. Everything known about that subject belongs in that bundle.

LATER SIBLINGS ARE MORE CURRENT. When they contradict earlier siblings, the later one is the current truth.

Your job: understand all children and decide what 3-6 coherent topics organize everything here. A reader should scan your topic names and immediately know which thread to pull for what they care about.

Merge topics that cover the same domain. If both children discuss the same subject, that is ONE topic.

HEADLINE RULES:
- Your headline must describe this node's UNIQUE CONTENT — what distinguishes it from siblings
- Do NOT use generic words: Overview, Summary, Integration, Layer, Platform, System, Architecture
- DO use concrete nouns: "Auth & Token Design", "Build Pipeline Decisions", "Bug Triage Results"

For each topic:
- name: clear, descriptive
- current: 3-5 sentences. What the CURRENT STATE is — latest findings, final decisions, resolved status. Dense with specifics.
- entities: every specific name, system, person, metric, document referenced
- corrections: what changed (wrong → right, with source)
- decisions: what was decided and why

Output valid JSON only:
{
  "headline": "2-6 word label — concrete, no generic words",
  "orientation": "3-5 sentences: what this node covers, which children to drill for which topics, and what the key takeaway is.",
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
