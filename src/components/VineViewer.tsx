import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { VineDrillDown } from './VineDrillDown';
import { VineIntelligence } from './VineIntelligence';

// ── Types ────────────────────────────────────────────────────────────────────

interface VineBunchMetadata {
    topics: string[];
    entities: string[];
    decisions: unknown[];
    corrections: unknown[];
    open_questions: string[];
    penultimate_summaries: string[];
}

interface VineBunch {
    id: number;
    vine_slug: string;
    bunch_slug: string;
    session_id: string;
    jsonl_path: string;
    bunch_index: number;
    first_ts: string | null;
    last_ts: string | null;
    message_count: number | null;
    chunk_count: number | null;
    apex_node_id: string | null;
    penultimate_node_ids: string[];
    status: string;
    metadata: VineBunchMetadata | null;
}

interface EraAnnotation {
    id: number;
    slug: string;
    node_id: string;
    annotation_type: string;
    content: string;
    question_context: string | null;
    author: string;
    created_at: string;
}

interface ParsedEra {
    era_index: number;
    label: string;
    start_bunch_index: number;
    end_bunch_index: number;
    transition_type: string | null;
}

interface ApexNode {
    id: string;
    slug: string;
    depth: number;
    summary: string;
    created_at: string;
}

interface VineViewerProps {
    slug: string;
    nodeCount: number;
    lastBuiltAt: string | null;
    onBack: () => void;
    onOpenBunch: (bunchSlug: string) => void;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function parseEraContent(content: string): ParsedEra | null {
    try {
        const parsed = JSON.parse(content);
        return {
            era_index: parsed.era_index ?? 0,
            label: parsed.label ?? 'Unknown ERA',
            start_bunch_index: parsed.start_bunch_index ?? 0,
            end_bunch_index: parsed.end_bunch_index ?? 0,
            transition_type: parsed.transition_type ?? null,
        };
    } catch {
        return null;
    }
}

function formatDate(dateStr: string | null): string {
    if (!dateStr) return 'Unknown';
    const d = new Date(dateStr);
    return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
}

function formatDuration(firstTs: string | null, lastTs: string | null): string {
    if (!firstTs || !lastTs) return '';
    const diffMs = new Date(lastTs).getTime() - new Date(firstTs).getTime();
    const hours = Math.floor(diffMs / (1000 * 60 * 60));
    const minutes = Math.floor((diffMs % (1000 * 60 * 60)) / (1000 * 60));
    if (hours > 0) return `${hours}h ${minutes}m`;
    return `${minutes}m`;
}

const TRANSITION_COLORS: Record<string, string> = {
    pivot: '#f97316',
    evolution: '#40d080',
    expansion: '#22D3EE',
    refinement: '#A78BFA',
    return: '#facc15',
};

// ── Component ────────────────────────────────────────────────────────────────

type VineViewTab = 'timeline' | 'explore' | 'intelligence';

export function VineViewer({ slug, nodeCount, lastBuiltAt, onBack, onOpenBunch }: VineViewerProps) {
    const [bunches, setBunches] = useState<VineBunch[]>([]);
    const [eras, setEras] = useState<ParsedEra[]>([]);
    const [apex, setApex] = useState<ApexNode | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [apexExpanded, setApexExpanded] = useState(false);
    const [viewTab, setViewTab] = useState<VineViewTab>('timeline');

    const fetchVineData = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const [bunchData, eraData, apexData] = await Promise.all([
                invoke<any>('pyramid_vine_bunches', { slug }),
                invoke<EraAnnotation[]>('pyramid_vine_eras', { slug }),
                invoke<any>('pyramid_apex', { slug }),
            ]);

            setBunches(Array.isArray(bunchData) ? bunchData : []);

            if (eraData) {
                const parsed = eraData
                    .map(a => parseEraContent(a.content))
                    .filter((e): e is ParsedEra => e !== null)
                    .sort((a, b) => a.era_index - b.era_index);
                setEras(parsed);
            }

            if (apexData) {
                setApex(apexData);
            }
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, [slug]);

    useEffect(() => {
        fetchVineData();
    }, [fetchVineData]);

    const handleRebuild = useCallback(async () => {
        try {
            await invoke('pyramid_vine_build', { vineSlug: slug, jsonlDirs: [] });
            // Refresh data after a short delay
            setTimeout(fetchVineData, 2000);
        } catch (err) {
            setError(String(err));
        }
    }, [slug, fetchVineData]);

    // Build a map of bunch_index → era boundaries for rendering
    const eraBoundaries = new Map<number, ParsedEra>();
    for (const era of eras) {
        eraBoundaries.set(era.start_bunch_index, era);
    }

    if (loading) {
        return (
            <div className="vine-viewer">
                <div className="vine-viewer-header">
                    <button className="btn btn-small btn-ghost" onClick={onBack}>← Back</button>
                </div>
                <div className="pyramid-loading">Loading vine data...</div>
            </div>
        );
    }

    return (
        <div className="vine-viewer">
            {/* ── Header ─────────────────────────────────────────── */}
            <div className="vine-viewer-header">
                <div className="vine-viewer-header-left">
                    <button className="btn btn-small btn-ghost" onClick={onBack}>← Back</button>
                    <h2 className="vine-viewer-title">{slug}</h2>
                    <span className="pyramid-card-badge badge-vine">Vine</span>
                </div>
                <div className="vine-viewer-header-right">
                    <div className="vine-viewer-stats">
                        <span className="vine-stat">{bunches.length} bunches</span>
                        <span className="vine-stat-sep">·</span>
                        <span className="vine-stat">{nodeCount} nodes</span>
                        <span className="vine-stat-sep">·</span>
                        <span className="vine-stat">
                            Built {lastBuiltAt ? formatDate(lastBuiltAt) : 'Never'}
                        </span>
                    </div>
                    <div className="vine-viewer-actions">
                        <button className="btn btn-small btn-primary" onClick={handleRebuild}>
                            Rebuild
                        </button>
                    </div>
                </div>
            </div>

            {error && (
                <div className="pyramid-error">
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            )}

            {/* ── View tabs: Timeline / Explore / Intelligence ──── */}
            <div className="vine-viewer-tabs">
                <button
                    className={`vine-viewer-tab ${viewTab === 'timeline' ? 'vine-viewer-tab-active' : ''}`}
                    onClick={() => setViewTab('timeline')}
                >
                    Timeline
                </button>
                <button
                    className={`vine-viewer-tab ${viewTab === 'explore' ? 'vine-viewer-tab-active' : ''}`}
                    onClick={() => setViewTab('explore')}
                >
                    Explore
                </button>
                <button
                    className={`vine-viewer-tab ${viewTab === 'intelligence' ? 'vine-viewer-tab-active' : ''}`}
                    onClick={() => setViewTab('intelligence')}
                >
                    Intelligence
                </button>
            </div>

            {/* ── Tab: Timeline (original view) ────────────────── */}
            {viewTab === 'timeline' && (
                <>
                    {/* Apex Section */}
                    {apex && (
                        <div className="vine-apex-section">
                            <div
                                className="vine-apex-header"
                                onClick={() => setApexExpanded(!apexExpanded)}
                            >
                                <h3>Project Apex</h3>
                                <span className="vine-apex-toggle">{apexExpanded ? '▲' : '▼'}</span>
                            </div>
                            {apexExpanded && (
                                <div className="vine-apex-content">
                                    <p>{apex.summary}</p>
                                </div>
                            )}
                        </div>
                    )}

                    {/* Timeline */}
                    {bunches.length === 0 ? (
                        <div className="pyramid-empty">
                            <h3>No bunches yet</h3>
                            <p>This vine has no conversation bunches. Run a rebuild to process conversations.</p>
                        </div>
                    ) : (
                        <div className="vine-timeline-container">
                            <div className="vine-timeline-scroll">
                                <div className="vine-timeline-line" />
                                <div className="vine-timeline-items">
                                    {bunches.map((bunch, idx) => {
                                        const era = eraBoundaries.get(bunch.bunch_index);
                                        const topics = bunch.metadata?.topics ?? [];
                                        const duration = formatDuration(bunch.first_ts, bunch.last_ts);

                                        return (
                                            <div key={bunch.id} className="vine-timeline-slot">
                                                {/* ERA boundary marker */}
                                                {era && (
                                                    <div className="vine-era-marker">
                                                        <div className="vine-era-line" />
                                                        <span className="vine-era-label">
                                                            ERA {era.era_index + 1}: {era.label}
                                                        </span>
                                                        {era.transition_type && (
                                                            <span
                                                                className="vine-transition-badge"
                                                                style={{
                                                                    background: `${TRANSITION_COLORS[era.transition_type] ?? '#888'}22`,
                                                                    color: TRANSITION_COLORS[era.transition_type] ?? '#888',
                                                                    borderColor: `${TRANSITION_COLORS[era.transition_type] ?? '#888'}44`,
                                                                }}
                                                            >
                                                                {era.transition_type}
                                                            </span>
                                                        )}
                                                    </div>
                                                )}

                                                {/* Bunch card */}
                                                <div
                                                    className="vine-bunch-card"
                                                    onClick={() => onOpenBunch(bunch.bunch_slug)}
                                                    title={`Open ${bunch.bunch_slug} pyramid`}
                                                >
                                                    <div className="vine-bunch-date">
                                                        {formatDate(bunch.first_ts)}
                                                    </div>
                                                    <div className="vine-bunch-meta">
                                                        {bunch.message_count ?? 0} messages
                                                        {duration && ` · ${duration}`}
                                                    </div>
                                                    {topics.length > 0 && (
                                                        <>
                                                            <div className="vine-bunch-divider" />
                                                            <div className="vine-bunch-topics">
                                                                {topics.slice(0, 3).map((t, i) => (
                                                                    <div key={i} className="vine-bunch-topic">
                                                                        {t}
                                                                    </div>
                                                                ))}
                                                            </div>
                                                        </>
                                                    )}
                                                    <div className="vine-bunch-status">
                                                        <span className={`vine-status-dot ${bunch.status === 'built' ? 'built' : 'pending'}`} />
                                                        {bunch.status}
                                                    </div>
                                                </div>
                                            </div>
                                        );
                                    })}
                                </div>
                            </div>
                        </div>
                    )}
                </>
            )}

            {/* ── Tab: Explore (drill-down navigation) ─────────── */}
            {viewTab === 'explore' && (
                <VineDrillDown slug={slug} onNavigateBunch={onOpenBunch} />
            )}

            {/* ── Tab: Intelligence (six intelligence passes) ──── */}
            {viewTab === 'intelligence' && (
                <VineIntelligence slug={slug} onNavigateBunch={onOpenBunch} />
            )}
        </div>
    );
}
