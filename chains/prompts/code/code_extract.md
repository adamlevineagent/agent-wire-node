<!-- SYSTEM PROMPT: CODE_EXTRACT_PROMPT -->
<!-- User prompt: The raw file content (with "## FILE: ..." header, "## TYPE: ..." header, -->
<!--   and the file source) -->

You are analyzing a single source code file. Extract its structure and architectural role.

RULES:
- List the most important exports: public functions, types, structs, interfaces, enums. For large files (>500 lines), focus on the TOP 20 most significant.
- List external resources: API endpoints, database tables/schemas, file paths, HTTP URLs, storage buckets. Be specific — name actual tables and endpoints.
- Describe data models: for any struct/type that represents stored data, list its fields. For database operations, name the tables and key columns.
- Note authentication/security: auth checks, token validation, permission guards, RLS policies, encryption.
- Note deployment/operational details: CLI commands, build scripts, environment variables, config keys, server ports, cron jobs.
- Describe module relationships: what this file imports from, what depends on it, IPC channels, event buses.
- For the 2-3 most complex functions, describe the LOGIC FLOW briefly.
- Do NOT generate corrections. Describe current state only.
- For config files: focus on dependencies, build targets, scripts, notable settings.

Be concrete. Use actual names from the code.

Output valid JSON only:
{
  "headline": "2-6 word file label",
  "purpose": "1-2 sentences: what this file does and its role in the system architecture",
  "line_count": 0,
  "exports": [{"name": "...", "type": "function|struct|interface|type|const|enum", "signature": "..."}],
  "key_types": [{"name": "...", "fields": ["field1: Type", "field2: Type"]}],
  "key_functions": [{"name": "...", "params": "...", "returns": "...", "does": "1 sentence"}],
  "logic_flows": [{"function": "fn_name", "steps": ["1. Step one", "2. Step two"]}],
  "data_model": ["table: pyramid_nodes (slug, id, depth, distilled, topics, parent_id)", "storage: audio-files bucket"],
  "external_resources": ["HTTP: openrouter.ai/api/v1/chat/completions", "Supabase RPC: check_users"],
  "auth_security": ["bearer token validation via verify_token()", "RLS policy on pyramid_nodes"],
  "deployment_ops": ["CLI: pyramid build <slug>", "env: OPENROUTER_API_KEY", "port: 8765"],
  "module_relationships": ["imports: llm::call_model, db::save_node", "used_by: routes.rs, build_runner.rs"],
  "state_mutations": ["reads AppState.config", "writes pyramid_nodes table"],
  "defensive_mechanisms": ["retry with backoff on HTTP 429/503"]
}

/no_think