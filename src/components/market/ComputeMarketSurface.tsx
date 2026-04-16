// ComputeMarketSurface.tsx — Browse the network's available compute providers.
//
// Per `docs/plans/compute-market-phase-2-exchange.md` §IV:
//   - Fetches via IPC `compute_market_surface` (calls Wire
//     /api/v1/compute/market-surface).
//   - Per-model aggregation: offers, pricing ranges, queue depths,
//     provider counts, network-observed performance medians.
//   - Read-only for Phase 2 (no "buy compute" until Phase 3
//     requester integration).
//
// The Wire response shape is not strictly typed in this component —
// we display whatever the Wire returns and gracefully degrade on
// missing fields. Phase 3 will firm up the contract when the
// requester side is built.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface MarketSurfaceProvider {
    node_id?: string;
    provider_type?: string;
    rate_per_m_input?: number;
    rate_per_m_output?: number;
    reservation_fee?: number;
    queue_depth?: number;
    max_queue_depth?: number;
    median_tps?: number;
    p95_latency_ms?: number;
    observation_count?: number;
}

interface MarketSurfaceModel {
    model_id: string;
    providers?: MarketSurfaceProvider[];
    min_rate_input?: number;
    max_rate_input?: number;
    min_rate_output?: number;
    max_rate_output?: number;
    total_queue_depth?: number;
    provider_count?: number;
    median_tps?: number;
    p95_latency_ms?: number;
}

interface MarketSurface {
    models?: MarketSurfaceModel[];
    fetched_at?: string;
}

type SortField = "price" | "queue" | "speed";

export function ComputeMarketSurface() {
    const [surface, setSurface] = useState<MarketSurface | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [modelFilter, setModelFilter] = useState<string>("");
    const [sortBy, setSortBy] = useState<SortField>("price");

    const refresh = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const args: { modelId?: string } = {};
            if (modelFilter.trim()) args.modelId = modelFilter.trim();
            const resp = await invoke<MarketSurface>("compute_market_surface", args);
            setSurface(resp);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, [modelFilter]);

    useEffect(() => {
        void refresh();
    }, [refresh]);

    const sortedModels = sortModels(surface?.models ?? [], sortBy);

    return (
        <div className="compute-market-surface">
            <h2>Compute Market Surface</h2>
            <p style={{ color: "#888", fontSize: 13 }}>
                Browse compute providers across the network. Read-only — Phase 2 is
                provider-side only; requester-side purchase arrives in Phase 3.
            </p>

            <div style={{ display: "flex", gap: 12, marginBottom: 16, alignItems: "center" }}>
                <input
                    type="text"
                    value={modelFilter}
                    onChange={(e) => setModelFilter(e.target.value)}
                    placeholder="Filter by model id (optional)"
                    style={{ flex: "1" }}
                />
                <label style={{ whiteSpace: "nowrap" }}>
                    Sort:&nbsp;
                    <select value={sortBy} onChange={(e) => setSortBy(e.target.value as SortField)}>
                        <option value="price">Price</option>
                        <option value="queue">Queue depth</option>
                        <option value="speed">Speed</option>
                    </select>
                </label>
                <button onClick={refresh} disabled={loading}>
                    {loading ? "Refreshing..." : "Refresh"}
                </button>
            </div>

            {error && (
                <div role="alert" style={{ color: "#c33", padding: "8px 0" }}>
                    {error}
                </div>
            )}

            {loading && !surface ? (
                <p>Loading market surface...</p>
            ) : sortedModels.length === 0 ? (
                <p style={{ color: "#888" }}>
                    No providers on the market for this filter. The network may be new or
                    the Wire may not have cached any offers yet.
                </p>
            ) : (
                <div>
                    {sortedModels.map((model) => (
                        <ModelCard key={model.model_id} model={model} />
                    ))}
                </div>
            )}

            {surface?.fetched_at && (
                <p style={{ color: "#888", fontSize: 11, marginTop: 16 }}>
                    Fetched at {surface.fetched_at}
                </p>
            )}
        </div>
    );
}

function sortModels(models: MarketSurfaceModel[], field: SortField): MarketSurfaceModel[] {
    const copy = [...models];
    switch (field) {
        case "price":
            copy.sort((a, b) => (a.min_rate_output ?? Infinity) - (b.min_rate_output ?? Infinity));
            break;
        case "queue":
            copy.sort((a, b) => (a.total_queue_depth ?? Infinity) - (b.total_queue_depth ?? Infinity));
            break;
        case "speed":
            copy.sort((a, b) => (b.median_tps ?? 0) - (a.median_tps ?? 0));
            break;
    }
    return copy;
}

function ModelCard({ model }: { model: MarketSurfaceModel }) {
    const [expanded, setExpanded] = useState(false);
    const providerCount = model.provider_count ?? (model.providers?.length ?? 0);

    return (
        <div
            style={{
                border: "1px solid #ddd",
                borderRadius: 6,
                padding: 12,
                marginBottom: 12,
            }}
        >
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline" }}>
                <h3 style={{ margin: 0 }}>{model.model_id}</h3>
                <div style={{ display: "flex", gap: 12, color: "#555", fontSize: 13 }}>
                    <span>{providerCount} provider{providerCount === 1 ? "" : "s"}</span>
                    {model.median_tps != null && (
                        <span>{formatNumber(model.median_tps, 0)} tok/s median</span>
                    )}
                    {model.p95_latency_ms != null && (
                        <span>{formatLatency(model.p95_latency_ms)} p95</span>
                    )}
                </div>
            </div>

            <div style={{ display: "flex", gap: 16, marginTop: 8, fontSize: 14 }}>
                {model.min_rate_input != null && model.max_rate_input != null && (
                    <div>
                        <strong>Input:</strong>{" "}
                        {model.min_rate_input === model.max_rate_input
                            ? `${model.min_rate_input}`
                            : `${model.min_rate_input}–${model.max_rate_input}`}{" "}
                        credits / M tokens
                    </div>
                )}
                {model.min_rate_output != null && model.max_rate_output != null && (
                    <div>
                        <strong>Output:</strong>{" "}
                        {model.min_rate_output === model.max_rate_output
                            ? `${model.min_rate_output}`
                            : `${model.min_rate_output}–${model.max_rate_output}`}{" "}
                        credits / M tokens
                    </div>
                )}
                {model.total_queue_depth != null && (
                    <div>
                        <strong>Total depth:</strong> {model.total_queue_depth}
                    </div>
                )}
            </div>

            {(model.providers?.length ?? 0) > 0 && (
                <button
                    onClick={() => setExpanded((x) => !x)}
                    style={{ marginTop: 8, fontSize: 13 }}
                >
                    {expanded ? "Hide" : "Show"} per-provider pricing
                </button>
            )}

            {expanded && model.providers && (
                <table style={{ width: "100%", marginTop: 8, borderCollapse: "collapse" }}>
                    <thead>
                        <tr>
                            <th style={cellStyle}>Node</th>
                            <th style={cellStyle}>Type</th>
                            <th style={cellStyle}>Input / M</th>
                            <th style={cellStyle}>Output / M</th>
                            <th style={cellStyle}>Queue</th>
                            <th style={cellStyle}>Median TPS</th>
                            <th style={cellStyle}>p95 Latency</th>
                            <th style={cellStyle}>Obs</th>
                        </tr>
                    </thead>
                    <tbody>
                        {model.providers.map((p, idx) => (
                            <tr key={`${p.node_id ?? ""}-${idx}`}>
                                <td style={cellStyle}>{shortNode(p.node_id)}</td>
                                <td style={cellStyle}>{p.provider_type ?? "?"}</td>
                                <td style={cellStyle}>{p.rate_per_m_input ?? "—"}</td>
                                <td style={cellStyle}>{p.rate_per_m_output ?? "—"}</td>
                                <td style={cellStyle}>
                                    {p.queue_depth ?? "—"}
                                    {p.max_queue_depth != null && ` / ${p.max_queue_depth}`}
                                </td>
                                <td style={cellStyle}>{formatNumber(p.median_tps, 0)}</td>
                                <td style={cellStyle}>{formatLatency(p.p95_latency_ms)}</td>
                                <td style={cellStyle} title="observation count (confidence)">
                                    {p.observation_count ?? 0}
                                </td>
                            </tr>
                        ))}
                    </tbody>
                </table>
            )}
        </div>
    );
}

function shortNode(node_id: string | undefined): string {
    if (!node_id) return "?";
    if (node_id.length <= 12) return node_id;
    return `${node_id.slice(0, 6)}…${node_id.slice(-4)}`;
}

function formatNumber(n: number | null | undefined, decimals: number): string {
    if (n == null || !Number.isFinite(n)) return "—";
    return n.toFixed(decimals);
}

function formatLatency(ms: number | null | undefined): string {
    if (ms == null || !Number.isFinite(ms)) return "—";
    if (ms < 1000) return `${Math.round(ms)}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
}

const cellStyle: React.CSSProperties = {
    padding: "4px 6px",
    borderBottom: "1px solid #eee",
    textAlign: "left",
    fontSize: 13,
};
