// SliderWidget — continuous range slider (for temperature-style
// fields). Mirrors the static readout next to the slider so the user
// sees the exact value. Falls back to sensible defaults when the
// annotation omits min/max/step.

import type { WidgetProps } from "./WidgetTypes";

export function SliderWidget({
  value,
  onChange,
  disabled,
  annotation,
}: WidgetProps) {
  const min = annotation.min ?? 0;
  const max = annotation.max ?? 1;
  const step = annotation.step ?? 0.01;
  const numericValue =
    typeof value === "number" ? value : typeof value === "string" ? Number(value) : min;
  const current = Number.isFinite(numericValue) ? numericValue : min;

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
      <input
        type="range"
        disabled={disabled}
        min={min}
        max={max}
        step={step}
        value={current}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{
          flex: 1,
          minWidth: 0,
          accentColor: "var(--accent-cyan)",
        }}
      />
      <span
        style={{
          fontSize: 12,
          fontFamily: "var(--font-mono, monospace)",
          color: "var(--text-primary)",
          minWidth: 48,
          textAlign: "right",
        }}
      >
        {formatSliderValue(current, step)}
        {annotation.suffix ? ` ${annotation.suffix}` : ""}
      </span>
    </div>
  );
}

function formatSliderValue(value: number, step: number): string {
  // Infer precision from the step size — 0.01 → 2 decimals, 0.1 → 1,
  // integer step → 0. Avoids "0.30000000004" artifacts.
  if (!Number.isFinite(step) || step >= 1) return String(Math.round(value));
  const decimals = Math.max(0, -Math.floor(Math.log10(step)));
  return value.toFixed(decimals);
}
