/**
 * Maps chain step definitions to viz primitives.
 *
 * Loads the chain YAML at build start via pyramid_get_build_chain,
 * then provides a step_name → viz primitive mapping. The CanvasRenderer
 * uses this to decide how to visualize each build step.
 */

import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { VizPrimitive } from './types';

/** Default primitive inference from chain step primitive type.
 *
 * Keys are the `primitive:` values from chain YAML files (e.g., extract,
 * web, evidence_loop). NOT the IR dispatch modes (for_each, pair_adjacent,
 * single) — those are internal to the Rust executor and never appear in
 * the chain YAML that the IPC returns. */
const PRIMITIVE_TO_VIZ: Record<string, VizPrimitive> = {
    // Node-producing primitives → dots appearing
    extract: 'node_fill',
    synthesize: 'node_fill',
    compress: 'node_fill',
    fuse: 'node_fill',
    // Edge-producing primitives → lines forming
    web: 'edge_draw',
    // Evidence primitives → verdict indicators
    evidence_loop: 'verdict_mark',
    // Clustering primitives → grouping visuals
    recursive_cluster: 'cluster_form',
    // Progress-only primitives (no structural viz)
    build_lifecycle: 'progress_only',
    cross_build_input: 'progress_only',
    process_gaps: 'progress_only',
    recursive_decompose: 'progress_only',
    classify: 'progress_only',
    container: 'progress_only',
    loop: 'progress_only',
    gate: 'progress_only',
};

interface ChainStep {
    name: string;
    primitive?: string;
    mode?: string;
    viz?: {
        type?: VizPrimitive;
        [key: string]: unknown;
    };
}

interface ChainResponse {
    chain_id: string;
    content_type: string;
    chain: {
        steps?: ChainStep[];
        [key: string]: unknown;
    };
}

export interface VizMapping {
    /** Look up the viz primitive for a given step name */
    getVizPrimitive(stepName: string): VizPrimitive;
    /** Whether the chain has been loaded */
    loaded: boolean;
}

export function useVizMapping(slug: string, isBuilding: boolean): VizMapping {
    const [chainData, setChainData] = useState<ChainResponse | null>(null);

    // Load chain definition when a build is active
    useEffect(() => {
        if (!isBuilding) return;
        invoke<ChainResponse>('pyramid_get_build_chain', { slug })
            .then(setChainData)
            .catch(() => setChainData(null));
    }, [slug, isBuilding]);

    // Build the step→viz mapping
    const stepMap = useMemo(() => {
        const map = new Map<string, VizPrimitive>();
        if (!chainData?.chain?.steps) return map;

        for (const step of chainData.chain.steps) {
            // Explicit viz override in chain YAML takes precedence
            if (step.viz?.type) {
                map.set(step.name, step.viz.type);
                continue;
            }
            // Infer from primitive type
            const primitive = step.primitive ?? step.mode;
            if (primitive && PRIMITIVE_TO_VIZ[primitive]) {
                map.set(step.name, PRIMITIVE_TO_VIZ[primitive]);
            } else {
                map.set(step.name, 'progress_only');
            }
        }
        return map;
    }, [chainData]);

    const getVizPrimitive = (stepName: string): VizPrimitive => {
        return stepMap.get(stepName) ?? 'progress_only';
    };

    return {
        getVizPrimitive,
        loaded: chainData !== null,
    };
}
