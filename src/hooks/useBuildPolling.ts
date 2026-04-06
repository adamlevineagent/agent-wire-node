import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { BuildStatus, BuildProgressV2, LiveNodeInfo } from '../components/theatre/types';

interface UseBuildPollingResult {
    status: BuildStatus | null;
    progress: BuildProgressV2 | null;
    liveNodes: LiveNodeInfo[] | null;
    isActive: boolean;
    error: string | null;
    cancel: () => Promise<void>;
    forceReset: () => Promise<void>;
}

export function useBuildPolling(slug: string): UseBuildPollingResult {
    const [status, setStatus] = useState<BuildStatus | null>(null);
    const [progress, setProgress] = useState<BuildProgressV2 | null>(null);
    const [liveNodes, setLiveNodes] = useState<LiveNodeInfo[] | null>(null);
    const [error, setError] = useState<string | null>(null);
    const activeRef = useRef(true);

    useEffect(() => {
        activeRef.current = true;
        let nodeTickCount = 0;

        const poll = async () => {
            while (activeRef.current) {
                try {
                    const promises: [Promise<BuildStatus>, Promise<BuildProgressV2 | null>] = [
                        invoke<BuildStatus>('pyramid_build_status', { slug }),
                        invoke<BuildProgressV2>('pyramid_build_progress_v2', { slug }).catch(() => null),
                    ];

                    // Poll live nodes every 3rd tick (~6s) during active build
                    const shouldPollNodes = nodeTickCount % 3 === 0;
                    const nodePromise = shouldPollNodes
                        ? invoke<LiveNodeInfo[]>('pyramid_build_live_nodes', { slug }).catch(() => null)
                        : Promise.resolve(null);

                    const [s, v2Data, nodes] = await Promise.all([...promises, nodePromise]);
                    if (!activeRef.current) break;

                    setStatus(s);
                    if (v2Data) setProgress(v2Data);
                    if (nodes) setLiveNodes(nodes);
                    nodeTickCount++;

                    if (['complete', 'complete_with_errors', 'failed', 'cancelled'].includes(s.status)) {
                        // Final node fetch on completion
                        const finalNodes = await invoke<LiveNodeInfo[]>('pyramid_build_live_nodes', { slug }).catch(() => null);
                        if (finalNodes) setLiveNodes(finalNodes);
                        break;
                    }

                    const isFinalizing =
                        s.status === 'running' &&
                        s.progress.total > 0 &&
                        s.progress.done >= s.progress.total;
                    await new Promise((r) => setTimeout(r, isFinalizing ? 500 : 2000));
                } catch (err) {
                    if (!activeRef.current) break;
                    setError(String(err));
                    break;
                }
            }
        };

        poll();
        return () => { activeRef.current = false; };
    }, [slug]);

    const isActive = status?.status === 'running';

    const cancel = useCallback(async () => {
        try { await invoke('pyramid_build_cancel', { slug }); } catch (err) { console.error('Cancel failed:', err); }
    }, [slug]);

    const forceReset = useCallback(async () => {
        try { await invoke('pyramid_build_force_reset', { slug }); } catch (err) { console.error('Force reset:', err); }
    }, [slug]);

    return { status, progress, liveNodes, isActive, error, cancel, forceReset };
}
