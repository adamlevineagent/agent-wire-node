// src/components/QualityBadges.tsx — Phase 14: shared quality-signal
// badges for Wire discovery results + My Tools update drawer.
//
// Per `docs/specs/wire-discovery-ranking.md` §Quality Signals in Search
// Results. Renders a horizontal row of inline badges summarizing each
// contribution's rating, adoption count, open rebuttals, supersession
// chain depth, and freshness. The emoji shorthand in the spec maps to
// plain-text glyphs here — the existing frontend doesn't ship an icon
// library for this surface, and adding one for Phase 14 is
// out-of-scope.

interface QualityBadgesProps {
    rating?: number;
    adoptionCount: number;
    openRebuttals: number;
    chainLength: number;
    freshnessDays: number;
}

export function QualityBadges(props: QualityBadgesProps) {
    const { rating, adoptionCount, openRebuttals, chainLength, freshnessDays } =
        props;

    return (
        <div
            style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 6,
                alignItems: "center",
                fontSize: 11,
            }}
        >
            {rating !== undefined && rating !== null && (
                <Badge
                    label={`${formatRating(rating)}`}
                    tone="rating"
                    title={`Average rating: ${formatRating(rating)} / 5`}
                />
            )}
            <Badge
                label={`${formatAdoption(adoptionCount)} users`}
                tone="adoption"
                title={`${adoptionCount} distinct pullers`}
            />
            {openRebuttals > 0 && (
                <Badge
                    label={`${openRebuttals} rebuttal${openRebuttals === 1 ? "" : "s"}`}
                    tone="warning"
                    title={`${openRebuttals} open rebuttal${openRebuttals === 1 ? "" : "s"} filed against this contribution`}
                />
            )}
            {chainLength > 0 && (
                <Badge
                    label={`v${chainLength}`}
                    tone="chain"
                    title={`Supersession chain length: ${chainLength}`}
                />
            )}
            <Badge
                label={formatFreshness(freshnessDays)}
                tone="fresh"
                title={`Last updated ${freshnessDays} days ago`}
            />
        </div>
    );
}

// ── Internals ────────────────────────────────────────────────────────────────

type BadgeTone = "rating" | "adoption" | "chain" | "fresh" | "warning";

function Badge({
    label,
    tone,
    title,
}: {
    label: string;
    tone: BadgeTone;
    title?: string;
}) {
    const { bg, fg } = paletteFor(tone);
    return (
        <span
            title={title}
            style={{
                display: "inline-flex",
                alignItems: "center",
                padding: "2px 8px",
                borderRadius: 999,
                background: bg,
                color: fg,
                fontWeight: 600,
                lineHeight: 1.4,
                whiteSpace: "nowrap",
            }}
        >
            {label}
        </span>
    );
}

function paletteFor(tone: BadgeTone): { bg: string; fg: string } {
    switch (tone) {
        case "rating":
            return { bg: "rgba(250, 204, 21, 0.14)", fg: "#facc15" };
        case "adoption":
            return { bg: "rgba(59, 130, 246, 0.14)", fg: "#60a5fa" };
        case "chain":
            return { bg: "rgba(139, 92, 246, 0.14)", fg: "#a78bfa" };
        case "fresh":
            return { bg: "rgba(16, 185, 129, 0.14)", fg: "#34d399" };
        case "warning":
            return { bg: "rgba(239, 68, 68, 0.16)", fg: "#f87171" };
    }
}

function formatRating(value: number): string {
    if (!Number.isFinite(value)) return "—";
    return value.toFixed(1);
}

function formatAdoption(count: number): string {
    if (count < 1000) return String(count);
    if (count < 10000) return `${(count / 1000).toFixed(1)}k`;
    return `${Math.round(count / 1000)}k`;
}

function formatFreshness(days: number): string {
    if (days >= 4_000_000_000) return "unknown";
    if (days < 1) return "today";
    if (days < 7) return `${days}d ago`;
    if (days < 30) return `${Math.floor(days / 7)}w ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
}
