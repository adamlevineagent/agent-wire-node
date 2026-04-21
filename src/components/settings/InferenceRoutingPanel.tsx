// src/components/settings/InferenceRoutingPanel.tsx
//
// Wave 4 (Walker Re-Plan Wire 2.1, plan §8 task 30-33): operator-facing
// editor for the active `dispatch_policy` contribution's
// `routing_rules[*].route_to` chain.
//
// Data flow:
//   - Load: invoke("pyramid_active_config_contribution",
//           { schemaType: "dispatch_policy", slug: null })
//     -> returns ConfigContribution | null. We parse yaml_content with
//     js-yaml into a DispatchPolicyYaml, then edit in-memory.
//   - Save: invoke("pyramid_supersede_config", {
//           contributionId, newYamlContent, note })
//     -> the generic supersede IPC already handles the operational sync
//     (see main.rs:9430) and chronicle writes.
//
// Scope: no new IPC/HTTP surfaces added. Backend is untouched. See
// walker-re-plan-wire-2.1.md §8 Wave 4 task 30.
//
// Design choices recorded in the Wave 4 friction-log:
//   - Edits the DEFAULT rule (first rule named "default" OR first rule
//     in the list when none is named "default"). Minimum-viable per
//     plan. Multi-rule editing is a future pass — the `Advanced: raw
//     YAML` fallback lives in the existing ToolsMode.tsx path and
//     remains the escape hatch.
//   - Drag-reorder via up/down buttons (plan forbids a drag-drop lib).
//   - "Apply" button is the debounce target: the actual supersede RPC
//     only fires on Apply press, and is throttled to 1/sec client-side
//     to prevent supersession flood if the operator mashes the button.
//   - Validation: provider_id is required. "fleet" and "market" are
//     sentinel walker routes; any other string is treated as a concrete
//     provider_id (no round-trip to a known-provider-catalog this pass;
//     plan says defer).
//   - `max_budget_credits` is optional; blank = no cap.

import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import yaml from "js-yaml";
import type { ConfigContribution } from "../../types/configContributions";

// ── IPC types ───────────────────────────────────────────────────────────

/** Mirrors Rust `PyramidMarketModel` in
 *  `src-tauri/src/pyramid/market_surface_cache.rs`. Returned by the
 *  `pyramid_market_models` IPC (Wave 4 task 29). */
interface PyramidMarketModel {
    model_id: string;
    active_offers: number;
    rate_in_per_m: number | null;
    rate_out_per_m: number | null;
    last_updated_at: string;
}

/** Subset of `compute_participation_policy` YAML we read for the
 *  network-compute sub-panel. Other fields round-trip untouched
 *  (we never write this contribution from this panel). */
interface ComputeParticipationPolicyShape {
    market_dispatch_max_wait_ms?: number;
    market_saturation_patience_secs?: number;
    [key: string]: unknown;
}

// ── localStorage keys ───────────────────────────────────────────────────

const DISCOVERY_BOOKMARK_KEY = "inferenceRouting.lastReviewedMarketModels";

// ── Types mirroring src-tauri/src/pyramid/dispatch_policy.rs ────────────

/** One provider entry in a routing chain. Mirrors Rust `RouteEntry`. */
export interface RouteEntry {
    provider_id: string;
    model_id?: string | null;
    tier_name?: string | null;
    is_local?: boolean;
    /** Wave 3 addition. `null`/absent → NO_BUDGET_CAP sentinel server-side. */
    max_budget_credits?: number | null;
}

/** Predicate subset used for display. Mirrors Rust `MatchConfig`. */
interface MatchConfigShape {
    work_type?: string | null;
    min_depth?: number | null;
    step_pattern?: string | null;
}

/** Mirrors Rust `RoutingRule`. */
interface RoutingRuleShape {
    name: string;
    match_config: MatchConfigShape;
    route_to: RouteEntry[];
    bypass_pool?: boolean;
    sequential?: boolean;
}

/** Mirrors Rust `DispatchPolicyYaml`. We preserve every field on
 *  round-trip so the panel never drops operator-set state it doesn't
 *  display. */
interface DispatchPolicyYaml {
    version: number;
    provider_pools?: Record<string, unknown>;
    routing_rules?: RoutingRuleShape[];
    escalation?: Record<string, unknown>;
    build_coordination?: Record<string, unknown>;
    max_batch_cost_usd?: number | null;
    max_daily_cost_usd?: number | null;
    // Allow any extra fields to round-trip cleanly. Rust side's
    // DispatchPolicyYaml doesn't `deny_unknown_fields` but preserving
    // the shape we see keeps future additions safe.
    [key: string]: unknown;
}

// ── Component ────────────────────────────────────────────────────────────

const SUPERSEDE_DEBOUNCE_MS = 1000;

export function InferenceRoutingPanel() {
    const [contribution, setContribution] = useState<ConfigContribution | null>(null);
    const [parsed, setParsed] = useState<DispatchPolicyYaml | null>(null);
    const [working, setWorking] = useState<DispatchPolicyYaml | null>(null);
    const [loading, setLoading] = useState(true);
    const [loadError, setLoadError] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);
    const [saveError, setSaveError] = useState<string | null>(null);
    const [saveOk, setSaveOk] = useState(false);
    const [note, setNote] = useState("");
    const lastSaveAtRef = useRef<number>(0);

    // ── Discovery section state (network compute models) ────────────────
    const [networkModels, setNetworkModels] = useState<PyramidMarketModel[]>([]);
    const [networkModelsError, setNetworkModelsError] = useState<string | null>(null);
    const [networkExpanded, setNetworkExpanded] = useState(false);
    const [reviewedBookmark, setReviewedBookmark] = useState<Set<string>>(() => {
        try {
            const raw = localStorage.getItem(DISCOVERY_BOOKMARK_KEY);
            if (!raw) return new Set();
            const parsed = JSON.parse(raw);
            return Array.isArray(parsed) ? new Set(parsed.map(String)) : new Set();
        } catch {
            return new Set();
        }
    });

    // ── Compute participation policy (for max_wait_ms readonly display) ─
    const [participationPolicy, setParticipationPolicy] =
        useState<ComputeParticipationPolicyShape | null>(null);

    // ── Load active dispatch_policy on mount ─────────────────────────────
    const reload = useCallback(async () => {
        setLoading(true);
        setLoadError(null);
        try {
            const row = await invoke<ConfigContribution | null>(
                "pyramid_active_config_contribution",
                { schemaType: "dispatch_policy", slug: null },
            );
            setContribution(row);
            if (row) {
                try {
                    const doc = yaml.load(row.yaml_content) as DispatchPolicyYaml;
                    if (!doc || typeof doc !== "object") {
                        throw new Error("YAML did not parse to an object");
                    }
                    setParsed(doc);
                    // Deep clone for working copy (structuredClone is available in modern Tauri webview).
                    setWorking(structuredClone(doc));
                } catch (parseErr) {
                    setLoadError(`Failed to parse dispatch_policy YAML: ${String(parseErr)}`);
                    setParsed(null);
                    setWorking(null);
                }
            } else {
                setParsed(null);
                setWorking(null);
            }
        } catch (err) {
            setLoadError(String(err));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        void reload();
    }, [reload]);

    // ── Network-compute models fetch (IPC pyramid_market_models) ─────────
    // Polled once per mount + on explicit refresh. The backend cache
    // itself polls every 60s; another layer of client polling is
    // unnecessary churn.
    const reloadNetworkModels = useCallback(async () => {
        setNetworkModelsError(null);
        try {
            const rows = await invoke<PyramidMarketModel[]>("pyramid_market_models");
            setNetworkModels(Array.isArray(rows) ? rows : []);
        } catch (err) {
            setNetworkModelsError(String(err));
            setNetworkModels([]);
        }
    }, []);

    useEffect(() => {
        void reloadNetworkModels();
    }, [reloadNetworkModels]);

    // ── Participation policy fetch (for max_wait_ms readonly display) ───
    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const row = await invoke<ConfigContribution | null>(
                    "pyramid_active_config_contribution",
                    { schemaType: "compute_participation_policy", slug: null },
                );
                if (cancelled) return;
                if (!row) {
                    setParticipationPolicy(null);
                    return;
                }
                try {
                    const doc = yaml.load(row.yaml_content) as ComputeParticipationPolicyShape;
                    if (doc && typeof doc === "object") {
                        setParticipationPolicy(doc);
                    } else {
                        setParticipationPolicy(null);
                    }
                } catch {
                    setParticipationPolicy(null);
                }
            } catch {
                if (!cancelled) setParticipationPolicy(null);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, []);

    // Set of model_ids new since last review (Discovery highlight).
    const newSinceReview = useMemo(() => {
        const s = new Set<string>();
        for (const m of networkModels) {
            if (!reviewedBookmark.has(m.model_id)) s.add(m.model_id);
        }
        return s;
    }, [networkModels, reviewedBookmark]);

    const markAllReviewed = useCallback(() => {
        const ids = networkModels.map((m) => m.model_id);
        try {
            localStorage.setItem(DISCOVERY_BOOKMARK_KEY, JSON.stringify(ids));
        } catch {
            // Quota errors are non-fatal — the bookmark is purely a UI
            // convenience, not load-bearing state.
        }
        setReviewedBookmark(new Set(ids));
    }, [networkModels]);

    // ── Identify "the default rule" index ────────────────────────────────
    // Minimum-viable MVP per plan: we edit the first rule whose name is
    // literally "default". If none, fall back to the first rule.
    const defaultRuleIndex = (() => {
        const rules = working?.routing_rules ?? [];
        const named = rules.findIndex((r) => r.name === "default");
        if (named !== -1) return named;
        return rules.length > 0 ? 0 : -1;
    })();

    const defaultRule = defaultRuleIndex >= 0 && working
        ? working.routing_rules![defaultRuleIndex]
        : null;

    // ── Dirty state: JSON-stringify round-trip is good enough for a diff ─
    const isDirty = (() => {
        if (!parsed || !working) return false;
        try {
            return JSON.stringify(parsed) !== JSON.stringify(working);
        } catch {
            return true;
        }
    })();

    // ── Mutators ─────────────────────────────────────────────────────────
    const updateRule = useCallback(
        (mutator: (rule: RoutingRuleShape) => void) => {
            setWorking((prev) => {
                if (!prev) return prev;
                const next: DispatchPolicyYaml = structuredClone(prev);
                const rules = next.routing_rules;
                if (!rules || defaultRuleIndex < 0 || defaultRuleIndex >= rules.length) {
                    return prev;
                }
                mutator(rules[defaultRuleIndex]);
                return next;
            });
            setSaveOk(false);
        },
        [defaultRuleIndex],
    );

    const addEntry = useCallback(() => {
        updateRule((rule) => {
            rule.route_to.push({
                provider_id: "",
                is_local: false,
            });
        });
    }, [updateRule]);

    const deleteEntry = useCallback((idx: number) => {
        updateRule((rule) => {
            rule.route_to.splice(idx, 1);
        });
    }, [updateRule]);

    const moveEntry = useCallback(
        (idx: number, direction: -1 | 1) => {
            updateRule((rule) => {
                const target = idx + direction;
                if (target < 0 || target >= rule.route_to.length) return;
                const [moved] = rule.route_to.splice(idx, 1);
                rule.route_to.splice(target, 0, moved);
            });
        },
        [updateRule],
    );

    const editEntry = useCallback(
        (idx: number, field: keyof RouteEntry, value: unknown) => {
            updateRule((rule) => {
                const entry = rule.route_to[idx];
                if (!entry) return;
                // Field-specific type coercion. We don't trust `value`
                // shape from an arbitrary input event.
                if (field === "is_local") {
                    entry.is_local = Boolean(value);
                } else if (field === "max_budget_credits") {
                    if (value === "" || value === null || value === undefined) {
                        entry.max_budget_credits = null;
                    } else {
                        const n = typeof value === "number" ? value : Number(value);
                        if (Number.isFinite(n) && n >= 0 && Number.isInteger(n)) {
                            entry.max_budget_credits = n;
                        }
                    }
                } else if (field === "provider_id") {
                    entry.provider_id = String(value ?? "");
                } else if (field === "model_id") {
                    const s = String(value ?? "");
                    entry.model_id = s === "" ? null : s;
                } else if (field === "tier_name") {
                    const s = String(value ?? "");
                    entry.tier_name = s === "" ? null : s;
                }
            });
        },
        [updateRule],
    );

    // ── Apply (debounced save) ──────────────────────────────────────────
    const handleApply = useCallback(async () => {
        if (!contribution || !working) return;
        if (!note.trim()) {
            setSaveError("Please enter a note describing the change.");
            return;
        }

        // Client-side throttle: cap 1 save per second.
        const now = Date.now();
        const sinceLast = now - lastSaveAtRef.current;
        if (sinceLast < SUPERSEDE_DEBOUNCE_MS) {
            setSaveError(
                `Too many saves — wait ${Math.ceil(
                    (SUPERSEDE_DEBOUNCE_MS - sinceLast) / 1000,
                )}s and try again.`,
            );
            return;
        }
        lastSaveAtRef.current = now;

        // Validate entries before save.
        const rule = working.routing_rules?.[defaultRuleIndex];
        if (!rule) {
            setSaveError("No routing rule to save.");
            return;
        }
        for (const [i, entry] of rule.route_to.entries()) {
            if (!entry.provider_id.trim()) {
                setSaveError(`Entry ${i + 1}: provider_id is required.`);
                return;
            }
            if (
                entry.max_budget_credits != null &&
                (!Number.isInteger(entry.max_budget_credits) ||
                    entry.max_budget_credits < 0)
            ) {
                setSaveError(
                    `Entry ${i + 1}: max_budget_credits must be a non-negative integer or blank.`,
                );
                return;
            }
        }

        setSaving(true);
        setSaveError(null);
        setSaveOk(false);
        try {
            const newYaml = yaml.dump(working, { lineWidth: -1, noRefs: true });
            await invoke("pyramid_supersede_config", {
                contributionId: contribution.contribution_id,
                newYamlContent: newYaml,
                note: note.trim(),
            });
            setSaveOk(true);
            setNote("");
            // Reload so the local `parsed` snapshot matches the new
            // committed version and dirty-state flips back to clean.
            await reload();
        } catch (err) {
            setSaveError(String(err));
        } finally {
            setSaving(false);
        }
    }, [contribution, working, note, defaultRuleIndex, reload]);

    const handleReset = useCallback(() => {
        if (!parsed) return;
        setWorking(structuredClone(parsed));
        setSaveError(null);
        setSaveOk(false);
    }, [parsed]);

    // ── Render ───────────────────────────────────────────────────────────
    if (loading) {
        return (
            <div className="settings-section">
                <div className="settings-section-header">Inference Routing</div>
                <p className="settings-section-desc">Loading dispatch policy…</p>
            </div>
        );
    }

    if (loadError) {
        return (
            <div className="settings-section">
                <div className="settings-section-header">Inference Routing</div>
                <p className="settings-section-desc" style={{ color: "var(--error, #c00)" }}>
                    {loadError}
                </p>
                <button type="button" onClick={() => void reload()}>
                    Retry
                </button>
            </div>
        );
    }

    if (!contribution || !working || !defaultRule) {
        return (
            <div className="settings-section">
                <div className="settings-section-header">Inference Routing</div>
                <p className="settings-section-desc">
                    No active dispatch policy found. A fresh install seeds one at
                    boot — if you're seeing this, check the app log for
                    hydration errors.
                </p>
                <button type="button" onClick={() => void reload()}>
                    Reload
                </button>
            </div>
        );
    }

    const entries = defaultRule.route_to;

    return (
        <div className="settings-section" data-testid="inference-routing-panel">
            <div className="settings-section-header">Inference Routing</div>
            <p className="settings-section-desc">
                Controls the order in which providers are tried for each LLM call.
                Edits apply to the <strong>{defaultRule.name}</strong> routing rule.
                Use <code>fleet</code> to route through your fleet, or{" "}
                <code>market</code> to route through network compute; any other
                value is a direct provider (e.g. <code>openrouter</code>,{" "}
                <code>ollama</code>).
            </p>

            <div style={{ marginBottom: 12, fontSize: "0.85em", opacity: 0.75 }}>
                {isDirty ? (
                    <span style={{ color: "var(--warning, #b80)" }}>
                        ● Unsaved changes
                    </span>
                ) : (
                    <span>✓ Synced</span>
                )}
            </div>

            {/* Read-only summary of other rules so operators see they exist. */}
            {(working.routing_rules?.length ?? 0) > 1 && (
                <details style={{ marginBottom: 12 }}>
                    <summary style={{ cursor: "pointer" }}>
                        Other routing rules ({(working.routing_rules?.length ?? 1) - 1})
                    </summary>
                    <ul style={{ fontSize: "0.85em", margin: "8px 0", paddingLeft: 20 }}>
                        {working.routing_rules!
                            .map((r, i) => ({ r, i }))
                            .filter(({ i }) => i !== defaultRuleIndex)
                            .map(({ r, i }) => (
                                <li key={i}>
                                    <strong>{r.name}</strong>
                                    {r.match_config.work_type && ` (${r.match_config.work_type})`}
                                    {" → "}
                                    {r.route_to.map((e) => e.provider_id).join(" → ") || "(empty)"}
                                </li>
                            ))}
                    </ul>
                    <p style={{ fontSize: "0.85em", opacity: 0.7, margin: 0 }}>
                        To edit other rules, use the Tools tab's raw YAML editor.
                    </p>
                </details>
            )}

            {/* Editable entry list for the default rule. */}
            <table
                className="route-entries"
                style={{ width: "100%", borderCollapse: "collapse", marginBottom: 12 }}
            >
                <thead>
                    <tr>
                        <th style={{ textAlign: "left", width: 40 }}>#</th>
                        <th style={{ textAlign: "left" }}>Provider</th>
                        <th style={{ textAlign: "left" }}>Model (optional)</th>
                        <th style={{ textAlign: "left" }}>Tier (optional)</th>
                        <th style={{ textAlign: "left", width: 60 }}>Local</th>
                        <th style={{ textAlign: "left" }}>Max budget (credits)</th>
                        <th style={{ textAlign: "left", width: 120 }}>Reorder</th>
                        <th style={{ width: 60 }}></th>
                    </tr>
                </thead>
                <tbody>
                    {entries.map((entry, idx) => [
                        <tr key={`${idx}-row`} data-testid={`route-entry-${idx}`}>
                            <td>{idx + 1}</td>
                            <td>
                                <input
                                    type="text"
                                    value={entry.provider_id}
                                    placeholder="fleet | market | openrouter | ollama | …"
                                    onChange={(e) =>
                                        editEntry(idx, "provider_id", e.target.value)
                                    }
                                    aria-label={`Provider for entry ${idx + 1}`}
                                    style={{ width: "100%" }}
                                />
                            </td>
                            <td>
                                <input
                                    type="text"
                                    value={entry.model_id ?? ""}
                                    onChange={(e) =>
                                        editEntry(idx, "model_id", e.target.value)
                                    }
                                    aria-label={`Model id for entry ${idx + 1}`}
                                    style={{ width: "100%" }}
                                />
                            </td>
                            <td>
                                <input
                                    type="text"
                                    value={entry.tier_name ?? ""}
                                    onChange={(e) =>
                                        editEntry(idx, "tier_name", e.target.value)
                                    }
                                    aria-label={`Tier for entry ${idx + 1}`}
                                    style={{ width: "100%" }}
                                />
                            </td>
                            <td style={{ textAlign: "center" }}>
                                <input
                                    type="checkbox"
                                    checked={Boolean(entry.is_local)}
                                    onChange={(e) =>
                                        editEntry(idx, "is_local", e.target.checked)
                                    }
                                    aria-label={`Local flag for entry ${idx + 1}`}
                                />
                            </td>
                            <td>
                                <input
                                    type="number"
                                    min={0}
                                    step={1}
                                    value={entry.max_budget_credits ?? ""}
                                    placeholder="no cap"
                                    onChange={(e) =>
                                        editEntry(idx, "max_budget_credits", e.target.value)
                                    }
                                    aria-label={`Max budget credits for entry ${idx + 1}`}
                                    style={{ width: "100%" }}
                                />
                            </td>
                            <td>
                                <button
                                    type="button"
                                    onClick={() => moveEntry(idx, -1)}
                                    disabled={idx === 0}
                                    aria-label={`Move entry ${idx + 1} up`}
                                    title="Move up"
                                    data-testid={`move-up-${idx}`}
                                >
                                    ↑
                                </button>
                                <button
                                    type="button"
                                    onClick={() => moveEntry(idx, 1)}
                                    disabled={idx === entries.length - 1}
                                    aria-label={`Move entry ${idx + 1} down`}
                                    title="Move down"
                                    data-testid={`move-down-${idx}`}
                                    style={{ marginLeft: 4 }}
                                >
                                    ↓
                                </button>
                            </td>
                            <td>
                                <button
                                    type="button"
                                    onClick={() => deleteEntry(idx)}
                                    aria-label={`Delete entry ${idx + 1}`}
                                    title="Delete entry"
                                    data-testid={`delete-${idx}`}
                                >
                                    ✕
                                </button>
                            </td>
                        </tr>,
                        // Sub-panel for network-compute rows (provider_id == "market").
                        // Shows the readonly `max_wait_ms` pulled from the active
                        // compute_participation_policy + a link to the Wire-side
                        // observability dashboard. Rendered as a full-width row under
                        // the main row — avoids restructuring the table layout.
                        entry.provider_id === "market" ? (
                            <tr
                                key={`${idx}-sub`}
                                data-testid={`route-entry-${idx}-network-subpanel`}
                            >
                                <td></td>
                                <td colSpan={7} style={{ paddingBottom: 12 }}>
                                    <div
                                        style={{
                                            fontSize: "0.85em",
                                            background: "var(--panel-alt, rgba(0,0,0,0.03))",
                                            padding: 8,
                                            borderRadius: 4,
                                        }}
                                    >
                                        <div style={{ marginBottom: 4 }}>
                                            <strong>Max wait before giving up</strong>:{" "}
                                            {participationPolicy?.market_dispatch_max_wait_ms != null ? (
                                                <code>
                                                    {participationPolicy.market_dispatch_max_wait_ms} ms
                                                </code>
                                            ) : (
                                                <span style={{ opacity: 0.6 }}>
                                                    (loading from participation policy…)
                                                </span>
                                            )}{" "}
                                            <span style={{ opacity: 0.7 }}>
                                                — edit via Compute Participation settings.
                                            </span>
                                        </div>
                                        <div>
                                            <a
                                                href="/ops"
                                                target="_blank"
                                                rel="noopener noreferrer"
                                                data-testid={`network-dashboard-link-${idx}`}
                                            >
                                                Observability dashboard →
                                            </a>
                                        </div>
                                    </div>
                                </td>
                            </tr>
                        ) : null,
                    ])}
                    {entries.length === 0 && (
                        <tr>
                            <td colSpan={8} style={{ padding: 12, opacity: 0.7 }}>
                                No entries. Add at least one — an empty chain means
                                the walker has nothing to try.
                            </td>
                        </tr>
                    )}
                </tbody>
            </table>

            <button
                type="button"
                onClick={addEntry}
                data-testid="add-entry"
                style={{ marginBottom: 12 }}
            >
                + Add entry
            </button>

            {/* Note input (required by pyramid_supersede_config). */}
            <div style={{ marginBottom: 12 }}>
                <label
                    htmlFor="inference-routing-note"
                    style={{ display: "block", marginBottom: 4, fontSize: "0.9em" }}
                >
                    Change note (required):
                </label>
                <input
                    id="inference-routing-note"
                    type="text"
                    value={note}
                    onChange={(e) => setNote(e.target.value)}
                    placeholder="e.g. prefer fleet before network compute for evidence work"
                    disabled={!isDirty}
                    style={{ width: "100%" }}
                />
            </div>

            {/* Apply / Reset controls. */}
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                <button
                    type="button"
                    onClick={() => void handleApply()}
                    disabled={!isDirty || saving || !note.trim()}
                    data-testid="apply-btn"
                    className="save-btn"
                >
                    {saving ? "Applying…" : "Apply"}
                </button>
                <button
                    type="button"
                    onClick={handleReset}
                    disabled={!isDirty || saving}
                    data-testid="reset-btn"
                >
                    Reset
                </button>
                {saveOk && (
                    <span style={{ color: "var(--success, #080)", fontSize: "0.9em" }}>
                        ✓ Applied
                    </span>
                )}
                {saveError && (
                    <span style={{ color: "var(--error, #c00)", fontSize: "0.9em" }}>
                        {saveError}
                    </span>
                )}
            </div>

            {/* Discovery: network compute models available through the
                 tunnel. Collapsible — kept out of the default view so the
                 primary routing-rules editor isn't visually crowded. New
                 model_ids (not in the "reviewed" bookmark) are flagged. */}
            <details
                style={{ marginTop: 20 }}
                open={networkExpanded}
                onToggle={(e) =>
                    setNetworkExpanded((e.target as HTMLDetailsElement).open)
                }
                data-testid="network-discovery-section"
            >
                <summary style={{ cursor: "pointer", fontWeight: 500 }}>
                    Network compute discovery
                    {networkModels.length > 0 && (
                        <span style={{ opacity: 0.7, marginLeft: 8, fontWeight: 400 }}>
                            ({networkModels.length} models available
                            {newSinceReview.size > 0
                                ? `, ${newSinceReview.size} new since last review`
                                : ""})
                        </span>
                    )}
                </summary>

                <div style={{ marginTop: 12 }}>
                    {networkModelsError && (
                        <p style={{ color: "var(--error, #c00)", fontSize: "0.9em" }}>
                            {networkModelsError}
                        </p>
                    )}
                    {!networkModelsError && networkModels.length === 0 && (
                        <p style={{ fontSize: "0.9em", opacity: 0.7 }}>
                            No models available yet. Either the tunnel hasn't
                            connected or the first refresh hasn't landed (checks
                            every 60s).
                        </p>
                    )}
                    {networkModels.length > 0 && (
                        <>
                            <table
                                style={{
                                    width: "100%",
                                    borderCollapse: "collapse",
                                    fontSize: "0.9em",
                                }}
                                data-testid="network-discovery-table"
                            >
                                <thead>
                                    <tr>
                                        <th style={{ textAlign: "left" }}>Model</th>
                                        <th style={{ textAlign: "right" }}>
                                            Available
                                        </th>
                                        <th style={{ textAlign: "right" }}>
                                            Input / M (credits)
                                        </th>
                                        <th style={{ textAlign: "right" }}>
                                            Output / M (credits)
                                        </th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {networkModels.map((m) => {
                                        const isNew = newSinceReview.has(m.model_id);
                                        return (
                                            <tr
                                                key={m.model_id}
                                                data-testid={`network-model-row-${m.model_id}`}
                                                style={{
                                                    background: isNew
                                                        ? "var(--highlight, rgba(255, 220, 100, 0.15))"
                                                        : undefined,
                                                }}
                                            >
                                                <td>
                                                    {isNew && (
                                                        <span
                                                            title="New since last review"
                                                            style={{
                                                                marginRight: 4,
                                                                color: "var(--warning, #b80)",
                                                            }}
                                                        >
                                                            ●
                                                        </span>
                                                    )}
                                                    <code>{m.model_id}</code>
                                                </td>
                                                <td style={{ textAlign: "right" }}>
                                                    {m.active_offers}
                                                </td>
                                                <td style={{ textAlign: "right" }}>
                                                    {m.rate_in_per_m ?? "—"}
                                                </td>
                                                <td style={{ textAlign: "right" }}>
                                                    {m.rate_out_per_m ?? "—"}
                                                </td>
                                            </tr>
                                        );
                                    })}
                                </tbody>
                            </table>
                            <div
                                style={{
                                    marginTop: 8,
                                    display: "flex",
                                    gap: 8,
                                    alignItems: "center",
                                    fontSize: "0.85em",
                                }}
                            >
                                <button
                                    type="button"
                                    onClick={markAllReviewed}
                                    disabled={newSinceReview.size === 0}
                                    data-testid="mark-reviewed-btn"
                                >
                                    Mark all reviewed
                                </button>
                                <button
                                    type="button"
                                    onClick={() => void reloadNetworkModels()}
                                    data-testid="refresh-network-models-btn"
                                >
                                    Refresh
                                </button>
                                {networkModels[0]?.last_updated_at && (
                                    <span style={{ opacity: 0.7 }}>
                                        snapshot{" "}
                                        {networkModels[0].last_updated_at}
                                    </span>
                                )}
                            </div>
                        </>
                    )}
                </div>
            </details>
        </div>
    );
}
