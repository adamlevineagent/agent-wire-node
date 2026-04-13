import { AccordionSection } from '../AccordionSection';
import type {
    DrillResultFull,
    EvidenceLink,
} from './inspector-types';

interface StructureSectionProps {
    drill: DrillResultFull;
    onNavigate: (nodeId: string) => void;
    /** Tracked open state for nested accordions — persists across navigation */
    openSubs?: Set<string>;
    onSubToggle?: (key: string, open: boolean) => void;
}

function truncate(text: string, max: number): string {
    if (!text || text.length <= max) return text ?? '';
    return text.slice(0, max) + '\u2026';
}

export function StructureSection({ drill, onNavigate, openSubs, onSubToggle }: StructureSectionProps) {
    const isOpen = (key: string, fallback: boolean) =>
        openSubs ? openSubs.has(key) : fallback;
    const handleToggle = (key: string) => (open: boolean) =>
        onSubToggle?.(key, open);
    const children = drill.children ?? [];
    const evidence = drill.evidence ?? [];
    const webEdges = drill.web_edges ?? [];
    const remoteWebEdges = drill.remote_web_edges ?? [];
    const gaps = drill.gaps ?? [];
    const transitions = drill.node?.transitions;
    const hasPrior = !!(transitions?.prior);
    const hasNext = !!(transitions?.next);
    const hasTransitions = hasPrior || hasNext;
    const questionContext = drill.question_context ?? null;
    const hasQuestionContext = !!(
        questionContext?.parent_question ||
        (questionContext?.sibling_questions && questionContext.sibling_questions.length > 0)
    );

    const sectionHasData = [
        children.length > 0,
        evidence.length > 0,
        webEdges.length > 0,
        remoteWebEdges.length > 0,
        hasTransitions,
        hasQuestionContext,
        gaps.length > 0,
    ];
    const firstNonEmptyIndex = sectionHasData.indexOf(true);

    if (firstNonEmptyIndex === -1) {
        return <div className="ni-structure ni-empty">No structure data available.</div>;
    }

    return (
        <div className="ni-structure">
            {/* Children */}
            {children.length > 0 && (
                <AccordionSection
                    key={`children-${isOpen('children', firstNonEmptyIndex === 0)}`}
                    title={`Children (${children.length})`}
                    defaultOpen={isOpen('children', firstNonEmptyIndex === 0)}
                    onToggle={handleToggle('children')}
                >
                    <div className="ni-children-list">
                        {children.map((child) => (
                            <div key={child.id} className="ni-child-item">
                                <span
                                    className="ni-child-headline clickable"
                                    onClick={() => onNavigate(child.id)}
                                >
                                    {child.headline}
                                </span>
                                <span className="ni-depth-badge">L{child.depth}</span>
                                <span className="ni-child-excerpt">
                                    {truncate(child.distilled, 100)}
                                </span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Evidence */}
            {evidence.length > 0 && (
                <AccordionSection
                    key={`evidence-${isOpen('evidence', firstNonEmptyIndex === 1)}`}
                    title={`Evidence (${evidence.length})`}
                    defaultOpen={isOpen('evidence', firstNonEmptyIndex === 1)}
                    onToggle={handleToggle('evidence')}
                >
                    <div className="ni-evidence-list">
                        {evidence.map((link, i) => (
                            <EvidenceLinkRow
                                key={i}
                                link={link}
                                onNavigate={onNavigate}
                            />
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Web Edges */}
            {webEdges.length > 0 && (
                <AccordionSection
                    key={`webedges-${isOpen('webedges', firstNonEmptyIndex === 2)}`}
                    title={`Web Edges (${webEdges.length})`}
                    defaultOpen={isOpen('webedges', firstNonEmptyIndex === 2)}
                    onToggle={handleToggle('webedges')}
                >
                    <div className="ni-web-edges">
                        {webEdges.map((edge, i) => (
                            <div key={i} className="ni-web-edge">
                                <span className="ni-edge-target">
                                    {edge.connected_headline}
                                </span>
                                <span className="ni-edge-rel">{edge.relationship}</span>
                                <span className="ni-edge-strength">
                                    {(edge.strength * 100).toFixed(0)}%
                                </span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Remote Web Edges */}
            {remoteWebEdges.length > 0 && (
                <AccordionSection
                    key={`remoteedges-${isOpen('remoteedges', firstNonEmptyIndex === 3)}`}
                    title={`Remote Web Edges (${remoteWebEdges.length})`}
                    defaultOpen={isOpen('remoteedges', firstNonEmptyIndex === 3)}
                    onToggle={handleToggle('remoteedges')}
                >
                    <div className="ni-remote-web-edges">
                        {remoteWebEdges.map((edge, i) => (
                            <div key={i} className="ni-remote-edge-row">
                                <span className="ni-remote-edge-slug">{edge.remote_slug}</span>
                                <span className="ni-remote-edge-path">{edge.remote_handle_path}</span>
                                <span className="ni-remote-edge-rel">{edge.relationship}</span>
                                <span className="ni-remote-edge-relevance">
                                    {(edge.relevance * 100).toFixed(0)}%
                                </span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Transitions */}
            {hasTransitions && (
                <AccordionSection
                    key={`transitions-${isOpen('transitions', firstNonEmptyIndex === 4)}`}
                    title="Transitions"
                    defaultOpen={isOpen('transitions', firstNonEmptyIndex === 4)}
                    onToggle={handleToggle('transitions')}
                >
                    <div className="ni-transitions">
                        {hasPrior && (
                            <div className="ni-transition">
                                <span className="ni-transition-label">Prior:</span>
                                <span className="ni-transition-text">{transitions!.prior}</span>
                            </div>
                        )}
                        {hasNext && (
                            <div className="ni-transition">
                                <span className="ni-transition-label">Next:</span>
                                <span className="ni-transition-text">{transitions!.next}</span>
                            </div>
                        )}
                    </div>
                </AccordionSection>
            )}

            {/* Question Context */}
            {hasQuestionContext && (
                <AccordionSection
                    key={`questionctx-${isOpen('questionctx', firstNonEmptyIndex === 5)}`}
                    title="Question Context"
                    defaultOpen={isOpen('questionctx', firstNonEmptyIndex === 5)}
                    onToggle={handleToggle('questionctx')}
                >
                    <div className="ni-question-context">
                        {questionContext!.parent_question && (
                            <div className="ni-question-parent">
                                <span className="ni-question-label">Parent Question:</span>
                                <span className="ni-question-text">
                                    {questionContext!.parent_question}
                                </span>
                            </div>
                        )}
                        {questionContext!.sibling_questions &&
                            questionContext!.sibling_questions.length > 0 && (
                            <div className="ni-question-siblings">
                                <span className="ni-question-label">Sibling Questions:</span>
                                <ul className="ni-sibling-list">
                                    {questionContext!.sibling_questions.map((q, i) => (
                                        <li key={i} className="ni-sibling-question">{q}</li>
                                    ))}
                                </ul>
                            </div>
                        )}
                    </div>
                </AccordionSection>
            )}

            {/* Gaps */}
            {gaps.length > 0 && (
                <AccordionSection
                    key={`gaps-${isOpen('gaps', firstNonEmptyIndex === 6)}`}
                    title={`Gaps (${gaps.length})`}
                    defaultOpen={isOpen('gaps', firstNonEmptyIndex === 6)}
                    onToggle={handleToggle('gaps')}
                >
                    <div className="ni-gaps">
                        {gaps.map((gap, i) => (
                            <div
                                key={i}
                                className={`ni-gap-item ${gap.resolved ? 'ni-gap-resolved' : ''}`}
                            >
                                <span className="ni-gap-layer">L{gap.layer}</span>
                                <span className="ni-gap-description">{gap.description}</span>
                                <span className="ni-gap-confidence">
                                    {(gap.resolution_confidence * 100).toFixed(0)}%
                                </span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}
        </div>
    );
}

/** Individual evidence link row */
function EvidenceLinkRow({
    link,
    onNavigate,
}: {
    link: EvidenceLink;
    onNavigate: (nodeId: string) => void;
}) {
    const verdictClass =
        link.verdict === 'KEEP'
            ? 'ni-verdict-keep'
            : link.verdict === 'DISCONNECT'
            ? 'ni-verdict-disconnect'
            : 'ni-verdict-missing';

    return (
        <div className="ni-evidence-item">
            <span className={`ni-verdict-badge ${verdictClass}`}>{link.verdict}</span>
            <span
                className="ni-evidence-source clickable"
                onClick={() => onNavigate(link.source_node_id)}
            >
                {link.source_node_id}
            </span>
            {link.weight != null && (
                <span className="ni-evidence-weight">w: {link.weight}</span>
            )}
            {link.reason && (
                <span className="ni-evidence-reason">{link.reason}</span>
            )}
            <span className={`ni-evidence-status ${link.live ? 'ni-live' : 'ni-superseded'}`}>
                {link.live ? 'live' : 'superseded'}
            </span>
        </div>
    );
}
