// YamlConfigRenderer — Phase 8: the generic YAML-to-UI primitive.
//
// Given a `SchemaAnnotation` and a `values` tree, dispatches each
// field annotation to the appropriate widget, groups them by
// visibility / named group, and exposes Accept + Notes actions
// at the bottom. Widget dispatch is a dumb switch on
// `annotation.widget` — individual widgets live in
// `./yaml-renderer/widgets/` and share the `WidgetProps` shape.
//
// Source of truth: docs/specs/yaml-to-ui-renderer.md (Renderer
// Contract section). The props contract here matches 1:1 so the
// Phase 9 generative config loop + Phase 10 ToolsMode creation UI
// can bind against it without any adapter shim.
//
// The renderer is deliberately display-only: it does NOT call
// `invoke()` itself and does NOT manage state. All state lives in
// the parent — the renderer just emits `onChange(path, value)`
// tuples, and the parent decides whether to mutate local state,
// round-trip through the LLM for a refined version, or persist a
// contribution. This separation lets the same component power
// creation, editing, history inspection, and notes refinement
// without branching.

import { useMemo, useState } from "react";
import type {
  FieldAnnotation,
  FieldVisibility,
  YamlConfigRendererProps,
} from "../types/yamlRenderer";
import {
  CodeWidget,
  GroupWidget,
  ListWidget,
  ModelSelectorWidget,
  NumberWidget,
  ReadonlyWidget,
  SelectWidget,
  SliderWidget,
  TextWidget,
  ToggleWidget,
  type WidgetProps,
} from "./yaml-renderer/widgets";

// ── Helpers ─────────────────────────────────────────────────────────────────

/**
 * Read a dotted path (`"defaults.temperature"`) from a nested object.
 * Returns `undefined` for any missing segment rather than throwing.
 */
function readPath(root: Record<string, unknown> | undefined, path: string): unknown {
  if (!root) return undefined;
  const parts = path.split(".");
  let current: unknown = root;
  for (const part of parts) {
    if (current == null || typeof current !== "object") return undefined;
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

/**
 * Loose equality used for the "inherits from default" indicator.
 * Scalars compare by value; arrays/objects compare by JSON.
 *
 * Note: the indicator caller must also guard against the "both
 * undefined" case — `valuesEqual(undefined, undefined)` returns
 * `true` here (intentionally, for scalar semantics), but the
 * caller should NOT render "← default" when neither a current
 * value nor a resolvable default actually exists. See
 * `shouldShowInheritanceIndicator` below.
 */
function valuesEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a == null || b == null) return a == null && b == null;
  if (typeof a === "object" || typeof b === "object") {
    try {
      return JSON.stringify(a) === JSON.stringify(b);
    } catch {
      return false;
    }
  }
  return false;
}

/**
 * Guard the "← default" label against the false-positive case
 * where both the current value and the resolved default are
 * `undefined`. `valuesEqual(undefined, undefined) === true` but
 * showing "← default" when no default exists is misleading: it
 * implies the field is correctly inheriting when in fact nothing
 * is being inherited. The indicator only renders when:
 *   1. `inherits_from` is declared on the annotation, AND
 *   2. the resolved default is defined (something to inherit from), AND
 *   3. the current value equals the resolved default.
 */
function shouldShowInheritanceIndicator(
  annotation: FieldAnnotation,
  value: unknown,
  resolvedDefault: unknown,
): boolean {
  if (annotation.inherits_from == null) return false;
  if (resolvedDefault === undefined) return false;
  return valuesEqual(value, resolvedDefault);
}

/**
 * Collect entries from the annotation `fields` map and sort them by
 * group (stable order of first appearance) → `order` → key. Fields
 * with `visibility: hidden` are dropped entirely.
 */
interface SortedField {
  path: string;
  annotation: FieldAnnotation;
}

function sortedFields(
  fields: Record<string, FieldAnnotation>,
): SortedField[] {
  const entries = Object.entries(fields).filter(
    ([, ann]) => ann.visibility !== "hidden",
  );
  // Preserve map order but apply a stable `order` sort within it.
  entries.sort((a, b) => {
    const ao = a[1].order ?? Number.MAX_SAFE_INTEGER;
    const bo = b[1].order ?? Number.MAX_SAFE_INTEGER;
    if (ao !== bo) return ao - bo;
    return a[0].localeCompare(b[0]);
  });
  return entries.map(([path, annotation]) => ({ path, annotation }));
}

/**
 * Group fields into visibility buckets. Basic fields are always
 * visible; advanced live in a collapsible section; hidden are
 * dropped (already filtered above).
 */
function splitByVisibility(
  entries: SortedField[],
): Record<FieldVisibility, SortedField[]> {
  const out: Record<FieldVisibility, SortedField[]> = {
    basic: [],
    advanced: [],
    hidden: [],
  };
  for (const entry of entries) {
    out[entry.annotation.visibility].push(entry);
  }
  return out;
}

/**
 * Further group fields within a bucket by their `group` property.
 * Returns a list of groups with `null` meaning "no group" (rendered
 * inline without a header).
 */
interface FieldGroup {
  name: string | null;
  entries: SortedField[];
}

function groupByGroup(entries: SortedField[]): FieldGroup[] {
  const out: FieldGroup[] = [];
  const index: Map<string | null, FieldGroup> = new Map();
  for (const entry of entries) {
    const key = entry.annotation.group ?? null;
    let group = index.get(key);
    if (!group) {
      group = { name: key, entries: [] };
      index.set(key, group);
      out.push(group);
    }
    group.entries.push(entry);
  }
  return out;
}

// ── Widget dispatch ─────────────────────────────────────────────────────────

/**
 * Pick a widget component from an annotation's `widget` string.
 * Unknown widget names fall back to `ReadonlyWidget` so the user
 * still sees the raw value rather than a broken render.
 */
function pickWidget(
  widget: string,
): (props: WidgetProps) => JSX.Element {
  switch (widget) {
    case "select":
      return SelectWidget;
    case "text":
      return TextWidget;
    case "number":
      return NumberWidget;
    case "slider":
      return SliderWidget;
    case "toggle":
      return ToggleWidget;
    case "readonly":
      return ReadonlyWidget;
    case "model_selector":
      return ModelSelectorWidget;
    case "list":
      return ListWidget;
    case "group":
      return GroupWidget;
    case "code":
      return CodeWidget;
    default:
      return ReadonlyWidget;
  }
}

// ── Subcomponents ───────────────────────────────────────────────────────────

interface FieldRowProps {
  path: string;
  annotation: FieldAnnotation;
  value: unknown;
  resolvedDefault: unknown;
  onChange: (path: string, value: unknown) => void;
  readOnly: boolean;
  optionSources: Record<string, unknown>;
  costEstimate?: number;
}

function FieldRow({
  path,
  annotation,
  value,
  resolvedDefault,
  onChange,
  readOnly,
  optionSources,
  costEstimate,
}: FieldRowProps) {
  const Widget = pickWidget(annotation.widget);
  const disabled =
    readOnly || annotation.read_only === true || annotation.widget === "readonly";
  const inheritsFromDefault = shouldShowInheritanceIndicator(
    annotation,
    value,
    resolvedDefault,
  );

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 4,
        padding: "8px 0",
        // Visual cue that this row is read-only / inspection-only.
        // Native `disabled` styling on form elements is too subtle on
        // the dark theme — tinting the whole row makes the state
        // unmistakable.
        opacity: disabled ? 0.55 : 1,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          gap: 8,
          flexWrap: "wrap",
        }}
      >
        <label
          style={{
            fontSize: 12,
            fontWeight: 600,
            color: "var(--text-primary)",
            letterSpacing: "0.01em",
          }}
        >
          {annotation.label}
        </label>
        {annotation.show_cost && costEstimate != null && (
          <span
            style={{
              fontSize: 11,
              padding: "1px 6px",
              borderRadius: "var(--radius-sm)",
              background: "rgba(34, 211, 238, 0.1)",
              color: "var(--accent-cyan)",
              fontFamily: "var(--font-mono, monospace)",
            }}
            title="Estimated USD per call"
          >
            ${costEstimate.toFixed(4)} est.
          </span>
        )}
        {inheritsFromDefault && (
          <span
            style={{
              fontSize: 11,
              color: "var(--text-secondary)",
              fontStyle: "italic",
            }}
          >
            ← {annotation.inherits_from} default
          </span>
        )}
      </div>
      <Widget
        value={value}
        onChange={(next) => onChange(path, next)}
        disabled={disabled}
        annotation={annotation}
        optionSources={
          optionSources as Record<
            string,
            import("../types/yamlRenderer").OptionValue[]
          >
        }
        costEstimate={costEstimate}
      />
      <div
        style={{
          fontSize: 11,
          color: "var(--text-secondary)",
          lineHeight: 1.4,
        }}
      >
        {annotation.help}
      </div>
    </div>
  );
}

interface FieldGroupSectionProps {
  group: FieldGroup;
  values: Record<string, unknown>;
  defaults: Record<string, unknown> | undefined;
  onChange: (path: string, value: unknown) => void;
  readOnly: boolean;
  optionSources: Record<string, unknown>;
  costEstimates: Record<string, number>;
}

function FieldGroupSection({
  group,
  values,
  defaults,
  onChange,
  readOnly,
  optionSources,
  costEstimates,
}: FieldGroupSectionProps) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 0,
        padding: group.name ? "4px 0" : 0,
      }}
    >
      {group.name && (
        <div
          style={{
            fontSize: 11,
            fontWeight: 600,
            textTransform: "uppercase",
            letterSpacing: "0.08em",
            color: "var(--text-secondary)",
            marginBottom: 4,
            paddingTop: 8,
            borderTop: "1px solid var(--glass-border)",
          }}
        >
          {group.name}
        </div>
      )}
      {group.entries.map(({ path, annotation }) => (
        <FieldRow
          key={path}
          path={path}
          annotation={annotation}
          value={readPath(values, path)}
          resolvedDefault={
            annotation.inherits_from
              ? readPath(defaults, annotation.inherits_from)
              : undefined
          }
          onChange={onChange}
          readOnly={readOnly}
          optionSources={optionSources}
          costEstimate={costEstimates[path]}
        />
      ))}
    </div>
  );
}

// ── YamlConfigRenderer ──────────────────────────────────────────────────────

export function YamlConfigRenderer({
  schema,
  values,
  defaults,
  onChange,
  onAccept,
  onNotes,
  optionSources,
  costEstimates = {},
  readOnly = false,
  onRefine,
  versionInfo,
}: YamlConfigRendererProps) {
  const [notesOpen, setNotesOpen] = useState(false);
  const [notesText, setNotesText] = useState("");
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const { basicGroups, advancedGroups, hasAdvanced } = useMemo(() => {
    const sorted = sortedFields(schema.fields);
    const byVisibility = splitByVisibility(sorted);
    return {
      basicGroups: groupByGroup(byVisibility.basic),
      advancedGroups: groupByGroup(byVisibility.advanced),
      hasAdvanced: byVisibility.advanced.length > 0,
    };
  }, [schema]);

  const headerTitle = schema.label ?? schema.schema_type;
  const headerDescription = schema.description;

  return (
    <div
      className="yaml-config-renderer"
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 12,
        padding: 16,
        background: "var(--bg-secondary, #151528)",
        border: "1px solid var(--glass-border)",
        borderRadius: 8,
      }}
    >
      {/* Header */}
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 4,
          paddingBottom: 8,
          borderBottom: "1px solid var(--glass-border)",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "baseline",
            gap: 10,
            flexWrap: "wrap",
          }}
        >
          <h3
            style={{
              margin: 0,
              fontSize: 15,
              fontWeight: 600,
              color: "var(--text-primary)",
            }}
          >
            {headerTitle}
          </h3>
          <span
            style={{
              fontSize: 11,
              color: "var(--text-secondary)",
              fontFamily: "var(--font-mono, monospace)",
            }}
          >
            v{schema.version}
          </span>
          {versionInfo && (
            <span
              style={{
                fontSize: 11,
                color: "var(--text-secondary)",
              }}
            >
              Version {versionInfo.version} of {versionInfo.totalVersions}
            </span>
          )}
        </div>
        {headerDescription && (
          <div
            style={{
              fontSize: 12,
              color: "var(--text-secondary)",
              lineHeight: 1.5,
            }}
          >
            {headerDescription}
          </div>
        )}
        {versionInfo?.triggeringNote && (
          <div
            style={{
              fontSize: 11,
              padding: "4px 8px",
              borderRadius: "var(--radius-sm)",
              background: "rgba(167, 139, 250, 0.08)",
              color: "var(--accent-purple)",
              fontStyle: "italic",
            }}
          >
            Note that produced this version: "{versionInfo.triggeringNote}"
          </div>
        )}
      </div>

      {/* Read-only preview banner — widgets below are disabled.
          Surfaces the refine entry point so the user doesn't have
          to scroll to the drawer footer to find it. */}
      {readOnly && (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 10,
            padding: "8px 12px",
            borderRadius: "var(--radius-sm)",
            background: "rgba(251, 191, 36, 0.08)",
            border: "1px solid rgba(251, 191, 36, 0.25)",
            color: "var(--text-primary)",
            fontSize: 12,
          }}
        >
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              gap: 2,
            }}
          >
            <div style={{ fontWeight: 600, color: "#fbbf24" }}>
              Read-only preview
            </div>
            <div
              style={{
                color: "var(--text-secondary)",
                fontSize: 11,
                lineHeight: 1.4,
              }}
            >
              Every change is a new version with a note. Click Edit to
              refine this config via the generative flow — direct field
              edits are intentionally disabled.
            </div>
          </div>
          {onRefine && (
            <button
              type="button"
              className="btn btn-primary btn-small"
              onClick={onRefine}
              style={{ flexShrink: 0 }}
            >
              Edit
            </button>
          )}
        </div>
      )}

      {/* Basic fields */}
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 0,
        }}
      >
        {basicGroups.length === 0 && (
          <div
            style={{
              fontSize: 12,
              color: "var(--text-secondary)",
              fontStyle: "italic",
              padding: "12px 0",
            }}
          >
            No basic fields defined in this schema.
          </div>
        )}
        {basicGroups.map((group, index) => (
          <FieldGroupSection
            key={group.name ?? `__basic_${index}__`}
            group={group}
            values={values}
            defaults={defaults}
            onChange={onChange}
            readOnly={readOnly}
            optionSources={optionSources}
            costEstimates={costEstimates}
          />
        ))}
      </div>

      {/* Advanced section (collapsible) */}
      {hasAdvanced && (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 0,
            marginTop: 4,
          }}
        >
          <button
            type="button"
            className="btn btn-ghost btn-small"
            onClick={() => setAdvancedOpen(!advancedOpen)}
            style={{
              justifyContent: "flex-start",
              padding: "6px 8px",
              fontSize: 12,
            }}
          >
            {advancedOpen ? "▼" : "▶"} Advanced
          </button>
          {advancedOpen && (
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 0,
                paddingLeft: 8,
                borderLeft: "2px solid var(--glass-border)",
                marginLeft: 4,
              }}
            >
              {advancedGroups.map((group, index) => (
                <FieldGroupSection
                  key={group.name ?? `__advanced_${index}__`}
                  group={group}
                  values={values}
                  defaults={defaults}
                  onChange={onChange}
                  readOnly={readOnly}
                  optionSources={optionSources}
                  costEstimates={costEstimates}
                />
              ))}
            </div>
          )}
        </div>
      )}

      {/* Action bar (hidden in readOnly mode) */}
      {!readOnly && (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 8,
            paddingTop: 8,
            borderTop: "1px solid var(--glass-border)",
          }}
        >
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={onAccept}
              title="Save these values as a new contribution version"
            >
              Accept
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => setNotesOpen(!notesOpen)}
              title="Refine this config by giving the LLM notes"
            >
              {notesOpen ? "Cancel Notes" : "Notes"}
            </button>
          </div>
          {notesOpen && (
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 6,
              }}
            >
              <textarea
                value={notesText}
                onChange={(e) => setNotesText(e.target.value)}
                placeholder={
                  "Describe what you'd like to change (e.g. 'Use cheaper model for source_extract, bump batch size for merges')."
                }
                rows={4}
                style={{
                  padding: "8px 10px",
                  background: "var(--bg-card)",
                  color: "var(--text-primary)",
                  border: "1px solid var(--glass-border)",
                  borderRadius: "var(--radius-sm)",
                  fontSize: 13,
                  lineHeight: 1.5,
                  resize: "vertical",
                  minHeight: 80,
                }}
              />
              <div style={{ display: "flex", gap: 8 }}>
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={notesText.trim().length === 0}
                  onClick={() => {
                    const trimmed = notesText.trim();
                    if (trimmed.length === 0) return;
                    onNotes(trimmed);
                    setNotesText("");
                    setNotesOpen(false);
                  }}
                >
                  Submit Notes
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
