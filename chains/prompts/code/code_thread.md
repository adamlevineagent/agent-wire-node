You are given all the file extractions from a single THREAD — a coherent subsystem or feature area of the codebase. These files were grouped together because they form a related unit.

Your job: synthesize this thread into a SINGLE AUTHORITATIVE REFERENCE NODE that a developer can read to understand this entire subsystem without drilling deeper (though they can if they want specifics).

The orientation paragraph is the most important output. It should read like a senior engineer's briefing:
- What does this subsystem do? (Include domain concept definition if this is a core concept like "Pyramid", "Vine", "Chain", "Wire", "Slug", "Partner")
- What are the key files and entry points?
- What external services/tables/APIs does it touch?
- How does data flow through this subsystem? (e.g., "Request arrives at /pyramid/:slug/build → PyramidBuilder.run_pipeline() → writes to pyramid_nodes table → emits build_progress event")
- What are the 2-3 things a developer MUST know before working here?

Then organize details into 3-6 sub-topics. For each:
- name: a clear aspect (e.g., "Authentication Flow", "Database Schema", "Build Pipeline", "Error Recovery")
- current: 2-3 sentences with SPECIFIC names AND data flows. Say "PyramidBuilder calls run_pipeline() which executes warm_pass, crystallization_pass, meta_analysis in sequence, writing results to pyramid_nodes(slug, id, depth, distilled, topics)" not "The builder runs several passes."
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
  "orientation": "4-6 sentences: senior engineer briefing. Domain concept definition, key files, entry points, data flow, external deps, gotchas.",
  "source_nodes": ["C-L0-000", "C-L0-005"],  // COPY EXACT node IDs from the input — preserve zero-padding (C-L0-070, NOT C-L0-70)
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "2-3 sentences with specific names, data flows, and operational details.",
      "entities": ["every_function()", "EveryType", "table: every_table(col1, col2)", "env: EVERY_VAR — controls X", "HTTP: POST /every/endpoint — does Y", "IPC: invoke('command')"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
