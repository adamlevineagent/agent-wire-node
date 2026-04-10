# Cross-Pyramid Observability Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Build visualization expansion (for `BuildEventBus`, `PyramidBuildViz.tsx`, reroll IPC), DADBEAR oversight page (for per-pyramid cost log), LLM output cache (for cost_usd on cache entries)
**Unblocks:** Multi-pyramid users, operator-grade cost visibility, one-click pause-all for DADBEAR
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

The current build viz and DADBEAR design assumes a single pyramid in focus. The event bus is per-slug, the cost log is queried per-slug, and the build viz subscribes to one pyramid at a time. For a user with 3-10 active pyramids, that means 3-10 separate windows to watch, 3-10 separate cost queries to eyeball, and no single place to say "pause everything" when they need to stop spending.

This spec adds a cross-pyramid layer on top of the existing per-pyramid infrastructure:

- **Cross-pyramid build timeline** — see all concurrent builds at once
- **Cross-pyramid cost rollup** — total spend across pyramids, with breakdown
- **Cross-pyramid DADBEAR controls** — pause all, scope by folder or circle
- **Cross-pyramid reroll** — reroll a step from the cross-pyramid view without drilling in

The existing per-pyramid components are composed, not replaced. The cross-pyramid view is a new surface that subscribes to all slugs.

---

## Problem

Current design is per-pyramid. Users with multiple active pyramids face:

1. **Build blindness**: three builds running in parallel, each in its own detail page. No single screen showing "what is my machine doing right now?"
2. **Cost fragmentation**: total spend requires clicking into each pyramid's DADBEAR oversight, summing manually. No weekly rollup across the portfolio.
3. **Pause friction**: "stop everything, I'm going to bed" requires toggling DADBEAR off on each pyramid one by one. Error-prone and slow.
4. **Reroll friction**: the user spots a wrong output in the cross-pyramid view (when it exists) and has to drill into the specific pyramid's build viz to reroll.

All four problems are solved by adding cross-pyramid aggregation views that compose the existing per-pyramid primitives.

---

## Cross-Pyramid Build Timeline View

A new UI component in the dashboard that subscribes to `BuildEventBus` across ALL pyramids (not filtered by slug). Shows every active build, every recently completed build, and every failed build in a single timeline.

### Layout

```
┌─────────────────────────────────────────────────────────────────────┐
│  Cross-Pyramid Build Timeline                    [Pause All DADBEAR] │
│                                                                      │
│  Active Builds (3)                                                   │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ opt-025                  extract_chunks    ██████░░  84/112 │  │
│  │                          $0.31 est  |  cache 27%   [View]   │  │
│  │                                                               │  │
│  │ goodnewseveryone         synthesize_threads ███░░░░  5/14   │  │
│  │                          $0.08 est  |  cache 42%   [View]   │  │
│  │                                                               │  │
│  │ core-selected-docs       apex_synthesis     ██████████ 1/1  │  │
│  │                          $0.02 est  |  cache 0%    [View]   │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  Recent (last hour)                                                  │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ vibesmithy-dev           completed    $0.42   14:07          │  │
│  │ opt-025                  completed    $1.18   13:31          │  │
│  │ wire-online-plan         failed       $0.04   13:12   [Log]  │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  Total today: $2.94 est  |  $2.71 actual  |  cache savings $1.82    │
└──────────────────────────────────────────────────────────────────────┘
```

### Behavior

- **Active builds** are sourced from `pyramid_active_builds` IPC (see below). Each row shows the current step, progress bar, estimated cost so far, and live cache hit percentage.
- **Recent** is a historical list from `pyramid_build_history`, showing the last 10 builds with status, final cost, and completion time. A "Log" button opens the build's failure log if applicable.
- **Clicking [View]** on an active build opens the existing `PyramidBuildViz.tsx` for that slug in a detail modal or sidebar panel. The detail view is unchanged — this spec reuses it, doesn't duplicate it.
- **Auto-update**: the component subscribes to `BuildEventBus` with no slug filter. Each event updates the matching active build row. New builds (via `ChainStepStarted` on a new build_id) are added. Completed builds migrate to the Recent section on `BuildCompleted` events.

### Event Subscription

The Tauri event listener pattern is extended. Instead of `listen("build-event", handler)` filtering by slug client-side, the handler receives ALL events and routes them to the appropriate per-slug state slot:

```typescript
interface CrossPyramidBuildState {
  activeBuildsBySlug: Map<string, BuildRowState>;
  recent: BuildRowState[];
  totalCost: CostAccumulator;
}

function reduceEvent(state: CrossPyramidBuildState, event: TaggedBuildEvent): CrossPyramidBuildState {
  const { slug, kind } = event;
  const row = state.activeBuildsBySlug.get(slug) ?? createNewRow(slug);
  const updatedRow = reduceRowEvent(row, kind);  // uses the same per-pyramid reducer
  // ...
}
```

The per-row reducer (`reduceRowEvent`) is the existing per-pyramid logic from `PyramidBuildViz.tsx`, factored out into a shared module `useBuildRowState.ts`. The cross-pyramid view uses it directly; the existing per-pyramid viz uses it through a wrapper that filters by slug.

### Component Composition

The existing `PyramidBuildViz.tsx` component is composed into a new cross-pyramid view component `CrossPyramidTimeline.tsx`:

```
CrossPyramidTimeline.tsx          — top-level page, subscribes to all slugs
  ├── ActiveBuildRow.tsx          — compact row showing one build
  │     └── (click) opens PyramidBuildViz.tsx in modal
  ├── RecentBuildRow.tsx          — historical row
  └── CrossPyramidCostFooter.tsx  — running total across all active builds
```

`PyramidBuildViz.tsx` is unchanged; it remains the detailed per-pyramid view. The cross-pyramid view is a new parent that composes it.

---

## Cross-Pyramid Cost Rollup

A new query path that aggregates `pyramid_cost_log` across all slugs. Shown in the DADBEAR Oversight page as a new section (not a new page), so the operator sees total spend alongside per-pyramid DADBEAR activity.

### Metrics

- **Total spend**: today / this week / this month / custom range
- **By pyramid**: sorted by spend, shows each slug with estimated + actual
- **By provider**: shows OpenRouter, direct provider costs split
- **By operation**: `build`, `stale_check`, `evidence`, `triage`, `manifest`, `reroll`
- **Reconciliation health**: estimated vs actual delta per provider, flagging pyramids where estimates diverge from actual webhook-reported costs by more than 10%

### Display

Added as a new section at the top of the DADBEAR Oversight page:

```
┌─ Spend Rollup ──────────────────────────────────────────────┐
│                                                              │
│  Range: [Today] [Week] [Month] [Custom]         Total: $12.47│
│                                                              │
│  By pyramid:                                                 │
│    opt-025              $4.21 est  $4.08 actual  -$0.13     │
│    goodnewseveryone     $3.14 est  $3.07 actual  -$0.07     │
│    core-selected-docs   $2.87 est  $2.91 actual  +$0.04     │
│    vibesmithy-dev       $1.42 est  $1.40 actual  -$0.02     │
│    wire-online-plan     $0.83 est  $0.81 actual  -$0.02     │
│                                                              │
│  By provider:                                                │
│    openrouter           $9.18 est  $8.94 actual             │
│    anthropic-direct     $3.29 est  $3.33 actual             │
│                                                              │
│  By operation:                                               │
│    build                $7.84                               │
│    evidence             $2.14                               │
│    stale_check          $1.41                               │
│    triage               $0.63                               │
│    manifest             $0.31                               │
│    reroll               $0.14                               │
│                                                              │
│  Reconciliation: 1 pyramid has > 10% delta (wire-online-plan)│
└──────────────────────────────────────────────────────────────┘
```

### Query

Cost rollup aggregates from `pyramid_cost_log`. The base query:

```sql
SELECT 
  slug,
  provider,
  operation,
  SUM(estimated_cost_usd) AS estimated,
  SUM(actual_cost_usd) AS actual,
  COUNT(*) AS call_count
FROM pyramid_cost_log
WHERE created_at >= ?1 AND created_at < ?2
GROUP BY slug, provider, operation;
```

The UI performs pivots client-side for the three views (by pyramid, by provider, by operation). All three views share the same query result, avoiding a round-trip per pivot.

### Aggregation Performance

Cost rollup queries scan `pyramid_cost_log` with date range filters. For a single user across 10 pyramids over a month, this is typically 10K-100K rows — fine for direct query. For larger deployments or longer ranges:

- **Default**: direct query with an index on `(created_at, slug)`.
- **Fallback**: if query time exceeds 500ms, a materialized summary table `pyramid_cost_summary` is updated on each cost_log insert.

```sql
CREATE TABLE IF NOT EXISTS pyramid_cost_summary (
    slug TEXT NOT NULL,
    date_bucket TEXT NOT NULL,              -- YYYY-MM-DD
    operation TEXT NOT NULL,
    provider TEXT NOT NULL,
    estimated_cost_usd REAL NOT NULL DEFAULT 0,
    actual_cost_usd REAL NOT NULL DEFAULT 0,
    call_count INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug, date_bucket, operation, provider)
);
```

The summary is updated on every `pyramid_cost_log` insert via a helper `update_cost_summary()` called from the same function that writes the cost log. For a week/month rollup, the query reads from the summary table (a few hundred rows) instead of the raw log (tens of thousands).

Switch to the summary table when direct queries exceed 500ms in production. Default to direct queries — the summary is an optimization, not the primary path.

---

## IPC Contract

```
GET pyramid_cost_rollup
  Input: {
    range: String,                 -- "today" | "week" | "month" | "custom"
    from?: String,                 -- ISO date, required if range = "custom"
    to?: String,                   -- ISO date, required if range = "custom"
    group_by: String               -- "pyramid" | "provider" | "operation"
  }
  Output: {
    total_estimated: f64,
    total_actual: f64,
    buckets: [{
      key: String,                 -- slug | provider | operation name
      estimated: f64,
      actual: f64,
      count: u64
    }]
  }

GET pyramid_active_builds
  Output: [{
    slug: String,
    build_id: String,
    status: String,                -- "running" | "idle" | "failed"
    step_progress: {
      current_step: String,
      total_steps: u64,
      completed_steps: u64,
      progress_pct: f64
    },
    cost_so_far_usd: f64,
    cache_hit_rate: f64,
    started_at: String             -- ISO timestamp
  }]

POST pyramid_pause_dadbear_all
  Input: {
    scope: String,                 -- "all" | "folder" | "circle"
    scope_value?: String           -- folder path or circle id, required if scope != "all"
  }
  Output: { affected: u64 }

POST pyramid_resume_dadbear_all
  Input: {
    scope: String,
    scope_value?: String
  }
  Output: { affected: u64 }
```

### Active Builds Query

`pyramid_active_builds` is backed by a query over `pyramid_build_runs` (the existing table tracking build lifecycle), filtered by `status IN ('running', 'idle')`. The `step_progress`, `cost_so_far_usd`, and `cache_hit_rate` fields are computed from `pyramid_pipeline_steps` and `pyramid_step_cache` joined on the build_id:

```sql
SELECT
  br.slug,
  br.build_id,
  br.status,
  br.started_at,
  (SELECT COUNT(*) FROM pyramid_pipeline_steps WHERE build_id = br.build_id AND status = 'done') AS completed_steps,
  (SELECT COUNT(*) FROM pyramid_pipeline_steps WHERE build_id = br.build_id) AS total_steps,
  (SELECT step_name FROM pyramid_pipeline_steps WHERE build_id = br.build_id AND status = 'running' ORDER BY started_at DESC LIMIT 1) AS current_step,
  (SELECT COALESCE(SUM(cost_usd), 0) FROM pyramid_step_cache WHERE build_id = br.build_id) AS cost_so_far,
  (SELECT CAST(COUNT(CASE WHEN force_fresh = 0 THEN 1 END) AS REAL) / NULLIF(COUNT(*), 0) 
    FROM pyramid_step_cache WHERE build_id = br.build_id) AS cache_hit_rate
FROM pyramid_build_runs br
WHERE br.status IN ('running', 'idle')
ORDER BY br.started_at DESC;
```

This is called by the cross-pyramid timeline on mount and then supplemented by live events from `BuildEventBus` for real-time updates. No polling needed during the build — the initial query seeds state, events keep it fresh.

---

## Pause-All Semantics

Pausing DADBEAR affects the tick loop behavior. The current `pyramid_dadbear_config` table has an `enabled` column; the tick loop already respects it. Pause-all sets this column to `false` on matching rows.

### Scope Behaviors

#### `scope: "all"`

Pause DADBEAR on every pyramid:

```sql
UPDATE pyramid_dadbear_config SET enabled = 0 WHERE enabled = 1;
```

The tick loop continues to fire every second but skips all configs (all have `enabled = 0`). No ticks dispatch.

#### `scope: "folder"`

Pause DADBEAR on pyramids whose `source_path` is within the given folder:

```sql
UPDATE pyramid_dadbear_config 
SET enabled = 0 
WHERE enabled = 1 
  AND (source_path = ?1 OR source_path LIKE ?1 || '/%');
```

Useful for "pause all my work pyramids while I'm in personal projects."

#### `scope: "circle"`

Pause DADBEAR on pyramids that are in a specific Wire circle:

```sql
UPDATE pyramid_dadbear_config 
SET enabled = 0 
WHERE enabled = 1 
  AND slug IN (
    SELECT slug FROM pyramid_metadata WHERE circle_id = ?1
  );
```

Useful for "pause all pyramids shared with team X."

### Resume

`pyramid_resume_dadbear_all` is the mirror operation — same scopes, but sets `enabled = 1`. Both pause and resume are idempotent (pausing an already-paused pyramid has no effect; the `affected` count is 0).

### Notification

After pause-all, the UI shows a toast: "Paused DADBEAR on N pyramids". The Cross-Pyramid Build Timeline shows a "DADBEAR Paused (N pyramids)" banner until resumed. In-flight builds are not affected — only background DADBEAR stale-check ticks are paused. A user who wants to also stop running builds must kill them manually.

### Pause-All vs Per-Pyramid Pause

The existing per-pyramid `pyramid_dadbear_pause(slug)` IPC remains. Pause-all is a convenience wrapper that iterates via bulk SQL. A pyramid paused by pause-all and then manually resumed on the per-pyramid page behaves normally — there is no pause-all "lock" separate from the per-config `enabled` column.

---

## Cross-Pyramid Reroll

A UI surface in the cross-pyramid build timeline that lets the user reroll any step in any pyramid without drilling in. Calls the same `pyramid_reroll_node` IPC as the single-pyramid reroll (defined in `build-viz-expansion.md`).

### UX

From the cross-pyramid timeline, clicking a step in an active build's step list opens a reroll modal:

```
┌─ Reroll step: synthesize_threads (goodnewseveryone) ─┐
│                                                       │
│  Node: C-L2-007                                       │
│  Current output:                                      │
│    "This cluster discusses the auth rewrite, with..."│
│                                                       │
│  Why reroll? (strongly encouraged)                    │
│  ┌─────────────────────────────────────────────────┐ │
│  │ Missing mention of the deadlock risk...         │ │
│  └─────────────────────────────────────────────────┘ │
│                                                       │
│                         [Cancel]     [Reroll]        │
└───────────────────────────────────────────────────────┘
```

This is the same modal as the per-pyramid reroll flow, just invoked from a different parent. The `pyramid_reroll_node` IPC is called with the step's slug, node_id, and note.

### Anti-Slot-Machine Enforcement

Same as per-pyramid — empty notes trigger a confirmation prompt, repeated rerolls show a warning. See `build-viz-expansion.md` for the full enforcement logic. Cross-pyramid reroll is not a special case; it's the same IPC with the same UI component rendered in a different location.

### Reroll for Intermediate Outputs

Cross-pyramid reroll extends to intermediate outputs the same way per-pyramid reroll does. See the "Reroll for Intermediate Outputs" section in `build-viz-expansion.md` — clustering decisions, web edges, triage decisions, and evidence answers can all be rerolled from the cross-pyramid timeline.

---

## Backend Subscription Model

The existing `BuildEventBus` is already per-slug (each slug has its own broadcast channel). The cross-pyramid view needs to subscribe to all slugs simultaneously.

### Option A: Per-Slug Subscription Loop

The Tauri backend maintains a registry of active builds and fans out events to subscribers. The cross-pyramid view subscribes once and receives events from every active build.

```rust
pub struct CrossPyramidEventRouter {
    subscribers: Vec<tokio::sync::mpsc::Sender<TaggedBuildEvent>>,
    active_slugs: HashMap<String, tokio::task::JoinHandle<()>>,
}

impl CrossPyramidEventRouter {
    pub fn add_slug(&mut self, slug: String, bus: Arc<BuildEventBus>) {
        let mut rx = bus.subscribe();
        let subscribers = self.subscribers.clone();
        let handle = tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                for sub in &subscribers {
                    let _ = sub.send(event.clone()).await;
                }
            }
        });
        self.active_slugs.insert(slug, handle);
    }
}
```

When a new build starts on slug X, the router's `add_slug(X, bus)` is called. When a build completes, the slug's forwarder task naturally drains and exits. The router tracks active slugs and ensures each has exactly one forwarder task.

### Option B: Unified Event Bus

Alternative: make the event bus single-channel with `slug` as a field on every event. The current design already has `slug` on `TaggedBuildEvent`, so this is mostly a refactor that collapses per-slug buses into one.

**Recommendation**: Option A (per-slug subscription loop with a router). Reason: the existing per-slug bus is a proven primitive; collapsing it for cross-pyramid is a refactor risk. The router adds ~50 lines of glue code and preserves per-slug isolation. Option B is simpler long-term but riskier short-term.

### Desktop (Tauri) Event Emission

For the Tauri frontend, the router emits events via `app_handle.emit_all("cross-build-event", &tagged_event)`. The cross-pyramid timeline component listens with `listen("cross-build-event", handler)` while the per-pyramid view continues to listen to its slug-specific `build-event` channel. The two channels coexist — they're not mutually exclusive.

### Web Surface

For the public web surface, a new WebSocket endpoint `/p/_/cross_ws` provides cross-pyramid events. Unlike the per-slug WebSocket `/p/{slug}/_ws`, this endpoint is scoped to the authenticated user's slugs (not a public firehose — an operator's pyramids only). Authentication uses the existing JWT.

---

## Files Modified

| Component | Files |
|-----------|-------|
| Event router | New `cross_pyramid_event_router.rs` — fan-out from per-slug buses to a unified subscriber list |
| IPC handlers | `routes.rs` — `handle_cost_rollup()`, `handle_active_builds()`, `handle_pause_dadbear_all()`, `handle_resume_dadbear_all()` |
| DB schema | `db.rs` — optional `pyramid_cost_summary` table (fallback for large deployments) |
| Cost aggregation | New `cost_rollup.rs` — query + pivot logic |
| Pause-all logic | `dadbear.rs` — bulk enable/disable by scope |
| Frontend top-level | New `CrossPyramidTimeline.tsx` — main cross-pyramid dashboard component |
| Frontend active row | New `ActiveBuildRow.tsx` — compact build row with progress and reroll hook |
| Frontend rollup | New `CostRollupSection.tsx` — added to DADBEAR Oversight page |
| Frontend state | Extract `useBuildRowState.ts` from `PyramidBuildViz.tsx` for reuse |
| Frontend router | `Dashboard.tsx` or routing config — add cross-pyramid timeline as a new top-level view |

---

## Implementation Order

1. **Event router** — backend fan-out from per-slug buses to a cross-pyramid subscriber list
2. **Active builds IPC** — `pyramid_active_builds` query and handler
3. **Cross-pyramid timeline frontend** — new dashboard component consuming the event router + active builds query
4. **Cost rollup IPC** — `pyramid_cost_rollup` handler with direct query
5. **Cost rollup frontend** — added to DADBEAR Oversight page as a new section
6. **Pause-all IPC** — bulk pause/resume handlers with scope semantics
7. **Pause-all frontend** — button on cross-pyramid timeline
8. **Cross-pyramid reroll** — wire up the reroll modal from the cross-pyramid timeline (reuses existing reroll IPC)
9. **Cost summary table** — only if direct queries exceed 500ms in real use

Steps 1-5 deliver the core value: operator sees all builds, sees all spend, can stop all DADBEAR. Steps 6-8 add the cross-pyramid action surfaces. Step 9 is a performance fallback.

---

## Integration Notes

### With LLM Output Cache

The cost rollup's "by operation" and per-pyramid breakdowns depend on `cost_usd` being logged consistently. The LLM output cache spec already logs `cost_usd` per cache entry; this spec consumes it via `pyramid_cost_log` which is fed from the same source.

Cache savings (from `build-viz-expansion.md`) are computed per-build. The cross-pyramid view can sum cache savings across builds to show "cache saved you $X this week across all pyramids."

### With Cache Warming on Pyramid Import

Imported pyramids contribute to cost rollup the same way native pyramids do — their cache entries have `cost_usd` fields, and when those entries are hit, the cost saving is logged. The "cache savings" total in the rollup includes imported cache savings, making Wire-pulled pyramids visibly cheaper than native builds in the cost view.

### With Build Viz Expansion

The per-pyramid `PyramidBuildViz.tsx` component is composed into the cross-pyramid view. The per-pyramid reroll modal is reused by the cross-pyramid view. The cross-pyramid view does NOT duplicate per-pyramid logic; it's a new parent that subscribes to all slugs and renders compact rows, with drill-down opening the existing detailed view.

---

## Open Questions

1. **Event router lifetime**: when does the router stop forwarding events for a completed build? Recommend: keep the forwarder task alive for 60 seconds after the last event, so late-arriving events (cleanup, final cost updates) still reach subscribers. After 60 seconds of silence, drop the forwarder and remove the slug from `active_slugs`.

2. **Cost rollup custom range**: the `custom` range accepts arbitrary `from`/`to` dates. Should there be a max range (e.g., 1 year) to prevent runaway queries? Recommend: cap at 1 year. If the user needs longer, they should export the data.

3. **Pause-all authorization**: should pause-all require an extra confirmation? It affects potentially many pyramids and is a mistake if clicked accidentally. Recommend: yes — a modal "Pause DADBEAR on N pyramids?" with count, Cancel, and Confirm buttons. The user has to see the count before confirming.

4. **Cross-pyramid reroll discoverability**: is the reroll button visible enough in the compact row layout? Recommend: show a small "..." menu on each active build row that includes "Reroll step...", "Pause DADBEAR", and "View details". This keeps the row compact while making actions discoverable.

5. **Mobile/narrow layouts**: the cross-pyramid timeline may not fit on narrow screens. Recommend: responsive design — on narrow screens, show only active builds (hide Recent) and stack the cost breakdown vertically. Full layout restored at desktop widths.

6. **Historical build retention**: the Recent section shows the last hour. Should there be a full history page? Recommend: yes, but out of scope for this spec. A separate "Build History" page with filters by slug/date/status is a natural follow-up. This spec covers only the active + recent-hour view.
