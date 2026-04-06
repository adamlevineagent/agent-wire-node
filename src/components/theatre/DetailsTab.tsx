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

            {/* Metadata */}
            {audit && (
                <div className="details-section">
                    <h4>Metadata</h4>
                    <table className="details-meta-table">
                        <tbody>
                            <tr><td>Model</td><td>{audit.model}</td></tr>
                            <tr><td>Tokens (in/out)</td><td>{audit.prompt_tokens.toLocaleString()} / {audit.completion_tokens.toLocaleString()}</td></tr>
                            {audit.latency_ms && <tr><td>Latency</td><td>{(audit.latency_ms / 1000).toFixed(1)}s</td></tr>}
                            {audit.generation_id && <tr><td>Generation ID</td><td className="mono">{audit.generation_id}</td></tr>}
                            <tr><td>Call Purpose</td><td>{audit.call_purpose}</td></tr>
                            <tr><td>Step</td><td>{audit.step_name}</td></tr>
                        </tbody>
                    </table>
                </div>
            )}

            {/* Node fields from drill */}
            {node && (
                <div className="details-section">
                    <h4>Node Data</h4>
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
        </div>
    );
}
