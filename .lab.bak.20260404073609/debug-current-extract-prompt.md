You are distilling a single document into a reference card. Not summarizing — distilling. Keep what someone MUST understand, discard what they don't.

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE. A few hundred words total. If your extraction approaches the length of the original document, you are rewriting it, not distilling it. Stop and cut.

TOPICS ARE DIMENSIONS OF UNDERSTANDING, NOT SECTIONS.
- If the document has 5 examples illustrating one thesis, that is ONE topic (the thesis), not five.
- If three sections describe one system from different angles, that is ONE topic (the system).
- Ask: "Would removing this topic leave a gap in understanding?" If no, it doesn't deserve to be a topic.

HOW TO DISTILL:
1. Read the whole document. What does it SAY? Not discuss — SAY.
2. What are the key CLAIMS, DECISIONS, and FINDINGS?
3. Group into the natural dimensions of understanding. Most documents have 2-4.
4. Write each topic as a dense sentence or two. Specific names, numbers, dates, decisions. No filler.

RULES:
- Be concrete: actual names, terms, references from the document
- Preserve temporal context: when written, what state the project was in
- Do NOT editorialize
- Topic names are used for clustering — name concepts specifically. "Configurable Pipeline Template" not "Pipeline."
- The `summary` field is a single-sentence distillation used when even the `current` field can't fit downstream. Make it count.
- Entities: cross-references to documents, systems, people, decisions

Output valid JSON only:
{
  "headline": "2-6 word document label",
  "orientation": "2-3 sentences. What this document is, what it concludes, what to take away.",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "One sentence: the key point of this topic.",
      "current": "One to three sentences. The specific claim, decision, or finding. Names, numbers, dates.",
      "entities": ["person: Alice", "system: Pyramid Engine", "decision: switched from REST to IPC"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
