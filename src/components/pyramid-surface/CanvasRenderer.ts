import type { PyramidRenderer } from './PyramidRenderer';
import type { SurfaceNode, SurfaceEdge, NodeEncoding, OverlayState, HitTestResult } from './types';
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

function nodeTitle(node: Pick<SurfaceNode, 'headline' | 'id' | 'selfPrompt'>): string {
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

        // 5. Apex label
        this.drawApexLabel(ctx, nodes);
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
            ctx.strokeStyle = style.color;
            ctx.lineWidth = style.lineWidth;

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

        // Encoding modulation (Phase 2b: brightness as alpha, borderThickness as stroke width)
        const encoding = overlays.weightIntensity
            ? this.nodeEncodings.get(node.id)
            : undefined;
        let strokeWidth = 0;

        if (encoding) {
            // Brightness modulates fill alpha
            fillColor = withAlpha(fillColor, 0.3 + encoding.brightness * 0.7);
            // borderThickness maps to stroke width (0-1 => 0-3px)
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
}
