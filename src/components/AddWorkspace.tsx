import { useState, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { BuildProgress } from './BuildProgress';
import { VineBuildProgress } from './VineBuildProgress';

const NOOP = () => {};

interface AddWorkspaceProps {
    onComplete: () => void;
    onCancel: () => void;
}

type Step = 'directory' | 'content-type' | 'vine-dirs' | 'configure' | 'question' | 'confirm' | 'building';

const PYRAMID_API_BASE = 'http://localhost:8765';

const DEFAULT_QUESTIONS: Record<string, string> = {
    code: "What are the key systems, patterns, and architecture of this codebase?",
    document: "What are the key concepts, decisions, and relationships in these documents?",
    conversation: "What are the key themes, decisions, and evolution across these conversations?",
};

const DEFAULT_IGNORES = [
    'node_modules', '.git', 'target', 'dist', 'build', '.next',
    '__pycache__', '.vscode', '.idea', 'coverage', '.cache',
    '.DS_Store', '.env', 'vendor', 'pkg',
];

function slugify(name: string): string {
    return name
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 64);
}

export function AddWorkspace({ onComplete, onCancel }: AddWorkspaceProps) {
    const [step, setStep] = useState<Step>('directory');
    const [paths, setPaths] = useState<string[]>([]);
    const [contentType, setContentType] = useState<'code' | 'document' | 'conversation' | 'vine' | null>(null);
    const [vinePastePath, setVinePastePath] = useState('');
    const [customIgnores, setCustomIgnores] = useState('');
    const [slug, setSlug] = useState('');
    const [creating, setCreating] = useState(false);
    const [apexQuestion, setApexQuestion] = useState('');
    const [error, setError] = useState<string | null>(null);
    // Model profile selector — populated lazily from pyramid_list_profiles.
    // Empty string means "use the operator's current default".
    const [profiles, setProfiles] = useState<string[]>([]);
    const [selectedProfile, setSelectedProfile] = useState<string>('');
    const [profilesError, setProfilesError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        invoke<string[]>('pyramid_list_profiles')
            .then(list => {
                if (cancelled) return;
                setProfiles(list);
            })
            .catch(err => {
                if (cancelled) return;
                setProfilesError(String(err));
            });
        return () => { cancelled = true; };
    }, []);

    const handlePickDirectory = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Select Workspace Directory',
            });
            if (selected) {
                const newPath = selected as string;
                setPaths(prev => {
                    // Don't add duplicates
                    if (prev.includes(newPath)) return prev;
                    const updated = [...prev, newPath];
                    // Auto-generate slug from the first directory name
                    if (updated.length === 1) {
                        const parts = newPath.split('/');
                        const folderName = parts[parts.length - 1] || parts[parts.length - 2] || 'workspace';
                        setSlug(slugify(folderName));
                    }
                    return updated;
                });
                setStep('content-type');
            }
        } catch (err) {
            setError(String(err));
        }
    }, []);

    const handleAddDirectory = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Add Another Directory',
            });
            if (selected) {
                const newPath = selected as string;
                setPaths(prev => {
                    if (prev.includes(newPath)) return prev;
                    return [...prev, newPath];
                });
            }
        } catch (err) {
            setError(String(err));
        }
    }, []);

    const handleRemovePath = useCallback((index: number) => {
        setPaths(prev => {
            const updated = prev.filter((_, i) => i !== index);
            // If we removed the first path, update the slug from the new first path
            if (index === 0 && updated.length > 0) {
                const parts = updated[0].split('/');
                const folderName = parts[parts.length - 1] || parts[parts.length - 2] || 'workspace';
                setSlug(slugify(folderName));
            }
            // If no paths left, go back to directory step
            if (updated.length === 0) {
                setStep('directory');
                setSlug('');
            }
            return updated;
        });
    }, []);

    const handlePickConversation = useCallback(async () => {
        try {
            // Default to Claude Code projects directory
            const homeDir = await invoke<string>('get_home_dir').catch(() => '');
            const claudeDir = homeDir ? `${homeDir}/.claude/projects` : undefined;

            const selected = await open({
                directory: false,
                title: 'Select Conversation JSONL',
                defaultPath: claudeDir,
                filters: [{ name: 'JSONL', extensions: ['jsonl'] }],
            });
            if (selected) {
                const filePath = selected as string;
                addPathAndAutoSlug(filePath);
                setContentType('conversation');
                setStep('question');  // Skip configure for conversations (no ignore patterns needed)
            }
        } catch (err) {
            setError(String(err));
        }
    }, []);

    // Helper to add a path and auto-generate slug from it
    const addPathAndAutoSlug = useCallback((newPath: string) => {
        setPaths(prev => {
            if (prev.includes(newPath)) return prev;
            const updated = [...prev, newPath];
            if (updated.length === 1) {
                const parts = newPath.split('/');
                const name = (parts[parts.length - 1] || parts[parts.length - 2] || 'workspace').replace('.jsonl', '');
                setSlug(slugify(name));
            }
            return updated;
        });
    }, []);

    const handleContentTypeSelect = useCallback((type: 'code' | 'document' | 'conversation' | 'vine') => {
        setContentType(type);
        setApexQuestion(DEFAULT_QUESTIONS[type] || '');
        if (type === 'vine') {
            // Go to vine directory selection step
            // Clear paths from step 1 since vine uses its own directory list
            setPaths([]);
            setSlug('');
            setStep('vine-dirs');
        } else if (type === 'conversation') {
            // If path already pasted and it's a .jsonl, skip picker
            if (paths.length > 0 && paths[0].endsWith('.jsonl')) {
                setStep('question');
            } else {
                handlePickConversation();
            }
        } else {
            setStep('configure');
        }
    }, [handlePickConversation, paths]);

    const handleVinePickDirectory = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Select JSONL Directory for Vine',
                multiple: true,
            });
            if (selected) {
                const newPaths = Array.isArray(selected) ? selected : [selected];
                setPaths(prev => {
                    const combined = [...prev];
                    for (const p of newPaths) {
                        if (!combined.includes(p)) combined.push(p);
                    }
                    if (combined.length > 0 && !slug) {
                        const parts = combined[0].split('/');
                        const folderName = parts[parts.length - 1] || parts[parts.length - 2] || 'vine';
                        setSlug(slugify(folderName + '-vine'));
                    }
                    return combined;
                });
            }
        } catch (err) {
            setError(String(err));
        }
    }, [slug]);

    const handleVineAddPastePath = useCallback(() => {
        const val = vinePastePath.trim();
        if (!val) return;
        setPaths(prev => {
            if (prev.includes(val)) return prev;
            const updated = [...prev, val];
            if (updated.length === 1 && !slug) {
                const parts = val.split('/');
                const folderName = parts[parts.length - 1] || parts[parts.length - 2] || 'vine';
                setSlug(slugify(folderName + '-vine'));
            }
            return updated;
        });
        setVinePastePath('');
    }, [vinePastePath, slug]);

    const handleVineCreate = useCallback(async () => {
        if (paths.length === 0 || !slug) return;
        setCreating(true);
        setError(null);

        try {
            // Create the slug via Tauri so it appears in the dashboard
            const sourcePath = paths.join(';');
            await invoke('pyramid_create_slug', {
                slug,
                contentType: 'vine',
                sourcePath,
            });

            // Kick off vine build via Tauri command
            await invoke('pyramid_vine_build', {
                vineSlug: slug,
                jsonlDirs: paths,
            });

            setStep('building');
        } catch (err) {
            setError(String(err));
        } finally {
            setCreating(false);
        }
    }, [paths, slug]);

    const handleContinueToConfirm = useCallback(() => {
        setStep('question');
    }, []);

    const handleCreate = useCallback(async (andBuild: boolean) => {
        if (paths.length === 0 || !contentType || !slug) return;
        setCreating(true);
        setError(null);

        try {
            // Single path as plain string; multiple paths as JSON array
            const sourcePath = paths.length === 1 ? paths[0] : JSON.stringify(paths);

            // Create the slug
            await invoke('pyramid_create_slug', {
                slug,
                contentType,
                sourcePath,
            });

            // Ingest content
            await invoke('pyramid_ingest', { slug });

            if (andBuild) {
                // Apply the selected model profile (if any) BEFORE the build
                // starts so the build pipeline picks up the new model selection.
                // Empty string = use current default; don't apply.
                if (selectedProfile && selectedProfile.trim()) {
                    try {
                        await invoke('pyramid_apply_profile', { profile: selectedProfile });
                    } catch (e) {
                        setError(`Failed to apply profile "${selectedProfile}": ${e}`);
                        setCreating(false);
                        return;
                    }
                }

                // Start question build (modern pipeline)
                await invoke('pyramid_question_build', {
                    slug,
                    question: apexQuestion,
                    granularity: 3,
                    maxDepth: 3,
                });
                setStep('building');
            } else {
                onComplete();
            }
        } catch (err) {
            setError(String(err));
        } finally {
            setCreating(false);
        }
    }, [paths, contentType, slug, apexQuestion, selectedProfile, onComplete]);

    const allIgnores = [
        ...DEFAULT_IGNORES,
        ...customIgnores.split('\n').map(s => s.trim()).filter(Boolean),
    ];

    return (
        <div className="add-workspace-panel">
            {/* Step indicator */}
            <div className="workspace-steps">
                {(contentType === 'vine'
                    ? (['directory', 'content-type', 'vine-dirs', 'confirm'] as Step[])
                    : contentType === 'conversation'
                    ? (['directory', 'content-type', 'question', 'confirm'] as Step[])
                    : (['directory', 'content-type', 'configure', 'question', 'confirm'] as Step[])
                ).map((s, i) => {
                    const stepOrder = contentType === 'vine'
                        ? ['directory', 'content-type', 'vine-dirs', 'confirm']
                        : contentType === 'conversation'
                        ? ['directory', 'content-type', 'question', 'confirm']
                        : ['directory', 'content-type', 'configure', 'question', 'confirm'];
                    const currentIndex = stepOrder.indexOf(step);
                    return (
                        <div
                            key={s}
                            className={`workspace-step ${step === s ? 'active' : ''} ${
                                currentIndex > i ? 'done' : ''
                            }`}
                        >
                            <span className="step-number">{i + 1}</span>
                            <span className="step-label">
                                {s === 'directory' ? (contentType === 'vine' ? 'Source' : 'Directories') : s === 'content-type' ? 'Type' : s === 'vine-dirs' ? 'Folders' : s === 'configure' ? 'Configure' : s === 'question' ? 'Question' : 'Confirm'}
                            </span>
                        </div>
                    );
                })}
            </div>

            {error && (
                <div className="workspace-error">
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            )}

            {/* Step 1: Pick Directories */}
            {step === 'directory' && (
                <div className="workspace-step-content">
                    <h2>Select Workspace</h2>
                    <p className="step-description">
                        Browse for directories or paste a path directly.
                    </p>

                    {paths.length > 0 && (
                        <div className="selected-paths" style={{ marginBottom: '12px' }}>
                            {paths.map((p, i) => (
                                <div key={p} className="selected-path-row">
                                    <span className="selected-path-text">{p}</span>
                                    <button className="btn btn-ghost btn-sm" onClick={() => handleRemovePath(i)} title="Remove">&times;</button>
                                </div>
                            ))}
                        </div>
                    )}

                    <div style={{ display: 'flex', gap: '8px', marginBottom: '12px' }}>
                        <input
                            type="text"
                            placeholder="Paste a path (file or directory)..."
                            className="input"
                            style={{ flex: 1 }}
                            onKeyDown={(e) => {
                                if (e.key === 'Enter') {
                                    const val = (e.target as HTMLInputElement).value.trim();
                                    if (val) {
                                        setPaths(prev => {
                                            if (prev.includes(val)) return prev;
                                            const updated = [...prev, val];
                                            if (updated.length === 1) {
                                                const parts = val.split('/');
                                                const name = (parts[parts.length - 1] || parts[parts.length - 2] || 'workspace').replace('.jsonl', '');
                                                setSlug(slugify(name));
                                            }
                                            return updated;
                                        });
                                        (e.target as HTMLInputElement).value = '';
                                        setStep('content-type');
                                    }
                                }
                            }}
                        />
                        <button className="btn btn-primary" onClick={handlePickDirectory}>
                            Browse...
                        </button>
                    </div>

                    {paths.length > 0 && (
                        <button className="btn btn-primary" onClick={() => setStep('content-type')} style={{ marginRight: '8px' }}>
                            Next
                        </button>
                    )}
                    <button className="btn btn-ghost" onClick={onCancel}>
                        Cancel
                    </button>
                </div>
            )}

            {/* Step 2: Content Type */}
            {step === 'content-type' && (
                <div className="workspace-step-content">
                    <h2>Choose Content Type</h2>

                    {/* Show selected directories */}
                    <div className="selected-paths">
                        {paths.map((p, i) => (
                            <div key={p} className="selected-path-row">
                                <span className="selected-path-text">{p}</span>
                                <button
                                    className="btn btn-ghost btn-sm"
                                    onClick={() => handleRemovePath(i)}
                                    title="Remove directory"
                                >
                                    &times;
                                </button>
                            </div>
                        ))}
                        <button className="btn btn-ghost btn-sm" onClick={handleAddDirectory}>
                            + Add Another Directory
                        </button>
                    </div>

                    <div className="content-type-cards">
                        <button
                            className={`content-type-card ${contentType === 'code' ? 'selected' : ''}`}
                            onClick={() => handleContentTypeSelect('code')}
                        >
                            <div className="content-type-icon">&lt;/&gt;</div>
                            <div className="content-type-name">Code Project</div>
                            <div className="content-type-desc">
                                Source code repository. The pyramid will analyze imports,
                                functions, types, and module structure. Choose this for:
                                GitHub repos, app codebases, libraries.
                            </div>
                        </button>

                        <button
                            className={`content-type-card ${contentType === 'document' ? 'selected' : ''}`}
                            onClick={() => handleContentTypeSelect('document')}
                        >
                            <div className="content-type-icon">&#x1F4C4;</div>
                            <div className="content-type-name">Documents</div>
                            <div className="content-type-desc">
                                Written documents. The pyramid will analyze content, themes,
                                entities, and relationships. Choose this for: design docs,
                                research notes, creative writing, specifications.
                            </div>
                        </button>

                        <button
                            className={`content-type-card ${contentType === 'conversation' ? 'selected' : ''}`}
                            onClick={() => handleContentTypeSelect('conversation')}
                        >
                            <div className="content-type-icon">&#x1F4AC;</div>
                            <div className="content-type-name">Conversation</div>
                            <div className="content-type-desc">
                                AI conversation transcript (JSONL). The pyramid will run
                                forward and reverse passes to extract what was decided,
                                what was corrected, and what mattered. Choose this for:
                                Claude Code sessions, chat logs, design discussions.
                            </div>
                        </button>

                        <button
                            className={`content-type-card ${contentType === 'vine' ? 'selected' : ''}`}
                            onClick={() => handleContentTypeSelect('vine')}
                        >
                            <div className="content-type-icon">&#x1F347;</div>
                            <div className="content-type-name">Vine</div>
                            <div className="content-type-desc">
                                Conversation Vine &mdash; connects multiple conversation
                                sessions into a temporal meta-pyramid. Pick folders
                                containing Claude Code JSONL files.
                            </div>
                        </button>
                    </div>

                    <div className="content-type-notice">
                        This determines how your pyramid is built. Code projects get import
                        analysis, function extraction, and module clustering. Documents get
                        entity extraction and thematic grouping. Choose the one that matches
                        your content.
                    </div>

                    <div className="step-nav">
                        <button className="btn btn-ghost" onClick={() => setStep('directory')}>
                            Back
                        </button>
                    </div>
                </div>
            )}

            {/* Step 2b: Vine Directory Selection */}
            {step === 'vine-dirs' && (
                <div className="workspace-step-content">
                    <h2>Select JSONL Directories</h2>
                    <p className="step-description">
                        Pick folders containing Claude Code JSONL conversation files.
                        Each folder becomes a bunch in the vine.
                    </p>

                    {paths.length > 0 && (
                        <div className="selected-paths" style={{ marginBottom: '12px' }}>
                            {paths.map((p, i) => (
                                <div key={p} className="selected-path-row">
                                    <span className="selected-path-text">{p}</span>
                                    <button className="btn btn-ghost btn-sm" onClick={() => handleRemovePath(i)} title="Remove">&times;</button>
                                </div>
                            ))}
                        </div>
                    )}

                    <div style={{ display: 'flex', gap: '8px', marginBottom: '8px' }}>
                        <input
                            type="text"
                            placeholder="Paste a path (e.g. ~/.claude/projects/my-app)..."
                            className="input"
                            style={{ flex: 1 }}
                            value={vinePastePath}
                            onChange={(e) => setVinePastePath(e.target.value)}
                            onKeyDown={(e) => {
                                if (e.key === 'Enter') handleVineAddPastePath();
                            }}
                        />
                        <button
                            className="btn btn-secondary"
                            onClick={handleVineAddPastePath}
                            disabled={!vinePastePath.trim()}
                            title="Add path"
                        >
                            +
                        </button>
                        <button className="btn btn-primary" onClick={handleVinePickDirectory}>
                            Browse...
                        </button>
                    </div>

                    <div className="vine-hint">
                        Tip: Hidden folders like <code>.claude/</code> may not appear in the
                        file picker. Use <kbd>Cmd+Shift+.</kbd> to show them, or paste the path above.
                    </div>

                    <div className="step-nav" style={{ marginTop: '16px' }}>
                        <button className="btn btn-ghost" onClick={() => { setStep('content-type'); setContentType(null); }}>
                            Back
                        </button>
                        <button
                            className="btn btn-primary"
                            onClick={() => setStep('confirm')}
                            disabled={paths.length === 0}
                        >
                            Next
                        </button>
                    </div>
                </div>
            )}

            {/* Step 3: Configure Ignores */}
            {step === 'configure' && (
                <div className="workspace-step-content">
                    <h2>Configure Ignore Patterns</h2>
                    <p className="step-description">
                        These directories will be skipped during ingestion:
                    </p>

                    <div className="ignore-list">
                        {DEFAULT_IGNORES.map((ig) => (
                            <span key={ig} className="ignore-tag">{ig}</span>
                        ))}
                    </div>

                    <div className="custom-ignores">
                        <label className="field-label">Additional ignores (one per line):</label>
                        <textarea
                            className="ignore-input"
                            value={customIgnores}
                            onChange={(e) => setCustomIgnores(e.target.value)}
                            placeholder="e.g.&#10;test-fixtures&#10;.terraform"
                            rows={4}
                        />
                    </div>

                    <div className="step-nav">
                        <button className="btn btn-ghost" onClick={() => setStep('content-type')}>
                            Back
                        </button>
                        <button className="btn btn-primary" onClick={handleContinueToConfirm}>
                            Continue
                        </button>
                    </div>
                </div>
            )}

            {/* Step: Apex Question */}
            {step === 'question' && (
                <div className="workspace-step-content">
                    <h2>Apex Question</h2>
                    <p className="step-description">
                        What should this pyramid answer? The question YAML pipeline will decompose this into sub-questions and build structured understanding.
                    </p>
                    <textarea
                        className="input"
                        rows={3}
                        value={apexQuestion}
                        onChange={(e) => setApexQuestion(e.target.value)}
                        placeholder="e.g. What are the key systems and architecture of this codebase?"
                        style={{ width: '100%', resize: 'vertical', fontFamily: 'inherit' }}
                    />

                    {/* Model profile selector — applies a layered override to
                        the LLM config so this build uses a different model
                        cascade. Empty value = use the operator's current
                        default. Profiles are loaded from
                        ~/Library/Application Support/wire-node/profiles/
                        (with ~/.gemini/wire-node/profiles/ as a fallback). */}
                    <div style={{ marginTop: '16px' }}>
                        <label className="field-label" style={{ display: 'block', marginBottom: '4px' }}>
                            Model profile:
                        </label>
                        {profilesError ? (
                            <p className="hint" style={{ color: '#ef4444', margin: 0 }}>
                                Could not load profiles: {profilesError}
                            </p>
                        ) : profiles.length === 0 ? (
                            <p className="hint" style={{ margin: 0 }}>
                                No profiles found. Drop JSON profile files into
                                <code style={{ marginLeft: '4px' }}>~/Library/Application Support/wire-node/profiles/</code>
                                and refresh.
                            </p>
                        ) : (
                            <>
                                <select
                                    className="input"
                                    value={selectedProfile}
                                    onChange={(e) => setSelectedProfile(e.target.value)}
                                    style={{ width: '100%' }}
                                >
                                    <option value="">(use current default)</option>
                                    {profiles.map(p => (
                                        <option key={p} value={p}>{p}</option>
                                    ))}
                                </select>
                                <p className="hint" style={{ marginTop: '4px', fontSize: '0.85em', opacity: 0.7 }}>
                                    Profiles are layered overrides on the LLM cascade. Selecting one applies it
                                    in-memory before this build starts and stays active for subsequent builds
                                    until you change it again. Edit profile JSON files on disk to add new options.
                                </p>
                            </>
                        )}
                    </div>

                    <div style={{ marginTop: '12px', display: 'flex', gap: '8px' }}>
                        <button className="btn btn-ghost" onClick={() => {
                            if (contentType === 'conversation') setStep('content-type');
                            else setStep('configure');
                        }}>
                            Back
                        </button>
                        <button
                            className="btn btn-primary"
                            onClick={() => setStep('confirm')}
                            disabled={!apexQuestion.trim()}
                        >
                            Next
                        </button>
                    </div>
                </div>
            )}

            {/* Step 4: Name & Confirm */}
            {step === 'confirm' && (
                <div className="workspace-step-content">
                    <h2>Name &amp; Confirm</h2>

                    <div className="confirm-field">
                        <label className="field-label">Slug name:</label>
                        <input
                            type="text"
                            className="slug-input"
                            value={slug}
                            onChange={(e) => setSlug(slugify(e.target.value))}
                            placeholder="my-project"
                        />
                    </div>

                    <div className="confirm-summary">
                        <div className="summary-row">
                            <span className="summary-label">Source{paths.length > 1 ? 's' : ''}:</span>
                            <span className="summary-value">
                                {paths.map((p, i) => (
                                    <div key={p} className="summary-path">{p}</div>
                                ))}
                            </span>
                        </div>
                        <div className="summary-row">
                            <span className="summary-label">Type:</span>
                            <span className="summary-value">
                                {contentType === 'code' ? 'Code Project' : contentType === 'vine' ? 'Conversation Vine' : contentType === 'conversation' ? 'Conversation' : 'Documents'}
                            </span>
                        </div>
                        {contentType !== 'vine' && (
                            <div className="summary-row">
                                <span className="summary-label">Question:</span>
                                <span className="summary-value">{apexQuestion}</span>
                            </div>
                        )}
                        {contentType !== 'vine' && contentType !== 'conversation' && (
                            <div className="summary-row">
                                <span className="summary-label">Ignoring:</span>
                                <span className="summary-value">{allIgnores.length} patterns</span>
                            </div>
                        )}
                        {contentType === 'vine' && (
                            <div className="summary-row">
                                <span className="summary-label">Directories:</span>
                                <span className="summary-value">{paths.length} folder{paths.length !== 1 ? 's' : ''}</span>
                            </div>
                        )}
                    </div>

                    <div className="confirm-estimate">
                        Estimated: build time depends on project size and API response times.
                    </div>

                    <div className="step-nav">
                        <button className="btn btn-ghost" onClick={() => {
                            if (contentType === 'vine') setStep('vine-dirs');
                            else setStep('question');
                        }}>
                            Back
                        </button>
                        {contentType === 'vine' ? (
                            <button
                                className="btn btn-primary"
                                onClick={handleVineCreate}
                                disabled={creating || !slug || paths.length === 0}
                            >
                                {creating ? 'Creating...' : 'Create & Build Vine'}
                            </button>
                        ) : (
                            <>
                                <button
                                    className="btn btn-secondary"
                                    onClick={() => handleCreate(false)}
                                    disabled={creating || !slug}
                                >
                                    {creating ? 'Creating...' : 'Create Only'}
                                </button>
                                <button
                                    className="btn btn-primary"
                                    onClick={() => handleCreate(true)}
                                    disabled={creating || !slug}
                                >
                                    {creating ? 'Creating...' : 'Create & Build'}
                                </button>
                            </>
                        )}
                    </div>
                </div>
            )}

            {/* Step 5: Building */}
            {step === 'building' && contentType === 'vine' && (
                <VineBuildProgress
                    slug={slug}
                    onComplete={NOOP}
                    onClose={onComplete}
                />
            )}
            {step === 'building' && contentType !== 'vine' && (
                <BuildProgress
                    slug={slug}
                    onComplete={NOOP}
                    onClose={onComplete}
                />
            )}
        </div>
    );
}
