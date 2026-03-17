import { useState, useEffect } from 'react';
import { useAppContext } from '../../contexts/AppContext';

interface HealthData {
    document_status_counts: Record<string, number>;
    total_revenue: number;
    revenue_trend_30d: number[];
    top_cited_documents: Array<{
        id: string;
        title: string;
        citation_count: number;
        revenue: number;
    }>;
    total_citations: number;
    citation_trend_7d: number[];
    anchored_count: number;
    external_corpora_citing: number;
    external_corpora_cited: number;
    self_citation_rate: number;
}

interface CorpusHealthDashboardProps {
    slug: string;
}

export function CorpusHealthDashboard({ slug }: CorpusHealthDashboardProps) {
    const { operatorApiCall } = useAppContext();
    const [health, setHealth] = useState<HealthData | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        setLoading(true);
        setError(null);
        operatorApiCall('GET', `/api/v1/wire/corpora/${slug}/health`)
            .then((data: any) => setHealth(data))
            .catch((err: any) => setError(err?.message || 'Failed to load health data'))
            .finally(() => setLoading(false));
    }, [slug, operatorApiCall]);

    if (loading) {
        return (
            <div className="health-dashboard">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading health metrics...</span>
                </div>
            </div>
        );
    }

    if (error || !health) {
        return (
            <div className="health-dashboard">
                <div className="corpora-error">
                    <span>{error || 'No health data available'}</span>
                </div>
            </div>
        );
    }

    const statusCounts = health.document_status_counts || {};
    const statusTotal = Object.values(statusCounts).reduce((a, b) => a + b, 0);
    const totalRevenue = health.total_revenue || 0;
    const totalCitations = health.total_citations || 0;
    const trendData = health.revenue_trend_30d || [];
    const maxTrend = Math.max(...trendData, 1);
    const citationTrend = health.citation_trend_7d || [];
    const recentCitationsPerDay = citationTrend.length > 0
        ? Math.round(citationTrend.reduce((a, b) => a + b, 0) / citationTrend.length)
        : 0;
    const recentRevenuePerWeek = trendData.length >= 7
        ? Math.round(trendData.slice(-7).reduce((a, b) => a + b, 0))
        : 0;
    const topCited = health.top_cited_documents || [];
    const topRevenueDoc = topCited.length > 0 ? topCited.sort((a, b) => b.revenue - a.revenue)[0] : null;
    const topCitedDoc = topCited.length > 0 ? topCited.sort((a, b) => b.citation_count - a.citation_count)[0] : null;

    return (
        <div className="health-dashboard">
            {/* Summary Cards */}
            <div className="health-summary">
                <div className="health-card">
                    <div className="health-card-label">Status</div>
                    <div className="health-card-value">{statusTotal} total</div>
                    <div className="health-card-breakdown">
                        <span className="count-published">
                            <span className="count-dot dot-green" /> {statusCounts.published || 0} published
                        </span>
                        <span className="count-draft">
                            <span className="count-dot dot-yellow" /> {statusCounts.draft || 0} draft
                        </span>
                        <span className="count-retracted">
                            <span className="count-dot dot-red" /> {statusCounts.retracted || 0} retracted
                        </span>
                    </div>
                </div>

                <div className="health-card">
                    <div className="health-card-label">Revenue</div>
                    <div className="health-card-value">{totalRevenue.toLocaleString()} cr</div>
                    <div className="health-card-breakdown">
                        <span className="health-trend-up">+{recentRevenuePerWeek.toLocaleString()}/week</span>
                        {topRevenueDoc && (
                            <span className="health-card-sub">Top: {topRevenueDoc.title}</span>
                        )}
                    </div>
                </div>

                <div className="health-card">
                    <div className="health-card-label">Citations</div>
                    <div className="health-card-value">{totalCitations.toLocaleString()} total</div>
                    <div className="health-card-breakdown">
                        <span className="health-trend-up">+{recentCitationsPerDay.toLocaleString()}/day</span>
                        {topCitedDoc && (
                            <span className="health-card-sub">Top: {topCitedDoc.title}</span>
                        )}
                    </div>
                </div>
            </div>

            {/* Top Cited Documents */}
            {topCited.length > 0 && (
                <div className="health-section">
                    <h4 className="health-section-title">Top Cited Documents</h4>
                    <div className="health-top-list">
                        {topCited.map((doc, i) => (
                            <div key={doc.id} className="health-top-item">
                                <span className="health-top-rank">{i + 1}.</span>
                                <span className="health-top-title">{doc.title}</span>
                                <span className="health-top-stat">{doc.citation_count} cit</span>
                                <span className="health-top-stat">{doc.revenue.toLocaleString()} cr</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Revenue Trend */}
            {trendData.length > 0 && (
                <div className="health-section">
                    <h4 className="health-section-title">Revenue Trend (30 days)</h4>
                    <div className="revenue-trend">
                        {trendData.map((val, i) => (
                            <div
                                key={i}
                                className="revenue-trend-bar"
                                style={{ height: `${Math.max((val / maxTrend) * 100, 2)}%` }}
                                title={`${val} cr`}
                            />
                        ))}
                    </div>
                </div>
            )}

            {/* Network Position */}
            <div className="health-section">
                <h4 className="health-section-title">Network Position</h4>
                <div className="health-network">
                    <div className="health-network-row">
                        <span className="health-network-label">Cited BY</span>
                        <span className="health-network-value">{health.external_corpora_citing} external corpora</span>
                    </div>
                    <div className="health-network-row">
                        <span className="health-network-label">Your corpus CITES</span>
                        <span className="health-network-value">{health.external_corpora_cited} external</span>
                    </div>
                    <div className="health-network-row">
                        <span className="health-network-label">Self-citation rate</span>
                        <span className="health-network-value">
                            {(health.self_citation_rate * 100).toFixed(0)}%
                            {health.self_citation_rate < 0.15 ? ' (healthy)' : ' (high)'}
                        </span>
                    </div>
                </div>
            </div>
        </div>
    );
}
