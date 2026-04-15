import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PromptTab } from './PromptTab';
import { ResponseTab } from './ResponseTab';
import { DetailsTab } from './DetailsTab';
import type { LiveNodeInfo, LlmAuditRecord } from './types';

interface NodeInspectorModalProps {
    slug: string;
    nodeId: string;
    allNodes: LiveNodeInfo[];
    onClose: () => void;
    onNavigate: (nodeId: string) => void;
}

type TabId = 'prompt' | 'response' | 'details';

export function NodeInspectorModal({ slug, nodeId, allNodes, onClose, onNavigate }: NodeInspectorModalProps) {
    const [activeTab, setActiveTab] = useState<TabId>('details');
    const [auditRecords, setAuditRecords] = useState<LlmAuditRecord[]>([]);
    const [loading, setLoading] = useState(true);
    const [drillData, setDrillData] = useState<any>(null);

    // Current node info
    const currentNode = allNodes.find(n => n.node_id === nodeId);
    const depth = currentNode?.depth ?? 0;

    // Fetch audit records + drill data when node changes
    useEffect(() => {
        setLoading(true);
        Promise.all([
            invoke<LlmAuditRecord[]>('pyramid_node_audit', { slug, nodeId }).catch(() => []),
            invoke<any>('pyramid_drill', { slug, nodeId }).catch(() => null),
        ]).then(([records, drill]) => {
            setAuditRecords(records);
            setDrillData(drill);
            setLoading(false);
        });
    }, [slug, nodeId]);

    // Latest audit record (most useful)
    const latestAudit = auditRecords.length > 0 ? auditRecords[auditRecords.length - 1] : null;

    // ── Navigation ──────────────────────────────────────────────────────
    const siblings = allNodes.filter(n => n.depth === depth && n.parent_id === currentNode?.parent_id);
    const siblingIndex = siblings.findIndex(n => n.node_id === nodeId);
    const prevSibling = siblingIndex > 0 ? siblings[siblingIndex - 1] : null;
    const nextSibling = siblingIndex < siblings.length - 1 ? siblings[siblingIndex + 1] : null;
    const parent = currentNode?.parent_id ? allNodes.find(n => n.node_id === currentNode.parent_id) : null;
    const children = allNodes.filter(n => n.parent_id === nodeId);

    // Keyboard navigation
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

    return (
        <div className="inspector-overlay" onClick={onClose}>
            <div className="inspector-modal" onClick={(e) => e.stopPropagation()}>
                {/* Header */}
                <div className="inspector-header">
                    <div className="inspector-nav">
                        <button
                            className="inspector-nav-btn"
                            disabled={!prevSibling}
                            onClick={() => prevSibling && onNavigate(prevSibling.node_id)}
                            title="Previous sibling (Left arrow)"
                        >&lt;</button>
                        <button
                            className="inspector-nav-btn"
                            disabled={!nextSibling}
                            onClick={() => nextSibling && onNavigate(nextSibling.node_id)}
                            title="Next sibling (Right arrow)"
                        >&gt;</button>
                        <button
                            className="inspector-nav-btn"
                            disabled={!parent}
                            onClick={() => parent && onNavigate(parent.node_id)}
                            title="Parent (Up arrow)"
                        >^</button>
                        <button
                            className="inspector-nav-btn"
                            disabled={children.length === 0}
                            onClick={() => children.length > 0 && onNavigate(children[0].node_id)}
                            title="First child (Down arrow)"
                        >v</button>
                    </div>
                    <div className="inspector-title">
                        <span className="inspector-headline">
                            {currentNode?.headline || nodeId}
                        </span>
                        <span className="inspector-depth-badge">L{depth}</span>
                    </div>
                    <button className="inspector-close" onClick={onClose}>X</button>
                </div>

                {/* Tab bar */}
                <div className="inspector-tabs">
                    {(['prompt', 'response', 'details'] as TabId[]).map(tab => (
                        <button
                            key={tab}
                            className={`inspector-tab ${activeTab === tab ? 'active' : ''}`}
                            onClick={() => setActiveTab(tab)}
                        >
                            {tab.charAt(0).toUpperCase() + tab.slice(1)}
                        </button>
                    ))}
                </div>

                {/* Tab content */}
                <div className="inspector-content">
                    {loading ? (
                        <div className="inspector-loading">Loading...</div>
                    ) : (
                        <>
                            {activeTab === 'prompt' && <PromptTab audit={latestAudit} drillData={drillData} />}
                            {activeTab === 'response' && <ResponseTab audit={latestAudit} drillData={drillData} />}
                            {activeTab === 'details' && (
                                <DetailsTab
                                    drillData={drillData}
                                    audit={latestAudit}
                                    children={children}
                                    onNavigate={onNavigate}
                                />
                            )}
                        </>
                    )}
                </div>

                {/* Footer with model info */}
                {latestAudit && (
                    <div className="inspector-footer">
                        <span>Model: {latestAudit.model.split('/').pop()}</span>
                        <span>{latestAudit.prompt_tokens.toLocaleString()} in / {latestAudit.completion_tokens.toLocaleString()} out</span>
                        {latestAudit.latency_ms && <span>{(latestAudit.latency_ms / 1000).toFixed(1)}s</span>}
                    </div>
                )}
            </div>
        </div>
    );
}
