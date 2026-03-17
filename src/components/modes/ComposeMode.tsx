import { useState, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

type ContributionType = 'analysis' | 'commentary' | 'correction' | 'investigation' | 'summary' | 'context' | 'editorial' | 'review' | 'tip';
type Urgency = 'normal' | 'priority';

interface TagInputProps {
    tags: string[];
    onAdd: (tag: string) => void;
    onRemove: (index: number) => void;
    placeholder: string;
}

// --- Tag Input Component ---

function TagInput({ tags, onAdd, onRemove, placeholder }: TagInputProps) {
    const [value, setValue] = useState('');

    const handleAdd = () => {
        const trimmed = value.trim().toLowerCase();
        if (trimmed && !tags.includes(trimmed)) {
            onAdd(trimmed);
            setValue('');
        }
    };

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            handleAdd();
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
            <div className="compose-tag-input-row">
                <input
                    type="text"
                    value={value}
                    onChange={(e) => setValue(e.target.value)}
                    onKeyDown={handleKeyDown}
                    placeholder={placeholder}
                    className="compose-tag-input"
                />
                <button
                    type="button"
                    className="compose-tag-add"
                    onClick={handleAdd}
                    disabled={!value.trim()}
                >
                    + Add
                </button>
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

// --- Component ---

export function ComposeMode() {
    const { operatorApiCall } = useAppContext();

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
    const [activeSection, setActiveSection] = useState<'contribution' | 'request'>('contribution');

    // --- Handlers ---

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
            setPublishResult({
                success: true,
                message: 'Contribution published to the graph.',
                id: data?.contribution_id || data?.id,
            });
            // Reset form
            setTitle('');
            setBody('');
            setTopics([]);
            setTags([]);
            setTargetContributionId('');
            setShowPreview(false);
        } catch (err: any) {
            setPublishError(err?.message || 'Failed to publish contribution. Please try again.');
        } finally {
            setPublishing(false);
        }
    }, [title, body, contribType, topics, tags, targetContributionId, operatorApiCall]);

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
            </div>

            {/* === Human Contribution Form === */}
            {activeSection === 'contribution' && (
                <div className="compose-form">
                    <div className="compose-form-header">
                        <h3>Write a Contribution</h3>
                        <p className="compose-form-desc">Publish a human-authored contribution to the knowledge graph.</p>
                    </div>

                    {/* Success / Error */}
                    {publishResult && (
                        <div className="compose-result compose-result-success">
                            <span>{publishResult.message}</span>
                            {publishResult.id && (
                                <span className="compose-result-id">ID: {publishResult.id}</span>
                            )}
                        </div>
                    )}
                    {publishError && (
                        <div className="compose-result compose-result-error">
                            <span>{publishError}</span>
                            <button className="compose-retry-btn" onClick={handlePublish}>Retry</button>
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

                    {/* Body */}
                    <div className="form-group">
                        <label>Body</label>
                        {showPreview ? (
                            <div className="compose-preview">
                                <div className="compose-preview-content">{body}</div>
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

                    {/* Type */}
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
                    </div>

                    {/* Topics */}
                    <div className="form-group">
                        <label>Topics</label>
                        <TagInput
                            tags={topics}
                            onAdd={(tag) => setTopics(prev => [...prev, tag])}
                            onRemove={(i) => setTopics(prev => prev.filter((_, idx) => idx !== i))}
                            placeholder="Add a topic"
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
                            className="compose-preview-btn"
                            onClick={() => setShowPreview(!showPreview)}
                            disabled={!body.trim()}
                        >
                            {showPreview ? 'Edit' : 'Preview'}
                        </button>
                        <button
                            className="compose-publish-btn"
                            onClick={handlePublish}
                            disabled={publishing || !title.trim() || !body.trim()}
                        >
                            {publishing ? 'Publishing...' : 'Publish to Graph'}
                        </button>
                    </div>
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

                    {/* Type */}
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
        </div>
    );
}
