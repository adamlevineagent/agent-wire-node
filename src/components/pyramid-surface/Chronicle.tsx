/**
 * Phase 4 — Chronicle view.
 *
 * Absolute-positioned bottom overlay showing build operations in
 * reverse chronological order (newest first). Entries are split into
 * decision (expandable) and mechanical (compact single-line) kinds.
 * Includes a header bar with close button for dismissal.
 */

import { useState, useRef, useCallback, useMemo } from 'react';
import type { ChronicleEntry } from './useChronicleStream';

// ── Category badge labels ───────────────────────────────────────────

const CATEGORY_LABELS: Record<string, string> = {
    node: 'NOD',
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
    retry: 'RTY',
    error: 'ERR',
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
    /** Close/dismiss the chronicle overlay. */
    onClose?: () => void;
    /** Active builds should show lifecycle/background rows, because those
     *  rows are the only honest signal while a long step is in flight. */
    isBuilding?: boolean;
    /** When false (default), hide background ops (cache hits, step starts).
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
    onClose,
    isBuilding = false,
    showMechanicalOps = false,
    autoExpandDecisions = true,
}: ChronicleProps) {
    const listRef = useRef<HTMLDivElement>(null);
    const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

    // Show all events during builds (mechanical ops are the main activity
    // during extraction/webbing). Filter only in post-build review mode.
    const hasAnyEntries = entries.length > 0;
    const allDecision = entries.filter((e) => e.kind === 'decision');
    const effectiveShowMechanical = isBuilding || showMechanicalOps || (hasAnyEntries && allDecision.length === 0);
    const visibleEntries = effectiveShowMechanical
        ? entries
        : allDecision;
    const hiddenCount = entries.length - visibleEntries.length;

    // Reverse chronological — newest first.
    const reversedEntries = useMemo(
        () => [...visibleEntries].reverse(),
        [visibleEntries],
    );

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

    // The first timestamp in the original (chronological) order, used for relative offsets.
    const firstTs = visibleEntries.length > 0 ? visibleEntries[0].timestamp : Date.now();

    // Suppress unused var warning — generation drives parent re-render
    // which feeds us new `entries`, so the list updates automatically.
    void generation;

    return (
        <div className="ps-chronicle">
            {/* Header bar with title and close button */}
            <div className="ps-chronicle-header">
                <span className="ps-chronicle-title">Chronicle</span>
                {hiddenCount > 0 && (
                    <span className="ps-chronicle-hidden-count">
                        {hiddenCount} background hidden
                    </span>
                )}
                {onClose && (
                    <button
                        className="ps-chronicle-close"
                        onClick={onClose}
                        title="Close chronicle"
                    >
                        &times;
                    </button>
                )}
            </div>
            <div
                className="ps-chronicle-list"
                ref={listRef}
            >
                {reversedEntries.length === 0 && hiddenCount === 0 && (
                    <div className="ps-chronicle-empty">Awaiting events...</div>
                )}
                {reversedEntries.length === 0 && hiddenCount > 0 && (
                    <div className="ps-chronicle-empty">
                        {hiddenCount} background event{hiddenCount !== 1 ? 's' : ''} hidden
                    </div>
                )}
                {reversedEntries.map((entry) => {
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
        </div>
    );
}
