/** Shared types for the Pyramid Surface visualization system. */

export interface SurfaceNode {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    /** Generic object kind from the read model. Question nodes are first-class surface nodes. */
    nodeKind?: 'knowledge' | 'question' | 'source' | string | null;
    question?: string | null;
    questionAbout?: string | null;
    questionCreates?: string | null;
    questionPromptHint?: string | null;
    answerNodeId?: string | null;
    answerHeadline?: string | null;
    answerDistilled?: string | null;
    answered?: boolean | null;
    selfPrompt?: string | null;
    threadId?: string | null;
    sourcePath?: string | null;
    parentId: string | null;
    parentIds: string[];
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

/** Viz primitive types from AD-1: chain YAML drives visualization */
export type VizPrimitive =
    | 'node_fill'       // Dots appearing in a layer band
    | 'edge_draw'       // Lines forming between existing nodes
    | 'cluster_form'    // Nodes visually grouping, parent appearing
    | 'verdict_mark'    // KEEP/DISCONNECT/MISSING indicators on nodes
    | 'progress_only';  // Non-structural activity pulse

/** Full YAML step viz metadata. `type` selects the primitive; the rest is runtime data. */
export interface VizStepConfig {
    type: VizPrimitive;
    source?: string;
    node_kind?: string;
    nodeKind?: string;
    [key: string]: unknown;
}

export type EvidenceVerdict = 'KEEP' | 'DISCONNECT' | 'MISSING';

/** Build-time viz state accumulated from events during a build */
export interface BuildVizState {
    /** Per-target-node verdict from VerdictProduced events */
    verdictsByNode: Map<string, EvidenceVerdict>;
    /** Per-source-node verdict from VerdictProduced events, used while targets are provisional */
    verdictsBySource: Map<string, EvidenceVerdict>;
    /** Cluster membership from ClusterAssignment events: cluster_id → node_ids */
    clusterMembers: Map<string, string[]>;
    /** New edges from EdgeCreated events (source_id, target_id) */
    newEdges: Array<{ sourceId: string; targetId: string }>;
}
