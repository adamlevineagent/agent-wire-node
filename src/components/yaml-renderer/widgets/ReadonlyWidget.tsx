// ReadonlyWidget — static display. The renderer uses this both for
// annotation-level `widget: readonly` fields and for the fallback
// when `readOnly` mode is enabled at the top level.
//
// Objects/arrays render as compact JSON; scalars render as their
// stringified form with an "(unset)" placeholder for nullish values.

import type { WidgetProps } from "./WidgetTypes";

export function ReadonlyWidget({ value, annotation }: WidgetProps) {
  return (
    <div
      style={{
        padding: "6px 10px",
        background: "var(--bg-card)",
        color: "var(--text-secondary)",
        border: "1px dashed var(--glass-border)",
        borderRadius: "var(--radius-sm)",
        fontSize: 13,
        fontFamily:
          typeof value === "object" && value != null
            ? "var(--font-mono, monospace)"
            : undefined,
        whiteSpace: "pre-wrap",
        wordBreak: "break-word",
      }}
    >
      {formatValue(value)}
      {annotation.suffix ? ` ${annotation.suffix}` : ""}
    </div>
  );
}

function formatValue(value: unknown): string {
  if (value == null) return "(unset)";
  if (typeof value === "string") return value || "(empty)";
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
