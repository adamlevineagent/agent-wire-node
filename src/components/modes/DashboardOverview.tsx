import { useState, useEffect, useCallback, useRef } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { TunnelStatus } from '../TunnelStatus';
import { ImpactStats } from '../ImpactStats';
import { ActivityFeed } from '../ActivityFeed';
import { OperatorAlerts, AttentionItem, Recommendation } from '../dashboard/OperatorAlerts';

interface CorpusSummary {
    id: string;
    slug: string;
    title: string;
    doc_count?: number;
    draft_count?: number;
    total_revenue?: number;
}

interface OverviewPool {
    balance: number;
    spend_rate?: number;
    runway_days?: number;
}

interface AgentEconomics {
    credits_spent?: number;
    credits_earned?: number;
    queries_today?: number;
    daily_query_budget?: number;
}

interface OverviewAgent {
    pseudo_id: string;
    name: string;
    status: string;
    economics: AgentEconomics;
}

interface OverviewData {
    attention: AttentionItem[];
    recommendations: Recommendation[];
    review_queue_count: number;
    pool: OverviewPool;
    agents: OverviewAgent[];
}

interface ReviewQueueItem {
    id: string;
    contribution_id: string;
    title: string;
    body?: string;
    agent_pseudonym: string;
    significance?: string;
    created_at: string;
}

interface PulseTask {
    id: string;
    title: string;
    status: string;
    priority: string;
    created_at: string;
}

interface PulseNotification {
    id: string;
    event_type: string;
    source_contribution_id: string;
    source_agent_pseudonym: string;
    created_at: string;
}

interface PulseAgent {
    id: string;
    name: string;
    last_seen_at: string;
}

interface PulseCircle {
    circle_id: string;
    circle_name: string;
    unread_count: number;
}

interface PulseData {
    unread_messages: number;
    active_tasks: PulseTask[];
    unread_notifications: PulseNotification[];
    fleet_online: PulseAgent[];
    circle_activity: PulseCircle[];
    cost: number;
}

interface DashboardOverviewProps {
    onNavigateInfrastructure: () => void;
}

export function DashboardOverview({ onNavigateInfrastructure }: DashboardOverviewProps) {
    const { state, operatorApiCall, wireApiCall, setMode } = useAppContext();
    const { credits, tunnelStatus, syncState } = state;

    const [pendingRequests, setPendingRequests] = useState<number>(0);
    const [corpora, setCorpora] = useState<CorpusSummary[]>([]);
    const [loadingCorpora, setLoadingCorpora] = useState(false);
    const [pulse, setPulse] = useState<PulseData | null>(null);
    const [pulseLoading, setPulseLoading] = useState(false);

    // Overview state (operator-scoped, refreshes every 60s)
    const [overview, setOverview] = useState<OverviewData | null>(null);
    const [overviewLoading, setOverviewLoading] = useState(false);
    const overviewIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

    // Review queue state
    const [reviewQueue, setReviewQueue] = useState<ReviewQueueItem[]>([]);
    const [reviewQueueExpanded, setReviewQueueExpanded] = useState(false);
    const [reviewActionLoading, setReviewActionLoading] = useState<string | null>(null);
    const [reviewActionError, setReviewActionError] = useState<string | null>(null);
    const [reviewNotes, setReviewNotes] = useState<Record<string, string>>({});

    const folderCount = syncState ? Object.keys(syncState.linked_folders).length : 0;
    const docCount = syncState?.cached_documents?.length || 0;

    // Fetch pending contribution requests (wire-scoped endpoint — uses wireApiCall)
    useEffect(() => {
        wireApiCall('GET', '/api/v1/wire/requests/pending')
            .then((data: any) => {
                const requests = data?.requests || data || [];
                setPendingRequests(Array.isArray(requests) ? requests.length : 0);
            })
            .catch(() => setPendingRequests(0));
    }, [wireApiCall]);

    // Fetch corpora summary
    useEffect(() => {
        if (!state.operatorSessionToken) return;
        setLoadingCorpora(true);
        operatorApiCall('GET', '/api/v1/wire/corpora?owner=me')
            .then((data: any) => {
                const list = data?.corpora || data || [];
                setCorpora(Array.isArray(list) ? list : []);
            })
            .catch(() => setCorpora([]))
            .finally(() => setLoadingCorpora(false));
    }, [operatorApiCall, state.operatorSessionToken]);

    // Fetch pulse data
    useEffect(() => {
        setPulseLoading(true);
        wireApiCall('GET', '/api/v1/wire/pulse')
            .then((data: any) => {
                setPulse(data as PulseData);
            })
            .catch(() => setPulse(null))
            .finally(() => setPulseLoading(false));
    }, [wireApiCall]);

    // Fetch operator overview (every 60s — less frequent than pulse)
    const fetchOverview = useCallback(() => {
        if (!state.operatorSessionToken) return;
        setOverviewLoading(true);
        operatorApiCall('GET', '/api/v1/operator/overview')
            .then((data: unknown) => {
                const d = data as OverviewData;
                setOverview({
                    attention: d?.attention || [],
                    recommendations: d?.recommendations || [],
                    review_queue_count: d?.review_queue_count || 0,
                    pool: d?.pool || { balance: 0 },
                    agents: d?.agents || [],
                });
            })
            .catch(() => setOverview(null))
            .finally(() => setOverviewLoading(false));
    }, [operatorApiCall, state.operatorSessionToken]);

    useEffect(() => {
        fetchOverview();
        overviewIntervalRef.current = setInterval(fetchOverview, 60000);
        return () => {
            if (overviewIntervalRef.current) clearInterval(overviewIntervalRef.current);
        };
    }, [fetchOverview]);

    // Fetch review queue
    const fetchReviewQueue = useCallback(() => {
        if (!state.operatorSessionToken) return;
        operatorApiCall('GET', '/api/v1/operator/review-queue')
            .then((data: unknown) => {
                const d = data as { items?: ReviewQueueItem[] };
                setReviewQueue(d?.items || (Array.isArray(data) ? (data as ReviewQueueItem[]) : []));
            })
            .catch(() => setReviewQueue([]));
    }, [operatorApiCall, state.operatorSessionToken]);

    useEffect(() => {
        fetchReviewQueue();
    }, [fetchReviewQueue]);

    // Review queue actions
    const handleReviewAction = useCallback((heldId: string, action: 'approve' | 'reject') => {
        setReviewActionLoading(heldId);
        setReviewActionError(null);
        const note = reviewNotes[heldId];
        operatorApiCall('POST', `/api/v1/operator/review-queue/${heldId}`, {
            action,
            ...(note ? { note } : {}),
        })
            .then(() => {
                setReviewQueue(prev => prev.filter(item => item.id !== heldId));
                setReviewNotes(prev => {
                    const next = { ...prev };
                    delete next[heldId];
                    return next;
                });
            })
            .catch((err: unknown) => {
                const msg = err instanceof Error ? err.message : 'Review action failed';
                setReviewActionError(msg);
            })
            .finally(() => setReviewActionLoading(null));
    }, [operatorApiCall, reviewNotes]);

    // Credit calculations
    const poolBalance = overview?.pool?.balance ?? 0;
    const spendRate = overview?.pool?.spend_rate ?? 0;
    const runwayDays = overview?.pool?.runway_days ?? (spendRate > 0 ? Math.floor(poolBalance / spendRate) : null);
    const lowBalance = poolBalance > 0 && runwayDays !== null && runwayDays < 7;

    const totalCorpusDocs = corpora.reduce((sum, c) => sum + (c.doc_count || 0), 0);

    const tunnelConnected = tunnelStatus?.status === 'Connected';
    const storageUsed = credits?.total_bytes_formatted || '0 B';

    function formatTimeAgo(dateStr: string): string {
        const diff = Date.now() - new Date(dateStr).getTime();
        const mins = Math.floor(diff / 60000);
        if (mins < 1) return 'just now';
        if (mins < 60) return `${mins}m ago`;
        const hours = Math.floor(mins / 60);
        if (hours < 24) return `${hours}h ago`;
        return `${Math.floor(hours / 24)}d ago`;
    }

    function priorityClass(priority: string): string {
        switch (priority?.toLowerCase()) {
            case 'critical': return 'pulse-priority-critical';
            case 'high': return 'pulse-priority-high';
            case 'medium': return 'pulse-priority-medium';
            default: return 'pulse-priority-low';
        }
    }

    return (
        <>
            {/* Action Items Bar */}
            <div className="dashboard-action-items">
                <button
                    className="action-item"
                    onClick={() => setMode('operations')}
                >
                    <span className="action-item-icon">&#x1F514;</span>
                    <span className="action-item-label">Notifications</span>
                    {state.notificationCount > 0 && (
                        <span className="notification-badge">{state.notificationCount}</span>
                    )}
                </button>
                <button
                    className="action-item"
                    onClick={() => setMode('compose')}
                >
                    <span className="action-item-icon">&#x1F4DD;</span>
                    <span className="action-item-label">Pending Requests</span>
                    {pendingRequests > 0 && (
                        <span className="notification-badge">{pendingRequests}</span>
                    )}
                </button>
                {pulse && pulse.unread_messages > 0 && (
                    <button
                        className="action-item"
                        onClick={() => setMode('operations')}
                    >
                        <span className="action-item-icon">&#x1F4AC;</span>
                        <span className="action-item-label">Messages</span>
                        <span className="notification-badge">{pulse.unread_messages}</span>
                    </button>
                )}
            </div>

            {/* Summary Cards */}
            <div className="dashboard-summary-cards">
                <div className="summary-card" onClick={() => setMode('fleet')}>
                    <div className="summary-card-header">
                        <span className="summary-card-icon">&#x1F916;</span>
                        <span className="summary-card-title">Fleet</span>
                    </div>
                    <div className="summary-card-metrics">
                        <div className="summary-metric">
                            <span className="summary-metric-value">{credits?.documents_served || 0}</span>
                            <span className="summary-metric-label">contributions</span>
                        </div>
                        <div className="summary-metric">
                            <span className="summary-metric-value">{credits?.pulls_served_total || 0}</span>
                            <span className="summary-metric-label">pulls served</span>
                        </div>
                    </div>
                </div>

                <div className="summary-card" onClick={onNavigateInfrastructure}>
                    <div className="summary-card-header">
                        <span className="summary-card-icon">&#x1F5A5;&#xFE0F;</span>
                        <span className="summary-card-title">Infrastructure</span>
                    </div>
                    <div className="summary-card-metrics">
                        <div className="summary-metric">
                            <span className="summary-metric-value">{storageUsed}</span>
                            <span className="summary-metric-label">data served</span>
                        </div>
                        <div className="summary-metric">
                            <span className={`summary-metric-value ${tunnelConnected ? 'text-green' : 'text-muted'}`}>
                                {tunnelConnected ? 'Online' : 'Offline'}
                            </span>
                            <span className="summary-metric-label">tunnel status</span>
                        </div>
                    </div>
                </div>

                <div className="summary-card" onClick={() => setMode('fleet')}>
                    <div className="summary-card-header">
                        <span className="summary-card-icon">&#x1F4DA;</span>
                        <span className="summary-card-title">Corpora</span>
                    </div>
                    <div className="summary-card-metrics">
                        {loadingCorpora ? (
                            <div className="summary-metric">
                                <span className="summary-metric-value">&mdash;</span>
                                <span className="summary-metric-label">loading...</span>
                            </div>
                        ) : (
                            <>
                                <div className="summary-metric">
                                    <span className="summary-metric-value">{corpora.length}</span>
                                    <span className="summary-metric-label">{corpora.length === 1 ? 'corpus' : 'corpora'}</span>
                                </div>
                                <div className="summary-metric">
                                    <span className="summary-metric-value">{totalCorpusDocs}</span>
                                    <span className="summary-metric-label">documents</span>
                                </div>
                            </>
                        )}
                    </div>
                </div>
            </div>

            {/* Pulse Cards */}
            <div className="pulse-cards-grid">
                {/* Fleet Presence */}
                <div className="pulse-card" onClick={() => setMode('fleet')}>
                    <div className="pulse-card-header">
                        <span className="pulse-card-title">Fleet Online</span>
                        {pulse && <span className="pulse-card-count">{pulse.fleet_online.length}</span>}
                    </div>
                    <div className="pulse-card-body">
                        {pulseLoading && <span className="pulse-loading">Loading...</span>}
                        {!pulseLoading && (!pulse || pulse.fleet_online.length === 0) && (
                            <span className="pulse-empty">No agents online</span>
                        )}
                        {pulse?.fleet_online.map((agent) => (
                            <div key={agent.id} className="pulse-agent-row">
                                <span className="pulse-agent-dot" />
                                <span className="pulse-agent-name">{agent.name}</span>
                                <span className="pulse-agent-seen">{formatTimeAgo(agent.last_seen_at)}</span>
                            </div>
                        ))}
                    </div>
                </div>

                {/* Active Tasks */}
                <div className="pulse-card">
                    <div className="pulse-card-header">
                        <span className="pulse-card-title">Active Tasks</span>
                        {pulse && <span className="pulse-card-count">{pulse.active_tasks.length}</span>}
                    </div>
                    <div className="pulse-card-body">
                        {pulseLoading && <span className="pulse-loading">Loading...</span>}
                        {!pulseLoading && (!pulse || pulse.active_tasks.length === 0) && (
                            <span className="pulse-empty">No active tasks</span>
                        )}
                        {pulse?.active_tasks.map((task) => (
                            <div key={task.id} className="pulse-task-row">
                                <span className={`pulse-task-priority ${priorityClass(task.priority)}`}>{task.priority}</span>
                                <span className="pulse-task-title">{task.title}</span>
                                <span className="pulse-task-status">{task.status}</span>
                            </div>
                        ))}
                    </div>
                </div>

                {/* Circle Activity */}
                <div className="pulse-card">
                    <div className="pulse-card-header">
                        <span className="pulse-card-title">Circles</span>
                        {pulse && <span className="pulse-card-count">{pulse.circle_activity.length}</span>}
                    </div>
                    <div className="pulse-card-body">
                        {pulseLoading && <span className="pulse-loading">Loading...</span>}
                        {!pulseLoading && (!pulse || pulse.circle_activity.length === 0) && (
                            <span className="pulse-empty">No circle activity</span>
                        )}
                        {pulse?.circle_activity.map((circle) => (
                            <div key={circle.circle_id} className="pulse-circle-row">
                                <span className="pulse-circle-name">{circle.circle_name}</span>
                                {circle.unread_count > 0 && (
                                    <span className="pulse-circle-badge">{circle.unread_count}</span>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            </div>

            {/* Operator Alerts + Recommendations */}
            {overview && (
                <OperatorAlerts
                    attention={overview.attention}
                    recommendations={overview.recommendations}
                />
            )}

            {/* Credit Monitoring */}
            {overview && (
                <div className="credit-monitoring-section">
                    <div className="credit-monitoring-header">
                        <span className="credit-monitoring-title">Credit Pool</span>
                        {lowBalance && (
                            <span className="credit-low-balance-badge">Low Balance</span>
                        )}
                    </div>
                    <div className="credit-monitoring-metrics">
                        <div className="credit-metric">
                            <span className="credit-metric-value">{poolBalance.toFixed(2)}</span>
                            <span className="credit-metric-label">pool balance</span>
                        </div>
                        {spendRate > 0 && (
                            <div className="credit-metric">
                                <span className="credit-metric-value">{spendRate.toFixed(2)}/day</span>
                                <span className="credit-metric-label">spend rate</span>
                            </div>
                        )}
                        {runwayDays !== null && (
                            <div className="credit-metric">
                                <span className={`credit-metric-value ${lowBalance ? 'credit-metric-warning' : ''}`}>
                                    {runwayDays}d
                                </span>
                                <span className="credit-metric-label">runway</span>
                            </div>
                        )}
                    </div>

                    {/* Per-agent spend breakdown */}
                    {overview.agents.length > 0 && (
                        <div className="credit-agent-breakdown">
                            <span className="credit-breakdown-title">Per-Agent Spend</span>
                            <div className="credit-agent-list">
                                {overview.agents.map((agent) => (
                                    <div key={agent.pseudo_id} className="credit-agent-row">
                                        <span className="credit-agent-name">{agent.name}</span>
                                        <span className="credit-agent-spent">
                                            {(agent.economics?.credits_spent ?? 0).toFixed(2)} spent
                                        </span>
                                        <span className="credit-agent-earned">
                                            {(agent.economics?.credits_earned ?? 0).toFixed(2)} earned
                                        </span>
                                    </div>
                                ))}
                            </div>
                        </div>
                    )}
                </div>
            )}

            {/* Review Queue */}
            {overview && overview.review_queue_count > 0 && (
                <div className="review-queue-section">
                    <div
                        className="review-queue-header"
                        onClick={() => setReviewQueueExpanded(!reviewQueueExpanded)}
                    >
                        <span className="review-queue-title">Review Queue</span>
                        <span className="review-queue-badge">{overview.review_queue_count}</span>
                        <span className="review-queue-toggle">{reviewQueueExpanded ? '\u25B2' : '\u25BC'}</span>
                    </div>
                    {reviewQueueExpanded && (
                        <div className="review-queue-list">
                            {reviewActionError && (
                                <div className="corpora-error" style={{ marginBottom: '8px' }}>
                                    <span>{reviewActionError}</span>
                                </div>
                            )}
                            {reviewQueue.length === 0 && !reviewActionError && (
                                <span className="pulse-loading">Loading queue...</span>
                            )}
                            {reviewQueue.map((item) => (
                                <div key={item.id} className="review-queue-item">
                                    <div className="review-queue-item-info">
                                        <span className="review-queue-item-title">{item.title}</span>
                                        <span className="review-queue-item-agent">by {item.agent_pseudonym}</span>
                                        {item.significance && (
                                            <span className="review-queue-item-significance">{item.significance}</span>
                                        )}
                                    </div>
                                    <div className="review-queue-item-actions">
                                        <input
                                            type="text"
                                            className="review-queue-note-input"
                                            placeholder="Optional note..."
                                            value={reviewNotes[item.id] || ''}
                                            onChange={(e) => setReviewNotes(prev => ({ ...prev, [item.id]: e.target.value }))}
                                        />
                                        <button
                                            className="review-queue-btn review-queue-approve"
                                            disabled={reviewActionLoading === item.id}
                                            onClick={() => handleReviewAction(item.id, 'approve')}
                                        >
                                            {reviewActionLoading === item.id ? '...' : 'Approve'}
                                        </button>
                                        <button
                                            className="review-queue-btn review-queue-reject"
                                            disabled={reviewActionLoading === item.id}
                                            onClick={() => handleReviewAction(item.id, 'reject')}
                                        >
                                            {reviewActionLoading === item.id ? '...' : 'Reject'}
                                        </button>
                                    </div>
                                </div>
                            ))}
                        </div>
                    )}
                </div>
            )}

            {/* Unread Notifications (inline) */}
            {pulse && pulse.unread_notifications.length > 0 && (
                <div className="pulse-notifications-strip">
                    <span className="pulse-notifications-label">Recent notifications</span>
                    <div className="pulse-notifications-list">
                        {pulse.unread_notifications.slice(0, 5).map((n) => (
                            <div key={n.id} className="pulse-notification-item">
                                <span className="pulse-notification-type">{n.event_type.replace(/_/g, ' ')}</span>
                                {n.source_agent_pseudonym && (
                                    <span className="pulse-notification-agent">from {n.source_agent_pseudonym}</span>
                                )}
                                <span className="pulse-notification-time">{formatTimeAgo(n.created_at)}</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Main Grid */}
            <div className="cc-grid">
                {/* Left Panel -- Tunnel Status */}
                <aside className="cc-sidebar">
                    <TunnelStatus credits={credits} tunnelStatus={tunnelStatus} />
                </aside>

                {/* Center Panel -- Impact Stats */}
                <main className="cc-main">
                    <ImpactStats credits={credits} />
                </main>

                {/* Right Panel -- Activity Feed */}
                <aside className="cc-activity">
                    <div className="panel-header">
                        <h3>Wire Activity Feed</h3>
                        <span className="live-dot" />
                    </div>
                    <ActivityFeed credits={credits} />
                </aside>
            </div>

            {/* Bottom Bar -- Network Summary */}
            <footer className="cc-footer">
                <div className="network-pulse">
                    <div className="pulse-waveform">
                        <div className="pulse-line" />
                        <div className="pulse-line delay" />
                    </div>
                    <div className="network-stats">
                        <div className="net-stat">
                            <span className="net-value">{folderCount}</span>
                            <span className="net-label">{folderCount === 1 ? "folder linked" : "folders linked"}</span>
                        </div>
                        <div className="net-divider" />
                        <div className="net-stat">
                            <span className="net-value">{docCount}</span>
                            <span className="net-label">documents cached</span>
                        </div>
                        <div className="net-divider" />
                        <div className="net-stat">
                            <span className="net-value">{credits?.total_bytes_formatted || "0 B"}</span>
                            <span className="net-label">total served</span>
                        </div>
                        <div className="net-divider" />
                        <div className="net-stat">
                            <span className="net-value glow">{Math.floor((credits?.server_credit_balance || 0) > 0 ? credits!.server_credit_balance : (credits?.credits_earned || 0))}</span>
                            <span className="net-label">credits earned</span>
                        </div>
                    </div>
                </div>
            </footer>
        </>
    );
}
