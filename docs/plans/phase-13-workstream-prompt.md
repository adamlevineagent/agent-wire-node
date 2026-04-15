# Workstream: Phase 13 — Build Viz Expansion + Reroll + Cross-Pyramid

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 are shipped. You are the implementer of Phase 13 — build visualization expansion (step-level introspection, per-call trace, cost accumulator, cache indicators), the reroll-with-notes IPC + UI (both for nodes AND for intermediate cache entries), the cross-pyramid build timeline, cross-pyramid cost rollup, and cross-pyramid pause-all DADBEAR.

Phase 13 is large because it rolls up three specs into one initiative phase. Do not defer. Each scope boundary is clearly documented.

## Context

Phase 6 shipped `pyramid_step_cache` + `StepContext` + cache hit/miss events. Phase 12 shipped the cache retrofit sweep so every production LLM call flows through `call_model_unified_with_options_and_ctx` with a cache-aware context. Phase 11 shipped cost reconciliation via OpenRouter Broadcast + cost discrepancy events. The `TaggedKind` enum already has `CacheHit`, `CacheMiss`, `CacheHitVerificationFailed`, `CostUpdate`, `CostReconciliationDiscrepancy`, `ProviderHealthChanged`, `ChainStepStarted`, `ChainStepFinished`, `BuildProgressV2`.

What's missing per the spec:
- `LlmCallStarted`, `LlmCallCompleted`, `StepRetry`, `StepError`
- `WebEdgeStarted`/`WebEdgeCompleted`
- `EvidenceProcessing`, `TriageDecision`, `GapProcessing`, `ClusterAssignment`
- `NodeRerolled`, `CacheInvalidated`, `ManifestGenerated`

The existing `PyramidBuildViz.tsx` is ~334 lines and shows pyramid layer/cell progress. It has no step timeline, no cost accumulator, no cache indicator, no reroll, no per-call trace.

There is no cross-pyramid view component yet. The cross-pyramid event router doesn't exist.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/build-viz-expansion.md` in full (630 lines).** Primary implementation contract for sections 1-3 (step timeline, reroll IPC, reroll UI).
3. **`docs/specs/cross-pyramid-observability.md` in full (489 lines).** Primary implementation contract for sections 4-6 (cross-pyramid router, cost rollup, pause-all).
4. `docs/specs/llm-output-cache.md` — read the "Reroll + Notes" and "Supersession" sections. Phase 13 consumes `pyramid_step_cache` provenance for the reroll flow.
5. `docs/specs/change-manifest-supersession.md` — read the "note" field handling. Phase 13 writes notes into manifests.
6. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 13 section (~line 268).
7. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 6, 11, 12 entries for event bus patterns.

### Code reading

8. **`src-tauri/src/pyramid/event_bus.rs` in full.** Understand existing variants, `is_discrete()`, 60ms coalescing, `TaggedBuildEvent` wrapper shape, and the Phase 4/6/11 variant placements so new variants land in the right section with the right serde attributes.
9. **`src/components/PyramidBuildViz.tsx` in full (~334 lines).** Understand the existing `BuildLayerState`, the polling + WebSocket pattern, the React state shape. You are EXTENDING this file, not replacing it. The cross-pyramid view composes it.
10. **`src-tauri/src/pyramid/llm.rs`** — find `call_model_unified_with_options_and_ctx`. Locate the places where the existing `CacheHit` / `CacheMiss` / `CacheHitVerificationFailed` events are emitted. Phase 13 adds `LlmCallStarted` (before HTTP send), `LlmCallCompleted` (after successful response parse), `StepRetry` (per retry attempt), and `StepError` (after retries exhausted).
11. **`src-tauri/src/pyramid/step_context.rs`** — the `StepContext` already has `bus: Option<Arc<BuildEventBus>>`. Phase 13 uses this existing field, no new plumbing needed.
12. `src-tauri/src/pyramid/chain_executor.rs` — find `execute_webbing` (or equivalent web edge generation) + `recursive_cluster` + the gap processing functions. Phase 13 adds event emission at these sites.
13. `src-tauri/src/pyramid/triage.rs` (new in Phase 12) — add `TriageDecision` event emission.
14. `src-tauri/src/pyramid/evidence_answering.rs` — add `EvidenceProcessing` events at `answer_questions` / `run_triage_gate`.
15. `src-tauri/src/pyramid/stale_helpers_upper.rs` — `generate_change_manifest`. Phase 13 emits `ManifestGenerated`.
16. `src-tauri/src/pyramid/db.rs` — find `pyramid_cost_log`, `pyramid_build_runs`, `pyramid_pipeline_steps`, `pyramid_step_cache` schemas. Understand columns available for cross-pyramid rollup queries.
17. `src-tauri/src/main.rs` — find the `invoke_handler!` list. You'll register new IPC commands: `pyramid_reroll_node` (or extend existing), `pyramid_cost_rollup`, `pyramid_active_builds`, `pyramid_pause_dadbear_all`, `pyramid_resume_dadbear_all`.
18. `src-tauri/src/pyramid/routes.rs` — grep for `handle_reroll` if any exists. Add new route handlers as needed.
19. `src/hooks/` — look for existing WebSocket/event-listener patterns. Phase 13 adds `useStepTimeline.ts` + `useBuildRowState.ts` + `useCrossPyramidTimeline.ts`.
20. `src/components/modes/` — how existing mode components are wired into the dashboard shell. The Cross-Pyramid Timeline may be a new mode or a new sub-page.

## What to build

### Part A — Build Viz Expansion (step timeline + cache indicators + cost accumulator)

#### A1. TaggedKind extensions (backend)

Add these variants to `event_bus.rs` in a new `// ── Phase 13: Build Viz Expansion ──` section. Match the existing `#[serde(tag = "type", rename_all = "snake_case")]` attribute.

- `LlmCallStarted { slug, build_id, step_name, primitive, model_tier, model_id, cache_key, depth, chunk_index }`
- `LlmCallCompleted { slug, build_id, step_name, cache_key, tokens_prompt, tokens_completion, cost_usd, latency_ms, model_id }`
- `StepRetry { slug, build_id, step_name, attempt, max_attempts, error, backoff_ms }`
- `StepError { slug, build_id, step_name, error, depth, chunk_index }`
- `WebEdgeStarted { slug, build_id, step_name, source_node_count }`
- `WebEdgeCompleted { slug, build_id, step_name, edges_created, latency_ms }`
- `EvidenceProcessing { slug, build_id, step_name, question_count, action, model_tier }`
- `TriageDecision { slug, build_id, step_name, item_id, decision, reason }`
- `GapProcessing { slug, build_id, step_name, depth, gap_count, action }`
- `ClusterAssignment { slug, build_id, step_name, depth, node_count, cluster_count }`
- `NodeRerolled { slug, build_id, node_id, step_name, note, new_cache_entry_id, manifest_id }`
- `CacheInvalidated { slug, build_id, cache_key, reason }`
- `ManifestGenerated { slug, build_id, manifest_id, depth, node_id }`

All are "discrete" (low-frequency, each one matters) so they naturally bypass the 60ms coalesce — no `is_discrete()` change needed as long as you keep Progress/V2Snapshot as the only coalesced variants.

#### A2. Event emission points (backend)

- **`llm.rs::call_model_unified_with_options_and_ctx`**: emit `LlmCallStarted` before the HTTP send, `LlmCallCompleted` after successful response parse (include actual_cost_usd if OpenRouter returned it, else estimated), `StepRetry` on each retry attempt inside the retry loop, `StepError` after retries exhausted. Use the existing `ctx.bus` + `ctx.slug` + `ctx.build_id`. Gate on `ctx.bus.is_some()` (no-op if the call is non-observable).
- **`chain_executor.rs::execute_webbing`**: emit `WebEdgeStarted` at entry and `WebEdgeCompleted` at return.
- **`chain_executor.rs::recursive_cluster`**: emit `ClusterAssignment` per cluster decision.
- **`evidence_answering.rs::run_triage_gate`**: emit `EvidenceProcessing { action: "triage" }` at start, then per-question triage emits `TriageDecision` (wired via `triage.rs`).
- **`evidence_answering.rs::answer_questions`**: emit `EvidenceProcessing { action: "answer" }` at start of the answering loop.
- **`triage.rs::triage_evidence_question` / the `rule_to_decision` helper**: emit `TriageDecision` at the point a decision is made.
- **`stale_helpers_upper.rs::generate_change_manifest`**: emit `ManifestGenerated` after the manifest row is inserted.
- **`pyramid_reroll_node` IPC handler (new, see A4)**: emit `NodeRerolled` after the new cache entry is written.
- **Cache invalidation walker (new, see A5)**: emit `CacheInvalidated` for each entry marked stale.

#### A3. `pyramid_step_cache` pre-population query

Add IPC `pyramid_step_cache_for_build(slug, build_id) -> Vec<CacheEntrySummary>` returning step_name, model_id, cost_usd, latency_ms, cache_key, created_at for pre-populating the viz on resume. Add a helper in `db.rs`.

#### A4. `pyramid_reroll_node` IPC (extended)

Per the spec, the reroll IPC supports BOTH a `node_id` (for node-creating reroll) AND an optional `cache_key` (for intermediate-output reroll). Exactly one must be present.

Shape:
```rust
#[derive(Deserialize)]
struct RerollInput {
    slug: String,
    node_id: Option<String>,
    cache_key: Option<String>,
    note: String,
    force_fresh: bool,  // clients always send true
}

#[derive(Serialize)]
struct RerollOutput {
    new_cache_entry_id: i64,
    manifest_id: Option<i64>,  // None when rerolling a non-node cache entry (intermediate output)
    new_content: Value,
}
```

Backend behavior:
1. Validate exactly one of `node_id` / `cache_key` is provided.
2. Resolve the target cache entry:
   - If `node_id`: look up the most-recent cache entry for the node's producing step (walk the build's step → cache link).
   - If `cache_key`: direct lookup by `(slug, cache_key)` in `pyramid_step_cache`.
3. Load the original prompt + inputs from the linked `pyramid_llm_audit` row (or reconstruct from the cache entry's stored metadata — whichever is authoritative in the current schema).
4. Construct the reroll prompt: original system prompt + a templated addition like `"The user requested a different output. Their feedback: {note}. The current output you should address: {cached_output}. Produce an improved version that incorporates their concern."`
5. Call the LLM via `call_model_unified_with_options_and_ctx` with a `StepContext` that has `force_fresh = true`, the same slug/build_id/step_name/depth/chunk_index as the original, and the resolved bus.
6. The cache layer stores the new row with `supersedes_cache_id = original.id`. Add a `note` column to `pyramid_step_cache` if it doesn't exist; populate it on reroll writes only.
7. For `node_id` reroll: also write a `pyramid_change_manifests` row with the note field populated. For `cache_key` (intermediate) reroll: skip the manifest — the change manifest is for node-level changes only, per the existing spec.
8. Call the downstream cache invalidation walker (A5) to mark dependents stale.
9. Emit `NodeRerolled` event.
10. Return the new cache_entry_id, manifest_id, and the new content.

**Anti-slot-machine rate limit:** count recent rerolls per node via `SELECT COUNT(*) FROM pyramid_step_cache WHERE slug=? AND step_name=? AND chunk_index=? AND created_at > datetime('now', '-10 minutes') AND supersedes_cache_id IS NOT NULL`. If count >= 3, include a warning flag in the response so the UI can surface the "providing specific feedback usually produces better results" banner. Do NOT hard-block.

#### A5. Downstream cache invalidation walker

New module or extension of existing cache code: `invalidate_downstream(conn, slug, rerolled_cache_key) -> Result<Vec<String>>` returning the list of invalidated cache keys.

Approach:
- Query `pyramid_step_cache` for entries whose `inputs_hash` depends on the rerolled output. Since the current `inputs_hash` is a SHA-256 of concatenated prompts (no explicit dependency graph), approximate by:
  1. Finding the rerolled step's producing node (if `node_id` linkage exists)
  2. Walking forward through `pyramid_evidence` KEEP edges (the evidence graph) to find downstream nodes
  3. For each downstream node, find its producing cache entry
  4. Mark it stale by adding an `invalidated_by` column to `pyramid_step_cache` (or reuse `force_fresh` semantics with a sentinel), so subsequent lookups treat the row as a miss
- Emit `CacheInvalidated` per invalidated entry.

If the dependency graph isn't trivially walkable from what's stored, document the limitation in the implementation log and ship the node-level invalidation only (walk the evidence graph for the node, stop at the first dependent step — no transitive walk). Transitive walking can be a Phase 13 follow-up.

#### A6. Frontend: Step Timeline in PyramidBuildViz

Extend `src/components/PyramidBuildViz.tsx`:

- Add a step timeline panel below the existing pyramid visualization.
- New React state: `StepTimelineState` with `steps: StepState[]`, `cost: CostAccumulator`, `expandedStep: string | null`.
- New event reducer: `reduceStepTimelineEvent(state, event)` handling all the new TaggedKind variants.
- Step rows render with the per-state visual treatment from the spec (pending/running/completed/cached/partial_cache/failed/retrying).
- Cost accumulator at top: `Cost: $X.XX est / $Y.YY actual | Cache savings: $Z.ZZ`.
- Clicking a step row expands to show per-call sub-rows (from `StepCall` state).

Extract `useBuildRowState.ts` (new hook) to encapsulate the reducer so the cross-pyramid view can reuse it. The hook takes initial state and an event stream and returns `(state, dispatch)`.

Extract `useStepTimeline.ts` (new hook) as a thin wrapper around `useBuildRowState` for the per-pyramid view with pre-population via the `pyramid_step_cache_for_build` IPC on mount.

#### A7. Frontend: Reroll modal

New component: `src/components/RerollModal.tsx`. Takes `{ slug, target: { type: "node", nodeId } | { type: "cache", cacheKey }, currentContent, onClose, onRerolled }`.

- Shows the current output content (read-only preview)
- "Why reroll?" textarea (labeled "strongly encouraged")
- "Reroll" button
- If the note textarea is empty, clicking "Reroll" first shows a confirm prompt: "Rerolling without feedback will just re-run the LLM with different randomness. Continue anyway?"
- If the backend response includes a rate-limit warning flag, render a banner: "You've rerolled this node N times. Providing specific feedback usually produces better results than additional attempts."
- Calls `invoke('pyramid_reroll_node', { slug, nodeId, cacheKey, note, force_fresh: true })`.

Mount from:
- The existing node context menu in `PyramidBuildViz.tsx` (add a "Reroll..." item)
- Each step row's expanded per-call sub-row (add a small "Reroll" button alongside the status/model/cost)
- The cross-pyramid timeline (Part B) — same modal component rendered from a different parent

### Part B — Cross-Pyramid Timeline + Rollup + Pause-All

#### B1. Backend: Cross-Pyramid Event Router

New module: `src-tauri/src/pyramid/cross_pyramid_event_router.rs`.

```rust
pub struct CrossPyramidEventRouter {
    subscribers: Arc<Mutex<Vec<tokio::sync::mpsc::Sender<TaggedBuildEvent>>>>,
    active_slugs: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl CrossPyramidEventRouter {
    pub fn new() -> Self { ... }
    pub fn register_slug(&self, slug: String, bus: Arc<BuildEventBus>);
    pub fn unregister_slug(&self, slug: &str);  // called when a build completes; keeps task alive for 60s grace then drops
    pub fn subscribe(&self) -> tokio::sync::mpsc::Receiver<TaggedBuildEvent>;
}
```

Add a `cross_pyramid_router: Arc<CrossPyramidEventRouter>` field to `PyramidState`. Wire it at state construction time in `main.rs`. Whenever a build starts and constructs/retrieves a `BuildEventBus`, call `router.register_slug(slug, bus)`.

Tauri emission: on each event received by the router's subscriber, re-emit via `app_handle.emit_all("cross-build-event", &event)`. The frontend listens to `"cross-build-event"`.

#### B2. Backend: `pyramid_active_builds` IPC

Query `pyramid_build_runs` + `pyramid_pipeline_steps` + `pyramid_step_cache` (using the spec's SQL around line 264). Return `Vec<ActiveBuildRow>`.

Register IPC in `main.rs`.

#### B3. Backend: `pyramid_cost_rollup` IPC

Parse the range parameter (`today`/`week`/`month`/`custom`) into `(from, to)` ISO timestamps. For `custom`, validate the `from`/`to` ISO strings are parseable and within a 1-year cap.

Query `pyramid_cost_log` with the GROUP BY `slug, provider, operation` from the spec (line 166). Return `CostRollupResponse` with buckets per the spec.

Do NOT add the `pyramid_cost_summary` materialized table in Phase 13 — that's the fallback. Direct query is fine for the target user count. Document the fallback as deferred to a performance follow-up.

#### B4. Backend: `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` IPC

Bulk UPDATE on `pyramid_dadbear_config.enabled` with the three scope behaviors (`all` / `folder` / `circle`). Return `{ affected: u64 }`.

Idempotent — pausing an already-paused pyramid has no effect, `affected` only counts rows where the UPDATE changed state.

Register both IPCs in `main.rs`.

#### B5. Frontend: Cross-Pyramid Timeline

New components:
- `src/components/CrossPyramidTimeline.tsx` — top-level page. Subscribes to `"cross-build-event"` Tauri event. On mount calls `pyramid_active_builds` to seed state. Renders active builds + recent list + cost accumulator.
- `src/components/ActiveBuildRow.tsx` — compact row per build with progress bar, current step, cost, cache %, "View" button.
- `src/components/CrossPyramidCostFooter.tsx` — running cost totals across all active builds.
- `src/hooks/useCrossPyramidTimeline.ts` — reduces cross-pyramid events into a `Map<slug, BuildRowState>` using the shared `useBuildRowState` logic.

Mount under the existing dashboard shell as a new mode (look at `src/components/modes/` for the pattern). Name it "Builds" or "Activity" depending on existing naming — match conventions.

Clicking "View" on an active build opens the existing `PyramidBuildViz.tsx` in a drawer or modal using the clicked slug.

#### B6. Frontend: Cost Rollup Section

New component: `src/components/CostRollupSection.tsx`. Shows:
- Range selector (Today/Week/Month/Custom)
- Client-side pivots (by pyramid, by provider, by operation)
- Reconciliation health indicator (count of pyramids with >10% delta)

Mounted on the DADBEAR Oversight page (Phase 15). For Phase 13, mount it on the Cross-Pyramid Timeline view as a sub-section so the operator sees it during the overnight build. Phase 15 will move/reuse it.

#### B7. Frontend: Pause All button + confirmation modal

Add to `CrossPyramidTimeline.tsx` header:
- "Pause All DADBEAR" button
- Clicking opens a confirmation modal: "Pause DADBEAR on N pyramids?" with the count from `pyramid_active_builds`
- Confirm calls `pyramid_pause_dadbear_all({ scope: "all" })`
- After success: toast "Paused DADBEAR on N pyramids", banner "DADBEAR Paused (N pyramids) [Resume]" at top of view
- "Resume" button calls `pyramid_resume_dadbear_all({ scope: "all" })`

Folder/circle scopes are deferred to Phase 14/15 — add the `scope: "all"` path only in Phase 13 and document the others as deferred.

### Part C — Tests

#### Rust tests
- `event_bus.rs` — add/extend tests confirming each new TaggedKind variant serializes correctly with the snake_case tag.
- `llm.rs` — tests confirming `LlmCallStarted`/`LlmCallCompleted` events are emitted via the ctx.bus when a StepContext is present. Use a channel-backed bus, make a mocked HTTP call (or use the existing Phase 6 test helper), assert the expected events arrive.
- `reroll` — new test file `tests_reroll.rs` or similar. Tests:
  - `test_reroll_by_node_id_creates_new_cache_entry_and_manifest`
  - `test_reroll_by_cache_key_creates_new_cache_entry_no_manifest`
  - `test_reroll_must_specify_exactly_one_target`
  - `test_reroll_supersedes_original_via_supersedes_cache_id`
  - `test_reroll_force_fresh_bypasses_cache_lookup`
  - `test_reroll_rate_limit_warning_after_3_in_10_minutes`
  - `test_reroll_invalidates_downstream_cache_entries` (spot check)
- `cross_pyramid_event_router.rs` — tests:
  - `test_router_forwards_events_from_one_slug`
  - `test_router_forwards_events_from_multiple_slugs_concurrently`
  - `test_router_grace_period_keeps_forwarder_alive_after_build_completes`
  - `test_router_unregister_after_grace_drops_forwarder`
- `cost_rollup` — tests:
  - `test_cost_rollup_today_range`
  - `test_cost_rollup_custom_range_validates_1_year_cap`
  - `test_cost_rollup_group_by_slug_vs_provider_vs_operation`
- `pause_all` — tests:
  - `test_pause_all_scope_all_sets_every_enabled_row_to_false`
  - `test_pause_all_scope_folder_matches_source_path_prefix`
  - `test_pause_all_idempotent_zero_affected_on_second_call`
  - `test_resume_all_mirrors_pause_all`

#### Frontend tests (only if a test runner exists — check `package.json`)

- Reducer tests for `useBuildRowState` — pure function, feeds synthetic event streams, asserts state transitions match the spec table (pending → running → cached/completed/failed).
- Reducer tests for `useCrossPyramidTimeline` — multi-slug event streams.
- `RerollModal` rendering test — empty-note confirm path.

If no test runner exists (look at Phase 8's history — it noted there isn't one), skip frontend tests and document manual verification steps.

## Scope boundaries

**In scope:**
- TaggedKind extensions (13 new variants)
- Event emission in llm.rs, chain_executor.rs, evidence_answering.rs, triage.rs, stale_helpers_upper.rs
- `pyramid_reroll_node` IPC with node_id + cache_key support
- Downstream cache invalidation (minimal walker; document limitations)
- `pyramid_step_cache` note column (if not present)
- `pyramid_step_cache_for_build` IPC for pre-population
- Step timeline UI in PyramidBuildViz.tsx
- Cost accumulator + cache indicator UI
- RerollModal component + mounts
- `CrossPyramidEventRouter` backend
- `pyramid_active_builds` IPC
- `pyramid_cost_rollup` IPC
- `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` IPC (scope=all only)
- CrossPyramidTimeline + ActiveBuildRow + CostRollupSection + CrossPyramidCostFooter frontend
- Shared `useBuildRowState.ts` hook
- Rust tests for all new modules
- Implementation log entry

**Out of scope:**
- DADBEAR Oversight page integration — that's Phase 15. For Phase 13, mount the cost rollup on the Cross-Pyramid Timeline view as a placeholder. Phase 15 will move it.
- Materialized `pyramid_cost_summary` table — deferred performance optimization.
- Transitive downstream cache invalidation beyond one level — deferred; document in log.
- Folder/circle scope for pause-all — deferred to Phase 14/15.
- Historical build report from `pyramid_llm_audit` — spec notes this is a future work item, not Phase 13.
- Build history page — spec notes it's a natural follow-up, not Phase 13.
- Frontend tests if no test runner exists
- CSS overhaul — match existing conventions, minimal new styles
- Mobile/narrow-layout responsiveness beyond existing patterns
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` from `src-tauri/` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — Phase 12 count (1101) + new Phase 13 tests. Same 7 pre-existing failures.
3. **Frontend build:** `npm run build` (or equivalent — check `package.json` scripts) — clean, no new TypeScript errors. No new frontend lint errors.
4. **Event emission smoke test:** manual verification path documented in the implementation log — start a dev build, observe step timeline populating with LlmCallStarted/Completed events, verify cost accumulator ticks up, verify cache hits render with green flash treatment.
5. **Reroll smoke test:** manual verification path — right-click a node in the build viz, pick Reroll, enter a note, submit, verify the new content replaces the old and the cache table has a superseded row with the note.
6. **Cross-pyramid timeline smoke test:** launch the app with 2+ pyramids, start builds on each, switch to the Cross-Pyramid Timeline view, verify both rows update in real-time.
7. **Pause-all smoke test:** click Pause All, confirm modal shows accurate count, confirm, verify all pyramid_dadbear_config rows show enabled=0 in a db dump, click Resume All, verify they flip back to enabled=1.

## Deviation protocol

Standard. Most likely deviations:

- **`pyramid_llm_audit` row linkage to cache entries** — if the current schema doesn't have a clean foreign key, the reroll path may need to store enough metadata on `pyramid_step_cache` itself (inputs_hash, original prompt, or a pointer to the audit row). Ship the simplest path that works; document the schema choice.
- **Downstream invalidation walker depth** — if the evidence graph walk is non-trivial to query, ship node-level invalidation only (invalidate the single rerolled entry + any immediate dependents) and document transitive walking as deferred.
- **Cost rollup with NULL actual_cost** — when Broadcast hasn't confirmed a row yet, `actual_cost_usd` is NULL. Use `COALESCE(actual_cost_usd, estimated_cost_usd)` for totals or report both separately. Document the choice.
- **Frontend polling vs WebSocket** — if the existing PyramidBuildViz uses polling and Tauri events, the new step timeline should use the same channel (Tauri `listen` events) rather than introducing a new WebSocket. Match the existing pattern.
- **`pyramid_step_cache.note` column** — if adding the column requires a migration, gate it via `pragma_table_info` check inside `init_pyramid_db` (Phase 4's idempotency pattern).
- **Cross-pyramid router lifetime** — per the spec, keep the forwarder task alive for 60 seconds after the last event to catch late arrivals. If a cleaner approach emerges (e.g., explicit "build completed" signal), use that and document.

## Implementation log protocol

Append Phase 13 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Include:

1. TaggedKind variants added.
2. Event emission sites added with file:line references.
3. Reroll IPC shape + behavior summary.
4. Downstream invalidation walker scope (single-level vs transitive).
5. Cross-pyramid router design (Option A per spec).
6. New frontend components + their mount points.
7. `useBuildRowState` extraction details.
8. Tests added and passing.
9. Manual verification steps for step timeline, reroll, cross-pyramid timeline, pause-all.
10. Any deviations from the spec, with rationale.
11. Status: `awaiting-verification`.

## Mandate

- **No backend API contract breaks.** Phase 4-12 IPC must keep working. Extend.
- **Phase 6's StepContext is the reachable path.** Use `ctx.bus` for event emission — do not invent a new event channel.
- **Fix all bugs found during the sweep.** Standard repo convention. If you find dead code in event emission (e.g., an event type defined but never emitted), emit it correctly or remove it.
- **Match existing frontend conventions.** Look at `PyramidBuildViz.tsx`, `ToolsMode.tsx`, `AddWorkspace.tsx`, `Settings*.tsx` for CSS/styling. Do NOT introduce a new styling system.
- **Notes enforcement is UX-level, backend is the safety net.** The UI should discourage empty notes but the backend accepts them (with a warning flag in the response). Do not hard-reject empty notes in the IPC — the UX pressure is sufficient and the backend is a last resort for malicious/automated callers that don't respect the UI.
- **Commit when done.** Single commit with message `phase-13: build viz expansion + reroll + cross-pyramid`. Body: 8-12 lines summarizing the step timeline, reroll, cache invalidation, cross-pyramid router, cost rollup, and pause-all. Do not amend. Do not push.

## End state

Phase 13 is complete when:

1. 13 new `TaggedKind` variants land in `event_bus.rs` with snake_case serde tags.
2. Events are emitted at every site listed in A2, with tests proving at least the LLM call + reroll + cross-pyramid paths fire correctly.
3. `pyramid_reroll_node` IPC supports both node_id and cache_key targets, respects force_fresh, writes change manifests for node reroll, and invalidates downstream cache entries at least at the single-level depth.
4. `pyramid_step_cache_for_build` IPC exists for viz pre-population.
5. `PyramidBuildViz.tsx` has a step timeline panel with per-step status rows, per-call sub-rows, cost accumulator, cache indicators, and reroll buttons.
6. `RerollModal.tsx` exists and is mounted from PyramidBuildViz + cross-pyramid view.
7. `CrossPyramidEventRouter` exists as a backend module and is wired into PyramidState.
8. `pyramid_active_builds`, `pyramid_cost_rollup`, `pyramid_pause_dadbear_all`, `pyramid_resume_dadbear_all` IPCs are registered and tested.
9. `CrossPyramidTimeline.tsx`, `ActiveBuildRow.tsx`, `CostRollupSection.tsx`, `CrossPyramidCostFooter.tsx`, `useBuildRowState.ts`, `useCrossPyramidTimeline.ts` exist and render correctly.
10. `cargo check --lib` + `cargo build --lib` + frontend build clean.
11. `cargo test --lib pyramid` at prior count + new Phase 13 tests. Same 7 pre-existing failures.
12. Implementation log Phase 13 entry complete with manual verification steps.
13. Single commit on branch `phase-13-build-viz-reroll`.

Begin with the spec files (both in full). Then the event_bus + PyramidBuildViz code. Then wire. The event-emission retrofit and the reroll IPC are the two most likely places to hit unforeseen complexity — tackle them early so scope remains honest.

Good luck. Build carefully.
