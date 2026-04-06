import { useState, useMemo } from 'react';
import type { LlmAuditRecord } from './types';

interface ResponseTabProps {
    audit: LlmAuditRecord | null;
}

interface ParsedResponse {
    headline?: string;
    distilled?: string;
    topics?: { name: string; current: string }[];
    verdicts?: { node_id: string; verdict: string; weight?: number; reason?: string }[];
    missing?: string[];
    corrections?: any[];
    decisions?: any[];
    terms?: any[];
}

export function ResponseTab({ audit }: ResponseTabProps) {
    const [showRaw, setShowRaw] = useState(false);

    const parsed = useMemo<ParsedResponse | null>(() => {
        if (!audit?.raw_response) return null;
        try {
            // Try to extract JSON from response (may have markdown fences)
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

    if (!audit) {
        return <div className="inspector-empty">No audit record available.</div>;
    }

    if (audit.status === 'pending') {
        return <div className="inspector-loading">Waiting for LLM response...</div>;
    }

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
                    {parsed.headline && (
                        <div className="response-field">
                            <strong>Headline:</strong> {parsed.headline}
                        </div>
                    )}
                    {parsed.distilled && (
                        <div className="response-field">
                            <strong>Distilled:</strong>
                            <p>{parsed.distilled}</p>
                        </div>
                    )}
                    {parsed.topics && parsed.topics.length > 0 && (
                        <div className="response-field">
                            <strong>Topics:</strong>
                            <ul>
                                {parsed.topics.map((t, i) => (
                                    <li key={i}><em>{t.name}:</em> {t.current}</li>
                                ))}
                            </ul>
                        </div>
                    )}
                    {parsed.verdicts && parsed.verdicts.length > 0 && (
                        <div className="response-field">
                            <strong>Evidence Verdicts:</strong>
                            <div className="verdict-list">
                                {parsed.verdicts.map((v, i) => (
                                    <div key={i} className={`verdict-item verdict-${v.verdict.toLowerCase()}`}>
                                        <span className="verdict-badge">{v.verdict}</span>
                                        <span className="verdict-node">{v.node_id}</span>
                                        {v.weight !== undefined && <span className="verdict-weight">{v.weight}</span>}
                                        {v.reason && <span className="verdict-reason">{v.reason}</span>}
                                    </div>
                                ))}
                            </div>
                        </div>
                    )}
                    {parsed.missing && parsed.missing.length > 0 && (
                        <div className="response-field">
                            <strong>Missing Evidence:</strong>
                            <ul>
                                {parsed.missing.map((m, i) => <li key={i}>{m}</li>)}
                            </ul>
                        </div>
                    )}
                </div>
            ) : (
                <pre className="response-raw">{audit.raw_response}</pre>
            )}
        </div>
    );
}
