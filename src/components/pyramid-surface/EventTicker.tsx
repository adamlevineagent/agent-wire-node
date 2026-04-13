/**
 * Phase 4 — Event Ticker.
 *
 * Single-line headline scroll along the bottom of the pyramid surface.
 * Shows the most recent ChronicleEntry headline with a category badge.
 * Expands to show detail after 2s of no new entries.
 */

import { useState, useEffect, useRef } from 'react';
import type { ChronicleEntry } from './useChronicleStream';

// ── Props ───────────────────────────────────────────────────────────

interface EventTickerProps {
    entries: ChronicleEntry[];
    /** Monotonic counter from useChronicleStream — detects new entries
     *  even when the bounded buffer wraps at MAX_ENTRIES. */
    generation: number;
    onEntryClick?: (entry: ChronicleEntry) => void;
}

// ── Component ───────────────────────────────────────────────────────

export function EventTicker({ entries, generation, onEntryClick }: EventTickerProps) {
    const [expanded, setExpanded] = useState(false);
    const expandTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const prevGenRef = useRef(0);

    const latest = entries.length > 0 ? entries[entries.length - 1] : null;

    // When a new entry arrives, collapse and restart the 2s expand timer.
    // Uses `generation` (monotonic counter) instead of entries.length so
    // the timer still resets when the bounded buffer wraps at MAX_ENTRIES.
    useEffect(() => {
        if (generation <= prevGenRef.current) {
            prevGenRef.current = generation;
            return;
        }
        prevGenRef.current = generation;

        setExpanded(false);
        if (expandTimerRef.current) clearTimeout(expandTimerRef.current);
        expandTimerRef.current = setTimeout(() => {
            setExpanded(true);
        }, 2000);

        return () => {
            if (expandTimerRef.current) clearTimeout(expandTimerRef.current);
        };
    }, [generation]);

    if (!latest) return null;

    const badgeClass = latest.kind === 'decision'
        ? 'ps-ticker-badge ps-ticker-badge--decision'
        : 'ps-ticker-badge ps-ticker-badge--mechanical';

    const barClass = expanded && latest.detail
        ? 'ps-ticker-bar ps-ticker-expanded'
        : 'ps-ticker-bar';

    return (
        <div
            className={barClass}
            onClick={() => onEntryClick?.(latest)}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                    onEntryClick?.(latest);
                }
            }}
        >
            <span className={badgeClass}>
                {latest.kind === 'decision' ? 'DEC' : 'MEC'}
            </span>
            <span className="ps-ticker-headline">{latest.headline}</span>
            {expanded && latest.detail && (
                <span className="ps-ticker-detail">{latest.detail}</span>
            )}
        </div>
    );
}
