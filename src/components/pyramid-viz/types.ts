/**
 * Pyramid Visualization Types
 *
 * Phase 1: Static render, hover tooltip, click popover, stale log coloring.
 */

/** Raw tree node as returned by the pyramid_tree Tauri command (recursively nested). */
export interface TreeNode {
  id: string;
  depth: number;
  headline: string;
  distilled: string;
  threadId?: string | null;
  sourcePath?: string | null;
  children: TreeNode[];
}

/** Flattened node for internal layout use. parentId is derived during flattening. */
export interface FlatNode {
  id: string;
  depth: number;
  headline: string;
  distilled: string;
  threadId?: string | null;
  sourcePath?: string | null;
  parentId: string | null;
  childIds: string[];
}

/** Node with computed layout position and visual state. */
export interface LayoutNode extends FlatNode {
  x: number;
  y: number;
  radius: number;
  state: NodeState;
}

export enum NodeState {
  STABLE = 'stable',
  STALE_CONFIRMED = 'stale_confirmed',
  JUST_UPDATED = 'just_updated',
  NOT_STALE = 'not_stale',
}

/** Edge connecting a child node to its parent, with a quadratic bezier control point. */
export interface LayoutEdge {
  fromId: string;
  toId: string;
  fromX: number;
  fromY: number;
  toX: number;
  toY: number;
  controlX: number;
  controlY: number;
}
