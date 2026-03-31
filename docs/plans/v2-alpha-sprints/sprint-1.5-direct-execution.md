# Sprint 1.5 — Direct Execution (Pillar 37 Fix)

## Context

Sprint 1 shipped the intent bar planner. The planner produces good plans — it understands intent and breaks it into steps. But execution fails because the planner outputs natural-language action descriptions ("Search for agents with zero contributions") that a hardcoded switch statement tries to pattern-match to 5 predefined operation strings. This is a Pillar 37 violation: we're prescribing outputs to intelligence by forcing the LLM to use our labels, then re-interpreting those labels ourselves.

**The fix:** Give the LLM the Wire Node's actual command vocabulary. The planner writes plans in the Wire's language. The execution layer just runs them.

## The Principle

The LLM receives:
1. **The user's intent** — natural language
2. **The available commands** — exact Tauri invoke names, parameter schemas, what each does

The LLM produces:
- Steps where each step IS an invocable command with concrete arguments

The execution layer:
- Reads `step.command` and `step.args`
- Validates command against `ALLOWED_COMMANDS` allowlist
- Calls `invoke(step.command, step.args)`
- No mapping, no switch statement, no interpretation
- Per-step try/catch: on failure, logs the error and continues to the next step (unless `step.on_error` is `"abort"`)

The intelligence decides what to invoke. We just run it.

## What Changes

### 1. Plan step format

**Before (Sprint 1):**
```json
{
  "id": "step-1",
  "action": "build_pyramid",
  "description": "Build a code pyramid",
  "estimated_cost": null,
  "params": { "corpus": "agent-wire-node-repo", "content_type": "code" }
}
```
The `action` string is interpreted by a switch statement that maps it to the actual Tauri command.

**After (Sprint 1.5):**
```json
{
  "id": "step-1",
  "command": "pyramid_build",
  "args": { "slug": "agent-wire-node-repo" },
  "description": "Build a code pyramid from the agent-wire-node repository",
  "estimated_cost": null,
  "on_error": "abort"
}
```
The `command` IS the Tauri invoke name. `args` IS the invoke argument object. The LLM writes the actual invocation — we just run it.

Conditional logic (e.g., "create slug if it doesn't exist, then build") is expressed as separate sequential steps:
```json
[
  { "id": "step-1", "command": "pyramid_list_slugs", "args": {}, "description": "List existing pyramids to check if slug exists" },
  { "id": "step-2", "command": "pyramid_create_slug", "args": { "slug": "agent-wire-node-repo", "contentType": "code", "sourcePath": "/path", "referencedSlugs": null }, "description": "Create pyramid workspace (skip if already exists)", "on_error": "continue" },
  { "id": "step-3", "command": "pyramid_build", "args": { "slug": "agent-wire-node-repo" }, "description": "Build the pyramid" }
]
```

For Wire API calls (not Tauri commands), the step uses:
```json
{
  "id": "step-2",
  "api_call": { "method": "POST", "path": "/api/v1/wire/agents/archive", "body": { "agent_id": "..." } },
  "auth": "operator",
  "description": "Archive the agent"
}
```

For navigation (no command, just UI routing):
```json
{
  "id": "step-3",
  "navigate": { "mode": "search", "view": "root", "props": { "query": "battery chemistry" } },
  "description": "Open Wire Search with the query pre-filled"
}
```

Three step types: `command` (Tauri invoke), `api_call` (Wire API), `navigate` (UI routing). The LLM picks the right one.

### 2. Command allowlist (CRITICAL)

The executor enforces a strict `ALLOWED_COMMANDS` set containing only the 7 documented Tauri commands:
- `pyramid_build`
- `pyramid_create_slug`
- `pyramid_build_cancel`
- `pyramid_list_slugs`
- `sync_content`
- `get_sync_status`
- `save_compose_draft`

Any command not in the set is rejected with "Command not allowed: {name}".

`operator_api_call` and `wire_api_call` are explicitly blocked in the command path — they are meta-escape hatches. API calls to the Wire go through the `api_call` step type instead, which routes through the appropriate API call function internally.

### 3. API call allowlist (MEDIUM)

The executor enforces an `ALLOWED_API_PATHS` set with the documented endpoints:
- `POST /api/v1/wire/agents/archive`
- `PATCH /api/v1/operator/agents/{id}/status`
- `POST /api/v1/wire/tasks`
- `PUT /api/v1/wire/tasks/{id}`
- `POST /api/v1/wire/rate`
- `POST /api/v1/contribute`

Unlisted API paths are rejected with "API path not allowed: {path}".

### 4. Per-step error handling (CRITICAL)

Each step is wrapped in try/catch. On failure:
1. The error is logged to the operation entry (command name + args + error message)
2. If `step.on_error` is `"abort"`, execution stops and the operation fails
3. If `step.on_error` is `"continue"` (default), execution proceeds to the next step

This prevents a single step failure from aborting an entire multi-step plan.

### 5. Planner system prompt update

Replace the "Available actions" section with an "Available commands" section that lists:

**Tauri commands (local node operations):**
- `pyramid_build` — `{ slug: string }` — Build a pyramid (slug must exist first)
- `pyramid_create_slug` — `{ slug: string, contentType: string, sourcePath: string, referencedSlugs: string[] | null }` — Create a new pyramid workspace
- `pyramid_build_cancel` — `{ slug: string }` — Cancel a running build
- `pyramid_list_slugs` — `{}` — List all pyramids (read-only, for checking existence)
- `sync_content` — `{}` — Trigger folder sync
- `get_sync_status` — `{}` — Check sync status (read-only)
- `save_compose_draft` — `{ draft: object }` — Save a composition draft

Only these 7 commands are allowed. `operator_api_call` and `wire_api_call` are blocked — use `api_call` steps instead.

**Wire API calls (network operations):**
- `POST /api/v1/wire/agents/archive` — `{ agent_id: string }` — Archive an agent (auth: operator)
- `PATCH /api/v1/operator/agents/{pseudoId}/status` — `{ status: "paused" | "active" | "revoked" }` — Change agent status (auth: operator)
- `POST /api/v1/wire/tasks` — `{ title, context, priority, scope }` — Create a task (auth: wire)
- `PUT /api/v1/wire/tasks/{taskId}` — `{ action: "claim" | "move" | "complete" | "archive", column?: string }` — Update a task (auth: wire)
- `POST /api/v1/wire/rate` — `{ item_id, item_type, accuracy?, usefulness?, flag? }` — Rate a contribution (auth: wire)
- `POST /api/v1/contribute` — `{ type, title, body, topics?, pricing_mode?: "emergent" | "fixed", price?: number | null, derived_from?: [{ source_type: string, source_item_id: string, weight: number, justification: string }] }` — Publish a contribution (auth: wire). `pricing_mode` defaults to "emergent". `price` is null for emergent pricing. `derived_from` cites sources with weight and justification.

**Navigation (UI routing):**
- `{ mode: "search", props: { query: "..." } }` — Open Search with pre-filled query
- `{ mode: "compose", props: { title: "...", body: "..." } }` — Open Compose with pre-filled content
- `{ mode: "fleet", props: {} }` — Open Fleet tab
- `{ mode: "operations", props: {} }` — Open Operations tab
- `{ mode: "knowledge", props: {} }` — Open Knowledge tab

The LLM sees EXACTLY what it can invoke. It writes the invocation. We run it.

### 6. Execution function rewrite

Replace the `executeStep` switch statement in IntentBar.tsx with a generic executor:

```typescript
const ALLOWED_COMMANDS = new Set([
    'pyramid_build', 'pyramid_create_slug', 'pyramid_build_cancel',
    'pyramid_list_slugs', 'sync_content', 'get_sync_status', 'save_compose_draft',
]);

const ALLOWED_API_PATHS = new Set([
    'POST /api/v1/wire/agents/archive',
    'PATCH /api/v1/operator/agents/*/status',
    'POST /api/v1/wire/tasks',
    'PUT /api/v1/wire/tasks/*',
    'POST /api/v1/wire/rate',
    'POST /api/v1/contribute',
]);

async function executeStep(step, ...deps): Promise<unknown> {
    if (step.command) {
        if (!ALLOWED_COMMANDS.has(step.command)) {
            throw new Error(`Command not allowed: ${step.command}`);
        }
        return invoke(step.command, step.args ?? {});
    }

    if (step.api_call) {
        // Validate against ALLOWED_API_PATHS
        const { method, path, body } = step.api_call;
        if (step.auth === 'operator') {
            return invoke('operator_api_call', { method, path, body });
        } else {
            return invoke('wire_api_call', { method, path, body });
        }
    }

    if (step.navigate) {
        setMode(step.navigate.mode);
        return { navigated: true };
    }

    throw new Error(`Step ${step.id} has no command, api_call, or navigate`);
}
```

The caller wraps each step in try/catch:
```typescript
for (const step of plan.steps) {
    try {
        await executeStep(step, ...);
    } catch (err) {
        logStepError(operationId, step, err);
        if (step.on_error === 'abort') throw err;
        // default: continue
    }
}
```

No switch statement. No action-to-command mapping. The LLM's output IS the execution plan.

### 7. Type updates

Update `PlanStep` in `src/types/planner.ts`:

```typescript
export interface PlanStep {
    id: string;
    description: string;
    estimated_cost: number | null;
    on_error?: 'abort' | 'continue';
    // Exactly one of these three:
    command?: string;
    args?: Record<string, unknown>;
    api_call?: { method: string; path: string; body?: unknown };
    auth?: 'operator' | 'wire';
    navigate?: { mode: string; view?: string; props?: Record<string, unknown> };
}
```

Note: `pre_steps` field is intentionally omitted. Conditional logic should be expressed as separate sequential steps. This is more Pillar 37 compliant — the LLM expresses the full execution plan as a flat step list, and each step stands alone.

### 8. Operation entry versioning

Add a `format_version` field to `OperationEntry`. On app load, clear operations with outdated format (version !== current). This prevents stale operations from Sprint 1 format from causing rendering issues.

```typescript
export interface OperationEntry {
    id: string;
    intent: string;
    status: 'running' | 'completed' | 'failed';
    steps: PlanStep[];
    currentStep: number;
    startedAt: number;
    result?: unknown;
    error?: string;
    stepErrors?: { stepId: string; command?: string; args?: unknown; error: string }[];
    format_version: number; // Current: 2
}
```

### 9. Security consideration

The command allowlist (`ALLOWED_COMMANDS`) restricts which Tauri commands the LLM can invoke to the 7 documented commands. The API path allowlist (`ALLOWED_API_PATHS`) restricts which Wire endpoints can be called. The operator approves each plan before execution (Pillar 23). Plan preview shows step descriptions prominently. Command/API details shown as collapsible secondary info.

The command/api_call/navigate step format maps to Wire action step concepts for Sprint 2.

---

## Phases

### Phase 1: Update PlanStep type
- `src/types/planner.ts` — new step format (command/api_call/navigate, on_error, format_version on OperationEntry)

### Phase 2: Update planner system prompt
- `chains/prompts/planner/planner-system.md` — replace "Available actions" with "Available commands" listing exact Tauri command names (with allowlist note), Wire API endpoints (with full body shapes including contribute schema with pricing_mode, price, derived_from), and navigation targets

### Phase 3: Rewrite IntentBar execution
- `src/components/IntentBar.tsx` — replace executeStep switch statement with generic executor using ALLOWED_COMMANDS set, ALLOWED_API_PATHS set, per-step try/catch with on_error support, no switch statement. Preview shows descriptions prominently with collapsible technical details.

### Phase 4: Update planner_call fallback prompt
- `src-tauri/src/main.rs` — update the PLANNER_FALLBACK_PROMPT and PLANNER_AVAILABLE_ACTIONS constants to match the new command vocabulary

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | PlanStep type update | Small | — |
| 2 | System prompt rewrite | Medium | — |
| 3 | IntentBar execution rewrite | Medium | Phase 1 |
| 4 | Fallback prompt update | Tiny | Phase 2 |

Phases 1+2 parallel, then 3+4 parallel.

## Verification

1. Type "archive all agents with no contributions" → plan shows actual Wire API calls (`POST /api/v1/wire/agents/archive`) with real parameters
2. Type "build a pyramid from my agent-wire-node code" → plan shows `pyramid_list_slugs` + `pyramid_create_slug` + `pyramid_build` as separate sequential steps with correct args
3. Type "search the Wire for battery chemistry" → plan shows navigation to search mode with query
4. Approve a plan → commands execute successfully (not "unknown action" errors)
5. LLM invents a non-existent command → step rejected with "Command not allowed: {name}"
6. LLM tries to use `operator_api_call` as a command → rejected with "Command not allowed: operator_api_call"
7. Step failure with `on_error: "continue"` → next step runs, error logged to operation
8. Step failure with `on_error: "abort"` → execution stops, operation marked failed
9. Old Sprint 1 operations cleared on app load (format_version mismatch)
10. `npx tsc --noEmit` + `cargo check` pass
