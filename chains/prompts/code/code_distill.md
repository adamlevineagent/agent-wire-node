You are reading N sibling nodes from a knowledge pyramid of a codebase. Each node describes a subsystem. Create a PARENT NODE that is richer and more useful than any individual child.

THE KEY PRINCIPLE: Each layer UP should be DENSER and more complete, not thinner. A parent node should contain everything a developer needs to understand the combined domain — not a thin index pointing to children. A developer who reads ONLY this node should walk away with real understanding, not just a table of contents.

INFORMATION DENSITY BY LAYER:
- APEX (merging everything, 2-4 children): Write the DEFINITIVE project briefing. 15-20 sentence orientation. 4-6 topics, each with 6-10 sentence "current" fields. A new developer reads ONLY this node and understands 60% of the system — architecture, data model, auth model, key APIs, build process, and how subsystems connect.
- L2/L3 (merging domain clusters, 2-4 children): Write a complete domain briefing. 10-15 sentence orientation. 3-6 topics, each with 5-8 sentence "current" fields. A developer reads this and can START WORKING in this area.
- Default (any other merge): 6-10 sentence orientation, 3-6 topics with 4-6 sentences each.

HEADLINE RULES:
- The headline must be DIFFERENT from any child headline. Never repeat "Pyramid Engine" if a child already uses it.
- Make it specific to THIS level of abstraction. If children are "Tauri Frontend Core" and "Dashboard UI", the parent might be "Desktop Application Stack".
- APEX headline MUST name the project and its purpose: "Wire Node: Knowledge Pyramid Desktop Platform" not "System Overview".

ORIENTATION — write like a senior architect's briefing document, not a summary:
- APEX: First sentence answers "What is this project and what problem does it solve?" Then: architecture, data flows between subsystems, auth model, key tables, key endpoints, build/deploy process, and what to explore first. Be exhaustive.
- Non-apex: What does this domain cover? How do the subsystems in it connect? What data flows between them? What tables/endpoints/env vars are shared? What are the gotchas?

CRITICAL — PRESERVE AND AMPLIFY CONCRETE DETAILS:
At every layer, include actual names. Higher layers should have MORE detail aggregated, not less:
- Table names with columns: "pyramid_nodes(slug, id, depth, distilled, topics, children, parent_id)" not "database tables"
- Endpoints with methods: "POST /pyramid/:slug/build triggers PyramidBuilder.run_pipeline()" not "REST API"
- Auth flows end-to-end: "Bearer token from localStorage → Authorization header → validate_token() queries sessions(token, expires_at) → returns UserSession{userId, role, permissions}" not "authentication"
- Env vars with purpose: "OPENROUTER_API_KEY — routes LLM calls through OpenRouter to model specified in LlmConfig" not "configuration"
- IPC commands with both sides: "invoke('getDashboardData') → Rust get_dashboard_data() → queries pyramid_slugs, returns Vec<SlugInfo>" not "IPC bridge"
- Data flows traced end-to-end: "Build: POST /pyramid/:slug/build → chain_executor loads code.yaml → forEach chunks dispatches to mercury-2 → parse JSON response → write pyramid_nodes row → thread_clustering via qwen → synthesize L1 threads → recursive_cluster to apex" not "processing pipeline"
- Error handling with specifics: "LLM dispatch: retry(2) at temp 0.1 on JSON parse failure → carry-left if all retries fail → logged as failure in BuildStatus.failures counter" not "error recovery"

For each topic:
- name: descriptive (e.g., "Pyramid Build Pipeline & Chain Execution", not "Build")
- current: Dense paragraphs. Include technologies, patterns, key components, AND operational details (tables, endpoints, env vars, data flows, error handling). The goal is that reading this topic tells you 80% of what you need to know about this aspect WITHOUT drilling deeper.
- entities: PRESERVE ALL named entities from children. Do not summarize "12 functions" — list all 12. At higher layers, this list should GROW as you aggregate children, not shrink.

Output valid JSON only:
{
  "headline": "2-6 word node label — specific and unique",
  "orientation": "DENSE briefing. See layer-specific length guidance above. Cover architecture, data flows, auth, key tables, key endpoints, connections between subsystems, gotchas.",
  "topics": [
    {
      "name": "Descriptive Topic Name",
      "current": "DENSE paragraph. Technologies, patterns, components, tables with columns, endpoints with methods, auth flows, data flows, error handling. A developer reads this and understands this aspect.",
      "entities": ["EveryType", "every_function()", "table: every_table(col1, col2, col3)", "env: EVERY_VAR — does X (default: Y)", "HTTP: POST /every/endpoint — does Z", "IPC: invoke('every_command') → rust_handler()"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
