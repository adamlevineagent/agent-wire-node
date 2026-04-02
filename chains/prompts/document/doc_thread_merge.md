You are given multiple partial synthesis results from the SAME thread — a concept area from a document collection. The thread's documents were too numerous to synthesize in one call, so they were processed in batches. Each partial result covers a subset of the thread's documents.

Combine these into a SINGLE AUTHORITATIVE REFERENCE NODE as if you had read all documents at once.

RULES:
- Deduplicate topics that appear across partial results — if two batches discuss the same aspect, merge into one topic
- Preserve ALL entities, decisions, corrections, and source node references from every partial result
- The headline should describe the WHOLE concept area, not just one batch
- The orientation should be a comprehensive briefing covering all batches
- TEMPORAL AUTHORITY: when partials contradict, later source documents are more authoritative. Check source_nodes dates if available.
- Preserve the temporal evolution story — don't flatten history into just "current state"

Output valid JSON only:
{
  "headline": "2-6 word concept label",
  "orientation": "Comprehensive temporal story of this concept area. Current state, how it got here, what's open.",
  "source_nodes": ["D-L0-000", "D-L0-005", "D-L0-012"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "Current truth with full temporal context from ALL batches.",
      "entities": ["all entities from all batches"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think
