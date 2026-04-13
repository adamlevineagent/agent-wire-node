import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AccordionSection } from '../AccordionSection';
import { ContentSection } from './ContentSection';
import { StructureSection } from './StructureSection';
import { EpisodicSection } from './EpisodicSection';
import { ProvenanceSection } from './ProvenanceSection';
import { LlmRecordSection } from './LlmRecordSection';
import type { LiveNodeInfo, LlmAuditRecord } from './types';
import type { DrillResultFull } from './inspector-types';

interface NodeInspectorPanelProps {
    slug: string;
    nodeId: string;
    allNodes: LiveNodeInfo[];
    onClose: () => void;
    onNavigate: (nodeId: string) => void;
}

export function NodeInspectorPanel({ slug, nodeId, allNodes, onClose, onNavigate }: NodeInspectorPanelProps) {
    const [auditRecords, setAuditRecords] = useState<LlmAuditRecord[]>([]);
    const [drillData, setDrillData] = useState<DrillResultFull | null>(null);
    const [loading, setLoading] = useState(true);
    // Track which sections are open — persists across node navigation.
    // Keyed by section title. Initialized with Content open.
    const [openSections, setOpenSections] = useState<Set<string>>(() => new Set(['Content']));

    // Current node info
    const currentNode = allNodes.find(n => n.node_id === nodeId);
    const depth = currentNode?.depth ?? 0;

    // ── Fetch audit records + drill data when node changes ──────────────
    useEffect(() => {
        setLoading(true);
        Promise.all([
            invoke<LlmAuditRecord[]>('pyramid_node_audit', { slug, nodeId }).catch(() => []),
            invoke<DrillResultFull>('pyramid_drill', { slug, nodeId }).catch(() => null),
        ]).then(([records, drill]) => {
            setAuditRecords(records);
            setDrillData(drill);
            setLoading(false);
        });
    }, [slug, nodeId]);

    // Latest audit record
    const latestAudit = auditRecords.length > 0 ? auditRecords[auditRecords.length - 1] : null;

    // ── Navigation ──────────────────────────────────────────────────────
    const siblings = allNodes.filter(n => n.depth === depth && n.parent_id === currentNode?.parent_id);
    const siblingIndex = siblings.findIndex(n => n.node_id === nodeId);
    const prevSibling = siblingIndex > 0 ? siblings[siblingIndex - 1] : null;
    const nextSibling = siblingIndex < siblings.length - 1 ? siblings[siblingIndex + 1] : null;
    const parent = currentNode?.parent_id ? allNodes.find(n => n.node_id === currentNode.parent_id) : null;
    const children = allNodes.filter(n => n.parent_id === nodeId);

    // ── Keyboard navigation ─────────────────────────────────────────────
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') { onClose(); return; }
            if (e.key === 'ArrowLeft' && prevSibling) { onNavigate(prevSibling.node_id); return; }
            if (e.key === 'ArrowRight' && nextSibling) { onNavigate(nextSibling.node_id); return; }
            if (e.key === 'ArrowUp' && parent) { onNavigate(parent.node_id); return; }
            if (e.key === 'ArrowDown' && children.length > 0) { onNavigate(children[0].node_id); return; }
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [prevSibling, nextSibling, parent, children, onClose, onNavigate]);

    // ── Section toggle handler (persists across navigation) ───────────
    const handleSectionToggle = useCallback((title: string, open: boolean) => {
        setOpenSections(prev => {
            const next = new Set(prev);
            if (open) next.add(title);
            else next.delete(title);
            return next;
        });
    }, []);

    const allSections = ['Content', 'Structure', 'Episodic', 'Provenance', 'LLM Record'];
    const allExpanded = allSections.every(s => openSections.has(s));

    // ── Expand / Collapse All ───────────────────────────────────────────
    const toggleExpandAll = useCallback(() => {
        setOpenSections(prev => {
            const allOpen = allSections.every(s => prev.has(s));
            return allOpen ? new Set<string>() : new Set(allSections);
        });
    }, []);

    return (
        <div className="ni-panel">
            {/* Header */}
            <div className="ni-header">
                <div className="ni-nav">
                    <button
                        className="ni-nav-btn"
                        disabled={!prevSibling}
                        onClick={() => prevSibling && onNavigate(prevSibling.node_id)}
                        title="Previous sibling (Left arrow)"
                    >&lt;</button>
                    <button
                        className="ni-nav-btn"
                        disabled={!nextSibling}
                        onClick={() => nextSibling && onNavigate(nextSibling.node_id)}
                        title="Next sibling (Right arrow)"
                    >&gt;</button>
                    <button
                        className="ni-nav-btn"
                        disabled={!parent}
                        onClick={() => parent && onNavigate(parent.node_id)}
                        title="Parent (Up arrow)"
                    >^</button>
                    <button
                        className="ni-nav-btn"
                        disabled={children.length === 0}
                        onClick={() => children.length > 0 && onNavigate(children[0].node_id)}
                        title="First child (Down arrow)"
                    >v</button>
                </div>
                <div className="ni-title">
                    <span className="ni-headline">
                        {currentNode?.headline || nodeId}
                    </span>
                    <span className="ni-depth-badge">L{depth}</span>
                </div>
                <div className="ni-header-actions">
                    <button
                        className="ni-expand-toggle"
                        onClick={toggleExpandAll}
                        title={allExpanded ? 'Collapse All' : 'Expand All'}
                    >
                        {allExpanded ? 'Collapse All' : 'Expand All'}
                    </button>
                    <button className="ni-close" onClick={onClose}>X</button>
                </div>
            </div>

            {/* Scrollable section body */}
            <div className="ni-body">
                {loading ? (
                    <div className="ni-loading">Loading...</div>
                ) : (
                    <div className="ni-sections">
                        {/* Content */}
                        {drillData && (
                            <AccordionSection
                                key={`Content-${openSections.has('Content')}`}
                                title="Content"
                                defaultOpen={openSections.has('Content')}
                                onToggle={(open) => handleSectionToggle('Content', open)}
                            >
                                <ContentSection
                                    node={drillData.node}
                                    openSubs={openSections}
                                    onSubToggle={handleSectionToggle}
                                />
                            </AccordionSection>
                        )}

                        {/* Structure */}
                        {drillData && (
                            <AccordionSection
                                key={`Structure-${openSections.has('Structure')}`}
                                title="Structure"
                                defaultOpen={openSections.has('Structure')}
                                onToggle={(open) => handleSectionToggle('Structure', open)}
                            >
                                <StructureSection
                                    drill={drillData}
                                    onNavigate={onNavigate}
                                    openSubs={openSections}
                                    onSubToggle={handleSectionToggle}
                                />
                            </AccordionSection>
                        )}

                        {/* Episodic */}
                        {drillData && (
                            <AccordionSection
                                key={`Episodic-${openSections.has('Episodic')}`}
                                title="Episodic"
                                defaultOpen={openSections.has('Episodic')}
                                onToggle={(open) => handleSectionToggle('Episodic', open)}
                            >
                                <EpisodicSection
                                    node={drillData.node}
                                    openSubs={openSections}
                                    onSubToggle={handleSectionToggle}
                                />
                            </AccordionSection>
                        )}

                        {/* Provenance */}
                        {drillData && (
                            <AccordionSection
                                key={`Provenance-${openSections.has('Provenance')}`}
                                title="Provenance"
                                defaultOpen={openSections.has('Provenance')}
                                onToggle={(open) => handleSectionToggle('Provenance', open)}
                            >
                                <ProvenanceSection node={drillData.node} />
                            </AccordionSection>
                        )}

                        {/* LLM Record */}
                        <AccordionSection
                            key={`LLM Record-${openSections.has('LLM Record')}`}
                            title="LLM Record"
                            defaultOpen={openSections.has('LLM Record')}
                            onToggle={(open) => handleSectionToggle('LLM Record', open)}
                        >
                            <LlmRecordSection
                                audit={latestAudit}
                                openSubs={openSections}
                                onSubToggle={handleSectionToggle}
                            />
                        </AccordionSection>
                    </div>
                )}
            </div>

            {/* Footer with model info */}
            {latestAudit && (
                <div className="ni-footer">
                    <span>Model: {latestAudit.model.split('/').pop()}</span>
                    <span>{latestAudit.prompt_tokens.toLocaleString()} in / {latestAudit.completion_tokens.toLocaleString()} out</span>
                    {latestAudit.latency_ms && <span>{(latestAudit.latency_ms / 1000).toFixed(1)}s</span>}
                </div>
            )}
        </div>
    );
}
