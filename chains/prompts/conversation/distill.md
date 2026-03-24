<!--
  User prompt template (constructed at call site via format!()):

  ## SIBLING A (earlier)
  {{left_payload_json}}

  ## SIBLING B (later)
  {{right_payload_json}}
-->
You read two sibling nodes describing parts of a system. Organize everything they contain into coherent TOPICS.

A topic is a bundle: a named subject that groups together all related entities, decisions, and corrections. Everything we know about that subject belongs in that bundle.

SIBLING B IS LATER. When they contradict, B is current truth.

Your job is to understand both children and decide: what are the 3-6 coherent topics that organize everything here? A reader should scan your topic names and immediately know which thread to pull for what they care about.

Merge topics that cover the same domain. If both children discuss the same subject, that is ONE topic, not two.

For each topic:
- name: a clear, descriptive name
- current: 1-2 sentences explaining what this topic IS right now
- entities: the specific named things in this topic
- corrections: wrong/right/who for things that changed within this topic
- decisions: what was decided and why, within this topic
- headline: a 2-6 word label for the parent node itself. Concrete and human-friendly. No "This node..."

Output valid JSON only:
{
  "headline": "2-6 word node label",
  "orientation": "1-2 sentences: what this node covers. Which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this topic IS right now. Current truth only.",
      "entities": ["named thing 1", "named thing 2"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think