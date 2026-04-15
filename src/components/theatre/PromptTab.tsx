import { useState } from 'react';
import type { LlmAuditRecord } from './types';

interface PromptTabProps {
    audit: LlmAuditRecord | null;
    drillData?: any;
}

export function PromptTab({ audit, drillData }: PromptTabProps) {
    const [systemExpanded, setSystemExpanded] = useState(false);

    if (audit) {
        const isInFlight = audit.status === 'pending';

        return (
            <div className="prompt-tab">
                {/* System prompt — collapsible */}
                <div className="prompt-section">
                    <div
                        className="prompt-section-header"
                        onClick={() => setSystemExpanded(!systemExpanded)}
                    >
                        <span>{systemExpanded ? '▼' : '▶'} System Prompt</span>
                        <span className="prompt-char-count">{audit.system_prompt.length.toLocaleString()} chars</span>
                    </div>
                    {systemExpanded && (
                        <pre className="prompt-content">{audit.system_prompt}</pre>
                    )}
                </div>

                {/* User prompt — always visible */}
                <div className="prompt-section">
                    <div className="prompt-section-header">
                        <span>User Prompt</span>
                        <span className="prompt-char-count">{audit.user_prompt.length.toLocaleString()} chars</span>
                    </div>
                    <pre className="prompt-content">{audit.user_prompt}</pre>
                </div>

                {/* In-flight indicator */}
                {isInFlight && (
                    <div className="prompt-inflight">
                        Waiting for LLM response...
                    </div>
                )}
            </div>
        );
    }

    // Fallback: no audit record — show what we know from the node
    const node = drillData?.node;
    return (
        <div className="prompt-tab">
            <div className="response-source-note">
                LLM prompt data is only available during or immediately after a build.
                Audit records are cleaned up when a new build starts.
            </div>
            {node?.self_prompt && (
                <div className="prompt-section">
                    <div className="prompt-section-header">
                        <span>Self-Prompt (from node)</span>
                    </div>
                    <pre className="prompt-content">{node.self_prompt}</pre>
                </div>
            )}
        </div>
    );
}
