# Workstream: Phase 8 — YAML-to-UI Renderer

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7 are shipped. You are the implementer of Phase 8 — the first pure-frontend phase in the initiative. You are building `YamlConfigRenderer`, a generic React component that renders any YAML document as an editable configuration UI driven by a schema annotation layer.

Phase 8 is load-bearing for Phases 9, 10, 14 and effectively every user-facing configuration surface going forward. The renderer is built once; every new YAML schema gets a configuration UI for free.

## Context

The existing Wire Node frontend is React (Tauri + Vite) in `src/`. There's a `ToolsMode.tsx` (at `src/components/modes/ToolsMode.tsx`) with `My Tools`/`Discover`/`Create` tabs. Phases 9 and 10 will wire up the Create tab to the generative config loop (intent → YAML → renderer → notes → contribution); Phase 8 just ships the renderer primitive. Phase 4 introduced the `pyramid_config_contributions` table with `schema_annotation` as one of the 14 schema_types — Phase 8's renderer loads annotations from there at runtime, NOT from disk.

The spec's scope is organized into 4 internal phases (Core Renderer, Dynamic Options + Cost, Advanced Widgets, Creation UI Integration). Phase 8 of the initiative ships **Phase 1 + Phase 2** of the renderer scope — core widgets + dynamic options + cost estimation. Phase 3's advanced widgets (list, group, code) should land too if scope allows. Phase 4 (creation UI integration) is Phase 10's work.

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/yaml-to-ui-renderer.md` — read in full (439 lines).** This is your primary implementation contract. Particular attention to: Architecture (~line 19), Schema Annotation Model (~line 58), Widget Types (~line 184), Dynamic Option Sources (~line 199), Visibility Levels (~line 216), Per-Step Override Pattern (~line 230), Cost Estimation (~line 260), Renderer Contract (~line 288), Schema Annotations for Known Config Types (~line 361), Renderer Implementation Scope (~line 379), Backend Contract (~line 407).
3. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 8 section. Note the line: "Schema annotations loaded from `schema_annotation` template contributions (not disk)."
4. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 4 (contribution table + schema_annotation schema_type) and Phase 5 (prompt/chain migration pattern — Phase 8 will follow the same pattern for schema annotations).

### Code reading (targeted)

5. `src/components/modes/ToolsMode.tsx` — the existing tab shell. Phase 8 does NOT modify this file; Phase 10 does. But read it to understand how it dispatches to child components and how the IPC calls are shaped.
6. `src/components/AddWorkspace.tsx` — an existing form component. Read it to see the project's form patterns, styling conventions, and TypeScript types.
7. `src/main.tsx` + `src/App.tsx` — understand the app shell. Your new component mounts under the existing shell.
8. `src/styles/` — scan for CSS conventions (CSS modules? Tailwind? plain CSS? look at what's there).
9. `src/components/PyramidDetailDrawer.tsx` — another form-like component for reference patterns.
10. `src-tauri/src/pyramid/config_contributions.rs` — find `load_active_config_contribution` and the 14-entry `schema_type` match. Phase 8 extends the dispatcher's `schema_annotation` branch so that loading returns the YAML annotation body.
11. `src-tauri/src/pyramid/provider.rs` — Phase 3's `ProviderRegistry`. The `tier_registry` + `provider_list` + `model_list:{provider}` dynamic sources query this.
12. `src-tauri/src/pyramid/wire_migration.rs` — Phase 5's prompt/chain migration pattern. Phase 8 adds schema annotation migration following the same shape.
13. `src-tauri/src/main.rs` — find the `invoke_handler!` block. You'll register new IPC commands there.
14. `chains/defaults/` + `chains/prompts/` — see the directory structure Phase 5 walks. Phase 8 adds `chains/schemas/` with at least one schema annotation file.

## What to build

### 1. Backend: schema annotation storage + IPC

#### a. Schema annotation loader

Add IPC command to `main.rs`:

```
pyramid_get_schema_annotation(schema_type: String) -> SchemaAnnotation | null
```

Implementation:
1. Query `pyramid_config_contributions` for `schema_type = 'schema_annotation'` where the YAML body's `applies_to` field matches the requested config schema_type
2. Deserialize the YAML into a `SchemaAnnotation` struct (new type in `types.rs` or a new `yaml_renderer.rs` module)
3. Return to the frontend as JSON

The `schema_annotation` schema_type follows the spec's structure (schema_type, version, fields: HashMap<String, FieldAnnotation>). Each field annotation has label, help, widget, visibility, etc.

#### b. Dynamic option resolver

Add IPC command to `main.rs`:

```
yaml_renderer_resolve_options(source: String) -> Vec<OptionValue>
```

Sources to support (per the spec):
- `tier_registry` — query `pyramid_tier_routing` and return tier names with model/provider metadata
- `provider_list` — query `pyramid_providers` and return provider rows
- `model_list:{provider_id}` — for the specific provider, return available models (for OpenRouter this is from the tier_routing entries that reference it; for Ollama it would eventually query `/api/tags` but that's Phase 10 scope — use whatever's in tier_routing for now)
- `node_fields` — return a static list of pyramid node schema top-level field names (headline, distilled, topics, terms, decisions, dead_ends, etc.)
- `chain_list` — query `pyramid_chain_assignments` + chain_loader to return available chain definitions
- `prompt_files` — query `pyramid_config_contributions` for `schema_type = 'skill'` contributions and return their paths

Return a `Vec<OptionValue>` where each entry has `value`, `label`, optional `description`, optional `meta`.

#### c. Cost estimator

Add IPC command to `main.rs`:

```
yaml_renderer_estimate_cost(provider: String, model: String, avg_input_tokens: u64, avg_output_tokens: u64) -> f64
```

Implementation:
1. Query `pyramid_tier_routing` for the (provider, model) pair
2. Parse `pricing_json` to get prompt/completion per-token prices (Phase 3's format — strings, per-token)
3. Compute: `input_tokens * prompt_price + output_tokens * completion_price`
4. Return as f64 USD

If the pair isn't found, return `0.0` and log a warning (the UI can show "cost unavailable").

#### d. Schema annotation migration

Extend Phase 5's `wire_migration::migrate_prompts_and_chains_to_contributions` to also walk `chains/schemas/**/*.yaml` and create `schema_annotation` contributions for each. Same idempotency pattern as Phase 5 (sentinel marker + per-file check).

For Phase 8, ship **at least one** schema annotation file on disk: `chains/schemas/chain-step.schema.yaml`. Use the example from the spec (lines 64-162) as the content — it's a complete chain-step annotation that exercises most widget types.

### 2. Frontend: YamlConfigRenderer

Create `src/components/YamlConfigRenderer.tsx` with the component contract from the spec's "Renderer Contract" section (~line 288).

#### Component structure

```tsx
// src/components/YamlConfigRenderer.tsx
export interface YamlConfigRendererProps {
  schema: SchemaAnnotation;
  values: Record<string, unknown>;
  defaults?: Record<string, unknown>;
  onChange: (path: string, value: unknown) => void;
  onAccept: () => void;
  onNotes: (note: string) => void;
  optionSources: Record<string, OptionValue[]>;
  costEstimates?: Record<string, number>;
  readOnly?: boolean;
  versionInfo?: VersionInfo;
}

export function YamlConfigRenderer(props: YamlConfigRendererProps): JSX.Element
```

Structure:
- Iterate the schema's `fields` entries sorted by visibility (basic first, then advanced collapsed, hidden omitted)
- Group by `group` property if present
- Render each field via a widget dispatcher (big switch on `widget` type)
- Show inheritance indicators when `inherits_from` is set and the value matches the default
- Show cost estimate next to fields with `show_cost: true`
- At the bottom: Accept + Notes buttons (or a "Version X of Y" header + readOnly mode display)

#### Widget implementations (all in `src/components/yaml-renderer/widgets/`)

Create a subfolder `src/components/yaml-renderer/widgets/` with one file per widget:

- `SelectWidget.tsx` — dropdown with static or dynamic options
- `TextWidget.tsx` — text input
- `NumberWidget.tsx` — number input with min/max/step
- `SliderWidget.tsx` — range slider (temperature-style)
- `ToggleWidget.tsx` — on/off switch
- `ReadonlyWidget.tsx` — static display
- `ModelSelectorWidget.tsx` — composite provider + model picker with context window display (uses `tier_registry` option source)
- `ListWidget.tsx` — add/remove item list (Phase 3 scope per the spec; ship if time allows)
- `GroupWidget.tsx` — collapsible nested sections (Phase 3 scope)
- `CodeWidget.tsx` — monospace text area for YAML/prompt content (Phase 3 scope)

Each widget takes `{ value, onChange, disabled, annotation, optionSources }` as props and returns a focused React element. Keep the widgets small and dumb.

#### Shared types

Create `src/types/yamlRenderer.ts` with the TypeScript interfaces from the spec's "Renderer Contract" section. Use exact names: `SchemaAnnotation`, `FieldAnnotation`, `WidgetType`, `OptionValue`, `VersionInfo`.

#### Dynamic options + cost hook

Create `src/hooks/useYamlRendererSources.ts`:

```typescript
export function useYamlRendererSources(
  schema: SchemaAnnotation,
): { optionSources: Record<string, OptionValue[]>; costEstimates: Record<string, number>; loading: boolean }
```

On mount:
1. Walk `schema.fields` to collect unique `options_from` values (including `item_options_from`)
2. Call `invoke('yaml_renderer_resolve_options', { source })` for each unique source
3. If any field has `show_cost: true`, compute cost estimates by calling `invoke('yaml_renderer_estimate_cost', ...)` for the current tier routing
4. Return the populated maps

### 3. Schema annotation files on disk

Ship `chains/schemas/chain-step.schema.yaml` with the spec's example content. This is the seed that migration will populate into contributions on first run.

Optionally ship a second smaller schema to exercise the renderer's handling of simpler configs (e.g., `dadbear.schema.yaml` with 4-5 fields).

### 4. Tests

#### Rust side
- `test_load_schema_annotation_from_contribution` — create a schema_annotation contribution, call `pyramid_get_schema_annotation`, verify it deserializes
- `test_resolve_options_tier_registry` — seed tier routing, call the resolver, verify options shape
- `test_estimate_cost_from_tier` — seed a tier with known pricing, verify the cost calculation
- `test_schema_annotation_migration_idempotent` — run twice, verify no duplicate rows

#### Frontend side
- If the project has a test runner set up (check `package.json`), add component tests for the renderer:
  - `YamlConfigRenderer` renders basic fields from a static schema
  - Widget dispatcher picks the right widget per annotation
  - onChange callback receives (path, value) tuples
  - Hidden fields are not rendered
  - Advanced fields are collapsed by default
  - Inheritance indicator shows when value === default
- If there's no test runner, document the manual verification steps in the implementation log instead

Check `package.json` for test infrastructure (Vitest, Jest, etc.). If present, use it. If not, skip frontend tests and rely on cargo check + manual verification.

## Scope boundaries

**In scope:**
- `pyramid_get_schema_annotation` IPC + backend query
- `yaml_renderer_resolve_options` IPC + dynamic source resolvers
- `yaml_renderer_estimate_cost` IPC + pricing calculation
- `wire_migration.rs` extension for schema annotations
- `chains/schemas/chain-step.schema.yaml` seed file (at minimum)
- `YamlConfigRenderer.tsx` React component
- Widget implementations for: select, text, number, slider, toggle, readonly, model_selector
- `src/types/yamlRenderer.ts` TypeScript type definitions
- `src/hooks/useYamlRendererSources.ts` hook
- Phase 3 advanced widgets (list, group, code) if scope allows
- Rust tests for backend; frontend tests if infra exists
- Implementation log entry

**Out of scope:**
- ToolsMode.tsx integration (Phase 10)
- Creation UI pipeline picker (Phase 10)
- Version history UI (Phase 13)
- Generative config LLM prompts (Phase 9)
- Refinement → LLM round trip (Phase 9)
- Per-schema-type annotation files beyond chain-step (add as needed for spot tests, but the full set is Phase 10)
- Ollama `/api/tags` live query for model_list (Phase 10)
- CSS/styling beyond matching existing conventions
- The 7 pre-existing unrelated test failures

## Verification criteria

1. **Rust:** `cargo check --lib`, `cargo build --lib` — clean, zero new warnings. `cargo test --lib pyramid` — 992 passing (Phase 7 count) + new Phase 8 tests. Same 7 pre-existing failures.
2. **Frontend:** `pnpm tsc` or `npm run build` (or whatever the project uses — check `package.json` scripts) — clean, no new TypeScript errors.
3. **Frontend lint:** If `eslint` or `biome` is in the project, run it on the new files — clean.
4. **Schema annotation on disk:** `chains/schemas/chain-step.schema.yaml` exists and is valid YAML.
5. **Migration test:** On fresh init, the schema annotation lands in `pyramid_config_contributions` with `schema_type = 'schema_annotation'`.
6. **IPC smoke test:** Add a manual verification step in the implementation log — describe how to invoke `pyramid_get_schema_annotation("chain_step_config")` from a dev harness and see the annotation returned.
7. **Widget dispatch:** Document in the log which widgets are implemented and which are deferred.

## Deviation protocol

Standard. Most likely deviations:
- **No frontend test runner.** If there's no Vitest/Jest/Playwright in `package.json`, skip frontend unit tests and document in the log. Rust tests still required.
- **`schema_annotation` body format divergence.** The spec shows a particular YAML shape. If the schema_annotation field layout in Phase 4's contribution table differs (e.g., the YAML body wrapper vs a direct annotation), match whatever Phase 4 expects and flag the spec for correction.
- **Advanced widgets missing** — if scope pressure forces you to defer `list`/`group`/`code`, document which are deferred and under what conditions they'd be added.

## Implementation log protocol

Append Phase 8 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the backend IPC commands, frontend component structure, widgets implemented vs deferred, schema annotation files shipped, tests, verification results, and any manual verification steps. Status: `awaiting-verification`.

## Mandate

- **Correct before fast.** The schema annotation contract is consumed by Phase 9, 10, 14. Get the type shapes right now to avoid ripple effects.
- **Use Phase 4's contribution storage for schema annotations.** Do NOT read schema annotations directly from disk at runtime — load them from `pyramid_config_contributions` via the `schema_annotation` schema_type. Disk files are seed data migrated into contributions on first run, same as Phase 5's prompt pattern.
- **No new hardcoded LLM-constraining numbers.** Widget defaults (min/max/step) are UI concerns, not LLM constraints — they're fine as long as they're schema-annotation-driven.
- **Match existing frontend conventions.** Look at neighboring components for CSS patterns, TypeScript style, and Tauri invoke patterns. Do NOT introduce a new styling system or framework.
- **Scope-aware tests.** Frontend tests if infra exists, Rust tests always. Document which you ran.
- **Fix all bugs found.** Standard.
- **Commit when done.** Single commit with message `phase-8: yaml-to-ui renderer`. Body: 5-7 lines summarizing backend IPC + frontend component + widgets + schema annotation seed + tests. Do not amend. Do not push.

## End state

Phase 8 is complete when:

1. `pyramid_get_schema_annotation` + `yaml_renderer_resolve_options` + `yaml_renderer_estimate_cost` IPC commands registered in `main.rs`.
2. Backend queries load schema annotations from `pyramid_config_contributions` (Phase 4 path), not disk.
3. Schema annotation migration added to `wire_migration.rs` (idempotent).
4. `chains/schemas/chain-step.schema.yaml` exists with the spec's example content.
5. `src/components/YamlConfigRenderer.tsx` exists and implements the Renderer Contract from the spec.
6. Widget files exist in `src/components/yaml-renderer/widgets/` for at least: select, text, number, slider, toggle, readonly, model_selector.
7. `src/types/yamlRenderer.ts` defines the TypeScript contract types.
8. `src/hooks/useYamlRendererSources.ts` hook handles dynamic option + cost resolution.
9. `cargo check`, `cargo build`, `cargo test --lib pyramid` pass with 992+ passing and same 7 pre-existing failures.
10. Frontend build (`npm run build` or equivalent) is clean.
11. Implementation log Phase 8 entry complete.
12. Single commit on branch `phase-8-yaml-to-ui-renderer`.

Begin with the spec (read in full) + neighboring frontend components for style reference. Then the code.

Good luck. Build carefully.
