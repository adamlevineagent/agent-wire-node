import { useState, useCallback, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { BuildProgress } from './BuildProgress';
import { VineBuildProgress } from './VineBuildProgress';

const NOOP = () => {};

interface AddWorkspaceProps {
    onComplete: () => void;
    onCancel: () => void;
}

type Step =
    | 'directory'
    | 'content-type'
    | 'conversation-preset'
    | 'vine-dirs'
    | 'configure'
    | 'question'
    | 'preview'
    | 'confirm'
    | 'building'
    // Phase 17: recursive folder ingestion wizard
    | 'folder-ingest-pick'
    | 'folder-ingest-review';

// Phase 17 IPC shapes — must match `folder_ingestion.rs`.
interface ClaudeCodeConversationDir {
    encoded_path: string;
    absolute_path: string;
    jsonl_count: number;
    earliest_mtime: string | null;
    latest_mtime: string | null;
    is_main: boolean;
    is_worktree: boolean;
}

type IngestionOperation =
    | { op: 'create_pyramid'; slug: string; content_type: string; source_path: string }
    | { op: 'create_vine'; slug: string; source_path: string }
    | { op: 'add_child_to_vine'; vine_slug: string; child_slug: string; position: number; child_type: string }
    | { op: 'register_dadbear_config'; slug: string; source_path: string; content_type: string; scan_interval_secs: number }
    | { op: 'register_claude_code_pyramid'; slug: string; source_path: string; is_main: boolean; is_worktree: boolean };

interface IngestionPlan {
    operations: IngestionOperation[];
    root_slug: string | null;
    root_source_path: string;
    total_files: number;
    total_ignored: number;
}

interface IngestionResult {
    pyramids_created: string[];
    vines_created: string[];
    dadbear_configs: string[];
    claude_code_pyramids: string[];
    compositions_added: number;
    root_slug: string | null;
    errors: string[];
}

interface IngestFolderOutput {
    plan: IngestionPlan;
    result: IngestionResult | null;
}

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

/** Chain preset for conversation pyramids */
type ConversationPreset = 'episodic' | 'retro';

const CONVERSATION_PRESETS: Record<ConversationPreset, {
    label: string;
    chainId: string;
    description: string;
}> = {
    episodic: {
        label: 'Episodic Memory',
        chainId: 'conversation-episodic',
        description: 'Builds a cognitive substrate for AI agent continuity. Forward + reverse temporal passes extract decisions, vocabulary, and commitments. The resulting pyramid serves as persistent memory that agents load at session boot.',
    },
    retro: {
        label: 'Retro / Meta-Learning',
        chainId: 'conversation-chronological',
        description: 'Chronological analysis optimized for human review. Extracts themes, turning points, corrections, and lessons learned. Use this for retrospectives, post-mortems, and pattern discovery across sessions.',
    },
};

/** Shape returned by the preview HTTP endpoint */
interface BuildPreviewResult {
    source_path: string;
    content_type: string;
    chain_id: string;
    file_count: number;
    estimated_total_tokens: number;
    estimated_pyramids: number;
    estimated_layers: number;
    estimated_nodes: number;
    estimated_cost_dollars: number;
    estimated_time_seconds: number;
    estimated_disk_bytes: number;
    warnings: PreviewWarning[];
    generated_at: string;
}

interface PreviewWarning {
    level: 'info' | 'warning' | 'error';
    file_path?: string;
    message: string;
}

function slugify(name: string): string {
    return name
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 64);
}

/** Format seconds into a human-friendly string like "~2-3 hours" or "~45 minutes" */
function formatEstimatedTime(seconds: number): string {
    if (seconds < 60) return `~${seconds} seconds`;
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `~${minutes} minutes`;
    const hours = seconds / 3600;
    const low = Math.floor(hours);
    const high = Math.ceil(hours);
    if (low === high) return `~${low} hour${low !== 1 ? 's' : ''}`;
    return `~${low}-${high} hours`;
}

/** Format bytes into human-readable size */
function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** Format USD cost */
function formatCost(dollars: number): string {
    if (dollars < 0.01) return '<$0.01';
    return `~$${dollars.toFixed(2)}`;
}

/** Determine file extension label from content type */
function fileTypeLabel(contentType: string): string {
    switch (contentType) {
        case 'conversation': return '.jsonl files';
        case 'code': return 'source files';
        case 'document': return 'document files';
        default: return 'files';
    }
}

export function AddWorkspace({ onComplete, onCancel }: AddWorkspaceProps) {
    const [step, setStep] = useState<Step>('directory');
    const [paths, setPaths] = useState<string[]>([]);
    const [contentType, setContentType] = useState<'code' | 'document' | 'conversation' | 'vine' | null>(null);
    const [conversationPreset, setConversationPreset] = useState<ConversationPreset>('episodic');
    const [vinePastePath, setVinePastePath] = useState('');
    const [customIgnores, setCustomIgnores] = useState('');
    const [slug, setSlug] = useState('');
    const [creating, setCreating] = useState(false);
    const [apexQuestion, setApexQuestion] = useState('');
    const [error, setError] = useState<string | null>(null);
    // Preview state
    const [previewLoading, setPreviewLoading] = useState(false);
    const [previewResult, setPreviewResult] = useState<BuildPreviewResult | null>(null);
    const [committing, setCommitting] = useState(false);
    // Phase 17: folder ingestion wizard state
    const [folderIngestPath, setFolderIngestPath] = useState<string>('');
    const [ccDirs, setCcDirs] = useState<ClaudeCodeConversationDir[]>([]);
    const [ccLoading, setCcLoading] = useState(false);
    const [includeClaudeCode, setIncludeClaudeCode] = useState(true);
    // Per-call override for the conversation scan root. `null` means
    // "use the active folder_ingestion_heuristics contribution's
    // default" (typically `~/.claude/projects`). Set via the "Change…"
    // button; cleared via "Reset to default" so users can flip back
    // without remembering the default path.
    const [ccOverridePath, setCcOverridePath] = useState<string | null>(null);
    const [folderPlan, setFolderPlan] = useState<IngestionPlan | null>(null);
    const [folderPlanLoading, setFolderPlanLoading] = useState(false);
    const [folderIngestResult, setFolderIngestResult] = useState<IngestionResult | null>(null);
    const [folderIngestError, setFolderIngestError] = useState<string | null>(null);
    // Model profile selector
    const [profiles, setProfiles] = useState<string[]>([]);
    const [selectedProfile, setSelectedProfile] = useState<string>('');
    const [profilesError, setProfilesError] = useState<string | null>(null);

    // Auth token for HTTP fetches
    const [authToken, setAuthToken] = useState('');
    useEffect(() => {
        invoke<string>('pyramid_get_auth_token').then(setAuthToken).catch(() => {});
    }, []);
    const authHeaders = useMemo(() => {
        const h: Record<string, string> = { 'Content-Type': 'application/json' };
        if (authToken) h['Authorization'] = `Bearer ${authToken}`;
        return h;
    }, [authToken]);

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

    /** Resolve the effective chain ID for the current configuration */
    const getEffectiveChainId = useCallback((): string => {
        if (contentType === 'conversation') {
            return CONVERSATION_PRESETS[conversationPreset].chainId;
        }
        // For code/document, the default chain is resolved server-side
        return 'question-pipeline';
    }, [contentType, conversationPreset]);

    const handlePickDirectory = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Select Workspace Directory',
            });
            if (selected) {
                const newPath = selected as string;
                setPaths(prev => {
                    if (prev.includes(newPath)) return prev;
                    const updated = [...prev, newPath];
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

    // ── Phase 17: Folder ingestion wizard handlers ──────────────────────

    const handlePickFolderForIngestion = useCallback(async () => {
        try {
            setFolderIngestError(null);
            const selected = await open({
                directory: true,
                title: 'Select a folder to ingest recursively',
            });
            if (!selected) return;
            const path = selected as string;
            setFolderIngestPath(path);
            setFolderPlan(null);
            setFolderIngestResult(null);
            // Fresh target — reset any stale override from a prior folder.
            setCcOverridePath(null);

            // Fetch Claude Code conversation dirs for this folder.
            setCcLoading(true);
            try {
                const dirs = await invoke<ClaudeCodeConversationDir[]>(
                    'pyramid_find_claude_code_conversations',
                    { targetFolder: path, conversationPathOverride: null },
                );
                setCcDirs(dirs);
                // Spec: default ON only when at least one match is found.
                setIncludeClaudeCode(dirs.length > 0);
            } catch (e) {
                setCcDirs([]);
                setIncludeClaudeCode(false);
                console.warn('find_claude_code_conversations failed', e);
            } finally {
                setCcLoading(false);
            }

            // Run a dry-run plan.
            setFolderPlanLoading(true);
            try {
                const output = await invoke<IngestFolderOutput>('pyramid_ingest_folder', {
                    input: {
                        target_folder: path,
                        include_claude_code: true,
                        dry_run: true,
                        conversation_path_override: null,
                    },
                });
                setFolderPlan(output.plan);
            } catch (e) {
                setFolderIngestError(String(e));
            } finally {
                setFolderPlanLoading(false);
            }

            setStep('folder-ingest-review');
        } catch (err) {
            setFolderIngestError(String(err));
        }
    }, []);

    /**
     * Re-scan Claude Code conversation dirs and re-plan after the user
     * changes (or resets) the conversation-path override. Shared by the
     * "Change…" picker and the "Reset to default" button.
     */
    const rescanClaudeCodeWithOverride = useCallback(
        async (overridePath: string | null) => {
            if (!folderIngestPath) return;
            setCcLoading(true);
            setFolderIngestError(null);
            let nextDirs: ClaudeCodeConversationDir[] = [];
            try {
                nextDirs = await invoke<ClaudeCodeConversationDir[]>(
                    'pyramid_find_claude_code_conversations',
                    {
                        targetFolder: folderIngestPath,
                        conversationPathOverride: overridePath,
                    },
                );
                setCcDirs(nextDirs);
                // Default ON only if the new scan found matches.
                setIncludeClaudeCode(nextDirs.length > 0);
            } catch (e) {
                setCcDirs([]);
                setIncludeClaudeCode(false);
                setFolderIngestError(
                    `find_claude_code_conversations failed: ${String(e)}`,
                );
            } finally {
                setCcLoading(false);
            }

            // Replan with the new override so the UI preview reflects
            // which CC pyramids will actually land.
            setFolderPlanLoading(true);
            try {
                const output = await invoke<IngestFolderOutput>(
                    'pyramid_ingest_folder',
                    {
                        input: {
                            target_folder: folderIngestPath,
                            include_claude_code: nextDirs.length > 0,
                            dry_run: true,
                            conversation_path_override: overridePath,
                        },
                    },
                );
                setFolderPlan(output.plan);
            } catch (e) {
                setFolderIngestError(String(e));
            } finally {
                setFolderPlanLoading(false);
            }
        },
        [folderIngestPath],
    );

    const handlePickCcOverrideFolder = useCallback(async () => {
        try {
            const selected = await open({
                directory: true,
                title: 'Select conversation folder',
            });
            if (!selected) return;
            const path = selected as string;
            setCcOverridePath(path);
            await rescanClaudeCodeWithOverride(path);
        } catch (err) {
            setFolderIngestError(String(err));
        }
    }, [rescanClaudeCodeWithOverride]);

    const handleResetCcOverride = useCallback(async () => {
        setCcOverridePath(null);
        await rescanClaudeCodeWithOverride(null);
    }, [rescanClaudeCodeWithOverride]);

    // Re-plan when includeClaudeCode toggle changes, so the user
    // sees the CC pyramids land/disappear in the preview.
    const handleToggleIncludeClaudeCode = useCallback(
        async (next: boolean) => {
            setIncludeClaudeCode(next);
            if (!folderIngestPath) return;
            setFolderPlanLoading(true);
            setFolderIngestError(null);
            try {
                const output = await invoke<IngestFolderOutput>('pyramid_ingest_folder', {
                    input: {
                        target_folder: folderIngestPath,
                        include_claude_code: next,
                        dry_run: true,
                        conversation_path_override: ccOverridePath,
                    },
                });
                setFolderPlan(output.plan);
            } catch (e) {
                setFolderIngestError(String(e));
            } finally {
                setFolderPlanLoading(false);
            }
        },
        [folderIngestPath, ccOverridePath],
    );

    const handleStartFolderIngestion = useCallback(async () => {
        if (!folderIngestPath) return;
        setCommitting(true);
        setFolderIngestError(null);
        try {
            const output = await invoke<IngestFolderOutput>('pyramid_ingest_folder', {
                input: {
                    target_folder: folderIngestPath,
                    include_claude_code: includeClaudeCode,
                    dry_run: false,
                    conversation_path_override: ccOverridePath,
                },
            });
            setFolderPlan(output.plan);
            setFolderIngestResult(output.result);
            // If everything succeeded, close the wizard.
            if (output.result && output.result.errors.length === 0) {
                onComplete();
            }
        } catch (e) {
            setFolderIngestError(String(e));
        } finally {
            setCommitting(false);
        }
    }, [folderIngestPath, includeClaudeCode, ccOverridePath, onComplete]);

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
            if (index === 0 && updated.length > 0) {
                const parts = updated[0].split('/');
                const folderName = parts[parts.length - 1] || parts[parts.length - 2] || 'workspace';
                setSlug(slugify(folderName));
            }
            if (updated.length === 0) {
                setStep('directory');
                setSlug('');
            }
            return updated;
        });
    }, []);

    const handlePickConversation = useCallback(async () => {
        try {
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
                setStep('conversation-preset');
            }
        } catch (err) {
            setError(String(err));
        }
    }, []);

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
            setPaths([]);
            setSlug('');
            setStep('vine-dirs');
        } else if (type === 'conversation') {
            // Conversation: go to preset selector first
            if (paths.length > 0 && paths[0].endsWith('.jsonl')) {
                setStep('conversation-preset');
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
            const sourcePath = paths.join(';');
            await invoke('pyramid_create_slug', {
                slug,
                contentType: 'vine',
                sourcePath,
            });

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

    // ── Preview-then-commit flow ──────────────────────────────────────────

    /** Generate a preview by calling the HTTP endpoint */
    const handlePreview = useCallback(async () => {
        if (paths.length === 0 || !contentType || !slug) return;
        setPreviewLoading(true);
        setPreviewResult(null);
        setError(null);

        try {
            // Ensure the slug exists before previewing
            const sourcePath = paths.length === 1 ? paths[0] : JSON.stringify(paths);

            // Create slug if it doesn't exist yet (preview needs it)
            try {
                await invoke('pyramid_create_slug', {
                    slug,
                    contentType,
                    sourcePath,
                });
            } catch (_e) {
                // Slug may already exist — that's fine
            }

            // Call the preview HTTP endpoint
            const chainId = getEffectiveChainId();
            const response = await fetch(`${PYRAMID_API_BASE}/pyramid/${slug}/preview`, {
                method: 'POST',
                headers: authHeaders,
                body: JSON.stringify({
                    source_path: sourcePath,
                    content_type: contentType,
                    chain_id: chainId,
                }),
            });

            if (!response.ok) {
                const errBody = await response.text();
                throw new Error(`Preview failed (${response.status}): ${errBody}`);
            }

            const result = await response.json() as BuildPreviewResult;
            setPreviewResult(result);
            setStep('preview');
        } catch (err) {
            setError(String(err));
        } finally {
            setPreviewLoading(false);
        }
    }, [paths, contentType, slug, getEffectiveChainId]);

    /** Commit after preview — creates DADBEAR config and starts background processing */
    const handleCommit = useCallback(async () => {
        if (!previewResult || !slug || !contentType) return;
        setCommitting(true);
        setError(null);

        try {
            const sourcePath = paths.length === 1 ? paths[0] : JSON.stringify(paths);
            const chainId = getEffectiveChainId();

            // Apply model profile if selected
            if (selectedProfile && selectedProfile.trim()) {
                try {
                    await invoke('pyramid_apply_profile', { profile: selectedProfile });
                } catch (e) {
                    setError(`Failed to apply profile "${selectedProfile}": ${e}`);
                    setCommitting(false);
                    return;
                }
            }

            // Ingest the content
            await invoke('pyramid_ingest', { slug });

            // Commit via the preview/commit endpoint — this creates DADBEAR config
            const commitResponse = await fetch(`${PYRAMID_API_BASE}/pyramid/${slug}/preview/commit`, {
                method: 'POST',
                headers: authHeaders,
                body: JSON.stringify({
                    source_path: sourcePath,
                    content_type: contentType,
                    chain_id: chainId,
                }),
            });

            if (!commitResponse.ok) {
                const errBody = await commitResponse.text();
                throw new Error(`Commit failed (${commitResponse.status}): ${errBody}`);
            }

            // For conversation pyramids, also set up a DADBEAR watch on the source folder
            if (contentType === 'conversation') {
                // Determine the folder to watch (parent directory of the file, or the path itself)
                const watchPath = sourcePath.endsWith('.jsonl')
                    ? sourcePath.substring(0, sourcePath.lastIndexOf('/'))
                    : sourcePath;

                if (watchPath) {
                    try {
                        await fetch(`${PYRAMID_API_BASE}/pyramid/${slug}/dadbear/watch`, {
                            method: 'POST',
                            headers: authHeaders,
                            body: JSON.stringify({
                                source_path: watchPath,
                                content_type: 'conversation',
                            }),
                        });
                    } catch (_e) {
                        // Non-fatal: DADBEAR watch is nice-to-have at this stage
                    }
                }
            }

            // Start the build
            await invoke('pyramid_question_build', {
                slug,
                question: apexQuestion,
                granularity: 3,
                maxDepth: 3,
            });

            setStep('building');
        } catch (err) {
            setError(String(err));
        } finally {
            setCommitting(false);
        }
    }, [previewResult, slug, contentType, paths, getEffectiveChainId, selectedProfile, apexQuestion]);

    /** Legacy create flow for non-conversation types (code, document) */
    const handleCreate = useCallback(async (andBuild: boolean) => {
        if (paths.length === 0 || !contentType || !slug) return;
        setCreating(true);
        setError(null);

        try {
            const sourcePath = paths.length === 1 ? paths[0] : JSON.stringify(paths);

            await invoke('pyramid_create_slug', {
                slug,
                contentType,
                sourcePath,
            });

            await invoke('pyramid_ingest', { slug });

            if (andBuild) {
                if (selectedProfile && selectedProfile.trim()) {
                    try {
                        await invoke('pyramid_apply_profile', { profile: selectedProfile });
                    } catch (e) {
                        setError(`Failed to apply profile "${selectedProfile}": ${e}`);
                        setCreating(false);
                        return;
                    }
                }

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

    // Compute step sequence for the step indicator
    const getStepSequence = (): Step[] => {
        // Phase 17 folder-ingestion path has its own two-step flow.
        if (step === 'folder-ingest-pick' || step === 'folder-ingest-review') {
            return ['directory', 'folder-ingest-review'];
        }
        if (contentType === 'vine') {
            return ['directory', 'content-type', 'vine-dirs', 'confirm'];
        }
        if (contentType === 'conversation') {
            return ['directory', 'content-type', 'conversation-preset', 'question', 'preview'];
        }
        return ['directory', 'content-type', 'configure', 'question', 'confirm'];
    };

    const stepLabels: Record<Step, string> = {
        'directory': 'Source',
        'content-type': 'Type',
        'conversation-preset': 'Preset',
        'vine-dirs': 'Folders',
        'configure': 'Configure',
        'question': 'Question',
        'preview': 'Preview',
        'confirm': 'Confirm',
        'building': 'Building',
        'folder-ingest-pick': 'Pick folder',
        'folder-ingest-review': 'Review',
    };

    return (
        <div className="add-workspace-panel">
            {/* Step indicator */}
            <div className="workspace-steps">
                {getStepSequence().map((s, i) => {
                    const stepOrder = getStepSequence();
                    const currentIndex = stepOrder.indexOf(step);
                    return (
                        <div
                            key={s}
                            className={`workspace-step ${step === s ? 'active' : ''} ${
                                currentIndex > i ? 'done' : ''
                            }`}
                        >
                            <span className="step-number">{i + 1}</span>
                            <span className="step-label">{stepLabels[s]}</span>
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

                    {/* Phase 17: recursive folder ingestion entry point */}
                    <div
                        style={{
                            marginTop: '24px',
                            paddingTop: '16px',
                            borderTop: '1px solid var(--border-color, #ddd)',
                        }}
                    >
                        <div
                            style={{
                                fontSize: '0.85em',
                                opacity: 0.7,
                                marginBottom: '6px',
                                textTransform: 'uppercase',
                                letterSpacing: '0.05em',
                            }}
                        >
                            Or
                        </div>
                        <button
                            className="btn btn-secondary"
                            onClick={handlePickFolderForIngestion}
                            style={{ marginRight: '8px' }}
                        >
                            Point at folder (recursive)
                        </button>
                        <p className="hint" style={{ marginTop: '6px', fontSize: '0.85em', opacity: 0.75 }}>
                            Walk a folder, auto-detect content types, and create a
                            topical vine hierarchy with one ingest. Claude Code
                            conversations for the folder are detected and attached
                            automatically.
                        </p>
                    </div>
                </div>
            )}

            {/* Phase 17: Folder Ingestion Review */}
            {step === 'folder-ingest-review' && (
                <div className="workspace-step-content">
                    <h2>Review Folder Ingestion</h2>

                    <div className="confirm-field" style={{ marginBottom: '12px' }}>
                        <label className="field-label">Target folder:</label>
                        <div className="summary-path" style={{ marginTop: '4px' }}>
                            {folderIngestPath || '(none selected)'}
                        </div>
                    </div>

                    {folderIngestError && (
                        <div className="workspace-error" style={{ marginBottom: '12px' }}>
                            {folderIngestError}
                            <button
                                className="workspace-error-dismiss"
                                onClick={() => setFolderIngestError(null)}
                            >
                                Dismiss
                            </button>
                        </div>
                    )}

                    {/* Claude Code detection */}
                    <div
                        className="confirm-field"
                        style={{
                            marginBottom: '16px',
                            padding: '12px',
                            border: '1px solid var(--border-color, #ddd)',
                            borderRadius: '6px',
                        }}
                    >
                        <label
                            style={{
                                display: 'flex',
                                alignItems: 'center',
                                gap: '8px',
                                cursor: ccDirs.length > 0 ? 'pointer' : 'not-allowed',
                                opacity: ccDirs.length > 0 ? 1 : 0.5,
                            }}
                        >
                            <input
                                type="checkbox"
                                checked={includeClaudeCode}
                                disabled={ccDirs.length === 0 || ccLoading || folderPlanLoading}
                                onChange={(e) => handleToggleIncludeClaudeCode(e.target.checked)}
                            />
                            <span style={{ fontWeight: 500 }}>
                                Include Claude Code conversations related to this folder
                            </span>
                        </label>

                        {/* Scan-location row — lets the user point at a
                            different conversation root (Cursor cache,
                            a backup, etc.) without editing the
                            folder_ingestion_heuristics contribution. */}
                        <div
                            style={{
                                marginTop: '8px',
                                display: 'flex',
                                alignItems: 'center',
                                flexWrap: 'wrap',
                                gap: '8px',
                                fontSize: '0.85em',
                            }}
                        >
                            <span style={{ opacity: 0.7 }}>Scan location:</span>
                            <code
                                style={{
                                    padding: '2px 6px',
                                    borderRadius: '3px',
                                    background: 'rgba(255,255,255,0.05)',
                                    fontFamily: 'var(--font-mono, monospace)',
                                    fontSize: '0.95em',
                                    wordBreak: 'break-all',
                                }}
                            >
                                {ccOverridePath ?? '~/.claude/projects'}
                            </code>
                            <button
                                type="button"
                                className="btn btn-ghost btn-small"
                                onClick={handlePickCcOverrideFolder}
                                disabled={ccLoading || folderPlanLoading}
                                style={{ padding: '2px 10px', fontSize: '0.9em' }}
                            >
                                Change…
                            </button>
                            {ccOverridePath && (
                                <button
                                    type="button"
                                    className="btn btn-ghost btn-small"
                                    onClick={handleResetCcOverride}
                                    disabled={ccLoading || folderPlanLoading}
                                    style={{ padding: '2px 10px', fontSize: '0.9em' }}
                                >
                                    Reset to default
                                </button>
                            )}
                        </div>

                        {ccLoading ? (
                            <p className="hint" style={{ marginTop: '8px', fontSize: '0.85em' }}>
                                Scanning {ccOverridePath ?? '~/.claude/projects'}…
                            </p>
                        ) : ccDirs.length === 0 ? (
                            <p className="hint" style={{ marginTop: '8px', fontSize: '0.85em' }}>
                                No matching conversation directories were found for
                                this folder. (Checked{' '}
                                <code>{ccOverridePath ?? '~/.claude/projects'}</code>{' '}
                                for entries whose encoded path matches the target. Use
                                Change… to point at a different source.)
                            </p>
                        ) : (
                            <div style={{ marginTop: '8px' }}>
                                <p className="hint" style={{ fontSize: '0.85em' }}>
                                    Found {ccDirs.length} matching conversation
                                    {ccDirs.length === 1 ? ' directory' : ' directories'} in{' '}
                                    <code>{ccOverridePath ?? '~/.claude/projects'}</code>:
                                </p>
                                <ul
                                    style={{
                                        margin: '6px 0 0 16px',
                                        padding: 0,
                                        fontSize: '0.85em',
                                        opacity: 0.85,
                                        fontFamily: 'var(--font-mono, monospace)',
                                    }}
                                >
                                    {ccDirs.map((dir) => (
                                        <li
                                            key={dir.encoded_path}
                                            style={{ marginBottom: '2px', listStyle: 'disc' }}
                                        >
                                            {dir.encoded_path}
                                            {dir.is_main && (
                                                <span
                                                    style={{
                                                        marginLeft: '6px',
                                                        padding: '1px 6px',
                                                        borderRadius: '3px',
                                                        background: 'var(--accent-color, #4a90e2)',
                                                        color: '#fff',
                                                        fontSize: '0.8em',
                                                    }}
                                                >
                                                    main
                                                </span>
                                            )}
                                            {dir.is_worktree && (
                                                <span
                                                    style={{
                                                        marginLeft: '6px',
                                                        padding: '1px 6px',
                                                        borderRadius: '3px',
                                                        background: 'var(--warning-color, #e2a04a)',
                                                        color: '#fff',
                                                        fontSize: '0.8em',
                                                    }}
                                                >
                                                    worktree
                                                </span>
                                            )}
                                            <span
                                                style={{
                                                    marginLeft: '6px',
                                                    opacity: 0.6,
                                                    fontSize: '0.9em',
                                                }}
                                            >
                                                ({dir.jsonl_count} jsonl)
                                            </span>
                                        </li>
                                    ))}
                                </ul>
                            </div>
                        )}
                    </div>

                    {/* Plan summary */}
                    {folderPlanLoading ? (
                        <div className="hint" style={{ marginBottom: '16px' }}>
                            Planning ingestion...
                        </div>
                    ) : folderPlan ? (
                        <div className="preview-display" style={{ marginBottom: '16px' }}>
                            <div className="preview-header">Ingestion Plan</div>
                            <div className="preview-divider" />
                            <div className="preview-grid">
                                <div className="preview-row">
                                    <span className="preview-label">Files scanned</span>
                                    <span className="preview-value">
                                        {folderPlan.total_files}{' '}
                                        {folderPlan.total_ignored > 0 && (
                                            <span style={{ opacity: 0.6 }}>
                                                ({folderPlan.total_ignored} ignored)
                                            </span>
                                        )}
                                    </span>
                                </div>
                                <div className="preview-row">
                                    <span className="preview-label">Pyramids</span>
                                    <span className="preview-value">
                                        {
                                            folderPlan.operations.filter(
                                                (op) => op.op === 'create_pyramid',
                                            ).length
                                        }
                                    </span>
                                </div>
                                <div className="preview-row">
                                    <span className="preview-label">Topical vines</span>
                                    <span className="preview-value">
                                        {
                                            folderPlan.operations.filter(
                                                (op) => op.op === 'create_vine',
                                            ).length
                                        }
                                    </span>
                                </div>
                                <div className="preview-row">
                                    <span className="preview-label">CC pyramids</span>
                                    <span className="preview-value">
                                        {
                                            folderPlan.operations.filter(
                                                (op) => op.op === 'register_claude_code_pyramid',
                                            ).length
                                        }
                                    </span>
                                </div>
                                <div className="preview-row">
                                    <span className="preview-label">DADBEAR configs</span>
                                    <span className="preview-value">
                                        {
                                            folderPlan.operations.filter(
                                                (op) => op.op === 'register_dadbear_config',
                                            ).length
                                        }
                                    </span>
                                </div>
                                {folderPlan.root_slug && (
                                    <div className="preview-row">
                                        <span className="preview-label">Root slug</span>
                                        <span className="preview-value">
                                            <code>{folderPlan.root_slug}</code>
                                        </span>
                                    </div>
                                )}
                            </div>
                            <details
                                style={{ marginTop: '12px', fontSize: '0.85em', opacity: 0.85 }}
                            >
                                <summary style={{ cursor: 'pointer' }}>
                                    Show full operation list
                                </summary>
                                <ul
                                    style={{
                                        margin: '8px 0 0 16px',
                                        padding: 0,
                                        fontFamily: 'var(--font-mono, monospace)',
                                        maxHeight: '240px',
                                        overflowY: 'auto',
                                    }}
                                >
                                    {folderPlan.operations.map((op, idx) => {
                                        let label = '';
                                        if (op.op === 'create_pyramid') {
                                            label = `+ pyramid  ${op.slug} (${op.content_type})`;
                                        } else if (op.op === 'create_vine') {
                                            label = `+ vine     ${op.slug}`;
                                        } else if (op.op === 'add_child_to_vine') {
                                            label = `    attach ${op.child_slug} (${op.child_type}) → ${op.vine_slug}`;
                                        } else if (op.op === 'register_dadbear_config') {
                                            label = `    dadbear ${op.slug} every ${op.scan_interval_secs}s`;
                                        } else if (op.op === 'register_claude_code_pyramid') {
                                            label = `+ cc       ${op.slug}${op.is_main ? ' (main)' : op.is_worktree ? ' (worktree)' : ''}`;
                                        }
                                        return (
                                            <li
                                                key={idx}
                                                style={{
                                                    listStyle: 'none',
                                                    fontSize: '0.9em',
                                                    whiteSpace: 'pre',
                                                }}
                                            >
                                                {label}
                                            </li>
                                        );
                                    })}
                                </ul>
                            </details>
                        </div>
                    ) : (
                        <div className="hint" style={{ marginBottom: '16px' }}>
                            No plan generated yet.
                        </div>
                    )}

                    {folderIngestResult && (
                        <div
                            className="confirm-summary"
                            style={{
                                marginBottom: '16px',
                                padding: '12px',
                                border: '1px solid var(--border-color, #ddd)',
                                borderRadius: '6px',
                            }}
                        >
                            <div className="summary-row">
                                <span className="summary-label">Pyramids created:</span>
                                <span className="summary-value">
                                    {folderIngestResult.pyramids_created.length}
                                </span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">Vines created:</span>
                                <span className="summary-value">
                                    {folderIngestResult.vines_created.length}
                                </span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">CC pyramids:</span>
                                <span className="summary-value">
                                    {folderIngestResult.claude_code_pyramids.length}
                                </span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">DADBEAR configs:</span>
                                <span className="summary-value">
                                    {folderIngestResult.dadbear_configs.length}
                                </span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">Compositions added:</span>
                                <span className="summary-value">
                                    {folderIngestResult.compositions_added}
                                </span>
                            </div>
                            {folderIngestResult.errors.length > 0 && (
                                <div className="summary-row">
                                    <span className="summary-label">Errors:</span>
                                    <span className="summary-value" style={{ color: '#ef4444' }}>
                                        {folderIngestResult.errors.length}
                                    </span>
                                </div>
                            )}
                        </div>
                    )}

                    <div className="step-nav">
                        <button
                            className="btn btn-ghost"
                            onClick={() => {
                                setStep('directory');
                                setFolderPlan(null);
                                setFolderIngestResult(null);
                                setCcDirs([]);
                                setFolderIngestPath('');
                            }}
                        >
                            Back
                        </button>
                        <button
                            className="btn btn-primary"
                            onClick={handleStartFolderIngestion}
                            disabled={
                                committing ||
                                folderPlanLoading ||
                                !folderPlan ||
                                folderPlan.operations.length === 0 ||
                                folderIngestResult !== null
                            }
                        >
                            {committing ? 'Creating...' : 'Start ingestion'}
                        </button>
                    </div>
                </div>
            )}

            {/* Step 2: Content Type */}
            {step === 'content-type' && (
                <div className="workspace-step-content">
                    <h2>Choose Content Type</h2>

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

            {/* Step 2b: Conversation Preset */}
            {step === 'conversation-preset' && (
                <div className="workspace-step-content">
                    <h2>Conversation Chain Preset</h2>
                    <p className="step-description">
                        Choose how this conversation should be processed. Episodic Memory is
                        the default for building agent memory that persists across sessions.
                    </p>

                    <div className="content-type-cards">
                        {(Object.entries(CONVERSATION_PRESETS) as [ConversationPreset, typeof CONVERSATION_PRESETS[ConversationPreset]][]).map(
                            ([key, preset]) => (
                                <button
                                    key={key}
                                    className={`content-type-card ${conversationPreset === key ? 'selected' : ''}`}
                                    onClick={() => setConversationPreset(key)}
                                >
                                    <div className="content-type-icon">
                                        {key === 'episodic' ? '\u{1F9E0}' : '\u{1F50D}'}
                                    </div>
                                    <div className="content-type-name">{preset.label}</div>
                                    <div className="content-type-desc">{preset.description}</div>
                                    {key === 'episodic' && (
                                        <div className="preset-default-badge">Default</div>
                                    )}
                                </button>
                            ),
                        )}
                    </div>

                    <div className="content-type-notice">
                        Chain: <code>{CONVERSATION_PRESETS[conversationPreset].chainId}</code>
                        {' '}&mdash; both presets use forward + reverse temporal passes.
                        Episodic Memory optimizes for agent consumption; Retro optimizes for
                        human review and pattern discovery.
                    </div>

                    <div className="step-nav">
                        <button className="btn btn-ghost" onClick={() => setStep('content-type')}>
                            Back
                        </button>
                        <button className="btn btn-primary" onClick={() => setStep('question')}>
                            Next
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

                    {/* Model profile selector */}
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

                    {/* Slug field — shown here for conversation preset flow since
                        it skips the confirm step and goes straight to preview */}
                    {contentType === 'conversation' && (
                        <div style={{ marginTop: '16px' }}>
                            <label className="field-label" style={{ display: 'block', marginBottom: '4px' }}>
                                Slug name:
                            </label>
                            <input
                                type="text"
                                className="slug-input"
                                value={slug}
                                onChange={(e) => setSlug(slugify(e.target.value))}
                                placeholder="my-project"
                            />
                        </div>
                    )}

                    <div style={{ marginTop: '12px', display: 'flex', gap: '8px' }}>
                        <button className="btn btn-ghost" onClick={() => {
                            if (contentType === 'conversation') setStep('conversation-preset');
                            else setStep('configure');
                        }}>
                            Back
                        </button>
                        {contentType === 'conversation' ? (
                            <button
                                className="btn btn-primary"
                                onClick={handlePreview}
                                disabled={!apexQuestion.trim() || !slug || previewLoading}
                            >
                                {previewLoading ? 'Scanning...' : 'Preview'}
                            </button>
                        ) : (
                            <button
                                className="btn btn-primary"
                                onClick={() => setStep('confirm')}
                                disabled={!apexQuestion.trim()}
                            >
                                Next
                            </button>
                        )}
                    </div>
                </div>
            )}

            {/* Step: Preview (conversation flow) */}
            {step === 'preview' && previewResult && (
                <div className="workspace-step-content">
                    <h2>Build Preview</h2>

                    <div className="preview-display">
                        <div className="preview-header">
                            Preview for <span className="preview-slug">&ldquo;{slug}&rdquo;</span>
                        </div>
                        <div className="preview-divider" />

                        <div className="preview-grid">
                            <div className="preview-row">
                                <span className="preview-label">Source</span>
                                <span className="preview-value preview-path">
                                    {previewResult.source_path}
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Files found</span>
                                <span className="preview-value">
                                    {previewResult.file_count} {fileTypeLabel(previewResult.content_type)}
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Chain</span>
                                <span className="preview-value">
                                    <code>{previewResult.chain_id}</code>
                                    {' '}({CONVERSATION_PRESETS[conversationPreset]?.label || 'Custom'})
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Estimated cost</span>
                                <span className="preview-value preview-cost">
                                    {formatCost(previewResult.estimated_cost_dollars)}
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Estimated time</span>
                                <span className="preview-value">
                                    {formatEstimatedTime(previewResult.estimated_time_seconds)}
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Structure</span>
                                <span className="preview-value">
                                    {previewResult.estimated_pyramids} pyramid{previewResult.estimated_pyramids !== 1 ? 's' : ''}
                                    {' / '}{previewResult.estimated_layers} layer{previewResult.estimated_layers !== 1 ? 's' : ''}
                                    {' / '}{previewResult.estimated_nodes} node{previewResult.estimated_nodes !== 1 ? 's' : ''}
                                </span>
                            </div>
                            <div className="preview-row">
                                <span className="preview-label">Disk usage</span>
                                <span className="preview-value">
                                    ~{formatBytes(previewResult.estimated_disk_bytes)}
                                </span>
                            </div>
                        </div>

                        {previewResult.warnings.length > 0 && (
                            <div className="preview-warnings">
                                {previewResult.warnings.map((w, i) => (
                                    <div
                                        key={i}
                                        className={`preview-warning preview-warning-${w.level}`}
                                    >
                                        <span className="preview-warning-icon">
                                            {w.level === 'error' ? '\u{26D4}' : w.level === 'warning' ? '\u{26A0}' : '\u{2139}'}
                                        </span>
                                        <span className="preview-warning-text">
                                            {w.message}
                                            {w.file_path && (
                                                <span className="preview-warning-path"> ({w.file_path})</span>
                                            )}
                                        </span>
                                    </div>
                                ))}
                            </div>
                        )}

                        {previewResult.warnings.length === 0 && (
                            <div className="preview-no-warnings">
                                No warnings
                            </div>
                        )}
                    </div>

                    <div className="preview-note">
                        DADBEAR will begin processing on the next scan cycle after commit.
                        You can close this wizard and check progress on the dashboard.
                    </div>

                    <div className="step-nav">
                        <button
                            className="btn btn-ghost"
                            onClick={() => {
                                setPreviewResult(null);
                                setStep('question');
                            }}
                        >
                            Back
                        </button>
                        <button
                            className="btn btn-primary"
                            onClick={handleCommit}
                            disabled={committing || previewResult.warnings.some(w => w.level === 'error')}
                        >
                            {committing ? 'Committing...' : 'Commit \u2014 Begin Building'}
                        </button>
                    </div>
                </div>
            )}

            {/* Step 4: Name & Confirm (non-conversation types) */}
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
