You are given extraction results from every document in a collection, PLUS a pre-classification that tags each document with:
- **temporal**: when it was written
- **conceptual**: what subject(s) it covers (normalized tags)
- **canonical**: whether it's the authoritative source on its subject
- **type**: design doc, audit, implementation plan, worksheet, etc.

Your job: organize ALL documents into 6-14 coherent THREADS. Each thread represents a CONCEPT AREA — a subject that a reader would naturally explore as a unit.

CLUSTERING RULES:
- **Primary axis is CONCEPTUAL**: documents about the same subject cluster together, regardless of type or date
- **Multiple types enrich a thread**: a design doc + audit + bugfix about auth ALL belong in the "Auth & Identity" thread — they tell the complete story
- **Temporal ordering within threads**: list assignments in chronological order (earliest first). This order determines how the synthesis reads the evolution of understanding.
- **Canonical status matters**: mark which document in each thread is the current authority. Later canonical docs supersede earlier ones on the same subject.
- **Max 15 documents per thread**. If a concept area has more, split by sub-concept.
- **No catch-all threads**: every document has a real subject. "Miscellaneous" is not allowed.
- **Zero orphans**: every source_node must appear in at least one assignment.

THREAD NAMING:
- Name threads by CONCEPT, not by type: "Auth & Identity Design" not "Design Documents"
- Be specific: "Wire Credit Economy" not "Economy"

CRITICAL: `assignments[].source_node` MUST be the exact `D-L0-XXX` ID, NOT the headline.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers and how the documents relate",
      "concept_tags": ["wire-auth", "identity"],
      "assignments": [
        {"source_node": "D-L0-000", "topic_index": 0, "topic_name": "Headline", "doc_type": "design", "date": "2026-02-10", "canonical": "foundational"},
        {"source_node": "D-L0-007", "topic_index": 7, "topic_name": "Headline", "doc_type": "audit", "date": "2026-02-25", "canonical": "canonical"}
      ]
    }
  ]
}

/no_think
