/** Shared types for the Pyramid Surface visualization system. */

export interface SurfaceNode {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    selfPrompt?: string | null;
    threadId?: string | null;
    sourcePath?: string | null;
    parentId: string | null;
    childIds: string[];
    /** Layout-computed position */
    x: number;
    y: number;
    radius: number;
    /** Visual state for stale detection overlay */
    state: NodeVisualState;
}

export enum NodeVisualState {
    STABLE = 'stable',
    STALE_CONFIRMED = 'stale_confirmed',
    JUST_UPDATED = 'just_updated',
    NOT_STALE = 'not_stale',
    /** Build-time: node is being processed */
    BUILDING = 'building',
    /** Build-time: node completed successfully */
    BUILD_COMPLETE = 'build_complete',
    /** Build-time: node failed */
    BUILD_FAILED = 'build_failed',
    /** Build-time: node was served from cache */
    CACHED = 'cached',
}

export interface SurfaceEdge {
    fromId: string;
    toId: string;
    fromX: number;
    fromY: number;
    toX: number;
    toY: number;
    controlX: number;
    controlY: number;
    /** Edge category for overlay filtering */
    category: EdgeCategory;
}

export enum EdgeCategory {
    STRUCTURAL = 'structural',    // parent→child
    WEB = 'web',                  // same-layer web edges
    EVIDENCE = 'evidence',        // KEEP evidence links
    BEDROCK = 'bedrock',          // L0→source file
}

/** Three-axis visual encoding from pyramid-surface-visual-encoding.md */
export interface NodeEncoding {
    /** Axis 1: direct citation intensity (0–1) */
    brightness: number;
    /** Axis 2: propagated importance from upstream (0–1) */
    saturation: number;
    /** Axis 3: lateral connectivity / web edge count (0–1) */
    borderThickness: number;
}

/** Deployment mode for the PyramidSurface component */
export type SurfaceMode = 'full' | 'nested' | 'ticker';

/** Layout mode toggle */
export type LayoutMode = 'pyramid' | 'density';

/** Which overlays are currently active */
export interface OverlayState {
    structure: boolean;
    web: boolean;
    staleness: boolean;
    provenance: boolean;
    build: boolean;
    weightIntensity: boolean;
}

/** Hit test result from renderer */
export interface HitTestResult {
    nodeId: string;
    node: SurfaceNode;
}

/** Stale log entry from DADBEAR. The `stale` field is a string from the backend:
 *  "yes" | "no" | "new" | "deleted" | "renamed" | "skipped" */
export interface StaleLogEntry {
    target_id: string;
    stale: string;
    checked_at: string;
    reason?: string;
}
