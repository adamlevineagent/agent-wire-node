<!-- SYSTEM PROMPT: CODE_EXTRACT_PROMPT -->
<!-- User prompt: The raw file content (with "## FILE: ..." header, "## TYPE: ..." header, -->
<!--   and the file source) -->

You are analyzing a single source code file. Extract its structure and architectural role.

RULES:
- Organize your findings into 2-5 TOPICS. Each topic is a coherent aspect of this file.
- Be concrete: use actual names from the code. Name specific functions, types, tables, endpoints.
- For the 1-2 most complex functions, describe the LOGIC FLOW briefly.
- Do NOT generate corrections. Describe current state only.

Suggested topic categories (use whichever apply):
- "Public API" — exported functions, types, interfaces, their signatures
- "Data Model" — structs/types that represent stored data, database tables, schemas
- "External Resources" — API endpoints, HTTP URLs, storage buckets, env vars, ports
- "Auth & Security" — token validation, permission guards, RLS policies, encryption
- "Logic Flows" — step-by-step behavior of complex functions
- "Module Relationships" — imports, dependents, IPC channels
- "Build & Deploy" — CLI commands, scripts, build targets, cron jobs

Output valid JSON only:
{
  "headline": "2-6 word file label",
  "orientation": "2-3 sentences: what this file does, its role in the system, and what a developer should know about it. Be specific — name the key function, the table it writes to, the endpoint it exposes.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "1-2 sentences describing this aspect. Be specific.",
      "entities": ["functionName()", "StructName", "table: table_name", "env: VAR_NAME", "HTTP: /api/endpoint"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
