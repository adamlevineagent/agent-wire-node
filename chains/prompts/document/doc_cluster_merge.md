You are merging thread clustering results from multiple batches of documents. Each batch independently grouped its documents into concept threads. Your job is to unify these into a single coherent set of threads for the entire collection.

You receive an array of batch results. Each batch result has a `threads` array and possibly an `unassigned` array. Documents across batches that cover the same concept should end up in the same thread.

PRINCIPLES:
- **Merge threads about the same concept.** If batch 1 has "Auth & Token Design" and batch 2 has "Authentication System", those are the same thread — merge them.
- **Preserve all assignments.** Every document from every batch must appear in exactly one thread or in unassigned. Do not drop documents.
- **Let the material decide the final count.** Don't force-merge unrelated threads just to reduce count. If 15 distinct concept areas exist, output 15 threads.
- **Thread names should be the best version.** Pick the most descriptive name from the batch results, or write a better one that covers the merged scope.
- **Keep threads focused.** If merging creates a very large thread, consider whether it's actually multiple related concepts that deserve their own threads.
- **Concept tags are union.** Merge concept_tags from all matching batch threads.

CRITICAL: `assignments[].source_node` MUST be the exact `D-L0-XXX` ID from the batch results. Copy them verbatim.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers",
      "concept_tags": ["wire-auth", "identity"],
      "assignments": [
        {"source_node": "D-L0-000", "topic_index": 0, "topic_name": "Headline"},
        {"source_node": "D-L0-007", "topic_index": 7, "topic_name": "Headline"}
      ]
    }
  ],
  "unassigned": ["D-L0-042"]
}

/no_think
