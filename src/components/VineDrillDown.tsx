import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

/* ── Types ──────────────────────────────────────────────────────────── */

interface L1Ref {
    id: string;
    headline: string;
}

interface DirectoryNode {
    node_id: string;
    headline: string;
    l1_refs: L1Ref[];
}

interface DrillDirectory {
    directories: DirectoryNode[];
}

interface DrillChild {
    id: string;
    headline: string;
    distilled_text: string;
    bunch_refs: string[];
}

interface DrillNodeDetail {
    node: {
        id: string;
        headline: string;
        distilled_text: string;
    };
    children: DrillChild[];
}

/* ── Props ──────────────────────────────────────────────────────────── */

interface VineDrillDownProps {
    slug: string;
    onNavigateBunch?: (bunchSlug: string) => void;
}

/* ── Component ──────────────────────────────────────────────────────── */

export function VineDrillDown({ slug, onNavigateBunch }: VineDrillDownProps) {
    const [directory, setDirectory] = useState<DrillDirectory | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // Expanded sub-apex card
    const [expandedNodeId, setExpandedNodeId] = useState<string | null>(null);

    // Selected L1 cluster detail
    const [selectedL1, setSelectedL1] = useState<string | null>(null);
    const [l1Detail, setL1Detail] = useState<DrillNodeDetail | null>(null);
    const [l1Loading, setL1Loading] = useState(false);

    const fetchDirectory = useCallback(async () => {
        setLoading(true);
        try {
            const data = await invoke<DrillDirectory>('pyramid_vine_drill', { slug });
            setDirectory(data);
            setError(null);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, [slug]);

    useEffect(() => {
        fetchDirectory();
    }, [fetchDirectory]);

    const toggleSubApex = useCallback((nodeId: string) => {
        setExpandedNodeId(prev => {
            if (prev === nodeId) return null;
            // Reset L1 selection when collapsing/switching
            setSelectedL1(null);
            setL1Detail(null);
            return nodeId;
        });
    }, []);

    const handleSelectL1 = useCallback(async (l1Id: string) => {
        if (selectedL1 === l1Id) {
            setSelectedL1(null);
            setL1Detail(null);
            return;
        }
        setSelectedL1(l1Id);
        setL1Loading(true);
        try {
            const data = await invoke<DrillNodeDetail>('pyramid_drill', { slug, nodeId: l1Id });
            setL1Detail(data);
        } catch (err) {
            setError(String(err));
        } finally {
            setL1Loading(false);
        }
    }, [slug, selectedL1]);

    const handleBunchClick = useCallback((bunchRef: string) => {
        onNavigateBunch?.(bunchRef);
    }, [onNavigateBunch]);

    /* ── Render states ─────────────────────────────────────────────── */

    if (loading) {
        return (
            <div className="vine-drilldown">
                <div className="vine-drilldown-header">
                    <h2>Vine Structure</h2>
                </div>
                <div className="pyramid-loading">Loading vine directory...</div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="vine-drilldown">
                <div className="vine-drilldown-header">
                    <h2>Vine Structure</h2>
                </div>
                <div className="pyramid-error">
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            </div>
        );
    }

    if (!directory || directory.directories.length === 0) {
        return (
            <div className="vine-drilldown">
                <div className="vine-drilldown-header">
                    <h2>Vine Structure</h2>
                </div>
                <div className="vine-drilldown-empty">
                    No vine directory data yet. Build the vine to populate this view.
                </div>
            </div>
        );
    }

    return (
        <div className="vine-drilldown">
            <div className="vine-drilldown-header">
                <h2>Vine Structure</h2>
                <span className="vine-drilldown-meta">
                    {directory.directories.length} sub-apex nodes
                </span>
            </div>

            <div className="vine-drilldown-layout">
                {/* Left panel: sub-apex nodes */}
                <div className="vine-drilldown-nodes">
                    {directory.directories.map(node => (
                        <div
                            key={node.node_id}
                            className={`vine-subapex-card ${expandedNodeId === node.node_id ? 'vine-subapex-card-expanded' : ''}`}
                        >
                            <div
                                className="vine-subapex-card-header"
                                onClick={() => toggleSubApex(node.node_id)}
                            >
                                <span className="vine-subapex-expand">
                                    {expandedNodeId === node.node_id ? '\u25BC' : '\u25B6'}
                                </span>
                                <div className="vine-subapex-card-info">
                                    <h3>{node.headline}</h3>
                                    <span className="vine-subapex-count">
                                        {node.l1_refs.length} cluster{node.l1_refs.length !== 1 ? 's' : ''}
                                    </span>
                                </div>
                            </div>

                            {expandedNodeId === node.node_id && (
                                <div className="vine-subapex-children">
                                    {node.l1_refs.map(l1 => (
                                        <div
                                            key={l1.id}
                                            className={`vine-l1-item ${selectedL1 === l1.id ? 'vine-l1-item-active' : ''}`}
                                            onClick={() => handleSelectL1(l1.id)}
                                        >
                                            <span className="vine-l1-bullet" />
                                            <span className="vine-l1-headline">{l1.headline}</span>
                                        </div>
                                    ))}
                                </div>
                            )}
                        </div>
                    ))}
                </div>

                {/* Right panel: L1 cluster detail */}
                <div className="vine-drilldown-detail">
                    {!selectedL1 && (
                        <div className="vine-drilldown-detail-placeholder">
                            Select a cluster to view its contents
                        </div>
                    )}

                    {selectedL1 && l1Loading && (
                        <div className="pyramid-loading">Loading cluster...</div>
                    )}

                    {selectedL1 && l1Detail && !l1Loading && (
                        <div className="vine-l1-detail">
                            <h3 className="vine-l1-detail-title">{l1Detail.node.headline}</h3>
                            <p className="vine-l1-detail-text">{l1Detail.node.distilled_text}</p>

                            {l1Detail.children.length > 0 && (
                                <div className="vine-l1-bunches">
                                    <span className="vine-l1-bunches-label">Bunches</span>
                                    {l1Detail.children.map(child => (
                                        <div key={child.id} className="vine-drill-bunch-card">
                                            <div className="vine-drill-bunch-card-header">
                                                <span className="vine-drill-bunch-headline">{child.headline}</span>
                                                {child.bunch_refs.map(ref => (
                                                    <button
                                                        key={ref}
                                                        className="vine-bunch-ref-pill"
                                                        onClick={() => handleBunchClick(ref)}
                                                        title={`Navigate to ${ref}`}
                                                    >
                                                        {ref}
                                                    </button>
                                                ))}
                                            </div>
                                            {child.distilled_text && (
                                                <p className="vine-drill-bunch-text">{child.distilled_text}</p>
                                            )}
                                        </div>
                                    ))}
                                </div>
                            )}
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}
