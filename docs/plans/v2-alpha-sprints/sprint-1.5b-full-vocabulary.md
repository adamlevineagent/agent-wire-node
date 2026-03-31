# Sprint 1.5b — Full Vocabulary Planner

## Context

Sprint 1.5 gave the LLM a curated subset of commands. The LLM struggled to produce correct plans because the vocabulary was too small and the LLM invented its own action names. The fix: expose the FULL Wire vocabulary (~150 operations, ~12K tokens) in a single prompt. The LLM reads the complete vocabulary, picks the right operations, and fills in parameters from context.

The full vocabulary fits easily in the primary model's 120K context window. No two-stage classification needed — one LLM call with the complete reference.

## What Was Built

### 1. Vocabulary files (15 categories, ~150 operations)

`chains/vocabulary/` contains 15 `.md` files, one per category:
- pyramid_build, pyramid_explore, pyramid_manage
- fleet_manage, fleet_tasks, fleet_mesh
- knowledge_sync, knowledge_docs
- wire_search, wire_compose, wire_social, wire_economics, wire_games
- system, navigate

Each file documents every command/endpoint in the category with: name, type (command/api_call/navigate), parameters with types, one-line description, example invocation JSON, and auth type.

Total: ~1,000 lines, ~50K characters, ~12K tokens. Fits alongside the system prompt, widget catalog, and user context in a single LLM call.

**The vocabulary is a Wire contribution (Pillar 2).** The `.md` files are the seed version. Once published to the Wire (Sprint 2), the Wire copy is authoritative. Anyone can publish an improved vocabulary via supersession. The planner uses the best-cited version.

### 2. Single-stage planner

`planner_call` in main.rs:
1. Reads ALL vocabulary files from `chains_dir/vocabulary/` and concatenates them
2. Loads the planner system prompt from `chains_dir/prompts/planner/planner-system.md`
3. Replaces `{{VOCABULARY}}` with the full concatenated vocabulary
4. Replaces `{{WIDGET_CATALOG}}` and `{{CONTEXT}}` as before
5. Single LLM call: temperature 0.3, max_tokens 2048, json_object response format
6. Parses with `extract_json()`, validates required fields, returns plan

One command, one LLM call, full vocabulary. No classification stage, no category routing.

### 3. Planner system prompt

`chains/prompts/planner/planner-system.md` uses `{{VOCABULARY}}` placeholder where the full vocabulary is interpolated at runtime. The prompt defines the role, step format (command/api_call/navigate), widget catalog, output schema, and Pillar 37-compliant guidelines.

### 4. Classifier prompt (created but not used)

`chains/prompts/planner/classifier-system.md` was created for the two-stage design but is not used in the single-stage architecture. Kept as a reference — if vocabulary grows beyond context window limits, the two-stage approach can be reactivated.

## Files

| File | Status |
|------|--------|
| `chains/vocabulary/*.md` (15 files) | Created — seed vocabulary |
| `chains/prompts/planner/planner-system.md` | Updated — `{{VOCABULARY}}` placeholder |
| `chains/prompts/planner/classifier-system.md` | Created — not used in single-stage |
| `src-tauri/src/main.rs` | Updated — single `planner_call` loads full vocabulary |
| `src/components/IntentBar.tsx` | Updated — single planning phase, no classify step |
| `src/types/planner.ts` | Updated — removed ClassifyResult |

## Verification

1. "Archive all agents with no contributions" → planner sees fleet_manage vocabulary → produces correct `POST /api/v1/wire/agents/archive` calls
2. "Build a pyramid from my agent-wire-node code" → planner sees pyramid_build vocabulary → produces `pyramid_create_slug` + `pyramid_build` with correct args
3. "Search the Wire for battery chemistry" → planner sees wire_search vocabulary → produces navigation with query
4. Single LLM call completes in ~2-3 seconds
5. `cargo check` + `npx tsc --noEmit` pass
