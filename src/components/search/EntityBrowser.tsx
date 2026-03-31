import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

interface WireEntity {
    id: string;
    name: string;
    entity_type?: string;
    aliases?: string[];
    mention_count?: number;
    first_seen?: string;
    last_seen?: string;
    related_contributions?: string[];
}

interface EntityBrowserProps {
    onEntitySelect?: (entity: WireEntity) => void;
}

const ENTITY_TYPES = ['all', 'person', 'organization', 'location', 'product', 'event', 'concept'] as const;

// --- Component ---

export function EntityBrowser({ onEntitySelect }: EntityBrowserProps) {
    const { wireApiCall } = useAppContext();

    const [entities, setEntities] = useState<WireEntity[]>([]);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [searchText, setSearchText] = useState('');
    const [selectedType, setSelectedType] = useState<string>('all');
    const [offset, setOffset] = useState(0);
    const [hasMore, setHasMore] = useState(false);
    const [selectedEntity, setSelectedEntity] = useState<WireEntity | null>(null);

    const LIMIT = 50;

    const fetchEntities = useCallback(async (reset: boolean = true) => {
        setLoading(true);
        setError(null);
        const newOffset = reset ? 0 : offset;
        try {
            const params = new URLSearchParams();
            if (selectedType !== 'all') params.set('type', selectedType);
            if (searchText.trim()) params.set('text', searchText.trim());
            params.set('limit', String(LIMIT));
            params.set('offset', String(newOffset));
            const qs = params.toString();
            const resp = await wireApiCall('GET', `/api/v1/wire/entities${qs ? '?' + qs : ''}`) as { entities?: WireEntity[]; total?: number };
            const items = resp?.entities || (Array.isArray(resp) ? resp as WireEntity[] : []);
            if (reset) {
                setEntities(items);
                setOffset(items.length);
            } else {
                setEntities((prev) => [...prev, ...items]);
                setOffset((prev) => prev + items.length);
            }
            setHasMore(items.length === LIMIT);
        } catch (err: unknown) {
            const msg = err instanceof Error ? err.message : String(err);
            if (msg.includes('401')) setError('Authentication expired. Please re-connect.');
            else if (msg.includes('429')) setError('Rate limited. Please wait a moment.');
            else setError(msg || 'Failed to load entities');
        } finally {
            setLoading(false);
        }
    }, [wireApiCall, selectedType, searchText, offset]);

    useEffect(() => {
        fetchEntities(true);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [selectedType]);

    const handleSearch = useCallback(() => {
        fetchEntities(true);
    }, [fetchEntities]);

    const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
        if (e.key === 'Enter') handleSearch();
    }, [handleSearch]);

    const handleEntityClick = useCallback((entity: WireEntity) => {
        if (selectedEntity?.id === entity.id) {
            setSelectedEntity(null);
        } else {
            setSelectedEntity(entity);
            if (onEntitySelect) onEntitySelect(entity);
        }
    }, [selectedEntity, onEntitySelect]);

    return (
        <div className="search-entity-browser">
            <div className="search-entity-controls">
                <div className="search-entity-search-row">
                    <input
                        type="text"
                        className="search-input"
                        placeholder="Search entities..."
                        value={searchText}
                        onChange={(e) => setSearchText(e.target.value)}
                        onKeyDown={handleKeyDown}
                    />
                    <button className="search-btn search-btn-secondary" onClick={handleSearch} disabled={loading}>
                        Search
                    </button>
                </div>
                <div className="search-entity-type-bar">
                    {ENTITY_TYPES.map((t) => (
                        <button
                            key={t}
                            className={`search-filter-chip ${selectedType === t ? 'search-filter-chip-active' : ''}`}
                            onClick={() => setSelectedType(t)}
                        >
                            {t === 'all' ? 'All Types' : t.charAt(0).toUpperCase() + t.slice(1)}
                        </button>
                    ))}
                </div>
            </div>

            {error && (
                <div className="search-error">
                    <span>{error}</span>
                    <button className="search-retry-btn" onClick={() => fetchEntities(true)}>Retry</button>
                </div>
            )}

            {loading && entities.length === 0 && (
                <div className="search-loading">Loading entities...</div>
            )}

            {!loading && !error && entities.length === 0 && (
                <div className="search-empty">No entities found. Try adjusting your filters.</div>
            )}

            <div className="search-entity-list">
                {entities.map((entity) => (
                    <div
                        key={entity.id}
                        className={`search-entity-item ${selectedEntity?.id === entity.id ? 'search-entity-item-selected' : ''}`}
                        onClick={() => handleEntityClick(entity)}
                        role="button"
                        tabIndex={0}
                        onKeyDown={(e) => { if (e.key === 'Enter') handleEntityClick(entity); }}
                    >
                        <div className="search-entity-name">{entity.name}</div>
                        <div className="search-entity-meta">
                            {entity.entity_type && (
                                <span className="search-entity-type-badge">{entity.entity_type}</span>
                            )}
                            {entity.mention_count != null && (
                                <span className="search-entity-mentions">{entity.mention_count} mentions</span>
                            )}
                        </div>
                        {selectedEntity?.id === entity.id && (
                            <div className="search-entity-detail">
                                {entity.aliases && entity.aliases.length > 0 && (
                                    <div className="search-entity-aliases">
                                        <strong>Aliases:</strong> {entity.aliases.join(', ')}
                                    </div>
                                )}
                                {entity.first_seen && (
                                    <div className="search-entity-seen">
                                        First seen: {new Date(entity.first_seen).toLocaleDateString()}
                                        {entity.last_seen && <> | Last seen: {new Date(entity.last_seen).toLocaleDateString()}</>}
                                    </div>
                                )}
                            </div>
                        )}
                    </div>
                ))}
            </div>

            {hasMore && !loading && (
                <button className="search-load-more" onClick={() => fetchEntities(false)}>
                    Load More
                </button>
            )}

            {loading && entities.length > 0 && (
                <div className="search-loading-more">Loading more...</div>
            )}
        </div>
    );
}
