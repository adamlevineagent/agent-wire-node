import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PromptTab } from './PromptTab';
import { ResponseTab } from './ResponseTab';
import { DetailsTab } from './DetailsTab';
import type { LiveNodeInfo, LlmAuditRecord } from './types';
import type { DrillResultFull } from './inspector-types';

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
    const [drillData, setDrillData] = useState<DrillResultFull | null>(null);

    // Current node info
    const currentNode = allNodes.find(n => n.node_id === nodeId);
    const questionNode = drillData?.question_node ?? null;
    const topLevelQuestion = drillData?.question ?? drillData?.node?.question ?? null;
    const depth = currentNode?.depth ?? questionNode?.visual_depth ?? drillData?.node?.depth ?? 0;
    const headline = currentNode?.question
        || currentNode?.headline
        || questionNode?.question
        || topLevelQuestion
        || drillData?.node?.headline
        || nodeId;

    // Fetch audit records + drill data when node changes
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

    // Latest audit record (most useful)
    const latestAudit = auditRecords.length > 0 ? auditRecords[auditRecords.length - 1] : null;

    // ── Navigation ──────────────────────────────────────────────────────
    const parentIds = currentNode?.parent_ids && currentNode.parent_ids.length > 0
        ? currentNode.parent_ids
        : questionNode?.parent_ids && questionNode.parent_ids.length > 0
            ? questionNode.parent_ids
            : currentNode?.parent_id
                ? [currentNode.parent_id]
                : questionNode?.parent_id
                    ? [questionNode.parent_id]
                    : [];
    const parentId = parentIds[0] ?? null;
    const siblingParentSet = new Set(parentIds);
    const siblings = allNodes.filter(n => {
        if (n.depth !== depth || n.node_id === nodeId) return false;
        const nodeParentIds = n.parent_ids && n.parent_ids.length > 0
            ? n.parent_ids
            : n.parent_id ? [n.parent_id] : [];
        return parentIds.length === 0
            ? nodeParentIds.length === 0
            : nodeParentIds.some(pid => siblingParentSet.has(pid));
    });
    const siblingsWithCurrent = [
        ...siblings,
        currentNode ?? (questionNode ? {
            node_id: questionNode.question_id,
            depth,
            headline,
            parent_id: parentId,
            parent_ids: parentIds,
            children: questionNode.children,
            node_kind: 'question',
            question: questionNode.question,
            status: questionNode.answered ? 'complete' : 'pending',
        } : null),
    ].filter(Boolean) as LiveNodeInfo[];
    siblingsWithCurrent.sort((a, b) => a.node_id.localeCompare(b.node_id));
    const siblingIndex = siblingsWithCurrent.findIndex(n => n.node_id === nodeId);
    const prevSibling = siblingIndex > 0 ? siblingsWithCurrent[siblingIndex - 1] : null;
    const nextSibling = siblingIndex >= 0 && siblingIndex < siblingsWithCurrent.length - 1 ? siblingsWithCurrent[siblingIndex + 1] : null;
    const parent = parentId ? allNodes.find(n => n.node_id === parentId) ?? null : null;
    const liveChildren = allNodes.filter(n => {
        const nodeParentIds = n.parent_ids && n.parent_ids.length > 0
            ? n.parent_ids
            : n.parent_id ? [n.parent_id] : [];
        return nodeParentIds.includes(nodeId);
    });
    const syntheticQuestionChildren: LiveNodeInfo[] = liveChildren.length > 0
        ? []
        : (questionNode?.children ?? []).map((childId) => {
            const known = allNodes.find(n => n.node_id === childId);
            return known ?? {
                node_id: childId,
                depth: Math.max(depth - 1, 0),
                headline: childId,
                parent_id: nodeId,
                parent_ids: [nodeId],
                children: [],
                node_kind: 'question',
                status: 'pending',
            };
        });
    const children = liveChildren.length > 0 ? liveChildren : syntheticQuestionChildren;

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
                            {headline}
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
