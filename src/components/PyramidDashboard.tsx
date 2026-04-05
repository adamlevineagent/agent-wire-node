import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { AddWorkspace } from './AddWorkspace';
import { AskQuestion } from './AskQuestion';
import { BuildProgress } from './BuildProgress';
import { VineBuildProgress } from './VineBuildProgress';
import { DADBEARPanel } from './DADBEARPanel';
import { FAQDirectory } from './FAQDirectory';
import { VineViewer } from './VineViewer';
import PyramidToolbar from './PyramidToolbar';
import { PyramidRow } from './PyramidRow';
import { PyramidDetailDrawer } from './PyramidDetailDrawer';
import {
    SlugInfo,
    PyramidPublicationInfo,
    EnrichedSlug,
    PublishResult,
    ContentType,
    AccessTier,
    AbsorptionMode,
    SortKey,
    CONTENT_TYPE_CONFIG,
    enrichSlug,
    getPublicationState,
    sortComparator,
} from './pyramid-types';

const VIBESMITHY_BASE_URL = 'http://localhost:3333';

// ─── Types ──────────────────────────────────────────────────────────────────

interface DadbearStatus {
    frozen: boolean;
    breaker_tripped: boolean;
}

type View = 'list' | 'add' | 'building' | 'dadbear' | 'faq' | 'vine' | 'asking';

// ─── Component ──────────────────────────────────────────────────────────────

export function PyramidDashboard() {
    // ─── Data ───────────────────────────────────────────────────────────────
    const [enrichedSlugs, setEnrichedSlugs] = useState<EnrichedSlug[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // ─── Filter / Sort ──────────────────────────────────────────────────────
    const [searchQuery, setSearchQuery] = useState('');
    const [activeTypes, setActiveTypes] = useState<Set<string>>(new Set());
    const [activeStatuses, setActiveStatuses] = useState<Set<string>>(new Set());
    const [sortBy, setSortBy] = useState<SortKey>('node_count');

    // ─── Selection + Drawer ─────────────────────────────────────────────────
    const [selectedSlug, setSelectedSlug] = useState<string | null>(null);

    // ─── Publishing ─────────────────────────────────────────────────────────
    const [publishingSlug, setPublishingSlug] = useState<string | null>(null);
    const [lastPublishResult, setLastPublishResult] = useState<Record<string, { success: boolean; message: string; wireUuid?: string }>>({});

    // ─── Sub-views ──────────────────────────────────────────────────────────
    const [view, setView] = useState<View>('list');
    const [buildingSlug, setBuildingSlug] = useState<string | null>(null);
    const [dadbearStatuses, setDadbearStatuses] = useState<Record<string, DadbearStatus>>({});
    const [askingSlug, setAskingSlug] = useState<string | null>(null);

    // TODO: Vine add-folders feature removed (dead code). Re-add when vine drawer gets a trigger button.

    // Collapsible sections
    const [collapsedSections, setCollapsedSections] = useState<Set<string>>(new Set(['empty']));

    // Agent onboarding
    const [onboardingOpen, setOnboardingOpen] = useState(false);
    const [onboardingCopied, setOnboardingCopied] = useState(false);
    const onboardingCopyTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
    useEffect(() => () => { if (onboardingCopyTimeout.current) clearTimeout(onboardingCopyTimeout.current); }, []);

    // ─── Data Fetching ──────────────────────────────────────────────────────

    const fetchData = useCallback(async () => {
        try {
            const [slugs, pubStatus] = await Promise.all([
                invoke<SlugInfo[]>('pyramid_list_slugs'),
                invoke<PyramidPublicationInfo[]>('pyramid_get_publication_status'),
            ]);

            const pubMap = new Map<string, PyramidPublicationInfo>();
            for (const p of pubStatus) {
                pubMap.set(p.slug, p);
            }

            const enriched = slugs.map(s => enrichSlug(s, pubMap.get(s.slug)));
            setEnrichedSlugs(enriched);
            setError(null);
            setLoading(false);
        } catch (err) {
            setError(String(err));
            setLoading(false);
        }
    }, []);

    const fetchDadbearStatuses = useCallback(async () => {
        try {
            const slugs = await invoke<SlugInfo[]>('pyramid_list_slugs');
            const results = await Promise.allSettled(
                slugs.map(s =>
                    invoke<{ frozen: boolean; breaker_tripped: boolean }>(
                        'pyramid_auto_update_config_get', { slug: s.slug }
                    ).then(config => ({ slug: s.slug, status: { frozen: config.frozen, breaker_tripped: config.breaker_tripped } }))
                )
            );
            const statuses: Record<string, DadbearStatus> = {};
            for (const r of results) {
                if (r.status === 'fulfilled') {
                    statuses[r.value.slug] = r.value.status;
                }
            }
            setDadbearStatuses(statuses);
        } catch (err) {
            // DADBEAR status is non-critical; log but don't surface
            console.error('Failed to fetch DADBEAR statuses:', err);
        }
    }, []);

    // Initial load + 15s lightweight refresh (slugs + pub status only)
    useEffect(() => {
        fetchData().then(() => fetchDadbearStatuses());
        const interval = setInterval(fetchData, 15000);
        return () => clearInterval(interval);
    }, [fetchData, fetchDadbearStatuses]);

    // Listen for build-complete events — refresh both data and DADBEAR statuses
    useEffect(() => {
        const unlisten = listen('pyramid-build-complete', () => {
            fetchData();
            fetchDadbearStatuses();
        });
        return () => { unlisten.then(fn => fn()); };
    }, [fetchData, fetchDadbearStatuses]);

    // ─── Filtering ──────────────────────────────────────────────────────────

    const filteredSlugs = useMemo(() => {
        return enrichedSlugs
            .filter(s => !s.archived_at)
            .filter(s => !searchQuery ||
                s.slug.toLowerCase().includes(searchQuery.toLowerCase()) ||
                s.source_path.toLowerCase().includes(searchQuery.toLowerCase()))
            .filter(s => activeTypes.size === 0 || activeTypes.has(s.content_type))
            .filter(s => {
                if (activeStatuses.size === 0) return true;
                const state = getPublicationState(s, publishingSlug);
                if (activeStatuses.has('built') && s.node_count > 0) return true;
                if (activeStatuses.has('empty') && s.node_count === 0) return true;
                if (activeStatuses.has('published') && state === 'published') return true;
                if (activeStatuses.has('stale') && state === 'stale') return true;
                if (activeStatuses.has('pinned') && s.pinned) return true;
                return false;
            })
            .sort(sortComparator(sortBy));
    }, [enrichedSlugs, searchQuery, activeTypes, activeStatuses, sortBy, publishingSlug]);

    // ─── Grouping ───────────────────────────────────────────────────────────

    const { groups, maxNodeCount, counts } = useMemo(() => {
        const nonEmpty = filteredSlugs.filter(s => s.node_count > 0);
        const empty = filteredSlugs.filter(s => s.node_count === 0);

        // Group non-empty by content_type
        const typeGroups = new Map<ContentType, EnrichedSlug[]>();
        for (const s of nonEmpty) {
            const list = typeGroups.get(s.content_type) ?? [];
            list.push(s);
            typeGroups.set(s.content_type, list);
        }

        // For each type group, sort so question pyramids with referenced_slugs
        // matching a slug in the list appear after their parent
        const sortedGroups: Array<{ key: string; label: string; color: string; slugs: EnrichedSlug[]; nestedSlugs: Set<string> }> = [];

        for (const [type, slugsInGroup] of typeGroups) {
            const config = CONTENT_TYPE_CONFIG[type];
            const parentSlugs: EnrichedSlug[] = [];
            const childrenByParent = new Map<string, EnrichedSlug[]>();

            if (type === 'question') {
                // Nesting is single-level within the question type group only.
                // Cross-type references (questions referencing code/document pyramids) are shown
                // in the drawer's "Built on:" section but don't affect list nesting.
                const questionSlugsInGroup = new Set(slugsInGroup.map(s => s.slug));
                for (const s of slugsInGroup) {
                    const parentRef = s.referenced_slugs.find(ref => questionSlugsInGroup.has(ref));
                    if (parentRef) {
                        const children = childrenByParent.get(parentRef) ?? [];
                        children.push(s);
                        childrenByParent.set(parentRef, children);
                    } else {
                        parentSlugs.push(s);
                    }
                }
            } else {
                parentSlugs.push(...slugsInGroup);
            }

            // Build final order: parent, then children
            const ordered: EnrichedSlug[] = [];
            const nested = new Set<string>();
            for (const p of parentSlugs) {
                ordered.push(p);
                const children = childrenByParent.get(p.slug);
                if (children) {
                    for (const c of children) {
                        ordered.push(c);
                        nested.add(c.slug);
                    }
                }
            }

            sortedGroups.push({
                key: type,
                label: `${config.label} (${slugsInGroup.length})`,
                color: config.color,
                slugs: ordered,
                nestedSlugs: nested,
            });
        }

        // Empty section
        if (empty.length > 0) {
            sortedGroups.push({
                key: 'empty',
                label: `Empty (${empty.length})`,
                color: 'rgba(255,255,255,0.2)',
                slugs: empty,
                nestedSlugs: new Set(),
            });
        }

        // Compute max node count for scale bars
        let max = 0;
        for (const s of filteredSlugs) {
            if (s.node_count > max) max = s.node_count;
        }

        // Compute counts for toolbar chips
        const allActive = enrichedSlugs.filter(s => !s.archived_at);
        const chipCounts = {
            total: allActive.length,
            built: allActive.filter(s => s.node_count > 0).length,
            empty: allActive.filter(s => s.node_count === 0).length,
            published: allActive.filter(s => getPublicationState(s, publishingSlug) === 'published').length,
            stale: allActive.filter(s => getPublicationState(s, publishingSlug) === 'stale').length,
            pinned: allActive.filter(s => s.pinned).length,
        };

        return { groups: sortedGroups, maxNodeCount: max, counts: chipCounts };
    }, [filteredSlugs, enrichedSlugs, publishingSlug]);

    // ─── Toggle helpers ─────────────────────────────────────────────────────

    const toggleType = useCallback((type: string) => {
        setActiveTypes(prev => {
            const next = new Set(prev);
            if (next.has(type)) next.delete(type);
            else next.add(type);
            return next;
        });
    }, []);

    const toggleStatus = useCallback((status: string) => {
        setActiveStatuses(prev => {
            const next = new Set(prev);
            if (next.has(status)) next.delete(status);
            else next.add(status);
            return next;
        });
    }, []);

    const toggleSection = useCallback((key: string) => {
        setCollapsedSections(prev => {
            const next = new Set(prev);
            if (next.has(key)) next.delete(key);
            else next.add(key);
            return next;
        });
    }, []);

    // ─── Drawer Callbacks ───────────────────────────────────────────────────

    const handlePublish = useCallback(async (slug: string): Promise<PublishResult> => {
        setPublishingSlug(slug);
        setLastPublishResult(prev => {
            const next = { ...prev };
            delete next[slug];
            return next;
        });
        try {
            const result = await invoke<PublishResult>('pyramid_publish', { slug });
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: true, message: 'Published to Wire', wireUuid: result.apex_wire_uuid ?? undefined },
            }));
            await fetchData();
            return result;
        } catch (err) {
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: false, message: String(err) },
            }));
            throw err;
        } finally {
            setPublishingSlug(null);
        }
    }, [fetchData]);

    const handleSetAccessTier = useCallback(async (slug: string, tier: AccessTier, price?: number, circles?: string[]) => {
        await invoke('pyramid_set_access_tier', { slug, tier, price: price ?? null, circles: circles ?? null });
    }, []);

    const handleSetAbsorption = useCallback(async (slug: string, mode: AbsorptionMode, chainId?: string, rateLimit?: number, dailyCap?: number) => {
        await invoke('pyramid_set_absorption_mode', {
            slug,
            mode,
            chain_id: chainId ?? null,
            rate_limit: rateLimit ?? null,
            daily_cap: dailyCap ?? null,
        });
    }, []);

    const handleDelete = useCallback(async (slug: string) => {
        await invoke('pyramid_archive_slug', { slug });
        setSelectedSlug(null);
        await fetchData();
    }, [fetchData]);

    const handleRebuild = useCallback(async (slug: string) => {
        try {
            await invoke('pyramid_build', { slug });
            setBuildingSlug(slug);
            setView('building');
        } catch (err) {
            setError(String(err));
        }
    }, []);

    // ─── Sub-view helpers (carried from original) ───────────────────────────

    const handleOpenVibesmithy = useCallback((slug: string) => {
        window.open(`${VIBESMITHY_BASE_URL}/space/${slug}`, '_blank');
    }, []);

    const handleAddComplete = useCallback(() => {
        setView('list');
        fetchData();
    }, [fetchData]);

    const handleBuildComplete = useCallback(() => {
        fetchData();
    }, [fetchData]);

    // ─── Agent Onboarding ───────────────────────────────────────────────────

    const generateOnboardingText = () => {
        const slugList = enrichedSlugs.map(s => `- ${s.slug} (${s.content_type}, ${s.node_count} nodes)`).join('\n');
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

    // ─── Drawer close (stable ref for escape handler) ────────────────────────

    const handleDrawerClose = useCallback(() => setSelectedSlug(null), []);

    // ─── Selected slug data for drawer ──────────────────────────────────────

    const selectedSlugData = useMemo(() => {
        if (!selectedSlug) return null;
        return enrichedSlugs.find(s => s.slug === selectedSlug) ?? null;
    }, [selectedSlug, enrichedSlugs]);

    // ─── Sub-view rendering ─────────────────────────────────────────────────

    if (view === 'add') {
        return <AddWorkspace onComplete={handleAddComplete} onCancel={() => setView('list')} />;
    }

    if (view === 'dadbear' && selectedSlug) {
        return (
            <DADBEARPanel
                slug={selectedSlug}
                contentType={enrichedSlugs.find(s => s.slug === selectedSlug)?.content_type}
                referencingSlugs={enrichedSlugs.find(s => s.slug === selectedSlug)?.referencing_slugs ?? []}
                onBack={() => {
                    setSelectedSlug(null);
                    setView('list');
                    fetchData();
                }}
                onNavigateToSlug={(targetSlug, _nodeId) => {
                    setSelectedSlug(targetSlug);
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
        const vineInfo = enrichedSlugs.find(s => s.slug === selectedSlug);
        return (
            <VineViewer
                slug={selectedSlug}
                nodeCount={vineInfo?.node_count ?? 0}
                lastBuiltAt={vineInfo?.last_built_at ?? null}
                onBack={() => {
                    setSelectedSlug(null);
                    setView('list');
                    fetchData();
                }}
                onOpenBunch={(bunchSlug) => {
                    window.open(`${VIBESMITHY_BASE_URL}/space/${bunchSlug}`, '_blank');
                }}
            />
        );
    }

    if (view === 'asking' && askingSlug) {
        // Build a SlugInfo-compatible array for AskQuestion (exclude archived)
        const allSlugsForAsking = enrichedSlugs
            .filter(s => !s.archived_at)
            .map(s => ({
                slug: s.slug,
                content_type: s.content_type,
                source_path: s.source_path,
                node_count: s.node_count,
                max_depth: s.max_depth,
                last_built_at: s.last_built_at,
                created_at: s.created_at,
                referenced_slugs: s.referenced_slugs,
                referencing_slugs: s.referencing_slugs,
                archived_at: s.archived_at ?? null,
            }));
        return (
            <AskQuestion
                baseSlug={askingSlug}
                allSlugs={allSlugsForAsking}
                onClose={() => {
                    setAskingSlug(null);
                    setView('list');
                    fetchData();
                }}
                onSlugCreated={() => {
                    fetchData();
                }}
            />
        );
    }

    if (view === 'building' && buildingSlug) {
        const buildSlugInfo = enrichedSlugs.find(s => s.slug === buildingSlug);
        const isVineBuild = buildSlugInfo?.content_type === 'vine';
        if (isVineBuild) {
            return (
                <VineBuildProgress
                    slug={buildingSlug}
                    onComplete={handleBuildComplete}
                    onClose={() => {
                        setBuildingSlug(null);
                        setView('list');
                        fetchData();
                    }}
                />
            );
        }
        return (
            <BuildProgress
                slug={buildingSlug}
                onComplete={handleBuildComplete}
                onRetry={(s) => handleRebuild(s)}
                onClose={() => {
                    setBuildingSlug(null);
                    setView('list');
                }}
            />
        );
    }

    // ─── Summary bar chips ──────────────────────────────────────────────────

    const renderSummaryBar = () => {
        const chips: Array<{ label: string; className: string }> = [];
        if (counts.published > 0) chips.push({ label: `${counts.published} published`, className: 'pyramid-summary-chip pyramid-summary-chip-published' });
        if (counts.stale > 0) chips.push({ label: `${counts.stale} stale`, className: 'pyramid-summary-chip pyramid-summary-chip-stale' });
        if (counts.empty > 0) chips.push({ label: `${counts.empty} empty`, className: 'pyramid-summary-chip pyramid-summary-chip-empty' });
        if (chips.length === 0) return null;
        return (
            <div className="pyramid-summary-bar">
                {chips.map(c => (
                    <span key={c.label} className={c.className}>{c.label}</span>
                ))}
            </div>
        );
    };

    // ─── Main list view ─────────────────────────────────────────────────────

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

            {!loading && enrichedSlugs.length > 0 && (
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

            {!loading && enrichedSlugs.length === 0 && (
                <div className="pyramid-empty">
                    <div className="pyramid-empty-icon">W</div>
                    <h3>Build your first knowledge pyramid</h3>
                    <p>
                        Link a project directory or document folder to create a corpus,
                        then build a layered knowledge pyramid you can query, publish,
                        and share with agents.
                    </p>
                    <button className="btn btn-primary" onClick={() => setView('add')}>
                        Link a Folder &amp; Build
                    </button>
                </div>
            )}

            {!loading && enrichedSlugs.length > 0 && (
                <>
                    <div style={selectedSlugData ? { marginRight: 420 } : undefined}>
                    <PyramidToolbar
                        searchQuery={searchQuery}
                        onSearchChange={setSearchQuery}
                        activeTypes={activeTypes}
                        onToggleType={toggleType}
                        activeStatuses={activeStatuses}
                        onToggleStatus={toggleStatus}
                        sortBy={sortBy}
                        onSortChange={setSortBy}
                        counts={counts}
                    />

                    {renderSummaryBar()}

                    <div className="pyramid-grouped-list">
                        {groups.map(group => {
                            const isCollapsed = collapsedSections.has(group.key);
                            return (
                                <div key={group.key} className="pyramid-section">
                                    <div
                                        className="pyramid-section-header"
                                        onClick={() => toggleSection(group.key)}
                                    >
                                        <span
                                            className="pyramid-section-dot"
                                            style={{ backgroundColor: group.color }}
                                        />
                                        {group.label}
                                        <span className="pyramid-section-chevron">
                                            {isCollapsed ? '\u25B8' : '\u25BE'}
                                        </span>
                                    </div>
                                    {!isCollapsed && group.slugs.map(s => (
                                        <PyramidRow
                                            key={s.slug}
                                            slug={s}
                                            isSelected={selectedSlug === s.slug}
                                            publishingSlug={publishingSlug}
                                            maxNodeCount={maxNodeCount}
                                            onClick={() => setSelectedSlug(s.slug)}
                                            isNested={group.nestedSlugs.has(s.slug)}
                                        />
                                    ))}
                                </div>
                            );
                        })}
                    </div>
                    </div>

                    <PyramidDetailDrawer
                        slug={selectedSlugData}
                        onClose={handleDrawerClose}
                        onPublish={handlePublish}
                        onSetAccessTier={handleSetAccessTier}
                        onSetAbsorption={handleSetAbsorption}
                        onDelete={handleDelete}
                        onRebuild={(slug) => {
                            setSelectedSlug(null);
                            handleRebuild(slug);
                        }}
                        onOpenDadbear={(slug) => {
                            setSelectedSlug(slug);
                            setView('dadbear');
                        }}
                        onOpenFaq={(slug) => {
                            setSelectedSlug(slug);
                            setView('faq');
                        }}
                        onOpenVine={(slug) => {
                            setSelectedSlug(slug);
                            setView('vine');
                        }}
                        onAskQuestion={(slug) => {
                            setAskingSlug(slug);
                            setView('asking');
                        }}
                        onOpenVibesmithy={handleOpenVibesmithy}
                        publishingSlug={publishingSlug}
                        lastPublishResult={lastPublishResult}
                    />
                </>
            )}

        </div>
    );
}
