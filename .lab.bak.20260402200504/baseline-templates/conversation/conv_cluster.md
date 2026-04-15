You are given the compressed L0 nodes from a conversation. Each node represents a chunk of the conversation with its key topics, decisions, corrections, and dead ends.

Your job: identify 6-14 coherent THREADS that organize ALL topics by SUBJECT. A thread is a topic strand that weaves through the conversation — "Auth Architecture" is a thread, "Credit Economy Design" is a thread.

RULES:
- Group by TOPIC, not by position. A subject discussed in chunks 3, 7, and 12 is ONE thread.
- Topics about the same subject from different chunks belong in the SAME thread
- Corrections chains MUST stay together: if chunk 3 proposes X and chunk 7 corrects to Y, both go in the same thread
- Dead ends go with the thread where they were rejected (not a separate "dead ends" thread)
- Fuzzy-match entities: "helpers" and "helper agents" and "9B helpers" are the same thing
- 6-14 threads total. Fewer is better if coverage is complete.
- NO catch-all threads: "Other" or "Miscellaneous" is not allowed
- ZERO ORPHANS: every L0 node must appear in at least one assignment
- A single L0 node MAY appear in up to 3 threads if it genuinely covers multiple topics

CRITICAL: `assignments[].source_node` MUST be the exact `L0-XXX` ID, NOT the headline.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — topic-based",
      "description": "1-2 sentences: what subject this thread covers",
      "assignments": [
        {"source_node": "L0-000", "topic_index": 0, "topic_name": "Original Headline"},
        {"source_node": "L0-007", "topic_index": 7, "topic_name": "Original Headline"}
      ]
    }
  ]
}

/no_think
