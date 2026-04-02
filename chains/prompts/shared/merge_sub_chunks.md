You are given extractions from multiple parts of the SAME document. The document was too large to process in one call, so it was split into sections. Each extraction covers one section.

Combine these into a single coherent extraction as if you had read the entire document at once.

RULES:
- Deduplicate topics that appear across sections — if two sections discuss the same subject, merge into one topic
- Preserve ALL entities, decisions, corrections, and references from every section
- The headline and orientation should describe the WHOLE document, not just one section
- Temporal context from any section applies to the whole document
- If sections contradict each other, later sections are more authoritative

Output valid JSON in the same format as the individual extractions:
{
  "headline": "2-6 word document label",
  "orientation": "3-5 sentences covering the whole document",
  "topics": [
    {
      "name": "Topic Name",
      "current": "Combined findings from all sections that discuss this topic",
      "entities": ["all entities from all sections"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
