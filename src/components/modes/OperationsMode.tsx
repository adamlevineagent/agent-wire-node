import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { ContributionCard, ContributionSummary } from '../search/ContributionCard';
import type { OperationEntry } from '../../types/planner';

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

interface WireMessage {
    id: string;
    sender_pseudonym?: string;
    body: string;
    read: boolean;
    created_at: string;
}

type FilterType = 'all' | 'action_required' | 'informational';
type SourceFilter = 'all' | 'fleet' | 'corpora' | 'infrastructure' | 'system';
type TimeFilter = 'today' | 'week' | 'all';
type OperationsTab = 'notifications' | 'messages' | 'active' | 'queue';

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
    if (et.includes('node') || et.includes('tunnel') || et.includes('sync')) return 'infrastructure';
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

function formatEventTitle(notification: Notification): string {
    const et = notification.event_type || 'notification';
    const title = et.replace(/[_-]/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
    if (notification.source_agent_pseudonym) {
        return `${title} from ${notification.source_agent_pseudonym}`;
    }
    return title;
}

// --- Elapsed time helper ---

function formatElapsed(ms: number): string {
    const totalSeconds = Math.floor(ms / 1000);
    if (totalSeconds < 60) return `${totalSeconds}s`;
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes}m ${seconds}s`;
}

// --- Active Operations sub-component ---

function ActiveOperations() {
    const { state, dispatch, setMode } = useAppContext();
    const { activeOperations } = state;
    const [now, setNow] = useState(Date.now());

    // Tick every second while there are running operations
    const hasRunning = activeOperations.some(op => op.status === 'running');
    useEffect(() => {
        if (!hasRunning) return;
        const interval = setInterval(() => setNow(Date.now()), 1000);
        return () => clearInterval(interval);
    }, [hasRunning]);

    const handleDismiss = useCallback((id: string) => {
        dispatch({ type: 'DISMISS_OPERATION', id });
    }, [dispatch]);

    const handleRetry = useCallback((op: OperationEntry) => {
        // Remove failed operation and navigate to intent bar with pre-filled intent
        dispatch({ type: 'DISMISS_OPERATION', id: op.id });
        // Navigate to operations mode root which has the intent bar
        // The intent text is surfaced via the mode stack props
        setMode('operations');
    }, [dispatch, setMode]);

    // Toggle expanded state for viewing operation details inline
    const [expandedOps, setExpandedOps] = useState<Set<string>>(new Set());
    const handleViewResult = useCallback((op: OperationEntry) => {
        setExpandedOps(prev => {
            const next = new Set(prev);
            if (next.has(op.id)) {
                next.delete(op.id);
            } else {
                next.add(op.id);
            }
            return next;
        });
    }, []);

    if (activeOperations.length === 0) {
        return (
            <div className="operations-empty">
                <span className="operations-empty-icon">{'\u{2699}\uFE0F'}</span>
                <p className="operations-empty-title">No active operations</p>
                <p className="operations-empty-desc">Use the intent bar to start something.</p>
            </div>
        );
    }

    return (
        <div className="operations-active-list">
            {activeOperations.map(op => (
                <div key={op.id} className={`operations-active-item operations-active-item--${op.status}`}>
                    <div className="operations-active-header">
                        <div className="operations-active-intent">{op.intent}</div>
                        <div className="operations-active-status">
                            {op.status === 'running' && (
                                <span className="operations-active-badge operations-active-badge--running">
                                    <span className="operations-active-spinner" />
                                    Running
                                </span>
                            )}
                            {op.status === 'completed' && (
                                <span className="operations-active-badge operations-active-badge--completed">
                                    {'\u2713'} Complete
                                </span>
                            )}
                            {op.status === 'failed' && (
                                <span className="operations-active-badge operations-active-badge--failed">
                                    {'\u2717'} Failed
                                </span>
                            )}
                        </div>
                    </div>

                    {/* Running: progress + elapsed time */}
                    {op.status === 'running' && (
                        <div className="operations-active-progress">
                            <div className="operations-active-step-info">
                                <span className="operations-active-step-count">
                                    Step {op.currentStep + 1} of {op.steps.length}
                                </span>
                                {op.steps[op.currentStep] && (
                                    <span className="operations-active-step-desc">
                                        {op.steps[op.currentStep].description}
                                    </span>
                                )}
                            </div>
                            <div className="operations-active-progress-bar">
                                <div
                                    className="operations-active-progress-fill"
                                    style={{ width: `${((op.currentStep + 1) / op.steps.length) * 100}%` }}
                                />
                            </div>
                            <div className="operations-active-elapsed">
                                {formatElapsed(now - op.startedAt)}
                            </div>
                        </div>
                    )}

                    {/* Completed: inline results + step details */}
                    {op.status === 'completed' && (
                        <>
                            {/* Step errors summary (if any steps failed with continue) */}
                            {op.stepErrors && op.stepErrors.length > 0 && (
                                <div style={{ padding: '8px 0', color: 'var(--accent-warning, #f59e0b)', fontSize: '13px' }}>
                                    {op.stepErrors.length} step(s) had errors (continued):
                                    {op.stepErrors.map((se, i) => (
                                        <div key={i} style={{ marginTop: '4px', paddingLeft: '12px', fontSize: '12px', color: 'var(--text-secondary)' }}>
                                            Step "{se.command ?? 'navigate'}": {se.error}
                                        </div>
                                    ))}
                                </div>
                            )}

                            <div className="operations-active-actions">
                                <button
                                    className="operations-active-result-btn"
                                    onClick={() => handleViewResult(op)}
                                >
                                    {expandedOps.has(op.id) ? 'Hide Details' : 'View Details'}
                                </button>
                                <button
                                    className="operations-active-dismiss-btn"
                                    onClick={() => handleDismiss(op.id)}
                                >
                                    Dismiss
                                </button>
                            </div>

                            {/* Expanded: show each step + its result/error */}
                            {expandedOps.has(op.id) && (
                                <div style={{ marginTop: '8px', borderTop: '1px solid var(--border-primary, #2a2a4a)', paddingTop: '8px' }}>
                                    {op.steps.map((step, i) => {
                                        const stepErr = op.stepErrors?.find(se => se.stepId === step.id);
                                        return (
                                            <div key={step.id} style={{ display: 'flex', gap: '8px', padding: '4px 0', fontSize: '13px' }}>
                                                <span style={{ color: stepErr ? 'var(--accent-warning, #f59e0b)' : 'var(--accent-green, #10b981)', minWidth: '16px' }}>
                                                    {stepErr ? '\u2717' : '\u2713'}
                                                </span>
                                                <div>
                                                    <div style={{ color: 'var(--text-primary, #e0e0e0)' }}>
                                                        {step.description}
                                                    </div>
                                                    {step.command && (
                                                        <div style={{ fontSize: '11px', color: 'var(--text-tertiary, #6b7280)', marginTop: '2px' }}>
                                                            {step.command}({step.args ? JSON.stringify(step.args).slice(0, 100) : '{}'})
                                                        </div>
                                                    )}
                                                    {stepErr && (
                                                        <div style={{ fontSize: '11px', color: 'var(--accent-warning, #f59e0b)', marginTop: '2px' }}>
                                                            Error: {stepErr.error}
                                                        </div>
                                                    )}
                                                </div>
                                            </div>
                                        );
                                    })}
                                    {op.result !== undefined && op.result !== null && (
                                        <div style={{ marginTop: '8px', padding: '8px', background: 'var(--bg-tertiary, #0f0f1a)', borderRadius: '4px', fontSize: '12px', fontFamily: 'monospace', color: 'var(--text-secondary)', maxHeight: '200px', overflow: 'auto' }}>
                                            <div style={{ marginBottom: '4px', fontSize: '11px', color: 'var(--text-tertiary)' }}>Last step result:</div>
                                            {JSON.stringify(op.result, null, 2).slice(0, 1000)}
                                        </div>
                                    )}
                                </div>
                            )}
                        </>
                    )}

                    {/* Failed: error + retry */}
                    {op.status === 'failed' && (
                        <div className="operations-active-failure">
                            {op.error && (
                                <div className="operations-active-error">{op.error}</div>
                            )}
                            <div className="operations-active-actions">
                                <button
                                    className="operations-active-retry-btn"
                                    onClick={() => handleRetry(op)}
                                >
                                    Retry
                                </button>
                                <button
                                    className="operations-active-dismiss-btn"
                                    onClick={() => handleDismiss(op.id)}
                                >
                                    Dismiss
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            ))}
        </div>
    );
}

function getResultLabel(op: OperationEntry): string {
    const step0 = op.steps[0];
    const cmd = step0?.command;
    const nav = step0?.navigate;
    if (cmd === 'pyramid_build' || cmd === 'pyramid_create_slug') return 'View in Understanding';
    if (nav?.mode === 'search') return 'View in Search';
    if (nav?.mode === 'compose') return 'View in Compose';
    if (nav?.mode === 'fleet' || step0?.command === 'operator_api_call' || step0?.command === 'wire_api_call') return 'View in Fleet';
    return 'View Result';
}

// --- Component ---

export function OperationsMode() {
    const { operatorApiCall, wireApiCall, state, dispatch, setMode } = useAppContext();

    // --- Sub-tab state ---
    const [activeTab, setActiveTab] = useState<OperationsTab>('notifications');

    // --- Notification state ---
    const [notifications, setNotifications] = useState<Notification[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [filterType, setFilterType] = useState<FilterType>('all');
    const [sourceFilter, setSourceFilter] = useState<SourceFilter>('all');
    const [timeFilter, setTimeFilter] = useState<TimeFilter>('all');

    // --- Expandable row state ---
    const [expandedId, setExpandedId] = useState<string | null>(null);
    const [expandedContribution, setExpandedContribution] = useState<ContributionSummary | null>(null);
    const [expandLoading, setExpandLoading] = useState(false);

    // --- Rating state ---
    const [ratingAccuracy, setRatingAccuracy] = useState(0.5);
    const [ratingUsefulness, setRatingUsefulness] = useState(0.5);
    const [ratingSubmitting, setRatingSubmitting] = useState(false);
    const [ratingSuccess, setRatingSuccess] = useState<string | null>(null);
    const [ratingError, setRatingError] = useState<string | null>(null);
    const [flagOpen, setFlagOpen] = useState(false);
    const [flagSubmitting, setFlagSubmitting] = useState(false);

    // --- Message state ---
    const [messages, setMessages] = useState<WireMessage[]>([]);
    const [messagesLoading, setMessagesLoading] = useState(false);
    const [messagesError, setMessagesError] = useState<string | null>(null);

    // --- Fetch notifications ---
    const fetchNotifications = useCallback(async () => {
        try {
            const data: any = await operatorApiCall('GET', '/api/v1/wire/notifications?read=all');
            const list = data?.notifications || data || [];
            const notifs = Array.isArray(list) ? list : [];
            setNotifications(notifs);
            setError(null);
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

    useEffect(() => {
        fetchNotifications();
    }, [fetchNotifications]);

    useEffect(() => {
        const interval = setInterval(fetchNotifications, 30_000);
        return () => clearInterval(interval);
    }, [fetchNotifications]);

    // --- Fetch messages ---
    const fetchMessages = useCallback(async () => {
        setMessagesLoading(true);
        try {
            const data: any = await wireApiCall('GET', '/api/v1/wire/messages');
            const msgList = data?.data?.messages || data?.messages || data || [];
            const msgs: WireMessage[] = Array.isArray(msgList) ? msgList : [];
            setMessages(msgs);
            setMessagesError(null);
            const unread = data?.inbox?.unread ?? msgs.filter((m: WireMessage) => !m.read).length;
            dispatch({ type: 'SET_MESSAGE_COUNT', count: unread });
        } catch (err: any) {
            setMessagesError(err?.message || 'Failed to load messages');
        } finally {
            setMessagesLoading(false);
        }
    }, [wireApiCall, dispatch]);

    // Fetch messages when switching to messages tab
    useEffect(() => {
        if (activeTab === 'messages') {
            fetchMessages();
        }
    }, [activeTab, fetchMessages]);

    // --- Expand notification to show contribution ---
    const handleExpand = useCallback(async (notificationId: string, contributionId?: string) => {
        if (expandedId === notificationId) {
            setExpandedId(null);
            setExpandedContribution(null);
            return;
        }
        setExpandedId(notificationId);
        setExpandedContribution(null);

        if (!contributionId) return;

        setExpandLoading(true);
        try {
            const data: any = await wireApiCall('GET', `/api/v1/wire/contribution/${contributionId}`);
            const contribution = data?.contribution || data?.data || data;
            if (contribution) {
                setExpandedContribution({
                    id: contribution.id || contributionId,
                    title: contribution.title || 'Untitled',
                    teaser: contribution.teaser,
                    body: contribution.body,
                    contribution_type: contribution.contribution_type,
                    author_pseudonym: contribution.author_pseudonym,
                    topics: contribution.topics,
                    price: contribution.price,
                    significance: contribution.significance,
                    avg_rating: contribution.avg_rating,
                    rating_count: contribution.rating_count,
                    created_at: contribution.created_at,
                    entity_mentions: contribution.entity_mentions,
                });
            }
        } catch (err) {
            console.error('Failed to fetch contribution:', err);
        } finally {
            setExpandLoading(false);
        }
    }, [expandedId, wireApiCall]);

    // --- Mark notification read ---
    const handleMarkRead = useCallback(async (notificationId: string) => {
        try {
            await operatorApiCall('POST', '/api/v1/wire/notifications/mark-read', {
                notification_ids: [notificationId],
            });
            setNotifications(prev =>
                prev.map(n => n.id === notificationId ? { ...n, read: true } : n)
            );
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

    // --- Mark message read ---
    const handleMarkMessageRead = useCallback(async (messageId: string) => {
        try {
            await wireApiCall('POST', '/api/v1/wire/messages', { action: 'read', ids: [messageId] });
            setMessages(prev =>
                prev.map(m => m.id === messageId ? { ...m, read: true } : m)
            );
            dispatch({ type: 'SET_MESSAGE_COUNT', count: Math.max(0, state.messageCount - 1) });
        } catch (err) {
            console.error('Failed to mark message read:', err);
        }
    }, [wireApiCall, dispatch, state.messageCount]);

    const handleMarkAllMessagesRead = useCallback(async () => {
        const unreadIds = messages.filter(m => !m.read).map(m => m.id);
        if (unreadIds.length === 0) return;
        try {
            await wireApiCall('POST', '/api/v1/wire/messages', { action: 'read', ids: unreadIds });
            setMessages(prev =>
                prev.map(m => ({ ...m, read: true }))
            );
            dispatch({ type: 'SET_MESSAGE_COUNT', count: 0 });
        } catch (err) {
            console.error('Failed to mark all messages read:', err);
        }
    }, [messages, wireApiCall, dispatch]);

    // --- Rating helpers ---
    const resetRatingState = useCallback(() => {
        setRatingAccuracy(0.5);
        setRatingUsefulness(0.5);
        setRatingSuccess(null);
        setRatingError(null);
        setFlagOpen(false);
    }, []);

    useEffect(() => {
        resetRatingState();
    }, [expandedId, resetRatingState]);

    const handleSubmitRating = useCallback(async (contributionId: string) => {
        setRatingSubmitting(true);
        setRatingError(null);
        setRatingSuccess(null);
        try {
            await wireApiCall('POST', '/api/v1/wire/rate', {
                item_id: contributionId,
                item_type: 'contribution',
                accuracy: ratingAccuracy,
                usefulness: ratingUsefulness,
            });
            setRatingSuccess('Rating submitted');
        } catch (err: any) {
            const msg = err?.message || 'Failed to submit rating';
            if (msg.includes('own') || msg.includes('operator')) {
                setRatingError('Cannot rate your own or same-operator contributions');
            } else {
                setRatingError(msg);
            }
        } finally {
            setRatingSubmitting(false);
        }
    }, [wireApiCall, ratingAccuracy, ratingUsefulness]);

    const handleSubmitFlag = useCallback(async (contributionId: string, flag: string) => {
        setFlagSubmitting(true);
        setRatingError(null);
        try {
            await wireApiCall('POST', '/api/v1/wire/rate', {
                item_id: contributionId,
                item_type: 'contribution',
                flag,
            });
            setRatingSuccess(`Flagged as ${flag.replace(/_/g, ' ')}`);
            setFlagOpen(false);
        } catch (err: any) {
            setRatingError(err?.message || 'Failed to submit flag');
        } finally {
            setFlagSubmitting(false);
        }
    }, [wireApiCall]);

    // --- Apply filters ---
    const filtered = notifications.filter(n => {
        if (filterType === 'action_required' && !isActionRequired(n)) return false;
        if (filterType === 'informational' && isActionRequired(n)) return false;
        if (sourceFilter !== 'all' && getSource(n) !== sourceFilter) return false;
        if (!isWithinTimeFilter(n.created_at, timeFilter)) return false;
        return true;
    });

    const unreadNotifCount = notifications.filter(n => !n.read).length;
    const unreadMsgCount = messages.filter(m => !m.read).length;

    if (loading) {
        return (
            <div className="mode-container operations-mode">
                <div className="operations-loading">
                    <div className="loading-spinner" />
                    <span>Loading operations...</span>
                </div>
            </div>
        );
    }

    return (
        <div className="mode-container operations-mode">
            {/* Header */}
            <div className="operations-header">
                <h2>Operations</h2>
                <div className="operations-header-actions">
                    {activeTab === 'notifications' && unreadNotifCount > 0 && (
                        <button className="operations-mark-all" onClick={handleMarkAllRead}>
                            Mark all as read ({unreadNotifCount})
                        </button>
                    )}
                    {activeTab === 'messages' && unreadMsgCount > 0 && (
                        <button className="operations-mark-all" onClick={handleMarkAllMessagesRead}>
                            Mark all as read ({unreadMsgCount})
                        </button>
                    )}
                </div>
            </div>

            {/* Sub-tabs */}
            <div className="operations-section-tabs">
                <button
                    className={`operations-section-tab ${activeTab === 'notifications' ? 'operations-section-tab-active' : ''}`}
                    onClick={() => setActiveTab('notifications')}
                >
                    Notifications
                    {unreadNotifCount > 0 && <span className="operations-section-badge">{unreadNotifCount}</span>}
                </button>
                <button
                    className={`operations-section-tab ${activeTab === 'messages' ? 'operations-section-tab-active' : ''}`}
                    onClick={() => setActiveTab('messages')}
                >
                    Messages
                    {unreadMsgCount > 0 && <span className="operations-section-badge">{unreadMsgCount}</span>}
                </button>
                <button
                    className={`operations-section-tab ${activeTab === 'active' ? 'operations-section-tab-active' : ''}`}
                    onClick={() => setActiveTab('active')}
                >
                    Active
                </button>
                <button
                    className={`operations-section-tab ${activeTab === 'queue' ? 'operations-section-tab-active' : ''}`}
                    onClick={() => setActiveTab('queue')}
                >
                    Queue
                </button>
            </div>

            {/* Error state */}
            {error && activeTab === 'notifications' && (
                <div className="operations-error">
                    <span>{error}</span>
                    <button onClick={fetchNotifications} className="operations-retry-btn">Retry</button>
                </div>
            )}

            {/* Notifications sub-tab */}
            {activeTab === 'notifications' && (
                <>
                    {/* Filter bars */}
                    <div className="operations-filters">
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
                                <option value="infrastructure">Infrastructure</option>
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
                    <div className="operations-feed">
                        {filtered.length === 0 ? (
                            <div className="operations-empty">
                                <span className="operations-empty-icon">{'\u{1F514}'}</span>
                                <p className="operations-empty-title">No notifications</p>
                                <p className="operations-empty-desc">
                                    {filterType !== 'all' || sourceFilter !== 'all' || timeFilter !== 'all'
                                        ? 'Try adjusting your filters.'
                                        : 'You\'re all caught up.'}
                                </p>
                            </div>
                        ) : (
                            filtered.map(notification => {
                                const isUnread = !notification.read;
                                const isExpanded = expandedId === notification.id;
                                return (
                                    <div
                                        key={notification.id}
                                        className={`operations-item ${isUnread ? 'operations-item-unread' : ''} ${isExpanded ? 'operations-item-expanded' : ''}`}
                                    >
                                        <div
                                            className="operations-item-row"
                                            onClick={() => {
                                                if (isUnread) handleMarkRead(notification.id);
                                                handleExpand(notification.id, notification.source_contribution_id);
                                            }}
                                        >
                                            <div className="operations-item-icon">
                                                {getEventIcon(notification.event_type)}
                                            </div>
                                            <div className="operations-item-content">
                                                <div className="operations-item-title">
                                                    {formatEventTitle(notification)}
                                                </div>
                                                {notification.source_contribution_id && !isExpanded && (
                                                    <div className="operations-item-desc">
                                                        Contribution {notification.source_contribution_id.slice(0, 8)}...
                                                    </div>
                                                )}
                                                <div className="operations-item-meta">
                                                    <span className="operations-item-source">{getSource(notification)}</span>
                                                    <span className="operations-item-time">{timeAgo(notification.created_at)}</span>
                                                </div>
                                            </div>
                                            <div className="operations-item-actions">
                                                {isUnread && (
                                                    <span className="operations-unread-dot" title="Unread" />
                                                )}
                                                <span className="operations-expand-indicator">
                                                    {isExpanded ? '\u25B2' : '\u25BC'}
                                                </span>
                                            </div>
                                        </div>

                                        {/* Expanded contribution detail */}
                                        {isExpanded && (
                                            <div className="operations-expanded-detail">
                                                {expandLoading ? (
                                                    <div className="operations-expand-loading">
                                                        <div className="loading-spinner" />
                                                        <span>Loading contribution...</span>
                                                    </div>
                                                ) : expandedContribution ? (
                                                    <div className="operations-contribution-wrapper">
                                                        <ContributionCard
                                                            contribution={expandedContribution}
                                                            expanded={true}
                                                            expandedBody={expandedContribution.body || null}
                                                        />
                                                        {notification.source_contribution_id && (
                                                            <div className="operations-expanded-actions">
                                                                {/* Rating controls */}
                                                                <div className="rating-panel" onClick={(e) => e.stopPropagation()}>
                                                                    <div className="rating-panel-header">
                                                                        <span className="rating-panel-title">Rate Contribution</span>
                                                                        <span className="rating-panel-constraint" title="Server enforces: cannot rate own contributions or contributions from your operator's agents">
                                                                            Cannot rate own/same-operator contributions
                                                                        </span>
                                                                    </div>
                                                                    <div className="rating-sliders">
                                                                        <div className="rating-slider-row">
                                                                            <label className="rating-slider-label">Accuracy</label>
                                                                            <input
                                                                                type="range"
                                                                                className="rating-slider"
                                                                                min="0"
                                                                                max="1"
                                                                                step="0.05"
                                                                                value={ratingAccuracy}
                                                                                onChange={(e) => setRatingAccuracy(parseFloat(e.target.value))}
                                                                                disabled={ratingSubmitting}
                                                                            />
                                                                            <span className="rating-slider-value">{ratingAccuracy.toFixed(2)}</span>
                                                                        </div>
                                                                        <div className="rating-slider-row">
                                                                            <label className="rating-slider-label">Usefulness</label>
                                                                            <input
                                                                                type="range"
                                                                                className="rating-slider"
                                                                                min="0"
                                                                                max="1"
                                                                                step="0.05"
                                                                                value={ratingUsefulness}
                                                                                onChange={(e) => setRatingUsefulness(parseFloat(e.target.value))}
                                                                                disabled={ratingSubmitting}
                                                                            />
                                                                            <span className="rating-slider-value">{ratingUsefulness.toFixed(2)}</span>
                                                                        </div>
                                                                        <button
                                                                            className="rating-submit-btn"
                                                                            onClick={() => handleSubmitRating(notification.source_contribution_id!)}
                                                                            disabled={ratingSubmitting}
                                                                        >
                                                                            {ratingSubmitting ? 'Submitting...' : 'Submit Rating'}
                                                                        </button>
                                                                    </div>

                                                                    {/* Flag dropdown */}
                                                                    <div className="rating-flag-section">
                                                                        <button
                                                                            className="rating-flag-toggle"
                                                                            onClick={() => setFlagOpen(!flagOpen)}
                                                                            disabled={flagSubmitting}
                                                                        >
                                                                            Flag {flagOpen ? '\u25B2' : '\u25BC'}
                                                                        </button>
                                                                        {flagOpen && (
                                                                            <div className="rating-flag-dropdown">
                                                                                {(['outdated_data', 'harmful_content', 'plagiarism'] as const).map((flag) => (
                                                                                    <button
                                                                                        key={flag}
                                                                                        className="rating-flag-option"
                                                                                        onClick={() => handleSubmitFlag(notification.source_contribution_id!, flag)}
                                                                                        disabled={flagSubmitting}
                                                                                    >
                                                                                        {flag.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}
                                                                                    </button>
                                                                                ))}
                                                                            </div>
                                                                        )}
                                                                    </div>

                                                                    {/* Feedback messages */}
                                                                    {ratingSuccess && <div className="rating-feedback rating-feedback-success">{ratingSuccess}</div>}
                                                                    {ratingError && <div className="rating-feedback rating-feedback-error">{ratingError}</div>}
                                                                </div>

                                                                <button
                                                                    className="operations-navigate-btn"
                                                                    onClick={(e) => {
                                                                        e.stopPropagation();
                                                                        setMode('search');
                                                                    }}
                                                                >
                                                                    View in Search
                                                                </button>
                                                            </div>
                                                        )}
                                                    </div>
                                                ) : notification.source_contribution_id ? (
                                                    <div className="operations-expand-empty">
                                                        Could not load contribution details.
                                                    </div>
                                                ) : (
                                                    <div className="operations-expand-empty">
                                                        No contribution linked to this notification.
                                                    </div>
                                                )}
                                            </div>
                                        )}
                                    </div>
                                );
                            })
                        )}
                    </div>
                </>
            )}

            {/* Messages sub-tab */}
            {activeTab === 'messages' && (
                <div className="operations-messages">
                    {messagesError && (
                        <div className="operations-error">
                            <span>{messagesError}</span>
                            <button onClick={fetchMessages} className="operations-retry-btn">Retry</button>
                        </div>
                    )}

                    {messagesLoading ? (
                        <div className="operations-loading">
                            <div className="loading-spinner" />
                            <span>Loading messages...</span>
                        </div>
                    ) : messages.length === 0 ? (
                        <div className="operations-empty">
                            <span className="operations-empty-icon">{'\u{1F4AC}'}</span>
                            <p className="operations-empty-title">No messages</p>
                            <p className="operations-empty-desc">Circle messages will appear here.</p>
                        </div>
                    ) : (
                        <div className="operations-feed">
                            {messages.map(msg => (
                                <div
                                    key={msg.id}
                                    className={`operations-message-item ${!msg.read ? 'operations-message-unread' : ''}`}
                                >
                                    <div className="operations-message-header">
                                        <span className="operations-message-sender">
                                            {msg.sender_pseudonym || 'Unknown'}
                                        </span>
                                        <span className="operations-message-time">
                                            {timeAgo(msg.created_at)}
                                        </span>
                                        {!msg.read && (
                                            <span className="operations-unread-dot" title="Unread" />
                                        )}
                                    </div>
                                    <div className="operations-message-body">
                                        {msg.body}
                                    </div>
                                    {!msg.read && (
                                        <button
                                            className="operations-message-read-btn"
                                            onClick={() => handleMarkMessageRead(msg.id)}
                                        >
                                            Mark as read
                                        </button>
                                    )}
                                </div>
                            ))}
                        </div>
                    )}
                </div>
            )}

            {/* Active sub-tab */}
            {activeTab === 'active' && (
                <ActiveOperations />
            )}

            {/* Queue sub-tab */}
            {activeTab === 'queue' && (
                <div className="operations-empty">
                    <span className="operations-empty-icon">{'\u{1F4CB}'}</span>
                    <p className="operations-empty-title">No queued tasks.</p>
                    <p className="operations-empty-desc">Queued operations will appear here.</p>
                </div>
            )}
        </div>
    );
}
