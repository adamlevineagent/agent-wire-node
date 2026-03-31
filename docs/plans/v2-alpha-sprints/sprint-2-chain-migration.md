# Sprint 2 — Planner to Wire Action + Chain-as-Contribution

## Context

Sprint 1 shipped the intent bar planner with named vocabulary commands (vocabulary registry). Sprint 2 publishes the planner definition as a Wire action contribution and closes the consume-transform-contribute loop (Pillar 36) by auto-publishing executed plans as reusable chain contributions.

## Prerequisites (all met)

- ✅ Sprint 1 shipped: planner_call works with vocabulary registry, named commands, widget catalog, Operations tracking
- ✅ Vocabulary registry shipped: YAML action definitions, dispatch translation, allowlist security
- ✅ Wire server: VALID_TYPES includes 'action', 'skill', 'template'
- ✅ Wire server: quote primitive deployed to prod
- ✅ Wire server: query governor deployed to prod

## Architecture Decisions

### Option C: Local execution, published definition

The planner runs locally (fast, private, uses node's OpenRouter key). The planner prompt/definition is published as an action contribution (forkable, citable, discoverable). Executed plans are published as chain contributions (reusable).

**What this gives us:**
- The planner becomes forkable — someone can publish a better planner
- The planner's improvements are tracked via supersession (Pillar 5)
- Executed plans become reusable chain templates on the Wire
- Citation royalties when others access the planner or chains

**What this defers (tracked debt):**
- The planner does NOT earn execution revenue under Option C (no Wire invocation = no UFF event). Revenue is citation-only.
- Full remote execution (planner invocable by other chains, remotely callable) requires Wire server LLM execution support — deferred to Sprint 3+.
- Binding cost quotes via the quote primitive are deferred to Sprint 3 — the quote engine can't cost-walk a locally-executed action. Sprint 2 shows local cost estimates per step instead.
- External chain execution security (sandboxing, permission categories, cost caps) is deferred — Sprint 2 only publishes chains, Sprint 3+ enables discovering and executing other people's chains. Security model ships with the feature that needs it.

### Application code vs plan steps

Post-execution publishing (Phase 2) is **application code** in IntentBar.tsx using the `wireApiCall` context wrapper. It is NOT a planner-generated plan step. The planner LLM cannot generate steps that call `wire_api_call` directly — that command is in BLOCKED_COMMANDS. If publishing ever moves into the planner's repertoire, it must use the `submit_contribution` vocabulary command.

---

## Phase 1: Publish Planner Action to Wire

Publish the planner definition as a Wire action contribution via `POST /api/v1/contribute`:

```json
{
  "type": "action",
  "title": "Wire Node Intent Planner v1",
  "body": "<JSON.stringify'd action definition>",
  "topics": ["planner", "intent", "wire-node"]
}
```

Note: `body` must be a JSON string (the contribute endpoint validates `isNonEmptyString`), not a raw JSON object. The action definition is serialized via `JSON.stringify()`. Max 50KB. Do NOT include `significance` — it is computed server-side.

The action definition body contains:
- `format_version`: 1
- `prompt_source`: the planner-system.md content (or hash/reference)
- `input_schema`: `{ intent: string, context: PlannerContext }`
- `output_schema`: `{ plan_id, steps, total_estimated_cost, ui_schema }`
- `execution`: `"local"` (marks this as locally-executed, not Wire-executable)

Store the returned contribution UUID as `PLANNER_ACTION_ID` in `src/config/wire-actions.ts`.

**Supersession strategy:** When the planner prompt changes materially, publish a new action contribution that supersedes the previous one via `derived_from` link, and update `PLANNER_ACTION_ID`. For Sprint 2 this is manual; Sprint 3+ can automate detection.

**UX (Pillar 42):** After publishing, show a notification in the Tools tab: "Planner v1 published to Wire" with the contribution link. The published planner appears in Tools > My Tools.

**Publish method:** Use `POST /api/v1/contribute` with the node's agent token (Pillar 25 — platform agents use the public API). No admin scripts or direct DB inserts.

**Files:**
- `src/config/wire-actions.ts` — store PLANNER_ACTION_ID
- One-time publish script or startup-time check

---

## Phase 2: Post-Execution Chain Publishing

After a plan executes successfully, offer to publish it as a reusable chain contribution.

**Publish toggle:** Add a `toggle` widget to the plan preview UI with `field: "publish_chain"`, `label: "Publish to Wire"`, defaulting to OFF. This is a new widget instance in the confirmation area — it does not currently exist and must be created.

**Informed consent (Pillar 23):** When the toggle is ON and the user clicks Approve, show a brief confirmation of what will be published: the chain title, step count, and a note that it becomes public and forkable. If the Wire charges a contribution deposit, show that cost.

**Post-execution flow (application code in IntentBar.tsx):**

1. Plan executes successfully (all steps complete or continue-on-error)
2. If `publish_chain` toggle was ON:
   - Generate title from the original intent text, truncated to 200 chars
   - Package the chain definition (NOT execution results — just the reusable recipe):
     ```json
     {
       "format_version": 1,
       "steps": [plan steps with command names and arg schemas],
       "input_schema": [extracted from ui_schema widgets],
       "required_commands": ["list_operator_agents", "archive_agent"]
     }
     ```
   - Call `wireApiCall('POST', '/api/v1/contribute', { type: 'action', title, body: JSON.stringify(chainDef), derived_from: [...] })` — this is application code, not a plan step
   - Show the contribution ID to the user ("Your plan was published to the Wire")
   - If publish fails (network error, server down), show error but do NOT fail the operation. Offer retry.

**derived_from links (Pillar 3 — strict validation):**

Only include links for assets that have Wire contribution UUIDs. Local-only pyramids/corpora are omitted.

Format per the contribute endpoint's validation:
```json
{
  "source_type": "contribution",
  "source_item_id": "<contribution UUID>",
  "weight": 0.5,
  "justification": "Chain queries this pyramid",
  "dead_end": false
}
```

**Chain validation before publishing:**
- Every step's command must exist in the vocabulary registry
- No step may use a BLOCKED_COMMANDS command
- Reject publication of chains containing unknown or blocked commands

**Files:**
- `src/components/IntentBar.tsx` — post-execution publish flow (application code using wireApiCall wrapper)
- `src/components/planner/PlanWidgets.tsx` — publish toggle widget instance in confirmation area

---

## Phase 3: Local Cost Estimation (Quote Mode Deferred)

~~Use the Wire's quote primitive for binding cost previews.~~

**Deferred to Sprint 3.** The quote engine cannot cost-walk a locally-executed action. Binding quotes require the Wire server to execute the action's steps, which requires `executeLlmStep` implementation (Sprint 3+).

**Sprint 2 approach:** Show local cost estimates per step in the CostPreview widget:
- Wire queries: "dynamic (governor-priced)" — cannot predict exact cost
- Pyramid builds: "LLM costs depend on corpus size" — proportional to OpenRouter token cost
- Wire contributions: show deposit cost if known
- Navigation steps: free

The CostPreview widget already supports `estimated_cost: null` with text descriptions. No new server integration needed.

---

## Phase 4: Tools Tab Shows Published Chains

When a chain is published (Phase 2), it appears in Tools > My Tools:

1. ToolsMode queries the Wire for the operator's published actions via `wireApiCall('POST', '/api/v1/wire/query', { q: 'type:action', ... })` or `wireApiCall('GET', '/api/v1/wire/my/contributions', {})`
2. Each published tool shows: title, step count, and creation date. Citation count and revenue shown if the API provides them; otherwise "available soon."
3. The user can view the chain definition
4. Update stale placeholder text ("Coming in Sprint 1" → current copy)

**Note:** Discovering and executing OTHER people's chains is Sprint 3+ (requires chain sandboxing/security model). Sprint 2 only shows YOUR published chains.

**Files:**
- `src/components/modes/ToolsMode.tsx` — async data fetching, loading/error/empty states, merge local + Wire tools
- `src/config/wire-actions.ts` — update LOCAL_TOOLS

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Publish planner action | Small | VALID_TYPES deployed ✅ |
| 2 | Post-execution chain publishing | Medium | Phase 1 |
| 3 | Local cost estimation | Small | None (uses existing CostPreview) |
| 4 | Tools tab shows published chains | Medium | Phase 2 |

---

## Vocabulary Prerequisites

Before building, update `chains/vocabulary_yaml/wire_compose.yaml`:
- Add `action`, `skill`, `template` to the `submit_contribution` type param description
- The LLM and vocabulary must know that action-type contributions are valid

Future (Sprint 3+): Add vocabulary commands for `wire_action_quote` and `wire_action_invoke` when remote execution ships.

---

## Verification

1. Planner action published to Wire with real UUID → PLANNER_ACTION_ID updated in config
2. Notification shown in Tools tab after planner publish
3. Execute plan with publish toggle ON → chain appears on Wire as contribution → visible in Tools > My Tools
4. Execute plan with publish toggle OFF → no contribution published
5. Publish failure → operation still shows ✓ Complete, error shown with retry option
6. Published chain validated: all commands in vocabulary, none in BLOCKED_COMMANDS
7. derived_from links have correct format (source_type, source_item_id, weight, justification, dead_end)
8. CostPreview shows local estimates (dynamic for queries, null for builds)
9. Tools tab loads published chains from Wire, shows loading/empty states
10. `cargo check` + `npx tsc --noEmit` pass

## Deferred (tracked debt)

- **Binding cost quotes** — deferred to Sprint 3 when Wire server LLM execution exists
- **External chain execution** — deferred to Sprint 3+ with sandboxing/security model
- **UFF execution revenue** — deferred to Sprint 3+ (Option C = citation royalties only)
- **Planner supersession automation** — deferred (manual PLANNER_ACTION_ID update for now)
- **Action discovery in planner** — Sprint 3+ ("You don't have a pyramid for X, but a chain exists on the Wire")

## Audit Trail

**Stage 1 pre-implementation audit (2 independent auditors, 21 findings merged):**
- Phase 3 quote mode redesigned → deferred to Sprint 3, replaced with local cost estimates
- Chain security (sandboxing) → explicitly deferred with risk documented
- wireApiCall application code vs plan steps → clarified in Architecture Decisions section
- submit_contribution vocabulary → must update before building (prerequisite)
- derived_from format → fully specified with all required fields
- Publish toggle → does not exist yet, added to Phase 2 scope
- body field → must be JSON.stringify'd string, documented
- Phase 4 endpoint → corrected to POST or my/contributions
- Chain body schema → defined with format_version, steps, input_schema, required_commands
- PLANNER_ACTION_ID supersession → manual update strategy documented
- UFF claim → corrected (citation royalties only under Option C)
- Phase 1 UX → added notification requirement (Pillar 42)
- Publish failure handling → added retry flow
- Phase 4 metrics → scoped to what API provides
- significance field → removed (server-computed)
- Stale placeholder copy → noted for update
