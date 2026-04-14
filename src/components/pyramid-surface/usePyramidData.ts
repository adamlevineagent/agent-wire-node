/**
 * Unified data hook for PyramidSurface.
 *
 * In static mode: loads tree via pyramid_tree IPC, applies stale states.
 * In build mode: polls BuildProgressV2 for layer progress, subscribes
 * to cross-build-event for per-step details, and maps build events to
 * SurfaceNode visual states (BUILDING, BUILD_COMPLETE, BUILD_FAILED, CACHED).
 */

import { useState, useEffect, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { SurfaceNode, SurfaceEdge, StaleLogEntry, BuildVizState } from './types';
import { NodeVisualState } from './types';
import { flattenTree, addBedrockLayer, computeLayout, deriveNodeStates } from './useUnifiedLayout';

// ── Types from existing IPC contracts ────────────────────────────────

interface TreeResponse {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    self_prompt?: string | null;
    thread_id?: string | null;
    source_path?: string | null;
    children: TreeResponse[];
}

interface BuildProgressV2 {
    done: number;
    total: number;
    layers: LayerProgress[];
    current_step: string | null;
    log: LogEntry[];
}

interface LayerProgress {
    depth: number;
    step_name: string;
    estimated_nodes: number;
    completed_nodes: number;
    failed_nodes: number;
    status: string; // "pending" | "active" | "complete"
    nodes: NodeStatus[] | null;
}

interface NodeStatus {
    node_id: string;
    status: string; // "complete" | "failed"
    label: string | null;
}

interface LogEntry {
    elapsed_secs: number;
    message: string;
}

interface BuildStatus {
    slug: string;
    status: string;
    progress: { done: number; total: number };
    elapsed_seconds: number;
    failures: number;
}

interface TaggedBuildEvent {
    slug: string;
    kind: Record<string, unknown> & { type: string };
}

interface LiveNodeInfo {
    node_id: string;
    depth: number;
    headline: string;
    parent_id: string | null;
    children: string[];
    status: string;
}

// ── Hook ─────────────────────────────────────────────────────────────

export interface PyramidDataResult {
    nodes: SurfaceNode[];
    edges: SurfaceEdge[];
    /** Whether a build is actively running */
    isBuilding: boolean;
    /** Current step label (e.g., "source_extract", "l0_webbing") */
    currentStep: string | null;
    /** Build progress counters */
    buildProgress: { done: number; total: number } | null;
    /** Activity log entries from the build */
    buildLog: LogEntry[];
    /** Build-time viz state (verdicts, clusters, edges) for primitive-specific rendering */
    buildVizState: BuildVizState;
    /** Loading state (initial tree fetch) */
    loading: boolean;
}

export function usePyramidData(
    slug: string,
    width: number,
    height: number,
    staleLog: StaleLogEntry[],
    expectedMaxDepth?: number | null,
): PyramidDataResult {
    const [treeData, setTreeData] = useState<TreeResponse[] | null>(null);
    const [liveNodes, setLiveNodes] = useState<LiveNodeInfo[]>([]);
    const [loading, setLoading] = useState(true);
    const [buildStatus, setBuildStatus] = useState<BuildStatus | null>(null);
    const [buildProgress, setBuildProgress] = useState<BuildProgressV2 | null>(null);
    const [buildNodeStates, setBuildNodeStates] = useState<Map<string, NodeVisualState>>(new Map());
    const [buildVizState, setBuildVizState] = useState<BuildVizState>({
        verdictsByNode: new Map(),
        clusterMembers: new Map(),
        newEdges: [],
    });

    const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
    const isBuilding = buildStatus?.status === 'running';

    // ── Reset all state on slug change ────────────────────────────────
    useEffect(() => {
        setLoading(true);
        setTreeData(null);
        setBuildStatus(null);
        setBuildProgress(null);
        setBuildNodeStates(new Map());
        setBuildVizState({ verdictsByNode: new Map(), clusterMembers: new Map(), newEdges: [] });
        invoke<TreeResponse[]>('pyramid_tree', { slug })
            .then(setTreeData)
            .catch(() => setTreeData(null))
            .finally(() => setLoading(false));
    }, [slug]);

    // ── Poll build status ───────────────────────────────────────────
    useEffect(() => {
        const poll = async () => {
            try {
                const status = await invoke<BuildStatus>('pyramid_build_status', { slug });
                setBuildStatus(status);

                if (status.status === 'running') {
                    const progress = await invoke<BuildProgressV2>(
                        'pyramid_build_progress_v2',
                        { slug },
                    );
                    setBuildProgress(progress);

                    // Map layer node statuses to visual states
                    const nodeStates = new Map<string, NodeVisualState>();
                    for (const layer of progress.layers) {
                        if (layer.nodes) {
                            for (const ns of layer.nodes) {
                                nodeStates.set(
                                    ns.node_id,
                                    ns.status === 'failed'
                                        ? NodeVisualState.BUILD_FAILED
                                        : NodeVisualState.BUILD_COMPLETE,
                                );
                            }
                        }
                    }
                    setBuildNodeStates(nodeStates);

                    // Fetch live nodes for rendering during build
                    const live = await invoke<LiveNodeInfo[]>(
                        'pyramid_build_live_nodes',
                        { slug },
                    ).catch(() => [] as LiveNodeInfo[]);
                    setLiveNodes(live);

                    // Also refresh tree (nodes commit to DB as build progresses)
                    const tree = await invoke<TreeResponse[]>('pyramid_tree', { slug }).catch(() => null);
                    if (tree && tree.length > 0) setTreeData(tree);
                }
            } catch {
                // Not building or IPC error — ignore
            }
        };

        poll(); // Immediate first poll
        const interval = isBuilding ? 2000 : 10000; // Fast poll during build, slow otherwise
        pollRef.current = setInterval(poll, interval);

        return () => {
            if (pollRef.current) clearInterval(pollRef.current);
        };
    }, [slug, isBuilding]);

    // ── Subscribe to build events for real-time node state updates ──
    // NOTE: The Rust CacheHit variant carries cache_key, not node_id,
    // so we cannot yet map cache hits to specific surface nodes. When
    // the backend adds node_id to CacheHit, add a handler here that
    // sets NodeVisualState.CACHED for the node.
    useEffect(() => {
        const unlisten = listen<TaggedBuildEvent>('cross-build-event', (ev) => {
            if (ev.payload.slug !== slug) return;
            if (ev.payload.slug === '__ollama__') return;

            const kind = ev.payload.kind;

            // Delta-landed events carry node_id — mark the node as
            // completed in real time (faster feedback than the next
            // poll cycle).
            if (kind.type === 'delta_landed' && typeof kind.node_id === 'string') {
                setBuildNodeStates((prev) => {
                    const next = new Map(prev);
                    next.set(kind.node_id as string, NodeVisualState.BUILD_COMPLETE);
                    return next;
                });
            }

            // Fix D: NodeProduced events mark the node as build-complete
            // in real time, before the next poll cycle refreshes the tree.
            if (kind.type === 'node_produced' && typeof kind.node_id === 'string') {
                setBuildNodeStates((prev) => {
                    const next = new Map(prev);
                    next.set(kind.node_id as string, NodeVisualState.BUILD_COMPLETE);
                    return next;
                });
            }

            // ── S2-1: Accumulate viz-primitive-specific state ──
            if (kind.type === 'verdict_produced' && typeof kind.node_id === 'string') {
                const verdict = kind.verdict as string;
                if (verdict === 'KEEP' || verdict === 'DISCONNECT' || verdict === 'MISSING') {
                    setBuildVizState((prev) => {
                        const next = { ...prev, verdictsByNode: new Map(prev.verdictsByNode) };
                        next.verdictsByNode.set(kind.node_id as string, verdict);
                        return next;
                    });
                }
            }

            if (kind.type === 'cluster_assignment') {
                const nodeCount = kind.node_count as number;
                const clusterCount = kind.cluster_count as number;
                const stepName = kind.step_name as string;
                if (nodeCount > 0 && clusterCount > 0) {
                    // ClusterAssignment gives aggregate counts, not per-node membership.
                    // Store as a synthetic entry — the renderer can use this for step-level indication.
                    setBuildVizState((prev) => {
                        const next = { ...prev, clusterMembers: new Map(prev.clusterMembers) };
                        next.clusterMembers.set(stepName, [`${clusterCount} clusters from ${nodeCount} nodes`]);
                        return next;
                    });
                }
            }

            if (kind.type === 'edge_created' && typeof kind.source_id === 'string' && typeof kind.target_id === 'string') {
                setBuildVizState((prev) => ({
                    ...prev,
                    newEdges: [...prev.newEdges.slice(-200), { sourceId: kind.source_id as string, targetId: kind.target_id as string }],
                }));
            }

            // Clear viz state when a new step starts (fresh state per step)
            if (kind.type === 'chain_step_started') {
                setBuildVizState({
                    verdictsByNode: new Map(),
                    clusterMembers: new Map(),
                    newEdges: [],
                });
            }
        });

        return () => { unlisten.then((fn) => fn()); };
    }, [slug]);

    // ── Compute layout ──────────────────────────────────────────────
    const computeResult = useMemo(() => {
        if (width === 0 || height === 0) {
            return { nodes: [] as SurfaceNode[], edges: [] as SurfaceEdge[] };
        }

        // Use tree data if available, otherwise build from live nodes during build
        let flat: ReturnType<typeof flattenTree>['nodes'] = [];
        let dagEdges: ReturnType<typeof flattenTree>['edges'] = [];

        if (treeData && treeData.length > 0) {
            const result = flattenTree(treeData);
            flat = result.nodes;
            dagEdges = result.edges;
        } else if (isBuilding && liveNodes.length > 0) {
            // Convert live nodes to flat layout nodes during build
            flat = liveNodes.map((ln) => ({
                id: ln.node_id,
                depth: ln.depth,
                headline: ln.headline,
                distilled: '',
                parentId: ln.parent_id,
                childIds: ln.children,
            }));
            // Build edges from parent relationships
            for (const ln of liveNodes) {
                if (ln.parent_id) {
                    dagEdges.push({ childId: ln.node_id, parentId: ln.parent_id });
                }
            }
        }

        if (flat.length === 0) {
            return { nodes: [] as SurfaceNode[], edges: [] as SurfaceEdge[] };
        }

        const withBedrock = addBedrockLayer(flat);
        let { nodes, edges } = computeLayout(
            withBedrock,
            dagEdges,
            width,
            height,
            expectedMaxDepth ?? undefined,
        );

        // Apply stale states (static mode)
        if (!isBuilding) {
            nodes = deriveNodeStates(nodes, staleLog);
        }

        // Apply build states (build mode) — overlay on top of layout
        if (isBuilding && buildNodeStates.size > 0) {
            nodes = nodes.map((node) => {
                const buildState = buildNodeStates.get(node.id);
                return buildState ? { ...node, state: buildState } : node;
            });
        }

        return { nodes, edges };
    }, [treeData, liveNodes, width, height, isBuilding, buildNodeStates, staleLog, expectedMaxDepth]);

    return {
        nodes: computeResult.nodes,
        edges: computeResult.edges,
        isBuilding: isBuilding ?? false,
        currentStep: buildProgress?.current_step ?? null,
        buildProgress: buildProgress
            ? { done: buildProgress.done, total: buildProgress.total }
            : null,
        buildLog: buildProgress?.log ?? [],
        buildVizState,
        loading,
    };
}
