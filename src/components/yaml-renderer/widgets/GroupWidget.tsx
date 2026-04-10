// GroupWidget — placeholder for a field whose widget is explicitly
// `group`. The renderer ships a full group UI at the top level
// (collapsible sections keyed by `annotation.group`), so this widget
// is only used when a field itself contains a nested object that
// should render as a labeled frame.
//
// Phase 8 renders the nested object as read-only compact JSON; full
// recursive nested-form support (fields within fields) is Phase 10
// once the schema annotation shape can declare nested `fields:`
// sub-maps.

import { useState } from "react";
import type { WidgetProps } from "./WidgetTypes";

export function GroupWidget({ value, annotation }: WidgetProps) {
  const [open, setOpen] = useState(true);
  const label = annotation.group ?? annotation.label ?? "Group";

  return (
    <div
      style={{
        border: "1px solid var(--glass-border)",
        borderRadius: "var(--radius-sm)",
        background: "rgba(255, 255, 255, 0.03)",
      }}
    >
      <button
        type="button"
        onClick={() => setOpen(!open)}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 10px",
          background: "transparent",
          border: "none",
          color: "var(--text-primary)",
          fontSize: 13,
          fontWeight: 500,
          cursor: "pointer",
        }}
      >
        <span>{label}</span>
        <span style={{ fontSize: 11, color: "var(--text-secondary)" }}>
          {open ? "▼" : "▶"}
        </span>
      </button>
      {open && (
        <div
          style={{
            padding: "8px 10px",
            borderTop: "1px solid var(--glass-border)",
            fontSize: 12,
            fontFamily: "var(--font-mono, monospace)",
            color: "var(--text-secondary)",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {formatValue(value)}
        </div>
      )}
    </div>
  );
}

function formatValue(value: unknown): string {
  if (value == null) return "(unset)";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
