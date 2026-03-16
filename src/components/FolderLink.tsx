import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { LinkedFolder } from "./Dashboard";

type SyncDirection = "Upload" | "Download";

interface FolderLinkProps {
    linkedFolders: Record<string, LinkedFolder>;
    onLinked: () => void;
}

export function FolderLink({ linkedFolders, onLinked }: FolderLinkProps) {
    const [corpusSlug, setCorpusSlug] = useState("");
    const [selectedPath, setSelectedPath] = useState("");
    const [direction, setDirection] = useState<SyncDirection>("Download");
    const [linking, setLinking] = useState(false);
    const [unlinking, setUnlinking] = useState<string | null>(null);
    const [error, setError] = useState("");
    const [showForm, setShowForm] = useState(false);

    const handlePickFolder = useCallback(async () => {
        try {
            const dir = await open({ directory: true });
            if (dir) {
                setSelectedPath(typeof dir === "string" ? dir : String(dir));
            }
        } catch (err) {
            console.error("Folder picker failed:", err);
        }
    }, []);

    const handleLink = useCallback(async () => {
        if (!selectedPath || !corpusSlug.trim()) {
            setError("Both folder path and corpus slug are required");
            return;
        }

        setLinking(true);
        setError("");

        try {
            await invoke("link_folder", {
                folderPath: selectedPath,
                corpusSlug: corpusSlug.trim(),
                direction,
            });
            setSelectedPath("");
            setCorpusSlug("");
            setDirection("Download");
            setShowForm(false);
            onLinked();
        } catch (err: any) {
            setError(err?.toString() || "Failed to link folder");
        } finally {
            setLinking(false);
        }
    }, [selectedPath, corpusSlug, direction, onLinked]);

    const handleUnlink = useCallback(async (folderPath: string) => {
        setUnlinking(folderPath);
        try {
            await invoke("unlink_folder", { folderPath });
            onLinked();
        } catch (err) {
            console.error("Unlink failed:", err);
        } finally {
            setUnlinking(null);
        }
    }, [onLinked]);

    const folderEntries = Object.entries(linkedFolders);

    return (
        <div className="folder-link">
            {/* Currently linked folders */}
            {folderEntries.length > 0 && (
                <div className="linked-folders-list">
                    {folderEntries.map(([path, linked]) => (
                        <div key={path} className="linked-folder-item">
                            <div className="linked-folder-info">
                                <span className="linked-folder-path" title={path}>
                                    {path.length > 45 ? "..." + path.slice(-42) : path}
                                </span>
                                <span className="linked-folder-corpus">{linked.corpus_slug}</span>
                                <span className={`linked-folder-direction ${linked.direction.toLowerCase()}`}>
                                    {linked.direction === "Upload" ? "\u2191 Upload" : "\u2193 Download"}
                                </span>
                            </div>
                            <button
                                className="unlink-btn"
                                onClick={() => handleUnlink(path)}
                                disabled={unlinking === path}
                                title="Unlink folder"
                            >
                                {unlinking === path ? "..." : "x"}
                            </button>
                        </div>
                    ))}
                </div>
            )}

            {/* Add new folder link */}
            {!showForm ? (
                <button
                    className="add-folder-btn"
                    onClick={() => setShowForm(true)}
                >
                    + Link Folder
                </button>
            ) : (
                <div className="folder-link-form">
                    <div className="folder-picker-row">
                        <button
                            className="pick-folder-btn"
                            onClick={handlePickFolder}
                            type="button"
                        >
                            Choose Folder...
                        </button>
                        <span className="selected-path">
                            {selectedPath
                                ? (selectedPath.length > 35 ? "..." + selectedPath.slice(-32) : selectedPath)
                                : "No folder selected"}
                        </span>
                    </div>

                    <div className="corpus-input-row">
                        <input
                            type="text"
                            value={corpusSlug}
                            onChange={(e) => setCorpusSlug(e.target.value)}
                            placeholder="Corpus slug (e.g. my-research)"
                            className="corpus-input"
                        />
                    </div>

                    {/* Sync Direction Toggle */}
                    <div className="direction-toggle-row">
                        <button
                            type="button"
                            className={`direction-btn ${direction === "Upload" ? "active" : ""}`}
                            onClick={() => setDirection("Upload")}
                        >
                            Upload to Agent Wire Automatically
                        </button>
                        <button
                            type="button"
                            className={`direction-btn ${direction === "Download" ? "active" : ""}`}
                            onClick={() => setDirection("Download")}
                        >
                            Download from Agent Wire Automatically
                        </button>
                    </div>

                    {error && <div className="folder-link-error">{error}</div>}

                    <div className="folder-link-actions">
                        <button
                            className="link-btn"
                            onClick={handleLink}
                            disabled={linking || !selectedPath || !corpusSlug.trim()}
                        >
                            {linking ? "Linking..." : "Link"}
                        </button>
                        <button
                            className="cancel-link-btn"
                            onClick={() => {
                                setShowForm(false);
                                setSelectedPath("");
                                setCorpusSlug("");
                                setDirection("Download");
                                setError("");
                            }}
                        >
                            Cancel
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
