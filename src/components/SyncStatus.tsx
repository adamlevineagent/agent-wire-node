import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { SyncState, CachedDocument, LinkedFolder, FileStatus, VersionHistoryResponse, DiffHunk } from "./Dashboard";
import { FolderLink } from "./FolderLink";
import { VersionHistory } from "./VersionHistory";
import { DiffViewer } from "./DiffViewer";

interface SyncStatusProps {
    syncState: SyncState | null;
    syncing: boolean;
    onSync: () => void;
}

function statusDotClass(status: FileStatus): string {
    switch (status) {
        case "InSync": return "in-sync";
        case "NeedsPull": return "needs-pull";
        case "NeedsPush": return "needs-push";
        case "Pulling": return "pulling";
        case "Pushing": return "pushing";
        case "Skipped": return "skipped";
        case "Error": return "error";
        default: return "in-sync";
    }
}

function statusLabel(status: FileStatus): string {
    switch (status) {
        case "InSync": return "In sync";
        case "NeedsPull": return "Needs download";
        case "NeedsPush": return "Needs upload";
        case "Pulling": return "Downloading...";
        case "Pushing": return "Uploading...";
        case "Skipped": return "Skipped (already exists)";
        case "Error": return "Error";
        default: return "";
    }
}

function statusIcon(status: FileStatus): string {
    switch (status) {
        case "InSync": return "\u2713";      // checkmark
        case "NeedsPull": return "\u2193";    // down arrow
        case "NeedsPush": return "\u2191";    // up arrow
        case "Pulling": return "\u21BB";      // rotating arrow
        case "Pushing": return "\u21BB";      // rotating arrow
        case "Skipped": return "\u2014";      // em dash (skip)
        case "Error": return "\u2717";        // X mark
        default: return "";
    }
}

const INTERVAL_OPTIONS = [
    { label: "1 min", value: 60 },
    { label: "5 min", value: 300 },
    { label: "15 min", value: 900 },
    { label: "30 min", value: 1800 },
    { label: "1 hour", value: 3600 },
];

export function SyncStatus({ syncState, syncing, onSync }: SyncStatusProps) {
    const [expandedFolder, setExpandedFolder] = useState<string | null>(null);
    const [versionDoc, setVersionDoc] = useState<CachedDocument | null>(null);
    const [versionHistory, setVersionHistory] = useState<VersionHistoryResponse | null>(null);
    const [loadingVersions, setLoadingVersions] = useState(false);
    const [diffData, setDiffData] = useState<DiffHunk[] | null>(null);
    const [diffTitle, setDiffTitle] = useState("");
    const [loadingDiff, setLoadingDiff] = useState(false);

    const totalMB = syncState
        ? (syncState.total_size_bytes / (1024 * 1024)).toFixed(1)
        : "0";

    const docCount = syncState?.cached_documents?.length || 0;
    const linkedFolders = syncState?.linked_folders || {};
    const folderEntries = Object.entries(linkedFolders);
    const autoSyncEnabled = syncState?.auto_sync_enabled ?? false;
    const autoSyncInterval = syncState?.auto_sync_interval_secs ?? 900;
    const syncProgress = syncState?.sync_progress;

    // Group cached documents by corpus_slug
    const docsByCorpus: Record<string, CachedDocument[]> = {};
    for (const doc of syncState?.cached_documents || []) {
        if (!docsByCorpus[doc.corpus_slug]) {
            docsByCorpus[doc.corpus_slug] = [];
        }
        docsByCorpus[doc.corpus_slug].push(doc);
    }

    const handleFolderSync = useCallback(async () => {
        onSync();
    }, [onSync]);

    const handleAutoSyncToggle = useCallback(async () => {
        try {
            await invoke("set_auto_sync", {
                enabled: !autoSyncEnabled,
                intervalSecs: autoSyncInterval,
            });
        } catch (err) {
            console.error("Failed to set auto-sync:", err);
        }
    }, [autoSyncEnabled, autoSyncInterval]);

    const handleIntervalChange = useCallback(async (secs: number) => {
        try {
            await invoke("set_auto_sync", {
                enabled: autoSyncEnabled,
                intervalSecs: secs,
            });
        } catch (err) {
            console.error("Failed to set auto-sync interval:", err);
        }
    }, [autoSyncEnabled]);

    const handleOpenFile = useCallback(async (folderPath: string, sourcePath: string) => {
        try {
            const fullPath = folderPath + "/" + sourcePath;
            await invoke("open_file", { path: fullPath });
        } catch (err) {
            console.error("Failed to open file:", err);
        }
    }, []);

    const handleViewVersions = useCallback(async (doc: CachedDocument) => {
        if (!doc.document_id) return;
        setVersionDoc(doc);
        setLoadingVersions(true);
        setVersionHistory(null);
        try {
            const history = await invoke<VersionHistoryResponse>("fetch_document_versions", {
                documentId: doc.document_id,
            });
            setVersionHistory(history);
        } catch (err) {
            console.error("Failed to fetch versions:", err);
        } finally {
            setLoadingVersions(false);
        }
    }, []);

    const handleDiff = useCallback(async (oldDocId: string, newDocId: string, title: string) => {
        setLoadingDiff(true);
        setDiffTitle(title);
        setDiffData(null);
        try {
            const hunks = await invoke<DiffHunk[]>("compute_diff", {
                oldDocId,
                newDocId,
            });
            setDiffData(hunks);
        } catch (err: any) {
            console.error("Failed to compute diff:", err);
            setDiffData([]);
        } finally {
            setLoadingDiff(false);
        }
    }, []);

    const handlePublishDoc = useCallback(async (docId: string, sourcePath: string) => {
        try {
            await invoke("update_document_status", { documentId: docId, status: "published" });
            onSync(); // refresh state from backend
        } catch (err) {
            console.error("Failed to publish:", err);
            alert(`Failed to publish ${sourcePath}: ${err}`);
        }
    }, [syncState, onSync]);

    const [bulkPublishing, setBulkPublishing] = useState<string | null>(null);
    const [bulkResults, setBulkResults] = useState<Record<string, { published: number; errors: number; total: number }>>({});
    const [bulkProgress, setBulkProgress] = useState<Record<string, { published: number; errors: number; total: number; batch: number }>>({});

    // Listen for bulk-publish-progress events from Rust
    useEffect(() => {
        const unlisten = listen<{ corpus_slug: string; published: number; errors: number; total: number; batch: number }>(
            "bulk-publish-progress",
            (event) => {
                setBulkProgress(prev => ({ ...prev, [event.payload.corpus_slug]: event.payload }));
            }
        );
        return () => { unlisten.then(fn => fn()); };
    }, []);

    const handleBulkPublish = useCallback(async (corpusSlug: string) => {
        if (!confirm(`Publish ALL draft documents in ${corpusSlug}? This makes them immutable.`)) return;
        setBulkPublishing(corpusSlug);
        setBulkResults(prev => { const next = { ...prev }; delete next[corpusSlug]; return next; });
        setBulkProgress(prev => { const next = { ...prev }; delete next[corpusSlug]; return next; });
        try {
            const result = await invoke<{ published: number; errors: number; total: number }>(
                "bulk_publish", { corpusSlug }
            );
            setBulkResults(prev => ({ ...prev, [corpusSlug]: result }));
            onSync(); // refresh
        } catch (err) {
            console.error("Bulk publish failed:", err);
            alert(`Bulk publish failed: ${err}`);
        } finally {
            setBulkPublishing(null);
            setBulkProgress(prev => { const next = { ...prev }; delete next[corpusSlug]; return next; });
        }
    }, [onSync]);

    const handleCloseVersions = useCallback(() => {
        setVersionDoc(null);
        setVersionHistory(null);
    }, []);

    const handleCloseDiff = useCallback(() => {
        setDiffData(null);
        setDiffTitle("");
    }, []);

    // Count files by status
    const statusCounts = { inSync: 0, pending: 0, active: 0, skipped: 0, errors: 0 };
    for (const doc of syncState?.cached_documents || []) {
        if (doc.sync_status === "InSync") statusCounts.inSync++;
        else if (doc.sync_status === "NeedsPull" || doc.sync_status === "NeedsPush") statusCounts.pending++;
        else if (doc.sync_status === "Pulling" || doc.sync_status === "Pushing") statusCounts.active++;
        else if (doc.sync_status === "Skipped") statusCounts.skipped++;
        else if (doc.sync_status === "Error") statusCounts.errors++;
    }

    return (
        <div className="sync-status">
            {/* Header */}
            <div className="cache-header">
                <div>
                    <span className="cache-size">{totalMB} MB</span>
                    <span className="cache-label"> synced</span>
                </div>
                <button
                    className="sync-button"
                    onClick={handleFolderSync}
                    disabled={syncing}
                >
                    {syncing ? (syncProgress || "Syncing...") : "Sync Now"}
                </button>
            </div>

            <div className="cache-meta">
                {docCount} document{docCount !== 1 ? "s" : ""} across {folderEntries.length} folder{folderEntries.length !== 1 ? "s" : ""}
                {syncState?.last_sync_at && (
                    <> -- Last sync: {new Date(syncState.last_sync_at).toLocaleString()}</>
                )}
            </div>

            {/* Sync status summary bar */}
            {docCount > 0 && (statusCounts.pending > 0 || statusCounts.active > 0 || statusCounts.skipped > 0 || statusCounts.errors > 0) && (
                <div className="sync-summary-bar">
                    {statusCounts.inSync > 0 && (
                        <span className="sync-summary-chip in-sync">{statusCounts.inSync} synced</span>
                    )}
                    {statusCounts.pending > 0 && (
                        <span className="sync-summary-chip pending">{statusCounts.pending} pending</span>
                    )}
                    {statusCounts.active > 0 && (
                        <span className="sync-summary-chip active">{statusCounts.active} transferring</span>
                    )}
                    {statusCounts.skipped > 0 && (
                        <span className="sync-summary-chip skipped">{statusCounts.skipped} skipped</span>
                    )}
                    {statusCounts.errors > 0 && (
                        <span className="sync-summary-chip error">{statusCounts.errors} failed</span>
                    )}
                </div>
            )}

            {/* Auto-sync settings */}
            <div className="auto-sync-settings">
                <div className="auto-sync-row">
                    <label className="auto-sync-toggle" onClick={handleAutoSyncToggle}>
                        <span className={`toggle-switch ${autoSyncEnabled ? "on" : ""}`}>
                            <span className="toggle-knob" />
                        </span>
                        <span className="auto-sync-label">Auto-sync</span>
                    </label>
                    {autoSyncEnabled && (
                        <div className="auto-sync-interval">
                            {INTERVAL_OPTIONS.map((opt) => (
                                <button
                                    key={opt.value}
                                    className={`interval-btn ${autoSyncInterval === opt.value ? "active" : ""}`}
                                    onClick={() => handleIntervalChange(opt.value)}
                                >
                                    {opt.label}
                                </button>
                            ))}
                        </div>
                    )}
                </div>
            </div>

            {/* Folder Link Manager */}
            <FolderLink
                linkedFolders={linkedFolders}
                onLinked={() => {
                    onSync();
                }}
            />

            {/* Per-folder sync state */}
            {folderEntries.length > 0 && (
                <div className="folder-sync-list">
                    <div className="section-header">Linked Folders</div>
                    {folderEntries.map(([folderPath, linked]) => {
                        const corpusDocs = docsByCorpus[linked.corpus_slug] || [];
                        const isExpanded = expandedFolder === folderPath;
                        const folderSize = corpusDocs.reduce((sum, d) => sum + d.file_size_bytes, 0);
                        const folderSynced = corpusDocs.filter(d => d.sync_status === "InSync").length;
                        const folderTotal = corpusDocs.length;

                        return (
                            <div key={folderPath} className="folder-sync-item">
                                <div
                                    className="folder-sync-header"
                                    onClick={() => setExpandedFolder(isExpanded ? null : folderPath)}
                                >
                                    <div className="folder-sync-info">
                                        <span className="folder-path" title={folderPath}>
                                            {folderPath.length > 40
                                                ? "..." + folderPath.slice(-37)
                                                : folderPath}
                                        </span>
                                        <span className="folder-corpus">{linked.corpus_slug}</span>
                                        <span className={`folder-direction ${linked.direction.toLowerCase()}`}>
                                            {linked.direction === "Upload" ? "\u2191 Upload" : linked.direction === "Download" ? "\u2193 Download" : "\u21C5 Both"}
                                        </span>
                                    </div>
                                    <div className="folder-sync-meta">
                                        {(() => {
                                            const draftCount = corpusDocs.filter(d => d.document_id && d.document_status !== "published" && d.document_status !== "retracted").length;
                                            const isPublishing = bulkPublishing === linked.corpus_slug;
                                            const progress = bulkProgress[linked.corpus_slug];
                                            if (isPublishing && progress) {
                                                const pct = Math.round((progress.published + progress.errors) / progress.total * 100);
                                                return (
                                                    <div className="bulk-progress" onClick={(e) => e.stopPropagation()}>
                                                        <div className="bulk-progress-bar">
                                                            <div
                                                                className="bulk-progress-fill"
                                                                style={{ width: `${pct}%` }}
                                                            />
                                                        </div>
                                                        <span className="bulk-progress-label">
                                                            {progress.published}/{progress.total} published
                                                            {progress.errors > 0 && ` (${progress.errors} failed)`}
                                                        </span>
                                                    </div>
                                                );
                                            }
                                            if (isPublishing) {
                                                return (
                                                    <span className="bulk-progress-label" onClick={(e) => e.stopPropagation()}>
                                                        Starting publish...
                                                    </span>
                                                );
                                            }
                                            return draftCount > 0 && !syncing ? (
                                                <button
                                                    className="folder-publish-btn"
                                                    onClick={(e) => {
                                                        e.stopPropagation();
                                                        handleBulkPublish(linked.corpus_slug);
                                                    }}
                                                    title={`Publish all ${draftCount} draft documents`}
                                                >
                                                    Publish {draftCount} drafts
                                                </button>
                                            ) : null;
                                        })()}
                                        {bulkResults[linked.corpus_slug] && (
                                            <span className="bulk-result" title={bulkResults[linked.corpus_slug].errors > 0 ? "Some failed" : "All published"}>
                                                {bulkResults[linked.corpus_slug].published}/{bulkResults[linked.corpus_slug].total} published
                                            </span>
                                        )}
                                        <span className="folder-doc-count">
                                            {folderSynced === folderTotal
                                                ? `${folderTotal} doc${folderTotal !== 1 ? "s" : ""}`
                                                : `${folderSynced}/${folderTotal} synced`
                                            }
                                        </span>
                                        <span className="folder-size">
                                            {(folderSize / (1024 * 1024)).toFixed(1)} MB
                                        </span>
                                        <span className="folder-expand">
                                            {isExpanded ? "v" : ">"}
                                        </span>
                                    </div>
                                </div>

                                {/* Expanded: per-file diff status */}
                                {isExpanded && (
                                    <div className="folder-files">
                                        {corpusDocs.length === 0 ? (
                                            <div className="folder-files-empty">
                                                No documents synced yet. Click "Sync Now" to start.
                                            </div>
                                        ) : (
                                            corpusDocs
                                                .sort((a, b) => {
                                                    const order: Record<string, number> = {
                                                        Pulling: 0, Pushing: 0,
                                                        NeedsPull: 1, NeedsPush: 1,
                                                        Error: 2,
                                                        Skipped: 3,
                                                        InSync: 4,
                                                    };
                                                    return (order[a.sync_status] ?? 3) - (order[b.sync_status] ?? 3);
                                                })
                                                .map((doc) => (
                                                <div key={doc.source_path} className={`file-item ${doc.sync_status === "Pulling" || doc.sync_status === "Pushing" ? "file-active" : ""}`}>
                                                    <div className="file-info">
                                                        <span
                                                            className={`file-status-dot ${statusDotClass(doc.sync_status)}`}
                                                            title={doc.error_message || statusLabel(doc.sync_status)}
                                                        >
                                                            {statusIcon(doc.sync_status)}
                                                        </span>
                                                        <span
                                                            className="file-path file-path-clickable"
                                                            onClick={() => handleOpenFile(folderPath, doc.source_path)}
                                                            title="Click to open file"
                                                        >
                                                            {doc.source_path}
                                                        </span>
                                                    </div>
                                                    <div className="file-meta">
                                                        {doc.file_size_bytes > 0 && (
                                                            <span className="file-size">
                                                                {(doc.file_size_bytes / 1024).toFixed(0)} KB
                                                            </span>
                                                        )}
                                                        {doc.sync_status === "Error" && doc.error_message && (
                                                            <span className="file-error" title={doc.error_message}>
                                                                failed
                                                            </span>
                                                        )}
                                                        {doc.document_id && doc.document_status !== "published" && doc.document_status !== "retracted" && (
                                                            <button
                                                                className="file-publish-btn"
                                                                onClick={(e) => {
                                                                    e.stopPropagation();
                                                                    handlePublishDoc(doc.document_id, doc.source_path);
                                                                }}
                                                                title="Publish this document (makes body immutable)"
                                                            >
                                                                publish
                                                            </button>
                                                        )}
                                                        {doc.document_id && doc.document_status === "published" && (
                                                            <span className="file-status-badge published" title="Published">pub</span>
                                                        )}
                                                        {doc.document_id && (
                                                            <button
                                                                className="file-versions-btn"
                                                                onClick={(e) => {
                                                                    e.stopPropagation();
                                                                    handleViewVersions(doc);
                                                                }}
                                                                title="View version history"
                                                            >
                                                                history
                                                            </button>
                                                        )}
                                                        {doc.body_hash && (
                                                            <span className="file-hash" title={doc.body_hash}>
                                                                {doc.body_hash.slice(0, 8)}
                                                            </span>
                                                        )}
                                                    </div>
                                                </div>
                                            ))
                                        )}
                                    </div>
                                )}
                            </div>
                        );
                    })}
                </div>
            )}

            {/* Empty state */}
            {folderEntries.length === 0 && docCount === 0 && (
                <div className="cache-empty">
                    <p>No folders linked yet</p>
                    <p className="empty-hint">
                        Link a local folder to a Wire corpus to start syncing documents
                    </p>
                </div>
            )}

            {/* Version History Panel */}
            {versionDoc && (
                <VersionHistory
                    document={versionDoc}
                    history={versionHistory}
                    loading={loadingVersions}
                    onClose={handleCloseVersions}
                    onDiff={handleDiff}
                />
            )}

            {/* Diff Viewer Panel */}
            {diffData !== null && (
                <DiffViewer
                    hunks={diffData}
                    title={diffTitle}
                    loading={loadingDiff}
                    onClose={handleCloseDiff}
                />
            )}
        </div>
    );
}
