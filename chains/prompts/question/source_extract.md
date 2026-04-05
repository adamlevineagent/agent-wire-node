You are distilling a single source into a reference card. Not summarizing — distilling. Keep what someone MUST understand, discard what they don't.

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE. A few hundred words total. If your extraction approaches the length of the original source, you are rewriting it, not distilling it. Stop and cut.

TOPICS ARE DIMENSIONS OF UNDERSTANDING, NOT SECTIONS.
- If the source has 5 examples illustrating one concept, that is ONE topic (the concept), not five.
- If three sections describe one system from different angles, that is ONE topic (the system).
- Ask: "Would removing this topic leave a gap in understanding?" If no, it doesn't deserve to be a topic.

HOW TO DISTILL:
1. Read the whole source. What does it DO or SAY? Not describe — DO or SAY.
2. What are the key CAPABILITIES, DECISIONS, MECHANISMS, or FINDINGS?
3. Group into the natural dimensions of understanding. Most sources have 2-4.
4. Write each topic as a dense sentence or two. Specific names, terms, identifiers. No filler.

RULES:
- Be concrete: actual names, terms, references from the source
- Preserve temporal context where present: when written, what state things were in
- Do NOT editorialize
- Topic names are used for clustering — name concepts specifically. "Spatial Canvas Renderer" not "Rendering."
- The `summary` field is a single-sentence distillation used when even the `current` field can't fit downstream. Make it count.
- Entities: cross-references to other sources, systems, people, decisions

Output valid JSON only:
{
  "headline": "2-6 word source label",
  "orientation": "2-3 sentences. What this source is, what it does or concludes, what to take away.",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "One sentence: the key point of this topic.",
      "current": "One to three sentences. The specific capability, decision, or finding. Names, identifiers, specifics.",
      "entities": ["system: Pyramid Engine", "component: CanvasRenderer", "decision: switched from REST to IPC"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
