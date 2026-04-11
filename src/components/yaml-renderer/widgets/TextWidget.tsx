// TextWidget — free-form string input.

import type { WidgetProps } from "./WidgetTypes";

export function TextWidget({
  value,
  onChange,
  disabled,
  annotation,
}: WidgetProps) {
  const current = value == null ? "" : String(value);
  return (
    <input
      className="input"
      type="text"
      value={current}
      disabled={disabled}
      placeholder={annotation.help}
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
    />
  );
}
