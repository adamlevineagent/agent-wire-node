import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

interface DocumentData {
    id: string;
    title: string;
    body?: string;
    status: 'draft' | 'published' | 'retracted';
    format?: string;
    word_count?: number;
    created_at?: string;
    updated_at?: string;
    pricing_mode?: 'free' | 'fixed' | 'surge';
    price?: number;
    author_share?: number;
    steward_share?: number;
    anchored?: boolean;
    total_revenue?: number;
    citation_count?: number;
    revenue_breakdown?: Array<{ source: string; amount: number }>;
    status_history?: Array<{ status: string; timestamp: string }>;
    versions?: Array<{ version: number; created_at: string; summary?: string }>;
}

interface DocumentDetailProps {
    documentId: string;
}

export function DocumentDetail({ documentId }: DocumentDetailProps) {
    const { operatorApiCall, popView } = useAppContext();
    const [doc, setDoc] = useState<DocumentData | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);
    const [bodyExpanded, setBodyExpanded] = useState(false);

    // Editable state
    const [editingTitle, setEditingTitle] = useState(false);
    const [titleDraft, setTitleDraft] = useState('');
    const [pricingMode, setPricingMode] = useState<string>('free');
    const [price, setPrice] = useState<number>(0);
    const [authorShare, setAuthorShare] = useState<number>(70);
    const [stewardShare, setStewardShare] = useState<number>(30);

    const pop = () => popView('knowledge');

    const loadDoc = useCallback(() => {
        setLoading(true);
        setError(null);
        operatorApiCall('GET', `/api/v1/wire/documents/${documentId}`)
            .then((data: any) => {
                setDoc(data);
                setTitleDraft(data?.title || '');
                setPricingMode(data?.pricing_mode || 'free');
                setPrice(data?.price || 0);
                setAuthorShare(data?.author_share ?? 70);
                setStewardShare(data?.steward_share ?? 30);
            })
            .catch((err: any) => setError(err?.message || 'Failed to load document'))
            .finally(() => setLoading(false));
    }, [documentId, operatorApiCall]);

    useEffect(() => { loadDoc(); }, [loadDoc]);

    const patchDoc = async (fields: Record<string, unknown>) => {
        setSaving(true);
        try {
            const updated = await operatorApiCall('PATCH', `/api/v1/wire/documents/${documentId}`, fields) as any;
            if (updated) setDoc(prev => prev ? { ...prev, ...updated } : updated);
            else loadDoc();
        } catch (err: any) {
            setError(err?.message || 'Failed to save');
        } finally {
            setSaving(false);
        }
    };

    const saveTitle = async () => {
        if (titleDraft !== doc?.title) await patchDoc({ title: titleDraft });
        setEditingTitle(false);
    };

    const transitionStatus = async (newStatus: 'published' | 'retracted') => {
        await patchDoc({ status: newStatus });
    };

    const savePricing = async () => {
        await patchDoc({ pricing_mode: pricingMode, price });
    };

    const saveSplits = async () => {
        await patchDoc({ author_share: authorShare, steward_share: stewardShare });
    };

    const handleAuthorShareChange = (val: number) => {
        const clamped = Math.max(0, Math.min(100, val));
        setAuthorShare(clamped);
        setStewardShare(100 - clamped);
    };

    if (loading) {
        return (
            <div className="document-detail">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading document...</span>
                </div>
            </div>
        );
    }

    if (error && !doc) {
        return (
            <div className="document-detail">
                <div className="corpus-detail-nav">
                    <button className="stewardship-btn stewardship-btn-ghost" onClick={pop}>Back</button>
                </div>
                <div className="corpora-error"><span>{error}</span></div>
            </div>
        );
    }

    const bodyPreview = doc?.body
        ? (bodyExpanded ? doc.body : doc.body.slice(0, 500) + (doc.body.length > 500 ? '...' : ''))
        : null;

    return (
        <div className="document-detail">
            {/* Navigation */}
            <div className="corpus-detail-nav">
                <button className="stewardship-btn stewardship-btn-ghost" onClick={pop}>Back</button>
                <span className="corpus-detail-breadcrumb">Document / {doc?.title || documentId}</span>
                {saving && <span className="doc-saving-indicator">Saving...</span>}
            </div>

            {error && <div className="curation-error-banner">{error}</div>}

            <div className="document-detail-panels">
                {/* Content Panel */}
                <div className="doc-panel">
                    <h4 className="doc-panel-title">Content</h4>
                    <div className="doc-panel-body">
                        {/* Title */}
                        <div className="doc-field">
                            <label className="doc-field-label">Title</label>
                            {editingTitle ? (
                                <input
                                    className="stewardship-input"
                                    value={titleDraft}
                                    onChange={e => setTitleDraft(e.target.value)}
                                    onBlur={saveTitle}
                                    onKeyDown={e => e.key === 'Enter' && saveTitle()}
                                    autoFocus
                                />
                            ) : (
                                <div
                                    className={`doc-field-value ${doc?.status === 'draft' ? 'doc-field-editable' : ''}`}
                                    onClick={() => doc?.status === 'draft' && setEditingTitle(true)}
                                >
                                    {doc?.title || 'Untitled'}
                                    {doc?.status === 'draft' && <span className="doc-edit-hint">click to edit</span>}
                                </div>
                            )}
                        </div>

                        {/* Body Preview */}
                        {bodyPreview && (
                            <div className="doc-field">
                                <label className="doc-field-label">Body</label>
                                <div className="doc-body-preview">
                                    {bodyPreview}
                                    {doc?.body && doc.body.length > 500 && (
                                        <button
                                            className="stewardship-btn stewardship-btn-ghost stewardship-btn-sm"
                                            onClick={() => setBodyExpanded(!bodyExpanded)}
                                        >
                                            {bodyExpanded ? 'Collapse' : 'Expand'}
                                        </button>
                                    )}
                                </div>
                            </div>
                        )}

                        {/* Meta */}
                        <div className="doc-meta-grid">
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Format</span>
                                <span className="doc-meta-value">{doc?.format || '--'}</span>
                            </div>
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Word Count</span>
                                <span className="doc-meta-value">
                                    {doc?.word_count != null ? doc.word_count.toLocaleString() : '--'}
                                </span>
                            </div>
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Created</span>
                                <span className="doc-meta-value">
                                    {doc?.created_at ? new Date(doc.created_at).toLocaleDateString() : '--'}
                                </span>
                            </div>
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Updated</span>
                                <span className="doc-meta-value">
                                    {doc?.updated_at ? new Date(doc.updated_at).toLocaleDateString() : '--'}
                                </span>
                            </div>
                        </div>
                    </div>
                </div>

                {/* Status Panel */}
                <div className="doc-panel">
                    <h4 className="doc-panel-title">Status</h4>
                    <div className="doc-panel-body">
                        <div className="doc-status-current">
                            <span className={`status-badge status-badge-${doc?.status}`}>
                                {doc?.status}
                            </span>
                            <div className="doc-status-actions">
                                {doc?.status === 'draft' && (
                                    <button
                                        className="stewardship-btn stewardship-btn-primary"
                                        onClick={() => transitionStatus('published')}
                                        disabled={saving}
                                    >
                                        Publish
                                    </button>
                                )}
                                {doc?.status === 'published' && (
                                    <button
                                        className="stewardship-btn stewardship-btn-warn"
                                        onClick={() => transitionStatus('retracted')}
                                        disabled={saving}
                                    >
                                        Retract
                                    </button>
                                )}
                            </div>
                        </div>
                        {doc?.status_history && doc.status_history.length > 0 && (
                            <div className="doc-status-history">
                                <label className="doc-field-label">History</label>
                                {doc.status_history.map((entry, i) => (
                                    <div key={i} className="doc-status-history-entry">
                                        <span className={`status-badge status-badge-${entry.status} status-badge-sm`}>
                                            {entry.status}
                                        </span>
                                        <span className="doc-meta-value">
                                            {new Date(entry.timestamp).toLocaleString()}
                                        </span>
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>
                </div>

                {/* Pricing Panel */}
                <div className="doc-panel">
                    <h4 className="doc-panel-title">Pricing</h4>
                    <div className="doc-panel-body">
                        <div className="doc-field">
                            <label className="doc-field-label">Mode</label>
                            <select
                                className="stewardship-select"
                                value={pricingMode}
                                onChange={e => setPricingMode(e.target.value)}
                                disabled={doc?.anchored}
                            >
                                <option value="free">Free</option>
                                <option value="fixed">Fixed</option>
                                <option value="surge">Surge</option>
                            </select>
                        </div>
                        {pricingMode !== 'free' && (
                            <div className="doc-field">
                                <label className="doc-field-label">Price (credits)</label>
                                <input
                                    type="number"
                                    className="stewardship-input"
                                    value={price}
                                    onChange={e => setPrice(Number(e.target.value))}
                                    disabled={doc?.anchored}
                                    min={0}
                                />
                            </div>
                        )}
                        <button
                            className="stewardship-btn stewardship-btn-primary stewardship-btn-sm"
                            onClick={savePricing}
                            disabled={saving || doc?.anchored}
                        >
                            Save Pricing
                        </button>
                        {doc?.anchored && (
                            <div className="doc-anchored-notice">
                                Pricing is anchored and cannot be changed.
                            </div>
                        )}
                    </div>
                </div>

                {/* Splits Panel */}
                <div className="doc-panel">
                    <h4 className="doc-panel-title">
                        Splits
                        {doc?.anchored && <span className="doc-lock-icon" title="Anchored">locked</span>}
                    </h4>
                    <div className="doc-panel-body">
                        <div className="doc-field">
                            <label className="doc-field-label">Author Share: {authorShare}%</label>
                            <input
                                type="range"
                                min={0}
                                max={100}
                                value={authorShare}
                                onChange={e => handleAuthorShareChange(Number(e.target.value))}
                                disabled={doc?.anchored}
                                className="stewardship-slider"
                            />
                        </div>
                        <div className="doc-field">
                            <label className="doc-field-label">Steward Share: {stewardShare}%</label>
                            <input
                                type="range"
                                min={0}
                                max={100}
                                value={stewardShare}
                                onChange={e => handleAuthorShareChange(100 - Number(e.target.value))}
                                disabled={doc?.anchored}
                                className="stewardship-slider"
                            />
                        </div>
                        <button
                            className="stewardship-btn stewardship-btn-primary stewardship-btn-sm"
                            onClick={saveSplits}
                            disabled={saving || doc?.anchored}
                        >
                            Save Splits
                        </button>
                        {doc?.anchored && (
                            <div className="doc-anchored-notice">
                                Splits are anchored and cannot be changed.
                            </div>
                        )}
                    </div>
                </div>

                {/* Economics Panel */}
                <div className="doc-panel">
                    <h4 className="doc-panel-title">Economics</h4>
                    <div className="doc-panel-body">
                        <div className="doc-meta-grid">
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Total Revenue</span>
                                <span className="doc-meta-value doc-meta-value-lg">
                                    {doc?.total_revenue != null ? `${doc.total_revenue.toLocaleString()} cr` : '--'}
                                </span>
                            </div>
                            <div className="doc-meta-item">
                                <span className="doc-meta-label">Citations</span>
                                <span className="doc-meta-value doc-meta-value-lg">
                                    {doc?.citation_count != null ? doc.citation_count.toLocaleString() : '--'}
                                </span>
                            </div>
                        </div>
                        {doc?.revenue_breakdown && doc.revenue_breakdown.length > 0 && (
                            <div className="doc-revenue-breakdown">
                                <label className="doc-field-label">Revenue Breakdown</label>
                                {doc.revenue_breakdown.map((entry, i) => (
                                    <div key={i} className="doc-revenue-row">
                                        <span>{entry.source}</span>
                                        <span>{entry.amount.toLocaleString()} cr</span>
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>
                </div>

                {/* Versions Panel */}
                {doc?.versions && doc.versions.length > 0 && (
                    <div className="doc-panel">
                        <h4 className="doc-panel-title">Versions</h4>
                        <div className="doc-panel-body">
                            {doc.versions.map((v, i) => (
                                <div key={i} className="doc-version-row">
                                    <span className="doc-version-num">v{v.version}</span>
                                    <span className="doc-meta-value">
                                        {new Date(v.created_at).toLocaleString()}
                                    </span>
                                    {v.summary && <span className="doc-version-summary">{v.summary}</span>}
                                </div>
                            ))}
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
}
