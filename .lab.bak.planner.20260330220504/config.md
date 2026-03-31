# Research Configuration — Planner Command Names

## Objective
Make the intent planner LLM produce plans using EXACT command names and API paths from the vocabulary, instead of inventing descriptive names like `list_agents`, `filter_agents`, `bulk_archive_agents`.

## Metrics

### Primary: Command Validity Rate (higher is better)
- **Measure**: % of plan steps whose `command` or `api_call.path` matches ALLOWED_COMMANDS / ALLOWED_API_PATH_PATTERNS
- **Direction**: higher is better
- **Baseline**: pending
- **Target**: 100% across all 4 test intents

### Secondary: Plan Structural Quality (higher is better)
- **Measure**: Agent judgment — does the plan make logical sense for the intent?
- **Direction**: higher is better

## Test Intents
1. "Please archive all my agents with zero contributions" → expects `api_call` with `POST /api/v1/wire/agents/archive` (possibly with GET /api/v1/operator/agents first)
2. "Build a pyramid from my agent-wire-node code" → expects `pyramid_create_slug` + `pyramid_build` commands
3. "Search the Wire for battery chemistry" → expects `navigate` step to search mode
4. "Create a task: review auth security" → expects `api_call` with `POST /api/v1/wire/tasks`

## Valid Commands (ALLOWED_COMMANDS)
pyramid_build, pyramid_create_slug, pyramid_build_cancel, pyramid_list_slugs, sync_content, get_sync_status, save_compose_draft

## Valid API Path Patterns
POST /api/v1/wire/agents/archive, PATCH /api/v1/operator/agents/*/status, POST /api/v1/wire/tasks, PUT /api/v1/wire/tasks/*, POST /api/v1/wire/rate, POST /api/v1/contribute

## Additional Valid Step Types
- `navigate` steps (any mode) — always valid
- `api_call` steps using paths from vocabulary that aren't in the allowlist — scored as "vocabulary-correct but not executor-allowed" (partial credit)

## Scope
- `chains/prompts/planner/planner-system.md` — the system prompt template
- `chains/vocabulary/*.md` — the 15 vocabulary files

## Constraints
- DO NOT change max_tokens (Pillar 43)
- DO NOT weaken ALLOWED_COMMANDS or ALLOWED_API_PATH_PATTERNS in IntentBar.tsx
- DO NOT modify Rust code in src-tauri/
- Output must remain valid JSON matching PlanStep type
- Pillar 37: describe goals, not prescriptions

## Run Command
```bash
python3 .lab/workspace/test_planner.py
```

## Wall-Clock Budget
2 minutes per experiment (LLM calls are fast)

## Termination
100% command validity across all 4 test intents

## Model
inception/mercury-2 (temperature 0.3)

## Baseline
Pending

## Best
Pending
