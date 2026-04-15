# Handback — IPC Blocker During Fresh Pyramid Research Setup

## Current State
- Fresh research branch created: `research/pyramid-quality-handoff`
- Previous lab archived to: `.lab.bak.20260403082921`
- Fresh lab scaffold created:
  - `.lab/config.md`
  - `.lab/results.tsv`
  - `.lab/log.md`
  - `.lab/branches.md`
  - `.lab/parking-lot.md`
  - `.lab/workspace/exp-0/`
- Cold start guide read: `chains/CHAIN-DEVELOPER-GUIDE.md`
- Handoff read: `.lab/handoff-pyramid-quality-researcher.md`

## What I Verified
- Local read surface works:
  - `node mcp-server/dist/cli.js health` returns `online`
  - `node mcp-server/dist/cli.js slugs` returns existing slugs
  - SQLite at `~/Library/Application Support/wire-node/pyramid.db` is readable
- Conversation sample corpus exists and is usable as source material:
  - `/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-The-Playful-Universe-vibesmithing-web`

## Blocker
All mutation paths I tried are IPC-gated in this environment, even though the cheat sheet still documents CLI/HTTP create/ingest/build flows.

### Failing paths
1. CLI mutation commands
   - `node mcp-server/dist/cli.js create-slug ...`
   - `node mcp-server/dist/cli.js ingest ...`
   - `node mcp-server/dist/cli.js build ...`
   - `node mcp-server/dist/cli.js vine-build ...`

2. Localhost HTTP mutation endpoints
   - `POST /pyramid/slugs`
   - `POST /pyramid/<slug>/ingest`
   - `POST /pyramid/<slug>/build`

### Representative response
```json
{"error":"moved to IPC","command":"pyramid_create_slug"}
```

## Evidence Collected
- The route layer explicitly says these operations moved to IPC:
  - `src-tauri/src/pyramid/routes.rs`
  - Matches found for:
    - `pyramid_create_slug`
    - `pyramid_ingest`
    - `pyramid_build`
    - `pyramid_vine_build`
- Existing docs are stale relative to runtime behavior:
  - `docs/pyramid-cli-cheatsheet.md` still presents mutation commands as terminal-usable

## Important Note
This is not a permissions problem. Filesystem access, DB access, and localhost read access all work. The issue is that the mutation surface available from this terminal has been intentionally redirected to Tauri IPC.

## Likely Next Debug Targets
1. Find the intended terminal-side bridge to Tauri IPC, if one exists.
   - Search for any script or helper that invokes Tauri commands from outside the desktop UI.
   - Look in:
     - `mcp-server/src/cli.ts`
     - `mcp-server/src/index.ts`
     - any planner/executor code that dispatches direct Tauri commands

2. Decide whether the docs should be updated.
   - `docs/pyramid-cli-cheatsheet.md` currently implies terminal mutation is available.
   - If terminal mutation is no longer supported, that document should say so clearly.

3. If no bridge exists, either:
   - perform create/ingest/build from the desktop UI manually and let research continue from read-only inspection, or
   - expose a non-UI mutation path again for research/debug workflows.

## Fresh Research Context Ready To Resume
- Objective: cross-pipeline pyramid quality from the handoff
- Planned baseline: fresh no-change builds for document, code, and conversation
- Current prompt/YAML observations already noted:
  - `document.yaml` uses `source_node`-only thread assignments
  - `code.yaml` still uses topic-level assignment schema in clustering + merge response schemas
  - `conversation.yaml` still uses topic-level assignment schema

## No Destructive Changes Made
- No repo files outside `.lab/` were modified in this fresh series
- No resets, stashes, or cleanup were performed
