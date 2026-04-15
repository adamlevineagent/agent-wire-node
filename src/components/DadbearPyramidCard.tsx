// Phase 6 (Canonical) — Per-pyramid card for the DADBEAR Oversight Page.
//
// Renders a single `WorkItemOverviewRow` as a compact work-pipeline card
// with hold list, pipeline counts, cost, and pause/resume buttons.
// Now uses the canonical holds-based status instead of boolean flags.

import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { WorkItemOverviewRow } from '../hooks/useDadbearOverviewV2';

interface DadbearPyramidCardProps {
    row: WorkItemOverviewRow;
    onViewActivity: (slug: string) => void;
    onConfigure: (slug: string) => void;
    onMutated: () => void;
}

function timeAgo(iso: string | null): string {
    if (!iso) return 'never';
    const normalized = iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z';
    const diff = Date.now() - new Date(normalized).getTime();
    if (Number.isNaN(diff)) return iso;
    const s = Math.floor(diff / 1000);
    if (s < 60) return `${s}s ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    const d = Math.floor(h / 24);
    return `${d}d ago`;
}

function currency(v: number): string {
    return `$${v.toFixed(2)}`;
}

export function DadbearPyramidCard({
    row,
    onViewActivity,
    onConfigure,
    onMutated,
}: DadbearPyramidCardProps) {
    const [busy, setBusy] = useState(false);
    const [localError, setLocalError] = useState<string | null>(null);

    // Derived from holds: any hold means paused from dispatch perspective
    const isPaused = row.derived_status !== 'active';

    const handleToggle = useCallback(async () => {
        setBusy(true);
        setLocalError(null);
        try {
            if (!isPaused) {
                await invoke('pyramid_dadbear_pause', { slug: row.slug });
            } else {
                await invoke('pyramid_dadbear_resume', { slug: row.slug });
            }
            onMutated();
        } catch (e) {
            setLocalError(String(e));
        } finally {
            setBusy(false);
        }
    }, [isPaused, row.slug, onMutated]);

    const statusText =
        row.derived_status === 'breaker'
            ? 'Breaker'
            : row.derived_status === 'paused'
                ? 'Paused'
                : row.derived_status === 'held'
                    ? 'Held'
                    : 'Active';
    const statusCls =
        row.derived_status === 'breaker'
            ? 'dadbear-card-status-breaker'
            : row.derived_status === 'paused' || row.derived_status === 'held'
                ? 'dadbear-card-status-paused'
                : 'dadbear-card-status-active';

    const cardClass = [
        'dadbear-card',
        row.derived_status === 'breaker'
            ? 'dadbear-card-breaker'
            : row.derived_status === 'paused' || row.derived_status === 'held'
                ? 'dadbear-card-paused'
                : 'dadbear-card-active',
    ]
        .filter(Boolean)
        .join(' ');

    return (
        <div className={cardClass}>
            {row.derived_status === 'breaker' && (
                <div className="dadbear-card-breaker-banner">Breaker tripped — resume to clear</div>
            )}
            <div className="dadbear-card-header">
                <h3 className="dadbear-card-title">{row.display_name}</h3>
                <span className={`dadbear-card-status ${statusCls}`}>
                    {statusText}
                </span>
            </div>

            {/* Hold list with reasons and timestamps */}
            {row.holds.length > 0 && (
                <div className="dadbear-card-holds">
                    {row.holds.map((h) => (
                        <div key={h.hold} className="dadbear-card-hold-row">
                            <span className="dadbear-card-hold-type">{h.hold}</span>
                            <span className="dadbear-card-hold-since">{timeAgo(h.held_since)}</span>
                            {h.reason && (
                                <span className="dadbear-card-hold-reason">{h.reason}</span>
                            )}
                        </div>
                    ))}
                </div>
            )}

            {/* Pipeline counts grid */}
            <div className="dadbear-card-grid">
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Pending obs</span>
                    <span className="dadbear-card-field-value">
                        {row.pending_observations}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Compiled</span>
                    <span className="dadbear-card-field-value">
                        {row.compiled_items}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Blocked</span>
                    <span className="dadbear-card-field-value">
                        {row.blocked_items}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Previewed</span>
                    <span className="dadbear-card-field-value">
                        {row.previewed_items}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Dispatched</span>
                    <span className="dadbear-card-field-value">
                        {row.dispatched_items}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Completed (24h)</span>
                    <span className="dadbear-card-field-value">
                        {row.completed_items_24h}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Applied (24h)</span>
                    <span className="dadbear-card-field-value">
                        {row.applied_items_24h}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Failed (24h)</span>
                    <span className="dadbear-card-field-value">
                        {row.failed_items_24h}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Stale</span>
                    <span className="dadbear-card-field-value">
                        {row.stale_items}
                    </span>
                </div>
                {row.preview_total_cost_usd > 0 && (
                    <div className="dadbear-card-field">
                        <span className="dadbear-card-field-label">Preview cost</span>
                        <span className="dadbear-card-field-value">
                            {currency(row.preview_total_cost_usd)}
                        </span>
                    </div>
                )}
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Actual cost (24h)</span>
                    <span className="dadbear-card-field-value">
                        {currency(row.actual_cost_24h_usd)}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Last compiled</span>
                    <span className="dadbear-card-field-value">
                        {timeAgo(row.last_compilation_at)}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Last dispatch</span>
                    <span className="dadbear-card-field-value">
                        {timeAgo(row.last_dispatch_at)}
                    </span>
                </div>
            </div>

            {localError && (
                <div className="dadbear-card-error">{localError}</div>
            )}

            <div className="dadbear-card-actions">
                <button
                    className={`btn ${isPaused ? 'btn-primary' : 'btn-secondary'}`}
                    disabled={busy}
                    onClick={handleToggle}
                >
                    {busy
                        ? '...'
                        : isPaused
                            ? 'Resume'
                            : 'Pause'}
                </button>
                <button
                    className="btn btn-secondary"
                    onClick={() => onConfigure(row.slug)}
                >
                    Configure
                </button>
                <button
                    className="btn btn-secondary"
                    onClick={() => onViewActivity(row.slug)}
                >
                    View Activity
                </button>
            </div>
        </div>
    );
}
