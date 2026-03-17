import { useState, useEffect } from 'react';
import { useAppContext } from '../../contexts/AppContext';

interface CurationItem {
    id: string;
    title: string;
    slug?: string;
    status: string;
    citations?: number;
    days_since_publish?: number;
    created_at?: string;
}

interface CurationData {
    pending_publish: CurationItem[];
    approaching_anchor: CurationItem[];
    zero_citations: CurationItem[];
}

interface CurationQueueProps {
    slug: string;
}

export function CurationQueue({ slug }: CurationQueueProps) {
    const { operatorApiCall, pushView } = useAppContext();
    const [data, setData] = useState<CurationData | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [actionLoading, setActionLoading] = useState<string | null>(null);

    const loadData = () => {
        setLoading(true);
        setError(null);
        operatorApiCall('GET', `/api/v1/wire/corpora/${slug}/curation`)
            .then((d: any) => setData(d))
            .catch((err: any) => setError(err?.message || 'Failed to load curation queue'))
            .finally(() => setLoading(false));
    };

    useEffect(() => { loadData(); }, [slug, operatorApiCall]);

    const publishAll = async () => {
        if (!data?.pending_publish.length) return;
        setActionLoading('publish-all');
        try {
            await operatorApiCall('POST', `/api/v1/wire/corpora/${slug}/bulk`, {
                action: 'publish',
                document_ids: data.pending_publish.map(d => d.id),
            });
            loadData();
        } catch (err: any) {
            setError(err?.message || 'Publish failed');
        } finally {
            setActionLoading(null);
        }
    };

    const bulkRetag = async (ids: string[]) => {
        setActionLoading('retag');
        try {
            await operatorApiCall('POST', `/api/v1/wire/corpora/${slug}/bulk`, {
                action: 'retag',
                document_ids: ids,
            });
            loadData();
        } catch (err: any) {
            setError(err?.message || 'Retag failed');
        } finally {
            setActionLoading(null);
        }
    };

    const push = (view: string, props: Record<string, unknown>) => {
        pushView('agents', view, props);
    };

    if (loading) {
        return (
            <div className="curation-queue">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading curation queue...</span>
                </div>
            </div>
        );
    }

    if (error && !data) {
        return (
            <div className="curation-queue">
                <div className="corpora-error"><span>{error}</span></div>
            </div>
        );
    }

    const pending = data?.pending_publish || [];
    const approaching = data?.approaching_anchor || [];
    const zeroCit = data?.zero_citations || [];
    const isEmpty = pending.length === 0 && approaching.length === 0 && zeroCit.length === 0;

    return (
        <div className="curation-queue">
            {error && <div className="curation-error-banner">{error}</div>}

            {isEmpty ? (
                <div className="curation-empty">
                    <p>No items need attention. Your corpus is in good shape.</p>
                </div>
            ) : (
                <>
                    {/* Pending Publish */}
                    {pending.length > 0 && (
                        <div className="curation-section">
                            <div className="curation-section-header">
                                <h4>
                                    <span className="curation-icon curation-icon-pending" />
                                    Pending Publish ({pending.length})
                                </h4>
                                <button
                                    className="stewardship-btn stewardship-btn-primary"
                                    disabled={actionLoading === 'publish-all'}
                                    onClick={publishAll}
                                >
                                    {actionLoading === 'publish-all' ? 'Publishing...' : 'Publish All'}
                                </button>
                            </div>
                            <div className="curation-items">
                                {pending.map((item) => (
                                    <div key={item.id} className="curation-item">
                                        <span className="curation-item-title">{item.title}</span>
                                        <span className="status-badge-draft">draft</span>
                                        <button
                                            className="stewardship-btn stewardship-btn-ghost"
                                            onClick={() => push('document-detail', { documentId: item.id })}
                                        >
                                            Review
                                        </button>
                                    </div>
                                ))}
                            </div>
                        </div>
                    )}

                    {/* Approaching Anchoring */}
                    {approaching.length > 0 && (
                        <div className="curation-section">
                            <div className="curation-section-header">
                                <h4>
                                    <span className="curation-icon curation-icon-anchor" />
                                    Approaching Anchoring ({approaching.length})
                                </h4>
                            </div>
                            <div className="curation-items">
                                {approaching.map((item) => (
                                    <div key={item.id} className="curation-item">
                                        <span className="curation-item-title">{item.title}</span>
                                        <span className="curation-item-meta">
                                            {item.citations} citations
                                        </span>
                                        <button
                                            className="stewardship-btn stewardship-btn-warm"
                                            onClick={() => push('document-detail', { documentId: item.id })}
                                        >
                                            Review Splits
                                        </button>
                                    </div>
                                ))}
                            </div>
                        </div>
                    )}

                    {/* Zero Citations */}
                    {zeroCit.length > 0 && (
                        <div className="curation-section">
                            <div className="curation-section-header">
                                <h4>
                                    <span className="curation-icon curation-icon-zero" />
                                    Zero Citations, 7+ days ({zeroCit.length})
                                </h4>
                                <button
                                    className="stewardship-btn stewardship-btn-ghost"
                                    disabled={actionLoading === 'retag'}
                                    onClick={() => bulkRetag(zeroCit.map(d => d.id))}
                                >
                                    {actionLoading === 'retag' ? 'Retagging...' : 'Bulk Retag'}
                                </button>
                            </div>
                            <div className="curation-items">
                                {zeroCit.map((item) => (
                                    <div key={item.id} className="curation-item">
                                        <span className="curation-item-title">{item.title}</span>
                                        <span className="curation-item-meta">
                                            {item.days_since_publish}d, 0 citations
                                        </span>
                                        <button
                                            className="stewardship-btn stewardship-btn-ghost"
                                            onClick={() => push('document-detail', { documentId: item.id })}
                                        >
                                            Review
                                        </button>
                                    </div>
                                ))}
                            </div>
                        </div>
                    )}
                </>
            )}
        </div>
    );
}
