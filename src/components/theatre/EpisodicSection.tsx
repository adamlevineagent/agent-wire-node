import { AccordionSection } from '../AccordionSection';
import type { PyramidNodeFull } from './inspector-types';

interface EpisodicSectionProps {
    node: PyramidNodeFull;
    /** Tracked open state for nested accordions — persists across navigation */
    openSubs?: Set<string>;
    onSubToggle?: (key: string, open: boolean) => void;
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

export function EpisodicSection({ node, openSubs, onSubToggle }: EpisodicSectionProps) {
    const isOpen = (key: string, fallback: boolean) =>
        openSubs ? openSubs.has(key) : fallback;
    const handleToggle = (key: string) => (open: boolean) =>
        onSubToggle?.(key, open);
    const entities = node.entities ?? [];
    const timeRange = node.time_range;
    const hasTimeRange = !!(timeRange?.start || timeRange?.end);
    const hasWeightStatus = node.weight != null || node.provisional || node.promoted_from;

    const sectionHasData = [
        entities.length > 0,
        hasTimeRange,
        !!hasWeightStatus,
    ];
    const firstNonEmptyIndex = sectionHasData.indexOf(true);

    if (firstNonEmptyIndex === -1) {
        return <div className="ni-episodic ni-empty">No episodic data available.</div>;
    }

    return (
        <div className="ni-episodic">
            {/* Entities */}
            {entities.length > 0 && (
                <AccordionSection
                    key={`entities-${isOpen('entities', firstNonEmptyIndex === 0)}`}
                    title={`Entities (${entities.length})`}
                    defaultOpen={isOpen('entities', firstNonEmptyIndex === 0)}
                    onToggle={handleToggle('entities')}
                >
                    <div>
                        {entities.map((entity, i) => (
                            <div key={i} className="ni-entity-row">
                                <span className="ni-entity-name">{entity.name}</span>
                                <span className="ni-entity-role">{entity.role}</span>
                                <div
                                    className="ni-importance-bar"
                                    style={{ '--importance': `${(entity.importance * 100).toFixed(0)}%` } as React.CSSProperties}
                                />
                                <span
                                    className="ni-liveness"
                                    data-status={entity.liveness?.toLowerCase() ?? 'unknown'}
                                >
                                    {entity.liveness}
                                </span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Temporal */}
            {hasTimeRange && (
                <AccordionSection
                    key={`temporal-${isOpen('temporal', firstNonEmptyIndex === 1)}`}
                    title="Temporal"
                    defaultOpen={isOpen('temporal', firstNonEmptyIndex === 1)}
                    onToggle={handleToggle('temporal')}
                >
                    <div className="ni-temporal">
                        {timeRange?.start && (
                            <div className="ni-temporal-field">
                                <span className="ni-temporal-label">Start:</span>
                                <span className="ni-temporal-value">
                                    {formatDate(timeRange.start)}
                                </span>
                            </div>
                        )}
                        {timeRange?.end && (
                            <div className="ni-temporal-field">
                                <span className="ni-temporal-label">End:</span>
                                <span className="ni-temporal-value">
                                    {formatDate(timeRange.end)}
                                </span>
                            </div>
                        )}
                    </div>
                </AccordionSection>
            )}

            {/* Weight & Status */}
            {hasWeightStatus && (
                <AccordionSection
                    key={`weightstatus-${isOpen('weightstatus', firstNonEmptyIndex === 2)}`}
                    title="Weight & Status"
                    defaultOpen={isOpen('weightstatus', firstNonEmptyIndex === 2)}
                    onToggle={handleToggle('weightstatus')}
                >
                    <div className="ni-weight-status">
                        {node.weight != null && (
                            <div className="ni-weight-field">
                                <span className="ni-weight-label">Weight:</span>
                                <span className="ni-weight-value">{node.weight}</span>
                            </div>
                        )}
                        {node.provisional && (
                            <span className="ni-provisional-badge">Provisional</span>
                        )}
                        {node.promoted_from && (
                            <div className="ni-promoted-field">
                                <span className="ni-promoted-label">Promoted from:</span>
                                <span className="ni-promoted-value">{node.promoted_from}</span>
                            </div>
                        )}
                    </div>
                </AccordionSection>
            )}
        </div>
    );
}
