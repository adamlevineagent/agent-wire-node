# Plan: Integrate Knowledge Pyramid Engine into agent-wire-node

## Context
The Knowledge Pyramid prototype (Python, 3,171 lines) is validated across 7 blind tests scoring 8-10/10 on conversations, code, and fiction. Now we port it to Rust and integrate it into the agent-wire-node Tauri v2 desktop app as the persistent knowledge backend for Vibesmithy.

## Target App
`/Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/`
- Tauri v2 + React/TypeScript frontend
- AppState with Arc<RwLock<T>> pattern
- Background tasks via tauri::async_runtime::spawn
- reqwest for HTTP, warp for local server
- JSON file persistence (NO SQLite currently)
- 2,859 lines in main.rs, 39 Tauri commands

## Source to Port
`/Users/adamlevine/AI Project Files/GoodNewsEveryone/pyramid-prototype/pyramid_engine.py`

## Module Structure
All new code under `src-tauri/src/pyramid/`:
```
pyramid/
  mod.rs        — PyramidState, Tauri commands, public API
  db.rs         — SQLite schema (4 tables), CRUD, init
  model.rs      — OpenRouter client, 3-model fallback cascade
  extract.rs    — JSON extraction from LLM responses
  prompts.rs    — 11 prompt constants (verbatim from Python)
  ingest.rs     — Conversation JSONL, code directory, document directory
  mechanical.rs — Import graph, spawn counter, string literals, IPC boundary
  pipeline.rs   — Forward/reverse/combine, concurrent extraction, thread clustering, synthesis
  query.rs      — apex, node, tree, drill, search, entities, resolved
```

## Phases

### Phase 1: Core Engine (~960 lines Rust)
Create:
- `pyramid/db.rs` — SQLite schema identical to Python (sources, chunks, pipeline_steps, nodes tables), rusqlite with bundled feature, CRUD functions
- `pyramid/model.rs` — OpenRouter HTTP via reqwest, 3-model fallback cascade (Mercury-2 120K → Qwen Flash 900K → Grok 4.20), 5 retries with exponential backoff, null content retry
- `pyramid/extract.rs` — Strip think tags, markdown fences, find JSON object, fix trailing commas
- `pyramid/prompts.rs` — All 11 prompts as const &str verbatim from Python
- `pyramid/mod.rs` — PyramidState struct, PyramidConfig, get_db_connection helper

Modify:
- `Cargo.toml` — add `rusqlite = { version = "0.31", features = ["bundled"] }`, `regex = "1"`
- `lib.rs` — add `pub mod pyramid;` + pyramid field in AppState

### Phase 2: Ingestion (~500 lines Rust)
Create:
- `pyramid/ingest.rs` — Three functions:
  - `ingest_conversation(path)` — Parse JSONL, chunk at ~100 lines, save to DB
  - `ingest_code(dir_path)` — Walk dir, skip SKIP_DIRS, 1 file = 1 chunk with language/lines metadata
  - `ingest_docs(dir_path)` — Walk dir, .txt/.md files, 1 doc = 1 chunk

### Phase 3: Pipeline (~1,100 lines Rust)
Create:
- `pyramid/mechanical.rs` — Regex-based: import graph, spawn counter, string literals, IPC boundary, complexity metrics, cluster_by_imports
- `pyramid/pipeline.rs` — All build orchestration:
  - Conversation: forward → reverse → combine → L1 pairing → threads → apex
  - Code: mechanical → concurrent L0 (semaphore 10) → import clustering → L1 groups → threads → apex
  - Documents: concurrent L0 → entity-overlap clustering → L1 → threads → apex
  - Shared: batched thread clustering (split >30K tokens), pairwise synthesis loop
  - Concurrency: tokio::spawn + Semaphore(10) + mpsc channel for DB writes

### Phase 4: Query Commands + Tauri Commands (~600 lines Rust)
Create:
- `pyramid/query.rs` — apex, node, tree, drill, search, corrections, terms, resolved, entities, status — all return Serialize structs
Modify:
- `pyramid/mod.rs` — ~12 Tauri commands: pyramid_ingest, pyramid_build, pyramid_status, pyramid_apex, pyramid_node, pyramid_tree, pyramid_search, pyramid_entities, pyramid_resolved, pyramid_list_sources, pyramid_grow
- `main.rs` — Register commands in invoke_handler (~30 lines), init pyramid state

### Phase 5: Frontend Components (~880 lines React/TS)
Create in `src/components/pyramid/`:
- `PyramidBrowser.tsx` — Tree view + node detail panel
- `PyramidSearch.tsx` — Search interface with results
- `EntityGraph.tsx` — Entity index view
- `SourceManager.tsx` — Ingest/build controls, folder picker
- `BuildProgress.tsx` — Real-time build progress polling
- `PyramidSettings.tsx` — API key + model config

## Key Decisions
- **rusqlite (bundled)** for pyramid DB — existing JSON persistence stays for auth/sync/credits
- **spawn_blocking** for DB ops (rusqlite Connection isn't Send)
- **Prompts verbatim from Python** — identical output, same blind test scores
- **Build is resumable** — every step checks DB before running

## Pre-Implementation: Update Conductor Skills

Before dispatching workstreams, update conductor skills to use Knowledge Pyramids:

### conductor-implement SKILL.md
Add a "Phase -1: Knowledge Pyramid Onboarding" section before Phase 0:
- Check if a pyramid DB exists for the target codebase
- If yes: every workstream agent gets instructions to use `python3 pyramid_engine.py` commands for codebase navigation
- Template addition to workstream prompt: "CODEBASE KNOWLEDGE: A Knowledge Pyramid is available. Before writing code, run `python3 /path/to/pyramid_engine.py apex <source_id>` to understand the architecture. Use `search <term>` to find relevant modules. Use `entities` to see cross-file connections."
- Pyramid DB path: `/Users/adamlevine/AI Project Files/GoodNewsEveryone/pyramid-prototype/home_mind.db`
- Source IDs: agent-wire-node = 9

### conductor-audit-pass, conductor-informed-audit, conductor-discovery-audit
Add pyramid instructions to auditor prompts:
- "A Knowledge Pyramid is available for this codebase. Use it to understand the architecture before auditing."

### conductor-holistic-audit, conductor-deep-audit
Same — add pyramid navigation to every auditor agent prompt.

## Verification
1. `cargo build` succeeds
2. Ingest relay-app itself, verify node counts match Python version
3. Blind test on Rust-generated pyramid — should score 10/10 same as Python
4. Frontend: invoke pyramid_apex, verify JSON renders
