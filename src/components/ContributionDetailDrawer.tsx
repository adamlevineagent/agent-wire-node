// src/components/ContributionDetailDrawer.tsx — Phase 10: single-
// contribution inspection drawer.
//
// Given a `ConfigContribution`, loads the Phase 8 `SchemaAnnotation`
// for its `schema_type`, parses its `yaml_content` into a values tree,
// and mounts `YamlConfigRenderer` in read-only mode. Optional version
// history tab fetches `pyramid_config_versions` and lets the user
// inspect each row's YAML + triggering note.
//
// Matches the `.pyramid-detail-drawer` CSS class pattern used elsewhere
// so it slides in from the right with the same look.

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import yaml from "js-yaml";
import { YamlConfigRenderer } from "./YamlConfigRenderer";
import { useYamlRendererSources } from "../hooks/useYamlRendererSources";
import type { SchemaAnnotation } from "../types/yamlRenderer";
import type { ConfigContribution } from "../types/configContributions";

interface ContributionDetailDrawerProps {
    /** The contribution being viewed. Set to `null` to hide the drawer. */
    contribution: ConfigContribution | null;
    /** Optional override: open the drawer straight into the history tab. */
    initialTab?: "details" | "history";
    /** Called when the user dismisses the drawer. */
    onClose: () => void;
    /** Called when the user clicks "Publish to Wire" in the footer. */
    onPublish?: (contribution: ConfigContribution) => void;
    /** Called when the user clicks "Edit" — caller should advance the
     *  parent's Create flow with this contribution as the base. */
    onEdit?: (contribution: ConfigContribution) => void;
}

type Tab = "details" | "history";

/**
 * Parse a YAML document body into a `Record<string, unknown>` for the
 * renderer. Silently returns an empty object on parse failure so the
 * drawer can still open — the renderer will show "no fields" gracefully.
 */
function parseYamlSafe(body: string): Record<string, unknown> {
    try {
        const parsed = yaml.load(body);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            return parsed as Record<string, unknown>;
        }
        return {};
    } catch (err) {
        console.warn("[ContributionDetailDrawer] YAML parse failed:", err);
        return {};
    }
}

export function ContributionDetailDrawer({
    contribution,
    initialTab = "details",
    onClose,
    onPublish,
    onEdit,
}: ContributionDetailDrawerProps) {
    const [tab, setTab] = useState<Tab>(initialTab);
    const [annotation, setAnnotation] = useState<SchemaAnnotation | null>(null);
    const [annotationLoading, setAnnotationLoading] = useState(false);
    const [versions, setVersions] = useState<ConfigContribution[]>([]);
    const [versionsLoading, setVersionsLoading] = useState(false);
    const [selectedVersionId, setSelectedVersionId] = useState<string | null>(null);
    const [historyError, setHistoryError] = useState<string | null>(null);

    // Reset internal state when the drawer opens a different contribution.
    useEffect(() => {
        if (!contribution) return;
        setTab(initialTab);
        setSelectedVersionId(null);
        setHistoryError(null);
        setVersions([]);
    }, [contribution?.contribution_id, initialTab]);

    // Escape to close.
    useEffect(() => {
        if (!contribution) return;
        const handleKey = (e: KeyboardEvent) => {
            if (e.key === "Escape") onClose();
        };
        document.addEventListener("keydown", handleKey);
        return () => document.removeEventListener("keydown", handleKey);
    }, [contribution, onClose]);

    // Fetch the schema annotation for the contribution's schema_type.
    useEffect(() => {
        if (!contribution) {
            setAnnotation(null);
            return;
        }
        let cancelled = false;
        setAnnotationLoading(true);
        invoke<SchemaAnnotation | null>("pyramid_get_schema_annotation", {
            schemaType: contribution.schema_type,
        })
            .then((result) => {
                if (cancelled) return;
                setAnnotation(result);
            })
            .catch((err) => {
                if (cancelled) return;
                console.warn(
                    "[ContributionDetailDrawer] schema annotation fetch failed:",
                    err,
                );
                setAnnotation(null);
            })
            .finally(() => {
                if (!cancelled) setAnnotationLoading(false);
            });
        return () => { cancelled = true; };
    }, [contribution?.schema_type]);

    // Lazily fetch version history the first time the user opens the tab.
    useEffect(() => {
        if (!contribution || tab !== "history" || versions.length > 0) return;
        let cancelled = false;
        setVersionsLoading(true);
        setHistoryError(null);
        invoke<ConfigContribution[]>("pyramid_config_versions", {
            schemaType: contribution.schema_type,
            slug: contribution.slug,
        })
            .then((rows) => {
                if (cancelled) return;
                setVersions(rows);
            })
            .catch((err) => {
                if (cancelled) return;
                setHistoryError(String(err));
            })
            .finally(() => {
                if (!cancelled) setVersionsLoading(false);
            });
        return () => { cancelled = true; };
    }, [contribution?.contribution_id, tab]);

    // Which row's YAML is currently rendered in the body.
    const activeRow = useMemo(() => {
        if (!contribution) return null;
        if (tab === "details") return contribution;
        if (selectedVersionId) {
            return (
                versions.find((v) => v.contribution_id === selectedVersionId) ??
                contribution
            );
        }
        // Default to the first version (latest chronologically — versions
        // are returned in chain order by the Phase 9 IPC).
        return versions[0] ?? contribution;
    }, [contribution, tab, selectedVersionId, versions]);

    const parsedValues = useMemo(
        () => (activeRow ? parseYamlSafe(activeRow.yaml_content) : {}),
        [activeRow],
    );

    const { optionSources, costEstimates } = useYamlRendererSources(
        annotation,
        parsedValues,
    );

    // noop callbacks for the renderer's required but unused hooks when
    // read-only.
    const noop = useCallback(() => {}, []);
    const noopPath = useCallback(() => {}, []);
    const noopNotes = useCallback(() => {}, []);

    if (!contribution) {
        return <div className="pyramid-detail-drawer pyramid-detail-drawer-hidden" />;
    }

    return (
        <div className="pyramid-detail-drawer">
            {/* Close button */}
            <button
                onClick={onClose}
                style={{
                    position: "absolute",
                    top: 12,
                    right: 12,
                    background: "none",
                    border: "none",
                    color: "inherit",
                    fontSize: 18,
                    cursor: "pointer",
                    opacity: 0.7,
                }}
                title="Close (Esc)"
            >
                ✕
            </button>

            {/* Header */}
            <div className="drawer-header">
                <span style={{ fontSize: 16, fontWeight: 700 }}>
                    {contribution.schema_type}
                </span>
                <div
                    style={{
                        display: "flex",
                        gap: 6,
                        flexWrap: "wrap",
                        alignItems: "center",
                    }}
                >
                    <StatusBadge status={contribution.status} />
                    <SourceBadge source={contribution.source} />
                    {contribution.wire_contribution_id && (
                        <span
                            style={{
                                fontSize: 10,
                                padding: "2px 6px",
                                borderRadius: 4,
                                background: "rgba(16, 185, 129, 0.15)",
                                color: "#10b981",
                            }}
                        >
                            Published
                        </span>
                    )}
                </div>
                <div
                    style={{
                        fontSize: 11,
                        color: "var(--text-secondary)",
                        opacity: 0.8,
                    }}
                >
                    {new Date(contribution.created_at).toLocaleString()}
                    {contribution.slug && (
                        <>
                            {" · "}
                            <code>{contribution.slug}</code>
                        </>
                    )}
                </div>
                {contribution.triggering_note && (
                    <div
                        style={{
                            fontSize: 11,
                            padding: "6px 8px",
                            marginTop: 4,
                            borderRadius: 4,
                            background: "rgba(167, 139, 250, 0.08)",
                            color: "var(--accent-purple, #a78bfa)",
                            fontStyle: "italic",
                            lineHeight: 1.4,
                        }}
                    >
                        "{contribution.triggering_note}"
                    </div>
                )}
            </div>

            {/* Tab switcher */}
            <div
                style={{
                    display: "flex",
                    gap: 4,
                    padding: "12px 0 8px",
                    borderBottom: "1px solid rgba(255,255,255,0.06)",
                }}
            >
                <TabButton
                    active={tab === "details"}
                    onClick={() => setTab("details")}
                >
                    Details
                </TabButton>
                <TabButton
                    active={tab === "history"}
                    onClick={() => setTab("history")}
                >
                    Version History
                </TabButton>
            </div>

            {/* History row picker */}
            {tab === "history" && (
                <div style={{ padding: "8px 0", fontSize: 12 }}>
                    {versionsLoading && (
                        <p style={{ color: "var(--text-secondary)", margin: 0 }}>
                            Loading versions…
                        </p>
                    )}
                    {historyError && (
                        <p
                            style={{
                                color: "#fca5a5",
                                margin: 0,
                                fontSize: 12,
                            }}
                        >
                            Failed to load history: {historyError}
                        </p>
                    )}
                    {!versionsLoading &&
                        !historyError &&
                        versions.length === 0 && (
                            <p
                                style={{
                                    color: "var(--text-secondary)",
                                    margin: 0,
                                }}
                            >
                                No version history yet.
                            </p>
                        )}
                    {versions.length > 0 && (
                        <div
                            style={{
                                display: "flex",
                                flexDirection: "column",
                                gap: 4,
                                maxHeight: 240,
                                overflowY: "auto",
                            }}
                        >
                            {versions.map((row, i) => {
                                const selected = activeRow?.contribution_id === row.contribution_id;
                                return (
                                    <button
                                        key={row.contribution_id}
                                        type="button"
                                        onClick={() =>
                                            setSelectedVersionId(
                                                row.contribution_id,
                                            )
                                        }
                                        style={{
                                            textAlign: "left",
                                            padding: "8px 10px",
                                            background: selected
                                                ? "rgba(34, 211, 238, 0.08)"
                                                : "rgba(255,255,255,0.02)",
                                            border: `1px solid ${
                                                selected
                                                    ? "rgba(34, 211, 238, 0.35)"
                                                    : "rgba(255,255,255,0.05)"
                                            }`,
                                            borderRadius: 6,
                                            color: "var(--text-primary)",
                                            cursor: "pointer",
                                            fontSize: 11,
                                            display: "flex",
                                            flexDirection: "column",
                                            gap: 2,
                                        }}
                                    >
                                        <div
                                            style={{
                                                display: "flex",
                                                justifyContent: "space-between",
                                                gap: 6,
                                            }}
                                        >
                                            <span
                                                style={{
                                                    fontWeight: 600,
                                                }}
                                            >
                                                v{versions.length - i}
                                            </span>
                                            <span
                                                style={{
                                                    opacity: 0.6,
                                                }}
                                            >
                                                {row.status}
                                            </span>
                                        </div>
                                        <div
                                            style={{
                                                opacity: 0.7,
                                                fontStyle: row.triggering_note
                                                    ? "italic"
                                                    : "normal",
                                            }}
                                        >
                                            {row.triggering_note ?? "(no note)"}
                                        </div>
                                        <div
                                            style={{
                                                fontSize: 10,
                                                opacity: 0.5,
                                            }}
                                        >
                                            {new Date(
                                                row.created_at,
                                            ).toLocaleString()}
                                        </div>
                                    </button>
                                );
                            })}
                        </div>
                    )}
                </div>
            )}

            {/* Body — renderer or fallback */}
            <div
                style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 8,
                    paddingTop: 8,
                }}
            >
                {annotationLoading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 12 }}>
                        Loading schema annotation…
                    </p>
                )}
                {!annotationLoading && annotation && activeRow && (
                    <YamlConfigRenderer
                        schema={annotation}
                        values={parsedValues}
                        onChange={noopPath}
                        onAccept={noop}
                        onNotes={noopNotes}
                        optionSources={optionSources}
                        costEstimates={costEstimates}
                        readOnly
                        versionInfo={
                            tab === "history" && activeRow.triggering_note
                                ? {
                                      version:
                                          versions.length -
                                          versions.findIndex(
                                              (v) =>
                                                  v.contribution_id ===
                                                  activeRow.contribution_id,
                                          ),
                                      totalVersions: versions.length || 1,
                                      triggeringNote:
                                          activeRow.triggering_note,
                                  }
                                : undefined
                        }
                    />
                )}
                {!annotationLoading && !annotation && activeRow && (
                    <div>
                        <p
                            style={{
                                color: "var(--text-secondary)",
                                fontSize: 12,
                                marginTop: 0,
                            }}
                        >
                            No UI schema annotation available for{" "}
                            <code>{contribution.schema_type}</code>. Raw YAML:
                        </p>
                        <pre
                            style={{
                                margin: 0,
                                padding: "10px 12px",
                                background: "rgba(0,0,0,0.35)",
                                border: "1px solid rgba(255,255,255,0.08)",
                                borderRadius: 6,
                                maxHeight: 360,
                                overflow: "auto",
                                fontSize: 11,
                                fontFamily: "var(--font-mono, monospace)",
                                color: "var(--text-primary)",
                                whiteSpace: "pre-wrap",
                                wordBreak: "break-word",
                            }}
                        >
                            {activeRow.yaml_content}
                        </pre>
                    </div>
                )}
            </div>

            {/* Footer actions */}
            <div className="drawer-actions">
                {onPublish && contribution.status === "active" && (
                    <button
                        type="button"
                        className="btn btn-primary"
                        onClick={() => onPublish(contribution)}
                    >
                        Publish to Wire
                    </button>
                )}
                {onEdit && (
                    <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={() => onEdit(contribution)}
                    >
                        Edit (refine from this version)
                    </button>
                )}
                <button
                    type="button"
                    className="btn btn-ghost"
                    onClick={onClose}
                >
                    Close
                </button>
            </div>
        </div>
    );
}

// ── Small presentational helpers ────────────────────────────────────────────

function TabButton({
    active,
    onClick,
    children,
}: {
    active: boolean;
    onClick: () => void;
    children: React.ReactNode;
}) {
    return (
        <button
            type="button"
            onClick={onClick}
            style={{
                background: active ? "rgba(34,211,238,0.08)" : "transparent",
                border: "none",
                borderBottom: active
                    ? "2px solid var(--accent-cyan)"
                    : "2px solid transparent",
                color: active
                    ? "var(--accent-cyan)"
                    : "var(--text-secondary)",
                padding: "6px 10px",
                fontSize: 12,
                fontWeight: 600,
                cursor: "pointer",
                letterSpacing: "0.02em",
            }}
        >
            {children}
        </button>
    );
}

function StatusBadge({ status }: { status: string }) {
    const palette: Record<string, { bg: string; fg: string }> = {
        active: { bg: "rgba(16, 185, 129, 0.15)", fg: "#10b981" },
        draft: { bg: "rgba(245, 158, 11, 0.15)", fg: "#f59e0b" },
        proposed: { bg: "rgba(139, 92, 246, 0.15)", fg: "#a78bfa" },
        rejected: { bg: "rgba(239, 68, 68, 0.15)", fg: "#fca5a5" },
        superseded: { bg: "rgba(107, 114, 128, 0.15)", fg: "#9ca3af" },
    };
    const colors = palette[status] ?? { bg: "rgba(107, 114, 128, 0.15)", fg: "#9ca3af" };
    return (
        <span
            style={{
                fontSize: 10,
                padding: "2px 6px",
                borderRadius: 4,
                background: colors.bg,
                color: colors.fg,
                textTransform: "capitalize",
                fontWeight: 600,
                letterSpacing: "0.02em",
            }}
        >
            {status}
        </span>
    );
}

function SourceBadge({ source }: { source: string }) {
    return (
        <span
            style={{
                fontSize: 10,
                padding: "2px 6px",
                borderRadius: 4,
                background: "rgba(255,255,255,0.06)",
                color: "var(--text-secondary)",
                textTransform: "capitalize",
                letterSpacing: "0.02em",
            }}
        >
            {source}
        </span>
    );
}
