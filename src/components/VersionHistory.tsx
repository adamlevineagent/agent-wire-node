import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CachedDocument, VersionHistoryResponse } from "./Dashboard";

interface VersionHistoryProps {
    document: CachedDocument;
    history: VersionHistoryResponse | null;
    loading: boolean;
    onClose: () => void;
    onDiff: (oldDocId: string, newDocId: string, title: string) => void;
}

function timeAgo(dateStr: string): string {
    const now = Date.now();
    const then = new Date(dateStr).getTime();
    const diff = now - then;
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return `${hrs}h ago`;
    const days = Math.floor(hrs / 24);
    if (days < 30) return `${days}d ago`;
    return new Date(dateStr).toLocaleDateString();
}

export function VersionHistory({ document: doc, history, loading, onClose, onDiff }: VersionHistoryProps) {
    const handlePin = useCallback(async (docId: string) => {
        try {
            // Find folder path from context — use a simple approach
            await invoke("pin_version", {
                documentId: docId,
                folderPath: ".", // Will be resolved by Tauri
            });
        } catch (err) {
            console.error("Failed to pin version:", err);
        }
    }, []);

    return (
        <div className="version-history-overlay" onClick={onClose}>
            <div className="version-history-panel" onClick={(e) => e.stopPropagation()}>
                <div className="version-history-header">
                    <div>
                        <h3>Version History</h3>
                        <span className="version-history-file">{doc.source_path}</span>
                    </div>
                    <button className="version-close-btn" onClick={onClose}>x</button>
                </div>

                <div className="version-history-body">
                    {loading ? (
                        <div className="version-loading">Loading version history...</div>
                    ) : !history || history.versions.length === 0 ? (
                        <div className="version-empty">No version history available for this document.</div>
                    ) : (
                        <>
                            <div className="version-count">
                                {history.total_versions} version{history.total_versions !== 1 ? "s" : ""}
                            </div>
                            <div className="version-timeline">
                                {history.versions.map((v, idx) => {
                                    const isLatest = idx === 0;
                                    const prevVersion = idx < history.versions.length - 1
                                        ? history.versions[idx + 1]
                                        : null;

                                    return (
                                        <div key={v.id} className={`version-item ${isLatest ? "version-latest" : ""}`}>
                                            <div className="version-item-dot" />
                                            <div className="version-item-content">
                                                <div className="version-item-header">
                                                    <span className="version-number">v{v.version_number}</span>
                                                    <span className={`version-status ${v.status}`}>{v.status}</span>
                                                    <span className="version-time">{timeAgo(v.created_at)}</span>
                                                </div>
                                                {v.title && (
                                                    <div className="version-title">{v.title}</div>
                                                )}
                                                <div className="version-meta">
                                                    {v.word_count != null && (
                                                        <span>{v.word_count.toLocaleString()} words</span>
                                                    )}
                                                    <span className="version-hash">{v.body_hash.slice(0, 10)}</span>
                                                </div>
                                                <div className="version-actions">
                                                    {prevVersion && (
                                                        <button
                                                            className="version-diff-btn"
                                                            onClick={() => onDiff(
                                                                prevVersion.id,
                                                                v.id,
                                                                `v${prevVersion.version_number} → v${v.version_number}`
                                                            )}
                                                        >
                                                            diff v{prevVersion.version_number}→v{v.version_number}
                                                        </button>
                                                    )}
                                                    {!isLatest && (
                                                        <button
                                                            className="version-diff-btn"
                                                            onClick={() => onDiff(
                                                                v.id,
                                                                history.versions[0].id,
                                                                `v${v.version_number} → v${history.versions[0].version_number} (latest)`
                                                            )}
                                                        >
                                                            diff vs latest
                                                        </button>
                                                    )}
                                                    <button
                                                        className="version-pin-btn"
                                                        onClick={() => handlePin(v.id)}
                                                        title="Pin this version locally"
                                                    >
                                                        pin
                                                    </button>
                                                </div>
                                            </div>
                                        </div>
                                    );
                                })}
                            </div>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
}
