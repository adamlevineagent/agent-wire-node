// src/components/MigrationPanel.tsx — Phase 18d: Schema Migration UI.
//
// Spec: docs/plans/phase-18d-workstream-prompt.md → "Frontend: ToolsMode
//       'Needs Migration' surface" section
//       docs/specs/generative-config-pattern.md → "Schema Definitions Are
//       Contributions" section (the migration flow described there)
//
// This panel renders the list of config contributions flagged with
// `needs_migration = 1` (the breadcrumb Phase 9 set when a
// schema_definition contribution superseded its prior version) and lets
// the user open the LLM-assisted migration review modal for any flagged
// row. It is mounted as a top-level tab in ToolsMode alongside My Tools,
// Discover, and Create.
//
// Architectural pattern: this is the read side of the migration flow.
// MigrationReviewModal is the write side (propose / accept / reject).

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { NeedsMigrationEntry } from "../types/configContributions";
import { MigrationReviewModal } from "./MigrationReviewModal";

interface MigrationPanelProps {
    /** Bumped from outside whenever the user accepts/rejects a
     *  migration so this panel re-fetches the latest list. */
    refreshToken?: number;
    /** Bump callback handed to the modal so accept/reject can refresh
     *  the parent's state alongside this panel. */
    onMigrationChanged?: () => void;
}

export function MigrationPanel({
    refreshToken,
    onMigrationChanged,
}: MigrationPanelProps) {
    const [entries, setEntries] = useState<NeedsMigrationEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [reviewing, setReviewing] = useState<NeedsMigrationEntry | null>(
        null,
    );

    // Local refresh token for accept/reject inside this panel — bumps
    // independently of the parent prop so the modal close handler can
    // trigger a re-fetch without leaving the tab.
    const [localToken, setLocalToken] = useState(0);
    const bumpLocal = useCallback(() => setLocalToken((n) => n + 1), []);

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        setError(null);

        invoke<NeedsMigrationEntry[]>("pyramid_list_configs_needing_migration")
            .then((rows) => {
                if (cancelled) return;
                setEntries(rows);
            })
            .catch((err) => {
                if (cancelled) return;
                console.warn(
                    "[MigrationPanel] list_configs_needing_migration failed:",
                    err,
                );
                setError(String(err));
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });

        return () => {
            cancelled = true;
        };
    }, [refreshToken, localToken]);

    const handleOpenReview = useCallback(
        (entry: NeedsMigrationEntry) => setReviewing(entry),
        [],
    );

    const handleCloseReview = useCallback(() => setReviewing(null), []);

    const handleMigrationChanged = useCallback(() => {
        bumpLocal();
        onMigrationChanged?.();
    }, [bumpLocal, onMigrationChanged]);

    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
            <header style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <h2
                    style={{
                        margin: 0,
                        fontSize: 18,
                        fontWeight: 700,
                        color: "var(--text-primary)",
                    }}
                >
                    Configs Needing Migration
                </h2>
                <p
                    style={{
                        margin: 0,
                        fontSize: 13,
                        color: "var(--text-secondary)",
                        lineHeight: 1.5,
                    }}
                >
                    These configurations were valid against an older schema. The
                    schema has since been refined — Wire Node can run an
                    LLM-assisted migration that preserves your settings while
                    making the YAML valid against the current schema. Every
                    proposed migration goes through your review before it's
                    applied.
                </p>
            </header>

            {loading && (
                <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                    Loading flagged configs…
                </p>
            )}

            {error && (
                <p
                    style={{
                        color: "#fca5a5",
                        fontSize: 13,
                        background: "rgba(248, 113, 113, 0.08)",
                        border: "1px solid rgba(248, 113, 113, 0.3)",
                        borderRadius: 6,
                        padding: 10,
                    }}
                >
                    {error}
                </p>
            )}

            {!loading && entries.length === 0 && !error && (
                <div
                    style={{
                        textAlign: "center",
                        padding: 32,
                        color: "var(--text-secondary)",
                        background: "rgba(255, 255, 255, 0.02)",
                        border: "1px dashed var(--border-primary)",
                        borderRadius: 8,
                    }}
                >
                    <p style={{ margin: 0, fontSize: 14 }}>
                        Nothing to migrate. All your configs are valid against
                        their current schemas.
                    </p>
                    <p
                        style={{
                            margin: "8px 0 0",
                            fontSize: 12,
                            opacity: 0.7,
                        }}
                    >
                        When a schema is refined on the Wire (or by you), any
                        affected configs will appear here for review.
                    </p>
                </div>
            )}

            <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                {entries.map((entry) => (
                    <MigrationCard
                        key={entry.contribution_id}
                        entry={entry}
                        onReview={() => handleOpenReview(entry)}
                    />
                ))}
            </div>

            {reviewing && (
                <MigrationReviewModal
                    entry={reviewing}
                    onClose={handleCloseReview}
                    onChanged={handleMigrationChanged}
                />
            )}
        </div>
    );
}

// ── Card subcomponent ────────────────────────────────────────────────

function MigrationCard({
    entry,
    onReview,
}: {
    entry: NeedsMigrationEntry;
    onReview: () => void;
}) {
    return (
        <div
            style={{
                background: "rgba(245, 158, 11, 0.04)",
                border: "1px solid rgba(245, 158, 11, 0.3)",
                borderRadius: 8,
                padding: 16,
                display: "flex",
                flexDirection: "column",
                gap: 8,
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "baseline",
                    gap: 10,
                    flexWrap: "wrap",
                }}
            >
                <span
                    style={{
                        fontSize: 14,
                        fontWeight: 600,
                        color: "var(--text-primary)",
                    }}
                >
                    {entry.schema_type}
                </span>
                {entry.slug && (
                    <code
                        style={{
                            fontSize: 11,
                            color: "var(--text-secondary)",
                            background: "rgba(255, 255, 255, 0.04)",
                            padding: "2px 6px",
                            borderRadius: 4,
                        }}
                    >
                        {entry.slug}
                    </code>
                )}
                <span
                    style={{
                        fontSize: 10,
                        padding: "2px 8px",
                        borderRadius: 4,
                        background: "rgba(245, 158, 11, 0.15)",
                        color: "#f59e0b",
                        fontWeight: 600,
                        textTransform: "uppercase",
                        letterSpacing: "0.04em",
                    }}
                >
                    Migration needed
                </span>
                <span
                    style={{
                        marginLeft: "auto",
                        fontSize: 10,
                        color: "var(--text-secondary)",
                        opacity: 0.7,
                    }}
                >
                    flagged at {formatTimestamp(entry.flagged_at)}
                </span>
            </div>

            {entry.supersession_note && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        fontStyle: "italic",
                        lineHeight: 1.5,
                    }}
                >
                    Schema change: "{entry.supersession_note}"
                </p>
            )}

            <details
                style={{
                    fontSize: 11,
                    color: "var(--text-secondary)",
                    cursor: "pointer",
                }}
            >
                <summary>Show current YAML</summary>
                <pre
                    style={{
                        margin: "8px 0 0",
                        padding: 10,
                        background: "rgba(0, 0, 0, 0.3)",
                        borderRadius: 4,
                        overflowX: "auto",
                        fontSize: 11,
                        lineHeight: 1.4,
                        maxHeight: 240,
                    }}
                >
                    {entry.current_yaml}
                </pre>
            </details>

            <div style={{ display: "flex", gap: 6 }}>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={onReview}
                    title="Open the LLM-assisted migration review modal"
                >
                    Propose migration
                </button>
            </div>
        </div>
    );
}

// ── Helpers ──────────────────────────────────────────────────────────

function formatTimestamp(iso: string): string {
    // Best-effort: render the ISO timestamp as a relative date when
    // recent, otherwise as a short YYYY-MM-DD HH:MM string. Falls back
    // to the raw string if parsing fails.
    try {
        const d = new Date(iso.replace(" ", "T") + "Z");
        if (Number.isNaN(d.getTime())) return iso;
        const now = Date.now();
        const diffMs = now - d.getTime();
        const diffHrs = diffMs / (1000 * 60 * 60);
        if (diffHrs < 1) {
            const mins = Math.round(diffMs / (1000 * 60));
            return `${mins}m ago`;
        }
        if (diffHrs < 24) {
            return `${Math.round(diffHrs)}h ago`;
        }
        return d.toISOString().slice(0, 16).replace("T", " ");
    } catch {
        return iso;
    }
}
