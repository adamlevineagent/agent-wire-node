import { useState, useMemo } from 'react';
import type { LlmAuditRecord } from './types';
import type { DrillResultFull } from './inspector-types';

interface ResponseTabProps {
    audit: LlmAuditRecord | null;
    drillData?: DrillResultFull | null;
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

function pruneEmpty(value: unknown): unknown {
    if (value === null || value === undefined || value === '') return undefined;
    if (Array.isArray(value)) {
        const items = value.map(pruneEmpty).filter((item) => item !== undefined);
        return items.length > 0 ? items : undefined;
    }
    if (typeof value === 'object') {
        const entries = Object.entries(value as Record<string, unknown>)
            .map(([key, entry]) => [key, pruneEmpty(entry)] as const)
            .filter(([, entry]) => entry !== undefined);
        return entries.length > 0 ? Object.fromEntries(entries) : undefined;
    }
    return value;
}

function buildStoredRecordView(drillData: DrillResultFull): Record<string, unknown> {
    const node = drillData.node;
    const stored = {
        question_node: drillData.question_node,
        question: drillData.question ?? node?.question,
        question_about: drillData.question_about ?? node?.question_about,
        question_creates: drillData.question_creates ?? node?.question_creates,
        question_prompt_hint: drillData.question_prompt_hint ?? node?.question_prompt_hint,
        answered: drillData.answered ?? node?.answered,
        answer_node_id: drillData.answer_node_id ?? node?.answer_node_id ?? drillData.linked_answer?.id,
        answer_headline: drillData.answer_headline ?? node?.answer_headline ?? drillData.linked_answer?.headline,
        answer_distilled: drillData.answer_distilled ?? node?.answer_distilled ?? drillData.linked_answer?.distilled,
        node,
        linked_answer: drillData.linked_answer,
        children: drillData.children,
        evidence: drillData.evidence,
        gaps: drillData.gaps,
        web_edges: drillData.web_edges,
        remote_web_edges: drillData.remote_web_edges,
        question_context: drillData.question_context,
    };
    return (pruneEmpty(stored) ?? {}) as Record<string, unknown>;
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

    // Fallback: no audit record, but we have drill data — show the full stored record.
    if (drillData?.node) {
        const nodeView = buildStoredRecordView(drillData);

        if (Object.keys(nodeView).length > 0) {
            return (
                <div className="response-tab">
                    <div className="response-source-note">
                        Showing stored record data (LLM audit record not available)
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
