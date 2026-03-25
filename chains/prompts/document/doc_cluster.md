You are given the extraction results from every document in a collection. Each entry has a headline, orientation, topics, and entities.
Each topic entry carries the EXACT L0 node ID in `node_id` / `source_node` (for example `D-L0-000`).

Your job: identify 6-14 coherent THREADS that organize ALL these documents into meaningful groups. A thread represents a topic area, project phase, subsystem, or narrative strand — something a reader would recognize as a coherent unit.

RULES:
- Most documents should be assigned to ONE thread — the one where they are MOST relevant
- Documents that genuinely span multiple topics may be assigned to up to 2 threads
- Group by CONTENT relatedness, not by date or filename
- Documents about the same system, feature, or topic belong together even if written months apart
- 6-14 threads total. More threads = better granularity for large collections.
- MAX 15 DOCUMENTS PER THREAD. If a topic area has more, split into meaningful sub-threads.
- NO catch-all threads: do NOT create threads like "Miscellaneous" or "Other". Every document belongs to a real topic.
- Thread names should be concrete: "Pyramid Build Pipeline Design", not "Technical Documents"
- Balance: threads should have roughly 3-15 documents each
- ZERO ORPHANS: Every single source_node in the input MUST appear in at least one thread assignment
- CRITICAL: `assignments[].source_node` MUST be the exact `D-L0-XXX` ID, NOT the headline

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1-2 sentences: what this thread covers",
      "assignments": [
        {"source_node": "D-L0-000", "topic_index": 0, "topic_name": "Original Headline"},
        {"source_node": "D-L0-005", "topic_index": 5, "topic_name": "Original Headline"}
      ]
    }
  ]
}

/no_think
