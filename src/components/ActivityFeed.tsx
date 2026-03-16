import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CreditStats } from "./Dashboard";

interface ServeEvent {
    document_id: string;
    bytes: number;
    timestamp: string;
    message: string;
    token_id: string;
    event_type: string;
}

interface ActivityFeedProps {
    credits: CreditStats | null;
}

export function ActivityFeed({ credits }: ActivityFeedProps) {
    const [events, setEvents] = useState<ServeEvent[]>([]);

    // Poll credits for recent events (the CreditTracker stores them)
    useEffect(() => {
        const poll = async () => {
            try {
                // The credits object from get_credits doesn't include events directly,
                // so we poll get_credits and derive activity from changing stats.
                // For now, build synthetic events from the credit stats.
                const cr = await invoke<any>("get_credits");
                if (cr && cr.recent_events) {
                    setEvents(cr.recent_events || []);
                }
            } catch {
                // Credits endpoint may not include recent_events in the DashboardStats shape
                // That's OK -- we show what we can
            }
        };
        poll();
        const interval = setInterval(poll, 3000);
        return () => clearInterval(interval);
    }, []);

    if (events.length === 0) {
        return (
            <div className="activity-empty">
                <div className="empty-icon">W</div>
                <p>Waiting for activity...</p>
                <p className="empty-hint">
                    Pull serves, sync events, and mechanical work completions will appear here
                </p>
            </div>
        );
    }

    return (
        <div className="activity-feed">
            {events.map((event, i) => {
                const isSync = event.event_type === "sync_push" || event.event_type === "sync_pull";
                const isPull = event.event_type === "serve";

                return (
                    <div
                        key={`${event.timestamp}-${i}`}
                        className={`activity-item ${isSync ? "activity-sync" : "activity-serve"}`}
                        style={{ animationDelay: `${i * 50}ms` }}
                    >
                        <div className="activity-left">
                            <div
                                className={`activity-icon ${isSync ? "activity-icon-sync" : ""}`}
                                title={isSync ? "Sync event" : "Document served"}
                            >
                                {isSync ? (event.event_type === "sync_push" ? "^" : "v") : "W"}
                            </div>
                            <div className="activity-details">
                                <span className={`activity-track-name ${isSync ? "activity-track-sync" : ""}`}>
                                    {event.document_id.length > 20
                                        ? event.document_id.slice(0, 8) + "..." + event.document_id.slice(-8)
                                        : event.document_id}
                                </span>
                                <span className="activity-meta">
                                    {isSync
                                        ? `${event.event_type === "sync_push" ? "Pushed" : "Pulled"} - ${formatBytes(event.bytes)}`
                                        : `${formatBytes(event.bytes)} served`}
                                </span>
                            </div>
                        </div>
                        <div className="activity-time">
                            {formatRelativeTime(event.timestamp)}
                        </div>
                    </div>
                );
            })}
        </div>
    );
}

function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatRelativeTime(timestamp: string): string {
    const now = Date.now();
    const then = new Date(timestamp).getTime();
    const diff = now - then;

    if (diff < 60_000) return "just now";
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return new Date(timestamp).toLocaleDateString();
}
