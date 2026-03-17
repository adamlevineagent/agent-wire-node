import { useState, useEffect } from 'react';
import { useAppContext } from '../../contexts/AppContext';

interface CorpusSummary {
    id: string;
    slug: string;
    title: string;
    description?: string;
    material_class?: string;
    visibility: 'public' | 'unlisted' | 'private';
    document_counts?: {
        published: number;
        draft: number;
        retracted: number;
        total: number;
    };
    total_revenue?: number;
    created_at?: string;
}

export function CorporaList() {
    const { operatorApiCall, pushView } = useAppContext();
    const [corpora, setCorpora] = useState<CorpusSummary[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        setLoading(true);
        setError(null);
        operatorApiCall('GET', '/api/v1/wire/corpora?owner=me&limit=100')
            .then((data: any) => {
                setCorpora(data?.items || []);
            })
            .catch((err: any) => {
                setError(err?.message || 'Failed to load corpora');
            })
            .finally(() => setLoading(false));
    }, [operatorApiCall]);

    const push = (view: string, props: Record<string, unknown>) => {
        pushView('agents', view, props);
    };

    if (loading) {
        return (
            <div className="corpora-list">
                <div className="corpora-list-header">
                    <h3>Corpora</h3>
                </div>
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading corpora...</span>
                </div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="corpora-list">
                <div className="corpora-list-header">
                    <h3>Corpora</h3>
                </div>
                <div className="corpora-error">
                    <span>Failed to load corpora: {error}</span>
                </div>
            </div>
        );
    }

    return (
        <div className="corpora-list">
            <div className="corpora-list-header">
                <h3>Corpora</h3>
                <button
                    className="stewardship-btn stewardship-btn-primary"
                    onClick={() => push('corpus-create', {})}
                >
                    + Create New
                </button>
            </div>

            {corpora.length === 0 ? (
                <div className="corpora-empty">
                    <p>No corpora yet. Create your first corpus to start publishing documents.</p>
                </div>
            ) : (
                <div className="corpora-grid">
                    {corpora.map((corpus) => {
                        const counts = corpus.document_counts || { published: 0, draft: 0, retracted: 0, total: 0 };
                        return (
                            <div
                                key={corpus.id || corpus.slug}
                                className="corpus-card"
                                onClick={() => push('corpus-detail', { slug: corpus.slug })}
                            >
                                <div className="corpus-card-header">
                                    <span className="corpus-card-title">{corpus.title || corpus.slug}</span>
                                    <span className={`visibility-badge visibility-${corpus.visibility}`}>
                                        {corpus.visibility}
                                    </span>
                                </div>

                                <div className="corpus-card-slug">{corpus.slug}</div>

                                {corpus.material_class && (
                                    <div className="corpus-card-class">{corpus.material_class}</div>
                                )}

                                <div className="corpus-card-counts">
                                    <span className="count-published" title="Published">
                                        <span className="count-dot dot-green" /> {counts.published}
                                    </span>
                                    <span className="count-draft" title="Draft">
                                        <span className="count-dot dot-yellow" /> {counts.draft}
                                    </span>
                                    <span className="count-retracted" title="Retracted">
                                        <span className="count-dot dot-red" /> {counts.retracted}
                                    </span>
                                    <span className="count-total" title="Total">
                                        {counts.total} total
                                    </span>
                                </div>

                                {corpus.total_revenue != null && (
                                    <div className="corpus-card-revenue">
                                        {corpus.total_revenue.toLocaleString()} cr earned
                                    </div>
                                )}

                                <div className="corpus-card-actions">
                                    <button
                                        className="stewardship-btn stewardship-btn-ghost"
                                        onClick={(e) => {
                                            e.stopPropagation();
                                            push('corpus-detail', { slug: corpus.slug });
                                        }}
                                    >
                                        Manage
                                    </button>
                                </div>
                            </div>
                        );
                    })}
                </div>
            )}
        </div>
    );
}
