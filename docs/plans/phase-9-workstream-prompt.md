# Workstream: Phase 9 — Generative Config Pattern

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8 are shipped. You are the implementer of Phase 9, which ships the **generative config loop** — the backend that turns a user's natural-language intent into a structured YAML config contribution via an LLM round trip, with notes-based refinement and contribution storage.

Phase 9 is substantial — it wires together Phase 4 (contribution CRUD), Phase 5 (bundled contributions manifest + Wire Native metadata), Phase 6 (StepContext for LLM calls), and Phase 8 (schema annotations for the renderer) into the first user-facing "describe what you want → see a config" loop. Phase 10 adds the frontend wizard on top.

## Context

The vision doc mandates notes over regeneration. The flow is:

```
User intent (natural language)
  → LLM generates structured YAML conforming to a schema
  → System renders YAML as editable UI (Phase 8's renderer)
  → User accepts or provides notes
  → YAML becomes a contribution (Phase 4)
  → Shared on Wire (Phase 5) → community discovers best versions
```

Phase 9 ships the backend generation, refinement, accept, and registry. Every piece goes through contributions — no hardcoded defaults, no privileged seeds. The `schema_definition`, `schema_annotation`, and generation `skill` for every config type are themselves contributions (bundled on first run, refinable, shareable).

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/generative-config-pattern.md` — read in full (422 lines).** Primary implementation contract. Particular attention to: Components section (schema registry, generation prompts as skills, schema annotations as templates, schema definitions as contributions, seed defaults as bundled contributions, notes-based refinement loop) at lines 29-145, Config Types at 147-299, IPC Contract at 300-358, Implementation Order at 373-386, Seed Defaults Architecture at 388-415.
3. `docs/specs/config-contribution-and-wire-sharing.md` — Phase 4's spec. **The canonical IPC definitions for accept/refine/rollback live here** — Phase 9's IPC MUST match byte-for-byte. Re-read the Notes Capture Lifecycle section (lines ~750-766).
4. `docs/specs/wire-contribution-mapping.md` — Phase 5's spec. Scan the "Bundled Contributions" section for the manifest format and bootstrap insertion path that Phase 9 extends.
5. `docs/specs/llm-output-cache.md` — Phase 6's spec. Re-read the StepContext section (~line 210) — Phase 9's LLM calls MUST pass a StepContext so cache hits work for generation.
6. `docs/specs/yaml-to-ui-renderer.md` — Phase 8's spec. You don't modify the renderer, but you consume its contract: the frontend gets a `SchemaAnnotation` + `values` from Phase 9's generation and renders it.
7. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 9 section + parallelism map.
8. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 4 (CRUD + sync dispatcher), Phase 5 (bundled contributions, canonical metadata), Phase 6 (StepContext retrofit pattern), Phase 8 (schema annotation loader + registry).
9. `docs/plans/pyramid-folders-model-routing-friction-log.md` — scan for wanderer findings from Phases 4/5/6/7/8 that establish the "primitive exists but integration missing" pattern. Do NOT repeat the pattern in Phase 9.

### Code reading (targeted)

10. `src-tauri/src/pyramid/config_contributions.rs` — Phase 4's CRUD + sync dispatcher. Phase 9 calls `create_config_contribution_with_metadata` (Phase 5) + `sync_config_to_operational` (Phase 4) for the accept path. Find `load_active_config_contribution`, `load_config_version_history`, `supersede_config_contribution` — these are what your new IPC handlers delegate to.
11. `src-tauri/src/pyramid/yaml_renderer.rs` — Phase 8's module. `load_schema_annotation_for(schema_type)` is already built; Phase 9's schema registry uses it.
12. `src-tauri/src/pyramid/wire_migration.rs` — Phase 5's bundled-contributions migration pattern. Phase 9 extends the migration with a new `walk_bundled_contributions_manifest` step that inserts generation skills + JSON schemas + seed configs.
13. `src-tauri/src/pyramid/wire_native_metadata.rs` — Phase 5's `default_wire_native_metadata` + canonical metadata. Phase 9's bundled contributions carry canonical metadata from the manifest.
14. `src-tauri/src/pyramid/step_context.rs` — Phase 6's StepContext. Phase 9's LLM calls use this for cache + event emission.
15. `src-tauri/src/pyramid/llm.rs` — find `call_model_via_registry` or `call_model_unified_with_options_and_ctx` — whichever is the canonical LLM entry point post-Phase-6.
16. `src-tauri/src/main.rs` — find the IPC command block. You'll register ~6 new commands.
17. `src-tauri/src/pyramid/provider.rs` — Phase 3's `ProviderRegistry::resolve_tier` (used by your StepContext construction).
18. `chains/prompts/` — the existing prompt directory. Phase 9 adds `chains/prompts/generation/` for the new generation skills.
19. `chains/schemas/` — Phase 8's schema annotation directory. Phase 9 adds matching `*.json` files for JSON schemas (or `chains/schemas/validation/` depending on your layout).

## What to build

### 1. Bundled contributions manifest

Create `src-tauri/assets/bundled_contributions.json` (or `chains/bundled_contributions.json` if that's cleaner — check where other bundled assets live in the repo).

Structure per the spec's "Seed Defaults Architecture" section:

```json
{
  "manifest_version": 1,
  "generated_at": "2026-04-10",
  "contributions": [
    {
      "contribution_id": "bundled-skill-generation-evidence_policy",
      "schema_type": "skill",
      "slug": null,
      "yaml_content": "# The generation prompt body in markdown",
      "wire_native_metadata": { ... },
      "triggering_note": "Bundled generation prompt for evidence_policy (app v0.2.0)"
    },
    {
      "contribution_id": "bundled-schema_definition-evidence_policy",
      "schema_type": "schema_definition",
      "slug": null,
      "yaml_content": "{ ...JSON schema... }",
      "wire_native_metadata": { ... },
      "triggering_note": "Bundled JSON schema for evidence_policy (app v0.2.0)"
    },
    {
      "contribution_id": "bundled-schema_annotation-evidence_policy",
      "schema_type": "schema_annotation",
      "slug": null,
      "yaml_content": "# The schema annotation YAML from Phase 8",
      "wire_native_metadata": { ... },
      "triggering_note": "Bundled UI annotation for evidence_policy (app v0.2.0)"
    },
    {
      "contribution_id": "bundled-evidence_policy-default",
      "schema_type": "evidence_policy",
      "slug": null,
      "yaml_content": "# Seed default evidence policy YAML",
      "wire_native_metadata": { ... },
      "triggering_note": "Bundled default evidence_policy (app v0.2.0)"
    }
  ]
}
```

For Phase 9 scope, ship the manifest with at minimum:
- `evidence_policy` (generation skill + JSON schema + schema annotation + seed default)
- `build_strategy` (same set)
- `dadbear_policy` (same set — schema annotation already exists from Phase 8)
- `tier_routing` (generation skill + JSON schema + seed default — schema annotation exists from Phase 8)
- `custom_prompts` (same set)

Stretch: `folder_ingestion_heuristics`, `schema_migration_policy`, `wire_discovery_weights`.

### 2. Bundled contribution bootstrap (extending `wire_migration.rs`)

Extend Phase 5's `migrate_prompts_and_chains_to_contributions` to ALSO load the bundled contributions manifest and insert each entry as a contribution with:
- `source = "bundled"`
- `status = "active"`
- `created_by = "bootstrap"`
- Canonical Wire Native metadata from the manifest (not auto-generated)

Idempotency via the existing `_prompt_migration_marker` sentinel OR a separate `_bundled_contributions_marker` sentinel (implementer's call — the spec is silent).

The `contribution_id` in the manifest becomes the DB's `contribution_id` — NOT auto-generated. This is so the schema registry can reference bundled contributions by stable ID and so app upgrades can replace bundled contributions without supersession drama.

**Edge case:** if a bundled contribution's `contribution_id` already exists in the DB (second run, or app upgrade), SKIP insertion (INSERT OR IGNORE semantics). Do NOT UPDATE — the user may have superseded the bundled default with their own refinement, and overwriting would clobber their work.

### 3. Schema registry (new `src-tauri/src/pyramid/schema_registry.rs`)

```rust
pub struct SchemaRegistry {
    // In-memory cache of active schemas, keyed by schema_type
    entries: RwLock<HashMap<String, ConfigSchema>>,
}

pub struct ConfigSchema {
    pub schema_type: String,
    pub display_name: String,
    pub description: String,
    pub schema_definition_contribution_id: String,
    pub schema_annotation_contribution_id: String,
    pub generation_skill_contribution_id: String,
    pub default_seed_contribution_id: Option<String>,
    pub version: u32,
}

impl SchemaRegistry {
    /// Load all active schemas by querying the contribution store.
    /// For each distinct schema_type in the manifest, resolve:
    ///   - schema_definition (active contribution where schema_type = 'schema_definition' AND applies_to = <target>)
    ///   - schema_annotation (via Phase 8's load_schema_annotation_for)
    ///   - generation skill (active contribution where schema_type = 'skill' AND tag contains "generation:<target>")
    ///   - default seed (active contribution where schema_type = <target> AND source = 'bundled')
    pub fn hydrate_from_contributions(conn: &Connection) -> Result<Self>
    
    /// Look up a schema by name. Returns None if no active schemas exist for that type.
    pub fn get(&self, schema_type: &str) -> Option<ConfigSchema>
    
    /// List all known schema types.
    pub fn list(&self) -> Vec<ConfigSchema>
    
    /// Re-hydrate from the DB. Called when a schema_definition / schema_annotation /
    /// generation skill contribution is superseded (via the dispatcher's
    /// invalidate_schema_registry_cache hook from Phase 4).
    pub fn invalidate(&self, conn: &Connection) -> Result<()>
}
```

The registry is held as a field on `PyramidState` (add `schema_registry: Arc<SchemaRegistry>`). Hydrate at boot after `init_pyramid_db` + bundled migration runs.

**Wire Phase 4's `invalidate_schema_registry_cache` stub** to actually invalidate this registry. The stub is in `config_contributions.rs:sync_config_to_operational` under the `schema_definition` branch.

### 4. Generation skill prompts (markdown files on disk, migrated to bundled contributions)

Create `chains/prompts/generation/` directory with one `.md` file per config type. Each is a generation prompt that takes `{schema}`, `{intent}`, `{current_yaml}`, `{notes}` placeholders and produces a YAML document.

Example: `chains/prompts/generation/evidence_policy.md`:

```markdown
You are generating an evidence triage policy YAML for a Wire Node knowledge pyramid.

Convert the user's natural-language intent into a valid evidence_policy YAML
conforming to the JSON Schema below.

## Schema
{schema}

## User Intent
{intent}

{if current_yaml}
## Current Values (you are refining this existing policy)
{current_yaml}
{end}

{if notes}
## User Refinement Notes
{notes}

Apply the user's notes to the current values. Keep everything else the same
unless the notes imply changes. Preserve the user's intent from prior rounds.
{end}

Output a complete, valid YAML document. Include brief comments explaining
non-obvious choices, especially where the notes drove a change.
```

The `{placeholder}` substitution is a simple string replace — no Jinja2, no handlebars. Phase 9's generation code substitutes the placeholders at call time.

Create one of these for each of the minimum set of schema types (evidence_policy, build_strategy, dadbear_policy, tier_routing, custom_prompts).

### 5. JSON schemas (shipped as bundled schema_definition contributions)

Create matching JSON Schema documents for each config type. These validate structure (required fields, types, enum values). Keep them minimal — Phase 9's validator is a best-effort safety net, not a strict gate.

Example evidence_policy schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Evidence Policy",
  "type": "object",
  "required": ["schema_type", "triage_rules"],
  "properties": {
    "schema_type": {"type": "string", "const": "evidence_policy"},
    "triage_rules": {
      "type": "array",
      "items": { ... }
    },
    "demand_signals": { ... },
    "budget": { ... }
  }
}
```

Store each JSON schema as a `schema_definition` contribution via the bundled manifest. The `applies_to` field in the Wire Native metadata is the target config schema_type (e.g., `"evidence_policy"`).

### 6. `pyramid_generate_config` IPC handler

```rust
#[tauri::command]
async fn pyramid_generate_config(
    state: State<'_, PyramidState>,
    schema_type: String,
    slug: Option<String>,
    intent: String,
) -> Result<GenerateConfigResponse, String>
```

Flow:
1. Look up `schema_type` in `state.schema_registry` — error if not found
2. Load the active generation skill contribution body
3. Load the active schema_definition contribution body (the JSON schema)
4. Substitute `{schema}` + `{intent}` in the skill body (leave `{current_yaml}` / `{notes}` blocks empty for initial generation)
5. Construct a StepContext (Phase 6) with `step_name = "generate_config"`, `primitive = "config_generation"`, `slug` = the passed slug (or "global" if None), `build_id` = `format!("gen-{contribution_id_prefix}")`
6. Call `call_model_unified_with_options_and_ctx` (Phase 6's cache-aware entry) with the substituted prompt
7. Parse the LLM response as YAML — error if invalid
8. Optionally validate the YAML against the JSON schema (Phase 9 uses the `jsonschema` crate if it's already in deps; otherwise skip validation with a comment)
9. Build canonical Wire Native metadata (`draft: true`, topics from schema_type, etc. — use `default_wire_native_metadata` from Phase 5)
10. Create a contribution via `create_config_contribution_with_metadata` with `source = "local"`, `created_by = "generative_config"`, `triggering_note = <user's intent string>`, status = `"draft"` (NOT active — user must Accept to promote to active)
11. Return `{ contribution_id, yaml_content, schema_type, schema_annotation, version: 1 }`

### 7. `pyramid_refine_config` IPC handler

```rust
#[tauri::command]
async fn pyramid_refine_config(
    state: State<'_, PyramidState>,
    contribution_id: String,
    current_yaml: String,
    note: String,
) -> Result<RefineConfigResponse, String>
```

**Notes enforcement:** reject requests where `note.trim().is_empty()` with a clear error message. This is the Notes Capture Lifecycle rule from Phase 4's spec.

Flow:
1. Load the contribution by `contribution_id`
2. Look up the schema in the registry
3. Load the generation skill + JSON schema
4. Substitute `{schema}` + `{intent}` (from the original contribution's triggering_note) + `{current_yaml}` (from the passed argument) + `{notes}` (from the passed argument)
5. Construct StepContext with `primitive = "config_refinement"`, `step_name = "refine_config"`, `force_fresh = false` (cache is fine — same prompt + same inputs = same output)
6. Call the LLM
7. Parse the response as YAML
8. Create a NEW contribution via `supersede_config_contribution` (Phase 4's helper) with `triggering_note = <user's note>`, status = `"draft"`
9. Return the new contribution + YAML + bumped version

### 8. `pyramid_accept_config` IPC handler

The canonical definition is in `config-contribution-and-wire-sharing.md`. This handler wraps Phase 4's accept path with the generative flow's view:

```rust
#[tauri::command]
async fn pyramid_accept_config(
    state: State<'_, PyramidState>,
    schema_type: String,
    slug: Option<String>,
    yaml: serde_json::Value,
    triggering_note: Option<String>,
) -> Result<AcceptConfigResponse, String>
```

Flow:
1. Either: (a) load an existing draft contribution by schema_type + slug and promote it to active, OR (b) if `yaml` is provided directly, create a new active contribution
2. Call Phase 4's `sync_config_to_operational` which dispatches to the schema-specific upsert
3. Return the full `AcceptConfigResponse` shape from the config-contribution spec

The spec says this writes to `pyramid_config_contributions` AND triggers `sync_config_to_operational()`. Match the canonical IPC signature byte-for-byte.

### 9. `pyramid_active_config` + `pyramid_config_versions` IPC handlers

Thin wrappers around Phase 4's existing functions:

```rust
#[tauri::command]
async fn pyramid_active_config(...) -> Result<ActiveConfigResponse, String>
// Calls load_active_config_contribution

#[tauri::command]
async fn pyramid_config_versions(...) -> Result<Vec<ConfigVersion>, String>
// Calls load_config_version_history
```

### 10. `pyramid_config_schemas` IPC handler

Lists all schemas from the registry:

```rust
#[tauri::command]
async fn pyramid_config_schemas(
    state: State<'_, PyramidState>,
) -> Result<Vec<ConfigSchemaSummary>, String>
// Returns state.schema_registry.list() as a summary
```

Each entry: `{ schema_type, display_name, description, has_generation_skill, has_annotation, has_default_seed }`

### 11. Schema migration (deferred-but-scaffolded)

The spec describes schema migration as "when a schema_definition is superseded, flag configs for migration, run a migration skill on each." Phase 9 scope:
- **In scope:** wire Phase 4's `flag_configs_for_migration` stub to actually walk `pyramid_config_contributions` and mark rows whose `schema_type` matches the superseded schema_definition's `applies_to`. Store the "needs migration" flag in a new column `needs_migration INTEGER DEFAULT 0` on `pyramid_config_contributions`.
- **Out of scope for Phase 9:** the migration skill + `pyramid_migrate_config` IPC command. Add a TODO in the migration helper pointing at Phase 10 for the actual migration LLM call.

### 12. Tests

- `test_bundled_contributions_migration_inserts_skills_and_schemas` — on fresh DB, verify the manifest's entries all land as contributions with `source = 'bundled'`
- `test_bundled_contributions_migration_idempotent` — run twice, verify no duplicate rows
- `test_bundled_contributions_skip_user_superseded` — bundled default exists, user supersedes with refinement, re-run migration — verify user's version stays active
- `test_schema_registry_hydrate_from_contributions` — seed minimal bundled contributions, call `hydrate_from_contributions`, verify the registry has entries
- `test_schema_registry_invalidate_re_reads` — change a schema_definition contribution, call invalidate, verify registry reflects the new state
- `test_generate_config_happy_path` — mock the LLM (see Phase 6 tests for the mocking pattern if one exists), call `pyramid_generate_config`, verify contribution lands with status='draft' + triggering_note = intent
- `test_refine_config_requires_note` — reject empty notes
- `test_refine_config_creates_supersession` — verify new contribution has `supersedes_id` pointing at the old one
- `test_accept_config_triggers_sync` — create a draft contribution, call accept, verify status flipped to active AND `sync_config_to_operational` was called (check that the operational table has a row with the contribution_id FK)
- `test_config_schemas_list_returns_bundled_set` — list schemas, verify all the bundled schema_types are present
- `test_flag_configs_for_migration_sets_needs_migration_column` — supersede a schema_definition, call the dispatcher's schema_definition branch, verify matching config rows have `needs_migration = 1`

## Scope boundaries

**In scope:**
- Bundled contributions manifest (min 5 schema types + their 3-piece sets = 15-20 contributions)
- `walk_bundled_contributions_manifest` migration extension
- `schema_registry.rs` module + `SchemaRegistry` struct + `PyramidState` field
- Wiring Phase 4's `invalidate_schema_registry_cache` stub
- Generation prompt markdown files under `chains/prompts/generation/`
- JSON schema files for each config type
- 6 new IPC commands: `pyramid_generate_config`, `pyramid_refine_config`, `pyramid_accept_config`, `pyramid_active_config`, `pyramid_config_versions`, `pyramid_config_schemas`
- Wiring Phase 4's `flag_configs_for_migration` stub
- `needs_migration` column on `pyramid_config_contributions`
- Tests

**Out of scope:**
- Frontend wizard (Phase 10)
- `pyramid_migrate_config` IPC command (Phase 10)
- Cross-pyramid config UI (Phase 10)
- Wire publish from generative flow (uses Phase 5 IPC directly)
- Wire pull + import from generative flow (uses Phase 5/7 paths)
- Custom chain generation (Phase 10 via the chain YAML path)
- Partial/streaming generation (Phase 10 UX concern)
- The 7 pre-existing unrelated test failures
- JSON Schema validation library integration if not already in deps (use text-level "is this parseable YAML" as the check for Phase 9)

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` — clean, zero new warnings.
2. `cargo test --lib pyramid::schema_registry` + `cargo test --lib pyramid::wire_migration` — all new tests passing.
3. `cargo test --lib pyramid` — 1010 passing (Phase 8 count) + new Phase 9 tests. Same 7 pre-existing failures. No new ones.
4. `grep -rn "bundled-" src-tauri/assets/ chains/` — verify bundled manifest + prompts + schemas exist on disk.
5. `grep -n "invalidate_schema_registry_cache\|flag_configs_for_migration" src-tauri/src/pyramid/` — verify both Phase 4 stubs are now wired (not just TODO logs).
6. Integration test: on fresh DB, call `pyramid_config_schemas` — verify the 5+ bundled schema types are listed.

## Deviation protocol

Standard. Most likely deviations:
- **JSON Schema validation library.** If `jsonschema` crate is not in deps, skip structural validation in Phase 9 (parse as YAML → pass). Adding a new crate dep for this is out of scope unless trivial. Flag the gap for Phase 10.
- **Generation prompt quality.** Your prompts are seeds, not production-grade. If the LLM produces weird output in testing, note it but don't over-iterate — users will refine via notes.
- **Schema migration scope.** If wiring `flag_configs_for_migration` reveals a deeper need (e.g., a whole new migration UI), stop at "set the flag column" and defer the UI + migration execution to Phase 10.

## Implementation log protocol

Append Phase 9 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document: bundled manifest structure, schema registry design, migration extension, IPC handlers, generation prompts shipped, JSON schemas shipped, stubs wired, tests, verification results. Status: `awaiting-verification`.

## Mandate

- **Correct before fast.** The schema registry is the foundation for Phase 10's UI. Get the contract right.
- **Every config goes through contributions.** Do NOT bypass Phase 4's create/sync path for shortcut storage. If you're tempted to write directly to an operational table, stop — that's the anti-pattern wanderers caught in Phases 4/5.
- **StepContext for every LLM call.** Do NOT call `call_model_unified_with_options` directly (the shim path that bypasses the cache). Use `call_model_unified_with_options_and_ctx` with a full StepContext.
- **Notes enforcement at the IPC boundary.** `pyramid_refine_config` rejects empty notes. Do not push the check to the LLM layer or the UI.
- **Bundled contributions are first-class citizens.** `source = 'bundled'` is just another value; bundled contributions are superseded normally by user work or Wire pulls.
- **Wire Phase 4's stubs.** `invalidate_schema_registry_cache` and `flag_configs_for_migration` are both Phase 4 TODOs that Phase 9 is responsible for actually implementing. Don't leave them as stubs.
- **Fix all bugs found.** Standard.
- **Commit when done.** Single commit with message `phase-9: generative config pattern`. Body: 5-7 lines summarizing bundled manifest + schema registry + IPC commands + stubs wired + tests. Do not amend. Do not push.

## End state

Phase 9 is complete when:

1. `src-tauri/assets/bundled_contributions.json` (or equivalent path) exists with ≥15 entries covering 5+ schema types.
2. `wire_migration.rs` extension loads the manifest and inserts bundled contributions idempotently, skipping user-superseded entries.
3. `schema_registry.rs` exists with `SchemaRegistry` + `ConfigSchema` + `hydrate_from_contributions` + `invalidate`.
4. `PyramidState` gains `schema_registry: Arc<SchemaRegistry>` populated at boot.
5. Phase 4's `invalidate_schema_registry_cache` and `flag_configs_for_migration` stubs are wired to actually do the work.
6. `needs_migration` column added to `pyramid_config_contributions` via idempotent migration.
7. 6 new IPC commands registered and backed by real handlers.
8. Generation skill markdown files exist under `chains/prompts/generation/` for each schema type.
9. JSON schemas exist (either on disk as seed files OR inline in the bundled manifest).
10. `cargo check`, `cargo build`, `cargo test --lib pyramid` all pass with 1010+ passing and same 7 pre-existing failures.
11. Implementation log Phase 9 entry complete.
12. Single commit on branch `phase-9-generative-config-pattern`.

Begin with the spec. Then Phase 4/5/6/8 code paths for patterns. Then write.

Good luck. Build carefully.
