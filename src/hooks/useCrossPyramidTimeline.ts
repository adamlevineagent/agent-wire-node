// Phase 13 — cross-pyramid timeline hook.
//
// Subscribes to the `cross-build-event` Tauri channel and routes
// every event into a per-slug `BuildRowState`, producing a
// `Map<slug, BuildRowState>` the frontend can render as a compact
// list of concurrent builds.
//
// On mount, the hook calls `pyramid_active_builds` to seed the
// initial map with active build summaries. Subsequent updates flow
// via live events.

import { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import {
    BuildRowState,
    TaggedBuildEvent,
    initialBuildRowState,
    reduceBuildRowEvent,
} from './useBuildRowState';

export interface ActiveBuildRow {
    slug: string;
    build_id: string;
    status: string;
    started_at: string;
    completed_steps: number;
    total_steps: number;
    current_step: string | null;
    cost_so_far_usd: number;
    cache_hit_rate: number;
}

export interface CrossPyramidState {
    byslug: Map<string, BuildRowState>;
    activeBuilds: ActiveBuildRow[];
}

export function useCrossPyramidTimeline() {
    const [state, setState] = useState<CrossPyramidState>({
        byslug: new Map(),
        activeBuilds: [],
    });

    const refreshActive = useCallback(async () => {
        try {
            const active = await invoke<ActiveBuildRow[]>('pyramid_active_builds');
            setState(prev => {
                const next: CrossPyramidState = {
                    byslug: new Map(prev.byslug),
                    activeBuilds: active,
                };
                for (const build of active) {
                    if (!next.byslug.has(build.slug)) {
                        next.byslug.set(build.slug, initialBuildRowState(build.slug));
                    }
                }
                return next;
            });
        } catch (e) {
            console.warn('useCrossPyramidTimeline: active builds fetch failed', e);
        }
    }, []);

    // Seed on mount.
    useEffect(() => {
        refreshActive();
    }, [refreshActive]);

    // Poll active builds every 30s as a safety net — the event
    // stream keeps state fresh in between.
    useEffect(() => {
        const t = setInterval(() => {
            refreshActive();
        }, 30_000);
        return () => clearInterval(t);
    }, [refreshActive]);

    // Subscribe to the shared cross-build-event channel and route
    // every event into the matching per-slug row.
    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        let active = true;

        (async () => {
            try {
                unlisten = await listen<TaggedBuildEvent>('cross-build-event', (ev) => {
                    if (!active) return;
                    const payload = ev.payload;
                    if (!payload) return;
                    setState(prev => {
                        const nextMap = new Map(prev.byslug);
                        const slugState =
                            nextMap.get(payload.slug) ?? initialBuildRowState(payload.slug);
                        nextMap.set(
                            payload.slug,
                            reduceBuildRowEvent(slugState, payload.kind),
                        );
                        return { ...prev, byslug: nextMap };
                    });
                });
            } catch (e) {
                console.warn('useCrossPyramidTimeline: listen failed', e);
            }
        })();

        return () => {
            active = false;
            if (unlisten) unlisten();
        };
    }, []);

    return { state, refreshActive };
}
