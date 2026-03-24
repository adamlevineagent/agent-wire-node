You are reading N sibling nodes from a knowledge pyramid of a codebase. Each node describes a subsystem. Organize everything into coherent TOPICS for the parent node.

A topic groups all related components, APIs, data models, and integration patterns. Everything we know about that subject belongs in that bundle.

Your job: what are the 3-6 coherent topics that organize everything here? A developer should scan your topic names and immediately know which one to drill into.

Merge topics that cover the same system. If multiple siblings discuss overlapping functionality, that is ONE topic.

CRITICAL — PRESERVE CONCRETE DETAILS through every layer:
- Actual table names with columns: "pyramid_nodes(slug, id, depth, distilled, topics, children)" not "database tables"
- Actual endpoints with methods: "POST /pyramid/:slug/build triggers PyramidBuilder.run_pipeline()" not "REST API"
- Actual auth mechanisms end-to-end: "Bearer token → validate_token() → sessions table → UserSession{role}" not "authentication"
- Actual env vars with purpose: "env: OPENROUTER_API_KEY — LLM routing" not "configuration"
- Actual IPC commands: "invoke('getDashboardData') → get_dashboard_data()" not "IPC bridge"
- Actual data flows: "Build request → chain_executor → dispatch per step → LLM call → parse JSON → write node" not "processing pipeline"
- Actual error handling: "retry(3) with temp 0.1 on JSON parse failure, carry-left on final fail" not "error recovery"

The orientation should explain HOW these subsystems connect:
- What data flows between them?
- What calls what?
- Where does a request enter and where does the result end up?

For each topic:
- name: descriptive (e.g., "Pyramid Build Pipeline & Chain Execution", not "Build")
- current: 2-3 sentences describing what this system IS with technologies, patterns, key components, AND operational details (tables, endpoints, env vars, data flows, error handling)
- entities: PRESERVE ALL named entities from children. Do not summarize "12 functions" — list all 12. These are what developers search for.
- headline: 2-6 word label. Concrete and specific.

Output valid JSON only:
{
  "headline": "2-6 word node label",
  "orientation": "2-4 sentences: what this node covers, how subsystems connect, which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this system IS. Technologies, patterns, key components, tables with columns, endpoints with methods, auth flows, data flows, error handling.",
      "entities": ["SpecificType", "specific_function()", "table: specific_table(col1, col2)", "env: SPECIFIC_VAR — does X", "HTTP: POST /specific/endpoint", "IPC: invoke('specific_command')"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
