/**
 * ComposedView — Force-directed graph visualization for composed pyramid + understanding web.
 *
 * Shows mechanical nodes (teal) and answer nodes (purple) connected by
 * evidence, child, and web edges. Uses a simple spring-based force simulation
 * rendered to canvas.
 */

import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useCanvasSetup } from './useCanvasSetup';

// ── Types ─────────────────────────────────────────────────────────────

export interface ComposedNode {
  id: string;
  slug: string;
  depth: number;
  headline: string;
  distilled: string;
  node_type: 'mechanical' | 'answer';
}

export interface ComposedEdge {
  source_id: string;
  target_id: string;
  weight: number;
  edge_type: 'evidence' | 'child' | 'web';
  live: boolean;
}

export interface ComposedViewData {
  nodes: ComposedNode[];
  edges: ComposedEdge[];
  slugs: string[];
}

interface SimNode extends ComposedNode {
  x: number;
  y: number;
  vx: number;
  vy: number;
  radius: number;
  connectionCount: number;
  pinned: boolean;
}

interface ComposedViewProps {
  slug: string;
  onDrill: (nodeId: string, nodeSlug: string) => void;
}

// ── Colors ────────────────────────────────────────────────────────────

const MECHANICAL_FILL = 'rgba(34, 211, 238, 0.55)';
const MECHANICAL_HOVER = 'rgba(34, 211, 238, 0.85)';
const ANSWER_FILL = 'rgba(168, 85, 247, 0.55)';
const ANSWER_HOVER = 'rgba(168, 85, 247, 0.85)';

const EDGE_COLORS: Record<string, { stroke: string; dead: string }> = {
  evidence: { stroke: 'rgba(34, 211, 238, 0.35)', dead: 'rgba(100, 100, 100, 0.12)' },
  child: { stroke: 'rgba(255, 255, 255, 0.18)', dead: 'rgba(100, 100, 100, 0.08)' },
  web: { stroke: 'rgba(168, 85, 247, 0.25)', dead: 'rgba(100, 100, 100, 0.08)' },
};

// ── Force simulation constants ────────────────────────────────────────

const REPULSION = 800;
const SPRING_K = 0.004;
const SPRING_REST = 80;
const DAMPING = 0.88;
const CENTER_GRAVITY = 0.008;
const SIM_STEPS_PER_FRAME = 3;
const SETTLE_VELOCITY = 0.05;
const MAX_SETTLE_FRAMES = 600; // stop sim after ~10s at 60fps

// ── Helpers ───────────────────────────────────────────────────────────

function truncate(text: string, max: number): string {
  if (!text || text.length <= max) return text || '';
  return text.slice(0, max - 3) + '...';
}

function nodeRadius(connectionCount: number): number {
  return Math.max(6, Math.min(22, 6 + connectionCount * 1.5));
}

// ── Component ─────────────────────────────────────────────────────────

export function ComposedView({ slug, onDrill }: ComposedViewProps) {
  const [data, setData] = useState<ComposedViewData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hoveredNode, setHoveredNode] = useState<SimNode | null>(null);
  const [tooltipPos, setTooltipPos] = useState({ x: 0, y: 0 });

  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const simNodesRef = useRef<SimNode[]>([]);
  const rafRef = useRef<number>(0);
  const dragRef = useRef<{ node: SimNode; offsetX: number; offsetY: number } | null>(null);
  const settledRef = useRef(false);
  const frameCountRef = useRef(0);
  const lastMouseMoveRef = useRef<number>(0);

  const { width, height } = useCanvasSetup([canvasRef], containerRef);

  // ── Fetch composed data ───────────────────────────────────────────

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    invoke<ComposedViewData>('pyramid_get_composed_view', { slug })
      .then((result) => {
        if (!cancelled) {
          setData(result);
          setLoading(false);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          const msg = String(err);
          if (msg.includes('not found') || msg.includes('unknown command') || msg.includes('did not find')) {
            setError('Composed view requires the pyramid_get_composed_view Tauri command (WS8-E).');
          } else {
            setError(msg);
          }
          setLoading(false);
        }
      });

    return () => { cancelled = true; };
  }, [slug]);

  // ── Initialize simulation nodes ───────────────────────────────────

  useEffect(() => {
    if (!data || width === 0 || height === 0) return;

    // Count connections per node
    const connMap = new Map<string, number>();
    for (const edge of data.edges) {
      connMap.set(edge.source_id, (connMap.get(edge.source_id) ?? 0) + 1);
      connMap.set(edge.target_id, (connMap.get(edge.target_id) ?? 0) + 1);
    }

    const cx = width / 2;
    const cy = height / 2;
    const spread = Math.min(width, height) * 0.35;

    const simNodes: SimNode[] = data.nodes.map((node, i) => {
      const angle = (i / data.nodes.length) * Math.PI * 2;
      const dist = spread * (0.3 + Math.random() * 0.7);
      const cc = connMap.get(node.id) ?? 0;
      return {
        ...node,
        x: cx + Math.cos(angle) * dist,
        y: cy + Math.sin(angle) * dist,
        vx: 0,
        vy: 0,
        radius: nodeRadius(cc),
        connectionCount: cc,
        pinned: false,
      };
    });

    simNodesRef.current = simNodes;
    settledRef.current = false;
    frameCountRef.current = 0;
  }, [data, width, height]);

  // ── Edge lookup for rendering ─────────────────────────────────────

  const edgeList = useMemo(() => data?.edges ?? [], [data]);

  // ── Simulation step ───────────────────────────────────────────────

  const simulate = useCallback(() => {
    const nodes = simNodesRef.current;
    if (nodes.length === 0) return;

    const cx = width / 2;
    const cy = height / 2;
    const nodeMap = new Map<string, SimNode>();
    for (const n of nodes) nodeMap.set(n.id, n);

    for (let step = 0; step < SIM_STEPS_PER_FRAME; step++) {
      // Repulsion (all pairs)
      for (let i = 0; i < nodes.length; i++) {
        for (let j = i + 1; j < nodes.length; j++) {
          const a = nodes[i];
          const b = nodes[j];
          let dx = a.x - b.x;
          let dy = a.y - b.y;
          let dist = Math.sqrt(dx * dx + dy * dy);
          if (dist < 1) { dx = Math.random() - 0.5; dy = Math.random() - 0.5; dist = 1; }
          const force = REPULSION / (dist * dist);
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          if (!a.pinned) { a.vx += fx; a.vy += fy; }
          if (!b.pinned) { b.vx -= fx; b.vy -= fy; }
        }
      }

      // Spring attraction (edges)
      for (const edge of edgeList) {
        const a = nodeMap.get(edge.source_id);
        const b = nodeMap.get(edge.target_id);
        if (!a || !b) continue;

        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const dist = Math.sqrt(dx * dx + dy * dy);
        if (dist < 0.1) continue;

        const displacement = dist - SPRING_REST;
        const force = SPRING_K * displacement * (edge.weight > 0 ? edge.weight : 0.5);
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;

        if (!a.pinned) { a.vx += fx; a.vy += fy; }
        if (!b.pinned) { b.vx -= fx; b.vy -= fy; }
      }

      // Center gravity
      for (const node of nodes) {
        if (node.pinned) continue;
        node.vx += (cx - node.x) * CENTER_GRAVITY;
        node.vy += (cy - node.y) * CENTER_GRAVITY;
      }

      // Integrate + damp
      for (const node of nodes) {
        if (node.pinned) continue;
        node.vx *= DAMPING;
        node.vy *= DAMPING;
        node.x += node.vx;
        node.y += node.vy;

        // Keep within bounds
        const pad = node.radius + 4;
        if (node.x < pad) { node.x = pad; node.vx = 0; }
        if (node.x > width - pad) { node.x = width - pad; node.vx = 0; }
        if (node.y < pad) { node.y = pad; node.vy = 0; }
        if (node.y > height - pad) { node.y = height - pad; node.vy = 0; }
      }
    }

    // Check if settled
    frameCountRef.current++;
    const maxV = nodes.reduce((m, n) => Math.max(m, Math.abs(n.vx), Math.abs(n.vy)), 0);
    if (maxV < SETTLE_VELOCITY || frameCountRef.current > MAX_SETTLE_FRAMES) {
      settledRef.current = true;
    }
  }, [width, height, edgeList]);

  // ── Draw ──────────────────────────────────────────────────────────

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const nodes = simNodesRef.current;
    const nodeMap = new Map<string, SimNode>();
    for (const n of nodes) nodeMap.set(n.id, n);

    ctx.clearRect(0, 0, width, height);

    if (nodes.length === 0) {
      ctx.font = '14px Inter, sans-serif';
      ctx.fillStyle = 'rgba(255, 255, 255, 0.3)';
      ctx.textAlign = 'center';
      ctx.fillText('No composed data available', width / 2, height / 2);
      return;
    }

    // Draw edges
    for (const edge of edgeList) {
      const a = nodeMap.get(edge.source_id);
      const b = nodeMap.get(edge.target_id);
      if (!a || !b) continue;

      const colors = EDGE_COLORS[edge.edge_type] ?? EDGE_COLORS.evidence;
      ctx.strokeStyle = edge.live ? colors.stroke : colors.dead;
      ctx.lineWidth = edge.live ? Math.max(0.5, edge.weight * 1.5) : 0.5;

      // Edge type styling
      if (edge.edge_type === 'child') {
        ctx.setLineDash([4, 4]);
      } else if (edge.edge_type === 'web') {
        ctx.setLineDash([2, 3]);
      } else {
        ctx.setLineDash([]);
      }

      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(b.x, b.y);
      ctx.stroke();
    }
    ctx.setLineDash([]);

    // Draw nodes
    for (const node of nodes) {
      const isHovered = hoveredNode?.id === node.id;
      const isMechanical = node.node_type === 'mechanical';
      const fillColor = isMechanical
        ? (isHovered ? MECHANICAL_HOVER : MECHANICAL_FILL)
        : (isHovered ? ANSWER_HOVER : ANSWER_FILL);
      const drawRadius = isHovered ? node.radius * 1.3 : node.radius;

      // Glow
      if (isHovered) {
        ctx.shadowBlur = 18;
        ctx.shadowColor = isMechanical ? 'rgba(34, 211, 238, 0.5)' : 'rgba(168, 85, 247, 0.5)';
      }

      ctx.beginPath();
      ctx.arc(node.x, node.y, drawRadius, 0, Math.PI * 2);
      ctx.fillStyle = fillColor;
      ctx.fill();

      // Border
      ctx.lineWidth = isHovered ? 1.5 : 0.8;
      ctx.strokeStyle = isMechanical
        ? 'rgba(34, 211, 238, 0.4)'
        : 'rgba(168, 85, 247, 0.4)';
      ctx.stroke();

      ctx.shadowBlur = 0;
      ctx.shadowColor = 'transparent';

      // Label for larger nodes
      if (node.radius >= 10) {
        const label = truncate(node.headline, 18);
        ctx.font = '9px Inter, sans-serif';
        ctx.fillStyle = 'rgba(255, 255, 255, 0.45)';
        ctx.textAlign = 'center';
        ctx.fillText(label, node.x, node.y + node.radius + 12);
      }
    }
  }, [width, height, edgeList, hoveredNode]);

  // ── Animation loop ────────────────────────────────────────────────

  useEffect(() => {
    if (loading || !data || width === 0 || height === 0) {
      draw();
      return;
    }

    let running = true;

    const loop = () => {
      if (!running) return;
      if (!settledRef.current || dragRef.current) {
        simulate();
      }
      draw();
      rafRef.current = requestAnimationFrame(loop);
    };

    rafRef.current = requestAnimationFrame(loop);

    return () => {
      running = false;
      cancelAnimationFrame(rafRef.current);
    };
  }, [loading, data, width, height, simulate, draw]);

  // ── Hit test ──────────────────────────────────────────────────────

  const hitTest = useCallback((clientX: number, clientY: number): SimNode | null => {
    const container = containerRef.current;
    if (!container) return null;
    const rect = container.getBoundingClientRect();
    const mx = clientX - rect.left;
    const my = clientY - rect.top;

    // Reverse iterate so top-rendered nodes get priority
    const nodes = simNodesRef.current;
    for (let i = nodes.length - 1; i >= 0; i--) {
      const node = nodes[i];
      const dx = mx - node.x;
      const dy = my - node.y;
      const hitRadius = node.radius + 4;
      if (dx * dx + dy * dy <= hitRadius * hitRadius) {
        return node;
      }
    }
    return null;
  }, []);

  // ── Mouse handlers ────────────────────────────────────────────────

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    const now = Date.now();
    if (now - lastMouseMoveRef.current < 16) return;
    lastMouseMoveRef.current = now;

    // Dragging
    if (dragRef.current) {
      const container = containerRef.current;
      if (!container) return;
      const rect = container.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      dragRef.current.node.x = mx - dragRef.current.offsetX;
      dragRef.current.node.y = my - dragRef.current.offsetY;
      dragRef.current.node.vx = 0;
      dragRef.current.node.vy = 0;
      // Unsettled while dragging
      settledRef.current = false;
      frameCountRef.current = 0;
      return;
    }

    const node = hitTest(e.clientX, e.clientY);
    setHoveredNode(node);

    if (node && containerRef.current) {
      const rect = containerRef.current.getBoundingClientRect();
      setTooltipPos({
        x: e.clientX - rect.left + 12,
        y: e.clientY - rect.top - 8,
      });
    }
  }, [hitTest]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    const node = hitTest(e.clientX, e.clientY);
    if (!node) return;

    const container = containerRef.current;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;

    node.pinned = true;
    dragRef.current = {
      node,
      offsetX: mx - node.x,
      offsetY: my - node.y,
    };

    e.preventDefault();
  }, [hitTest]);

  const handleMouseUp = useCallback(() => {
    if (dragRef.current) {
      dragRef.current.node.pinned = false;
      dragRef.current = null;
      // Let sim re-settle
      settledRef.current = false;
      frameCountRef.current = Math.max(frameCountRef.current, MAX_SETTLE_FRAMES - 120);
    }
  }, []);

  const handleMouseLeave = useCallback(() => {
    setHoveredNode(null);
    if (dragRef.current) {
      dragRef.current.node.pinned = false;
      dragRef.current = null;
    }
  }, []);

  const handleClick = useCallback((e: React.MouseEvent) => {
    // Only fire click if not dragging
    if (dragRef.current) return;
    const node = hitTest(e.clientX, e.clientY);
    if (node) {
      onDrill(node.id, node.slug);
    }
  }, [hitTest, onDrill]);

  // ── Render ────────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="composed-view-container" ref={containerRef}>
        <div className="composed-view-loading">Loading composed view...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="composed-view-container" ref={containerRef}>
        <div className="composed-view-error">{error}</div>
      </div>
    );
  }

  return (
    <div className="composed-view-container" ref={containerRef}>
      <canvas
        ref={canvasRef}
        className="composed-view-canvas"
        onMouseMove={handleMouseMove}
        onMouseDown={handleMouseDown}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseLeave}
        onClick={handleClick}
      />

      {/* Legend */}
      <div className="composed-view-legend">
        <span className="composed-view-legend-item">
          <span className="composed-view-legend-dot mechanical" />
          Mechanical
        </span>
        <span className="composed-view-legend-item">
          <span className="composed-view-legend-dot answer" />
          Answer
        </span>
        <span className="composed-view-legend-sep" />
        <span className="composed-view-legend-item">
          <span className="composed-view-legend-line evidence" />
          Evidence
        </span>
        <span className="composed-view-legend-item">
          <span className="composed-view-legend-line child" />
          Child
        </span>
        <span className="composed-view-legend-item">
          <span className="composed-view-legend-line web" />
          Web
        </span>
      </div>

      {/* Tooltip */}
      {hoveredNode && !dragRef.current && (
        <div
          className="pyramid-viz-tooltip"
          style={{
            left: tooltipPos.x,
            top: tooltipPos.y,
            opacity: 1,
          }}
        >
          <div style={{ fontWeight: 500, marginBottom: 2 }}>
            {hoveredNode.headline || hoveredNode.id}
          </div>
          <div style={{ opacity: 0.45, fontSize: 10, marginBottom: 4 }}>
            {hoveredNode.id}
          </div>
          <div style={{
            fontSize: 10,
            marginBottom: 4,
            color: hoveredNode.node_type === 'mechanical'
              ? 'var(--accent-cyan, #22d3ee)'
              : '#a855f7',
          }}>
            {hoveredNode.node_type} node
            {hoveredNode.slug !== slug ? ` (${hoveredNode.slug})` : ''}
          </div>
          {hoveredNode.distilled && (
            <div style={{ opacity: 0.7, fontSize: 11 }}>
              {truncate(hoveredNode.distilled, 160)}
            </div>
          )}
          <div style={{ opacity: 0.4, fontSize: 10, marginTop: 4 }}>
            L{hoveredNode.depth} &middot; {hoveredNode.connectionCount} connections
          </div>
        </div>
      )}
    </div>
  );
}
