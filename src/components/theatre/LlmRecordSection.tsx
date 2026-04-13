import { useState, useMemo } from 'react';
import { AccordionSection } from '../AccordionSection';
import type { LlmAuditRecord } from './types';

interface LlmRecordSectionProps {
    audit: LlmAuditRecord | null;
    /** Tracked open state for nested accordions — persists across navigation */
    openSubs?: Set<string>;
    onSubToggle?: (key: string, open: boolean) => void;
}

interface ParsedResponse {
    headline?: string;
    distilled?: string;
    topics?: { name: string; current: string }[];
    verdicts?: { node_id: string; verdict: string; weight?: number; reason?: string }[];
    missing?: string[];
}

export function LlmRecordSection({ audit, openSubs, onSubToggle }: LlmRecordSectionProps) {
    const isOpen = (key: string, fallback: boolean) =>
        openSubs ? openSubs.has(key) : fallback;
    const handleToggle = (key: string) => (open: boolean) =>
        onSubToggle?.(key, open);
    const [showRaw, setShowRaw] = useState(false);

    const parsed = useMemo<ParsedResponse | null>(() => {
        if (!audit?.raw_response) return null;
        try {
            let text = audit.raw_response;
            // Strip markdown fences if present
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
        return <div className="ni-llm-record ni-empty">No LLM audit record available.</div>;
    }

    if (audit.status === 'pending') {
        return (
            <div className="ni-llm-record">
                <div className="ni-llm-pending">Waiting for LLM response...</div>
            </div>
        );
    }

    return (
        <div className="ni-llm-record">
            {/* System Prompt */}
            <AccordionSection
                key={`sysprompt-${isOpen('sysprompt', false)}`}
                title={`System Prompt (${audit.system_prompt?.length?.toLocaleString() ?? 0} chars)`}
                defaultOpen={isOpen('sysprompt', false)}
                onToggle={handleToggle('sysprompt')}
            >
                <pre className="ni-prompt-text">{audit.system_prompt}</pre>
            </AccordionSection>

            {/* User Prompt */}
            <AccordionSection
                key={`userprompt-${isOpen('userprompt', true)}`}
                title="User Prompt"
                defaultOpen={isOpen('userprompt', true)}
                onToggle={handleToggle('userprompt')}
            >
                <pre className="ni-prompt-text">{audit.user_prompt}</pre>
            </AccordionSection>

            {/* Response */}
            <AccordionSection
                key={`response-${isOpen('response', true)}`}
                title="Response"
                defaultOpen={isOpen('response', true)}
                onToggle={handleToggle('response')}
            >
                <div className="ni-response">
                    {audit.status === 'failed' ? (
                        <div className="ni-response-error">
                            LLM call failed: {audit.raw_response}
                        </div>
                    ) : (
                        <>
                            <div className="ni-response-toggle">
                                <button
                                    className={`ni-toggle-btn ${!showRaw ? 'active' : ''}`}
                                    onClick={() => setShowRaw(false)}
                                >
                                    Structured
                                </button>
                                <button
                                    className={`ni-toggle-btn ${showRaw ? 'active' : ''}`}
                                    onClick={() => setShowRaw(true)}
                                >
                                    Raw
                                </button>
                            </div>

                            {showRaw ? (
                                <pre className="ni-response-raw">
                                    {audit.raw_response}
                                </pre>
                            ) : parsed ? (
                                <div className="ni-response-structured">
                                    {parsed.headline && (
                                        <div className="ni-response-field">
                                            <strong>Headline:</strong> {parsed.headline}
                                        </div>
                                    )}
                                    {parsed.distilled && (
                                        <div className="ni-response-field">
                                            <strong>Distilled:</strong>
                                            <p>{parsed.distilled}</p>
                                        </div>
                                    )}
                                    {parsed.topics && parsed.topics.length > 0 && (
                                        <div className="ni-response-field">
                                            <strong>Topics:</strong>
                                            <ul>
                                                {parsed.topics.map((t, i) => (
                                                    <li key={i}>
                                                        <em>{t.name}:</em> {t.current}
                                                    </li>
                                                ))}
                                            </ul>
                                        </div>
                                    )}
                                    {parsed.verdicts && parsed.verdicts.length > 0 && (
                                        <div className="ni-response-field">
                                            <strong>Evidence Verdicts:</strong>
                                            <div className="ni-verdict-list">
                                                {parsed.verdicts.map((v, i) => {
                                                    const cls =
                                                        v.verdict === 'KEEP'
                                                            ? 'ni-verdict-keep'
                                                            : v.verdict === 'DISCONNECT'
                                                            ? 'ni-verdict-disconnect'
                                                            : 'ni-verdict-missing';
                                                    return (
                                                        <div key={i} className={`ni-verdict-item ${cls}`}>
                                                            <span className="ni-verdict-badge">{v.verdict}</span>
                                                            <span className="ni-verdict-node">{v.node_id}</span>
                                                            {v.weight !== undefined && (
                                                                <span className="ni-verdict-weight">{v.weight}</span>
                                                            )}
                                                            {v.reason && (
                                                                <span className="ni-verdict-reason">{v.reason}</span>
                                                            )}
                                                        </div>
                                                    );
                                                })}
                                            </div>
                                        </div>
                                    )}
                                    {parsed.missing && parsed.missing.length > 0 && (
                                        <div className="ni-response-field">
                                            <strong>Missing Evidence:</strong>
                                            <ul>
                                                {parsed.missing.map((m, i) => (
                                                    <li key={i}>{m}</li>
                                                ))}
                                            </ul>
                                        </div>
                                    )}
                                </div>
                            ) : (
                                <pre className="ni-response-raw">
                                    {audit.raw_response}
                                </pre>
                            )}
                        </>
                    )}
                </div>
            </AccordionSection>

            {/* Metadata */}
            <AccordionSection
                key={`metadata-${isOpen('metadata', true)}`}
                title="Metadata"
                defaultOpen={isOpen('metadata', true)}
                onToggle={handleToggle('metadata')}
            >
                <table className="ni-meta-table">
                    <tbody>
                        <tr>
                            <td className="ni-meta-label">Model</td>
                            <td className="ni-meta-value">{audit.model}</td>
                        </tr>
                        <tr>
                            <td className="ni-meta-label">Tokens (in/out)</td>
                            <td className="ni-meta-value">
                                {audit.prompt_tokens.toLocaleString()} / {audit.completion_tokens.toLocaleString()}
                            </td>
                        </tr>
                        {audit.latency_ms != null && (
                            <tr>
                                <td className="ni-meta-label">Latency</td>
                                <td className="ni-meta-value">
                                    {(audit.latency_ms / 1000).toFixed(1)}s
                                </td>
                            </tr>
                        )}
                        {audit.generation_id && (
                            <tr>
                                <td className="ni-meta-label">Generation ID</td>
                                <td className="ni-meta-value mono">{audit.generation_id}</td>
                            </tr>
                        )}
                        <tr>
                            <td className="ni-meta-label">Call Purpose</td>
                            <td className="ni-meta-value">{audit.call_purpose}</td>
                        </tr>
                        <tr>
                            <td className="ni-meta-label">Step</td>
                            <td className="ni-meta-value">{audit.step_name}</td>
                        </tr>
                        <tr>
                            <td className="ni-meta-label">Status</td>
                            <td className={`ni-meta-value ni-status-${audit.status}`}>
                                {audit.status}
                            </td>
                        </tr>
                    </tbody>
                </table>
            </AccordionSection>
        </div>
    );
}
