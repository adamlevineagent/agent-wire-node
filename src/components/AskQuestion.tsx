import { useState, useEffect, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { BuildProgress } from './BuildProgress';

const PYRAMID_API_BASE = 'http://localhost:8765';

interface SlugInfo {
    slug: string;
    content_type: string;
    source_path: string;
    node_count: number;
    max_depth: number;
    last_built_at: string | null;
    created_at: string;
    referenced_slugs: string[];
}

interface AskQuestionProps {
    /** The base pyramid slug to ask a question on */
    baseSlug: string;
    /** All slugs available (used to compute accreted references) */
    allSlugs: SlugInfo[];
    /** Called when user closes the dialog or build completes */
    onClose: () => void;
    /** Called after slug creation so parent can refresh slug list */
    onSlugCreated: () => void;
}

/**
 * Auto-generate a kebab-case slug from a question string.
 * Takes the first 4 significant words, lowercased, max 30 chars.
 */
function questionToSlug(question: string): string {
    const stopWords = new Set([
        'a', 'an', 'the', 'is', 'are', 'was', 'were', 'be', 'been',
        'do', 'does', 'did', 'and', 'or', 'but', 'in', 'on', 'at',
        'to', 'for', 'of', 'with', 'by', 'from', 'it', 'its', 'i',
        'me', 'my', 'we', 'our', 'you', 'your', 'he', 'she', 'they',
        'this', 'that', 'these', 'those', 'can', 'could', 'would',
        'should', 'will', 'shall', 'may', 'might', 'has', 'have', 'had',
    ]);

    const words = question
        .toLowerCase()
        .replace(/[^a-z0-9\s]/g, '')
        .split(/\s+/)
        .filter(w => w.length > 0 && !stopWords.has(w));

    const significant = words.slice(0, 4);
    if (significant.length === 0) return 'question';

    const slug = significant.join('-');
    return slug.slice(0, 30).replace(/-$/, '');
}

function slugify(raw: string): string {
    return raw
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 64);
}

type Phase = 'input' | 'creating' | 'building';

export function AskQuestion({ baseSlug, allSlugs, onClose, onSlugCreated }: AskQuestionProps) {
    const [question, setQuestion] = useState('');
    const [slug, setSlug] = useState('');
    const [phase, setPhase] = useState<Phase>('input');
    const [error, setError] = useState<string | null>(null);
    const [showAdvanced, setShowAdvanced] = useState(false);
    const [granularity, setGranularity] = useState(3);
    const [maxDepth, setMaxDepth] = useState(5);

    // Compute the accreted reference set:
    // The base slug + all question slugs that already reference it
    const autoReferences = useMemo(() => {
        const refs = [baseSlug];
        for (const s of allSlugs) {
            if (
                s.content_type === 'question' &&
                s.slug !== baseSlug &&
                s.referenced_slugs?.includes(baseSlug)
            ) {
                refs.push(s.slug);
            }
        }
        return refs;
    }, [baseSlug, allSlugs]);

    // Manual reference overrides (advanced mode)
    const [manualRefs, setManualRefs] = useState<string[] | null>(null);

    const effectiveRefs = manualRefs ?? autoReferences;

    // Auto-generate slug when question changes
    useEffect(() => {
        if (question.trim()) {
            setSlug(questionToSlug(question));
        } else {
            setSlug('');
        }
    }, [question]);

    const toggleManualRef = useCallback((refSlug: string) => {
        setManualRefs(prev => {
            const current = prev ?? [...autoReferences];
            if (current.includes(refSlug)) {
                // Don't allow removing the base slug
                if (refSlug === baseSlug) return current;
                return current.filter(s => s !== refSlug);
            } else {
                return [...current, refSlug];
            }
        });
    }, [autoReferences, baseSlug]);

    // All non-vine, non-question slugs that could be referenced
    const availableRefs = useMemo(() => {
        return allSlugs.filter(s =>
            s.slug !== slug && // exclude self
            s.node_count > 0   // must have content
        );
    }, [allSlugs, slug]);

    const handleBuild = useCallback(async () => {
        if (!question.trim() || !slug.trim()) return;

        setPhase('creating');
        setError(null);

        try {
            // Step 1: Create the question slug via HTTP (supports referenced_slugs)
            const createResp = await fetch(`${PYRAMID_API_BASE}/pyramid/slugs`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    slug: slug,
                    content_type: 'question',
                    source_path: '',
                    referenced_slugs: effectiveRefs,
                }),
            });

            if (!createResp.ok) {
                const errBody = await createResp.json().catch(() => ({ error: createResp.statusText }));
                throw new Error(errBody.error || `Failed to create slug: ${createResp.status}`);
            }

            onSlugCreated();

            // Step 2: Trigger question build via HTTP
            const buildResp = await fetch(`${PYRAMID_API_BASE}/pyramid/${slug}/build/question`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    question: question.trim(),
                    granularity,
                    max_depth: maxDepth,
                }),
            });

            if (!buildResp.ok) {
                const errBody = await buildResp.json().catch(() => ({ error: buildResp.statusText }));
                throw new Error(errBody.error || `Failed to start build: ${buildResp.status}`);
            }

            setPhase('building');
        } catch (err) {
            setError(String(err));
            setPhase('input');
        }
    }, [question, slug, effectiveRefs, granularity, maxDepth, onSlugCreated]);

    // Building phase — show build progress
    if (phase === 'building') {
        return (
            <BuildProgress
                slug={slug}
                onComplete={() => {
                    onSlugCreated();
                }}
                onClose={onClose}
            />
        );
    }

    const baseIsQuestion = allSlugs.find(s => s.slug === baseSlug)?.content_type === 'question';

    return (
        <div className="ask-question-overlay">
            <div className="ask-question-dialog">
                <div className="ask-question-header">
                    <h3>
                        {baseIsQuestion ? 'Ask Another Question' : 'Ask a Question'}
                    </h3>
                    <button
                        className="ask-question-close"
                        onClick={onClose}
                        title="Close"
                    >
                        &times;
                    </button>
                </div>

                <p className="ask-question-description">
                    Ask a question about <strong>{baseSlug}</strong>. This creates a new question pyramid
                    that decomposes your question across the referenced knowledge.
                </p>

                {error && (
                    <div className="ask-question-error">
                        {error}
                        <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                            Dismiss
                        </button>
                    </div>
                )}

                <div className="ask-question-field">
                    <label className="field-label">Your question:</label>
                    <textarea
                        className="ask-question-input"
                        value={question}
                        onChange={(e) => setQuestion(e.target.value)}
                        placeholder="What is the overall architecture and how do the pieces fit together?"
                        rows={3}
                        autoFocus
                        disabled={phase === 'creating'}
                    />
                </div>

                <div className="ask-question-field">
                    <label className="field-label">Slug name:</label>
                    <input
                        type="text"
                        className="slug-input"
                        value={slug}
                        onChange={(e) => setSlug(slugify(e.target.value))}
                        placeholder="auto-generated-from-question"
                        disabled={phase === 'creating'}
                    />
                    <span className="ask-question-slug-hint">Auto-generated from question. Edit if needed.</span>
                </div>

                <div className="ask-question-references">
                    <label className="field-label">Building on:</label>
                    <div className="ask-question-ref-list">
                        {effectiveRefs.map(ref => (
                            <span key={ref} className="ask-question-ref-tag">
                                {ref}
                                {ref === baseSlug && <span className="ask-question-ref-base"> (base)</span>}
                            </span>
                        ))}
                    </div>
                </div>

                <div className="ask-question-advanced-toggle">
                    <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setShowAdvanced(!showAdvanced)}
                    >
                        {showAdvanced ? '\u25B2 Hide Advanced' : '\u25BC Advanced'}
                    </button>
                </div>

                {showAdvanced && (
                    <div className="ask-question-advanced">
                        <div className="ask-question-advanced-section">
                            <label className="field-label">Select references:</label>
                            <div className="ask-question-ref-picker">
                                {availableRefs.map(s => {
                                    const checked = effectiveRefs.includes(s.slug);
                                    const isBase = s.slug === baseSlug;
                                    return (
                                        <label
                                            key={s.slug}
                                            className={`ask-question-ref-option${isBase ? ' ref-locked' : ''}`}
                                        >
                                            <input
                                                type="checkbox"
                                                checked={checked}
                                                disabled={isBase || phase === 'creating'}
                                                onChange={() => toggleManualRef(s.slug)}
                                            />
                                            <span className="ref-option-slug">{s.slug}</span>
                                            <span className={`pyramid-card-badge badge-${s.content_type}`}>
                                                {s.content_type}
                                            </span>
                                        </label>
                                    );
                                })}
                            </div>
                        </div>

                        <div className="ask-question-params">
                            <div className="ask-question-param">
                                <label className="field-label">Granularity:</label>
                                <input
                                    type="number"
                                    className="input input-sm"
                                    value={granularity}
                                    onChange={(e) => setGranularity(Math.max(1, Math.min(10, parseInt(e.target.value) || 3)))}
                                    min={1}
                                    max={10}
                                    disabled={phase === 'creating'}
                                />
                            </div>
                            <div className="ask-question-param">
                                <label className="field-label">Max depth:</label>
                                <input
                                    type="number"
                                    className="input input-sm"
                                    value={maxDepth}
                                    onChange={(e) => setMaxDepth(Math.max(1, Math.min(10, parseInt(e.target.value) || 5)))}
                                    min={1}
                                    max={10}
                                    disabled={phase === 'creating'}
                                />
                            </div>
                        </div>
                    </div>
                )}

                <div className="ask-question-actions">
                    <button className="btn btn-ghost" onClick={onClose} disabled={phase === 'creating'}>
                        Cancel
                    </button>
                    <button
                        className="btn btn-primary btn-ask-build"
                        onClick={handleBuild}
                        disabled={!question.trim() || !slug.trim() || phase === 'creating'}
                    >
                        {phase === 'creating' ? 'Creating...' : 'Build'}
                    </button>
                </div>
            </div>
        </div>
    );
}
