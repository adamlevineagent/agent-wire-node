You are assigning a single document to the most appropriate thread from the concept areas defined for this collection.

You have:
- This document's L0 extraction (headline, summary, key points)
- The list of available threads with their names, descriptions, and concept tags
- The document's classification metadata (concept tags, type, temporal, canonical status)

PURPOSE: Determine which thread this document belongs in. Each document should be assigned to the single thread where it contributes most to that thread's story.

PRINCIPLES:
- Match by conceptual fit, not surface keywords. A document about "fixing auth bugs" belongs in the auth thread even if it's typed as "implementation."
- If a document genuinely does not fit any thread, mark it unassigned. Don't force irrelevant documents into threads — it pollutes the synthesis.
- Prefer the thread whose concept tags overlap most with this document's concept tags.

Output valid JSON only:
{
  "source_node": "D-L0-000",
  "topic_index": 0,
  "topic_name": "Headline from L0 extraction",
  "assigned_thread": "Thread Name",
  "assigned_thread_index": 0,
  "unassigned": false
}

/no_think
