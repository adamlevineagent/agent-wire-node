import type { LiveNodeInfo, LlmAuditRecord } from './types';

interface DetailsTabProps {
    drillData: any;
    audit: LlmAuditRecord | null;
    children: LiveNodeInfo[];
    onNavigate: (nodeId: string) => void;
}

export function DetailsTab({ drillData, audit, children, onNavigate }: DetailsTabProps) {
    if (!drillData) {
        return <div className="inspector-empty">No drill data available.</div>;
    }

    const node = drillData.node;
    const evidence = drillData.evidence ?? [];
    const gaps = drillData.gaps ?? [];
    const webEdges = drillData.web_edges ?? [];

    return (
        <div className="details-tab">
            {/* Distilled text — the main content */}
            {node?.distilled && (
                <div className="details-section">
                    <h4>Distilled</h4>
                    <div className="details-distilled">{node.distilled}</div>
                </div>
            )}

            {/* Topics */}
            {node?.topics?.length > 0 && (
                <div className="details-section">
                    <h4>Topics ({node.topics.length})</h4>
                    <div className="details-topics">
                        {node.topics.map((t: any, i: number) => (
                            <div key={i} className="details-topic">
                                <div className="details-topic-name">{t.name}</div>
                                {t.current && <div className="details-topic-current">{t.current}</div>}
                                {t.summary && <div className="details-topic-summary">{t.summary}</div>}
                                {t.entities?.length > 0 && (
                                    <div className="details-topic-entities">
                                        {t.entities.map((e: string, j: number) => (
                                            <span key={j} className="details-entity-tag">{e}</span>
                                        ))}
                                    </div>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Corrections */}
            {node?.corrections?.length > 0 && (
                <div className="details-section">
                    <h4>Corrections ({node.corrections.length})</h4>
                    <ul className="details-corrections">
                        {(typeof node.corrections === 'string'
                            ? [node.corrections]
                            : Array.isArray(node.corrections) ? node.corrections : []
                        ).map((c: any, i: number) => (
                            <li key={i}>{typeof c === 'string' ? c : c.text || c.description || JSON.stringify(c)}</li>
                        ))}
                    </ul>
                </div>
            )}

            {/* Decisions */}
            {node?.decisions?.length > 0 && (
                <div className="details-section">
                    <h4>Decisions ({node.decisions.length})</h4>
                    <ul className="details-decisions">
                        {(typeof node.decisions === 'string'
                            ? [node.decisions]
                            : Array.isArray(node.decisions) ? node.decisions : []
                        ).map((d: any, i: number) => (
                            <li key={i}>{typeof d === 'string' ? d : d.text || d.description || JSON.stringify(d)}</li>
                        ))}
                    </ul>
                </div>
            )}

            {/* Terms */}
            {node?.terms?.length > 0 && (
                <div className="details-section">
                    <h4>Terms ({node.terms.length})</h4>
                    <div className="details-terms">
                        {(typeof node.terms === 'string'
                            ? [node.terms]
                            : Array.isArray(node.terms) ? node.terms : []
                        ).map((t: any, i: number) => (
                            <div key={i} className="details-term">
                                {typeof t === 'string' ? t : (
                                    <>
                                        <span className="details-term-name">{t.name || t.term}</span>
                                        {(t.definition || t.description) && (
                                            <span className="details-term-def">{t.definition || t.description}</span>
                                        )}
                                    </>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Evidence links */}
            {evidence.length > 0 && (
                <div className="details-section">
                    <h4>Evidence ({evidence.length})</h4>
                    <div className="details-evidence-list">
                        {evidence.map((e: any, i: number) => (
                            <div key={i} className={`details-evidence-item verdict-${(e.verdict || 'keep').toLowerCase()}`}>
                                <span className="verdict-badge">{e.verdict || 'KEEP'}</span>
                                <span
                                    className="details-evidence-source clickable"
                                    onClick={() => onNavigate(e.source_node_id || e.target_node_id)}
                                >
                                    {e.source_node_id || e.target_node_id}
                                </span>
                                {e.weight !== undefined && <span className="verdict-weight">{e.weight}</span>}
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Gaps */}
            {gaps.length > 0 && (
                <div className="details-section">
                    <h4>Gaps ({gaps.length})</h4>
                    <ul>
                        {gaps.map((g: any, i: number) => (
                            <li key={i} className={g.resolved ? 'gap-resolved' : ''}>
                                {g.description}
                                {g.resolved && <span className="gap-resolved-badge">resolved</span>}
                            </li>
                        ))}
                    </ul>
                </div>
            )}

            {/* Children */}
            {children.length > 0 && (
                <div className="details-section">
                    <h4>Children ({children.length})</h4>
                    <div className="details-children">
                        {children.map(c => (
                            <div
                                key={c.node_id}
                                className="details-child clickable"
                                onClick={() => onNavigate(c.node_id)}
                            >
                                <span className="details-child-id">{c.node_id}</span>
                                <span className="details-child-headline">{c.headline}</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Web edges */}
            {webEdges.length > 0 && (
                <div className="details-section">
                    <h4>Web Edges ({webEdges.length})</h4>
                    <div className="details-web-edges">
                        {webEdges.map((e: any, i: number) => (
                            <div key={i} className="details-web-edge">
                                <span className="details-edge-target">{e.connected_headline || e.connected_to}</span>
                                <span className="details-edge-rel">{e.relationship}</span>
                                <span className="details-edge-strength">{(e.strength * 100).toFixed(0)}%</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Self-prompt & dead ends */}
            {(node?.self_prompt || node?.dead_ends?.length > 0) && (
                <div className="details-section">
                    <h4>Provenance</h4>
                    {node.self_prompt && (
                        <div className="details-field">
                            <strong>Self-Prompt:</strong> {node.self_prompt}
                        </div>
                    )}
                    {node.dead_ends?.length > 0 && (
                        <div className="details-field">
                            <strong>Dead Ends:</strong>
                            <ul>{node.dead_ends.map((d: string, i: number) => <li key={i}>{d}</li>)}</ul>
                        </div>
                    )}
                </div>
            )}

            {/* Metadata from audit */}
            {audit && (
                <div className="details-section">
                    <h4>LLM Metadata</h4>
                    <table className="details-meta-table">
                        <tbody>
                            <tr><td>Model</td><td>{audit.model}</td></tr>
                            <tr><td>Tokens (in/out)</td><td>{audit.prompt_tokens.toLocaleString()} / {audit.completion_tokens.toLocaleString()}</td></tr>
                            {audit.latency_ms && <tr><td>Latency</td><td>{(audit.latency_ms / 1000).toFixed(1)}s</td></tr>}
                            <tr><td>Step</td><td>{audit.step_name}</td></tr>
                        </tbody>
                    </table>
                </div>
            )}
        </div>
    );
}
