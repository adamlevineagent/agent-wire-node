import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useCanvasSetup } from './pyramid-viz/useCanvasSetup';
import { usePyramidLayout } from './pyramid-viz/usePyramidLayout';
import type { TreeNode, FlatNode, LayoutNode, LayoutEdge } from './pyramid-viz/types';
import { NodeState } from './pyramid-viz/types';
import { OverlaySelector } from './OverlaySelector';
import { ComposedView } from './pyramid-viz/ComposedView';

// ── Shared type imports (mirrors DADBEARPanel's definitions) ──────────

interface StaleLogEntry {
  id: number;
  slug: string;
  batch_id: string;
  layer: number;
  target_id: string;
  stale: string;
  reason: string;
  checker_index: number;
  checker_batch_size: number;
  checked_at: string;
  cost_tokens: number | null;
  cost_usd: number | null;
}

interface AutoUpdateStatus {
  auto_update: boolean;
  frozen: boolean;
  breaker_tripped: boolean;
  pending_mutations_by_layer: Record<string, number>;
  last_check_at: string | null;
  phase: string | null;
  phase_detail: string | null;
  timer_fires_at: string | null;
  last_result_summary: string | null;
}

interface EvidenceLink {
  slug: string;
  source_node_id: string;
  target_node_id: string;
  verdict: "KEEP" | "DISCONNECT" | "MISSING";
  weight: number | null;
  reason: string | null;
  live?: boolean;
}

interface GapReport {
  question_id: string;
  description: string;
  layer: number;
}

interface QuestionContext {
  parent_question: string | null;
  sibling_questions: string[];
}

interface DrillResult {
  node: {
    id: string;
    slug: string;
    depth: number;
    chunk_index: number | null;
    headline: string;
    distilled: string;
    self_prompt: string;
    children: string[];
    parent_id: string | null;
    superseded_by: string | null;
    created_at: string;
    topics: Array<{name: string; current: string; entities: string[]; corrections: any[]; decisions: any[]}>;
    corrections: any[];
    decisions: any[];
    terms: any[];
    dead_ends: string[];
  };
  children: Array<{
    id: string;
    slug: string;
    depth: number;
    headline: string;
    distilled: string;
    self_prompt: string;
    chunk_index: number | null;
    children: string[];
    parent_id: string | null;
    superseded_by: string | null;
    created_at: string;
    topics: Array<{name: string; current: string; entities: string[]; corrections: any[]; decisions: any[]}>;
    corrections: any[];
    decisions: any[];
    terms: any[];
    dead_ends: string[];
  }>;
  web_edges?: Array<{
    connected_to: string;
    connected_headline: string;
    relationship: string;
    strength: number;
  }>;
  remote_web_edges?: Array<{
    remote_handle_path: string;
    remote_slug: string;
    relationship: string;
    relevance: number;
    build_id: string;
  }>;
  evidence?: Array<EvidenceLink>;
  gaps?: Array<GapReport>;
  question_context?: QuestionContext | null;
}

// ── Props ─────────────────────────────────────────────────────────────

type VizMode = 'pyramid' | 'composed';

interface PyramidVisualizationProps {
  slug: string;
  contentType?: string;
  referencingSlugs?: string[];
  staleLog: StaleLogEntry[];
  status: AutoUpdateStatus | null;
  onNavigateToSlug?: (slug: string, nodeId: string) => void;
}

// ── Cross-slug helpers ────────────────────────────────────────────────

/** Parse a handle-path child ID like "other-slug/0/L0-003" into parts */
function parseCrossSlugId(childId: string): { isExternal: boolean; slug: string; depth: string; nodeId: string } | null {
  if (!childId.includes('/')) return null;
  const parts = childId.split('/');
  if (parts.length < 3) return null;
  return {
    isExternal: true,
    slug: parts[0],
    depth: parts[1],
    nodeId: parts.slice(2).join('/'),
  };
}

// ── Helpers ───────────────────────────────────────────────────────────

function flattenTree(roots: TreeNode[], parentId: string | null = null): FlatNode[] {
  const result: FlatNode[] = [];
  for (const node of roots) {
    result.push({
      id: node.id,
      depth: node.depth,
      headline: node.headline,
      distilled: node.distilled,
      threadId: node.threadId ?? null,
      sourcePath: node.sourcePath ?? null,
      parentId,
      childIds: node.children.map((c) => c.id),
    });
    result.push(...flattenTree(node.children, node.id));
  }
  return result;
}

function normalizeTreeNode(value: unknown): TreeNode | null {
  if (!value || typeof value !== 'object') return null;

  const raw = value as Record<string, unknown>;
  if (typeof raw.id !== 'string') return null;

  const depthValue =
    typeof raw.depth === 'number' ? raw.depth : Number(raw.depth);
  if (!Number.isFinite(depthValue)) return null;

  const children = Array.isArray(raw.children)
    ? raw.children
        .map((child) => normalizeTreeNode(child))
        .filter((child): child is TreeNode => child !== null)
    : [];

  return {
    id: raw.id,
    depth: depthValue,
    headline:
      typeof raw.headline === 'string' && raw.headline.trim().length > 0
        ? raw.headline
        : raw.id,
    distilled: typeof raw.distilled === 'string' ? raw.distilled : '',
    threadId:
      typeof raw.threadId === 'string'
        ? raw.threadId
        : typeof raw.thread_id === 'string'
          ? raw.thread_id
          : null,
    sourcePath:
      typeof raw.sourcePath === 'string'
        ? raw.sourcePath
        : typeof raw.source_path === 'string'
          ? raw.source_path
          : null,
    children,
  };
}

function nodeTitle(node: Pick<FlatNode, 'headline' | 'id'>): string {
  return node.headline?.trim() ? node.headline : node.id;
}

function normalizeTreeData(value: unknown): TreeNode[] {
  if (Array.isArray(value)) {
    return value
      .map((node) => normalizeTreeNode(node))
      .filter((node): node is TreeNode => node !== null);
  }

  if (value && typeof value === 'object') {
    const raw = value as Record<string, unknown>;

    if (Array.isArray(raw.roots)) {
      return raw.roots
        .map((node) => normalizeTreeNode(node))
        .filter((node): node is TreeNode => node !== null);
    }

    const singleNode = normalizeTreeNode(raw);
    if (singleNode) {
      return [singleNode];
    }
  }

  console.warn('PyramidVisualization: unexpected pyramid_tree payload', value);
  return [];
}

function isStaleValue(value: string): boolean {
  return value === 'yes' || value === 'Yes' || value === '1' || value === 'true';
}

function deriveNodeState(
  node: Pick<FlatNode, 'id' | 'threadId' | 'sourcePath'>,
  log: StaleLogEntry[],
): NodeState {
  const targets = new Set<string>([node.id]);
  if (node.threadId) targets.add(node.threadId);
  if (node.sourcePath) targets.add(node.sourcePath);

  const entry = log
    .filter((e) => targets.has(e.target_id))
    .sort(
      (a, b) =>
        new Date(b.checked_at).getTime() - new Date(a.checked_at).getTime(),
    )[0];

  if (!entry) return NodeState.STABLE;

  const age = Date.now() - new Date(entry.checked_at).getTime();
  const ACTIVE_WINDOW = 30 * 60 * 1000;
  const JUST_UPDATED_WINDOW = 60 * 1000;

  if (age > ACTIVE_WINDOW) return NodeState.STABLE;

  if (isStaleValue(entry.stale)) {
    if (age < JUST_UPDATED_WINDOW) return NodeState.JUST_UPDATED;
    return NodeState.STALE_CONFIRMED;
  }

  return NodeState.NOT_STALE;
}

// ── Node color map ────────────────────────────────────────────────────

const NODE_COLORS: Record<NodeState, { fill: string; hover: string }> = {
  [NodeState.STABLE]: {
    fill: 'rgba(34, 211, 238, 0.4)',
    hover: 'rgba(34, 211, 238, 0.7)',
  },
  [NodeState.STALE_CONFIRMED]: {
    fill: 'rgba(64, 208, 128, 0.9)',
    hover: 'rgba(64, 208, 128, 1.0)',
  },
  [NodeState.JUST_UPDATED]: {
    fill: 'rgba(64, 208, 128, 0.7)',
    hover: 'rgba(64, 208, 128, 0.9)',
  },
  [NodeState.NOT_STALE]: {
    fill: 'rgba(72, 230, 255, 0.82)',
    hover: 'rgba(120, 240, 255, 0.98)',
  },
};

const EDGE_COLOR = 'rgba(34, 211, 238, 0.15)';
const LABEL_COLOR = 'rgba(255, 255, 255, 0.2)';

// ── Component ─────────────────────────────────────────────────────────

export function PyramidVisualization({ slug, contentType, referencingSlugs, staleLog, status, onNavigateToSlug }: PyramidVisualizationProps) {
  const isQuestion = contentType === 'question';
  const [vizMode, setVizMode] = useState<VizMode>(isQuestion ? 'composed' : 'pyramid');

  // Reset mode when slug/contentType changes
  useEffect(() => {
    setVizMode(contentType === 'question' ? 'composed' : 'pyramid');
  }, [slug, contentType]);

  // State
  const [treeData, setTreeData] = useState<TreeNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [hoveredNode, setHoveredNode] = useState<LayoutNode | null>(null);
  const [tooltipPos, setTooltipPos] = useState({ x: 0, y: 0 });
  const [popoverNode, setPopoverNode] = useState<LayoutNode | null>(null);
  const [drillData, setDrillData] = useState<DrillResult | null>(null);
  const [drillLoading, setDrillLoading] = useState(false);

  // Overlay state: tracks which question overlay build_ids are toggled on
  const [activeOverlays, setActiveOverlays] = useState<Set<string>>(new Set());
  const handleToggleOverlay = useCallback((buildId: string) => {
    setActiveOverlays(prev => {
      const next = new Set(prev);
      if (next.has(buildId)) {
        next.delete(buildId);
      } else {
        next.add(buildId);
      }
      return next;
    });
  }, []);

  // Refs
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rafRef = useRef<number>(0);
  const lastMouseMoveRef = useRef<number>(0);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isIdleRef = useRef(false);

  // Canvas setup
  const { width, height } = useCanvasSetup([canvasRef], containerRef);

  // Flatten tree
  const flatNodes = useMemo(() => {
    return flattenTree(treeData);
  }, [treeData]);

  // Layout
  const { nodes: layoutNodes, edges } = usePyramidLayout(flatNodes, width, height);

  // Diagnostic: log layout results
  // Apply stale log state to layout nodes
  const stateNodes = useMemo(() => {
    return layoutNodes.map((node) => ({
      ...node,
      state: deriveNodeState(node, staleLog),
    }));
  }, [layoutNodes, staleLog]);

  // maxDepth for layer labels
  const maxDepth = useMemo(() => {
    if (stateNodes.length === 0) return 0;
    return Math.max(...stateNodes.map((n) => n.depth));
  }, [stateNodes]);

  // ── Fetch tree data ─────────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false;
    setLoading(true);

    invoke<unknown>('pyramid_tree', { slug })
      .then((data) => {
        if (!cancelled) {
          const normalized = normalizeTreeData(data);
          setTreeData(normalized);
          setLoading(false);
        }
      })
      .catch((err) => {
        console.error('PyramidViz: pyramid_tree FAILED:', err);
        if (!cancelled) {
          setTreeData([]);
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [slug]);

  // ── Render loop ─────────────────────────────────────────────────────

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    ctx.clearRect(0, 0, width, height);

    if (stateNodes.length === 0) {
      // Empty state: faint triangle outline
      ctx.beginPath();
      ctx.moveTo(width / 2, 40);
      ctx.lineTo(width - 40, height - 40);
      ctx.lineTo(40, height - 40);
      ctx.closePath();
      ctx.strokeStyle = 'rgba(34, 211, 238, 0.08)';
      ctx.lineWidth = 1;
      ctx.stroke();

      ctx.font = '14px Inter, sans-serif';
      ctx.fillStyle = 'rgba(255, 255, 255, 0.3)';
      ctx.textAlign = 'center';
      ctx.fillText('Build a pyramid to see it here', width / 2, height / 2);
      return;
    }

    // Draw edges
    ctx.strokeStyle = EDGE_COLOR;
    ctx.lineWidth = 0.5;
    for (const edge of edges) {
      ctx.beginPath();
      ctx.moveTo(edge.fromX, edge.fromY);
      ctx.quadraticCurveTo(edge.controlX, edge.controlY, edge.toX, edge.toY);
      ctx.stroke();
    }

    // Draw layer labels
    ctx.font = '10px monospace';
    ctx.fillStyle = LABEL_COLOR;
    ctx.textAlign = 'left';
    const drawnLabels = new Set<number>();
    for (const node of stateNodes) {
      if (!drawnLabels.has(node.depth)) {
        drawnLabels.add(node.depth);
        ctx.fillText(`L${node.depth}`, 12, node.y + 3);
      }
    }

    // Draw nodes (lower depth first so higher depth renders on top)
    const sortedNodes = [...stateNodes].sort((a, b) => a.depth - b.depth);

    for (const node of sortedNodes) {
      const isHovered = hoveredNode?.id === node.id;
      const colors = NODE_COLORS[node.state];
      const fillColor = isHovered ? colors.hover : colors.fill;
      const drawRadius = isHovered ? node.radius * 1.4 : node.radius;

      if (node.state === NodeState.STALE_CONFIRMED || node.state === NodeState.JUST_UPDATED) {
        ctx.shadowBlur = isHovered ? 22 : 16;
        ctx.shadowColor = 'rgba(64, 208, 128, 0.55)';
      } else if (node.state === NodeState.NOT_STALE) {
        ctx.shadowBlur = isHovered ? 18 : 12;
        ctx.shadowColor = 'rgba(72, 230, 255, 0.42)';
      } else {
        ctx.shadowBlur = 0;
        ctx.shadowColor = 'transparent';
      }

      ctx.beginPath();
      ctx.arc(node.x, node.y, drawRadius, 0, Math.PI * 2);
      ctx.fillStyle = fillColor;
      ctx.fill();

      if (node.state !== NodeState.STABLE) {
        ctx.lineWidth = isHovered ? 1.5 : 1;
        ctx.strokeStyle =
          node.state === NodeState.NOT_STALE
            ? 'rgba(180, 248, 255, 0.65)'
            : 'rgba(140, 255, 190, 0.7)';
        ctx.stroke();
      }
    }

    ctx.shadowBlur = 0;
    ctx.shadowColor = 'transparent';

    // Apex label
    const apexNodes = stateNodes.filter((n) => n.depth === maxDepth);
    if (apexNodes.length === 1) {
      const apex = apexNodes[0];
      const label =
        nodeTitle(apex).length > 34
          ? nodeTitle(apex).slice(0, 34) + '...'
          : nodeTitle(apex);
      ctx.font = '11px Inter, sans-serif';
      ctx.fillStyle = 'rgba(255, 255, 255, 0.6)';
      ctx.textAlign = 'center';
      ctx.fillText(label, apex.x, apex.y + apex.radius + 16);
    }
  }, [width, height, stateNodes, edges, hoveredNode, maxDepth]);

  // rAF loop with idle detection
  const startLoop = useCallback(() => {
    isIdleRef.current = false;
    if (idleTimerRef.current) clearTimeout(idleTimerRef.current);

    const loop = () => {
      draw();
      if (!isIdleRef.current) {
        rafRef.current = requestAnimationFrame(loop);
      }
    };
    cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(loop);

    // Idle after 5s of no state changes
    idleTimerRef.current = setTimeout(() => {
      isIdleRef.current = true;
      // Do one final draw
      draw();
    }, 5000);
  }, [draw]);

  useEffect(() => {
    if (loading || stateNodes.length === 0) {
      // Still draw empty state once
      draw();
      return;
    }
    startLoop();
    return () => {
      cancelAnimationFrame(rafRef.current);
      if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
    };
  }, [loading, stateNodes, startLoop, draw]);

  // Restart loop on staleLog or status changes
  useEffect(() => {
    if (!loading && stateNodes.length > 0) {
      startLoop();
    }
  }, [staleLog, status, loading, stateNodes.length, startLoop]);

  // ── Mouse interaction ───────────────────────────────────────────────

  const hitTest = useCallback(
    (clientX: number, clientY: number): LayoutNode | null => {
      const container = containerRef.current;
      if (!container) return null;
      const rect = container.getBoundingClientRect();
      const mx = clientX - rect.left;
      const my = clientY - rect.top;

      // Iterate reverse depth order (highest depth = apex first, more important)
      const sorted = [...stateNodes].sort((a, b) => b.depth - a.depth);
      for (const node of sorted) {
        const dx = mx - node.x;
        const dy = my - node.y;
        const hitRadius = node.radius + 4;
        if (dx * dx + dy * dy <= hitRadius * hitRadius) {
          return node;
        }
      }
      return null;
    },
    [stateNodes],
  );

  const handleMouseMove = useCallback(
    (e: React.MouseEvent) => {
      const now = Date.now();
      if (now - lastMouseMoveRef.current < 16) return; // 16ms throttle
      lastMouseMoveRef.current = now;

      const node = hitTest(e.clientX, e.clientY);
      setHoveredNode(node);

      if (node && containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        setTooltipPos({
          x: e.clientX - rect.left + 12,
          y: e.clientY - rect.top - 8,
        });
      }

      // Restart loop on mouse activity if idle
      if (isIdleRef.current) {
        startLoop();
      }
    },
    [hitTest, startLoop],
  );

  const handleMouseLeave = useCallback(() => {
    setHoveredNode(null);
  }, []);

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      const node = hitTest(e.clientX, e.clientY);
      if (node) {
        setPopoverNode(node);
        setDrillData(null);
        setDrillLoading(true);
        invoke<DrillResult>('pyramid_drill', { slug, nodeId: node.id })
          .then((data) => {
            setDrillData(data);
            setDrillLoading(false);
          })
          .catch(() => {
            setDrillLoading(false);
          });
      } else {
        setPopoverNode(null);
        setDrillData(null);
      }
    },
    [hitTest, slug],
  );

  const handlePopoverChildClick = useCallback(
    (childId: string) => {
      const child = stateNodes.find((n) => n.id === childId);
      if (child) {
        setPopoverNode(child);
      } else {
        // Node not in layout tree (e.g. superseded) — create synthetic node
        // using current popover position so drill data still displays
        const currentPos = popoverNode;
        if (currentPos) {
          setPopoverNode({
            id: childId,
            depth: (currentPos.depth ?? 0) + 1,
            headline: '',
            distilled: '',
            threadId: null,
            sourcePath: null,
            parentId: currentPos.id,
            childIds: [],
            x: currentPos.x,
            y: currentPos.y,
            radius: currentPos.radius,
            state: NodeState.STABLE,
          });
        }
      }
      setDrillData(null);
      setDrillLoading(true);
      invoke<DrillResult>('pyramid_drill', { slug, nodeId: childId })
        .then((data) => {
          setDrillData(data);
          setDrillLoading(false);
        })
        .catch(() => {
          setDrillLoading(false);
        });
    },
    [stateNodes, slug, popoverNode],
  );

  // ── Stale history for popover ───────────────────────────────────────

  const popoverStaleHistory = useMemo(() => {
    if (!popoverNode) return [];
    const targets = new Set<string>([popoverNode.id]);
    if (popoverNode.threadId) targets.add(popoverNode.threadId);
    if (popoverNode.sourcePath) targets.add(popoverNode.sourcePath);
    return staleLog
      .filter((e) => targets.has(e.target_id))
      .slice(0, 10);
  }, [popoverNode, staleLog]);

  // ── Popover position ────────────────────────────────────────────────

  const popoverStyle = useMemo(() => {
    if (!popoverNode) return {};
    // Position near the node, offset right
    let left = popoverNode.x + popoverNode.radius + 16;
    let top = popoverNode.y - 60;

    // Keep within bounds (popover is 460px wide via CSS)
    if (left + 460 > width) {
      left = popoverNode.x - popoverNode.radius - 476;
    }
    if (left < 8) left = 8;
    if (top < 8) top = 8;

    return { left, top };
  }, [popoverNode, width, height]);

  // ── Render ──────────────────────────────────────────────────────────

  // ── Composed view drill handler ────────────────────────────────────
  const handleComposedDrill = useCallback((nodeId: string, nodeSlug: string) => {
    if (nodeSlug !== slug && onNavigateToSlug) {
      onNavigateToSlug(nodeSlug, nodeId);
    } else {
      // Drill into node — switch to pyramid view and trigger popover
      setVizMode('pyramid');
      // Find the node in layout and open popover
      const layoutNode = stateNodes.find(n => n.id === nodeId);
      if (layoutNode) {
        setPopoverNode(layoutNode);
        setDrillData(null);
        setDrillLoading(true);
        invoke<DrillResult>('pyramid_drill', { slug, nodeId })
          .then((data) => { setDrillData(data); setDrillLoading(false); })
          .catch(() => { setDrillLoading(false); });
      }
    }
  }, [slug, onNavigateToSlug, stateNodes]);

  if (loading && treeData.length === 0) {
    return (
      <div className="pyramid-viz-container">
        <div className="pyramid-viz-inner">
          <div className="pyramid-viz-empty">Loading pyramid...</div>
        </div>
      </div>
    );
  }

  return (
    <div className="pyramid-viz-container">
      {/* View toggle for question slugs */}
      {isQuestion && (
        <div className="composed-view-toggle-bar">
          <button
            className={`composed-view-toggle-btn${vizMode === 'composed' ? ' active' : ''}`}
            onClick={() => setVizMode('composed')}
          >
            Composed View
          </button>
          <button
            className={`composed-view-toggle-btn${vizMode === 'pyramid' ? ' active' : ''}`}
            onClick={() => setVizMode('pyramid')}
          >
            Pyramid View
          </button>
        </div>
      )}

      {vizMode === 'composed' && isQuestion ? (
        <ComposedView slug={slug} onDrill={handleComposedDrill} />
      ) : (
        <>
      <OverlaySelector
        slug={slug}
        activeOverlays={activeOverlays}
        onToggleOverlay={handleToggleOverlay}
        referencingSlugs={referencingSlugs}
      />
      <div className="pyramid-viz-inner" ref={containerRef}>
        <canvas
          ref={canvasRef}
          className="pyramid-viz-canvas"
          onMouseMove={handleMouseMove}
          onMouseLeave={handleMouseLeave}
          onClick={handleClick}
        />

        {/* Layer labels (HTML overlay for crisp text) */}
        {stateNodes.length > 0 &&
          Array.from(new Set(stateNodes.map((n) => n.depth))).map((depth) => {
            const nodesAtDepth = stateNodes.filter((n) => n.depth === depth);
            const yCenter = nodesAtDepth[0]?.y ?? 0;
            return (
              <span
                key={`label-${depth}`}
                className="pyramid-viz-layer-label"
                style={{ top: yCenter - 5 }}
              >
                L{depth}
              </span>
            );
          })}

        {/* Tooltip */}
        {hoveredNode && !popoverNode && (
          <div
            className="pyramid-viz-tooltip"
            style={{
              left: tooltipPos.x,
              top: tooltipPos.y,
              opacity: 1,
            }}
          >
            <div style={{ fontWeight: 500, marginBottom: 2 }}>
              {nodeTitle(hoveredNode)}
            </div>
            <div style={{ opacity: 0.45, fontSize: 10, marginBottom: 4 }}>
              {hoveredNode.id}
            </div>
            {hoveredNode.threadId && hoveredNode.threadId !== hoveredNode.id && (
              <div style={{ opacity: 0.6, fontSize: 10, marginBottom: 4 }}>
                thread {hoveredNode.threadId}
              </div>
            )}
            {hoveredNode.sourcePath && (
              <div style={{ opacity: 0.5, fontSize: 10, marginBottom: 4 }}>
                {hoveredNode.sourcePath.split('/').slice(-2).join('/')}
              </div>
            )}
            <div style={{ opacity: 0.7, fontSize: 11 }}>
              {hoveredNode.distilled.slice(0, 100)}
              {hoveredNode.distilled.length > 100 ? '...' : ''}
            </div>
            <div style={{ opacity: 0.5, fontSize: 10, marginTop: 2 }}>
              L{hoveredNode.depth}
            </div>
          </div>
        )}

        {/* Popover */}
        {popoverNode && (
          <div
            className="pyramid-viz-popover"
            style={popoverStyle}
          >
            <button
              className="pyramid-viz-popover-close"
              onClick={(e) => {
                e.stopPropagation();
                setPopoverNode(null);
                setDrillData(null);
              }}
            >
              x
            </button>

            {/* THE QUESTION (self_prompt) */}
            {drillData?.node.self_prompt?.trim() && (
              <div style={{
                fontStyle: 'italic',
                fontSize: 14,
                color: 'var(--accent-cyan)',
                marginBottom: 12,
                paddingRight: 20,
                lineHeight: 1.4,
              }}>
                {drillData.node.self_prompt}
              </div>
            )}

            <div style={{ fontWeight: 600, marginBottom: 14, paddingRight: 20, fontSize: 15 }}>
              {nodeTitle(popoverNode)}
              <span style={{ opacity: 0.4, marginLeft: 8, fontWeight: 400, fontSize: 11 }}>
                L{popoverNode.depth}
              </span>
            </div>

            <div style={{ marginBottom: 8, opacity: 0.45, fontSize: 11 }}>
              {popoverNode.id}
            </div>

            {popoverNode.threadId && popoverNode.threadId !== popoverNode.id && (
              <div style={{ marginBottom: 8, opacity: 0.6, fontSize: 11 }}>
                Thread: {popoverNode.threadId}
              </div>
            )}

            {popoverNode.sourcePath && (
              <div style={{ marginBottom: 10, opacity: 0.55, fontSize: 11, wordBreak: 'break-all' }}>
                {popoverNode.sourcePath}
              </div>
            )}

            {/* Distillation */}
            <div style={{ marginBottom: 12, lineHeight: 1.5, opacity: 0.85 }}>
              {drillLoading
                ? 'Loading...'
                : drillData?.node.distilled ?? popoverNode.distilled}
            </div>

            {/* Evidence */}
            {drillData?.evidence && drillData.evidence.length > 0 && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Evidence
                </div>
                {drillData.evidence.map((ev, i) => {
                  const isSuperseded = ev.live === false;
                  const verdictIcon = ev.verdict === 'KEEP' ? '\u2713' : ev.verdict === 'DISCONNECT' ? '\u2717' : '?';
                  const verdictColor = ev.verdict === 'KEEP'
                    ? 'var(--accent-green)'
                    : ev.verdict === 'DISCONNECT'
                      ? '#e87040'
                      : '#e8c840';
                  const sourceHeadline = drillData.children.find(c => c.id === ev.source_node_id)?.headline
                    ?? (drillData.node.id === ev.source_node_id ? drillData.node.headline : null)
                    ?? ev.source_node_id;
                  return (
                    <div
                      key={`ev-${i}`}
                      className={`evidence-row${isSuperseded ? ' evidence-superseded' : ''}`}
                    >
                      <span style={{ color: isSuperseded ? undefined : verdictColor, fontWeight: 600, flexShrink: 0 }}>{verdictIcon}</span>
                      <span style={{ flexShrink: 0 }}>
                        {ev.verdict.toUpperCase()}{ev.weight != null ? ` (${ev.weight.toFixed(1)})` : ''}
                      </span>
                      {isSuperseded && (
                        <span className="evidence-superseded-badge">superseded</span>
                      )}
                      <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {sourceHeadline}{ev.reason ? ` \u2014 ${ev.reason}` : ''}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}

            {/* Gaps */}
            {drillData?.gaps && drillData.gaps.length > 0 && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Gaps
                </div>
                {drillData.gaps.map((gap, i) => (
                  <div key={`gap-${i}`} style={{ fontSize: 12, padding: '2px 0', opacity: 0.8, color: '#e8c840' }}>
                    {'\u26A0'} {gap.description}
                  </div>
                ))}
              </div>
            )}

            {/* Children */}
            {drillData && drillData.children.length > 0 && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Children
                </div>
                {drillData.children.map((child) => {
                  const crossSlug = parseCrossSlugId(child.id);
                  return (
                    <div
                      key={child.id}
                      className="drill-child-row"
                      onClick={(e) => {
                        e.stopPropagation();
                        if (crossSlug && onNavigateToSlug) {
                          onNavigateToSlug(crossSlug.slug, crossSlug.nodeId);
                        } else {
                          handlePopoverChildClick(child.id);
                        }
                      }}
                    >
                      {crossSlug && (
                        <span className="cross-slug-badge" title={`From ${crossSlug.slug} at depth ${crossSlug.depth}`}>
                          {crossSlug.slug}
                          <span className="cross-slug-depth">L{crossSlug.depth}</span>
                        </span>
                      )}
                      <span className="drill-child-text">
                        {(child.headline?.trim() ? child.headline : child.id)}: {child.distilled.slice(0, 60)}
                        {child.distilled.length > 60 ? '...' : ''}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}

            {/* Question Tree Context */}
            {drillData?.question_context && (
              drillData.question_context.parent_question ||
              drillData.question_context.sibling_questions.length > 0
            ) && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Question Tree
                </div>
                {drillData.question_context.parent_question && (
                  <div style={{ fontSize: 11, opacity: 0.6, padding: '2px 0' }}>
                    Parent: {drillData.question_context.parent_question}
                  </div>
                )}
                {drillData.question_context.sibling_questions.length > 0 && (
                  <div style={{ fontSize: 11, opacity: 0.6, padding: '2px 0' }}>
                    Siblings: {drillData.question_context.sibling_questions.join(', ')}
                  </div>
                )}
              </div>
            )}

            {/* Web Edges (Related) */}
            {drillData?.web_edges && drillData.web_edges.length > 0 && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Related
                </div>
                {drillData.web_edges.map((edge, i) => (
                  <div key={`edge-${i}`} style={{ fontSize: 12, padding: '2px 0', opacity: 0.7 }}>
                    {'\u2194'} {edge.connected_headline} ({edge.strength.toFixed(1)})
                  </div>
                ))}
              </div>
            )}

            {/* Remote Web Edges (WS-ONLINE-F) */}
            {drillData?.remote_web_edges && drillData.remote_web_edges.length > 0 && (
              <div style={{ marginBottom: 12 }}>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Remote Connections
                </div>
                {drillData.remote_web_edges.map((edge, i) => (
                  <div key={`remote-edge-${i}`} style={{
                    fontSize: 12, padding: '4px 6px', opacity: 0.8,
                    display: 'flex', alignItems: 'center', gap: 6,
                  }}>
                    <span style={{
                      fontSize: 9, padding: '1px 4px', borderRadius: 3,
                      background: 'rgba(100, 180, 255, 0.15)',
                      color: 'rgba(100, 180, 255, 0.9)',
                      fontWeight: 600, textTransform: 'uppercase',
                    }}>
                      remote
                    </span>
                    <span style={{ opacity: 0.7 }}>
                      {edge.remote_slug}
                    </span>
                    <span style={{ opacity: 0.5, fontSize: 11 }}>
                      {edge.relationship} ({edge.relevance.toFixed(1)})
                    </span>
                  </div>
                ))}
              </div>
            )}

            {/* Stale history */}
            {popoverStaleHistory.length > 0 && (
              <div>
                <div style={{
                  fontSize: 11, opacity: 0.5, marginBottom: 4, marginTop: 12,
                  textTransform: 'uppercase', letterSpacing: '0.5px',
                }}>
                  Stale History
                </div>
                {popoverStaleHistory.map((entry) => {
                  const isStale = isStaleValue(entry.stale);
                  return (
                    <div
                      key={entry.id}
                      style={{
                        fontSize: 11,
                        padding: '2px 0',
                        opacity: 0.7,
                        display: 'flex',
                        gap: 8,
                      }}
                    >
                      <span
                        style={{
                          color: isStale
                            ? '#e87040'
                            : 'var(--accent-green)',
                          fontWeight: 500,
                        }}
                      >
                        {isStale ? 'STALE' : 'OK'}
                      </span>
                      <span style={{ opacity: 0.5 }}>
                        {new Date(entry.checked_at).toLocaleTimeString([], {
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </span>
                      <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {entry.reason}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </div>
        </>
      )}
    </div>
  );
}
