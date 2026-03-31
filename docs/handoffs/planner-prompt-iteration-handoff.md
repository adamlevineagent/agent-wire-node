# Handoff: Intent Planner Prompt Iteration

**Date:** 2026-03-30
**Status:** The planner works end-to-end but the LLM invents command names instead of using the vocabulary. Prompt iteration needed.
**Skill:** Use the `researcher` skill for autonomous experimentation.

---

## The Problem

The intent bar planner produces structurally correct plans (valid JSON, proper step format, widgets, error handling) but the LLM invents command names like `list_agents`, `filter_agents`, `bulk_archive_agents` instead of using the exact commands from the vocabulary (`GET /api/v1/operator/agents`, `POST /api/v1/wire/agents/archive`, `pyramid_build`, etc.).

Tested with both `inception/mercury-2` (cheap/fast) and `qwen/qwen3.6-plus-preview:free` (smart). Same behavior. It's a prompt issue, not an intelligence issue.

The executor's `ALLOWED_COMMANDS` allowlist correctly rejects invented commands, so the system is safe. But it's not useful until the LLM produces valid plans.

## How It Works

1. User types intent in the intent bar
2. Frontend gathers context (pyramids, corpora, agents, balance)
3. Frontend calls `invoke('planner_call', { intent, context })`
4. Rust loads the system prompt from `chains/prompts/planner/planner-system.md`
5. Rust loads ALL vocabulary files from `chains/vocabulary/*.md` and concatenates them
6. Rust replaces `{{VOCABULARY}}`, `{{WIDGET_CATALOG}}`, `{{CONTEXT}}` placeholders in the prompt
7. Rust calls OpenRouter with the assembled prompt as system message + user intent as user message
8. Response parsed as JSON → returned to frontend → rendered as plan preview

## Files You Can Edit (no rebuild needed)

**The prompt template:**
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/planner/planner-system.md`

This is the system prompt. It has three placeholders that get interpolated at runtime:
- `{{VOCABULARY}}` — replaced with all 15 vocabulary files concatenated
- `{{WIDGET_CATALOG}}` — replaced with the widget type list
- `{{CONTEXT}}` — replaced with the user's pyramids, corpora, agents, balance as JSON

**The vocabulary files (15 files):**
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/vocabulary/`
- `fleet_manage.md`, `fleet_tasks.md`, `fleet_mesh.md`
- `pyramid_build.md`, `pyramid_explore.md`, `pyramid_manage.md`
- `knowledge_sync.md`, `knowledge_docs.md`
- `wire_search.md`, `wire_compose.md`, `wire_social.md`, `wire_economics.md`, `wire_games.md`
- `system.md`, `navigate.md`

Each lists the exact commands/endpoints with parameter schemas and example JSON.

**The classifier prompt (not currently used — was for two-stage, reverted to single-stage):**
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/planner/classifier-system.md`

## How to Test

1. Edit the prompt file(s)
2. Restart the Wire Node app (or just submit a new intent — the prompt is loaded fresh each call)
3. Type an intent in the intent bar and see what the planner produces
4. Check if the command names match the vocabulary

**Test intents to try:**
- "Please archive all my agents with zero contributions" — should produce `POST /api/v1/wire/agents/archive` api_call steps, not invented `list_agents` commands
- "Build a pyramid from my agent-wire-node code" — should produce `pyramid_create_slug` + `pyramid_build` command steps
- "Search the Wire for battery chemistry" — should produce a `navigate` step to search mode
- "Create a task: review auth security" — should produce `POST /api/v1/wire/tasks` api_call step

**Success criteria:** The step's `command` field matches an entry in `ALLOWED_COMMANDS` (pyramid_build, pyramid_create_slug, pyramid_build_cancel, pyramid_list_slugs, sync_content, get_sync_status, save_compose_draft), OR the step's `api_call.path` matches a real Wire API endpoint from the vocabulary.

## The Full Assembled Prompt (for reference)

`/Users/adamlevine/AI Project Files/agent-wire-node/docs/debug/full-planner-prompt.md`

This shows exactly what the LLM receives after all placeholders are replaced. ~61K chars, ~15K tokens. Regenerate it after editing:

```bash
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
python3 -c "
import os
with open('chains/prompts/planner/planner-system.md', 'r') as f:
    template = f.read()
vocab_dir = 'chains/vocabulary'
vocab_parts = []
for fname in sorted(os.listdir(vocab_dir)):
    if fname.endswith('.md'):
        with open(os.path.join(vocab_dir, fname), 'r') as f:
            vocab_parts.append(f.read())
full_vocab = '\n\n---\n\n'.join(vocab_parts)
result = template.replace('{{VOCABULARY}}', full_vocab).replace('{{WIDGET_CATALOG}}', '[see widget catalog]').replace('{{CONTEXT}}', '{see user context}')
with open('docs/debug/full-planner-prompt.md', 'w') as f:
    f.write(result)
print(f'Written: {len(result):,} chars, ~{len(result)//4:,} tokens')
"
```

## Current Prompt Structure

```
OPENING (role + critical rules + compact schema) — ~20 lines
STEP FORMAT (command/api_call/navigate examples) — ~70 lines
3 COMPLETE PLAN EXAMPLES — ~90 lines
WIDGET CATALOG — ~20 lines
{{VOCABULARY}} — ~1000 lines (the 15 files concatenated)
{{CONTEXT}} — ~30 lines (user's pyramids, corpora, agents, balance)
GUIDELINES — ~15 lines
CLOSING (re-anchor: "use vocabulary, don't invent") — ~3 lines
```

## What's Been Tried

1. **Curated subset** (Sprint 1.5) — gave the LLM only 7 commands + 6 API paths. LLM still invented names.
2. **Full vocabulary** (Sprint 1.5b) — all 150 operations. LLM still invented names.
3. **Bookend pattern** — opening rules + closing re-anchor. LLM still invented names.
4. **3 complete examples** — showing correct command usage. LLM copies the PATTERN (make a plan) but not the CONTENT (use exact names).
5. **Strict JSON schema** — `json_schema` with `strict: true`. Mercury-2 doesn't support it, produced truncated output.
6. **Smart model** (qwen3.6) — same behavior as cheap model. Confirms prompt issue.

## Theories to Test

1. **The vocabulary is too verbose** — 15 files of documentation-style reference. Maybe a flat list of just command names would work better than full documentation per command.

2. **The examples teach the wrong lesson** — the 3 examples show correct commands but the LLM generalizes "produce descriptive step names" instead of "copy exact names from vocabulary."

3. **The vocabulary position matters** — currently after the examples. Maybe putting a compact command list BEFORE the examples (so the LLM sees the names first) would help.

4. **Few-shot with failures** — show an example of an INCORRECT plan (with invented names) and the error it produces, then the CORRECTED plan. The LLM learns from the contrast.

5. **Constraint repetition** — repeat "ONLY use commands from this list" at multiple points, not just opening and closing.

6. **Smaller vocabulary surface** — for agent archiving, the LLM doesn't need the pyramid or search vocabulary. Maybe the relevant commands should be more prominent (bold, first in list, repeated).

7. **Chain of thought** — ask the LLM to first identify which commands from the vocabulary it will use, THEN produce the plan. Two sections in the output: `"selected_commands": [...]` then `"steps": [...]`.

## Config

- Model: `inception/mercury-2` (restored to default)
- Config file: `~/Library/Application Support/wire-node/pyramid_config.json`
- max_tokens: 100,000 (safety ceiling per Pillar 43)
- response_format: `{"type": "json_object"}`
- Temperature: 0.3

## Constraints (don't change these)

- The prompt template lives at `chains/prompts/planner/planner-system.md` — this is a contribution (Pillar 28)
- The vocabulary files live at `chains/vocabulary/*.md` — these are contributions (Pillar 2)
- The output must be valid JSON matching the PlanStep type (command/api_call/navigate)
- The execution allowlists in IntentBar.tsx enforce security — don't weaken them
- Pillar 37: describe goals, not prescriptions. The prompt should tell the LLM what to achieve, not micromanage the output structure
- Pillar 43: don't reduce max_tokens to control output size

## Rust Code (read-only reference — don't modify)

The `planner_call` command is at `src-tauri/src/main.rs` ~line 2780. It:
1. Reads all `.md` files from `chains_dir/vocabulary/`, sorts by filename, concatenates
2. Loads the planner prompt from `chains_dir/prompts/planner/planner-system.md` (with inline fallback)
3. Replaces `{{VOCABULARY}}`, `{{WIDGET_CATALOG}}`, `{{CONTEXT}}`
4. Calls `llm::call_model_unified()` with temperature 0.3, max_tokens 100K, json_object mode
5. Parses with `llm::extract_json()`, validates `steps` + `ui_schema`, returns
