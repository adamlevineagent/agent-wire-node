import type { SurfaceNode, SurfaceEdge, NodeEncoding, OverlayState, HitTestResult } from './types';

/**
 * Abstract renderer interface for the Pyramid Surface.
 * Implementations: CanvasRenderer (Standard), DomRenderer (Minimal), GpuRenderer (Rich).
 */
export interface PyramidRenderer {
    /** Attach to a container element. Called once on mount. */
    attach(container: HTMLElement): void;

    /** Detach and clean up resources. Called on unmount. */
    destroy(): void;

    /** Resize to match container dimensions. Called on window/container resize. */
    resize(width: number, height: number): void;

    /** Render a full frame with the given nodes and edges. */
    render(
        nodes: SurfaceNode[],
        edges: SurfaceEdge[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void;

    /** Apply three-axis visual encoding to a specific node. */
    setNodeEncoding(nodeId: string, encoding: NodeEncoding): void;

    /** Apply encodings in bulk (more efficient than per-node calls). */
    setNodeEncodings(encodings: Map<string, NodeEncoding>): void;

    /** Hit test: given viewport coordinates, return the node under the cursor (if any). */
    hitTest(x: number, y: number, nodes: SurfaceNode[]): HitTestResult | null;

    /** Get the current DPI-adjusted dimensions. */
    getDimensions(): { width: number; height: number };
}
