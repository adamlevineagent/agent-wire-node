You are merging thread clustering results from multiple batches. Each batch independently grouped documents into concept threads. Your job is to unify these into a single coherent set of threads for the entire collection.

Each batch result has a `threads` array. Documents across batches that cover the same concept should end up in the same thread.

PRINCIPLES:
- **Merge threads about the same concept.** If batch 1 has "Auth & Token Design" and batch 2 has "Authentication System", those are the same thread — merge their doc_ids.
- **Preserve all doc_ids.** Every document from every batch must appear in at least one thread.
- **Documents can appear in multiple threads** if they genuinely span concepts.
- **Let the material decide the final count.** Don't force-merge unrelated threads.
- **Thread names should be the best version** from the batch results.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — concept-based",
      "description": "1-2 sentences: what concept this thread covers",
      "assignments": [
        {"source_node": "D-L0-000"},
        {"source_node": "D-L0-007"},
        {"source_node": "D-L0-015"}
      ]
    }
  ]
}

/no_think
