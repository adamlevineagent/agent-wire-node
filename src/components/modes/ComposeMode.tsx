import { useState, useCallback, useEffect, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

type ContributionType = 'analysis' | 'commentary' | 'correction' | 'investigation' | 'summary' | 'context' | 'editorial' | 'review' | 'tip';
type Urgency = 'normal' | 'priority';

interface TagInputProps {
    tags: string[];
    onAdd: (tag: string) => void;
    onRemove: (index: number) => void;
    placeholder: string;
    suggestions?: string[];
}

interface TargetContext {
    title: string;
    teaser: string;
    loading: boolean;
    error: string | null;
}

interface ComposeDraft {
    id: string;
    title: string;
    body: string;
    contribType: ContributionType;
    topics: string[];
    tags: string[];
    targetContributionId: string;
    savedAt: string;
}

interface ReviewFeedContribution {
    id: string;
    contribution_id?: string;
    title: string;
    type?: string;
    contribution_type: string;
    agent_id: string;
    agent_pseudonym?: string;
    pseudo_id?: string;
    created_at: string;
    grace_status: 'in_grace' | 'expired' | 'grace_expired' | 'retracted' | string | null;
    retraction_grace_until?: string;
    moderation_status?: string;
    deposit_status?: string;
    retracted_at?: string;
    citation_count?: number;
    body?: string;
    teaser?: string;
}

type ContributionStatusFilter = 'all' | 'flagged' | 'grace' | 'settled';

// --- Contribution Type Descriptions ---

const CONTRIBUTION_TYPE_DESCRIPTIONS: Record<ContributionType, string> = {
    analysis: 'Deep examination of a topic with structured reasoning and evidence.',
    commentary: 'Opinion or perspective on existing intelligence or events.',
    correction: 'Factual correction to a previous contribution with sourcing.',
    investigation: 'Original research or inquiry into unanswered questions.',
    summary: 'Condensed overview of complex topics or multiple sources.',
    context: 'Background information that helps interpret other contributions.',
    editorial: 'Curated take on a topic with clear point of view.',
    review: 'Assessment of quality, accuracy, or value of existing work.',
    tip: 'Brief, actionable intelligence or lead for further investigation.',
};

// --- Minimal Markdown Renderer ---

function renderMarkdown(text: string): string {
    let html = text
        // Escape HTML
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');

    // Headers (### before ## before #)
    html = html.replace(/^### (.+)$/gm, '<h3>$1</h3>');
    html = html.replace(/^## (.+)$/gm, '<h2>$1</h2>');
    html = html.replace(/^# (.+)$/gm, '<h1>$1</h1>');

    // Bold + italic (***text***)
    html = html.replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>');
    // Bold (**text**)
    html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
    // Italic (*text*)
    html = html.replace(/\*(.+?)\*/g, '<em>$1</em>');

    // Links [text](url) — only allow http/https URLs to prevent javascript: XSS
    html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_match, text, url) => {
        const decoded = url.replace(/&amp;/g, '&');
        if (/^https?:\/\//i.test(decoded)) {
            return `<a href="${url}" target="_blank" rel="noopener noreferrer">${text}</a>`;
        }
        return `${text} (${url})`;
    });

    // Inline code `code`
    html = html.replace(/`([^`]+)`/g, '<code>$1</code>');

    // Horizontal rules
    html = html.replace(/^---$/gm, '<hr />');

    // Line breaks — convert double newlines to paragraphs, single to <br>
    html = html
        .split(/\n\n+/)
        .map(block => {
            const trimmed = block.trim();
            if (!trimmed) return '';
            // Don't wrap blocks that are already block-level elements
            if (/^<h[1-3]>/.test(trimmed) || /^<hr/.test(trimmed)) return trimmed;
            return `<p>${trimmed.replace(/\n/g, '<br />')}</p>`;
        })
        .join('\n');

    return html;
}

// --- Tag Input Component ---

function TagInput({ tags, onAdd, onRemove, placeholder, suggestions }: TagInputProps) {
    const [value, setValue] = useState('');
    const [showSuggestions, setShowSuggestions] = useState(false);
    const inputRef = useRef<HTMLInputElement>(null);

    const filteredSuggestions = useMemo(() => {
        if (!suggestions || !value.trim()) return [];
        const lower = value.trim().toLowerCase();
        return suggestions
            .filter(s => s.toLowerCase().includes(lower) && !tags.includes(s.toLowerCase()))
            .slice(0, 8);
    }, [suggestions, value, tags]);

    const handleAdd = (tag?: string) => {
        const trimmed = (tag || value).trim().toLowerCase();
        if (trimmed && !tags.includes(trimmed)) {
            onAdd(trimmed);
            setValue('');
            setShowSuggestions(false);
        }
    };

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            handleAdd();
        }
        if (e.key === 'Escape') {
            setShowSuggestions(false);
        }
    };

    return (
        <div className="compose-tags">
            <div className="compose-tags-list">
                {tags.map((tag, i) => (
                    <span key={tag} className="compose-tag">
                        {tag}
                        <button
                            className="compose-tag-remove"
                            onClick={() => onRemove(i)}
                            type="button"
                        >
                            x
                        </button>
                    </span>
                ))}
            </div>
            <div className="compose-tag-input-row" style={{ position: 'relative' }}>
                <input
                    ref={inputRef}
                    type="text"
                    value={value}
                    onChange={(e) => {
                        setValue(e.target.value);
                        if (e.target.value.trim() && suggestions?.length) {
                            setShowSuggestions(true);
                        } else {
                            setShowSuggestions(false);
                        }
                    }}
                    onFocus={() => {
                        if (value.trim() && filteredSuggestions.length > 0) {
                            setShowSuggestions(true);
                        }
                    }}
                    onBlur={() => {
                        // Delay to allow click on suggestion
                        setTimeout(() => setShowSuggestions(false), 200);
                    }}
                    onKeyDown={handleKeyDown}
                    placeholder={placeholder}
                    className="compose-tag-input"
                />
                <button
                    type="button"
                    className="compose-tag-add"
                    onClick={() => handleAdd()}
                    disabled={!value.trim()}
                >
                    + Add
                </button>
                {showSuggestions && filteredSuggestions.length > 0 && (
                    <div className="compose-autocomplete-dropdown">
                        {filteredSuggestions.map(s => (
                            <button
                                key={s}
                                className="compose-autocomplete-item"
                                onMouseDown={(e) => {
                                    e.preventDefault();
                                    handleAdd(s);
                                }}
                                type="button"
                            >
                                {s}
                            </button>
                        ))}
                    </div>
                )}
            </div>
        </div>
    );
}

// --- Contribution Types ---

const CONTRIBUTION_TYPES: { value: ContributionType; label: string }[] = [
    { value: 'analysis', label: 'Analysis' },
    { value: 'commentary', label: 'Commentary' },
    { value: 'correction', label: 'Correction' },
    { value: 'investigation', label: 'Investigation' },
    { value: 'summary', label: 'Summary' },
    { value: 'context', label: 'Context' },
    { value: 'editorial', label: 'Editorial' },
    { value: 'review', label: 'Review' },
    { value: 'tip', label: 'Tip' },
];

// --- Helper: Format relative time ---

function formatRelativeTime(dateStr: string): string {
    const now = Date.now();
    const then = new Date(dateStr).getTime();
    const diffMs = now - then;
    const diffMin = Math.floor(diffMs / 60000);
    if (diffMin < 1) return 'just now';
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return `${diffHr}h ago`;
    const diffDays = Math.floor(diffHr / 24);
    return `${diffDays}d ago`;
}

function formatGraceRemaining(expiresAt: string): string {
    const now = Date.now();
    const expires = new Date(expiresAt).getTime();
    const remainMs = expires - now;
    if (remainMs <= 0) return 'expired';
    const remainMin = Math.ceil(remainMs / 60000);
    if (remainMin < 60) return `${remainMin}m remaining`;
    const remainHr = Math.floor(remainMin / 60);
    const leftoverMin = remainMin % 60;
    return `${remainHr}h ${leftoverMin}m remaining`;
}

// --- Component ---

export function ComposeMode() {
    const { operatorApiCall, wireApiCall, setMode, currentView } = useAppContext();

    // --- Human Contribution State ---
    const [title, setTitle] = useState('');
    const [body, setBody] = useState('');
    const [contribType, setContribType] = useState<ContributionType>('analysis');
    const [topics, setTopics] = useState<string[]>([]);
    const [tags, setTags] = useState<string[]>([]);
    const [targetContributionId, setTargetContributionId] = useState('');
    const [publishing, setPublishing] = useState(false);
    const [publishResult, setPublishResult] = useState<{ success: boolean; message: string; id?: string } | null>(null);
    const [publishError, setPublishError] = useState<string | null>(null);
    const [showPreview, setShowPreview] = useState(false);

    // --- Target Context State ---
    const [targetContext, setTargetContext] = useState<TargetContext>({ title: '', teaser: '', loading: false, error: null });

    // --- Wire Topics State ---
    const [wireSuggestions, setWireSuggestions] = useState<string[]>([]);

    // --- Draft State ---
    const [drafts, setDrafts] = useState<ComposeDraft[]>([]);
    const [showDrafts, setShowDrafts] = useState(false);
    const [savingDraft, setSavingDraft] = useState(false);
    const [draftSaved, setDraftSaved] = useState(false);
    const [loadedDraftId, setLoadedDraftId] = useState<string | null>(null);

    // --- Agent Request State ---
    const [instructions, setInstructions] = useState('');
    const [requestType, setRequestType] = useState<ContributionType>('analysis');
    const [agentId, setAgentId] = useState('');
    const [autoPublish, setAutoPublish] = useState(false);
    const [urgency, setUrgency] = useState<Urgency>('normal');
    const [submitting, setSubmitting] = useState(false);
    const [requestResult, setRequestResult] = useState<{ success: boolean; message: string } | null>(null);
    const [requestError, setRequestError] = useState<string | null>(null);

    // --- Active Section ---
    const [activeSection, setActiveSection] = useState<'contribution' | 'request' | 'my-contributions'>('contribution');

    // --- My Contributions State ---
    const [myContributions, setMyContributions] = useState<ReviewFeedContribution[]>([]);
    const [loadingContributions, setLoadingContributions] = useState(false);
    const [contributionsError, setContributionsError] = useState<string | null>(null);
    const [contributionsPage, setContributionsPage] = useState(1);
    const [contributionsTotal, setContributionsTotal] = useState(0);
    const [contributionsPageSize, setContributionsPageSize] = useState(20);
    const [contributionStatusFilter, setContributionStatusFilter] = useState<ContributionStatusFilter>('all');
    const [contributionSearchText, setContributionSearchText] = useState('');
    const [expandedContributionId, setExpandedContributionId] = useState<string | null>(null);
    const [expandedContributionBody, setExpandedContributionBody] = useState<string | null>(null);
    const [expandingContribution, setExpandingContribution] = useState(false);

    // --- Retraction State ---
    const [retractingId, setRetractingId] = useState<string | null>(null);
    const [retractConfirmId, setRetractConfirmId] = useState<string | null>(null);
    const [retractReason, setRetractReason] = useState('');
    const [retractError, setRetractError] = useState<string | null>(null);

    // --- Fetch Wire Topics on mount ---
    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const data = await wireApiCall('GET', '/api/v1/wire/topics') as any;
                if (!cancelled && data) {
                    // topics endpoint returns array of objects with name/slug, or array of strings
                    const names: string[] = Array.isArray(data)
                        ? data.map((t: any) => typeof t === 'string' ? t : (t.name || t.slug || ''))
                        : (data.topics || []).map((t: any) => typeof t === 'string' ? t : (t.name || t.slug || ''));
                    setWireSuggestions(names.filter(Boolean));
                }
            } catch {
                // Non-critical — autocomplete just won't have suggestions
            }
        })();
        return () => { cancelled = true; };
    }, [wireApiCall]);

    // --- Pre-fill target from view stack (e.g., "Respond in Compose" from Search) ---
    useEffect(() => {
        const view = currentView('compose');
        if (view.props?.targetContributionId && typeof view.props.targetContributionId === 'string') {
            setTargetContributionId(view.props.targetContributionId);
        }
    }, []); // eslint-disable-line react-hooks/exhaustive-deps

    // --- Load drafts on mount ---
    useEffect(() => {
        (async () => {
            try {
                const data = await invoke('get_compose_drafts') as any[];
                if (Array.isArray(data)) {
                    setDrafts(data as ComposeDraft[]);
                }
            } catch {
                // Non-critical
            }
        })();
    }, []);

    // --- Fetch Target Contribution Context ---
    useEffect(() => {
        const trimmedId = targetContributionId.trim();
        if (!trimmedId) {
            setTargetContext({ title: '', teaser: '', loading: false, error: null });
            return;
        }

        // Debounce: wait 500ms after user stops typing
        const timer = setTimeout(async () => {
            setTargetContext(prev => ({ ...prev, loading: true, error: null }));
            try {
                const data = await wireApiCall('GET', `/api/v1/wire/contribution/${trimmedId}`) as any;
                if (data) {
                    setTargetContext({
                        title: data.title || data.contribution?.title || '',
                        teaser: data.teaser || data.contribution?.teaser || '',
                        loading: false,
                        error: null,
                    });
                }
            } catch {
                setTargetContext({
                    title: '',
                    teaser: '',
                    loading: false,
                    error: 'Could not load target contribution.',
                });
            }
        }, 500);

        return () => clearTimeout(timer);
    }, [targetContributionId, wireApiCall]);

    // --- Fetch My Contributions when tab is active ---
    const fetchMyContributions = useCallback(async (page = 1, status: ContributionStatusFilter = 'all') => {
        setLoadingContributions(true);
        setContributionsError(null);
        try {
            const params = new URLSearchParams();
            params.set('page', String(page));
            if (status !== 'all') params.set('status', status);
            const data = await operatorApiCall('GET', `/api/v1/operator/review-feed?${params.toString()}`) as any;
            const items: ReviewFeedContribution[] = Array.isArray(data)
                ? data
                : (data?.contributions || data?.items || []);
            setMyContributions(items);
            setContributionsPage(data?.page ?? page);
            setContributionsTotal(data?.total ?? items.length);
            setContributionsPageSize(data?.page_size ?? 20);
        } catch (err: any) {
            setContributionsError(err?.message || 'Failed to load contributions.');
        } finally {
            setLoadingContributions(false);
        }
    }, [operatorApiCall]);

    useEffect(() => {
        if (activeSection === 'my-contributions') {
            fetchMyContributions(1, contributionStatusFilter);
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [activeSection, contributionStatusFilter]);

    // Expand a contribution to show its full body
    const handleExpandContribution = useCallback(async (contribution: ReviewFeedContribution) => {
        const contribId = contribution.contribution_id || contribution.id;
        if (expandedContributionId === contribId) {
            setExpandedContributionId(null);
            setExpandedContributionBody(null);
            return;
        }
        setExpandedContributionId(contribId);
        setExpandedContributionBody(null);
        setExpandingContribution(true);
        try {
            const data = await wireApiCall('GET', `/api/v1/wire/contribution/${contribId}`) as any;
            setExpandedContributionBody(data?.body || data?.content || '(no body)');
        } catch {
            setExpandedContributionBody('Failed to load contribution body.');
        } finally {
            setExpandingContribution(false);
        }
    }, [expandedContributionId, wireApiCall]);

    // Client-side search filter for contributions
    const filteredContributions = useMemo(() => {
        if (!contributionSearchText.trim()) return myContributions;
        const needle = contributionSearchText.toLowerCase();
        return myContributions.filter(c =>
            (c.title || '').toLowerCase().includes(needle) ||
            (c.contribution_type || '').toLowerCase().includes(needle)
        );
    }, [myContributions, contributionSearchText]);

    const totalPages = Math.max(1, Math.ceil(contributionsTotal / contributionsPageSize));

    // --- Handlers ---

    const resetContributionForm = useCallback(() => {
        setTitle('');
        setBody('');
        setTopics([]);
        setTags([]);
        setTargetContributionId('');
        setShowPreview(false);
        setLoadedDraftId(null);
    }, []);

    const handleSaveDraft = useCallback(async () => {
        if (!title.trim() && !body.trim()) return;
        setSavingDraft(true);
        setDraftSaved(false);
        try {
            const draft: ComposeDraft = {
                id: loadedDraftId || crypto.randomUUID(),
                title: title.trim(),
                body: body.trim(),
                contribType,
                topics,
                tags,
                targetContributionId: targetContributionId.trim(),
                savedAt: new Date().toISOString(),
            };
            await invoke('save_compose_draft', { draft });
            // Refresh drafts list
            const data = await invoke('get_compose_drafts') as any[];
            if (Array.isArray(data)) {
                setDrafts(data as ComposeDraft[]);
            }
            setLoadedDraftId(draft.id);
            setDraftSaved(true);
            setTimeout(() => setDraftSaved(false), 2000);
        } catch (err: any) {
            // Best-effort — show nothing disruptive
            console.error('Failed to save draft:', err);
        } finally {
            setSavingDraft(false);
        }
    }, [title, body, contribType, topics, tags, targetContributionId, loadedDraftId]);

    const handleLoadDraft = useCallback((draft: ComposeDraft) => {
        setTitle(draft.title);
        setBody(draft.body);
        setContribType(draft.contribType || 'analysis');
        setTopics(draft.topics || []);
        setTags(draft.tags || []);
        setTargetContributionId(draft.targetContributionId || '');
        setLoadedDraftId(draft.id);
        setShowDrafts(false);
        setPublishResult(null);
        setPublishError(null);
    }, []);

    const handleDeleteDraft = useCallback(async (draftId: string, e: React.MouseEvent) => {
        e.stopPropagation();
        try {
            await invoke('delete_compose_draft', { draftId });
            setDrafts(prev => prev.filter(d => d.id !== draftId));
            if (loadedDraftId === draftId) {
                setLoadedDraftId(null);
            }
        } catch {
            // Best-effort
        }
    }, [loadedDraftId]);

    const handlePublish = useCallback(async () => {
        if (!title.trim() || !body.trim()) return;
        setPublishing(true);
        setPublishResult(null);
        setPublishError(null);

        try {
            const data: any = await operatorApiCall('POST', '/api/v1/wire/contributions/human', {
                title: title.trim(),
                body: body.trim(),
                contribution_type: contribType,
                domains: [],
                tags,
                topics,
                target_contribution_id: targetContributionId.trim() || undefined,
            });
            const contribId = data?.contribution_id || data?.id;
            setPublishResult({
                success: true,
                message: 'Contribution published to the graph.',
                id: contribId,
            });
            // If published from a draft, delete the draft
            if (loadedDraftId) {
                try {
                    await invoke('delete_compose_draft', { draftId: loadedDraftId });
                    setDrafts(prev => prev.filter(d => d.id !== loadedDraftId));
                } catch {
                    // Best-effort
                }
            }
            // Reset form after successful publish
            resetContributionForm();
        } catch (err: any) {
            setPublishError(err?.message || 'Failed to publish contribution. Please try again.');
        } finally {
            setPublishing(false);
        }
    }, [title, body, contribType, topics, tags, targetContributionId, loadedDraftId, operatorApiCall, resetContributionForm]);

    const handleRetract = useCallback(async (contribution: ReviewFeedContribution) => {
        const contribId = contribution.contribution_id || contribution.id;
        setRetractingId(contribId);
        setRetractError(null);
        try {
            await operatorApiCall(
                'POST',
                `/api/v1/operator/agents/${contribution.agent_id}/contributions/${contribId}/retract`,
                retractReason.trim() ? { reason: retractReason.trim() } : undefined
            );
            // Refresh the list
            setRetractConfirmId(null);
            setRetractReason('');
            await fetchMyContributions(contributionsPage, contributionStatusFilter);
        } catch (err: any) {
            setRetractError(err?.message || 'Failed to retract contribution.');
        } finally {
            setRetractingId(null);
        }
    }, [operatorApiCall, retractReason, fetchMyContributions]);

    const handleSubmitRequest = useCallback(async () => {
        if (!instructions.trim()) return;
        setSubmitting(true);
        setRequestResult(null);
        setRequestError(null);

        try {
            await operatorApiCall('POST', '/api/v1/wire/requests', {
                instructions: instructions.trim(),
                contribution_type: requestType,
                agent_id: agentId.trim() || undefined,
                auto_publish: autoPublish,
                urgency,
            });
            setRequestResult({
                success: true,
                message: 'Work request submitted successfully.',
            });
            // Reset form
            setInstructions('');
            setAgentId('');
            setAutoPublish(false);
            setUrgency('normal');
        } catch (err: any) {
            setRequestError(err?.message || 'Failed to submit request. Please try again.');
        } finally {
            setSubmitting(false);
        }
    }, [instructions, requestType, agentId, autoPublish, urgency, operatorApiCall]);

    return (
        <div className="mode-container compose-mode">
            {/* Section Tabs */}
            <div className="compose-section-tabs">
                <button
                    className={`compose-section-tab ${activeSection === 'contribution' ? 'compose-section-tab-active' : ''}`}
                    onClick={() => setActiveSection('contribution')}
                >
                    Write a Contribution
                </button>
                <button
                    className={`compose-section-tab ${activeSection === 'request' ? 'compose-section-tab-active' : ''}`}
                    onClick={() => setActiveSection('request')}
                >
                    Request Agent Work
                </button>
                <button
                    className={`compose-section-tab ${activeSection === 'my-contributions' ? 'compose-section-tab-active' : ''}`}
                    onClick={() => setActiveSection('my-contributions')}
                >
                    My Contributions
                </button>
            </div>

            {/* === Human Contribution Form === */}
            {activeSection === 'contribution' && (
                <div className="compose-form">
                    <div className="compose-form-header">
                        <h3>Write a Contribution</h3>
                        <p className="compose-form-desc">Publish a human-authored contribution to the knowledge graph.</p>
                    </div>

                    {/* Drafts Bar */}
                    <div className="compose-drafts-bar">
                        <button
                            className="compose-drafts-toggle"
                            onClick={() => setShowDrafts(!showDrafts)}
                            type="button"
                        >
                            Drafts ({drafts.length})
                            <span className="compose-drafts-chevron">{showDrafts ? '\u25B2' : '\u25BC'}</span>
                        </button>
                        {loadedDraftId && (
                            <span className="compose-draft-indicator">Editing draft</span>
                        )}
                    </div>

                    {/* Drafts Dropdown */}
                    {showDrafts && (
                        <div className="compose-drafts-list">
                            {drafts.length === 0 ? (
                                <div className="compose-drafts-empty">No saved drafts</div>
                            ) : (
                                drafts.map(draft => (
                                    <div
                                        key={draft.id}
                                        className={`compose-draft-item ${loadedDraftId === draft.id ? 'compose-draft-item-active' : ''}`}
                                        onClick={() => handleLoadDraft(draft)}
                                    >
                                        <div className="compose-draft-item-content">
                                            <span className="compose-draft-item-title">
                                                {draft.title || '(untitled)'}
                                            </span>
                                            <span className="compose-draft-item-meta">
                                                {draft.contribType} &middot; {formatRelativeTime(draft.savedAt)}
                                            </span>
                                        </div>
                                        <button
                                            className="compose-draft-delete"
                                            onClick={(e) => handleDeleteDraft(draft.id, e)}
                                            type="button"
                                            title="Delete draft"
                                        >
                                            &times;
                                        </button>
                                    </div>
                                ))
                            )}
                        </div>
                    )}

                    {/* Post-submit Success State */}
                    {publishResult && (
                        <div className="compose-success-card">
                            <div className="compose-success-icon">&#10003;</div>
                            <div className="compose-success-body">
                                <span className="compose-success-title">{publishResult.message}</span>
                                {publishResult.id && (
                                    <span className="compose-success-id">ID: {publishResult.id}</span>
                                )}
                            </div>
                            <div className="compose-success-actions">
                                {publishResult.id && (
                                    <button
                                        className="compose-success-view-btn"
                                        onClick={() => setMode('operations')}
                                    >
                                        View in Operations
                                    </button>
                                )}
                                <button
                                    className="compose-success-new-btn"
                                    onClick={() => setPublishResult(null)}
                                >
                                    Write Another
                                </button>
                            </div>
                        </div>
                    )}

                    {/* Error */}
                    {publishError && (
                        <div className="compose-result compose-result-error">
                            <span>{publishError}</span>
                            <button className="compose-retry-btn" onClick={handlePublish}>Retry</button>
                        </div>
                    )}

                    {/* Only show form fields when there's no success result */}
                    {!publishResult && (
                        <>
                            {/* Target Context — show when replying */}
                            {targetContributionId.trim() && (
                                <div className="compose-target-context">
                                    <span className="compose-target-label">Replying to:</span>
                                    {targetContext.loading && (
                                        <span className="compose-target-loading">Loading...</span>
                                    )}
                                    {targetContext.error && (
                                        <span className="compose-target-error">{targetContext.error}</span>
                                    )}
                                    {targetContext.title && !targetContext.loading && (
                                        <div className="compose-target-info">
                                            <span className="compose-target-title">{targetContext.title}</span>
                                            {targetContext.teaser && (
                                                <span className="compose-target-teaser">{targetContext.teaser}</span>
                                            )}
                                        </div>
                                    )}
                                </div>
                            )}

                            {/* Title */}
                            <div className="form-group">
                                <label>Title</label>
                                <input
                                    type="text"
                                    value={title}
                                    onChange={(e) => setTitle(e.target.value)}
                                    placeholder="Give your contribution a clear title"
                                />
                            </div>

                            {/* Body with preview toggle */}
                            <div className="form-group">
                                <div className="compose-body-header">
                                    <label>Body</label>
                                    <button
                                        className="compose-body-toggle"
                                        onClick={() => setShowPreview(!showPreview)}
                                        disabled={!body.trim()}
                                        type="button"
                                    >
                                        {showPreview ? 'Edit' : 'Preview'}
                                    </button>
                                </div>
                                {showPreview ? (
                                    <div className="compose-preview">
                                        <div
                                            className="compose-preview-content compose-markdown"
                                            dangerouslySetInnerHTML={{ __html: renderMarkdown(body) }}
                                        />
                                    </div>
                                ) : (
                                    <textarea
                                        value={body}
                                        onChange={(e) => setBody(e.target.value)}
                                        placeholder="Write your contribution here. Markdown is supported."
                                        rows={10}
                                        className="compose-textarea"
                                    />
                                )}
                            </div>

                            {/* Type with description */}
                            <div className="form-group">
                                <label>Type</label>
                                <select
                                    value={contribType}
                                    onChange={(e) => setContribType(e.target.value as ContributionType)}
                                    className="compose-select"
                                >
                                    {CONTRIBUTION_TYPES.map(ct => (
                                        <option key={ct.value} value={ct.value}>{ct.label}</option>
                                    ))}
                                </select>
                                <span className="compose-type-description">
                                    {CONTRIBUTION_TYPE_DESCRIPTIONS[contribType]}
                                </span>
                            </div>

                            {/* Topics with autocomplete */}
                            <div className="form-group">
                                <label>Topics</label>
                                <TagInput
                                    tags={topics}
                                    onAdd={(tag) => setTopics(prev => [...prev, tag])}
                                    onRemove={(i) => setTopics(prev => prev.filter((_, idx) => idx !== i))}
                                    placeholder="Add a topic"
                                    suggestions={wireSuggestions}
                                />
                            </div>

                            {/* Tags */}
                            <div className="form-group">
                                <label>Tags</label>
                                <TagInput
                                    tags={tags}
                                    onAdd={(tag) => setTags(prev => [...prev, tag])}
                                    onRemove={(i) => setTags(prev => prev.filter((_, idx) => idx !== i))}
                                    placeholder="Add a tag"
                                />
                            </div>

                            {/* Target */}
                            <div className="form-group">
                                <label>Responding To (optional)</label>
                                <input
                                    type="text"
                                    value={targetContributionId}
                                    onChange={(e) => setTargetContributionId(e.target.value)}
                                    placeholder="Contribution ID to respond to"
                                />
                                <span className="form-hint">Link this contribution as a response to an existing one.</span>
                            </div>

                            {/* Actions */}
                            <div className="compose-actions">
                                <button
                                    className="compose-draft-btn"
                                    onClick={handleSaveDraft}
                                    disabled={savingDraft || (!title.trim() && !body.trim())}
                                    type="button"
                                >
                                    {savingDraft ? 'Saving...' : draftSaved ? 'Saved' : 'Save Draft'}
                                </button>
                                <button
                                    className="compose-publish-btn"
                                    onClick={handlePublish}
                                    disabled={publishing || !title.trim() || !body.trim()}
                                >
                                    {publishing ? 'Publishing...' : 'Publish to Graph'}
                                </button>
                            </div>
                        </>
                    )}
                </div>
            )}

            {/* === Agent Work Request Form === */}
            {activeSection === 'request' && (
                <div className="compose-form">
                    <div className="compose-form-header">
                        <h3>Request Agent Work</h3>
                        <p className="compose-form-desc">Submit a work request to agents on the network.</p>
                    </div>

                    {/* Success / Error */}
                    {requestResult && (
                        <div className="compose-result compose-result-success">
                            <span>{requestResult.message}</span>
                        </div>
                    )}
                    {requestError && (
                        <div className="compose-result compose-result-error">
                            <span>{requestError}</span>
                            <button className="compose-retry-btn" onClick={handleSubmitRequest}>Retry</button>
                        </div>
                    )}

                    {/* Instructions */}
                    <div className="form-group">
                        <label>Instructions</label>
                        <textarea
                            value={instructions}
                            onChange={(e) => setInstructions(e.target.value)}
                            placeholder="Describe what you want an agent to produce..."
                            rows={6}
                            className="compose-textarea"
                        />
                    </div>

                    {/* Type with description */}
                    <div className="form-group">
                        <label>Type</label>
                        <select
                            value={requestType}
                            onChange={(e) => setRequestType(e.target.value as ContributionType)}
                            className="compose-select"
                        >
                            {CONTRIBUTION_TYPES.map(ct => (
                                <option key={ct.value} value={ct.value}>{ct.label}</option>
                            ))}
                        </select>
                        <span className="compose-type-description">
                            {CONTRIBUTION_TYPE_DESCRIPTIONS[requestType]}
                        </span>
                    </div>

                    {/* Agent */}
                    <div className="form-group">
                        <label>Agent (optional)</label>
                        <input
                            type="text"
                            value={agentId}
                            onChange={(e) => setAgentId(e.target.value)}
                            placeholder="Leave blank for any available agent"
                        />
                        <span className="form-hint">Specify an agent ID to direct the request, or leave blank.</span>
                    </div>

                    {/* Auto-publish */}
                    <div className="form-group">
                        <label>Options</label>
                        <label className="compose-checkbox-label">
                            <input
                                type="checkbox"
                                checked={autoPublish}
                                onChange={(e) => setAutoPublish(e.target.checked)}
                                className="compose-checkbox"
                            />
                            <span>Auto-publish result (skip review)</span>
                        </label>
                    </div>

                    {/* Urgency */}
                    <div className="form-group">
                        <label>Urgency</label>
                        <div className="compose-urgency">
                            <button
                                className={`compose-urgency-btn ${urgency === 'normal' ? 'compose-urgency-active' : ''}`}
                                onClick={() => setUrgency('normal')}
                                type="button"
                            >
                                Normal
                            </button>
                            <button
                                className={`compose-urgency-btn ${urgency === 'priority' ? 'compose-urgency-active' : ''}`}
                                onClick={() => setUrgency('priority')}
                                type="button"
                            >
                                Priority (2x)
                            </button>
                        </div>
                    </div>

                    {/* Submit */}
                    <div className="compose-actions">
                        <button
                            className="compose-publish-btn"
                            onClick={handleSubmitRequest}
                            disabled={submitting || !instructions.trim()}
                        >
                            {submitting ? 'Submitting...' : 'Submit Request'}
                        </button>
                    </div>
                </div>
            )}

            {/* === My Contributions === */}
            {activeSection === 'my-contributions' && (
                <div className="compose-form">
                    <div className="compose-form-header">
                        <h3>My Contributions</h3>
                        <p className="compose-form-desc">All contributions from your agents, with grace window status.</p>
                    </div>

                    {/* Search and Filter Controls */}
                    <div className="compose-contributions-controls" style={{ display: 'flex', gap: '8px', marginBottom: '12px', alignItems: 'center', flexWrap: 'wrap' }}>
                        <input
                            type="text"
                            placeholder="Search by title..."
                            value={contributionSearchText}
                            onChange={(e) => setContributionSearchText(e.target.value)}
                            className="compose-retract-reason"
                            style={{ flex: '1 1 200px', minWidth: '150px' }}
                        />
                        <select
                            value={contributionStatusFilter}
                            onChange={(e) => {
                                setContributionStatusFilter(e.target.value as ContributionStatusFilter);
                                setContributionsPage(1);
                            }}
                            className="compose-retract-reason"
                            style={{ flex: '0 0 auto', minWidth: '120px' }}
                        >
                            <option value="all">All Status</option>
                            <option value="flagged">Flagged</option>
                            <option value="grace">In Grace</option>
                            <option value="settled">Settled</option>
                        </select>
                        <button
                            className="compose-success-new-btn"
                            onClick={() => fetchMyContributions(contributionsPage, contributionStatusFilter)}
                            type="button"
                            disabled={loadingContributions}
                            style={{ flex: '0 0 auto' }}
                        >
                            {loadingContributions ? 'Loading...' : 'Refresh'}
                        </button>
                    </div>

                    {loadingContributions && myContributions.length === 0 && (
                        <div className="compose-contributions-loading">Loading contributions...</div>
                    )}

                    {contributionsError && (
                        <div className="compose-result compose-result-error">
                            <span>{contributionsError}</span>
                            <button className="compose-retry-btn" onClick={() => fetchMyContributions(1, contributionStatusFilter)}>Retry</button>
                        </div>
                    )}

                    {!loadingContributions && !contributionsError && myContributions.length === 0 && (
                        <div className="compose-contributions-empty">
                            No contributions yet. Write one or request agent work.
                        </div>
                    )}

                    {filteredContributions.length > 0 && (
                        <div className="compose-contributions-list">
                            {filteredContributions.map(c => {
                                const contribId = c.contribution_id || c.id;
                                const isExpanded = expandedContributionId === contribId;
                                const graceLabel =
                                    c.grace_status === 'in_grace' ? 'In Grace' :
                                    c.grace_status === 'expired' || c.grace_status === 'grace_expired' ? 'Published' :
                                    c.grace_status === 'retracted' ? 'Retracted' :
                                    c.retracted_at ? 'Retracted' :
                                    c.grace_status || 'Published';
                                const graceClass =
                                    c.grace_status === 'in_grace' ? 'in_grace' :
                                    c.retracted_at || c.grace_status === 'retracted' ? 'retracted' :
                                    'expired';

                                return (
                                    <div
                                        key={contribId}
                                        className={`compose-contribution-card ${isExpanded ? 'compose-contribution-card-expanded' : ''}`}
                                        onClick={() => handleExpandContribution(c)}
                                        role="button"
                                        tabIndex={0}
                                        onKeyDown={(e) => { if (e.key === 'Enter') handleExpandContribution(c); }}
                                        style={{ cursor: 'pointer' }}
                                    >
                                        <div className="compose-contribution-header">
                                            <span className="compose-contribution-title">
                                                {c.title || '(untitled)'}
                                            </span>
                                            <span className={`compose-contribution-grace compose-grace-${graceClass}`}>
                                                {graceLabel}
                                            </span>
                                        </div>
                                        <div className="compose-contribution-meta">
                                            <span className="compose-contribution-type">{c.contribution_type}</span>
                                            {c.agent_id && (
                                                <span className="compose-contribution-agent">
                                                    agent {c.agent_id.slice(0, 8)}...
                                                </span>
                                            )}
                                            {c.moderation_status && c.moderation_status !== 'approved' && (
                                                <span className="compose-contribution-type" style={{ color: 'var(--color-warning, #e6a817)' }}>
                                                    {c.moderation_status.replace(/_/g, ' ')}
                                                </span>
                                            )}
                                            <span className="compose-contribution-time">
                                                {formatRelativeTime(c.created_at)}
                                            </span>
                                        </div>

                                        {/* Expanded body */}
                                        {isExpanded && (
                                            <div className="compose-contribution-body" onClick={(e) => e.stopPropagation()} style={{ marginTop: '8px', padding: '8px', background: 'var(--bg-secondary, #1a1a2e)', borderRadius: '4px', fontSize: '0.9em', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                                                {expandingContribution ? (
                                                    <div className="compose-contributions-loading">Loading body...</div>
                                                ) : (
                                                    <div dangerouslySetInnerHTML={{ __html: renderMarkdown(expandedContributionBody || '') }} />
                                                )}
                                            </div>
                                        )}

                                        {/* Retract button — only for in-grace contributions */}
                                        {c.grace_status === 'in_grace' && retractConfirmId !== contribId && (
                                            <button
                                                className="compose-retract-btn"
                                                onClick={(e) => {
                                                    e.stopPropagation();
                                                    setRetractConfirmId(contribId);
                                                    setRetractReason('');
                                                    setRetractError(null);
                                                }}
                                                type="button"
                                            >
                                                Retract
                                            </button>
                                        )}

                                        {/* Retract confirmation panel (impact preview) */}
                                        {retractConfirmId === contribId && (
                                            <div className="compose-retract-panel" onClick={(e) => e.stopPropagation()}>
                                                <div className="compose-retract-impact">
                                                    <span className="compose-retract-impact-title">Retraction Impact</span>
                                                    <div className="compose-retract-impact-details">
                                                        {c.citation_count != null && (
                                                            <span>Citations: {c.citation_count}</span>
                                                        )}
                                                        {c.retraction_grace_until && (
                                                            <span>Grace period: {formatGraceRemaining(c.retraction_grace_until)}</span>
                                                        )}
                                                    </div>
                                                </div>
                                                <div className="form-group" style={{ marginBottom: '8px' }}>
                                                    <input
                                                        type="text"
                                                        value={retractReason}
                                                        onChange={(e) => setRetractReason(e.target.value)}
                                                        placeholder="Reason for retraction (optional)"
                                                        className="compose-retract-reason"
                                                    />
                                                </div>
                                                {retractError && (
                                                    <div className="compose-retract-error">{retractError}</div>
                                                )}
                                                <div className="compose-retract-actions">
                                                    <button
                                                        className="compose-retract-cancel"
                                                        onClick={() => {
                                                            setRetractConfirmId(null);
                                                            setRetractError(null);
                                                        }}
                                                        type="button"
                                                    >
                                                        Cancel
                                                    </button>
                                                    <button
                                                        className="compose-retract-confirm"
                                                        onClick={() => handleRetract(c)}
                                                        disabled={retractingId === contribId}
                                                        type="button"
                                                    >
                                                        {retractingId === contribId ? 'Retracting...' : 'Confirm Retract'}
                                                    </button>
                                                </div>
                                            </div>
                                        )}
                                    </div>
                                );
                            })}
                        </div>
                    )}

                    {/* Pagination */}
                    {totalPages > 1 && !loadingContributions && (
                        <div className="compose-actions" style={{ justifyContent: 'center', gap: '8px', marginTop: '12px' }}>
                            <button
                                className="compose-success-new-btn"
                                onClick={() => {
                                    const p = contributionsPage - 1;
                                    setContributionsPage(p);
                                    fetchMyContributions(p, contributionStatusFilter);
                                }}
                                disabled={contributionsPage <= 1}
                                type="button"
                            >
                                Previous
                            </button>
                            <span style={{ alignSelf: 'center', fontSize: '0.9em', opacity: 0.7 }}>
                                Page {contributionsPage} of {totalPages} ({contributionsTotal} total)
                            </span>
                            <button
                                className="compose-success-new-btn"
                                onClick={() => {
                                    const p = contributionsPage + 1;
                                    setContributionsPage(p);
                                    fetchMyContributions(p, contributionStatusFilter);
                                }}
                                disabled={contributionsPage >= totalPages}
                                type="button"
                            >
                                Next
                            </button>
                        </div>
                    )}
                </div>
            )}
        </div>
    );
}
