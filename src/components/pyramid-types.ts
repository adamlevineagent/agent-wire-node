// pyramid-types.ts — Canonical shared types for the Pyramids Command Center
//
// ALL pyramid UI components MUST import from this file.
// Do NOT create local type definitions for these concepts.

// ─── Data from IPC ──────────────────────────────────────────────────────────

/** From IPC: pyramid_list_slugs */
export interface SlugInfo {
    slug: string;
    content_type: ContentType;
    source_path: string;
    node_count: number;
    max_depth: number;
    last_built_at: string | null;
    created_at: string;
    referenced_slugs: string[];
    referencing_slugs: string[];
    archived_at: string | null;
}

/** From IPC: pyramid_get_publication_status */
export interface PyramidPublicationInfo {
    slug: string;
    node_count: number;
    unpublished_count: number;
    last_published_build_id: string | null;
    current_build_id: string | null;
    last_built_at: string | null;
    pinned: boolean;
    source_tunnel_url: string | null;
}

/** From IPC: pyramid_get_access_tier */
export interface AccessTierInfo {
    access_tier: AccessTier;
    access_price: number | null;
    allowed_circles: string[] | null;
    cached_emergent_price: number | null;
}

/** From IPC: pyramid_get_config */
export interface PyramidConfigInfo {
    api_key_set: boolean;
    auth_token_set: boolean;
    primary_model: string;
    fallback_model_1: string;
    fallback_model_2: string;
}

/** From IPC: pyramid_get_absorption_config */
export interface AbsorptionConfig {
    mode: AbsorptionMode;
    chain_id: string | null;
    rate_limit_per_operator: number;
    daily_spend_cap: number;
}

/** From IPC: pyramid_publish (return value) */
export interface PublishResult {
    slug: string;
    apex_wire_uuid: string | null;
    node_count: number;
    id_mappings: Array<{
        local_id: string;
        wire_handle_path: string;
        wire_uuid: string | null;
        published_at: string;
    }>;
}

// ─── Enums ──────────────────────────────────────────────────────────────────

export type ContentType = "code" | "document" | "conversation" | "vine" | "question";
export type AccessTier = "public" | "circle-scoped" | "priced" | "embargoed";
export type AbsorptionMode = "open" | "absorb-all" | "absorb-selective";

// ─── Derived / UI types ─────────────────────────────────────────────────────

/** Merged slug + publication data for display */
export interface EnrichedSlug {
    // From SlugInfo
    slug: string;
    content_type: ContentType;
    source_path: string;
    node_count: number;
    max_depth: number;
    last_built_at: string | null;
    created_at: string;
    referenced_slugs: string[];
    referencing_slugs: string[];
    archived_at: string | null;
    // From PyramidPublicationInfo (may be absent)
    unpublished_count: number;
    last_published_build_id: string | null;
    current_build_id: string | null;
    pinned: boolean;
    source_tunnel_url: string | null;
}

export type PublicationState = "unpublished" | "published" | "stale" | "publishing";
export type SortKey = "node_count" | "recently_built" | "recently_created" | "alphabetical";

/** Content type display config */
export const CONTENT_TYPE_CONFIG: Record<ContentType, { label: string; color: string; icon: string }> = {
    code: { label: "Code", color: "#22d3ee", icon: "{ }" },        // cyan
    question: { label: "Questions", color: "#c084fc", icon: "?" },  // purple
    document: { label: "Documents", color: "#f9a8d4", icon: "D" },  // pink
    conversation: { label: "Conversations", color: "#4ade80", icon: "C" }, // green
    vine: { label: "Vines", color: "#fbbf24", icon: "V" },         // amber
};

// ─── Utility functions ──────────────────────────────────────────────────────

/** Determine publication state from enriched slug data */
export function getPublicationState(slug: EnrichedSlug, publishingSlug: string | null): PublicationState {
    if (publishingSlug === slug.slug) return "publishing";
    if (!slug.last_published_build_id) return "unpublished";
    if (slug.current_build_id !== slug.last_published_build_id || slug.unpublished_count > 0) return "stale";
    return "published";
}

/** Format relative time (e.g., "3d ago", "2h ago") */
export function relativeTime(dateStr: string | null): string {
    if (!dateStr) return "never";
    const hasTimezone = /[Z]$|[+-]\d{2}:\d{2}$|[+-]\d{4}$/.test(dateStr);
    const date = new Date(hasTimezone ? dateStr : dateStr + "Z");
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    if (diffMins < 1) return "just now";
    if (diffMins < 60) return `${diffMins}m ago`;
    const diffHrs = Math.floor(diffMins / 60);
    if (diffHrs < 24) return `${diffHrs}h ago`;
    const diffDays = Math.floor(diffHrs / 24);
    if (diffDays < 30) return `${diffDays}d ago`;
    return `${Math.floor(diffDays / 30)}mo ago`;
}

/** Merge SlugInfo + PyramidPublicationInfo into EnrichedSlug */
export function enrichSlug(slug: SlugInfo, pub: PyramidPublicationInfo | undefined): EnrichedSlug {
    return {
        ...slug,
        unpublished_count: pub?.unpublished_count ?? 0,
        last_published_build_id: pub?.last_published_build_id ?? null,
        current_build_id: pub?.current_build_id ?? null,
        pinned: pub?.pinned ?? false,
        source_tunnel_url: pub?.source_tunnel_url ?? null,
    };
}

/** Sort comparator factory */
export function sortComparator(key: SortKey): (a: EnrichedSlug, b: EnrichedSlug) => number {
    switch (key) {
        case "node_count":
            return (a, b) => b.node_count - a.node_count;
        case "recently_built":
            return (a, b) => {
                if (!a.last_built_at && !b.last_built_at) return 0;
                if (!a.last_built_at) return 1;
                if (!b.last_built_at) return -1;
                return new Date(b.last_built_at).getTime() - new Date(a.last_built_at).getTime();
            };
        case "recently_created":
            return (a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime();
        case "alphabetical":
            return (a, b) => a.slug.localeCompare(b.slug);
    }
}
