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
// missing fields. Phase 3 will firm up the contract.

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
        // Auto-poll every 30s so new offers appearing on the network show
        // up without manual refresh. Matches the dashboard's IPC poll
        // cadence (REFRESH_INTERVAL_MS in ComputeMarketDashboard).
        // Cheap — unauthed public endpoint with 5-min Wire-side cache.
        const handle = setInterval(() => void refresh(), 30_000);
        return () => clearInterval(handle);
    }, [refresh]);

    const sortedModels = sortModels(surface?.models ?? [], sortBy);

    return (
        <div className="compute-surface-panel">
            <div className="compute-surface-header">
                <div className="compute-surface-header-text">
                    <h3 className="compute-section-title">Market surface</h3>
                    <p className="compute-section-sub">
                        Browse compute providers across the network. Read-only view of
                        who's helping with which models right now — sorted by price,
                        latency, or queue depth.
                    </p>
                </div>
                <button
                    className="compute-ghost-btn compute-ghost-btn-sm"
                    onClick={refresh}
                    disabled={loading}
                    title="Refetch market surface from the Wire"
                >
                    {loading ? "…" : "Refresh"}
                </button>
            </div>

            <div className="compute-surface-filters">
                <div className="compute-surface-filter-field">
                    <label className="compute-field-label" htmlFor="compute-surface-filter">
                        Filter by model
                    </label>
                    <input
                        id="compute-surface-filter"
                        className="compute-input"
                        type="text"
                        value={modelFilter}
                        onChange={(e) => setModelFilter(e.target.value)}
                        placeholder="Optional — e.g. gemma3"
                    />
                </div>
                <div className="compute-surface-filter-field">
                    <label className="compute-field-label" htmlFor="compute-surface-sort">
                        Sort
                    </label>
                    <select
                        id="compute-surface-sort"
                        className="compute-input"
                        value={sortBy}
                        onChange={(e) => setSortBy(e.target.value as SortField)}
                    >
                        <option value="price">Price (lowest first)</option>
                        <option value="queue">Queue depth (shortest first)</option>
                        <option value="speed">Speed (fastest first)</option>
                    </select>
                </div>
            </div>

            {error && (
                <div className="compute-market-error" role="alert">
                    {error}
                </div>
            )}

            {loading && !surface ? (
                <div className="compute-empty">Loading market surface…</div>
            ) : sortedModels.length === 0 ? (
                <div className="compute-empty">
                    <div className="compute-empty-title">No providers on the market</div>
                    <div className="compute-empty-desc">
                        The network may be new, or the Wire hasn't cached any offers matching
                        this filter. Providers surface on the Wire when they start serving.
                    </div>
                </div>
            ) : (
                <div className="compute-surface-list">
                    {sortedModels.map((model) => (
                        <ModelCard key={model.model_id} model={model} />
                    ))}
                </div>
            )}

            {surface?.fetched_at && (
                <div className="compute-surface-fetched">
                    Fetched at {surface.fetched_at}
                </div>
            )}
        </div>
    );
}

function sortModels(models: MarketSurfaceModel[], field: SortField): MarketSurfaceModel[] {
    const copy = [...models];
    switch (field) {
        case "price":
            copy.sort(
                (a, b) => (a.min_rate_output ?? Infinity) - (b.min_rate_output ?? Infinity),
            );
            break;
        case "queue":
            copy.sort(
                (a, b) => (a.total_queue_depth ?? Infinity) - (b.total_queue_depth ?? Infinity),
            );
            break;
        case "speed":
            copy.sort((a, b) => (b.median_tps ?? 0) - (a.median_tps ?? 0));
            break;
    }
    return copy;
}

function ModelCard({ model }: { model: MarketSurfaceModel }) {
    const [expanded, setExpanded] = useState(false);
    const providerCount = model.provider_count ?? model.providers?.length ?? 0;

    return (
        <div className="compute-surface-card">
            <div className="compute-surface-card-header">
                <h4 className="compute-surface-card-model">{model.model_id}</h4>
                <div className="compute-surface-card-meta">
                    <span>
                        {providerCount} provider{providerCount === 1 ? "" : "s"}
                    </span>
                    {model.median_tps != null && (
                        <span>{formatNumber(model.median_tps, 0)} tok/s median</span>
                    )}
                    {model.p95_latency_ms != null && (
                        <span>{formatLatency(model.p95_latency_ms)} p95</span>
                    )}
                </div>
            </div>

            <dl className="compute-surface-card-stats">
                {model.min_rate_input != null && model.max_rate_input != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Input</dt>
                        <dd className="compute-mono">
                            {formatRange(model.min_rate_input, model.max_rate_input)}
                        </dd>
                    </div>
                )}
                {model.min_rate_output != null && model.max_rate_output != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Output</dt>
                        <dd className="compute-mono">
                            {formatRange(model.min_rate_output, model.max_rate_output)}
                        </dd>
                    </div>
                )}
                {model.total_queue_depth != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Total queue</dt>
                        <dd className="compute-mono">{model.total_queue_depth}</dd>
                    </div>
                )}
            </dl>

            {(model.providers?.length ?? 0) > 0 && (
                <>
                    <button
                        className="compute-ghost-btn compute-ghost-btn-sm compute-surface-card-toggle"
                        onClick={() => setExpanded((x) => !x)}
                    >
                        {expanded ? "Hide" : "Show"} per-provider pricing
                    </button>
                    {expanded && model.providers && (
                        <div className="compute-surface-provider-table">
                            <div className="compute-surface-provider-row compute-surface-provider-head">
                                <div>Node</div>
                                <div>Type</div>
                                <div className="compute-col-num">Input / M</div>
                                <div className="compute-col-num">Output / M</div>
                                <div className="compute-col-num">Queue</div>
                                <div className="compute-col-num">TPS</div>
                                <div className="compute-col-num">p95</div>
                                <div className="compute-col-num">Obs</div>
                            </div>
                            {model.providers.map((p, idx) => (
                                <div
                                    className="compute-surface-provider-row"
                                    key={`${p.node_id ?? ""}-${idx}`}
                                >
                                    <div className="compute-surface-provider-node" title={p.node_id}>
                                        {shortNode(p.node_id)}
                                    </div>
                                    <div>{p.provider_type ?? "?"}</div>
                                    <div className="compute-col-num compute-mono">
                                        {p.rate_per_m_input ?? "—"}
                                    </div>
                                    <div className="compute-col-num compute-mono">
                                        {p.rate_per_m_output ?? "—"}
                                    </div>
                                    <div className="compute-col-num compute-mono">
                                        {p.queue_depth ?? "—"}
                                        {p.max_queue_depth != null && (
                                            <span className="compute-surface-provider-cap">
                                                /{p.max_queue_depth}
                                            </span>
                                        )}
                                    </div>
                                    <div className="compute-col-num compute-mono">
                                        {formatNumber(p.median_tps, 0)}
                                    </div>
                                    <div className="compute-col-num compute-mono">
                                        {formatLatency(p.p95_latency_ms)}
                                    </div>
                                    <div
                                        className="compute-col-num compute-mono"
                                        title="observation count (confidence)"
                                    >
                                        {p.observation_count ?? 0}
                                    </div>
                                </div>
                            ))}
                        </div>
                    )}
                </>
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

function formatRange(min: number, max: number): string {
    return min === max ? `${min}` : `${min}–${max}`;
}
