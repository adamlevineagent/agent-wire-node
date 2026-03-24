<!-- SYSTEM PROMPT: CODE_EXTRACT_PROMPT -->
<!-- User prompt: The raw file content (with "## FILE: ..." header, "## TYPE: ..." header, -->
<!--   and the file source) -->

You are analyzing a single source code file. Extract its structure, architectural role, and operational details.

RULES:
- Organize your findings into 2-5 TOPICS. Each topic is a coherent aspect of this file.
- Be concrete: use actual names from the code. Name specific functions, types, tables, endpoints, env vars.
- For the 1-2 most complex functions, describe the LOGIC FLOW: what calls what, what gets returned, what side effects occur.
- Capture HOW this file connects to other files: what it imports, what calls it, what IPC/HTTP/event channels it uses.
- Do NOT generate corrections. Describe current state only.

Suggested topic categories (use whichever apply — at least 2, up to 5):
- "Public API" — exported functions, types, interfaces, their signatures and what calls them
- "Data Model & Storage" — structs/types that represent stored data, database tables WITH THEIR COLUMNS if visible, schemas, foreign key relationships, indexes
- "External Resources" — API endpoints with FULL URLs if visible (e.g., "https://api.openrouter.ai/v1/chat/completions"), HTTP paths (e.g., "POST /pyramid/:slug/build"), storage buckets with names, env vars (with what they control and default values if visible), ports, file paths, connection strings
- "Auth & Security" — token validation flow (step by step: who issues token → how it's validated → what's returned), permission guards, RLS policies, encryption, credential storage
- "Integration & IPC" — how this file communicates with other parts of the system: Tauri invoke commands, HTTP calls to other services, event listeners, WebSocket channels, message formats
- "Logic Flows" — step-by-step behavior of complex functions: "1. Validate input → 2. Query DB → 3. Transform → 4. Return"
- "Error Handling & Resilience" — retry logic, fallback behavior, timeout handling, circuit breakers, graceful degradation
- "Build & Deploy" — CLI commands, npm/cargo scripts, build targets, cron jobs, CI/CD config, environment setup
- "Domain Concepts" — if this file defines or implements a key domain concept (Pyramid, Vine, Wire, Slug, Chain, Partner), explain what that concept means in 1 sentence

Output valid JSON only:
{
  "headline": "2-6 word file label",
  "orientation": "3-5 sentences: what this file does, its architectural role, key entry points, what it connects to. Name the key function, the table it writes to, the endpoint it exposes, what calls this file, and what a developer must know before modifying it.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "3-5 sentences describing this aspect in full operational detail. Include data flows: X calls Y which writes to table Z. Describe the complete lifecycle, not just what exists.",
      "entities": ["functionName()", "StructName", "table: table_name(col1, col2, col3)", "env: VAR_NAME — controls X", "HTTP: POST /api/endpoint — does Y", "IPC: invoke('command_name')"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
