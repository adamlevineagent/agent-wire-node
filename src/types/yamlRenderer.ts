// src/types/yamlRenderer.ts — Phase 8: YAML-to-UI renderer contract.
//
// Mirrors the Rust types in `src-tauri/src/pyramid/yaml_renderer.rs`
// 1:1 so `invoke('pyramid_get_schema_annotation', ...)` and
// `invoke('yaml_renderer_resolve_options', ...)` can deserialize
// directly into these interfaces without runtime conversion.
//
// Source of truth: `docs/specs/yaml-to-ui-renderer.md` → Renderer
// Contract section (~line 288).

/**
 * Widget type enumeration. The renderer dispatches on this string to
 * pick a concrete widget component. Phase 8 ships the core set plus
 * the Phase 3 advanced widgets (list, group, code).
 */
export type WidgetType =
  | "select"
  | "text"
  | "number"
  | "slider"
  | "toggle"
  | "list"
  | "group"
  | "model_selector"
  | "code"
  | "readonly";

/**
 * Visibility level for a field. `hidden` fields are never rendered;
 * `advanced` fields live inside a collapsed section by default;
 * `basic` fields render inline.
 */
export type FieldVisibility = "basic" | "advanced" | "hidden";

/**
 * A single option entry for a `select` or `model_selector` widget.
 * Static options come from the annotation file; dynamic options come
 * from `yaml_renderer_resolve_options`.
 */
export interface OptionValue {
  /** The value to write to the YAML when this option is selected. */
  value: string;
  /** The human-readable label shown in the dropdown. */
  label: string;
  /**
   * Optional secondary text (rendered as a subtitle or tooltip).
   * For tier registry entries this is typically the model id + provider.
   */
  description?: string;
  /**
   * Optional extra data. For tier_registry options this carries
   * `provider_id`, `model_id`, `context_limit`, `max_completion_tokens`,
   * and the per-token pricing — the `model_selector` widget reads
   * these to render rich blurbs without a second IPC round trip.
   */
  meta?: Record<string, unknown>;
}

/**
 * Per-field annotation — mirrors `FieldAnnotation` in Rust.
 * Everything except `label`, `help`, `widget`, and `visibility` is
 * optional; unset fields are omitted from the JSON envelope.
 */
export interface FieldAnnotation {
  /** Human-readable field name shown above the widget. */
  label: string;
  /** Tooltip/description explaining what this field does. */
  help: string;
  /** Widget type name. See `WidgetType`. */
  widget: WidgetType;
  /** One of `basic`, `advanced`, `hidden`. */
  visibility: FieldVisibility;
  /**
   * Dotted path to the field this inherits from. When set, the
   * renderer shows "← default" when the current value equals the
   * resolved default.
   */
  inherits_from?: string;
  /**
   * Whether to display an estimated cost-per-call next to this field.
   * Used for model_tier fields that have pricing data in the
   * tier routing table.
   */
  show_cost?: boolean;
  /** Static options for `select` widgets. Mutually exclusive with `options_from`. */
  options?: OptionValue[];
  /**
   * Dynamic option source name. Resolved at mount time via
   * `yaml_renderer_resolve_options`. See the Phase 8 spec for the
   * set of supported sources.
   */
  options_from?: string;
  /** Minimum value for `number`/`slider` widgets. */
  min?: number;
  /** Maximum value for `number`/`slider` widgets. */
  max?: number;
  /** Step size for `number`/`slider` widgets. */
  step?: number;
  /** Unit label shown after the value (e.g. "tokens", "ms"). */
  suffix?: string;
  /** Widget type for items in a `list` widget. */
  item_widget?: string;
  /** Dynamic options source for list item widgets. */
  item_options_from?: string;
  /** Named group for visual organization. */
  group?: string;
  /** Show-but-don't-allow-editing flag. */
  read_only?: boolean;
  /**
   * Conditional visibility expression (e.g. `"split_strategy != null"`).
   * Phase 8 ships the type but the renderer does not yet evaluate
   * conditions — deferred to Phase 10.
   */
  condition?: string;
  /**
   * Explicit display order within a group. Lower numbers render first.
   * Breaks ties from the annotation file's natural key order.
   */
  order?: number;
}

/**
 * The top-level schema annotation document — mirrors
 * `SchemaAnnotation` in Rust.
 *
 * `schema_type` is the annotation's own identifier. `applies_to` is
 * the target config schema_type (e.g. `chain_step_config`). Simple
 * annotation files omit `applies_to` and let it default to
 * `schema_type`.
 */
export interface SchemaAnnotation {
  schema_type: string;
  version: number;
  applies_to?: string;
  label?: string;
  description?: string;
  /**
   * Field-level annotations keyed by dotted field path. The renderer
   * iterates this map, sorts by (group, order, key), and dispatches
   * each field to the appropriate widget.
   */
  fields: Record<string, FieldAnnotation>;
}

/**
 * Version metadata for the notes paradigm. When a config has a
 * multi-version history, the renderer can display "Version 2 of 5"
 * along with the triggering note that produced the current version.
 * Phase 8 ships the type; Phase 13 wires the navigation UI.
 */
export interface VersionInfo {
  version: number;
  totalVersions: number;
  /** The note that produced this version (provenance). */
  triggeringNote?: string;
}

/**
 * Props passed to the generic `YamlConfigRenderer` component.
 * Matches the spec's Renderer Contract section.
 */
export interface YamlConfigRendererProps {
  /** The schema annotation for this config type. */
  schema: SchemaAnnotation;
  /** Current YAML values as a (nested) plain object. */
  values: Record<string, unknown>;
  /** Chain/parent defaults for inheritance display. */
  defaults?: Record<string, unknown>;
  /** Field-level change callback. The renderer emits (path, value) tuples. */
  onChange: (path: string, value: unknown) => void;
  /** User accepts current values. */
  onAccept: () => void;
  /** User provides refinement notes (triggers the generative config round trip in Phase 9). */
  onNotes: (note: string) => void;
  /**
   * Pre-resolved dynamic options, keyed by `options_from` name.
   * Produced by the `useYamlRendererSources` hook. The renderer
   * does NOT call `invoke` directly — option resolution happens
   * one level up so the parent can cache per-schema.
   */
  optionSources: Record<string, OptionValue[]>;
  /** Pre-computed cost estimates (path → USD per call). */
  costEstimates?: Record<string, number>;
  /** View-only mode (for history inspection). */
  readOnly?: boolean;
  /**
   * When `readOnly=true`, the renderer shows a prominent "Read-only
   * preview — Edit to refine" banner. If `onRefine` is provided, the
   * banner includes an inline Edit button that calls it. Without
   * `onRefine`, the banner still renders but the button is hidden.
   *
   * The refine flow itself (generative config loop) lives upstream —
   * this prop is just the entry point.
   */
  onRefine?: () => void;
  /** Version context for the notes paradigm. */
  versionInfo?: VersionInfo;
}
