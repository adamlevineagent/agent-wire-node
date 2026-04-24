import type { PyramidRenderer } from './PyramidRenderer';
import type { SurfaceNode, SurfaceEdge, NodeEncoding, OverlayState, HitTestResult, VizStepConfig, BuildVizState } from './types';
import { NodeVisualState, EdgeCategory } from './types';

// ── Node color map ──────────────────────────────────────────────────

const NODE_COLORS: Record<string, { fill: string; hover: string }> = {
    [NodeVisualState.STABLE]: {
        fill: 'rgba(34, 211, 238, 0.4)',
        hover: 'rgba(34, 211, 238, 0.7)',
    },
    [NodeVisualState.STALE_CONFIRMED]: {
        fill: 'rgba(64, 208, 128, 0.9)',
        hover: 'rgba(64, 208, 128, 1.0)',
    },
    [NodeVisualState.JUST_UPDATED]: {
        fill: 'rgba(64, 208, 128, 0.7)',
        hover: 'rgba(64, 208, 128, 0.9)',
    },
    [NodeVisualState.NOT_STALE]: {
        fill: 'rgba(72, 230, 255, 0.82)',
        hover: 'rgba(120, 240, 255, 0.98)',
    },
    [NodeVisualState.BUILD_COMPLETE]: {
        fill: 'rgba(34, 211, 238, 0.7)',
        hover: 'rgba(34, 211, 238, 0.85)',
    },
    [NodeVisualState.BUILD_FAILED]: {
        fill: 'rgba(255, 100, 100, 0.7)',
        hover: 'rgba(255, 100, 100, 0.85)',
    },
    [NodeVisualState.BUILDING]: {
        fill: 'rgba(34, 211, 238, 0.3)',
        hover: 'rgba(34, 211, 238, 0.5)',
    },
    [NodeVisualState.CACHED]: {
        fill: 'rgba(34, 211, 238, 0.5)',
        hover: 'rgba(34, 211, 238, 0.65)',
    },
};

const BEDROCK_FILL = 'rgba(120, 160, 180, 0.3)';
const BEDROCK_HOVER_FILL = 'rgba(120, 160, 180, 0.55)';
const LABEL_COLOR = 'rgba(255, 255, 255, 0.2)';

// ── Edge style config ───────────────────────────────────────────────

interface EdgeStyle {
    color: string;
    lineWidth: number;
    dash?: number[];
}

const EDGE_STYLES: Record<string, EdgeStyle> = {
    [EdgeCategory.STRUCTURAL]: {
        color: 'rgba(34, 211, 238, 0.15)',
        lineWidth: 0.5,
    },
    [EdgeCategory.BEDROCK]: {
        color: 'rgba(120, 160, 180, 0.1)',
        lineWidth: 0.5,
    },
    [EdgeCategory.WEB]: {
        color: 'rgba(168, 85, 247, 0.25)',
        lineWidth: 0.5,
        dash: [2, 3],
    },
    [EdgeCategory.EVIDENCE]: {
        color: 'rgba(34, 211, 238, 0.35)',
        lineWidth: 1,
    },
};

// ── Helpers ─────────────────────────────────────────────────────────

function nodeTitle(node: Pick<SurfaceNode, 'headline' | 'id' | 'selfPrompt' | 'question'>): string {
    if (node.question?.trim()) return node.question;
    if (node.selfPrompt?.trim()) return node.selfPrompt;
    return node.headline?.trim() ? node.headline : node.id;
}

/** Parse "rgba(r, g, b, a)" into components. Returns null on failure. */
function parseRgba(rgba: string): { r: number; g: number; b: number; a: number } | null {
    const m = rgba.match(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+))?\s*\)/);
    if (!m) return null;
    return {
        r: parseInt(m[1], 10),
        g: parseInt(m[2], 10),
        b: parseInt(m[3], 10),
        a: m[4] !== undefined ? parseFloat(m[4]) : 1,
    };
}

/** Rebuild an rgba string with modified alpha. */
function withAlpha(rgba: string, alphaMultiplier: number): string {
    const parsed = parseRgba(rgba);
    if (!parsed) return rgba;
    const a = Math.min(1, Math.max(0, parsed.a * alphaMultiplier));
    return `rgba(${parsed.r}, ${parsed.g}, ${parsed.b}, ${a.toFixed(3)})`;
}

/**
 * Modulate color saturation via HSL.
 * saturationFactor: 0 = fully desaturated (gray), 1 = original color.
 */
function withSaturation(rgba: string, saturationFactor: number): string {
    const parsed = parseRgba(rgba);
    if (!parsed) return rgba;
    const { r, g, b, a } = parsed;

    // Convert RGB [0-255] to HSL
    const rn = r / 255, gn = g / 255, bn = b / 255;
    const max = Math.max(rn, gn, bn), min = Math.min(rn, gn, bn);
    const l = (max + min) / 2;
    let h = 0, s = 0;

    if (max !== min) {
        const d = max - min;
        s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
        switch (max) {
            case rn: h = ((gn - bn) / d + (gn < bn ? 6 : 0)) / 6; break;
            case gn: h = ((bn - rn) / d + 2) / 6; break;
            case bn: h = ((rn - gn) / d + 4) / 6; break;
        }
    }

    // Apply saturation modulation
    s = s * Math.max(0, Math.min(1, saturationFactor));

    // Convert HSL back to RGB
    const hue2rgb = (p: number, q: number, t: number): number => {
        if (t < 0) t += 1;
        if (t > 1) t -= 1;
        if (t < 1/6) return p + (q - p) * 6 * t;
        if (t < 1/2) return q;
        if (t < 2/3) return p + (q - p) * (2/3 - t) * 6;
        return p;
    };

    let ro: number, go: number, bo: number;
    if (s === 0) {
        ro = go = bo = l;
    } else {
        const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
        const p = 2 * l - q;
        ro = hue2rgb(p, q, h + 1/3);
        go = hue2rgb(p, q, h);
        bo = hue2rgb(p, q, h - 1/3);
    }

    return `rgba(${Math.round(ro * 255)}, ${Math.round(go * 255)}, ${Math.round(bo * 255)}, ${a.toFixed(3)})`;
}

// ── CanvasRenderer ──────────────────────────────────────────────────

export class CanvasRenderer implements PyramidRenderer {
    private canvas: HTMLCanvasElement | null = null;
    private ctx: CanvasRenderingContext2D | null = null;
    private container: HTMLElement | null = null;
    private dpr = 1;
    private w = 0;
    private h = 0;
    private rafId = 0;
    private nodeEncodings = new Map<string, NodeEncoding>();
    private pulsePhase = 0;
    private lastPulseTime = 0;
    private activeVizConfig: VizStepConfig | null = null;
    private buildVizState: BuildVizState | null = null;
    private linkIntensities = new Map<string, number>();
    private densityLabelThreshold = 0;

    // ── Lifecycle ───────────────────────────────────────────────────

    attach(container: HTMLElement): void {
        this.container = container;

        const canvas = document.createElement('canvas');
        canvas.style.display = 'block';
        container.appendChild(canvas);
        this.canvas = canvas;

        const ctx = canvas.getContext('2d');
        if (!ctx) {
            throw new Error('CanvasRenderer: failed to get 2d context');
        }
        this.ctx = ctx;
        this.dpr = window.devicePixelRatio || 1;

        // Size to container
        const rect = container.getBoundingClientRect();
        this.applySize(rect.width, rect.height);
    }

    destroy(): void {
        if (this.rafId) {
            cancelAnimationFrame(this.rafId);
            this.rafId = 0;
        }
        if (this.canvas && this.container) {
            this.container.removeChild(this.canvas);
        }
        this.canvas = null;
        this.ctx = null;
        this.container = null;
        this.nodeEncodings.clear();
    }

    resize(width: number, height: number): void {
        this.applySize(width, height);
    }

    private applySize(width: number, height: number): void {
        if (!this.canvas || !this.ctx) return;
        this.w = width;
        this.h = height;
        this.dpr = window.devicePixelRatio || 1;

        this.canvas.width = Math.round(width * this.dpr);
        this.canvas.height = Math.round(height * this.dpr);
        this.canvas.style.width = `${width}px`;
        this.canvas.style.height = `${height}px`;

        this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    }

    // ── Encoding ────────────────────────────────────────────────────

    setNodeEncoding(nodeId: string, encoding: NodeEncoding): void {
        this.nodeEncodings.set(nodeId, encoding);
    }

    setNodeEncodings(encodings: Map<string, NodeEncoding>): void {
        this.nodeEncodings = new Map(encodings);
    }

    setActiveVizConfig(config: VizStepConfig | null): void {
        this.activeVizConfig = config;
    }

    setBuildVizState(state: BuildVizState): void {
        this.buildVizState = state;
    }

    setLinkIntensities(intensities: Map<string, number>): void {
        this.linkIntensities = new Map(intensities);
    }

    setDensityLabelThreshold(minRadius: number): void {
        this.densityLabelThreshold = minRadius;
    }

    // ── Hit testing ─────────────────────────────────────────────────

    hitTest(x: number, y: number, nodes: SurfaceNode[]): HitTestResult | null {
        if (!this.container || nodes.length === 0) return null;

        const rect = this.container.getBoundingClientRect();
        const mx = x - rect.left;
        const my = y - rect.top;

        // Reverse depth order: apex (highest depth) gets priority
        const sorted = [...nodes].sort((a, b) => b.depth - a.depth);

        for (const node of sorted) {
            const dx = mx - node.x;
            const dy = my - node.y;
            const hitRadius = node.radius + 4;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
                return { nodeId: node.id, node };
            }
        }

        return null;
    }

    // ── Dimensions ──────────────────────────────────────────────────

    getDimensions(): { width: number; height: number } {
        return { width: this.w, height: this.h };
    }

    // ── Main render ─────────────────────────────────────────────────

    render(
        nodes: SurfaceNode[],
        edges: SurfaceEdge[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        const ctx = this.ctx;
        if (!ctx) return;

        // Update pulse phase for building nodes
        const now = performance.now();
        if (this.lastPulseTime > 0) {
            const dt = now - this.lastPulseTime;
            this.pulsePhase = (this.pulsePhase + dt * 0.003) % (Math.PI * 2);
        }
        this.lastPulseTime = now;

        // 1. Clear
        ctx.clearRect(0, 0, this.w, this.h);

        if (nodes.length === 0) {
            this.drawEmptyState(ctx);
            return;
        }

        // 2. Draw edges
        this.drawEdges(ctx, edges, overlays);

        // 3. Layer labels
        this.drawLayerLabels(ctx, nodes);

        // 4. Draw nodes (sorted by depth ascending; apex renders on top)
        this.drawNodes(ctx, nodes, overlays, hoveredNodeId);

        // 5. Viz primitive overlay (build-time visuals)
        this.drawVizOverlay(ctx, nodes);

        // 6. Apex label
        this.drawApexLabel(ctx, nodes);

        // 7. Density-mode node labels (nodes above threshold)
        this.drawDensityLabels(ctx, nodes);
    }

    // ── Draw sub-routines ───────────────────────────────────────────

    private drawEmptyState(ctx: CanvasRenderingContext2D): void {
        ctx.beginPath();
        ctx.moveTo(this.w / 2, 40);
        ctx.lineTo(this.w - 40, this.h - 40);
        ctx.lineTo(40, this.h - 40);
        ctx.closePath();
        ctx.strokeStyle = 'rgba(34, 211, 238, 0.08)';
        ctx.lineWidth = 1;
        ctx.stroke();

        ctx.font = '14px Inter, sans-serif';
        ctx.fillStyle = 'rgba(255, 255, 255, 0.3)';
        ctx.textAlign = 'center';
        ctx.fillText('Build a pyramid to see it here', this.w / 2, this.h / 2);
    }

    private drawEdges(
        ctx: CanvasRenderingContext2D,
        edges: SurfaceEdge[],
        overlays: OverlayState,
    ): void {
        for (const edge of edges) {
            // Filter by overlay state
            if (edge.category === EdgeCategory.BEDROCK && !overlays.provenance) continue;
            if (edge.category === EdgeCategory.WEB && !overlays.web) continue;
            if (edge.category === EdgeCategory.EVIDENCE && !overlays.structure) continue;
            // STRUCTURAL edges always render

            const style = EDGE_STYLES[edge.category] ?? EDGE_STYLES[EdgeCategory.STRUCTURAL];

            ctx.save();

            // Link intensity modulation
            const intensityKey = `${edge.fromId}\u2192${edge.toId}`;
            const intensity = this.linkIntensities.get(intensityKey);
            if (intensity !== undefined && overlays.weightIntensity) {
                ctx.lineWidth = 0.5 + intensity * 3;
                ctx.globalAlpha = 0.1 + intensity * 0.5;
                ctx.strokeStyle = style.color;
            } else {
                ctx.strokeStyle = style.color;
                ctx.lineWidth = style.lineWidth;
            }

            if (style.dash) {
                ctx.setLineDash(style.dash);
            }

            ctx.beginPath();
            ctx.moveTo(edge.fromX, edge.fromY);
            ctx.quadraticCurveTo(edge.controlX, edge.controlY, edge.toX, edge.toY);
            ctx.stroke();

            ctx.restore();
        }
    }

    private drawLayerLabels(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        ctx.font = '10px monospace';
        ctx.fillStyle = LABEL_COLOR;
        ctx.textAlign = 'left';

        const drawnDepths = new Set<number>();
        for (const node of nodes) {
            if (drawnDepths.has(node.depth)) continue;
            drawnDepths.add(node.depth);
            const label = node.depth === -1 ? 'BEDROCK' : `L${node.depth}`;
            ctx.fillText(label, 12, node.y + 3);
        }
    }

    private drawNodes(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        // Sort by depth ascending so apex (highest depth) renders last (on top)
        const sorted = [...nodes].sort((a, b) => a.depth - b.depth);

        for (const node of sorted) {
            const isHovered = node.id === hoveredNodeId;
            const isBedrock = node.depth === -1;

            // Determine effective visual state: if staleness overlay is off, treat as stable
            const effectiveState = overlays.staleness ? node.state : NodeVisualState.STABLE;

            if (isBedrock) {
                this.drawBedrockNode(ctx, node, isHovered);
            } else {
                this.drawStandardNode(ctx, node, effectiveState, isHovered, overlays);
            }
        }

        // Reset shadow after all nodes
        ctx.shadowBlur = 0;
        ctx.shadowColor = 'transparent';
    }

    private drawBedrockNode(
        ctx: CanvasRenderingContext2D,
        node: SurfaceNode,
        isHovered: boolean,
    ): void {
        const drawRadius = isHovered ? node.radius * 1.6 : node.radius;

        ctx.shadowBlur = 0;
        ctx.shadowColor = 'transparent';

        ctx.beginPath();
        ctx.arc(node.x, node.y, drawRadius, 0, Math.PI * 2);
        ctx.fillStyle = isHovered ? BEDROCK_HOVER_FILL : BEDROCK_FILL;
        ctx.fill();

        // Rotated filename label below
        ctx.save();
        ctx.font = '8px Inter, sans-serif';
        ctx.fillStyle = 'rgba(120, 160, 180, 0.5)';
        ctx.textAlign = 'left';
        ctx.translate(node.x, node.y + drawRadius + 6);
        ctx.rotate(-Math.PI / 4);
        const label = node.headline.length > 24
            ? node.headline.slice(0, 22) + '..'
            : node.headline;
        ctx.fillText(label, 0, 0);
        ctx.restore();
    }

    private drawStandardNode(
        ctx: CanvasRenderingContext2D,
        node: SurfaceNode,
        state: NodeVisualState,
        isHovered: boolean,
        overlays: OverlayState,
    ): void {
        const colors = NODE_COLORS[state] ?? NODE_COLORS[NodeVisualState.STABLE];
        let fillColor = isHovered ? colors.hover : colors.fill;
        const drawRadius = isHovered ? node.radius * 1.4 : node.radius;

        // Three-axis encoding modulation (Phase 3b: brightness, saturation, border)
        const encoding = overlays.weightIntensity
            ? this.nodeEncodings.get(node.id)
            : undefined;
        let strokeWidth = 0;

        if (encoding) {
            // Axis 1 — Brightness modulates fill alpha
            fillColor = withAlpha(fillColor, 0.3 + encoding.brightness * 0.7);
            // Axis 2 — Saturation modulates color vividness via HSL
            // 0.2 base keeps desaturated nodes visible; 0.8 range for full encoding
            fillColor = withSaturation(fillColor, 0.2 + encoding.saturation * 0.8);
            // Axis 3 — borderThickness maps to stroke width (0-1 => 0-3px)
            strokeWidth = encoding.borderThickness * 3;
        }

        // Building state: pulse effect
        if (state === NodeVisualState.BUILDING) {
            const pulse = 0.5 + 0.5 * Math.sin(this.pulsePhase);
            fillColor = withAlpha(fillColor, 0.3 + pulse * 0.4);
        }

        // Cached state: subtle indicator ring
        const isCached = state === NodeVisualState.CACHED;

        // Glow on stale/not_stale/just_updated nodes
        if (state === NodeVisualState.STALE_CONFIRMED || state === NodeVisualState.JUST_UPDATED) {
            ctx.shadowBlur = isHovered ? 22 : 16;
            ctx.shadowColor = 'rgba(64, 208, 128, 0.55)';
        } else if (state === NodeVisualState.NOT_STALE) {
            ctx.shadowBlur = isHovered ? 18 : 12;
            ctx.shadowColor = 'rgba(72, 230, 255, 0.42)';
        } else {
            ctx.shadowBlur = 0;
            ctx.shadowColor = 'transparent';
        }

        // Fill
        ctx.beginPath();
        ctx.arc(node.x, node.y, drawRadius, 0, Math.PI * 2);
        ctx.fillStyle = fillColor;
        ctx.fill();

        // State border (non-stable nodes)
        if (state !== NodeVisualState.STABLE) {
            ctx.lineWidth = isHovered ? 1.5 : 1;
            ctx.strokeStyle = state === NodeVisualState.NOT_STALE
                ? 'rgba(180, 248, 255, 0.65)'
                : state === NodeVisualState.BUILD_FAILED
                    ? 'rgba(255, 140, 140, 0.7)'
                    : 'rgba(140, 255, 190, 0.7)';
            ctx.stroke();
        }

        // Encoding-driven border
        if (encoding && strokeWidth > 0) {
            ctx.shadowBlur = 0;
            ctx.shadowColor = 'transparent';
            ctx.beginPath();
            ctx.arc(node.x, node.y, drawRadius + strokeWidth / 2, 0, Math.PI * 2);
            ctx.lineWidth = strokeWidth;
            ctx.strokeStyle = 'rgba(34, 211, 238, 0.6)';
            ctx.stroke();
        }

        // Cached indicator: dashed ring
        if (isCached) {
            ctx.shadowBlur = 0;
            ctx.shadowColor = 'transparent';
            ctx.save();
            ctx.setLineDash([2, 2]);
            ctx.beginPath();
            ctx.arc(node.x, node.y, drawRadius + 2, 0, Math.PI * 2);
            ctx.lineWidth = 0.5;
            ctx.strokeStyle = 'rgba(34, 211, 238, 0.4)';
            ctx.stroke();
            ctx.restore();
        }
    }

    private drawVizOverlay(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        const activeVizPrimitive = this.activeVizConfig?.type;
        if (!activeVizPrimitive || !this.buildVizState) return;

        if (activeVizPrimitive === 'node_fill') {
            return;
        }

        if (activeVizPrimitive === 'progress_only') {
            this.drawProgressOnlyOverlay(ctx, nodes);
        } else if (activeVizPrimitive === 'edge_draw') {
            this.drawEdgeDrawOverlay(ctx, nodes);
        } else if (activeVizPrimitive === 'verdict_mark') {
            this.drawVerdictMarkOverlay(ctx, nodes);
        } else if (activeVizPrimitive === 'cluster_form') {
            this.drawClusterFormOverlay(ctx, nodes);
        }
    }

    private drawProgressOnlyOverlay(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        if (nodes.length === 0) return;

        const pulse = 0.5 + 0.5 * Math.sin(this.pulsePhase);
        const alpha = 0.10 + 0.10 * pulse;
        const radiusPad = 3 + 2 * pulse;

        ctx.save();
        ctx.lineWidth = 1;
        ctx.strokeStyle = `rgba(0, 255, 255, ${alpha})`;
        ctx.shadowBlur = 6;
        ctx.shadowColor = `rgba(0, 255, 255, ${alpha})`;

        for (const node of nodes) {
            ctx.beginPath();
            ctx.arc(node.x, node.y, node.radius + radiusPad, 0, Math.PI * 2);
            ctx.stroke();
        }

        ctx.restore();
    }

    private drawEdgeDrawOverlay(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        const newEdges = this.buildVizState?.newEdges;
        if (!newEdges || newEdges.length === 0) return;

        const fadeAlpha = 0.4 + 0.6 * (0.5 + 0.5 * Math.sin(this.pulsePhase));

        for (const edge of newEdges) {
            const source = nodes.find((n) => n.id === edge.sourceId);
            const target = nodes.find((n) => n.id === edge.targetId);
            if (!source || !target) continue;

            ctx.save();
            ctx.globalAlpha = fadeAlpha;

            // Glow
            ctx.shadowBlur = 8;
            ctx.shadowColor = 'rgba(0, 255, 255, 0.5)';

            ctx.strokeStyle = 'rgba(0, 255, 255, 0.7)';
            ctx.lineWidth = 1.5;

            ctx.beginPath();
            ctx.moveTo(source.x, source.y);
            ctx.lineTo(target.x, target.y);
            ctx.stroke();

            ctx.restore();
        }

        // Subtle label
        ctx.save();
        ctx.font = '10px Inter, sans-serif';
        ctx.fillStyle = `rgba(0, 255, 255, ${0.2 + 0.15 * Math.sin(this.pulsePhase)})`;
        ctx.textAlign = 'right';
        ctx.fillText('creating connections...', this.w - 16, this.h - 16);
        ctx.restore();
    }

    private drawVerdictMarkOverlay(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        const verdicts = new Map(this.buildVizState?.verdictsBySource ?? []);
        for (const [nodeId, verdict] of this.buildVizState?.verdictsByNode ?? []) {
            verdicts.set(nodeId, verdict);
        }
        if (verdicts.size === 0) return;

        for (const [nodeId, verdict] of verdicts) {
            const node = nodes.find((n) => n.id === nodeId);
            if (!node) continue;

            const ringRadius = node.radius + 4;

            ctx.save();
            ctx.lineWidth = 2;

            if (verdict === 'KEEP') {
                ctx.strokeStyle = 'rgba(64, 208, 128, 0.8)';
            } else if (verdict === 'DISCONNECT') {
                ctx.strokeStyle = 'rgba(255, 165, 0, 0.8)';
            } else {
                // MISSING: yellow pulsing ring
                const pulseAlpha = 0.4 + 0.4 * Math.sin(this.pulsePhase);
                ctx.strokeStyle = `rgba(255, 220, 50, ${pulseAlpha})`;
            }

            ctx.beginPath();
            ctx.arc(node.x, node.y, ringRadius, 0, Math.PI * 2);
            ctx.stroke();
            ctx.restore();
        }
    }

    private drawClusterFormOverlay(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        const clusters = this.buildVizState?.clusterMembers;
        if (!clusters || clusters.size === 0) return;

        const clusterKeys = Array.from(clusters.keys());
        const hueStep = 360 / Math.max(clusterKeys.length, 1);

        for (let ci = 0; ci < clusterKeys.length; ci++) {
            const memberIds = clusters.get(clusterKeys[ci]);
            if (!memberIds || memberIds.length === 0) continue;

            const memberNodes: SurfaceNode[] = [];
            for (const mid of memberIds) {
                const n = nodes.find((nd) => nd.id === mid);
                if (n) memberNodes.push(n);
            }
            if (memberNodes.length === 0) continue;

            const hue = ci * hueStep;

            // Compute bounding box with padding
            let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
            for (const n of memberNodes) {
                minX = Math.min(minX, n.x - n.radius);
                minY = Math.min(minY, n.y - n.radius);
                maxX = Math.max(maxX, n.x + n.radius);
                maxY = Math.max(maxY, n.y + n.radius);
            }
            const pad = 8;
            minX -= pad; minY -= pad; maxX += pad; maxY += pad;

            // Tinted background
            ctx.save();
            ctx.fillStyle = `hsla(${hue}, 60%, 50%, 0.06)`;
            ctx.strokeStyle = `hsla(${hue}, 60%, 60%, 0.2)`;
            ctx.lineWidth = 1;

            const rx = 6; // corner radius
            ctx.beginPath();
            ctx.moveTo(minX + rx, minY);
            ctx.lineTo(maxX - rx, minY);
            ctx.arcTo(maxX, minY, maxX, minY + rx, rx);
            ctx.lineTo(maxX, maxY - rx);
            ctx.arcTo(maxX, maxY, maxX - rx, maxY, rx);
            ctx.lineTo(minX + rx, maxY);
            ctx.arcTo(minX, maxY, minX, maxY - rx, rx);
            ctx.lineTo(minX, minY + rx);
            ctx.arcTo(minX, minY, minX + rx, minY, rx);
            ctx.closePath();
            ctx.fill();
            ctx.stroke();

            // Shared glow on each member node
            for (const n of memberNodes) {
                ctx.beginPath();
                ctx.arc(n.x, n.y, n.radius + 2, 0, Math.PI * 2);
                ctx.fillStyle = `hsla(${hue}, 70%, 60%, 0.1)`;
                ctx.fill();
            }

            ctx.restore();
        }
    }

    private drawApexLabel(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        if (nodes.length === 0) return;

        const maxDepth = Math.max(...nodes.map((n) => n.depth));
        const apexNodes = nodes.filter((n) => n.depth === maxDepth);

        if (apexNodes.length !== 1) return;

        const apex = apexNodes[0];
        const title = nodeTitle(apex);
        const label = title.length > 34 ? title.slice(0, 34) + '...' : title;

        ctx.shadowBlur = 0;
        ctx.shadowColor = 'transparent';
        ctx.font = '11px Inter, sans-serif';
        ctx.fillStyle = 'rgba(255, 255, 255, 0.6)';
        ctx.textAlign = 'center';
        ctx.fillText(label, apex.x, apex.y + apex.radius + 16);
    }

    private drawDensityLabels(
        ctx: CanvasRenderingContext2D,
        nodes: SurfaceNode[],
    ): void {
        if (this.densityLabelThreshold <= 0) return;

        ctx.save();
        ctx.shadowBlur = 0;
        ctx.shadowColor = 'transparent';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';

        for (const node of nodes) {
            if (node.radius < this.densityLabelThreshold) continue;
            if (node.depth < 0) continue; // skip bedrock

            const title = nodeTitle(node);
            const label = title.length > 28 ? title.slice(0, 26) + '..' : title;

            // Scale font size with node radius
            const fontSize = Math.max(9, Math.min(14, node.radius * 0.6));
            ctx.font = `${fontSize}px Inter, sans-serif`;
            ctx.fillStyle = 'rgba(255, 255, 255, 0.7)';
            ctx.fillText(label, node.x, node.y + node.radius + 4);
        }

        ctx.restore();
    }
}
