# Sprint 4 — Auto-Fulfill (ALPHA MILESTONE)

## Context

Sprints 1-3 built the intent bar, chain publishing, and remote pyramid access. But execution requires the user to manually approve every plan. Sprint 4 completes the alpha loop by adding auto-execute for safe plans, cost estimation, and fleet task delegation.

Platform helpers (ephemeral agents managed by the Wire server) are deferred — zero helper infrastructure exists, `executeLlmStep` returns 501. Hook point documented in IntentBar.tsx.

## The Alpha Loop (complete after Sprint 4)

1. User types intent → planner builds plan with named vocabulary commands
2. **Safe plan** (navigation, read-only): auto-executes immediately if toggle ON
3. **Effectful plan** (costs, writes, builds): always shows preview for approval (Pillar 23)
4. Node executes locally via vocabulary registry dispatch
5. Result in Operations with per-step detail
6. Chain optionally published to Wire

---

## Phase 1: Auto-Execute Toggle + Safety Tiers

### Safety Classification (Pillar 23 conformance)

Auto-execute ONLY skips preview for plans where ALL steps are **safe-tier**. Effectful plans always preview regardless of the toggle.

**Safe tier** (auto-executable): navigation commands (`go_to_*`), read-only queries (`pyramid_list_slugs`, `pyramid_apex`, `pyramid_search`, `list_operator_agents`, `get_sync_status`, etc.)

**Effectful tier** (always preview): Wire API writes (`wire_contribute`, `archive_agent`, `create_task`), pyramid builds (`pyramid_build`, `pyramid_question_build`), mutations (`pyramid_delete_slug`, `update_agent_status`), any command that spends credits.

Classification: check each step's command against the vocabulary registry dispatch type. `navigate` → safe. `tauri` commands in a known safe-list → safe. `wire_api`/`operator_api` → effectful. Unknown → effectful.

### Config + State

Add `auto_execute: boolean` to **`PyramidConfig`** in `src-tauri/src/pyramid/mod.rs` with `#[serde(default)]` (defaults to `false`). Persisted via existing `pyramid_config.json` save/load. NOT in WireNodeConfig (that's system infrastructure config).

Frontend state:
1. New action type `SET_AUTO_EXECUTE` with `{ enabled: boolean }` in `AppAction` union
2. New `autoExecute: boolean` field in `AppState` (default `false`)
3. Hydration: on AppShell mount, call `invoke('pyramid_get_config')` → set `autoExecute` from response
4. Toggle dispatch: PyramidSettings calls `invoke('pyramid_set_config', { autoExecute: value })` → dispatches `SET_AUTO_EXECUTE`

### IntentBar Changes

Extract execution logic from `handleApprove` into a shared `executePlan(intent, plan, context, widgetValues)` function. Both `handleApprove` and the auto-execute path call it. This is necessary because `handleApprove` reads `barState.phase === 'preview'` which won't be set in the auto-execute path (React state batching issue).

After planning completes (where `setBarState({ phase: 'preview', ... })` currently is):
```
if (state.autoExecute && isAllSafeTier(plan.steps, vocabRegistry)) {
    // Safe plan + auto-execute ON → skip preview, execute immediately
    await executePlan(intent, plan, context, {});
} else {
    // Effectful plan OR auto-execute OFF → show preview for approval
    setBarState({ phase: 'preview', intent, plan, context });
}
```

**Note:** In auto-execute mode, `widgetValues` are empty (user had no chance to fill widgets). Safe-tier plans typically have no interactive widgets. If a safe plan somehow has widgets, it falls through to preview.

### Cancel During Execution

Add cancel button to the executing phase UI. Add `if (cancelRef.current) break;` at the top of the execution loop before each step. Mark cancelled operations as 'cancelled' not 'failed'. Critical for auto-execute since user had no preview.

### Auto-Execute + Publish Toggle

When auto-execute skips preview, the publish toggle is never shown. `widgetValues['publish_chain']` is `undefined`, so the `=== true` check correctly skips publishing. Auto-executed plans are never published unless a future `auto_publish` setting is added. Acceptable for alpha.

**Files:**
- `src-tauri/src/pyramid/mod.rs` — add `auto_execute` to PyramidConfig
- `src-tauri/src/main.rs` — extend pyramid_set_config/pyramid_get_config to handle auto_execute
- `src/contexts/AppContext.tsx` — SET_AUTO_EXECUTE action, autoExecute state field
- `src/components/IntentBar.tsx` — extract executePlan, add safety tier check, cancel during execution
- `src/components/PyramidSettings.tsx` — auto-execute toggle
- `src/components/Sidebar.tsx` — auto-execute indicator
- `src/components/AppShell.tsx` — hydrate autoExecute on mount

---

## Phase 2: Cost Estimation (Local)

Local cost classification — NOT quote engine integration. The quote engine requires a published action ID which Sprint 4 plans don't have. Local estimation is sufficient for alpha.

Classification per dispatch type:
- `navigate` → "Free"
- `tauri` (local commands) → "Free" or "Local LLM cost" (for pyramid_build, pyramid_question_build)
- `wire_api` queries → "Dynamic (governor-adjusted)" — cannot predict exact cost
- `wire_api` writes → "Wire credit cost" — deposit for contributions, stamp for remote queries
- Unknown → "Cost varies"

Enhance existing CostPreview widget in PlanWidgets.tsx to show per-step classification and aggregate.

**Files:**
- `src/components/IntentBar.tsx` — classify steps after planning, populate cost data
- `src/components/planner/PlanWidgets.tsx` — enhance CostPreview with per-step breakdown

---

## Phase 3: Fleet Task Posting

For operators with fleet agents who want to delegate work:

When auto-execute is OFF and fleet agents are online (use `context.fleet.online_count` from just-gathered planning context, not stale `state.fleetOnlineCount`), show "Post to Fleet" alongside "Execute Locally" in the confirmation widget.

### Task body mapping

Plan steps map to Wire tasks as:
- `title`: step.description (max 500 chars)
- `context`: JSON.stringify({ command, args, plan_id, step_index, on_error })
- `priority`: 'normal' (or 'high' if on_error === 'abort')

Fleet task execution is **manual delegation** — agents see the task, understand the command from context, and execute it themselves. This is NOT automatic offloading. The plan should set this expectation.

### Error handling

If any task POST fails (e.g., 409 task limit), show which steps were posted and which failed. Allow retry of failed posts.

### Polling

Poll `GET /api/v1/wire/tasks/{taskId}` every 10 seconds for each posted task. Map task state (`backlog`→pending, `claimed`→in_progress, `active`→executing, `complete`→done, `archived`→dismissed) to operation step status in the Operations tab.

**Files:**
- `src/components/IntentBar.tsx` — "Post to Fleet" button + task creation
- `src/components/planner/PlanWidgets.tsx` — fleet posting option in confirmation area
- `src/components/modes/OperationsMode.tsx` — fleet task status display

---

## Phase 4: End-to-End Polish

1. Operation results persist through tab switches (already working)
2. "Retry from step N" for failed operations (retry the whole plan from the failed step onward — simpler than per-step retry, doesn't require persisting full execution context)
3. Post-execution summary: steps completed, steps failed, credits spent
4. Published chains visible in Tools tab (verify Sprint 2 integration works)
5. Auto-execute indicator in sidebar (subtle icon/text when ON)

**Files:**
- `src/components/modes/OperationsMode.tsx` — retry from step, summary
- `src/components/Sidebar.tsx` — auto-execute indicator

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Auto-execute toggle + safety tiers + cancel | Medium | None |
| 2 | Local cost estimation | Small | None |
| 3 | Fleet task posting | Medium | Phase 1 |
| 4 | End-to-end polish | Small | Phases 1-3 |

---

## Verification (ALPHA ACCEPTANCE CRITERIA)

1. **Safe plan + auto-execute ON**: "go to fleet" → executes immediately, no preview shown
2. **Effectful plan + auto-execute ON**: "build a pyramid" → preview shown with cost estimate, user must approve
3. **Auto-execute OFF**: all plans show preview (current behavior)
4. **Fleet posting**: operator with agents sees "Post to Fleet" → tasks appear in Fleet > Tasks
5. **Cancel**: user can cancel mid-execution → remaining steps skipped, operation marked cancelled
6. **Cost transparency**: CostPreview shows per-step classification (free/dynamic/LLM cost)
7. **Settings**: auto-execute toggle in Settings, persists across restarts
8. **Pillar 23**: effectful plans ALWAYS preview regardless of toggle
9. `cargo check` + `npx tsc --noEmit` pass

## Deferred (tracked)

- **Platform helpers** — multi-sprint server infrastructure. Hook in IntentBar.tsx.
- **Binding cost quotes** — requires published action IDs. Local estimation for alpha.
- **Auto-publish** — setting to always publish auto-executed plans. Future.
- **No-node consumer path** — Sprint 5 (Vibesmithy standalone).

## Audit Trail

**Audit round 1 (original Sprint 4, 2 auditors):**
- Plan split: helpers deferred, scope narrowed to node-local alpha
- Both auditors recommended Sprint 4a (local) vs 4b (helpers)

**Audit round 2 (rewritten Sprint 4, 2 auditors, 12+12 issues):**
- C1+C2: Wrong config struct → PyramidConfig, not WireNodeConfig
- C3: Quote section mislabeled → renamed to "Cost Estimation (Local)"
- C4 (Pillar 23): Auto-execute needs safety tiers → safe vs effectful classification added
- M1: Fleet task shape mismatch → explicit body mapping specified
- M2: AppState hydration → full action type + load path specified
- M3: IntentBar refactor → extract executePlan function
- M4: Cancel during execution → added to Phase 1
- M5: Auto-execute + publish interaction → documented, auto_publish deferred
- M6: No cost logic → dispatch-type classification specified
- m1: Fleet count source → use context.fleet.online_count
- m2: Retry mechanism → "retry from step N" approach
- m3: Auto-re-quote orphaned bullet → removed
