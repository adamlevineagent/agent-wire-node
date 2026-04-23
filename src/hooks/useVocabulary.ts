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

const DEFAULT_POLL_MS = 60_000;
const WIRE_NODE_BASE_URL = 'http://localhost:8765';

function fallbackFor(vocabKind: string): VocabEntry[] {
    if (vocabKind === 'annotation_type') return FALLBACK_ANNOTATION_TYPES;
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
