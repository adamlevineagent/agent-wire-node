/**
 * Force-directed "Density" layout for the Pyramid Surface.
 *
 * Proximity encodes relationship strength: highly-connected nodes
 * cluster together, central nodes grow larger.  Parameters come from
 * the viz config contribution (DensityConfig), never hardcoded.
 *
 * Standard tier: simulation runs to convergence synchronously on each
 * input change and returns a static result.  Rich-tier live rAF loop
 * is a future enhancement.
 */

import { useMemo } from 'react';
import type { SurfaceNode, SurfaceEdge, NodeEncoding } from './types';
import { EdgeCategory } from './types';

// ── Config interface (mirrors viz config contribution) ───────────────

export interface DensityConfig {
    repulsion: number | 'auto';
    attraction: number | 'auto';
    damping: number | 'auto';
    settle_threshold: number | 'auto';
    label_min_radius: number | 'auto';
    max_iterations: number | 'auto';
    center_gravity: number | 'auto';
}

// ── Internal simulation types ────────────────────────────────────────

interface SimNode {
    id: string;
    x: number;
    y: number;
    vx: number;
    vy: number;
    radius: number;
    mass: number;
}

interface SimEdge {
    fromIdx: number;
    toIdx: number;
    strength: number;
    category: EdgeCategory;
}

// ── Auto-derive helpers ──────────────────────────────────────────────

function resolveRepulsion(
    cfg: number | 'auto',
    nodeCount: number,
    canvasArea: number,
): number {
    if (cfg !== 'auto') return cfg;
    // Scale with sqrt(nodeCount) * canvasArea — more nodes need stronger
    // repulsion to prevent overlap, canvas area normalizes the force.
    return Math.sqrt(Math.max(nodeCount, 1)) * canvasArea * 0.00001;
}

function resolveAttraction(
    cfg: number | 'auto',
    avgEdgeCount: number,
): number {
    if (cfg !== 'auto') return cfg;
    // Inversely proportional to avg edge count so dense graphs don't
    // collapse into a singularity.
    return 0.05 / Math.max(avgEdgeCount, 0.5);
}

function resolveDamping(cfg: number | 'auto'): number {
    if (cfg !== 'auto') return cfg;
    return 0.9;
}

function resolveSettleThreshold(
    cfg: number | 'auto',
    avgRadius: number,
): number {
    if (cfg !== 'auto') return cfg;
    return 0.1 * Math.max(avgRadius, 1);
}

function resolveLabelMinRadius(
    cfg: number | 'auto',
    avgRadius: number,
): number {
    if (cfg !== 'auto') return cfg;
    // Show labels on nodes at least 1.8x the average size
    return avgRadius * 1.8;
}

function resolveMaxIterations(cfg: number | 'auto', nodeCount: number): number {
    if (cfg !== 'auto') return cfg;
    // Scale with node count: small graphs converge fast, large ones need more room.
    // Floor 100, ceiling 500, linear between 10 and 200 nodes.
    return Math.max(100, Math.min(500, Math.round(100 + (nodeCount / 200) * 400)));
}

function resolveCenterGravity(cfg: number | 'auto'): number {
    if (cfg !== 'auto') return cfg;
    return 0.01;
}

// ── Bezier control point (matches pyramid layout convention) ─────────

function controlPoint(
    fromX: number,
    fromY: number,
    toX: number,
    toY: number,
): { cx: number; cy: number } {
    return {
        cx: (fromX + toX) / 2,
        cy: (fromY + toY) / 2,
    };
}

// ── Simulation core ──────────────────────────────────────────────────

const MIN_DISTANCE = 1; // prevent division by zero

function runSimulation(
    simNodes: SimNode[],
    simEdges: SimEdge[],
    width: number,
    height: number,
    repulsion: number,
    attraction: number,
    damping: number,
    settleThreshold: number,
    maxIterations: number,
    centerGravity: number,
): boolean {
    const cx = width / 2;
    const cy = height / 2;
    const n = simNodes.length;
    let settled = false;

    for (let iter = 0; iter < maxIterations; iter++) {
        // Reset forces
        for (let i = 0; i < n; i++) {
            // We accumulate forces directly into velocity via Euler integration
        }

        // (a) Repulsion: all-pairs inverse square
        for (let i = 0; i < n; i++) {
            let fx = 0;
            let fy = 0;
            const ni = simNodes[i];

            for (let j = 0; j < n; j++) {
                if (i === j) continue;
                const nj = simNodes[j];
                let dx = ni.x - nj.x;
                let dy = ni.y - nj.y;
                let distSq = dx * dx + dy * dy;
                if (distSq < MIN_DISTANCE) distSq = MIN_DISTANCE;
                const dist = Math.sqrt(distSq);
                const force = repulsion / distSq;
                fx += (dx / dist) * force;
                fy += (dy / dist) * force;
            }

            // Apply repulsion (inversely scaled by mass so heavy nodes resist)
            ni.vx += fx / ni.mass;
            ni.vy += fy / ni.mass;
        }

        // (b) Attraction: edge-connected pairs pulled together
        for (const edge of simEdges) {
            if (edge.category !== EdgeCategory.WEB && edge.category !== EdgeCategory.EVIDENCE) {
                // Only web and evidence edges create attraction in density mode.
                // Structural edges are parent-child; they don't encode semantic
                // relationship strength the same way.
                continue;
            }
            const a = simNodes[edge.fromIdx];
            const b = simNodes[edge.toIdx];
            const dx = b.x - a.x;
            const dy = b.y - a.y;
            const dist = Math.sqrt(dx * dx + dy * dy) || MIN_DISTANCE;
            const force = attraction * edge.strength * dist;
            const fx = (dx / dist) * force;
            const fy = (dy / dist) * force;
            a.vx += fx / a.mass;
            a.vy += fy / a.mass;
            b.vx -= fx / b.mass;
            b.vy -= fy / b.mass;
        }

        // Also give structural edges a mild attraction to keep the graph
        // from fragmenting into disconnected islands.
        for (const edge of simEdges) {
            if (edge.category !== EdgeCategory.STRUCTURAL && edge.category !== EdgeCategory.BEDROCK) {
                continue;
            }
            const a = simNodes[edge.fromIdx];
            const b = simNodes[edge.toIdx];
            const dx = b.x - a.x;
            const dy = b.y - a.y;
            const dist = Math.sqrt(dx * dx + dy * dy) || MIN_DISTANCE;
            // Mild structural cohesion — 20% of the primary attraction
            const force = attraction * 0.2 * dist;
            const fx = (dx / dist) * force;
            const fy = (dy / dist) * force;
            a.vx += fx / a.mass;
            a.vy += fy / a.mass;
            b.vx -= fx / b.mass;
            b.vy -= fy / b.mass;
        }

        // (c) Center gravity — soft pull toward canvas center
        for (let i = 0; i < n; i++) {
            const ni = simNodes[i];
            ni.vx += (cx - ni.x) * centerGravity;
            ni.vy += (cy - ni.y) * centerGravity;
        }

        // (d) Apply damping + update positions
        let maxVelocity = 0;
        for (let i = 0; i < n; i++) {
            const ni = simNodes[i];
            ni.vx *= damping;
            ni.vy *= damping;
            ni.x += ni.vx;
            ni.y += ni.vy;

            // Clamp to canvas bounds (with radius padding)
            const pad = ni.radius + 4;
            ni.x = Math.max(pad, Math.min(width - pad, ni.x));
            ni.y = Math.max(pad, Math.min(height - pad, ni.y));

            const v = Math.sqrt(ni.vx * ni.vx + ni.vy * ni.vy);
            if (v > maxVelocity) maxVelocity = v;
        }

        // (e) Convergence check
        if (maxVelocity < settleThreshold) {
            settled = true;
            break;
        }
    }

    return settled;
}

// ── Hook ─────────────────────────────────────────────────────────────

export function useDensityLayout(
    nodes: SurfaceNode[],
    edges: SurfaceEdge[],
    width: number,
    height: number,
    encodings: Map<string, NodeEncoding>,
    densityConfig: DensityConfig,
    active: boolean,
): { nodes: SurfaceNode[]; edges: SurfaceEdge[]; settled: boolean; labelMinRadius: number } {
    return useMemo(() => {
        // When not in density mode, pass through unchanged
        if (!active || nodes.length === 0 || width === 0 || height === 0) {
            return {
                nodes,
                edges,
                settled: true,
                labelMinRadius: 0,
            };
        }

        // Build index: node id → array index
        const idToIdx = new Map<string, number>();
        for (let i = 0; i < nodes.length; i++) {
            idToIdx.set(nodes[i].id, i);
        }

        // Compute avg radius and avg edge count for auto-derivation
        let radiusSum = 0;
        for (const node of nodes) radiusSum += node.radius;
        const avgRadius = radiusSum / nodes.length;

        // Count web/evidence edges per node
        const edgeCountByNode = new Map<string, number>();
        for (const edge of edges) {
            if (edge.category === EdgeCategory.WEB || edge.category === EdgeCategory.EVIDENCE) {
                edgeCountByNode.set(edge.fromId, (edgeCountByNode.get(edge.fromId) ?? 0) + 1);
                edgeCountByNode.set(edge.toId, (edgeCountByNode.get(edge.toId) ?? 0) + 1);
            }
        }
        let totalEdges = 0;
        for (const c of edgeCountByNode.values()) totalEdges += c;
        const avgEdgeCount = nodes.length > 0 ? totalEdges / nodes.length : 0;

        // Resolve auto parameters
        const canvasArea = width * height;
        const repulsion = resolveRepulsion(densityConfig.repulsion, nodes.length, canvasArea);
        const attraction = resolveAttraction(densityConfig.attraction, avgEdgeCount);
        const damping = resolveDamping(densityConfig.damping);
        const settleThreshold = resolveSettleThreshold(densityConfig.settle_threshold, avgRadius);
        const labelMinRadius = resolveLabelMinRadius(densityConfig.label_min_radius, avgRadius);
        const maxIterations = resolveMaxIterations(densityConfig.max_iterations, nodes.length);
        const centerGravity = resolveCenterGravity(densityConfig.center_gravity);

        // Build simulation nodes: radius scaled by encoding brightness (centrality)
        const simNodes: SimNode[] = nodes.map((node) => {
            const enc = encodings.get(node.id);
            // Central nodes (high brightness) get larger radius
            const brightnessScale = enc ? (0.6 + enc.brightness * 1.4) : 1.0;
            const radius = node.radius * brightnessScale;
            // Mass proportional to encoding aggregate: heavier = resists movement
            const mass = enc ? (1.0 + (enc.brightness + enc.saturation) * 2.0) : 1.0;
            return {
                id: node.id,
                x: node.x,
                y: node.y,
                vx: 0,
                vy: 0,
                radius,
                mass,
            };
        });

        // Build simulation edges
        const simEdges: SimEdge[] = [];
        for (const edge of edges) {
            const fromIdx = idToIdx.get(edge.fromId);
            const toIdx = idToIdx.get(edge.toId);
            if (fromIdx === undefined || toIdx === undefined) continue;
            // Web/evidence edges use strength 1.0 — the attraction coefficient
            // already encodes the global pull strength.
            simEdges.push({
                fromIdx,
                toIdx,
                strength: 1.0,
                category: edge.category,
            });
        }

        // Run simulation
        const settled = runSimulation(
            simNodes,
            simEdges,
            width,
            height,
            repulsion,
            attraction,
            damping,
            settleThreshold,
            maxIterations,
            centerGravity,
        );

        // Map simulation positions back to SurfaceNodes
        const densityNodes: SurfaceNode[] = nodes.map((node, i) => ({
            ...node,
            x: simNodes[i].x,
            y: simNodes[i].y,
            radius: simNodes[i].radius,
        }));

        // Rebuild edges with new positions
        const posMap = new Map<string, { x: number; y: number }>();
        for (const sn of simNodes) posMap.set(sn.id, { x: sn.x, y: sn.y });

        const densityEdges: SurfaceEdge[] = edges.map((edge) => {
            const from = posMap.get(edge.fromId);
            const to = posMap.get(edge.toId);
            if (!from || !to) return edge;
            const cp = controlPoint(from.x, from.y, to.x, to.y);
            return {
                ...edge,
                fromX: from.x,
                fromY: from.y,
                toX: to.x,
                toY: to.y,
                controlX: cp.cx,
                controlY: cp.cy,
            };
        });

        return {
            nodes: densityNodes,
            edges: densityEdges,
            settled,
            labelMinRadius,
        };
    }, [nodes, edges, width, height, encodings, densityConfig, active]);
}
