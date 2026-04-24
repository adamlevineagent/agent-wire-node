/**
 * Maps chain step definitions to viz primitives and exposes chain metadata.
 *
 * Eagerly loads the chain YAML via pyramid_get_build_chain so that
 * expectedMaxDepth is available for layout anchoring before builds start.
 * Also provides a step_name → viz primitive mapping that the CanvasRenderer
 * uses to decide how to visualize each build step.
 */

import { useState, useEffect, useMemo, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { VizPrimitive, VizStepConfig } from './types';

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
    viz?: Partial<VizStepConfig>;
}

interface ChainResponse {
    chain_id: string;
    content_type: string;
    /** Configured max pyramid depth for question/conversation builds; null for mechanical. */
    max_depth: number | null;
    chain: {
        steps?: ChainStep[];
        [key: string]: unknown;
    };
}

export interface VizMapping {
    /** Look up the full YAML viz metadata for a given step name */
    getVizConfig(stepName: string): VizStepConfig;
    /** Look up the viz primitive for a given step name */
    getVizPrimitive(stepName: string): VizPrimitive;
    /** Whether the chain has been loaded */
    loaded: boolean;
    /** Configured max depth for question/conversation builds, null for mechanical or unknown */
    expectedMaxDepth: number | null;
}

export function useVizMapping(slug: string, _isBuilding?: boolean): VizMapping {
    const [chainData, setChainData] = useState<ChainResponse | null>(null);

    // Always load chain definition for the slug so expectedMaxDepth is
    // available before a build starts. The IPC just reads slug info +
    // chain YAML — fast and safe to call any time.
    useEffect(() => {
        invoke<ChainResponse>('pyramid_get_build_chain', { slug })
            .then(setChainData)
            .catch(() => setChainData(null));
    }, [slug]);

    // Build the step→viz mapping
    const stepMap = useMemo(() => {
        const map = new Map<string, VizStepConfig>();
        if (!chainData?.chain?.steps) return map;

        for (const step of chainData.chain.steps) {
            // Explicit viz override in chain YAML takes precedence
            if (step.viz?.type) {
                map.set(step.name, { ...step.viz, type: step.viz.type });
                continue;
            }
            // Infer from primitive type
            const primitive = step.primitive ?? step.mode;
            if (primitive && PRIMITIVE_TO_VIZ[primitive]) {
                map.set(step.name, { type: PRIMITIVE_TO_VIZ[primitive] });
            } else {
                map.set(step.name, { type: 'progress_only' });
            }
        }
        return map;
    }, [chainData]);

    const getVizConfig = useCallback((stepName: string): VizStepConfig => {
        return stepMap.get(stepName) ?? { type: 'progress_only' };
    }, [stepMap]);

    const getVizPrimitive = useCallback((stepName: string): VizPrimitive => {
        return getVizConfig(stepName).type;
    }, [getVizConfig]);

    return {
        getVizConfig,
        getVizPrimitive,
        loaded: chainData !== null,
        expectedMaxDepth: chainData?.max_depth ?? null,
    };
}
