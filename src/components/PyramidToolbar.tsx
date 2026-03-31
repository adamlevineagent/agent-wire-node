import React from "react";
import { ContentType, SortKey, CONTENT_TYPE_CONFIG } from "./pyramid-types";

interface PyramidToolbarProps {
    searchQuery: string;
    onSearchChange: (query: string) => void;
    activeTypes: Set<string>;
    onToggleType: (type: string) => void;
    activeStatuses: Set<string>;
    onToggleStatus: (status: string) => void;
    sortBy: SortKey;
    onSortChange: (key: SortKey) => void;
    counts: {
        total: number;
        built: number;
        empty: number;
        published: number;
        stale: number;
        pinned: number;
    };
}

const STATUS_CHIPS: Array<{ key: string; label: string }> = [
    { key: "built", label: "Built" },
    { key: "empty", label: "Empty" },
    { key: "published", label: "Published" },
    { key: "stale", label: "Stale" },
    { key: "pinned", label: "Pinned" },
];

const SORT_OPTIONS: Array<{ key: SortKey; label: string }> = [
    { key: "node_count", label: "Largest first" },
    { key: "recently_built", label: "Recently built" },
    { key: "recently_created", label: "Recently created" },
    { key: "alphabetical", label: "A \u2192 Z" },
];

const PyramidToolbar: React.FC<PyramidToolbarProps> = ({
    searchQuery,
    onSearchChange,
    activeTypes,
    onToggleType,
    activeStatuses,
    onToggleStatus,
    sortBy,
    onSortChange,
    counts,
}) => {
    // Build content type chips from config, only for types that have data
    const contentTypeEntries = (Object.entries(CONTENT_TYPE_CONFIG) as Array<[ContentType, { label: string; color: string; icon: string }]>);

    return (
        <div className="pyramid-toolbar">
            {/* Search input */}
            <input
                type="text"
                className="pyramid-toolbar-search"
                placeholder="Search pyramids..."
                value={searchQuery}
                onChange={(e) => onSearchChange(e.target.value)}
            />

            {/* Content type filter chips */}
            {contentTypeEntries.map(([type, config]) => {
                const isActive = activeTypes.has(type);
                return (
                    <button
                        key={type}
                        className={`pyramid-filter-chip${isActive ? " pyramid-filter-chip-active" : ""}`}
                        style={isActive ? { backgroundColor: config.color, color: "#000" } : undefined}
                        onClick={() => onToggleType(type)}
                        title={config.label}
                    >
                        {config.icon} {config.label}
                    </button>
                );
            })}

            {/* Status filter chips */}
            {STATUS_CHIPS.map(({ key, label }) => {
                const count = counts[key as keyof typeof counts];
                if (count === 0) return null;
                const isActive = activeStatuses.has(key);
                return (
                    <button
                        key={key}
                        className={`pyramid-filter-chip${isActive ? " pyramid-filter-chip-active" : ""}`}
                        style={isActive ? { backgroundColor: "var(--accent-cyan)", color: "#000" } : undefined}
                        onClick={() => onToggleStatus(key)}
                    >
                        {label} ({count})
                    </button>
                );
            })}

            {/* Sort dropdown */}
            <select
                className="pyramid-sort-select"
                value={sortBy}
                onChange={(e) => onSortChange(e.target.value as SortKey)}
            >
                {SORT_OPTIONS.map(({ key, label }) => (
                    <option key={key} value={key}>
                        {label}
                    </option>
                ))}
            </select>
        </div>
    );
};

export default PyramidToolbar;
