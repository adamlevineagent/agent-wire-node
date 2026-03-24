import { useMemo } from 'react';
import type { FlatNode, LayoutNode, LayoutEdge } from './types';
import { NodeState } from './types';

const PADDING = 30;
const BASE_RADIUS = 5;
const APEX_RADIUS = 22;
const STAGGER_THRESHOLD = 200; // L0 nodes above this count get 2-row stagger

function nodeRadius(depth: number, maxDepth: number): number {
  if (maxDepth === 0) return APEX_RADIUS;
  return BASE_RADIUS + (depth / maxDepth) * (APEX_RADIUS - BASE_RADIUS);
}

/**
 * Computes node positions using trapezoid band geometry and edge bezier curves.
 *
 * - Groups nodes by depth
 * - Each layer band narrows toward the top (pyramid shape)
 * - Nodes evenly spaced within their band
 * - Radius interpolated linearly from base to apex
 * - Returns LayoutNode[] and LayoutEdge[]
 */
export function usePyramidLayout(
  flatNodes: FlatNode[],
  width: number,
  height: number,
): { nodes: LayoutNode[]; edges: LayoutEdge[] } {
  return useMemo(() => {
    if (flatNodes.length === 0 || width === 0 || height === 0) {
      return { nodes: [], edges: [] };
    }

    // Group by depth
    const byDepth = new Map<number, FlatNode[]>();
    let maxDepth = 0;
    for (const node of flatNodes) {
      if (node.depth > maxDepth) maxDepth = node.depth;
      const list = byDepth.get(node.depth) ?? [];
      list.push(node);
      byDepth.set(node.depth, list);
    }

    // Guard: single node
    if (maxDepth === 0) {
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
      const yCenter = PADDING + usableHeight * (1 - depth / maxDepth);
      const bandHalfWidth = (usableWidth / 2) * (1 - (depth / maxDepth) * 0.85);
      const radius = nodeRadius(depth, maxDepth);
      const count = nodesAtDepth.length;

      // Stagger base layer if very dense
      const useStagger = depth === 0 && count > STAGGER_THRESHOLD;

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

    // Compute edges (child -> parent)
    const edges: LayoutEdge[] = [];
    for (const node of layoutNodes) {
      if (node.parentId) {
        const parent = nodeMap.get(node.parentId);
        if (parent) {
          // Control point: vertical midpoint, horizontal midpoint
          const controlX = (node.x + parent.x) / 2;
          const controlY = (node.y + parent.y) / 2;
          edges.push({
            fromId: node.id,
            toId: parent.id,
            fromX: node.x,
            fromY: node.y,
            toX: parent.x,
            toY: parent.y,
            controlX,
            controlY,
          });
        }
      }
    }

    return { nodes: layoutNodes, edges };
  }, [flatNodes, width, height]);
}
