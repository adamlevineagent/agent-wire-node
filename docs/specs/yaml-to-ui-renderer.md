# YAML-to-UI Renderer Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Prerequisite for:** Generative config pattern, per-step model routing UI, evidence policy editor, DADBEAR oversight, creation UI
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

A generic React component that renders any YAML document as an editable configuration UI, driven by a schema annotation layer. The renderer is the single reusable piece that gives every configurable behavior in Wire Node an intent-to-YAML-to-UI-to-contribution surface. Build it once; every new YAML schema gets a configuration UI for free.

The renderer does NOT know what it's rendering. It receives a YAML value tree and a schema annotation, and produces a form. The schema annotation tells it how to present each field — widget type, visibility, constraints, help text, grouping.

---

## Architecture

```
┌─────────────────────┐     ┌──────────────────────┐
│  Schema Annotation   │     │    YAML Document      │
│  (per config type)   │     │  (runtime values)     │
│                      │     │                        │
│  field: model_tier   │     │  model_tier: mid       │
│    widget: select    │     │  temperature: 0.3      │
│    options_from:     │     │  concurrency: 10       │
│      tier_registry   │     │                        │
│    visibility: basic │     │                        │
└─────────┬───────────┘     └──────────┬─────────────┘
          │                            │
          ▼                            ▼
   ┌──────────────────────────────────────┐
   │         YamlConfigRenderer           │
   │  (generic React component)           │
   │                                      │
   │  - Composites schema + values        │
   │  - Renders appropriate widgets       │
   │  - Emits onChange(path, value)        │
   │  - Groups by visibility level        │
   │  - Shows per-field provider/cost     │
   └──────────────────────────────────────┘
```

### Data Flow

1. Rust backend loads the YAML document (chain definition, evidence policy, DADBEAR config, etc.)
2. Rust backend loads the matching schema annotation (looked up by schema_type)
3. Both are sent to the frontend via Tauri IPC as JSON
4. `YamlConfigRenderer` composites them into a form
5. User edits produce `onChange(path, value)` callbacks
6. Changed values are sent back to Rust via IPC as a partial YAML patch
7. Rust applies the patch and persists (as a contribution with supersession)

---

## Schema Annotation Model

Each configurable YAML type has a corresponding **schema annotation file** (`*.schema.yaml`) that lives alongside the config type definition. The annotation file does NOT duplicate the YAML structure — it describes how to render each field path.

### Schema Annotation Structure

```yaml
schema_type: chain_step_config
version: 1

# Annotations keyed by dotted field path (relative to the rendered scope)
fields:
  model_tier:
    label: "Model Tier"
    help: "Which compute tier to use for this step. Inherits from chain defaults if not set."
    widget: select
    options_from: tier_registry    # dynamic: populated from the provider registry at render time
    visibility: basic
    inherits_from: defaults.model_tier
    show_cost: true                # display estimated cost per call from the tier routing table

  temperature:
    label: "Temperature"
    help: "LLM sampling temperature. Lower = more deterministic, higher = more creative."
    widget: slider
    min: 0.0
    max: 1.0
    step: 0.05
    visibility: basic
    inherits_from: defaults.temperature

  concurrency:
    label: "Parallel Workers"
    help: "How many items to process simultaneously. Higher = faster but more resource-intensive."
    widget: number
    min: 1
    max: 50
    # Bounds derived from provider capabilities; overridable in schema annotations
    visibility: basic

  on_error:
    label: "Error Strategy"
    help: "What to do when a step fails."
    widget: select
    options:
      - value: "retry(1)"
        label: "Retry once"
      - value: "retry(2)"
        label: "Retry twice"
      - value: "retry(3)"
        label: "Retry three times"
      - value: "skip"
        label: "Skip and continue"
      - value: "abort"
        label: "Abort build"
    visibility: basic
    inherits_from: defaults.on_error

  max_input_tokens:
    label: "Max Input Size"
    help: "Token limit per LLM call. Larger inputs are split automatically."
    widget: number
    min: 1000
    max: 200000
    # Upper bound derived from model context window via provider registry auto-detection
    step: 1000
    suffix: "tokens"
    visibility: advanced

  batch_size:
    label: "Batch Size"
    help: "Number of items per LLM call in batched operations."
    widget: number
    min: 1
    max: 100
    # Upper bound scales with available context window
    visibility: advanced

  split_strategy:
    label: "Split Strategy"
    help: "How to split oversized inputs that exceed max_input_tokens."
    widget: select
    options:
      - value: "sections"
        label: "By sections"
      - value: "lines"
        label: "By lines"
      - value: "tokens"
        label: "By token count"
    visibility: advanced

  dehydrate:
    label: "Dehydration Rules"
    help: "Fields to drop from inputs when they exceed the token budget."
    widget: list
    item_widget: select
    item_options_from: node_fields    # dynamic: populated from the node schema
    visibility: advanced

  compact_inputs:
    label: "Compact Mode"
    help: "Strip whitespace from inputs to save tokens."
    widget: toggle
    visibility: advanced
```

### Field Annotation Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `label` | string | yes | Human-readable field name |
| `help` | string | yes | Tooltip/description explaining what this field does |
| `widget` | enum | yes | Widget type (see below) |
| `visibility` | enum | yes | `basic`, `advanced`, or `hidden` |
| `inherits_from` | string | no | Dotted path to the field this inherits from (shows "← tier default" in UI) |
| `show_cost` | bool | no | Whether to display cost-per-call estimate next to this field |
| `options` | array | conditional | Static options for `select` widgets |
| `options_from` | string | conditional | Dynamic option source for `select` widgets |
| `min`, `max`, `step` | number | conditional | Constraints for `number` and `slider` widgets |
| `suffix` | string | no | Unit label shown after the value (e.g., "tokens", "ms") |
| `item_widget` | string | no | Widget type for items in a `list` widget |
| `item_options_from` | string | no | Dynamic options for list item widgets |
| `group` | string | no | Named group for visual organization |
| `read_only` | bool | no | Show but don't allow editing |
| `condition` | string | no | Show this field only when condition is true (e.g., `"split_strategy != null"`) |

### Widget Types

| Widget | Renders As | Use When |
|--------|-----------|----------|
| `select` | Dropdown | Finite set of valid values |
| `text` | Text input | Free-form string |
| `number` | Number input with +/- | Integer or float with bounds |
| `slider` | Range slider | Number with continuous range (temperature) |
| `toggle` | On/off switch | Boolean |
| `list` | Add/remove item list | Array of values |
| `group` | Collapsible section | Nested object with sub-fields |
| `model_selector` | Provider + model picker | Specifically for model_tier fields, shows provider and model side by side with context window info |
| `code` | Monospace text area | YAML/JSON/prompt content |
| `readonly` | Static display | Non-editable informational fields |

### Dynamic Option Sources

Some `select` widgets need options that come from runtime state (not static YAML). The `options_from` property names a **data source** that the renderer resolves at mount time via IPC:

| Source | Resolves To | Used By |
|--------|------------|---------|
| `tier_registry` | Available tier names from the tier routing table | model_tier fields |
| `provider_list` | Registered providers (OpenRouter, Ollama, etc.) | provider selector |
| `model_list:{provider}` | Available models for a specific provider | model selector |
| `node_fields` | Top-level fields in the pyramid node schema | dehydration rules |
| `chain_list` | Loaded chain definitions | invoke_chain |
| `prompt_files` | Available prompt files in the prompts directory | instruction fields |

The renderer calls `invoke('yaml_renderer_resolve_options', { source: 'tier_registry' })` on mount and caches the result.

---

## Visibility Levels

Fields are organized into three visibility levels, rendered as collapsible sections:

1. **Basic** — Always visible. The fields most users will configure: model_tier, temperature, concurrency, error strategy. Typically 3-6 fields per step.

2. **Advanced** — Collapsed by default, expandable. Token limits, batch sizes, split strategies, dehydration rules. Power users who want fine-grained control.

3. **Hidden** — Not rendered. Internal fields the user shouldn't touch: primitive, instruction paths, node_id_pattern, response_schema, save_as, when conditions. The YAML controls these; the UI doesn't expose them.

The rendered UI shows steps grouped by pipeline phase, with each step showing its basic fields inline and advanced fields in an expandable section.

---

## Per-Step Override Pattern

Chain definitions have a `defaults` block and per-step values. The UI must clearly communicate inheritance:

```
  Tier Defaults
  ┌──────────────────────────────────────────────┐
  │  synth_heavy  →  OpenRouter / m2.7   [900k]  │
  │  extractor    →  OpenRouter / mercury-2       │
  │  web          →  OpenRouter / mercury-2       │
  └──────────────────────────────────────────────┘

  Step: source_extract
  ┌──────────────────────────────────────────────┐
  │  Model Tier:  extractor  ← tier default       │
  │  Temperature: 0.3        ← chain default      │
  │  Workers:     10                               │
  │  Error:       retry(3)   (override)            │
  └──────────────────────────────────────────────┘
```

When a step's field matches the chain default, show "← chain default" or "← tier default" as a muted label. When overridden, show "(override)" and allow clearing back to the default.

The renderer achieves this by:
1. Receiving both the `defaults` block and the step values
2. For each field with `inherits_from`, comparing the step value to the resolved default
3. Rendering the appropriate inheritance indicator

---

## Cost Estimation

For fields with `show_cost: true`, the renderer displays an estimated per-call cost. This requires:

1. The tier routing table (tier → provider + model)
2. A cost-per-token estimate for each provider+model pair (from provider registry metadata)
3. An average input/output token count for this step type (from historical data or defaults)

Display format: `$0.003 est.` next to the model tier field. When the user changes the tier/model, the cost updates live.

Cost data comes from the backend via IPC: `invoke('yaml_renderer_estimate_cost', { provider, model, avg_input_tokens, avg_output_tokens })`.

---

## Notes Paradigm Integration

The rendered UI supports the notes paradigm from the vision doc:

1. Every rendered form has an **Accept** and **Notes** action at the bottom
2. **Accept** saves the current YAML values as a contribution
3. **Notes** opens a text area where the user types feedback
4. On notes submission: the existing YAML + user notes are sent to the LLM, which produces a new version
5. The new version supersedes the previous, with the note attached as provenance
6. The UI re-renders with the new values
7. Version history is accessible (shows each version with its triggering note)

This is NOT a renderer concern — the renderer only provides the Accept/Notes buttons and emits events. The notes-to-new-version loop is orchestrated by the parent component.

### Renderer Contract

```typescript
interface YamlConfigRendererProps {
  schema: SchemaAnnotation;          // The schema annotation for this config type
  values: Record<string, unknown>;   // Current YAML values
  defaults?: Record<string, unknown>; // Chain/parent defaults for inheritance display
  onChange: (path: string, value: unknown) => void;  // Field-level change callback
  onAccept: () => void;              // User accepts current values
  onNotes: (note: string) => void;   // User provides refinement notes
  optionSources: Record<string, OptionValue[]>;  // Pre-resolved dynamic options
  costEstimates?: Record<string, number>;  // Pre-computed cost estimates per step
  readOnly?: boolean;                // View-only mode (for history inspection)
  versionInfo?: {                    // Version context for notes paradigm
    version: number;
    totalVersions: number;
    triggeringNote?: string;         // Note that produced this version
  };
}

interface SchemaAnnotation {
  schema_type: string;
  version: number;
  fields: Record<string, FieldAnnotation>;
}

interface FieldAnnotation {
  label: string;
  help: string;
  widget: WidgetType;
  visibility: 'basic' | 'advanced' | 'hidden';
  inherits_from?: string;
  show_cost?: boolean;
  options?: OptionValue[];
  options_from?: string;
  min?: number;
  max?: number;
  step?: number;
  suffix?: string;
  item_widget?: string;
  item_options_from?: string;
  group?: string;
  read_only?: boolean;
  condition?: string;
}

type WidgetType = 'select' | 'text' | 'number' | 'slider' | 'toggle' | 'list' | 'group' | 'model_selector' | 'code' | 'readonly';

interface OptionValue {
  value: string;
  label: string;
  description?: string;  // Shown as tooltip or secondary text
  meta?: Record<string, unknown>;  // Extra data (e.g., context_window for models)
}
```

---

## Chain Config as Creation UI

The "add workspace / generate pyramid" flow is driven by loaded chain YAMLs, not hardcoded content type options. The renderer participates in creation as follows:

1. Backend scans loaded chain definitions and returns a list: `[{ id, name, description, content_type }]`
2. The creation UI presents these as selectable pipeline options (replacing the hardcoded code/document/conversation/question picker)
3. When the user selects a pipeline, the renderer shows its configurable fields as a pre-build configuration form
4. The user adjusts model routing, concurrency, etc. before starting the build
5. Custom chains pulled from the Wire appear as creation options without UI changes
6. Folder ingestion mode becomes another option alongside the others

The creation UI delegates to `YamlConfigRenderer` with `readOnly=false` and the chain's schema annotation.

---

## Schema Annotations for Known Config Types

The initial set of config types that need schema annotations:

| Config Type | Schema File | First Consumer |
|------------|-------------|----------------|
| `chain_step_config` | `chain-step.schema.yaml` | Per-step model routing UI |
| `chain_defaults_config` | `chain-defaults.schema.yaml` | Tier defaults panel |
| `provider_config` | `provider.schema.yaml` | Provider registry settings |
| `tier_routing_config` | `tier-routing.schema.yaml` | Tier-to-model mapping |
| `evidence_policy` | `evidence-policy.schema.yaml` | Evidence triage editor |
| `dadbear_config` | `dadbear.schema.yaml` | DADBEAR oversight page |
| `build_strategy` | `build-strategy.schema.yaml` | Build config editor |

Each schema annotation file is a YAML document following the structure defined above. They live in `chains/schemas/` alongside the chain definitions.

---

## Renderer Implementation Scope

### Phase 1: Core Renderer
- `YamlConfigRenderer` component with basic/advanced/hidden visibility
- Widget implementations: select, text, number, slider, toggle, readonly
- Inheritance display (← default labels)
- onChange/onAccept/onNotes callbacks
- Static option support

### Phase 2: Dynamic Options + Cost
- Dynamic option resolution via IPC (`options_from`)
- `model_selector` composite widget (provider + model + context window)
- Cost estimation display
- Conditional field visibility (`condition` property)

### Phase 3: Advanced Widgets
- `list` widget (add/remove items, item_widget)
- `group` widget (collapsible nested sections)
- `code` widget (monospace editor for prompts/YAML)
- Version history navigation

### Phase 4: Creation UI Integration
- Chain list as pipeline selector
- Pre-build configuration form
- Folder ingestion as a pipeline option

---

## Backend Contract

The Rust backend needs these IPC commands:

```
pyramid_get_schema_annotation(schema_type: String) -> SchemaAnnotation
pyramid_get_config_values(schema_type: String, config_id: String) -> Value
pyramid_set_config_values(schema_type: String, config_id: String, patch: Value) -> ()
yaml_renderer_resolve_options(source: String) -> Vec<OptionValue>
yaml_renderer_estimate_cost(provider: String, model: String, avg_input_tokens: u64, avg_output_tokens: u64) -> f64
pyramid_accept_config(schema_type: String, config_id: String, yaml: Value) -> String  // returns contribution_id
pyramid_refine_config(schema_type: String, config_id: String, current_yaml: Value, note: String) -> { yaml: Value, version: u32 }
pyramid_config_versions(schema_type: String, config_id: String) -> Vec<VersionInfo>
```

---

## What This Spec Does NOT Cover

- **The generative config LLM prompts** — how intent-to-YAML generation works. That's a separate spec (generative config pattern).
- **Wire sharing mechanics** — how configs become contributions on the Wire. Uses existing contribution infrastructure.
- **Individual schema annotation files** — the actual field-by-field annotations for each config type. Written per-type when that config type is implemented.
- **CSS/styling** — follows existing Wire Node design system.

---

## Open Questions

1. **Schema annotation storage**: Should schema annotations be compiled into the Rust binary, loaded from disk at runtime, or fetched from the Wire? Disk (alongside chain YAMLs) is simplest and allows user customization.

2. **Validation**: Should the renderer enforce field constraints (min/max/required) or just display them? Recommend: renderer enforces and shows inline errors; backend validates on save as a safety net.

3. **Diff display**: When the notes paradigm produces a new version, should the renderer highlight what changed between versions? Useful but not required for v1.
