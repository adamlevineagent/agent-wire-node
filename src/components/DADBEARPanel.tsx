import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PyramidVisualization } from './PyramidVisualization';

interface AutoUpdateConfig {
    slug: string;
    auto_update: boolean;
    debounce_minutes: number;
    min_changed_files: number;
    runaway_threshold: number;
    breaker_tripped: boolean;
    breaker_tripped_at: string | null;
    frozen: boolean;
    frozen_at: string | null;
}

interface AutoUpdateStatus {
    auto_update: boolean;
    frozen: boolean;
    breaker_tripped: boolean;
    pending_mutations_by_layer: Record<string, number>;
    last_check_at: string | null;
    phase: string | null;
    phase_detail: string | null;
    timer_fires_at: string | null;
    last_result_summary: string | null;
}

interface StaleLogEntry {
    id: number;
    slug: string;
    batch_id: string;
    layer: number;
    target_id: string;
    stale: string;
    reason: string;
    checker_index: number;
    checker_batch_size: number;
    checked_at: string;
    cost_tokens: number | null;
    cost_usd: number | null;
}

interface CostSummary {
    slug: string;
    total_spend: number;
    total_calls: number;
    by_source: Array<{ source: string; spend: number; calls: number }>;
    by_check_type: Array<{ check_type: string; spend: number; calls: number }>;
    by_layer: Array<{ layer: number; spend: number; calls: number }>;
    recent_calls: Array<{
        id: number;
        operation: string;
        model: string;
        input_tokens: number;
        output_tokens: number;
        cost_usd: number;
        source: string;
        layer: number | null;
        check_type: string | null;
        created_at: string;
    }>;
}

interface AnnotationEntry {
    id: number;
    slug: string;
    node_id: string;
    annotation_type: string;
    content: string;
    question_context: string | null;
    author: string;
    created_at: string;
}

interface ContributionsData {
    annotations: AnnotationEntry[];
    totalAnnotations: number;
    uniqueAuthors: number;
    totalFaqs: number;
    lastAuthor: string | null;
    lastContributionAt: string | null;
}

interface DADBEARPanelProps {
    slug: string;
    contentType?: string;
    referencingSlugs?: string[];
    onBack: () => void;
    onNavigateToSlug?: (slug: string, nodeId: string) => void;
}

interface L0SweepResult {
    status: string;
    slug: string;
    tracked_files: number;
    enqueued: number;
    already_pending: number;
}

export function DADBEARPanel({ slug, contentType, referencingSlugs, onBack, onNavigateToSlug }: DADBEARPanelProps) {
    // Config state
    const [config, setConfig] = useState<AutoUpdateConfig | null>(null);
    const [editDebounce, setEditDebounce] = useState(5);
    const [editMinFiles, setEditMinFiles] = useState(1);
    const [editThreshold, setEditThreshold] = useState(0.5);
    const [editAutoUpdate, setEditAutoUpdate] = useState(true);
    const [configDirty, setConfigDirty] = useState(false);
    const [saving, setSaving] = useState(false);

    // Status state
    const [status, setStatus] = useState<AutoUpdateStatus | null>(null);
    const [statusError, setStatusError] = useState<string | null>(null);
    const pollInFlight = useRef(false);

    // Stale log state
    const [staleLog, setStaleLog] = useState<StaleLogEntry[]>([]);
    const [logLayerFilter, setLogLayerFilter] = useState<string>('all');
    const [logStaleFilter, setLogStaleFilter] = useState<string>('all');

    // Cost state
    const [cost, setCost] = useState<CostSummary | null>(null);
    const [costWindow, setCostWindow] = useState<string>('all');

    // Contributions state
    const [contributions, setContributions] = useState<ContributionsData | null>(null);

    // Run Now state
    const [runningNow, setRunningNow] = useState(false);
    const [sweepingL0, setSweepingL0] = useState(false);

    // Countdown state
    const [countdownSeconds, setCountdownSeconds] = useState<number | null>(null);
    const [countdownRestarted, setCountdownRestarted] = useState(false);
    const prevTimerFiresAt = useRef<string | null>(null);

    // Node counts (independent of DADBEAR)
    const [nodeCounts, setNodeCounts] = useState<Record<number, number> | null>(null);

    // Loading
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // Agent instructions card state
    const [agentInstructionsOpen, setAgentInstructionsOpen] = useState(false);
    const [agentInstructionsCopied, setAgentInstructionsCopied] = useState(false);
    const agentCopyTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

    // ── Load initial data ───────────────────────────────────────────

    const loadConfig = useCallback(async () => {
        try {
            const data = await invoke<AutoUpdateConfig>('pyramid_auto_update_config_get', { slug });
            setConfig(data);
            setEditDebounce(data.debounce_minutes);
            setEditMinFiles(data.min_changed_files);
            setEditThreshold(data.runaway_threshold);
            setEditAutoUpdate(data.auto_update);
            setConfigDirty(false);
        } catch {
            // Config might not exist for this slug yet
        }
    }, [slug]);

    const loadStatus = useCallback(async () => {
        if (pollInFlight.current) return;
        pollInFlight.current = true;
        try {
            const data = await invoke<AutoUpdateStatus>('pyramid_auto_update_status', { slug });
            setStatus(data);
        } catch (err) {
            setStatusError(String(err));
        } finally {
            pollInFlight.current = false;
        }
    }, [slug]);

    const loadStaleLog = useCallback(async () => {
        try {
            const layer = logLayerFilter === 'all' ? undefined : parseInt(logLayerFilter);
            const staleOnly = logStaleFilter === 'yes' ? true : logStaleFilter === 'no' ? false : undefined;
            const data = await invoke<StaleLogEntry[]>('pyramid_stale_log', {
                slug,
                limit: 50,
                layer: layer ?? null,
                staleOnly: staleOnly ?? null,
            });
            setStaleLog(data);
        } catch {
            setStaleLog([]);
        }
    }, [slug, logLayerFilter, logStaleFilter]);

    const loadCost = useCallback(async () => {
        try {
            const w = costWindow === 'all' ? null : costWindow;
            const data = await invoke<CostSummary>('pyramid_cost_summary', { slug, window: w });
            setCost(data);
        } catch {
            setCost(null);
        }
    }, [slug, costWindow]);

    const loadContributions = useCallback(async () => {
        try {
            const [annotations, faqDir] = await Promise.all([
                invoke<AnnotationEntry[]>('pyramid_annotations_recent', { slug, limit: 5 }),
                invoke<{ total_faqs?: number }>('pyramid_faq_directory', { slug }).catch(() => ({ total_faqs: 0 })),
            ]);
            const totalAnnotations = annotations.length;
            const authors = new Set(annotations.map(a => a.author));
            const lastAnnotation = annotations[0] ?? null;
            setContributions({
                annotations: annotations.slice(0, 5),
                totalAnnotations,
                uniqueAuthors: authors.size,
                totalFaqs: (faqDir as Record<string, unknown>).total_faqs as number ?? 0,
                lastAuthor: lastAnnotation?.author ?? null,
                lastContributionAt: lastAnnotation?.created_at ?? null,
            });
        } catch {
            setContributions(null);
        }
    }, [slug]);

    useEffect(() => {
        setLoading(true);
        Promise.all([loadConfig(), loadStatus(), loadStaleLog(), loadCost(), loadContributions()])
            .catch(() => setError('Failed to load DADBEAR data'))
            .finally(() => setLoading(false));
    }, [loadConfig, loadStatus, loadStaleLog, loadCost, loadContributions]);

    // ── Node counts (works independently of DADBEAR config) ─────────

    useEffect(() => {
        invoke<Array<{ depth: number }>>('pyramid_tree', { slug }).then(tree => {
            const counts: Record<number, number> = {};
            for (const node of tree) {
                counts[node.depth] = (counts[node.depth] || 0) + 1;
            }
            setNodeCounts(counts);
        }).catch(() => {
            // Tree endpoint may not exist for all pyramids, that's OK
        });
    }, [slug]);

    // ── Polling (10s interval, skip if in-flight) ───────────────────

    useEffect(() => {
        const interval = setInterval(() => {
            loadStatus();
            loadStaleLog();
            loadCost();
            loadContributions();
        }, 10_000);
        return () => clearInterval(interval);
    }, [loadStatus, loadStaleLog, loadCost, loadContributions]);

    // ── Countdown timer (1s interval) ──────────────────────────────

    useEffect(() => {
        const tfa = status?.timer_fires_at ?? null;
        // Detect timer restart (new mutation reset the debounce)
        if (tfa && prevTimerFiresAt.current && tfa !== prevTimerFiresAt.current) {
            setCountdownRestarted(true);
            setTimeout(() => setCountdownRestarted(false), 2000);
        }
        prevTimerFiresAt.current = tfa;
    }, [status?.timer_fires_at]);

    useEffect(() => {
        const tfa = status?.timer_fires_at;
        if (!tfa || status?.phase !== 'debounce') {
            setCountdownSeconds(null);
            return;
        }
        const update = () => {
            const remaining = Math.max(0, Math.floor((new Date(tfa).getTime() - Date.now()) / 1000));
            setCountdownSeconds(remaining);
        };
        update();
        const interval = setInterval(update, 1000);
        return () => clearInterval(interval);
    }, [status?.timer_fires_at, status?.phase]);

    // ── Refresh log and cost when filters change ────────────────────

    useEffect(() => { loadStaleLog(); }, [loadStaleLog]);
    useEffect(() => { loadCost(); }, [loadCost]);

    // ── Config save ─────────────────────────────────────────────────

    const handleSaveConfig = async () => {
        setSaving(true);
        try {
            await invoke('pyramid_auto_update_config_set', {
                slug,
                debounceMinutes: editDebounce,
                minChangedFiles: editMinFiles,
                runawayThreshold: editThreshold,
                autoUpdate: editAutoUpdate,
            });
            await Promise.all([loadConfig(), loadStatus()]);
            setConfigDirty(false);
        } catch (err) {
            setError(String(err));
        } finally {
            setSaving(false);
        }
    };

    // ── Freeze / Unfreeze ───────────────────────────────────────────

    const handleFreeze = async () => {
        try {
            await invoke('pyramid_auto_update_freeze', { slug });
            await loadConfig();
            await loadStatus();
        } catch (err) {
            setError(String(err));
        }
    };

    const handleUnfreeze = async () => {
        try {
            await invoke('pyramid_auto_update_unfreeze', { slug });
            await loadConfig();
            await loadStatus();
        } catch (err) {
            setError(String(err));
        }
    };

    // ── Breaker actions ─────────────────────────────────────────────

    const handleBreakerResume = async () => {
        try {
            await invoke('pyramid_breaker_resume', { slug });
            await loadConfig();
            await loadStatus();
        } catch (err) {
            setError(String(err));
        }
    };

    const handleArchiveAndRebuild = async () => {
        try {
            const result = await invoke<{ new_slug: string }>('pyramid_breaker_archive_and_rebuild', { slug });
            // Trigger build on the new slug
            await invoke('pyramid_build', { slug: result.new_slug });
            onBack();
        } catch (err) {
            setError(String(err));
        }
    };

    // ── Run Now ──────────────────────────────────────────────────────

    const handleRunNow = async () => {
        setRunningNow(true);
        try {
            await invoke('pyramid_auto_update_run_now', { slug });
            await loadStatus();
            await loadStaleLog();
            await loadCost();
        } catch (err) {
            setError(String(err));
        } finally {
            setRunningNow(false);
        }
    };

    const handleL0Sweep = async () => {
        setSweepingL0(true);
        try {
            await invoke<L0SweepResult>('pyramid_auto_update_l0_sweep', { slug });
            await loadStatus();
            await loadStaleLog();
            await loadCost();
            await loadContributions();
        } catch (err) {
            const message = String(err);
            if (message.includes('pyramid_auto_update_l0_sweep')) {
                setError('Force L0 Sweep needs one app restart to finish wiring the new backend command.');
            } else {
                setError(message);
            }
        } finally {
            setSweepingL0(false);
        }
    };

    // ── Helpers ─────────────────────────────────────────────────────

    const formatDate = (dateStr: string | null) => {
        if (!dateStr) return 'Never';
        const d = new Date(dateStr.includes('+') || dateStr.endsWith('Z') ? dateStr : dateStr + 'Z');
        return d.toLocaleDateString() + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    };

    const formatTimeAgo = (dateStr: string | null) => {
        if (!dateStr) return 'never';
        const seconds = Math.floor((Date.now() - new Date(dateStr.includes('+') || dateStr.endsWith('Z') ? dateStr : dateStr + 'Z').getTime()) / 1000);
        if (seconds < 60) return `${seconds}s ago`;
        const minutes = Math.floor(seconds / 60);
        if (minutes < 60) return `${minutes}m ago`;
        const hours = Math.floor(minutes / 60);
        if (hours < 24) return `${hours}h ago`;
        return `${Math.floor(hours / 24)}d ago`;
    };

    const enginePhase = status?.phase ?? 'idle';
    const isFrozen = status?.frozen ?? config?.frozen ?? false;
    const isBreakerTripped = status?.breaker_tripped ?? config?.breaker_tripped ?? false;

    // ── Accumulated-brightness DADBEAR model ─────────────────────────
    const stepCounts = useMemo(() => {
        const recent = staleLog.filter(e => {
            const age = Date.now() - new Date(e.checked_at).getTime();
            return age < 30 * 60 * 1000; // last 30 minutes
        });

        const l0Entries = recent.filter(e => e.layer === 0);
        const l1PlusEntries = recent.filter(e => e.layer > 0);
        const staleEntries = recent.filter(e => e.stale === 'Yes' || e.stale === 'yes' || e.stale === '1' || e.stale === 'true');

        return {
            D: l0Entries.length,       // Detect
            A1: l0Entries.length,      // Accumulate (same as detect)
            D2: l0Entries.length,      // Debounce (same as detect)
            B: l0Entries.length,       // Batch (evaluated = batched)
            E: recent.length,          // Evaluate (all checks)
            A2: staleEntries.length,   // Act (stale = acted on)
            R: l1PlusEntries.length,   // Recurse (L1+ = cascading)
        };
    }, [staleLog]);

    const mostRecentTimeForStep = (stepKey: string): string | undefined => {
        const recent = staleLog.filter(e => {
            const age = Date.now() - new Date(e.checked_at).getTime();
            return age < 30 * 60 * 1000;
        });

        if (stepKey === 'R') {
            const l1Plus = recent.filter(e => e.layer > 0);
            return l1Plus[0]?.checked_at;
        }
        if (stepKey === 'A2') {
            const stale = recent.filter(e => e.stale === 'Yes' || e.stale === 'yes' || e.stale === '1' || e.stale === 'true');
            return stale[0]?.checked_at;
        }
        return recent[0]?.checked_at; // D, A1, D2, B, E all use most recent entry
    };

    const getBrightness = (count: number, mostRecentTime?: string): number => {
        if (count === 0) return 0;
        if (!mostRecentTime) return 0.3;
        const ageMs = Date.now() - new Date(mostRecentTime).getTime();
        const ageMinutes = ageMs / 60000;
        // Full brightness within 5 min, fades to 0.2 over 30 min
        return Math.max(0.2, 1 - (ageMinutes / 30));
    };

    const acronymSteps = [
        { letter: 'Detect', key: 'D', count: stepCounts.D },
        { letter: 'Accumulate', key: 'A1', count: stepCounts.A1 },
        { letter: 'Debounce', key: 'D2', count: stepCounts.D2 },
        { letter: 'Batch', key: 'B', count: stepCounts.B },
        { letter: 'Evaluate', key: 'E', count: stepCounts.E },
        { letter: 'Act', key: 'A2', count: stepCounts.A2 },
        { letter: 'Recurse', key: 'R', count: stepCounts.R },
    ];

    const getStatusInfo = (): { text: string; className: string } => {
        if (!status && statusError) return { text: 'DADBEAR not configured', className: 'dadbear-status-hibernating' };
        if (!status) return { text: 'Loading...', className: '' };
        if (config?.frozen) return { text: 'DADBEAR is hibernating, wake him up by unfreezing on pyramid page', className: 'dadbear-status-snowcave' };
        if (config?.breaker_tripped) return { text: 'DADBEAR needs your attention', className: 'dadbear-status-tripped' };

        // Use engine phase for richer status
        const phase = status.phase ?? 'idle';
        const detail = status.phase_detail ?? '';
        const summary = status.last_result_summary ?? '';

        switch (phase) {
            case 'debounce':
                return { text: 'DADBEAR is counting down', className: 'dadbear-status-counting' };
            case 'evaluating':
                return { text: 'DADBEAR is sniffing around', className: 'dadbear-status-counting' };
            case 'cascading':
                return { text: 'DADBEAR is climbing the pyramid', className: 'dadbear-status-climbing' };
            case 'done_stale':
                return { text: `DADBEAR ${summary}`, className: 'dadbear-status-updated' };
            case 'done_clean':
                return { text: `DADBEAR ${summary}`, className: 'dadbear-status-hibernating' };
            default: {
                // Fallback: check pending mutations for backward compat
                const pendingByLayer = status.pending_mutations_by_layer;
                const totalPending = Object.values(pendingByLayer).reduce((a, b) => a + b, 0);
                if (totalPending > 0) {
                    return { text: 'DADBEAR is counting down', className: 'dadbear-status-counting' };
                }
                return { text: 'DADBEAR is patient', className: 'dadbear-status-hibernating' };
            }
        }
    };

    const generateAgentInstructions = () => {
        const autoUpdateStr = config?.auto_update ? 'enabled' : 'disabled';
        const debounceStr = config ? `${config.debounce_minutes}` : '?';
        const lastCheckStr = status?.last_check_at ? formatDate(status.last_check_at) : 'never';
        return `# Pyramid: ${slug}
# Access the knowledge pyramid and contribute findings back.

## Explore
\`\`\`bash
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" apex ${slug}
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" search ${slug} "your query"
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" drill ${slug} <NODE_ID>
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" faq ${slug} "your question"
\`\`\`

## Contribute
\`\`\`bash
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" annotate ${slug} <NODE_ID> "Your finding.\\n\\nGeneralized understanding: Mechanism knowledge for future agents." --question "Question this answers" --author "your-name" --type observation
\`\`\`

## DADBEAR Status
Auto-update: ${autoUpdateStr}
Debounce: ${debounceStr} minutes
Last check: ${lastCheckStr}

## Tips
- Start with \`apex\` to understand the codebase overview
- Use \`search\` to find specific topics
- Use \`faq\` before investigating — someone may have already answered your question
- Always annotate with "Generalized understanding:" section
- Use \`--type correction\` when something in the pyramid is wrong`;
    };

    const handleCopyAgentInstructions = () => {
        navigator.clipboard.writeText(generateAgentInstructions()).then(() => {
            setAgentInstructionsCopied(true);
            if (agentCopyTimeout.current) clearTimeout(agentCopyTimeout.current);
            agentCopyTimeout.current = setTimeout(() => setAgentInstructionsCopied(false), 2000);
        });
    };

    const statusInfo = getStatusInfo();

    const totalPending = status
        ? Object.values(status.pending_mutations_by_layer).reduce((a, b) => a + b, 0)
        : 0;
    const maxPending = status
        ? Math.max(1, ...Object.values(status.pending_mutations_by_layer))
        : 1;

    if (loading) {
        return <div className="dadbear-loading">Loading DADBEAR data...</div>;
    }

    return (
        <div className="dadbear-panel">
            <div className="dadbear-panel-header">
                <button className="dadbear-back-btn" onClick={onBack}>
                    &larr; Back
                </button>
                <div>
                    <h2>DADBEAR</h2>
                    <div className="dadbear-acronym">
                        {acronymSteps.map((step, i) => {
                            const brightness = getBrightness(step.count, mostRecentTimeForStep(step.key));
                            return (
                                <div key={i} className="dadbear-acronym-step">
                                    <span
                                        className="dadbear-acronym-letter"
                                        style={{
                                            color: brightness > 0
                                                ? `rgba(34, 211, 238, ${brightness})`
                                                : 'var(--text-muted)',
                                            textShadow: brightness > 0.7
                                                ? `0 0 ${brightness * 12}px rgba(34, 211, 238, ${brightness * 0.5})`
                                                : 'none'
                                        }}
                                    >
                                        {step.letter}
                                    </span>
                                    <span className="dadbear-acronym-count" style={{
                                        opacity: step.count > 0 ? 1 : 0.3
                                    }}>
                                        {step.count}
                                    </span>
                                </div>
                            );
                        })}
                    </div>
                </div>
                <span className="dadbear-slug-label">{slug}</span>
            </div>

            {error && (
                <div className="pyramid-error" style={{ marginBottom: 16 }}>
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            )}

            {/* ── Body: 2-column layout ─────────────────────────────── */}
            <div className="dadbear-body-layout">
                {/* ── Left column: pyramid visualization ──────────────── */}
                <div className="dadbear-left-col">
                    <PyramidVisualization
                        slug={slug}
                        contentType={contentType}
                        referencingSlugs={referencingSlugs}
                        staleLog={staleLog}
                        status={status}
                        onNavigateToSlug={onNavigateToSlug}
                    />
                </div>

                {/* ── Right column: all monitoring panels ─────────────── */}
                <div className="dadbear-right-col">
                    {/* ── Status + Controls Section ──────────────────── */}
                    <div className="dadbear-status-section">
                        <h3>Live Status</h3>

                        <div className={`dadbear-status-text ${statusInfo.className}`}>
                            {statusInfo.text}
                        </div>

                        {nodeCounts && (
                            <div className="dadbear-node-counts" style={{
                                display: 'flex', gap: '12px', marginTop: '8px', fontSize: '13px', opacity: 0.8
                            }}>
                                {[3, 2, 1, 0].filter(d => nodeCounts[d]).map(d => (
                                    <span key={d} style={{ fontFamily: 'monospace' }}>
                                        L{d}: {nodeCounts[d]}
                                    </span>
                                ))}
                            </div>
                        )}

                        {statusError && !status && contentType !== 'question' && contentType !== 'conversation' && (
                            <button
                                className="btn btn-primary btn-sm"
                                style={{ marginTop: '8px' }}
                                onClick={async () => {
                                    try {
                                        await invoke('pyramid_auto_update_config_init', { slug });
                                        setStatusError(null);
                                        await loadConfig();
                                        await loadStatus();
                                    } catch (err) {
                                        setStatusError(String(err));
                                    }
                                }}
                            >
                                Initialize DADBEAR
                            </button>
                        )}

                        {/* Phase detail */}
                        {status?.phase_detail && (status.phase === 'evaluating' || status.phase === 'cascading') && (
                            <div className="dadbear-phase-detail">{status.phase_detail}</div>
                        )}

                        {/* Live countdown during debounce */}
                        {enginePhase === 'debounce' && countdownSeconds != null && (
                            <div>
                                <div className="dadbear-countdown">
                                    {Math.floor(countdownSeconds / 60)}:{String(countdownSeconds % 60).padStart(2, '0')}
                                </div>
                                {countdownRestarted && (
                                    <div className="dadbear-countdown-reset">NEW CHANGE — RESTARTING COUNTDOWN</div>
                                )}
                            </div>
                        )}

                        {/* Vertical bar chart — fills available space */}
                        {status && (
                            <div className="dadbear-bar-chart">
                                {[0, 1, 2, 3].map(layer => {
                                    const count = status.pending_mutations_by_layer[String(layer)] ?? 0;
                                    const heightPct = (count / maxPending) * 100;
                                    return (
                                        <div key={layer} className="dadbear-bar-column">
                                            <div className="dadbear-bar-fill" style={{ height: `${heightPct}%` }} />
                                            <span className="dadbear-bar-label">L{layer}</span>
                                            <span className="dadbear-bar-count">{count}</span>
                                        </div>
                                    );
                                })}
                            </div>
                        )}

                        {/* Bottom section: last check + controls */}
                        <div className="dadbear-status-footer">
                            {status && (
                                <div className="dadbear-last-check">
                                    Last check: {formatDate(status.last_check_at)}
                                </div>
                            )}

                            <div className="dadbear-controls">
                                <button
                                    className={`dadbear-freeze-btn ${isFrozen ? 'frozen' : 'active'}`}
                                    onClick={isFrozen ? handleUnfreeze : handleFreeze}
                                >
                                    {isFrozen ? 'Unfreeze' : 'Freeze'}
                                </button>

                                <span className={`dadbear-breaker-indicator ${isBreakerTripped ? 'tripped' : 'ok'}`}>
                                    <span className={`dadbear-breaker-dot ${isBreakerTripped ? 'tripped' : 'ok'}`} />
                                    Breaker: {isBreakerTripped ? 'TRIPPED' : 'OK'}
                                </span>

                                {isBreakerTripped && (
                                    <>
                                        <button className="dadbear-action-btn" onClick={handleBreakerResume}>
                                            Resume
                                        </button>
                                        <button className="dadbear-action-btn danger" onClick={handleArchiveAndRebuild}>
                                            Archive + Rebuild
                                        </button>
                                    </>
                                )}

                                {!isFrozen && !isBreakerTripped && (
                                    <button
                                        className="dadbear-action-btn l0-sweep"
                                        onClick={handleL0Sweep}
                                        disabled={sweepingL0 || runningNow}
                                    >
                                        {sweepingL0 ? 'Sweeping L0...' : 'Force L0 Sweep'}
                                    </button>
                                )}

                                {!isFrozen && !isBreakerTripped && totalPending > 0 && (
                                    <button
                                        className="dadbear-action-btn run-now"
                                        onClick={handleRunNow}
                                        disabled={runningNow || sweepingL0}
                                    >
                                        {runningNow ? 'Running...' : 'Run Now'}
                                    </button>
                                )}
                            </div>
                        </div>
                    </div>

                    {/* ── Config Section ─────────────────────────────── */}
                    <div className="dadbear-config-section">
                        <h3>Configuration</h3>

                        <div className="dadbear-field">
                            <label>Auto-Update</label>
                            <label className="dadbear-toggle">
                                <input
                                    type="checkbox"
                                    checked={editAutoUpdate}
                                    onChange={(e) => { setEditAutoUpdate(e.target.checked); setConfigDirty(true); }}
                                />
                                <span className="dadbear-toggle-track" />
                                <span className="dadbear-toggle-thumb" />
                            </label>
                        </div>

                        <div className="dadbear-field">
                            <label>Debounce ({editDebounce} min)</label>
                            <input
                                type="range"
                                className="dadbear-slider"
                                min={1}
                                max={30}
                                value={editDebounce}
                                onChange={(e) => { setEditDebounce(parseInt(e.target.value)); setConfigDirty(true); }}
                            />
                        </div>

                        <div className="dadbear-field">
                            <label>Min Changed Files ({editMinFiles})</label>
                            <input
                                type="range"
                                className="dadbear-slider"
                                min={1}
                                max={20}
                                value={editMinFiles}
                                onChange={(e) => { setEditMinFiles(parseInt(e.target.value)); setConfigDirty(true); }}
                            />
                        </div>

                        <div className="dadbear-field">
                            <label>Runaway Threshold ({(editThreshold * 100).toFixed(0)}%)</label>
                            <input
                                type="range"
                                className="dadbear-slider"
                                min={10}
                                max={100}
                                value={editThreshold * 100}
                                onChange={(e) => { setEditThreshold(parseInt(e.target.value) / 100); setConfigDirty(true); }}
                            />
                        </div>

                        <button
                            className="dadbear-save-btn"
                            onClick={handleSaveConfig}
                            disabled={!configDirty || saving}
                        >
                            {saving ? 'Saving...' : 'Save Config'}
                        </button>
                    </div>

                    {/* ── Stale Log Section ──────────────────────────── */}
                    <div className="dadbear-log-section">
                        <h3>Stale Check Log</h3>

                        <div className="dadbear-log-filters">
                            <select value={logLayerFilter} onChange={(e) => setLogLayerFilter(e.target.value)}>
                                <option value="all">All Layers</option>
                                <option value="0">L0</option>
                                <option value="1">L1</option>
                                <option value="2">L2</option>
                                <option value="3">L3</option>
                            </select>
                            <select value={logStaleFilter} onChange={(e) => setLogStaleFilter(e.target.value)}>
                                <option value="all">All Results</option>
                                <option value="yes">Stale Only</option>
                                <option value="no">Not Stale</option>
                            </select>
                        </div>

                        <table className="stale-log-table">
                            <thead>
                                <tr>
                                    <th>Layer</th>
                                    <th>Target</th>
                                    <th>Stale</th>
                                    <th>Reason</th>
                                    <th>Cost</th>
                                    <th>Time</th>
                                </tr>
                            </thead>
                            <tbody>
                                {staleLog.length === 0 ? (
                                    <tr>
                                        <td colSpan={6} style={{ textAlign: 'center', color: 'var(--text-muted)', padding: 20 }}>
                                            No stale checks recorded yet
                                        </td>
                                    </tr>
                                ) : staleLog.map(entry => (
                                    <tr key={entry.id}>
                                        <td>L{entry.layer}</td>
                                        <td title={entry.target_id}>
                                            {entry.target_id.length > 20
                                                ? entry.target_id.slice(0, 20) + '...'
                                                : entry.target_id}
                                        </td>
                                        <td>
                                            <span className={`stale-badge ${entry.stale === '1' || entry.stale === 'true' || entry.stale === 'yes' ? 'stale-yes' : 'stale-no'}`}>
                                                {entry.stale === '1' || entry.stale === 'true' || entry.stale === 'yes' ? 'Yes' : 'No'}
                                            </span>
                                        </td>
                                        <td title={entry.reason}>
                                            {entry.reason.length > 40
                                                ? entry.reason.slice(0, 40) + '...'
                                                : entry.reason}
                                        </td>
                                        <td>{entry.cost_usd != null ? `$${entry.cost_usd.toFixed(4)}` : '-'}</td>
                                        <td>{formatDate(entry.checked_at)}</td>
                                    </tr>
                                ))}
                            </tbody>
                        </table>
                    </div>

                    {/* ── Cost Section ───────────────────────────────── */}
                    <div className="dadbear-cost-section">
                        <h3>Cost Observatory</h3>

                        <div className="dadbear-cost-window">
                            {['all', '24h', '7d', '30d'].map(w => (
                                <button
                                    key={w}
                                    className={costWindow === w ? 'active' : ''}
                                    onClick={() => setCostWindow(w)}
                                >
                                    {w === 'all' ? 'All' : w}
                                </button>
                            ))}
                        </div>

                        {cost && (
                            <>
                                <div className="dadbear-cost-total">
                                    ${cost.total_spend.toFixed(4)}
                                </div>
                                <div className="dadbear-cost-calls">
                                    {cost.total_calls} API call{cost.total_calls !== 1 ? 's' : ''}
                                </div>

                                {cost.by_check_type.length > 0 && (
                                    <div className="dadbear-cost-breakdown">
                                        {cost.by_check_type.map(ct => (
                                            <div key={ct.check_type} className="dadbear-cost-row">
                                                <span className="dadbear-cost-row-label">{ct.check_type}</span>
                                                <span className="dadbear-cost-row-value">
                                                    ${ct.spend.toFixed(4)} ({ct.calls})
                                                </span>
                                            </div>
                                        ))}
                                    </div>
                                )}
                            </>
                        )}
                    </div>

                    {/* ── Knowledge Contributions ────────────────────── */}
                    <div className="dadbear-contributions">
                        <h3>Knowledge Contributions</h3>
                        {contributions ? (
                            <>
                                <div className="contribution-stats">
                                    <div className="contribution-stat-row">
                                        <span>\uD83D\uDCDD {contributions.totalAnnotations} annotation{contributions.totalAnnotations !== 1 ? 's' : ''} from {contributions.uniqueAuthors} agent{contributions.uniqueAuthors !== 1 ? 's' : ''}</span>
                                    </div>
                                    <div className="contribution-stat-row">
                                        <span>\uD83D\uDCDA {contributions.totalFaqs} FAQ entr{contributions.totalFaqs !== 1 ? 'ies' : 'y'} (flat mode)</span>
                                    </div>
                                    {contributions.lastAuthor && (
                                        <div className="contribution-stat-row">
                                            <span>\uD83D\uDD04 Last contribution: {formatTimeAgo(contributions.lastContributionAt)} by <span className="contribution-author-badge">{contributions.lastAuthor}</span></span>
                                        </div>
                                    )}
                                </div>
                                {contributions.annotations.length > 0 && (
                                    <div className="contribution-recent">
                                        <div className="contribution-recent-label">Recent:</div>
                                        {contributions.annotations.map(a => (
                                            <div key={a.id} className="contribution-item">
                                                <span className="contribution-author-badge">{a.author}</span>
                                                <span className="contribution-content" title={a.content}>
                                                    {a.content.length > 60 ? a.content.slice(0, 60) + '...' : a.content}
                                                </span>
                                            </div>
                                        ))}
                                    </div>
                                )}
                            </>
                        ) : (
                            <div className="contribution-empty">No contributions yet</div>
                        )}
                    </div>

                    {/* ── Recent Calls ───────────────────────────────── */}
                    {cost && cost.recent_calls.length > 0 && (
                        <div className="dadbear-log-section">
                            <h3>Recent API Calls</h3>
                            <table className="stale-log-table">
                                <thead>
                                    <tr>
                                        <th>Operation</th>
                                        <th>Model</th>
                                        <th>Tokens</th>
                                        <th>Cost</th>
                                        <th>Source</th>
                                        <th>Time</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {cost.recent_calls.map(call => (
                                        <tr key={call.id}>
                                            <td>{call.operation}</td>
                                            <td title={call.model}>
                                                {call.model.length > 20 ? '...' + call.model.slice(-18) : call.model}
                                            </td>
                                            <td>{call.input_tokens + call.output_tokens}</td>
                                            <td>${call.cost_usd.toFixed(4)}</td>
                                            <td>{call.source}</td>
                                            <td>{formatDate(call.created_at)}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                    )}

                    {/* ── Agent Instructions ─────────────────────────── */}
                    <div className="agent-onboarding-card">
                        <div className="agent-onboarding-header" onClick={() => setAgentInstructionsOpen(!agentInstructionsOpen)}>
                            <h3>Agent Instructions</h3>
                            <div className="agent-onboarding-header-actions">
                                <button
                                    className={`copy-btn${agentInstructionsCopied ? ' copied' : ''}`}
                                    onClick={(e) => { e.stopPropagation(); handleCopyAgentInstructions(); }}
                                >
                                    {agentInstructionsCopied ? 'Copied!' : 'Copy to Clipboard'}
                                </button>
                                <span className="agent-onboarding-toggle">{agentInstructionsOpen ? '\u25B2' : '\u25BC'}</span>
                            </div>
                        </div>
                        {agentInstructionsOpen && (
                            <div className="agent-onboarding-content">
                                <pre>{generateAgentInstructions()}</pre>
                            </div>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
}
