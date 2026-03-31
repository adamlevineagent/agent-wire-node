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
    // Use recent_events from credits prop (already polled by parent Dashboard)
    const events: ServeEvent[] = credits?.recent_events ?? [];

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
                const isWork = event.event_type.startsWith("work_");
                const isServe = event.event_type === "serve";

                const iconChar = isSync
                    ? (event.event_type === "sync_push" ? "^" : "v")
                    : isWork ? "+" : "W";
                const iconClass = isSync
                    ? "activity-icon-sync"
                    : isWork ? "activity-icon-work" : "";
                const itemClass = isSync
                    ? "activity-sync"
                    : isWork ? "activity-work" : "activity-serve";

                return (
                    <div
                        key={`${event.timestamp}-${i}`}
                        className={`activity-feed-item ${itemClass}`}
                        style={{ animationDelay: `${i * 50}ms` }}
                    >
                        <div className="activity-left">
                            <div
                                className={`activity-icon ${iconClass}`}
                                title={isSync ? "Sync event" : isWork ? "Work completed" : "Document served"}
                            >
                                {iconChar}
                            </div>
                            <div className="activity-details">
                                <span className={`activity-track-name ${isSync ? "activity-track-sync" : isWork ? "activity-track-work" : ""}`}>
                                    {event.message || (event.document_id.length > 20
                                        ? event.document_id.slice(0, 8) + "..." + event.document_id.slice(-8)
                                        : event.document_id)}
                                </span>
                                <span className="activity-meta">
                                    {isSync
                                        ? `${event.event_type === "sync_push" ? "Pushed" : "Pulled"} - ${formatBytes(event.bytes)}`
                                        : isWork
                                        ? event.event_type.replace("work_", "")
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
