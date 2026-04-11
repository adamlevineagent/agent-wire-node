// Phase 15 — Provider Health section for the DADBEAR Oversight Page.
//
// Renders the list of provider health entries from Phase 11's
// `pyramid_provider_health` IPC. Each row gets a color-coded chip,
// reason text, recent-signal counts, and an Acknowledge button.
// Degraded/alerting providers surface a red banner at the top.

import type { ProviderHealthEntry } from '../hooks/useProviderHealth';

interface ProviderHealthBannerProps {
    data: ProviderHealthEntry[];
    loading: boolean;
    error: string | null;
    onAcknowledge: (providerId: string) => Promise<void>;
}

function healthClass(health: string): string {
    switch (health) {
        case 'healthy':
            return 'provider-health-chip-healthy';
        case 'degraded':
            return 'provider-health-chip-degraded';
        case 'alerting':
        case 'unhealthy':
            return 'provider-health-chip-alerting';
        default:
            return 'provider-health-chip-unknown';
    }
}

function healthLabel(health: string): string {
    switch (health) {
        case 'healthy':
            return 'Healthy';
        case 'degraded':
            return 'Degraded';
        case 'alerting':
            return 'Alerting';
        case 'unhealthy':
            return 'Unhealthy';
        default:
            return health;
    }
}

export function ProviderHealthBanner({
    data,
    loading,
    error,
    onAcknowledge,
}: ProviderHealthBannerProps) {
    const degraded = data.filter(
        (p) => p.health !== 'healthy' && !p.acknowledged_at,
    );

    return (
        <section className="provider-health-section">
            <div className="provider-health-header">
                <h3>Provider Health</h3>
                {degraded.length > 0 && (
                    <span className="provider-health-degraded-count">
                        {degraded.length} provider
                        {degraded.length === 1 ? '' : 's'} need attention
                    </span>
                )}
            </div>

            {loading && data.length === 0 && (
                <div className="provider-health-loading">
                    Loading provider health…
                </div>
            )}
            {error && <div className="provider-health-error">{error}</div>}

            {data.length === 0 && !loading && !error && (
                <div className="provider-health-empty">
                    No providers configured.
                </div>
            )}

            <ul className="provider-health-list">
                {data.map((entry) => (
                    <li
                        key={entry.provider_id}
                        className={`provider-health-row ${
                            entry.health === 'healthy'
                                ? 'provider-health-row-healthy'
                                : 'provider-health-row-degraded'
                        }`}
                    >
                        <div className="provider-health-row-main">
                            <span
                                className={`provider-health-chip ${healthClass(entry.health)}`}
                            >
                                {healthLabel(entry.health)}
                            </span>
                            <span className="provider-health-name">
                                {entry.display_name}
                            </span>
                            <span className="provider-health-type">
                                {entry.provider_type}
                            </span>
                        </div>
                        {entry.reason && (
                            <div className="provider-health-reason">
                                {entry.reason}
                            </div>
                        )}
                        <div className="provider-health-signals">
                            <span>
                                Discrepancies:{' '}
                                {entry.recent_discrepancies}
                            </span>
                            <span>
                                Missing broadcasts:{' '}
                                {entry.recent_broadcast_missing}
                            </span>
                            <span>
                                Orphans: {entry.recent_orphans}
                            </span>
                        </div>
                        {entry.health !== 'healthy' && (
                            <div className="provider-health-actions">
                                <button
                                    className="btn btn-secondary"
                                    onClick={() =>
                                        onAcknowledge(entry.provider_id)
                                    }
                                >
                                    Acknowledge
                                </button>
                            </div>
                        )}
                    </li>
                ))}
            </ul>
        </section>
    );
}
