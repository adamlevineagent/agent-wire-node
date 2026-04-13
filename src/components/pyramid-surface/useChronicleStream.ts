/**
 * Phase 4 — Chronicle event stream hook.
 *
 * Subscribes to `cross-build-event` Tauri channel (same pattern as
 * useStepTimeline) and categorises every TaggedKind event into a
 * ChronicleEntry suitable for the Chronicle list and EventTicker.
 *
 * Independent of any UI component — both Chronicle.tsx and
 * EventTicker.tsx consume the same entries array.
 */

import { useState, useEffect, useCallback } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { TaggedBuildEvent, TaggedKind, KnownTaggedKind } from '../../hooks/useBuildRowState';

// ── Public types ────────────────────────────────────────────────────

export interface ChronicleEntry {
    id: string;
    timestamp: number;
    kind: 'decision' | 'mechanical';
    category: string;
    headline: string;
    detail?: string;
    nodeId?: string;
    stepName?: string;
}

export interface ChronicleStreamResult {
    entries: ChronicleEntry[];
    /** Monotonic counter — increments on every appended entry, even when
     *  the bounded buffer wraps and entries.length stays at MAX_ENTRIES. */
    generation: number;
    clear: () => void;
}

// ── Constants ───────────────────────────────────────────────────────

const MAX_ENTRIES = 500;

// ── Mapping helpers ─────────────────────────────────────────────────

let _nextId = 0;
function nextId(): string {
    return `ce-${++_nextId}-${Date.now()}`;
}

function mapEvent(event: TaggedKind): ChronicleEntry | null {
    const known = event as KnownTaggedKind;
    const ts = Date.now();

    switch (known.type) {
        // ── Decision events ─────────────────────────────────────────
        case 'verdict_produced': {
            const weight = known.weight != null ? ` (w=${Number(known.weight).toFixed(2)})` : '';
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'decision',
                category: 'verdict',
                headline: `${known.verdict} ${known.source_id} \u2192 ${known.node_id}${weight}`,
                detail: `Verdict: ${known.verdict} linking source ${known.source_id} to node ${known.node_id}${weight}. Step: ${known.step_name}`,
                nodeId: known.node_id,
                stepName: known.step_name,
            };
        }
        case 'triage_decision': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'decision',
                category: 'triage',
                headline: `Triage: ${known.decision} ${known.item_id.slice(0, 12)}`,
                detail: `Decision: ${known.decision} for item ${known.item_id}. Reason: ${known.reason}`,
                stepName: known.step_name,
            };
        }
        case 'reconciliation_emitted': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'decision',
                category: 'reconciliation',
                headline: `Reconciliation: ${known.orphan_count} orphans, ${known.central_count} central`,
                detail: `Graph reconciliation found ${known.orphan_count} orphan nodes and ${known.central_count} central nodes.`,
            };
        }
        case 'gap_processing': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'decision',
                category: 'gap',
                headline: `Gap ${known.action}: ${known.gap_count} gaps at L${known.depth}`,
                detail: `Gap processing (${known.action}): ${known.gap_count} gaps at depth ${known.depth}. Step: ${known.step_name}`,
                stepName: known.step_name,
            };
        }
        case 'evidence_processing': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'decision',
                category: 'evidence',
                headline: `Evidence: ${known.question_count} questions (${known.model_tier})`,
                detail: `Evidence processing: ${known.action} on ${known.question_count} questions using ${known.model_tier} tier. Step: ${known.step_name}`,
                stepName: known.step_name,
            };
        }

        // ── Mechanical events ───────────────────────────────────────
        case 'cache_hit': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'cache',
                headline: `Cache hit: ${known.step_name}`,
                stepName: known.step_name,
            };
        }
        case 'chain_step_started': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'step',
                headline: `Step started: ${known.step_name}`,
                stepName: known.step_name,
            };
        }
        case 'chain_step_finished': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'step',
                headline: `Step finished: ${known.step_name} (${known.status}, ${known.elapsed_seconds.toFixed(1)}s)`,
                stepName: known.step_name,
            };
        }
        case 'web_edge_started': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'edge',
                headline: `Webbing: ${known.source_node_count} source nodes`,
                stepName: known.step_name,
            };
        }
        case 'web_edge_completed': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'edge',
                headline: `Webbing complete: ${known.edges_created} edges (${known.latency_ms}ms)`,
                stepName: known.step_name,
            };
        }
        case 'node_skipped': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'skip',
                headline: `Skipped: ${known.node_id} (${known.reason})`,
                nodeId: known.node_id,
                stepName: known.step_name,
            };
        }
        case 'cost_update': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'cost',
                headline: `Cost: $${known.cost_so_far_usd.toFixed(4)}`,
            };
        }
        case 'llm_call_completed': {
            return {
                id: nextId(),
                timestamp: ts,
                kind: 'mechanical',
                category: 'llm',
                headline: `LLM: ${known.model_id} ${known.tokens_prompt + known.tokens_completion}tok ${known.latency_ms}ms`,
                stepName: known.step_name,
            };
        }
        default:
            return null;
    }
}

// ── Hook ────────────────────────────────────────────────────────────

export function useChronicleStream(slug: string): ChronicleStreamResult {
    const [entries, setEntries] = useState<ChronicleEntry[]>([]);
    const [generation, setGeneration] = useState(0);

    // Clear resets the entry buffer and generation.
    const clear = useCallback(() => {
        setEntries([]);
        setGeneration(0);
    }, []);

    // Reset on slug change.
    useEffect(() => {
        setEntries([]);
        setGeneration(0);
    }, [slug]);

    // Subscribe to cross-build-event, filter by slug, map to
    // ChronicleEntry, and append to the bounded array.
    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        let active = true;

        (async () => {
            try {
                unlisten = await listen<TaggedBuildEvent>('cross-build-event', (ev) => {
                    if (!active) return;
                    const payload = ev.payload;
                    if (!payload || payload.slug !== slug) return;
                    // Defensive: exclude __ollama__ pull-progress events
                    // (matches convention in useCrossPyramidTimeline and
                    // usePyramidData).
                    if (payload.slug === '__ollama__') return;

                    const entry = mapEvent(payload.kind);
                    if (!entry) return;

                    setEntries((prev) => {
                        const next = [...prev, entry];
                        // Bound to MAX_ENTRIES — drop oldest when full.
                        if (next.length > MAX_ENTRIES) {
                            return next.slice(next.length - MAX_ENTRIES);
                        }
                        return next;
                    });
                    // Monotonic counter so consumers detect new entries
                    // even when the bounded buffer wraps at MAX_ENTRIES.
                    setGeneration((g) => g + 1);
                });
            } catch (e) {
                console.warn('useChronicleStream: listen failed', e);
            }
        })();

        return () => {
            active = false;
            if (unlisten) unlisten();
        };
    }, [slug]);

    return { entries, generation, clear };
}
