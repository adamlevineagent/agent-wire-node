# Workstream: Phase 4 — Config Contribution Foundation

## Who you are

You are an implementer joining an active 17-phase initiative to turn Wire Node into a Wire-native application. Phases 0a, 0b, 1, 2, and 3 are shipped. You are the implementer of Phase 4, which introduces the unified `pyramid_config_contributions` table as the source of truth for every configurable behavior in Wire Node. This is a foundation phase — Phases 5, 9, 10 build on it directly.

Phase 4 is substantial but bounded. It's mostly plumbing: new schema, CRUD helpers, a dispatch function, a one-time migration, and IPC endpoints. The architectural lens ("everything is a contribution") is the whole point of the phase.

## Context

Today, configs live in scattered operational tables (`pyramid_dadbear_config`, etc.) with no version history, no notes provenance, and no path to the Wire. Phase 4 introduces one unified contribution table that becomes the source of truth for all config types. Existing operational tables remain as runtime caches — the executor still reads from them on the hot path; Phase 4's sync function populates them from contributions.

The architectural principle: **every behavioral configuration is a contribution**. Not a separate table. A contribution with a `contribution_id`, a supersession chain, a `triggering_note`, and a path to Wire sharing.

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — original handoff, deviation protocol.
2. **`docs/specs/config-contribution-and-wire-sharing.md` — read in full, end-to-end.** This is your primary implementation contract. Particular attention to: the `pyramid_config_contributions` schema (lines ~40-70), the 14-entry schema_type vocabulary (lines ~75-90), the `sync_config_to_operational()` dispatch (lines ~240-343), the operational table schemas at the bottom (lines ~769-833), the notes capture lifecycle (lines ~750-766), and the migration plan (lines ~413-421).
3. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 4 section. Understand what Phase 4 unblocks (Phases 5, 9, 10 all depend on it).
4. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 0b, 1, 2, 3 entries for patterns.

### Code reading

5. `src-tauri/src/pyramid/db.rs` — **targeted.** You'll add several tables and CRUD helpers. Grep for `init_pyramid_db` (you'll append table creation), find the existing `pyramid_dadbear_config` schema + CRUD (you'll add a `contribution_id` column via migration), find the existing patterns used by Phase 3's provider registry helpers (`get_provider`, `save_provider`, etc.) — match that style.
6. `src-tauri/src/pyramid/mod.rs` — read `PyramidState` (around line 720+). You may add a new field for a "config contributions cache" or similar, but prefer reading from DB on demand unless the hot path demands caching.
7. `src-tauri/src/pyramid/types.rs` — scan for existing type conventions. You'll add `ConfigContribution`, `ContributionStatus`, `ContributionSource`, `ConfigSyncError` types.
8. `src-tauri/src/main.rs` — find the existing IPC command block (`invoke_handler!`). You'll register new commands at the end.
9. `src-tauri/src/pyramid/config_helper.rs` — existing file. Note the `#[deprecated]` wrapper for `config_for_model`. You will NOT remove it. Phase 4 is orthogonal.
10. **Wire contribution mapping stub:** Phase 5 (`wire-contribution-mapping.md`) defines the full `WireNativeMetadata` struct. Phase 4 creates the column `wire_native_metadata_json` and stores JSON into it, but does NOT validate the JSON against the canonical schema yet — that's Phase 5's job. For Phase 4, initialize the column with `"{}"` on every new contribution and defer canonical validation to Phase 5.

## What to build

### 1. Schema: `pyramid_config_contributions` table

Add to `init_pyramid_db` per the spec:

```sql
CREATE TABLE IF NOT EXISTS pyramid_config_contributions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contribution_id TEXT NOT NULL UNIQUE,
    slug TEXT,
    schema_type TEXT NOT NULL,
    yaml_content TEXT NOT NULL,
    wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
    wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
    supersedes_id TEXT,
    superseded_by_id TEXT,
    triggering_note TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    source TEXT NOT NULL DEFAULT 'local',
    wire_contribution_id TEXT,
    created_by TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    accepted_at TEXT,
    FOREIGN KEY (supersedes_id) REFERENCES pyramid_config_contributions(contribution_id)
);

CREATE INDEX IF NOT EXISTS idx_config_contrib_slug_type
    ON pyramid_config_contributions(slug, schema_type);
CREATE INDEX IF NOT EXISTS idx_config_contrib_active
    ON pyramid_config_contributions(slug, schema_type, status)
    WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_config_contrib_supersedes
    ON pyramid_config_contributions(supersedes_id);
CREATE INDEX IF NOT EXISTS idx_config_contrib_wire
    ON pyramid_config_contributions(wire_contribution_id);
```

### 2. New operational tables (defined in the spec's "Operational Table Schemas" section)

Add to `init_pyramid_db`:
- `pyramid_evidence_policy`
- `pyramid_build_strategy`
- `pyramid_custom_prompts`
- `pyramid_folder_ingestion_heuristics`

Each table has a `contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id)` FK. Use `ON DELETE CASCADE` only if the spec calls for it (it doesn't — leave defaults).

### 3. Migration: add `contribution_id` column to `pyramid_dadbear_config`

Existing table. Add:
```sql
ALTER TABLE pyramid_dadbear_config ADD COLUMN contribution_id TEXT REFERENCES pyramid_config_contributions(id);
```

Use a try-catch pattern (or schema version check) so re-running on existing DBs doesn't error. Look at how other recent column additions handle this.

### 4. Contribution CRUD (new file `src-tauri/src/pyramid/config_contributions.rs`)

```rust
pub fn create_config_contribution(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
    yaml_content: &str,
    triggering_note: Option<&str>,
    source: &str,
    created_by: Option<&str>,
) -> Result<String>  // returns contribution_id

pub fn supersede_config_contribution(
    conn: &Connection,
    prior_contribution_id: &str,
    new_yaml_content: &str,
    triggering_note: &str,  // required — not Option
    source: &str,
    created_by: Option<&str>,
) -> Result<String>  // returns new contribution_id. Atomically:
                     // 1. mark prior as status='superseded', superseded_by_id=new
                     // 2. insert new with status='active', supersedes_id=prior
                     // All in one transaction.

pub fn load_active_config_contribution(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Option<ConfigContribution>>

pub fn load_config_version_history(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Vec<ConfigContribution>>  // ordered oldest-to-newest, walks the supersedes chain

pub fn load_contribution_by_id(
    conn: &Connection,
    contribution_id: &str,
) -> Result<Option<ConfigContribution>>

pub fn list_pending_proposals(
    conn: &Connection,
    slug: Option<&str>,
) -> Result<Vec<ConfigContribution>>  // status='proposed'

pub fn accept_proposal(
    conn: &Connection,
    contribution_id: &str,
) -> Result<()>  // transitions status 'proposed' -> 'active', supersedes prior active if any

pub fn reject_proposal(
    conn: &Connection,
    contribution_id: &str,
    reason: Option<&str>,
) -> Result<()>  // transitions status 'proposed' -> 'rejected'
```

Contribution IDs are UUIDs (v4). Use `uuid::Uuid::new_v4().to_string()`.

### 5. Types (add to `types.rs` or define inline in `config_contributions.rs`)

```rust
pub struct ConfigContribution {
    pub id: i64,
    pub contribution_id: String,
    pub slug: Option<String>,
    pub schema_type: String,
    pub yaml_content: String,
    pub wire_native_metadata_json: String,
    pub wire_publication_state_json: String,
    pub supersedes_id: Option<String>,
    pub superseded_by_id: Option<String>,
    pub triggering_note: Option<String>,
    pub status: String,  // "active", "proposed", "rejected", "superseded"
    pub source: String,  // "local", "wire", "agent", "bundled", "migration"
    pub wire_contribution_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub accepted_at: Option<String>,
}

pub enum ConfigSyncError {
    ValidationFailed(String),
    UnknownSchemaType(String),
    SerdeError(serde_yaml::Error),
    DbError(rusqlite::Error),
    Other(anyhow::Error),
}
```

### 6. `sync_config_to_operational()` dispatcher

Per the spec's 14-branch match statement. **This is the biggest piece of Phase 4.** Implementation guidance:

- Place the function in `config_contributions.rs`.
- The function takes `&Connection`, `&Arc<BuildEventBus>`, and `&ConfigContribution`.
- Validation step (JSON Schema against `schema_definition` contribution) is **stubbed for Phase 4**: Phase 9 provides the schema definitions. For Phase 4, your validation helper just returns `Ok(())` and a TODO comment referencing Phase 9. Do NOT silently pass invalid YAMLs — just note the stub.
- Dispatch by `schema_type`. Full 14 branches per the spec.
- Many branches call helpers that don't exist yet (`invalidate_prompt_cache`, `register_chain_with_registry`, `flag_configs_for_migration`, etc.). For Phase 4, these are **stub functions that log a TODO and return `Ok(())`**. Define them in `config_contributions.rs` (or a sibling module) with clear "Phase X wires this up" comments. Phase 6 wires up `invalidate_prompt_cache`, Phase 9 wires up `flag_configs_for_migration`, etc.
- For the branches that DO have real operational tables today (`dadbear_policy`, `tier_routing`, `step_overrides`, `evidence_policy`, `build_strategy`, `custom_prompts`, `folder_ingestion_heuristics`), implement the real upsert. The `upsert_*` helpers go in `db.rs` alongside the table definitions.
- `tier_routing` and `step_overrides` already exist (Phase 3). The sync dispatcher should call the existing Phase 3 upsert helpers (you may need to add them if they don't match the needed signature — but do not rewrite the existing data model).
- Emit `TaggedKind::ConfigSynced` after a successful sync. Add this variant to `event_bus.rs` (or wherever `TaggedKind` is defined). The payload is `{ slug: Option<String>, schema_type: String, contribution_id: String, prior_contribution_id: Option<String> }`.

The `ConfigSynced` event is consumed by Phase 13 (build viz expansion). Phase 4 just emits it; no consumer exists yet.

### 7. Upsert helpers for new operational tables

In `db.rs`:

- `upsert_evidence_policy(conn, slug, yaml_struct, contribution_id)` — parses the EvidencePolicy struct (you'll define it minimally matching the spec's fields), writes to `pyramid_evidence_policy`
- `upsert_build_strategy(conn, slug, yaml_struct, contribution_id)`
- `upsert_custom_prompts(conn, slug, yaml_struct, contribution_id)`
- `upsert_folder_ingestion_heuristics(conn, slug, yaml_struct, contribution_id)`

For each, define the Rust struct for the YAML contents minimally — enough to deserialize a valid YAML. Full struct definitions live in future phases (evidence triage, etc.). For Phase 4 you only need the fields that get written into the operational table columns.

### 8. `upsert_dadbear_policy`: write to the existing `pyramid_dadbear_config` table

Extend the existing DADBEAR CRUD pattern to accept a `contribution_id` parameter. The spec says "The contribution sync writes into [the existing table's] existing columns (`scan_interval_secs`, `debounce_secs`, etc.) rather than creating a new table."

Map the `dadbear_policy` YAML fields to the existing DADBEAR config row columns. This is an in-place extension of `save_dadbear_config` (or a new sibling `save_dadbear_config_from_contribution`).

### 9. Bootstrap migration

In `init_pyramid_db` (or an adjacent `migrate_legacy_configs_to_contributions` function called after `init_pyramid_db`):

1. After the contribution table exists, check: has migration been run? (Use a seed marker row with `schema_type = '_migration_marker'` or similar, OR check if any row has `source = 'migration'`.)
2. If not run: read every row from `pyramid_dadbear_config`. For each row, serialize its fields to a `dadbear_policy` YAML document, insert a `pyramid_config_contributions` row with `source = 'migration'`, `status = 'active'`, `triggering_note = 'Migrated from legacy pyramid_dadbear_config'`, `wire_native_metadata_json = '{}'`. Update the original DADBEAR row's new `contribution_id` column to reference the new contribution.
3. Mark migration complete.

**Idempotency matters.** Running `init_pyramid_db` twice must not create duplicate migration rows.

### 10. IPC endpoints (main.rs)

Register these in `invoke_handler!`:

```
pyramid_create_config_contribution
pyramid_supersede_config (requires non-empty note per Notes Capture Lifecycle)
pyramid_active_config_contribution (read)
pyramid_config_version_history (read)
pyramid_propose_config (agent proposal, requires non-empty note)
pyramid_pending_proposals (read)
pyramid_accept_proposal
pyramid_reject_proposal
pyramid_rollback_config (creates a new version with the rolled-back content; requires note)
```

Notes enforcement: `pyramid_supersede_config` and `pyramid_propose_config` reject requests where the note is empty or whitespace-only, with a clear error. This is a compile-time-style invariant from the spec.

Do NOT implement `pyramid_publish_to_wire`, `pyramid_pull_wire_config`, `pyramid_search_wire_configs`, `pyramid_generate_config`, `pyramid_refine_config`, or `pyramid_reroll_config` — those are Phase 5 / Phase 9 / Phase 13 scope.

### 11. Tests

Add to `config_contributions.rs`:

- `test_create_and_load_active_contribution` — create, load, verify fields
- `test_supersede_creates_chain` — create v1, supersede to v2, supersede to v3, load history, verify chain order + statuses
- `test_supersede_requires_note` — verify `supersede_config_contribution` with empty note errors
- `test_load_version_history_ordering` — verify oldest-to-newest
- `test_propose_and_accept` — create proposal, accept, verify it becomes active and supersedes prior
- `test_propose_and_reject` — create proposal, reject, verify it stays in history with status='rejected'
- `test_sync_dadbear_policy_end_to_end` — create a dadbear_policy contribution, call `sync_config_to_operational`, verify `pyramid_dadbear_config` row was written with the correct `contribution_id`
- `test_sync_evidence_policy_end_to_end` — same for evidence policy
- `test_bootstrap_migration_idempotent` — create a DB with legacy DADBEAR rows, run migration, verify contributions created; run again, verify no duplicates
- `test_unknown_schema_type_fails_loudly` — call `sync_config_to_operational` with `schema_type: "not_a_real_thing"`, verify it returns `ConfigSyncError::UnknownSchemaType`

All existing tests must continue to pass. The 7 pre-existing unrelated failures are expected and acceptable.

## Scope boundaries

**In scope:**
- `pyramid_config_contributions` table + indices
- 4 new operational tables (`pyramid_evidence_policy`, `pyramid_build_strategy`, `pyramid_custom_prompts`, `pyramid_folder_ingestion_heuristics`)
- `contribution_id` FK column added to existing `pyramid_dadbear_config`
- Contribution CRUD in a new `config_contributions.rs` module
- Types for `ConfigContribution`, `ConfigSyncError`, minimal YAML structs per schema_type
- `sync_config_to_operational()` with all 14 match branches (real upserts for 6 types, stubs for the rest with clear TODO comments pointing at future phases)
- `TaggedKind::ConfigSynced` event variant
- Bootstrap migration of existing `pyramid_dadbear_config` rows to contributions (idempotent)
- IPC endpoints for contribution CRUD + agent proposals
- Tests

**Out of scope:**
- JSON Schema validation (Phase 9 provides schemas; stub the validation step)
- `wire_native_metadata_json` canonical validation (Phase 5)
- Wire publication / Wire pull IPC (Phase 5 / Phase 10)
- Generative config LLM flow (Phase 9)
- Custom chain bundle serialization / disk sync (Phase 9)
- ToolsMode.tsx frontend changes (Phase 10)
- Migration of `pyramid_chain_publications` (Phase 5)
- `pyramid_reroll_config` IPC (Phase 13)
- Real implementations of `invalidate_prompt_cache`, `register_chain_with_registry`, `flag_configs_for_migration`, etc. — stubs only
- Schema definitions for any schema_type YAML (Phase 9)
- The existing 7 pre-existing unrelated test failures

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` from `src-tauri/` — clean, zero new warnings in files you touched.
2. `cargo test --lib pyramid::config_contributions` — all new tests passing (10+ tests).
3. `cargo test --lib pyramid` — existing 842 + your new Phase 4 tests all pass. Same 7 pre-existing failures. No new failures.
4. `grep -n "pyramid_config_contributions" src-tauri/src/pyramid/db.rs` — shows the table creation in `init_pyramid_db` + the CRUD functions.
5. `grep -n "sync_config_to_operational" src-tauri/src/pyramid/config_contributions.rs` — shows the 14-branch dispatcher.

## Deviation protocol

Standard protocol. Most likely deviations:

- **Existing DADBEAR schema doesn't cleanly map to YAML.** If the columns have types that don't serialize cleanly (e.g., binary blobs), flag it. Default: use string escape, note in friction log.
- **Operational table naming collisions.** If one of the new tables happens to conflict with an already-existing table name, flag and use a different name.
- **YAML parsing errors on migration.** If a legacy DADBEAR row has data that won't serialize to YAML, mark the contribution with `source = 'migration_failed'` and note the row in the friction log rather than aborting the whole migration.

## Implementation log protocol

Append Phase 4 entry in `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the schema additions, CRUD helpers, dispatcher shape, migration logic, IPC endpoints, and verification results. Status: `awaiting-verification`.

## Mandate

- **Correct before fast.** The contribution table is foundational — Phases 5, 9, 10 all depend on it. Get the schema and CRUD right.
- **No new scope.** The 14-branch dispatcher has many stubs; that's fine and correct for Phase 4. Future phases replace the stubs.
- **Scope boundary for existing code.** Do NOT modify Phase 3's provider registry CRUD except to call into it from the sync dispatcher.
- **Pillar 37 awareness.** No new hardcoded LLM-constraining numbers. Phase 4 doesn't make LLM calls, so this should be a non-issue.
- **Fix all bugs found.** Standard repo convention.
- **Commit when done.** Single commit with message `phase-4: config contribution foundation`. Body: 5-7 lines summarizing schema + CRUD + dispatcher + migration + IPC. Do not amend. Do not push.

## End state

Phase 4 is complete when:

1. `pyramid_config_contributions` table + 4 new operational tables + `contribution_id` FK on `pyramid_dadbear_config` all exist in `init_pyramid_db`.
2. `config_contributions.rs` exists with the full CRUD + dispatcher.
3. Bootstrap migration converts legacy DADBEAR rows to contributions (idempotent).
4. 9 IPC endpoints are registered and wired up.
5. `TaggedKind::ConfigSynced` event variant exists.
6. All tests pass, no regressions.
7. Implementation log Phase 4 entry is complete.
8. Single commit on branch `phase-4-config-contributions`.

Begin with reading. The spec is your implementation contract.

Good luck. Build carefully.
