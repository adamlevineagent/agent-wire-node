import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SyncState, CachedDocument, LinkedFolder } from "./Dashboard";
import { FolderLink } from "./FolderLink";

interface SyncStatusProps {
    syncState: SyncState | null;
    syncing: boolean;
    onSync: () => void;
}

export function SyncStatus({ syncState, syncing, onSync }: SyncStatusProps) {
    const [expandedFolder, setExpandedFolder] = useState<string | null>(null);

    const totalMB = syncState
        ? (syncState.total_size_bytes / (1024 * 1024)).toFixed(1)
        : "0";

    const docCount = syncState?.cached_documents?.length || 0;
    const linkedFolders = syncState?.linked_folders || {};
    const folderEntries = Object.entries(linkedFolders);

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
                    {syncing ? "Syncing..." : "Sync Now"}
                </button>
            </div>

            <div className="cache-meta">
                {docCount} document{docCount !== 1 ? "s" : ""} across {folderEntries.length} folder{folderEntries.length !== 1 ? "s" : ""}
                {syncState?.last_sync_at && (
                    <> -- Last sync: {new Date(syncState.last_sync_at).toLocaleString()}</>
                )}
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
                                            {linked.direction === "Upload" ? "\u2191 Upload" : "\u2193 Download"}
                                        </span>
                                    </div>
                                    <div className="folder-sync-meta">
                                        <span className="folder-doc-count">
                                            {corpusDocs.length} doc{corpusDocs.length !== 1 ? "s" : ""}
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
                                            corpusDocs.map((doc) => (
                                                <div key={doc.document_id} className="file-item">
                                                    <div className="file-info">
                                                        <span className="file-status-dot in-sync" title="In sync" />
                                                        <span className="file-path">
                                                            {doc.source_path}
                                                        </span>
                                                    </div>
                                                    <div className="file-meta">
                                                        <span className="file-size">
                                                            {(doc.file_size_bytes / 1024).toFixed(0)} KB
                                                        </span>
                                                        <span className="file-hash" title={doc.body_hash}>
                                                            {doc.body_hash.slice(0, 8)}
                                                        </span>
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

            {/* Cached documents list (if no linked folders) */}
            {folderEntries.length === 0 && docCount === 0 && (
                <div className="cache-empty">
                    <p>No folders linked yet</p>
                    <p className="empty-hint">
                        Link a local folder to a Wire corpus to start syncing documents
                    </p>
                </div>
            )}
        </div>
    );
}
