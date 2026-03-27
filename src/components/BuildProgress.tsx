import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface BuildStatus {
    slug: string;
    status: string; // "idle" | "running" | "complete" | "complete_with_errors" | "failed" | "cancelled"
    progress: { done: number; total: number };
    elapsed_seconds: number;
    failures: number;
}

interface BuildProgressProps {
    slug: string;
    onComplete?: (status: BuildStatus) => void;
    onClose?: () => void;
}

export function BuildProgress({ slug, onComplete, onClose }: BuildProgressProps) {
    const [status, setStatus] = useState<BuildStatus | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let active = true;

        const poll = async () => {
            while (active) {
                try {
                    const s = await invoke<BuildStatus>('pyramid_build_status', { slug });
                    if (!active) break;
                    setStatus(s);

                    if (['complete', 'complete_with_errors', 'failed', 'cancelled'].includes(s.status)) {
                        onComplete?.(s);
                        break;
                    }

                    const isFinalizing =
                        s.status === 'running' &&
                        s.progress.total > 0 &&
                        s.progress.done >= s.progress.total;
                    await new Promise((r) => setTimeout(r, isFinalizing ? 500 : 2000));
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

    const handleCancel = useCallback(async () => {
        try {
            await invoke('pyramid_build_cancel');
        } catch (err) {
            console.error('Cancel failed:', err);
        }
    }, []);

    const pct = status?.progress.total
        ? Math.round((status.progress.done / status.progress.total) * 100)
        : 0;

    const elapsed = status?.elapsed_seconds
        ? `${Math.floor(status.elapsed_seconds / 60)}m ${Math.floor(status.elapsed_seconds % 60)}s`
        : '0s';

    const isComplete = status?.status === 'complete' || status?.status === 'complete_with_errors';
    const isFailed = status?.status === 'failed';
    const isCancelled = status?.status === 'cancelled';
    const isRunning = status?.status === 'running';
    const isFinalizing =
        isRunning &&
        (status?.progress.total ?? 0) > 0 &&
        (status?.progress.done ?? 0) >= (status?.progress.total ?? 0);

    return (
        <div className="build-progress-panel">
            <div className="build-progress-header">
                <h3>Building Pyramid: {slug}</h3>
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
                {isCancelled && (
                    <span className="build-status-badge failed">Cancelled</span>
                )}
            </div>

            {error && (
                <div className="build-error">
                    Error: {error}
                </div>
            )}

            <div className="build-progress-bar-container">
                <div className="build-progress-bar">
                    <div
                        className={`build-progress-fill ${isComplete ? 'complete' : isFailed ? 'failed' : ''}`}
                        style={{ width: `${pct}%` }}
                    />
                </div>
                <div className="build-progress-stats">
                    <span>{pct}% ({status?.progress.done || 0}/{status?.progress.total || 0} estimated nodes)</span>
                    <span>Elapsed: {elapsed}</span>
                </div>
            </div>

            {isComplete && status && (
                <div className="build-complete-summary">
                    Pyramid built! {status.progress.done} nodes built.{status.failures > 0 ? ` (${status.failures} failures)` : ''}
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
                    Build failed. Check the logs for details.
                    {onClose && (
                        <button className="btn btn-secondary" onClick={onClose}>
                            Back to Dashboard
                        </button>
                    )}
                </div>
            )}

            {isRunning && (
                <div className="build-actions">
                    <button className="btn btn-danger" onClick={handleCancel}>
                        Cancel Build
                    </button>
                </div>
            )}
        </div>
    );
}
