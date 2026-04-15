import { useState, useMemo } from 'react';
import type { LlmAuditRecord } from './types';

interface ResponseTabProps {
    audit: LlmAuditRecord | null;
    drillData?: any;
}

/** Render any JSON value as structured content */
function JsonValue({ value, depth = 0 }: { value: unknown; depth?: number }) {
    if (value === null || value === undefined) return null;

    if (typeof value === 'string') {
        // Multi-line strings get pre-wrap treatment
        if (value.includes('\n') && depth > 0) {
            return <div className="response-str response-multiline">{value}</div>;
        }
        return <span className="response-str">{value}</span>;
    }
    if (typeof value === 'number' || typeof value === 'boolean') {
        return <span className="response-primitive">{String(value)}</span>;
    }
    if (Array.isArray(value)) {
        if (value.length === 0) return <span className="response-empty">[]</span>;
        // Array of strings → bullet list
        if (value.every((v) => typeof v === 'string')) {
            return (
                <ul className="response-list">
                    {value.map((item, i) => (
                        <li key={i}>{item}</li>
                    ))}
                </ul>
            );
        }
        // Array of objects → render each as a card
        return (
            <div className="response-array">
                {value.map((item, i) => (
                    <div key={i} className="response-array-item">
                        <JsonValue value={item} depth={depth + 1} />
                    </div>
                ))}
            </div>
        );
    }
    if (typeof value === 'object') {
        const entries = Object.entries(value as Record<string, unknown>);
        if (entries.length === 0) return <span className="response-empty">{'{}'}</span>;
        return (
            <div className={`response-obj ${depth > 0 ? 'response-obj-nested' : ''}`}>
                {entries.map(([k, v]) => (
                    <div key={k} className="response-kv">
                        <span className="response-key">{k.replace(/_/g, ' ')}:</span>
                        <div className="response-val">
                            <JsonValue value={v} depth={depth + 1} />
                        </div>
                    </div>
                ))}
            </div>
        );
    }
    return <span>{String(value)}</span>;
}

export function ResponseTab({ audit, drillData }: ResponseTabProps) {
    const [showRaw, setShowRaw] = useState(false);

    const parsed = useMemo<Record<string, unknown> | null>(() => {
        if (!audit?.raw_response) return null;
        try {
            let text = audit.raw_response;
            if (text.includes('```')) {
                text = text.split('\n').filter(l => !l.trim().startsWith('```')).join('\n');
            }
            const start = text.indexOf('{');
            const end = text.lastIndexOf('}');
            if (start >= 0 && end > start) {
                return JSON.parse(text.slice(start, end + 1));
            }
            return JSON.parse(text);
        } catch {
            return null;
        }
    }, [audit?.raw_response]);

    // If we have audit data, show it (original behavior)
    if (audit && audit.status !== 'pending') {
        if (audit.status === 'failed') {
            return (
                <div className="response-tab">
                    <div className="response-error">
                        LLM call failed: {audit.raw_response}
                    </div>
                </div>
            );
        }

        return (
            <div className="response-tab">
                <div className="response-toggle">
                    <button
                        className={`response-toggle-btn ${!showRaw ? 'active' : ''}`}
                        onClick={() => setShowRaw(false)}
                    >Structured</button>
                    <button
                        className={`response-toggle-btn ${showRaw ? 'active' : ''}`}
                        onClick={() => setShowRaw(true)}
                    >Raw</button>
                </div>

                {showRaw ? (
                    <pre className="response-raw">{audit.raw_response}</pre>
                ) : parsed ? (
                    <div className="response-structured">
                        <JsonValue value={parsed} />
                    </div>
                ) : (
                    <pre className="response-raw">{audit.raw_response}</pre>
                )}
            </div>
        );
    }

    // Fallback: no audit record, but we have drill data — show node content as structured view
    if (drillData?.node) {
        const node = drillData.node;
        // Build a clean object from the node fields that are interesting
        const nodeView: Record<string, unknown> = {};
        if (node.distilled) nodeView.distilled = node.distilled;
        if (node.headline) nodeView.headline = node.headline;
        if (node.topics?.length > 0) nodeView.topics = node.topics;
        if (node.corrections?.length > 0) nodeView.corrections = node.corrections;
        if (node.decisions?.length > 0) nodeView.decisions = node.decisions;
        if (node.terms?.length > 0) nodeView.terms = node.terms;
        if (node.dead_ends?.length > 0) nodeView.dead_ends = node.dead_ends;
        if (node.self_prompt) nodeView.self_prompt = node.self_prompt;

        if (Object.keys(nodeView).length > 0) {
            return (
                <div className="response-tab">
                    <div className="response-source-note">
                        Showing stored node data (LLM audit record not available)
                    </div>
                    <div className="response-structured">
                        <JsonValue value={nodeView} />
                    </div>
                </div>
            );
        }
    }

    if (audit?.status === 'pending') {
        return <div className="inspector-loading">Waiting for LLM response...</div>;
    }

    return <div className="inspector-empty">No response data available.</div>;
}
