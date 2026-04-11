// Phase 15 — Provider Health hook for the DADBEAR Oversight Page.
//
// Wraps Phase 11's `pyramid_provider_health` IPC with a 30s poll.
// Shape mirrors the Rust `ProviderHealthEntry` struct in
// `src-tauri/src/pyramid/db.rs`.

import { useEffect, useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface ProviderHealthEntry {
    provider_id: string;
    display_name: string;
    provider_type: string;
    health: 'healthy' | 'degraded' | 'alerting' | string;
    reason: string | null;
    since: string | null;
    acknowledged_at: string | null;
    recent_discrepancies: number;
    recent_broadcast_missing: number;
    recent_orphans: number;
}

export interface UseProviderHealthResult {
    data: ProviderHealthEntry[];
    loading: boolean;
    error: string | null;
    refetch: () => Promise<void>;
    acknowledge: (providerId: string) => Promise<void>;
}

const DEFAULT_POLL_MS = 30_000;

export function useProviderHealth(
    pollIntervalMs: number = DEFAULT_POLL_MS,
): UseProviderHealthResult {
    const [data, setData] = useState<ProviderHealthEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const cancelledRef = useRef(false);

    const doFetch = useCallback(async () => {
        try {
            const result = await invoke<ProviderHealthEntry[]>(
                'pyramid_provider_health',
            );
            if (cancelledRef.current) return;
            setData(Array.isArray(result) ? result : []);
            setError(null);
        } catch (e) {
            if (cancelledRef.current) return;
            setError(String(e));
        } finally {
            if (!cancelledRef.current) setLoading(false);
        }
    }, []);

    const acknowledge = useCallback(
        async (providerId: string) => {
            try {
                await invoke('pyramid_acknowledge_provider_health', {
                    providerId,
                });
                await doFetch();
            } catch (e) {
                if (!cancelledRef.current) setError(String(e));
            }
        },
        [doFetch],
    );

    useEffect(() => {
        cancelledRef.current = false;
        doFetch();
        const interval = window.setInterval(doFetch, pollIntervalMs);
        return () => {
            cancelledRef.current = true;
            window.clearInterval(interval);
        };
    }, [doFetch, pollIntervalMs]);

    return { data, loading, error, refetch: doFetch, acknowledge };
}
