/**
 * PyramidCard — per-pyramid miniature card for the grid view.
 *
 * Shows a MiniaturePyramid shape, slug name, key stats,
 * building indicator, and activity glow.
 */

import { useMemo } from 'react';
import { MiniaturePyramid } from './MiniaturePyramid';
import { CONTENT_TYPE_CONFIG, relativeTime } from '../pyramid-types';
import type { GridPyramid } from './useGridData';

interface PyramidCardProps {
    pyramid: GridPyramid;
    maxDotsPerLayer: number;
    onClick: (slug: string) => void;
}

/**
 * Approximate layer distribution for the miniature shape.
 * Without per-depth node counts from a full tree load, we use a heuristic:
 * each layer gets roughly half the previous, with L0 as the wide base.
 */
function approximateLayers(
    nodeCount: number,
    maxDepth: number,
): { depth: number; count: number }[] {
    if (nodeCount === 0 || maxDepth < 0) return [];

    const layers: { depth: number; count: number }[] = [];
    let remaining = nodeCount;

    for (let d = 0; d <= maxDepth; d++) {
        const isLast = d === maxDepth;
        // Each higher layer gets roughly half the previous
        // L0 ~60%, L1 ~25%, L2 ~10%, L3 ~5%...
        const fraction = d === 0 ? 0.6 : d === 1 ? 0.25 : 0.5;
        const count = isLast
            ? Math.max(1, remaining)
            : Math.max(1, Math.ceil(remaining * fraction));
        layers.push({ depth: d, count });
        remaining -= count;
        if (remaining <= 0 && !isLast) {
            // Still add remaining depths with count=1 for shape
            for (let r = d + 1; r <= maxDepth; r++) {
                layers.push({ depth: r, count: 1 });
            }
            break;
        }
    }

    return layers;
}

/**
 * Compute glow intensity from lastActivityMs.
 * Recent activity (< 5s) = full glow (1.0), fading to 0 over 60s.
 */
function glowIntensity(lastActivityMs: number): number {
    if (lastActivityMs === 0) return 0;
    const age = Date.now() - lastActivityMs;
    if (age < 0) return 1;
    if (age < 5_000) return 1;
    if (age > 60_000) return 0;
    // Linear fade from 1.0 at 5s to 0 at 60s
    return 1 - (age - 5_000) / 55_000;
}

export function PyramidCard({ pyramid, maxDotsPerLayer, onClick }: PyramidCardProps) {
    const layers = useMemo(
        () => approximateLayers(pyramid.nodeCount, pyramid.maxDepth),
        [pyramid.nodeCount, pyramid.maxDepth],
    );

    const contentConfig = CONTENT_TYPE_CONFIG[pyramid.contentType];
    const intensity = glowIntensity(pyramid.lastActivityMs);

    return (
        <button
            className="ps-card"
            style={intensity > 0 ? { '--glow-intensity': intensity } as React.CSSProperties : undefined}
            onClick={() => onClick(pyramid.slug)}
            title={`${pyramid.slug} — ${pyramid.sourcePath}`}
        >
            {/* Activity glow overlay */}
            {intensity > 0 && <div className="ps-card-glow" />}

            {/* Miniature pyramid shape */}
            <div className="ps-card-mini">
                <MiniaturePyramid
                    layers={layers}
                    maxDotsPerLayer={maxDotsPerLayer}
                    width={140}
                    height={80}
                />
                {/* Building indicator */}
                {pyramid.isBuilding && (
                    <div className="ps-card-building">
                        <span className="ps-card-building-dot" />
                        <span className="ps-card-building-dot" />
                        <span className="ps-card-building-dot" />
                    </div>
                )}
            </div>

            {/* Slug name */}
            <div className="ps-card-name">{pyramid.slug}</div>

            {/* Stats row */}
            <div className="ps-card-stats">
                <span className="ps-card-badge" style={{ borderColor: contentConfig.color, color: contentConfig.color }}>
                    {contentConfig.icon} {contentConfig.label}
                </span>
                <span className="ps-card-stat">{pyramid.nodeCount} nodes</span>
                <span className="ps-card-stat">{relativeTime(pyramid.lastBuiltAt)}</span>
            </div>
        </button>
    );
}
