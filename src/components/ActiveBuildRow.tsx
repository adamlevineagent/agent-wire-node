// Phase 13 — compact row for the CrossPyramidTimeline view.
//
// Renders one active build as a single row with progress, current
// step, cost so far, cache hit percentage, and a "View" button
// that opens the detailed PyramidBuildViz in a drawer.

import { BuildRowState } from '../hooks/useBuildRowState';
import { ActiveBuildRow as ActiveBuildSummary } from '../hooks/useCrossPyramidTimeline';

interface ActiveBuildRowProps {
    summary: ActiveBuildSummary;
    liveState?: BuildRowState;
    onView: () => void;
}

export function ActiveBuildRow({ summary, liveState, onView }: ActiveBuildRowProps) {
    // Prefer the live event stream for cost / current step; fall
    // back to the DB-computed summary values for fields the live
    // stream doesn't track.
    const liveCost = liveState?.cost.estimatedUsd ?? summary.cost_so_far_usd;
    const currentStep =
        liveState?.currentStep ?? summary.current_step ?? '(idle)';
    const cachePct = Math.round(summary.cache_hit_rate * 100);
    const progressPct =
        summary.total_steps > 0
            ? Math.min(
                  100,
                  Math.round((summary.completed_steps / summary.total_steps) * 100),
              )
            : 0;

    const stepCount = liveState?.steps.length ?? summary.total_steps;
    const statusLabel = summary.status || 'running';

    return (
        <div className={`cpt-build-row cpt-build-row-${statusLabel}`}>
            <div className="cpt-build-header">
                <div className="cpt-build-slug">{summary.slug}</div>
                <div className="cpt-build-status">{statusLabel}</div>
            </div>
            <div className="cpt-build-body">
                <div className="cpt-build-current-step">{currentStep}</div>
                <div className="cpt-build-progress">
                    <div
                        className="cpt-build-progress-fill"
                        style={{ width: `${progressPct}%` }}
                    />
                </div>
                <div className="cpt-build-stats">
                    <span>
                        {summary.completed_steps}/{summary.total_steps || stepCount} steps
                    </span>
                    <span>${liveCost.toFixed(3)}</span>
                    {cachePct > 0 && <span>cache {cachePct}%</span>}
                </div>
            </div>
            <button className="cpt-build-view btn btn-secondary" onClick={onView}>
                View
            </button>
        </div>
    );
}
