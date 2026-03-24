You are given the extraction results from every source file in a codebase. Each entry has a headline, purpose, exports, key types, key functions, external resources, and other metadata.

Your job: identify 8-14 coherent THREADS that organize ALL these files into meaningful groups. A thread represents a subsystem, feature area, or architectural layer — something a developer would recognize as "the auth system", "the build pipeline", "the UI components", "the database layer", etc.

RULES:
- Every file must be assigned to exactly ONE thread
- Group by functional relatedness, not directory structure
- Files that import from each other or share types/APIs belong together
- Configuration files (package.json, tsconfig, Cargo.toml) go with the system they configure
- Test files go with the module they test
- 8-14 threads total.
- SPLIT large systems: if a thread would contain more than 20 files, break it into meaningful sub-threads (e.g., "Pyramid Build Pipeline" + "Pyramid Query & Persistence" instead of one giant "Pyramid Engine" thread)
- NO catch-all threads: do NOT create threads like "Utilities", "Miscellaneous", or "Other". Every file belongs to a real subsystem. Small helper files go with the system they support.
- Thread names should be concrete and recognizable: "Chain Execution Engine", not "Module Group 3"
- Balance: threads should have roughly 5-20 files each. Very small threads (1-2 files) should be merged into related threads.

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