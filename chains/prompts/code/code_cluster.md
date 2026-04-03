You are given projected metadata from every source file in a codebase: `node_id`, `headline`, `orientation`, and `topics` (each with `name` and `summary`). Each item represents ONE FILE. Use the headline, orientation, and topic names/summaries to understand what each file does and group accordingly. Group FILES into threads — do not split a file's topics across different threads.

Your job: identify coherent THREADS that organize ALL these files into meaningful groups. A thread represents a subsystem, feature area, or architectural layer — something a developer would recognize as "the auth system", "the build pipeline", "the UI components", "the database layer", etc. Let the material decide how many threads — don't force a specific range.

The input may include `file_level_connections`: concrete cross-file links discovered from L0 webbing. Use those as strong evidence when deciding what belongs together, especially when files share tables, endpoints, IPC channels, or types.

RULES:
- Group files into coherent architectural subsystems (e.g., "Authentication System", "Build Pipeline", "UI Components", "Database Layer")
- A file should usually be assigned to the subsystem where it's most relevant. However, if a file genuinely sits on a structural seam and spans multiple subsystems (e.g., a router file, a massive orchestrator, or cross-cutting middleware), you MAY assign it to multiple threads. Do this when both subsystems require the file's context to be complete.
- Group by functional relatedness, not directory structure
- Files that import from each other or share types/APIs belong together
- Configuration files (e.g., package.json) and test files go with the system they support
- ZERO ORPHANS: Every single source_node from the input MUST appear in at least one thread assignment. Missing files are a critical failure.
- CRITICAL FIELD RULE: `assignments[].source_node` MUST be the exact `C-L0-XXX` ID from the input. Use `topic_name` for the human-readable headline.
- Put the human-readable file/topic title in `topic_name`, not `source_node`.
- BAD: `{"source_node":"MCP Server Package Config","topic_index":0,"topic_name":"MCP Server Package Config"}`
- GOOD: `{"source_node":"C-L0-000","topic_index":0,"topic_name":"MCP Server Package Config"}`

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1-2 sentences: what this subsystem/feature does",
      "assignments": [
        {"source_node": "C-L0-000", "topic_index": 0, "topic_name": "Original Headline"},
        {"source_node": "C-L0-005", "topic_index": 5, "topic_name": "Original Headline"}
      ]
    }
  ]
}

/no_think
