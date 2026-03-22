import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AddWorkspace } from './AddWorkspace';
import { BuildProgress } from './BuildProgress';

interface SlugInfo {
    slug: string;
    content_type: string; // "code" | "document" | "conversation"
    source_path: string;
    node_count: number;
    max_depth: number;
    last_built_at: string | null;
    created_at: string;
}

interface BuildStatus {
    slug: string;
    status: string;
    progress: { done: number; total: number };
    elapsed_seconds: number;
}

type View = 'list' | 'add' | 'building';

export function PyramidDashboard() {
    const [slugs, setSlugs] = useState<SlugInfo[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [view, setView] = useState<View>('list');
    const [buildingSlug, setBuildingSlug] = useState<string | null>(null);
    const [deletingSlug, setDeletingSlug] = useState<string | null>(null);
    const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

    const fetchSlugs = useCallback(async () => {
        try {
            const data = await invoke<SlugInfo[]>('pyramid_list_slugs');
            setSlugs(data);
            setError(null);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        fetchSlugs();
    }, [fetchSlugs]);

    const handleRebuild = useCallback(async (slug: string) => {
        try {
            await invoke('pyramid_build', { slug });
            setBuildingSlug(slug);
            setView('building');
        } catch (err) {
            setError(String(err));
        }
    }, []);

    const handleDelete = useCallback(async (slug: string) => {
        setDeletingSlug(slug);
        try {
            await invoke('pyramid_delete_slug', { slug });
            setConfirmDelete(null);
            await fetchSlugs();
        } catch (err) {
            setError(String(err));
        } finally {
            setDeletingSlug(null);
        }
    }, [fetchSlugs]);

    const handleOpenVibesmithy = useCallback((slug: string) => {
        window.open(`http://localhost:3333/space/${slug}`, '_blank');
    }, []);

    const handleAddComplete = useCallback(() => {
        setView('list');
        fetchSlugs();
    }, [fetchSlugs]);

    const handleBuildComplete = useCallback(() => {
        fetchSlugs();
    }, [fetchSlugs]);

    const formatDate = (dateStr: string | null) => {
        if (!dateStr) return 'Never';
        const d = new Date(dateStr);
        return d.toLocaleDateString() + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    };

    const contentTypeLabel = (ct: string) => {
        switch (ct) {
            case 'code': return 'Code';
            case 'document': return 'Documents';
            case 'conversation': return 'Conversation';
            default: return ct;
        }
    };

    const contentTypeBadgeClass = (ct: string) => {
        switch (ct) {
            case 'code': return 'badge-code';
            case 'document': return 'badge-document';
            case 'conversation': return 'badge-conversation';
            default: return '';
        }
    };

    if (view === 'add') {
        return <AddWorkspace onComplete={handleAddComplete} onCancel={() => setView('list')} />;
    }

    if (view === 'building' && buildingSlug) {
        return (
            <BuildProgress
                slug={buildingSlug}
                onComplete={handleBuildComplete}
                onClose={() => {
                    setBuildingSlug(null);
                    setView('list');
                }}
            />
        );
    }

    return (
        <div className="pyramid-dashboard">
            <div className="pyramid-dashboard-header">
                <h2>Workspaces</h2>
                <button className="btn btn-primary" onClick={() => setView('add')}>
                    + Add Workspace
                </button>
            </div>

            {error && (
                <div className="pyramid-error">
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            )}

            {loading && (
                <div className="pyramid-loading">Loading workspaces...</div>
            )}

            {!loading && slugs.length === 0 && (
                <div className="pyramid-empty">
                    <div className="pyramid-empty-icon">&#x1F3D7;</div>
                    <h3>No workspaces yet</h3>
                    <p>Add a workspace to build your first knowledge pyramid.</p>
                    <button className="btn btn-primary" onClick={() => setView('add')}>
                        Add Your First Workspace
                    </button>
                </div>
            )}

            {!loading && slugs.length > 0 && (
                <div className="pyramid-cards">
                    {slugs.map((s) => (
                        <div key={s.slug} className="pyramid-card">
                            <div className="pyramid-card-header">
                                <h3 className="pyramid-card-slug">{s.slug}</h3>
                                <span className={`pyramid-card-badge ${contentTypeBadgeClass(s.content_type)}`}>
                                    {contentTypeLabel(s.content_type)}
                                </span>
                            </div>

                            <div className="pyramid-card-path" title={s.source_path}>
                                {s.source_path.length > 50
                                    ? '...' + s.source_path.slice(-47)
                                    : s.source_path}
                            </div>

                            <div className="pyramid-card-stats">
                                <div className="pyramid-stat">
                                    <span className="pyramid-stat-value">{s.node_count}</span>
                                    <span className="pyramid-stat-label">nodes</span>
                                </div>
                                <div className="pyramid-stat">
                                    <span className="pyramid-stat-value">{s.max_depth}</span>
                                    <span className="pyramid-stat-label">depth</span>
                                </div>
                                <div className="pyramid-stat">
                                    <span className="pyramid-stat-value">{formatDate(s.last_built_at)}</span>
                                    <span className="pyramid-stat-label">last built</span>
                                </div>
                            </div>

                            <div className="pyramid-card-status">
                                {s.node_count > 0 ? (
                                    <span className="pyramid-status-indicator idle">Ready</span>
                                ) : (
                                    <span className="pyramid-status-indicator needs-build">Needs Build</span>
                                )}
                            </div>

                            <div className="pyramid-card-actions">
                                <button
                                    className="btn btn-small btn-primary"
                                    onClick={() => handleOpenVibesmithy(s.slug)}
                                    disabled={s.node_count === 0}
                                >
                                    Open in Vibesmithy
                                </button>
                                <button
                                    className="btn btn-small btn-secondary"
                                    onClick={() => handleRebuild(s.slug)}
                                >
                                    Rebuild
                                </button>
                                {confirmDelete === s.slug ? (
                                    <div className="delete-confirm">
                                        <span>Delete "{s.slug}"?</span>
                                        <button
                                            className="btn btn-small btn-danger"
                                            onClick={() => handleDelete(s.slug)}
                                            disabled={deletingSlug === s.slug}
                                        >
                                            {deletingSlug === s.slug ? 'Deleting...' : 'Confirm'}
                                        </button>
                                        <button
                                            className="btn btn-small btn-ghost"
                                            onClick={() => setConfirmDelete(null)}
                                        >
                                            Cancel
                                        </button>
                                    </div>
                                ) : (
                                    <button
                                        className="btn btn-small btn-ghost btn-danger-text"
                                        onClick={() => setConfirmDelete(s.slug)}
                                    >
                                        Delete
                                    </button>
                                )}
                            </div>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
}
