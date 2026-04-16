// ComputeChronicle.tsx — Persistent compute observability.
// Renders event history table, filter bar, stats cards, and fleet analytics.

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

// ── Types ──────────────────────────────────────────────────────────────

interface ComputeEvent {
    id: number;
    job_path: string;
    event_type: string;
    timestamp: string;
    model_id: string | null;
    source: string;
    slug: string | null;
    build_id: string | null;
    chain_name: string | null;
    content_type: string | null;
    step_name: string | null;
    primitive: string | null;
    depth: number | null;
    task_label: string | null;
    metadata: Record<string, unknown> | null;
}

interface ComputeSummary {
    group_key: string;
    total_events: number;
    completed_count: number;
    failed_count: number;
    total_latency_ms: number;
    avg_latency_ms: number;
    total_tokens_prompt: number;
    total_tokens_completion: number;
    total_cost_usd: number;
    fleet_count: number;
    local_count: number;
    cloud_count: number;
}

interface ChronicleDimensions {
    slugs: string[];
    models: string[];
    sources: string[];
    chain_names: string[];
    event_types: string[];
}

// ── Helpers ────────────────────────────────────────────────────────────

const SOURCE_COLORS: Record<string, string> = {
    local: '#00D4FF',
    fleet: '#A855F7',
    cloud: '#6B7280',
    fleet_received: '#0891B2',
    market: '#EAB308',
    market_received: '#CA8A04',
};

const EVENT_TYPE_COLORS: Record<string, string> = {
    // Local lifecycle
    enqueued: '#6B7280',
    started: '#3B82F6',
    completed: '#22C55E',
    failed: '#EF4444',
    cloud_returned: '#9CA3AF',

    // Dispatcher-side (my node sent the job out) — purple/violet hues
    fleet_dispatched_async: '#A855F7',
    fleet_peer_overloaded: '#F59E0B',
    fleet_dispatch_timeout: '#F97316',
    fleet_dispatch_failed: '#DC2626',
    fleet_result_received: '#8B5CF6',
    fleet_result_failed: '#EF4444',
    fleet_result_orphaned: '#D97706',
    fleet_result_forgery_attempt: '#B91C1C',
    fleet_pending_orphaned: '#B45309',

    // Peer-side (my node is serving a job for someone else) — teal/cyan hues
    fleet_job_accepted: '#0891B2',
    fleet_admission_rejected: '#E11D48',
    fleet_job_completed: '#14B8A6',
    fleet_callback_delivered: '#0EA5E9',
    fleet_callback_failed: '#EF4444',
    fleet_callback_exhausted: '#991B1B',

    // Worker / delivery health — amber for lost, red for hard failures
    fleet_worker_heartbeat_lost: '#F59E0B',
    fleet_worker_sweep_lost: '#EA580C',
    fleet_delivery_cas_lost: '#DC2626',
};

function formatTimestamp(ts: string): string {
    try {
        const d = new Date(ts);
        return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    } catch { return ts; }
}

function formatMs(ms: unknown): string {
    if (typeof ms !== 'number') return '-';
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
}

function formatCost(cost: unknown): string {
    if (typeof cost !== 'number' || cost === 0) return '-';
    if (cost < 0.01) return `$${cost.toFixed(4)}`;
    return `$${cost.toFixed(2)}`;
}

function truncateJobPath(path: string, max = 30): string {
    if (path.length <= max) return path;
    return path.slice(0, max - 2) + '..';
}

function shortModel(model: string | null): string {
    if (!model) return '-';
    const parts = model.split('/');
    return parts[parts.length - 1] ?? model;
}

// ── Time range presets ────────────────────────────────────────────────

type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d';

function getTimeRange(range: TimeRange): { after: string; before: string; bucketMinutes: number } {
    const now = new Date();
    const ms: Record<TimeRange, number> = {
        '1h': 3600_000,
        '6h': 21600_000,
        '24h': 86400_000,
        '7d': 604800_000,
        '30d': 2592000_000,
    };
    const buckets: Record<TimeRange, number> = {
        '1h': 1,
        '6h': 5,
        '24h': 15,
        '7d': 60,
        '30d': 360,
    };
    const start = new Date(now.getTime() - ms[range]);
    return {
        after: start.toISOString(),
        before: now.toISOString(),
        bucketMinutes: buckets[range],
    };
}

// ── Main Component ────────────────────────────────────────────────────

type ChronicleView = 'table' | 'fleet';

export function ComputeChronicle() {
    const [view, setView] = useState<ChronicleView>('table');
    const [events, setEvents] = useState<ComputeEvent[]>([]);
    const [summary, setSummary] = useState<ComputeSummary[]>([]);
    const [dimensions, setDimensions] = useState<ChronicleDimensions | null>(null);
    const [loading, setLoading] = useState(false);

    // Filters
    const [timeRange, setTimeRange] = useState<TimeRange>('24h');
    const [filterSlug, setFilterSlug] = useState<string>('');
    const [filterModel, setFilterModel] = useState<string>('');
    const [filterSource, setFilterSource] = useState<string>('');
    const [filterEventType, setFilterEventType] = useState<string>('');

    // Pagination
    const [page, setPage] = useState(0);
    const pageSize = 50;

    const fetchDimensions = useCallback(async () => {
        try {
            const dims = await invoke<ChronicleDimensions>('get_chronicle_dimensions');
            setDimensions(dims);
        } catch { /* dimensions not available yet */ }
    }, []);

    const fetchEvents = useCallback(async () => {
        setLoading(true);
        try {
            const range = getTimeRange(timeRange);
            const params: Record<string, unknown> = {
                after: range.after,
                before: range.before,
                limit: pageSize,
                offset: page * pageSize,
            };
            if (filterSlug) params.slug = filterSlug;
            if (filterModel) params.modelId = filterModel;
            if (filterSource) params.source = filterSource;
            if (filterEventType) params.eventType = filterEventType;

            const data = await invoke<ComputeEvent[]>('get_compute_events', params);
            setEvents(data);
        } catch { setEvents([]); }
        setLoading(false);
    }, [timeRange, filterSlug, filterModel, filterSource, filterEventType, page]);

    const fetchSummary = useCallback(async () => {
        try {
            const range = getTimeRange(timeRange);
            const data = await invoke<ComputeSummary[]>('get_compute_summary', {
                periodStart: range.after,
                periodEnd: range.before,
                groupBy: 'source',
            });
            setSummary(data);
        } catch { setSummary([]); }
    }, [timeRange]);

    useEffect(() => {
        fetchDimensions();
    }, [fetchDimensions]);

    useEffect(() => {
        fetchEvents();
        fetchSummary();
    }, [fetchEvents, fetchSummary]);

    // Aggregate stats from summary
    const totalCompleted = summary.reduce((s, r) => s + r.completed_count, 0);
    const totalFailed = summary.reduce((s, r) => s + r.failed_count, 0);
    const totalCost = summary.reduce((s, r) => s + r.total_cost_usd, 0);
    const avgLatency = summary.length > 0
        ? summary.reduce((s, r) => s + r.avg_latency_ms * r.completed_count, 0) /
          Math.max(totalCompleted, 1)
        : 0;
    const fleetCount = summary.reduce((s, r) => s + r.fleet_count, 0);
    const localCount = summary.reduce((s, r) => s + r.local_count, 0);
    const cloudCount = summary.reduce((s, r) => s + r.cloud_count, 0);

    return (
        <div className="chronicle-container">
            {/* Sub-navigation */}
            <div className="chronicle-view-tabs">
                <button
                    className={`chronicle-view-tab ${view === 'table' ? 'chronicle-view-tab-active' : ''}`}
                    onClick={() => setView('table')}
                >
                    History
                </button>
                <button
                    className={`chronicle-view-tab ${view === 'fleet' ? 'chronicle-view-tab-active' : ''}`}
                    onClick={() => setView('fleet')}
                >
                    Fleet Analytics
                </button>
            </div>

            {/* Stats cards */}
            <div className="chronicle-stats">
                <div className="chronicle-stat-card">
                    <div className="chronicle-stat-label">Completed</div>
                    <div className="chronicle-stat-value">{totalCompleted}</div>
                </div>
                <div className="chronicle-stat-card">
                    <div className="chronicle-stat-label">Failed</div>
                    <div className="chronicle-stat-value chronicle-stat-failed">{totalFailed}</div>
                </div>
                <div className="chronicle-stat-card">
                    <div className="chronicle-stat-label">Avg Latency</div>
                    <div className="chronicle-stat-value">{formatMs(avgLatency)}</div>
                </div>
                <div className="chronicle-stat-card">
                    <div className="chronicle-stat-label">Cost</div>
                    <div className="chronicle-stat-value">{formatCost(totalCost)}</div>
                </div>
                <div className="chronicle-stat-card">
                    <div className="chronicle-stat-label">Sources</div>
                    <div className="chronicle-stat-breakdown">
                        {localCount > 0 && <span style={{ color: SOURCE_COLORS.local }}>Local: {localCount}</span>}
                        {fleetCount > 0 && <span style={{ color: SOURCE_COLORS.fleet }}>Fleet: {fleetCount}</span>}
                        {cloudCount > 0 && <span style={{ color: SOURCE_COLORS.cloud }}>Cloud: {cloudCount}</span>}
                        {localCount === 0 && fleetCount === 0 && cloudCount === 0 && <span>-</span>}
                    </div>
                </div>
            </div>

            {/* Filter bar */}
            <div className="chronicle-filters">
                <div className="chronicle-time-range">
                    {(['1h', '6h', '24h', '7d', '30d'] as TimeRange[]).map(r => (
                        <button
                            key={r}
                            className={`chronicle-time-btn ${timeRange === r ? 'chronicle-time-btn-active' : ''}`}
                            onClick={() => { setTimeRange(r); setPage(0); }}
                        >
                            {r}
                        </button>
                    ))}
                </div>

                <select
                    className="chronicle-filter-select"
                    value={filterSlug}
                    onChange={e => { setFilterSlug(e.target.value); setPage(0); }}
                >
                    <option value="">All Pyramids</option>
                    {dimensions?.slugs.map(s => <option key={s} value={s}>{s}</option>)}
                </select>

                <select
                    className="chronicle-filter-select"
                    value={filterModel}
                    onChange={e => { setFilterModel(e.target.value); setPage(0); }}
                >
                    <option value="">All Models</option>
                    {dimensions?.models.map(m => <option key={m} value={m}>{shortModel(m)}</option>)}
                </select>

                <select
                    className="chronicle-filter-select"
                    value={filterSource}
                    onChange={e => { setFilterSource(e.target.value); setPage(0); }}
                >
                    <option value="">All Sources</option>
                    {dimensions?.sources.map(s => <option key={s} value={s}>{s}</option>)}
                </select>

                <select
                    className="chronicle-filter-select"
                    value={filterEventType}
                    onChange={e => { setFilterEventType(e.target.value); setPage(0); }}
                >
                    <option value="">All Events</option>
                    {dimensions?.event_types.map(t => <option key={t} value={t}>{t}</option>)}
                </select>

                <button className="chronicle-refresh-btn" onClick={() => { fetchEvents(); fetchSummary(); fetchDimensions(); }}>
                    Refresh
                </button>
            </div>

            {/* Content */}
            {view === 'table' && (
                <ChronicleTable
                    events={events}
                    loading={loading}
                    page={page}
                    pageSize={pageSize}
                    onPageChange={setPage}
                />
            )}
            {view === 'fleet' && (
                <FleetAnalytics summary={summary} />
            )}
        </div>
    );
}

// ── Event Table ───────────────────────────────────────────────────────

function ChronicleTable({
    events,
    loading,
    page,
    pageSize,
    onPageChange,
}: {
    events: ComputeEvent[];
    loading: boolean;
    page: number;
    pageSize: number;
    onPageChange: (p: number) => void;
}) {
    return (
        <div className="chronicle-table-container">
            {loading && <div className="chronicle-loading">Loading...</div>}
            <table className="chronicle-table">
                <thead>
                    <tr>
                        <th>Time</th>
                        <th>Job</th>
                        <th>Type</th>
                        <th>Model</th>
                        <th>Source</th>
                        <th>Pyramid</th>
                        <th>Step</th>
                        <th>Latency</th>
                        <th>Tokens</th>
                        <th>Cost</th>
                    </tr>
                </thead>
                <tbody>
                    {events.length === 0 && !loading && (
                        <tr>
                            <td colSpan={10} className="chronicle-empty">
                                No compute events found for this time range.
                            </td>
                        </tr>
                    )}
                    {events.map(evt => (
                        <tr key={evt.id} className="chronicle-row">
                            <td className="chronicle-cell-time">{formatTimestamp(evt.timestamp)}</td>
                            <td className="chronicle-cell-job" title={evt.job_path}>
                                {truncateJobPath(evt.job_path)}
                            </td>
                            <td>
                                <span
                                    className="chronicle-event-badge"
                                    style={{ backgroundColor: EVENT_TYPE_COLORS[evt.event_type] ?? '#6B7280' }}
                                >
                                    {evt.event_type}
                                </span>
                            </td>
                            <td className="chronicle-cell-model">{shortModel(evt.model_id)}</td>
                            <td>
                                <span
                                    className="chronicle-source-dot"
                                    style={{ backgroundColor: SOURCE_COLORS[evt.source] ?? '#6B7280' }}
                                />
                                {evt.source}
                            </td>
                            <td className="chronicle-cell-slug">{evt.slug ?? '-'}</td>
                            <td className="chronicle-cell-step">{evt.step_name ?? '-'}</td>
                            <td className="chronicle-cell-latency">
                                {formatMs(evt.metadata?.latency_ms)}
                            </td>
                            <td className="chronicle-cell-tokens">
                                {typeof evt.metadata?.tokens_prompt === 'number'
                                    ? `${evt.metadata.tokens_prompt}/${evt.metadata?.tokens_completion ?? 0}`
                                    : '-'}
                            </td>
                            <td className="chronicle-cell-cost">
                                {formatCost(evt.metadata?.cost_usd)}
                            </td>
                        </tr>
                    ))}
                </tbody>
            </table>

            {/* Pagination */}
            <div className="chronicle-pagination">
                <button
                    disabled={page === 0}
                    onClick={() => onPageChange(page - 1)}
                    className="chronicle-page-btn"
                >
                    Previous
                </button>
                <span className="chronicle-page-info">
                    Page {page + 1} ({events.length} events)
                </span>
                <button
                    disabled={events.length < pageSize}
                    onClick={() => onPageChange(page + 1)}
                    className="chronicle-page-btn"
                >
                    Next
                </button>
            </div>
        </div>
    );
}

// ── Fleet Analytics ───────────────────────────────────────────────────

function FleetAnalytics({ summary }: { summary: ComputeSummary[] }) {
    const fleetSummary = summary.find(s => s.group_key === 'fleet');
    const localSummary = summary.find(s => s.group_key === 'local');
    const cloudSummary = summary.find(s => s.group_key === 'cloud');
    const fleetReceivedSummary = summary.find(s => s.group_key === 'fleet_received');

    return (
        <div className="chronicle-fleet-analytics">
            <h3 className="chronicle-fleet-title">Fleet Dispatch Analytics</h3>

            <div className="chronicle-fleet-grid">
                {/* Dispatch stats */}
                <div className="chronicle-fleet-card">
                    <div className="chronicle-fleet-card-title">Fleet Dispatched</div>
                    <div className="chronicle-fleet-card-value">
                        {fleetSummary?.total_events ?? 0} events
                    </div>
                    <div className="chronicle-fleet-card-detail">
                        Completed: {fleetSummary?.completed_count ?? 0} |
                        Failed: {fleetSummary?.failed_count ?? 0}
                    </div>
                    {fleetSummary && fleetSummary.total_events > 0 && (
                        <div className="chronicle-fleet-card-detail">
                            Avg latency: {formatMs(fleetSummary.avg_latency_ms)}
                        </div>
                    )}
                </div>

                {/* Fleet received stats */}
                <div className="chronicle-fleet-card">
                    <div className="chronicle-fleet-card-title">Fleet Received</div>
                    <div className="chronicle-fleet-card-value">
                        {fleetReceivedSummary?.total_events ?? 0} jobs served
                    </div>
                    <div className="chronicle-fleet-card-detail">
                        Completed: {fleetReceivedSummary?.completed_count ?? 0}
                    </div>
                </div>

                {/* Latency comparison */}
                <div className="chronicle-fleet-card">
                    <div className="chronicle-fleet-card-title">Latency Comparison</div>
                    <div className="chronicle-fleet-latency-bars">
                        {localSummary && localSummary.completed_count > 0 && (
                            <div className="chronicle-fleet-latency-row">
                                <span className="chronicle-fleet-latency-label" style={{ color: SOURCE_COLORS.local }}>
                                    Local
                                </span>
                                <span>{formatMs(localSummary.avg_latency_ms)}</span>
                            </div>
                        )}
                        {fleetSummary && fleetSummary.completed_count > 0 && (
                            <div className="chronicle-fleet-latency-row">
                                <span className="chronicle-fleet-latency-label" style={{ color: SOURCE_COLORS.fleet }}>
                                    Fleet
                                </span>
                                <span>{formatMs(fleetSummary.avg_latency_ms)}</span>
                            </div>
                        )}
                        {cloudSummary && cloudSummary.completed_count > 0 && (
                            <div className="chronicle-fleet-latency-row">
                                <span className="chronicle-fleet-latency-label" style={{ color: SOURCE_COLORS.cloud }}>
                                    Cloud
                                </span>
                                <span>{formatMs(cloudSummary.avg_latency_ms)}</span>
                            </div>
                        )}
                        {!localSummary && !fleetSummary && !cloudSummary && (
                            <div className="chronicle-fleet-latency-row">No data yet</div>
                        )}
                    </div>
                </div>

                {/* Cost breakdown */}
                <div className="chronicle-fleet-card">
                    <div className="chronicle-fleet-card-title">Cost by Source</div>
                    <div className="chronicle-fleet-latency-bars">
                        {summary.filter(s => s.total_cost_usd > 0).map(s => (
                            <div key={s.group_key} className="chronicle-fleet-latency-row">
                                <span
                                    className="chronicle-fleet-latency-label"
                                    style={{ color: SOURCE_COLORS[s.group_key] ?? '#6B7280' }}
                                >
                                    {s.group_key}
                                </span>
                                <span>{formatCost(s.total_cost_usd)}</span>
                            </div>
                        ))}
                        {summary.every(s => s.total_cost_usd === 0) && (
                            <div className="chronicle-fleet-latency-row">No costs recorded</div>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
}
