# Workstream: Phase 14 — Wire Discovery & Ranking

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13 are shipped. You are the implementer of Phase 14 — the ranking layer over Wire search, the recommendations engine, the supersession notification/update system, quality badges UI, and the previously-stubbed Wire search/pull IPC commands.

Phase 10 placeholder'd the Discover tab with a "Coming in Phase 14" message because `pyramid_search_wire_configs` and `pyramid_pull_wire_config` didn't exist. Phase 14 ships them AND adds the ranking layer, recommendations, and update flow.

## Context

Phase 5 shipped Wire publishing (`pyramid_publish_to_wire` + `PyramidPublisher`). Phase 10 shipped the ToolsMode Discover tab as a placeholder. Phase 5's `PyramidPublisher` in `wire_publish.rs` is the existing Wire HTTP client — extend it with search, pull, and supersession-check methods.

There is currently NO `pyramid_search_wire_configs` IPC or `pyramid_pull_wire_config` IPC. Phase 14 creates them.

`wire_discovery_weights` is a schema_type that does NOT currently exist in the bundled contributions manifest (`src-tauri/assets/bundled_contributions.json`). Phase 14 adds it.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/wire-discovery-ranking.md` in full (419 lines).** Primary implementation contract.
3. `docs/specs/config-contribution-and-wire-sharing.md` — scan the "Pull flow" section for the pull semantics Phase 14 will implement.
4. `docs/specs/wire-contribution-mapping.md` — scan the "Canonical Wire Native Documents" section. Phase 14's search endpoint reads this metadata from Wire's response.
5. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 14 section (~line 280).

### Code reading

6. **`src-tauri/src/pyramid/wire_publish.rs`** — read the `PyramidPublisher::post_contribution`, `publish_contribution_with_metadata`, `export_cache_manifest` patterns. You extend this struct with new methods: `search_contributions`, `pull_contribution`, `check_supersessions`, `fetch_contribution_metadata`.
7. **`src-tauri/src/pyramid/config_contributions.rs`** — understand `insert_contribution`, the `source` field (`"bundled" | "local" | "wire" | "import"`), and `sync_config_to_operational`. Phase 14's pull flow lands contributions with `source = "wire"`.
8. **`src-tauri/src/pyramid/wire_native_metadata.rs`** — `WireNativeMetadata` + `WirePublicationState`. Phase 14 reads these from pulled contributions.
9. `src-tauri/src/pyramid/wire_migration.rs` — bundled seed migration. Phase 14 adds a `wire_discovery_weights` bundled contribution.
10. `src-tauri/assets/bundled_contributions.json` — the current seed manifest. You add the `wire_discovery_weights` + `wire_auto_update_settings` entries.
11. `src-tauri/src/main.rs` — find `invoke_handler!` list and the Wire-related IPC block. Register new IPCs there.
12. **`src/components/modes/ToolsMode.tsx`** — find `DiscoverPanel` (the placeholder from Phase 10). Phase 14 replaces it with the real discovery UI. Also find `MyToolsPanel` — Phase 14 extends it with update badges.
13. `src/components/ContributionDetailDrawer.tsx` (Phase 10) — Phase 14 can reuse the pattern or extend it for the update drawer.
14. `src/components/YamlConfigRenderer.tsx` (Phase 8) — used for rendering the contribution preview in the update drawer and discover detail drawer.
15. `src/hooks/` — check for existing Tauri `invoke` wrappers. Phase 14 adds `useWireDiscovery.ts`, `useWireUpdates.ts`, etc.

## What to build

### 1. Backend: Wire HTTP client extensions

Extend `PyramidPublisher` in `wire_publish.rs`:

```rust
impl PyramidPublisher {
    /// POST /api/v1/contributions/search
    /// Returns a flat list of Wire contributions matching the query.
    pub async fn search_contributions(
        &self,
        schema_type: &str,
        query: Option<&str>,
        tags: Option<&[String]>,
        limit: u32,
    ) -> Result<Vec<WireContributionSearchResult>>

    /// GET /api/v1/contributions/{wire_contribution_id}
    /// Returns the full contribution metadata + yaml_content.
    pub async fn fetch_contribution(
        &self,
        wire_contribution_id: &str,
    ) -> Result<WireContributionFull>

    /// POST /api/v1/contributions/check_supersessions
    /// Input: list of wire_contribution_ids the user has pulled.
    /// Output: for each ID, whether a newer version exists.
    pub async fn check_supersessions(
        &self,
        contribution_ids: &[String],
    ) -> Result<Vec<SupersessionCheckEntry>>
}
```

Shape the response types as the spec's "IPC Contract" section prescribes. The Wire HTTP endpoints may not exist on the server side yet — this is shipping both halves of the discovery contract. For local testing, mock the Wire responses. In production, the Wire server will need matching handlers (out of Phase 14 scope; document the Wire-side dependency).

**HTTP path:** use the existing session API token pattern (`get_api_token(&state.auth)`). Base URL from the existing `WIRE_URL` env var / settings.

### 2. Backend: ranking engine

New module: `src-tauri/src/pyramid/wire_discovery.rs`.

```rust
pub struct RankingSignals {
    pub rating: Option<f32>,           // 1-5
    pub adoption_count: u64,
    pub freshness_days: u32,
    pub chain_length: u32,
    pub reputation: Option<f32>,
    pub upheld_rebuttals: u32,
    pub filed_rebuttals: u32,
    pub kept_count: u64,
    pub total_pullers: u64,
}

pub struct RankingWeights {
    pub rating: f64,
    pub adoption: f64,
    pub freshness: f64,
    pub chain: f64,
    pub reputation: f64,
    pub challenge: f64,
    pub internalization: f64,
}

impl Default for RankingWeights {
    // spec line 73-83 — seed weights
}

pub fn normalize_signals(signals: &RankingSignals, max_adoption_in_set: u64) -> NormalizedSignals

pub fn compute_score(normalized: &NormalizedSignals, weights: &RankingWeights) -> f64

pub fn explain_ranking(
    entry: &DiscoveryResult,
    normalized: &NormalizedSignals,
    raw: &RankingSignals,
) -> Option<String>
```

Implementation notes:
- **Missing-signal redistribution**: if a signal is `None`/zero-by-default (e.g., a brand-new contribution with no adoption or rebuttals), its weight is redistributed across present signals. Do NOT treat missing signals as zeros that drag the score down — the spec is explicit that new contributions get a fair shot.
- **Normalization happens against the result set max**, not a global max. `max_adoption_in_set` is computed from the search results.
- **Freshness decay**: `max(0, 1 - days_since_update / 180)` — linear decay over 180 days.
- **Chain length**: `min(chain_length / 10, 1.0)`.
- **Challenge**: `1 - (upheld_rebuttals / (filed_rebuttals + 1))`.
- **Internalization**: `kept_count / max(1, total_pullers)`.

Weights are loaded from the active `wire_discovery_weights` contribution via `load_active_config_contribution`. Cached in-memory with a 5-minute TTL per the spec. On supersession of the weights contribution, the cache is invalidated (use the existing event bus or just set `last_refreshed < 5min ago` on each lookup).

### 3. Backend: recommendations engine

Add to `wire_discovery.rs`:

```rust
pub struct PyramidProfile {
    pub slug: String,
    pub source_type: Option<String>,       // "code" | "document" | "conversation" | "mixed"
    pub tier_routing_providers: Vec<String>,  // sorted unique provider_ids
}

pub async fn compute_recommendations(
    publisher: &PyramidPublisher,
    profile: &PyramidProfile,
    schema_type: &str,
    limit: u32,
) -> Result<Vec<Recommendation>>
```

Implementation:
1. Load the pyramid's profile from local DB: source_type from `pyramid_metadata` or equivalent; tier_routing_providers from `pyramid_tier_routing`.
2. Call `publisher.search_contributions(schema_type, None, None, 100)` to get a broad candidate set.
3. For each candidate, compute a similarity score based on the spec's signal table (source_type_overlap, tier_routing_similarity).
4. Return top-N with rationale strings.

**V1 scope** per the spec: only source_type overlap + tier_routing_similarity. Apex embedding similarity is v2. Cross-schema recommendations are v2. Document both as deferred.

**Rationale string examples:**
- "Used by 3 code pyramids with similar tier routing"
- "Top-rated {schema_type} for code pyramids using local models"
- "Pulled by N users with matching tier routing"

### 4. Backend: supersession polling

New module: `src-tauri/src/pyramid/wire_update_poller.rs`.

```rust
pub struct WireUpdatePoller {
    state: Arc<PyramidState>,
    interval_secs: u64,  // default from wire_update_polling contribution, fallback 6 hours
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl WireUpdatePoller {
    pub fn start(state: Arc<PyramidState>);
    pub fn stop(&mut self);

    async fn run_once(&self) -> Result<()> {
        // 1. List all local contributions with wire_contribution_id != NULL AND status = 'active'
        // 2. Group by schema_type
        // 3. Call publisher.check_supersessions() for each group
        // 4. Upsert results into pyramid_wire_update_cache
        // 5. If the contribution's schema_type has auto_update enabled, pull latest + activate
        //    (credential safety gate: refuse if new version introduces new credential refs)
        // 6. Emit WireUpdateAvailable event (new TaggedKind variant) for each update found
    }
}
```

Wire the poller into `main.rs` at state construction time. Start it after the database is initialized.

**New TaggedKind variant** in `event_bus.rs`:
- `WireUpdateAvailable { local_contribution_id, schema_type, latest_wire_contribution_id, chain_length_delta }`
- `WireAutoUpdateApplied { local_contribution_id, schema_type, new_local_contribution_id, chain_length_delta }`

### 5. Backend: pyramid_wire_update_cache table

Add to `db.rs`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_wire_update_cache (
    local_contribution_id TEXT PRIMARY KEY
        REFERENCES pyramid_config_contributions(contribution_id),
    latest_wire_contribution_id TEXT NOT NULL,
    chain_length_delta INTEGER NOT NULL,
    changes_summary TEXT,
    author_handles_json TEXT,
    checked_at TEXT NOT NULL DEFAULT (datetime('now')),
    acknowledged_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_wire_update_cache_ack ON pyramid_wire_update_cache(acknowledged_at);
```

Helpers:
- `upsert_wire_update_cache(conn, local_id, latest_id, chain_length_delta, changes_summary, author_handles) -> Result<()>`
- `list_pending_wire_updates(conn, slug: Option<&str>) -> Result<Vec<WireUpdateEntry>>` — filters out rows where `acknowledged_at IS NOT NULL`
- `acknowledge_wire_update(conn, local_id) -> Result<()>`
- `delete_wire_update_cache(conn, local_id) -> Result<()>` — called when the user pulls the update

### 6. Backend: pull flow

Add to `wire_discovery.rs` or a new `wire_pull.rs`:

```rust
pub async fn pull_wire_contribution(
    state: &PyramidState,
    latest_wire_contribution_id: &str,
    local_contribution_id_to_supersede: Option<&str>,
    activate: bool,
) -> Result<PullOutcome>
```

Implementation:
1. Call `publisher.fetch_contribution(wire_id)` to get the full contribution payload.
2. **Credential safety gate**: scan the yaml_content for `${VAR_NAME}` patterns. Compare against the existing `.credentials` file's defined vars. If the pulled contribution references undefined vars, reject the pull with a clear error message listing the missing vars.
3. Build a new local contribution row: `source = "wire"`, `wire_contribution_id = latest_wire_contribution_id`, yaml_content from the payload, supersedes_contribution_id = local_contribution_id_to_supersede.
4. `insert_contribution(conn, new_contribution)`.
5. If `activate = true`:
   - Set `status = "active"` on the new contribution.
   - Set the prior active contribution (if any) to `status = "superseded"`.
   - Call `sync_config_to_operational` to propagate to runtime tables.
6. Delete the corresponding `pyramid_wire_update_cache` row if it exists.
7. Emit `ConfigSynced` event (already exists from Phase 4).

### 7. Backend: IPC commands (new)

Add all of the following to `main.rs` and register in `invoke_handler!`:

- `pyramid_wire_discover { schema_type, query, tags, limit, sort_by } -> Vec<DiscoveryResult>`
- `pyramid_wire_recommendations { slug, schema_type, limit } -> Vec<Recommendation>`
- `pyramid_wire_update_available { slug } -> Vec<WireUpdateEntry>`
- `pyramid_wire_auto_update_toggle { schema_type, enabled } -> { ok: bool }` — writes a new `wire_auto_update_settings` contribution
- `pyramid_wire_auto_update_status -> Vec<AutoUpdateSettingEntry>`
- `pyramid_wire_pull_latest { local_contribution_id, latest_wire_contribution_id } -> { new_local_contribution_id, activated }`
- `pyramid_wire_acknowledge_update { local_contribution_id } -> { ok }`
- `pyramid_search_wire_configs { schema_type, query, tags } -> Vec<DiscoveryResult>` — **alias of `pyramid_wire_discover` so Phase 10's ToolsMode Discover placeholder can be swapped to the real call without an IPC rename**
- `pyramid_pull_wire_config { wire_contribution_id, slug, activate } -> { new_local_contribution_id }` — Phase 10 stub name, shipping now

All new IPCs use the existing `state.auth` / `get_api_token` pattern for Wire auth.

**`pyramid_wire_auto_update_toggle` implementation**: writes a new `wire_auto_update_settings` contribution — yes, the toggle state is ITSELF a contribution per the spec. The YAML body is `{ schema_type_name: bool, ... }` per the spec line 182-189. `wire_auto_update_settings` is a new bundled schema_type.

### 8. Backend: bundled seed contributions

Add to `src-tauri/assets/bundled_contributions.json`:

1. `wire_discovery_weights` — the default seed weights from the spec line 73-83.
2. `wire_auto_update_settings` — all schema_types set to `false` by default.
3. `wire_update_polling` — `{ interval_secs: 21600 }` (6 hours).

Each bundled contribution follows the existing shape (contribution_id, schema_type, slug=null, yaml_content, source="bundled", ...).

Extend `wire_migration::migrate_bundled_contributions_to_db` to handle the new schema_types (check if it's needed — the existing dispatcher may already handle generic schema_types; verify).

Extend `config_contributions::sync_config_to_operational` to handle the new schema_types:
- `wire_discovery_weights` → no operational table; the in-memory cache reads it on demand. Just log "synced" and return Ok.
- `wire_auto_update_settings` → new operational table `pyramid_wire_auto_update_settings(schema_type, enabled, contribution_id, updated_at)` OR keep it in-contribution (read from contribution store at runtime). Recommend: in-contribution, no operational table. The UI reads settings via IPC that calls the loader.
- `wire_update_polling` → the `WireUpdatePoller` reads it at startup and on supersession.

Add these three schema_types to the allowed list in `config_contributions.rs` (the 14-branch dispatcher — may need to grow to 17).

### 9. Frontend: Discover tab rewrite

Replace the Phase 10 placeholder in `src/components/modes/ToolsMode.tsx` `DiscoverPanel`:

Components:
- **Search bar**: schema_type dropdown (populated from `pyramid_config_schemas`), free-text query, tag input, sort-by dropdown (`score` | `rating` | `adoption` | `fresh` | `chain_length`).
- **Results list**: renders `DiscoveryResult` entries with `QualityBadges` component (new, see below), description, rationale string, and a "Pull" button.
- **Recommendations banner** (when a slug is selected): shows up to 5 recommendations with rationale strings. Clicking opens the detail drawer.
- **Detail drawer**: reuses `ContributionDetailDrawer` pattern. Shows full metadata, supersession chain info, YamlConfigRenderer preview, "Pull" / "Pull and activate" buttons.

Calls:
- `invoke('pyramid_wire_discover', { schema_type, query, tags, limit: 20, sort_by })` on search submit.
- `invoke('pyramid_wire_recommendations', { slug, schema_type, limit: 5 })` on mount when a slug is selected.
- `invoke('pyramid_pull_wire_config', { wire_contribution_id, slug, activate })` on Pull.

### 10. Frontend: `QualityBadges` component

New component `src/components/QualityBadges.tsx`:

```tsx
interface QualityBadgesProps {
  rating?: number;              // 1-5
  adoptionCount: number;
  openRebuttals: number;
  chainLength: number;
  freshnessDays: number;
}
```

Renders a row of inline badges:
- Star icon + rating (e.g., "⭐ 4.7")
- People icon + adoption (e.g., "218 users")
- Alert icon + open_rebuttals (only if > 0)
- Refresh icon + chain_length
- Clock icon + "Updated Xd ago" / "Updated Xmo ago"

Match the existing frontend icon set (check what's used in ContributionDetailDrawer or other Phase 10 components). If there's no existing icon library, use plain text fallbacks or ASCII glyphs rather than adding a new dependency.

### 11. Frontend: My Tools update badges + update drawer

Extend `MyToolsPanel` in `ToolsMode.tsx`:
- On mount, call `invoke('pyramid_wire_update_available')` to get all pending updates.
- For each contribution card that matches a pending update, render an "Update available" badge + icon.
- Clicking the badge opens a new drawer (`WireUpdateDrawer.tsx`):
  - Shows current version summary + triggering_note.
  - Shows new version summary + triggering_note.
  - If chain_length_delta > 1, shows intermediate versions with triggering_notes.
  - Shows changes_summary (from the cached entry).
  - "Pull latest" button → `invoke('pyramid_wire_pull_latest', ...)` → refresh list.
  - "Dismiss" button → `invoke('pyramid_wire_acknowledge_update', ...)` → removes badge until next poll.

### 12. Frontend: Settings auto-update toggles

Find the Settings component (may be `src/components/Settings.tsx` or similar). Add a new section:
- **Auto-Update from Wire**
- Per-schema_type toggle list (loaded via `pyramid_wire_auto_update_status`)
- Warning banner: "Auto-update pulls new versions without prompting. Contributions that reference new credentials will always require manual review."
- Toggle click → `invoke('pyramid_wire_auto_update_toggle', { schema_type, enabled })`.

If no Settings component exists yet, document the auto-update UI as deferred to a follow-up Settings cleanup phase and surface the toggles as a new modal accessible from the Discover tab instead.

### 13. Rust tests

Add tests to:
- `wire_discovery.rs`:
  - `test_normalize_signals_handles_missing_signals`
  - `test_compute_score_with_redistributed_weights`
  - `test_explain_ranking_builds_rationale_from_signals`
  - `test_recommendations_filter_by_schema_type`
  - `test_recommendations_match_source_type_overlap`
  - `test_recommendations_match_tier_routing_similarity`
- `wire_update_poller.rs`:
  - `test_poller_detects_supersession` (mock the publisher)
  - `test_poller_auto_update_respects_toggle`
  - `test_poller_auto_update_refuses_new_credentials`
- `db.rs` phase14_tests:
  - `test_upsert_wire_update_cache_idempotent`
  - `test_list_pending_wire_updates_filters_acknowledged`
  - `test_delete_wire_update_cache`
- `config_contributions.rs`:
  - `test_sync_wire_discovery_weights_no_operational_table`
  - `test_sync_wire_auto_update_settings`
- `wire_publish.rs` (test extensions — if existing test harness exists):
  - `test_search_contributions_request_shape`
  - `test_check_supersessions_request_shape`

### 14. Frontend tests (only if a test runner exists)

- `QualityBadges` renders all badges correctly given sample props
- Discover tab calls the right IPC on search submit

If no test runner exists, skip frontend tests and document manual verification steps.

## Scope boundaries

**In scope:**
- Wire HTTP client extensions (search, fetch, check_supersessions)
- Ranking engine (signals, normalization, scoring, rationale)
- Recommendations engine (source_type + tier_routing similarity only)
- Supersession polling worker + update cache table
- Pull flow with credential safety gate
- 7+ new IPC commands + aliases for Phase 10's stubs
- Three new bundled contributions (`wire_discovery_weights`, `wire_auto_update_settings`, `wire_update_polling`)
- ToolsMode Discover tab rewrite
- ToolsMode My Tools update badges + update drawer
- QualityBadges shared component
- Auto-update toggles in Settings (or a follow-up if Settings doesn't exist)
- Rust tests + implementation log

**Out of scope:**
- Wire server-side discovery endpoint implementation (document as a dependency on the GoodNewsEveryone repo)
- Apex embedding similarity signal (v2)
- Cross-schema recommendations (v2)
- Circle-scoped weights contributions (v2)
- Anti-sybil / author-dampener (v2)
- DADBEAR Oversight page integration (Phase 15)
- Frontend tests if no runner
- CSS overhaul — match existing conventions
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` from `src-tauri/` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — Phase 13 count (1137) + new Phase 14 tests. Same 7 pre-existing failures.
3. **Frontend build:** `npm run build` — clean, no new TypeScript errors.
4. **IPC registration:** `grep -n "pyramid_wire_discover\|pyramid_wire_recommendations\|pyramid_wire_pull_latest\|pyramid_search_wire_configs\|pyramid_pull_wire_config" src-tauri/src/main.rs` — each should appear in both the function definition AND the `invoke_handler!` list.
5. **Bundled contributions present:** grep `src-tauri/assets/bundled_contributions.json` for `wire_discovery_weights` + `wire_auto_update_settings` + `wire_update_polling`.
6. **Manual verification path** documented in the log:
   - Launch dev, switch to ToolsMode Discover, pick a schema_type, search, verify results render with badges + rationale.
   - Pull a contribution, verify it lands in My Tools.
   - Verify the poller runs (check logs for "Wire update poll").
   - Toggle an auto-update setting in Settings, verify the backend row updates.

## Deviation protocol

Standard. Most likely deviations:

- **Wire server discovery endpoints don't exist yet**: if the Wire server hasn't shipped `/api/v1/contributions/search` and friends, the frontend+backend will compile and test, but real pulls will 404. Ship the client code correctly, add a feature flag `WIRE_DISCOVERY_ENABLED` gated on the existence of the server endpoints (or just document the server dependency in the implementation log). Integration tests should use a mock HTTP server.
- **`pyramid_metadata` source_type / tier_routing fields**: if the pyramid metadata table doesn't have clean `source_type` or provider listings, derive them from `pyramid_tier_routing` and from the pyramid's chain assignments. Document any inference logic used.
- **Bundled contribution manifest format**: if Phase 5/8/9's bundled contributions use a different field shape than what you need for `wire_discovery_weights`, match the existing shape and document.
- **Settings component missing**: if `Settings.tsx` doesn't exist in the frontend, add the auto-update UI as a new tab in ToolsMode or as a modal from the Discover tab header. Do NOT block the phase on creating a full Settings page.
- **Supersession polling vs app-close**: the poller runs in a background tokio task. On app close (Tauri shutdown), the task should be cleanly aborted. Match the existing pattern for other background workers (DADBEAR tick loop, stale engine).
- **Credential safety gate scanning**: grep for `${VAR_NAME}` patterns in YAML text. If YAML has comments, handle them reasonably. Document the regex used.

## Implementation log protocol

Append Phase 14 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Include:
1. Modules added
2. Wire HTTP client extensions with method signatures
3. Ranking engine implementation notes (missing-signal redistribution, normalization choices)
4. Recommendations signal weights (source_type + tier_routing)
5. Update poller design + cadence
6. Pull flow + credential safety gate
7. New IPC commands list
8. New bundled contributions shipped
9. Frontend components + their mount points
10. Tests added and passing
11. Manual verification steps
12. Deviations with rationale
13. Status: `awaiting-verification`

## Mandate

- **No backend API contract breaks.** Phase 4-13 IPC must keep working. Extend, don't replace.
- **Missing signals are neutral, not zero.** The ranking engine MUST redistribute weights for missing signals. New contributions must get a fair shot — this is spec-mandated and a quality-of-results issue.
- **Pull flow must respect credential safety.** A pulled contribution that introduces a new credential reference gets rejected with a clear message. The user manually reviews it via the normal contribution flow.
- **Auto-update default is false for every schema_type.** The user opts in per category. Document this as a deliberate UX choice.
- **Fix all bugs found during the sweep.** Standard repo convention.
- **Match existing frontend conventions.** Phase 10 set the ToolsMode pattern. Match it.
- **Commit when done.** Single commit with message `phase-14: wire discovery + ranking + recommendations + update polling`. Body: 8-12 lines summarizing the Wire client extensions, ranking engine, recommendations, supersession polling, pull flow, 7+ new IPCs, 3 new bundled contributions, ToolsMode Discover rewrite, update drawer, quality badges. Do not amend. Do not push.

## End state

Phase 14 is complete when:

1. `PyramidPublisher` has `search_contributions`, `fetch_contribution`, and `check_supersessions` methods.
2. `src-tauri/src/pyramid/wire_discovery.rs` implements the ranking engine (signals, normalization, scoring, rationale) with default seed weights.
3. Recommendations engine filters to `schema_type` + ranks by source_type overlap + tier_routing similarity.
4. `WireUpdatePoller` exists and runs in the background against `pyramid_wire_update_cache`.
5. Pull flow works with credential safety gate; writes new contributions with `source = "wire"`; supersedes the prior active version when requested.
6. All Phase 14 IPC commands registered in `invoke_handler!` AND the Phase 10 stub aliases (`pyramid_search_wire_configs`, `pyramid_pull_wire_config`) work.
7. `wire_discovery_weights`, `wire_auto_update_settings`, `wire_update_polling` exist as bundled contributions in `bundled_contributions.json`.
8. ToolsMode Discover tab renders real search results with quality badges + recommendations banner.
9. ToolsMode My Tools shows update badges + update drawer.
10. `QualityBadges.tsx` exists as a reusable component.
11. Auto-update toggle UI lives in Settings (or documented deferral with a modal fallback).
12. `cargo check --lib` + `cargo build --lib` + `npm run build` clean.
13. `cargo test --lib pyramid` at prior count + new Phase 14 tests. Same 7 pre-existing failures.
14. Implementation log Phase 14 entry complete with manual verification steps.
15. Single commit on branch `phase-14-wire-discovery-ranking`.

Begin with the spec. Then the existing Wire publisher. Then wire.

Good luck. Build carefully.
