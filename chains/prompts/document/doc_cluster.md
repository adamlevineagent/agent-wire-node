You are organizing a collection of documents into concept threads. Each thread groups documents that tell a coherent story about the same subject — a reader exploring that thread should come away understanding one complete area of knowledge.

You have projected metadata from every document: `node_id`, `headline`, `orientation`, and `topics` (with topic names and details). Use the headline, orientation, and topic structure to understand what each document is about and group accordingly.

PURPOSE: A reader will explore this collection one thread at a time. Each thread becomes a synthesis that traces how understanding of a concept evolved across documents. Your groupings determine what stories get told.

PRINCIPLES:
- **Group by concept, not by type.** A design doc, an audit, and a bugfix about the same subject belong together — they tell the complete story of that subject.
- **Let the material decide the shape.** Some collections have 3 natural concept areas. Others have 20. Don't force documents into too few or too many groups — follow the natural boundaries in the material.
- **Not everything belongs.** If a document is genuinely tangential (a changelog, an index file, boilerplate), leave it unassigned. Forcing irrelevant documents into threads pollutes the synthesis.
- **Name threads by what they're ABOUT.** "Auth & Token Design" not "Design Documents." Be specific enough that a reader can scan thread names and know exactly where to look.
- **Keep threads focused.** If a concept area has many documents, consider whether it's actually multiple related concepts that deserve their own threads. Prefer splitting into meaningful sub-threads over stuffing one thread with 20+ docs.
- **NO catch-all threads:** do NOT create threads like "Miscellaneous", "Other", or "General". Every document belongs to a real concept area. Small tangential docs go with the system they relate to most.
- **ZERO ORPHANS:** Every single source_node in the input MUST appear in at least one thread assignment OR in the unassigned list. Missing documents are a critical failure.

CRITICAL: `assignments[].source_node` MUST be the exact `D-L0-XXX` ID, NOT the headline.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers and how the documents relate",
      "concept_tags": ["wire-auth", "identity"],
      "assignments": [
        {"source_node": "D-L0-000", "topic_index": 0, "topic_name": "Headline"},
        {"source_node": "D-L0-007", "topic_index": 7, "topic_name": "Headline"}
      ]
    }
  ],
  "unassigned": ["D-L0-042", "D-L0-099"]
}

/no_think
