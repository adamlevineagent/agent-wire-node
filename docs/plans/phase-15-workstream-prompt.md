# Workstream: Phase 15 — DADBEAR Oversight Page

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14 are shipped. You are the implementer of Phase 15 — the DADBEAR Oversight Page. This is a unified operator-grade view that assembles all the DADBEAR activity, cost reconciliation, provider health, orphan broadcasts, deferred questions, cross-pyramid builds, and pause-all controls that Phases 11, 12, 13, 14 built separately.

Phase 15 is primarily a frontend assembly phase. Backend IPCs are mostly already in place; a few new aggregation IPCs round out the page.

## Context

What's already built across prior phases:
- **Phase 11**: `pyramid_provider_health`, `pyramid_acknowledge_provider_health`, `pyramid_list_orphan_broadcasts`, provider health state machine.
- **Phase 12**: `pyramid_reevaluate_deferred_questions`, demand signals, deferred questions.
- **Phase 13**: `pyramid_active_builds`, `pyramid_cost_rollup`, `pyramid_pause_dadbear_all` (scope=all), `pyramid_resume_dadbear_all` (scope=all), `CrossPyramidTimeline.tsx`, `CostRollupSection.tsx` (currently mounted on CrossPyramidTimeline as a placeholder).
- **Phase 14**: `pyramid_wire_update_available`, `pyramid_wire_pull_latest`, `WireUpdatePoller`, `WireUpdateAvailable` + `WireAutoUpdateApplied` events.
- **Existing**: per-pyramid `pyramid_dadbear_pause`/`pyramid_dadbear_resume` (if they exist — check first), `pyramid_dadbear_config` table.

What Phase 15 adds:
- **`pyramid_dadbear_overview` IPC**: one call returning the full aggregated view (per-pyramid status, aggregate costs, pending mutations, in-flight stale checks, demand signal counts, deferred question counts).
- **Per-pyramid `pyramid_dadbear_pause` / `pyramid_dadbear_resume` IPCs** if not already present.
- **`pyramid_set_default_norms` IPC** for the "Set Default Norms" control (edits the global `dadbear_policy` contribution via the Phase 9 flow).
- **`DadbearOversightPage.tsx`** — the new top-level page component.
- Relocation of `CostRollupSection.tsx` from Phase 13's placeholder mount (Cross-Pyramid Timeline) to the new Oversight page as the spec intended.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/evidence-triage-and-dadbear.md` Part 3 (~line 613)** — the DADBEAR Oversight Page spec itself. Also **Part 4 (~line 674)** for orphan-broadcast + leak detection UI integration.
3. `docs/specs/cross-pyramid-observability.md` — scan the "Cost Rollup Section" spec. Phase 15 re-mounts this section.
4. `docs/specs/change-manifest-supersession.md` — Phase 15's deferred-question list surfaces manifest references.
5. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 15 section (~line 292).
6. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 11, 12, 13, 14 entries for the IPCs/components Phase 15 composes.

### Code reading

7. **`src-tauri/src/main.rs`** — find all existing `pyramid_*` IPCs. Confirm what's already registered. Note the `pyramid_active_builds`, `pyramid_cost_rollup`, `pyramid_pause_dadbear_all`, `pyramid_resume_dadbear_all`, `pyramid_provider_health`, `pyramid_acknowledge_provider_health`, `pyramid_list_orphan_broadcasts`, `pyramid_reevaluate_deferred_questions`, `pyramid_wire_update_available` registrations.
8. **`src-tauri/src/pyramid/db.rs`** — find the `pyramid_dadbear_config` table + helpers (`save_dadbear_config`, `get_enabled_dadbear_configs`). You'll read from these plus `pyramid_stale_check_log`, `pyramid_pending_mutations`, `pyramid_cost_log`, `pyramid_demand_signals`, `pyramid_deferred_questions`.
9. `src-tauri/src/pyramid/stale_engine.rs` — understand how DADBEAR tick loop reads config, counts in-flight checks. You may need to expose a small `get_in_flight_count(slug)` helper for the oversight IPC.
10. **`src/components/CrossPyramidTimeline.tsx` (Phase 13)** — understand the existing cross-pyramid layout. Phase 15 can either extend it into a tab on the same page or create a new page — recommend a new top-level page that shares the `useBuildRowState` hook.
11. **`src/components/CostRollupSection.tsx` (Phase 13)** — used as-is on the new Oversight page.
12. `src/components/modes/ToolsMode.tsx` (Phase 10/14) — existing mode pattern for reference.
13. `src/hooks/useCrossPyramidTimeline.ts` (Phase 13) — reuse for live updates.
14. `src/App.tsx` or `src/components/modes/` — find how modes are registered in the dashboard shell.

## What to build

### 1. Backend: `pyramid_dadbear_overview` IPC

New IPC returning aggregated per-pyramid DADBEAR status in one call.

```rust
#[derive(Serialize)]
struct DadbearOverviewRow {
    slug: String,
    display_name: String,
    enabled: bool,
    scan_interval_secs: u64,
    debounce_secs: u64,
    last_scan_at: Option<String>,
    next_scan_at: Option<String>,
    pending_mutations_count: u64,
    in_flight_stale_checks: u64,
    deferred_questions_count: u64,
    demand_signals_24h: u64,
    cost_24h_estimated_usd: f64,
    cost_24h_actual_usd: f64,
    cost_reconciliation_status: String,  // "healthy" | "pending" | "discrepancy" | "broadcast_missing"
    recent_manifest_count: u64,  // change manifests in last 24h
}

#[derive(Serialize)]
struct DadbearOverviewResponse {
    pyramids: Vec<DadbearOverviewRow>,
    totals: DadbearOverviewTotals,
}

#[derive(Serialize)]
struct DadbearOverviewTotals {
    total_estimated_24h_usd: f64,
    total_actual_24h_usd: f64,
    total_pending_mutations: u64,
    total_in_flight_checks: u64,
    total_deferred_questions: u64,
    paused_count: u64,
    active_count: u64,
}

#[tauri::command]
async fn pyramid_dadbear_overview(...) -> Result<DadbearOverviewResponse, String>
```

Implementation:
- Query `pyramid_dadbear_config` for all rows (slug + enabled + intervals)
- For each slug, query:
  - `COUNT(*) FROM pyramid_pending_mutations WHERE slug = ?` → pending_mutations_count
  - `COUNT(*) FROM pyramid_stale_check_log WHERE slug = ? AND completed_at IS NULL` → in_flight_stale_checks (or equivalent — read the stale_check_log schema)
  - `COUNT(*) FROM pyramid_deferred_questions WHERE slug = ?` → deferred_questions_count
  - `COUNT(*) FROM pyramid_demand_signals WHERE slug = ? AND created_at > datetime('now', '-24 hours')` → demand_signals_24h
  - `SUM(estimated_cost), SUM(actual_cost) FROM pyramid_cost_log WHERE slug = ? AND created_at > datetime('now', '-24 hours')` → cost_24h
  - Reconciliation status: aggregate the `reconciliation_status` column of `pyramid_cost_log` for the 24h window. If any row is `'discrepancy'`, status = `'discrepancy'`. If any is `'broadcast_missing'`, status = `'broadcast_missing'`. Else if all are confirmed, status = `'healthy'`. Else `'pending'`.
  - Recent manifest count: `COUNT(*) FROM pyramid_change_manifests WHERE slug = ? AND created_at > datetime('now', '-24 hours')` (if the table exists)
- Aggregate totals across rows
- Return the response

Register in `main.rs::invoke_handler!`.

### 2. Backend: per-pyramid DADBEAR pause/resume IPCs

If `pyramid_dadbear_pause(slug)` and `pyramid_dadbear_resume(slug)` don't already exist, add them. They simply UPDATE `pyramid_dadbear_config SET enabled = 0/1 WHERE slug = ?`.

If they do exist, verify the signatures match the spec (take slug, return `{ ok: bool }` or similar).

### 3. Backend: `pyramid_set_default_norms` IPC (optional — check spec scope)

The spec's "Set Default Norms" button is a UI shortcut to edit the global `dadbear_policy` contribution. Phase 9 + 10 already ship the generative config flow for editing any contribution; the button just opens that flow with schema_type=`dadbear_policy`, slug=None, via the existing `pyramid_generate_config` IPC (Phase 9).

Recommend: NO new IPC. The button is a UI-only affordance that opens the generative config flow with pre-filled schema_type. Document this in the implementation log.

### 4. Backend: `pyramid_dadbear_activity_log` IPC (optional)

Returns the recent DADBEAR tick events for a specific slug — useful for a detail drawer on the oversight page:

```rust
#[derive(Serialize)]
struct DadbearActivityEntry {
    timestamp: String,
    event_type: String,   // "scan_started", "scan_completed", "stale_check", "mutation_applied", "error"
    slug: String,
    node_id: Option<String>,
    details: Option<String>,  // JSON payload
}

#[tauri::command]
async fn pyramid_dadbear_activity_log(
    slug: String,
    limit: Option<i64>,
) -> Result<Vec<DadbearActivityEntry>, String>
```

Sources: `pyramid_stale_check_log` + `pyramid_pending_mutations` + `pyramid_change_manifests`. UNION + ORDER BY timestamp DESC.

Register in `invoke_handler!`.

### 5. Frontend: `DadbearOversightPage.tsx`

New top-level page component. Match existing mode component patterns.

Layout (per the spec line 628-658):
```
DADBEAR Oversight

┌─ Global Controls ──────────────────────────────┐
│  [Pause All]  [Resume All]  [Set Default Norms] │
└────────────────────────────────────────────────┘

Per-Pyramid Status           [Filter: All ▾]
┌──────────────────────────────────────────────────┐
│ slug                                             │
│   Status: Active / Paused                        │
│   Next scan: in 15s / Last scan: 2min ago        │
│   Pending mutations: N                           │
│   In-flight stale checks: M                      │
│   Deferred questions: K                          │
│   Demand signals (24h): D                        │
│   Cost (24h): $X.XX est / $Y.YY actual           │
│   Reconciliation: healthy / pending / discrepancy│
│   [Pause/Resume] [Configure] [View Activity]     │
└──────────────────────────────────────────────────┘

[Cost Reconciliation Section — from Phase 13]

[Provider Health Section]

[Orphan Broadcasts Section]
```

Components used:
- `CostRollupSection.tsx` (Phase 13) — mounted here now. REMOVE the temporary mount from Phase 13's CrossPyramidTimeline (or leave both — your call, but spec intent is this page is the canonical home).
- `ProviderHealthBanner.tsx` (NEW) — shows `pyramid_provider_health` results with per-provider health chip + reason + "Acknowledge" button.
- `OrphanBroadcastsPanel.tsx` (NEW) — shows `pyramid_list_orphan_broadcasts` results with dismissible rows.
- `DadbearPyramidCard.tsx` (NEW) — one card per pyramid, rendering a row from `pyramid_dadbear_overview`.
- `DadbearActivityDrawer.tsx` (NEW) — opens on "View Activity" click, shows `pyramid_dadbear_activity_log` for the selected slug.

### 6. Frontend: `useDadbearOverview.ts` hook

```typescript
export function useDadbearOverview(pollIntervalMs = 10000) {
  const [data, setData] = useState<DadbearOverviewResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const fetch = async () => {
      try {
        const result = await invoke<DadbearOverviewResponse>('pyramid_dadbear_overview');
        if (!cancelled) {
          setData(result);
          setError(null);
        }
      } catch (e: any) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    fetch();
    const interval = setInterval(fetch, pollIntervalMs);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [pollIntervalMs]);

  return { data, loading, error, refetch: () => { /* trigger fetch */ } };
}
```

Also add:
- `useProviderHealth.ts` — wraps `pyramid_provider_health` with a 30s poll.
- `useOrphanBroadcasts.ts` — wraps `pyramid_list_orphan_broadcasts` with manual refresh.

### 7. Frontend: Mount the new page into the app shell

Find the existing mode/page registry. Add `DadbearOversightPage` as a new top-level mode/tab/route.

Recommend naming: "Oversight" or "DADBEAR" in the nav.

### 8. Frontend: relocate `CostRollupSection.tsx`

Phase 13 mounted `CostRollupSection.tsx` on `CrossPyramidTimeline` as a temporary home. Phase 15 moves it to `DadbearOversightPage`. Remove the Phase 13 mount OR keep it in both places (a small duplication is fine). Document the choice.

### 9. Frontend: "Set Default Norms" flow

The "Set Default Norms" button doesn't need a new backend IPC. It opens the existing Phase 9/10 generative config flow with `schema_type = 'dadbear_policy'` and `slug = null` (global). The user refines the global policy, accepts, and the oversight page's next poll reflects the new defaults.

Wire the button to open the Phase 9/10 `CreatePanel` workflow with those preset values. If that's not trivially possible due to how Phase 10 wired the CreatePanel, open a new modal that dispatches the same invoke calls.

### 10. Phase 11 Orphan Broadcast integration

The Phase 11 spec Part 4 calls for a red banner + acknowledge UI for orphan broadcasts. Phase 15 implements this as the `OrphanBroadcastsPanel.tsx` component:

- Red banner at the top of the Oversight page if any unacknowledged orphans exist: "⚠ Orphan broadcasts detected — potential credential leak"
- Clicking the banner scrolls to the Orphan Broadcasts Panel.
- Each orphan row shows: received_at, provider_id, generation_id, step_name, cost_usd, session_id.
- Per-row Acknowledge button — needs a new IPC `pyramid_acknowledge_orphan_broadcast(orphan_id, reason)`. Check if this exists; add it if not.

### 11. Rust tests

- `pyramid_dadbear_overview` IPC handler: seed a DB with multiple slugs, some paused, some with pending mutations, some with demand signals; call the IPC; assert the response shape.
- `pyramid_dadbear_activity_log` IPC (if shipped): seed `pyramid_stale_check_log` + `pyramid_pending_mutations` rows; call the IPC; assert union + ordering.
- Aggregate totals: test that the totals field sums correctly across rows.

### 12. Frontend tests (if test runner exists)

- `DadbearOversightPage` renders loading state, then data, then error.
- `useDadbearOverview` polls on the configured interval.
- `ProviderHealthBanner` renders health colors correctly.

If no test runner, skip and document manual verification.

## Scope boundaries

**In scope:**
- `pyramid_dadbear_overview` IPC
- `pyramid_dadbear_activity_log` IPC (recommended)
- `pyramid_acknowledge_orphan_broadcast` IPC (if not already present)
- Per-pyramid `pyramid_dadbear_pause` / `pyramid_dadbear_resume` IPCs (if not already present)
- `DadbearOversightPage.tsx` + supporting components
- `useDadbearOverview.ts`, `useProviderHealth.ts`, `useOrphanBroadcasts.ts` hooks
- Relocation of `CostRollupSection.tsx` to the new page
- "Set Default Norms" button wiring into Phase 9/10 generative config flow
- Mount of the new page into the dashboard shell
- Rust tests for new IPCs

**Out of scope:**
- Cross-pyramid deferred question management (Phase 12 shipped the reactivation flow)
- New backend metrics infrastructure — Phase 15 assembles existing data
- Advanced filtering/sorting beyond basic "Active/Paused" filter
- CSV export of overview data
- Historical charts (e.g., cost over time) — defer to a follow-up
- Circle/folder scoped pause-all (Phase 14/17 scope)
- Frontend tests if no runner
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` from `src-tauri/` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — Phase 14 count (1170) + new Phase 15 tests. Same 7 pre-existing failures.
3. **Frontend build:** `npm run build` — clean, no new TypeScript errors.
4. **IPC registration:** grep `main.rs` for `pyramid_dadbear_overview` — should appear in function definition AND `invoke_handler!`.
5. **Manual verification path** documented:
   - Launch dev, navigate to Oversight page, see list of pyramids with status cards
   - Click Pause on a pyramid → IPC fires → next poll shows Paused status
   - Click Set Default Norms → opens the generative config flow for dadbear_policy
   - Provider Health section shows green/yellow/red for each provider
   - Orphan Broadcasts panel shows dismissible rows

## Deviation protocol

Standard. Most likely deviations:

- **`pyramid_change_manifests` table may not exist** or be named differently. Check first; omit the recent_manifest_count field if the table doesn't exist.
- **`pyramid_stale_check_log` schema for in_flight detection**: if there's no `completed_at` column, use a different signal (e.g., rows created in last N seconds without a matching completion event).
- **Per-pyramid `pyramid_dadbear_pause` IPC already exists**: don't re-add; just call the existing one.
- **Cost reconciliation status aggregation** may need a different query depending on how `reconciliation_status` is populated.
- **"Set Default Norms" integration** may not be trivial if Phase 10's CreatePanel doesn't expose a preset mechanism. If that's the case, fire a custom event or use URL hash navigation to open ToolsMode → Create with preset state.
- **`pyramid_acknowledge_orphan_broadcast` IPC may already exist** from Phase 11. Check first.

## Implementation log protocol

Append Phase 15 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Include:
1. New IPCs (shape + file:line)
2. New frontend components + mount points
3. CostRollupSection relocation decision
4. Set Default Norms flow implementation
5. Manual verification steps
6. Any deviations with rationale
7. Status: `awaiting-verification`

## Mandate

- **Phase 15 is frontend assembly first.** Most backend work is already done. Do not rebuild existing IPCs.
- **Match existing frontend conventions.** Look at Phase 13's CrossPyramidTimeline + Phase 14's DiscoverPanel for patterns.
- **Do not create a new styling system.** Reuse existing CSS classes from Phase 13's cross-pyramid work.
- **Fix all bugs found.** Standard repo convention.
- **Commit when done.** Single commit with message `phase-15: dadbear oversight page`. Body: 5-8 lines summarizing the page, new IPCs, component composition, and any deviations. Do not amend. Do not push.

## End state

Phase 15 is complete when:

1. `pyramid_dadbear_overview` IPC ships + passes tests.
2. `DadbearOversightPage.tsx` exists and renders per-pyramid status cards, cost rollup, provider health, orphan broadcasts.
3. `useDadbearOverview.ts` hook polls the new IPC on a configurable interval.
4. `ProviderHealthBanner.tsx`, `OrphanBroadcastsPanel.tsx`, `DadbearPyramidCard.tsx`, `DadbearActivityDrawer.tsx` exist and render correctly.
5. Set Default Norms button opens the generative config flow.
6. `CostRollupSection.tsx` is mounted on the new page.
7. New page is registered in the dashboard shell/nav.
8. `cargo check --lib` + `cargo build --lib` + `npm run build` clean.
9. `cargo test --lib pyramid` at Phase 14 count + new tests. Same 7 pre-existing failures.
10. Implementation log Phase 15 entry complete with manual verification steps.
11. Single commit on branch `phase-15-dadbear-oversight`.

Begin with the spec + existing Phase 11/12/13/14 IPC list + existing frontend components. Then build.

Good luck. Build carefully.
