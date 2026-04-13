/**
 * Phase 4 — Chronicle view.
 *
 * Full scrollable list of build operations, split into decision
 * (expandable) and mechanical (compact single-line) entries.
 * Auto-scrolls to bottom during active builds; shows a "Jump to
 * latest" button when the user scrolls up.
 */

import { useState, useEffect, useRef, useCallback } from 'react';
import type { ChronicleEntry } from './useChronicleStream';

// ── Category badge labels ───────────────────────────────────────────

const CATEGORY_LABELS: Record<string, string> = {
    verdict: 'VRD',
    triage: 'TRI',
    gap: 'GAP',
    reconciliation: 'REC',
    evidence: 'EVD',
    cache: 'CHE',
    step: 'STP',
    edge: 'EDG',
    skip: 'SKP',
    cost: 'CST',
    llm: 'LLM',
};

// ── Relative timestamp formatting ───────────────────────────────────

function relativeTimestamp(entry: ChronicleEntry, firstTs: number): string {
    const delta = (entry.timestamp - firstTs) / 1000;
    if (delta < 60) return `+${delta.toFixed(1)}s`;
    const mins = Math.floor(delta / 60);
    const secs = delta % 60;
    return `+${mins}m${secs.toFixed(0)}s`;
}

// ── Props ───────────────────────────────────────────────────────────

interface ChronicleProps {
    slug: string;
    entries: ChronicleEntry[];
    /** Monotonic counter from useChronicleStream — detects new entries
     *  even when the bounded buffer wraps at MAX_ENTRIES. */
    generation: number;
    onArtifactClick?: (nodeId: string) => void;
    /** When false (default), hide mechanical ops (cache hits, step starts).
     *  Driven by viz config `chronicle.show_mechanical_ops`. */
    showMechanicalOps?: boolean;
    /** When true (default), decision entries start expanded.
     *  Driven by viz config `chronicle.auto_expand_decisions`. */
    autoExpandDecisions?: boolean;
}

// ── Component ───────────────────────────────────────────────────────

export function Chronicle({
    slug: _slug,
    entries,
    generation,
    onArtifactClick,
    showMechanicalOps = false,
    autoExpandDecisions = true,
}: ChronicleProps) {
    const listRef = useRef<HTMLDivElement>(null);
    const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
    const [autoScroll, setAutoScroll] = useState(true);
    const prevGenRef = useRef(0);

    // Filter entries based on showMechanicalOps setting.
    const visibleEntries = showMechanicalOps
        ? entries
        : entries.filter((e) => e.kind === 'decision');

    // Toggle expansion for decision entries.
    const toggleExpanded = useCallback((id: string) => {
        setExpandedIds((prev) => {
            const next = new Set(prev);
            if (next.has(id)) {
                next.delete(id);
            } else {
                next.add(id);
            }
            return next;
        });
    }, []);

    // Detect user scroll — disable auto-scroll when scrolled up.
    const handleScroll = useCallback(() => {
        const el = listRef.current;
        if (!el) return;
        const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        setAutoScroll(atBottom);
    }, []);

    // Auto-scroll to bottom when new entries arrive and autoScroll is on.
    // Uses `generation` (monotonic counter) instead of entries.length so
    // scroll still fires when the bounded buffer wraps at MAX_ENTRIES.
    useEffect(() => {
        if (!autoScroll) return;
        if (generation <= prevGenRef.current) {
            prevGenRef.current = generation;
            return;
        }
        prevGenRef.current = generation;
        const el = listRef.current;
        if (el) {
            el.scrollTop = el.scrollHeight;
        }
    }, [generation, autoScroll]);

    // Jump to latest.
    const jumpToLatest = useCallback(() => {
        const el = listRef.current;
        if (el) {
            el.scrollTop = el.scrollHeight;
        }
        setAutoScroll(true);
    }, []);

    const firstTs = visibleEntries.length > 0 ? visibleEntries[0].timestamp : Date.now();

    return (
        <div className="ps-chronicle">
            <div
                className="ps-chronicle-list"
                ref={listRef}
                onScroll={handleScroll}
            >
                {visibleEntries.length === 0 && (
                    <div className="ps-chronicle-empty">Awaiting events...</div>
                )}
                {visibleEntries.map((entry) => {
                    const isDecision = entry.kind === 'decision';
                    // When autoExpandDecisions is on, decisions start expanded
                    // unless the user explicitly collapsed them.
                    const isExpanded = isDecision && (
                        autoExpandDecisions
                            ? !expandedIds.has(entry.id)  // toggle set tracks collapsed ones
                            : expandedIds.has(entry.id)   // toggle set tracks expanded ones
                    );
                    const entryClass = isDecision
                        ? 'ps-chronicle-entry ps-chronicle-decision'
                        : 'ps-chronicle-entry ps-chronicle-mechanical';

                    return (
                        <div
                            key={entry.id}
                            className={entryClass}
                            onClick={isDecision ? () => toggleExpanded(entry.id) : undefined}
                        >
                            <span className="ps-chronicle-timestamp">
                                {relativeTimestamp(entry, firstTs)}
                            </span>
                            <span
                                className={`ps-chronicle-badge ps-chronicle-badge--${entry.kind}`}
                                title={entry.category}
                            >
                                {CATEGORY_LABELS[entry.category] ?? entry.category.slice(0, 3).toUpperCase()}
                            </span>
                            <span className="ps-chronicle-headline">
                                {entry.headline}
                                {entry.nodeId && onArtifactClick && (
                                    <button
                                        className="ps-chronicle-artifact-link"
                                        onClick={(e) => {
                                            e.stopPropagation();
                                            onArtifactClick(entry.nodeId!);
                                        }}
                                        title={`Go to ${entry.nodeId}`}
                                    >
                                        {entry.nodeId.slice(0, 10)}
                                    </button>
                                )}
                            </span>
                            {isExpanded && entry.detail && (
                                <div className="ps-chronicle-detail">
                                    {entry.detail}
                                </div>
                            )}
                        </div>
                    );
                })}
            </div>
            {!autoScroll && visibleEntries.length > 0 && (
                <button className="ps-chronicle-jump" onClick={jumpToLatest}>
                    Jump to latest
                </button>
            )}
        </div>
    );
}
