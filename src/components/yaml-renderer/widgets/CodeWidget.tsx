// CodeWidget — monospace textarea for YAML / prompt / JSON content.
// Phase 8 ships a plain textarea; syntax highlighting / line numbers
// are Phase 10 or later (requires a real editor dependency).

import type { WidgetProps } from "./WidgetTypes";

export function CodeWidget({
  value,
  onChange,
  disabled,
  annotation,
}: WidgetProps) {
  const current = value == null ? "" : typeof value === "string" ? value : JSON.stringify(value, null, 2);
  return (
    <textarea
      value={current}
      disabled={disabled}
      placeholder={annotation.help}
      onChange={(e) => onChange(e.target.value)}
      rows={Math.max(6, Math.min(24, current.split("\n").length + 1))}
      style={{
        padding: "8px 10px",
        background: "var(--bg-card)",
        color: "var(--text-primary)",
        border: "1px solid var(--glass-border)",
        borderRadius: "var(--radius-sm)",
        fontSize: 12,
        fontFamily: "var(--font-mono, 'SF Mono', 'Monaco', 'Menlo', monospace)",
        lineHeight: 1.5,
        width: "100%",
        minHeight: 120,
        resize: "vertical",
      }}
    />
  );
}
