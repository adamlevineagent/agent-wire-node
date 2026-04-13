import { useMemo } from 'react';
import type { SurfaceNode, SurfaceEdge, StaleLogEntry } from './types';
import { NodeVisualState, EdgeCategory } from './types';

// ── Constants ────────────────────────────────────────────────────────
const PADDING = 30;
const BASE_RADIUS = 5;
const APEX_RADIUS = 22;
const BEDROCK_RADIUS = 3;
const STAGGER_THRESHOLD = 200;
const ACTIVE_WINDOW_MS = 30 * 60 * 1000;
const JUST_UPDATED_WINDOW_MS = 60 * 1000;

// ── Flattening ───────────────────────────────────────────────────────

interface RawTreeNode {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    self_prompt?: string | null;
    thread_id?: string | null;
    source_path?: string | null;
    children: RawTreeNode[];
}

interface FlatInput {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    selfPrompt?: string | null;
    threadId?: string | null;
    sourcePath?: string | null;
    parentId: string | null;
    childIds: string[];
}

interface DagEdge {
    childId: string;
    parentId: string;
}

export function flattenTree(
    roots: RawTreeNode[],
    parentId: string | null = null,
    seen = new Set<string>(),
    edges: DagEdge[] = [],
): { nodes: FlatInput[]; edges: DagEdge[] } {
    const result: FlatInput[] = [];
    for (const node of roots) {
        if (parentId) edges.push({ childId: node.id, parentId });
        if (!seen.has(node.id)) {
            seen.add(node.id);
            result.push({
                id: node.id,
                depth: node.depth,
                headline: node.headline,
                distilled: node.distilled,
                selfPrompt: node.self_prompt,
                threadId: node.thread_id,
                sourcePath: node.source_path,
                parentId,
                childIds: node.children.map((c) => c.id),
            });
            const sub = flattenTree(node.children, node.id, seen, edges);
            result.push(...sub.nodes);
        }
    }
    return { nodes: result, edges };
}

/** Add synthetic bedrock nodes for source files referenced by L0 nodes. */
export function addBedrockLayer(nodes: FlatInput[]): FlatInput[] {
    const sourceToL0 = new Map<string, string[]>();
    for (const node of nodes) {
        if (node.depth === 0 && node.sourcePath) {
            const list = sourceToL0.get(node.sourcePath) ?? [];
            list.push(node.id);
            sourceToL0.set(node.sourcePath, list);
        }
    }
    const bedrockNodes: FlatInput[] = [];
    for (const [path, l0Ids] of sourceToL0) {
        bedrockNodes.push({
            id: `bedrock:${path}`,
            depth: -1,
            headline: path.split('/').pop() ?? path,
            distilled: path,
            parentId: null,
            childIds: l0Ids,
        });
    }
    return [...nodes, ...bedrockNodes];
}

// ── Layout ───────────────────────────────────────────────────────────

function nodeRadius(depth: number, maxDepth: number): number {
    if (depth < 0) return BEDROCK_RADIUS;
    if (maxDepth === 0) return APEX_RADIUS;
    return BASE_RADIUS + (depth / maxDepth) * (APEX_RADIUS - BASE_RADIUS);
}

export function computeLayout(
    flatNodes: FlatInput[],
    dagEdges: DagEdge[],
    width: number,
    height: number,
): { nodes: SurfaceNode[]; edges: SurfaceEdge[] } {
    if (flatNodes.length === 0 || width === 0 || height === 0) {
        return { nodes: [], edges: [] };
    }

    const depths = flatNodes.map((n) => n.depth);
    const minDepth = Math.min(...depths);
    const maxDepth = Math.max(...depths);
    const depthRange = Math.max(maxDepth - minDepth, 1);

    const usableWidth = width - PADDING * 2;
    const usableHeight = height - PADDING * 2;
    const xCenter = width / 2;

    // Group by depth
    const byDepth = new Map<number, FlatInput[]>();
    for (const node of flatNodes) {
        const list = byDepth.get(node.depth) ?? [];
        list.push(node);
        byDepth.set(node.depth, list);
    }

    // Position nodes
    const positionMap = new Map<string, SurfaceNode>();
    for (const [depth, group] of byDepth) {
        const normalizedDepth = (depth - minDepth) / depthRange;
        const yCenter = PADDING + usableHeight * (1 - normalizedDepth);
        const radius = nodeRadius(depth, maxDepth);

        const narrowFactor = depth < 0 ? 0 : maxDepth > 0 ? (depth / maxDepth) * 0.85 : 0;
        const bandHalfWidth = (usableWidth / 2) * (1 - narrowFactor);
        const count = group.length;
        const useStagger = (depth === 0 || depth === -1) && count > STAGGER_THRESHOLD;

        for (let i = 0; i < count; i++) {
            const t = count > 1 ? i / (count - 1) : 0.5;
            const x = xCenter - bandHalfWidth + t * bandHalfWidth * 2;
            let y = yCenter;
            if (useStagger) {
                y += i % 2 === 0 ? -(radius + 1) : radius + 1;
            }

            const node = group[i];
            positionMap.set(node.id, {
                id: node.id,
                depth: node.depth,
                headline: node.headline,
                distilled: node.distilled,
                selfPrompt: node.selfPrompt,
                threadId: node.threadId,
                sourcePath: node.sourcePath,
                parentId: node.parentId,
                childIds: node.childIds,
                x,
                y,
                radius,
                state: NodeVisualState.STABLE,
            });
        }
    }

    // Build edges from DAG edge list
    const edges: SurfaceEdge[] = [];
    for (const { childId, parentId } of dagEdges) {
        const child = positionMap.get(childId);
        const parent = positionMap.get(parentId);
        if (child && parent) {
            edges.push({
                fromId: childId,
                toId: parentId,
                fromX: child.x,
                fromY: child.y,
                toX: parent.x,
                toY: parent.y,
                controlX: (child.x + parent.x) / 2,
                controlY: (child.y + parent.y) / 2,
                category: parent.depth === -1 || child.depth === -1
                    ? EdgeCategory.BEDROCK
                    : EdgeCategory.STRUCTURAL,
            });
        }
    }

    // Add bedrock edges (bedrock node → its L0 children)
    for (const node of positionMap.values()) {
        if (node.depth === -1) {
            for (const childId of node.childIds) {
                const child = positionMap.get(childId);
                if (child) {
                    edges.push({
                        fromId: child.id,
                        toId: node.id,
                        fromX: child.x,
                        fromY: child.y,
                        toX: node.x,
                        toY: node.y,
                        controlX: (child.x + node.x) / 2,
                        controlY: (child.y + node.y) / 2,
                        category: EdgeCategory.BEDROCK,
                    });
                }
            }
        }
    }

    return { nodes: Array.from(positionMap.values()), edges };
}

// ── Stale state derivation ───────────────────────────────────────────

export function deriveNodeStates(
    nodes: SurfaceNode[],
    staleLog: StaleLogEntry[],
): SurfaceNode[] {
    if (staleLog.length === 0) return nodes;

    const logByTarget = new Map<string, StaleLogEntry[]>();
    for (const entry of staleLog) {
        const list = logByTarget.get(entry.target_id) ?? [];
        list.push(entry);
        logByTarget.set(entry.target_id, list);
    }

    return nodes.map((node) => {
        const targets = [node.id, node.threadId, node.sourcePath].filter(Boolean) as string[];
        let latest: StaleLogEntry | undefined;
        for (const t of targets) {
            const entries = logByTarget.get(t);
            if (entries) {
                for (const e of entries) {
                    if (!latest || e.checked_at > latest.checked_at) {
                        latest = e;
                    }
                }
            }
        }
        if (!latest) return node;

        const age = Date.now() - new Date(latest.checked_at).getTime();
        if (age > ACTIVE_WINDOW_MS) return node; // stable

        // Backend returns stale as a string: "yes", "no", "new", "deleted", "renamed", "skipped"
        const isStale = latest.stale === 'yes' || latest.stale === 'Yes'
            || latest.stale === '1' || latest.stale === 'true';

        let state: NodeVisualState;
        if (isStale) {
            state = age < JUST_UPDATED_WINDOW_MS ? NodeVisualState.JUST_UPDATED : NodeVisualState.STALE_CONFIRMED;
        } else {
            state = NodeVisualState.NOT_STALE;
        }

        return { ...node, state };
    });
}

// ── Hook ─────────────────────────────────────────────────────────────

export function useUnifiedLayout(
    treeRoots: RawTreeNode[] | null,
    width: number,
    height: number,
    staleLog: StaleLogEntry[],
) {
    return useMemo(() => {
        if (!treeRoots || treeRoots.length === 0) {
            return { nodes: [] as SurfaceNode[], edges: [] as SurfaceEdge[] };
        }

        const { nodes: flat, edges: dagEdges } = flattenTree(treeRoots);
        const withBedrock = addBedrockLayer(flat);
        const { nodes, edges } = computeLayout(withBedrock, dagEdges, width, height);
        const statedNodes = deriveNodeStates(nodes, staleLog);

        return { nodes: statedNodes, edges };
    }, [treeRoots, width, height, staleLog]);
}
