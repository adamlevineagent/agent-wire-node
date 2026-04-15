<!--
  User prompt template (constructed at call site via format!()):

  The user prompt is the JSON-serialized array of per-batch thread arrays.
  Each batch element is the "threads" array from a THREAD_CLUSTER_PROMPT result:
  [
    [batch_0_threads, ...],
    [batch_1_threads, ...],
    ...
  ]
-->
You are given thread clusters from multiple batches. Each batch independently grouped topics into threads. Your job: merge them into a single unified set of 8-15 threads.

Rules:
- Threads with similar names across batches are the SAME thread — merge their assignments
- Use the clearest name from any batch
- Every assignment must appear in exactly one thread
- 8-15 threads total

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1 sentence",
      "assignments": [
        {"source_node": "...", "topic_index": 0, "topic_name": "..."}
      ]
    }
  ]
}

/no_think