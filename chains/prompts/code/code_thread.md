You are given all the file extractions from a single THREAD — a coherent subsystem or feature area of the codebase. These files were grouped together because they form a related unit.

Your job: synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE so rich that a developer can understand this entire subsystem WITHOUT drilling deeper. The node should be a complete briefing — not a summary that points elsewhere, but a document that CONTAINS the knowledge.

ORIENTATION — write a COMPREHENSIVE senior engineer briefing (8-15 sentences). Cover ALL of these:
- What does this subsystem do? (Include domain concept definition if this is a core concept like "Pyramid", "Vine", "Chain", "Wire", "Slug", "Partner")
- What are the key files and entry points? Name them specifically.
- What external services/tables/APIs does it touch? Name them with full paths/URLs.
- How does data flow through this subsystem? Trace a concrete request end-to-end: "Request arrives at POST /pyramid/:slug/build → handler calls PyramidBuilder.run_pipeline() → executes warm_pass, crystallization_pass, meta_analysis → each pass reads from pyramid_chunks and writes to pyramid_nodes(slug, id, depth, distilled, topics, children, parent_id) → emits build_progress event via WebSocket"
- What are the 2-3 things a developer MUST know before working here? (gotchas, invariants, performance concerns)
- What does error handling look like? (retries, fallbacks, what happens on failure)
- How does auth work in this subsystem? (if applicable — who can access what, how tokens are validated)

Then organize details into 3-8 sub-topics. For each:
- name: a clear aspect (e.g., "Authentication Flow", "Database Schema", "Build Pipeline", "Error Recovery")
- current: 4-8 sentences with SPECIFIC names, data flows, AND operational details. Describe the full lifecycle. Say "PyramidBuilder calls run_pipeline() which executes warm_pass (reads all L0 nodes, identifies stale ones via StaleDetector.compute_stale_nodes(), marks them for rebuild), then crystallization_pass (dispatches LLM calls via ChainDispatcher, parses JSON responses, writes new nodes to pyramid_nodes), then meta_analysis (generates FAQ entries, updates pyramid_meta). Each pass can fail independently — failed passes are logged but don't abort the pipeline. The builder writes progress to a shared Arc<RwLock<BuildStatus>> polled by the frontend every 2s."
- entities: EVERY specific function, type, table (with columns if known), endpoint (with method and path), env var (with what it controls), IPC command, file path mentioned in the child extractions for this topic. List them ALL — these are what developers grep for.

IMPORTANT — preserve these operational details that testers consistently miss:
- Database table names WITH column names: "table: pyramid_nodes(slug, id, depth, distilled, topics, children, parent_id)"
- Auth flows end-to-end: "Bearer token → validate_token() checks against sessions table → returns UserSession with role"
- Integration channels: "Frontend calls invoke('getDashboardData') → Rust handler → queries pyramid_slugs → returns SlugInfo"
- Error handling patterns: "Retries 3x with exponential backoff, falls back to carry-left on final failure"
- Build commands: "npm run tauri:build → vite build → cargo build --release → tauri bundler"

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "8-15 sentences: COMPREHENSIVE senior engineer briefing. Domain concept definition, every key file with its role, entry points, complete data flow traced end-to-end, external services with URLs/tables, auth model for this subsystem, error handling patterns, and gotchas. A developer reads ONLY this orientation and can start working.",
  "source_nodes": ["C-L0-000", "C-L0-005"],  // COPY EXACT node IDs from the input — preserve zero-padding (C-L0-070, NOT C-L0-70)
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "4-8 sentences. Full operational description with specific names, complete data flows, table schemas, endpoint contracts, error handling. Dense enough that a developer understands this aspect without drilling to L0.",
      "entities": ["every_function()", "EveryType", "table: every_table(col1, col2)", "env: EVERY_VAR — controls X", "HTTP: POST /every/endpoint — does Y", "IPC: invoke('command')"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
