// SelectWidget — dropdown with static or dynamic options.
//
// Resolves the option list from either `annotation.options` (static)
// or `optionSources[annotation.options_from]` (dynamic). If both are
// unset the widget renders a disabled empty select with a small
// "no options available" helper so the user knows something is
// missing vs. the widget being intentionally blank.

import type { WidgetProps } from "./WidgetTypes";
import type { OptionValue } from "../../../types/yamlRenderer";

export function SelectWidget({
  value,
  onChange,
  disabled,
  annotation,
  optionSources,
}: WidgetProps) {
  const options: OptionValue[] = resolveOptions(annotation, optionSources);
  const current = value == null ? "" : String(value);
  const hasOptions = options.length > 0;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <select
        className="input"
        value={current}
        disabled={disabled || !hasOptions}
        onChange={(e) => onChange(e.target.value)}
        style={{
          padding: "6px 10px",
          background: "var(--bg-card)",
          color: "var(--text-primary)",
          border: "1px solid var(--glass-border)",
          borderRadius: "var(--radius-sm)",
          fontSize: 13,
          width: "100%",
        }}
      >
        {!current && (
          <option value="" disabled>
            {hasOptions ? "Select an option…" : "No options available"}
          </option>
        )}
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
            {opt.description ? ` — ${opt.description}` : ""}
          </option>
        ))}
      </select>
    </div>
  );
}

function resolveOptions(
  annotation: WidgetProps["annotation"],
  optionSources: WidgetProps["optionSources"],
): OptionValue[] {
  if (annotation.options && annotation.options.length > 0) {
    return annotation.options;
  }
  if (annotation.options_from) {
    return optionSources[annotation.options_from] ?? [];
  }
  return [];
}
