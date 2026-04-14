import { useState, useCallback, useRef, useEffect } from 'react';
import { useBuildPolling } from '../hooks/useBuildPolling';
import { PipelineTimeline } from './theatre/PipelineTimeline';
import { PyramidSurface } from './pyramid-surface/PyramidSurface';
import { ActivityLog } from './theatre/ActivityLog';
import { NodeInspectorPanel } from './theatre/NodeInspectorPanel';
import type { BuildStatus } from './theatre/types';

interface PyramidTheatreProps {
    slug: string;
    onComplete?: (status: BuildStatus) => void;
    onClose?: () => void;
    onRetry?: (slug: string) => void;
    requestFullScreen?: (active: boolean) => void;
}

export function PyramidTheatre({ slug, onComplete, onClose, onRetry, requestFullScreen }: PyramidTheatreProps) {
    const { status, progress, liveNodes, isActive, error, cancel, forceReset } = useBuildPolling(slug);
    const [inspectedNodeId, setInspectedNodeId] = useState<string | null>(null);
    const [logCollapsed, setLogCollapsed] = useState(false);
    const onCompleteRef = useRef(onComplete);
    useEffect(() => { onCompleteRef.current = onComplete; });

    // Fire onComplete when build finishes
    useEffect(() => {
        if (status && ['complete', 'complete_with_errors', 'failed', 'cancelled'].includes(status.status)) {
            onCompleteRef.current?.(status);
        }
    }, [status?.status]);

    // Request fullscreen when build starts, release when it finishes
    useEffect(() => {
        if (!requestFullScreen) return;
        if (status?.status === 'running') {
            requestFullScreen(true);
        } else if (status && ['complete', 'complete_with_errors', 'failed', 'cancelled'].includes(status.status)) {
            requestFullScreen(false);
        }
    }, [status?.status, requestFullScreen]);

    const handleNodeClick = useCallback((nodeId: string) => {
        setInspectedNodeId(nodeId);
    }, []);

    const handleCloseInspector = useCallback(() => {
        setInspectedNodeId(null);
    }, []);

    const handleNavigate = useCallback((nodeId: string) => {
        setInspectedNodeId(nodeId);
    }, []);

    // ── Derived state ───────────────────────────────────────────────────
    const done = progress?.done ?? status?.progress.done ?? 0;
    const total = progress?.total ?? status?.progress.total ?? 0;
    const elapsed = status?.elapsed_seconds
        ? `${Math.floor(status.elapsed_seconds / 60)}m ${Math.floor(status.elapsed_seconds % 60)}s`
        : '0s';

    const isComplete = status?.status === 'complete' || status?.status === 'complete_with_errors';
    const isFailed = status?.status === 'failed';
    const isCancelled = status?.status === 'cancelled';
    const isRunning = status?.status === 'running';
    const isStuck = isRunning && (status?.elapsed_seconds ?? 0) > 1800;

    return (
        <div className="pyramid-theatre">
            {/* Header */}
            <div className="theatre-header">
                <h3>Building Pyramid: {slug}</h3>
                <div className="theatre-header-right">
                    {isRunning && (
                        <span className="build-status-badge running">
                            {progress?.current_step
                                ? progress.current_step.replace(/_/g, ' ')
                                : 'Running'}
                        </span>
                    )}
                    {isComplete && <span className="build-status-badge complete">Complete</span>}
                    {isFailed && <span className="build-status-badge failed">Failed</span>}
                    {isCancelled && <span className="build-status-badge failed">Cancelled</span>}
                    <span className="theatre-stats">{elapsed} | {done}/{total} steps</span>
                </div>
            </div>

            {error && <div className="build-error">Error: {error}</div>}

            {/* Pipeline timeline */}
            <PipelineTimeline
                currentStep={progress?.current_step ?? null}
                layers={progress?.layers ?? []}
            />

            {/* Live spatial pyramid */}
            <PyramidSurface
                slug={slug}
                mode="full"
                onNodeClick={handleNodeClick}
            />

            {/* Activity log (collapsible) */}
            <div className="theatre-log-toggle" onClick={() => setLogCollapsed(!logCollapsed)}>
                {logCollapsed ? 'Show Activity' : 'Hide Activity'}
            </div>
            <ActivityLog log={progress?.log ?? []} collapsed={logCollapsed} />

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
                    <button className="btn btn-danger" onClick={cancel}>Cancel Build</button>
                    {isStuck && (
                        <button className="btn btn-danger" onClick={forceReset} style={{ marginLeft: 8 }}>
                            Force Reset (stuck &gt;30m)
                        </button>
                    )}
                </div>
            )}

            {/* Node Inspector Panel */}
            {inspectedNodeId && (
                <NodeInspectorPanel
                    slug={slug}
                    nodeId={inspectedNodeId}
                    allNodes={liveNodes ?? []}
                    onClose={handleCloseInspector}
                    onNavigate={handleNavigate}
                />
            )}
        </div>
    );
}
