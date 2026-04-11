// src/components/MigrationReviewModal.tsx — Phase 18d: review modal for
// LLM-assisted schema migrations.
//
// Spec: docs/plans/phase-18d-workstream-prompt.md → "Frontend: ToolsMode
//       'Needs Migration' surface" section
//
// This modal owns the propose → review → accept/reject flow for a single
// flagged config. It's opened from MigrationPanel when the user clicks
// "Propose migration" on a flagged card.
//
// Flow (matches Phase 9's draft/refine/accept pattern exactly):
//
//   1. Mount: call pyramid_propose_config_migration (the LLM round-trip).
//      Show a "Generating migration…" spinner during the call.
//   2. Render: side-by-side old YAML vs new YAML diff + the schema_from
//      and schema_to bodies for context. The user reads through the
//      proposed change and the inline `# migrated: ...` comments.
//   3. User picks: Accept, Reject, or Cancel. Accept calls
//      pyramid_accept_config_migration; Reject calls
//      pyramid_reject_config_migration; Cancel just closes the modal.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
    AcceptMigrationOutcome,
    MigrationProposal,
    NeedsMigrationEntry,
    RejectMigrationOutcome,
} from "../types/configContributions";

interface MigrationReviewModalProps {
    entry: NeedsMigrationEntry;
    onClose: () => void;
    /** Called after accept or reject so the parent panel can refresh
     *  its list of flagged configs. */
    onChanged: () => void;
}

type ModalStep =
    | "loading_proposal"
    | "review"
    | "submitting_accept"
    | "submitting_reject"
    | "error";

export function MigrationReviewModal({
    entry,
    onClose,
    onChanged,
}: MigrationReviewModalProps) {
    const [step, setStep] = useState<ModalStep>("loading_proposal");
    const [proposal, setProposal] = useState<MigrationProposal | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [userNote, setUserNote] = useState<string>("");
    const [acceptNote, setAcceptNote] = useState<string>("");

    // Mount: kick off the propose IPC. The LLM round-trip happens here.
    useEffect(() => {
        let cancelled = false;
        setStep("loading_proposal");
        setError(null);

        invoke<MigrationProposal>("pyramid_propose_config_migration", {
            input: {
                contribution_id: entry.contribution_id,
                user_note: userNote.trim() ? userNote.trim() : null,
            },
        })
            .then((p) => {
                if (cancelled) return;
                setProposal(p);
                setStep("review");
            })
            .catch((err) => {
                if (cancelled) return;
                console.warn(
                    "[MigrationReviewModal] propose failed:",
                    err,
                );
                setError(String(err));
                setStep("error");
            });

        return () => {
            cancelled = true;
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [entry.contribution_id]); // intentionally not depending on userNote — re-propose is a manual action

    const handleRetry = useCallback(() => {
        // Re-trigger the propose effect by toggling step. The simplest
        // way is to call invoke again directly.
        setStep("loading_proposal");
        setError(null);
        invoke<MigrationProposal>("pyramid_propose_config_migration", {
            input: {
                contribution_id: entry.contribution_id,
                user_note: userNote.trim() ? userNote.trim() : null,
            },
        })
            .then((p) => {
                setProposal(p);
                setStep("review");
            })
            .catch((err) => {
                console.warn("[MigrationReviewModal] retry failed:", err);
                setError(String(err));
                setStep("error");
            });
    }, [entry.contribution_id, userNote]);

    const handleAccept = useCallback(async () => {
        if (!proposal) return;
        setStep("submitting_accept");
        setError(null);
        try {
            const outcome = await invoke<AcceptMigrationOutcome>(
                "pyramid_accept_config_migration",
                {
                    input: {
                        draft_id: proposal.draft_id,
                        accept_note: acceptNote.trim()
                            ? acceptNote.trim()
                            : null,
                    },
                },
            );
            if (!outcome.sync_succeeded) {
                console.warn(
                    "[MigrationReviewModal] accept landed but sync failed:",
                    outcome,
                );
            }
            onChanged();
            onClose();
        } catch (err) {
            console.warn("[MigrationReviewModal] accept failed:", err);
            setError(String(err));
            setStep("review");
        }
    }, [acceptNote, onChanged, onClose, proposal]);

    const handleReject = useCallback(async () => {
        if (!proposal) return;
        setStep("submitting_reject");
        setError(null);
        try {
            await invoke<RejectMigrationOutcome>(
                "pyramid_reject_config_migration",
                {
                    input: {
                        draft_id: proposal.draft_id,
                    },
                },
            );
            onChanged();
            onClose();
        } catch (err) {
            console.warn("[MigrationReviewModal] reject failed:", err);
            setError(String(err));
            setStep("review");
        }
    }, [onChanged, onClose, proposal]);

    return (
        <div className="diff-overlay" onClick={onClose}>
            <div
                className="diff-panel"
                style={{
                    maxWidth: 1100,
                    minWidth: 720,
                    maxHeight: "90vh",
                    overflow: "auto",
                }}
                onClick={(e) => e.stopPropagation()}
            >
                <div className="diff-header">
                    <div>
                        <h3 style={{ margin: 0 }}>Schema Migration Review</h3>
                        <span className="diff-title">
                            {entry.schema_type}
                            {entry.slug ? ` · ${entry.slug}` : " · global"}
                        </span>
                    </div>
                    <button
                        className="diff-close-btn"
                        onClick={onClose}
                        aria-label="Close"
                    >
                        x
                    </button>
                </div>

                <div
                    className="diff-body"
                    style={{ padding: 16, display: "flex", flexDirection: "column", gap: 16 }}
                >
                    {step === "loading_proposal" && (
                        <div
                            style={{
                                display: "flex",
                                flexDirection: "column",
                                alignItems: "center",
                                gap: 8,
                                padding: 32,
                                color: "var(--text-secondary)",
                            }}
                        >
                            <p style={{ margin: 0, fontSize: 14 }}>
                                Asking the LLM to migrate this config…
                            </p>
                            <p
                                style={{
                                    margin: 0,
                                    fontSize: 12,
                                    opacity: 0.7,
                                }}
                            >
                                One round trip via the bundled migrate_config
                                skill. Cached if the same migration was
                                proposed before.
                            </p>
                        </div>
                    )}

                    {step === "error" && (
                        <div
                            style={{
                                background: "rgba(248, 113, 113, 0.08)",
                                border: "1px solid rgba(248, 113, 113, 0.3)",
                                borderRadius: 6,
                                padding: 12,
                                color: "#fca5a5",
                                fontSize: 13,
                            }}
                        >
                            <p style={{ margin: "0 0 8px" }}>
                                Migration proposal failed.
                            </p>
                            <pre
                                style={{
                                    margin: 0,
                                    fontSize: 11,
                                    whiteSpace: "pre-wrap",
                                    wordBreak: "break-word",
                                }}
                            >
                                {error}
                            </pre>
                            <button
                                type="button"
                                className="btn btn-secondary btn-small"
                                style={{ marginTop: 8 }}
                                onClick={handleRetry}
                            >
                                Retry
                            </button>
                        </div>
                    )}

                    {(step === "review" ||
                        step === "submitting_accept" ||
                        step === "submitting_reject") &&
                        proposal && (
                            <ReviewBody
                                proposal={proposal}
                                entry={entry}
                                userNote={userNote}
                                onUserNoteChange={setUserNote}
                                acceptNote={acceptNote}
                                onAcceptNoteChange={setAcceptNote}
                                onAccept={handleAccept}
                                onReject={handleReject}
                                onRetry={handleRetry}
                                error={error}
                                submitting={
                                    step === "submitting_accept" ||
                                    step === "submitting_reject"
                                }
                            />
                        )}
                </div>
            </div>
        </div>
    );
}

// ── Review body ──────────────────────────────────────────────────────

function ReviewBody({
    proposal,
    entry,
    userNote,
    onUserNoteChange,
    acceptNote,
    onAcceptNoteChange,
    onAccept,
    onReject,
    onRetry,
    error,
    submitting,
}: {
    proposal: MigrationProposal;
    entry: NeedsMigrationEntry;
    userNote: string;
    onUserNoteChange: (s: string) => void;
    acceptNote: string;
    onAcceptNoteChange: (s: string) => void;
    onAccept: () => void;
    onReject: () => void;
    onRetry: () => void;
    error: string | null;
    submitting: boolean;
}) {
    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
            {entry.supersession_note && (
                <div
                    style={{
                        background: "rgba(245, 158, 11, 0.06)",
                        border: "1px solid rgba(245, 158, 11, 0.25)",
                        borderRadius: 6,
                        padding: 10,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                    }}
                >
                    <strong style={{ color: "var(--text-primary)" }}>
                        Schema change rationale:
                    </strong>{" "}
                    {entry.supersession_note}
                </div>
            )}

            <div
                style={{
                    display: "grid",
                    gridTemplateColumns: "1fr 1fr",
                    gap: 12,
                }}
            >
                <YamlPanel title="Old YAML (before migration)" body={proposal.old_yaml} />
                <YamlPanel
                    title="New YAML (LLM proposal)"
                    body={proposal.new_yaml}
                    highlight
                />
            </div>

            <details>
                <summary
                    style={{
                        cursor: "pointer",
                        fontSize: 12,
                        color: "var(--text-secondary)",
                    }}
                >
                    Show schemas (advanced)
                </summary>
                <div
                    style={{
                        display: "grid",
                        gridTemplateColumns: "1fr 1fr",
                        gap: 12,
                        marginTop: 8,
                    }}
                >
                    <YamlPanel title="Prior schema" body={proposal.schema_from} />
                    <YamlPanel title="New schema" body={proposal.schema_to} />
                </div>
            </details>

            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <label
                    style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        fontWeight: 600,
                    }}
                    htmlFor="user-note"
                >
                    Optional guidance for the LLM (re-propose to apply)
                </label>
                <textarea
                    id="user-note"
                    rows={2}
                    placeholder="e.g. preserve the high-priority rules even if the schema changed"
                    value={userNote}
                    onChange={(e) => onUserNoteChange(e.target.value)}
                    disabled={submitting}
                    style={{
                        width: "100%",
                        padding: 8,
                        fontSize: 12,
                        fontFamily: "inherit",
                        background: "rgba(255, 255, 255, 0.04)",
                        border: "1px solid var(--border-primary)",
                        borderRadius: 4,
                        color: "var(--text-primary)",
                        resize: "vertical",
                    }}
                />
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onRetry}
                    disabled={submitting}
                    style={{ alignSelf: "flex-start" }}
                >
                    Re-propose with guidance
                </button>
            </div>

            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <label
                    style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        fontWeight: 600,
                    }}
                    htmlFor="accept-note"
                >
                    Optional accept note (becomes the supersession provenance)
                </label>
                <input
                    id="accept-note"
                    type="text"
                    placeholder="e.g. accepted after manually checking the comments"
                    value={acceptNote}
                    onChange={(e) => onAcceptNoteChange(e.target.value)}
                    disabled={submitting}
                    style={{
                        width: "100%",
                        padding: 8,
                        fontSize: 12,
                        fontFamily: "inherit",
                        background: "rgba(255, 255, 255, 0.04)",
                        border: "1px solid var(--border-primary)",
                        borderRadius: 4,
                        color: "var(--text-primary)",
                    }}
                />
            </div>

            {error && (
                <div
                    style={{
                        background: "rgba(248, 113, 113, 0.08)",
                        border: "1px solid rgba(248, 113, 113, 0.3)",
                        borderRadius: 6,
                        padding: 10,
                        color: "#fca5a5",
                        fontSize: 12,
                    }}
                >
                    {error}
                </div>
            )}

            <div
                style={{
                    display: "flex",
                    gap: 8,
                    justifyContent: "flex-end",
                    paddingTop: 8,
                    borderTop: "1px solid var(--border-primary)",
                }}
            >
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onReject}
                    disabled={submitting}
                    title="Discard the LLM proposal. The original config stays flagged."
                >
                    Reject
                </button>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={onAccept}
                    disabled={submitting}
                    title="Promote the migrated YAML to active and clear the migration flag"
                >
                    {submitting ? "Submitting…" : "Accept migration"}
                </button>
            </div>
        </div>
    );
}

// ── YAML viewer pane ─────────────────────────────────────────────────

function YamlPanel({
    title,
    body,
    highlight,
}: {
    title: string;
    body: string;
    highlight?: boolean;
}) {
    return (
        <div
            style={{
                display: "flex",
                flexDirection: "column",
                gap: 6,
                minWidth: 0,
            }}
        >
            <h4
                style={{
                    margin: 0,
                    fontSize: 11,
                    fontWeight: 700,
                    color: "var(--text-secondary)",
                    textTransform: "uppercase",
                    letterSpacing: "0.04em",
                }}
            >
                {title}
            </h4>
            <pre
                style={{
                    margin: 0,
                    padding: 12,
                    background: highlight
                        ? "rgba(16, 185, 129, 0.06)"
                        : "rgba(0, 0, 0, 0.3)",
                    border: highlight
                        ? "1px solid rgba(16, 185, 129, 0.3)"
                        : "1px solid var(--border-primary)",
                    borderRadius: 4,
                    overflowX: "auto",
                    fontSize: 11,
                    lineHeight: 1.5,
                    maxHeight: 320,
                    color: "var(--text-primary)",
                }}
            >
                {body}
            </pre>
        </div>
    );
}
