// Phase 13 — running total footer for the CrossPyramidTimeline.
//
// Sums the estimated + actual cost across every per-slug
// BuildRowState currently tracked in `useCrossPyramidTimeline`.

import { BuildRowState } from '../hooks/useBuildRowState';

interface Props {
    byslug: Map<string, BuildRowState>;
}

export function CrossPyramidCostFooter({ byslug }: Props) {
    let totalEstimated = 0;
    let totalActual: number | null = null;
    let totalSavings = 0;
    for (const row of byslug.values()) {
        totalEstimated += row.cost.estimatedUsd;
        if (row.cost.actualUsd !== null) {
            totalActual = (totalActual ?? 0) + row.cost.actualUsd;
        }
        totalSavings += row.cost.cacheSavingsUsd;
    }

    if (byslug.size === 0) {
        return null;
    }

    return (
        <div className="cpt-cost-footer">
            <div className="cpt-cost-footer-total">
                Total active spend: <strong>${totalEstimated.toFixed(2)}</strong> est
                {totalActual !== null && (
                    <> / <strong>${totalActual.toFixed(2)}</strong> actual</>
                )}
            </div>
            {totalSavings > 0 && (
                <div className="cpt-cost-footer-savings">
                    Cache savings so far: <strong>${totalSavings.toFixed(2)}</strong>
                </div>
            )}
        </div>
    );
}
