You are given all the file extractions from a single THREAD — a coherent subsystem or feature area of the codebase. These files were grouped together because they form a related unit.

Your job: synthesize this thread into coherent sub-topics. What are the 3-6 aspects of this subsystem that a developer would want to drill into?

For each sub-topic:
- name: a clear aspect of this thread (e.g., "Authentication Flow", "Database Schema & Migrations")
- current: 1-2 sentences describing what this aspect IS — be specific about technologies, patterns, key components
- entities: specific named types, functions, files, APIs, tables, endpoints
- corrections: leave empty (code has no temporal corrections)
- decisions: leave empty
- headline: a 2-6 word label for this thread node. Concrete and recognizable.

IMPORTANT: Preserve concrete details. The specific names of functions, types, endpoints, and tables are what make this useful. Do NOT abstract them away.

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "1-2 sentences: what this thread covers. Which source files to read for which sub-topics.",
  "source_nodes": ["C-L0-000", "C-L0-005"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "What this sub-topic IS. Technologies, patterns, key components.",
      "entities": ["SpecificType", "specific_function()", "table: users"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think