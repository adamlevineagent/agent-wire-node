import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
    EnrichedSlug,
    PublicationState,
    AccessTier,
    AbsorptionMode,
    AccessTierInfo,
    AbsorptionConfig,
    PublishResult,
    CONTENT_TYPE_CONFIG,
    getPublicationState,
    relativeTime,
} from "./pyramid-types";

// ─── Props ──────────────────────────────────────────────────────────────────

interface PyramidDetailDrawerProps {
    slug: EnrichedSlug | null;
    onClose: () => void;
    onPublish: (slug: string) => Promise<PublishResult>;
    onSetAccessTier: (slug: string, tier: AccessTier, price?: number, circles?: string[]) => Promise<void>;
    onSetAbsorption: (slug: string, mode: AbsorptionMode, chainId?: string, rateLimit?: number, dailyCap?: number) => Promise<void>;
    onDelete: (slug: string) => Promise<void>;
    onRebuild: (slug: string) => void;
    onOpenDadbear?: (slug: string) => void;
    onOpenFaq?: (slug: string) => void;
    onOpenVine?: (slug: string) => void;
    onAskQuestion?: (slug: string) => void;
    onOpenVibesmithy?: (slug: string) => void;
    publishingSlug: string | null;
    lastPublishResult: Record<string, { success: boolean; message: string; wireUuid?: string }>;
}

// ─── Component ──────────────────────────────────────────────────────────────

export function PyramidDetailDrawer({
    slug,
    onClose,
    onPublish,
    onSetAccessTier,
    onSetAbsorption,
    onDelete,
    onRebuild,
    onOpenDadbear,
    onOpenFaq,
    onOpenVine,
    onAskQuestion,
    onOpenVibesmithy,
    publishingSlug,
    lastPublishResult,
}: PyramidDetailDrawerProps) {
    // ─── Internal state ─────────────────────────────────────────────────────

    // Access tier draft
    const [accessTierOpen, setAccessTierOpen] = useState(false);
    const [accessTierDraft, setAccessTierDraft] = useState<{
        tier: AccessTier;
        price: string;
        circles: string;
    }>({ tier: "public", price: "", circles: "" });
    const [savingAccessTier, setSavingAccessTier] = useState(false);
    const [cachedEmergentPrice, setCachedEmergentPrice] = useState<number | null>(null);

    // Absorption draft
    const [absorptionOpen, setAbsorptionOpen] = useState(false);
    const [absorptionDraft, setAbsorptionDraft] = useState<{
        mode: AbsorptionMode;
        chainId: string;
        rateLimit: string;
        dailyCap: string;
    }>({ mode: "open", chainId: "", rateLimit: "3", dailyCap: "100" });
    const [savingAbsorption, setSavingAbsorption] = useState(false);

    // Delete confirmation
    const [deleteConfirm, setDeleteConfirm] = useState(false);
    const [deleteError, setDeleteError] = useState<string | null>(null);

    // Config validation errors (shown inline per section, not shared)
    const [accessTierError, setAccessTierError] = useState<string | null>(null);
    const [absorptionError, setAbsorptionError] = useState<string | null>(null);

    // Save success flash
    const [accessTierSaved, setAccessTierSaved] = useState(false);
    const [absorptionSaved, setAbsorptionSaved] = useState(false);

    // Scroll-to-top ref
    const drawerRef = useRef<HTMLDivElement>(null);

    // Publishing state for this drawer's publish button
    const [localPublishing, setLocalPublishing] = useState(false);
    const [localPublishResult, setLocalPublishResult] = useState<{
        success: boolean;
        message: string;
        wireUuid?: string;
    } | null>(null);

    // ─── Escape key to close drawer ───────────────────────────────────────────

    useEffect(() => {
        if (!slug) return;
        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.key === "Escape") onClose();
        };
        document.addEventListener("keydown", handleKeyDown);
        return () => document.removeEventListener("keydown", handleKeyDown);
    }, [slug, onClose]);

    // ─── Fetch access tier and absorption config on slug change ─────────────

    useEffect(() => {
        if (!slug) return;

        // Guard against stale responses when slug changes rapidly
        let cancelled = false;

        // Reset state when slug changes
        setDeleteConfirm(false);
        setDeleteError(null);
        setLocalPublishResult(null);
        setAccessTierError(null);
        setAbsorptionError(null);
        setAccessTierSaved(false);
        setAbsorptionSaved(false);
        setAccessTierOpen(false);
        setAbsorptionOpen(false);
        drawerRef.current?.scrollTo(0, 0);

        // Fetch access tier
        invoke<AccessTierInfo>("pyramid_get_access_tier", { slug: slug.slug })
            .then((data) => {
                if (cancelled) return;
                setAccessTierDraft({
                    tier: data.access_tier,
                    price: data.access_price != null ? String(data.access_price) : "",
                    circles: data.allowed_circles ? data.allowed_circles.join(", ") : "",
                });
                setCachedEmergentPrice(data.cached_emergent_price);
            })
            .catch((err) => {
                if (cancelled) return;
                console.error("Failed to fetch access tier:", err);
            });

        // Fetch absorption config
        invoke<AbsorptionConfig>("pyramid_get_absorption_config", { slug: slug.slug })
            .then((data) => {
                if (cancelled) return;
                setAbsorptionDraft({
                    mode: data.mode,
                    chainId: data.chain_id || "",
                    rateLimit: String(data.rate_limit_per_operator),
                    dailyCap: String(data.daily_spend_cap),
                });
            })
            .catch((err) => {
                if (cancelled) return;
                console.error("Failed to fetch absorption config:", err);
            });

        return () => { cancelled = true; };
    }, [slug?.slug]);

    // ─── Handlers ───────────────────────────────────────────────────────────

    const handlePublish = useCallback(async () => {
        if (!slug) return;
        setLocalPublishing(true);
        setLocalPublishResult(null);
        try {
            const result = await onPublish(slug.slug);
            setLocalPublishResult({
                success: true,
                message: "Published to Wire",
                wireUuid: result.apex_wire_uuid ?? undefined,
            });
        } catch (err) {
            setLocalPublishResult({
                success: false,
                message: String(err),
            });
        } finally {
            setLocalPublishing(false);
        }
    }, [slug, onPublish]);

    const handleSaveAccessTier = useCallback(async () => {
        if (!slug) return;
        setAccessTierError(null);
        setSavingAccessTier(true);
        try {
            // Credits are whole numbers — parseInt is intentional
            const price = accessTierDraft.price.trim() === ""
                ? undefined
                : parseInt(accessTierDraft.price, 10);
            const circles = accessTierDraft.circles.trim() === ""
                ? undefined
                : accessTierDraft.circles.split(",").map((c) => c.trim()).filter(Boolean);

            if (price !== undefined && isNaN(price)) {
                setAccessTierError("Price must be a number");
                return;
            }

            await onSetAccessTier(slug.slug, accessTierDraft.tier, price, circles);
            setAccessTierSaved(true);
            setTimeout(() => setAccessTierSaved(false), 1500);
        } catch (err) {
            setAccessTierError(String(err));
        } finally {
            setSavingAccessTier(false);
        }
    }, [slug, accessTierDraft, onSetAccessTier]);

    const handleSaveAbsorption = useCallback(async () => {
        if (!slug) return;
        setAbsorptionError(null);
        setSavingAbsorption(true);
        try {
            const rateLimit = absorptionDraft.rateLimit.trim() === ""
                ? undefined
                : parseInt(absorptionDraft.rateLimit, 10);
            const dailyCap = absorptionDraft.dailyCap.trim() === ""
                ? undefined
                : parseInt(absorptionDraft.dailyCap, 10);
            const chainId = absorptionDraft.chainId.trim() || undefined;

            if (rateLimit !== undefined && isNaN(rateLimit)) {
                setAbsorptionError("Rate limit must be a number");
                return;
            }
            if (dailyCap !== undefined && isNaN(dailyCap)) {
                setAbsorptionError("Daily cap must be a number");
                return;
            }

            await onSetAbsorption(slug.slug, absorptionDraft.mode, chainId, rateLimit, dailyCap);
            setAbsorptionSaved(true);
            setTimeout(() => setAbsorptionSaved(false), 1500);
        } catch (err) {
            setAbsorptionError(String(err));
        } finally {
            setSavingAbsorption(false);
        }
    }, [slug, absorptionDraft, onSetAbsorption]);

    const handleDelete = useCallback(async () => {
        if (!slug) return;
        try {
            await onDelete(slug.slug);
            onClose();
        } catch (err) {
            setDeleteError(String(err));
        }
    }, [slug, onDelete, onClose]);

    const handleCopyWireAddress = useCallback((wireUuid: string) => {
        if (!slug) return;
        const address = `wire://${slug.slug}/${wireUuid}`;
        navigator.clipboard.writeText(address).catch((err) => {
            console.error("Failed to copy:", err);
        });
    }, [slug]);

    // ─── Derived values ─────────────────────────────────────────────────────

    if (!slug) {
        return (
            <div className="pyramid-detail-drawer pyramid-detail-drawer-hidden" />
        );
    }

    const pubState: PublicationState = getPublicationState(slug, publishingSlug);
    const contentConfig = CONTENT_TYPE_CONFIG[slug.content_type];
    const isPublishing = localPublishing || publishingSlug === slug.slug;

    // Use either the local publish result or the parent-provided one
    const publishResult = localPublishResult ?? lastPublishResult[slug.slug] ?? null;

    // ─── Publication state badge ────────────────────────────────────────────

    function pubStateBadge(state: PublicationState) {
        const colors: Record<PublicationState, string> = {
            published: "#22c55e",
            stale: "#eab308",
            unpublished: "#6b7280",
            publishing: "#3b82f6",
        };
        const labels: Record<PublicationState, string> = {
            published: "Published",
            stale: "Stale",
            unpublished: "Unpublished",
            publishing: "Publishing...",
        };
        return (
            <span
                style={{
                    display: "inline-block",
                    padding: "2px 8px",
                    borderRadius: 8,
                    fontSize: 11,
                    fontWeight: 600,
                    color: "#fff",
                    backgroundColor: colors[state],
                }}
            >
                {labels[state]}
            </span>
        );
    }

    // ─── Render ─────────────────────────────────────────────────────────────

    return (
        <div className="pyramid-detail-drawer" ref={drawerRef}>
            {/* 1. Header */}
            <div className="drawer-header">
                <button
                    onClick={onClose}
                    style={{
                        position: "absolute",
                        top: 12,
                        right: 12,
                        background: "none",
                        border: "none",
                        color: "inherit",
                        fontSize: 18,
                        cursor: "pointer",
                        opacity: 0.7,
                    }}
                    title="Close"
                >
                    &#x2715;
                </button>

                <span style={{ fontSize: 18, fontWeight: 700 }}>{slug.slug}</span>

                <span
                    style={{
                        display: "inline-block",
                        padding: "2px 8px",
                        borderRadius: 8,
                        fontSize: 11,
                        fontWeight: 600,
                        color: "#000",
                        backgroundColor: contentConfig.color,
                        alignSelf: "flex-start",
                    }}
                >
                    {contentConfig.icon} {contentConfig.label}
                </span>

                <span
                    style={{
                        fontSize: 12,
                        fontFamily: "monospace",
                        opacity: 0.6,
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                    }}
                    title={slug.source_path}
                >
                    {slug.source_path}
                </span>
            </div>

            {/* 2. Stats grid */}
            <div className="drawer-stats-grid">
                <div className="drawer-stat">
                    <span className="drawer-stat-value">{slug.node_count}</span>
                    <span className="drawer-stat-label">Nodes</span>
                </div>
                <div className="drawer-stat">
                    <span className="drawer-stat-value">{slug.max_depth}</span>
                    <span className="drawer-stat-label">Depth</span>
                </div>
                <div className="drawer-stat">
                    <span className="drawer-stat-value" title={slug.last_built_at ?? undefined}>
                        {relativeTime(slug.last_built_at)}
                    </span>
                    <span className="drawer-stat-label">Built</span>
                </div>
                <div className="drawer-stat">
                    <span className="drawer-stat-value" title={slug.created_at}>
                        {relativeTime(slug.created_at)}
                    </span>
                    <span className="drawer-stat-label">Created</span>
                </div>
            </div>

            {/* 3. References */}
            {slug.content_type === "question" && slug.referenced_slugs.length > 0 && (
                <div className="drawer-references">
                    Built on: {slug.referenced_slugs.join(", ")}
                </div>
            )}
            {slug.content_type !== "question" && slug.referencing_slugs.length > 0 && (
                <div className="drawer-references">
                    Referenced by: {slug.referencing_slugs.length} question pyramid{slug.referencing_slugs.length !== 1 ? "s" : ""}
                </div>
            )}

            {/* 4. Publication section */}
            <div className="drawer-publish-section">
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    {pubStateBadge(pubState)}
                </div>

                {/* Stale build ID comparison */}
                {pubState === "stale" && slug.last_published_build_id && slug.current_build_id && (
                    <div style={{ fontSize: 11, fontFamily: "monospace", opacity: 0.7 }}>
                        pub: {slug.last_published_build_id.slice(0, 8)} {"\u2192"} current: {slug.current_build_id.slice(0, 8)}
                    </div>
                )}

                {/* Publish Now button */}
                <button
                    className="folder-publish-btn"
                    onClick={handlePublish}
                    disabled={isPublishing || slug.node_count === 0 || pubState === "published"}
                    style={{ width: "100%" }}
                >
                    {isPublishing ? "Publishing..." : "Publish Now"}
                </button>

                {/* Publish result */}
                {publishResult && publishResult.success && (
                    <div
                        style={{
                            padding: "8px 12px",
                            borderRadius: 6,
                            backgroundColor: "rgba(34, 197, 94, 0.15)",
                            border: "1px solid rgba(34, 197, 94, 0.3)",
                            fontSize: 12,
                        }}
                    >
                        <div style={{ fontWeight: 600, marginBottom: 4 }}>{publishResult.message}</div>
                        {publishResult.wireUuid && (
                            <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                                <span style={{ fontFamily: "monospace", fontSize: 11, opacity: 0.8 }}>
                                    wire://{slug.slug}/{publishResult.wireUuid}
                                </span>
                                <button
                                    className="folder-publish-btn"
                                    onClick={() => handleCopyWireAddress(publishResult.wireUuid!)}
                                    style={{ fontSize: 10, padding: "2px 6px" }}
                                >
                                    Copy Wire Address
                                </button>
                            </div>
                        )}
                    </div>
                )}
                {publishResult && !publishResult.success && (
                    <div
                        style={{
                            padding: "8px 12px",
                            borderRadius: 6,
                            backgroundColor: "rgba(239, 68, 68, 0.15)",
                            border: "1px solid rgba(239, 68, 68, 0.3)",
                            fontSize: 12,
                            color: "#ef4444",
                        }}
                    >
                        {publishResult.message}
                    </div>
                )}
            </div>

            {/* 5. Access Tier (collapsible) */}
            <div className="drawer-config-section">
                <button
                    onClick={() => setAccessTierOpen((prev) => !prev)}
                    style={{
                        background: "none",
                        border: "none",
                        color: "inherit",
                        cursor: "pointer",
                        width: "100%",
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        padding: "4px 0",
                        fontSize: 14,
                        fontWeight: 600,
                    }}
                >
                    <span>Access Tier</span>
                    <span style={{ fontSize: 12 }}>
                        {!accessTierOpen && (
                            <span style={{ opacity: 0.6, marginRight: 8, fontWeight: 400 }}>
                                {accessTierDraft.tier}
                            </span>
                        )}
                        {accessTierOpen ? "\u25B4" : "\u25BE"}
                    </span>
                </button>

                {accessTierOpen && (
                    <div style={{ display: "flex", flexDirection: "column", gap: 10, paddingTop: 8 }}>
                        <div className="pyramid-access-tier-field">
                            <label>Tier</label>
                            <select
                                value={accessTierDraft.tier}
                                onChange={(e) =>
                                    setAccessTierDraft((prev) => ({
                                        ...prev,
                                        tier: e.target.value as AccessTier,
                                    }))
                                }
                            >
                                <option value="public">Public</option>
                                <option value="circle-scoped">Circle-Scoped</option>
                                <option value="priced">Priced</option>
                                <option value="embargoed">Embargoed</option>
                            </select>
                        </div>

                        {accessTierDraft.tier === "priced" && (
                            <div className="pyramid-access-tier-field">
                                <label>
                                    Price (credits)
                                    {cachedEmergentPrice != null && (
                                        <span style={{ opacity: 0.6, fontSize: 11, marginLeft: 6 }}>
                                            Emergent: {cachedEmergentPrice}
                                        </span>
                                    )}
                                </label>
                                <input
                                    type="text"
                                    value={accessTierDraft.price}
                                    onChange={(e) =>
                                        setAccessTierDraft((prev) => ({
                                            ...prev,
                                            price: e.target.value,
                                        }))
                                    }
                                    placeholder="blank = use emergent price"
                                />
                            </div>
                        )}

                        {accessTierDraft.tier === "circle-scoped" && (
                            <div className="pyramid-access-tier-field">
                                <label>Circles (comma-separated)</label>
                                <input
                                    type="text"
                                    value={accessTierDraft.circles}
                                    onChange={(e) =>
                                        setAccessTierDraft((prev) => ({
                                            ...prev,
                                            circles: e.target.value,
                                        }))
                                    }
                                    placeholder="circle-uuid-1, circle-uuid-2"
                                />
                            </div>
                        )}

                        <button
                            className="folder-publish-btn"
                            onClick={handleSaveAccessTier}
                            disabled={savingAccessTier}
                        >
                            {savingAccessTier ? "Saving..." : accessTierSaved ? "Saved" : "Save Access Tier"}
                        </button>
                        {accessTierError && (
                            <div style={{
                                padding: "6px 10px",
                                borderRadius: 6,
                                backgroundColor: "rgba(239, 68, 68, 0.15)",
                                border: "1px solid rgba(239, 68, 68, 0.3)",
                                fontSize: 12,
                                color: "#ef4444",
                            }}>
                                {accessTierError}
                            </div>
                        )}
                    </div>
                )}
            </div>

            {/* 6. Absorption Config (collapsible) */}
            <div className="drawer-config-section">
                <button
                    onClick={() => setAbsorptionOpen((prev) => !prev)}
                    style={{
                        background: "none",
                        border: "none",
                        color: "inherit",
                        cursor: "pointer",
                        width: "100%",
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        padding: "4px 0",
                        fontSize: 14,
                        fontWeight: 600,
                    }}
                >
                    <span>Absorption</span>
                    <span style={{ fontSize: 12 }}>
                        {!absorptionOpen && (
                            <span style={{ opacity: 0.6, marginRight: 8, fontWeight: 400 }}>
                                {absorptionDraft.mode}
                            </span>
                        )}
                        {absorptionOpen ? "\u25B4" : "\u25BE"}
                    </span>
                </button>

                {absorptionOpen && (
                    <div style={{ display: "flex", flexDirection: "column", gap: 10, paddingTop: 8 }}>
                        <div className="pyramid-access-tier-field">
                            <label>Mode</label>
                            <select
                                value={absorptionDraft.mode}
                                onChange={(e) =>
                                    setAbsorptionDraft((prev) => ({
                                        ...prev,
                                        mode: e.target.value as AbsorptionMode,
                                    }))
                                }
                            >
                                <option value="open">Open (questioner owns web)</option>
                                <option value="absorb-all">Absorb All (owner funds)</option>
                                <option value="absorb-selective">Absorb Selective (chain evaluates)</option>
                            </select>
                        </div>

                        {absorptionDraft.mode === "absorb-all" && (
                            <>
                                <div className="pyramid-access-tier-field">
                                    <label>Rate Limit (builds/hour per operator)</label>
                                    <input
                                        type="text"
                                        value={absorptionDraft.rateLimit}
                                        onChange={(e) =>
                                            setAbsorptionDraft((prev) => ({
                                                ...prev,
                                                rateLimit: e.target.value,
                                            }))
                                        }
                                        placeholder="3"
                                    />
                                </div>
                                <div className="pyramid-access-tier-field">
                                    <label>Daily Spend Cap (credits/day)</label>
                                    <input
                                        type="text"
                                        value={absorptionDraft.dailyCap}
                                        onChange={(e) =>
                                            setAbsorptionDraft((prev) => ({
                                                ...prev,
                                                dailyCap: e.target.value,
                                            }))
                                        }
                                        placeholder="100"
                                    />
                                </div>
                            </>
                        )}

                        {absorptionDraft.mode === "absorb-selective" && (
                            <div className="pyramid-access-tier-field">
                                <label>Action Chain ID</label>
                                <input
                                    type="text"
                                    value={absorptionDraft.chainId}
                                    onChange={(e) =>
                                        setAbsorptionDraft((prev) => ({
                                            ...prev,
                                            chainId: e.target.value,
                                        }))
                                    }
                                    placeholder="chain-id for evaluation"
                                />
                            </div>
                        )}

                        <button
                            className="folder-publish-btn"
                            onClick={handleSaveAbsorption}
                            disabled={savingAbsorption}
                        >
                            {savingAbsorption ? "Saving..." : absorptionSaved ? "Saved" : "Save Absorption Config"}
                        </button>
                        {absorptionError && (
                            <div style={{
                                padding: "6px 10px",
                                borderRadius: 6,
                                backgroundColor: "rgba(239, 68, 68, 0.15)",
                                border: "1px solid rgba(239, 68, 68, 0.3)",
                                fontSize: 12,
                                color: "#ef4444",
                            }}>
                                {absorptionError}
                            </div>
                        )}
                    </div>
                )}
            </div>

            {/* 7. Navigation actions */}
            <div className="drawer-actions">
                {onOpenDadbear && (
                    <button
                        className="folder-publish-btn"
                        onClick={() => onOpenDadbear(slug.slug)}
                        style={{ width: "100%" }}
                    >
                        DADBEAR Auto-Update
                    </button>
                )}
                {onOpenFaq && slug.node_count > 0 && (
                    <button
                        className="folder-publish-btn"
                        onClick={() => onOpenFaq(slug.slug)}
                        style={{ width: "100%" }}
                    >
                        FAQ Directory
                    </button>
                )}
                {onOpenVine && slug.content_type === "vine" && (
                    <button
                        className="folder-publish-btn"
                        onClick={() => onOpenVine(slug.slug)}
                        style={{ width: "100%" }}
                    >
                        View Vine
                    </button>
                )}
                {onAskQuestion && slug.node_count > 0 && (
                    <button
                        className="folder-publish-btn"
                        onClick={() => onAskQuestion(slug.slug)}
                        style={{ width: "100%" }}
                    >
                        Ask Question
                    </button>
                )}
                {onOpenVibesmithy && slug.node_count > 0 && (
                    <button
                        className="folder-publish-btn"
                        onClick={() => onOpenVibesmithy(slug.slug)}
                        style={{ width: "100%" }}
                    >
                        Open in Vibesmithy
                    </button>
                )}
                <button
                    className="folder-publish-btn"
                    onClick={() => onRebuild(slug.slug)}
                    style={{ width: "100%" }}
                >
                    Rebuild
                </button>

                {!deleteConfirm ? (
                    <button
                        className="folder-publish-btn"
                        onClick={() => setDeleteConfirm(true)}
                        style={{
                            width: "100%",
                            backgroundColor: "rgba(239, 68, 68, 0.15)",
                            borderColor: "rgba(239, 68, 68, 0.3)",
                            color: "#ef4444",
                        }}
                    >
                        Delete
                    </button>
                ) : (
                    <div
                        style={{
                            display: "flex",
                            flexDirection: "column",
                            gap: 6,
                            padding: "8px 12px",
                            borderRadius: 6,
                            backgroundColor: "rgba(239, 68, 68, 0.1)",
                            border: "1px solid rgba(239, 68, 68, 0.3)",
                        }}
                    >
                        <span style={{ fontSize: 12, fontWeight: 600, color: "#ef4444" }}>
                            Are you sure? This cannot be undone.
                        </span>
                        <div style={{ display: "flex", gap: 8 }}>
                            <button
                                className="folder-publish-btn"
                                onClick={handleDelete}
                                style={{
                                    flex: 1,
                                    backgroundColor: "#ef4444",
                                    borderColor: "#ef4444",
                                    color: "#fff",
                                }}
                            >
                                Confirm Delete
                            </button>
                            <button
                                className="folder-publish-btn"
                                onClick={() => setDeleteConfirm(false)}
                                style={{ flex: 1 }}
                            >
                                Cancel
                            </button>
                        </div>
                    </div>
                )}
                {deleteError && (
                    <div style={{
                        padding: "6px 10px",
                        borderRadius: 6,
                        backgroundColor: "rgba(239, 68, 68, 0.15)",
                        border: "1px solid rgba(239, 68, 68, 0.3)",
                        fontSize: 12,
                        color: "#ef4444",
                    }}>
                        {deleteError}
                    </div>
                )}
            </div>
        </div>
    );
}
