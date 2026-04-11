import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStepTimeline } from '../hooks/useStepTimeline';
import { StepState, StepCall, CostAccumulator } from '../hooks/useBuildRowState';
import { RerollModal, RerollTarget } from './RerollModal';

// ── Types matching Rust BuildProgressV2 ─────────────────────────────────────

interface LayerProgress {
    depth: number;
    step_name: string;
    estimated_nodes: number;
    completed_nodes: number;
    failed_nodes: number;
    status: string; // "pending" | "active" | "complete"
    nodes: NodeStatus[] | null;
}

interface NodeStatus {
    node_id: string;
    status: string; // "complete" | "failed"
    label: string | null;
}

interface LogEntry {
    elapsed_secs: number;
    message: string;
}

interface BuildProgressV2 {
    done: number;
    total: number;
    layers: LayerProgress[];
    current_step: string | null;
    log: LogEntry[];
}

interface BuildStatus {
    slug: string;
    status: string;
    progress: { done: number; total: number };
    elapsed_seconds: number;
    failures: number;
}

interface PyramidBuildVizProps {
    slug: string;
    onComplete?: (status: BuildStatus) => void;
    onClose?: () => void;
    onRetry?: (slug: string) => void;
}

// ── Pyramid Build Visualization ─────────────────────────────────────────────

export function PyramidBuildViz({ slug, onComplete, onClose, onRetry }: PyramidBuildVizProps) {
    const [v2, setV2] = useState<BuildProgressV2 | null>(null);
    const [status, setStatus] = useState<BuildStatus | null>(null);
    const [error, setError] = useState<string | null>(null);
    const logRef = useRef<HTMLDivElement>(null);
    const onCompleteRef = useRef(onComplete);
    useEffect(() => { onCompleteRef.current = onComplete; });

    // Phase 13: step timeline state. The hook seeds from the
    // `pyramid_step_cache_for_build` IPC (latest-build path) and
    // then listens for live events on `cross-build-event`.
    // Verifier fix: passing `null` (not the slug) triggers the
    // backend's "latest build for this slug" resolution so the
    // seed query actually matches rows.
    const { state: timelineState } = useStepTimeline(slug, null);
    const [expandedStep, setExpandedStep] = useState<string | null>(null);
    const [rerollTarget, setRerollTarget] = useState<RerollTarget | null>(null);
    const [rerollContent, setRerollContent] = useState<string | null>(null);

    // Poll both v1 (for status) and v2 (for layers)
    useEffect(() => {
        let active = true;

        const poll = async () => {
            while (active) {
                try {
                    // Poll both endpoints
                    const [s, v2Data] = await Promise.all([
                        invoke<BuildStatus>('pyramid_build_status', { slug }),
                        invoke<BuildProgressV2>('pyramid_build_progress_v2', { slug }).catch(() => null),
                    ]);
                    if (!active) break;

                    setStatus(s);
                    if (v2Data) setV2(v2Data);

                    if (['complete', 'complete_with_errors', 'failed', 'cancelled'].includes(s.status)) {
                        onCompleteRef.current?.(s);
                        break;
                    }

                    const isFinalizing =
                        s.status === 'running' &&
                        s.progress.total > 0 &&
                        s.progress.done >= s.progress.total;
                    await new Promise((r) => setTimeout(r, isFinalizing ? 500 : 2000));
                } catch (err) {
                    if (!active) break;
                    setError(String(err));
                    break;
                }
            }
        };

        poll();
        return () => { active = false; };
    }, [slug]);

    // Auto-scroll log
    useEffect(() => {
        if (logRef.current) {
            logRef.current.scrollTop = logRef.current.scrollHeight;
        }
    }, [v2?.log]);

    const handleCancel = useCallback(async () => {
        try { await invoke('pyramid_build_cancel', { slug }); } catch (err) { console.error('Cancel failed:', err); }
    }, [slug]);

    const handleForceReset = useCallback(async () => {
        try { await invoke('pyramid_build_force_reset', { slug }); } catch (err) { console.error('Force reset:', err); }
    }, [slug]);

    // ── Derived state ───────────────────────────────────────────────────────

    const done = v2?.done ?? status?.progress.done ?? 0;
    const total = v2?.total ?? status?.progress.total ?? 0;
    const pct = total > 0 ? Math.min(Math.round((done / total) * 100), 100) : 0;
    const elapsed = status?.elapsed_seconds
        ? `${Math.floor(status.elapsed_seconds / 60)}m ${Math.floor(status.elapsed_seconds % 60)}s`
        : '0s';

    const isComplete = status?.status === 'complete' || status?.status === 'complete_with_errors';
    const isFailed = status?.status === 'failed';
    const isCancelled = status?.status === 'cancelled';
    const isRunning = status?.status === 'running';
    const isFinalizing = isRunning && total > 0 && done >= total;
    const isStuck = isRunning && (status?.elapsed_seconds ?? 0) > 1800;

    // Sort layers by depth ascending. With column-reverse CSS, the first item (L0)
    // renders at the bottom, and higher layers stack above — forming the pyramid.
    const layers = (v2?.layers ?? []).slice().sort((a, b) => a.depth - b.depth);

    return (
        <div className="pyramid-build-viz">
            {/* Header */}
            <div className="pbv-header">
                <h3>Building Pyramid: {slug}</h3>
                {isRunning && (
                    <span className="build-status-badge running">
                        {isFinalizing
                            ? (() => {
                                const activeLayer = layers.find(l => l.status === 'active');
                                return activeLayer ? `Finishing L${activeLayer.depth}` : 'Finishing layer';
                            })()
                            : v2?.current_step ? formatStepName(v2.current_step) : 'Running'}
                    </span>
                )}
                {isComplete && <span className="build-status-badge complete">Complete</span>}
                {isFailed && <span className="build-status-badge failed">Failed</span>}
                {isCancelled && <span className="build-status-badge failed">Cancelled</span>}
            </div>

            {error && <div className="build-error">Error: {error}</div>}

            {/* Pyramid visualization — anchored to bottom, grows upward */}
            <div className="pbv-pyramid">
                {layers.length > 0 ? (
                    <>
                        {/* Step indicator — shows between layers when a non-node step is running */}
                        {isRunning && v2?.current_step && !isFinalizing && (() => {
                            const stepLabel = formatStepName(v2.current_step);
                            const allLayersComplete = layers.every(l => l.status === 'complete');
                            const hasActiveLayer = layers.some(l => l.status === 'active');
                            // Show indicator when all existing layers are complete but no new layer is active
                            if (allLayersComplete || (!hasActiveLayer && layers.length > 0)) {
                                return <div className="pbv-step-indicator">{stepLabel}</div>;
                            }
                            return null;
                        })()}
                        {layers.map((layer) => {
                            const maxDepth = Math.max(...layers.map(l => l.depth));
                            return <PyramidLayer key={`${layer.depth}-${layer.step_name}`} layer={layer} isApexLayer={layer.depth === maxDepth && layer.estimated_nodes === 1} />;
                        })}
                    </>
                ) : (
                    <div className="pbv-waiting">Waiting for build to start...</div>
                )}
            </div>

            {/* Stats line */}
            <div className="pbv-stats">
                <span>{elapsed} elapsed</span>
                <span className="pbv-stats-sep" />
                <span>{done}/{total} steps</span>
            </div>

            {/* Log panel */}
            {v2 && v2.log.length > 0 && (
                <div className="pbv-log-panel">
                    <div className="pbv-log-header">Activity</div>
                    <div className="pbv-log-scroll" ref={logRef}>
                        {v2.log.map((entry, i) => (
                            <div key={i} className="pbv-log-entry">
                                <span className="pbv-log-time">
                                    {Math.floor(entry.elapsed_secs / 60)}:{String(Math.floor(entry.elapsed_secs % 60)).padStart(2, '0')}
                                </span>
                                <span className="pbv-log-msg">{entry.message}</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Phase 13: step timeline — per-step introspection */}
            {timelineState.steps.length > 0 && (
                <StepTimelinePanel
                    steps={timelineState.steps}
                    cost={timelineState.cost}
                    expandedStep={expandedStep}
                    onToggleStep={step => setExpandedStep(expandedStep === step ? null : step)}
                    onRerollCall={(stepName, call) => {
                        setRerollTarget({ type: 'cache', cacheKey: call.cacheKey, stepName });
                        setRerollContent(null);
                    }}
                />
            )}

            {/* Phase 13: reroll modal */}
            {rerollTarget && (
                <RerollModal
                    slug={slug}
                    target={rerollTarget}
                    currentContent={rerollContent}
                    onClose={() => setRerollTarget(null)}
                    onRerolled={() => {
                        setRerollTarget(null);
                    }}
                />
            )}

            {/* Completion / failure / actions */}
            {isComplete && status && (
                <div className="build-complete-summary">
                    Pyramid built! {status.progress.done} nodes.
                    {status.failures > 0 ? ` (${status.failures} failures)` : ''}
                    <div className="build-complete-actions">
                        <button className="btn btn-primary" onClick={() => window.open(`http://localhost:3333/space/${slug}`, '_blank')}>
                            Open in Vibesmithy
                        </button>
                        {onClose && <button className="btn btn-secondary" onClick={onClose}>Back to Dashboard</button>}
                    </div>
                </div>
            )}

            {isFailed && (
                <div className="build-failed-message">
                    Build failed. Check the logs for details.
                    <div className="build-complete-actions">
                        {onRetry && <button className="btn btn-primary" onClick={() => onRetry(slug)}>Retry Build</button>}
                        {onClose && <button className="btn btn-secondary" onClick={onClose}>Back to Dashboard</button>}
                    </div>
                </div>
            )}

            {isRunning && (
                <div className="build-actions">
                    <button className="btn btn-danger" onClick={handleCancel}>Cancel Build</button>
                    {isStuck && (
                        <button className="btn btn-danger" onClick={handleForceReset} style={{ marginLeft: 8 }}>
                            Force Reset (stuck &gt;30m)
                        </button>
                    )}
                </div>
            )}
        </div>
    );
}

// ── Step name formatting ────────────────────────────────────────────────────

const STEP_LABELS: Record<string, string> = {
    'l0_webbing': 'Cross-referencing...',
    'thread_clustering_batch': 'Clustering documents...',
    'thread_clustering': 'Merging clusters...',
    'l1_webbing': 'Cross-referencing threads...',
    'l2_webbing': 'Cross-referencing layers...',
    'upper_layer_synthesis': 'Organizing layers...',
};

function formatStepName(stepName: string): string {
    return STEP_LABELS[stepName] ?? stepName.replace(/_/g, ' ') + '...';
}

// ── Layer rendering ─────────────────────────────────────────────────────────

function PyramidLayer({ layer, isApexLayer = false }: { layer: LayerProgress; isApexLayer?: boolean }) {
    const isComplete = layer.status === 'complete';
    const isActive = layer.status === 'active';
    const pendingCount = Math.max(0, layer.estimated_nodes - layer.completed_nodes - layer.failed_nodes);

    return (
        <div className={`pbv-layer ${isComplete ? 'pbv-layer-complete' : ''} ${isActive ? 'pbv-layer-active' : ''}`}>
            <div className="pbv-layer-label">
                <span className="pbv-layer-depth">L{layer.depth}</span>
                {isComplete && <span className="pbv-layer-check" />}
            </div>
            <div className="pbv-layer-content">
                {isApexLayer ? (
                    <ApexNode completed={layer.completed_nodes > 0} label={layer.nodes?.[0]?.label ?? null} />
                ) : (
                    <CellGrid layer={layer} pendingCount={pendingCount} />
                )}
            </div>
            <div className="pbv-layer-count">
                {layer.completed_nodes}/{layer.estimated_nodes}
            </div>
        </div>
    );
}

// ── Apex diamond ────────────────────────────────────────────────────────────

function ApexNode({ completed, label }: { completed: boolean; label: string | null }) {
    return (
        <div className={`pbv-apex ${completed ? 'pbv-apex-lit' : ''}`} title={label ?? 'Apex'}>
            <div className="pbv-apex-diamond" />
        </div>
    );
}

// ── Phase 13: Step Timeline Panel ─────────────────────────────────

interface StepTimelinePanelProps {
    steps: StepState[];
    cost: CostAccumulator;
    expandedStep: string | null;
    onToggleStep: (stepName: string) => void;
    onRerollCall: (stepName: string, call: StepCall) => void;
}

function StepTimelinePanel({
    steps,
    cost,
    expandedStep,
    onToggleStep,
    onRerollCall,
}: StepTimelinePanelProps) {
    const formatCost = (v: number) => `$${v.toFixed(2)}`;
    return (
        <div className="pbv-step-timeline">
            <div className="pbv-step-timeline-header">
                <div className="pbv-step-timeline-title">Step Timeline</div>
                <div className="pbv-cost-accumulator">
                    <span>
                        Cost: <strong>{formatCost(cost.estimatedUsd)}</strong> est
                        {cost.actualUsd !== null && (
                            <> / <strong>{formatCost(cost.actualUsd)}</strong> actual</>
                        )}
                    </span>
                    {cost.cacheSavingsUsd > 0 && (
                        <span className="pbv-cache-savings">
                            Cache savings: <strong>{formatCost(cost.cacheSavingsUsd)}</strong>
                        </span>
                    )}
                </div>
            </div>
            <div className="pbv-step-timeline-rows">
                {steps.map(step => (
                    <StepRow
                        key={step.stepName}
                        step={step}
                        expanded={expandedStep === step.stepName}
                        onToggle={() => onToggleStep(step.stepName)}
                        onRerollCall={onRerollCall}
                    />
                ))}
            </div>
        </div>
    );
}

interface StepRowProps {
    step: StepState;
    expanded: boolean;
    onToggle: () => void;
    onRerollCall: (stepName: string, call: StepCall) => void;
}

function StepRow({ step, expanded, onToggle, onRerollCall }: StepRowProps) {
    const statusClass = `pbv-step-status pbv-step-status-${step.status}`;
    const totalCalls = step.calls.length;
    const hitsLabel = step.cacheHits > 0 ? `${step.cacheHits}/${totalCalls} cached` : '';
    return (
        <div className={`pbv-step-row pbv-step-row-${step.status}`}>
            <button className="pbv-step-row-header" onClick={onToggle}>
                <span className={statusClass}>{step.status}</span>
                <span className="pbv-step-name">{step.stepName}</span>
                {step.activityHint && (
                    <span className="pbv-step-hint">{step.activityHint}</span>
                )}
                <span className="pbv-step-cost">${step.totalCostUsd.toFixed(3)}</span>
                {hitsLabel && <span className="pbv-step-cache-badge">{hitsLabel}</span>}
                <span className="pbv-step-chevron">{expanded ? '▾' : '▸'}</span>
            </button>
            {expanded && step.calls.length > 0 && (
                <div className="pbv-step-calls">
                    {step.calls.map((call, i) => (
                        <StepCallRow
                            key={`${call.cacheKey}-${i}`}
                            call={call}
                            onReroll={() => onRerollCall(step.stepName, call)}
                        />
                    ))}
                </div>
            )}
        </div>
    );
}

function StepCallRow({
    call,
    onReroll,
}: {
    call: StepCall;
    onReroll: () => void;
}) {
    const statusClass = `pbv-call-status pbv-call-status-${call.status}`;
    return (
        <div className="pbv-step-call">
            <span className={statusClass}>{call.status}</span>
            <span className="pbv-call-model">{call.modelId}</span>
            {call.costUsd !== undefined && (
                <span className="pbv-call-cost">${call.costUsd.toFixed(4)}</span>
            )}
            {call.latencyMs !== undefined && (
                <span className="pbv-call-latency">{call.latencyMs}ms</span>
            )}
            {call.tokensPrompt !== undefined && call.tokensCompletion !== undefined && (
                <span className="pbv-call-tokens">
                    {call.tokensPrompt}/{call.tokensCompletion}
                </span>
            )}
            {call.error && <span className="pbv-call-error" title={call.error}>error</span>}
            <button
                className="pbv-call-reroll"
                onClick={onReroll}
                disabled={call.status === 'running'}
                title="Reroll this call with a note"
            >
                Reroll
            </button>
        </div>
    );
}

// ── Cell grid (all layers except apex) ───────────────────────────────────────

function CellGrid({ layer, pendingCount }: { layer: LayerProgress; pendingCount: number }) {
    const nodes = layer.nodes;
    const total = layer.estimated_nodes;

    // For layers with per-node detail
    if (nodes) {
        const completedNodes = nodes.filter(n => n.status === 'complete');
        const failedNodes = nodes.filter(n => n.status === 'failed');
        return (
            <div className="pbv-cell-grid" style={{ '--cell-count': total } as React.CSSProperties}>
                {completedNodes.map((node) => (
                    <div key={node.node_id} className="pbv-cell pbv-cell-complete" title={node.label ?? node.node_id} />
                ))}
                {failedNodes.map((node) => (
                    <div key={node.node_id} className="pbv-cell pbv-cell-failed" title={`Failed: ${node.node_id}`} />
                ))}
                {Array.from({ length: pendingCount }).map((_, i) => (
                    <div key={`p-${i}`} className="pbv-cell pbv-cell-pending" />
                ))}
            </div>
        );
    }

    // For large layers without per-node detail — render count-based cells
    return (
        <div className="pbv-cell-grid" style={{ '--cell-count': total } as React.CSSProperties}>
            {Array.from({ length: layer.completed_nodes }).map((_, i) => (
                <div key={`c-${i}`} className="pbv-cell pbv-cell-complete" />
            ))}
            {Array.from({ length: layer.failed_nodes }).map((_, i) => (
                <div key={`f-${i}`} className="pbv-cell pbv-cell-failed" />
            ))}
            {Array.from({ length: pendingCount }).map((_, i) => (
                <div key={`p-${i}`} className="pbv-cell pbv-cell-pending" />
            ))}
        </div>
    );
}
