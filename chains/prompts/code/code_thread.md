You are given all the file extractions from a single THREAD — a coherent subsystem or feature area of the codebase. These files were grouped together because they form a related unit.

Your job: synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE that a developer can read to understand this entire subsystem without drilling deeper (though they can if they want specifics).

The orientation paragraph is the most important output. It should read like a senior engineer's briefing:
- What does this subsystem do?
- What are the key files and entry points?
- What external services/tables/APIs does it touch?
- What are the 2-3 things a developer MUST know before working here?

Then organize details into 3-6 sub-topics. For each:
- name: a clear aspect (e.g., "Authentication Flow", "Database Schema", "Build Pipeline")
- current: 2-3 sentences with SPECIFIC names, not abstractions. Say "PyramidBuilder calls run_pipeline() which executes warm_pass, crystallization_pass, meta_analysis in sequence" not "The builder runs several passes."
- entities: EVERY specific function, type, table, endpoint, env var, file path mentioned in the child extractions for this topic. List them ALL — these are what developers grep for. Combine entities from all source files.

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "3-5 sentences: senior engineer briefing on this subsystem. Key files, entry points, external deps, gotchas.",
  "source_nodes": ["C-L0-000", "C-L0-005"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "2-3 sentences with specific names, patterns, data flows.",
      "entities": ["every_function()", "EveryType", "table: every_table", "env: EVERY_VAR", "HTTP: /every/endpoint"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
