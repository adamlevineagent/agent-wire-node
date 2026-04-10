// ModelSelectorWidget — composite tier picker that shows provider +
// model + context window + cost in one row. Reads the
// `tier_registry` source by default; the annotation can override
// via `options_from` for a more specific source.
//
// The extra metadata (provider_id, model_id, context_limit, prompt
// pricing) comes from the `OptionValue.meta` payload the Rust
// resolver attaches to each tier entry — see
// `yaml_renderer::tier_entry_to_option` for the exact keys.

import type { WidgetProps } from "./WidgetTypes";
import type { OptionValue } from "../../../types/yamlRenderer";

export function ModelSelectorWidget({
  value,
  onChange,
  disabled,
  annotation,
  optionSources,
  costEstimate,
}: WidgetProps) {
  // Default to `tier_registry` so the common chain-step use case
  // works without the annotation having to name the source explicitly.
  const sourceName = annotation.options_from ?? "tier_registry";
  const options: OptionValue[] = optionSources[sourceName] ?? [];
  const current = value == null ? "" : String(value);
  const selected = options.find((opt) => opt.value === current);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      <select
        className="input"
        value={current}
        disabled={disabled || options.length === 0}
        onChange={(e) => onChange(e.target.value)}
        style={{
          padding: "6px 10px",
          background: "var(--bg-card)",
          color: "var(--text-primary)",
          border: "1px solid var(--glass-border)",
          borderRadius: "var(--radius-sm)",
          fontSize: 13,
        }}
      >
        {!current && (
          <option value="" disabled>
            {options.length === 0 ? "No tiers available" : "Select a tier…"}
          </option>
        )}
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
            {opt.description ? ` — ${opt.description}` : ""}
          </option>
        ))}
      </select>
      {selected && (
        <div
          style={{
            display: "flex",
            flexWrap: "wrap",
            gap: 10,
            fontSize: 11,
            color: "var(--text-secondary)",
          }}
        >
          <ProviderBadge meta={selected.meta} />
          <ContextBadge meta={selected.meta} />
          {costEstimate != null && (
            <span
              style={{
                padding: "2px 6px",
                borderRadius: "var(--radius-sm)",
                background: "rgba(34, 211, 238, 0.1)",
                color: "var(--accent-cyan)",
              }}
            >
              ~${costEstimate.toFixed(4)} / call
            </span>
          )}
        </div>
      )}
    </div>
  );
}

function ProviderBadge({ meta }: { meta?: Record<string, unknown> }) {
  if (!meta) return null;
  const provider = meta["provider_id"];
  const model = meta["model_id"];
  if (!provider && !model) return null;
  return (
    <span
      style={{
        padding: "2px 6px",
        borderRadius: "var(--radius-sm)",
        background: "rgba(167, 139, 250, 0.1)",
        color: "var(--accent-purple)",
        fontFamily: "var(--font-mono, monospace)",
      }}
    >
      {String(provider ?? "?")} / {String(model ?? "?")}
    </span>
  );
}

function ContextBadge({ meta }: { meta?: Record<string, unknown> }) {
  const contextLimit = meta?.["context_limit"];
  if (contextLimit == null) return null;
  const ctx = typeof contextLimit === "number" ? contextLimit : Number(contextLimit);
  if (!Number.isFinite(ctx) || ctx <= 0) return null;
  const label = ctx >= 1_000_000 ? `${(ctx / 1_000_000).toFixed(1)}M` : `${Math.round(ctx / 1000)}k`;
  return (
    <span
      style={{
        padding: "2px 6px",
        borderRadius: "var(--radius-sm)",
        background: "rgba(255, 255, 255, 0.06)",
      }}
    >
      context: {label}
    </span>
  );
}
