import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { LinkedFolder } from "./Dashboard";

type SyncDirection = "Upload" | "Download" | "Both";

interface CorpusInfo {
    slug: string;
    title: string;
    visibility: string | null;
    document_count: number | null;
    has_paid_documents?: boolean;
}

interface FolderLinkProps {
    linkedFolders: Record<string, LinkedFolder>;
    onLinked: () => void;
}

export function FolderLink({ linkedFolders, onLinked }: FolderLinkProps) {
    const [selectedPath, setSelectedPath] = useState("");
    const [direction, setDirection] = useState<SyncDirection>("Download");
    const [selectedCorpusSlug, setSelectedCorpusSlug] = useState("");
    const [linking, setLinking] = useState(false);
    const [unlinking, setUnlinking] = useState<string | null>(null);
    const [error, setError] = useState("");
    const [showForm, setShowForm] = useState(false);

    // Corpus fetching state
    const [myCorpora, setMyCorpora] = useState<CorpusInfo[]>([]);
    const [publicCorpora, setPublicCorpora] = useState<CorpusInfo[]>([]);
    const [loadingCorpora, setLoadingCorpora] = useState(false);
    const [corporaError, setCorporaError] = useState("");

    // Create-new corpus state
    const [showCreateNew, setShowCreateNew] = useState(false);
    const [newSlug, setNewSlug] = useState("");
    const [newTitle, setNewTitle] = useState("");
    const [creating, setCreating] = useState(false);

    const fetchCorpora = useCallback(async (dir: SyncDirection) => {
        setLoadingCorpora(true);
        setCorporaError("");
        try {
            const mine: CorpusInfo[] = await invoke("list_my_corpora");
            setMyCorpora(mine);

            if (dir === "Download" || dir === "Both") {
                const pub: CorpusInfo[] = await invoke("list_public_corpora");
                setPublicCorpora(pub);
            } else {
                setPublicCorpora([]);
            }
        } catch (err: any) {
            setCorporaError(err?.toString() || "Failed to load corpora");
        } finally {
            setLoadingCorpora(false);
        }
    }, []);

    // Fetch corpora when the form opens
    useEffect(() => {
        if (showForm) {
            fetchCorpora(direction);
        }
    }, [showForm]); // eslint-disable-line react-hooks/exhaustive-deps

    // Re-fetch when direction changes (while form is open)
    useEffect(() => {
        if (showForm) {
            setSelectedCorpusSlug("");
            setShowCreateNew(false);
            fetchCorpora(direction);
        }
    }, [direction]); // eslint-disable-line react-hooks/exhaustive-deps

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

    const handleSelectCorpus = useCallback((slug: string) => {
        setSelectedCorpusSlug(slug);
        setShowCreateNew(false);
        setError("");
    }, []);

    const handleCreateCorpus = useCallback(async () => {
        const trimmedSlug = newSlug.trim();
        const trimmedTitle = newTitle.trim();
        if (!trimmedSlug || !trimmedTitle) {
            setError("Both slug and title are required to create a corpus");
            return;
        }

        setCreating(true);
        setError("");

        try {
            const created: CorpusInfo = await invoke("create_corpus", {
                slug: trimmedSlug,
                title: trimmedTitle,
            });
            // Add to my corpora list and select it
            setMyCorpora((prev) => [...prev, created]);
            setSelectedCorpusSlug(created.slug);
            setShowCreateNew(false);
            setNewSlug("");
            setNewTitle("");
        } catch (err: any) {
            setError(err?.toString() || "Failed to create corpus");
        } finally {
            setCreating(false);
        }
    }, [newSlug, newTitle]);

    const handleLink = useCallback(async () => {
        if (!selectedPath) {
            setError("Please select a folder");
            return;
        }
        if (!selectedCorpusSlug) {
            setError("Please select a corpus");
            return;
        }

        setLinking(true);
        setError("");

        try {
            await invoke("link_folder", {
                folderPath: selectedPath,
                corpusSlug: selectedCorpusSlug,
                direction,
            });
            setSelectedPath("");
            setSelectedCorpusSlug("");
            setDirection("Download");
            setShowForm(false);
            setShowCreateNew(false);
            setMyCorpora([]);
            setPublicCorpora([]);
            onLinked();
        } catch (err: any) {
            setError(err?.toString() || "Failed to link folder");
        } finally {
            setLinking(false);
        }
    }, [selectedPath, selectedCorpusSlug, direction, onLinked]);

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

    const handleCancel = useCallback(() => {
        setShowForm(false);
        setSelectedPath("");
        setSelectedCorpusSlug("");
        setDirection("Download");
        setError("");
        setShowCreateNew(false);
        setNewSlug("");
        setNewTitle("");
        setMyCorpora([]);
        setPublicCorpora([]);
        setCorporaError("");
    }, []);

    const folderEntries = Object.entries(linkedFolders);

    const canLink = selectedPath && selectedCorpusSlug && !linking;

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
                                    {linked.direction === "Upload" ? "\u2191 Upload" : linked.direction === "Download" ? "\u2193 Download" : "\u21C5 Both"}
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
                    {/* Step 1: Pick folder */}
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

                    {/* Step 2: Pick direction */}
                    <div className="direction-toggle-row">
                        <button
                            type="button"
                            className={`direction-btn ${direction === "Upload" ? "active" : ""}`}
                            onClick={() => setDirection("Upload")}
                        >
                            Upload
                        </button>
                        <button
                            type="button"
                            className={`direction-btn ${direction === "Download" ? "active" : ""}`}
                            onClick={() => setDirection("Download")}
                        >
                            Download
                        </button>
                        <button
                            type="button"
                            className={`direction-btn ${direction === "Both" ? "active" : ""}`}
                            onClick={() => setDirection("Both")}
                        >
                            Both
                        </button>
                    </div>

                    {/* Step 3: Pick corpus */}
                    <div className="corpus-picker">
                        {loadingCorpora ? (
                            <div className="corpus-section">Loading corpora...</div>
                        ) : corporaError ? (
                            <div className="folder-link-error">{corporaError}</div>
                        ) : (
                            <>
                                {/* Your Corpora section */}
                                <div className="corpus-section">Your Corpora</div>
                                {myCorpora.length > 0 ? (
                                    myCorpora.map((c) => (
                                        <div
                                            key={c.slug}
                                            className={`corpus-option ${selectedCorpusSlug === c.slug ? "selected" : ""}`}
                                            onClick={() => handleSelectCorpus(c.slug)}
                                        >
                                            <div className="corpus-option-title">{c.title}</div>
                                            <div className="corpus-option-meta">
                                                {c.slug}
                                                {c.document_count != null && ` \u00b7 ${c.document_count} doc${c.document_count !== 1 ? "s" : ""}`}
                                            </div>
                                            <span className="corpus-badge free">Free</span>
                                        </div>
                                    ))
                                ) : (
                                    <div className="corpus-option-meta" style={{ padding: "6px 0" }}>
                                        {direction === "Upload"
                                            ? "No corpora yet \u2014 create one below"
                                            : "No corpora of your own"}
                                    </div>
                                )}

                                {/* Public Corpora section (Download only) */}
                                {direction === "Download" && publicCorpora.length > 0 && (
                                    <>
                                        <div className="corpus-section">Public Corpora</div>
                                        {publicCorpora.map((c) => (
                                            <div
                                                key={c.slug}
                                                className={`corpus-option ${selectedCorpusSlug === c.slug ? "selected" : ""}`}
                                                onClick={() => handleSelectCorpus(c.slug)}
                                            >
                                                <div className="corpus-option-title">{c.title}</div>
                                                <div className="corpus-option-meta">
                                                    {c.slug}
                                                    {c.document_count != null && ` \u00b7 ${c.document_count} doc${c.document_count !== 1 ? "s" : ""}`}
                                                </div>
                                                <span className="corpus-badge paid">
                                                    1 credit/doc + content fees
                                                </span>
                                            </div>
                                        ))}
                                    </>
                                )}

                                {/* Create New Corpus (Upload only) */}
                                {direction === "Upload" && (
                                    <div className="create-corpus-section">
                                        {!showCreateNew ? (
                                            <button
                                                type="button"
                                                className="create-corpus-btn"
                                                onClick={() => {
                                                    setShowCreateNew(true);
                                                    setSelectedCorpusSlug("");
                                                }}
                                            >
                                                + Create New Corpus
                                            </button>
                                        ) : (
                                            <>
                                                <div className="corpus-input-row">
                                                    <input
                                                        type="text"
                                                        value={newSlug}
                                                        onChange={(e) => setNewSlug(e.target.value)}
                                                        placeholder="Corpus slug (e.g. my-research)"
                                                        className="corpus-input"
                                                    />
                                                </div>
                                                <div className="corpus-input-row">
                                                    <input
                                                        type="text"
                                                        value={newTitle}
                                                        onChange={(e) => setNewTitle(e.target.value)}
                                                        placeholder="Corpus title (e.g. My Research Notes)"
                                                        className="corpus-input"
                                                    />
                                                </div>
                                                <div className="folder-link-actions">
                                                    <button
                                                        type="button"
                                                        className="link-btn"
                                                        onClick={handleCreateCorpus}
                                                        disabled={creating || !newSlug.trim() || !newTitle.trim()}
                                                    >
                                                        {creating ? "Creating..." : "Create"}
                                                    </button>
                                                    <button
                                                        type="button"
                                                        className="cancel-link-btn"
                                                        onClick={() => {
                                                            setShowCreateNew(false);
                                                            setNewSlug("");
                                                            setNewTitle("");
                                                        }}
                                                    >
                                                        Cancel
                                                    </button>
                                                </div>
                                            </>
                                        )}
                                    </div>
                                )}
                            </>
                        )}
                    </div>

                    {error && <div className="folder-link-error">{error}</div>}

                    <div className="folder-link-actions">
                        <button
                            className="link-btn"
                            onClick={handleLink}
                            disabled={!canLink}
                        >
                            {linking ? "Linking..." : "Link"}
                        </button>
                        <button
                            className="cancel-link-btn"
                            onClick={handleCancel}
                        >
                            Cancel
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
