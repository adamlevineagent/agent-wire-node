// NumberWidget — numeric input with optional min/max/step and suffix.
//
// Handles both integer and float values via a single numeric input.
// The `suffix` annotation (e.g. "tokens", "sec") renders to the right
// of the field as a static unit hint.

import type { WidgetProps } from "./WidgetTypes";

export function NumberWidget({
  value,
  onChange,
  disabled,
  annotation,
}: WidgetProps) {
  const current =
    value == null
      ? ""
      : typeof value === "number"
        ? String(value)
        : String(value);

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
      <input
        className="input"
        type="number"
        value={current}
        disabled={disabled}
        min={annotation.min}
        max={annotation.max}
        step={annotation.step}
        onChange={(e) => {
          const raw = e.target.value;
          if (raw === "") {
            onChange(undefined);
            return;
          }
          const parsed = Number(raw);
          onChange(Number.isFinite(parsed) ? parsed : raw);
        }}
        style={{
          padding: "6px 10px",
          background: "var(--bg-card)",
          color: "var(--text-primary)",
          border: "1px solid var(--glass-border)",
          borderRadius: "var(--radius-sm)",
          fontSize: 13,
          flex: 1,
          minWidth: 0,
        }}
      />
      {annotation.suffix && (
        <span
          style={{
            fontSize: 12,
            color: "var(--text-secondary)",
            whiteSpace: "nowrap",
          }}
        >
          {annotation.suffix}
        </span>
      )}
    </div>
  );
}
