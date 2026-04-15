# Hotfix: Restore HTTP mutation routes — BLOCKING

## The Problem
All pyramid mutation routes (create, ingest, build, cancel, etc.) return HTTP 410 "moved to IPC". The MCP server, CLI tools, and any external agent can't trigger builds. The researcher skill can't run experiments. The autonomous improvement loop is broken.

This is a regression. These routes worked before. Now they're stubbed out with 410 responses.

## What's broken
Every POST endpoint in `routes.rs` that mutates pyramid state returns:
```json
{"error":"moved to IPC","command":"pyramid_create_slug"}
```

Affected: `pyramid_create_slug`, `pyramid_build`, `pyramid_ingest`, `pyramid_build_cancel`, `pyramid_set_config`, `pyramid_archive_slug`, `pyramid_purge_slug`, `pyramid_crystallize`, `pyramid_vine_build`, `pyramid_question_build`, `pyramid_publish`, `pyramid_check_staleness`, `pyramid_chain_import`, and more.

## The Fix
Restore the HTTP mutation handlers. The IPC handlers can coexist — both HTTP and IPC should be able to trigger the same operations. The HTTP routes call the same underlying functions that the IPC commands call.

## Why this matters
- The MCP server is the external interface for agents to interact with Wire Node
- The researcher skill needs to trigger builds to iterate on prompt quality
- Any external tool (CI, scripts, other agents) needs HTTP mutation access
- Without HTTP mutation, pyramid quality improvement requires manual UI interaction — which violates the contribution architecture (agents should be able to improve everything)

## Files
- `src-tauri/src/pyramid/routes.rs` — every 410 stub needs to be restored to its actual handler

## Scope
This is ~25 routes that need handlers restored. The handlers existed before — this is reverting a regression, not writing new code. Check git history for the working versions.
