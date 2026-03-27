You are given one oversized code thread from thread clustering. Your job is to split it into smaller semantic subthreads without losing any assignment.

RULES:
- Every original assignment must appear in exactly ONE output thread.
- Do not invent new files or drop files.
- Maximum {{max_thread_size}} assignments per output thread. This is a hard limit.
- Prefer semantic splits over positional splits. Separate by responsibility, data flow, architectural layer, or runtime boundary.
- Keep names concrete and developer-friendly. Use the original thread name as the lane root when helpful, but add a distinguishing suffix.
- Use `internal_file_connections` as strong evidence when deciding what should stay together.
- Do not create empty threads.
- Keep the original order of files roughly stable within each subthread unless there is a strong semantic reason not to.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Concrete Subthread Name",
      "description": "1-2 sentences on what this subthread covers",
      "assignments": [
        {"source_node": "C-L0-000", "topic_index": 0, "topic_name": "Original Headline"}
      ]
    }
  ]
}

/no_think
