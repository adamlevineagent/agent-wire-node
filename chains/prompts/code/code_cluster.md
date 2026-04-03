You are given projected metadata from every source file in a codebase: `node_id`, `headline`, `orientation`, and `topics` (each with `name` and `summary`). Each item represents ONE FILE. Use the headline, orientation, and topic names/summaries to understand what each file does and group accordingly. Group FILES into threads — do not split a file's topics across different threads.

Your job: identify coherent THREADS that organize ALL these files into meaningful groups. A thread represents a subsystem, feature area, or architectural layer — something a developer would recognize as "the auth system", "the build pipeline", "the UI components", "the database layer", etc. Let the material decide how many threads — don't force a specific range.

The input may include `file_level_connections`: concrete cross-file links discovered from L0 webbing. Use those as strong evidence when deciding what belongs together, especially when files share tables, endpoints, IPC channels, or types.

RULES:
- Most files should be assigned to ONE thread — the one where they are MOST relevant
- Files that genuinely span multiple subsystems (e.g., routes.rs defines auth + API + IPC; mod.rs re-exports from multiple domains; AppShell.tsx manages auth state + routing + layout) may be assigned to up to 3 threads. Use this sparingly — only when the file's TOPICS are genuinely split across domains, not just because it imports from multiple modules
- Group by functional relatedness, not directory structure
- Files that import from each other or share types/APIs belong together
- Configuration files (package.json, tsconfig, Cargo.toml) go with the system they configure
- Test files go with the module they test
- Let the material decide how many threads. Prefer splitting over merging.
- Keep threads focused. If a subsystem has many files, consider whether it's actually multiple related subsystems that deserve their own threads.
- NO catch-all threads: do NOT create threads like "Utilities", "Miscellaneous", or "Other". Every file belongs to a real subsystem. Small helper files go with the system they support.
- Thread names should be concrete and recognizable: "Chain Execution Engine", not "Module Group 3"
- Balance: very small threads (1-2 files) should usually be merged into the closest related thread unless they're genuinely distinct.
- ZERO ORPHANS: Every single source_node in the input MUST appear in at least one thread assignment. After generating your output, mentally verify: does every C-L0-XXX from the input appear in at least one assignment? If not, add it to the most relevant thread. Missing files are a critical failure.
- CRITICAL FIELD RULE: `assignments[].source_node` MUST be the exact `C-L0-XXX` ID copied verbatim from the input topic's `node_id` / `source_node` field. Do NOT put the headline in `source_node`.
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
