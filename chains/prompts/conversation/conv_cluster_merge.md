You are merging thread clustering results from multiple batches of a conversation. Each batch independently grouped its L0 nodes into topic threads. Your job is to unify these into a single coherent set of threads for the entire conversation.

You receive an array of batch results. Each batch result has a `threads` array. Topics across batches that belong to the same subject should end up in the same thread.

PRINCIPLES:
- **Merge threads about the same subject.** If batch 1 has "Auth Architecture" and batch 2 has "Authentication Design Decisions", those are the same thread — merge them. Use the most precise name.
- **ZERO ORPHANS: Every single L0-XXX from every batch result must appear in exactly one thread assignment.** No node may be left out. There is no `unassigned` escape hatch — every topic belongs to a real thread. If a node seems tangential, it goes with the thread it relates to most.
- **Let the material decide the final count.** Don't force-merge unrelated subjects just to reduce count. If 8 distinct topic strands exist, output 8 threads.
- **Thread names should be concrete and topic-based:** "Credit Economy Design", "Auth & Session Architecture", "Deployment Pipeline", not "Group 1" or "Miscellaneous".
- **Keep threads focused.** If merging creates a very large thread (10+ assignments), consider whether it's actually two related subjects that deserve separate threads.
- **Preserve correction chains.** If batch 1 assigns a proposal to thread X and batch 2 assigns the correction of that proposal, both must end up in the same merged thread.
- **A single L0 node MAY appear in up to 3 threads** if it genuinely covers multiple topics — preserve these multi-assignments from the batch results.
- **CRITICAL: `assignments[].source_node` MUST be the exact `L0-XXX` ID copied verbatim from the batch results. Do NOT use the headline in this field.**

After generating your output, verify: does every L0-XXX ID from every batch result appear in at least one thread? If any are missing, add them now.

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
