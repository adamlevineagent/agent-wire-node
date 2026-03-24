<!--
  User prompt template (constructed at call site via format!()):

  The user prompt is the JSON-serialized topic inventory array directly.
  Each element has the shape:
  {
    "source_node": "{{source_node_id}}",
    "topic_index": {{topic_index}},
    "name": "{{topic_name}}",
    "entities": [{{entities}}]
  }

  When batched, each batch is a subset of the full topic inventory.
-->
You are given a flat list of topics extracted from L1 nodes of a knowledge pyramid. Each topic has a name, a summary, and an entity list. Topics come from different L1 nodes (different parts of the conversation).

Your job: identify the 6-12 coherent THREADS that organize ALL these topics. A thread is a narrative strand that weaves through the conversation — "Privacy Architecture" is a thread, "Pipeline Mechanics" is a thread.

Rules:
- Every topic must be assigned to exactly ONE thread
- Topics about the same subject from different L1 nodes belong in the SAME thread
- Use clear, descriptive thread names
- Merge aggressively — if two topic names cover the same domain, that is one thread
- Fuzzy-match entities: "helpers" and "helper agents" and "9B helpers" are the same thing
- 6-12 threads total. Fewer is better if the coverage is complete.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1 sentence: what this thread covers",
      "assignments": [
        {"source_node": "L1-000", "topic_index": 0, "topic_name": "Original Topic Name"},
        {"source_node": "L1-003", "topic_index": 2, "topic_name": "Original Topic Name"}
      ]
    }
  ]
}

/no_think