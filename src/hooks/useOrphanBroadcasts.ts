// Phase 15 — Orphan Broadcasts hook for the DADBEAR Oversight Page.
//
// Wraps Phase 11's `pyramid_list_orphan_broadcasts` and Phase 15's
// `pyramid_acknowledge_orphan_broadcast`. Orphan broadcasts are the
// primary signal of credential exfiltration (per the spec Part 4),
// so the Oversight page surfaces unacknowledged rows with a red
// banner and per-row Acknowledge buttons.

import { useEffect, useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface OrphanBroadcastRow {
    id: number;
    received_at: string;
    provider_id: string | null;
    generation_id: string | null;
    session_id: string | null;
    pyramid_slug: string | null;
    build_id: string | null;
    step_name: string | null;
    model: string | null;
    cost_usd: number | null;
    tokens_in: number | null;
    tokens_out: number | null;
    acknowledged_at: string | null;
    acknowledgment_reason: string | null;
}

export interface UseOrphanBroadcastsResult {
    data: OrphanBroadcastRow[];
    loading: boolean;
    error: string | null;
    refetch: () => Promise<void>;
    acknowledge: (orphanId: number, reason?: string) => Promise<void>;
}

const DEFAULT_POLL_MS = 60_000;

export function useOrphanBroadcasts(
    pollIntervalMs: number = DEFAULT_POLL_MS,
    includeAcknowledged: boolean = false,
): UseOrphanBroadcastsResult {
    const [data, setData] = useState<OrphanBroadcastRow[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const cancelledRef = useRef(false);

    const doFetch = useCallback(async () => {
        try {
            const result = await invoke<OrphanBroadcastRow[]>(
                'pyramid_list_orphan_broadcasts',
                {
                    limit: 200,
                    includeAcknowledged,
                },
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
    }, [includeAcknowledged]);

    const acknowledge = useCallback(
        async (orphanId: number, reason?: string) => {
            try {
                await invoke('pyramid_acknowledge_orphan_broadcast', {
                    orphanId,
                    reason: reason ?? null,
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
