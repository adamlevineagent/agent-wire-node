// Phase 15 — Activity drawer for the DADBEAR Oversight Page.
//
// Opens when the user clicks "View Activity" on a pyramid card.
// Fetches `pyramid_dadbear_activity_log` for the selected slug and
// renders the union of stale-check, pending-mutation, and change
// manifest rows in a time-descending list.

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface DadbearActivityEntry {
    timestamp: string;
    event_type: string;
    slug: string;
    target_id: string | null;
    details: string | null;
}

interface DadbearActivityDrawerProps {
    slug: string;
    onClose: () => void;
}

function formatTime(iso: string): string {
    const normalized = iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z';
    try {
        return new Date(normalized).toLocaleString();
    } catch {
        return iso;
    }
}

function eventTypeLabel(t: string): string {
    if (t === 'stale_check_stale') return 'Stale check: stale';
    if (t === 'stale_check_fresh') return 'Stale check: fresh';
    if (t === 'change_manifest_applied') return 'Change manifest applied';
    if (t.startsWith('mutation_applied_')) {
        return `Mutation applied: ${t.slice('mutation_applied_'.length)}`;
    }
    if (t.startsWith('mutation_pending_')) {
        return `Mutation pending: ${t.slice('mutation_pending_'.length)}`;
    }
    return t;
}

function eventTypeClass(t: string): string {
    if (t.startsWith('stale_check_stale')) return 'activity-row-stale';
    if (t.startsWith('mutation_applied_')) return 'activity-row-applied';
    if (t.startsWith('mutation_pending_')) return 'activity-row-pending';
    if (t === 'change_manifest_applied') return 'activity-row-manifest';
    return 'activity-row-generic';
}

export function DadbearActivityDrawer({
    slug,
    onClose,
}: DadbearActivityDrawerProps) {
    const [entries, setEntries] = useState<DadbearActivityEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        setError(null);

        invoke<DadbearActivityEntry[]>('pyramid_dadbear_activity_log', {
            slug,
            limit: 200,
        })
            .then((result) => {
                if (cancelled) return;
                setEntries(Array.isArray(result) ? result : []);
            })
            .catch((e) => {
                if (cancelled) return;
                setError(String(e));
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });
        return () => {
            cancelled = true;
        };
    }, [slug]);

    return (
        <div
            className="dadbear-activity-backdrop"
            onClick={onClose}
        >
            <div
                className="dadbear-activity-drawer"
                onClick={(e) => e.stopPropagation()}
            >
                <div className="dadbear-activity-header">
                    <h3>Activity — {slug}</h3>
                    <button
                        className="dadbear-activity-close"
                        onClick={onClose}
                        aria-label="Close"
                    >
                        ×
                    </button>
                </div>

                {loading && (
                    <div className="dadbear-activity-loading">Loading…</div>
                )}
                {error && (
                    <div className="dadbear-activity-error">{error}</div>
                )}

                {!loading && !error && entries.length === 0 && (
                    <div className="dadbear-activity-empty">
                        No recent activity for this pyramid.
                    </div>
                )}

                <ul className="dadbear-activity-list">
                    {entries.map((entry, idx) => {
                        let parsed: Record<string, unknown> | null = null;
                        if (entry.details) {
                            try {
                                parsed = JSON.parse(entry.details);
                            } catch {
                                parsed = null;
                            }
                        }
                        return (
                            <li
                                key={`${entry.timestamp}-${idx}`}
                                className={`dadbear-activity-row ${eventTypeClass(entry.event_type)}`}
                            >
                                <div className="dadbear-activity-row-header">
                                    <span className="dadbear-activity-time">
                                        {formatTime(entry.timestamp)}
                                    </span>
                                    <span className="dadbear-activity-type">
                                        {eventTypeLabel(entry.event_type)}
                                    </span>
                                </div>
                                {entry.target_id && (
                                    <div className="dadbear-activity-target">
                                        target: {entry.target_id}
                                    </div>
                                )}
                                {parsed && Object.keys(parsed).length > 0 && (
                                    <pre className="dadbear-activity-details">
                                        {JSON.stringify(parsed, null, 2)}
                                    </pre>
                                )}
                            </li>
                        );
                    })}
                </ul>
            </div>
        </div>
    );
}
