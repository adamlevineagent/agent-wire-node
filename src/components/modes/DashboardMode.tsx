import { useState, useEffect } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { TunnelStatus } from '../TunnelStatus';
import { ImpactStats } from '../ImpactStats';
import { ActivityFeed } from '../ActivityFeed';

interface CorpusSummary {
    id: string;
    slug: string;
    title: string;
    doc_count?: number;
    draft_count?: number;
    total_revenue?: number;
}

export function DashboardMode() {
    const { state, operatorApiCall, setMode } = useAppContext();
    const { credits, tunnelStatus, syncState } = state;

    const [pendingRequests, setPendingRequests] = useState<number>(0);
    const [corpora, setCorpora] = useState<CorpusSummary[]>([]);
    const [loadingCorpora, setLoadingCorpora] = useState(false);

    const folderCount = syncState ? Object.keys(syncState.linked_folders).length : 0;
    const docCount = syncState?.cached_documents?.length || 0;

    // Fetch pending contribution requests
    useEffect(() => {
        if (!state.operatorSessionToken) return;
        operatorApiCall('GET', '/api/v1/wire/requests/pending')
            .then((data: any) => {
                const requests = data?.requests || data || [];
                setPendingRequests(Array.isArray(requests) ? requests.length : 0);
            })
            .catch(() => setPendingRequests(0));
    }, [operatorApiCall, state.operatorSessionToken]);

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

    const totalCorpusDocs = corpora.reduce((sum, c) => sum + (c.doc_count || 0), 0);
    const totalDrafts = corpora.reduce((sum, c) => sum + (c.draft_count || 0), 0);
    const totalRevenue = corpora.reduce((sum, c) => sum + (c.total_revenue || 0), 0);

    const tunnelConnected = tunnelStatus?.status === 'Connected';
    const storageUsed = credits?.total_bytes_formatted || '0 B';

    return (
        <div className="mode-container dashboard-enhanced">
            {/* Action Items Bar */}
            <div className="dashboard-action-items">
                <button
                    className="action-item"
                    onClick={() => setMode('activity')}
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
                <button
                    className="action-item"
                    onClick={() => setMode('warroom')}
                >
                    <span className="action-item-icon">&#x1F4E1;</span>
                    <span className="action-item-label">Curation Queue</span>
                    {totalDrafts > 0 && (
                        <span className="notification-badge">{totalDrafts}</span>
                    )}
                </button>
            </div>

            {/* Summary Cards */}
            <div className="dashboard-summary-cards">
                <div className="summary-card" onClick={() => setMode('agents')}>
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

                <div className="summary-card" onClick={() => setMode('node')}>
                    <div className="summary-card-header">
                        <span className="summary-card-icon">&#x1F5A5;&#xFE0F;</span>
                        <span className="summary-card-title">Node</span>
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

                <div className="summary-card" onClick={() => setMode('warroom')}>
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
        </div>
    );
}
