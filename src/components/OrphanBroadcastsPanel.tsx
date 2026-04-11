// Phase 15 — Orphan Broadcasts panel for the DADBEAR Oversight Page.
//
// Per `evidence-triage-and-dadbear.md` Part 4, orphan broadcasts are
// the primary signal of credential exfiltration: a broadcast trace
// arrives with a metadata shape no local `pyramid_cost_log` row
// expects. Each row is surfaced here with received_at, provider_id,
// model, cost, and session_id so the admin can triage and
// acknowledge. Acknowledgement is scoped to individual rows — the
// panel does not bulk-dismiss.

import { useState } from 'react';
import type { OrphanBroadcastRow } from '../hooks/useOrphanBroadcasts';

interface OrphanBroadcastsPanelProps {
    data: OrphanBroadcastRow[];
    loading: boolean;
    error: string | null;
    onAcknowledge: (orphanId: number, reason?: string) => Promise<void>;
}

function formatMoney(v: number | null): string {
    if (v === null || v === undefined) return '—';
    return `$${v.toFixed(4)}`;
}

function formatDate(iso: string): string {
    const normalized = iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z';
    try {
        return new Date(normalized).toLocaleString();
    } catch {
        return iso;
    }
}

export function OrphanBroadcastsPanel({
    data,
    loading,
    error,
    onAcknowledge,
}: OrphanBroadcastsPanelProps) {
    const [ackingId, setAckingId] = useState<number | null>(null);
    const [ackReasons, setAckReasons] = useState<Record<number, string>>({});

    const unacknowledged = data.filter((r) => !r.acknowledged_at);
    const hasLeakWarning = unacknowledged.length > 0;

    return (
        <section
            className={`orphan-broadcasts-section ${
                hasLeakWarning ? 'orphan-broadcasts-section-alert' : ''
            }`}
            id="orphan-broadcasts"
        >
            <div className="orphan-broadcasts-header">
                <h3>Orphan Broadcasts</h3>
                {hasLeakWarning && (
                    <span className="orphan-broadcasts-warning">
                        Potential credential leak — {unacknowledged.length} unreviewed
                    </span>
                )}
            </div>

            {loading && data.length === 0 && (
                <div className="orphan-broadcasts-loading">
                    Loading orphan broadcasts…
                </div>
            )}
            {error && <div className="orphan-broadcasts-error">{error}</div>}

            {data.length === 0 && !loading && !error && (
                <div className="orphan-broadcasts-empty">
                    No orphan broadcasts. Synchronous + broadcast paths are
                    in agreement.
                </div>
            )}

            <ul className="orphan-broadcasts-list">
                {data.map((row) => (
                    <li
                        key={row.id}
                        className={`orphan-broadcasts-row ${
                            row.acknowledged_at
                                ? 'orphan-broadcasts-row-acked'
                                : 'orphan-broadcasts-row-unacked'
                        }`}
                    >
                        <div className="orphan-broadcasts-row-line">
                            <span className="orphan-broadcasts-time">
                                {formatDate(row.received_at)}
                            </span>
                            <span className="orphan-broadcasts-provider">
                                {row.provider_id ?? '(unknown)'}
                            </span>
                            <span className="orphan-broadcasts-model">
                                {row.model ?? '(model ?)'}
                            </span>
                            <span className="orphan-broadcasts-cost">
                                {formatMoney(row.cost_usd)}
                            </span>
                        </div>
                        <div className="orphan-broadcasts-row-meta">
                            <span>
                                gen_id: {row.generation_id ?? '(none)'}
                            </span>
                            <span>
                                session: {row.session_id ?? '(none)'}
                            </span>
                            {row.step_name && (
                                <span>step: {row.step_name}</span>
                            )}
                            {row.tokens_in !== null && row.tokens_out !== null && (
                                <span>
                                    {row.tokens_in} in / {row.tokens_out} out
                                </span>
                            )}
                        </div>
                        {row.acknowledged_at && (
                            <div className="orphan-broadcasts-acked-line">
                                Acknowledged {formatDate(row.acknowledged_at)}
                                {row.acknowledgment_reason && (
                                    <> — {row.acknowledgment_reason}</>
                                )}
                            </div>
                        )}
                        {!row.acknowledged_at && (
                            <div className="orphan-broadcasts-ack-form">
                                <input
                                    type="text"
                                    placeholder="Reason (optional)"
                                    value={ackReasons[row.id] ?? ''}
                                    onChange={(e) =>
                                        setAckReasons({
                                            ...ackReasons,
                                            [row.id]: e.target.value,
                                        })
                                    }
                                    disabled={ackingId === row.id}
                                />
                                <button
                                    className="btn btn-secondary"
                                    disabled={ackingId === row.id}
                                    onClick={async () => {
                                        setAckingId(row.id);
                                        try {
                                            await onAcknowledge(
                                                row.id,
                                                ackReasons[row.id] || undefined,
                                            );
                                        } finally {
                                            setAckingId(null);
                                        }
                                    }}
                                >
                                    {ackingId === row.id
                                        ? 'Acknowledging…'
                                        : 'Acknowledge'}
                                </button>
                            </div>
                        )}
                    </li>
                ))}
            </ul>
        </section>
    );
}
