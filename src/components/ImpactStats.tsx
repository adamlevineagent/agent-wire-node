import type { CreditStats } from "./Dashboard";

interface ImpactStatsProps {
    credits: CreditStats | null;
}

function formatNumber(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return n.toLocaleString();
}

function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatCredits(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return n.toFixed(2);
}

function parseUptimeHours(uptime: string): number {
    let hours = 0;
    const dMatch = uptime.match(/(\d+)d/);
    const hMatch = uptime.match(/(\d+)h/);
    const mMatch = uptime.match(/(\d+)m/);
    if (dMatch) hours += parseInt(dMatch[1]) * 24;
    if (hMatch) hours += parseInt(hMatch[1]);
    if (mMatch) hours += parseInt(mMatch[1]) / 60;
    return hours;
}

function calcPerHour(count: number, uptimeHours: number): string {
    if (count === 0 || uptimeHours < 0.01) return "0";
    const rate = count / uptimeHours;
    if (rate >= 1000) return `${(rate / 1000).toFixed(1)}K`;
    if (rate >= 100) return Math.round(rate).toString();
    return rate.toFixed(1);
}

export function ImpactStats({ credits }: ImpactStatsProps) {
    const totalDocs = credits?.documents_served || 0;
    const totalPulls = credits?.pulls_served_total || 0;
    const creditsEarned = credits?.credits_earned || 0;
    const bytesFormatted = credits?.total_bytes_formatted || "0 B";
    const todayDocs = credits?.today_documents_served || 0;
    const todayBytes = credits?.today_bytes_served || 0;
    const sessionDocs = credits?.session_documents_served || 0;
    const sessionBytes = credits?.session_bytes_served || 0;
    const uptimeHours = parseUptimeHours(credits?.session_uptime || "0m");

    return (
        <div className="impact-stats">
            {/* Hero -- Credits + Total Pulls */}
            <div className="impact-hero">
                <div className="hero-row">
                    <div className="hero-stat primary">
                        <div className="hero-value glow">
                            {formatCredits(creditsEarned)}
                        </div>
                        <div className="hero-label">
                            credits earned
                        </div>
                    </div>
                    <div className="hero-stat secondary">
                        <div className="hero-value">{formatNumber(totalPulls)}</div>
                        <div className="hero-label">pulls served</div>
                    </div>
                </div>
            </div>

            {/* Row 1 -- Network Contribution */}
            <div className="impact-grid">
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(totalDocs)}</div>
                    <div className="impact-caption">documents served</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{bytesFormatted}</div>
                    <div className="impact-caption">total data served</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{calcPerHour(sessionDocs, uptimeHours)}</div>
                    <div className="impact-caption">docs / hr</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{credits?.session_uptime || "0m"}</div>
                    <div className="impact-caption">session uptime</div>
                </div>
            </div>

            {/* Row 2 -- Today's Activity */}
            <div className="impact-grid">
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(todayDocs)}</div>
                    <div className="impact-caption">served today</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatBytes(todayBytes)}</div>
                    <div className="impact-caption">data today</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(sessionDocs)}</div>
                    <div className="impact-caption">session docs</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatBytes(sessionBytes)}</div>
                    <div className="impact-caption">session data</div>
                </div>
            </div>

            {/* Achievements */}
            {credits?.achievements && credits.achievements.length > 0 && (
                <div className="achievements-section">
                    <h4>Achievements</h4>
                    <div className="achievements-list">
                        {credits.achievements.map((a) => (
                            <div
                                key={a.id}
                                className={`achievement-card ${a.current_level === 0 ? "locked" : ""}`}
                            >
                                <div className="achievement-header">
                                    <span className="achievement-emoji">{a.emoji}</span>
                                    <div className="achievement-info">
                                        <span className="achievement-name">
                                            {a.current_level > 0 ? a.current_name : "???"}
                                        </span>
                                        {a.current_level > 0 && (
                                            <span className="achievement-level">Lvl {a.current_level}</span>
                                        )}
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
                                            {formatNumber(a.current_value)} / {formatNumber(a.next_threshold || 0)} &rarr; {a.next_name}
                                        </span>
                                    </div>
                                )}
                                {!a.next_name && a.current_level > 0 && (
                                    <div className="achievement-maxed">MAX</div>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            )}
        </div>
    );
}
