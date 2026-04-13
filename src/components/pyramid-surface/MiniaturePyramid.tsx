import { useRef, useEffect, useMemo } from 'react';

interface MiniaturePyramidProps {
    /** Node counts per depth level, sorted ascending by depth. depth -1 = bedrock. */
    layers: { depth: number; count: number }[];
    /** Max dots per layer — from viz config. Collapse to this ceiling. */
    maxDotsPerLayer: number;
    /** Per-dot activity intensity (0–1) keyed by "depth:dotIndex". Cool-to-hot glow. */
    activity?: Map<string, number>;
    /** Canvas dimensions */
    width: number;
    height: number;
    /** If true, disable collapse entirely (supercomputer mode) */
    forceAllNodes?: boolean;
    /** Optional highlight: "you are here" indicator */
    highlightDepth?: number;
    highlightDotIndex?: number;
}

// Cool-to-hot color ramp: white → cyan → green → yellow → orange → red
function activityColor(intensity: number): string {
    if (intensity <= 0) return 'rgba(255, 255, 255, 0.3)';
    if (intensity < 0.2) return `rgba(34, 211, 238, ${0.4 + intensity * 2})`;
    if (intensity < 0.5) return `rgba(64, 208, 128, ${0.5 + intensity})`;
    if (intensity < 0.8) return `rgba(238, 200, 34, ${0.6 + intensity * 0.4})`;
    return `rgba(255, 100, 60, ${0.8 + intensity * 0.2})`;
}

export function MiniaturePyramid({
    layers,
    maxDotsPerLayer,
    activity,
    width,
    height,
    forceAllNodes = false,
    highlightDepth,
    highlightDotIndex,
}: MiniaturePyramidProps) {
    const canvasRef = useRef<HTMLCanvasElement>(null);

    // Compute collapsed dot counts per layer
    const collapsedLayers = useMemo(() => {
        return layers.map(({ depth, count }) => {
            const rendered = forceAllNodes ? count : Math.min(count, maxDotsPerLayer);
            return { depth, actualCount: count, renderedCount: rendered };
        });
    }, [layers, maxDotsPerLayer, forceAllNodes]);

    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas) return;
        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        const dpr = window.devicePixelRatio || 1;
        canvas.width = width * dpr;
        canvas.height = height * dpr;
        canvas.style.width = `${width}px`;
        canvas.style.height = `${height}px`;
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

        // Clear
        ctx.clearRect(0, 0, width, height);

        if (collapsedLayers.length === 0) return;

        const padding = 2;
        const usableW = width - padding * 2;
        const usableH = height - padding * 2;
        const xCenter = width / 2;

        const depths = collapsedLayers.map((l) => l.depth);
        const minDepth = Math.min(...depths);
        const maxDepth = Math.max(...depths);
        const depthRange = Math.max(maxDepth - minDepth, 1);

        // Dot size: 1–4px depending on available space
        const maxRendered = Math.max(...collapsedLayers.map((l) => l.renderedCount));
        const dotRadius = Math.max(0.5, Math.min(2, (usableW / (maxRendered * 3))));

        for (const layer of collapsedLayers) {
            const normalizedDepth = (layer.depth - minDepth) / depthRange;
            const y = padding + usableH * (1 - normalizedDepth);

            // Narrowing: higher depth = narrower
            const narrowFactor = layer.depth < 0 ? 0 : maxDepth > 0 ? (layer.depth / maxDepth) * 0.85 : 0;
            const bandHalfWidth = (usableW / 2) * (1 - narrowFactor);

            // Pack tighter as collapse ratio increases
            const collapseRatio = layer.actualCount / layer.renderedCount;
            const packFactor = Math.min(1, 1 / Math.sqrt(collapseRatio));

            for (let i = 0; i < layer.renderedCount; i++) {
                const t = layer.renderedCount > 1 ? i / (layer.renderedCount - 1) : 0.5;
                const x = xCenter - bandHalfWidth * packFactor + t * bandHalfWidth * 2 * packFactor;

                // Activity intensity for this dot
                const key = `${layer.depth}:${i}`;
                const intensity = activity?.get(key) ?? 0;

                // Highlight "you are here"
                const isHighlight = layer.depth === highlightDepth && i === highlightDotIndex;

                ctx.beginPath();
                ctx.arc(x, y, isHighlight ? dotRadius * 2 : dotRadius, 0, Math.PI * 2);
                ctx.fillStyle = isHighlight ? 'rgba(255, 255, 255, 1)' : activityColor(intensity);
                ctx.fill();

                // Glow on active dots
                if (intensity > 0.3 || isHighlight) {
                    ctx.shadowBlur = isHighlight ? 6 : 3;
                    ctx.shadowColor = isHighlight ? 'rgba(255, 255, 255, 0.8)' : activityColor(intensity);
                    ctx.beginPath();
                    ctx.arc(x, y, dotRadius, 0, Math.PI * 2);
                    ctx.fill();
                    ctx.shadowBlur = 0;
                }
            }
        }
    }, [collapsedLayers, activity, width, height, highlightDepth, highlightDotIndex]);

    return (
        <canvas
            ref={canvasRef}
            className="miniature-pyramid"
            style={{ width, height, display: 'block' }}
        />
    );
}
