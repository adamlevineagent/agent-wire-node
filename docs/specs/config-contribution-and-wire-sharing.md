# Config Contribution & Wire Sharing Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Generative config pattern (for schema registry + generation flow), provider registry (for Wire auth)
**Unblocks:** Config marketplace on Wire, agent-proposed config changes, full notes paradigm for configs, DADBEAR-as-contribution
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Every behavioral configuration in Wire Node is a contribution. Not a separate table. Not a settings row. A contribution with a contribution_id, a supersession chain, a triggering note, and Wire shareability.

Today, configs live in separate tables (`pyramid_dadbear_config`, `pyramid_auto_update_config`, etc.) with no version history, no provenance, and no path to the Wire. This spec introduces a single `pyramid_config_contributions` table that becomes the source of truth for all config types. The existing operational tables remain as runtime caches — fast lookup for the executor, populated by a sync mechanism when contributions are accepted.

---

## Current State

| Config Type | Current Storage | Problems |
|---|---|---|
| DADBEAR policy | `pyramid_dadbear_config` (per-row fields) | No version history, no notes, no Wire path |
| Auto-update config | `pyramid_auto_update_config` | Same |
| Tier routing | Hardcoded defaults + chain YAML `model_tier` fields | No user-editable surface, no sharing |
| Step overrides | Chain YAML inline | Coupled to chain definition, not independently versionable |
| Custom prompts | Prompt files on disk | No contribution wrapper, no Wire sharing |
| Evidence policy | Not yet implemented (spec exists in generative-config-pattern) | Needs contribution storage from day one |
| Build strategy | Not yet implemented | Same |
| Folder ingestion heuristics | `pyramid_dadbear_config.content_type` + hardcoded scanner | No policy YAML, no sharing |

The generative config pattern spec defines the generation flow (intent -> YAML -> UI -> accept) and lists config types. This spec defines the storage and sharing layer underneath that flow.

---

## Unified Contribution Table

### pyramid_config_contributions

```sql
CREATE TABLE IF NOT EXISTS pyramid_config_contributions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contribution_id TEXT NOT NULL UNIQUE,    -- UUID, the durable identity
    slug TEXT,                               -- NULL for global configs (tier routing, providers)
    schema_type TEXT NOT NULL,               -- discriminator: "evidence_policy", "dadbear_policy", etc.
    yaml_content TEXT NOT NULL,              -- the full config YAML (validated against schema)
    wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',  -- Canonical WireNativeMetadata (see wire-contribution-mapping.md)
    wire_publication_state_json TEXT NOT NULL DEFAULT '{}', -- WirePublicationState (resolved UUIDs, handle-paths, chain refs — kept separate from canonical metadata so it stays portable)
    supersedes_id TEXT,                      -- local contribution_id of the prior version (NULL for v1)
    superseded_by_id TEXT,                   -- local contribution_id of the next version (NULL if active)
    triggering_note TEXT,                    -- the user/agent note that motivated this version
    status TEXT NOT NULL DEFAULT 'active',   -- "active", "proposed", "rejected", "superseded"
    source TEXT NOT NULL DEFAULT 'local',    -- "local", "wire", "agent", "bundled", "migration"
    wire_contribution_id TEXT,               -- Wire UUID (denormalized from wire_publication_state_json for indexing)
    created_by TEXT,                         -- "user", agent name, or Wire handle
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    accepted_at TEXT,                        -- when a proposed config was accepted
    FOREIGN KEY (supersedes_id) REFERENCES pyramid_config_contributions(contribution_id)
);

CREATE INDEX idx_config_contrib_slug_type
    ON pyramid_config_contributions(slug, schema_type);
CREATE INDEX idx_config_contrib_active
    ON pyramid_config_contributions(slug, schema_type, status)
    WHERE status = 'active';
CREATE INDEX idx_config_contrib_supersedes
    ON pyramid_config_contributions(supersedes_id);
CREATE INDEX idx_config_contrib_wire
    ON pyramid_config_contributions(wire_contribution_id);
```

### Schema Type Vocabulary

| schema_type | Scope | Description |
|---|---|---|
| `skill` | global | Prompt skill (markdown body). Generation prompts, extraction prompts, merge prompts, heal prompts, prepare prompts, migration prompts. Wire type: `skill`. |
| `schema_definition` | global | JSON schema for validating config YAMLs. Wire type: `template`. |
| `schema_annotation` | global | YAML-to-UI renderer metadata for a config schema. Wire type: `template`. |
| `evidence_policy` | per-pyramid | Triage rules, demand signals, budget |
| `build_strategy` | per-pyramid | Model tiers, concurrency, evidence mode per build phase |
| `dadbear_policy` | per-pyramid | Scan intervals, debounce, propagation, maintenance schedule |
| `tier_routing` | global | Model tier -> provider + model mappings |
| `step_overrides` | per-pyramid | Per-step model tier or prompt overrides |
| `custom_prompts` | per-pyramid | Extraction focus, synthesis style, vocabulary priority |
| `custom_chains` | per-pyramid | Full chain YAML (extends chain_publish pattern) |
| `folder_ingestion_heuristics` | per-pyramid | File patterns, ignore rules, content type detection |
| `wire_discovery_weights` | global | Ranking weights for Wire discovery (see `wire-discovery-ranking.md`) |
| `wire_auto_update_settings` | global | Per-schema_type auto-update toggles (see `wire-discovery-ranking.md`) |

Global configs use `slug = NULL`. Per-pyramid configs use the pyramid's slug.

The full mapping of each `schema_type` to its Wire contribution type (`skill` / `template` / `action`) and the Wire Native Documents metadata schema lives in `wire-contribution-mapping.md`.

### Active Version Resolution

The active version for a given `(slug, schema_type)` is the latest contribution where `status = 'active'` and `superseded_by_id IS NULL`:

```sql
SELECT * FROM pyramid_config_contributions
WHERE slug = ?1 AND schema_type = ?2
  AND status = 'active'
  AND superseded_by_id IS NULL
ORDER BY created_at DESC
LIMIT 1;
```

For global configs, the query uses `slug IS NULL`.

---

## Supersession Chain

Each config version forms a chain linked by `supersedes_id`:

```
v1 (contribution_id: "abc-001", supersedes_id: NULL)
  note: "Generated from intent: conservative local-only policy"
    ↓ superseded_by_id → "abc-002"
v2 (contribution_id: "abc-002", supersedes_id: "abc-001")
  note: "Tighten scan interval to 10s, add demand signals"
    ↓ superseded_by_id → "abc-003"
v3 (contribution_id: "abc-003", supersedes_id: "abc-002")
  note: "Agent suggested: reduce batch_size based on observed OOM"
    ↓ (active — superseded_by_id: NULL)
```

When v3 is created:
1. v2's `superseded_by_id` is set to "abc-003"
2. v2's `status` is set to "superseded"
3. v3's `status` is set to "active"
4. v3's `triggering_note` records the reason for the change

All three versions remain in the table. The full chain is queryable for audit, rollback, and Wire publication (sharing the chain shows how a config evolved).

### Version History Query

```sql
-- Walk the chain backward from active version
WITH RECURSIVE chain AS (
    SELECT * FROM pyramid_config_contributions
    WHERE slug = ?1 AND schema_type = ?2
      AND status = 'active' AND superseded_by_id IS NULL
    UNION ALL
    SELECT c.* FROM pyramid_config_contributions c
    JOIN chain ON c.contribution_id = chain.supersedes_id
)
SELECT * FROM chain ORDER BY created_at ASC;
```

---

## Notes Paradigm Integration

The generative config pattern spec defines the notes-based refinement loop. This spec defines how notes become contribution provenance:

1. User provides notes on current config via UI
2. Current YAML + notes are sent to the LLM (per generative config pattern)
3. LLM produces new YAML (v(n+1))
4. New YAML is stored as a contribution with:
   - `supersedes_id` = current active contribution_id
   - `triggering_note` = the user's note text
   - `status` = "active"
5. Previous version's `status` set to "superseded", `superseded_by_id` set to new contribution_id
6. Operational tables updated (see Operational Table Sync below)

The note is the provenance. Looking at a config's version history, every transition has a human-readable reason attached.

---

## Agent Proposal Flow

Agents can propose config changes via the MCP server. Proposals are contributions with `status: "proposed"` that require user review.

### Proposal Creation (Agent Side)

```
POST pyramid_propose_config
  Input: {
    schema_type: "dadbear_policy",
    slug: "my-pyramid",
    yaml_content: "...",          -- the proposed YAML
    note: "Reducing batch_size from 5 to 2 because last 3 builds hit OOM at batch_size 5",
    agent_name: "build-optimizer"
  }
  Output: { contribution_id: String, status: "proposed" }
```

The proposed contribution is stored with:
- `status` = "proposed"
- `source` = "agent"
- `created_by` = agent_name
- `supersedes_id` = current active contribution_id (what it would replace)
- `triggering_note` = agent's note

### Review Flow (User Side)

The UI shows pending proposals for each config type. The user can:

**Accept**: Sets `status` = "active" on the proposal, supersedes the current active version, triggers operational table sync. Sets `accepted_at` to now.

**Reject**: Sets `status` = "rejected". The proposal remains in history but never becomes active.

**Refine**: User adds notes to the proposal. The proposal's YAML + user notes go through the generative config LLM to produce a refined version. The refined version supersedes the original proposal (not the current active config). When accepted, the refined version supersedes the current active config.

### IPC Commands

```
GET pyramid_pending_proposals
  Input: { slug?: String }
  Output: [{ contribution_id, schema_type, slug, yaml_content, note, agent_name, created_at }]

POST pyramid_accept_proposal
  Input: { contribution_id: String }
  Output: { accepted_contribution_id: String }

POST pyramid_reject_proposal
  Input: { contribution_id: String, reason?: String }
  Output: { ok: bool }
```

---

## Operational Table Sync

The existing operational tables (`pyramid_dadbear_config`, etc.) remain for runtime reads. They are fast, denormalized, and optimized for the executor's hot path. The contribution table is the source of truth and audit trail.

### Sync Direction

```
pyramid_config_contributions (source of truth)
    ↓ on accept/activate
operational tables (runtime cache)
```

Write path: always write to `pyramid_config_contributions` first, then sync to operational tables.
Read path: executor reads from operational tables (fast). UI reads from contribution table (for version history, notes, proposals).

### Sync Mechanism

When a contribution transitions to `status = 'active'`, `sync_config_to_operational()` runs the full sync pipeline:

1. **Validate** — load the YAML body, fetch the active `schema_definition` contribution for this `schema_type`, validate the YAML against the JSON Schema. Validation failure aborts the sync and leaves the prior active contribution in place. The caller sees a `ConfigValidationError` with the specific field failures.
2. **Dispatch by schema_type** — route to the schema-specific upsert. Each upsert is a single transaction: delete/update the operational row, insert the new state, record `contribution_id` FK.
3. **Emit `ConfigSynced` event** — fires on the `BuildEventBus` with payload `{ slug, schema_type, contribution_id, prior_contribution_id }`.
4. **Trigger affected systems to reload** — per schema_type (see table below).

```rust
pub fn sync_config_to_operational(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    contribution: &ConfigContribution,
) -> Result<(), ConfigSyncError> {
    // 1. Validate against active schema_definition for this schema_type
    let schema_def = load_active_schema_definition(conn, &contribution.schema_type)?;
    validate_yaml_against_schema(&contribution.yaml_content, &schema_def)
        .map_err(ConfigSyncError::ValidationFailed)?;

    // 2. Dispatch by schema_type
    let prior_id = load_prior_active_contribution_id(conn, &contribution.slug, &contribution.schema_type)?;
    match contribution.schema_type.as_str() {
        "dadbear_policy" => {
            let yaml: DadbearPolicy = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_dadbear_config(conn, &contribution.slug, &yaml, &contribution.contribution_id)?;
            trigger_dadbear_reload(bus, &contribution.slug);
        }
        "evidence_policy" => {
            let yaml: EvidencePolicy = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_evidence_policy(conn, &contribution.slug, &yaml, &contribution.contribution_id)?;
            reevaluate_deferred_questions(conn, &contribution.slug, &yaml)?;  // see evidence-triage spec
        }
        "build_strategy" => {
            let yaml: BuildStrategy = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_build_strategy(conn, &contribution.slug, &yaml, &contribution.contribution_id)?;
        }
        "tier_routing" => {
            let yaml: TierRouting = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_tier_routing(conn, &yaml, &contribution.contribution_id)?;  // global, no slug
            invalidate_provider_resolver_cache();
        }
        "custom_prompts" => {
            let yaml: CustomPrompts = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_custom_prompts(conn, &contribution.slug, &yaml, &contribution.contribution_id)?;
            invalidate_prompt_cache();
        }
        "step_overrides" => {
            // Step overrides are a per-pyramid+chain bundle: delete all existing
            // rows for (slug, chain_id) then insert one row per override entry.
            let bundle: StepOverridesBundle = serde_yaml::from_str(&contribution.yaml_content)?;
            replace_step_overrides_bundle(conn, &contribution.slug, &bundle, &contribution.contribution_id)?;
            invalidate_provider_resolver_cache();
        }
        "custom_chains" => {
            // Custom chains write to disk and register the chain with the chain registry.
            sync_custom_chain_to_disk(conn, &contribution.contribution_id)?;
            register_chain_with_registry(conn, &contribution.contribution_id)?;
            invalidate_prompt_cache();  // chain may reference new skills
        }
        "folder_ingestion_heuristics" => {
            let yaml: FolderIngestionHeuristics = serde_yaml::from_str(&contribution.yaml_content)?;
            upsert_folder_ingestion_heuristics(conn, &contribution.slug, &yaml, &contribution.contribution_id)?;
        }
        "skill" => {
            // Skills write their body to the prompt cache directly; no separate operational table.
            // The prompt body is served from pyramid_config_contributions.yaml_content via the cache.
            invalidate_prompt_cache();
        }
        "schema_definition" => {
            // Schema definitions are loaded on demand by validate_yaml_against_schema().
            // Superseding a schema flags downstream configs for migration.
            flag_configs_for_migration(conn, &contribution.schema_type_target()?)?;
            invalidate_schema_registry_cache();
        }
        "schema_annotation" => {
            // Schema annotations feed the YAML-to-UI renderer. Invalidate the renderer cache.
            invalidate_schema_annotation_cache();
        }
        "wire_discovery_weights" => {
            // Ranking algorithm weights. Invalidate the discovery ranking cache.
            invalidate_wire_discovery_cache();
        }
        "wire_auto_update_settings" => {
            // Per-schema_type auto-update toggles. Triggers scheduler reconfiguration.
            reconfigure_wire_update_scheduler(conn)?;
        }
        other => {
            // Unknown types are a bug — schema registry should only emit known types.
            // Fail loudly rather than silently skipping sync.
            return Err(ConfigSyncError::UnknownSchemaType(other.to_string()));
        }
    }

    // 3. Emit ConfigSynced event
    bus.emit(TaggedKind::ConfigSynced {
        slug: contribution.slug.clone(),
        schema_type: contribution.schema_type.clone(),
        contribution_id: contribution.contribution_id.clone(),
        prior_contribution_id: prior_id,
    });

    Ok(())
}
```

### Config Types with Operational Sync

| schema_type | Operational table / target | Sync strategy | Reload trigger |
|---|---|---|---|
| `dadbear_policy` | `pyramid_dadbear_config` (existing) | UPSERT on slug, write into existing columns | `trigger_dadbear_reload(slug)` — DADBEAR tick picks up new config on next cycle (it already re-reads per tick) |
| `evidence_policy` | `pyramid_evidence_policy` (new) | UPSERT on slug | `reevaluate_deferred_questions(slug, new_policy)` — see evidence-triage-and-dadbear.md |
| `build_strategy` | `pyramid_build_strategy` (new) | UPSERT on slug | None — read on next build start |
| `tier_routing` | `pyramid_tier_routing` (existing) | UPSERT on global key | `invalidate_provider_resolver_cache()` — next LLM call re-resolves the tier |
| `custom_prompts` | `pyramid_custom_prompts` (new) | UPSERT on slug | `invalidate_prompt_cache()` — prompt composition layer re-reads |
| `step_overrides` | `pyramid_step_overrides` (see provider-registry.md) | DELETE by (slug, chain_id) + INSERT bundle | `invalidate_provider_resolver_cache()` |
| `custom_chains` | Disk files (`chains/custom/`, `chains/prompts/`) + `pyramid_chain_registry` | Write chain YAML + prompt files, register chain | `invalidate_prompt_cache()` + `register_chain_with_registry()` |
| `folder_ingestion_heuristics` | `pyramid_folder_ingestion_heuristics` (new) | UPSERT on slug | None — read on next folder scan |
| `skill` | Served from `pyramid_config_contributions.yaml_content` via `prompt_cache` | `invalidate_prompt_cache()` | Next LLM call re-resolves prompt body |
| `schema_definition` | Loaded on demand by `validate_yaml_against_schema()` | `flag_configs_for_migration(target_schema_type)` + `invalidate_schema_registry_cache()` | Existing configs flagged for LLM-assisted migration |
| `schema_annotation` | Loaded on demand by YAML-to-UI renderer | `invalidate_schema_annotation_cache()` | Renderer re-reads on next mount |
| `wire_discovery_weights` | Used by ranking algorithm (see wire-discovery-ranking.md) | `invalidate_wire_discovery_cache()` | Next discovery query re-applies weights |
| `wire_auto_update_settings` | Used by Wire update scheduler (see wire-discovery-ranking.md) | `reconfigure_wire_update_scheduler()` | Scheduler picks up new per-schema_type toggles |

Each operational row carries a `contribution_id` FK back to the `pyramid_config_contributions` row it was produced from, so the executor can always trace an operational value back to its provenance.

### Schema-specific sync helper signatures

Each schema_type has a dedicated upsert function with a uniform signature:

```rust
fn upsert_evidence_policy(
    conn: &Connection,
    slug: &Option<String>,              // None = global
    yaml: &EvidencePolicy,              // parsed YAML struct
    contribution_id: &str,              // FK back to pyramid_config_contributions
) -> Result<()>;
```

All upserts:
1. Run inside a single transaction
2. DELETE the prior row (if per-slug) OR UPDATE the global row
3. INSERT the new state with the FK
4. Return error if the new state fails row-level constraints (e.g., `creator_split` doesn't sum to 48 for circle contributions)

The per-schema_type constraint enforcement is in addition to the JSON Schema validation done at step 1 of `sync_config_to_operational()`. JSON Schema validates structure; these helpers validate business rules (sum constraints, referential integrity, etc.).

### custom_prompts Operational Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_custom_prompts (
    slug TEXT,                        -- NULL = global
    extraction_focus TEXT,            -- "architectural decisions and their rationale"
    synthesis_style TEXT,             -- "concise, decision-oriented"
    vocabulary_priority_json TEXT,    -- JSON array of priority types
    ignore_patterns_json TEXT,        -- JSON array of patterns
    contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id),
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug)
);
```

**Runtime behavior:** when the chain executor prepares an LLM call, it reads the active `pyramid_custom_prompts` row for the current slug (falling back to the row where `slug IS NULL` if no per-pyramid row exists) and appends `extraction_focus` / `synthesis_style` as additional system prompt context. `vocabulary_priority_json` reorders the vocabulary filter's weighting, and `ignore_patterns_json` augments the vocabulary filter's drop list.

### step_overrides Operational Table

`pyramid_step_overrides` is defined in `provider-registry.md`. The contribution sync path writes into it:

- `schema_type` = `"step_overrides"`
- `yaml_content` is a YAML document listing overrides as `{ slug, chain_id, step_name, field_name, value }` entries
- sync strategy: `DELETE` all rows from `pyramid_step_overrides` where `(slug, chain_id)` matches the bundle's scope, then `INSERT` one row per override entry, each carrying the `contribution_id` FK

This makes step_overrides a **per-pyramid+chain bundle**: users accept the whole bundle at once, never individual entries. A bundle is superseded as a unit by the next contribution that scopes to the same `(slug, chain_id)`.

### Bootstrap: Migrating Existing Configs

On first run after migration:
1. Read all rows from `pyramid_dadbear_config`
2. For each, serialize current field values to YAML
3. Insert into `pyramid_config_contributions` with `source = "migration"`, `status = "active"`, `triggering_note = "Migrated from legacy table"`
4. The operational tables remain populated — no runtime disruption

This is a one-time data migration. Going forward, the contribution table is the entry point.

---

## Wire Native Documents Integration

Every contribution in `pyramid_config_contributions` carries **canonical** Wire Native Documents metadata from the moment of creation. The `wire_native_metadata_json` column stores a `WireNativeMetadata` struct that mirrors the canonical schema in `GoodNewsEveryone/docs/wire-native-documents.md` exactly:

- **Routing**: `destination` (corpus/contribution/both), `corpus`, `contribution_type` (skill/template/action/analysis/...), `scope` (unscoped/fleet/circle:<name>)
- **Identity**: `topics`, structured `entities` ({name, type, role}), `maturity` (draft/design/canon/deprecated)
- **Relationships**: `derived_from` + `supersedes` + `related`, all using canonical `ref:` / `doc:` / `corpus:` reference formats (NOT resolved UUIDs — path references resolve at sync time)
- **Claims**: `claims` array with `trackable` + optional `end_date`
- **Economics**: `price` OR `pricing_curve` (mutually exclusive), `embargo_until`
- **Distribution**: `pin_to_lists`, `notify_subscribers`
- **Circle splits**: `creator_split` (must sum to 48 slots, operator meta-pools)
- **Lifecycle**: `auto_supersede`, `sync_mode` (auto/review/manual)
- **Decomposition**: `sections` map (one source produces multiple contributions)

The full struct definition, field-by-field defaults, and validation rules live in `wire-contribution-mapping.md`. **Do not duplicate them here or diverge from the canonical field names.**

Because the metadata is captured at creation time, publishing to Wire is a button click — no re-entry of metadata, no separate publish form. Every path that creates a row in `pyramid_config_contributions` (generate, refine, propose, pull, bootstrap migration, bundled seed load) initializes `wire_native_metadata_json` per the "Creation-Time Capture" table in `wire-contribution-mapping.md`.

The `schema_type` of a contribution maps to a Wire contribution type via the mapping table in `wire-contribution-mapping.md`. The publish pipeline uses that mapping to select the Wire type.

**Separate column for publication state**: `wire_publication_state_json` stores the resolved publication state separately from the canonical metadata: `wire_contribution_id`, `handle_path`, `chain_root`, `chain_head`, `last_resolved_derived_from`. Keeping publication state out of the canonical metadata means the metadata stays portable across users (path references resolve against each user's local corpus + the Wire graph, not against a specific user's UUID cache).

---

## Wire Publication

Configs publish to the Wire via `pyramid_publish_to_wire` (canonical definition in `wire-contribution-mapping.md`). The publication path reads `wire_native_metadata_json` from the stored contribution — the user does not re-enter metadata at publish time.

The full publish flow, dry-run preview, cost breakdown, supersession chain carryover, section decomposition, and 28-slot `derived_from` allocation are all defined in `wire-contribution-mapping.md`. This section covers only the mechanical linkage back to the contribution store.

### Writeback

After a successful publish:

1. `wire_publication_state_json.wire_contribution_id` is set to the Wire UUID returned
2. `wire_publication_state_json.handle_path` is populated with the Wire-assigned handle-path (e.g., `playful/77/3`)
3. `wire_publication_state_json.chain_root` and `chain_head` are populated per supersession linkage
4. `wire_publication_state_json.last_resolved_derived_from` caches the resolved references + allocated slots for audit
5. `wire_native_metadata_json.maturity` transitions from `draft` to `design` (or whatever the user set) on first publish
6. `pyramid_id_map` records the mapping (`local_id = contribution_id`, `wire_uuid = returned UUID`)

### Version Chain Publication

The full supersession chain can be published to Wire, not just the active version. When a refined contribution is published, its `wire_native_metadata.supersedes` field is pre-populated from the prior version's Wire ref (see `wire-contribution-mapping.md` for the auto-population rules). Wire uses the `supersedes` field to link the new contribution into the existing supersession chain:

```
Wire contribution "abc-wire-001" (v1, initial policy)
  superseded_by →
Wire contribution "abc-wire-002" (v2, tightened intervals, note: "...")
  superseded_by →
Wire contribution "abc-wire-003" (v3, agent-suggested batch_size, note: "...")
```

Other users browsing the Wire see the full history, including why each change was made. If a user's local chain contains versions that were never published (e.g., drafts), only the published versions form the Wire chain.

---

## Wire Pull (Config Discovery + Import)

Users can search the Wire for configs and pull them into their local node. This adapts the corpus sync pattern (`SyncState`, `SyncDiff`, `LinkedFolder`) for config-shaped data.

### Search

```
POST pyramid_search_wire_configs
  Input: {
    schema_type: "dadbear_policy",   -- filter by type
    tags?: ["conservative", "code"], -- optional tag filter
    query?: "low cost maintenance"   -- optional text search
  }
  Output: [{
    wire_contribution_id: String,
    schema_type: String,
    description: String,
    tags: [String],
    author_handle: String,
    version_chain_length: i64,
    created_at: String,
    yaml_preview: String             -- first 500 chars of the YAML for preview
  }]
```

This calls the Wire's search/discovery API filtered by `content_type = "configuration"` and `schema_type`.

### Pull

```
POST pyramid_pull_wire_config
  Input: {
    wire_contribution_id: String,
    slug: String,                    -- which pyramid to apply this to (or NULL for global)
    activate: bool                   -- true = make active immediately, false = import as proposed
  }
  Output: { contribution_id: String, status: String }
```

Pull creates a local contribution with:
- `source` = "wire"
- `wire_contribution_id` = the Wire UUID
- `yaml_content` = the pulled YAML
- `status` = "active" (if `activate: true`) or "proposed" (if `activate: false`)
- `triggering_note` = "Pulled from Wire: {author_handle}/{description}"

If `activate: true`, the current active config (if any) is superseded and operational tables are synced.

### Pulled Config Refinement

After pulling a Wire config, the user can refine it with notes (same flow as local configs). The refinement creates a new local contribution that supersedes the pulled one. The pulled version's `wire_contribution_id` is preserved in the chain history.

---

## Linkage to Chain Publication

The existing `chain_publish.rs` publishes chain YAMLs + prompts as Wire contributions via `ChainBundle`. The `custom_chains` schema_type in this spec subsumes that pattern:

- `chain_publish.rs` continues to handle the bundle serialization (chain YAML + prompt files)
- The bundle is stored as `yaml_content` in a `pyramid_config_contributions` row with `schema_type = "custom_chains"`
- Publication uses the same Wire path but now has version history and notes

This is a backward-compatible extension. Existing `pyramid_chain_publications` records can be migrated to `pyramid_config_contributions` with `schema_type = "custom_chains"`.

---

## Custom Chain Bundle Serialization

A `custom_chain` contribution's `yaml_content` is a YAML document with two top-level keys: a `chain` block holding the chain definition, and a `prompts` map holding every prompt file the chain references. This keeps the chain and its prompts atomic — one contribution, one version, one supersession.

### Bundle Format

```yaml
chain: |
  # Full chain YAML content
  schema_version: 1
  id: my-custom-chain
  name: My Custom Chain
  description: Custom extraction chain tuned for architectural decisions
  steps:
    - name: extract
      kind: for_each
      prompt: question/source_extract.md
      ...
prompts:
  "question/source_extract.md": |
    # Prompt file content
    You are an extractor focused on architectural decisions...
  "shared/merge_sub_chunks.md": |
    # Another prompt file content
    Merge the following sub-chunks into a single coherent result...
```

The `chain` value is a single block scalar containing the full chain YAML. The `prompts` value is a map where each key is the path of the prompt file **relative to the prompts root**, and each value is the file's content as a block scalar.

### Accept / Sync Path

When a `custom_chains` contribution is activated, `sync_custom_chain_to_disk(contribution_id)` runs:

1. Parse `yaml_content` into `{ chain: String, prompts: Map<String, String> }`
2. Parse the `chain` string as a `ChainDefinition` struct — abort if it fails to deserialize
3. Confirm every prompt path referenced by the chain YAML (via its `prompt:` / `merge_prompt:` fields) exists either in the bundle's `prompts` map OR already on disk under the standard prompts directory — abort if any are missing
4. Confirm `chain.id` is unique: no other active `custom_chains` contribution uses the same id and no built-in chain ships with that id
5. Write the chain YAML to `~/Library/Application Support/wire-node/chains/custom/{chain_id}.yaml`
6. For each `(path, content)` entry in `prompts`, write `content` to `~/Library/Application Support/wire-node/chains/prompts/{path}`, creating intermediate directories as needed

On supersession, the previous bundle's files are left in place until the new bundle's writes succeed — if the new bundle fails validation, the previous chain remains runnable.

### Validation Rules

| Rule | Enforcement point |
|---|---|
| `chain` block parses as a valid `ChainDefinition` | Before accept |
| `chain.id` is unique across active contributions and built-in chains | Before accept |
| Every prompt path referenced by the chain resolves (bundle or standard dir) | Before accept |
| Prompt paths use forward slashes and contain no `..` segments | Before accept |
| Bundle size fits within the Wire contribution size budget | Before publication (not local accept) |

### Migration of Existing Chain Publications

On first run after this spec is implemented, existing rows in `pyramid_chain_publications` are converted to `custom_chains` contributions:

1. Read each publication row to get the `chain_id` and Wire UUID
2. Read the chain YAML from disk (`chains/custom/{chain_id}.yaml`)
3. Walk the chain YAML to collect every referenced prompt path
4. Read each prompt file from disk (`chains/prompts/{path}`)
5. Serialize all of it into the bundle format above
6. Insert a `pyramid_config_contributions` row with `schema_type = "custom_chains"`, `status = "active"`, `source = "migration"`, `wire_contribution_id` carried over from the publication row, `triggering_note = "Migrated from pyramid_chain_publications"`

After migration, `pyramid_chain_publications` is redundant and can be dropped in a follow-up migration.

---

## IPC Contract (Full)

```
# Config contribution lifecycle
POST pyramid_create_config_contribution
  Input: { schema_type, slug?, yaml_content, note?, source? }
  Output: { contribution_id: String }

POST pyramid_supersede_config
  Input: { contribution_id: String, new_yaml_content: String, note: String }
  Output: { new_contribution_id: String }

GET pyramid_active_config_contribution
  Input: { schema_type, slug? }
  Output: { contribution_id, yaml_content, version_chain_length, created_at, triggering_note }

GET pyramid_config_version_history
  Input: { schema_type, slug? }
  Output: [{ contribution_id, yaml_content, triggering_note, status, source, created_at }]

POST pyramid_rollback_config
  Input: { contribution_id: String }  -- contribution to roll back TO
  Output: { new_contribution_id: String }  -- creates a new version with the rolled-back content

# Agent proposals
POST pyramid_propose_config
  Input: { schema_type, slug, yaml_content, note, agent_name }
  Output: { contribution_id: String }

GET pyramid_pending_proposals
  Input: { slug?: String }
  Output: [{ contribution_id, schema_type, slug, yaml_content, note, created_by, created_at }]

POST pyramid_accept_proposal
  Input: { contribution_id: String }
  Output: { accepted_contribution_id: String }

POST pyramid_reject_proposal
  Input: { contribution_id: String, reason?: String }
  Output: { ok: bool }

# Wire sharing
# Canonical publish lifecycle lives in wire-contribution-mapping.md.
# Publishing reads the stored wire_native_metadata_json, so no tags/description are passed here.
POST pyramid_publish_to_wire
  Input: { contribution_id: String, confirm: bool }
  Output: { wire_contribution_id: String, handle_path: String }
  See: wire-contribution-mapping.md

POST pyramid_dry_run_publish
  Input: { contribution_id: String }
  Output: { visibility, cost_breakdown, supersession_chain, derived_from_resolved, warnings }
  See: wire-contribution-mapping.md

POST pyramid_search_wire_configs
  Input: { schema_type, tags?, query? }
  Output: [{ wire_contribution_id, schema_type, description, tags, author_handle, yaml_preview }]

POST pyramid_pull_wire_config
  Input: { wire_contribution_id: String, slug?: String, activate: bool }
  Output: { contribution_id: String, status: String }

# Notes-based refinement (generative config intent + refinement loop)
POST pyramid_generate_config
  Input: { schema_type: String, slug?: String, intent: String }
  Output: { yaml_content: String, contribution_id: String }

POST pyramid_refine_config
  Input: { contribution_id: String, current_yaml: Value, note: String }
  Output: { yaml: Value, version: u32, new_contribution_id: String }

# Force-fresh reroll (bypass cache for a specific config contribution)
POST pyramid_reroll_config
  Input: { contribution_id: String, note?: String }
  Output: { new_contribution_id: String, yaml_content: String }
```

`pyramid_refine_config` and `pyramid_reroll_config` enforce the Notes Capture Lifecycle rules:
- `pyramid_refine_config` requires a non-empty `note` (the refinement reason). Empty-note refinements are rejected at the IPC boundary with an error.
- `pyramid_reroll_config` accepts an optional note but the UI layer enforces the anti-slot-machine confirmation when the note is empty.

---

## Files Modified

| Phase | Files |
|---|---|
| DB schema | `db.rs` — new `pyramid_config_contributions` table, migration logic |
| Contribution CRUD | New `config_contributions.rs` — create, supersede, resolve active, history query |
| Operational sync | `config_contributions.rs` — `sync_config_to_operational()` per schema_type |
| Agent proposal | MCP server handlers — `pyramid_propose_config` |
| Wire publication | `wire_publish.rs` — add `publish_config()` method to `PyramidPublisher` |
| Wire pull | New `config_pull.rs` — search + pull from Wire |
| Migration | `db.rs` — one-time migration of `pyramid_dadbear_config` rows to contributions |
| IPC commands | `main.rs` or `routes.rs` — new Tauri/HTTP commands for contribution lifecycle |
| Frontend | `ToolsMode.tsx` (existing) — extend My Tools/Discover/Create tabs for config contributions |

### Frontend: ToolsMode.tsx

The existing `ToolsMode.tsx` has three tabs — `My Tools`, `Discover`, `Create` — with Discover and Create as "Coming in Sprint 3" placeholders. This is the natural home:

- **My Tools** tab: Currently shows Wire-published `action` type contributions. Extend `MyToolsPanel` to show all config contributions from `pyramid_config_contributions`. Group by `schema_type`. Each entry shows: description, version count, status (active/proposed), "Publish to Wire" button for unpublished configs.
- **Discover** tab: Replace placeholder with Wire config browser. Search by `schema_type` + tags via `pyramid_search_wire_configs`. Preview configs via `YamlConfigRenderer` in read-only mode. "Pull" button calls `pyramid_pull_wire_config`.
- **Create** tab: Replace placeholder with generative config entry point. Schema type selector + intent text area. Calls `pyramid_generate_config`, renders result via `YamlConfigRenderer`, supports notes refinement, accept creates a config contribution.

Version history and proposal review can be inline expandable sections within the My Tools entries, or a detail drawer (matching `PyramidDetailDrawer` pattern).

---

## Implementation Order

1. **DB table + migration** — create `pyramid_config_contributions`, migrate existing `pyramid_dadbear_config` rows
2. **Contribution CRUD** — create, supersede, active resolution, version history
3. **Operational sync** — write-through from contributions to operational tables (DADBEAR first)
4. **Notes paradigm** — generative config pattern writes to contributions instead of operational tables
5. **Agent proposals** — MCP endpoint for proposed contributions, review UI
6. **Wire publication** — publish configs as Wire contributions
7. **Wire pull** — search + import from Wire

Steps 1-4 are the critical path. Steps 5-7 extend the system to agents and Wire.

---

## Open Questions

1. **Conflict resolution on Wire pull**: If a user pulls a config that conflicts with their current active config (e.g., different schema version), how to handle? Recommend: always create as proposed, let the user review. The `activate: true` shortcut skips review but is opt-in.

2. **Schema versioning**: When a schema_type's JSON Schema evolves (new fields, changed structure), how do old YAML configs validate? Recommend: schemas are additive (new optional fields only). Breaking changes create a new schema_type (e.g., `dadbear_policy_v2`). Migration between schema versions is an LLM-assisted refinement: "Migrate this YAML to the v2 schema."

3. **Global vs per-pyramid operational sync**: `tier_routing` is global but some pyramids may want per-pyramid overrides. Recommend: per-pyramid configs with `slug` set override global configs with `slug IS NULL`. Resolution order: per-pyramid -> global -> hardcoded default. This is already how tier routing conceptually works.

4. **Contribution size limits**: Custom chains can be large (chain YAML + all prompt files). Should `yaml_content` have a size limit? Recommend: no hard limit in v1. The data is text and compresses well. If storage becomes an issue, add content-addressable deduplication (same pattern as the LLM output cache).

---

## Notes Capture Lifecycle

Notes are the provenance that justifies every config transition, but not every refinement path needs a note. The rules below clarify when notes are required, encouraged, or unnecessary:

| Path | Notes | Reason |
|---|---|---|
| Initial config generation (intent -> YAML) | Not needed — intent IS the note | The user's intent string becomes the `triggering_note` on the v1 contribution |
| Config refinement (v1 -> v2) | **Required** | Cannot supersede without a note explaining why the change was made |
| Pulled config refinement | **Required** | Same as local refinement — supersession must carry a reason |
| Node reroll via `force_fresh` | **Strongly encouraged** (not blocked) | Prevents slot-machine rerolls but allows emergency rerolls when a generated YAML is unusable |
| Stale check manifest generation | N/A | Automated — no user note; the LLM produces a `reason` field that lives with the manifest, not in `triggering_note` |
| Agent-proposed config | **Required** | The agent must justify the proposal; the note becomes the user's primary signal for accept/reject |

Enforcement lives at the IPC boundary: `pyramid_supersede_config` and `pyramid_propose_config` both reject requests with an empty or whitespace-only `note`. Refinement paths (`pyramid_refine_config`) surface the note requirement in the UI before the LLM is even called, so users cannot burn tokens on a refinement that would fail to save.

`force_fresh` reroll goes through a dedicated IPC (`pyramid_reroll_config`) that accepts an optional note. The UI surfaces a note field and warns users that rerolls without notes lose supersession provenance, but does not block the request.

---

## Operational Table Schemas

Each config type whose `sync_config_to_operational()` writes to a dedicated operational table is listed below. Every operational table carries a `contribution_id` FK back to `pyramid_config_contributions` so the executor can always resolve an operational value to the contribution that produced it. These tables are SOURCE = contributions — they are not written directly; they exist only as runtime caches populated by the sync path.

### pyramid_evidence_policy

```sql
CREATE TABLE IF NOT EXISTS pyramid_evidence_policy (
    slug TEXT,                        -- NULL = global
    triage_rules_json TEXT NOT NULL,  -- JSON array of rule objects
    demand_signals_json TEXT NOT NULL,
    budget_json TEXT NOT NULL,
    contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id),
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug)
);
```

### pyramid_build_strategy

```sql
CREATE TABLE IF NOT EXISTS pyramid_build_strategy (
    slug TEXT,                        -- NULL = global
    initial_build_json TEXT NOT NULL,
    maintenance_json TEXT NOT NULL,
    quality_json TEXT NOT NULL,
    contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id),
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug)
);
```

### pyramid_custom_prompts

```sql
CREATE TABLE IF NOT EXISTS pyramid_custom_prompts (
    slug TEXT,                        -- NULL = global
    extraction_focus TEXT,
    synthesis_style TEXT,
    vocabulary_priority_json TEXT,
    ignore_patterns_json TEXT,
    contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id),
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug)
);
```

### pyramid_folder_ingestion_heuristics

```sql
CREATE TABLE IF NOT EXISTS pyramid_folder_ingestion_heuristics (
    slug TEXT,                          -- NULL = global default
    min_files_for_pyramid INTEGER NOT NULL DEFAULT 3,
    max_file_size_bytes INTEGER NOT NULL DEFAULT 10485760,
    max_recursion_depth INTEGER NOT NULL DEFAULT 10,
    content_type_rules_json TEXT NOT NULL,  -- JSON array of detection rules
    ignore_patterns_json TEXT NOT NULL,     -- JSON array of glob patterns
    respect_gitignore INTEGER NOT NULL DEFAULT 1,
    respect_pyramid_ignore INTEGER NOT NULL DEFAULT 1,
    vine_collapse_single_child INTEGER NOT NULL DEFAULT 1,
    contribution_id TEXT NOT NULL REFERENCES pyramid_config_contributions(id),
    updated_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug)
);
```

Runtime behavior: `folder_ingestion.rs` reads the active row for the target slug (falling back to `slug IS NULL` for the global default) at the start of each folder walk. All decision points in the walk algorithm (see `vine-of-vines-and-folder-ingestion.md`) reference columns in this table — no hardcoded numbers.

### pyramid_dadbear_config (existing)

`pyramid_dadbear_config` already exists as the operational table for DADBEAR policy. The contribution sync writes into its existing columns (`scan_interval_secs`, `debounce_secs`, etc.) rather than creating a new table. A `contribution_id` column is added to the existing table via migration so DADBEAR rows gain the same provenance link as the new tables above.

### pyramid_step_overrides (defined in provider-registry.md)

`pyramid_step_overrides` is owned by the provider registry spec. This spec adds a `contribution_id` column via migration and treats the table as an operational sink for `step_overrides` contributions. See the step_overrides sync strategy above for the DELETE-then-INSERT bundle behavior.
