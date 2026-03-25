You are given the opening section (first ~20 lines) of every document in a collection. For each document, classify it along four axes.

AXES:

1. **TEMPORAL** — When was this written? Extract any date, phase, sprint, or version indicator. If no explicit date, estimate relative ordering from context ("this references the v2 audit" = after the v2 audit). Output as ISO date if possible, or a relative marker.

2. **CONCEPTUAL** — What is this ABOUT? Tag with 1-3 subject areas using consistent terminology across all documents. If document A is about "auth" and document B is about "identity and login," those are the SAME concept — normalize to one term. Aim for 5-10 distinct concept tags across the entire collection.

3. **CANONICAL** — Is this the authoritative current source on its subject?
   - `canonical` — this is the latest/best source on this topic
   - `superseded` — a later document in this collection covers the same ground more recently
   - `partial` — canonical for some aspects, superseded for others
   - `foundational` — establishes baseline concepts that later docs build on (not superseded, but not the latest either)

4. **TYPE** — What kind of document is this?
   - `design` — architecture, specification, technical design, proposal
   - `audit` — review, assessment, analysis of existing system
   - `implementation` — handoff, plan, step-by-step build guide
   - `strategy` — vision, roadmap, business direction, positioning
   - `report` — bug report, test results, status update, findings
   - `reference` — API docs, schema definitions, configuration guide
   - `worksheet` — working document, notes, scratch, exploration
   - `meta` — process docs, retrospectives, meeting notes

RULES:
- Normalize concept tags: use the SAME tag for the same subject across all documents
- When two documents cover the same concept, the LATER one is more likely canonical
- A document can be canonical for one concept and superseded for another
- If you can't determine a date, use the filename or directory structure as a hint
- Be specific with concept tags: "wire-auth" not "security", "pyramid-build" not "technical"

Output valid JSON only:
{
  "documents": [
    {
      "source_node": "D-L0-000",
      "title": "Document title from header",
      "temporal": {"date": "2026-02-18", "confidence": "explicit"},
      "conceptual": ["wire-auth", "identity-system"],
      "canonical": "canonical",
      "type": "design",
      "supersedes": null,
      "superseded_by": null
    }
  ],
  "concept_taxonomy": {
    "wire-auth": "Authentication and identity system design",
    "pyramid-build": "Knowledge pyramid construction pipeline",
    "platform-economy": "Credit system, pricing, marketplace"
  }
}

/no_think
