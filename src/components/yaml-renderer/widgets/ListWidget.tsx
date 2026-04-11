// ListWidget — add/remove list of items. Phase 8 supports lists of
// scalar strings picked via a sub-widget (most commonly a select
// against a dynamic option source like `node_fields`). Items are
// rendered as pill rows; the "+" button appends a new blank entry
// that the user fills via the sub-widget dropdown.
//
// Composite widgets (lists of objects, nested forms) are out of
// Phase 8 scope — deferred to Phase 10.

import type { WidgetProps } from "./WidgetTypes";
import type { OptionValue } from "../../../types/yamlRenderer";

export function ListWidget({
  value,
  onChange,
  disabled,
  annotation,
  optionSources,
}: WidgetProps) {
  const items: unknown[] = Array.isArray(value) ? value : [];
  const itemOptions: OptionValue[] =
    (annotation.item_options_from && optionSources[annotation.item_options_from]) ||
    [];
  const itemWidget = annotation.item_widget ?? "text";

  const updateItem = (index: number, next: unknown) => {
    const copy = [...items];
    copy[index] = next;
    onChange(copy);
  };

  const removeItem = (index: number) => {
    const copy = [...items];
    copy.splice(index, 1);
    onChange(copy);
  };

  const addItem = () => {
    onChange([...items, ""]);
  };

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 6,
        padding: 8,
        border: "1px dashed var(--glass-border)",
        borderRadius: "var(--radius-sm)",
        background: "rgba(255, 255, 255, 0.02)",
      }}
    >
      {items.length === 0 && (
        <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>
          No items. Click "+ Add item" to insert one.
        </div>
      )}
      {items.map((item, index) => (
        <div
          key={index}
          style={{ display: "flex", gap: 6, alignItems: "center" }}
        >
          {itemWidget === "select" ? (
            <select
              className="input"
              value={item == null ? "" : String(item)}
              disabled={disabled}
              onChange={(e) => updateItem(index, e.target.value)}
              style={{
                flex: 1,
                minWidth: 0,
                padding: "4px 8px",
                background: "var(--bg-card)",
                color: "var(--text-primary)",
                border: "1px solid var(--glass-border)",
                borderRadius: "var(--radius-sm)",
                fontSize: 13,
              }}
            >
              <option value="" disabled>
                {itemOptions.length === 0
                  ? "No options available"
                  : "Select…"}
              </option>
              {itemOptions.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          ) : (
            <input
              className="input"
              type="text"
              value={item == null ? "" : String(item)}
              disabled={disabled}
              onChange={(e) => updateItem(index, e.target.value)}
              style={{
                flex: 1,
                minWidth: 0,
                padding: "4px 8px",
                background: "var(--bg-card)",
                color: "var(--text-primary)",
                border: "1px solid var(--glass-border)",
                borderRadius: "var(--radius-sm)",
                fontSize: 13,
              }}
            />
          )}
          <button
            type="button"
            className="btn btn-ghost btn-small"
            disabled={disabled}
            onClick={() => removeItem(index)}
            title="Remove item"
          >
            ×
          </button>
        </div>
      ))}
      <button
        type="button"
        className="btn btn-ghost btn-small"
        disabled={disabled}
        onClick={addItem}
        style={{ alignSelf: "flex-start" }}
      >
        + Add item
      </button>
    </div>
  );
}
