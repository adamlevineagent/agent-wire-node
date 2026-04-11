// Phase 15 — Per-pyramid card for the DADBEAR Oversight Page.
//
// Renders a single `DadbearOverviewRow` as a compact status card
// with pause/resume, view-activity, and configure buttons. Wired to
// `pyramid_dadbear_pause` / `pyramid_dadbear_resume`.

import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { DadbearOverviewRow } from '../hooks/useDadbearOverview';

interface DadbearPyramidCardProps {
    row: DadbearOverviewRow;
    onViewActivity: (slug: string) => void;
    onConfigure: (slug: string) => void;
    onMutated: () => void;
}

function timeAgo(iso: string | null): string {
    if (!iso) return 'never';
    // ISO may arrive as `YYYY-MM-DD HH:MM:SS` (SQL) or full ISO.
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

function timeUntil(iso: string | null): string {
    if (!iso) return 'due now';
    const normalized = iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z';
    const diff = new Date(normalized).getTime() - Date.now();
    if (Number.isNaN(diff)) return iso;
    if (diff <= 0) return 'due now';
    const s = Math.floor(diff / 1000);
    if (s < 60) return `in ${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `in ${m}m`;
    const h = Math.floor(m / 60);
    return `in ${h}h`;
}

function currency(v: number): string {
    return `$${v.toFixed(2)}`;
}

function statusLabel(status: string): string {
    switch (status) {
        case 'healthy':
            return 'Healthy';
        case 'pending':
            return 'Pending confirmation';
        case 'discrepancy':
            return 'Discrepancy detected';
        case 'broadcast_missing':
            return 'Broadcast missing';
        default:
            return status;
    }
}

function statusClass(status: string): string {
    switch (status) {
        case 'healthy':
            return 'dadbear-card-recon-healthy';
        case 'pending':
            return 'dadbear-card-recon-pending';
        case 'discrepancy':
            return 'dadbear-card-recon-discrepancy';
        case 'broadcast_missing':
            return 'dadbear-card-recon-broadcast-missing';
        default:
            return 'dadbear-card-recon-pending';
    }
}

export function DadbearPyramidCard({
    row,
    onViewActivity,
    onConfigure,
    onMutated,
}: DadbearPyramidCardProps) {
    const [busy, setBusy] = useState(false);
    const [localError, setLocalError] = useState<string | null>(null);

    const handleToggle = useCallback(async () => {
        setBusy(true);
        setLocalError(null);
        try {
            if (row.enabled) {
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
    }, [row.enabled, row.slug, onMutated]);

    const cardClass = [
        'dadbear-card',
        row.enabled ? 'dadbear-card-active' : 'dadbear-card-paused',
        row.cost_reconciliation_status === 'discrepancy' ||
        row.cost_reconciliation_status === 'broadcast_missing'
            ? 'dadbear-card-alert'
            : '',
    ]
        .filter(Boolean)
        .join(' ');

    return (
        <div className={cardClass}>
            <div className="dadbear-card-header">
                <h3 className="dadbear-card-title">{row.display_name}</h3>
                <span
                    className={`dadbear-card-status ${
                        row.enabled ? 'dadbear-card-status-active' : 'dadbear-card-status-paused'
                    }`}
                >
                    {row.enabled ? 'Active' : 'Paused'}
                </span>
            </div>

            <div className="dadbear-card-grid">
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Next scan</span>
                    <span className="dadbear-card-field-value">
                        {row.enabled
                            ? timeUntil(row.next_scan_at)
                            : '—'}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Last scan</span>
                    <span className="dadbear-card-field-value">
                        {timeAgo(row.last_scan_at)}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Pending mutations</span>
                    <span className="dadbear-card-field-value">
                        {row.pending_mutations_count}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">In-flight checks</span>
                    <span className="dadbear-card-field-value">
                        {row.in_flight_stale_checks}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Deferred questions</span>
                    <span className="dadbear-card-field-value">
                        {row.deferred_questions_count}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Demand (24h)</span>
                    <span className="dadbear-card-field-value">
                        {row.demand_signals_24h}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Cost (24h)</span>
                    <span className="dadbear-card-field-value">
                        {currency(row.cost_24h_estimated_usd)} est
                        {row.cost_24h_actual_usd > 0 && (
                            <>
                                {' '}
                                / {currency(row.cost_24h_actual_usd)} actual
                            </>
                        )}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Reconciliation</span>
                    <span
                        className={`dadbear-card-field-value ${statusClass(
                            row.cost_reconciliation_status,
                        )}`}
                    >
                        {statusLabel(row.cost_reconciliation_status)}
                    </span>
                </div>
                <div className="dadbear-card-field">
                    <span className="dadbear-card-field-label">Manifests (24h)</span>
                    <span className="dadbear-card-field-value">
                        {row.recent_manifest_count}
                    </span>
                </div>
            </div>

            {localError && (
                <div className="dadbear-card-error">{localError}</div>
            )}

            <div className="dadbear-card-actions">
                <button
                    className={`btn ${row.enabled ? 'btn-secondary' : 'btn-primary'}`}
                    disabled={busy}
                    onClick={handleToggle}
                >
                    {busy
                        ? '…'
                        : row.enabled
                            ? 'Pause'
                            : 'Resume'}
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
