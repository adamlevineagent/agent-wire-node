import type { PyramidNodeFull } from './inspector-types';

interface ProvenanceSectionProps {
    node: PyramidNodeFull;
}

function formatDate(iso: string): string {
    try {
        return new Date(iso).toLocaleDateString(undefined, {
            year: 'numeric',
            month: 'short',
            day: 'numeric',
            hour: '2-digit',
            minute: '2-digit',
        });
    } catch {
        return iso;
    }
}

export function ProvenanceSection({ node }: ProvenanceSectionProps) {
    const hasSelfPrompt = !!(node.self_prompt);
    const hasBuildId = !!(node.build_id);
    const hasCreatedAt = !!(node.created_at);
    const hasVersion = node.current_version != null;
    const hasChainPhase = !!(node.current_version_chain_phase);
    const hasSuperseded = !!(node.superseded_by);

    const hasAnything =
        hasSelfPrompt || hasBuildId || hasCreatedAt || hasVersion || hasSuperseded;

    if (!hasAnything) {
        return <div className="ni-provenance ni-empty">No provenance data available.</div>;
    }

    return (
        <div className="ni-provenance">
            {/* Self-prompt */}
            {hasSelfPrompt && (
                <div className="ni-provenance-row">
                    <span className="ni-provenance-label">Self-Prompt</span>
                    <p className="ni-self-prompt">{node.self_prompt}</p>
                </div>
            )}

            {/* Build ID */}
            {hasBuildId && (
                <div className="ni-provenance-row">
                    <span className="ni-provenance-label">Build ID</span>
                    <span className="ni-provenance-value mono">{node.build_id}</span>
                </div>
            )}

            {/* Created at */}
            {hasCreatedAt && (
                <div className="ni-provenance-row">
                    <span className="ni-provenance-label">Created</span>
                    <span className="ni-provenance-value">{formatDate(node.created_at)}</span>
                </div>
            )}

            {/* Version */}
            {hasVersion && (
                <div className="ni-provenance-row">
                    <span className="ni-provenance-label">Version</span>
                    <span className="ni-provenance-value">
                        v{node.current_version}
                        {hasChainPhase && (
                            <span className="ni-chain-phase">
                                {' '}(chain phase: {node.current_version_chain_phase})
                            </span>
                        )}
                    </span>
                </div>
            )}

            {/* Superseded by */}
            {hasSuperseded && (
                <div className="ni-provenance-row ni-superseded">
                    <span className="ni-provenance-label">Superseded By</span>
                    <span className="ni-provenance-value mono">{node.superseded_by}</span>
                </div>
            )}
        </div>
    );
}
