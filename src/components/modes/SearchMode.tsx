import { useState, useEffect, useCallback, useRef } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { ContributionCard, type ContributionSummary } from '../search/ContributionCard';
import { EntityBrowser } from '../search/EntityBrowser';

// --- Types ---

type SearchTab = 'feed' | 'results' | 'entities' | 'topics' | 'pyramids';
type FeedMode = 'new' | 'popular' | 'trending';
type SortOrder = 'relevance' | 'newest' | 'oldest' | 'price_asc' | 'price_desc' | 'significance';

interface WireTopic {
    id: string;
    name: string;
    slug?: string;
    contribution_count?: number;
    description?: string;
}

interface SearchFilters {
    topics: string[];
    contributionType: string;
    significanceMin: string;
    significanceMax: string;
    priceMin: string;
    priceMax: string;
    sort: SortOrder;
    dateFrom: string;
    dateTo: string;
}

const CONTRIBUTION_TYPES = [
    { value: '', label: 'All Types' },
    { value: 'analysis', label: 'Analysis' },
    { value: 'commentary', label: 'Commentary' },
    { value: 'correction', label: 'Correction' },
    { value: 'investigation', label: 'Investigation' },
    { value: 'summary', label: 'Summary' },
    { value: 'context', label: 'Context' },
    { value: 'editorial', label: 'Editorial' },
    { value: 'review', label: 'Review' },
    { value: 'tip', label: 'Tip' },
];

const SORT_OPTIONS: { value: SortOrder; label: string }[] = [
    { value: 'relevance', label: 'Relevance' },
    { value: 'newest', label: 'Newest First' },
    { value: 'oldest', label: 'Oldest First' },
    { value: 'price_asc', label: 'Price: Low to High' },
    { value: 'price_desc', label: 'Price: High to Low' },
    { value: 'significance', label: 'Significance' },
];

const DEFAULT_FILTERS: SearchFilters = {
    topics: [],
    contributionType: '',
    significanceMin: '',
    significanceMax: '',
    priceMin: '',
    priceMax: '',
    sort: 'relevance',
    dateFrom: '',
    dateTo: '',
};

const LIMIT = 20;

// --- Helpers ---

function buildQueryParams(text: string, filters: SearchFilters, offset: number): string {
    const params = new URLSearchParams();
    params.set('text', text);
    params.set('limit', String(LIMIT));
    if (offset > 0) params.set('offset', String(offset));
    if (filters.contributionType) params.set('type', filters.contributionType);
    if (filters.topics.length > 0) params.set('topics', filters.topics.join(','));
    if (filters.significanceMin) params.set('significance_min', filters.significanceMin);
    // Note: significance_max and price_min/price_max are not supported server-side
    // price_below is the server param for max price filtering
    if (filters.priceMax) params.set('price_below', filters.priceMax);
    if (filters.sort !== 'relevance') {
        // Server supports 'recency' (default) and 'significance'; map client sort values
        if (filters.sort === 'significance') params.set('sort', 'significance');
        // 'newest' maps to default recency, no need to set
    }
    if (filters.dateFrom) params.set('since', filters.dateFrom);
    return params.toString();
}

interface ApiError {
    status?: number;
    message?: string;
}

function parseApiError(err: unknown): ApiError {
    const msg = err instanceof Error ? err.message : String(err);
    if (msg.includes('402')) return { status: 402, message: 'Insufficient credits. Earn more credits by contributing or serving data.' };
    if (msg.includes('429')) return { status: 429, message: 'Rate limited. Please wait a moment before trying again.' };
    if (msg.includes('401')) return { status: 401, message: 'Authentication expired. Please reconnect to the Wire.' };
    return { message: msg || 'An unexpected error occurred.' };
}

// --- Main Component ---

export function SearchMode() {
    const { wireApiCall, state, dispatch, setMode, navigateView } = useAppContext();

    // --- Tab state ---
    const [activeTab, setActiveTab] = useState<SearchTab>('feed');

    // --- Feed state ---
    const [feedMode, setFeedMode] = useState<FeedMode>('new');
    const [feedItems, setFeedItems] = useState<ContributionSummary[]>([]);
    const [feedLoading, setFeedLoading] = useState(false);
    const [feedError, setFeedError] = useState<string | null>(null);
    const [feedOffset, setFeedOffset] = useState(0);
    const [feedHasMore, setFeedHasMore] = useState(false);

    // --- Search state ---
    const [searchText, setSearchText] = useState('');
    const [filters, setFilters] = useState<SearchFilters>(DEFAULT_FILTERS);
    const [showFilters, setShowFilters] = useState(false);
    const [results, setResults] = useState<ContributionSummary[]>([]);
    const [searchLoading, setSearchLoading] = useState(false);
    const [searchError, setSearchError] = useState<string | null>(null);
    const [searchOffset, setSearchOffset] = useState(0);
    const [searchHasMore, setSearchHasMore] = useState(false);
    const [lastQueryCost, setLastQueryCost] = useState<number | null>(null);
    const [showCostWarning, setShowCostWarning] = useState(false);

    // --- Topics state ---
    const [topics, setTopics] = useState<WireTopic[]>([]);
    const [topicsLoading, setTopicsLoading] = useState(false);
    const [topicsError, setTopicsError] = useState<string | null>(null);
    const [selectedTopic, setSelectedTopic] = useState<WireTopic | null>(null);
    const [topicContributions, setTopicContributions] = useState<ContributionSummary[]>([]);
    const [topicLoading, setTopicLoading] = useState(false);

    // --- Expand state ---
    const [expandedId, setExpandedId] = useState<string | null>(null);
    const [expandedBody, setExpandedBody] = useState<string | null>(null);
    const [expandLoading, setExpandLoading] = useState(false);

    // --- Rating state ---
    const [ratingAccuracy, setRatingAccuracy] = useState(0.5);
    const [ratingUsefulness, setRatingUsefulness] = useState(0.5);
    const [ratingSubmitting, setRatingSubmitting] = useState(false);
    const [ratingSuccess, setRatingSuccess] = useState<string | null>(null);
    const [ratingError, setRatingError] = useState<string | null>(null);
    const [flagOpen, setFlagOpen] = useState(false);
    const [flagSubmitting, setFlagSubmitting] = useState(false);

    // --- Filter topic input ---
    const [topicInput, setTopicInput] = useState('');

    const searchInputRef = useRef<HTMLInputElement>(null);

    // === Feed ===

    const fetchFeed = useCallback(async (reset: boolean = true) => {
        setFeedLoading(true);
        setFeedError(null);
        const newOffset = reset ? 0 : feedOffset;
        try {
            const resp = await wireApiCall('GET', `/api/v1/wire/feed?mode=${feedMode}&limit=${LIMIT}&offset=${newOffset}`) as
                { feed?: ContributionSummary[]; contributions?: ContributionSummary[]; items?: ContributionSummary[]; has_more?: boolean } | ContributionSummary[];
            const items = Array.isArray(resp) ? resp : (resp?.feed || resp?.contributions || resp?.items || []);
            const serverHasMore = !Array.isArray(resp) ? resp?.has_more : undefined;
            if (reset) {
                setFeedItems(items);
                setFeedOffset(items.length);
            } else {
                setFeedItems((prev) => [...prev, ...items]);
                setFeedOffset((prev) => prev + items.length);
            }
            setFeedHasMore(serverHasMore ?? items.length === LIMIT);
        } catch (err: unknown) {
            const parsed = parseApiError(err);
            setFeedError(parsed.message || 'Failed to load feed');
        } finally {
            setFeedLoading(false);
        }
    }, [wireApiCall, feedMode, feedOffset]);

    useEffect(() => {
        if (activeTab === 'feed') {
            fetchFeed(true);
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [feedMode, activeTab]);

    // === Search ===

    const refreshBalance = useCallback(async () => {
        try {
            const resp = await wireApiCall('GET', '/api/v1/wire/my/earnings') as { current_balance?: number };
            if (resp?.current_balance != null) {
                dispatch({ type: 'SET_CREDIT_BALANCE', balance: resp.current_balance });
            }
        } catch {
            // Non-critical — balance display may be stale
        }
    }, [wireApiCall, dispatch]);

    const executeSearch = useCallback(async (reset: boolean = true) => {
        if (!searchText.trim()) return;
        setSearchLoading(true);
        setSearchError(null);
        setShowCostWarning(false);
        const newOffset = reset ? 0 : searchOffset;
        try {
            const qs = buildQueryParams(searchText.trim(), filters, newOffset);
            const resp = await wireApiCall('GET', `/api/v1/wire/query?${qs}`) as {
                items?: ContributionSummary[];
                results?: ContributionSummary[];
                contributions?: ContributionSummary[];
                query_cost?: number;
                total?: number;
            };
            const items = resp?.items || resp?.results || resp?.contributions || (Array.isArray(resp) ? resp as ContributionSummary[] : []);
            if (resp?.query_cost != null) setLastQueryCost(resp.query_cost);
            if (reset) {
                setResults(items);
                setSearchOffset(items.length);
            } else {
                setResults((prev) => [...prev, ...items]);
                setSearchOffset((prev) => prev + items.length);
            }
            setSearchHasMore(items.length === LIMIT);
            setActiveTab('results');
            // Refresh balance after search (costs credits)
            refreshBalance();
        } catch (err: unknown) {
            const parsed = parseApiError(err);
            setSearchError(parsed.message || 'Search failed');
        } finally {
            setSearchLoading(false);
        }
    }, [wireApiCall, searchText, filters, searchOffset, refreshBalance]);

    const handleSearchSubmit = useCallback(() => {
        if (!searchText.trim()) return;
        executeSearch(true);
    }, [searchText, executeSearch]);

    const handleSearchKeyDown = useCallback((e: React.KeyboardEvent) => {
        if (e.key === 'Enter') handleSearchSubmit();
    }, [handleSearchSubmit]);

    // === Expand contribution ===

    const handleExpand = useCallback(async (id: string) => {
        if (expandedId === id) {
            setExpandedId(null);
            setExpandedBody(null);
            return;
        }
        setExpandedId(id);
        setExpandedBody(null);
        setExpandLoading(true);
        try {
            const resp = await wireApiCall('GET', `/api/v1/wire/contribution/${id}`) as {
                body?: string;
                content?: string;
            };
            setExpandedBody(resp?.body || resp?.content || null);
            refreshBalance();
        } catch (err: unknown) {
            const parsed = parseApiError(err);
            setExpandedBody(`Error: ${parsed.message}`);
        } finally {
            setExpandLoading(false);
        }
    }, [wireApiCall, expandedId, refreshBalance]);

    // === Rating/Flag/Respond ===

    // Reset rating state when expanding a different contribution
    useEffect(() => {
        setRatingAccuracy(0.5);
        setRatingUsefulness(0.5);
        setRatingSuccess(null);
        setRatingError(null);
        setFlagOpen(false);
    }, [expandedId]);

    const handleSubmitRating = useCallback(async (contributionId: string) => {
        setRatingSubmitting(true);
        setRatingError(null);
        setRatingSuccess(null);
        try {
            await wireApiCall('POST', '/api/v1/wire/rate', {
                item_id: contributionId,
                item_type: 'contribution',
                accuracy: ratingAccuracy,
                usefulness: ratingUsefulness,
            });
            setRatingSuccess('Rating submitted');
        } catch (err: unknown) {
            const msg = err instanceof Error ? err.message : String(err);
            if (msg.includes('own') || msg.includes('operator')) {
                setRatingError('Cannot rate your own or same-operator contributions');
            } else {
                setRatingError(msg || 'Failed to submit rating');
            }
        } finally {
            setRatingSubmitting(false);
        }
    }, [wireApiCall, ratingAccuracy, ratingUsefulness]);

    const handleSubmitFlag = useCallback(async (contributionId: string, flag: string) => {
        setFlagSubmitting(true);
        setRatingError(null);
        try {
            await wireApiCall('POST', '/api/v1/wire/rate', {
                item_id: contributionId,
                item_type: 'contribution',
                flag,
            });
            setRatingSuccess(`Flagged as ${flag.replace(/_/g, ' ')}`);
            setFlagOpen(false);
        } catch (err: unknown) {
            const msg = err instanceof Error ? err.message : String(err);
            setRatingError(msg || 'Failed to submit flag');
        } finally {
            setFlagSubmitting(false);
        }
    }, [wireApiCall]);

    const handleRespond = useCallback((contributionId: string) => {
        // Navigate to compose mode with contribution ID pre-filled as target
        navigateView('compose', 'respond', { targetContributionId: contributionId });
        setMode('compose');
    }, [setMode, navigateView]);

    const renderContributionActions = useCallback((contribution: ContributionSummary) => {
        if (expandedId !== contribution.id) return null;
        return (
            <div className="rating-panel" onClick={(e) => e.stopPropagation()}>
                <div className="rating-panel-header">
                    <span className="rating-panel-title">Rate Contribution</span>
                    <span className="rating-panel-constraint" title="Server enforces: cannot rate own contributions or contributions from your operator's agents">
                        Cannot rate own/same-operator contributions
                    </span>
                </div>
                <div className="rating-sliders">
                    <div className="rating-slider-row">
                        <label className="rating-slider-label">Accuracy</label>
                        <input
                            type="range"
                            className="rating-slider"
                            min="0"
                            max="1"
                            step="0.05"
                            value={ratingAccuracy}
                            onChange={(e) => setRatingAccuracy(parseFloat(e.target.value))}
                            disabled={ratingSubmitting}
                        />
                        <span className="rating-slider-value">{ratingAccuracy.toFixed(2)}</span>
                    </div>
                    <div className="rating-slider-row">
                        <label className="rating-slider-label">Usefulness</label>
                        <input
                            type="range"
                            className="rating-slider"
                            min="0"
                            max="1"
                            step="0.05"
                            value={ratingUsefulness}
                            onChange={(e) => setRatingUsefulness(parseFloat(e.target.value))}
                            disabled={ratingSubmitting}
                        />
                        <span className="rating-slider-value">{ratingUsefulness.toFixed(2)}</span>
                    </div>
                    <button
                        className="rating-submit-btn"
                        onClick={() => handleSubmitRating(contribution.id)}
                        disabled={ratingSubmitting}
                    >
                        {ratingSubmitting ? 'Submitting...' : 'Submit Rating'}
                    </button>
                </div>

                {/* Flag dropdown */}
                <div className="rating-flag-section">
                    <button
                        className="rating-flag-toggle"
                        onClick={() => setFlagOpen(!flagOpen)}
                        disabled={flagSubmitting}
                    >
                        Flag {flagOpen ? '\u25B2' : '\u25BC'}
                    </button>
                    {flagOpen && (
                        <div className="rating-flag-dropdown">
                            {(['outdated_data', 'harmful_content', 'plagiarism'] as const).map((flag) => (
                                <button
                                    key={flag}
                                    className="rating-flag-option"
                                    onClick={() => handleSubmitFlag(contribution.id, flag)}
                                    disabled={flagSubmitting}
                                >
                                    {flag.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase())}
                                </button>
                            ))}
                        </div>
                    )}
                </div>

                {/* Respond button */}
                <button
                    className="rating-respond-btn"
                    onClick={() => handleRespond(contribution.id)}
                >
                    Respond in Compose
                </button>

                {/* Feedback messages */}
                {ratingSuccess && <div className="rating-feedback rating-feedback-success">{ratingSuccess}</div>}
                {ratingError && <div className="rating-feedback rating-feedback-error">{ratingError}</div>}
            </div>
        );
    }, [expandedId, ratingAccuracy, ratingUsefulness, ratingSubmitting, flagOpen, flagSubmitting, ratingSuccess, ratingError, handleSubmitRating, handleSubmitFlag, handleRespond]);

    // === Topics ===

    const fetchTopics = useCallback(async () => {
        setTopicsLoading(true);
        setTopicsError(null);
        try {
            const resp = await wireApiCall('GET', '/api/v1/wire/topics') as
                { topics?: WireTopic[] } | WireTopic[];
            const items = Array.isArray(resp) ? resp : (resp?.topics || []);
            setTopics(items);
        } catch (err: unknown) {
            const parsed = parseApiError(err);
            setTopicsError(parsed.message || 'Failed to load topics');
        } finally {
            setTopicsLoading(false);
        }
    }, [wireApiCall]);

    useEffect(() => {
        if (activeTab === 'topics' && topics.length === 0) {
            fetchTopics();
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [activeTab]);

    const handleTopicSelect = useCallback(async (topic: WireTopic) => {
        if (selectedTopic?.id === topic.id) {
            setSelectedTopic(null);
            setTopicContributions([]);
            return;
        }
        setSelectedTopic(topic);
        setTopicLoading(true);
        try {
            // Search by topic name via feed or query
            const resp = await wireApiCall('GET', `/api/v1/wire/feed?topic=${encodeURIComponent(topic.name)}&limit=${LIMIT}`) as
                { feed?: ContributionSummary[]; contributions?: ContributionSummary[]; items?: ContributionSummary[] } | ContributionSummary[];
            const items = Array.isArray(resp) ? resp : (resp?.feed || resp?.contributions || resp?.items || []);
            setTopicContributions(items);
        } catch {
            setTopicContributions([]);
        } finally {
            setTopicLoading(false);
        }
    }, [wireApiCall, selectedTopic]);

    // === Filter helpers ===

    const handleFilterChange = useCallback((key: keyof SearchFilters, value: string | string[]) => {
        setFilters((prev) => ({ ...prev, [key]: value }));
    }, []);

    const addFilterTopic = useCallback(() => {
        const trimmed = topicInput.trim().toLowerCase();
        if (trimmed && !filters.topics.includes(trimmed)) {
            setFilters((prev) => ({ ...prev, topics: [...prev.topics, trimmed] }));
            setTopicInput('');
        }
    }, [topicInput, filters.topics]);

    const removeFilterTopic = useCallback((index: number) => {
        setFilters((prev) => ({
            ...prev,
            topics: prev.topics.filter((_, i) => i !== index),
        }));
    }, []);

    const clearFilters = useCallback(() => {
        setFilters(DEFAULT_FILTERS);
        setTopicInput('');
    }, []);

    // === Render helpers ===

    function renderContributionList(
        items: ContributionSummary[],
        loading: boolean,
        error: string | null,
        hasMore: boolean,
        onLoadMore: () => void,
        onRetry: () => void,
        emptyMessage: string,
    ) {
        return (
            <>
                {error && (
                    <div className="search-error">
                        <span>{error}</span>
                        <button className="search-retry-btn" onClick={onRetry}>Retry</button>
                    </div>
                )}

                {loading && items.length === 0 && (
                    <div className="search-loading">
                        <div className="search-loading-spinner" />
                        Loading...
                    </div>
                )}

                {!loading && !error && items.length === 0 && (
                    <div className="search-empty">{emptyMessage}</div>
                )}

                <div className="search-results-list">
                    {items.map((item) => (
                        <ContributionCard
                            key={item.id}
                            contribution={item}
                            onExpand={handleExpand}
                            expanded={expandedId === item.id}
                            expandedBody={expandedId === item.id ? expandedBody : null}
                            loading={expandedId === item.id && expandLoading}
                            renderActions={renderContributionActions}
                        />
                    ))}
                </div>

                {hasMore && !loading && (
                    <button className="search-load-more" onClick={onLoadMore}>
                        Load More
                    </button>
                )}

                {loading && items.length > 0 && (
                    <div className="search-loading-more">Loading more...</div>
                )}
            </>
        );
    }

    // === Render ===

    return (
        <div className="mode-container search-mode">
            {/* Header */}
            <div className="search-header">
                <h2 className="search-title">Wire Search</h2>
                <div className="search-balance">
                    Balance: <strong>{(state.creditBalance || state.credits?.server_credit_balance || 0).toLocaleString()}</strong> credits
                </div>
            </div>

            {/* Search bar — always visible */}
            <div className="search-bar">
                <div className="search-input-row">
                    <input
                        ref={searchInputRef}
                        type="text"
                        className="search-input search-input-main"
                        placeholder="Search the Wire... (costs ~100 credits)"
                        value={searchText}
                        onChange={(e) => setSearchText(e.target.value)}
                        onKeyDown={handleSearchKeyDown}
                        onFocus={() => setShowCostWarning(true)}
                    />
                    <button
                        className="search-btn search-btn-primary"
                        onClick={handleSearchSubmit}
                        disabled={searchLoading || !searchText.trim()}
                    >
                        {searchLoading ? 'Searching...' : 'Search'}
                    </button>
                    <button
                        className={`search-btn search-btn-secondary ${showFilters ? 'search-btn-active' : ''}`}
                        onClick={() => setShowFilters(!showFilters)}
                    >
                        Filters {showFilters ? '\u25B2' : '\u25BC'}
                    </button>
                </div>

                {/* Cost warning */}
                {showCostWarning && searchText.trim() && (
                    <div className="search-cost-warning">
                        <span className="search-cost-icon">\u26A0</span>
                        <span>Base cost: ~100 credits</span>
                        {lastQueryCost != null && (
                            <span className="search-cost-last">Last query cost: {lastQueryCost} credits</span>
                        )}
                    </div>
                )}

                {/* Filter bar */}
                {showFilters && (
                    <div className="search-filters">
                        <div className="search-filters-row">
                            <div className="search-filter-group">
                                <label>Type</label>
                                <select
                                    className="search-select"
                                    value={filters.contributionType}
                                    onChange={(e) => handleFilterChange('contributionType', e.target.value)}
                                >
                                    {CONTRIBUTION_TYPES.map((t) => (
                                        <option key={t.value} value={t.value}>{t.label}</option>
                                    ))}
                                </select>
                            </div>
                            <div className="search-filter-group">
                                <label>Sort</label>
                                <select
                                    className="search-select"
                                    value={filters.sort}
                                    onChange={(e) => handleFilterChange('sort', e.target.value)}
                                >
                                    {SORT_OPTIONS.map((o) => (
                                        <option key={o.value} value={o.value}>{o.label}</option>
                                    ))}
                                </select>
                            </div>
                            <div className="search-filter-group">
                                <label>Significance</label>
                                <div className="search-filter-range">
                                    <input
                                        type="number"
                                        className="search-input-sm"
                                        placeholder="Min"
                                        value={filters.significanceMin}
                                        onChange={(e) => handleFilterChange('significanceMin', e.target.value)}
                                        min="0" max="100"
                                    />
                                    <span className="search-range-sep">-</span>
                                    <input
                                        type="number"
                                        className="search-input-sm"
                                        placeholder="Max"
                                        value={filters.significanceMax}
                                        onChange={(e) => handleFilterChange('significanceMax', e.target.value)}
                                        min="0" max="100"
                                    />
                                </div>
                            </div>
                            <div className="search-filter-group">
                                <label>Price</label>
                                <div className="search-filter-range">
                                    <input
                                        type="number"
                                        className="search-input-sm"
                                        placeholder="Min"
                                        value={filters.priceMin}
                                        onChange={(e) => handleFilterChange('priceMin', e.target.value)}
                                        min="0"
                                    />
                                    <span className="search-range-sep">-</span>
                                    <input
                                        type="number"
                                        className="search-input-sm"
                                        placeholder="Max"
                                        value={filters.priceMax}
                                        onChange={(e) => handleFilterChange('priceMax', e.target.value)}
                                        min="0"
                                    />
                                </div>
                            </div>
                        </div>
                        <div className="search-filters-row">
                            <div className="search-filter-group">
                                <label>Date From</label>
                                <input
                                    type="date"
                                    className="search-input-date"
                                    value={filters.dateFrom}
                                    onChange={(e) => handleFilterChange('dateFrom', e.target.value)}
                                />
                            </div>
                            <div className="search-filter-group">
                                <label>Date To</label>
                                <input
                                    type="date"
                                    className="search-input-date"
                                    value={filters.dateTo}
                                    onChange={(e) => handleFilterChange('dateTo', e.target.value)}
                                />
                            </div>
                            <div className="search-filter-group search-filter-topics">
                                <label>Topics</label>
                                <div className="search-topic-input-row">
                                    <input
                                        type="text"
                                        className="search-input-sm"
                                        placeholder="Add topic"
                                        value={topicInput}
                                        onChange={(e) => setTopicInput(e.target.value)}
                                        onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); addFilterTopic(); } }}
                                    />
                                    <button className="search-btn-xs" onClick={addFilterTopic} disabled={!topicInput.trim()}>+</button>
                                </div>
                                {filters.topics.length > 0 && (
                                    <div className="search-topic-chips">
                                        {filters.topics.map((t, i) => (
                                            <span key={t} className="search-topic-chip">
                                                {t}
                                                <button className="search-topic-remove" onClick={() => removeFilterTopic(i)}>x</button>
                                            </span>
                                        ))}
                                    </div>
                                )}
                            </div>
                            <div className="search-filter-group search-filter-actions">
                                <button className="search-btn search-btn-ghost" onClick={clearFilters}>Clear Filters</button>
                            </div>
                        </div>
                    </div>
                )}
            </div>

            {/* Tab bar */}
            <div className="search-tabs">
                <button
                    className={`search-tab ${activeTab === 'feed' ? 'search-tab-active' : ''}`}
                    onClick={() => setActiveTab('feed')}
                >
                    Feed
                </button>
                <button
                    className={`search-tab ${activeTab === 'results' ? 'search-tab-active' : ''}`}
                    onClick={() => setActiveTab('results')}
                    disabled={results.length === 0 && !searchError}
                >
                    Results {results.length > 0 && `(${results.length})`}
                </button>
                <button
                    className={`search-tab ${activeTab === 'entities' ? 'search-tab-active' : ''}`}
                    onClick={() => setActiveTab('entities')}
                >
                    Entities
                </button>
                <button
                    className={`search-tab ${activeTab === 'topics' ? 'search-tab-active' : ''}`}
                    onClick={() => setActiveTab('topics')}
                >
                    Topics
                </button>
                <button
                    className={`search-tab ${activeTab === 'pyramids' ? 'search-tab-active' : ''}`}
                    onClick={() => setActiveTab('pyramids')}
                >
                    Pyramids
                </button>
            </div>

            {/* Tab content */}
            <div className="search-content">
                {/* Feed tab */}
                {activeTab === 'feed' && (
                    <div className="search-feed">
                        <div className="search-feed-modes">
                            {(['new', 'popular', 'trending'] as FeedMode[]).map((mode) => (
                                <button
                                    key={mode}
                                    className={`search-filter-chip ${feedMode === mode ? 'search-filter-chip-active' : ''}`}
                                    onClick={() => setFeedMode(mode)}
                                >
                                    {mode.charAt(0).toUpperCase() + mode.slice(1)}
                                </button>
                            ))}
                        </div>
                        {renderContributionList(
                            feedItems,
                            feedLoading,
                            feedError,
                            feedHasMore,
                            () => fetchFeed(false),
                            () => fetchFeed(true),
                            'No contributions in feed yet. Check back soon.',
                        )}
                    </div>
                )}

                {/* Results tab */}
                {activeTab === 'results' && (
                    <div className="search-results">
                        {lastQueryCost != null && (
                            <div className="search-results-cost">
                                Query cost: {lastQueryCost} credits
                            </div>
                        )}
                        {renderContributionList(
                            results,
                            searchLoading,
                            searchError,
                            searchHasMore,
                            () => executeSearch(false),
                            () => executeSearch(true),
                            'No results found. Try different search terms or filters.',
                        )}
                    </div>
                )}

                {/* Entities tab */}
                {activeTab === 'entities' && (
                    <EntityBrowser />
                )}

                {/* Topics tab */}
                {activeTab === 'topics' && (
                    <div className="search-topics">
                        {topicsError && (
                            <div className="search-error">
                                <span>{topicsError}</span>
                                <button className="search-retry-btn" onClick={fetchTopics}>Retry</button>
                            </div>
                        )}

                        {topicsLoading && (
                            <div className="search-loading">
                                <div className="search-loading-spinner" />
                                Loading topics...
                            </div>
                        )}

                        {!topicsLoading && !topicsError && topics.length === 0 && (
                            <div className="search-empty">No topics available.</div>
                        )}

                        <div className="search-topics-grid">
                            {topics.map((topic) => (
                                <div
                                    key={topic.id}
                                    className={`search-topic-card ${selectedTopic?.id === topic.id ? 'search-topic-card-selected' : ''}`}
                                    onClick={() => handleTopicSelect(topic)}
                                    role="button"
                                    tabIndex={0}
                                    onKeyDown={(e) => { if (e.key === 'Enter') handleTopicSelect(topic); }}
                                >
                                    <div className="search-topic-card-name">{topic.name}</div>
                                    {topic.contribution_count != null && (
                                        <div className="search-topic-card-count">{topic.contribution_count} contributions</div>
                                    )}
                                    {topic.description && (
                                        <div className="search-topic-card-desc">{topic.description}</div>
                                    )}
                                </div>
                            ))}
                        </div>

                        {selectedTopic && (
                            <div className="search-topic-detail">
                                <h3 className="search-topic-detail-title">
                                    Contributions tagged: {selectedTopic.name}
                                </h3>
                                {topicLoading ? (
                                    <div className="search-loading">Loading...</div>
                                ) : topicContributions.length === 0 ? (
                                    <div className="search-empty">No contributions found for this topic.</div>
                                ) : (
                                    <div className="search-results-list">
                                        {topicContributions.map((item) => (
                                            <ContributionCard
                                                key={item.id}
                                                contribution={item}
                                                onExpand={handleExpand}
                                                expanded={expandedId === item.id}
                                                expandedBody={expandedId === item.id ? expandedBody : null}
                                                loading={expandedId === item.id && expandLoading}
                                                renderActions={renderContributionActions}
                                            />
                                        ))}
                                    </div>
                                )}
                            </div>
                        )}
                    </div>
                )}

                {/* Pyramids tab — discover published pyramids on the Wire */}
                {activeTab === 'pyramids' && (
                    <div className="search-pyramids">
                        <p style={{ color: 'var(--text-secondary)', fontSize: '14px', marginBottom: '16px' }}>
                            Discover knowledge pyramids published by other operators on the Wire.
                            Query costs: 1 credit stamp + access price (if set).
                        </p>
                        <div style={{ padding: '24px', textAlign: 'center', color: 'var(--text-tertiary)' }}>
                            <p>Pyramid discovery will search the Wire for published pyramid metadata.</p>
                            <p style={{ fontSize: '13px', marginTop: '8px' }}>
                                Use the search bar above with the Pyramids tab selected to find pyramids by topic.
                            </p>
                            <p style={{ fontSize: '12px', marginTop: '16px', color: 'var(--text-tertiary)' }}>
                                Coming: browsable pyramid catalog with access tiers, pricing, and one-click remote query.
                            </p>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
}
