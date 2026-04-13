import type { PyramidRenderer } from './PyramidRenderer';
import type { SurfaceNode, SurfaceEdge, NodeEncoding, OverlayState, HitTestResult, VizPrimitive, BuildVizState } from './types';
import { NodeVisualState, EdgeCategory } from './types';

/**
 * Minimal DOM-based renderer for accessibility and low-end hardware.
 * Renders nodes as positioned divs — no canvas, no GPU.
 * Used when viz config tier = "minimal".
 */
export class DomRenderer implements PyramidRenderer {
    private container: HTMLElement | null = null;
    private wrapper: HTMLDivElement | null = null;
    private nodeElements = new Map<string, HTMLDivElement>();
    private encodings = new Map<string, NodeEncoding>();
    private width = 0;
    private height = 0;

    attach(container: HTMLElement): void {
        this.container = container;
        this.wrapper = document.createElement('div');
        this.wrapper.className = 'ps-dom-wrapper';
        this.wrapper.style.cssText = 'position:relative;width:100%;height:100%;overflow:hidden;';
        container.appendChild(this.wrapper);
    }

    destroy(): void {
        if (this.wrapper && this.container) {
            this.container.removeChild(this.wrapper);
        }
        this.wrapper = null;
        this.container = null;
        this.nodeElements.clear();
        this.encodings.clear();
    }

    resize(width: number, height: number): void {
        this.width = width;
        this.height = height;
        if (this.wrapper) {
            this.wrapper.style.width = `${width}px`;
            this.wrapper.style.height = `${height}px`;
        }
    }

    render(
        nodes: SurfaceNode[],
        _edges: SurfaceEdge[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        if (!this.wrapper) return;

        // Remove stale elements
        const currentIds = new Set(nodes.map((n) => n.id));
        for (const [id, el] of this.nodeElements) {
            if (!currentIds.has(id)) {
                el.remove();
                this.nodeElements.delete(id);
            }
        }

        // Update or create elements
        for (const node of nodes) {
            let el = this.nodeElements.get(node.id);
            if (!el) {
                el = document.createElement('div');
                el.className = 'ps-dom-node';
                el.dataset.nodeId = node.id;
                this.wrapper.appendChild(el);
                this.nodeElements.set(node.id, el);
            }

            const size = node.radius * 2;
            const isHovered = node.id === hoveredNodeId;
            const scale = isHovered ? 1.4 : 1;
            const encoding = this.encodings.get(node.id);

            const saturateFilter = this.nodeSaturateFilter(encoding, overlays);
            el.style.cssText = `
                position:absolute;
                left:${node.x - node.radius}px;
                top:${node.y - node.radius}px;
                width:${size}px;
                height:${size}px;
                border-radius:50%;
                box-sizing:border-box;
                transform:scale(${scale});
                transition:transform 0.1s ease;
                cursor:pointer;
                background:${this.nodeColor(node, overlays, encoding)};
                border:${this.nodeBorder(encoding, overlays)};
                filter:${saturateFilter};
                opacity:${node.depth < 0 ? 0.3 : 1};
            `;

            el.title = `${node.headline}\nL${node.depth}`;
        }

        // No edge rendering in DOM mode — edges are structural only in canvas/GPU
    }

    private nodeColor(node: SurfaceNode, overlays: OverlayState, encoding?: NodeEncoding): string {
        if (!overlays.staleness) {
            const alpha = encoding && overlays.weightIntensity
                ? 0.3 + encoding.brightness * 0.7
                : 0.5;
            return `rgba(34, 211, 238, ${alpha})`;
        }

        switch (node.state) {
            case NodeVisualState.STALE_CONFIRMED:
            case NodeVisualState.JUST_UPDATED:
                return 'rgba(64, 208, 128, 0.9)';
            case NodeVisualState.NOT_STALE:
                return 'rgba(72, 230, 255, 0.82)';
            case NodeVisualState.BUILD_COMPLETE:
                return 'rgba(34, 211, 238, 0.7)';
            case NodeVisualState.BUILD_FAILED:
                return 'rgba(255, 100, 100, 0.7)';
            case NodeVisualState.BUILDING:
                return 'rgba(34, 211, 238, 0.3)';
            case NodeVisualState.CACHED:
                return 'rgba(34, 211, 238, 0.5)';
            default:
                return 'rgba(34, 211, 238, 0.4)';
        }
    }

    /** Axis 2 — Saturation via CSS filter. Returns saturate() value string.
     *  0.2 base keeps desaturated nodes visible; 0.8 range for full encoding. */
    private nodeSaturateFilter(encoding: NodeEncoding | undefined, overlays: OverlayState): string {
        if (!encoding || !overlays.weightIntensity) return 'none';
        // saturate(0) = grayscale, saturate(1) = original, saturate(>1) = boosted
        // Map encoding.saturation [0,1] to filter range [0.2, 1.0]
        const filterValue = 0.2 + encoding.saturation * 0.8;
        return `saturate(${filterValue.toFixed(2)})`;
    }

    private nodeBorder(encoding: NodeEncoding | undefined, overlays: OverlayState): string {
        if (!encoding || !overlays.weightIntensity) return 'none';
        const thickness = Math.round(encoding.borderThickness * 3);
        if (thickness <= 0) return 'none';
        return `${thickness}px solid rgba(34, 211, 238, 0.4)`;
    }

    setNodeEncoding(nodeId: string, encoding: NodeEncoding): void {
        this.encodings.set(nodeId, encoding);
    }

    setNodeEncodings(encodings: Map<string, NodeEncoding>): void {
        this.encodings = new Map(encodings);
    }

    setActiveVizPrimitive(_primitive: VizPrimitive | null): void {
        // No-op: DOM mode does not render viz overlays
    }

    setBuildVizState(_state: BuildVizState): void {
        // No-op: DOM mode does not render viz overlays
    }

    setLinkIntensities(_intensities: Map<string, number>): void {
        // No-op: DOM mode does not render link intensities
    }

    hitTest(x: number, y: number, nodes: SurfaceNode[]): HitTestResult | null {
        // DOM hit testing: check which element is at the point
        if (!this.wrapper) return null;
        const rect = this.wrapper.getBoundingClientRect();
        const mx = x - rect.left;
        const my = y - rect.top;

        // Reverse depth order (apex first)
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

    getDimensions(): { width: number; height: number } {
        return { width: this.width, height: this.height };
    }
}
