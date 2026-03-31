# Sprint 1 — Intent Bar Planner

## Context

Sprint 0 shipped the v2 tab restructuring with a live status sidebar and an intent bar UI placeholder that shows "coming soon" on submit. Sprint 1 makes the intent bar intelligent — the user types what they want, the system plans how to do it, previews the cost, and executes on approval.

**Pre-implementation check:** Run `cargo check` from `src-tauri/` and `npx tsc --noEmit` from the repo root before starting. Both must pass clean. If either fails, fix pre-existing issues first.

## Prerequisites

1. **Wire server: VALID_TYPES fix** — add `'action'`, `'skill'`, `'template'` to VALID_TYPES in `contribute-core.ts`. One-line change. Deploy before publishing the planner action.

2. **Wire server: Query governor** (noted, not Sprint 1) — the flat 100-credit query cost IS now deployed. The governor is live. Response shape: `query_cost` (number) + `governor: { effective_count, bucket_index }`. Sprint 1's cost estimation should not hardcode prices. Cost displays should show "estimated" and pull from endpoints where possible.

## Architecture

### The Planner Runs Locally (Sprint 1)

There is no Tauri command to call LLMs from the frontend today. The LLM infrastructure (`llm.rs::call_model_unified()`) is internal-only, used by the chain executor. Sprint 1 adds a new Tauri command `planner_call` that:

1. Takes: `intent` (string), `context` (JSON object with pyramids, corpora, agents, balance)
2. Builds a system prompt that includes: the widget catalog, instructions for producing a plan object, the user's available assets
3. Calls `call_model_unified()` with a cheap fast model (the node's configured `primary_model`)
4. Parses the LLM response as JSON
5. Returns the structured plan to the frontend

This is a LOCAL call — no Wire server round-trip. The node's existing OpenRouter key handles authentication. The output format is chain-compatible (Pillar 2) so Sprint 2 can migrate the planner to a published Wire action chain (Pillar 17).

**Why not Wire action invoke?** The Wire server's `executeAction` supports `mode: 'review' | 'trusted'` for preview-then-commit (Pillar 23), but: (a) `executeLlmStep` throws `501 Not Implemented` — LLM execution on the Wire server isn't built yet, (b) mode='review' is DEPRECATED (logs warning, falls through to full execution). The Wire now supports mode='quote' which returns a SpotQuote { token, price, actionId, creatorAgentId, expiresAt, ttl } without executing. Sprint 2 uses quote mode for cost previews. For Sprint 1, the planner runs locally. Sprint 2 can either implement Wire-side LLM execution, or publish the planner as a `wire` type action that calls back to the node's LLM.

### Plan Format

The planner returns:
```json
{
  "plan_id": "uuid",
  "intent": "original user text",
  "steps": [
    {
      "id": "step-1",
      "action": "build_pyramid",
      "description": "Build a code pyramid from GoodNewsEveryone corpus (LLM costs depend on corpus size)",
      "estimated_cost": null,
      "params": { "corpus": "agent-wire-node-repo", "content_type": "code" }
    }
  ],
  "total_estimated_cost": 0,
  "ui_schema": [
    { "type": "corpus_selector", "field": "corpus", "label": "Select source", "multi": false },
    { "type": "text_input", "field": "question", "label": "What do you want to understand?", "placeholder": "e.g. How does the auth system work?" },
    { "type": "cost_preview", "amount": 0 },
    { "type": "toggle", "field": "publish_chain", "label": "Publish this plan to the Wire", "default": false },
    { "type": "confirmation", "summary": "Build a knowledge pyramid and answer your question" }
  ]
}
```

### Execution Model

When the user approves a plan, Sprint 1 executes it LOCALLY via existing Tauri commands:
- `build_pyramid` action → full sequence: (1) resolve corpus name to `source_path` from gathered context's corpora list, (2) check if slug exists via `invoke('pyramid_list_slugs')`, (3) if not, create it via `invoke('pyramid_create_slug', { slug, contentType, sourcePath, referencedSlugs: null })`, (4) then build via `invoke('pyramid_build', { slug })`. The planner's params must include `corpus` name and `content_type`; `source_path` is resolved from the gathered context.
- `search_wire` action → navigates to Search tab with pre-filled query
- `create_task` action → navigates to Fleet > Tasks with pre-filled form
- `manage_agent` action → navigates to Fleet > agent drawer
- `compose_contribution` action → navigates to Compose tab with pre-filled title and body from the plan step's params

This is NOT Wire action chain dispatch yet (Sprint 2). Sprint 1 maps plan steps to local operations.

### Sprint 2 Migration Path (Pillar 17)

The plan format's `steps[].action` field maps to Wire action step concepts:
- `build_pyramid` → `operation: 'wire', tool: 'pyramid_build'`
- `search_wire` → `operation: 'wire', tool: 'wire_query'`
- `create_task` → `operation: 'wire', tool: 'wire_task_create'`
- `manage_agent` → `operation: 'wire', tool: 'wire_agent_status'`
- `compose_contribution` → `operation: 'wire', tool: 'wire_contribute'`

Sprint 2 replaces the local action mapping with Wire action chain dispatch. The plan format's `steps` become `WireActionStep` objects. The `ui_schema` and user inputs become the chain's `input`. This migration is format-preserving — the planner's output structure doesn't change, only the execution path.

---

## Phase 0: Wire Server VALID_TYPES Fix

Add `'action'`, `'skill'`, `'template'` to VALID_TYPES in `GoodNewsEveryone/src/lib/server/contribute-core.ts`.

**Files:**
- `GoodNewsEveryone/src/lib/server/contribute-core.ts` — add 3 values to VALID_TYPES array

Deploy to Wire server. This unblocks publishing the planner action (Sprint 2) and is needed for the Tools tab's Create feature.

---

## Phase 1: Planner Tauri Command (Rust)

New command `planner_call` in `src-tauri/src/main.rs`:

```rust
#[tauri::command]
async fn planner_call(
    state: tauri::State<'_, SharedState>,
    intent: String,
    context: serde_json::Value,
) -> Result<serde_json::Value, String>
```

Implementation:
1. Read LLM config from `state.pyramid.config`
2. Load system prompt from file: `state.pyramid.chains_dir.join("prompts/planner/planner-system.md")` using `std::fs::read_to_string`. Include an inline fallback constant in case the file is missing (follow the pattern in `extraction_schema.rs` lines 69-76 — try load, `warn!` on miss, fall back to a hardcoded default prompt). The prompt template at `planner-system.md` should use `{{WIDGET_CATALOG}}`, `{{AVAILABLE_ACTIONS}}`, and `{{CONTEXT}}` as placeholders. The `planner_call` command reads `WIDGET_CATALOG` from the config (or a shared JSON constant), serializes it, and replaces the placeholder in the loaded template before sending to the LLM. This avoids duplicating the catalog in both the .md file and widget-catalog.ts. `{{AVAILABLE_ACTIONS}}` is interpolated with the list of local actions and their parameter schemas. `{{CONTEXT}}` is interpolated with the user's gathered `PlannerContext` JSON.
3. Build system prompt by interpolating into the loaded template:
   - Replace `{{WIDGET_CATALOG}}` with the serialized widget catalog (what UI components the planner can use)
   - Replace `{{AVAILABLE_ACTIONS}}` with the list of local actions and their parameter schemas
   - Replace `{{CONTEXT}}` with the user's context (pyramids, corpora, agents, balance) — include the `PlannerContext` schema definition so the LLM knows what fields to expect
   - Include instructions for the output schema (steps, costs, ui_schema)
4. Build user prompt: the raw intent string
5. Call `llm::call_model_unified()` with `response_format: Some(&json!({"type": "json_object"}))` for structured output. Use `response_format: Some(&json!({"type": "json_object"}))` for Sprint 1 — this works on all models without schema enforcement. Reserve strict `json_schema` mode for Sprint 2. Set `max_tokens: 2048` (plans are compact).
6. Parse response JSON using `llm::extract_json()` (not raw `serde_json::from_str`) — it handles markdown fences, think tags, and trailing commas. Validate required fields: `steps` (non-empty array), `ui_schema` (array). If JSON parsing fails, retry once with "Please respond with valid JSON only" appended to the user prompt.
7. Return the plan object. Unknown action types in steps produce a per-step user-visible error, not a silent skip or full abort.

Register in `generate_handler![]`.

**Files:**
- `src-tauri/src/main.rs` — add `planner_call` command + register

---

## Phase 2: Widget Components (Frontend)

Create 6 React components that render from `ui_schema` entries:

`src/components/planner/PlanWidgets.tsx` — a single file exporting all widget components + a `PlanWidget` dispatcher:

```tsx
function PlanWidget({ widget, value, onChange, context }: { widget: WidgetSchema, value: any, onChange: (field: string, value: any) => void, context: PlannerContext }) {
    switch (widget.type) {
        case 'corpus_selector': return <CorpusSelector {...widget} value={value} onChange={onChange} options={context.corpora} />;
        case 'text_input': return <TextInputWidget {...widget} value={value} onChange={onChange} />;
        case 'cost_preview': return <CostPreview {...widget} />;
        case 'toggle': return <ToggleWidget {...widget} value={value} onChange={onChange} />;
        case 'agent_selector': return <AgentSelector {...widget} value={value} onChange={onChange} agents={context.agents} />;
        case 'confirmation': return <ConfirmationWidget {...widget} />;
        default: return null; // Unknown widget type — silently skip
    }
}
```

Widget details:
- **CorpusSelector** — dropdown of available corpora. Reads options from `context.corpora` (passed as prop). Multi-select optional.
- **TextInputWidget** — labeled text input with placeholder.
- **CostPreview** — shows estimated cost with "estimated" label. Shows current balance for context. Aware that costs are approximate (governor-ready). **Note:** Query costs are governor-dynamic and only known at query time (cannot be pre-estimated). Action costs are quotable via spot quote (mode='quote') which returns a binding price + TTL. The CostPreview should distinguish these two cost types.
- **ToggleWidget** — checkbox/toggle with label.
- **AgentSelector** — dropdown of fleet agents. Reads options from `context.agents` (passed as prop). Multi-select optional.
- **ConfirmationWidget** — summary text + "Approve" and "Cancel" buttons.

**Files:**
- New: `src/components/planner/PlanWidgets.tsx`
- `src/styles/dashboard.css` — add `.plan-widget-*` styles

---

## Phase 3: IntentBar Rewrite (Frontend)

Replace the IntentBar stub with the real planner flow:

### PlannerContext Schema

The gathering step produces a `PlannerContext` that flows through the entire pipeline — it's passed to the planner LLM call and to PlanWidget components:

```typescript
interface PlannerContext {
    pyramids: { slug: string; node_count: number; content_type: string }[];
    corpora: { slug: string; path: string; doc_count: number }[];
    fleet: { online_count: number; task_count: number };
    agents: { id: string; name: string; status: string }[];
    balance: number;
}
```

Keep agent data to pulse-level summary (not full roster). The `agents` array comes from a separate roster call for widget population. Include this schema in the planner system prompt so the LLM knows what to expect.

Define `PlannerContext` and `PlanStep` types in a shared file `src/types/planner.ts` (not inline in IntentBar.tsx). Both IntentBar and PlanWidgets import from this shared location.

### State Management

Use `useState<IntentBarState>` with a discriminated union:

```typescript
type IntentBarState =
  | { phase: 'idle' }
  | { phase: 'gathering', intent: string }
  | { phase: 'planning', intent: string }
  | { phase: 'preview', intent: string, plan: PlanResult, context: PlannerContext }
  | { phase: 'executing', intent: string, plan: PlanResult, currentStep: number }
  | { phase: 'complete', intent: string, result: unknown }
  | { phase: 'error', intent: string, error: string };
```

Disable the Go button and input field in ALL non-idle phases (gathering, planning, preview, executing, complete, error). Only the Idle phase accepts new input. This prevents double-submit during LLM calls. Auto-collapse preview panel on `activeMode` change via `useEffect`.

The gathering and planning phases inherit the LLM timeout from config (`base_timeout_secs`, default 120s). If the LLM call times out, transition to Error state with a 'Planning timed out -- try again' message. Add a Cancel button visible during gathering and planning phases that aborts the operation and returns to Idle.

### Flow:
1. User types intent, hits Go
2. **Gather context** (Pillar 25 — via API). Use `Promise.allSettled` for the 3+ gathering calls. Include successful results in context. For failed calls, include `{ failed: true, source: 'pulse' }` (or whichever source) so the planner knows what context is missing.
   - `invoke('pyramid_list_slugs')` → pyramids
   - `invoke('get_sync_status')` → corpora/docs
   - `wireApiCall('GET', '/api/v1/wire/pulse')` → fleet/tasks/balance
   - `wireApiCall('GET', '/api/v1/wire/roster')` → agent roster (for AgentSelector widget)
   - Transform SyncState to PlannerContext.corpora: group `syncState.cached_documents` by their linked folder's corpus slug, count documents per corpus, extract the folder path. Result: `{ slug, path, doc_count }[]`.
   - Show user what info is being gathered (transparency card)
   - IntentBar keeps `gatheredContext` in state alongside the plan (stored in the `preview` phase of `IntentBarState`)
3. **Call planner**: `invoke('planner_call', { intent, context: gatheredContext })`
4. **Render plan**: map `ui_schema` to PlanWidget components. Pass `context: PlannerContext` prop to the `PlanWidget` dispatcher alongside `ui_schema`. CorpusSelector reads `context.corpora` for its dropdown options. AgentSelector reads `context.agents` for its dropdown options. User fills in inputs.
5. **On approve**: execute plan steps locally:
   - Map each step's `action` to a local operation
   - Push an `OperationEntry` to AppContext `activeOperations`
   - Execute via existing Tauri commands
   - Update progress in Operations > Active
6. **On complete**: show result link, move to Operations > Completed

### AppContext additions:
- `activeOperations: OperationEntry[]` — array of running/completed operations
- `OperationEntry` type: `{ id, intent, status: 'running'|'completed'|'failed', steps: PlanStep[], currentStep, result?, error? }`
- Actions: `ADD_OPERATION`, `UPDATE_OPERATION`, `COMPLETE_OPERATION`

### IntentBar layout:
- The plan preview renders BELOW the intent bar as an expandable panel (not a modal), inside `.intent-bar-wrapper` with `max-height: 50vh; overflow-y: auto`
- No AppShell.tsx layout changes needed — the flex layout handles expansion naturally
- Dismissible via Cancel or clicking away
- On approve, collapses to a progress indicator

**Files:**
- `src/components/IntentBar.tsx` — full rewrite
- `src/contexts/AppContext.tsx` — add activeOperations + OperationEntry + actions

---

## Phase 4: Operations > Active (Frontend)

Replace the empty state in OperationsMode's Active sub-tab with real operation tracking:

- Reads `activeOperations` from AppContext
- Each operation shows: intent text, current step / total steps, status indicator, elapsed time
- Completed operations show result link (e.g., "Pyramid built → View in Understanding") and a "Dismiss" button
- Failed operations show error + Retry button
- `OperationEntry.steps` typed as `PlanStep[]` matching the plan format
- Operations persist in state for the session (not across restarts for Sprint 1)

**Files:**
- `src/components/modes/OperationsMode.tsx` — Active sub-tab implementation

---

## Phase 5: Planner System Prompt (Rust)

The planner's system prompt is critical — it defines what the planner can do and how it thinks. This is itself a contribution (Pillar 28 — the recipe is improvable).

The prompt includes:
1. **Role**: "You are the Wire Node intent planner. You take a user's natural language intent and produce a structured execution plan."
2. **Available actions**: list of local actions the planner can use (build_pyramid, search_wire, create_task, manage_agent, compose_contribution) with their parameters
3. **Widget catalog**: the full WIDGET_CATALOG array so the planner knows what UI components exist
4. **Context**: user's pyramids (names, types, node counts), corpora (slugs, doc counts), agents (names, online status), credit balance
5. **Output format**: exact JSON schema for the plan object
6. **Guidelines** (Pillar 37 — describe goals, not prescriptions):
   - The plan should give the user enough information to make an informed decision before committing (Pillar 23). Use the widget catalog to build the right preview for the intent.
   - Cost estimates are approximate — pricing is dynamic (governor-adjusted). Show what you can estimate and label it as estimated. Governor costs cannot be pre-estimated (the governor increments on each query call). The planner should say "query costs are dynamic" rather than trying to predict them.
   - The user should understand what will happen, what it will cost, and what they'll get back.
   - Let the planner intelligence decide which widgets serve each intent — don't prescribe specific widgets for every plan.
   - Pyramid builds consume LLM tokens proportional to corpus size. If you cannot estimate the cost precisely, set `estimated_cost` to `null` and note "LLM costs depend on corpus size" in the step description. Never show cost as 0 for operations that consume resources.

Store the prompt as a `.md` file at `chains/prompts/planner/planner-system.md` — loaded at runtime (Pillar 28: prompt files as contributions, iterable without recompiling). The prompt is versioned and publishable — when someone improves the planner's reasoning (handles more intents, produces better plans), that improvement is a contribution to the Wire.

In dev mode, this file is found at `{repo_root}/chains/prompts/planner/planner-system.md`. In release mode, `chains_dir` resolves to `{data_dir}/chains/` — the .md file won't be found there. The inline fallback constant (Phase 1 step 2) always fires in release. To make the file available in release: add `planner-system.md` to the `ensure_default_chains()` bootstrap in `src-tauri/src/pyramid/chain_loader.rs` (add `prompts/planner` to the `dirs_to_create` array, and write the prompt content like existing chain YAMLs).

**Files:**
- New: `chains/prompts/planner/planner-system.md` — planner system prompt
- `src-tauri/src/main.rs` — load prompt from file in `planner_call`
- `src-tauri/src/pyramid/chain_loader.rs` — add planner prompt to `ensure_default_chains()` bootstrap

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 0 | Wire server VALID_TYPES fix | Tiny | — |
| 1 | Planner Tauri command | Medium | — |
| 2 | Widget components | Medium | — |
| 3 | IntentBar rewrite | Large | Phase 1 + 2 |
| 4 | Operations Active | Small | Phase 3 |
| 5 | Planner system prompt | Small | Phase 1 |

Phases 0, 1, 2, 5 can run in parallel. Phase 3 needs 1 + 2. Phase 4 needs 3.

## Audit Gates (Pillar 39)

Serial verifier after each phase. Audit until clean.

---

## Key Files

| File | Changes |
|------|---------|
| `GoodNewsEveryone/src/lib/server/contribute-core.ts` | Add action/skill/template to VALID_TYPES (P0) |
| `src-tauri/src/main.rs` | `planner_call` command + register (P1) |
| New: `src/types/planner.ts` | PlannerContext + PlanStep shared types (P2) |
| New: `src/components/planner/PlanWidgets.tsx` | 6 widget components + dispatcher (P2) |
| `src/components/IntentBar.tsx` | Full rewrite — planner integration (P3) |
| `src/contexts/AppContext.tsx` | activeOperations, OperationEntry, actions (P3) |
| `src/components/modes/OperationsMode.tsx` | Active sub-tab real content (P4) |
| New: `chains/prompts/planner/planner-system.md` | Planner system prompt (P5) |
| `src/styles/dashboard.css` | Plan widget + intent bar expansion styles |

## Verification

1. Type "How does auth work in my codebase?" → context gathered → planner returns plan → corpus selector + question input + cost preview shown
2. Fill in corpus, type question, approve → pyramid build starts → appears in Operations > Active → completes → link to Understanding
3. Type "Search the Wire for battery chemistry" → planner returns search plan → approve → navigates to Search with pre-filled query
4. Type "Archive agent Ember-Beta" → planner returns fleet action → confirmation widget → approve → agent archived
5. Type gibberish → planner returns "I don't understand" or best-effort interpretation
6. Cost preview shows "estimated" not exact amounts
7. Planner system prompt loads from .md file (not hardcoded)
8. `npx tsc --noEmit` + `cargo check` pass
9. Widget catalog in planner prompt matches widget-catalog.ts
