// Phase 13 — Cross-Pyramid Timeline top-level view.
//
// Shows every active build in a single list, a running cost
// accumulator across all slugs, the spend rollup section, and the
// "Pause All DADBEAR" button. Clicking "View" on a row opens the
// detailed PyramidBuildViz in a drawer.

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useCrossPyramidTimeline } from '../hooks/useCrossPyramidTimeline';
import { ActiveBuildRow } from './ActiveBuildRow';
import { CrossPyramidCostFooter } from './CrossPyramidCostFooter';
import { CostRollupSection } from './CostRollupSection';
import { PyramidBuildViz } from './PyramidBuildViz';

interface PauseAllConfirmModalProps {
    count: number;
    onCancel: () => void;
    onConfirm: () => void;
}

function PauseAllConfirmModal({ count, onCancel, onConfirm }: PauseAllConfirmModalProps) {
    return (
        <div className="cpt-confirm-backdrop" onClick={onCancel}>
            <div className="cpt-confirm-modal" onClick={e => e.stopPropagation()}>
                <h3>Pause DADBEAR?</h3>
                <p>
                    This will pause DADBEAR auto-update on <strong>{count}</strong>{' '}
                    pyramid{count === 1 ? '' : 's'}. In-flight builds will continue;
                    only the background stale-check loop stops.
                </p>
                <div className="cpt-confirm-actions">
                    <button className="btn btn-secondary" onClick={onCancel}>
                        Cancel
                    </button>
                    <button className="btn btn-danger" onClick={onConfirm}>
                        Pause All
                    </button>
                </div>
            </div>
        </div>
    );
}

export function CrossPyramidTimeline() {
    const { state, refreshActive } = useCrossPyramidTimeline();
    const [viewSlug, setViewSlug] = useState<string | null>(null);
    const [confirming, setConfirming] = useState(false);
    const [pausedBanner, setPausedBanner] = useState<number | null>(null);
    const [toast, setToast] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);

    const doPauseAll = useCallback(async () => {
        setConfirming(false);
        setError(null);
        try {
            const resp = await invoke<{ affected: number }>('pyramid_pause_dadbear_all', {
                scope: 'all',
                scopeValue: null,
            });
            setPausedBanner(resp.affected);
            setToast(`Paused DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`);
            window.setTimeout(() => setToast(null), 4000);
        } catch (e) {
            setError(String(e));
        }
    }, []);

    const doResumeAll = useCallback(async () => {
        setError(null);
        try {
            const resp = await invoke<{ affected: number }>('pyramid_resume_dadbear_all', {
                scope: 'all',
                scopeValue: null,
            });
            setPausedBanner(null);
            setToast(`Resumed DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`);
            window.setTimeout(() => setToast(null), 4000);
        } catch (e) {
            setError(String(e));
        }
    }, []);

    const activeCount = state.activeBuilds.length;

    return (
        <div className="cross-pyramid-timeline">
            <div className="cpt-header">
                <h2>Cross-Pyramid Build Timeline</h2>
                <div className="cpt-header-actions">
                    <button className="btn btn-secondary" onClick={refreshActive}>
                        Refresh
                    </button>
                    <button
                        className="btn btn-danger"
                        onClick={() => setConfirming(true)}
                        disabled={pausedBanner !== null}
                    >
                        Pause All DADBEAR
                    </button>
                </div>
            </div>

            {pausedBanner !== null && (
                <div className="cpt-paused-banner">
                    DADBEAR paused on {pausedBanner} pyramid
                    {pausedBanner === 1 ? '' : 's'}
                    <button className="btn btn-primary" onClick={doResumeAll}>
                        Resume
                    </button>
                </div>
            )}

            {error && <div className="cpt-error">{error}</div>}
            {toast && <div className="cpt-toast">{toast}</div>}

            <section className="cpt-active-section">
                <div className="cpt-section-header">Active Builds ({activeCount})</div>
                {activeCount === 0 ? (
                    <div className="cpt-empty">No active builds.</div>
                ) : (
                    <div className="cpt-build-list">
                        {state.activeBuilds.map(summary => (
                            <ActiveBuildRow
                                key={summary.slug}
                                summary={summary}
                                liveState={state.byslug.get(summary.slug)}
                                onView={() => setViewSlug(summary.slug)}
                            />
                        ))}
                    </div>
                )}
            </section>

            <CrossPyramidCostFooter byslug={state.byslug} />

            <CostRollupSection />

            {viewSlug && (
                <div className="cpt-drawer-backdrop" onClick={() => setViewSlug(null)}>
                    <div className="cpt-drawer" onClick={e => e.stopPropagation()}>
                        <button
                            className="cpt-drawer-close"
                            onClick={() => setViewSlug(null)}
                            aria-label="Close"
                        >
                            ×
                        </button>
                        <PyramidBuildViz slug={viewSlug} onClose={() => setViewSlug(null)} />
                    </div>
                </div>
            )}

            {confirming && (
                <PauseAllConfirmModal
                    count={activeCount}
                    onCancel={() => setConfirming(false)}
                    onConfirm={doPauseAll}
                />
            )}
        </div>
    );
}
