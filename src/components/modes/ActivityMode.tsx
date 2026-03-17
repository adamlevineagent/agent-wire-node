import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

interface Notification {
    id: string;
    subscription_id?: string;
    event_type: string;
    source_contribution_id?: string;
    source_agent_pseudonym?: string;
    read: boolean;
    agent_id?: string;
    operator_id?: string;
    created_at: string;
}

type FilterType = 'all' | 'action_required' | 'informational';
type SourceFilter = 'all' | 'fleet' | 'corpora' | 'node' | 'system';
type TimeFilter = 'today' | 'week' | 'all';

// --- Helpers ---

function timeAgo(timestamp: string): string {
    const diff = Date.now() - new Date(timestamp).getTime();
    if (diff < 60_000) return 'just now';
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
}

const eventTypeIcons: Record<string, string> = {
    contribution: '\u{1F4DD}',
    transaction: '\u{1F4B0}',
    agent: '\u{1F916}',
    corpus: '\u{1F4DA}',
    node: '\u{1F5A5}\uFE0F',
    handle: '\u{1F3F7}\uFE0F',
    system: '\u{2699}\uFE0F',
    message: '\u{1F4AC}',
    update: '\u{1F4E6}',
    announcement: '\u{1F4E2}',
    bug_report: '\u{1F41B}',
    curation: '\u{2705}',
    request: '\u{1F4CB}',
    credit: '\u{2B50}',
};

function getEventIcon(eventType: string): string {
    return eventTypeIcons[eventType] || '\u{1F514}';
}

function getSource(notification: Notification): string {
    const et = notification.event_type?.toLowerCase() || '';
    if (et.includes('agent') || et.includes('fleet')) return 'fleet';
    if (et.includes('corpus') || et.includes('document') || et.includes('curation')) return 'corpora';
    if (et.includes('node') || et.includes('tunnel') || et.includes('sync')) return 'node';
    return 'system';
}

function isActionRequired(notification: Notification): boolean {
    const et = notification.event_type?.toLowerCase() || '';
    return et.includes('request') || et.includes('curation') || et.includes('action') || et.includes('review');
}

function isWithinTimeFilter(timestamp: string, filter: TimeFilter): boolean {
    if (filter === 'all') return true;
    const now = Date.now();
    const time = new Date(timestamp).getTime();
    if (filter === 'today') return now - time < 86_400_000;
    if (filter === 'week') return now - time < 7 * 86_400_000;
    return true;
}

function getActionLabel(notification: Notification): string | null {
    const et = notification.event_type?.toLowerCase() || '';
    if (et.includes('document') || et.includes('contribution')) return 'View Document';
    if (et.includes('curation') || et.includes('review')) return 'Review';
    if (et.includes('transaction') || et.includes('credit')) return 'View Transaction';
    return null;
}

function formatEventTitle(notification: Notification): string {
    const et = notification.event_type || 'notification';
    // Convert snake_case/kebab-case to title case
    const title = et.replace(/[_-]/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
    if (notification.source_agent_pseudonym) {
        return `${title} from ${notification.source_agent_pseudonym}`;
    }
    return title;
}

// --- Component ---

export function ActivityMode() {
    const { operatorApiCall, state, dispatch } = useAppContext();

    const [notifications, setNotifications] = useState<Notification[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [filterType, setFilterType] = useState<FilterType>('all');
    const [sourceFilter, setSourceFilter] = useState<SourceFilter>('all');
    const [timeFilter, setTimeFilter] = useState<TimeFilter>('all');

    const fetchNotifications = useCallback(async () => {
        try {
            const data: any = await operatorApiCall('GET', '/api/v1/wire/notifications?read=all');
            const list = data?.notifications || data || [];
            const notifs = Array.isArray(list) ? list : [];
            setNotifications(notifs);
            setError(null);
            // Dispatch unread count to global state for sidebar badge
            const unreadCount = typeof data?.unread_count === 'number'
                ? data.unread_count
                : notifs.filter((n: Notification) => !n.read).length;
            dispatch({ type: 'SET_NOTIFICATION_COUNT', count: unreadCount });
        } catch (err: any) {
            setError(err?.message || 'Failed to load notifications');
        } finally {
            setLoading(false);
        }
    }, [operatorApiCall, dispatch]);

    // Initial fetch
    useEffect(() => {
        fetchNotifications();
    }, [fetchNotifications]);

    // Auto-refresh every 30 seconds
    useEffect(() => {
        const interval = setInterval(fetchNotifications, 30_000);
        return () => clearInterval(interval);
    }, [fetchNotifications]);

    const handleMarkRead = useCallback(async (notificationId: string) => {
        try {
            await operatorApiCall('POST', '/api/v1/wire/notifications/mark-read', {
                notification_ids: [notificationId],
            });
            setNotifications(prev =>
                prev.map(n => n.id === notificationId ? { ...n, read: true } : n)
            );
            // Update global count
            dispatch({ type: 'SET_NOTIFICATION_COUNT', count: Math.max(0, state.notificationCount - 1) });
        } catch (err) {
            console.error('Failed to mark notification read:', err);
        }
    }, [operatorApiCall, dispatch, state.notificationCount]);

    const handleMarkAllRead = useCallback(async () => {
        const unreadIds = notifications.filter(n => !n.read).map(n => n.id);
        if (unreadIds.length === 0) return;
        try {
            await operatorApiCall('POST', '/api/v1/wire/notifications/mark-read', {
                notification_ids: unreadIds,
            });
            setNotifications(prev =>
                prev.map(n => ({ ...n, read: true }))
            );
            dispatch({ type: 'SET_NOTIFICATION_COUNT', count: 0 });
        } catch (err) {
            console.error('Failed to mark all read:', err);
        }
    }, [notifications, operatorApiCall, dispatch]);

    // Apply filters
    const filtered = notifications.filter(n => {
        if (filterType === 'action_required' && !isActionRequired(n)) return false;
        if (filterType === 'informational' && isActionRequired(n)) return false;
        if (sourceFilter !== 'all' && getSource(n) !== sourceFilter) return false;
        if (!isWithinTimeFilter(n.created_at, timeFilter)) return false;
        return true;
    });

    const unreadCount = notifications.filter(n => !n.read).length;

    if (loading) {
        return (
            <div className="mode-container activity-mode">
                <div className="activity-loading">
                    <div className="loading-spinner" />
                    <span>Loading activity...</span>
                </div>
            </div>
        );
    }

    return (
        <div className="mode-container activity-mode">
            {/* Header */}
            <div className="activity-header">
                <h2>Activity</h2>
                {unreadCount > 0 && (
                    <button className="activity-mark-all" onClick={handleMarkAllRead}>
                        Mark all as read ({unreadCount})
                    </button>
                )}
            </div>

            {/* Error state */}
            {error && (
                <div className="activity-error">
                    <span>{error}</span>
                    <button onClick={fetchNotifications} className="activity-retry-btn">Retry</button>
                </div>
            )}

            {/* Filter bars */}
            <div className="activity-filters">
                <div className="filter-tabs">
                    {([['all', 'All'], ['action_required', 'Action Required'], ['informational', 'Informational']] as const).map(([key, label]) => (
                        <button
                            key={key}
                            className={`filter-tab ${filterType === key ? 'filter-tab-active' : ''}`}
                            onClick={() => setFilterType(key)}
                        >
                            {label}
                        </button>
                    ))}
                </div>
                <div className="filter-dropdowns">
                    <select
                        className="filter-select"
                        value={sourceFilter}
                        onChange={(e) => setSourceFilter(e.target.value as SourceFilter)}
                    >
                        <option value="all">All Sources</option>
                        <option value="fleet">Fleet</option>
                        <option value="corpora">Corpora</option>
                        <option value="node">Node</option>
                        <option value="system">System</option>
                    </select>
                    <select
                        className="filter-select"
                        value={timeFilter}
                        onChange={(e) => setTimeFilter(e.target.value as TimeFilter)}
                    >
                        <option value="all">All Time</option>
                        <option value="week">This Week</option>
                        <option value="today">Today</option>
                    </select>
                </div>
            </div>

            {/* Notification list */}
            <div className="activity-feed">
                {filtered.length === 0 ? (
                    <div className="activity-empty">
                        <span className="activity-empty-icon">{'\u{1F514}'}</span>
                        <p className="activity-empty-title">No notifications</p>
                        <p className="activity-empty-desc">
                            {filterType !== 'all' || sourceFilter !== 'all' || timeFilter !== 'all'
                                ? 'Try adjusting your filters.'
                                : 'You\'re all caught up.'}
                        </p>
                    </div>
                ) : (
                    filtered.map(notification => {
                        const isUnread = !notification.read;
                        const actionLabel = getActionLabel(notification);
                        return (
                            <div
                                key={notification.id}
                                className={`activity-item ${isUnread ? 'activity-item-unread' : ''}`}
                                onClick={() => isUnread && handleMarkRead(notification.id)}
                            >
                                <div className="activity-item-icon">
                                    {getEventIcon(notification.event_type)}
                                </div>
                                <div className="activity-item-content">
                                    <div className="activity-item-title">
                                        {formatEventTitle(notification)}
                                    </div>
                                    {notification.source_contribution_id && (
                                        <div className="activity-item-desc">
                                            Contribution {notification.source_contribution_id.slice(0, 8)}...
                                        </div>
                                    )}
                                    <div className="activity-item-meta">
                                        <span className="activity-item-source">{getSource(notification)}</span>
                                        <span className="activity-item-time">{timeAgo(notification.created_at)}</span>
                                    </div>
                                </div>
                                <div className="activity-item-actions">
                                    {isUnread && (
                                        <span className="activity-unread-dot" title="Unread" />
                                    )}
                                    {actionLabel && (
                                        <button
                                            className="activity-action-btn"
                                            onClick={(e) => {
                                                e.stopPropagation();
                                                if (!isUnread) return;
                                                handleMarkRead(notification.id);
                                            }}
                                        >
                                            {actionLabel}
                                        </button>
                                    )}
                                </div>
                            </div>
                        );
                    })
                )}
            </div>
        </div>
    );
}
