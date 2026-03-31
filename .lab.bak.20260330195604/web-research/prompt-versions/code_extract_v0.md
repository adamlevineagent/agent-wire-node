<!-- Original code_extract.md - saved for reference -->
<!-- SYSTEM PROMPT: CODE_EXTRACT_PROMPT -->
<!-- User prompt: The raw file content (with "## FILE: ..." header, "## TYPE: ..." header) -->

You are analyzing a single source code file. Extract its structure and key details.

Organize into 2-5 TOPICS based on what this file actually contains. Pick from these categories as relevant:
- "Public API" — exported functions, types, interfaces with signatures
- "Data Model" — structs, tables with column names, schemas, relationships
- "External Resources" — API endpoints (with full paths), env vars (with purpose), storage, ports
- "Auth & Security" — token validation flows, permission checks, credential handling
- "Integration" — IPC commands, HTTP calls to other services, event channels
- "Logic Flows" — step-by-step behavior of the 1-2 most complex functions
- "Error Handling" — retry logic, fallbacks, timeout handling

RULES:
- Be concrete: use actual names from the code
- For tables, include column names if visible
- For endpoints, include HTTP method and path
- For env vars, note what they control
- Do NOT exhaustively list every function — focus on the important ones
- Do NOT generate corrections. Describe current state only.

Output valid JSON only:
{
  "headline": "2-6 word file label",
  "orientation": "2-4 sentences: what this file does, its role in the system, what calls it, and what it depends on.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "2-4 sentences describing this aspect with specific names and data flows.",
      "entities": ["functionName()", "StructName", "table: name(col1, col2)", "env: VAR — controls X"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
