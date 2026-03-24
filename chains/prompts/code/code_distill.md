You read two sibling nodes describing parts of a codebase. Organize everything they contain into coherent TOPICS.

A topic is a named system, module, or capability that groups together all related components, APIs, data models, and architectural patterns. Everything we know about that subject belongs in that bundle.

Your job is to understand both siblings and decide: what are the 3-6 coherent topics that organize everything here? A developer should scan your topic names and immediately know which topic to drill into for what they need.

Merge topics that cover the same system. If both siblings discuss overlapping functionality, that is ONE topic, not two.

For each topic:
- name: a clear, descriptive name (e.g., "Authentication & Session Management", not "Auth")
- current: 2-3 sentences describing what this system IS — be specific about technologies, patterns, key components, AND operational details (tables, endpoints, env vars, ports, auth flows)
- entities: the specific named types, functions, files, APIs, tables, endpoints, env vars, and CLI commands in this topic. PRESERVE ALL of them — do not summarize "5 functions" into a count. List the actual names.
- corrections: leave empty (code nodes have no temporal corrections)
- decisions: leave empty (code nodes have no decisions)
- headline: a 2-6 word label for the parent node. Concrete and specific. No "This node..."

CRITICAL: The pyramid's value comes from preserving concrete details through every layer. A developer reading this node should find:
- Actual table names (pyramid_nodes, pyramid_chunks) not "database tables"
- Actual endpoints (/pyramid/:slug/build) not "REST API"
- Actual auth mechanisms (bearer token, Supabase Auth) not "authentication"
- Actual env vars (OPENROUTER_API_KEY) not "configuration"
- Actual type names (PyramidNode, ChainDefinition) not "data types"

Output valid JSON only:
{
  "headline": "2-6 word node label",
  "orientation": "1-2 sentences: what this node covers. Which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this system IS. Technologies, patterns, key components, tables, endpoints, auth.",
      "entities": ["SpecificType", "specific_function()", "table: specific_table", "env: SPECIFIC_VAR"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think