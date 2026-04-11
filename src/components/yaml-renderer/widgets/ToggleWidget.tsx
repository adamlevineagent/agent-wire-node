// ToggleWidget — boolean on/off switch rendered as a styled checkbox
// with an inline label so the user sees the state without a
// separate legend.

import type { WidgetProps } from "./WidgetTypes";

export function ToggleWidget({
  value,
  onChange,
  disabled,
}: WidgetProps) {
  const checked = Boolean(value);
  return (
    <label
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 8,
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.5 : 1,
      }}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        style={{
          width: 16,
          height: 16,
          accentColor: "var(--accent-cyan)",
          cursor: disabled ? "not-allowed" : "pointer",
        }}
      />
      <span
        style={{
          fontSize: 13,
          color: "var(--text-primary)",
        }}
      >
        {checked ? "On" : "Off"}
      </span>
    </label>
  );
}
