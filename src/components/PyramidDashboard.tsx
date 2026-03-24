import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { AddWorkspace } from './AddWorkspace';
import { BuildProgress } from './BuildProgress';
import { VineBuildProgress } from './VineBuildProgress';
import { DADBEARPanel } from './DADBEARPanel';
import { FAQDirectory } from './FAQDirectory';
import { VineViewer } from './VineViewer';

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

interface DadbearStatus {
    frozen: boolean;
    breaker_tripped: boolean;
}

type View = 'list' | 'add' | 'building' | 'dadbear' | 'faq' | 'vine';

export function PyramidDashboard() {
    const [slugs, setSlugs] = useState<SlugInfo[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [view, setView] = useState<View>('list');
    const [buildingSlug, setBuildingSlug] = useState<string | null>(null);
    const [deletingSlug, setDeletingSlug] = useState<string | null>(null);
    const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
    const [selectedSlug, setSelectedSlug] = useState<string | null>(null);
    const [dadbearStatuses, setDadbearStatuses] = useState<Record<string, DadbearStatus>>({});
    const [vineAddFolders, setVineAddFolders] = useState<string | null>(null); // slug being edited
    const [vineNewPaths, setVineNewPaths] = useState<string[]>([]);
    const [vinePastePath, setVinePastePath] = useState('');

    const fetchDadbearStatuses = useCallback(async (slugList: SlugInfo[]) => {
        const statuses: Record<string, DadbearStatus> = {};
        for (const s of slugList) {
            try {
                const config = await invoke<{ frozen: boolean; breaker_tripped: boolean }>(
                    'pyramid_auto_update_config_get', { slug: s.slug }
                );
                statuses[s.slug] = { frozen: config.frozen, breaker_tripped: config.breaker_tripped };
            } catch {
                // No auto-update config for this slug — skip
            }
        }
        setDadbearStatuses(statuses);
    }, []);

    const fetchSlugs = useCallback(async () => {
        try {
            const data = await invoke<SlugInfo[]>('pyramid_list_slugs');
            setSlugs(data);
            setError(null);
            fetchDadbearStatuses(data);
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

    const handleVineAddFoldersOpen = useCallback((s: SlugInfo) => {
        // Parse existing directories from source_path (semicolon-joined)
        const existingDirs = s.source_path ? s.source_path.split(';').filter(Boolean) : [];
        setVineNewPaths(existingDirs);
        setVineAddFolders(s.slug);
        setVinePastePath('');
    }, []);

    const handleVinePickNewDir = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Add JSONL Directory to Vine',
                multiple: true,
            });
            if (selected) {
                const newPaths = Array.isArray(selected) ? selected : [selected];
                setVineNewPaths(prev => {
                    const combined = [...prev];
                    for (const p of newPaths) {
                        if (!combined.includes(p)) combined.push(p);
                    }
                    return combined;
                });
            }
        } catch (err) {
            setError(String(err));
        }
    }, []);

    const handleVineAddPastePath = useCallback(() => {
        const val = vinePastePath.trim();
        if (!val) return;
        setVineNewPaths(prev => {
            if (prev.includes(val)) return prev;
            return [...prev, val];
        });
        setVinePastePath('');
    }, [vinePastePath]);

    const handleVineRebuildWithFolders = useCallback(async () => {
        if (!vineAddFolders || vineNewPaths.length === 0) return;
        setError(null);
        try {
            // Update the slug's source_path
            const newSourcePath = vineNewPaths.join(';');
            await invoke('pyramid_create_slug', {
                slug: vineAddFolders,
                contentType: 'vine',
                sourcePath: newSourcePath,
            }).catch(() => {
                // Slug may already exist — that's fine, we just need to rebuild
            });

            // Trigger vine rebuild via Tauri command
            await invoke('pyramid_vine_build', {
                vineSlug: vineAddFolders,
                jsonlDirs: vineNewPaths,
            });

            setBuildingSlug(vineAddFolders);
            setVineAddFolders(null);
            setView('building');
        } catch (err) {
            setError(String(err));
        }
    }, [vineAddFolders, vineNewPaths]);

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
            case 'vine': return 'Vine';
            default: return ct;
        }
    };

    const contentTypeBadgeClass = (ct: string) => {
        switch (ct) {
            case 'code': return 'badge-code';
            case 'document': return 'badge-document';
            case 'conversation': return 'badge-conversation';
            case 'vine': return 'badge-vine';
            default: return '';
        }
    };

    // Agent Onboarding card state
    const [onboardingOpen, setOnboardingOpen] = useState(false);
    const [onboardingCopied, setOnboardingCopied] = useState(false);
    const onboardingCopyTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

    const generateOnboardingText = () => {
        const slugList = slugs.map(s => `- ${s.slug} (${s.content_type}, ${s.node_count} nodes)`).join('\n');
        return `# Knowledge Pyramid Access

You have access to a Knowledge Pyramid system running on localhost:8765. Use the pyramid CLI to explore and contribute.

## Available Pyramids
${slugList}

## Query Commands
\`\`\`bash
# Search for a topic
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" search <SLUG> "your query"

# Get the apex (top-level summary)
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" apex <SLUG>

# Drill into a specific node
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" drill <SLUG> <NODE_ID>

# Match a question to FAQ
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" faq <SLUG> "your question"

# Check system health
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" health
\`\`\`

## Contributing Back
When you learn something generalizable, annotate it:
\`\`\`bash
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" annotate <SLUG> <NODE_ID> "Your finding here. Include specific details.\\n\\nGeneralized understanding: The mechanism-level knowledge that would help future agents." --question "What question does this answer?" --author "your-agent-name" --type observation
\`\`\`

Always include the "Generalized understanding:" section — this triggers FAQ generalization so future agents benefit from your work.

## Annotation Types
- observation: General finding or documentation
- correction: Something is wrong or outdated
- question: Unanswered question for future investigation
- friction: Something that was harder than it should be
- idea: Improvement suggestion`;
    };

    const handleCopyOnboarding = () => {
        navigator.clipboard.writeText(generateOnboardingText()).then(() => {
            setOnboardingCopied(true);
            if (onboardingCopyTimeout.current) clearTimeout(onboardingCopyTimeout.current);
            onboardingCopyTimeout.current = setTimeout(() => setOnboardingCopied(false), 2000);
        });
    };

    if (view === 'add') {
        return <AddWorkspace onComplete={handleAddComplete} onCancel={() => setView('list')} />;
    }

    if (view === 'dadbear' && selectedSlug) {
        return (
            <DADBEARPanel
                slug={selectedSlug}
                onBack={() => {
                    setSelectedSlug(null);
                    setView('list');
                    fetchSlugs();
                }}
            />
        );
    }

    if (view === 'faq' && selectedSlug) {
        return (
            <FAQDirectory
                slug={selectedSlug}
                onBack={() => {
                    setSelectedSlug(null);
                    setView('list');
                }}
            />
        );
    }

    if (view === 'vine' && selectedSlug) {
        const vineInfo = slugs.find(s => s.slug === selectedSlug);
        return (
            <VineViewer
                slug={selectedSlug}
                nodeCount={vineInfo?.node_count ?? 0}
                lastBuiltAt={vineInfo?.last_built_at ?? null}
                onBack={() => {
                    setSelectedSlug(null);
                    setView('list');
                    fetchSlugs();
                }}
                onOpenBunch={(bunchSlug) => {
                    window.open(`http://localhost:3333/space/${bunchSlug}`, '_blank');
                }}
            />
        );
    }

    if (view === 'building' && buildingSlug) {
        const buildSlugInfo = slugs.find(s => s.slug === buildingSlug);
        const isVineBuild = buildSlugInfo?.content_type === 'vine';
        if (isVineBuild) {
            return (
                <VineBuildProgress
                    slug={buildingSlug}
                    onComplete={handleBuildComplete}
                    onClose={() => {
                        setBuildingSlug(null);
                        setView('list');
                        fetchSlugs();
                    }}
                />
            );
        }
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

            {!loading && slugs.length > 0 && (
                <div className="agent-onboarding-card">
                    <div className="agent-onboarding-header" onClick={() => setOnboardingOpen(!onboardingOpen)}>
                        <h3>Agent Onboarding Instructions</h3>
                        <div className="agent-onboarding-header-actions">
                            <button
                                className={`copy-btn${onboardingCopied ? ' copied' : ''}`}
                                onClick={(e) => { e.stopPropagation(); handleCopyOnboarding(); }}
                            >
                                {onboardingCopied ? 'Copied!' : 'Copy to Clipboard'}
                            </button>
                            <span className="agent-onboarding-toggle">{onboardingOpen ? '\u25B2' : '\u25BC'}</span>
                        </div>
                    </div>
                    {onboardingOpen && (
                        <div className="agent-onboarding-content">
                            <pre>{generateOnboardingText()}</pre>
                        </div>
                    )}
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
                                {dadbearStatuses[s.slug]?.frozen ? (
                                    <span className="pyramid-status-indicator frozen">Frozen — DADBEAR is hibernating</span>
                                ) : dadbearStatuses[s.slug]?.breaker_tripped ? (
                                    <span className="pyramid-status-indicator breaker-tripped">DADBEAR needs your attention</span>
                                ) : s.node_count > 0 ? (
                                    <span className="pyramid-status-indicator idle">Ready</span>
                                ) : (
                                    <span className="pyramid-status-indicator needs-build">Needs Build</span>
                                )}
                            </div>

                            <div className="pyramid-card-actions">
                                {s.content_type === 'vine' ? (
                                    <>
                                        <button
                                            className="btn btn-small btn-primary"
                                            onClick={() => { setSelectedSlug(s.slug); setView('vine'); }}
                                            disabled={s.node_count === 0}
                                        >
                                            Open Vine
                                        </button>
                                        <button
                                            className="btn btn-small btn-secondary vine-add-folders-btn"
                                            onClick={() => handleVineAddFoldersOpen(s)}
                                            title="Add folders to vine"
                                        >
                                            &#x1F4C1;+ Add Folders
                                        </button>
                                    </>
                                ) : (
                                    <button
                                        className="btn btn-small btn-primary"
                                        onClick={() => handleOpenVibesmithy(s.slug)}
                                        disabled={s.node_count === 0}
                                    >
                                        Open in Vibesmithy
                                    </button>
                                )}
                                <button
                                    className={`pyramid-card-dadbear-btn${dadbearStatuses[s.slug]?.frozen ? ' dadbear-attention-frozen' : ''}${dadbearStatuses[s.slug]?.breaker_tripped ? ' dadbear-attention-tripped' : ''}`}
                                    onClick={() => { setSelectedSlug(s.slug); setView('dadbear'); }}
                                    title="DADBEAR Auto-Update Panel"
                                    disabled={s.node_count === 0}
                                >
                                    &#x1F43B;
                                </button>
                                <button
                                    className="pyramid-card-faq-btn"
                                    onClick={() => { setSelectedSlug(s.slug); setView('faq'); }}
                                    title="FAQ Directory"
                                    disabled={s.node_count === 0}
                                >
                                    &#x1F4D6;
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

            {/* Vine Add Folders Dialog */}
            {vineAddFolders && (
                <div className="vine-add-folders-overlay">
                    <div className="vine-add-folders-dialog">
                        <h3>Add Folders to Vine: {vineAddFolders}</h3>
                        <p className="step-description">
                            Add directories containing JSONL conversation files.
                        </p>

                        <div className="selected-paths" style={{ marginBottom: '12px', maxHeight: '200px', overflowY: 'auto' }}>
                            {vineNewPaths.map((p, i) => (
                                <div key={p} className="selected-path-row">
                                    <span className="selected-path-text">{p}</span>
                                    <button
                                        className="btn btn-ghost btn-sm"
                                        onClick={() => setVineNewPaths(prev => prev.filter((_, idx) => idx !== i))}
                                        title="Remove"
                                    >
                                        &times;
                                    </button>
                                </div>
                            ))}
                        </div>

                        <div style={{ display: 'flex', gap: '8px', marginBottom: '8px' }}>
                            <input
                                type="text"
                                placeholder="Paste a path..."
                                className="input"
                                style={{ flex: 1 }}
                                value={vinePastePath}
                                onChange={(e) => setVinePastePath(e.target.value)}
                                onKeyDown={(e) => {
                                    if (e.key === 'Enter') handleVineAddPastePath();
                                }}
                            />
                            <button
                                className="btn btn-secondary"
                                onClick={handleVineAddPastePath}
                                disabled={!vinePastePath.trim()}
                            >
                                +
                            </button>
                            <button className="btn btn-primary" onClick={handleVinePickNewDir}>
                                Browse...
                            </button>
                        </div>

                        <div className="vine-hint">
                            Use <kbd>Cmd+Shift+.</kbd> to show hidden folders in the file picker.
                        </div>

                        <div className="step-nav" style={{ marginTop: '16px' }}>
                            <button className="btn btn-ghost" onClick={() => { setVineAddFolders(null); setVineNewPaths([]); }}>
                                Cancel
                            </button>
                            <button
                                className="btn btn-primary"
                                onClick={handleVineRebuildWithFolders}
                                disabled={vineNewPaths.length === 0}
                            >
                                Rebuild Vine
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
}
