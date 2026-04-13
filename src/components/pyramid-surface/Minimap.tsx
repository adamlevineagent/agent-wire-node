/**
 * Phase 4 — Minimap.
 *
 * Thin wrapper around MiniaturePyramid with default sizing (100x60)
 * and the "you are here" highlight feature already built into
 * MiniaturePyramid.
 */

import { MiniaturePyramid } from './MiniaturePyramid';

// ── Props ───────────────────────────────────────────────────────────

interface MinimapProps {
    layers: { depth: number; count: number }[];
    maxDotsPerLayer: number;
    highlightDepth?: number;
    highlightDotIndex?: number;
    width?: number;
    height?: number;
}

// ── Component ───────────────────────────────────────────────────────

export function Minimap({
    layers,
    maxDotsPerLayer,
    highlightDepth,
    highlightDotIndex,
    width = 100,
    height = 60,
}: MinimapProps) {
    return (
        <MiniaturePyramid
            layers={layers}
            maxDotsPerLayer={maxDotsPerLayer}
            width={width}
            height={height}
            highlightDepth={highlightDepth}
            highlightDotIndex={highlightDotIndex}
        />
    );
}
