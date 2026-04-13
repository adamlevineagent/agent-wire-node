import { AccordionSection } from '../AccordionSection';
import type { PyramidNodeFull } from './inspector-types';

interface ContentSectionProps {
    node: PyramidNodeFull;
    /** Tracked open state for nested accordions — persists across navigation */
    openSubs?: Set<string>;
    onSubToggle?: (key: string, open: boolean) => void;
}

export function ContentSection({ node, openSubs, onSubToggle }: ContentSectionProps) {
    const isOpen = (key: string, fallback: boolean) =>
        openSubs ? openSubs.has(key) : fallback;
    const handleToggle = (key: string) => (open: boolean) =>
        onSubToggle?.(key, open);
    const hasDistilled = !!(node.distilled);
    const narrativeLevels = [...(node.narrative?.levels ?? [])].sort(
        (a, b) => a.zoom - b.zoom
    );
    const topics = node.topics ?? [];
    const corrections = node.corrections ?? [];
    const decisions = node.decisions ?? [];
    const terms = node.terms ?? [];
    const keyQuotes = node.key_quotes ?? [];
    const deadEnds = node.dead_ends ?? [];

    // Track which is the first non-empty section so we can default-open it
    const sectionHasData = [
        hasDistilled,
        narrativeLevels.length > 0,
        topics.length > 0,
        corrections.length > 0,
        decisions.length > 0,
        terms.length > 0,
        keyQuotes.length > 0,
        deadEnds.length > 0,
    ];
    const firstNonEmptyIndex = sectionHasData.indexOf(true);

    const allEmpty = firstNonEmptyIndex === -1;
    if (allEmpty) {
        return <div className="ni-content ni-empty">No content data available.</div>;
    }

    return (
        <div className="ni-content">
            {/* Distilled summary */}
            {hasDistilled && (
                <div className="ni-distilled">
                    <p className="ni-distilled-text">{node.distilled}</p>
                </div>
            )}

            {/* Narrative */}
            {narrativeLevels.length > 0 && (
                <AccordionSection
                    key={`narrative-${isOpen('narrative', firstNonEmptyIndex === 1)}`}
                    title={`Narrative (${narrativeLevels.length} level${narrativeLevels.length !== 1 ? 's' : ''})`}
                    defaultOpen={isOpen('narrative', firstNonEmptyIndex === 1)}
                    onToggle={handleToggle('narrative')}
                >
                    <div className="ni-narrative">
                        {narrativeLevels.map((level, i) => (
                            <div key={i} className="ni-narrative-level">
                                <span className="ni-narrative-zoom">Zoom {level.zoom}</span>
                                <p className="ni-narrative-text">{level.text}</p>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Topics */}
            {topics.length > 0 && (
                <AccordionSection
                    key={`topics-${isOpen('topics', firstNonEmptyIndex === 2)}`}
                    title={`Topics (${topics.length})`}
                    defaultOpen={isOpen('topics', firstNonEmptyIndex === 2)}
                    onToggle={handleToggle('topics')}
                >
                    <div className="ni-topics">
                        {topics.map((topic, i) => (
                            <div key={i} className="ni-topic">
                                <div className="ni-topic-header">
                                    <span className="ni-topic-name">{topic.name}</span>
                                </div>
                                <p className="ni-topic-current">{topic.current}</p>

                                {/* Topic corrections */}
                                {topic.corrections && topic.corrections.length > 0 && (
                                    <div className="ni-topic-corrections">
                                        <span className="ni-topic-sub-label">Corrections:</span>
                                        {topic.corrections.map((c, ci) => (
                                            <div key={ci} className="ni-correction">
                                                <span className="ni-correction-wrong">{c.wrong}</span>
                                                <span className="ni-correction-arrow">&rarr;</span>
                                                <span className="ni-correction-right">{c.right}</span>
                                                <span className="ni-correction-who">({c.who})</span>
                                            </div>
                                        ))}
                                    </div>
                                )}

                                {/* Topic decisions */}
                                {topic.decisions && topic.decisions.length > 0 && (
                                    <div className="ni-topic-decisions">
                                        <span className="ni-topic-sub-label">Decisions:</span>
                                        {topic.decisions.map((d, di) => (
                                            <div key={di} className="ni-decision">
                                                <span className="ni-decision-text">{d.decided}</span>
                                                <span className="ni-decision-why">{d.why}</span>
                                                <span className="ni-stance-badge">
                                                    {d.stance}
                                                </span>
                                                <span className="ni-decision-importance">
                                                    imp: {d.importance}
                                                </span>
                                            </div>
                                        ))}
                                    </div>
                                )}

                                {/* Topic entities as inline tags */}
                                {topic.entities && topic.entities.length > 0 && (
                                    <div className="ni-topic-entities">
                                        {topic.entities.map((e, ei) => (
                                            <span key={ei} className="ni-entity-tag">{e}</span>
                                        ))}
                                    </div>
                                )}
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Node-level Corrections */}
            {corrections.length > 0 && (
                <AccordionSection
                    key={`corrections-${isOpen('corrections', firstNonEmptyIndex === 3)}`}
                    title={`Corrections (${corrections.length})`}
                    defaultOpen={isOpen('corrections', firstNonEmptyIndex === 3)}
                    onToggle={handleToggle('corrections')}
                >
                    <div className="ni-topic-corrections">
                        {corrections.map((c, ci) => (
                            <div key={ci} className="ni-correction">
                                <span className="ni-correction-wrong">{c.wrong}</span>
                                <span className="ni-correction-arrow">&rarr;</span>
                                <span className="ni-correction-right">{c.right}</span>
                                <span className="ni-correction-who">({c.who})</span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Node-level Decisions */}
            {decisions.length > 0 && (
                <AccordionSection
                    key={`decisions-${isOpen('decisions', firstNonEmptyIndex === 4)}`}
                    title={`Decisions (${decisions.length})`}
                    defaultOpen={isOpen('decisions', firstNonEmptyIndex === 4)}
                    onToggle={handleToggle('decisions')}
                >
                    <div className="ni-topic-decisions">
                        {decisions.map((d, di) => (
                            <div key={di} className="ni-decision">
                                <span className="ni-decision-text">{d.decided}</span>
                                <span className="ni-decision-why">{d.why}</span>
                                <span className="ni-stance-badge">{d.stance}</span>
                                <span className="ni-decision-importance">imp: {d.importance}</span>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Terms */}
            {terms.length > 0 && (
                <AccordionSection
                    key={`terms-${isOpen('terms', firstNonEmptyIndex === 5)}`}
                    title={`Terms (${terms.length})`}
                    defaultOpen={isOpen('terms', firstNonEmptyIndex === 5)}
                    onToggle={handleToggle('terms')}
                >
                    <table className="ni-term-table">
                        <thead>
                            <tr>
                                <th>Term</th>
                                <th>Definition</th>
                            </tr>
                        </thead>
                        <tbody>
                            {terms.map((t, i) => (
                                <tr key={i} className="ni-term-row">
                                    <td className="ni-term-name">{t.term}</td>
                                    <td className="ni-term-def">{t.definition}</td>
                                </tr>
                            ))}
                        </tbody>
                    </table>
                </AccordionSection>
            )}

            {/* Key Quotes */}
            {keyQuotes.length > 0 && (
                <AccordionSection
                    key={`quotes-${isOpen('quotes', firstNonEmptyIndex === 6)}`}
                    title={`Key Quotes (${keyQuotes.length})`}
                    defaultOpen={isOpen('quotes', firstNonEmptyIndex === 6)}
                    onToggle={handleToggle('quotes')}
                >
                    <div className="ni-quotes">
                        {keyQuotes.map((q, i) => (
                            <div key={i} className="ni-quote">
                                <blockquote className="ni-quote-text">
                                    &ldquo;{q.text}&rdquo;
                                </blockquote>
                                <div className="ni-quote-meta">
                                    <span className="ni-quote-speaker">
                                        {q.speaker}
                                        {q.speaker_role && (
                                            <span className="ni-quote-role"> ({q.speaker_role})</span>
                                        )}
                                    </span>
                                    <span className="ni-quote-importance">imp: {q.importance}</span>
                                    {q.chunk_ref && (
                                        <span className="ni-quote-chunk-ref">{q.chunk_ref}</span>
                                    )}
                                </div>
                            </div>
                        ))}
                    </div>
                </AccordionSection>
            )}

            {/* Dead Ends */}
            {deadEnds.length > 0 && (
                <AccordionSection
                    key={`deadends-${isOpen('deadends', firstNonEmptyIndex === 7)}`}
                    title={`Dead Ends (${deadEnds.length})`}
                    defaultOpen={isOpen('deadends', firstNonEmptyIndex === 7)}
                    onToggle={handleToggle('deadends')}
                >
                    <ul className="ni-dead-ends">
                        {deadEnds.map((d, i) => (
                            <li key={i} className="ni-dead-end">{d}</li>
                        ))}
                    </ul>
                </AccordionSection>
            )}
        </div>
    );
}
