# Sprint 0 — Wire Node v2 Tab Restructuring + Live Sidebar

## Context

Wire Node v1.1 has 9 tabs organized by data type (Pyramids, Network, Search, Compose, Fleet, Node, Activity, Identity, Settings). The v2 vision reorganizes around the user's mental model: Your World (what you have), In Motion (what's happening), The Wire (the network), You (account).

This sprint restructures tabs, merges Knowledge (Corpora + Sync), merges Operations (Fleet + Activity), renames Pyramids → Understanding, and builds the live status sidebar. **The intent bar + planner is Sprint 1** — Sprint 0 is the structural foundation only.

## Current → New Tab Mapping

**Current (9 tabs):**
1. Pyramids, 2. Network, 3. Search, 4. Compose, 5. Fleet, 6. Node, 7. Activity, 8. Identity, 9. Settings

**Revised (10 tabs, 4 sections):**

| # | Section | Tab label | Mode key | Contains |
|---|---------|-----------|----------|----------|
| 1 | YOUR WORLD | Understanding | `pyramids` | Pyramids (relabeled) |
| 2 | YOUR WORLD | Knowledge | `knowledge` | NEW: Corpora + Local Sync |
| 3 | YOUR WORLD | Tools | `tools` | NEW: Actions, chains, skills, templates — create, discover, publish, monitor |
| 4 | IN MOTION | Fleet | `fleet` | Fleet Overview + Tasks + Coordination (no Corpora) |
| 5 | IN MOTION | Operations | `operations` | NEW: merges Activity content. Active chains (future), Completed, Queue (future), Messages, Notifications |
| 6 | THE WIRE | Search | `search` | Unchanged |
| 7 | THE WIRE | Compose | `compose` | Unchanged |
| 8 | THE WIRE | Network | `dashboard` | Dashboard + Market + Infrastructure (Remote + Logs) |
| 9 | YOU | Identity | `identity` | Unchanged |
| 10 | YOU | Settings | `settings` | Unchanged |

**Changes from current:**
- ADD mode `knowledge`, `tools`, and `operations`
- REMOVE mode `node` and `activity`
- Rename sidebar labels: Pyramids→Understanding, Mesh→Coordination in Fleet sub-tabs
- Network absorbs Node's Market, Remote, Logs as sub-tabs
- Knowledge gets Corpora + Sync as sub-tabs
- Operations replaces Activity with same content + future active/queue views

**Intent bar:** A text input fixed at the top of the content area (above the current mode content). For Sprint 0, it is a UI placeholder only (shows 'Coming soon' on submit). Sprint 1 adds the intelligent planner to understand intent, build a plan, preview it, and execute it.

**Three pillar requirements for the intent bar:**

1. **Pillar 2/28 (contributions all the way down):** The planner's output (the plan) is structured as chain-compatible data from day one. Not throwaway JSON — a format that CAN be contributed to the Wire. Publishing is optional (user controls via a toggle, default configurable). The plan is a contribution in format even if not published. When published, it becomes a reusable action chain others can fork.

2. **Pillar 17 (chains invoke chains):** The planner IS an action chain, not an inline LLM call from React. The Wire server already has full action chain infrastructure (`POST /api/v1/wire/action/invoke` and `/chain` — confirmed live). The planner runs through this chain dispatch, making it publishable, forkable, and improvable. This is a Sprint 1 capability — Sprint 0 establishes the UI and data structures only.

3. **Pillar 25 (platform agents use the public API):** Planner context (pyramids, corpora, agents, balance) is gathered via API calls — the same endpoints agents use — not by reading React state. The planner gets the same view of the world any agent would have. The user is shown what information is being provided to the planner and can approve/deny.

**Plan presentation:** The planner produces a custom UI for each plan — not a generic "steps + cost" card. The plan UI adapts to the intent: a pyramid build plan shows corpus selection + question input + layer options. A search plan shows topic refinement + cost preview. A fleet action shows agent selection + confirmation. This is rendered in a definable space (modal or inline panel) that the planner's output defines. The user fills in any required inputs, reviews the plan, and approves.

---

## Phase 0: Planner Action Provisioning (prerequisite for Sprint 1)

The planner action schema, config files, and widget catalog are defined here for reference. The actual planner implementation (LLM calls, action creation, seeding) is Sprint 1.

### Planner action schema (Wire contribution)

The planner action is a Wire contribution with `type='action'`:
```
{
  type: 'action',
  subtype: 'planner',
  title: 'Intent Planner',
  description: 'Takes user intent + context, returns a structured plan with steps, costs, and ui_schema.',
  input_schema: { intent: 'string', context: 'object' },
  output_schema: { steps: 'array', total_estimated_cost: 'number', ui_schema: 'object', publish_as_chain: 'boolean' }
}
```

### Wire protocol fix: VALID_TYPES must include intelligence primitives

The contribute endpoint (`contribute-core.ts`) rejects `type='action'` because VALID_TYPES doesn't include it. This is a protocol-level bug — Pillar 19 defines actions, skills, and templates as first-class intelligence primitives. They should be publishable through the same contribute endpoint as everything else.

**Fix:** Add `'action'`, `'skill'`, `'template'` to VALID_TYPES in `GoodNewsEveryone/src/lib/server/contribute-core.ts`. This is a one-line change. Deploy to Wire server before Sprint 1 (when the planner action needs to be published).

### Seed script

Seed script runs in Sprint 1 (not Sprint 0). For Sprint 0, `src/config/wire-actions.ts` contains hardcoded planner action metadata (title, description, type, placeholder UUID). No database seeding.

### Config file: `src/config/wire-actions.ts`

For Sprint 0, this file contains hardcoded planner action metadata (title, description, type, placeholder UUID) used by the Tools tab. No seed script, no Wire query. Sprint 1 replaces the placeholder UUID with a real one from the seed script.

```ts
export const PLANNER_ACTION_ID = '<placeholder-uuid>';
export const PLANNER_ACTION_META = {
  title: 'Intent Planner',
  description: 'Takes user intent + context, returns a structured plan with steps, costs, and ui_schema.',
  type: 'action',
  subtype: 'planner',
};
```

### Widget catalog: `src/config/widget-catalog.ts`

Define and export the initial 6 widget types with their prop schemas:

```ts
export const WIDGET_CATALOG = [
  { type: 'corpus_selector', description: 'Select from available corpora', props: { multi: 'boolean', filter: 'string?' } },
  { type: 'text_input', description: 'Free text input field', props: { field: 'string', label: 'string', placeholder: 'string?' } },
  { type: 'cost_preview', description: 'Show estimated cost breakdown', props: { amount: 'number', breakdown: 'object?' } },
  { type: 'toggle', description: 'Boolean toggle with label', props: { field: 'string', label: 'string', default: 'boolean?' } },
  { type: 'agent_selector', description: 'Select from available agents', props: { multi: 'boolean', filter: 'string?' } },
  { type: 'confirmation', description: 'Review and confirm action', props: { summary: 'string', details: 'string?' } }
] as const;
```

The planner LLM sees this catalog and produces `ui_schema` entries referencing these types. New widget types are added here as new plan types emerge.

---

## Phase 1: Mode Type + Sidebar + Router (atomic)

All changes ship together (TypeScript will error if modes are removed but still referenced).

### AppContext.tsx
- Mode type: `'pyramids' | 'knowledge' | 'tools' | 'fleet' | 'operations' | 'search' | 'compose' | 'dashboard' | 'identity' | 'settings'`
- Remove: `'node'`, `'activity'`
- Add: `'knowledge'`, `'tools'`, `'operations'`
- ALL_MODES: update to match
- Verify that `modeStacks` and `activeMode` are NOT persisted to localStorage or disk. If they are, add migration logic on load: map `'node'` to `'dashboard'` and `'activity'` to `'operations'`.
- New AppState fields for sidebar status: `pyramidCount: number` (polled via `invoke('pyramid_list_slugs')` every 30s in AppShell), `latestApexQuestion: string | null`, `fleetOnlineCount: number` and `taskCount: number` (from pulse, polled every 60s — lift from DashboardMode to AppShell), `draftCount: number` (polled via `invoke('get_compose_drafts')` every 30s). Active chain count and tool count are hardcoded to 0 for Sprint 0.
- New AppState fields for Knowledge sidebar: `docCount: number` (derived from `syncState.cached_documents.length`, already polled), `corpusCount: number` (derived from `Object.keys(syncState.linked_folders).length`, already polled), `lastSyncTime: string | null` (from `syncState.last_sync_at`, already polled). Note: these are DERIVED from existing `syncState` — no new polling needed.
- Note: AppContext will be extended again in Phase 6 with intent bar state and `activeOperations: OperationEntry[]`. Phase 1 adds Mode type changes, ALL_MODES, sidebar status fields, and Knowledge metrics.

### Sidebar.tsx — Live Status Sidebar (v3 vision)

The sidebar is a **dashboard-in-a-column**, not a static menu. Each tab item shows its live state at a glance. The user reads the entire system by scanning the sidebar without clicking anything.

**Each item renders TWO lines:**
1. Tab name + headline metric (e.g., "Operations · 2 active")
2. Context line with detail (e.g., "●● running · 1 needs review")

**Three visual states per item:**
- **Glowing** (CSS: bright text + subtle glow animation) — active work or needs attention
- **Subtle** (CSS: normal text, no glow) — has content, nothing urgent
- **Dim** (CSS: muted text) — idle, nothing happening

Items do NOT reorder — visual weight shifts instead. Glowing items draw the eye; dim items recede.

**Item status data sources:**

New AppState fields for sidebar status: `pyramidCount: number` (polled via `invoke('pyramid_list_slugs')` every 30s in AppShell), `latestApexQuestion: string | null`, `fleetOnlineCount: number` and `taskCount: number` (from pulse, polled every 60s — lift from DashboardMode to AppShell), `draftCount: number` (polled via `invoke('get_compose_drafts')` every 30s). Active chain count and tool count are hardcoded to 0 for Sprint 0.

| Tab | Headline | Context | Glow when |
|-----|----------|---------|-----------|
| Understanding | pyramid count | latest apex question or "Building..." | build in progress |
| Knowledge | doc count | corpus count + last sync time | sync in progress |
| Tools | local tool count | published count + "earning" | new tool available |
| Operations | active chain count | running dots + needs-review count | chains running OR unreviewed results |
| Fleet | online agent count | total agents + task count | agent needs attention |
| Search | — | — | — (always dim unless results pending) |
| Compose | draft count or — | — | — (subtle if drafts exist) |

**Bottom section (compact, single-line):**
- Network: credit balance + green/red online dot
- @handle (identity)
- Gear icon (settings)

**CSS:** 7 two-line items x 48px + 3 single-line bottom items x 32px = 432px. On standard laptop viewports (768px), the sidebar scrolls. Pin the bottom 3 compact items (Network status, @handle, gear icon) BELOW the scrollable area using `position: sticky; bottom: 0` so they remain always-visible. Add `overflow-y: auto` to the scrollable section above them. Glow animation: subtle `box-shadow` pulse using `var(--accent-cyan)`.

**Glow priority:** Max 2 items animate simultaneously. The most urgent item gets full glow animation. The second gets a subtle pulse. Additional items needing attention get a static bright indicator (colored dot, no animation). Priority order: Operations > Fleet > Knowledge > Understanding > Tools.

- Remove Node and Activity entries
- Add Knowledge, Tools, Operations entries

### ModeRouter.tsx
- Remove `node` and `activity` cases
- Add `knowledge` → KnowledgeMode
- Add `tools` → ToolsMode
- Add `operations` → OperationsMode

---

## Phase 2: KnowledgeMode (new)

`src/components/modes/KnowledgeMode.tsx` — 2 sub-tabs:

1. **Corpora** — renders `CorporaList` (moved from Fleet). Same stack-based navigation for CorpusDetail + DocumentDetail.
2. **Local Sync** — renders `SyncStatus` (moved from Node). Same props passing from AppContext state.

KnowledgeMode must include view-stack routing: call `currentView('knowledge')` at the top, conditionally render `CorpusDetail` when view is `'corpus-detail'`, `DocumentDetail` when view is `'document-detail'`. Copy the pattern from FleetMode lines 15-32, replacing `'fleet'` with `'knowledge'`. FleetMode must REMOVE its CorpusDetail/DocumentDetail routing since those components will no longer push onto the `'fleet'` stack.

CurationQueue stays in Fleet — its `pushView` stays as `'fleet'`. It is NOT moved to Knowledge. CurationQueue is a fleet/agent action, not a knowledge management action.

Update stewardship files:
- `CorporaList.tsx` — `pushView('knowledge', ...)` (was `'fleet'`)
- `CorpusDetail.tsx` — `pushView`/`popView` → `'knowledge'`
- `DocumentDetail.tsx` — `popView('knowledge')`

---

## Phase 2b: ToolsMode (new)

`src/components/modes/ToolsMode.tsx` — the tools management surface.

For Sprint 0, a minimal but real implementation:
- **My Tools** sub-tab — For Sprint 0, ToolsMode reads from `src/config/wire-actions.ts` (hardcoded metadata). No seed script, no Wire query. Sprint 1 adds real data via the contribute endpoint. The sidebar Tools metric shows this count. Each tool shows: title, type (action/chain/skill/template), description, usage count, revenue earned (if published).
- **Discover** sub-tab (placeholder) — "Search the Wire for tools. Coming soon."
- **Create** sub-tab (placeholder) — "Describe what you need, intelligence builds it. Coming soon."

The planner (Phase 6) queries the My Tools list when building plans. Published tools from Phase 5 (chain-as-contribution) appear here automatically.

---

## Phase 3: OperationsMode (new, replaces Activity)

`src/components/modes/OperationsMode.tsx` — takes over from ActivityMode:

Sub-tabs:
1. **Notifications** — existing notification list from ActivityMode (filters, expandable rows, rating, flagging)
2. **Messages** — existing circle messages from ActivityMode
3. **Active** — For Sprint 0: shows 'No active operations' empty state. The `activeOperations: OperationEntry[]` AppContext field is defined (ready for Sprint 1) but nothing populates it in Sprint 0. In Sprint 1, the intent bar's planner pushes entries here.
4. **Queue** (placeholder for Sprint 2) — "No queued tasks."

This is ActivityMode restructured into an Operations frame. Active shows an empty state in Sprint 0 (populated by intent bar in Sprint 1); Queue is a placeholder for future chain scheduling.

OperationsMode should copy ActivityMode's notification and message logic into its Notifications and Messages sub-tabs (restructured with tab navigation, not wrapped). ActivityMode is 687 lines — extract notification logic (~400 lines) into Notifications sub-tab and message logic (~200 lines) into Messages sub-tab.

Rename `SourceFilter` value `'node'` to `'infrastructure'`. Update `getSource()` and the dropdown label accordingly. Use CSS class `'operations-mode'` instead of `'activity-mode'`. Add CSS migration: rename/alias `activity-*` classes to `operations-*`.

---

## Phase 4: Fleet loses Corpora, Mesh → Coordination

### FleetMode.tsx
- Remove `'corpora'` from FleetTab type and tab navigation
- Rename `'mesh'` label from "Mesh" to "Coordination"
- 3 sub-tabs: Fleet Overview, Coordination, Tasks

### MeshPanel.tsx
- No code changes — just the tab label in FleetMode changes

---

## Phase 5: Network absorbs Node

### DashboardMode.tsx (mode key `dashboard`, label "Network")
- Add sub-tab navigation (like NodeMode/FleetMode pattern):
  - **Dashboard** (default) — existing pulse + overview + review queue + credits content
  - **Market** — existing `MarketView` (moved from Node)
  - **Infrastructure** — existing `RemoteConnectionStatus` + `LogViewer` (moved from Node)

### DashboardOverview sub-component
- Extract existing DashboardMode content into a `DashboardOverview` sub-component (~500 lines). DashboardMode becomes a thin shell with sub-tab navigation + conditional rendering of `DashboardOverview`, `MarketView`, or `InfrastructurePanel` (new wrapper around `RemoteConnectionStatus` + `LogViewer`). Keeps DashboardMode under 80 lines.

### Update cross-references to removed modes
- Update `DashboardMode.tsx`: change `setMode('activity')` to `setMode('operations')` (lines 253, 274). Change the Node summary card's onClick to activate the Infrastructure sub-tab within DashboardMode (e.g., `setActiveTab('infrastructure')`). Update the card title from 'Node' to 'Infrastructure.'
- Update `ComposeMode.tsx`: change `setMode('activity')` to `setMode('operations')` (line 727), update button label from `'View in Activity'` to `'View in Operations'`.

### NodeMode.tsx
- DELETE this file (all sub-components moved to Network)

---

## Phase 6: Intent Bar Placeholder

**The full intent bar + planner is Sprint 1.** Sprint 0 adds the UI element only.

### IntentBar component (Sprint 0 — UI only)
`src/components/IntentBar.tsx` — persistent text input above mode content:

- Fixed position at top of content area (below header, above mode content)
- Text input with "What do you want to do?" placeholder
- Submit button
- For Sprint 0: no planner. On submit, show a "Coming soon — the planner is being built" message. The intent bar exists visually so the layout is established and users see where the interaction point will be.

### AppShell.tsx
- Render IntentBar above the ModeRouter content area
- IntentBar is always visible regardless of active tab

---

## Sprint 1 Reference: Intent Bar + Planner (NOT part of Sprint 0)

### IntentBar component
`src/components/IntentBar.tsx` — persistent text input above mode content:

- Fixed position at top of content area (below header, above mode content)
- Text input with "What do you want to do?" placeholder
- Submit button

### Planner chain (Pillar 17 — the planner IS a chain)

On intent submit:
1. **Gather context via API** (Pillar 25 — same endpoints agents use):
   - `wireApiCall('GET', '/api/v1/wire/pulse')` → fleet status, tasks, balance overview
   - `invoke('pyramid_list_slugs')` → user's pyramids
   - `invoke('get_sync_status')` → user's linked corpora/folders
   - User is shown what info is being gathered (transparency)

2. **Dispatch planner** (Wire action chain):
   - `wireApiCall('POST', '/api/v1/wire/action/invoke', { action_id: PLANNER_ACTION_ID, mode: 'review', input: { intent, context, widgetCatalog: WIDGET_CATALOG } })` — the planner is a published Wire action chain (improvable, forkable, earnable per Pillar 17).
   - Include the widget catalog schema in the planner input: `input: { intent, context, widgetCatalog: WIDGET_CATALOG }` where `WIDGET_CATALOG` is imported from `src/config/widget-catalog.ts` — a JSON array of `{ type, description, props }` entries. The planner LLM sees which widgets exist and their accepted props.

3. **Planner returns a plan object** (Pillar 2 — chain-compatible format):
   ```
   {
     steps: [{ action_id, description, estimated_cost, requires_input?: { field, type, label } }],
     total_estimated_cost: number,
     ui_schema: { ... },  // defines the custom plan UI
     publish_as_chain: boolean  // user toggleable, default from settings
   }
   ```

4. **Render custom plan UI** — the plan includes a `ui_schema` that defines what the user sees:
   - Widget catalog pattern: the planner returns an array of known widget types (e.g., `{ type: 'corpus_selector' }`, `{ type: 'text_input', field: 'question', label: '...' }`, `{ type: 'cost_preview', amount: ... }`, `{ type: 'toggle', field: 'publish_chain', label: '...' }`). React maps each type to a pre-built component. Catalog starts with 6 widget types and grows as new plan types emerge.
   - For a pyramid build: corpus selector, question input, layer options
   - For a search: topic refinement, cost preview
   - For a fleet action: agent selector, confirmation
   - Rendered in a modal or inline panel below the intent bar
   - User fills in required inputs, reviews steps + cost, toggles "publish this plan as a chain" on/off

5. **On approve** — execute the plan:
   - On plan approval, push an entry to `activeOperations` in AppContext. Poll build status to update progress. On completion, move to Completed.
   - `wireApiCall('POST', '/api/v1/wire/action/chain', { action_id, mode: 'trusted', input: { ...plan, ...userInputs }, chain_id, max_cost: Math.ceil(total_estimated_cost * 1.2) })` — `max_cost` enforces the cost ceiling the user approved, preventing overruns per Pillar 23.
   - Progress appears in Operations > Active
   - Result appears in the appropriate tab when complete

6. **Optionally publish** — if "publish as chain" is on, the executed plan becomes a contribution on the Wire. Others can fork it.

### PLANNER_ACTION_ID config
- `PLANNER_ACTION_ID` is stored in `src/config/wire-actions.ts` as a typed constant. The seed script writes the UUID to stdout and the developer copies it into this file. Future: discovery by well-known tag instead of hardcoded UUID.

### Seed script (Sprint 1)
The planner action is published via the standard contribute endpoint (after the VALID_TYPES fix). The seed script calls `wireApiCall('POST', '/api/v1/contribute', { type: 'action', ... })` like any other contribution. No special endpoints needed.

- Outputs the resulting UUID to stdout
- Developer copies the UUID into `src/config/wire-actions.ts`
- Runs once per Wire deployment (not part of the build pipeline)

### Sprint 1 planner implementation
The planner action runs a single LLM step via the node's configured OpenRouter model with a system prompt that includes: the user's context (pyramids, corpora, agents, balance), the widget catalog, and instructions to produce a plan object matching the `output_schema`. The LLM call happens CLIENT-SIDE via the existing `invoke('pyramid_llm_call')` or equivalent Tauri command initially, migrating to the Wire action invoke endpoint when `POST /api/v1/wire/action/create` is built.

**Pillar 17 path:** The planner starts as a local LLM call, then migrates to a publishable Wire action chain once `POST /api/v1/wire/action/create` exists. The output format (plan object with `ui_schema`) is chain-compatible from day one so the migration is format-preserving.

### Wire server requirement
- `POST /api/v1/wire/action/create` endpoint is built in Sprint 1. The planner is published as a Wire action chain (improvable, forkable, earnable).
- **Action discovery endpoint** (for future sprints) — the planner will eventually search for existing chains by capability.

### AppShell.tsx
- Render IntentBar above the ModeRouter content area
- IntentBar is always visible regardless of active tab

---

## Files to Create/Modify

| File | Change |
|------|--------|
| `src/contexts/AppContext.tsx` | Mode type: remove `node`/`activity`, add `knowledge`/`operations` |
| `src/components/Sidebar.tsx` | New tab order, section headers, labels |
| `src/components/ModeRouter.tsx` | Remove node/activity, add knowledge/operations |
| New: `src/components/modes/KnowledgeMode.tsx` | Corpora + Sync sub-tabs |
| New: `src/components/modes/ToolsMode.tsx` | Tools management — My Tools, Discover, Create |
| New: `src/components/modes/OperationsMode.tsx` | Replaces ActivityMode with Operations frame |
| `src/components/modes/FleetMode.tsx` | Remove Corpora sub-tab, rename Mesh → Coordination |
| `src/components/modes/DashboardMode.tsx` | Add sub-tabs: Dashboard, Market, Infrastructure |
| DELETE: `src/components/modes/NodeMode.tsx` | All content moved to Network or Knowledge |
| DELETE: `src/components/modes/ActivityMode.tsx` | Replaced by OperationsMode |
| `src/components/stewardship/CorporaList.tsx` | `pushView('knowledge', ...)` |
| `src/components/stewardship/CorpusDetail.tsx` | `pushView`/`popView` → `'knowledge'` |
| `src/components/stewardship/DocumentDetail.tsx` | `popView('knowledge')` |
| `src/components/stewardship/CurationQueue.tsx` | Stays in Fleet — `pushView('fleet', ...)` unchanged |
| `src/components/modes/ComposeMode.tsx` | `setMode('activity')` → `setMode('operations')`, update button label |
| `src/components/AppShell.tsx` | Render IntentBar placeholder above mode content, add sidebar status polling |
| New: `src/config/wire-actions.ts` | Planner action metadata constant (hardcoded for Sprint 0) |
| New: `src/config/widget-catalog.ts` | Widget type catalog (Sprint 1 — defined in Sprint 0 for reference) |
| New: `src/components/IntentBar.tsx` | Intent bar UI placeholder (no planner — Sprint 1) |
| New: `src/components/modes/DashboardOverview.tsx` | Extracted from DashboardMode (~500 lines) |
| New: `src/components/modes/InfrastructurePanel.tsx` | Wrapper around RemoteConnectionStatus + LogViewer |
| `src/styles/dashboard.css` | Section headers, intent bar styles, sub-tab styles for Network, `operations-*` CSS classes (alias `activity-*`) |

## Tools Tab (first-class — YOUR WORLD section)

Actions, chains, skills, and templates are contributions on the Wire graph. They are the third leg of YOUR WORLD: Understanding (what you've built), Knowledge (what you build from), **Tools** (how you build). They deserve their own tab because they have their own intelligence-driven workflows:

1. **Create** — describe what you need, built-in intelligence builds the action/chain/skill/template for you. "I need a chain that ingests a folder and builds a code pyramid" → intelligence creates it.
2. **Discover** — intelligence-assisted search of the Wire for existing tools. "Find me an action chain for legal document analysis" → searches, previews, shows cost to acquire.
3. **Acquire** — pull tools from the Wire to local (one-time purchase, then available for local chains without re-paying).
4. **Publish** — put your tools on the Wire, set pricing (or free for citation compounding), contribute back. (Pillar 36 — using the Wire populates the Wire)
5. **Monitor** — see which of your published tools are being used, forked, cited, earning. Revenue tracking per tool.

The planner (intent bar) searches the Tools inventory when building plans. When a plan executes successfully and the user opts to publish it, the resulting chain appears here.

For Sprint 0: Tools tab exists as a browsable list of locally available tools (reading from hardcoded metadata in `src/config/wire-actions.ts`). Create/Discover/Acquire are placeholder sub-tabs showing "Coming in Sprint 1-2 when action discovery is built on the Wire server." The tab establishes the first-class position; the intelligence fills in over subsequent sprints.

---

## Verification

1. App launches with 10 tabs. Sidebar shows live status for each (headline metric + context line)
2. Understanding shows pyramid count + latest apex question. Glows when build is in progress.
3. Knowledge shows doc count + corpus count + last sync. Glows during sync.
4. Tools shows local tool count. Shows planner action metadata from hardcoded config.
5. Fleet has 3 sub-tabs: Fleet Overview, Coordination, Tasks. Shows online agent count.
6. Operations has Notifications + Messages + Active (empty state for Sprint 0) + Queue (placeholder). Glows when notifications arrive.
7. Search, Compose show dim (idle) state. Compose shows draft count if drafts exist.
8. Network (bottom section, compact): credit balance + online dot.
9. No "Node" or "Activity" tabs exist
10. Stewardship drill-down works in Knowledge tab (view-stack routing)
11. Intent bar visible at top, accepts text, shows 'Coming soon' message on submit
12. No LLM calls or plan rendering in Sprint 0 — planner is Sprint 1
13. `npx tsc --noEmit` + `cargo check` pass
