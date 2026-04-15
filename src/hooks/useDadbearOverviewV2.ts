// Phase 6 (Canonical) — Work-item-centric DADBEAR overview hook.
//
// Polls `pyramid_dadbear_overview_v2` on a configurable interval and
// exposes the work-item-centric per-pyramid status + global totals.
// Mirrors the polling pattern of the original `useDadbearOverview.ts`
// but returns the new canonical types (holds, pipeline counts, epochs).

import { useEffect, useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface WorkItemOverviewHold {
    hold: string;
    held_since: string;
    reason: string | null;
}

export interface WorkItemOverviewRow {
    slug: string;
    display_name: string;
    holds: WorkItemOverviewHold[];
    derived_status: 'active' | 'paused' | 'breaker' | 'held';
    epoch_id: string;
    recipe_version: string | null;
    // Pipeline counts
    pending_observations: number;
    compiled_items: number;
    blocked_items: number;
    previewed_items: number;
    dispatched_items: number;
    completed_items_24h: number;
    applied_items_24h: number;
    failed_items_24h: number;
    stale_items: number;
    // Cost
    preview_total_cost_usd: number;
    actual_cost_24h_usd: number;
    // Timing
    last_compilation_at: string | null;
    last_dispatch_at: string | null;
}

export interface WorkItemOverviewTotals {
    active_count: number;
    paused_count: number;
    breaker_count: number;
    total_compiled: number;
    total_dispatched: number;
    total_blocked: number;
    total_cost_24h_usd: number;
}

export interface WorkItemOverviewResponse {
    pyramids: WorkItemOverviewRow[];
    totals: WorkItemOverviewTotals;
}

export interface UseDadbearOverviewV2Result {
    data: WorkItemOverviewResponse | null;
    loading: boolean;
    error: string | null;
    refetch: () => Promise<void>;
}

const DEFAULT_POLL_MS = 10_000;

export function useDadbearOverviewV2(
    pollIntervalMs: number = DEFAULT_POLL_MS,
): UseDadbearOverviewV2Result {
    const [data, setData] = useState<WorkItemOverviewResponse | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const cancelledRef = useRef(false);

    const doFetch = useCallback(async () => {
        try {
            const result = await invoke<WorkItemOverviewResponse>(
                'pyramid_dadbear_overview_v2',
            );
            if (cancelledRef.current) return;
            setData(result);
            setError(null);
        } catch (e) {
            if (cancelledRef.current) return;
            setError(String(e));
        } finally {
            if (!cancelledRef.current) setLoading(false);
        }
    }, []);

    useEffect(() => {
        cancelledRef.current = false;
        doFetch();
        const interval = window.setInterval(doFetch, pollIntervalMs);
        return () => {
            cancelledRef.current = true;
            window.clearInterval(interval);
        };
    }, [doFetch, pollIntervalMs]);

    return { data, loading, error, refetch: doFetch };
}
