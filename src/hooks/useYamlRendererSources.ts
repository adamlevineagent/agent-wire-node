// src/hooks/useYamlRendererSources.ts — Phase 8 resolver hook.
//
// Walks a `SchemaAnnotation` to collect every unique `options_from`
// (and `item_options_from`) name, then calls
// `invoke('yaml_renderer_resolve_options', { source })` once per
// unique source and caches the result in a plain object. For fields
// with `show_cost: true`, also calls
// `invoke('yaml_renderer_estimate_cost', ...)` against the currently-
// selected tier to produce a per-field cost map.
//
// The cost estimation currently uses a constant average token budget
// (8k input / 2k output). Phase 10 will replace this with
// historical-average lookups per step type.
//
// Parent components pass the hook's return value straight into
// `YamlConfigRenderer` via `optionSources` + `costEstimates`.

import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  OptionValue,
  SchemaAnnotation,
} from "../types/yamlRenderer";

// Phase 8 default tokens budget for cost estimates. Rough per-step
// averages; Phase 10 replaces with historical lookups.
const DEFAULT_AVG_INPUT_TOKENS = 8_000;
const DEFAULT_AVG_OUTPUT_TOKENS = 2_000;

export interface UseYamlRendererSourcesResult {
  optionSources: Record<string, OptionValue[]>;
  costEstimates: Record<string, number>;
  loading: boolean;
  error: string | null;
}

export function useYamlRendererSources(
  schema: SchemaAnnotation | null | undefined,
  values: Record<string, unknown>,
): UseYamlRendererSourcesResult {
  const [optionSources, setOptionSources] = useState<
    Record<string, OptionValue[]>
  >({});
  const [costEstimates, setCostEstimates] = useState<Record<string, number>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Collect unique option source names from the schema. Both
  // `options_from` (for select/model_selector) and `item_options_from`
  // (for list) count.
  const uniqueSources = useMemo(() => {
    if (!schema) return [];
    const set = new Set<string>();
    for (const annotation of Object.values(schema.fields)) {
      if (annotation.options_from) set.add(annotation.options_from);
      if (annotation.item_options_from) set.add(annotation.item_options_from);
    }
    return Array.from(set);
  }, [schema]);

  // Collect the list of (path, annotation) pairs that want cost
  // estimates. We keep the list serializable so the effect
  // dependency array stays stable.
  const costFieldPaths = useMemo(() => {
    if (!schema) return [] as string[];
    return Object.entries(schema.fields)
      .filter(([, annotation]) => annotation.show_cost === true)
      .map(([path]) => path);
  }, [schema]);

  // Fetch option sources on mount / schema change. This IS a per-
  // schema one-shot — the hook does not poll. Parents that need
  // refresh-after-mutation can bump a key on the renderer.
  useEffect(() => {
    if (!schema || uniqueSources.length === 0) {
      setOptionSources({});
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);

    (async () => {
      const out: Record<string, OptionValue[]> = {};
      for (const source of uniqueSources) {
        try {
          const resolved = await invoke<OptionValue[]>(
            "yaml_renderer_resolve_options",
            { source },
          );
          if (cancelled) return;
          out[source] = Array.isArray(resolved) ? resolved : [];
        } catch (err) {
          console.warn(
            `[useYamlRendererSources] resolve_options failed for source="${source}":`,
            err,
          );
          if (cancelled) return;
          out[source] = [];
        }
      }
      if (!cancelled) {
        setOptionSources(out);
        setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
    // `uniqueSources` is a memoized list — the JSON string lets us
    // avoid a deep equality check while still re-running when the
    // schema's source list genuinely changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schema, JSON.stringify(uniqueSources)]);

  // Extract only the tier-path values that cost estimation actually
  // reads. This avoids re-running the cost effect on every unrelated
  // field edit — the earlier implementation depended on the full
  // `values` object, so a keystroke in any field triggered a fresh
  // IPC roundtrip per cost-annotated path. Now we build a stable
  // lookup string from the values that matter, and the effect only
  // re-runs when one of those specific tier values changes.
  const costPathValues = useMemo(() => {
    if (costFieldPaths.length === 0) return "";
    const parts: string[] = [];
    for (const path of costFieldPaths) {
      const raw = readPath(values, path);
      parts.push(`${path}=${typeof raw === "string" ? raw : ""}`);
    }
    return parts.join("|");
  }, [costFieldPaths, values]);

  // Fetch cost estimates for every field that wants one. For Phase 8
  // the cost pair comes from the currently-selected tier — we look up
  // the tier option in `optionSources.tier_registry` to find the
  // (provider, model) pair and then call the estimator.
  useEffect(() => {
    if (!schema || costFieldPaths.length === 0) {
      setCostEstimates({});
      return;
    }
    let cancelled = false;
    const tierOptions = optionSources["tier_registry"] ?? [];
    if (tierOptions.length === 0) {
      setCostEstimates({});
      return;
    }

    (async () => {
      const estimates: Record<string, number> = {};
      for (const path of costFieldPaths) {
        const currentTier = readPath(values, path);
        if (currentTier == null || typeof currentTier !== "string") continue;
        const tier = tierOptions.find((opt) => opt.value === currentTier);
        if (!tier || !tier.meta) continue;
        const provider = String(tier.meta["provider_id"] ?? "");
        const model = String(tier.meta["model_id"] ?? "");
        if (!provider || !model) continue;
        try {
          const cost = await invoke<number>("yaml_renderer_estimate_cost", {
            provider,
            model,
            avgInputTokens: DEFAULT_AVG_INPUT_TOKENS,
            avgOutputTokens: DEFAULT_AVG_OUTPUT_TOKENS,
          });
          if (cancelled) return;
          if (typeof cost === "number" && Number.isFinite(cost)) {
            estimates[path] = cost;
          }
        } catch (err) {
          console.warn(
            `[useYamlRendererSources] estimate_cost failed for ${provider}/${model}:`,
            err,
          );
        }
      }
      if (!cancelled) setCostEstimates(estimates);
    })();

    return () => {
      cancelled = true;
    };
    // `costPathValues` is a stable serialization of only the tier
    // values the effect uses — far cheaper to diff than the full
    // `values` object. `optionSources` is the output of the prior
    // effect; depending on it is correct because tier metadata is
    // what the cost lookup consumes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schema, costFieldPaths, costPathValues, optionSources]);

  return { optionSources, costEstimates, loading, error };
}

/**
 * Internal: read a dotted path from a nested object. Duplicates the
 * helper in `YamlConfigRenderer.tsx` so the hook can stand alone
 * (two call sites, one implementation each — tolerable at current
 * scale).
 */
function readPath(root: Record<string, unknown>, path: string): unknown {
  const parts = path.split(".");
  let current: unknown = root;
  for (const part of parts) {
    if (current == null || typeof current !== "object") return undefined;
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}
