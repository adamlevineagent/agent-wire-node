// Phase 13 — Cross-Pyramid Timeline top-level view.
//
// Shows every active build in a single list, a running cost
// accumulator across all slugs, and the "Pause..." button (Phase 18c
// L9 — was "Pause All DADBEAR" in Phase 13).
// Clicking "View" on a row opens the detailed PyramidBuildViz in a
// drawer.
//
// Phase 15 relocated the `CostRollupSection` from this view to the
// DADBEAR Oversight page (the spec-intended home for the spend
// rollup). Cost attribution for individual in-flight builds still
// lives in `CrossPyramidCostFooter`; the full rollup with pivots
// now lives in `DadbearOversightPage.tsx`.
//
// Phase 18c (L9): the Pause All button now opens a scope picker modal
// (DadbearPauseScopeModal) instead of going straight to scope=all.
// The user can pause every pyramid (the original behavior), every
// pyramid under a folder path, or — once the schema lands — every
// pyramid in a specific Wire circle.

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useCrossPyramidTimeline } from '../hooks/useCrossPyramidTimeline';
import { ActiveBuildRow } from './ActiveBuildRow';
import { CrossPyramidCostFooter } from './CrossPyramidCostFooter';
import { PyramidBuildViz } from './PyramidBuildViz';
import {
    DadbearPauseScopeModal,
    type DadbearPauseScope,
} from './DadbearPauseScopeModal';

export function CrossPyramidTimeline() {
    const { state, refreshActive } = useCrossPyramidTimeline();
    const [viewSlug, setViewSlug] = useState<string | null>(null);
    // Phase 18c (L9): when set, the scope picker modal is open in
    // either "pause" or "resume" mode. Single state field instead of
    // a separate boolean per action — simpler open/close semantics.
    const [scopeModalAction, setScopeModalAction] = useState<
        'pause' | 'resume' | null
    >(null);
    const [pausedBanner, setPausedBanner] = useState<number | null>(null);
    const [toast, setToast] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);

    // Phase 18c (L9): scope picker confirms with a (scope, scope_value)
    // tuple. Pass them through to the existing pause/resume IPCs which
    // already accept these parameters.
    const doPauseWithScope = useCallback(
        async (scope: DadbearPauseScope, scopeValue: string | null) => {
            setScopeModalAction(null);
            setError(null);
            try {
                const resp = await invoke<{ affected: number }>(
                    'pyramid_pause_dadbear_all',
                    { scope, scopeValue },
                );
                setPausedBanner(resp.affected);
                setToast(
                    `Paused DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`,
                );
                window.setTimeout(() => setToast(null), 4000);
            } catch (e) {
                setError(String(e));
            }
        },
        [],
    );

    const doResumeWithScope = useCallback(
        async (scope: DadbearPauseScope, scopeValue: string | null) => {
            setScopeModalAction(null);
            setError(null);
            try {
                const resp = await invoke<{ affected: number }>(
                    'pyramid_resume_dadbear_all',
                    { scope, scopeValue },
                );
                setPausedBanner(null);
                setToast(
                    `Resumed DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`,
                );
                window.setTimeout(() => setToast(null), 4000);
            } catch (e) {
                setError(String(e));
            }
        },
        [],
    );

    // Resume from the banner: short-circuits the scope picker because
    // the banner shows up after a pause-all-style action and the user
    // typically wants the same scope back. Defaults to scope=all to
    // mirror the legacy banner-resume behavior; users who paused via
    // a more specific scope can re-open the picker via the resume
    // path on the DADBEAR Oversight page.
    const doResumeAll = useCallback(async () => {
        await doResumeWithScope('all', null);
    }, [doResumeWithScope]);

    const activeCount = state.activeBuilds.length;

    return (
        <div className="cross-pyramid-timeline">
            <div className="cpt-header">
                <h2>Cross-Pyramid Build Timeline</h2>
                <div className="cpt-header-actions">
                    <button className="btn btn-secondary" onClick={refreshActive}>
                        Refresh
                    </button>
                    {/* Phase 18c (L9): "Pause..." (with ellipsis) signals
                        the scope picker modal opens, replacing Phase 13's
                        "Pause All DADBEAR" which went straight to scope=all. */}
                    <button
                        className="btn btn-danger"
                        onClick={() => setScopeModalAction('pause')}
                        disabled={pausedBanner !== null}
                    >
                        Pause...
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

            {/* Phase 18c (L9): scope picker modal for pause/resume. */}
            {scopeModalAction && (
                <DadbearPauseScopeModal
                    action={scopeModalAction}
                    onCancel={() => setScopeModalAction(null)}
                    onConfirm={
                        scopeModalAction === 'pause'
                            ? doPauseWithScope
                            : doResumeWithScope
                    }
                />
            )}
        </div>
    );
}
