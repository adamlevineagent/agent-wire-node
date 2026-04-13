/**
 * Maps chain step definitions to viz primitives.
 *
 * Loads the chain YAML at build start via pyramid_get_build_chain,
 * then provides a step_name → viz primitive mapping. The CanvasRenderer
 * uses this to decide how to visualize each build step.
 */

import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';

/** Viz primitive types from AD-1 */
export type VizPrimitive =
    | 'node_fill'      // Dots appearing in a layer band
    | 'edge_draw'      // Lines forming between nodes
    | 'cluster_form'   // Nodes grouping, parent appearing
    | 'verdict_mark'   // KEEP/DISCONNECT/MISSING indicators
    | 'progress_only'; // Status text, no structural change

/** Default primitive inference from chain step primitive type */
const PRIMITIVE_TO_VIZ: Record<string, VizPrimitive> = {
    for_each: 'node_fill',
    pair_adjacent: 'node_fill',
    single: 'node_fill',
    recursive_cluster: 'cluster_form',
    recursive_pair: 'cluster_form',
    web: 'edge_draw',
    evidence_loop: 'verdict_mark',
    // Recipe primitives → progress only
    build_lifecycle: 'progress_only',
    cross_build_input: 'progress_only',
    process_gaps: 'progress_only',
    recursive_decompose: 'progress_only',
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
