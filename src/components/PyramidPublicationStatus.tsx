import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ─── Types ───────────────────────────────────────────────────────────────────

interface PyramidPublicationInfo {
    slug: string;
    node_count: number;
    unpublished_count: number;
    last_published_build_id: string | null;
    current_build_id: string | null;
    last_built_at: string | null;
}

type PublishingState = "idle" | "publishing";

// ─── Component ───────────────────────────────────────────────────────────────

export function PyramidPublicationStatus() {
    const [pyramids, setPyramids] = useState<PyramidPublicationInfo[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [publishingSlug, setPublishingSlug] = useState<string | null>(null);
    const [autoPublishSlugs, setAutoPublishSlugs] = useState<Set<string>>(new Set());
    const [lastPublishResult, setLastPublishResult] = useState<Record<string, { success: boolean; message: string }>>({});

    // ─── Fetch publication status ────────────────────────────────────────────

    const fetchStatus = useCallback(async () => {
        try {
            const data: PyramidPublicationInfo[] = await invoke("pyramid_get_publication_status");
            setPyramids(data);
            setError(null);
        } catch (err) {
            console.error("Failed to fetch publication status:", err);
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        fetchStatus();
        // Refresh every 15 seconds to pick up build completions
        const interval = setInterval(fetchStatus, 15000);
        return () => clearInterval(interval);
    }, [fetchStatus]);

    // Listen for build-complete events to auto-refresh
    useEffect(() => {
        const unlisten = listen("pyramid-build-complete", () => {
            fetchStatus();
        });
        return () => { unlisten.then(fn => fn()); };
    }, [fetchStatus]);

    // ─── Publish Now ─────────────────────────────────────────────────────────

    const handlePublishNow = useCallback(async (slug: string) => {
        setPublishingSlug(slug);
        setLastPublishResult(prev => {
            const next = { ...prev };
            delete next[slug];
            return next;
        });
        try {
            await invoke("pyramid_publish", { slug });
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: true, message: "Published successfully" },
            }));
            // Refresh status after publish
            await fetchStatus();
        } catch (err) {
            console.error("Publish failed:", err);
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: false, message: String(err) },
            }));
        } finally {
            setPublishingSlug(null);
        }
    }, [fetchStatus]);

    // ─── Auto-publish toggle ─────────────────────────────────────────────────
    // Stored client-side for now; WS-ONLINE-A backend timer reads from
    // PyramidSyncState which is populated at startup. A full round-trip
    // IPC (set_auto_publish per slug) will be wired when the backend
    // PyramidSyncState is exposed to IPC. For now, toggling updates a
    // local set and logs intent.

    const handleAutoPublishToggle = useCallback((slug: string) => {
        setAutoPublishSlugs(prev => {
            const next = new Set(prev);
            if (next.has(slug)) {
                next.delete(slug);
            } else {
                next.add(slug);
            }
            return next;
        });
    }, []);

    // ─── Helpers ─────────────────────────────────────────────────────────────

    function publicationState(p: PyramidPublicationInfo): "unpublished" | "published" | "stale" {
        if (!p.last_published_build_id) return "unpublished";
        if (p.current_build_id && p.last_published_build_id !== p.current_build_id) return "stale";
        if (p.unpublished_count > 0) return "stale";
        return "published";
    }

    function statusDotClass(state: "unpublished" | "published" | "stale" | "publishing"): string {
        switch (state) {
            case "unpublished": return "pub-status-unpublished";
            case "published": return "pub-status-published";
            case "stale": return "pub-status-stale";
            case "publishing": return "pub-status-publishing";
        }
    }

    function statusLabel(state: "unpublished" | "published" | "stale" | "publishing"): string {
        switch (state) {
            case "unpublished": return "Never published";
            case "published": return "Up to date";
            case "stale": return "Unpublished changes";
            case "publishing": return "Publishing...";
        }
    }

    function statusIcon(state: "unpublished" | "published" | "stale" | "publishing"): string {
        switch (state) {
            case "unpublished": return "\u25CB";  // empty circle
            case "published": return "\u2713";    // checkmark
            case "stale": return "\u2191";        // up arrow
            case "publishing": return "\u21BB";   // rotating arrow
        }
    }

    // ─── Render ──────────────────────────────────────────────────────────────

    if (loading) {
        return (
            <div className="pyramid-pub-status">
                <div className="pyramid-pub-loading">Loading pyramid status...</div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="pyramid-pub-status">
                <div className="pyramid-pub-error">
                    Failed to load pyramid status: {error}
                    <button className="sync-button" onClick={fetchStatus} style={{ marginLeft: 8 }}>
                        Retry
                    </button>
                </div>
            </div>
        );
    }

    if (pyramids.length === 0) {
        return (
            <div className="pyramid-pub-status">
                <div className="cache-empty">
                    <p>No pyramids found</p>
                    <p className="empty-hint">
                        Create a pyramid workspace to start building and publishing knowledge
                    </p>
                </div>
            </div>
        );
    }

    return (
        <div className="pyramid-pub-status">
            {/* Header */}
            <div className="cache-header">
                <div>
                    <span className="cache-size">{pyramids.length}</span>
                    <span className="cache-label"> pyramid{pyramids.length !== 1 ? "s" : ""}</span>
                </div>
                <button
                    className="sync-button"
                    onClick={fetchStatus}
                    title="Refresh publication status"
                >
                    Refresh
                </button>
            </div>

            {/* Summary bar */}
            {(() => {
                const published = pyramids.filter(p => publicationState(p) === "published").length;
                const stale = pyramids.filter(p => publicationState(p) === "stale").length;
                const unpublished = pyramids.filter(p => publicationState(p) === "unpublished").length;
                return (published > 0 || stale > 0 || unpublished > 0) ? (
                    <div className="sync-summary-bar">
                        {published > 0 && (
                            <span className="sync-summary-chip in-sync">{published} published</span>
                        )}
                        {stale > 0 && (
                            <span className="sync-summary-chip pending">{stale} pending</span>
                        )}
                        {unpublished > 0 && (
                            <span className="sync-summary-chip skipped">{unpublished} never published</span>
                        )}
                    </div>
                ) : null;
            })()}

            {/* Pyramid list */}
            <div className="pyramid-pub-list">
                {pyramids.map((p) => {
                    const isPublishing = publishingSlug === p.slug;
                    const state = isPublishing ? "publishing" as const : publicationState(p);
                    const publishedCount = p.node_count - p.unpublished_count;
                    const result = lastPublishResult[p.slug];

                    return (
                        <div key={p.slug} className="pyramid-pub-item">
                            <div className="pyramid-pub-row">
                                {/* Status dot + slug name */}
                                <div className="pyramid-pub-info">
                                    <span
                                        className={`pyramid-pub-dot ${statusDotClass(state)}`}
                                        title={statusLabel(state)}
                                    >
                                        {statusIcon(state)}
                                    </span>
                                    <span className="pyramid-pub-slug">{p.slug}</span>
                                </div>

                                {/* Right side: counts + actions */}
                                <div className="pyramid-pub-actions">
                                    {/* Node counts */}
                                    <span className="pyramid-pub-count" title={`${publishedCount} of ${p.node_count} nodes published`}>
                                        {publishedCount}/{p.node_count} nodes
                                    </span>

                                    {/* Last built */}
                                    {p.last_built_at && (
                                        <span className="pyramid-pub-time" title={`Last built: ${p.last_built_at}`}>
                                            {new Date(p.last_built_at).toLocaleDateString()}
                                        </span>
                                    )}

                                    {/* Auto-publish toggle */}
                                    <label
                                        className="auto-sync-toggle"
                                        onClick={(e) => {
                                            e.stopPropagation();
                                            handleAutoPublishToggle(p.slug);
                                        }}
                                        title="Auto-publish when new builds complete"
                                    >
                                        <span className={`toggle-switch ${autoPublishSlugs.has(p.slug) ? "on" : ""}`}>
                                            <span className="toggle-knob" />
                                        </span>
                                        <span className="auto-sync-label">Auto</span>
                                    </label>

                                    {/* Publish Now button */}
                                    <button
                                        className="folder-publish-btn"
                                        onClick={() => handlePublishNow(p.slug)}
                                        disabled={isPublishing || p.node_count === 0}
                                        title={
                                            p.node_count === 0
                                                ? "No nodes to publish"
                                                : p.unpublished_count === 0 && state === "published"
                                                    ? "All nodes already published"
                                                    : `Publish ${p.unpublished_count} unpublished nodes`
                                        }
                                    >
                                        {isPublishing ? "Publishing..." : "Publish Now"}
                                    </button>
                                </div>
                            </div>

                            {/* Publish result feedback */}
                            {result && (
                                <div className={`pyramid-pub-result ${result.success ? "success" : "error"}`}>
                                    {result.message}
                                </div>
                            )}

                            {/* Build ID info */}
                            {p.last_published_build_id && (
                                <div className="pyramid-pub-build-info">
                                    <span className="file-hash" title={`Published build: ${p.last_published_build_id}`}>
                                        pub: {p.last_published_build_id.slice(0, 8)}
                                    </span>
                                    {p.current_build_id && p.current_build_id !== p.last_published_build_id && (
                                        <span className="file-hash" title={`Current build: ${p.current_build_id}`}>
                                            current: {p.current_build_id.slice(0, 8)}
                                        </span>
                                    )}
                                </div>
                            )}
                        </div>
                    );
                })}
            </div>
        </div>
    );
}
