import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';

/* ═══════════════════════════════════════════════════════════════════
   Types for each intelligence tab
   ═══════════════════════════════════════════════════════════════════ */

interface EraEntry {
    id: string;
    label: string;
    date_range: { start: string; end: string } | null;
    dominant_topics: string[];
    narrative_summary: string;
    bunch_refs: string[];
}

interface DecisionEntry {
    id: string;
    question: string;
    answer: string;
    evolution_chain: string[];
}

interface EntityEntry {
    id: string;
    canonical_name: string;
    aliases: string[];
    bunch_refs: string[];
}

interface ThreadEntry {
    id: string;
    name: string;
    bunch_span: string[];
    lifecycle: 'active' | 'dormant' | 'resolved';
    web_edges: { target_thread_id: string; target_thread_name: string }[];
}

interface CorrectionEntry {
    id: string;
    before: string;
    after: string;
    bunch_refs: string[];
}

interface IntegrityResult {
    orphans: number;
    broken_refs: number;
    unclustered_l0: number;
    unreachable_nodes: number;
}

/* ═══════════════════════════════════════════════════════════════════ */

type IntelTab = 'eras' | 'decisions' | 'entities' | 'threads' | 'corrections' | 'integrity';

interface VineIntelligenceProps {
    slug: string;
    onHighlightBunches?: (bunchRefs: string[]) => void;
    onNavigateBunch?: (bunchSlug: string) => void;
}

export function VineIntelligence({ slug, onHighlightBunches, onNavigateBunch }: VineIntelligenceProps) {
    const [activeTab, setActiveTab] = useState<IntelTab>('eras');

    const tabs: { key: IntelTab; label: string }[] = [
        { key: 'eras', label: 'ERAs' },
        { key: 'decisions', label: 'Decisions' },
        { key: 'entities', label: 'Entities' },
        { key: 'threads', label: 'Threads' },
        { key: 'corrections', label: 'Corrections' },
        { key: 'integrity', label: 'Integrity' },
    ];

    return (
        <div className="vine-intel">
            {/* Tab bar */}
            <div className="vine-intel-tabbar">
                {tabs.map(t => (
                    <button
                        key={t.key}
                        className={`vine-intel-tab ${activeTab === t.key ? 'vine-intel-tab-active' : ''}`}
                        onClick={() => setActiveTab(t.key)}
                    >
                        {t.label}
                    </button>
                ))}
            </div>

            {/* Tab content — lazy loaded on activation */}
            <div className="vine-intel-content">
                {activeTab === 'eras' && (
                    <ErasTab slug={slug} onHighlightBunches={onHighlightBunches} />
                )}
                {activeTab === 'decisions' && (
                    <DecisionsTab slug={slug} onNavigateBunch={onNavigateBunch} />
                )}
                {activeTab === 'entities' && (
                    <EntitiesTab slug={slug} onNavigateBunch={onNavigateBunch} />
                )}
                {activeTab === 'threads' && (
                    <ThreadsTab slug={slug} onNavigateBunch={onNavigateBunch} />
                )}
                {activeTab === 'corrections' && (
                    <CorrectionsTab slug={slug} onNavigateBunch={onNavigateBunch} />
                )}
                {activeTab === 'integrity' && (
                    <IntegrityTab slug={slug} />
                )}
            </div>
        </div>
    );
}

/* ── Shared hook for lazy-loaded tab data via Tauri invoke ──────────── */

function useTabData<T>(slug: string, command: string) {
    const [data, setData] = useState<T | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        invoke<T>(command, { slug })
            .then(result => {
                if (!cancelled) {
                    setData(result);
                    setError(null);
                }
            })
            .catch(err => {
                if (!cancelled) setError(String(err));
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });
        return () => { cancelled = true; };
    }, [slug, command]);

    return { data, loading, error, setError };
}

/* ── Tab 1: ERAs ───────────────────────────────────────────────────── */

function ErasTab({ slug, onHighlightBunches }: {
    slug: string;
    onHighlightBunches?: (refs: string[]) => void;
}) {
    const { data, loading, error, setError } = useTabData<EraEntry[]>(slug, 'pyramid_vine_eras');

    if (loading) return <div className="pyramid-loading">Loading ERAs...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data || data.length === 0) return <TabEmpty label="ERAs" />;

    return (
        <div className="vine-intel-list">
            {data.map(era => (
                <div
                    key={era.id}
                    className="vine-intel-era-card"
                    onClick={() => onHighlightBunches?.(era.bunch_refs)}
                >
                    <div className="vine-intel-era-header">
                        <span className="vine-intel-era-label">{era.label}</span>
                        {era.date_range && (
                            <span className="vine-intel-era-date">
                                {era.date_range.start} &ndash; {era.date_range.end}
                            </span>
                        )}
                    </div>
                    {era.dominant_topics.length > 0 && (
                        <div className="vine-intel-era-topics">
                            {era.dominant_topics.map((topic, i) => (
                                <span key={i} className="vine-topic-pill">{topic}</span>
                            ))}
                        </div>
                    )}
                    <p className="vine-intel-era-narrative">{era.narrative_summary}</p>
                    {era.bunch_refs.length > 0 && (
                        <div className="vine-intel-era-refs">
                            {era.bunch_refs.map(ref => (
                                <span key={ref} className="vine-ref-tag">{ref}</span>
                            ))}
                        </div>
                    )}
                </div>
            ))}
        </div>
    );
}

/* ── Tab 2: Decisions ──────────────────────────────────────────────── */

function DecisionsTab({ slug, onNavigateBunch }: {
    slug: string;
    onNavigateBunch?: (ref: string) => void;
}) {
    const { data, loading, error, setError } = useTabData<DecisionEntry[]>(slug, 'pyramid_vine_decisions');
    const [searchQuery, setSearchQuery] = useState('');
    const [expandedId, setExpandedId] = useState<string | null>(null);

    const filtered = useMemo(() => {
        if (!data) return [];
        const q = searchQuery.toLowerCase().trim();
        if (!q) return data;
        return data.filter(d =>
            d.question.toLowerCase().includes(q) ||
            d.answer.toLowerCase().includes(q)
        );
    }, [data, searchQuery]);

    if (loading) return <div className="pyramid-loading">Loading decisions...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data || data.length === 0) return <TabEmpty label="Decisions" />;

    return (
        <div className="vine-intel-list">
            <div className="vine-intel-search">
                <input
                    type="text"
                    placeholder="Search decisions..."
                    value={searchQuery}
                    onChange={e => setSearchQuery(e.target.value)}
                    className="vine-intel-search-input"
                />
            </div>
            {filtered.map(d => (
                <div
                    key={d.id}
                    className={`vine-decision-entry ${expandedId === d.id ? 'vine-decision-entry-expanded' : ''}`}
                >
                    <div
                        className="vine-decision-header"
                        onClick={() => setExpandedId(prev => prev === d.id ? null : d.id)}
                    >
                        <span className="vine-decision-expand">
                            {expandedId === d.id ? '\u25BC' : '\u25B6'}
                        </span>
                        <span className="vine-decision-question">{d.question}</span>
                    </div>
                    {expandedId === d.id && (
                        <div className="vine-decision-body">
                            <p className="vine-decision-answer">{d.answer}</p>
                            {d.evolution_chain.length > 0 && (
                                <div className="vine-decision-chain">
                                    <span className="vine-intel-label">Evolution:</span>
                                    {d.evolution_chain.map(ref => (
                                        <button
                                            key={ref}
                                            className="vine-bunch-ref-pill"
                                            onClick={() => onNavigateBunch?.(ref)}
                                        >
                                            {ref}
                                        </button>
                                    ))}
                                </div>
                            )}
                        </div>
                    )}
                </div>
            ))}
            {filtered.length === 0 && searchQuery && (
                <div className="vine-intel-empty">No decisions match &ldquo;{searchQuery}&rdquo;</div>
            )}
        </div>
    );
}

/* ── Tab 3: Entities ───────────────────────────────────────────────── */

function EntitiesTab({ slug, onNavigateBunch }: {
    slug: string;
    onNavigateBunch?: (ref: string) => void;
}) {
    const { data, loading, error, setError } = useTabData<EntityEntry[]>(slug, 'pyramid_vine_entities');
    const [searchQuery, setSearchQuery] = useState('');

    const filtered = useMemo(() => {
        if (!data) return [];
        const q = searchQuery.toLowerCase().trim();
        if (!q) return data;
        return data.filter(e =>
            e.canonical_name.toLowerCase().includes(q) ||
            e.aliases.some(a => a.toLowerCase().includes(q))
        );
    }, [data, searchQuery]);

    if (loading) return <div className="pyramid-loading">Loading entities...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data || data.length === 0) return <TabEmpty label="Entities" />;

    return (
        <div className="vine-intel-list">
            <div className="vine-intel-search">
                <input
                    type="text"
                    placeholder="Search entities..."
                    value={searchQuery}
                    onChange={e => setSearchQuery(e.target.value)}
                    className="vine-intel-search-input"
                />
            </div>
            {filtered.map(ent => (
                <div key={ent.id} className="vine-entity-card">
                    <div className="vine-entity-header">
                        <span className="vine-entity-name">{ent.canonical_name}</span>
                        <span className="vine-entity-alias-count">
                            {ent.aliases.length} alias{ent.aliases.length !== 1 ? 'es' : ''}
                        </span>
                    </div>
                    {ent.aliases.length > 0 && (
                        <div className="vine-entity-aliases">
                            {ent.aliases.map((a, i) => (
                                <span key={i} className="vine-alias-pill">{a}</span>
                            ))}
                        </div>
                    )}
                    {ent.bunch_refs.length > 0 && (
                        <div className="vine-entity-refs">
                            <span className="vine-intel-label">Referenced in:</span>
                            {ent.bunch_refs.map(ref => (
                                <button
                                    key={ref}
                                    className="vine-bunch-ref-pill"
                                    onClick={() => onNavigateBunch?.(ref)}
                                >
                                    {ref}
                                </button>
                            ))}
                        </div>
                    )}
                </div>
            ))}
            {filtered.length === 0 && searchQuery && (
                <div className="vine-intel-empty">No entities match &ldquo;{searchQuery}&rdquo;</div>
            )}
        </div>
    );
}

/* ── Tab 4: Threads ────────────────────────────────────────────────── */

function ThreadsTab({ slug, onNavigateBunch }: {
    slug: string;
    onNavigateBunch?: (ref: string) => void;
}) {
    const { data, loading, error, setError } = useTabData<ThreadEntry[]>(slug, 'pyramid_vine_threads');

    if (loading) return <div className="pyramid-loading">Loading threads...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data || data.length === 0) return <TabEmpty label="Threads" />;

    const lifecycleClass = (lc: string) => {
        if (lc === 'active') return 'vine-lifecycle-active';
        if (lc === 'dormant') return 'vine-lifecycle-dormant';
        return 'vine-lifecycle-resolved';
    };

    return (
        <div className="vine-intel-list">
            {data.map(thread => (
                <div key={thread.id} className="vine-thread-card">
                    <div className="vine-thread-header">
                        <span className="vine-thread-name">{thread.name}</span>
                        <span className={`vine-lifecycle-pill ${lifecycleClass(thread.lifecycle)}`}>
                            {thread.lifecycle}
                        </span>
                    </div>
                    {thread.bunch_span.length > 0 && (
                        <div className="vine-thread-span">
                            <span className="vine-intel-label">Spans:</span>
                            {thread.bunch_span.map(ref => (
                                <button
                                    key={ref}
                                    className="vine-bunch-ref-pill"
                                    onClick={() => onNavigateBunch?.(ref)}
                                >
                                    {ref}
                                </button>
                            ))}
                        </div>
                    )}
                    {thread.web_edges.length > 0 && (
                        <div className="vine-thread-edges">
                            <span className="vine-intel-label">Connected to:</span>
                            {thread.web_edges.map(edge => (
                                <span key={edge.target_thread_id} className="vine-edge-pill">
                                    {edge.target_thread_name}
                                </span>
                            ))}
                        </div>
                    )}
                </div>
            ))}
        </div>
    );
}

/* ── Tab 5: Corrections ────────────────────────────────────────────── */

function CorrectionsTab({ slug, onNavigateBunch }: {
    slug: string;
    onNavigateBunch?: (ref: string) => void;
}) {
    const { data, loading, error, setError } = useTabData<CorrectionEntry[]>(slug, 'pyramid_vine_corrections');

    if (loading) return <div className="pyramid-loading">Loading corrections...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data || data.length === 0) return <TabEmpty label="Corrections" />;

    return (
        <div className="vine-intel-list">
            {data.map(corr => (
                <div key={corr.id} className="vine-correction-card">
                    <div className="vine-correction-diff">
                        <div className="vine-correction-before">
                            <span className="vine-correction-label">Before</span>
                            <p>{corr.before}</p>
                        </div>
                        <div className="vine-correction-arrow">&rarr;</div>
                        <div className="vine-correction-after">
                            <span className="vine-correction-label">After</span>
                            <p>{corr.after}</p>
                        </div>
                    </div>
                    {corr.bunch_refs.length > 0 && (
                        <div className="vine-correction-refs">
                            <span className="vine-intel-label">Bunches:</span>
                            {corr.bunch_refs.map(ref => (
                                <button
                                    key={ref}
                                    className="vine-bunch-ref-pill"
                                    onClick={() => onNavigateBunch?.(ref)}
                                >
                                    {ref}
                                </button>
                            ))}
                        </div>
                    )}
                </div>
            ))}
        </div>
    );
}

/* ── Tab 6: Integrity ──────────────────────────────────────────────── */

function IntegrityTab({ slug }: { slug: string }) {
    const { data, loading, error, setError } = useTabData<IntegrityResult>(slug, 'pyramid_vine_integrity');

    if (loading) return <div className="pyramid-loading">Running integrity check...</div>;
    if (error) return <TabError message={error} onDismiss={() => setError(null)} />;
    if (!data) return <TabEmpty label="Integrity" />;

    const allClear = data.orphans === 0
        && data.broken_refs === 0
        && data.unclustered_l0 === 0
        && data.unreachable_nodes === 0;

    const checks = [
        { label: 'Orphan nodes', value: data.orphans },
        { label: 'Broken references', value: data.broken_refs },
        { label: 'Unclustered L0 nodes', value: data.unclustered_l0 },
        { label: 'Unreachable nodes', value: data.unreachable_nodes },
    ];

    return (
        <div className="vine-intel-list">
            <div className="vine-integrity-status">
                {allClear ? (
                    <div className="vine-integrity-ok">
                        <span className="vine-integrity-icon vine-integrity-icon-ok">&#x2713;</span>
                        <span>All clear &mdash; no issues detected</span>
                    </div>
                ) : (
                    <div className="vine-integrity-warn">
                        <span className="vine-integrity-icon vine-integrity-icon-warn">&#x26A0;</span>
                        <span>Issues detected</span>
                    </div>
                )}
            </div>
            <div className="vine-integrity-checks">
                {checks.map(c => (
                    <div key={c.label} className="vine-integrity-row">
                        <span className="vine-integrity-check-label">{c.label}</span>
                        <span className={`vine-integrity-check-value ${c.value > 0 ? 'vine-integrity-check-warn' : ''}`}>
                            {c.value}
                        </span>
                    </div>
                ))}
            </div>
        </div>
    );
}

/* ── Shared small components ───────────────────────────────────────── */

function TabError({ message, onDismiss }: { message: string; onDismiss: () => void }) {
    return (
        <div className="pyramid-error">
            {message}
            <button className="workspace-error-dismiss" onClick={onDismiss}>Dismiss</button>
        </div>
    );
}

function TabEmpty({ label }: { label: string }) {
    return (
        <div className="vine-intel-empty">
            No {label.toLowerCase()} data yet. Build the vine intelligence passes to populate this view.
        </div>
    );
}
