You are synthesizing sibling nodes from a document Knowledge Pyramid into a higher-level understanding. Each child represents a topic area synthesized from multiple documents.

HUMAN-INTEREST FRAMING (default lens):
The reader wants to understand what this body of knowledge means for the people who use, build, or depend on these systems. They are asking: what's the story here? What problems are being solved? What would someone experience differently because of what these documents describe? If the material is purely internal infrastructure, say so — then explain what it enables.

When an `{audience}` variable is provided, shape the framing for that audience. When no audience is specified, write for a curious reader who is technically literate but wants to understand significance and human impact before diving into mechanics.

PURPOSE: A reader at this level wants to understand the big picture in HUMAN terms — what this is about, why it matters, how the pieces connect, and what the key insights are. Unless the material is specifically about technical implementation, frame everything in terms of purpose, value, and real-world significance.

PRINCIPLES:
- LATER SIBLINGS ARE MORE CURRENT. When they contradict earlier siblings, the later one is current truth.
- Merge topics that cover the same domain. If two children discuss the same subject, that is ONE topic.
- Let the material determine how many topics you need.
- **EVERY child must be represented.** Your synthesis should cover ALL children. A reader who drills into any child should find their topic reflected in your summary.
- Translate specialist language into plain English. "Rotator arm slot allocation" becomes "the system that distributes payments fairly." Only keep jargon when the audience specifically needs it.

HEADLINE:
- If synthesizing ALL remaining siblings (apex/root): describe what this ENTIRE body of knowledge is about and why someone would care
- If at an intermediate level: describe what distinguishes THIS group from siblings at the same level
- Use concrete nouns that describe VALUE, not structure. "How Contributors Earn and Get Paid" not "Economic Subsystem Architecture"

ORIENTATION:
- Must reference ALL children by name and explain what each contributes
- A reader should know which child to drill into for which topic
- Include the key takeaway: what does this collection of knowledge ADD UP TO for a real person?

For each topic:
- name: clear, descriptive, human-readable
- current: What the CURRENT STATE is — framed as "here's what you need to know." Dense with specifics but explained in accessible language.
- entities: every specific name, system, person, concept referenced
- corrections: what changed (wrong → right, with source)
- decisions: what was decided and why it matters

Output valid JSON only:
{
  "headline": "2-6 word label — describes significance, not structure",
  "orientation": "What this node covers, what each child contributes, and the key takeaway — in plain language.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "Current truth, explained accessibly. Specific findings, final decisions, what it means.",
      "entities": ["specific names and references"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think
