// src/components/PublishPreviewModal.tsx — Phase 10: dry-run publish preview.
//
// Takes a config contribution id, fetches the Phase 5 `DryRunReport`
// via `pyramid_dry_run_publish`, and renders the visibility, canonical
// YAML, cost breakdown, supersession chain, section decomposition, and
// warnings. A Confirm button calls `pyramid_publish_to_wire` with
// `confirm: true` and surfaces the resulting handle path.
//
// Source of truth: docs/specs/wire-contribution-mapping.md → "Publish IPC"
// section + docs/plans/phase-10-workstream-prompt.md → "Dry-run publish
// modal" section.

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
    DryRunReport,
    PublishToWireResponse,
} from "../types/configContributions";

interface PublishPreviewModalProps {
    /** Contribution id to preview + publish. */
    contributionId: string;
    /** Optional display-only schema_type for the modal header. */
    schemaType?: string;
    /** Called when the user dismisses the modal. */
    onClose: () => void;
    /** Called after a successful publish, with the backend response. */
    onPublished?: (result: PublishToWireResponse) => void;
}

export function PublishPreviewModal({
    contributionId,
    schemaType,
    onClose,
    onPublished,
}: PublishPreviewModalProps) {
    const [report, setReport] = useState<DryRunReport | null>(null);
    const [loading, setLoading] = useState(true);
    const [loadError, setLoadError] = useState<string | null>(null);
    const [publishing, setPublishing] = useState(false);
    const [publishError, setPublishError] = useState<string | null>(null);
    const [publishResult, setPublishResult] =
        useState<PublishToWireResponse | null>(null);
    // Phase 18c (L4): cache manifest opt-in. Defaults to FALSE — the
    // user must explicitly check the box to ship cached LLM outputs
    // alongside the contribution. No clever auto-check heuristic; the
    // privacy gate is the user's deliberate action.
    const [includeCacheManifest, setIncludeCacheManifest] = useState(false);

    // Fetch the dry-run report on mount. Phase 5's handler doesn't
    // require auth — it's a pure local transform over the contribution
    // row — so it can run immediately.
    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        setLoadError(null);
        invoke<DryRunReport>("pyramid_dry_run_publish", { contributionId })
            .then((result) => {
                if (cancelled) return;
                setReport(result);
            })
            .catch((err: unknown) => {
                if (cancelled) return;
                setLoadError(String(err));
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });
        return () => { cancelled = true; };
    }, [contributionId]);

    // Block close while a publish is in flight — the backend write is
    // past the point of no return, and closing mid-flight creates a
    // ghost publish the user never sees confirmation for.
    const safeClose = useCallback(() => {
        if (publishing) return;
        onClose();
    }, [publishing, onClose]);

    // Escape key closes the modal (unless publishing).
    useEffect(() => {
        const handleKey = (e: KeyboardEvent) => {
            if (e.key === "Escape") safeClose();
        };
        document.addEventListener("keydown", handleKey);
        return () => document.removeEventListener("keydown", handleKey);
    }, [safeClose]);

    const handleConfirm = useCallback(async () => {
        setPublishing(true);
        setPublishError(null);
        try {
            // Phase 18c (L4): pass the cache-manifest opt-in through
            // to the backend. Default-OFF; only `true` when the user
            // explicitly checked the Advanced Publishing Options box.
            const result = await invoke<PublishToWireResponse>(
                "pyramid_publish_to_wire",
                {
                    contributionId,
                    confirm: true,
                    includeCacheManifest,
                },
            );
            setPublishResult(result);
            if (onPublished) onPublished(result);
        } catch (err: unknown) {
            setPublishError(String(err));
        } finally {
            setPublishing(false);
        }
    }, [contributionId, includeCacheManifest, onPublished]);

    return (
        <div
            className="fleet-token-modal-overlay"
            onClick={safeClose}
            role="dialog"
            aria-modal="true"
            aria-label="Publish to Wire preview"
        >
            <div
                className="fleet-token-modal"
                onClick={(e) => e.stopPropagation()}
                style={{
                    maxWidth: 640,
                    maxHeight: "85vh",
                    overflowY: "auto",
                    gap: 12,
                }}
            >
                {/* Header */}
                <div
                    style={{
                        display: "flex",
                        alignItems: "baseline",
                        justifyContent: "space-between",
                        gap: 12,
                    }}
                >
                    <h4 className="fleet-token-modal-title">
                        Publish to Wire
                        {schemaType && (
                            <span
                                style={{
                                    marginLeft: 8,
                                    fontSize: 12,
                                    color: "var(--text-secondary)",
                                    fontFamily: "var(--font-mono, monospace)",
                                }}
                            >
                                {schemaType}
                            </span>
                        )}
                    </h4>
                    <button
                        type="button"
                        className="btn btn-ghost btn-small"
                        onClick={safeClose}
                        disabled={publishing}
                        title={publishing ? "Publishing… can't close" : "Close"}
                        style={{ padding: "2px 10px" }}
                    >
                        ✕
                    </button>
                </div>

                {loading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        Building dry-run report…
                    </p>
                )}

                {loadError && (
                    <div
                        style={{
                            padding: "10px 12px",
                            background: "rgba(239, 68, 68, 0.08)",
                            border: "1px solid rgba(239, 68, 68, 0.25)",
                            borderRadius: 6,
                            color: "#fca5a5",
                            fontSize: 13,
                        }}
                    >
                        Failed to build dry-run report: {loadError}
                    </div>
                )}

                {report && !publishResult && (
                    <>
                        {/* Visibility + Wire type */}
                        <Section title="Visibility">
                            <KeyValue label="Wire type" value={report.wire_type} />
                            <KeyValue label="Scope" value={report.visibility} />
                            {report.tags.length > 0 && (
                                <div
                                    style={{
                                        display: "flex",
                                        flexWrap: "wrap",
                                        gap: 6,
                                        marginTop: 4,
                                    }}
                                >
                                    {report.tags.map((tag) => (
                                        <span
                                            key={tag}
                                            style={{
                                                fontSize: 11,
                                                padding: "2px 8px",
                                                borderRadius: 10,
                                                background:
                                                    "rgba(34, 211, 238, 0.1)",
                                                color: "var(--accent-cyan)",
                                                fontFamily:
                                                    "var(--font-mono, monospace)",
                                            }}
                                        >
                                            {tag}
                                        </span>
                                    ))}
                                </div>
                            )}
                        </Section>

                        {/* Warnings */}
                        {report.warnings.length > 0 && (
                            <Section title="Warnings">
                                <ul
                                    style={{
                                        margin: 0,
                                        padding: "0 0 0 16px",
                                        color: "#fbbf24",
                                        fontSize: 12,
                                        lineHeight: 1.5,
                                    }}
                                >
                                    {report.warnings.map((w, i) => (
                                        <li key={i}>{w}</li>
                                    ))}
                                </ul>
                            </Section>
                        )}

                        {/* Cost breakdown */}
                        <Section title="Cost breakdown">
                            <KeyValue
                                label="Deposit"
                                value={`${report.cost_breakdown.deposit_credits} credits`}
                            />
                            <KeyValue
                                label="Publish fee"
                                value={`${report.cost_breakdown.publish_fee} credits`}
                            />
                            <KeyValue
                                label="Author price"
                                value={`${report.cost_breakdown.author_price} credits`}
                            />
                            <KeyValue
                                label="Estimated total"
                                value={`${report.cost_breakdown.estimated_total} credits`}
                                highlight
                            />
                        </Section>

                        {/* Supersession chain */}
                        {report.supersession_chain.length > 0 && (
                            <Section title="Supersession chain">
                                <ol
                                    style={{
                                        margin: 0,
                                        padding: "0 0 0 16px",
                                        color: "var(--text-secondary)",
                                        fontSize: 12,
                                        lineHeight: 1.6,
                                    }}
                                >
                                    {report.supersession_chain.map((link, i) => (
                                        <li key={`${link.handle_path}-${i}`}>
                                            <code style={{ fontSize: 11 }}>
                                                {link.handle_path}
                                            </code>{" "}
                                            <span style={{ opacity: 0.6 }}>
                                                ({link.maturity})
                                            </span>
                                        </li>
                                    ))}
                                </ol>
                            </Section>
                        )}

                        {/* Section decomposition */}
                        {report.section_previews.length > 0 && (
                            <Section title="Section decomposition">
                                <ul
                                    style={{
                                        margin: 0,
                                        padding: "0 0 0 16px",
                                        color: "var(--text-secondary)",
                                        fontSize: 12,
                                        lineHeight: 1.6,
                                    }}
                                >
                                    {report.section_previews.map((s, i) => (
                                        <li key={i}>
                                            {s.heading} —{" "}
                                            <span style={{ opacity: 0.6 }}>
                                                {s.contribution_type}
                                            </span>
                                            {!s.will_publish && (
                                                <span
                                                    style={{
                                                        marginLeft: 6,
                                                        color: "#fbbf24",
                                                    }}
                                                >
                                                    (skipped)
                                                </span>
                                            )}
                                        </li>
                                    ))}
                                </ul>
                            </Section>
                        )}

                        {/* Derived_from */}
                        {report.resolved_derived_from.length > 0 && (
                            <Section title="Derived from (28-slot allocation)">
                                <ul
                                    style={{
                                        margin: 0,
                                        padding: "0 0 0 16px",
                                        color: "var(--text-secondary)",
                                        fontSize: 12,
                                        lineHeight: 1.6,
                                    }}
                                >
                                    {report.resolved_derived_from.map((d, i) => (
                                        <li key={`${d.reference}-${i}`}>
                                            <code style={{ fontSize: 11 }}>
                                                {d.kind}:{d.reference}
                                            </code>{" "}
                                            <span style={{ opacity: 0.6 }}>
                                                weight={d.weight.toFixed(3)},{" "}
                                                slots={d.allocated_slots}
                                            </span>
                                        </li>
                                    ))}
                                </ul>
                            </Section>
                        )}

                        {/* Canonical YAML preview */}
                        <Section title="Canonical YAML">
                            <pre
                                style={{
                                    margin: 0,
                                    padding: "10px 12px",
                                    background: "rgba(0,0,0,0.35)",
                                    border: "1px solid rgba(255,255,255,0.08)",
                                    borderRadius: 6,
                                    maxHeight: 200,
                                    overflow: "auto",
                                    fontSize: 11,
                                    fontFamily: "var(--font-mono, monospace)",
                                    color: "var(--text-primary)",
                                    whiteSpace: "pre-wrap",
                                    wordBreak: "break-word",
                                }}
                            >
                                {report.canonical_yaml}
                            </pre>
                        </Section>

                        {/* Phase 18c (L4): Advanced publishing options. The
                            cache manifest opt-in lives here so it stays
                            visually separate from the must-review fields
                            above. Default-OFF; the user must explicitly
                            check the box to ship cached LLM outputs. */}
                        <Section title="Advanced publishing options">
                            <label
                                htmlFor="phase-18c-cache-opt-in"
                                style={{
                                    display: "flex",
                                    alignItems: "flex-start",
                                    gap: 10,
                                    cursor: "pointer",
                                    paddingTop: 4,
                                }}
                            >
                                <input
                                    type="checkbox"
                                    id="phase-18c-cache-opt-in"
                                    checked={includeCacheManifest}
                                    onChange={(e) =>
                                        setIncludeCacheManifest(
                                            e.target.checked,
                                        )
                                    }
                                    disabled={publishing}
                                    style={{
                                        marginTop: 3,
                                        flexShrink: 0,
                                        cursor: "pointer",
                                    }}
                                />
                                <div
                                    style={{
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: 6,
                                        fontSize: 12,
                                        lineHeight: 1.5,
                                        color: "var(--text-primary)",
                                    }}
                                >
                                    <div
                                        style={{
                                            fontWeight: 600,
                                            color: "var(--text-primary)",
                                        }}
                                    >
                                        Include cache manifest
                                    </div>
                                    <div
                                        style={{
                                            color: "var(--text-secondary)",
                                        }}
                                    >
                                        Pullers of this pyramid will be able
                                        to reuse your cached LLM outputs to
                                        rebuild instantly without re-running
                                        expensive model calls — a large cost
                                        saving for popular pyramids.
                                    </div>
                                    <div
                                        style={{
                                            color: "#fbbf24",
                                            paddingTop: 2,
                                        }}
                                    >
                                        Warning: cached outputs may contain
                                        excerpts from your source material.
                                        Only enable for pyramids whose source
                                        is already public and whose L0 nodes
                                        reference public corpus documents.
                                    </div>
                                    {includeCacheManifest && (
                                        <div
                                            style={{
                                                color: "var(--accent-cyan)",
                                                paddingTop: 2,
                                                fontStyle: "italic",
                                            }}
                                        >
                                            Cache manifest will be attached
                                            to this publish. Audit count of
                                            L0 nodes referencing private
                                            sources is a follow-up — review
                                            your source visibility manually
                                            before confirming.
                                        </div>
                                    )}
                                </div>
                            </label>
                        </Section>

                        {publishError && (
                            <div
                                style={{
                                    padding: "10px 12px",
                                    background: "rgba(239, 68, 68, 0.08)",
                                    border: "1px solid rgba(239, 68, 68, 0.25)",
                                    borderRadius: 6,
                                    color: "#fca5a5",
                                    fontSize: 13,
                                }}
                            >
                                Publish failed: {publishError}
                            </div>
                        )}

                        {/* Actions */}
                        <div
                            style={{
                                display: "flex",
                                justifyContent: "flex-end",
                                gap: 8,
                                marginTop: 4,
                            }}
                        >
                            <button
                                type="button"
                                className="btn btn-secondary"
                                onClick={onClose}
                                disabled={publishing}
                            >
                                Cancel
                            </button>
                            <button
                                type="button"
                                className="btn btn-primary"
                                onClick={handleConfirm}
                                disabled={publishing}
                                title="Publish this contribution to the Wire"
                            >
                                {publishing ? "Publishing…" : "Confirm & Publish"}
                            </button>
                        </div>
                    </>
                )}

                {/* Success state */}
                {publishResult && (
                    <div
                        style={{
                            padding: "12px 14px",
                            background: "rgba(16, 185, 129, 0.08)",
                            border: "1px solid rgba(16, 185, 129, 0.25)",
                            borderRadius: 6,
                            display: "flex",
                            flexDirection: "column",
                            gap: 8,
                        }}
                    >
                        <div
                            style={{
                                color: "#10b981",
                                fontWeight: 600,
                                fontSize: 14,
                            }}
                        >
                            Published to Wire
                        </div>
                        <KeyValue
                            label="Wire contribution id"
                            value={publishResult.wire_contribution_id}
                            mono
                        />
                        {publishResult.handle_path && (
                            <KeyValue
                                label="Handle path"
                                value={publishResult.handle_path}
                                mono
                            />
                        )}
                        <KeyValue label="Type" value={publishResult.wire_type} />
                        {publishResult.sections_published.length > 0 && (
                            <KeyValue
                                label="Sections published"
                                value={publishResult.sections_published.join(", ")}
                            />
                        )}
                        {/* Phase 18c (L4): surface the cache manifest
                            attachment state in the success view so the user
                            sees confirmation that their opt-in took effect.
                            `null` means the user did not opt in (default
                            behavior); a number means the manifest was
                            attached with that many entries. */}
                        {publishResult.cache_manifest_entries !== null &&
                        publishResult.cache_manifest_entries !== undefined ? (
                            <KeyValue
                                label="Cache manifest"
                                value={`${publishResult.cache_manifest_entries} entries attached`}
                            />
                        ) : null}
                        <div
                            style={{
                                display: "flex",
                                justifyContent: "flex-end",
                                marginTop: 4,
                            }}
                        >
                            <button
                                type="button"
                                className="btn btn-primary"
                                onClick={onClose}
                            >
                                Done
                            </button>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
}

// ── Tiny helpers ────────────────────────────────────────────────────────────

function Section({
    title,
    children,
}: {
    title: string;
    children: React.ReactNode;
}) {
    return (
        <section
            style={{
                display: "flex",
                flexDirection: "column",
                gap: 4,
                paddingTop: 10,
                borderTop: "1px solid rgba(255,255,255,0.06)",
            }}
        >
            <h5
                style={{
                    margin: 0,
                    fontSize: 11,
                    fontWeight: 600,
                    textTransform: "uppercase",
                    letterSpacing: "0.08em",
                    color: "var(--text-secondary)",
                }}
            >
                {title}
            </h5>
            {children}
        </section>
    );
}

function KeyValue({
    label,
    value,
    highlight,
    mono,
}: {
    label: string;
    value: string;
    highlight?: boolean;
    mono?: boolean;
}) {
    return (
        <div
            style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 12,
                fontSize: 12,
                lineHeight: 1.5,
            }}
        >
            <span style={{ color: "var(--text-secondary)" }}>{label}</span>
            <span
                style={{
                    color: highlight ? "var(--accent-cyan)" : "var(--text-primary)",
                    fontWeight: highlight ? 600 : 400,
                    fontFamily: mono
                        ? "var(--font-mono, monospace)"
                        : "inherit",
                    wordBreak: "break-all",
                    textAlign: "right",
                }}
            >
                {value}
            </span>
        </div>
    );
}
