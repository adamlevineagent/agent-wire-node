/**
 * Three-axis visual encoding for pyramid nodes.
 *
 * Axis 1 (brightness): aggregate KEEP weight — how much is this node cited?
 * Axis 2 (saturation): propagated importance from apex — how close to what matters most?
 * Axis 3 (borderThickness): web edge count — how laterally connected?
 *
 * See docs/plans/pyramid-surface-visual-encoding.md for the full spec.
 */

import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { NodeEncoding } from './types';

// ── Types matching Rust VisualEncodingData ────────────────────────────

interface NodeEncodingRow {
    node_id: string;
    depth: number;
    aggregate_keep_weight: number;
    web_edge_count: number;
}

interface EvidenceLinkRow {
    source_id: string;
    target_id: string;
    weight: number;
}

interface VisualEncodingData {
    nodes: NodeEncodingRow[];
    evidence_links: EvidenceLinkRow[];
    apex_ids: string[];
}

// ── Power curve ramp ─────────────────────────────────────────────────

/** Apply power curve: most visual range in top quartile. Gamma 2.2. */
function powerCurve(value: number, gamma = 2.2): number {
    return Math.pow(Math.max(0, Math.min(1, value)), 1 / gamma);
}

/** Normalize a value into [0, 1] given the observed range. */
function normalize(value: number, min: number, max: number): number {
    if (max <= min) return 0;
    return (value - min) / (max - min);
}

// ── BFS Importance Propagation ───────────────────────────────────────

/**
 * Compute propagated importance via BFS from apex(es) downward.
 * Each apex starts at 1.0. Importance flows through KEEP evidence links,
 * attenuated by link weight at each hop.
 *
 * For multi-apex pyramids, importance accumulates (can exceed 1.0).
 * The caller normalizes after computation.
 */
function computePropagatedImportance(
    nodes: NodeEncodingRow[],
    evidenceLinks: EvidenceLinkRow[],
    apexIds: string[],
): Map<string, number> {
    const importance = new Map<string, number>();

    // Initialize all nodes to 0
    for (const node of nodes) {
        importance.set(node.node_id, 0);
    }

    // Apex nodes start at 1.0
    for (const apexId of apexIds) {
        importance.set(apexId, 1.0);
    }

    // Build adjacency: target_id → [{ source_id, weight }]
    // Evidence links go from source (lower layer) to target (higher layer, citer).
    // BFS walks DOWN from apex, so we need: for each citing node (target),
    // find what it cites (sources) and propagate importance downward.
    const citesDownward = new Map<string, { sourceId: string; weight: number }[]>();
    for (const link of evidenceLinks) {
        const list = citesDownward.get(link.target_id) ?? [];
        list.push({ sourceId: link.source_id, weight: link.weight });
        citesDownward.set(link.target_id, list);
    }

    // Sort nodes by depth descending (apex first) for top-down BFS
    const nodesByDepth = [...nodes].sort((a, b) => b.depth - a.depth);

    // Process each node in depth order: propagate its importance to its sources
    for (const node of nodesByDepth) {
        const nodeImportance = importance.get(node.node_id) ?? 0;
        if (nodeImportance === 0) continue;

        const sources = citesDownward.get(node.node_id);
        if (!sources) continue;

        for (const { sourceId, weight } of sources) {
            const current = importance.get(sourceId) ?? 0;
            importance.set(sourceId, current + nodeImportance * weight);
        }
    }

    return importance;
}

// ── Hook ─────────────────────────────────────────────────────────────

export interface VisualEncodingResult {
    /** Per-node encoding (brightness, saturation, borderThickness), all in [0, 1] */
    encodings: Map<string, NodeEncoding>;
    /** Per-link visual intensity: link_weight × upstream propagated importance */
    linkIntensities: Map<string, number>;
    /** Whether encoding data has been loaded */
    loaded: boolean;
}

export function useVisualEncoding(slug: string, enabled: boolean): VisualEncodingResult {
    const [data, setData] = useState<VisualEncodingData | null>(null);

    // Fetch encoding data
    useEffect(() => {
        if (!enabled || !slug) {
            setData(null);
            return;
        }
        invoke<VisualEncodingData>('pyramid_get_visual_encoding_data', { slug })
            .then(setData)
            .catch(() => setData(null));
    }, [slug, enabled]);

    // Compute encodings
    const result = useMemo((): VisualEncodingResult => {
        if (!data || data.nodes.length === 0) {
            return { encodings: new Map(), linkIntensities: new Map(), loaded: false };
        }

        // Compute propagated importance
        const propagated = computePropagatedImportance(
            data.nodes,
            data.evidence_links,
            data.apex_ids,
        );

        // Find ranges for normalization
        let minWeight = Infinity, maxWeight = -Infinity;
        let minPropagated = Infinity, maxPropagated = -Infinity;
        let minEdges = Infinity, maxEdges = -Infinity;

        for (const node of data.nodes) {
            const w = node.aggregate_keep_weight;
            const p = propagated.get(node.node_id) ?? 0;
            const e = node.web_edge_count;

            if (w < minWeight) minWeight = w;
            if (w > maxWeight) maxWeight = w;
            if (p < minPropagated) minPropagated = p;
            if (p > maxPropagated) maxPropagated = p;
            if (e < minEdges) minEdges = e;
            if (e > maxEdges) maxEdges = e;
        }

        // Build per-node encodings
        const encodings = new Map<string, NodeEncoding>();
        for (const node of data.nodes) {
            const rawBrightness = normalize(node.aggregate_keep_weight, minWeight, maxWeight);
            const rawSaturation = normalize(propagated.get(node.node_id) ?? 0, minPropagated, maxPropagated);
            const rawBorder = normalize(node.web_edge_count, minEdges, maxEdges);

            encodings.set(node.node_id, {
                brightness: powerCurve(rawBrightness),
                saturation: powerCurve(rawSaturation),
                borderThickness: powerCurve(rawBorder),
            });
        }

        // Compute per-link intensities
        const linkIntensities = new Map<string, number>();
        for (const link of data.evidence_links) {
            const upstreamImportance = propagated.get(link.target_id) ?? 0;
            const normalizedUpstream = normalize(upstreamImportance, minPropagated, maxPropagated);
            const intensity = link.weight * powerCurve(normalizedUpstream);
            linkIntensities.set(`${link.source_id}→${link.target_id}`, intensity);
        }

        return { encodings, linkIntensities, loaded: true };
    }, [data]);

    return result;
}
