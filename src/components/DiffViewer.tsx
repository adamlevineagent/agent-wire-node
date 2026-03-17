import { useMemo } from "react";
import type { DiffHunk } from "./Dashboard";

interface DiffViewerProps {
    hunks: DiffHunk[];
    title: string;
    loading: boolean;
    onClose: () => void;
}

export function DiffViewer({ hunks, title, loading, onClose }: DiffViewerProps) {
    const stats = useMemo(() => {
        let insertions = 0;
        let deletions = 0;
        let insertWords = 0;
        let deleteWords = 0;
        for (const h of hunks) {
            if (h.tag === "insert") {
                insertions++;
                insertWords += h.content.split(/\s+/).filter(Boolean).length;
            } else if (h.tag === "delete") {
                deletions++;
                deleteWords += h.content.split(/\s+/).filter(Boolean).length;
            }
        }
        return { insertions, deletions, insertWords, deleteWords };
    }, [hunks]);

    const isEmpty = hunks.length === 0 || (hunks.length === 1 && hunks[0].tag === "equal");

    return (
        <div className="diff-overlay" onClick={onClose}>
            <div className="diff-panel" onClick={(e) => e.stopPropagation()}>
                <div className="diff-header">
                    <div>
                        <h3>Diff</h3>
                        <span className="diff-title">{title}</span>
                    </div>
                    <div className="diff-stats">
                        {!loading && !isEmpty && (
                            <>
                                <span className="diff-stat-add">+{stats.insertWords} words</span>
                                <span className="diff-stat-del">-{stats.deleteWords} words</span>
                            </>
                        )}
                    </div>
                    <button className="diff-close-btn" onClick={onClose}>x</button>
                </div>

                <div className="diff-body">
                    {loading ? (
                        <div className="diff-loading">Computing word-level diff...</div>
                    ) : isEmpty ? (
                        <div className="diff-empty">No differences found. Documents are identical.</div>
                    ) : (
                        <div className="diff-content">
                            {hunks.map((hunk, i) => {
                                if (hunk.tag === "equal") {
                                    // For long equal sections, collapse the middle
                                    const words = hunk.content.split(/\s+/);
                                    if (words.length > 40) {
                                        const start = words.slice(0, 15).join(" ");
                                        const end = words.slice(-15).join(" ");
                                        return (
                                            <span key={i} className="diff-equal">
                                                {start}
                                                <span className="diff-collapsed">
                                                    {" "}[...{words.length - 30} words...]{" "}
                                                </span>
                                                {end}
                                            </span>
                                        );
                                    }
                                    return <span key={i} className="diff-equal">{hunk.content}</span>;
                                }
                                if (hunk.tag === "delete") {
                                    return <span key={i} className="diff-delete">{hunk.content}</span>;
                                }
                                if (hunk.tag === "insert") {
                                    return <span key={i} className="diff-insert">{hunk.content}</span>;
                                }
                                return null;
                            })}
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}
