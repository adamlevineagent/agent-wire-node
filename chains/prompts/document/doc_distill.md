You are reading sibling nodes from a document Knowledge Pyramid. Each node represents a topic area synthesized from multiple documents. Your job is to synthesize them into a higher-level understanding.

PURPOSE: A reader at this level wants to understand the big picture — how these topic areas relate, what the key themes are, and what matters most. Each topic you create should be a coherent bundle of related findings, decisions, and open items.

PRINCIPLES:
- LATER SIBLINGS ARE MORE CURRENT. When they contradict earlier siblings, the later one is current truth.
- Merge topics that cover the same domain. If two children discuss the same subject, that is ONE topic.
- Let the material determine how many topics you need.
- **EVERY child must be represented.** Your synthesis should cover ALL children, not just the most interesting ones. A reader who drills into any child should find their topic reflected in your summary.

HEADLINE:
- If you are synthesizing ALL the remaining sibling nodes (likely the apex/root level), your headline should describe the ENTIRE collection — what is this body of knowledge about?
- If you are at an intermediate level with siblings above you, your headline should describe what distinguishes THIS group from other groups at the same level.
- Use concrete nouns. Avoid filler words.

ORIENTATION:
- Must reference ALL children by name and explain what each contributes
- A reader should know which child to drill into for which topic
- Include the key takeaway: what does this collection of knowledge ADD UP TO?

For each topic:
- name: clear, descriptive
- current: What the CURRENT STATE is — latest findings, final decisions, resolved status. Dense with specifics.
- entities: every specific name, system, person, metric, document referenced
- corrections: what changed (wrong → right, with source)
- decisions: what was decided and why

Output valid JSON only:
{
  "headline": "2-6 word label",
  "orientation": "What this node covers, what each child contributes, and the key takeaway.",
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
