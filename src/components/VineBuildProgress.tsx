import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface VineBuildStatus {
    status: 'running' | 'complete' | 'failed' | 'not_found';
    error: string | null;
}

interface BunchStatus {
    slug: string;
    status: string;
    node_count: number;
}

interface VineBuildProgressProps {
    slug: string;
    onComplete?: () => void;
    onClose?: () => void;
    requestFullScreen?: (active: boolean) => void;
}

export function VineBuildProgress({ slug, onComplete, onClose, requestFullScreen }: VineBuildProgressProps) {
    const [buildStatus, setBuildStatus] = useState<VineBuildStatus | null>(null);
    const [bunches, setBunches] = useState<BunchStatus[]>([]);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let active = true;

        const poll = async () => {
            while (active) {
                try {
                    // Poll build status
                    const status: VineBuildStatus = await invoke('pyramid_vine_build_status', { slug });
                    if (!active) break;
                    setBuildStatus(status);

                    // Poll bunches
                    try {
                        const bunchData: any = await invoke('pyramid_vine_bunches', { slug });
                        if (!active) break;
                        const nextBunches = Array.isArray(bunchData) ? bunchData : bunchData?.bunches || [];
                        setBunches(nextBunches);

                        const completed = nextBunches.filter(
                            (b: BunchStatus) => b.status === 'complete' || b.node_count > 0,
                        ).length;
                        const isFinalizing =
                            status.status === 'running' &&
                            nextBunches.length > 0 &&
                            completed >= nextBunches.length;

                        if (status.status === 'complete' || status.status === 'failed') {
                            onComplete?.();
                            break;
                        }

                        await new Promise((r) => setTimeout(r, isFinalizing ? 500 : 3000));
                        continue;
                    } catch {
                        // Bunches endpoint may not be available yet
                    }

                    if (status.status === 'complete' || status.status === 'failed') {
                        onComplete?.();
                        break;
                    }

                    await new Promise((r) => setTimeout(r, 3000));
                    continue;
                } catch (err) {
                    if (!active) break;
                    setError(String(err));
                    break;
                }
            }
        };

        poll();
        return () => { active = false; };
    }, [slug, onComplete]);

    // Request fullscreen when build starts, release when it finishes
    useEffect(() => {
        if (!requestFullScreen) return;
        if (buildStatus?.status === 'running') {
            requestFullScreen(true);
        } else if (buildStatus && ['complete', 'failed'].includes(buildStatus.status)) {
            requestFullScreen(false);
        }
    }, [buildStatus?.status, requestFullScreen]);

    const isComplete = buildStatus?.status === 'complete';
    const isFailed = buildStatus?.status === 'failed';
    const isRunning = buildStatus?.status === 'running' || (!buildStatus && !error);

    const completedBunches = bunches.filter(b => b.status === 'complete' || b.node_count > 0).length;
    const totalBunches = bunches.length;
    const pct = totalBunches > 0 ? Math.round((completedBunches / totalBunches) * 100) : 0;
    const isFinalizing = isRunning && totalBunches > 0 && completedBunches >= totalBunches;

    return (
        <div className="build-progress-panel">
            <div className="build-progress-header">
                <h3>Building Vine: {slug}</h3>
                {isRunning && (
                    <span className="build-status-badge running">
                        {isFinalizing ? 'Finalizing' : 'Running'}
                    </span>
                )}
                {isComplete && (
                    <span className="build-status-badge complete">Complete</span>
                )}
                {isFailed && (
                    <span className="build-status-badge failed">Failed</span>
                )}
            </div>

            {error && (
                <div className="build-error">
                    Error: {error}
                </div>
            )}

            {buildStatus?.error && (
                <div className="build-error">
                    Build error: {buildStatus.error}
                </div>
            )}

            <div className="build-progress-bar-container">
                <div className="build-progress-bar">
                    <div
                        className={`build-progress-fill ${isComplete ? 'complete' : isFailed ? 'failed' : ''}`}
                        style={{ width: `${isComplete ? 100 : pct}%` }}
                    />
                </div>
                <div className="build-progress-stats">
                    <span>
                        {isRunning && 'Building vine...'}
                        {isComplete && 'Vine built!'}
                        {isFailed && 'Build failed.'}
                        {totalBunches > 0 && ` (${completedBunches}/${totalBunches} bunches)`}
                    </span>
                </div>
            </div>

            {bunches.length > 0 && (
                <div className="vine-bunch-list">
                    {bunches.map((b) => (
                        <div key={b.slug} className="vine-bunch-row">
                            <span className="vine-bunch-slug">{b.slug}</span>
                            <span className={`vine-bunch-status ${b.status === 'complete' || b.node_count > 0 ? 'done' : 'pending'}`}>
                                {b.status === 'complete' || b.node_count > 0
                                    ? `${b.node_count} nodes`
                                    : b.status || 'pending'}
                            </span>
                        </div>
                    ))}
                </div>
            )}

            {isComplete && (
                <div className="build-complete-summary">
                    Vine built with {totalBunches} bunch{totalBunches !== 1 ? 'es' : ''}.
                    <div className="build-complete-actions">
                        <button
                            className="btn btn-primary"
                            onClick={() => window.open(`http://localhost:3333/space/${slug}`, '_blank')}
                        >
                            Open in Vibesmithy
                        </button>
                        {onClose && (
                            <button className="btn btn-secondary" onClick={onClose}>
                                Back to Dashboard
                            </button>
                        )}
                    </div>
                </div>
            )}

            {isFailed && (
                <div className="build-failed-message">
                    Vine build failed. Check the logs for details.
                    {onClose && (
                        <button className="btn btn-secondary" onClick={onClose}>
                            Back to Dashboard
                        </button>
                    )}
                </div>
            )}
        </div>
    );
}
