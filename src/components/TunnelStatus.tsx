import type { CreditStats, Achievement } from "./Dashboard";

interface TunnelConnectionData {
    tunnel_id: string | null;
    tunnel_url: string | null;
    status: string | { Error: string };
}

interface TunnelStatusProps {
    credits: CreditStats | null;
    tunnelStatus?: TunnelConnectionData | null;
}

function formatThreshold(id: string, value: number): string {
    if (id === "data_shared") {
        if (value >= 1024 * 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024 * 1024)).toFixed(0)} TB`;
        if (value >= 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024)).toFixed(0)} GB`;
        if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(0)} MB`;
        return `${value}`;
    }
    if (id === "time_hosting") {
        const hours = value / 3600;
        if (hours >= 8760) return `${(hours / 8760).toFixed(0)}yr`;
        if (hours >= 720) return `${(hours / 720).toFixed(0)}mo`;
        if (hours >= 168) return `${(hours / 168).toFixed(0)}wk`;
        if (hours >= 24) return `${(hours / 24).toFixed(0)}d`;
        return `${hours.toFixed(0)}h`;
    }
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(0)}M`;
    if (value >= 1_000) return `${(value / 1_000).toFixed(0)}K`;
    return value.toLocaleString();
}

function formatCurrentValue(id: string, value: number): string {
    if (id === "data_shared") {
        if (value >= 1024 * 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024 * 1024)).toFixed(1)} TB`;
        if (value >= 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
        if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(0)} MB`;
        return `${value}`;
    }
    if (id === "time_hosting") {
        const hours = value / 3600;
        if (hours >= 8760) return `${(hours / 8760).toFixed(1)}yr`;
        if (hours >= 720) return `${(hours / 720).toFixed(1)}mo`;
        if (hours >= 24) return `${(hours / 24).toFixed(1)}d`;
        if (hours >= 1) return `${hours.toFixed(1)}h`;
        return `${(hours * 60).toFixed(0)}m`;
    }
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
    if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
    return value.toLocaleString();
}

export function TunnelStatus({ credits, tunnelStatus }: TunnelStatusProps) {
    const totalServed = credits?.documents_served || 0;
    const creditsEarned = credits?.credits_earned || 0;
    const uptime = credits?.session_uptime || "0m";
    const firstStarted = credits?.first_started_at;
    const tunnelUrl = tunnelStatus?.tunnel_url;

    const isConnected = tunnelStatus?.status === "Connected";
    const connectionIndicator = isConnected ? "[ON]" : "[OFF]";

    const uptimeSeconds = credits?.total_uptime_seconds || 0;
    const nodeAgeDays = Math.floor(uptimeSeconds / 86400);

    const memberSince = firstStarted
        ? new Date(firstStarted).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" })
        : "Today";

    const achievements = credits?.achievements || [];
    const active = achievements.filter((a) => a.current_level > 0);
    const upcoming = achievements.filter((a) => a.current_level === 0);

    return (
        <div className="tunnel-status">
            {/* Node Badge */}
            <div className="tier-card">
                <div className="tier-icon-wrapper">
                    <div className="wire-logo-tier">W</div>
                </div>
                <div className="tier-name">Wire Node</div>
                <div className="tier-served">{totalServed.toLocaleString()} documents served</div>
                <div className="tier-credits">{creditsEarned.toFixed(2)} credits earned</div>
            </div>

            {/* Tunnel Info */}
            <div className="tunnel-info">
                {tunnelUrl && (
                    <div className="info-row">
                        <span className="info-icon">{connectionIndicator}</span>
                        <div>
                            <div className="info-label">Wire Endpoint</div>
                            <div className="info-value tunnel-endpoint">
                                {tunnelUrl.replace("https://", "")}
                            </div>
                        </div>
                    </div>
                )}
                <div className="info-row">
                    <span className="info-icon">[i]</span>
                    <div>
                        <div className="info-label">Member Since</div>
                        <div className="info-value">{memberSince}</div>
                    </div>
                </div>
                <div className="info-row">
                    <span className="info-icon">[t]</span>
                    <div>
                        <div className="info-label">Node Age</div>
                        <div className="info-value">
                            {nodeAgeDays === 0 ? "Day 1" : `${nodeAgeDays} day${nodeAgeDays !== 1 ? "s" : ""}`}
                        </div>
                    </div>
                </div>
                <div className="info-row">
                    <span className="info-icon">[+]</span>
                    <div>
                        <div className="info-label">Session Uptime</div>
                        <div className="info-value">{uptime}</div>
                    </div>
                </div>
            </div>

            {/* Achievements */}
            <div className="achievements-section">
                <h4>Achievements</h4>
                <div className="achievements-list">
                    {active.map((a) => (
                        <div key={a.id} className="achievement-card">
                            <div className="achievement-header">
                                <span className="achievement-emoji">{a.emoji}</span>
                                <div className="achievement-info">
                                    <span className="achievement-name">{a.current_name}</span>
                                    <span className="achievement-level">Lvl {a.current_level}</span>
                                </div>
                            </div>
                            {a.next_name && (
                                <div className="achievement-progress">
                                    <div className="achievement-bar">
                                        <div
                                            className="achievement-fill"
                                            style={{ width: `${a.progress_pct}%` }}
                                        />
                                    </div>
                                    <span className="achievement-next">
                                        {formatCurrentValue(a.id, a.current_value)} / {formatThreshold(a.id, a.next_threshold!)} &rarr; {a.next_name}
                                    </span>
                                </div>
                            )}
                            {!a.next_name && (
                                <div className="achievement-maxed">MAX</div>
                            )}
                        </div>
                    ))}
                    {upcoming.map((a) => (
                        <div key={a.id} className="achievement-card locked">
                            <div className="achievement-header">
                                <span className="achievement-emoji">{a.emoji}</span>
                                <div className="achievement-info">
                                    <span className="achievement-name">???</span>
                                </div>
                            </div>
                            {a.next_name && (
                                <div className="achievement-progress">
                                    <div className="achievement-bar">
                                        <div className="achievement-fill" style={{ width: "0%" }} />
                                    </div>
                                    <span className="achievement-next">
                                        0 / {formatThreshold(a.id, a.next_threshold!)} &rarr; {a.next_name}
                                    </span>
                                </div>
                            )}
                        </div>
                    ))}
                </div>
            </div>
        </div>
    );
}
