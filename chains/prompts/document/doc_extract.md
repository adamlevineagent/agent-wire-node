You are analyzing a single document. It could be a technical design doc, audit report, meeting notes, bug report, creative writing, research paper, specification, or any other document type.

First, identify what KIND of document this is, then extract its structure accordingly.

Organize your findings into 2-5 TOPICS. Each topic is a coherent aspect of this document.

RULES:
- Be concrete: use actual names, terms, and references from the document
- Capture the document's KEY CLAIMS, DECISIONS, and CONCLUSIONS — not just what it discusses, but what it SAYS
- Note all references to other documents, systems, people, or external resources
- Preserve temporal context: when was this written? What state was the project in?
- Do NOT editorialize. Describe what the document states, not what you think about it.

Suggested topic categories (use whichever apply — at least 2, up to 5):
- "Key Findings" — main conclusions, discoveries, results, audit outcomes
- "Decisions & Rationale" — what was decided and why, alternatives considered, trade-offs made
- "Technical Details" — specific implementations, configurations, architectures, schemas, algorithms described
- "Action Items & Status" — tasks assigned, bugs found, fixes applied, what's still open
- "People & Context" — who authored this, who's mentioned, what project/phase this belongs to, timeline
- "References & Dependencies" — other documents, systems, APIs, tools mentioned or depended upon
- "Open Questions" — unresolved issues, unknowns, things deferred for later

Output valid JSON only:
{
  "headline": "2-6 word document label",
  "orientation": "3-5 sentences: what this document is, when it was written (if visible), what project/system it's about, what its main conclusion or purpose is, and what a reader should take away from it.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "3-5 sentences describing this aspect. Include specific findings, numbers, names, dates. Quote key conclusions directly. Describe what was decided, not just that a decision was made.",
      "entities": ["person: Alice", "system: Pyramid Engine", "decision: switched from REST to IPC", "bug: FK constraint on pyramid_nodes", "doc: design-spec-v2.md"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
