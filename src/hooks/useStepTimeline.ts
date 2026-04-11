// Phase 13 — per-pyramid step timeline hook.
//
// Thin wrapper around `useBuildRowState` that:
//   1. Seeds the initial state from `pyramid_step_cache_for_build`
//      on mount so a user opening a running build sees the
//      completed steps immediately instead of waiting for events.
//   2. Subscribes to the Tauri `build-event` channel (actually, the
//      shared `cross-build-event` fan-out) filtered by slug and
//      routes every matching event to the reducer.
//
// Returns the reduced state plus a forceReload hook the UI can
// call if the user pulls to refresh or a reroll lands.

import { useEffect, useRef, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import {
    BuildRowState,
    TaggedBuildEvent,
    TaggedKind,
    initialBuildRowState,
    reduceBuildRowEvent,
} from './useBuildRowState';

interface CacheEntrySummary {
    id: number;
    slug: string;
    build_id: string;
    step_name: string;
    chunk_index: number;
    depth: number;
    cache_key: string;
    model_id: string;
    cost_usd: number | null;
    latency_ms: number | null;
    created_at: string;
    force_fresh: boolean;
    supersedes_cache_id: number | null;
    note: string | null;
    invalidated_by: string | null;
}

/// Seed a fresh `BuildRowState` from a list of cache-entry summaries
/// returned by `pyramid_step_cache_for_build`. Each summary becomes
/// a synthetic completed-or-cached call on the matching step.
function seedStateFromCacheEntries(
    slug: string,
    entries: CacheEntrySummary[],
): BuildRowState {
    const base = initialBuildRowState(slug);
    const stepMap = new Map<string, ReturnType<typeof makeEmptyStep>>();

    function makeEmptyStep(stepName: string, depth: number) {
        return {
            stepName,
            primitive: '',
            modelTier: '',
            status: 'cached' as const,
            calls: [] as BuildRowState['steps'][number]['calls'],
            totalCostUsd: 0,
            totalTokensPrompt: 0,
            totalTokensCompletion: 0,
            cacheHits: 0,
            cacheMisses: 0,
            depth,
        };
    }

    let totalCost = 0;
    for (const entry of entries) {
        const key = entry.step_name;
        let step = stepMap.get(key);
        if (!step) {
            step = makeEmptyStep(entry.step_name, entry.depth);
            stepMap.set(key, step);
        }
        step.calls.push({
            cacheKey: entry.cache_key,
            status: entry.force_fresh ? 'completed' : 'cached',
            modelId: entry.model_id,
            costUsd: entry.cost_usd ?? 0,
            latencyMs: entry.latency_ms ?? 0,
        });
        if (entry.force_fresh) {
            step.cacheMisses += 1;
            step.totalCostUsd += entry.cost_usd ?? 0;
        } else {
            step.cacheHits += 1;
        }
        totalCost += entry.cost_usd ?? 0;
    }

    base.steps = Array.from(stepMap.values());
    // Re-derive step status from the seeded calls.
    for (const step of base.steps) {
        const allCached = step.calls.every(c => c.status === 'cached');
        step.status = allCached ? 'cached' : 'completed';
    }
    base.cost.estimatedUsd = totalCost;
    return base;
}

export function useStepTimeline(slug: string, buildId: string | null) {
    const [state, setState] = useState<BuildRowState>(() => initialBuildRowState(slug));
    const stateRef = useRef(state);
    stateRef.current = state;

    const refresh = useCallback(async () => {
        // Phase 13 verifier fix: allow null buildId — the backend
        // resolves the latest build for the slug when buildId is
        // absent. The previous implementation early-returned here
        // which silently bypassed the pre-populate.
        try {
            const entries = await invoke<CacheEntrySummary[]>(
                'pyramid_step_cache_for_build',
                { slug, buildId },
            );
            setState(seedStateFromCacheEntries(slug, entries));
        } catch (e) {
            // Swallow — if the pre-populate query fails we fall back
            // to live event reduction only. A hard failure here
            // shouldn't block the build viz from rendering.
            console.warn('useStepTimeline: cache seed failed', e);
        }
    }, [slug, buildId]);

    // Seed on mount / slug / build_id change.
    useEffect(() => {
        refresh();
    }, [refresh]);

    // Subscribe to the shared cross-build-event stream and filter
    // by slug. The backend emits via `cross-build-event` regardless
    // of which slug the event came from; we discard events for
    // other slugs here so the reducer only sees events for this row.
    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        let active = true;

        (async () => {
            try {
                unlisten = await listen<TaggedBuildEvent>('cross-build-event', (ev) => {
                    if (!active) return;
                    const payload = ev.payload;
                    if (!payload || payload.slug !== slug) return;
                    setState(prev => reduceBuildRowEvent(prev, payload.kind));
                });
            } catch (e) {
                console.warn('useStepTimeline: listen failed', e);
            }
        })();

        return () => {
            active = false;
            if (unlisten) unlisten();
        };
    }, [slug]);

    return { state, refresh };
}
