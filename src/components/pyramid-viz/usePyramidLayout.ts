import { useMemo } from 'react';
import type { FlatNode, LayoutNode, LayoutEdge } from './types';
import { NodeState } from './types';

const PADDING = 30;
const BASE_RADIUS = 5;
const APEX_RADIUS = 22;
const STAGGER_THRESHOLD = 200; // L0 nodes above this count get 2-row stagger

function nodeRadius(depth: number, maxDepth: number): number {
  if (depth < 0) return 3; // BEDROCK: tiny dots for source files
  if (maxDepth === 0) return APEX_RADIUS;
  return BASE_RADIUS + (depth / maxDepth) * (APEX_RADIUS - BASE_RADIUS);
}

/**
 * Computes node positions using trapezoid band geometry and edge bezier curves.
 *
 * Supports DAG structure: accepts an optional explicit edge list so nodes
 * with multiple parents (e.g., an L0 cited by two L1s) render correctly
 * as a single node with multiple edges, not duplicated nodes.
 */
export function usePyramidLayout(
  flatNodes: FlatNode[],
  width: number,
  height: number,
  dagEdges?: Array<{ childId: string; parentId: string }>,
): { nodes: LayoutNode[]; edges: LayoutEdge[] } {
  return useMemo(() => {
    if (flatNodes.length === 0 || width === 0 || height === 0) {
      return { nodes: [], edges: [] };
    }

    // Group by depth
    const byDepth = new Map<number, FlatNode[]>();
    let maxDepth = 0;
    let minDepth = 0;
    for (const node of flatNodes) {
      if (node.depth > maxDepth) maxDepth = node.depth;
      if (node.depth < minDepth) minDepth = node.depth;
      const list = byDepth.get(node.depth) ?? [];
      list.push(node);
      byDepth.set(node.depth, list);
    }

    const depthRange = maxDepth - minDepth;

    // Guard: single node (no depth range)
    if (depthRange === 0) {
      const node = flatNodes[0];
      const layoutNode: LayoutNode = {
        ...node,
        x: width / 2,
        y: height / 2,
        radius: APEX_RADIUS,
        state: NodeState.STABLE,
      };
      return { nodes: [layoutNode], edges: [] };
    }

    const usableWidth = width - PADDING * 2;
    const usableHeight = height - PADDING * 2;
    const xCenter = width / 2;

    const layoutNodes: LayoutNode[] = [];

    for (const [depth, nodesAtDepth] of byDepth) {
      const normalizedDepth = (depth - minDepth) / depthRange;
      const yCenter = PADDING + usableHeight * (1 - normalizedDepth);
      // BEDROCK (depth -1) and L0 get full width; higher layers narrow toward apex
      const narrowFactor = depth < 0 ? 0 : (depth / maxDepth) * 0.85;
      const bandHalfWidth = (usableWidth / 2) * (1 - narrowFactor);
      const radius = nodeRadius(depth, maxDepth);
      const count = nodesAtDepth.length;

      // Stagger base layer and BEDROCK if very dense
      const useStagger = (depth === 0 || depth === -1) && count > STAGGER_THRESHOLD;

      for (let i = 0; i < count; i++) {
        let x: number;
        let y: number;

        if (count === 1) {
          x = xCenter;
          y = yCenter;
        } else {
          const t = count > 1 ? i / (count - 1) : 0.5;
          x = xCenter - bandHalfWidth + t * bandHalfWidth * 2;
          y = yCenter;

          if (useStagger) {
            // Alternate rows offset by radius + 1px
            y += (i % 2 === 0 ? -(radius + 1) : radius + 1);
          }
        }

        layoutNodes.push({
          ...nodesAtDepth[i],
          x,
          y,
          radius,
          state: NodeState.STABLE,
        });
      }
    }

    // Build lookup for edge computation
    const nodeMap = new Map<string, LayoutNode>();
    for (const n of layoutNodes) {
      nodeMap.set(n.id, n);
    }

    const edges: LayoutEdge[] = [];

    // Use explicit DAG edges if provided (multi-parent support),
    // otherwise fall back to parentId (single-parent tree)
    if (dagEdges && dagEdges.length > 0) {
      for (const { childId, parentId } of dagEdges) {
        const child = nodeMap.get(childId);
        const parent = nodeMap.get(parentId);
        if (child && parent) {
          edges.push({
            fromId: child.id,
            toId: parent.id,
            fromX: child.x,
            fromY: child.y,
            toX: parent.x,
            toY: parent.y,
            controlX: (child.x + parent.x) / 2,
            controlY: (child.y + parent.y) / 2,
          });
        }
      }
    } else {
      // Fallback: single-parent edges from parentId
      for (const node of layoutNodes) {
        if (node.parentId) {
          const parent = nodeMap.get(node.parentId);
          if (parent) {
            edges.push({
              fromId: node.id,
              toId: parent.id,
              fromX: node.x,
              fromY: node.y,
              toX: parent.x,
              toY: parent.y,
              controlX: (node.x + parent.x) / 2,
              controlY: (node.y + parent.y) / 2,
            });
          }
        }
      }
    }

    // BEDROCK edges: connect L0 nodes down to their source file BEDROCK nodes
    for (const node of layoutNodes) {
      if (node.depth === -1 && node.childIds) {
        for (const childId of node.childIds) {
          const child = nodeMap.get(childId);
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
            });
          }
        }
      }
    }

    return { nodes: layoutNodes, edges };
  }, [flatNodes, width, height, dagEdges]);
}
