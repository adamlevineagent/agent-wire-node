/**
 * PyramidNavPage — Dedicated navigation page for interacting with memory pyramids.
 *
 * Layout (4 regions per design):
 *   Left rail:   Pyramid Navigator (slug list, vine-bedrock relationships)
 *   Main area:   Pyramid Visualization + Reading Mode Selector + Question Prompt Bar
 *   Right panel: Canonical Identities (vocabulary)
 *   Bottom bar:  DADBEAR Status (watch configs, pending ingests, recovery)
 *
 * Node Detail View opens as a slide-over when a node is clicked.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../contexts/AppContext';
import { SlugInfo, CONTENT_TYPE_CONFIG, relativeTime } from './pyramid-types';

// ── Constants ───────────────────────────────────────────────────────────

const PYRAMID_API_BASE = 'http://localhost:8765';

type ReadingMode = 'memoir' | 'walk' | 'thread' | 'decisions' | 'speaker' | 'search';

const READING_MODES: { key: ReadingMode; label: string }[] = [
    { key: 'memoir', label: 'Memoir' },
    { key: 'walk', label: 'Walk' },
    { key: 'thread', label: 'Thread' },
    { key: 'decisions', label: 'Decisions' },
    { key: 'speaker', label: 'Speaker' },
    { key: 'search', label: 'Search' },
];

// ── Types ───────────────────────────────────────────────────────────────

interface TreeNode {
    id: string;
    depth: number;
    headline: string;
    distilled?: string;
    self_prompt?: string;
    children: TreeNode[];
}

interface DrillResult {
    node: {
        id: string;
        slug: string;
        depth: number;
        chunk_index: number | null;
        headline: string;
        distilled: string;
        self_prompt: string;
        children: string[];
        parent_id: string | null;
        superseded_by: string | null;
        created_at: string;
        topics: Array<{ name: string; current: string; entities: string[]; corrections: any[]; decisions: any[] }>;
        corrections: any[];
        decisions: any[];
        terms: any[];
        dead_ends: string[];
    };
    children: Array<{
        id: string;
        slug: string;
        depth: number;
        headline: string;
        distilled: string;
        self_prompt: string;
        chunk_index: number | null;
        children: string[];
        parent_id: string | null;
        superseded_by: string | null;
        created_at: string;
        topics: any[];
        corrections: any[];
        decisions: any[];
        terms: any[];
        dead_ends: string[];
    }>;
    web_edges?: Array<{
        connected_to: string;
        connected_headline: string;
        relationship: string;
        strength: number;
    }>;
    evidence?: Array<{
        slug: string;
        source_node_id: string;
        target_node_id: string;
        verdict: string;
        weight: number | null;
        reason: string | null;
    }>;
    question_context?: { parent_question: string | null; sibling_questions: string[] } | null;
}

interface VocabEntry {
    name: string;
    category: string | null;
    importance: number | null;
    liveness: string;
    detail?: any;
}

interface DadbearWatchConfig {
    slug: string;
    watch_path: string;
    debounce_minutes: number;
    min_changed_files: number;
    runaway_threshold: number;
    breaker_tripped: boolean;
    breaker_tripped_at: string | null;
    frozen: boolean;
    frozen_at: string | null;
}

interface DadbearStatusData {
    watch_configs: DadbearWatchConfig[];
    pending_ingests: number;
    active_scans: number;
    last_scan_at: string | null;
}

interface RecoveryStatusData {
    stale_count: number;
    dead_letter_count: number;
    orphan_count: number;
    provisional_sessions: number;
    last_recovery_at: string | null;
}

interface AutoUpdateStatus {
    auto_update: boolean;
    frozen: boolean;
    breaker_tripped: boolean;
    pending_mutations_by_layer: Record<string, number>;
    last_check_at: string | null;
    phase: string | null;
    phase_detail: string | null;
    timer_fires_at: string | null;
    last_result_summary: string | null;
}

interface BedrockEntry {
    bedrock_slug: string;
    node_count: number;
    last_built_at: string | null;
}

// ── Props ───────────────────────────────────────────────────────────────

interface PyramidNavPageProps {
    initialSlug?: string;
    onBack: () => void;
}

// ── Helpers ─────────────────────────────────────────────────────────────

function flattenTreeByDepth(roots: TreeNode[]): Map<number, TreeNode[]> {
    const byDepth = new Map<number, TreeNode[]>();

    function walk(node: TreeNode) {
        const list = byDepth.get(node.depth) ?? [];
        list.push(node);
        byDepth.set(node.depth, list);
        for (const child of node.children) {
            walk(child);
        }
    }

    for (const root of roots) walk(root);
    return byDepth;
}

function findLeftmostSlope(roots: TreeNode[]): Set<string> {
    const slope = new Set<string>();
    let current: TreeNode | null = roots[0] ?? null;
    while (current) {
        slope.add(current.id);
        current = current.children[0] ?? null;
    }
    return slope;
}

// ── Component ───────────────────────────────────────────────────────────

export function PyramidNavPage({ initialSlug, onBack }: PyramidNavPageProps) {
    // ── Slug list & selection ───────────────────────────────────────────
    const [slugs, setSlugs] = useState<SlugInfo[]>([]);
    const [selectedSlug, setSelectedSlug] = useState<string | null>(initialSlug ?? null);
    const [slugsLoading, setSlugsLoading] = useState(true);

    // ── Tree data ──────────────────────────────────────────────────────
    const [tree, setTree] = useState<TreeNode[]>([]);
    const [treeLoading, setTreeLoading] = useState(false);

    // ── Node detail ────────────────────────────────────────────────────
    const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
    const [drillResult, setDrillResult] = useState<DrillResult | null>(null);
    const [drillLoading, setDrillLoading] = useState(false);

    // ── Reading mode ───────────────────────────────────────────────────
    const [readingMode, setReadingMode] = useState<ReadingMode>('memoir');
    const [readingData, setReadingData] = useState<any>(null);
    const [readingLoading, setReadingLoading] = useState(false);

    // ── Question bar ───────────────────────────────────────────────────
    const [question, setQuestion] = useState('');
    const [questionResult, setQuestionResult] = useState<any>(null);
    const [questionLoading, setQuestionLoading] = useState(false);

    // ── Vocabulary (right panel) ───────────────────────────────────────
    const [vocabulary, setVocabulary] = useState<VocabEntry[]>([]);
    const [vocabLoading, setVocabLoading] = useState(false);

    // ── DADBEAR status (bottom bar) ────────────────────────────────────
    const [dadbearStatus, setDadbearStatus] = useState<DadbearStatusData | null>(null);
    const [autoUpdateStatus, setAutoUpdateStatus] = useState<AutoUpdateStatus | null>(null);
    const [recoveryStatus, setRecoveryStatus] = useState<RecoveryStatusData | null>(null);
    const [bedrocks, setBedrocks] = useState<BedrockEntry[]>([]);

    // ── Multi-chain overlays ───────────────────────────────────────────
    const [overlayTab, setOverlayTab] = useState<string>('default');

    // ── Search mode ────────────────────────────────────────────────────
    const [searchQuery, setSearchQuery] = useState('');
    const [searchResults, setSearchResults] = useState<any[]>([]);
    const [searchLoading, setSearchLoading] = useState(false);

    // ── Error state ────────────────────────────────────────────────────
    const [error, setError] = useState<string | null>(null);

    // ── Auth token for HTTP fetches ───────────────────────────────────
    const [authToken, setAuthToken] = useState('');
    useEffect(() => {
        invoke<string>('pyramid_get_auth_token').then(setAuthToken).catch(() => {});
    }, []);
    const authHeaders = useMemo(() => {
        if (!authToken) return {};
        return { 'Authorization': `Bearer ${authToken}` } as Record<string, string>;
    }, [authToken]);

    // ─── Fetch slug list ───────────────────────────────────────────────

    const fetchSlugs = useCallback(async () => {
        setSlugsLoading(true);
        try {
            const data = await invoke<SlugInfo[]>('pyramid_list_slugs');
            setSlugs(data);
            // Auto-select first slug if none selected
            if (!selectedSlug && data.length > 0) {
                setSelectedSlug(data[0].slug);
            }
        } catch (err) {
            setError(`Failed to load slugs: ${err}`);
        } finally {
            setSlugsLoading(false);
        }
    }, [selectedSlug]);

    useEffect(() => {
        fetchSlugs();
    }, []);

    // ─── Fetch tree when slug changes ──────────────────────────────────

    useEffect(() => {
        if (!selectedSlug) return;
        setTreeLoading(true);
        setTree([]);
        setSelectedNodeId(null);
        setDrillResult(null);
        setReadingData(null);
        setQuestionResult(null);

        invoke<unknown>('pyramid_tree', { slug: selectedSlug })
            .then((raw) => {
                const nodes = normalizeTreeNodes(raw);
                setTree(nodes);
            })
            .catch((err) => {
                setError(`Failed to load tree: ${err}`);
            })
            .finally(() => setTreeLoading(false));
    }, [selectedSlug]);

    // ─── Fetch vocabulary when slug changes ────────────────────────────

    useEffect(() => {
        if (!selectedSlug || !authToken) return;
        setVocabLoading(true);
        setVocabulary([]);

        fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/vocabulary`, { headers: authHeaders })
            .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
            .then(data => {
                // Backend returns VocabularyCatalog: { topics, entities, decisions, terms, practices }
                // Flatten all categories into a single array
                const entries: VocabEntry[] = [
                    ...(data?.topics || []),
                    ...(data?.entities || []),
                    ...(data?.decisions || []),
                    ...(data?.terms || []),
                    ...(data?.practices || []),
                ];
                setVocabulary(entries);
            })
            .catch(() => {
                // Vocabulary may not exist yet — that's OK
                setVocabulary([]);
            })
            .finally(() => setVocabLoading(false));
    }, [selectedSlug, authToken, authHeaders]);

    // ─── Fetch DADBEAR status when slug changes ────────────────────────

    useEffect(() => {
        if (!selectedSlug) return;

        // DADBEAR auto-update status (via IPC)
        invoke<AutoUpdateStatus>('pyramid_auto_update_status', { slug: selectedSlug })
            .then(setAutoUpdateStatus)
            .catch(() => setAutoUpdateStatus(null));

        // DADBEAR watch status (via HTTP)
        if (authToken) {
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/dadbear/status`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(setDadbearStatus)
                .catch(() => setDadbearStatus(null));

            // Recovery status (via HTTP)
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/recovery/status`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(setRecoveryStatus)
                .catch(() => setRecoveryStatus(null));

            // Vine bedrocks (via HTTP)
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/vine/bedrocks`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(data => {
                    const items = Array.isArray(data) ? data : (data?.bedrocks ?? []);
                    setBedrocks(items);
                })
                .catch(() => setBedrocks([]));
        }
    }, [selectedSlug, authToken, authHeaders]);

    // ─── Fetch reading mode data ───────────────────────────────────────

    useEffect(() => {
        if (!selectedSlug || readingMode === 'search') return;
        setReadingLoading(true);
        setReadingData(null);

        // Use IPC for tree/apex, reading mode HTTP endpoints for the rest
        if (readingMode === 'memoir') {
            invoke<any>('pyramid_apex', { slug: selectedSlug })
                .then(setReadingData)
                .catch(() => setReadingData(null))
                .finally(() => setReadingLoading(false));
        } else if (readingMode === 'walk') {
            setReadingData(tree);
            setReadingLoading(false);
        } else if (!authToken) {
            // HTTP reading modes need auth — wait for token
            setReadingLoading(false);
        } else if (readingMode === 'decisions') {
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/reading/decisions`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(setReadingData)
                .catch(() => setReadingData(null))
                .finally(() => setReadingLoading(false));
        } else if (readingMode === 'speaker') {
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/reading/speaker?role=human`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(setReadingData)
                .catch(() => setReadingData(null))
                .finally(() => setReadingLoading(false));
        } else if (readingMode === 'thread') {
            fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/reading/thread?identity=*`, { headers: authHeaders })
                .then(r => r.ok ? r.json() : Promise.reject(`HTTP ${r.status}`))
                .then(setReadingData)
                .catch(() => setReadingData(null))
                .finally(() => setReadingLoading(false));
        } else {
            setReadingLoading(false);
        }
    }, [selectedSlug, readingMode, authToken, authHeaders]);

    // ─── Node drill ────────────────────────────────────────────────────

    const handleNodeClick = useCallback((nodeId: string) => {
        if (!selectedSlug) return;
        setSelectedNodeId(nodeId);
        setDrillLoading(true);

        invoke<DrillResult>('pyramid_drill', { slug: selectedSlug, nodeId })
            .then(setDrillResult)
            .catch((err) => {
                setError(`Failed to drill: ${err}`);
                setDrillResult(null);
            })
            .finally(() => setDrillLoading(false));
    }, [selectedSlug]);

    const closeNodeDetail = useCallback(() => {
        setSelectedNodeId(null);
        setDrillResult(null);
    }, []);

    // ─── Question submit ───────────────────────────────────────────────

    const handleQuestionSubmit = useCallback(async () => {
        if (!selectedSlug || !question.trim()) return;
        setQuestionLoading(true);
        setQuestionResult(null);

        try {
            // Use the search endpoint as the primary question mechanism
            const r = await fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/search?q=${encodeURIComponent(question.trim())}`, { headers: authHeaders });
            if (r.ok) {
                const data = await r.json();
                setQuestionResult(data);
            } else {
                setError(`Question failed: HTTP ${r.status}`);
            }
        } catch (err) {
            setError(`Question failed: ${err}`);
        } finally {
            setQuestionLoading(false);
        }
    }, [selectedSlug, question, authHeaders]);

    // ─── Search ────────────────────────────────────────────────────────

    const handleSearch = useCallback(async () => {
        if (!selectedSlug || !searchQuery.trim()) return;
        setSearchLoading(true);
        setSearchResults([]);

        try {
            const r = await fetch(`${PYRAMID_API_BASE}/pyramid/${selectedSlug}/search?q=${encodeURIComponent(searchQuery.trim())}`, { headers: authHeaders });
            if (r.ok) {
                const data = await r.json();
                setSearchResults(Array.isArray(data) ? data : (data?.results ?? data?.hits ?? [data]));
            }
        } catch {
            // Ignore search errors
        } finally {
            setSearchLoading(false);
        }
    }, [selectedSlug, searchQuery]);

    // ─── Derived data ──────────────────────────────────────────────────

    const depthMap = useMemo(() => flattenTreeByDepth(tree), [tree]);
    const slopeIds = useMemo(() => findLeftmostSlope(tree), [tree]);
    const maxDepth = useMemo(() => {
        let max = 0;
        for (const d of depthMap.keys()) if (d > max) max = d;
        return max;
    }, [depthMap]);

    const selectedSlugInfo = useMemo(() => {
        return slugs.find(s => s.slug === selectedSlug) ?? null;
    }, [slugs, selectedSlug]);

    // Group vocabulary by category
    const vocabByCategory = useMemo(() => {
        const groups: Record<string, VocabEntry[]> = {};
        for (const v of vocabulary) {
            const cat = v.category || 'other';
            if (!groups[cat]) groups[cat] = [];
            groups[cat].push(v);
        }
        // Sort each group by importance
        for (const cat of Object.keys(groups)) {
            groups[cat].sort((a, b) => (b.importance ?? 0) - (a.importance ?? 0));
        }
        return groups;
    }, [vocabulary]);

    const totalPendingMutations = useMemo(() => {
        if (!autoUpdateStatus?.pending_mutations_by_layer) return 0;
        return Object.values(autoUpdateStatus.pending_mutations_by_layer).reduce((a, b) => a + b, 0);
    }, [autoUpdateStatus]);

    // ─── Render ────────────────────────────────────────────────────────

    return (
        <div className="pnav-page">
            {/* ── Left Rail: Pyramid Navigator ──────────────────────── */}
            <aside className="pnav-left-rail">
                <div className="pnav-left-header">
                    <button className="btn btn-small btn-ghost" onClick={onBack}>
                        &larr; Back
                    </button>
                    <h3 className="pnav-left-title">Pyramids</h3>
                </div>

                {slugsLoading ? (
                    <div className="pnav-loading-small">Loading...</div>
                ) : (
                    <div className="pnav-slug-list">
                        {slugs.filter(s => !s.archived_at).map(s => {
                            const cfg = CONTENT_TYPE_CONFIG[s.content_type] ?? { label: s.content_type, color: '#888', icon: '?' };
                            const isSelected = s.slug === selectedSlug;
                            const isVine = s.content_type === 'vine';
                            const isReferenced = selectedSlugInfo?.referenced_slugs?.includes(s.slug);
                            const isReferencing = s.referencing_slugs?.includes(selectedSlug ?? '');

                            return (
                                <div
                                    key={s.slug}
                                    className={`pnav-slug-item${isSelected ? ' pnav-slug-selected' : ''}${isReferenced ? ' pnav-slug-referenced' : ''}${isReferencing ? ' pnav-slug-referencing' : ''}`}
                                    onClick={() => setSelectedSlug(s.slug)}
                                >
                                    <div className="pnav-slug-icon" style={{ color: cfg.color }}>
                                        {cfg.icon}
                                    </div>
                                    <div className="pnav-slug-info">
                                        <div className="pnav-slug-name">{s.slug}</div>
                                        <div className="pnav-slug-meta">
                                            {s.node_count} nodes
                                            {s.max_depth > 0 && ` \u00B7 L${s.max_depth}`}
                                            {s.last_built_at && ` \u00B7 ${relativeTime(s.last_built_at)}`}
                                        </div>
                                    </div>
                                    {isVine && <span className="pnav-badge pnav-badge-vine">vine</span>}
                                    {isReferenced && <span className="pnav-badge pnav-badge-bedrock">bedrock</span>}
                                </div>
                            );
                        })}
                    </div>
                )}

                {/* Vine bedrocks section */}
                {bedrocks.length > 0 && (
                    <div className="pnav-bedrocks-section">
                        <h4 className="pnav-section-title">Bedrocks</h4>
                        {bedrocks.map(b => (
                            <div
                                key={b.bedrock_slug}
                                className="pnav-bedrock-item"
                                onClick={() => setSelectedSlug(b.bedrock_slug)}
                            >
                                <span className="pnav-bedrock-name">{b.bedrock_slug}</span>
                                <span className="pnav-bedrock-meta">
                                    {b.node_count} nodes
                                    {b.last_built_at && ` \u00B7 ${relativeTime(b.last_built_at)}`}
                                </span>
                            </div>
                        ))}
                    </div>
                )}
            </aside>

            {/* ── Main Area ────────────────────────────────────────── */}
            <main className="pnav-main">
                {!selectedSlug ? (
                    <div className="pnav-empty">
                        <h3>Select a pyramid</h3>
                        <p>Choose a pyramid from the left rail to begin exploring.</p>
                    </div>
                ) : (
                    <>
                        {/* Header with slug name */}
                        <div className="pnav-main-header">
                            <h2 className="pnav-main-title">{selectedSlug}</h2>
                            {selectedSlugInfo && (
                                <span className="pnav-main-meta">
                                    {selectedSlugInfo.node_count} nodes
                                    {' \u00B7 '}
                                    {selectedSlugInfo.max_depth} layers
                                    {selectedSlugInfo.content_type && (
                                        <>
                                            {' \u00B7 '}
                                            <span style={{ color: CONTENT_TYPE_CONFIG[selectedSlugInfo.content_type]?.color ?? '#888' }}>
                                                {CONTENT_TYPE_CONFIG[selectedSlugInfo.content_type]?.label ?? selectedSlugInfo.content_type}
                                            </span>
                                        </>
                                    )}
                                </span>
                            )}
                        </div>

                        {/* Question Prompt Bar */}
                        <div className="pnav-question-bar">
                            <input
                                type="text"
                                className="pnav-question-input"
                                placeholder="Ask a question about this pyramid..."
                                value={question}
                                onChange={e => setQuestion(e.target.value)}
                                onKeyDown={e => {
                                    if (e.key === 'Enter') handleQuestionSubmit();
                                }}
                                disabled={questionLoading}
                            />
                            <button
                                className="btn btn-primary btn-small pnav-question-btn"
                                onClick={handleQuestionSubmit}
                                disabled={!question.trim() || questionLoading}
                            >
                                {questionLoading ? 'Asking...' : 'Ask'}
                            </button>
                        </div>

                        {/* Reading Mode Selector */}
                        <div className="pnav-reading-tabs">
                            {READING_MODES.map(m => (
                                <button
                                    key={m.key}
                                    className={`pnav-reading-tab${readingMode === m.key ? ' pnav-reading-tab-active' : ''}`}
                                    onClick={() => setReadingMode(m.key)}
                                >
                                    {m.label}
                                </button>
                            ))}
                        </div>

                        {/* Question result */}
                        {questionResult && (
                            <div className="pnav-question-result">
                                <div className="pnav-question-result-header">
                                    <h4>Question Result</h4>
                                    <button className="btn btn-ghost btn-small" onClick={() => setQuestionResult(null)}>
                                        Dismiss
                                    </button>
                                </div>
                                <QuestionResultView data={questionResult} onNodeClick={handleNodeClick} />
                            </div>
                        )}

                        {/* Main content area based on reading mode */}
                        <div className="pnav-content-area">
                            {readingMode === 'search' ? (
                                <SearchModeView
                                    searchQuery={searchQuery}
                                    setSearchQuery={setSearchQuery}
                                    results={searchResults}
                                    loading={searchLoading}
                                    onSearch={handleSearch}
                                    onNodeClick={handleNodeClick}
                                />
                            ) : readingMode === 'memoir' ? (
                                <MemoirView data={readingData} loading={readingLoading} />
                            ) : readingMode === 'walk' ? (
                                <WalkView
                                    depthMap={depthMap}
                                    slopeIds={slopeIds}
                                    maxDepth={maxDepth}
                                    loading={treeLoading}
                                    onNodeClick={handleNodeClick}
                                />
                            ) : readingMode === 'thread' ? (
                                <ThreadView data={readingData} loading={readingLoading} onNodeClick={handleNodeClick} />
                            ) : readingMode === 'decisions' ? (
                                <DecisionsView data={readingData} loading={readingLoading} />
                            ) : readingMode === 'speaker' ? (
                                <SpeakerView data={readingData} loading={readingLoading} />
                            ) : (
                                <div className="pnav-loading-small">Loading...</div>
                            )}

                            {/* Pyramid layer visualization (always shown below reading content in walk mode) */}
                            {readingMode !== 'walk' && tree.length > 0 && !treeLoading && (
                                <div className="pnav-pyramid-viz">
                                    <h4 className="pnav-section-title">Pyramid Structure</h4>
                                    <WalkView
                                        depthMap={depthMap}
                                        slopeIds={slopeIds}
                                        maxDepth={maxDepth}
                                        loading={false}
                                        onNodeClick={handleNodeClick}
                                        compact
                                    />
                                </div>
                            )}
                        </div>
                    </>
                )}
            </main>

            {/* ── Right Panel: Canonical Identities ─────────────────── */}
            <aside className="pnav-right-panel">
                <h3 className="pnav-right-title">Canonical Identities</h3>

                {vocabLoading ? (
                    <div className="pnav-loading-small">Loading vocabulary...</div>
                ) : vocabulary.length === 0 ? (
                    <div className="pnav-vocab-empty">
                        No vocabulary catalog yet.
                        {selectedSlug && ' Build the pyramid to populate.'}
                    </div>
                ) : (
                    <div className="pnav-vocab-groups">
                        {Object.entries(vocabByCategory).map(([category, entries]) => (
                            <VocabCategoryGroup key={category} category={category} entries={entries} />
                        ))}
                    </div>
                )}
            </aside>

            {/* ── Node Detail Slide-Over ────────────────────────────── */}
            {selectedNodeId && (
                <div className="pnav-node-detail-overlay" onClick={closeNodeDetail}>
                    <div className="pnav-node-detail-panel" onClick={e => e.stopPropagation()}>
                        <NodeDetailView
                            drillResult={drillResult}
                            loading={drillLoading}
                            onClose={closeNodeDetail}
                            onNavigateNode={handleNodeClick}
                            selectedSlug={selectedSlug!}
                        />
                    </div>
                </div>
            )}

            {/* ── Bottom Bar: DADBEAR Status ────────────────────────── */}
            <footer className="pnav-bottom-bar">
                <DadbearStatusBar
                    dadbearStatus={dadbearStatus}
                    autoUpdateStatus={autoUpdateStatus}
                    recoveryStatus={recoveryStatus}
                    totalPendingMutations={totalPendingMutations}
                    slug={selectedSlug}
                />
            </footer>

            {/* ── Error toast ───────────────────────────────────────── */}
            {error && (
                <div className="pnav-error-toast">
                    <span>{error}</span>
                    <button className="pnav-error-dismiss" onClick={() => setError(null)}>
                        &times;
                    </button>
                </div>
            )}
        </div>
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Sub-components
// ═══════════════════════════════════════════════════════════════════════

// ── Vocabulary Category Group ──────────────────────────────────────────

function VocabCategoryGroup({ category, entries }: { category: string; entries: VocabEntry[] }) {
    const [expanded, setExpanded] = useState(true);
    const label = category.charAt(0).toUpperCase() + category.slice(1);
    const liveCount = entries.filter(e => e.liveness === 'live').length;

    return (
        <div className="pnav-vocab-category">
            <div className="pnav-vocab-category-header" onClick={() => setExpanded(!expanded)}>
                <span className="pnav-vocab-category-label">{label}</span>
                <span className="pnav-vocab-category-count">
                    {liveCount}/{entries.length}
                </span>
                <span className="pnav-vocab-toggle">{expanded ? '\u25B2' : '\u25BC'}</span>
            </div>
            {expanded && (
                <div className="pnav-vocab-entries">
                    {entries.slice(0, 20).map((v, i) => (
                        <div key={i} className={`pnav-vocab-entry${v.liveness === 'live' ? '' : ' pnav-vocab-mooted'}`}>
                            <span className="pnav-vocab-name">{v.name}</span>
                            {(v.importance ?? 0) > 0 && (
                                <span className="pnav-vocab-importance">
                                    {'*'.repeat(Math.min(3, Math.ceil((v.importance ?? 0) * 3)))}
                                </span>
                            )}
                        </div>
                    ))}
                    {entries.length > 20 && (
                        <div className="pnav-vocab-more">+{entries.length - 20} more</div>
                    )}
                </div>
            )}
        </div>
    );
}

// ── Memoir View ────────────────────────────────────────────────────────

function MemoirView({ data, loading }: { data: any; loading: boolean }) {
    if (loading) return <div className="pnav-loading-small">Loading memoir...</div>;
    if (!data) return <div className="pnav-empty-content">No apex data available.</div>;

    const summary = data.summary ?? data.distilled ?? data.headline ?? '';
    const headline = data.headline ?? data.id ?? 'Apex';

    // Narrative multi-zoom: prefer the deepest zoom level's prose
    const narrativeText = data.narrative?.levels?.[0]?.text ?? '';

    return (
        <div className="pnav-memoir">
            <h3 className="pnav-memoir-headline">{headline}</h3>
            {narrativeText ? (
                <div className="pnav-memoir-narrative">
                    <p>{narrativeText}</p>
                </div>
            ) : summary ? (
                <p className="pnav-memoir-summary">{summary}</p>
            ) : null}
            {data.topics && data.topics.length > 0 && (
                <div className="pnav-memoir-topics">
                    <h5>Key Topics</h5>
                    <div className="pnav-tag-list">
                        {data.topics.map((t: any, i: number) => (
                            <span key={i} className="pnav-tag pnav-tag-topic">
                                {typeof t === 'string' ? t : t.name ?? JSON.stringify(t)}
                            </span>
                        ))}
                    </div>
                </div>
            )}
            {data.decisions && data.decisions.length > 0 && (
                <div className="pnav-memoir-decisions">
                    <h5>Decisions</h5>
                    {data.decisions.slice(0, 10).map((d: any, i: number) => (
                        <div key={i} className="pnav-decision-item">
                            <span className={`pnav-decision-stance pnav-stance-${d.stance ?? 'open'}`}>
                                {d.stance ?? 'open'}
                            </span>
                            <span className="pnav-decision-text">
                                {typeof d === 'string' ? d : d.decided ?? d.question ?? d.name ?? JSON.stringify(d)}
                            </span>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
}

// ── Walk View (Layer Visualization) ────────────────────────────────────

function WalkView({
    depthMap,
    slopeIds,
    maxDepth,
    loading,
    onNodeClick,
    compact = false,
}: {
    depthMap: Map<number, TreeNode[]>;
    slopeIds: Set<string>;
    maxDepth: number;
    loading: boolean;
    onNodeClick: (nodeId: string) => void;
    compact?: boolean;
}) {
    if (loading) return <div className="pnav-loading-small">Loading structure...</div>;
    if (depthMap.size === 0) return <div className="pnav-empty-content">No nodes in this pyramid.</div>;

    // Render from apex (max depth) down to L0
    const layers: number[] = [];
    for (let d = maxDepth; d >= 0; d--) {
        if (depthMap.has(d)) layers.push(d);
    }

    return (
        <div className={`pnav-walk${compact ? ' pnav-walk-compact' : ''}`}>
            {layers.map(depth => {
                const nodes = depthMap.get(depth) ?? [];
                const isApex = depth === maxDepth;

                return (
                    <div key={depth} className="pnav-layer">
                        <div className="pnav-layer-label">
                            {isApex ? 'Apex' : `L${depth}`}
                            <span className="pnav-layer-count">({nodes.length})</span>
                        </div>
                        <div className="pnav-layer-nodes">
                            {nodes.slice(0, compact ? 8 : 50).map(node => {
                                const isSlope = slopeIds.has(node.id);
                                return (
                                    <div
                                        key={node.id}
                                        className={`pnav-node${isSlope ? ' pnav-node-slope' : ''}${isApex ? ' pnav-node-apex' : ''}`}
                                        onClick={() => onNodeClick(node.id)}
                                        title={node.headline}
                                    >
                                        <span className="pnav-node-headline">
                                            {node.headline.length > 60
                                                ? node.headline.slice(0, 57) + '...'
                                                : node.headline}
                                        </span>
                                    </div>
                                );
                            })}
                            {nodes.length > (compact ? 8 : 50) && (
                                <div className="pnav-layer-overflow">
                                    +{nodes.length - (compact ? 8 : 50)} more
                                </div>
                            )}
                        </div>
                    </div>
                );
            })}
        </div>
    );
}

// ── Thread View ────────────────────────────────────────────────────────

function ThreadView({
    data,
    loading,
    onNodeClick,
}: {
    data: any;
    loading: boolean;
    onNodeClick: (nodeId: string) => void;
}) {
    if (loading) return <div className="pnav-loading-small">Loading thread...</div>;
    if (!data) return <div className="pnav-empty-content">No thread data available.</div>;

    const mentions = Array.isArray(data) ? data : (data?.mentions ?? []);

    if (mentions.length === 0) return <div className="pnav-empty-content">No mentions found for this identity.</div>;

    return (
        <div className="pnav-threads">
            {data?.identity && <div className="pnav-decisions-count">Thread: {data.identity} ({mentions.length} mentions)</div>}
            {mentions.map((m: any, i: number) => (
                <div key={i} className="pnav-thread-item" onClick={() => m.node_id && onNodeClick(m.node_id)} style={{ cursor: m.node_id ? 'pointer' : 'default' }}>
                    <div className="pnav-thread-header">
                        <span className="pnav-thread-name">
                            {m.headline ?? m.node_id ?? `Mention ${i + 1}`}
                        </span>
                        {m.depth != null && (
                            <span className={`pnav-thread-lifecycle pnav-lifecycle-active`}>
                                L{m.depth}
                            </span>
                        )}
                    </div>
                    {m.matched_text && <span className="pnav-thread-desc">matched: {m.matched_text}</span>}
                </div>
            ))}
        </div>
    );
}

// ── Decisions View ─────────────────────────────────────────────────────

function DecisionsView({ data, loading }: { data: any; loading: boolean }) {
    if (loading) return <div className="pnav-loading-small">Loading decisions...</div>;
    if (!data) return <div className="pnav-empty-content">No decisions data available.</div>;

    const decisions = Array.isArray(data) ? data : (data?.decisions ?? []);

    if (decisions.length === 0) return <div className="pnav-empty-content">No decisions found.</div>;

    return (
        <div className="pnav-decisions-list">
            <div className="pnav-decisions-count">{data?.total_count ?? decisions.length} decisions</div>
            {decisions.map((d: any, i: number) => (
                <div key={i} className="pnav-decision-card">
                    <div className="pnav-decision-card-header">
                        <span className={`pnav-decision-stance pnav-stance-${d.stance ?? 'open'}`}>
                            {d.stance ?? 'open'}
                        </span>
                        <span className="pnav-decision-question">
                            {d.decided ?? d.question ?? d.name ?? `Decision ${i + 1}`}
                        </span>
                    </div>
                    {d.why && <p className="pnav-decision-answer">{d.why}</p>}
                    {d.source_node_id && (
                        <div className="pnav-decision-source">from {d.source_node_id} (depth {d.source_depth ?? '?'})</div>
                    )}
                </div>
            ))}
        </div>
    );
}

// ── Speaker View ───────────────────────────────────────────────────────

function SpeakerView({ data, loading }: { data: any; loading: boolean }) {
    if (loading) return <div className="pnav-loading-small">Loading speaker quotes...</div>;
    if (!data) return <div className="pnav-empty-content">No speaker data available.</div>;

    const quotes = Array.isArray(data) ? data : (data?.quotes ?? []);

    if (quotes.length === 0) return <div className="pnav-empty-content">No quotes found for this speaker role. The episodic chain may not have extracted key_quotes with speaker_role from this conversation.</div>;

    return (
        <div className="pnav-entities-list">
            <div className="pnav-decisions-count">{data?.total_count ?? quotes.length} quotes ({data?.role ?? 'human'})</div>
            {quotes.map((q: any, i: number) => (
                <div key={i} className="pnav-entity-card">
                    <div className="pnav-entity-name">
                        {q.text ?? q.quote ?? `Quote ${i + 1}`}
                    </div>
                    {q.importance != null && q.importance > 0 && (
                        <div className="pnav-entity-aliases">
                            Importance: {q.importance.toFixed(2)}
                        </div>
                    )}
                    {q.source_node_id && (
                        <div className="pnav-entity-refs">
                            from {q.source_node_id}
                        </div>
                    )}
                </div>
            ))}
        </div>
    );
}

// ── Search Mode View ───────────────────────────────────────────────────

function SearchModeView({
    searchQuery,
    setSearchQuery,
    results,
    loading,
    onSearch,
    onNodeClick,
}: {
    searchQuery: string;
    setSearchQuery: (q: string) => void;
    results: any[];
    loading: boolean;
    onSearch: () => void;
    onNodeClick: (nodeId: string) => void;
}) {
    return (
        <div className="pnav-search-mode">
            <div className="pnav-search-bar">
                <input
                    type="text"
                    className="pnav-search-input"
                    placeholder="Search verbatim..."
                    value={searchQuery}
                    onChange={e => setSearchQuery(e.target.value)}
                    onKeyDown={e => { if (e.key === 'Enter') onSearch(); }}
                />
                <button
                    className="btn btn-primary btn-small"
                    onClick={onSearch}
                    disabled={!searchQuery.trim() || loading}
                >
                    {loading ? 'Searching...' : 'Search'}
                </button>
            </div>
            {results.length > 0 && (
                <div className="pnav-search-results">
                    {results.map((r: any, i: number) => (
                        <div key={i} className="pnav-search-result" onClick={() => (r.node_id ?? r.id) && onNodeClick(r.node_id ?? r.id)}>
                            <div className="pnav-search-result-headline">
                                {r.headline ?? r.title ?? r.id ?? `Result ${i + 1}`}
                            </div>
                            {r.snippet && <div className="pnav-search-result-snippet">{r.snippet}</div>}
                            {r.distilled && <div className="pnav-search-result-snippet">{r.distilled}</div>}
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
}

// ── Question Result View ───────────────────────────────────────────────

function QuestionResultView({ data, onNodeClick }: { data: any; onNodeClick: (nodeId: string) => void }) {
    if (!data) return null;

    // Handle various shapes the search endpoint can return
    const results = Array.isArray(data) ? data : (data?.results ?? data?.hits ?? [data]);

    return (
        <div className="pnav-question-results">
            {results.map((r: any, i: number) => (
                <div key={i} className="pnav-question-result-item" onClick={() => (r.node_id ?? r.id) && onNodeClick(r.node_id ?? r.id)}>
                    <div className="pnav-question-result-headline">
                        {r.headline ?? r.title ?? r.id ?? `Result ${i + 1}`}
                    </div>
                    {(r.snippet || r.distilled) && (
                        <div className="pnav-question-result-text">
                            {r.snippet ?? r.distilled}
                        </div>
                    )}
                </div>
            ))}
            {results.length === 0 && (
                <div className="pnav-empty-content">No results found.</div>
            )}
        </div>
    );
}

// ── Node Detail View ───────────────────────────────────────────────────

function NodeDetailView({
    drillResult,
    loading,
    onClose,
    onNavigateNode,
    selectedSlug,
}: {
    drillResult: DrillResult | null;
    loading: boolean;
    onClose: () => void;
    onNavigateNode: (nodeId: string) => void;
    selectedSlug: string;
}) {
    if (loading) {
        return (
            <div className="pnav-node-detail">
                <div className="pnav-node-detail-header">
                    <h3>Loading...</h3>
                    <button className="pnav-node-close" onClick={onClose}>&times;</button>
                </div>
                <div className="pnav-loading-small">Fetching node details...</div>
            </div>
        );
    }

    if (!drillResult) {
        return (
            <div className="pnav-node-detail">
                <div className="pnav-node-detail-header">
                    <h3>No Data</h3>
                    <button className="pnav-node-close" onClick={onClose}>&times;</button>
                </div>
                <div className="pnav-empty-content">Could not load node details.</div>
            </div>
        );
    }

    const node = drillResult.node;

    return (
        <div className="pnav-node-detail">
            <div className="pnav-node-detail-header">
                <div>
                    <h3>{node.headline}</h3>
                    <span className="pnav-node-detail-meta">
                        L{node.depth}
                        {node.chunk_index !== null && ` \u00B7 Chunk ${node.chunk_index}`}
                        {' \u00B7 '}
                        {node.id}
                    </span>
                </div>
                <button className="pnav-node-close" onClick={onClose}>&times;</button>
            </div>

            <div className="pnav-node-detail-body">
                {/* Distilled content */}
                {node.distilled && (
                    <div className="pnav-node-section">
                        <h4>Summary</h4>
                        <p>{node.distilled}</p>
                    </div>
                )}

                {/* Self prompt */}
                {node.self_prompt && (
                    <div className="pnav-node-section">
                        <h4>Self Prompt</h4>
                        <p className="pnav-node-self-prompt">{node.self_prompt}</p>
                    </div>
                )}

                {/* Topics */}
                {node.topics && node.topics.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Topics</h4>
                        <div className="pnav-tag-list">
                            {node.topics.map((t, i) => (
                                <span key={i} className="pnav-tag pnav-tag-topic">
                                    {t.name}
                                    {t.current && <span className="pnav-tag-status"> ({t.current})</span>}
                                </span>
                            ))}
                        </div>
                    </div>
                )}

                {/* Decisions */}
                {node.decisions && node.decisions.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Decisions</h4>
                        {node.decisions.map((d: any, i: number) => (
                            <div key={i} className="pnav-decision-item">
                                <span className={`pnav-decision-stance pnav-stance-${d.stance ?? 'open'}`}>
                                    {d.stance ?? 'open'}
                                </span>
                                <span>{d.decided ?? d.question ?? d.name ?? JSON.stringify(d)}</span>
                            </div>
                        ))}
                    </div>
                )}

                {/* Terms */}
                {node.terms && node.terms.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Terms</h4>
                        <div className="pnav-tag-list">
                            {node.terms.map((t: any, i: number) => (
                                <span key={i} className="pnav-tag pnav-tag-term">
                                    {typeof t === 'string' ? t : t.term ?? t.name ?? JSON.stringify(t)}
                                </span>
                            ))}
                        </div>
                    </div>
                )}

                {/* Dead ends */}
                {node.dead_ends && node.dead_ends.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Dead Ends</h4>
                        <ul className="pnav-dead-ends">
                            {node.dead_ends.map((de, i) => (
                                <li key={i}>{de}</li>
                            ))}
                        </ul>
                    </div>
                )}

                {/* Children */}
                {drillResult.children && drillResult.children.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Children ({drillResult.children.length})</h4>
                        <div className="pnav-children-list">
                            {drillResult.children.map(child => (
                                <div
                                    key={child.id}
                                    className="pnav-child-item"
                                    onClick={() => onNavigateNode(child.id)}
                                >
                                    <span className="pnav-child-depth">L{child.depth}</span>
                                    <span className="pnav-child-headline">{child.headline}</span>
                                </div>
                            ))}
                        </div>
                    </div>
                )}

                {/* Web edges (ties_to) */}
                {drillResult.web_edges && drillResult.web_edges.length > 0 && (
                    <div className="pnav-node-section">
                        <h4>Cross-References ({drillResult.web_edges.length})</h4>
                        <div className="pnav-web-edges">
                            {drillResult.web_edges.map((edge, i) => (
                                <div
                                    key={i}
                                    className="pnav-web-edge"
                                    onClick={() => onNavigateNode(edge.connected_to)}
                                >
                                    <span className="pnav-web-edge-headline">{edge.connected_headline}</span>
                                    <span className="pnav-web-edge-rel">{edge.relationship}</span>
                                </div>
                            ))}
                        </div>
                    </div>
                )}

                {/* Question context */}
                {drillResult.question_context && (
                    <div className="pnav-node-section">
                        <h4>Question Context</h4>
                        {drillResult.question_context.parent_question && (
                            <p><strong>Parent:</strong> {drillResult.question_context.parent_question}</p>
                        )}
                        {drillResult.question_context.sibling_questions.length > 0 && (
                            <div>
                                <strong>Siblings:</strong>
                                <ul>
                                    {drillResult.question_context.sibling_questions.map((q, i) => (
                                        <li key={i}>{q}</li>
                                    ))}
                                </ul>
                            </div>
                        )}
                    </div>
                )}
            </div>
        </div>
    );
}

// ── DADBEAR Status Bar ─────────────────────────────────────────────────

function DadbearStatusBar({
    dadbearStatus,
    autoUpdateStatus,
    recoveryStatus,
    totalPendingMutations,
    slug,
}: {
    dadbearStatus: DadbearStatusData | null;
    autoUpdateStatus: AutoUpdateStatus | null;
    recoveryStatus: RecoveryStatusData | null;
    totalPendingMutations: number;
    slug: string | null;
}) {
    return (
        <div className="pnav-dadbear-bar">
            {/* DADBEAR status */}
            <div className="pnav-dadbear-section">
                <span className="pnav-dadbear-label">DADBEAR</span>
                {autoUpdateStatus ? (
                    <>
                        <span className={`pnav-status-dot${autoUpdateStatus.frozen ? ' pnav-dot-frozen' : autoUpdateStatus.breaker_tripped ? ' pnav-dot-breaker' : ' pnav-dot-ok'}`} />
                        <span className="pnav-dadbear-text">
                            {autoUpdateStatus.frozen
                                ? 'Frozen'
                                : autoUpdateStatus.breaker_tripped
                                    ? 'Breaker Tripped'
                                    : autoUpdateStatus.auto_update
                                        ? 'Active'
                                        : 'Manual'}
                        </span>
                        {autoUpdateStatus.phase && (
                            <span className="pnav-dadbear-phase">
                                {autoUpdateStatus.phase}
                                {autoUpdateStatus.phase_detail && `: ${autoUpdateStatus.phase_detail}`}
                            </span>
                        )}
                    </>
                ) : (
                    <span className="pnav-dadbear-text pnav-text-muted">Not configured</span>
                )}
            </div>

            {/* Pending mutations */}
            {totalPendingMutations > 0 && (
                <div className="pnav-dadbear-section">
                    <span className="pnav-dadbear-label">Pending</span>
                    <span className="pnav-dadbear-text">{totalPendingMutations} mutations</span>
                </div>
            )}

            {/* Watch configs */}
            {dadbearStatus && dadbearStatus.watch_configs && dadbearStatus.watch_configs.length > 0 && (
                <div className="pnav-dadbear-section">
                    <span className="pnav-dadbear-label">Watches</span>
                    <span className="pnav-dadbear-text">{dadbearStatus.watch_configs.length} folder(s)</span>
                </div>
            )}

            {/* Pending ingests */}
            {dadbearStatus && dadbearStatus.pending_ingests > 0 && (
                <div className="pnav-dadbear-section">
                    <span className="pnav-dadbear-label">Ingests</span>
                    <span className="pnav-dadbear-text pnav-text-warn">{dadbearStatus.pending_ingests} pending</span>
                </div>
            )}

            {/* Recovery status */}
            {recoveryStatus && (recoveryStatus.stale_count > 0 || recoveryStatus.dead_letter_count > 0) && (
                <div className="pnav-dadbear-section">
                    <span className="pnav-dadbear-label">Health</span>
                    {recoveryStatus.stale_count > 0 && (
                        <span className="pnav-dadbear-text pnav-text-warn">{recoveryStatus.stale_count} stale</span>
                    )}
                    {recoveryStatus.dead_letter_count > 0 && (
                        <span className="pnav-dadbear-text pnav-text-error">{recoveryStatus.dead_letter_count} dead letters</span>
                    )}
                </div>
            )}

            {/* Provisional sessions */}
            {recoveryStatus && recoveryStatus.provisional_sessions > 0 && (
                <div className="pnav-dadbear-section">
                    <span className="pnav-dadbear-label">Provisional</span>
                    <span className="pnav-dadbear-text">{recoveryStatus.provisional_sessions} active</span>
                </div>
            )}

            {/* Last check */}
            {autoUpdateStatus?.last_check_at && (
                <div className="pnav-dadbear-section pnav-dadbear-last">
                    <span className="pnav-dadbear-text pnav-text-muted">
                        Checked {relativeTime(autoUpdateStatus.last_check_at)}
                    </span>
                </div>
            )}
        </div>
    );
}

// ── Tree normalization helper ──────────────────────────────────────────

function normalizeTreeNodes(raw: unknown): TreeNode[] {
    if (!raw || !Array.isArray(raw)) return [];

    return raw
        .map((item: any) => normalizeTreeNode(item))
        .filter((n): n is TreeNode => n !== null);
}

function normalizeTreeNode(raw: unknown): TreeNode | null {
    if (!raw || typeof raw !== 'object') return null;
    const r = raw as any;
    if (typeof r.id !== 'string') return null;

    const depth = typeof r.depth === 'number' ? r.depth : Number(r.depth);
    if (!Number.isFinite(depth)) return null;

    return {
        id: r.id,
        depth,
        headline: r.headline ?? r.id,
        distilled: r.distilled ?? r.summary ?? undefined,
        self_prompt: r.self_prompt ?? r.selfPrompt ?? undefined,
        children: Array.isArray(r.children)
            ? r.children.map((c: any) => normalizeTreeNode(c)).filter((n: any): n is TreeNode => n !== null)
            : [],
    };
}
