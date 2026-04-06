import { useRef, useEffect, useCallback, useState } from 'react';
import { useCanvasSetup } from '../pyramid-viz/useCanvasSetup';
import type { LiveNodeInfo, SpatialNode } from './types';

interface LivePyramidStageProps {
    nodes: LiveNodeInfo[];
    currentStep: string | null;
    isActive: boolean;
    onNodeClick: (nodeId: string) => void;
}

// ── Layout constants ────────────────────────────────────────────────────────

const NODE_RADIUS = 8;
const NODE_RADIUS_LARGE = 12;
const LAYER_PADDING = 60;
const TOP_PADDING = 40;
const BOTTOM_PADDING = 40;
const LABEL_FONT = '11px -apple-system, BlinkMacSystemFont, sans-serif';
const LAYER_LABEL_FONT = 'bold 12px -apple-system, BlinkMacSystemFont, sans-serif';

// ── Color palette ───────────────────────────────────────────────────────────

const COLORS = {
    nodePending: '#555',
    nodeInflight: '#f5a623',
    nodeComplete: '#4ecdc4',
    nodeFailed: '#e74c3c',
    edge: 'rgba(255,255,255,0.12)',
    edgeActive: 'rgba(78,205,196,0.3)',
    label: '#aaa',
    layerLabel: '#666',
    bg: 'transparent',
};

export function LivePyramidStage({ nodes, currentStep, isActive, onNodeClick }: LivePyramidStageProps) {
    const containerRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const { width, height } = useCanvasSetup([canvasRef], containerRef);
    const spatialNodesRef = useRef<SpatialNode[]>([]);
    const animFrameRef = useRef(0);
    const [hoveredNode, setHoveredNode] = useState<string | null>(null);
    const pulseRef = useRef(0);

    // ── Compute spatial layout from LiveNodeInfo ────────────────────────
    useEffect(() => {
        if (width === 0 || height === 0 || nodes.length === 0) return;

        const maxDepth = Math.max(...nodes.map(n => n.depth));
        const nodesByDepth = new Map<number, LiveNodeInfo[]>();
        for (const n of nodes) {
            const list = nodesByDepth.get(n.depth) || [];
            list.push(n);
            nodesByDepth.set(n.depth, list);
        }

        const layerCount = maxDepth + 1;
        const usableHeight = height - TOP_PADDING - BOTTOM_PADDING;
        const layerHeight = layerCount > 1 ? usableHeight / layerCount : usableHeight;

        const existing = new Map(spatialNodesRef.current.map(n => [n.id, n]));
        const newSpatial: SpatialNode[] = [];

        for (let depth = 0; depth <= maxDepth; depth++) {
            const layerNodes = nodesByDepth.get(depth) || [];
            const count = layerNodes.length;
            if (count === 0) continue;

            // Y: L0 at bottom, apex at top
            const y = height - BOTTOM_PADDING - depth * layerHeight;

            // Band width narrows as depth increases (pyramid shape)
            const bandFraction = 1 - (depth / (maxDepth + 1)) * 0.6;
            const bandWidth = (width - 2 * LAYER_PADDING) * bandFraction;
            const bandStart = (width - bandWidth) / 2;

            // 500+ nodes at L0: use summary rectangles (10 nodes per rect)
            const useSummaryRects = count > 500 && depth === 0;
            const radius = useSummaryRects ? NODE_RADIUS
                : count <= 50 ? NODE_RADIUS_LARGE
                : NODE_RADIUS;
            const useStagger = count > 200 && count <= 500 && depth === 0;

            if (useSummaryRects) {
                const groupSize = 10;
                const groupCount = Math.ceil(count / groupSize);
                for (let g = 0; g < groupCount; g++) {
                    const groupNodes = layerNodes.slice(g * groupSize, (g + 1) * groupSize);
                    const xFraction = groupCount === 1 ? 0.5 : g / (groupCount - 1);
                    const targetX = bandStart + xFraction * bandWidth;
                    const completeCount = groupNodes.filter(n => n.status === 'complete').length;
                    const groupStatus = completeCount === groupNodes.length ? 'complete'
                        : completeCount > 0 ? 'inflight'
                        : 'pending';
                    const representative = groupNodes[0];
                    const prev = existing.get(representative.node_id);

                    newSpatial.push({
                        id: representative.node_id,
                        depth,
                        headline: `${groupNodes.length} nodes (${completeCount} complete)`,
                        parentId: representative.parent_id,
                        children: groupNodes.map(n => n.node_id),
                        status: groupStatus as SpatialNode['status'],
                        x: prev?.x ?? targetX,
                        y: prev?.y ?? y,
                        targetX,
                        targetY: y,
                        radius: NODE_RADIUS_LARGE, // larger for summary rects
                    });
                }
            } else {
                for (let i = 0; i < count; i++) {
                    const node = layerNodes[i];
                    const xFraction = count === 1 ? 0.5 : i / (count - 1);
                    const targetX = bandStart + xFraction * bandWidth;
                    const staggerY = useStagger ? (i % 2 === 0 ? -10 : 10) : 0;
                    const targetY = y + staggerY;

                    const prev = existing.get(node.node_id);
                    const status = node.status === 'complete' ? 'complete'
                        : node.status === 'pending' ? 'inflight'
                        : node.status === 'superseded' ? 'complete'
                        : 'pending';

                    newSpatial.push({
                        id: node.node_id,
                        depth,
                        headline: node.headline,
                        parentId: node.parent_id,
                        children: node.children,
                        status: status as SpatialNode['status'],
                        x: prev?.x ?? targetX,
                        y: prev?.y ?? targetY,
                        targetX,
                        targetY,
                        radius,
                    });
                }
            }
        }

        spatialNodesRef.current = newSpatial;
    }, [nodes, width, height]);

    // ── Animation loop ──────────────────────────────────────────────────
    const draw = useCallback(() => {
        const canvas = canvasRef.current;
        if (!canvas) return;
        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        const spatial = spatialNodesRef.current;
        pulseRef.current += 0.05;
        const pulse = Math.sin(pulseRef.current) * 0.3 + 0.7;

        // Animate positions (spring toward target)
        let needsRedraw = false;
        for (const node of spatial) {
            const dx = node.targetX - node.x;
            const dy = node.targetY - node.y;
            if (Math.abs(dx) > 0.5 || Math.abs(dy) > 0.5) {
                node.x += dx * 0.15;
                node.y += dy * 0.15;
                needsRedraw = true;
            } else {
                node.x = node.targetX;
                node.y = node.targetY;
            }
        }

        // Clear
        ctx.clearRect(0, 0, width, height);

        // Build lookup for edges. For summary rects (L0 grouped into rects of 10),
        // the spatial node's children array contains the node IDs IT represents.
        // Build a reverse index: any child node_id → the spatial rect that owns it.
        const nodeMap = new Map(spatial.map(n => [n.id, n]));
        const ownerMap = new Map<string, SpatialNode>();
        for (const n of spatial) {
            if (n.children && n.children.length > 0) {
                for (const childId of n.children) {
                    // Only map L0 children owned by summary rects at depth 0.
                    // L1+ nodes' children arrays are evidence links (handled below),
                    // not group ownership — don't map those.
                    if (n.depth === 0) {
                        ownerMap.set(childId, n);
                    }
                }
            }
        }

        // Draw edges: use parentId (mechanical) AND children arrays (evidence-based).
        // Question pyramids store L0→L1 connections in children[], not parent_id.
        ctx.lineWidth = 1;
        const drawnEdges = new Set<string>();
        for (const node of spatial) {
            // Parent-based edges (L1→L2, L2→L3)
            if (node.parentId) {
                const parent = nodeMap.get(node.parentId);
                if (parent) {
                    const edgeKey = `${node.id}-${parent.id}`;
                    if (!drawnEdges.has(edgeKey)) {
                        drawnEdges.add(edgeKey);
                        ctx.strokeStyle = node.status === 'complete' ? COLORS.edgeActive : COLORS.edge;
                        ctx.beginPath();
                        const cpY = (node.y + parent.y) / 2;
                        ctx.moveTo(parent.x, parent.y);
                        ctx.quadraticCurveTo(parent.x, cpY, node.x, node.y);
                        ctx.stroke();
                    }
                }
            }
            // Children-based edges (L1→L0 via children array).
            // For L1+ nodes, children[] lists L0 evidence node IDs. When L0 is in
            // summary-rect mode, resolve each child to its owning rect via ownerMap,
            // then direct lookup via nodeMap as a fallback.
            if (node.depth > 0 && node.children) {
                for (const childId of node.children) {
                    const child = ownerMap.get(childId) ?? nodeMap.get(childId);
                    if (child) {
                        const edgeKey = `${child.id}-${node.id}`;
                        if (!drawnEdges.has(edgeKey)) {
                            drawnEdges.add(edgeKey);
                            ctx.strokeStyle = child.status === 'complete' ? COLORS.edgeActive : COLORS.edge;
                            ctx.beginPath();
                            const cpY = (child.y + node.y) / 2;
                            ctx.moveTo(node.x, node.y);
                            ctx.quadraticCurveTo(node.x, cpY, child.x, child.y);
                            ctx.stroke();
                        }
                    }
                }
            }
        }

        // Draw nodes
        for (const node of spatial) {
            const isHovered = hoveredNode === node.id;
            const r = isHovered ? node.radius * 1.4 : node.radius;

            ctx.beginPath();
            ctx.arc(node.x, node.y, r, 0, Math.PI * 2);

            switch (node.status) {
                case 'complete':
                    ctx.fillStyle = COLORS.nodeComplete;
                    ctx.fill();
                    break;
                case 'inflight':
                    ctx.strokeStyle = COLORS.nodeInflight;
                    ctx.lineWidth = 2;
                    ctx.globalAlpha = pulse;
                    ctx.stroke();
                    ctx.globalAlpha = 1;
                    break;
                case 'failed':
                    ctx.fillStyle = COLORS.nodeFailed;
                    ctx.fill();
                    // Red X
                    ctx.strokeStyle = '#fff';
                    ctx.lineWidth = 2;
                    ctx.beginPath();
                    ctx.moveTo(node.x - 4, node.y - 4);
                    ctx.lineTo(node.x + 4, node.y + 4);
                    ctx.moveTo(node.x + 4, node.y - 4);
                    ctx.lineTo(node.x - 4, node.y + 4);
                    ctx.stroke();
                    break;
                default: // pending
                    ctx.fillStyle = COLORS.nodePending;
                    ctx.fill();
            }

            // Label for hovered node
            if (isHovered && node.headline) {
                ctx.font = LABEL_FONT;
                ctx.fillStyle = '#fff';
                ctx.textAlign = 'center';
                const label = node.headline.length > 40 ? node.headline.slice(0, 37) + '...' : node.headline;
                ctx.fillText(label, node.x, node.y - r - 6);
            }
        }

        // Layer labels
        const depths = [...new Set(spatial.map(n => n.depth))].sort((a, b) => a - b);
        ctx.font = LAYER_LABEL_FONT;
        ctx.fillStyle = COLORS.layerLabel;
        ctx.textAlign = 'left';
        for (const d of depths) {
            const firstNode = spatial.find(n => n.depth === d);
            if (firstNode) {
                ctx.fillText(`L${d}`, 10, firstNode.y + 4);
            }
        }

        // Continue animation if active or positions still settling
        if (isActive || needsRedraw) {
            animFrameRef.current = requestAnimationFrame(draw);
        }
    }, [width, height, isActive, hoveredNode]);

    useEffect(() => {
        animFrameRef.current = requestAnimationFrame(draw);
        return () => cancelAnimationFrame(animFrameRef.current);
    }, [draw]);

    // Trigger redraw when nodes change
    useEffect(() => {
        animFrameRef.current = requestAnimationFrame(draw);
    }, [nodes, draw]);

    // ── Hit testing ─────────────────────────────────────────────────────
    const handleMouseMove = useCallback((e: React.MouseEvent) => {
        const canvas = canvasRef.current;
        if (!canvas) return;
        const rect = canvas.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;

        let found: string | null = null;
        for (const node of spatialNodesRef.current) {
            const dx = mx - node.x;
            const dy = my - node.y;
            if (dx * dx + dy * dy <= (node.radius + 4) * (node.radius + 4)) {
                found = node.id;
                break;
            }
        }
        setHoveredNode(found);
        if (canvas) canvas.style.cursor = found ? 'pointer' : 'default';
    }, []);

    const handleClick = useCallback((e: React.MouseEvent) => {
        const canvas = canvasRef.current;
        if (!canvas) return;
        const rect = canvas.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;

        for (const node of spatialNodesRef.current) {
            const dx = mx - node.x;
            const dy = my - node.y;
            if (dx * dx + dy * dy <= (node.radius + 4) * (node.radius + 4)) {
                onNodeClick(node.id);
                return;
            }
        }
    }, [onNodeClick]);

    return (
        <div className="theatre-stage" ref={containerRef}>
            <canvas
                ref={canvasRef}
                onMouseMove={handleMouseMove}
                onClick={handleClick}
                onMouseLeave={() => setHoveredNode(null)}
            />
            {nodes.length === 0 && (
                <div className="theatre-stage-waiting">
                    {currentStep ? `${currentStep.replace(/_/g, ' ')}...` : 'Waiting for build to start...'}
                </div>
            )}
            {hoveredNode && (() => {
                const node = spatialNodesRef.current.find(n => n.id === hoveredNode);
                if (!node || !node.headline) return null;
                return (
                    <div
                        className="theatre-tooltip"
                        style={{
                            left: node.x,
                            top: node.y - node.radius - 30,
                        }}
                    >
                        {node.headline}
                    </div>
                );
            })()}
        </div>
    );
}
