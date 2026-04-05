import React from "react";
import {
    EnrichedSlug,
    CONTENT_TYPE_CONFIG,
    getPublicationState,
    relativeTime,
} from "./pyramid-types";

interface PyramidRowProps {
    slug: EnrichedSlug;
    isSelected: boolean;
    publishingSlug: string | null;
    maxNodeCount: number;
    onClick: () => void;
    isNested?: boolean;
    activeBuild?: { status: string; progress: { done: number; total: number } } | null;
    onBuildClick?: (slug: string) => void;
}

export const PyramidRow: React.FC<PyramidRowProps> = ({
    slug,
    isSelected,
    publishingSlug,
    maxNodeCount,
    onClick,
    isNested,
    activeBuild,
    onBuildClick,
}) => {
    const config = CONTENT_TYPE_CONFIG[slug.content_type];
    const pubState = getPublicationState(slug, publishingSlug);
    const buildTime = relativeTime(slug.last_built_at);
    const isLarge = slug.node_count >= 500;

    const rowClasses = [
        "pyramid-row",
        isSelected ? "pyramid-row-selected" : "",
        isNested ? "pyramid-row-nested" : "",
    ]
        .filter(Boolean)
        .join(" ");

    // Format node count display
    const nodeCountText =
        slug.node_count === 0 ? "empty" : `${slug.node_count} nodes`;

    // Publication status dot
    const renderStatusDot = () => {
        switch (pubState) {
            case "published":
                return (
                    <span
                        className="pyramid-row-status"
                        style={{ backgroundColor: "#4ade80" }}
                        title="Published"
                    />
                );
            case "stale":
                return (
                    <span
                        className="pyramid-row-status pyramid-row-status-pulse"
                        style={{ backgroundColor: "#fbbf24" }}
                        title="Stale"
                    />
                );
            case "publishing":
                return (
                    <span
                        className="pyramid-row-status pyramid-row-status-spin"
                        style={{ borderColor: "#22d3ee transparent transparent transparent" }}
                        title="Publishing..."
                    />
                );
            case "unpublished":
                // Suppress — no noise for unpublished
                return <span className="pyramid-row-status" style={{ visibility: "hidden" }} />;
        }
    };

    // Scale bar for large pyramids
    const renderScaleBar = () => {
        if (!isLarge || maxNodeCount === 0) return null;
        const widthPct = (slug.node_count / maxNodeCount) * 100;
        // Parse the hex color into rgba at 20% opacity
        const hex = config.color;
        const r = parseInt(hex.slice(1, 3), 16);
        const g = parseInt(hex.slice(3, 5), 16);
        const b = parseInt(hex.slice(5, 7), 16);
        return (
            <div
                className="pyramid-row-scale-bar"
                style={{
                    width: `${widthPct}%`,
                    background: `rgba(${r}, ${g}, ${b}, 0.2)`,
                }}
            />
        );
    };

    return (
        <div
            className={rowClasses}
            onClick={onClick}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    onClick();
                }
            }}
            style={isNested ? ({ "--content-type-color": config.color } as React.CSSProperties) : undefined}
        >
            {/* Content type dot */}
            <span
                className="pyramid-row-dot"
                style={{ backgroundColor: config.color }}
            />

            {/* Slug name */}
            <span className="pyramid-row-name">{slug.slug}</span>

            {/* Active build badge */}
            {activeBuild && (
                <span
                    className="build-badge"
                    style={{
                        display: 'inline-flex',
                        alignItems: 'center',
                        gap: '4px',
                        padding: '2px 8px',
                        borderRadius: '10px',
                        fontSize: '11px',
                        fontWeight: 600,
                        backgroundColor: 'rgba(0, 200, 150, 0.15)',
                        color: 'rgb(0, 200, 150)',
                        animation: 'pulse 2s ease-in-out infinite',
                        cursor: 'pointer',
                        marginLeft: '8px',
                    }}
                    onClick={(e) => {
                        e.stopPropagation();
                        onBuildClick?.(slug.slug);
                    }}
                >
                    Building... {activeBuild.progress.total > 0
                        ? `${activeBuild.progress.done}/${activeBuild.progress.total}`
                        : ''}
                </span>
            )}

            {/* Flex spacer */}
            <span style={{ flex: 1 }} />

            {/* Node count */}
            <span
                className={`pyramid-row-count${isLarge ? " pyramid-row-count-large" : ""}${slug.node_count === 0 ? " pyramid-row-count-empty" : ""}`}
            >
                {nodeCountText}
            </span>

            {/* Relative build time */}
            <span className="pyramid-row-time">{buildTime}</span>

            {/* Publication status */}
            {renderStatusDot()}

            {/* Pinned badge */}
            {slug.pinned && <span className="pyramid-row-pinned">pinned</span>}

            {/* Scale bar for 500+ node pyramids */}
            {renderScaleBar()}
        </div>
    );
};

export default PyramidRow;
