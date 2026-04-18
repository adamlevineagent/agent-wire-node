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

interface PriceRange {
    min?: number;
    median?: number;
    max?: number;
}

interface MarketSurfaceModel {
    model_id: string;
    // Wire contract rev 1.5 shape.
    active_offers?: number;
    providers?: number;  // count, not array — matches Wire /market-surface
    price?: {
        rate_per_m_input?: PriceRange;
        rate_per_m_output?: PriceRange;
    };
    queue?: {
        total_capacity?: number;
        current_depth?: number;
        unbounded_offers?: number;
    };
    performance?: {
        p50_latency_ms?: number | null;
        p95_latency_ms?: number | null;
        median_tps?: number | null;
    };
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
            copy.sort((a, b) => {
                const aOut = a.price?.rate_per_m_output?.min ?? Infinity;
                const bOut = b.price?.rate_per_m_output?.min ?? Infinity;
                return aOut - bOut;
            });
            break;
        case "queue":
            copy.sort((a, b) => {
                const aQ = a.queue?.current_depth ?? Infinity;
                const bQ = b.queue?.current_depth ?? Infinity;
                return aQ - bQ;
            });
            break;
        case "speed":
            copy.sort((a, b) => {
                const aT = a.performance?.median_tps ?? 0;
                const bT = b.performance?.median_tps ?? 0;
                return bT - aT;
            });
            break;
    }
    return copy;
}

function ModelCard({ model }: { model: MarketSurfaceModel }) {
    // Wire's public /market-surface returns `providers` as a COUNT, not
    // an array. `active_offers` is also a count (multiple offers per
    // provider is possible but we surface providers as the headline).
    const providerCount = model.providers ?? 0;
    const activeOffers = model.active_offers ?? 0;

    const priceIn = model.price?.rate_per_m_input;
    const priceOut = model.price?.rate_per_m_output;
    const tps = model.performance?.median_tps;
    const p95 = model.performance?.p95_latency_ms;
    const p50 = model.performance?.p50_latency_ms;
    const currentDepth = model.queue?.current_depth;
    const totalCapacity = model.queue?.total_capacity;

    return (
        <div className="compute-surface-card">
            <div className="compute-surface-card-header">
                <h4 className="compute-surface-card-model">{model.model_id}</h4>
                <div className="compute-surface-card-meta">
                    <span>
                        {providerCount} provider{providerCount === 1 ? "" : "s"}
                    </span>
                    {activeOffers > 0 && activeOffers !== providerCount && (
                        <span>
                            {activeOffers} offer{activeOffers === 1 ? "" : "s"}
                        </span>
                    )}
                    {tps != null && (
                        <span>{formatNumber(tps, 0)} tok/s median</span>
                    )}
                    {p95 != null && (
                        <span>{formatLatency(p95)} p95</span>
                    )}
                </div>
            </div>

            <dl className="compute-surface-card-stats">
                {priceIn && priceIn.min != null && priceIn.max != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Input</dt>
                        <dd className="compute-mono">
                            {formatRange(priceIn.min, priceIn.max)}
                        </dd>
                    </div>
                )}
                {priceOut && priceOut.min != null && priceOut.max != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Output</dt>
                        <dd className="compute-mono">
                            {formatRange(priceOut.min, priceOut.max)}
                        </dd>
                    </div>
                )}
                {currentDepth != null && totalCapacity != null && (
                    <div className="compute-surface-card-stat">
                        <dt>Queue</dt>
                        <dd className="compute-mono">
                            {currentDepth}/{totalCapacity}
                        </dd>
                    </div>
                )}
                {p50 != null && (
                    <div className="compute-surface-card-stat">
                        <dt>p50 latency</dt>
                        <dd className="compute-mono">{formatLatency(p50)}</dd>
                    </div>
                )}
            </dl>

            {/* Per-provider detail is only available on the authed
                /market-surface/detailed endpoint. The public aggregate
                surface (this component) exposes counts + ranges only. */}
        </div>
    );
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
