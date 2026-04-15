# DADBEAR Canonical Architecture

> MPS revision — incorporates all findings from three external audit rounds.

## The Wrong Frame and the Right Frame

**Wrong frame (v1–v2 of this plan):** "How do we clean up the state model so the oversight page works?" This produced better flags and tables but optimized the wrong atom.

**Right frame:** DADBEAR is a **compiler** from source observations into durable compute work items. The LLM call is the execution atom, not the pyramid. The pyramid is the artifact. Contributions are the persistence atom. DADBEAR's job is to observe changes, compile them through a recipe into discrete work items, and submit those items for dispatch through the universal compute queue. Holds block dispatch, never observation. Local runtime truth is append-only events; mutable tables are projections.

## The Five Things DADBEAR Does

```
1. OBSERVE  — watcher detects file changes, writes observation events (ALWAYS, even when held)
2. COMPILE  — recipe + staleness check turns observations into discrete work items
3. PREVIEW  — work items get cost/routing/policy preview before commitment (Pillar 23)
4. DISPATCH — committed work items enter the universal compute queue
5. APPLY    — results supersede old contributions idempotently
```

Holds block step 4 (dispatch). They never block step 1 (observation) and never destroy accumulated work from step 2 (compilation). The watcher observes. The compiler compiles. The dispatcher waits.

## Append-Only Event Streams

Local runtime truth is append-only event streams plus compilation/dispatch state. Mutable tables are projections. Everything else is derived.

### `dadbear_observation_events`

```sql
CREATE TABLE dadbear_observation_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    source TEXT NOT NULL,             -- 'watcher' | 'cascade' | 'rescan' | 'evidence' | 'vine'
    source_path TEXT,                 -- filesystem path (NULL for internal events like cascade)
    event_type TEXT NOT NULL,         -- 'file_created' | 'file_modified' | 'file_deleted' | 'file_renamed'
                                      -- | 'cascade_stale' | 'evidence_growth' | 'vine_stale'
    file_path TEXT,                   -- filesystem path (NULL for internal events)
    content_hash TEXT,                -- SHA-256 of new content (NULL for deletes/internal)
    previous_hash TEXT,               -- SHA-256 of old content (NULL for creates/internal)
    target_node_id TEXT,              -- for cascade/internal events: the node being affected
    layer INTEGER,                    -- for cascade events: the target layer
    detected_at TEXT NOT NULL,
    metadata_json TEXT                -- rename candidate pair, cascade reason, etc.
);
CREATE INDEX idx_obs_slug ON dadbear_observation_events(slug, detected_at);
CREATE INDEX idx_obs_cursor ON dadbear_observation_events(slug, id);
```

**Event sources.** Observations come from multiple sources, not just the file watcher:
- **`watcher`** — file system changes (create/modify/delete/rename)
- **`cascade`** — internal events from stale check results triggering upper-layer re-evaluation (replaces the current `propagate_confirmed_stales` WAL writes)
- **`rescan`** — full hash rescan on unfreeze (replaces `routes.rs` unfreeze rescan WAL writes)
- **`evidence`** — evidence set growth from `chain_executor.rs`
- **`vine`** — vine composition stale events

The `source` column distinguishes external observations from internal cascade events. The compiler handles all sources uniformly.

**Source identity.** Provenance uses `file_path + content_hash`. Rename detection is handled by the watcher's existing rename tracking logic (2-second window, LLM evaluation) which writes `file_renamed` observation events. No separate document identity table is needed — `pyramid_file_hashes` already maps `(slug, file_path) → node_ids`.

**Replaces:** `pyramid_pending_mutations` WAL (which was mutable — rows marked `processed`), plus all non-watcher mutation writers: `stale_helpers.rs` cascade writes, `chain_executor.rs` evidence growth writes, `vine_composition.rs` stale writes, `routes.rs` unfreeze rescan writes, and `stale_engine.rs` cascade propagation. All of these currently INSERT into `pyramid_pending_mutations` — they all become observation event writes instead.

**Retention.** Observation events are append-only but not infinite. Events older than the compilation cursor for all epochs AND older than a configurable retention window (default 30 days, from `dadbear_norms`) can be archived or deleted. Once compiled and all dependent work items are in terminal states, the events are historical audit data. The supervisor runs a retention pass periodically.

**The watcher writes here unconditionally.** Paused, frozen, breaker-tripped — doesn't matter. If a file changed, the observation is recorded. On pause, the watcher stops OS-level polling but records any queued events before stopping. On resume, the watcher does a full hash diff scan against `pyramid_file_hashes` to catch changes that occurred during the pause, writing any detected changes as observation events with `source = 'rescan'`.

### `dadbear_hold_events`

```sql
CREATE TABLE dadbear_hold_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    hold TEXT NOT NULL,               -- 'frozen' | 'breaker' | 'cost_limit' | ...
    action TEXT NOT NULL,             -- 'placed' | 'cleared'
    reason TEXT,                      -- human/machine context
    created_at TEXT NOT NULL
);
CREATE INDEX idx_hold_slug ON dadbear_hold_events(slug, created_at);
```

**Projection:** `dadbear_active_holds` — the set of currently active holds per slug. Derived by scanning hold_events and computing the net state (last 'placed' without a subsequent 'cleared' for each hold type).

```sql
CREATE VIEW dadbear_active_holds AS
SELECT slug, hold, MAX(created_at) as held_since
FROM dadbear_hold_events
WHERE action = 'placed'
  AND NOT EXISTS (
    SELECT 1 FROM dadbear_hold_events e2
    WHERE e2.slug = dadbear_hold_events.slug
      AND e2.hold = dadbear_hold_events.hold
      AND e2.action = 'cleared'
      AND e2.created_at > dadbear_hold_events.created_at
  )
GROUP BY slug, hold;
```

In practice this view may be too expensive for the tick loop. A materialized projection table `dadbear_holds_projection` is maintained incrementally on each hold event:

```sql
CREATE TABLE dadbear_holds_projection (
    slug TEXT NOT NULL,
    hold TEXT NOT NULL,
    held_since TEXT NOT NULL,
    reason TEXT,
    PRIMARY KEY (slug, hold)
);
```

Insert on 'placed', delete on 'cleared'. The event stream is the truth; the projection is the fast-path.

**Reconciliation.** On supervisor startup (and once per hour thereafter), the supervisor recomputes the projection from the event stream and compares. If they differ (crash between event write and projection update, bug in a new hold type), the projection is overwritten and a warning is logged. Hold events are low-volume so this is cheap.

**Dual-write during transition (Phases 2–7).** During the migration period, `auto_update_ops::freeze/unfreeze/trip_breaker/resume_breaker` write to BOTH the new hold events + projection AND the old `pyramid_auto_update_config` columns. This ensures every existing consumer (server.rs startup, watcher.rs config loading, stale_engine.rs breaker polling, all IPC handlers, frontend) continues to work. The dual-write is removed in Phase 7 when the old table is dropped.

### `dadbear_work_items`

The core innovation. DADBEAR compiles observations into discrete, durable work items.

```sql
CREATE TABLE dadbear_work_items (
    id TEXT PRIMARY KEY,              -- semantic path: {slug}:{epoch_short}:{primitive}:{layer}:{target_id}
                                      -- e.g. "opt-025:ep3:stale_check:0:node-abc123"
                                      -- human/LLM-readable, serves as idempotency key through dispatch
    slug TEXT NOT NULL,
    batch_id TEXT NOT NULL,           -- semantic path: {slug}:{epoch_short}:batch-{cursor_position}
    epoch_id TEXT NOT NULL,           -- semantic path: {slug}:{recipe_id_short}:{norms_id_short}:{timestamp}

    -- What to do
    recipe_contribution_id TEXT,      -- the chain/recipe that produced this item
    step_name TEXT NOT NULL,          -- step within the recipe
    primitive TEXT NOT NULL,          -- 'stale_check' | 'extract' | 'cluster' | 'synthesize' | ...
    layer INTEGER NOT NULL,
    target_id TEXT,                   -- node/edge being operated on

    -- Materialized call context (durable — survives crash, reconstructs StepContext)
    system_prompt TEXT NOT NULL,
    user_prompt TEXT NOT NULL,
    model_tier TEXT NOT NULL,
    resolved_model_id TEXT,
    resolved_provider_id TEXT,
    temperature REAL,
    max_tokens INTEGER,
    response_format_json TEXT,

    -- StepContext reconstruction fields (Law 4: every LLM call gets a StepContext)
    build_id TEXT,                    -- build run this item belongs to (cache key component)
    chunk_index INTEGER,              -- chunk within the step (cache key component)
    prompt_hash TEXT,                 -- SHA-256 of prompt template (cache key component)
    force_fresh INTEGER DEFAULT 0,    -- bypass cache (reroll)

    -- Provenance
    observation_event_ids TEXT,       -- JSON array of observation_event IDs that caused this item
    compiled_at TEXT NOT NULL,

    -- Lifecycle (state transitions are compare-and-set: UPDATE ... WHERE state = expected)
    state TEXT NOT NULL DEFAULT 'compiled',
        -- 'compiled'   — ready for preview/dispatch (deps may not be met yet)
        -- 'previewed'  — cost/routing preview completed, commit approved
        -- 'dispatched' — submitted to compute queue (attempt in flight)
        -- 'completed'  — result received and durably stored
        -- 'applied'    — result applied to pyramid (contribution created/superseded)
        -- 'failed'     — execution failed (retryable)
        -- 'blocked'    — dispatch blocked by active hold
        -- 'stale'      — superseded by a newer compilation epoch
    state_changed_at TEXT NOT NULL,
    blocked_from TEXT,                -- prior state before blocking (NULL when not blocked)
                                      -- on hold clear: restore to blocked_from unless preview expired

    -- Preview reference (populated at preview step)
    preview_id TEXT,                  -- references dadbear_dispatch_previews

    -- Result (populated at completion)
    result_json TEXT,
    result_cost_usd REAL,
    result_tokens_in INTEGER,
    result_tokens_out INTEGER,
    result_latency_ms INTEGER,
    completed_at TEXT,

    -- Application (populated when result is applied to pyramid)
    applied_at TEXT,
    application_contribution_id TEXT  -- the new/superseded contribution created
);
CREATE INDEX idx_wi_slug_state ON dadbear_work_items(slug, state);
CREATE INDEX idx_wi_batch ON dadbear_work_items(batch_id);
CREATE INDEX idx_wi_epoch ON dadbear_work_items(slug, epoch_id);
```

**This IS the `ComputeRequest` the second audit called for.** It's the durable form of what `QueueEntry` carries in memory today. The work item ID is a **semantic path** — human and LLM readable — that flows through dispatch, execution, and application. Every consumer (steward, oversight page, chronicle, fleet peers) can reason about what the work IS from the ID alone. Duplicate results for the same path are discarded.

**StepContext reconstruction.** On crash recovery, a full `StepContext` is rebuilt from the work item's durable fields: `slug`, `build_id`, `step_name`, `primitive`, `layer` (→depth), `chunk_index`, `model_tier`, `resolved_model_id`, `resolved_provider_id`, `prompt_hash`, `force_fresh`. The `db_path` is derived from slug at runtime. The `bus` (event channel) is a runtime object obtained from the supervisor. This covers every field in the current `StepContext` struct — no information is lost across a crash boundary.

**State transitions are compare-and-set (CAS).** Every transition uses `UPDATE ... SET state = ?1 WHERE id = ?2 AND state = ?3` inside a `BEGIN IMMEDIATE` transaction. If the CAS fails, the transition was already made (by a prior attempt or concurrent process). This prevents double-dispatch, double-completion, and double-application. The supervisor's crash recovery phase completes fully before the normal tick loop begins — no interleaving.

**Compiler deduplication.** Before emitting a new work item, the compiler checks for existing non-terminal items targeting the same `(slug, target_id, step_name, layer)`. If a `compiled`, `blocked`, `previewed`, or `dispatched` item already exists for that target, the compiler skips emission. This prevents duplicate work when the compiler runs while items are blocked (the auditor-identified duplication scenario: item blocked → compiler runs again → duplicate emitted).

**Dispatched items during hold placement.** Items already in `dispatched` state when a hold is placed continue to completion. Holds affect only the dispatch gate, not in-flight work. When a dispatched item completes, its result is applied normally even if holds are active — the work was approved pre-hold. Only future dispatch is blocked.

**`QueueEntry` extension.** Phase 3 extends `QueueEntry` with `work_item_id: Option<String>` and `attempt_id: Option<String>` fields. All existing callers pass `None` (backward compatible). DADBEAR dispatch sets both. The GPU loop result callback uses these IDs to correlate completions back to durable work items.

### `dadbear_work_item_deps` — the work DAG

Higher-layer pyramid work depends on lower-layer results being applied. The compiler emits explicit dependency edges.

```sql
CREATE TABLE dadbear_work_item_deps (
    work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
    depends_on_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
    PRIMARY KEY (work_item_id, depends_on_id)
);
CREATE INDEX idx_deps_upstream ON dadbear_work_item_deps(depends_on_id);
```

A work item is **dispatchable** only when all its dependencies are in `applied` state. The dispatcher checks:

```sql
SELECT wi.* FROM dadbear_work_items wi
WHERE wi.state IN ('compiled', 'previewed')
  AND wi.slug = ?1
  AND NOT EXISTS (
    SELECT 1 FROM dadbear_work_item_deps d
    JOIN dadbear_work_items dep ON d.depends_on_id = dep.id
    WHERE d.work_item_id = wi.id AND dep.state != 'applied'
  )
  AND NOT EXISTS (
    SELECT 1 FROM dadbear_holds_projection h WHERE h.slug = wi.slug
  )
```

The DAG is built incrementally. The compiler emits L0 stale-check items with no deps. When L0 results are applied and trigger L1 mutations, the compiler emits L1 items that depend on the applied L0 items. This matches the existing DADBEAR recursive pattern: each layer's supersession creates a mutation that triggers the next layer's compilation.

### `dadbear_dispatch_previews` — batch-level commit contracts

Preview is not per-item metadata — it's a batch-level contract with a policy snapshot, TTL, and enforcement plan.

```sql
CREATE TABLE dadbear_dispatch_previews (
    id TEXT PRIMARY KEY,               -- semantic path: {slug}:{batch_id}:{policy_hash_short}
    slug TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    policy_hash TEXT NOT NULL,         -- SHA-256 of dispatch_policy contribution used
    norms_hash TEXT NOT NULL,          -- SHA-256 of dadbear_norms contribution used
    item_count INTEGER NOT NULL,
    total_cost_usd REAL NOT NULL,
    total_wall_time_secs REAL,
    enforcement_cost_usd REAL,         -- cost of review/challenge at configured level
    enforcement_level TEXT,            -- 'none' | 'sample' | 'full' — from steward config
    routing_summary_json TEXT,         -- per-model/provider breakdown
    expires_at TEXT NOT NULL,          -- TTL: preview invalid after this (policy may have changed)
    committed_at TEXT,                 -- NULL until operator/auto-commit approves
    created_at TEXT NOT NULL
);
CREATE INDEX idx_preview_batch ON dadbear_dispatch_previews(batch_id);
```

Dispatch only proceeds when: a preview exists, hasn't expired, has been committed, and its `policy_hash` still matches the active dispatch policy. If the policy changes between preview and dispatch, the preview is invalidated and must be regenerated. For auto-maintenance within budget, preview + commit happens atomically.

### `dadbear_compilation_state` — versioned compilation cursor

The compiler tracks where it left off per slug, versioned by the recipe and norms that were active during compilation. When the recipe or norms change, a new epoch begins and compiled items from the old epoch are marked `stale`.

```sql
CREATE TABLE dadbear_compilation_state (
    slug TEXT PRIMARY KEY,
    epoch_id TEXT NOT NULL,            -- semantic path: {slug}:{recipe_id_short}:{norms_id_short}:{timestamp}
    recipe_contribution_id TEXT,       -- recipe active during this epoch
    norms_contribution_id TEXT,        -- norms active during this epoch
    last_compiled_observation_id INTEGER, -- cursor: highest observation event ID compiled
    epoch_start_observation_id INTEGER,  -- observation ID when this epoch began (reset target)
    epoch_started_at TEXT NOT NULL
);
```

When the recipe or norms contribution changes:
1. New `epoch_id` is generated
2. `last_compiled_observation_id` resets to the start of the old epoch (or 0 for full recompilation — configurable)
3. All `compiled`/`blocked` work items from the old epoch → `stale`
4. The compiler recompiles observations under the new recipe

This is Pillar 28 in action: the recipe is a contribution, safely evolvable, and the compiler knows how to react to recipe changes.

### `dadbear_work_attempts` — per-attempt execution log

Each dispatch of a work item is an attempt with its own identity. Failed attempts don't destroy the work item.

```sql
CREATE TABLE dadbear_work_attempts (
    id TEXT PRIMARY KEY,               -- semantic path: {work_item_id}:a{attempt_number}
    work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
    attempt_number INTEGER NOT NULL,
    dispatched_at TEXT NOT NULL,
    model_id TEXT NOT NULL,
    routing TEXT NOT NULL,             -- 'local' | 'cloud' | 'fleet' | 'market'
    result_json TEXT,
    cost_usd REAL,
    tokens_in INTEGER,
    tokens_out INTEGER,
    latency_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
        -- 'pending'   — dispatched, awaiting result
        -- 'completed' — result received
        -- 'failed'    — execution error
        -- 'timeout'   — no response within SLA
    review_status TEXT NOT NULL DEFAULT 'none',
        -- 'none'            — no review requested (local/fleet with no enforcement)
        -- 'pending_review'  — dispatched for review (market protocol)
        -- 'reviewed_pass'   — review passed
        -- 'reviewed_flag'   — flagged by reviewer
        -- 'challenged'      — challenge panel invoked
    cost_log_id TEXT,             -- FK to pyramid_cost_log for detailed reconciliation data
    completed_at TEXT,
    error TEXT
);
CREATE INDEX idx_attempts_wi ON dadbear_work_attempts(work_item_id);
```

The `attempt.id` is propagated to the compute queue entry and through to the result callback. On crash recovery, pending attempts with no completion are either re-dispatched (creating a new attempt) or timed out, depending on elapsed time.

**Review/challenge boundary:** DADBEAR does not own the review pipeline. The compute substrate (queue → fleet → market) handles execution-to-verification. DADBEAR receives verified results (or failure with review context). The `review_status` field gives DADBEAR observability into the verification pipeline without coupling to its internals. For local/fleet dispatch with no market protocol, `review_status` stays `none`.

### `dadbear_result_applications`

When a completed work item's result is applied to the pyramid (creating/superseding contributions). Application is idempotent: `INSERT ... ON CONFLICT DO NOTHING` keyed by `(work_item_id, target_id)`.

```sql
CREATE TABLE dadbear_result_applications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
    slug TEXT NOT NULL,
    target_id TEXT NOT NULL,           -- node/edge that was created/superseded
    action TEXT NOT NULL,              -- 'created' | 'superseded' | 'tombstoned'
    old_contribution_id TEXT,
    new_contribution_id TEXT,
    applied_at TEXT NOT NULL,
    UNIQUE(work_item_id, target_id)    -- idempotent: same item can't apply to same target twice
);
```

### `pyramid_build_metadata` — per-slug build-derived facts

```sql
CREATE TABLE pyramid_build_metadata (
    slug TEXT PRIMARY KEY,
    ingested_extensions TEXT DEFAULT '[]',   -- JSON array of file extensions discovered during build
    ingested_config_files TEXT DEFAULT '[]', -- JSON array of config filenames (e.g., "Cargo.toml")
    updated_at TEXT
);
```

Absorbs `ingested_extensions` and `ingested_config_files` from `pyramid_auto_update_config`. These are build-derived facts consumed by the watcher for path filtering (`is_trackable_path`). Updated on build completion. Not operator config — not a contribution.

## Prompt Freshness

**Prompts are materialized at dispatch time, not compile time.** The compiler stores placeholder prompts (what the work item represents, which target, which primitive). The supervisor materializes real prompts from `stale_helpers.rs` / `stale_helpers_upper.rs` before dispatch, using the current pyramid state. This means prompts always reflect the latest data. The `prompt_hash` is computed at materialization time. For crash recovery: `dispatched` items already have materialized prompts (they were materialized before enqueue). The supervisor re-dispatches them without re-materialization.

## Cost and Audit Log Relationship

Work items and attempts carry summary cost fields for the supervisor's hot path (`result_cost_usd`, `result_tokens_in/out`). The existing `pyramid_cost_log` remains the source of truth for the cost observatory (CostRollupSection, reconciliation, broadcast confirmation). Work attempts carry a `cost_log_id TEXT` FK that joins to `pyramid_cost_log` when detailed reconciliation data is needed (model, provider, broadcast status, etc.).

`pyramid_stale_check_log` is subsumed by `dadbear_work_attempts` (stale check results are work item completions). `pyramid_change_manifests` is subsumed by `dadbear_result_applications` (applied changes are result applications). Both old tables are dropped in Phase 7.

## Migration Inventory — Non-Watcher WAL Writers

The following code paths currently write directly to `pyramid_pending_mutations` and must be migrated to write `dadbear_observation_events` instead:

| Code path | File | Current write | New event type |
|-----------|------|--------------|----------------|
| Stale check cascade | `stale_helpers.rs:113` | `confirmed_stale` at L1+ | `cascade_stale` with `source='cascade'` |
| Cross-thread edge propagation | `stale_helpers_upper.rs:1425` | `confirmed_stale` from edge re-eval | `cascade_stale` with `source='cascade'` |
| In-place node update cascade | `stale_helpers_upper.rs:2827` | `confirmed_stale` from in-place updates | `cascade_stale` with `source='cascade'` |
| In-place edge stale | `stale_helpers_upper.rs:2853` | `edge_stale` from in-place updates | `edge_stale` with `source='cascade'` |
| Child-to-parent cascade | `stale_helpers_upper.rs:3312` | `confirmed_stale` from supersession | `cascade_stale` with `source='cascade'` |
| Child-to-parent edge stale | `stale_helpers_upper.rs:3337` | `edge_stale` from supersession | `edge_stale` with `source='cascade'` |
| Evidence set growth | `chain_executor.rs:6158` | `evidence_set_growth` at L0 | `evidence_growth` with `source='evidence'` |
| Vine bedrock stale | `build_runner.rs:421` | `confirmed_stale` for vines | `vine_stale` with `source='vine'` |
| Vine node stale | `vine_composition.rs:377` | `confirmed_stale` for vines | `vine_stale` with `source='vine'` |
| Stale engine cascade | `stale_engine.rs:1069` | `targeted_l0_stale` during drain | `targeted_stale` with `source='cascade'` |
| Stale engine propagation | `stale_engine.rs:1679` | cascade from `propagate_confirmed_stales` | `cascade_stale` with `source='cascade'` |
| Unfreeze rescan | `routes.rs:4933-4948` | `file_change`/`deleted_file` | `file_modified`/`file_deleted` with `source='rescan'` |
| Forced L0 sweep | `routes.rs:5007` | `enqueue_full_l0_sweep` | `full_sweep` with `source='rescan'` |
| Staleness bridge | `staleness_bridge.rs:129` | reads `pyramid_pending_mutations` | Rewrite to read `dadbear_observation_events` with cursor stored as `last_bridge_observation_id` on `pyramid_build_metadata` (interim cursor until Phase 3's `dadbear_compilation_state` subsumes it) |
| **Result application** | `dadbear_result_applications` (NEW) | N/A — new path | When a work item result is applied (node created/superseded), write a `cascade_stale` observation for affected parent nodes. This is the **feedback loop** that drives incremental DAG construction. |

**Verification:** 15 non-test INSERT sites across 9 files, plus 1 new write path from result applications. All listed.

**Implementation note:** All 15 existing sites should dual-write via a shared `write_observation_event()` helper function that takes the same conceptual parameters as the current INSERT but writes to the new table. This reduces per-site migration work and ensures consistent event format. Phase 1 may need sub-phases given the 15-site surface area.

## The Master Gate

Before (4 booleans, 2 tables):
```sql
WHERE d.enabled = 1 AND a.auto_update = 1 AND a.frozen = 0 AND a.breaker_tripped = 0
```

After (1 anti-join on projection):
```sql
SELECT d.* FROM pyramid_dadbear_config d
WHERE NOT EXISTS (
    SELECT 1 FROM dadbear_holds_projection h WHERE h.slug = d.slug
)
```

The new gate derives the active slug set from `watch_root` contributions materialized into `pyramid_dadbear_config`, not from legacy flags. Contribution existence IS the enable gate — if a slug has no `watch_root` contribution (no cache row), it's not in the result set. Holds block dispatch but the compiler always runs. Only dispatch is gated.

## Contribution Split: Identity / Norms / Dispatch

The overloaded `dadbear_policy` splits into three contribution types:

### `watch_root` — local source binding (identity)

```yaml
schema_type: watch_root
slug: my-project
source_path: /Users/adam/projects/my-project
content_type: code
```

"This slug watches this path." Pure identity. No behavior. Multiple roots per slug (a pyramid can watch multiple directories).

### `dadbear_norms` — scan behavior (norms)

```yaml
schema_type: dadbear_norms
slug: my-project              # or null for global defaults
scan_interval_secs: 10
debounce_secs: 30
session_timeout_secs: 1800
batch_size: 5
min_changed_files: 1
runaway_threshold: 0.5
retention_window_days: 30
```

"How should DADBEAR scan this slug?" Supports global defaults (slug=NULL) + per-slug overrides via layered resolver.

### `dispatch_policy` — budget and routing (already exists)

The existing `dispatch_policy` contribution type already handles model routing, tier mapping, and provider selection. DADBEAR work items are dispatched through it. Cost limits and quality enforcement compose here.

## The DADBEAR Compiler

The compiler is the new core of DADBEAR. It replaces the current `drain_and_dispatch` pattern with a four-stage pipeline.

### Stage 1: Compile observations into work items (DAG-aware, epoch-versioned)

```
Inputs:
  - Observation events since last_compiled_observation_id for this epoch
  - Recipe snapshot (chain YAML, pinned by recipe_contribution_id)
  - Current pyramid state (existing nodes, edges, layers)
  - Applied results from prior compilation passes (for cross-layer deps)

Output:
  - dadbear_work_items rows in 'compiled' state
  - dadbear_work_item_deps edges for cross-layer dependencies
  - Compilation cursor advanced
```

The compiler runs on a timer (scan_interval from `dadbear_norms`). It reads new observations, applies the recipe's staleness-check logic, and emits discrete work items. **Prompts are materialized at dispatch time, not compile time.** The compiler stores prompt template references + input references (target node ID, layer, primitive) as placeholder text. The supervisor materializes real prompts at dispatch time by calling the same prompt construction logic from `stale_helpers.rs` / `stale_helpers_upper.rs` with current pyramid state. This ensures prompts use the freshest data and aligns with the plan's data freshness check (re-hash inputs at dispatch, mark stale if they differ).

**Observation → primitive mapping.** The compiler maps observation event types to work item primitives, unifying the ingest and stale paths. This matches the 6 dispatch categories in the current `drain_and_dispatch` (stale_engine.rs:930-965):
- `file_created` → `extract` (new file → create L0 nodes)
- `file_modified` → `stale_check` (changed file → check if L0 nodes are stale)
- `file_deleted` → `tombstone` (deleted file → tombstone affected L0 nodes)
- `file_renamed` → `rename_candidate` (potential rename → LLM evaluates merge vs. separate)
- `cascade_stale` → `stale_check` at the target layer (upper-layer staleness from lower-layer supersession)
- `edge_stale` → `edge_check` (edge re-evaluation from in-place updates or supersession cascade)
- `targeted_stale` → `stale_check` at L0 (targeted re-check of specific nodes)
- `evidence_growth` → `stale_check` at L0 (new evidence may make existing understanding stale)
- `vine_stale` → `stale_check` for vine nodes
- `connection_check` → `connection_check` (web edge re-evaluation)
- `node_stale` → `node_stale_check` (direct node staleness check, distinct from cascade)
- `faq_category_stale` → `faq_redistill` (FAQ category re-distillation via LLM)
- `full_sweep` → `extract` for all source files (forced full L0 rebuild)

**Batch primitives.** Most primitives produce one work item per target (one LLM call). `faq_redistill` is an exception: the current `faq_category_stale` dispatch (`stale_engine.rs:1326-1397`) runs a multi-step meta-pass across multiple FAQ nodes. The compiler should decompose this into individual work items — one per affected FAQ ID, each running a single LLM call for re-distillation. If decomposition is not feasible for a given primitive, the work item may issue multiple LLM calls internally, but cost/attempt tracking should still be per-item (aggregate cost on the work item, individual call costs on `pyramid_cost_log` via `cost_log_id`).

**Debounce disambiguation.** The codebase has two distinct "debounce" concepts: (a) `debounce_secs` on `DadbearWatchConfig` — the ingest/session promotion debounce (how long to wait after file changes before promoting an ingest session), and (b) `debounce_minutes` on `AutoUpdateConfig` — the stale engine's per-layer compilation debounce (how long to wait after mutations accumulate before running the drain). In the new model, `dadbear_norms.debounce_secs` replaces (a) — the compiler's scan-interval timer. The compiler's own compilation pass debounce (how long to wait after new observations before compiling) is a separate concern — it is the `scan_interval_secs` field. If finer control is needed, add `compilation_debounce_secs` to `dadbear_norms`.

**`response_format_json` reconstruction.** This field is NOT on `StepContext` — it lives on `QueueEntry` directly. During crash recovery, `response_format_json` is read from the durable work item and injected directly onto the reconstructed `QueueEntry`, bypassing StepContext. The plan's "StepContext reconstruction" covers cache/event plumbing fields; `response_format_json` is a call parameter, not a context field.

**`target_id` serialization.** For primitives with composite targets, `target_id` uses `/` as the internal separator (NOT `:` which is the path-level delimiter): edge operations → `"edge/L2-003/L2-007"`, rename candidates → `"rename/old_path/new_path"`, connection checks → `"conn/42"`. This ensures the top-level work item ID `{slug}:{epoch_short}:{primitive}:{layer}:{target_id}` can always be parsed with `splitn(5, ':')` — the first four fields are fixed, the fifth is the entire target_id including any internal `/` separators. The deduplication check `(slug, target_id, step_name, layer)` relies on these formats being stable.

**Parsing contract.** All work item path consumers MUST use `splitn(5, ':')` to extract fields. Field 5 is the complete target_id. Attempt IDs append `:a{N}` — extract by splitting from the right on the last `:a\d+` segment. Target_id components must never produce a final segment matching `/^a\d+$/` (no current node ID format does — they use `L0-003`, `C-L1-002`, `thread-abc` patterns).

**Epoch ID `_short` definition.** `recipe_id_short` and `norms_id_short` are the first 8 hex characters of the contribution UUID (hyphens removed). 32 bits of entropy, ~1% collision at 9,300 epochs per slug. The `{timestamp}` suffix is the uniqueness guarantee — short IDs are for human readability only, not uniqueness. Format: `opt-025:a1b2c3d4:e5f6g7h8:20260415T0130`.

**Backfill translation table.** Phase 1 migrates existing `pyramid_pending_mutations` rows to `dadbear_observation_events`. The current WAL uses mutation_type values (`file_change`, `new_file`, `deleted_file`, `rename_candidate`, `confirmed_stale`, `edge_stale`, `node_stale`, `faq_category_stale`, `evidence_set_growth`, `targeted_l0_stale`, `connection_check`). The backfill maps: `file_change` → `file_modified`, `new_file` → `file_created`, `deleted_file` → `file_deleted`, `rename_candidate` → `file_renamed`, `confirmed_stale` → `cascade_stale`, and the rest map 1:1 to their new names.

**DAG construction is incremental.** The compiler emits L0 stale-check items with no dependencies. When L0 results are applied (superseding/creating L0 nodes), the application itself is a new observation. The compiler's next pass sees it, determines whether L1 needs updating, and emits L1 items with dependency edges pointing to the applied L0 items. This continues up the pyramid. The DAG is not fully emitted upfront — it grows as lower layers complete.

**Epoch versioning.** The compiler pins its output to the current epoch (recipe + norms snapshot). Each work item carries `epoch_id`. When the recipe or norms contribution changes:
1. New epoch created in `dadbear_compilation_state`
2. All `compiled`/`blocked` items from the old epoch → `stale`
3. Cursor resets — the compiler recompiles relevant observations under the new recipe
4. `dispatched`/`completed` items from the old epoch continue to completion (their prompts are already materialized — changing the recipe mid-flight doesn't invalidate in-flight work, only future compilations)

The compiler runs even when holds are active. It emits items in `compiled` state regardless. The dispatcher decides whether to promote them.

### Stage 2: Preview — batch-level commit contracts (Pillar 23)

For each batch of `compiled` work items whose dependencies are met:

```
Inputs:
  - Batch of dispatchable work items
  - Dispatch policy snapshot (hashed for staleness detection)
  - Enforcement level from steward config
  - Current queue depths

Output:
  - dadbear_dispatch_previews row with:
    - Total cost (inference + enforcement)
    - Wall time estimate
    - Per-model routing breakdown
    - Policy hash + TTL
  - Work items updated: preview_id set, state → 'previewed'
```

The preview is a **commit contract**: it captures the exact policy, cost, and routing that will be used. If the dispatch policy changes before commit, the preview's `policy_hash` no longer matches and the preview is invalidated. The batch must be re-previewed.

For background maintenance (DADBEAR auto-updates), preview + commit happens atomically within configured budget limits. When preview exceeds budget, a `cost_limit` hold is placed automatically instead of silently dropping work. The oversight page shows preview totals per batch.

### Stage 3: Commit

The preview is approved — either automatically (within budget) or by operator confirmation. The preview's `committed_at` is stamped. This is the economic commitment point (Pillar 23: informed, never surprising).

### Stage 4: Dispatch (idempotent, CAS-guarded)

For each committed, previewed work item whose dependencies are met and whose slug has no active holds:

```
Inputs:
  - Work item with materialized prompts
  - Committed preview (not expired, policy hash matches)
  - No active holds for this slug
  - All dependency items in 'applied' state

Output:
  - Work attempt row created (attempt.id as execution-level idempotency key)
  - QueueEntry submitted to compute queue (carries work_item.id + attempt.id)
  - Work item state → 'dispatched' (via CAS: only if currently 'previewed')
```

**Idempotent dispatch.** The CAS transition `UPDATE SET state='dispatched' WHERE id=? AND state='previewed'` ensures exactly-once dispatch. If the CAS fails, another process already dispatched it. The attempt ID flows through the compute queue to the result callback, preventing duplicate completion.

**Crash recovery.** On startup, the supervisor scans for `dispatched` items with no `completed` attempt. For each:
- If elapsed time < SLA timeout: wait (the in-memory queue may have been processing it when the crash occurred; the provider may still return a result via webhook)
- If elapsed time > SLA timeout: create a new attempt and re-dispatch

**Holds check at dispatch time.** If any hold exists for the slug, the item stays in `previewed` (or is moved to `blocked` for observability). When holds clear, blocked items re-enter the dispatch pipeline.

## The Runtime Supervisor

Replaces ad-hoc event-driven syncing with a single reconciliation loop.

```rust
struct DadbearSupervisor {
    /// Runs continuously. Each tick:
    /// 1. Load active watch_root contributions → desired set of watchers
    /// 2. Reconcile watchers: start missing, stop orphaned
    /// 3. Load dadbear_norms (via resolver) → desired engine configs
    /// 4. Reconcile engines: create missing, hot-reload changed, stop orphaned
    /// 5. For each active engine:
    ///    a. Run compiler (observations → work items)
    ///    b. Check holds projection
    ///    c. If no holds: preview + dispatch pending work items
    ///    d. If holds: mark pending items as 'blocked'
    /// 6. For each completed work item: apply results to pyramid
    /// 7. Emit events for UI (DadbearStateChanged, WorkItemProgress, etc.)
}
```

The supervisor is **idempotent**. It can crash and restart. It reads event streams and projections, computes desired state, and reconciles. No state is lost because observations and work items are durable.

## Non-Destructive Holds

Holds block dispatch (step 4 of the five things). They NEVER:
- Discard watcher events (observations always recorded)
- Mark WAL/observation rows as processed
- Cancel compiled work items (items move to 'blocked', not deleted)
- Destroy in-memory state that can't be reconstructed

When a hold is placed:
1. Hold event written to `dadbear_hold_events`
2. Projection updated (INSERT into `dadbear_holds_projection`)
3. In-memory engine notified (stops dispatching, keeps compiling)
4. Pending `compiled`/`previewed` work items → `blocked` with `blocked_from` preserving prior state
5. Watcher continues recording observations

When ALL holds for a slug are cleared:
1. Hold event written (action='cleared')
2. Projection updated (DELETE from `dadbear_holds_projection`)
3. In-memory engine notified (resumes dispatching)
4. For each `blocked` item:
   - If `blocked_from = 'previewed'` AND preview still valid (not expired, policy hash matches): restore to `previewed`
   - If `blocked_from = 'previewed'` BUT preview expired or policy changed: restore to `compiled` (must re-preview)
   - If `blocked_from = 'compiled'`: restore to `compiled`
   - Clear `blocked_from` to NULL
5. Supervisor's next tick picks up accumulated observations and compiles new work items

## Pre-Existing Bugs to Fix During Implementation

These bugs exist in the current system and must be fixed as part of the migration:

1. **IPC `pyramid_dadbear_resume` skips hash rescan.** The UI "Resume" button (main.rs:7433) unfreezes without running the hash rescan that detects changes during freeze. The HTTP route (`routes.rs:4846`) does this correctly. Fix: add hash rescan to the IPC path. In the new system, this is moot — observations are always recorded, so no rescan is needed on unfreeze.

2. **`stale_engine::start_poll_loop` exits permanently on breaker trip.** The poll loop (stale_engine.rs:441) `break`s out when breaker is detected, and `resume_breaker()` does not restart it. Fix: poll loop should `continue` and re-check the flag, or `resume_breaker()` should restart the loop. In the new system, the supervisor's reconciliation loop replaces the poll loop entirely.

3. **Triple freeze path with double event emission.** The IPC `pyramid_dadbear_pause` (main.rs:7401) calls `auto_update_ops::freeze()` then `engine.freeze()` which calls `auto_update_ops::freeze()` again. Double DB write and double event emission. Fix: IPC should call engine.freeze() only, which handles both. In the new system, freeze writes a single hold event.

4. **`DadbearActivityDrawer` reads from tables being dropped.** The activity log IPC assembles from `pyramid_stale_check_log`, `pyramid_pending_mutations`, and `pyramid_change_manifests`. All three are dropped in Phase 7. Fix: rewrite to read from `dadbear_work_items` + `dadbear_work_attempts` + `dadbear_observation_events` in Phase 6.

## What Gets Deleted

| Artifact | Reason |
|----------|--------|
| `pyramid_auto_update_config` table | Replaced by holds projection + contribution resolver |
| `pyramid_dadbear_config.enabled` column | Holds are the universal pause; contribution existence is the enable gate |
| `pyramid_pending_mutations` table | Replaced by `dadbear_observation_events` (append-only, not mutable WAL) |
| `enable_dadbear_for_slug()` / `disable_dadbear_for_slug()` | Replaced by hold placement/clearing |
| `auto_update` flag | Contribution existence is the gate |
| `drain_and_dispatch()` pattern | Replaced by compiler → preview → dispatch pipeline |
| `sync_config_to_operational()` for `auto_update_policy` | Contribution resolver reads directly; event emission for hot-reload |
| Ghost-engine backfill code | Supervisor reconciles desired state from contributions |
| Watcher event discarding on pause | Observations are always recorded |
| `freeze()` marking WAL rows processed | Non-destructive: holds block dispatch, don't erase work |

## What Gets Created

| Artifact | Role |
|----------|------|
| `dadbear_observation_events` | Append-only observation stream with source document identity |
| `dadbear_hold_events` | Append-only hold stream |
| `dadbear_holds_projection` | Materialized active holds (fast-path for master gate) |
| `dadbear_work_items` | Durable work items with epoch versioning and idempotency keys |
| `dadbear_work_item_deps` | Explicit dependency DAG between work items (cross-layer) |
| `dadbear_dispatch_previews` | Batch-level commit contracts with policy hash and TTL |
| `dadbear_compilation_state` | Per-slug epoch-versioned compilation cursor |
| `dadbear_work_attempts` | Per-dispatch attempt log with review status observability |
| `dadbear_result_applications` | Idempotent result application log |
| `pyramid_build_metadata` | Per-slug build-derived facts (extensions, config files) |
| `DadbearSupervisor` | Runtime reconciliation loop |
| `DadbearCompiler` | DAG-aware, epoch-versioned observation → work item compilation |
| `watch_root` contribution type | Local source binding (identity) |
| `dadbear_norms` contribution type | Scan behavior (norms) |
| Contribution resolver | Layered merge of global + per-slug for all config types |

## What Stays (Rewritten)

| Artifact | New Role |
|----------|----------|
| `pyramid_dadbear_config` (stripped) | Materialized cache of `watch_root` + `dadbear_norms` contributions |
| `auto_update_ops.rs` | Hold event writing + projection maintenance |
| `stale_engine.rs` | Becomes the per-slug compiler engine within the supervisor |
| `watcher.rs` | Observation recording (never discards, never pauses recording) |
| `dadbear_extend.rs` tick loop | Becomes the supervisor's main loop |
| Event bus | Carries `DadbearHoldsChanged`, `WorkItemStateChanged`, `DadbearConfigChanged` |

## Implementation Phases

### Phase 0: Contribution resolver + contribution split

- **Create `DadbearConfigChanged` event type** in `TaggedKind` enum (`event_bus.rs`), add serialization, implement subscriber side. Currently `trigger_dadbear_reload()` in `config_contributions.rs` is a no-op stub — this replaces it.
- Implement layered resolver for global + per-slug merge
- Register `watch_root` and `dadbear_norms` in the schema registry, including `display_name_for` and `description_for` entries for the UI schema picker
- Add new dispatcher branches in `config_contributions.rs` for both types
- **Data migration — `dadbear_policy`:** For each active `dadbear_policy` contribution: parse YAML, create `watch_root` contribution (slug + source_path + content_type), create `dadbear_norms` contribution (slug + scan_interval_secs + debounce_secs + session_timeout_secs + batch_size). For multi-root slugs (N source paths), create N `watch_root` contributions but only ONE `dadbear_norms` using the minimum scan_interval and maximum debounce across the conflicting rows. Supersede the original `dadbear_policy` contributions.
- **Data migration — `auto_update_policy`:** For each active `auto_update_policy` contribution: parse YAML, merge `min_changed_files` and `runaway_threshold` into the slug's `dadbear_norms` contribution (these were previously on a separate contribution type). `debounce_minutes` from `auto_update_policy` maps to `debounce_secs * 60` in `dadbear_norms`. `cost_reconciliation` from `DadbearPolicyYaml` moves to `dispatch_policy` (it's economic policy, not scan norms). Supersede the original `auto_update_policy` contributions.
- Keep old `dadbear_policy` and `auto_update_policy` dispatchers for rollback safety (deprecated but functional)
- Wire resolver into existing stale engine for hot-reload via `DadbearConfigChanged` event
- Global defaults work immediately (resolver merges slug=NULL + per-slug)
- Update `sync_config_to_operational` for `dadbear_policy`: when a global contribution syncs, rebuild cache rows for all slugs

### Phase 1: Append-only observation events

- Create `dadbear_observation_events` table and `pyramid_build_metadata` table
- Rewrite watcher to write observations unconditionally (no pause discarding) — dual-write to BOTH old WAL and new event stream
- Rewrite all non-watcher WAL writers (see Migration Inventory) to also write to new event stream
- Rewrite `staleness_bridge.rs` to read from new event stream
- **Do NOT change freeze behavior on old WAL yet.** During Phases 1–2, the old `drain_and_dispatch` still consumes `pyramid_pending_mutations` with its existing processed-on-freeze semantics. The new event stream is written in parallel but only consumed starting in Phase 3 when the compiler exists.
- Create migration: existing `pyramid_pending_mutations` rows → observation events (backfill for history)
- Compilation cursor tracks which observations have been compiled (initially set to max existing observation ID)
- Migrate `ingested_extensions`/`ingested_config_files` from `pyramid_auto_update_config` to `pyramid_build_metadata`

### Phase 2: Hold events + projection

- Create `dadbear_hold_events` and `dadbear_holds_projection` tables
- Rewrite `auto_update_ops` to write hold events + maintain projection
- Rewrite master gate to use holds anti-join
- Migration: current frozen/breaker state → hold events + projection rows
- `pyramid_auto_update_config` kept alive during transition — current server.rs startup, watcher.rs config loading, and stale_engine.rs breaker polling still read it. Reads are additive (holds projection is the new authority; old table is consulted only by consumers not yet rewritten). Drop deferred to Phase 7 after the supervisor replaces all legacy consumers.

### Phase 3: Durable work items + work DAG

- Create `dadbear_work_items`, `dadbear_work_item_deps`, `dadbear_compilation_state`, `dadbear_work_attempts`, `dadbear_result_applications` tables
- Implement the compiler: observation events → work items with materialized prompts and dependency edges
- Compilation state tracks epoch (recipe + norms snapshot) and cursor
- Work items enter compute queue via existing `QueueEntry` (work_item.id + attempt.id for correlation)
- CAS state transitions on work items (prevent double-dispatch, double-completion, double-application)
- On completion: result written back to work item via CAS, application is idempotent (UNIQUE constraint)
- Crash recovery: on startup, scan dispatched items with no completed attempt; re-dispatch with new attempt after SLA timeout

### Phase 4: Preview + commit contracts

- Create `dadbear_dispatch_previews` table
- **Extend `DispatchPolicyYaml` with budget fields:** `max_batch_cost_usd: Option<f64>` (auto-commit ceiling — batches under this cost dispatch without operator approval), `max_daily_cost_usd: Option<f64>` (daily cost cap per slug — `cost_limit` hold placed when exceeded). These are contribution-configurable (operators and agents can tune them). Without these fields, preview is observability-only with no gate — DADBEAR would auto-commit everything, defeating the purpose of preview-then-commit.
- Batch-level preview: total cost, wall time, enforcement cost, routing breakdown, policy hash, TTL
- Auto-commit within `max_batch_cost_usd` for background maintenance; batches exceeding it require operator confirmation
- Manual commit path for operator-reviewed batches
- Expired or policy-mismatched previews are regenerated before dispatch
- `cost_limit` hold placed automatically when preview exceeds `max_daily_cost_usd` or when a batch exceeds `max_batch_cost_usd` and no operator is present to confirm
- Oversight page shows preview totals per slug and per batch

### Phase 5: Runtime supervisor

- Implement `DadbearSupervisor` as the single reconciliation loop
- **Replaces** `dadbear_extend.rs` tick loop, ad-hoc stale engine management, AND the `server.rs` `init_stale_engines` startup path. Server.rs spawns the supervisor instead of manually iterating `pyramid_auto_update_config` and constructing stale engines.
- **Startup ordering:** The supervisor MUST be spawned AFTER the GPU processing loop (main.rs:11405). The GPU loop must be running before any producer enqueues work. The supervisor's crash recovery phase should not dispatch until GPU loop is confirmed started. This matches the current startup order (GPU loop → stale engines → DADBEAR extend).
- **Prompt materialization at dispatch time.** The compiler stores placeholder prompts (template references + input references). Before dispatching a work item, the supervisor materializes real prompts by calling the existing prompt construction logic from `stale_helpers.rs` (L0) and `stale_helpers_upper.rs` (L1+) with the current pyramid state. This reads the target node's content, existing understanding, file diffs, and formats them into system_prompt + user_prompt. The materialized prompts are written back to the work item row. The `prompt_hash` is computed at this point for the data freshness check. This is the bridge between the old dispatch flow (stale_helpers build prompts inline) and the new flow (supervisor materializes, then enqueues).
- **Result flow:** The supervisor is the code that enqueues work items and awaits their oneshot results. For concurrent dispatch, the supervisor spawns per-item result handlers via `JoinSet` — each handler awaits its oneshot, writes the result to `dadbear_work_items` via CAS (`dispatched` → `completed`), writes to `dadbear_work_attempts`, writes the `cost_log_id` FK, and emits a `WorkItemStateChanged` event. The supervisor's main loop polls the JoinSet for completions and applies results to the pyramid.
- Reconciles: desired watchers (from `watch_root` contributions) → actual watchers
- Reconciles: desired engines (from `dadbear_norms` via resolver) → actual engines
- Reconciles: compiled work items + deps + holds → dispatch decisions
- DAG-aware: only dispatches items whose dependencies are in `applied` state
- Epoch-aware: marks items from stale epochs, triggers recompilation on recipe/norms change
- Crash recovery phase runs to completion BEFORE normal tick loop begins (no interleaving)
- Holds projection reconciliation on startup (recompute from event stream, compare, overwrite if diverged)
- Applies completed results to pyramid (supersede/create contributions) AND writes cascade observation events for affected parent nodes (the feedback loop). Cascade observations MUST include `{"triggering_work_item_id": "<the applied work item's semantic path>"}` in `metadata_json` so the compiler creates precise cross-layer dependency edges. Also emits `connection_check` observations for edges of superseded nodes (this event type is born from the feedback loop, not from WAL). Connection check observations MUST also include the triggering work item ID.
- **Result application acquires `LockManager::write(slug)` before any pyramid mutations** (superseding/creating contributions). JoinSet handlers only write to DADBEAR-owned tables (`dadbear_work_items`, `dadbear_work_attempts`); actual pyramid mutations are serialized through the lock manager to prevent races with concurrent builds, delta processing, or other write paths.
- Runs retention pass for observation events (archive events older than cursor + retention window, where retention window comes from `dadbear_norms.retention_window_days`, default 30)
- **Decommissions old `drain_and_dispatch`** — the compiler is now the sole consumer of observations. Old WAL freeze semantics no longer needed.

### Phase 6: Frontend — work-item-centric oversight

**New IPC contracts** (must be defined before frontend work begins):

```typescript
// Replaces DadbearOverviewRow
interface WorkItemOverviewRow {
    slug: string;
    display_name: string;
    holds: { hold: string; held_since: string; reason: string | null }[];
    derived_status: 'active' | 'paused' | 'breaker' | 'held';
    epoch_id: string;
    recipe_version: string | null;
    // Pipeline counts
    pending_observations: number;
    compiled_items: number;
    blocked_items: number;
    previewed_items: number;
    dispatched_items: number;
    completed_items_24h: number;
    applied_items_24h: number;
    failed_items_24h: number;
    stale_items: number;
    // Cost
    preview_total_cost_usd: number;
    actual_cost_24h_usd: number;
    // Timing
    last_compilation_at: string | null;
    last_dispatch_at: string | null;
}

// Replaces DadbearOverviewTotals
interface WorkItemOverviewTotals {
    active_count: number;
    paused_count: number;
    breaker_count: number;
    total_compiled: number;
    total_dispatched: number;
    total_blocked: number;
    total_cost_24h_usd: number;
}
```

- Oversight page pivots from "is this pyramid paused?" to work pipeline view
- Per-pyramid cards show pipeline counts (compiled/blocked/dispatched/completed/applied)
- Hold management: place/clear individual holds with reasons and timestamps
- Activity drawer rewritten to read from observation_events + work_items + work_attempts (replacing 3-table union from old tables)
- **DADBEARPanel.tsx** also rewritten — it reads from `pyramid_auto_update_config` and `pyramid_stale_check_log` (both dropped in Phase 7). Either subsumed by the Oversight Page or rewritten to use new IPC contracts.

### Phase 7: Drop legacy tables + schema cleanup

- Drop `pyramid_auto_update_config` (deferred from Phase 2 — all consumers now use holds projection + contribution resolver via supervisor)
- Drop `pyramid_pending_mutations` (all observation data in new event stream; old WAL dual-write removed)
- Drop `pyramid_stale_check_log` (subsumed by `dadbear_work_attempts`)
- Drop `pyramid_change_manifests` (subsumed by `dadbear_result_applications`). **Note:** `reroll.rs` reads/writes this table for version tracking and audit — rewrite to use `dadbear_result_applications` before dropping.
- Drop `pyramid_connection_check_log` (subsumed by `dadbear_work_attempts` with `connection_check` primitive)
- Drop `pyramid_dadbear_config.enabled` column
- Migrate contribution schema: remove `enabled` from `dadbear_policy` (now split into `watch_root` + `dadbear_norms`)
- Remove legacy enable/disable routes and functions
- Remove `drain_and_dispatch` code path (replaced by compiler in Phase 3)
- Remove deprecated `dadbear_policy` dispatcher branch (kept for rollback in Phase 0)
- Remove server.rs startup `init_stale_engines` path (replaced by supervisor in Phase 5)
- Remove watcher.rs config loading from `pyramid_auto_update_config`
- Remove stale_engine.rs breaker polling from `pyramid_auto_update_config`
- Clean up `init_pyramid_db()` migration code

## Composability Tests

**"Can I add a new hold type?"**
INSERT a row into `dadbear_hold_events`. The projection updates. The master gate blocks dispatch. The oversight page shows the new hold. No schema changes, no code changes to the gate or UI.

**"Can I add a new work item type?"**
New `primitive` value in the compiler. The work item table, deps, attempts, and applications work unchanged. The oversight page shows the new type automatically.

**"What happens when the process crashes mid-dispatch?"**
Work items in `dispatched` state have pending attempts with no completion. On restart, the supervisor checks elapsed time against SLA. Timed-out attempts are marked `timeout`; the work item gets a new attempt and is re-dispatched. Prompts and routing are fully materialized — no re-compilation needed. The work item ID is the idempotency key; if the original call actually completed (provider-side) but the result wasn't recorded, the duplicate result is discarded via CAS on the work item state.

**"What happens when the recipe changes mid-flight?"**
In-flight (`dispatched`/`completed`) items from the old epoch continue to completion — their prompts are already materialized and the work is valid under the recipe that compiled it. `compiled`/`blocked` items from the old epoch → `stale`. The compiler starts a new epoch, resets the cursor, and recompiles relevant observations under the new recipe. No work is lost; no in-flight work is interrupted.

**"How do cross-layer dependencies work?"**
The compiler emits L0 stale-check items with no deps. When L0 items complete and are applied, the application event is an observation. The compiler's next pass sees it, checks L1 staleness, and emits L1 items with dependency edges to the applied L0 items. The dispatcher only picks up L1 items after their L0 deps are in `applied` state. The DAG builds incrementally — matching the existing DADBEAR recursive pattern.

**"What happens when an operator freezes and the watcher trips breaker?"**
Two hold events written. Two projection rows. Display shows "Breaker" (priority). Clearing breaker leaves frozen hold. Clearing frozen leaves breaker. Slug only dispatches when ALL holds are cleared. Throughout, the watcher records observations and the compiler compiles work items — they just stay in `compiled`/`blocked` state.

**"What observations exist while frozen?"**
All of them. The watcher never stops recording. The compiler still compiles (items are blocked, not discarded). When all holds clear, the supervisor finds compiled items whose deps are met, previews them, and dispatches. Nothing is lost.

**"What does the oversight page show?"**
Not "is this pyramid paused?" but the full work pipeline: observations pending compilation, work items by state (compiled/blocked/previewed/dispatched/completed/applied/stale), active holds with reasons and timestamps, preview totals with cost and routing, and the per-slug compilation epoch with recipe version.

**"Can an agent improve DADBEAR's behavior?"**
The recipe is a contribution. The norms are a contribution. The dispatch policy is a contribution. An agent proposes a new `dadbear_norms` contribution (better scan interval), or a new chain recipe (better staleness check), or a new dispatch policy (smarter routing). The operator reviews and accepts. The supervisor hot-reloads. A new compilation epoch starts. No code changes.

**"Who owns review/challenge?"**
The compute substrate. DADBEAR produces work items and receives results. For market dispatch, the compute market protocol handles blind review, challenge panels, and re-dispatch. DADBEAR sees `review_status` on attempts for observability but never drives the review pipeline. For local/fleet dispatch, review is steward-configured via the dispatch policy's enforcement level.
