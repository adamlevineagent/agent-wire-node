// Phase 15 — DADBEAR Oversight Page hook.
//
// Polls `pyramid_dadbear_overview` on a configurable interval and
// exposes the aggregated per-pyramid status + global totals to the
// Oversight page. Data shape mirrors the Rust `DadbearOverviewResponse`
// struct in `src-tauri/src/main.rs`.

import { useEffect, useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface DadbearOverviewRow {
    slug: string;
    display_name: string;
    enabled: boolean;
    scan_interval_secs: number;
    debounce_secs: number;
    last_scan_at: string | null;
    next_scan_at: string | null;
    pending_mutations_count: number;
    in_flight_stale_checks: number;
    deferred_questions_count: number;
    demand_signals_24h: number;
    cost_24h_estimated_usd: number;
    cost_24h_actual_usd: number;
    cost_reconciliation_status:
        | 'healthy'
        | 'pending'
        | 'discrepancy'
        | 'broadcast_missing'
        | string;
    recent_manifest_count: number;
    frozen: boolean;
    breaker_tripped: boolean;
    auto_update: boolean;
}

export interface DadbearOverviewTotals {
    total_estimated_24h_usd: number;
    total_actual_24h_usd: number;
    total_pending_mutations: number;
    total_in_flight_checks: number;
    total_deferred_questions: number;
    paused_count: number;
    active_count: number;
    frozen_count: number;
    breaker_count: number;
}

export interface DadbearOverviewResponse {
    pyramids: DadbearOverviewRow[];
    totals: DadbearOverviewTotals;
}

export interface UseDadbearOverviewResult {
    data: DadbearOverviewResponse | null;
    loading: boolean;
    error: string | null;
    refetch: () => Promise<void>;
}

const DEFAULT_POLL_MS = 10_000;

export function useDadbearOverview(
    pollIntervalMs: number = DEFAULT_POLL_MS,
): UseDadbearOverviewResult {
    const [data, setData] = useState<DadbearOverviewResponse | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const cancelledRef = useRef(false);

    const doFetch = useCallback(async () => {
        try {
            const result = await invoke<DadbearOverviewResponse>(
                'pyramid_dadbear_overview',
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
