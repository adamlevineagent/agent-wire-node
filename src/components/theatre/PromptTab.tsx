import { useState } from 'react';
import type { LlmAuditRecord } from './types';

interface PromptTabProps {
    audit: LlmAuditRecord | null;
}

export function PromptTab({ audit }: PromptTabProps) {
    const [systemExpanded, setSystemExpanded] = useState(false);

    if (!audit) {
        return <div className="inspector-empty">No audit record available for this node.</div>;
    }

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
