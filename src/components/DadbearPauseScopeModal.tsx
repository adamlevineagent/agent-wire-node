// Phase 18c (L9) — Reusable scope picker modal for the bulk pause/resume
// flow on DADBEAR. Consumed by CrossPyramidTimeline.tsx (the cross-pyramid
// build view) and DadbearOversightPage.tsx (the per-pyramid status view),
// both of which previously shipped a "Pause All" button hard-wired to
// `scope: "all"`.
//
// Spec: docs/specs/cross-pyramid-observability.md "Pause-All Semantics"
// (~line 286). Implementation note: the spec defines three scopes —
// `all`, `folder`, and `circle`. The `circle` scope depends on a
// `pyramid_metadata.circle_id` schema that doesn't exist in the local
// DB (circle membership lives only in the Wire JWT claim layer). It is
// rendered as a disabled radio with a "Coming soon" hint, with the
// rationale documented in deferral-ledger.md as a follow-up.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type DadbearPauseScope = "all" | "folder" | "circle";
export type DadbearPauseAction = "pause" | "resume";

interface DadbearPauseScopeModalProps {
    /** Whether the user is invoking pause or resume. The verb in the
     *  primary button + the count direction depend on this. */
    action: DadbearPauseAction;
    /** Called when the user confirms. The parent should call
     *  `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` with
     *  the returned scope + scope_value, then refetch the affected
     *  rows on its own page. */
    onConfirm: (scope: DadbearPauseScope, scopeValue: string | null) => void;
    /** Called when the user cancels. The parent unmounts the modal. */
    onCancel: () => void;
}

interface CountResponse {
    count: number;
}

export function DadbearPauseScopeModal({
    action,
    onConfirm,
    onCancel,
}: DadbearPauseScopeModalProps) {
    const [scope, setScope] = useState<DadbearPauseScope>("all");
    const [folderInput, setFolderInput] = useState("");
    const [folderOptions, setFolderOptions] = useState<string[]>([]);
    const [count, setCount] = useState<number>(0);
    const [countLoading, setCountLoading] = useState(false);
    const [countError, setCountError] = useState<string | null>(null);

    // Fetch the distinct folder list once on mount so the dropdown
    // can offer concrete picks. Falls back to a free text input if
    // the IPC fails (the folder is just a string match — typing a
    // path the user knows works fine).
    useEffect(() => {
        let cancelled = false;
        invoke<string[]>("pyramid_list_dadbear_source_paths")
            .then((paths) => {
                if (cancelled) return;
                setFolderOptions(paths);
            })
            .catch(() => {
                if (cancelled) return;
                // Silently fall through — the text input still works.
                setFolderOptions([]);
            });
        return () => {
            cancelled = true;
        };
    }, []);

    // Live count preview. Re-runs whenever the scope OR the folder
    // input changes so the user sees "Pause N pyramid(s)" update as
    // they type/pick. The count IPC is a pure SELECT — no mutation.
    useEffect(() => {
        let cancelled = false;
        const scopeValue = scope === "folder" ? folderInput : null;
        // Skip the round-trip when the folder input is empty for the
        // folder scope — the backend returns 0 for that case anyway.
        if (scope === "folder" && !scopeValue) {
            setCount(0);
            setCountError(null);
            return;
        }
        // Circle scope is deferred — the IPC returns 0 for it.
        // Don't bother making the call.
        if (scope === "circle") {
            setCount(0);
            setCountError(null);
            return;
        }
        setCountLoading(true);
        setCountError(null);
        invoke<CountResponse>("pyramid_count_freeze_scope", {
            scope,
            scopeValue,
            targetState: action === "pause" ? "freeze" : "unfreeze",
        })
            .then((resp) => {
                if (cancelled) return;
                setCount(resp.count);
            })
            .catch((err: unknown) => {
                if (cancelled) return;
                setCount(0);
                setCountError(String(err));
            })
            .finally(() => {
                if (!cancelled) setCountLoading(false);
            });
        return () => {
            cancelled = true;
        };
    }, [scope, folderInput, action]);

    const handleConfirm = useCallback(() => {
        const scopeValue = scope === "folder" ? folderInput : null;
        onConfirm(scope, scopeValue);
    }, [scope, folderInput, onConfirm]);

    const verbing = action === "pause" ? "Pausing" : "Resuming";
    const verb = action === "pause" ? "Pause" : "Resume";
    const buttonClass =
        action === "pause" ? "btn btn-danger" : "btn btn-primary";
    const confirmDisabled =
        (scope === "folder" && folderInput.trim().length === 0) ||
        scope === "circle" ||
        count === 0;

    return (
        <div
            className="cpt-confirm-backdrop"
            onClick={onCancel}
            role="dialog"
            aria-modal="true"
            aria-label={`${verb} DADBEAR with scope picker`}
        >
            <div
                className="cpt-confirm-modal"
                onClick={(e) => e.stopPropagation()}
                style={{ minWidth: 420, maxWidth: 540 }}
            >
                <h3 style={{ marginTop: 0 }}>{verb} DADBEAR</h3>

                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                        marginBottom: 12,
                    }}
                >
                    <div
                        style={{
                            fontSize: 11,
                            fontWeight: 600,
                            textTransform: "uppercase",
                            letterSpacing: "0.08em",
                            color: "var(--text-secondary)",
                        }}
                    >
                        Scope
                    </div>

                    <label
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: 8,
                            cursor: "pointer",
                        }}
                    >
                        <input
                            type="radio"
                            name="dadbear-scope"
                            value="all"
                            checked={scope === "all"}
                            onChange={() => setScope("all")}
                        />
                        <span>All pyramids</span>
                    </label>

                    <label
                        style={{
                            display: "flex",
                            alignItems: "flex-start",
                            gap: 8,
                            cursor: "pointer",
                        }}
                    >
                        <input
                            type="radio"
                            name="dadbear-scope"
                            value="folder"
                            checked={scope === "folder"}
                            onChange={() => setScope("folder")}
                            style={{ marginTop: 4 }}
                        />
                        <div
                            style={{
                                display: "flex",
                                flexDirection: "column",
                                gap: 4,
                                flex: 1,
                            }}
                        >
                            <span>Pyramids under folder:</span>
                            <input
                                type="text"
                                value={folderInput}
                                placeholder="/path/to/folder"
                                disabled={scope !== "folder"}
                                onChange={(e) =>
                                    setFolderInput(e.target.value)
                                }
                                onFocus={() => setScope("folder")}
                                list="dadbear-source-paths"
                                style={{
                                    width: "100%",
                                    padding: "4px 8px",
                                    fontSize: 12,
                                    fontFamily:
                                        "var(--font-mono, monospace)",
                                    background: "rgba(0,0,0,0.25)",
                                    border:
                                        "1px solid rgba(255,255,255,0.12)",
                                    borderRadius: 4,
                                    color: "var(--text-primary)",
                                }}
                            />
                            {/* HTML datalist powers the autocomplete
                                drop-down. Native to <input list="..."> —
                                no extra component, no extra deps. */}
                            <datalist id="dadbear-source-paths">
                                {folderOptions.map((p) => (
                                    <option key={p} value={p} />
                                ))}
                            </datalist>
                        </div>
                    </label>

                    <label
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: 8,
                            cursor: "not-allowed",
                            opacity: 0.45,
                        }}
                        title="Circle scoping is deferred — local DB has no circle_id schema yet."
                    >
                        <input
                            type="radio"
                            name="dadbear-scope"
                            value="circle"
                            checked={scope === "circle"}
                            disabled
                            onChange={() => setScope("circle")}
                        />
                        <span>Pyramids in circle (coming soon)</span>
                    </label>
                </div>

                <p
                    style={{
                        fontSize: 13,
                        lineHeight: 1.5,
                        margin: "12px 0",
                        color: "var(--text-secondary)",
                    }}
                >
                    {countLoading && <em>{verbing} preview…</em>}
                    {!countLoading && countError && (
                        <span style={{ color: "#fca5a5" }}>
                            Count error: {countError}
                        </span>
                    )}
                    {!countLoading && !countError && (
                        <>
                            This will {action === "pause" ? "freeze" : "unfreeze"} DADBEAR
                            auto-update for{" "}
                            <strong>{count}</strong> pyramid
                            {count === 1 ? "" : "s"}. In-flight builds
                            are not affected. Use{" "}
                            {action === "pause" ? "Resume" : "Pause"} to
                            {action === "pause" ? " unfreeze" : " re-freeze"}.
                        </>
                    )}
                </p>

                <div className="cpt-confirm-actions">
                    <button
                        className="btn btn-secondary"
                        onClick={onCancel}
                    >
                        Cancel
                    </button>
                    <button
                        className={buttonClass}
                        onClick={handleConfirm}
                        disabled={confirmDisabled}
                        title={
                            confirmDisabled
                                ? "Pick a scope with a non-zero match"
                                : `${verb} ${count} pyramid${count === 1 ? "" : "s"}`
                        }
                    >
                        {verb} {count > 0 ? count : ""}
                    </button>
                </div>
            </div>
        </div>
    );
}
