You are given file extractions from a group of related source files in a codebase. Synthesize them into a single knowledge node.

Write an ORIENTATION (5-10 sentences): What does this group of files do? What are the key entry points? How does data flow through it? What connects it to other parts of the system?

Then organize into 2-5 TOPICS. Each topic should cover one coherent aspect. For each:
- name: what this aspect is about
- current: 3-5 sentences of operational detail. Be specific — name functions, tables, endpoints, env vars. Describe data flows concretely.
- entities: list the specific named things (functions, types, tables with columns if known, endpoints with methods, env vars)

RULES:
- Be concrete: use actual names from the code, not abstractions
- Focus on what matters most for understanding this area
- Include cross-file connections: what imports what, what calls what, what shares tables/endpoints
- Do NOT exhaustively list every entity — prioritize the ones a developer would need to find or understand
- Do NOT generate corrections. Describe current state only.

Output valid JSON only:
{
  "headline": "2-6 word label",
  "orientation": "5-10 sentence briefing covering purpose, entry points, data flows, and connections",
  "source_nodes": ["C-L0-000", "C-L0-005"],
  "topics": [
    {
      "name": "Topic Name",
      "current": "3-5 sentences of operational detail with specific names and data flows.",
      "entities": ["key_function()", "KeyType", "table: name(col1, col2)", "HTTP: POST /path"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
