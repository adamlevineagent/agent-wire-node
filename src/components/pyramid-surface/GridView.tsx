/**
 * GridView — responsive grid layout showing all pyramids as miniature cards.
 *
 * Mission Control overview: see every pyramid at a glance, sorted by
 * name / activity / node count / last built.
 */

import { useGridData } from './useGridData';
import { PyramidCard } from './PyramidCard';
import type { GridSortKey } from './useGridData';

interface GridViewProps {
    onSelectPyramid: (slug: string) => void;
    maxDotsPerLayer: number;
}

const SORT_OPTIONS: { key: GridSortKey; label: string }[] = [
    { key: 'activity', label: 'Activity' },
    { key: 'name', label: 'Name' },
    { key: 'nodeCount', label: 'Nodes' },
    { key: 'lastBuilt', label: 'Last Built' },
];

export function GridView({ onSelectPyramid, maxDotsPerLayer }: GridViewProps) {
    const { pyramids, loading, sortBy, setSortBy, refresh } = useGridData();

    // ── Loading state ───────────────────────────────────────────────
    if (loading && pyramids.length === 0) {
        return (
            <div className="ps-grid-loading">
                <div className="ps-grid-loading-text">Loading pyramids...</div>
            </div>
        );
    }

    // ── Empty state ─────────────────────────────────────────────────
    if (!loading && pyramids.length === 0) {
        return (
            <div className="ps-grid-empty">
                <div className="ps-grid-empty-title">No pyramids yet</div>
                <div className="ps-grid-empty-sub">
                    Create a workspace to start building knowledge pyramids
                </div>
            </div>
        );
    }

    return (
        <div className="ps-grid-wrapper">
            {/* Sort controls */}
            <div className="ps-grid-controls">
                <div className="ps-grid-sort-group">
                    {SORT_OPTIONS.map((opt) => (
                        <button
                            key={opt.key}
                            className={`ps-grid-sort-btn ${sortBy === opt.key ? 'active' : ''}`}
                            onClick={() => setSortBy(opt.key)}
                        >
                            {opt.label}
                        </button>
                    ))}
                </div>
                <button className="ps-grid-refresh-btn" onClick={refresh} title="Refresh">
                    Refresh
                </button>
            </div>

            {/* Pyramid card grid */}
            <div className="ps-grid">
                {pyramids.map((p) => (
                    <PyramidCard
                        key={p.slug}
                        pyramid={p}
                        maxDotsPerLayer={maxDotsPerLayer}
                        onClick={onSelectPyramid}
                    />
                ))}
            </div>
        </div>
    );
}
