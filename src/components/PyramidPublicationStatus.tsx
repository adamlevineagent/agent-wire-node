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
    pinned: boolean;
    source_tunnel_url: string | null;
}

type PublishingState = "idle" | "publishing";
type AccessTier = "public" | "circle-scoped" | "priced" | "embargoed";

interface AccessTierInfo {
    access_tier: AccessTier;
    access_price: number | null;
    allowed_circles: string[] | null;
    cached_emergent_price: number | null;
}

// ─── Component ───────────────────────────────────────────────────────────────

export function PyramidPublicationStatus() {
    const [pyramids, setPyramids] = useState<PyramidPublicationInfo[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [publishingSlug, setPublishingSlug] = useState<string | null>(null);
    const [autoPublishSlugs, setAutoPublishSlugs] = useState<Set<string>>(new Set());
    const [lastPublishResult, setLastPublishResult] = useState<Record<string, { success: boolean; message: string }>>({});

    // ─── WS-ONLINE-E: Access Tier State ─────────────────────────────────────
    const [accessTiers, setAccessTiers] = useState<Record<string, AccessTierInfo>>({});
    const [expandedAccessSlug, setExpandedAccessSlug] = useState<string | null>(null);
    const [accessTierDraft, setAccessTierDraft] = useState<{
        tier: AccessTier;
        price: string;
        circles: string;
    }>({ tier: "public", price: "", circles: "" });
    const [savingAccessTier, setSavingAccessTier] = useState(false);

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

    // ─── WS-ONLINE-E: Access tier management ──────────────────────────────────

    const fetchAccessTier = useCallback(async (slug: string) => {
        try {
            const data = await invoke<AccessTierInfo>("pyramid_get_access_tier", { slug });
            setAccessTiers(prev => ({ ...prev, [slug]: data }));
            return data;
        } catch (err) {
            console.error("Failed to fetch access tier for", slug, err);
            return null;
        }
    }, []);

    const handleExpandAccessTier = useCallback(async (slug: string) => {
        if (expandedAccessSlug === slug) {
            setExpandedAccessSlug(null);
            return;
        }
        const data = await fetchAccessTier(slug);
        if (data) {
            setAccessTierDraft({
                tier: data.access_tier,
                price: data.access_price != null ? String(data.access_price) : "",
                circles: data.allowed_circles ? JSON.stringify(data.allowed_circles) : "",
            });
        }
        setExpandedAccessSlug(slug);
    }, [expandedAccessSlug, fetchAccessTier]);

    const handleSaveAccessTier = useCallback(async (slug: string) => {
        setSavingAccessTier(true);
        try {
            const price = accessTierDraft.price.trim() === "" ? null : parseInt(accessTierDraft.price, 10);
            const circles = accessTierDraft.circles.trim() === "" ? null : accessTierDraft.circles.trim();

            if (price !== null && isNaN(price)) {
                setLastPublishResult(prev => ({
                    ...prev,
                    [slug]: { success: false, message: "Price must be a number" },
                }));
                return;
            }

            await invoke("pyramid_set_access_tier", {
                slug,
                tier: accessTierDraft.tier,
                price,
                circles,
            });

            await fetchAccessTier(slug);
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: true, message: `Access tier set to ${accessTierDraft.tier}` },
            }));
        } catch (err) {
            console.error("Failed to save access tier:", err);
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: false, message: String(err) },
            }));
        } finally {
            setSavingAccessTier(false);
        }
    }, [accessTierDraft, fetchAccessTier]);

    // ─── WS-ONLINE-D: Pinning actions ─────────────────────────────────────────

    const [unpinningSlug, setUnpinningSlug] = useState<string | null>(null);
    const [refreshingSlug, setRefreshingSlug] = useState<string | null>(null);

    const handleUnpin = useCallback(async (slug: string) => {
        setUnpinningSlug(slug);
        try {
            await invoke("pyramid_unpin", { slug });
            await fetchStatus();
        } catch (err) {
            console.error("Unpin failed:", err);
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: false, message: `Unpin failed: ${String(err)}` },
            }));
        } finally {
            setUnpinningSlug(null);
        }
    }, [fetchStatus]);

    const handleRefreshPinned = useCallback(async (slug: string, tunnelUrl: string) => {
        setRefreshingSlug(slug);
        try {
            await invoke("pyramid_pin_remote", { tunnelUrl: tunnelUrl, slug });
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: true, message: "Refreshed from remote" },
            }));
            await fetchStatus();
        } catch (err) {
            console.error("Refresh pinned failed:", err);
            setLastPublishResult(prev => ({
                ...prev,
                [slug]: { success: false, message: `Refresh failed: ${String(err)}` },
            }));
        } finally {
            setRefreshingSlug(null);
        }
    }, [fetchStatus]);

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
                const pinned = pyramids.filter(p => p.pinned).length;
                return (published > 0 || stale > 0 || unpublished > 0 || pinned > 0) ? (
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
                        {pinned > 0 && (
                            <span className="sync-summary-chip in-sync">{pinned} pinned</span>
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
                                {/* Status dot + slug name + pinned badge */}
                                <div className="pyramid-pub-info">
                                    <span
                                        className={`pyramid-pub-dot ${statusDotClass(state)}`}
                                        title={statusLabel(state)}
                                    >
                                        {statusIcon(state)}
                                    </span>
                                    <span className="pyramid-pub-slug">{p.slug}</span>
                                    {p.pinned && (
                                        <span
                                            className="pyramid-pinned-badge"
                                            title={p.source_tunnel_url ? `Pinned from ${p.source_tunnel_url}` : "Pinned"}
                                        >
                                            pinned
                                        </span>
                                    )}
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

                                    {/* Publish Now button (for local pyramids) */}
                                    {!p.pinned && (
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
                                    )}

                                    {/* WS-ONLINE-D: Pinned pyramid actions */}
                                    {p.pinned && p.source_tunnel_url && (
                                        <button
                                            className="folder-publish-btn"
                                            onClick={() => handleRefreshPinned(p.slug, p.source_tunnel_url!)}
                                            disabled={refreshingSlug === p.slug}
                                            title={`Refresh from ${p.source_tunnel_url}`}
                                        >
                                            {refreshingSlug === p.slug ? "Refreshing..." : "Refresh Now"}
                                        </button>
                                    )}
                                    {p.pinned && (
                                        <button
                                            className="folder-publish-btn pyramid-unpin-btn"
                                            onClick={() => handleUnpin(p.slug)}
                                            disabled={unpinningSlug === p.slug}
                                            title="Unpin (node data will be preserved)"
                                        >
                                            {unpinningSlug === p.slug ? "Unpinning..." : "Unpin"}
                                        </button>
                                    )}
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

                            {/* WS-ONLINE-D: Pinned source info */}
                            {p.pinned && p.source_tunnel_url && (
                                <div className="pyramid-pub-build-info">
                                    <span className="file-hash" title={`Source: ${p.source_tunnel_url}`}>
                                        source: {p.source_tunnel_url.replace(/^https?:\/\//, "").slice(0, 24)}...
                                    </span>
                                </div>
                            )}

                            {/* WS-ONLINE-E: Access Tier Controls */}
                            {!p.pinned && (
                                <div className="pyramid-access-tier-section">
                                    <button
                                        className="pyramid-access-tier-toggle"
                                        onClick={() => handleExpandAccessTier(p.slug)}
                                        title="Configure access tier for remote queries"
                                    >
                                        Access: {accessTiers[p.slug]?.access_tier || "public"}
                                        {accessTiers[p.slug]?.cached_emergent_price != null && accessTiers[p.slug]?.access_tier === "priced" && (
                                            <span className="pyramid-emergent-price">
                                                {" "}({accessTiers[p.slug]?.access_price ?? accessTiers[p.slug]?.cached_emergent_price} credits)
                                            </span>
                                        )}
                                        <span className="pyramid-access-tier-chevron">
                                            {expandedAccessSlug === p.slug ? "\u25B4" : "\u25BE"}
                                        </span>
                                    </button>

                                    {expandedAccessSlug === p.slug && (
                                        <div className="pyramid-access-tier-panel">
                                            <div className="pyramid-access-tier-field">
                                                <label>Tier</label>
                                                <select
                                                    value={accessTierDraft.tier}
                                                    onChange={(e) => setAccessTierDraft(prev => ({
                                                        ...prev,
                                                        tier: e.target.value as AccessTier,
                                                    }))}
                                                >
                                                    <option value="public">Public</option>
                                                    <option value="circle-scoped">Circle-Scoped</option>
                                                    <option value="priced">Priced</option>
                                                    <option value="embargoed">Embargoed</option>
                                                </select>
                                            </div>

                                            {accessTierDraft.tier === "circle-scoped" && (
                                                <div className="pyramid-access-tier-field">
                                                    <label>Allowed Circles (JSON array)</label>
                                                    <input
                                                        type="text"
                                                        value={accessTierDraft.circles}
                                                        onChange={(e) => setAccessTierDraft(prev => ({
                                                            ...prev,
                                                            circles: e.target.value,
                                                        }))}
                                                        placeholder='["circle-uuid-1","circle-uuid-2"]'
                                                    />
                                                </div>
                                            )}

                                            {accessTierDraft.tier === "priced" && (
                                                <div className="pyramid-access-tier-field">
                                                    <label>
                                                        Price Override (credits)
                                                        {accessTiers[p.slug]?.cached_emergent_price != null && (
                                                            <span className="pyramid-emergent-hint">
                                                                {" "}Emergent: {accessTiers[p.slug]?.cached_emergent_price}
                                                            </span>
                                                        )}
                                                    </label>
                                                    <input
                                                        type="text"
                                                        value={accessTierDraft.price}
                                                        onChange={(e) => setAccessTierDraft(prev => ({
                                                            ...prev,
                                                            price: e.target.value,
                                                        }))}
                                                        placeholder="blank = use emergent price"
                                                    />
                                                </div>
                                            )}

                                            <button
                                                className="folder-publish-btn"
                                                onClick={() => handleSaveAccessTier(p.slug)}
                                                disabled={savingAccessTier}
                                            >
                                                {savingAccessTier ? "Saving..." : "Save Access Tier"}
                                            </button>
                                        </div>
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
