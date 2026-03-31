import { useAppContext } from '../../contexts/AppContext';

export interface AttentionItem {
    type: string;
    severity: 'critical' | 'warning' | 'info';
    message: string;
    agent_pseudo_id?: string;
    agent_name?: string;
    details?: Record<string, unknown>;
}

export interface Recommendation {
    type: string;
    message: string;
    agent_pseudo_id?: string;
    agent_name?: string;
}

interface OperatorAlertsProps {
    attention: AttentionItem[];
    recommendations: Recommendation[];
}

function severityColor(severity: string): string {
    switch (severity) {
        case 'critical': return 'alert-severity-red';
        case 'warning': return 'alert-severity-amber';
        case 'info':
        default: return 'alert-severity-blue';
    }
}

function severityIcon(severity: string): string {
    switch (severity) {
        case 'critical': return '\u26A0\uFE0F';
        case 'warning': return '\u26A1';
        case 'info':
        default: return '\u2139\uFE0F';
    }
}

export function OperatorAlerts({ attention, recommendations }: OperatorAlertsProps) {
    const { setMode } = useAppContext();

    const handleAlertClick = (item: AttentionItem) => {
        // Navigate to fleet; agent detail drawer (Phase 1a) may not exist yet
        setMode('fleet');
    };

    if (attention.length === 0 && recommendations.length === 0) {
        return null;
    }

    return (
        <div className="operator-alerts-container">
            {/* Attention Alerts */}
            {attention.length > 0 && (
                <div className="operator-alerts-section">
                    <div className="operator-alerts-header">
                        <span className="operator-alerts-title">Attention Required</span>
                        <span className="operator-alerts-count">{attention.length}</span>
                    </div>
                    <div className="operator-alerts-list">
                        {attention.map((item, idx) => (
                            <div
                                key={`alert-${idx}`}
                                className={`operator-alert-item ${severityColor(item.severity)}`}
                                onClick={() => handleAlertClick(item)}
                                title={item.agent_pseudo_id ? 'Click to view fleet' : 'Agent detail coming soon'}
                            >
                                <span className="operator-alert-icon">{severityIcon(item.severity)}</span>
                                <div className="operator-alert-content">
                                    <span className="operator-alert-type">{item.type.replace(/_/g, ' ')}</span>
                                    <span className="operator-alert-message">{item.message}</span>
                                    {item.agent_name && (
                                        <span className="operator-alert-agent">{item.agent_name}</span>
                                    )}
                                </div>
                                <span className={`operator-alert-severity-badge ${severityColor(item.severity)}`}>
                                    {item.severity}
                                </span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Recommendations */}
            {recommendations.length > 0 && (
                <div className="operator-recommendations-section">
                    <div className="operator-alerts-header">
                        <span className="operator-alerts-title">Recommendations</span>
                    </div>
                    <div className="operator-recommendations-list">
                        {recommendations.map((rec, idx) => (
                            <div key={`rec-${idx}`} className="operator-recommendation-item">
                                <span className="operator-recommendation-icon">{'\u{1F4A1}'}</span>
                                <div className="operator-recommendation-content">
                                    <span className="operator-recommendation-type">{rec.type.replace(/_/g, ' ')}</span>
                                    <span className="operator-recommendation-message">{rec.message}</span>
                                    {rec.agent_name && (
                                        <span className="operator-recommendation-agent">{rec.agent_name}</span>
                                    )}
                                </div>
                            </div>
                        ))}
                    </div>
                </div>
            )}
        </div>
    );
}
