import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CreditStats } from "./Dashboard";

interface WorkStats {
    total_jobs_completed: number;
    total_credits_earned: number;
    session_jobs_completed: number;
    session_credits_earned: number;
    consecutive_errors: number;
    last_work_at: string | null;
    is_polling: boolean;
}

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
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return Math.floor(n).toString();
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
    const [workStats, setWorkStats] = useState<WorkStats | null>(null);

    useEffect(() => {
        const fetchWork = async () => {
            try {
                const ws = await invoke<WorkStats>("get_work_stats");
                setWorkStats(ws);
            } catch {
                // Work stats not available yet
            }
        };
        fetchWork();
        const interval = setInterval(fetchWork, 3000);
        return () => clearInterval(interval);
    }, []);

    const totalDocs = credits?.documents_served || 0;
    const totalPulls = credits?.pulls_served_total || 0;
    const creditsEarned = credits?.credits_earned || 0;
    const bytesFormatted = credits?.total_bytes_formatted || "0 B";
    const todayDocs = credits?.today_documents_served || 0;
    const todayBytes = credits?.today_bytes_served || 0;
    const sessionDocs = credits?.session_documents_served || 0;
    const sessionBytes = credits?.session_bytes_served || 0;
    const uptimeHours = parseUptimeHours(credits?.session_uptime || "0m");

    const totalJobs = workStats?.total_jobs_completed || 0;
    const sessionJobs = workStats?.session_jobs_completed || 0;
    const sessionWorkCredits = workStats?.session_credits_earned || 0;
    const isPolling = workStats?.is_polling ?? false;
    const serverBalance = credits?.server_credit_balance || 0;

    return (
        <div className="impact-stats">
            {/* Hero -- Balance + Earned + Jobs */}
            <div className="impact-hero">
                <div className="hero-row">
                    <div className="hero-stat primary">
                        <div className="hero-value glow">
                            {formatCredits(serverBalance > 0 ? serverBalance : creditsEarned)}
                        </div>
                        <div className="hero-label">
                            {serverBalance > 0 ? "credit balance" : "credits earned"}
                        </div>
                    </div>
                    <div className="hero-stat secondary">
                        <div className="hero-value">{formatNumber(totalJobs)}</div>
                        <div className="hero-label">
                            jobs completed {isPolling && <span className="work-polling-dot" title="Polling for work">*</span>}
                        </div>
                    </div>
                    <div className="hero-stat secondary">
                        <div className="hero-value">{formatNumber(totalPulls)}</div>
                        <div className="hero-label">pulls served</div>
                    </div>
                </div>
            </div>

            {/* Row 1 -- Work + Network */}
            <div className="impact-grid">
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(sessionJobs)}</div>
                    <div className="impact-caption">session jobs</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatCredits(sessionWorkCredits)}</div>
                    <div className="impact-caption">session credits</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(totalDocs)}</div>
                    <div className="impact-caption">documents served</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{bytesFormatted}</div>
                    <div className="impact-caption">total data served</div>
                </div>
            </div>

            {/* Row 2 -- Session + Uptime */}
            <div className="impact-grid">
                <div className="impact-card">
                    <div className="impact-number">{calcPerHour(sessionJobs, uptimeHours)}</div>
                    <div className="impact-caption">jobs / hr</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{credits?.session_uptime || "0m"}</div>
                    <div className="impact-caption">session uptime</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatNumber(todayDocs)}</div>
                    <div className="impact-caption">served today</div>
                </div>
                <div className="impact-card">
                    <div className="impact-number">{formatBytes(todayBytes)}</div>
                    <div className="impact-caption">data today</div>
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
