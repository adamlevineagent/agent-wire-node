// Phase 6c-C — React hook for reading the Wire node's vocabulary
// registry. Exposes the list of valid names + full entries for a
// vocab_kind (e.g. "annotation_type"). Polls on a 60s interval so new
// operator-published vocab entries surface in UI dropdowns without a
// page reload.
//
// Backed by the zero-auth HTTP endpoint `GET /vocabulary/:vocab_kind`
// that 6c-A shipped. If the Wire node is unreachable, the hook falls
// back to the minimal genesis set for annotation_type so the UI
// never renders an empty dropdown in the failure-mode case.

import { useEffect, useState, useCallback, useRef } from 'react';

export interface VocabEntry {
    name: string;
    description: string;
    handler_chain_id: string | null;
    reactive: boolean;
    creates_delta: boolean;
}

interface VocabListResponse {
    vocab_kind: string;
    entries: VocabEntry[];
}

export interface UseVocabularyResult {
    entries: VocabEntry[];
    names: string[];
    loading: boolean;
    error: string | null;
    refetch: () => Promise<void>;
    /** True when the displayed list is the hardcoded genesis fallback
     * because the fetch failed. UIs may want to surface a banner. */
    isFallback: boolean;
}

const FALLBACK_ANNOTATION_TYPES: VocabEntry[] = [
    { name: 'observation', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'correction', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: true },
    { name: 'question', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'friction', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'idea', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'era', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'transition', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'health_check', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'directory', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'steel_man', description: '(fallback)', handler_chain_id: null, reactive: true, creates_delta: false },
    { name: 'red_team', description: '(fallback)', handler_chain_id: null, reactive: true, creates_delta: false },
];

// Phase 6c-D: node_shape + role_name fallbacks mirror the Rust genesis seed
// in `vocab_genesis.rs`. Used for loading-state rendering when the network
// fetch hasn't completed yet.
const FALLBACK_NODE_SHAPES: VocabEntry[] = [
    { name: 'scaffolding', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'debate', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'meta_layer', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
    { name: 'gap', description: '(fallback)', handler_chain_id: null, reactive: false, creates_delta: false },
];

const FALLBACK_ROLE_NAMES: VocabEntry[] = [
    { name: 'accretion_handler', description: '(fallback)', handler_chain_id: 'starter-accretion-handler', reactive: false, creates_delta: false },
    { name: 'reconciler', description: '(fallback)', handler_chain_id: 'starter-reconciler', reactive: false, creates_delta: false },
    { name: 'evidence_tester', description: '(fallback)', handler_chain_id: 'starter-evidence-tester', reactive: false, creates_delta: false },
    { name: 'judge', description: '(fallback)', handler_chain_id: 'starter-judge', reactive: false, creates_delta: false },
    { name: 'debate_steward', description: '(fallback)', handler_chain_id: 'starter-debate-steward', reactive: false, creates_delta: false },
    { name: 'meta_layer_oracle', description: '(fallback)', handler_chain_id: 'starter-meta-layer-oracle', reactive: false, creates_delta: false },
    { name: 'synthesizer', description: '(fallback)', handler_chain_id: 'starter-synthesizer', reactive: false, creates_delta: false },
    { name: 'gap_dispatcher', description: '(fallback)', handler_chain_id: 'starter-gap-dispatcher', reactive: false, creates_delta: false },
    { name: 'sweep', description: '(fallback)', handler_chain_id: 'starter-sweep', reactive: false, creates_delta: false },
    { name: 'authorize_question', description: '(fallback)', handler_chain_id: 'starter-authorize-question', reactive: false, creates_delta: false },
    { name: 'cascade_handler', description: '(fallback)', handler_chain_id: 'starter-cascade-judge-gated', reactive: false, creates_delta: false },
];

const DEFAULT_POLL_MS = 60_000;
const WIRE_NODE_BASE_URL = 'http://localhost:8765';

function fallbackFor(vocabKind: string): VocabEntry[] {
    if (vocabKind === 'annotation_type') return FALLBACK_ANNOTATION_TYPES;
    if (vocabKind === 'node_shape') return FALLBACK_NODE_SHAPES;
    if (vocabKind === 'role_name') return FALLBACK_ROLE_NAMES;
    return [];
}

export function useVocabulary(
    vocabKind: string,
    pollIntervalMs: number = DEFAULT_POLL_MS,
): UseVocabularyResult {
    const [entries, setEntries] = useState<VocabEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [isFallback, setIsFallback] = useState(false);
    const cancelledRef = useRef(false);

    const doFetch = useCallback(async () => {
        try {
            const url = `${WIRE_NODE_BASE_URL}/vocabulary/${encodeURIComponent(vocabKind)}`;
            const resp = await fetch(url);
            if (!resp.ok) {
                if (cancelledRef.current) return;
                setEntries(fallbackFor(vocabKind));
                setIsFallback(true);
                setError(`status ${resp.status}`);
                return;
            }
            const body = (await resp.json()) as VocabListResponse;
            if (cancelledRef.current) return;
            setEntries(Array.isArray(body?.entries) ? body.entries : []);
            setIsFallback(false);
            setError(null);
        } catch (e) {
            if (cancelledRef.current) return;
            setEntries(fallbackFor(vocabKind));
            setIsFallback(true);
            setError(String(e));
        } finally {
            if (!cancelledRef.current) setLoading(false);
        }
    }, [vocabKind]);

    useEffect(() => {
        cancelledRef.current = false;
        doFetch();
        const interval = window.setInterval(doFetch, pollIntervalMs);
        return () => {
            cancelledRef.current = true;
            window.clearInterval(interval);
        };
    }, [doFetch, pollIntervalMs]);

    const names = entries.map((e) => e.name);

    return { entries, names, loading, error, refetch: doFetch, isFallback };
}

/** Convenience wrapper for the most common case — annotation_type. */
export function useAnnotationTypes(
    pollIntervalMs?: number,
): UseVocabularyResult {
    return useVocabulary('annotation_type', pollIntervalMs);
}

/** Phase 6c-D: convenience wrapper for node_shape. Returns the full list
 * of valid shapes (`scaffolding`, `debate`, `meta_layer`, `gap`, and
 * anything an operator has published). */
export function useNodeShapes(
    pollIntervalMs?: number,
): UseVocabularyResult {
    return useVocabulary('node_shape', pollIntervalMs);
}

/** Phase 6c-D: convenience wrapper for role_name. Returns the full list
 * of valid role names bound per-pyramid via `pyramid_role_bindings`
 * (Phase 1 genesis roles + cascade_handler + anything an operator has
 * published). */
export function useRoleNames(
    pollIntervalMs?: number,
): UseVocabularyResult {
    return useVocabulary('role_name', pollIntervalMs);
}
