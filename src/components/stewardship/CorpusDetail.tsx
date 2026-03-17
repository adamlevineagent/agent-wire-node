import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { CorpusHealthDashboard } from './CorpusHealthDashboard';
import { CurationQueue } from './CurationQueue';

type Tab = 'documents' | 'health' | 'curation';
type SortField = 'title' | 'status' | 'created_at';
type SortDir = 'asc' | 'desc';
type StatusFilter = 'all' | 'draft' | 'published' | 'retracted';

interface Document {
    id: string;
    title: string;
    status: 'draft' | 'published' | 'retracted';
    format?: string;
    word_count?: number;
    created_at?: string;
}

interface CorpusData {
    id: string;
    slug: string;
    title: string;
    description?: string;
    visibility: 'public' | 'unlisted' | 'private';
    material_class?: string;
}

interface CorpusDetailProps {
    slug: string;
}

export function CorpusDetail({ slug }: CorpusDetailProps) {
    const { operatorApiCall, pushView, popView } = useAppContext();
    const [corpus, setCorpus] = useState<CorpusData | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [activeTab, setActiveTab] = useState<Tab>('documents');

    // Separately fetched documents
    const [documents, setDocuments] = useState<Document[]>([]);
    const [docsLoading, setDocsLoading] = useState(true);

    // Documents tab state
    const [sortField, setSortField] = useState<SortField>('created_at');
    const [sortDir, setSortDir] = useState<SortDir>('desc');
    const [statusFilter, setStatusFilter] = useState<StatusFilter>('all');
    const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
    const [bulkLoading, setBulkLoading] = useState<string | null>(null);

    // Editable fields
    const [editingTitle, setEditingTitle] = useState(false);
    const [titleDraft, setTitleDraft] = useState('');
    const [editingDesc, setEditingDesc] = useState(false);
    const [descDraft, setDescDraft] = useState('');

    const loadCorpus = useCallback(() => {
        setLoading(true);
        setError(null);
        operatorApiCall('GET', `/api/v1/wire/corpora/${slug}`)
            .then((data: any) => {
                setCorpus(data);
                setTitleDraft(data?.title || '');
                setDescDraft(data?.description || '');
            })
            .catch((err: any) => setError(err?.message || 'Failed to load corpus'))
            .finally(() => setLoading(false));
    }, [slug, operatorApiCall]);

    const loadDocuments = useCallback(() => {
        setDocsLoading(true);
        operatorApiCall('GET', `/api/v1/wire/corpora/${slug}/documents?limit=100`)
            .then((data: any) => {
                setDocuments(data?.items || []);
            })
            .catch((err: any) => setError(err?.message || 'Failed to load documents'))
            .finally(() => setDocsLoading(false));
    }, [slug, operatorApiCall]);

    useEffect(() => { loadCorpus(); }, [loadCorpus]);
    useEffect(() => { loadDocuments(); }, [loadDocuments]);

    const pop = () => popView('agents');
    const push = (view: string, props: Record<string, unknown>) => pushView('agents', view, props);

    // Save metadata
    const saveTitle = async () => {
        if (!corpus || titleDraft === corpus.title) { setEditingTitle(false); return; }
        try {
            await operatorApiCall('PATCH', `/api/v1/wire/corpora/${slug}`, { title: titleDraft });
            setCorpus({ ...corpus, title: titleDraft });
        } catch (err: any) {
            setError(err?.message || 'Failed to update title');
        }
        setEditingTitle(false);
    };

    const saveDesc = async () => {
        if (!corpus || descDraft === (corpus.description || '')) { setEditingDesc(false); return; }
        try {
            await operatorApiCall('PATCH', `/api/v1/wire/corpora/${slug}`, { description: descDraft });
            setCorpus({ ...corpus, description: descDraft });
        } catch (err: any) {
            setError(err?.message || 'Failed to update description');
        }
        setEditingDesc(false);
    };

    const toggleVisibility = async () => {
        if (!corpus) return;
        const next = corpus.visibility === 'public' ? 'unlisted' : corpus.visibility === 'unlisted' ? 'private' : 'public';
        try {
            await operatorApiCall('PATCH', `/api/v1/wire/corpora/${slug}`, { visibility: next });
            setCorpus({ ...corpus, visibility: next });
        } catch (err: any) {
            setError(err?.message || 'Failed to update visibility');
        }
    };

    // Document actions
    const publishDoc = async (docId: string) => {
        try {
            await operatorApiCall('PATCH', `/api/v1/wire/documents/${docId}`, { status: 'published' });
            loadDocuments();
        } catch (err: any) {
            setError(err?.message || 'Failed to publish document');
        }
    };

    const retractDoc = async (docId: string) => {
        try {
            await operatorApiCall('PATCH', `/api/v1/wire/documents/${docId}`, { status: 'retracted' });
            loadDocuments();
        } catch (err: any) {
            setError(err?.message || 'Failed to retract document');
        }
    };

    // Bulk actions
    const bulkAction = async (action: string) => {
        setBulkLoading(action);
        try {
            await operatorApiCall('POST', `/api/v1/wire/corpora/${slug}/bulk`, {
                action,
                document_ids: selectedIds.size > 0 ? Array.from(selectedIds) : undefined,
            });
            setSelectedIds(new Set());
            loadDocuments();
        } catch (err: any) {
            setError(err?.message || `Bulk ${action} failed`);
        } finally {
            setBulkLoading(null);
        }
    };

    // Sort & filter
    const filtered = documents.filter(d => statusFilter === 'all' || d.status === statusFilter);
    const sorted = [...filtered].sort((a, b) => {
        let cmp = 0;
        if (sortField === 'title') cmp = (a.title || '').localeCompare(b.title || '');
        else if (sortField === 'status') cmp = (a.status || '').localeCompare(b.status || '');
        else cmp = (a.created_at || '').localeCompare(b.created_at || '');
        return sortDir === 'asc' ? cmp : -cmp;
    });

    const toggleSort = (field: SortField) => {
        if (sortField === field) setSortDir(d => d === 'asc' ? 'desc' : 'asc');
        else { setSortField(field); setSortDir('asc'); }
    };

    const toggleSelect = (id: string) => {
        const next = new Set(selectedIds);
        if (next.has(id)) next.delete(id); else next.add(id);
        setSelectedIds(next);
    };

    const toggleSelectAll = () => {
        if (selectedIds.size === sorted.length) setSelectedIds(new Set());
        else setSelectedIds(new Set(sorted.map(d => d.id)));
    };

    if (loading) {
        return (
            <div className="corpus-detail">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading corpus...</span>
                </div>
            </div>
        );
    }

    if (error && !corpus) {
        return (
            <div className="corpus-detail">
                <div className="corpus-detail-nav">
                    <button className="stewardship-btn stewardship-btn-ghost" onClick={pop}>Back</button>
                </div>
                <div className="corpora-error"><span>{error}</span></div>
            </div>
        );
    }

    return (
        <div className="corpus-detail">
            {/* Navigation */}
            <div className="corpus-detail-nav">
                <button className="stewardship-btn stewardship-btn-ghost" onClick={pop}>Back</button>
                <span className="corpus-detail-breadcrumb">Corpora / {corpus?.title || slug}</span>
            </div>

            {/* Header */}
            <div className="corpus-detail-header">
                <div className="corpus-detail-title-row">
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
                        <h2 className="corpus-detail-title" onClick={() => setEditingTitle(true)}>
                            {corpus?.title || slug}
                        </h2>
                    )}
                    <button
                        className={`visibility-badge visibility-${corpus?.visibility}`}
                        onClick={toggleVisibility}
                        title="Click to cycle visibility"
                    >
                        {corpus?.visibility}
                    </button>
                </div>
                {editingDesc ? (
                    <textarea
                        className="stewardship-textarea"
                        value={descDraft}
                        onChange={e => setDescDraft(e.target.value)}
                        onBlur={saveDesc}
                        rows={3}
                        autoFocus
                    />
                ) : (
                    <p
                        className="corpus-detail-desc"
                        onClick={() => setEditingDesc(true)}
                    >
                        {corpus?.description || 'Click to add description...'}
                    </p>
                )}
            </div>

            {error && <div className="curation-error-banner">{error}</div>}

            {/* Tabs */}
            <div className="corpus-tabs">
                <button
                    className={`corpus-tab ${activeTab === 'documents' ? 'corpus-tab-active' : ''}`}
                    onClick={() => setActiveTab('documents')}
                >
                    Documents
                </button>
                <button
                    className={`corpus-tab ${activeTab === 'health' ? 'corpus-tab-active' : ''}`}
                    onClick={() => setActiveTab('health')}
                >
                    Health
                </button>
                <button
                    className={`corpus-tab ${activeTab === 'curation' ? 'corpus-tab-active' : ''}`}
                    onClick={() => setActiveTab('curation')}
                >
                    Curation
                </button>
            </div>

            {/* Tab Content */}
            <div className="corpus-tab-content">
                {activeTab === 'documents' && (
                    <div className="document-list">
                        {/* Bulk Toolbar */}
                        <div className="bulk-toolbar">
                            <div className="bulk-toolbar-left">
                                <select
                                    className="stewardship-select"
                                    value={statusFilter}
                                    onChange={e => setStatusFilter(e.target.value as StatusFilter)}
                                >
                                    <option value="all">All statuses</option>
                                    <option value="draft">Draft</option>
                                    <option value="published">Published</option>
                                    <option value="retracted">Retracted</option>
                                </select>
                                <span className="bulk-toolbar-count">
                                    {sorted.length} document{sorted.length !== 1 ? 's' : ''}
                                    {selectedIds.size > 0 && ` (${selectedIds.size} selected)`}
                                </span>
                            </div>
                            <div className="bulk-toolbar-right">
                                <button
                                    className="stewardship-btn stewardship-btn-primary"
                                    disabled={bulkLoading === 'publish'}
                                    onClick={() => bulkAction('publish')}
                                >
                                    {bulkLoading === 'publish' ? 'Publishing...' : 'Publish All Drafts'}
                                </button>
                                <button
                                    className="stewardship-btn stewardship-btn-ghost"
                                    disabled={bulkLoading === 'retag'}
                                    onClick={() => bulkAction('retag')}
                                >
                                    {bulkLoading === 'retag' ? 'Retagging...' : 'Bulk Retag'}
                                </button>
                                <button
                                    className="stewardship-btn stewardship-btn-ghost"
                                    disabled={bulkLoading === 'reprice'}
                                    onClick={() => bulkAction('reprice')}
                                >
                                    {bulkLoading === 'reprice' ? 'Repricing...' : 'Bulk Reprice'}
                                </button>
                            </div>
                        </div>

                        {/* Table Header */}
                        <div className="document-row document-row-header">
                            <span className="doc-cell doc-cell-check">
                                <input
                                    type="checkbox"
                                    checked={selectedIds.size === sorted.length && sorted.length > 0}
                                    onChange={toggleSelectAll}
                                />
                            </span>
                            <span className="doc-cell doc-cell-title sortable" onClick={() => toggleSort('title')}>
                                Title {sortField === 'title' ? (sortDir === 'asc' ? '\u25B2' : '\u25BC') : ''}
                            </span>
                            <span className="doc-cell doc-cell-status sortable" onClick={() => toggleSort('status')}>
                                Status {sortField === 'status' ? (sortDir === 'asc' ? '\u25B2' : '\u25BC') : ''}
                            </span>
                            <span className="doc-cell doc-cell-format">Format</span>
                            <span className="doc-cell doc-cell-words">Words</span>
                            <span className="doc-cell doc-cell-date sortable" onClick={() => toggleSort('created_at')}>
                                Created {sortField === 'created_at' ? (sortDir === 'asc' ? '\u25B2' : '\u25BC') : ''}
                            </span>
                            <span className="doc-cell doc-cell-actions">Actions</span>
                        </div>

                        {/* Document Rows */}
                        {sorted.length === 0 ? (
                            <div className="document-empty">
                                <p>No documents {statusFilter !== 'all' ? `with status "${statusFilter}"` : 'in this corpus'}.</p>
                            </div>
                        ) : (
                            sorted.map((doc) => (
                                <div
                                    key={doc.id}
                                    className={`document-row ${selectedIds.has(doc.id) ? 'document-row-selected' : ''}`}
                                >
                                    <span className="doc-cell doc-cell-check">
                                        <input
                                            type="checkbox"
                                            checked={selectedIds.has(doc.id)}
                                            onChange={() => toggleSelect(doc.id)}
                                        />
                                    </span>
                                    <span
                                        className="doc-cell doc-cell-title doc-cell-clickable"
                                        onClick={() => push('document-detail', { documentId: doc.id })}
                                    >
                                        {doc.title}
                                    </span>
                                    <span className="doc-cell doc-cell-status">
                                        <span className={`status-badge status-badge-${doc.status}`}>
                                            {doc.status}
                                        </span>
                                    </span>
                                    <span className="doc-cell doc-cell-format">{doc.format || '--'}</span>
                                    <span className="doc-cell doc-cell-words">
                                        {doc.word_count != null ? doc.word_count.toLocaleString() : '--'}
                                    </span>
                                    <span className="doc-cell doc-cell-date">
                                        {doc.created_at ? new Date(doc.created_at).toLocaleDateString() : '--'}
                                    </span>
                                    <span className="doc-cell doc-cell-actions">
                                        {doc.status === 'draft' && (
                                            <button
                                                className="stewardship-btn stewardship-btn-sm stewardship-btn-primary"
                                                onClick={() => publishDoc(doc.id)}
                                            >
                                                Publish
                                            </button>
                                        )}
                                        {doc.status === 'published' && (
                                            <button
                                                className="stewardship-btn stewardship-btn-sm stewardship-btn-warn"
                                                onClick={() => retractDoc(doc.id)}
                                            >
                                                Retract
                                            </button>
                                        )}
                                        <button
                                            className="stewardship-btn stewardship-btn-sm stewardship-btn-ghost"
                                            onClick={() => push('document-detail', { documentId: doc.id })}
                                        >
                                            Edit
                                        </button>
                                    </span>
                                </div>
                            ))
                        )}
                    </div>
                )}

                {activeTab === 'health' && <CorpusHealthDashboard slug={slug} />}
                {activeTab === 'curation' && <CurationQueue slug={slug} />}
            </div>
        </div>
    );
}
