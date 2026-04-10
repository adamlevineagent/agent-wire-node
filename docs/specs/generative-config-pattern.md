# Generative Configuration Pattern Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** YAML-to-UI renderer, provider registry
**Unblocks:** Evidence triage, DADBEAR policy editor, custom chains, custom prompts
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Every behavioral configuration in Wire Node follows the same flow:

```
User intent (natural language)
    → LLM generates structured YAML conforming to a schema
    → System renders YAML as editable UI (via YAML-to-UI renderer)
    → User accepts or provides notes
    → YAML becomes runtime config (stored as a contribution)
    → Shared on Wire → community discovers best versions
```

This spec defines the generation infrastructure, not any individual config type. The infrastructure is built once; every new configurable behavior gets intent-to-YAML-to-UI-to-contribution for free.

---

## Components

### 1. Schema Registry (backed by contributions)

A registry of valid config types. The registry is **not** a list of on-disk files — it queries the `pyramid_config_contributions` table for the active `schema_definition`, `schema_annotation`, and `skill` contributions that together define each config type.

Each config type resolves to:
- A **JSON Schema** for structural validation — stored as a `schema_definition` contribution (Wire type: `template`, tags `["schema", "validation", schema_type]`)
- A **schema annotation** for the YAML-to-UI renderer — stored as a `schema_annotation` contribution (Wire type: `template`, tags `["schema", "annotation", schema_type]`)
- A **generation prompt** for intent -> YAML — stored as a `skill` contribution (Wire type: `skill`, tags `["prompt", "wire-node", "generation", schema_type]`)
- **Default values** — a seed `template` contribution shipped with the binary as `source = "bundled"`

```rust
pub struct ConfigSchema {
    pub schema_type: String,                          // "evidence_policy", "build_strategy", etc.
    pub schema_definition_contribution_id: String,    // points at a schema_definition contribution
    pub schema_annotation_contribution_id: String,    // points at a schema_annotation contribution
    pub generation_skill_contribution_id: String,     // points at a skill contribution
    pub default_seed_contribution_id: Option<String>, // seed config, if any
    pub version: u32,
}
```

The schema registry loads its entries by querying the contribution store for all active `schema_definition` contributions, then joining with the matching `schema_annotation` and generation `skill` contributions by the shared `schema_type` tag. On first run, the app ships with seed contributions (bundled inside the binary as a manifest) that get inserted as `source = "bundled"` contributions. Users can then supersede them with their own versions.

No filesystem lookups. No `chains/schemas/` directory. The registry is always a view over the contribution store.

### 2. Generation Prompts Are Skills

Each `schema_type` has an associated **generation skill** — a Wire skill contribution with tags `["prompt", "wire-node", "generation", schema_type]`. The `pyramid_generate_config` IPC command looks up the active generation skill for the requested `schema_type` and uses its body as the LLM instruction.

The skill body receives:
- User intent (natural language)
- The JSON Schema definition (so the LLM knows valid structure) — fetched from the active `schema_definition` contribution
- The current values (if refining, not generating from scratch)
- User notes (if this is a refinement round)

Example skill body for `evidence_policy` generation:

```markdown
You are generating an evidence triage policy YAML for a Wire Node knowledge pyramid.

The user has expressed their intent in natural language. Convert this to a valid
evidence_policy YAML conforming to the schema below.

## Schema
{schema}

## User Intent
{intent}

## Current Values (if refining)
{current_yaml}

## User Notes (if refining)
{notes}

Output the complete YAML document. Include comments explaining non-obvious choices.
```

Because generation prompts are skills, they can be refined via notes, superseded by newer versions, pulled from Wire, and auto-populate `derived_from` when referenced by actions. See `wire-contribution-mapping.md` for the full mapping from local prompt files to Wire skill contributions.

### 3. Schema Annotations Are Templates

Each `schema_type` has an associated **schema annotation template** — a Wire template contribution with tags `["schema", "annotation", schema_type]`. The YAML-to-UI renderer loads these via `pyramid_get_schema_annotation()`, which resolves the active schema_annotation contribution for the requested schema_type and returns its `yaml_content`.

Schema annotations control the renderer: widget types, layout hints, field labels, dynamic options, hidden sections. Because they're templates, they can be refined, versioned, and shared on Wire like any other contribution. A user who wants a more compact evidence_policy editor can refine the annotation via notes, accept the new version, and see the updated UI immediately — no rebuild, no restart.

### 4. Schema Definitions Are Contributions

The JSON schema that validates a config YAML is itself a **template contribution** with tags `["schema", "validation", schema_type]`. When a schema is superseded (a new version published with additional required fields or changed structure), the user's existing config contributions for that `schema_type` are flagged as "may need migration" and ToolsMode shows a "Migrate" button that calls the LLM to refine the old YAML into the new schema shape.

The migration flow:

1. User clicks "Migrate" on a flagged config contribution
2. Backend calls the LLM with: old YAML + old schema + new schema + a migration skill (`schema_type = "skill"`, tags `["prompt", "wire-node", "schema-migration"]`)
3. LLM produces a refined YAML matching the new schema
4. Backend creates a new config contribution with `supersedes_id` pointing at the old version and `triggering_note = "Migrated from schema v{old} to v{new}"`
5. User reviews the migrated YAML in the renderer (which now uses the new schema annotation)
6. User accepts -> becomes the active version

Schema migration is itself a contribution-driven flow: the migration skill can be refined, the weights for what constitutes "may need migration" live in a `schema_migration_policy` template, etc. No hardcoded migration logic.

### 5. Seed Defaults Are Themselves Contributions

The first-run seeded defaults for each schema_type are shipped with the app as bundled contributions. They're inserted on first startup with `source = "bundled"`, `status = "active"`, and the Wire Native metadata `draft: false`, `tags: [schema_type, "built-in"]`.

Users can refine, supersede, or pull alternatives from Wire. The bundled defaults are the **starting point**, not absolute standards. A bundled default looks identical to a user-refined or Wire-pulled contribution in the store — only the `source` field distinguishes them. This means every behavior described in this spec (refinement, notes, version history, Wire publish) works on seed defaults from day one.

See `wire-contribution-mapping.md` for the bundled contributions manifest format and the bootstrap insertion path.

### 6. Notes-Based Refinement Loop

The vision doc mandates notes over regeneration. The refinement flow:

1. **v1**: LLM generates YAML from intent
2. **User reviews** via YAML-to-UI renderer
3. **User provides notes**: "less aggressive, local only for maintenance"
4. **v2**: LLM takes existing YAML + notes → generates new version
5. **v2 supersedes v1** — both exist in version history, note attached as provenance
6. Repeat until user accepts

The LLM sees the full existing YAML plus the notes. This is refinement, not regeneration:
- The LLM knows what the user implicitly accepted (everything not mentioned)
- The LLM knows what to change (the notes)
- Intent narrows with each round

### 7. Contribution Storage

Accepted configs are stored as contributions using the existing contribution pattern:
- Config YAML is the contribution content
- Schema type is the contribution type
- Supersession chain tracks version history with notes as provenance
- Shareable on Wire (contribution is the native sharing unit)
- Wire Native Documents metadata is captured at creation time (see `wire-contribution-mapping.md`) so publishing is a button click — no re-entry of metadata

---

## Config Types

> **All values shown below are seed defaults.** Implementation MUST NOT hardcode any of these values. Every field flows from the user's active config contribution for this schema_type.

### Evidence Policy
```yaml
schema_type: evidence_policy
fields:
  triage_rules:
    - condition: "first_build AND depth == 0"
      action: answer           # answer | defer | skip
      model_tier: stale_local
      priority: normal         # normal | high | low
    - condition: "stale_check AND no_demand_signals"
      action: defer
      check_interval: "never"  # "never" | "7d" | "30d" | "on_demand"
    - condition: "stale_check AND has_demand_signals"
      action: answer
      model_tier: stale_local

  demand_signals:
    - type: agent_query_count
      threshold: 2
      window: "14d"
    - type: user_drill_count
      threshold: 1
      window: "7d"

  budget:
    maintenance_model_tier: stale_local
    initial_build_model_tier: stale_local
    max_concurrent_evidence: 1
    triage_batch_size: 15       # how many questions per triage LLM call
```

### Build Strategy
```yaml
schema_type: build_strategy
fields:
  initial_build:
    model_tier: synth_heavy
    concurrency: 10
    evidence_mode: deep         # deep | shallow | skip
    webbing: true
  maintenance:
    model_tier: stale_local
    concurrency: 1
    evidence_mode: demand_only
    webbing: false
  quality:
    min_distillation_length: 200
    require_evidence: true
    require_webbing: true
```

### DADBEAR Policy
```yaml
schema_type: dadbear_policy
fields:
  scan_interval_secs: 30
  debounce_secs: 60
  session_timeout_secs: 1800
  batch_size: 5
  stale_propagation:
    max_cascade_depth: 3
    propagation_model_tier: stale_local
  maintenance_schedule:
    mode: demand_only            # always | demand_only | manual
    check_interval: "7d"
    model_tier: stale_local
```

### Custom Prompts
```yaml
schema_type: custom_prompts
fields:
  extraction_focus: "architectural decisions and their rationale"
  synthesis_style: "concise, decision-oriented"
  vocabulary_priority:
    - decisions
    - entities
    - practices
  ignore_patterns:
    - "boilerplate"
    - "generated code"
    - "test fixtures"
```

### Folder Ingestion Heuristics

#### Full YAML schema
```yaml
schema_type: folder_ingestion_heuristics
fields:
  min_files_for_pyramid: 3
  max_file_size_bytes: 10485760
  max_recursion_depth: 10

  content_type_detection:
    # Each rule maps file signals to a content type.
    # Processed top-to-bottom; first match wins.
    rules:
      - signal: { extensions: [".rs", ".ts", ".tsx", ".py", ".go", ".js", ".java", ".rb", ".c", ".cpp", ".h"] }
        content_type: code
      - signal: { extensions: [".md", ".txt", ".pdf", ".doc", ".docx", ".rst"] }
        content_type: document
      - signal: { extensions: [".json"], structure_check: "conversation_messages_array" }
        content_type: conversation
      - signal: { extensions: [".yaml", ".yml"], structure_check: "chain_definition" }
        content_type: skip   # chain YAMLs are not content
      - signal: { match_all: true }
        content_type: skip   # default: skip unknown

  ignore_patterns:
    - "node_modules/"
    - "target/"
    - ".git/"
    - "*.lock"
    - "*.bin"
    - "*.exe"
    - "*.dylib"
    - ".DS_Store"

  respect_gitignore: true
  respect_pyramid_ignore: true

  vine_collapse_single_child: true   # single-child vines get inlined into parent
```

#### Generation prompt

The generation prompt is a **skill contribution** with tags `["prompt", "wire-node", "generation", "folder_ingestion_heuristics"]`. The LLM receives the user's natural-language intent about how they want their folder ingested and produces a `folder_ingestion_heuristics` YAML. It should consider the user's constraints — for example: "I have deeply nested code folders" implies raising `max_recursion_depth`; "I want docs to be their own pyramid" implies ensuring `.md` detection is prioritized in the `content_type_detection.rules`; "I have large binary assets" implies tightening `ignore_patterns`. The prompt follows the standard generation prompt shape described above (schema + intent + current_yaml + notes) and is resolved from the contribution store at generation time — no filesystem path.

#### Schema annotation reference

The schema annotation for the YAML-to-UI renderer is a **template contribution** with tags `["schema", "annotation", "folder_ingestion_heuristics"]`. See `yaml-to-ui-renderer.md` for the annotation format. Widget types: `content_type_detection.rules` renders as a list widget with nested group items; `ignore_patterns` as a list widget with text items; booleans as toggles; numeric fields as number widgets with appropriate bounds.

### Custom Chain
```yaml
schema_type: custom_chain
fields:
  chain_id: ""
  name: ""
  description: ""
  content_type: code
  steps: []
  defaults:
    model_tier: synth_heavy
    temperature: 0.3
```

---

## IPC Contract

> **IPC authority note:** The canonical definitions for all contribution-lifecycle IPC commands live in `config-contribution-and-wire-sharing.md`. This spec's signatures below MUST match that file exactly. The generative config layer is a thin wrapper over the contribution layer — it does not define its own IPC surface, only the generation prompts and schema registry behind these commands.

```
# Generate config from intent (creates v1 contribution)
POST pyramid_generate_config
  Input: { schema_type: String, slug?: String, intent: String }
  Output: { yaml_content: String, contribution_id: String }
  Note: intent string becomes the triggering_note for the initial contribution.

# Refine with notes (creates a new superseding version)
POST pyramid_refine_config
  Input: { contribution_id: String, current_yaml: Value, note: String }
  Output: { yaml: Value, version: u32, new_contribution_id: String }
  Note: `note` is REQUIRED (non-empty). Empty notes rejected at IPC boundary
        per the Notes Capture Lifecycle rules in config-contribution spec.

# Accept current config (activates a contribution + triggers operational sync)
POST pyramid_accept_config
  Input: { schema_type: String, slug?: String, yaml: Value, triggering_note?: String }
  Output: {
    contribution_id: String,
    yaml_content: String,                       // the stored canonical YAML
    version: u32,                                // supersession chain depth (v1 = 1, refinement = 2, ...)
    triggering_note: String,                    // note that produced this version
    status: String,                              // "active" after successful sync
    wire_native_metadata: WireNativeMetadata,   // canonical metadata captured at creation
    sync_result: {                               // operational sync outcome
      operational_table: String,                 // e.g. "pyramid_evidence_policy"
      reload_triggered: Vec<String>,              // e.g. ["invalidate_provider_resolver_cache"]
    },
  }
  Note: Writes to pyramid_config_contributions AND triggers sync_config_to_operational().
        Returns the full contribution state so the UI can render the accepted result
        without a follow-up query. Canonical IPC definition lives in config-contribution-and-wire-sharing.md.

# Get active config for a slug
GET pyramid_active_config
  Input: { schema_type: String, slug?: String }
  Output: { contribution_id, yaml_content, version_chain_length, created_at, triggering_note }

# Get version history
GET pyramid_config_versions
  Input: { schema_type: String, slug?: String }
  Output: [{ contribution_id, yaml_content, triggering_note, status, source, created_at }]

# List available schema types (from the schema registry)
GET pyramid_config_schemas
  Output: [{
    schema_type,
    display_name,
    description,
    schema_definition_contribution_id,
    schema_annotation_contribution_id,
    generation_skill_contribution_id,
  }]
```

---

## Wire Sharing

When a user accepts a config, it becomes a contribution. On the Wire:
- Tagged with schema_type + descriptive tags the user provides
- Other users can search by schema_type + tags
- Pulling a config installs it as the active config for that schema_type
- The full version chain (including notes) is visible — "they started with default, made it local-only, tightened intervals, added demand signals"

The sharing mechanism uses existing Wire contribution infrastructure. No new sharing plumbing needed.

---

## Implementation Order

1. **Bundled contributions bootstrap** — load the bundled manifest and insert `skill`, `schema_definition`, `schema_annotation`, and seed `template` contributions on first run
2. **Schema registry** — build in-memory registry by querying the contribution store for active `schema_definition` + `schema_annotation` + generation `skill` contributions
3. **Generation endpoint** — `pyramid_generate_config` IPC command; resolves the active generation skill from the contribution store and sends its body to the LLM
4. **Notes refinement** — `pyramid_refine_config` IPC command
5. **Contribution storage** — `pyramid_accept_config` saves as contribution with Wire Native metadata
6. **Active config resolution** — `pyramid_active_config` returns the latest accepted version for a slug
7. **Version history** — `pyramid_config_versions` returns the full chain
8. **Schema migration** — detect config contributions whose `schema_definition` has been superseded, surface "Migrate" UI, run the migration skill

The YAML-to-UI renderer (separate spec) handles the frontend. This spec covers the backend generation and storage. Wire publish flow lives in `wire-contribution-mapping.md`.

---

## Seed Defaults Architecture

Rather than hardcoding seed defaults in migration code, seed defaults are shipped with the app as **bundled contributions**. This keeps defaults under the same supersession system as user-authored configs — no privileged hardcoded values, no special-case paths, no "absolute standards."

### How bundled contributions work

- On first startup (or first activation of a `schema_type`), the app inserts the bundled contribution into `pyramid_config_contributions` with:
  - `source = "bundled"`
  - `status = "active"`
  - `triggering_note = "Bundled default shipped with app version X.Y.Z"`
  - `wire_native_metadata_json` populated from the bundle manifest (title, description, tags, `draft: false`)
- Bundled contributions ship inside the binary as a JSON manifest (`assets/bundled_contributions.json`) — see `wire-contribution-mapping.md` for the manifest format and bootstrap path.
- Each bundled contribution has its own `contribution_id` (prefix: `bundled-`) so it participates in supersession normally.
- Bundled contributions cover every schema_type the spec defines, and also cover the `skill`, `schema_definition`, and `schema_annotation` contributions that power the schema registry itself — the registry is fully bootstrapped from the manifest on first run.

### What users can do with bundled defaults

- **Use as-is** — no action needed; the bundled contribution is active on first startup.
- **Refine with notes** — creates a local version superseding the bundled one. Standard contribution flow.
- **Pull an alternative from Wire** — the pulled version supersedes the bundled one. Standard Wire sharing flow.
- **Restore the bundled default** — re-activates the bundled contribution, superseding whatever is currently active. The supersession row is tagged `source = "revert-to-bundled"` for audit.

### Rationale

Bundled defaults are not privileged. They can be superseded by anything — user notes, Wire pulls, restores — and they can themselves be restored by request. Every config in the system, whether shipped by Anthropic, written by the user, or pulled from Wire, lives in the same supersession chain with the same provenance semantics. This fully honors the vision's "no absolute standards" principle: the app ships with starting points, not with rules.

The app upgrade path is also uniform: a new app version may bring a newer bundled contribution (`v2.yaml`), but it does NOT automatically supersede the user's active config. It is available for restoration, and the user sees it in the schema's version browser as an option to pull forward.

---

## Open Questions

1. **Schema discovery on Wire**: Should schema types themselves be shareable? A user creates a new config type (e.g., "security_review_policy") with its own schema — should others be able to pull that schema type? Recommend: yes, eventually. V1: fixed set of built-in schemas.

2. **Cross-pyramid configs**: Some configs (like provider registry, tier routing) are global, not per-pyramid. The generative config pattern should handle both scopes. Recommend: `config_id` is either a `slug` (per-pyramid) or `"global"` (system-wide).
