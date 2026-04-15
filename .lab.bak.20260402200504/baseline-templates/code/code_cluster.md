You are given the extraction results from every source file in a codebase. Each entry has a headline, purpose, exports, key types, key functions, external resources, and other metadata.
Each topic entry also carries the EXACT L0 node ID in `node_id` / `source_node` (for example `C-L0-000`).

Your job: identify 10-18 coherent THREADS that organize ALL these files into meaningful groups. A thread represents a subsystem, feature area, or architectural layer — something a developer would recognize as "the auth system", "the build pipeline", "the UI components", "the database layer", etc.

The input may include `file_level_connections`: concrete cross-file links discovered from L0 webbing. Use those as strong evidence when deciding what belongs together, especially when files share tables, endpoints, IPC channels, or types.

RULES:
- Most files should be assigned to ONE thread — the one where they are MOST relevant
- Files that genuinely span multiple subsystems (e.g., routes.rs defines auth + API + IPC; mod.rs re-exports from multiple domains; AppShell.tsx manages auth state + routing + layout) may be assigned to up to 3 threads. Use this sparingly — only when the file's TOPICS are genuinely split across domains, not just because it imports from multiple modules
- Group by functional relatedness, not directory structure
- Files that import from each other or share types/APIs belong together
- Configuration files (package.json, tsconfig, Cargo.toml) go with the system they configure
- Test files go with the module they test
- 10-18 threads total. More threads = better granularity. Prefer splitting over merging.
- MAX 12 FILES PER THREAD. If a subsystem has more than 12 files, split it into meaningful sub-threads (e.g., "Pyramid Build Pipeline" + "Pyramid Query & Persistence" instead of one giant "Pyramid Engine" thread). This is a hard limit — threads over 12 files produce truncated synthesis.
- NO catch-all threads: do NOT create threads like "Utilities", "Miscellaneous", or "Other". Every file belongs to a real subsystem. Small helper files go with the system they support.
- Thread names should be concrete and recognizable: "Chain Execution Engine", not "Module Group 3"
- Balance: threads should have roughly 4-12 files each. Very small threads (1-2 files) should be merged into related threads.
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
