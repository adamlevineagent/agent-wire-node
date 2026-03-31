import React, { useCallback } from 'react';

// --- Types ---

export interface ContributionSummary {
    id: string;
    title: string;
    teaser?: string;
    body?: string;
    contribution_type?: string;
    author_pseudonym?: string;
    topics?: string[];
    price?: number;
    significance?: number;
    avg_rating?: number;
    rating_count?: number;
    created_at?: string;
    updated_at?: string;
    entity_mentions?: string[];
}

interface ContributionCardProps {
    contribution: ContributionSummary;
    onExpand?: (id: string) => void;
    expanded?: boolean;
    expandedBody?: string | null;
    loading?: boolean;
    renderActions?: (contribution: ContributionSummary) => React.ReactNode;
}

// --- Helpers ---

function timeAgo(timestamp: string): string {
    const diff = Date.now() - new Date(timestamp).getTime();
    if (diff < 60_000) return 'just now';
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
}

const typeColors: Record<string, string> = {
    analysis: 'var(--accent-cyan)',
    commentary: 'var(--accent-purple)',
    correction: '#f06060',
    investigation: 'var(--accent-warm)',
    summary: 'var(--accent-green)',
    context: 'var(--text-secondary)',
    editorial: '#e090e0',
    review: '#70b0ff',
    tip: 'var(--accent-warm)',
};

function renderStars(rating: number): string {
    const full = Math.round(rating);
    return '\u2605'.repeat(full) + '\u2606'.repeat(5 - full);
}

// --- Component ---

export function ContributionCard({ contribution, onExpand, expanded, expandedBody, loading, renderActions }: ContributionCardProps) {
    const handleClick = useCallback(() => {
        if (onExpand) onExpand(contribution.id);
    }, [onExpand, contribution.id]);

    const typeColor = typeColors[contribution.contribution_type || ''] || 'var(--text-secondary)';

    return (
        <div
            className={`search-card ${expanded ? 'search-card-expanded' : ''}`}
            onClick={!expanded ? handleClick : undefined}
            role={!expanded ? 'button' : undefined}
            tabIndex={!expanded ? 0 : undefined}
            onKeyDown={!expanded ? (e) => { if (e.key === 'Enter') handleClick(); } : undefined}
        >
            <div className="search-card-header">
                <div className="search-card-title-row">
                    <h3 className="search-card-title">{contribution.title || 'Untitled'}</h3>
                    {contribution.price != null && contribution.price > 0 && (
                        <span className="search-card-price">{contribution.price} cr</span>
                    )}
                    {contribution.price === 0 && (
                        <span className="search-card-price search-card-price-free">Free</span>
                    )}
                </div>

                <div className="search-card-meta">
                    {contribution.contribution_type && (
                        <span className="search-card-type" style={{ color: typeColor, borderColor: typeColor }}>
                            {contribution.contribution_type}
                        </span>
                    )}
                    {contribution.author_pseudonym && (
                        <span className="search-card-author">{contribution.author_pseudonym}</span>
                    )}
                    {contribution.created_at && (
                        <span className="search-card-time">{timeAgo(contribution.created_at)}</span>
                    )}
                    {contribution.avg_rating != null && contribution.rating_count != null && contribution.rating_count > 0 && (
                        <span className="search-card-rating" title={`${contribution.avg_rating.toFixed(1)} avg from ${contribution.rating_count} ratings`}>
                            <span className="search-card-stars">{renderStars(contribution.avg_rating)}</span>
                            <span className="search-card-rating-count">({contribution.rating_count})</span>
                        </span>
                    )}
                    {contribution.significance != null && (
                        <span className="search-card-significance" title="Significance score">
                            S:{contribution.significance}
                        </span>
                    )}
                </div>
            </div>

            {contribution.teaser && !expanded && (
                <p className="search-card-teaser">{contribution.teaser}</p>
            )}

            {contribution.topics && contribution.topics.length > 0 && (
                <div className="search-card-topics">
                    {contribution.topics.map((t) => (
                        <span key={t} className="search-card-topic">{t}</span>
                    ))}
                </div>
            )}

            {expanded && (
                <div className="search-card-body">
                    {loading ? (
                        <div className="search-card-loading">Loading full content...</div>
                    ) : expandedBody ? (
                        <div className="search-card-content">{expandedBody}</div>
                    ) : contribution.body ? (
                        <div className="search-card-content">{contribution.body}</div>
                    ) : (
                        <div className="search-card-loading">No body content available</div>
                    )}
                    {renderActions && renderActions(contribution)}
                    {onExpand && (
                        <button
                            className="search-card-collapse"
                            onClick={(e) => { e.stopPropagation(); onExpand(contribution.id); }}
                        >
                            Collapse
                        </button>
                    )}
                </div>
            )}
        </div>
    );
}
