/**
 * useGridData — loads all pyramid summaries + tracks activity for the grid view.
 *
 * Fetches SlugInfo list, polls build status per slug,
 * and subscribes to cross-build-event for per-slug activity timestamps.
 */

import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { SlugInfo, ContentType } from '../pyramid-types';

// ── Public types ────────────────────────────────────────────────────

export interface GridPyramid {
    slug: string;
    contentType: ContentType;
    sourcePath: string;
    nodeCount: number;
    maxDepth: number;
    lastBuiltAt: string | null;
    createdAt: string;
    isBuilding: boolean;
    /** Timestamp (ms since epoch) of most recent build event — drives glow intensity */
    lastActivityMs: number;
}

export type GridSortKey = 'name' | 'activity' | 'nodeCount' | 'lastBuilt';

export interface UseGridDataResult {
    pyramids: GridPyramid[];
    loading: boolean;
    sortBy: GridSortKey;
    setSortBy: (key: GridSortKey) => void;
    refresh: () => void;
}

// ── Sort comparators ────────────────────────────────────────────────

function gridSortComparator(key: GridSortKey): (a: GridPyramid, b: GridPyramid) => number {
    switch (key) {
        case 'name':
            return (a, b) => a.slug.localeCompare(b.slug);
        case 'activity':
            return (a, b) => b.lastActivityMs - a.lastActivityMs;
        case 'nodeCount':
            return (a, b) => b.nodeCount - a.nodeCount;
        case 'lastBuilt':
            return (a, b) => {
                if (!a.lastBuiltAt && !b.lastBuiltAt) return 0;
                if (!a.lastBuiltAt) return 1;
                if (!b.lastBuiltAt) return -1;
                return new Date(b.lastBuiltAt).getTime() - new Date(a.lastBuiltAt).getTime();
            };
    }
}

// ── Hook ────────────────────────────────────────────────────────────

interface BuildStatusResponse {
    slug: string;
    status: string;
    progress: { done: number; total: number };
    elapsed_seconds: number;
    failures: number;
}

interface TaggedBuildEvent {
    slug: string;
    kind: Record<string, unknown> & { type: string };
}

export function useGridData(): UseGridDataResult {
    const [slugInfos, setSlugInfos] = useState<SlugInfo[]>([]);
    const [buildingSlugs, setBuildingSlugs] = useState<Set<string>>(new Set());
    const [activityMap, setActivityMap] = useState<Map<string, number>>(new Map());
    const [loading, setLoading] = useState(true);
    const [sortBy, setSortBy] = useState<GridSortKey>('activity');
    const [generation, setGeneration] = useState(0);

    const buildPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
    const glowTickRef = useRef<ReturnType<typeof setInterval> | null>(null);

    /**
     * Glow fade tick: bumps a counter every 2s when any slug has recent activity
     * (within the 60s glow window). This forces useMemo to recompute, which
     * causes PyramidCard to re-render with updated glowIntensity values.
     * Without this, glows would only update on unrelated re-renders.
     */
    const [glowTick, setGlowTick] = useState(0);

    useEffect(() => {
        const hasActiveGlow = () => {
            const now = Date.now();
            for (const ts of activityMap.values()) {
                if (ts > 0 && now - ts < 60_000) return true;
            }
            return false;
        };

        // Start ticking if any glow is active
        if (hasActiveGlow()) {
            if (!glowTickRef.current) {
                glowTickRef.current = setInterval(() => {
                    if (hasActiveGlow()) {
                        setGlowTick((t) => t + 1);
                    } else {
                        // All glows expired — stop ticking and do one final render
                        if (glowTickRef.current) clearInterval(glowTickRef.current);
                        glowTickRef.current = null;
                        setGlowTick((t) => t + 1);
                    }
                }, 2_000);
            }
        }

        return () => {
            if (glowTickRef.current) clearInterval(glowTickRef.current);
            glowTickRef.current = null;
        };
    }, [activityMap]);

    // ── Load slug list ───────────────────────────────────────────────
    const loadData = useCallback(async () => {
        setLoading(true);
        try {
            const slugs = await invoke<SlugInfo[]>('pyramid_list_slugs');
            setSlugInfos(slugs);
        } catch {
            // IPC failure — show empty state
        } finally {
            setLoading(false);
        }
    }, []);

    // Initial load + reload on generation bump
    useEffect(() => {
        loadData();
    }, [loadData, generation]);

    const refresh = useCallback(() => {
        setGeneration((g) => g + 1);
    }, []);

    // ── Poll build status per slug every 10s ─────────────────────────
    useEffect(() => {
        if (slugInfos.length === 0) return;

        const pollBuilds = async () => {
            try {
                const results = await Promise.allSettled(
                    slugInfos.map((s) =>
                        invoke<BuildStatusResponse>('pyramid_build_status', { slug: s.slug }),
                    ),
                );
                const building = new Set<string>();
                for (let i = 0; i < results.length; i++) {
                    const result = results[i];
                    if (result.status === 'fulfilled' && result.value.status === 'running') {
                        building.add(slugInfos[i].slug);
                    }
                }
                setBuildingSlugs(building);
            } catch {
                // ignore
            }
        };

        pollBuilds();
        buildPollRef.current = setInterval(pollBuilds, 10_000);
        return () => {
            if (buildPollRef.current) clearInterval(buildPollRef.current);
        };
    }, [slugInfos]);

    // ── Subscribe to cross-build-event for activity timestamps ───────
    useEffect(() => {
        const unlisten = listen<TaggedBuildEvent>('cross-build-event', (ev) => {
            const slug = ev.payload.slug;
            if (!slug || slug === '__ollama__') return;

            setActivityMap((prev) => {
                const next = new Map(prev);
                next.set(slug, Date.now());
                return next;
            });
        });

        return () => {
            unlisten.then((fn) => fn());
        };
    }, []);

    // ── Merge + sort into GridPyramid[] ──────────────────────────────
    // glowTick is included as a dependency to force recomputation during glow fade.
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    const pyramids = useMemo(() => {
        const merged: GridPyramid[] = slugInfos
            .filter((s) => !s.archived_at)
            .map((s) => ({
                slug: s.slug,
                contentType: s.content_type,
                sourcePath: s.source_path,
                nodeCount: s.node_count,
                maxDepth: s.max_depth,
                lastBuiltAt: s.last_built_at,
                createdAt: s.created_at,
                isBuilding: buildingSlugs.has(s.slug),
                lastActivityMs: activityMap.get(s.slug) ?? 0,
            }));

        return merged.sort(gridSortComparator(sortBy));
    }, [slugInfos, buildingSlugs, activityMap, sortBy, glowTick]);

    return { pyramids, loading, sortBy, setSortBy, refresh };
}
