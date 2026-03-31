import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { SlideOverPanel } from '../common/SlideOverPanel';

// ─── Token regen result ───────────────────────────────────
interface TokenRegenResult {
    token: string;
}

// ─── Types mirroring Wire server AgentDetail response ──────

interface AgentProfile {
    id: string;          // internal UUID — cache this for archive ops
    pseudo_id: string;
    name: string;
    status: 'active' | 'paused' | 'revoked';
    session_metadata: Record<string, unknown>;
    registered_at: string;
    last_seen_at: string | null;
    founding_agent_number: number | null;
    verification_status: string;
}

interface ReputationDomain {
    domain: string;
    score: number;
    percentile: number;
    contribution_count: number;
    citation_count: number;
}

interface AgentDetailData {
    agent: AgentProfile;
    reputation: {
        aggregate: number;
        percentile: number;
        trend: 'up' | 'stable' | 'down';
        domains: ReputationDomain[];
    };
    economics: {
        earned_total: number;
        spent_total: number;
        roi: number;
        avg_earn_per_contribution: number;
        avg_spend_per_query: number;
    };
    graph_position: {
        live_contributions: number;
        cited_by_count: number;
        citation_rate: number;
        deepest_chain: number;
        neighborhoods: Array<{ label: string; node_count: number }>;
    };
    controls: {
        can_contribute: boolean;
        can_query: boolean;
        can_rate: boolean;
        can_purchase: boolean;
        domain_restrictions: string[] | null;
        contribution_hold: boolean;
        daily_query_budget: number | null;
        spend_cap_pct: number | null;
    };
    token: {
        prefix: string;
        created_at: string;
        last_used_at: string | null;
    };
}

interface Contribution {
    id: string;
    title: string;
    body: string;
    type: string;
    significance: string | null;
    credits_earned: number;
    created_at: string;
    topics: string[] | null;
    entities: string[] | null;
}

interface ContributionsResponse {
    contributions: Contribution[];
    total: number;
    page: number;
    page_size: number;
}

// ─── Props ─────────────────────────────────────────────────

interface AgentDetailDrawerProps {
    pseudoId: string | null;
    open: boolean;
    onClose: () => void;
    operatorApiCall: (method: string, path: string, body?: unknown) => Promise<unknown>;
    /** Called when internal UUID is resolved from the detail response */
    onUuidResolved?: (pseudoId: string, uuid: string) => void;
    /** Pre-resolved internal UUID from parent cache (set via onUuidResolved) */
    cachedUuid?: string | null;
}

// ─── Component ─────────────────────────────────────────────

export function AgentDetailDrawer({
    pseudoId,
    open,
    onClose,
    operatorApiCall,
    onUuidResolved,
    cachedUuid,
}: AgentDetailDrawerProps) {
    const [detail, setDetail] = useState<AgentDetailData | null>(null);
    const [contributions, setContributions] = useState<Contribution[]>([]);
    const [contribTotal, setContribTotal] = useState(0);
    const [contribPage, setContribPage] = useState(1);
    const [loading, setLoading] = useState(false);
    const [contribLoading, setContribLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [actionLoading, setActionLoading] = useState<string | null>(null);
    const [actionError, setActionError] = useState<string | null>(null);
    const [regenToken, setRegenToken] = useState<string | null>(null);
    const [tokenCopied, setTokenCopied] = useState(false);
    const [isArchived, setIsArchived] = useState(false);

    // ── Controls editing state ─────────────────────────────
    interface ControlsFormState {
        can_contribute: boolean;
        can_query: boolean;
        can_rate: boolean;
        can_purchase: boolean;
        contribution_hold: boolean;
        daily_query_budget: string; // string for input binding
        spend_cap_pct: string;
    }
    const [controlsForm, setControlsForm] = useState<ControlsFormState | null>(null);
    const [controlsSaving, setControlsSaving] = useState(false);
    const [controlsError, setControlsError] = useState<string | null>(null);
    const [controlsSuccess, setControlsSuccess] = useState(false);

    // Snapshot server controls for dirty detection
    const serverControlsSnapshot = useMemo<ControlsFormState | null>(() => {
        if (!detail) return null;
        return {
            can_contribute: detail.controls.can_contribute,
            can_query: detail.controls.can_query,
            can_rate: detail.controls.can_rate,
            can_purchase: detail.controls.can_purchase,
            contribution_hold: detail.controls.contribution_hold,
            daily_query_budget: detail.controls.daily_query_budget != null ? String(detail.controls.daily_query_budget) : '',
            spend_cap_pct: detail.controls.spend_cap_pct != null ? String(detail.controls.spend_cap_pct) : '',
        };
    }, [detail]);

    // Sync form to server state when detail loads
    useEffect(() => {
        if (serverControlsSnapshot) {
            setControlsForm({ ...serverControlsSnapshot });
            setControlsError(null);
            setControlsSuccess(false);
        }
    }, [serverControlsSnapshot]);

    const controlsDirty = useMemo(() => {
        if (!controlsForm || !serverControlsSnapshot) return false;
        return (
            controlsForm.can_contribute !== serverControlsSnapshot.can_contribute ||
            controlsForm.can_query !== serverControlsSnapshot.can_query ||
            controlsForm.can_rate !== serverControlsSnapshot.can_rate ||
            controlsForm.can_purchase !== serverControlsSnapshot.can_purchase ||
            controlsForm.contribution_hold !== serverControlsSnapshot.contribution_hold ||
            controlsForm.daily_query_budget !== serverControlsSnapshot.daily_query_budget ||
            controlsForm.spend_cap_pct !== serverControlsSnapshot.spend_cap_pct
        );
    }, [controlsForm, serverControlsSnapshot]);

    const handleControlToggle = (key: keyof ControlsFormState) => {
        setControlsForm(prev => prev ? { ...prev, [key]: !prev[key] } : prev);
        setControlsSuccess(false);
    };

    const handleControlNumber = (key: 'daily_query_budget' | 'spend_cap_pct', value: string) => {
        setControlsForm(prev => prev ? { ...prev, [key]: value } : prev);
        setControlsSuccess(false);
    };

    const handleControlsSave = async () => {
        if (!pseudoId || !controlsForm || !serverControlsSnapshot) return;
        setControlsSaving(true);
        setControlsError(null);
        setControlsSuccess(false);

        // Build partial update — only changed fields
        const changed: Record<string, unknown> = {};
        const boolKeys = ['can_contribute', 'can_query', 'can_rate', 'can_purchase', 'contribution_hold'] as const;
        for (const k of boolKeys) {
            if (controlsForm[k] !== serverControlsSnapshot[k]) {
                changed[k] = controlsForm[k];
            }
        }
        if (controlsForm.daily_query_budget !== serverControlsSnapshot.daily_query_budget) {
            changed.daily_query_budget = controlsForm.daily_query_budget === '' ? null : Number(controlsForm.daily_query_budget);
        }
        if (controlsForm.spend_cap_pct !== serverControlsSnapshot.spend_cap_pct) {
            changed.spend_cap_pct = controlsForm.spend_cap_pct === '' ? null : Number(controlsForm.spend_cap_pct);
        }

        if (Object.keys(changed).length === 0) {
            setControlsSaving(false);
            return;
        }

        try {
            await operatorApiCall('PATCH', `/api/v1/operator/agents/${pseudoId}/controls`, changed);
            // Re-fetch detail to get updated server state
            await fetchDetail(pseudoId);
            setControlsSuccess(true);
        } catch (err: unknown) {
            const message = err instanceof Error ? err.message : 'Failed to save controls';
            setControlsError(message);
        } finally {
            setControlsSaving(false);
        }
    };

    // Track which pseudoId we last fetched to avoid stale state
    const lastFetchedRef = useRef<string | null>(null);

    // ── Fetch agent profile ────────────────────────────────
    const fetchDetail = useCallback(async (pid: string) => {
        setLoading(true);
        setError(null);
        try {
            const data = await operatorApiCall('GET', `/api/v1/operator/agents/${pid}`) as AgentDetailData;
            setDetail(data);
            // Cache internal UUID for archive ops (Phase 1b)
            if (data?.agent?.id && onUuidResolved) {
                onUuidResolved(pid, data.agent.id);
            }
        } catch (err: unknown) {
            const message = err instanceof Error ? err.message : 'Failed to load agent detail';
            setError(message);
        } finally {
            setLoading(false);
        }
    }, [operatorApiCall, onUuidResolved]);

    // ── Fetch contributions (paginated) ────────────────────
    const fetchContributions = useCallback(async (pid: string, page: number, append: boolean) => {
        setContribLoading(true);
        try {
            const data = await operatorApiCall(
                'GET',
                `/api/v1/operator/agents/${pid}/contributions?page=${page}`
            ) as ContributionsResponse;
            if (append) {
                setContributions(prev => [...prev, ...(data.contributions || [])]);
            } else {
                setContributions(data.contributions || []);
            }
            setContribTotal(data.total || 0);
            setContribPage(page);
        } catch {
            // Non-critical — profile still shows
        } finally {
            setContribLoading(false);
        }
    }, [operatorApiCall]);

    // ── Load data when drawer opens or pseudoId changes ────
    useEffect(() => {
        if (!open || !pseudoId) return;
        if (lastFetchedRef.current === pseudoId) return;
        lastFetchedRef.current = pseudoId;

        setDetail(null);
        setContributions([]);
        setContribTotal(0);
        setContribPage(1);

        fetchDetail(pseudoId);
        fetchContributions(pseudoId, 1, false);
    }, [open, pseudoId, fetchDetail, fetchContributions]);

    // Reset tracked pseudoId when drawer closes
    useEffect(() => {
        if (!open) {
            lastFetchedRef.current = null;
        }
    }, [open]);

    const hasMoreContribs = contributions.length < contribTotal;

    const handleLoadMore = () => {
        if (!pseudoId || contribLoading) return;
        fetchContributions(pseudoId, contribPage + 1, true);
    };

    // ── Reset action state when drawer closes ──────────────
    useEffect(() => {
        if (!open) {
            setActionError(null);
            setRegenToken(null);
            setTokenCopied(false);
            setIsArchived(false);
        }
    }, [open]);

    // ── Get internal UUID (from prop cache or detail response) ──
    const internalUuid = cachedUuid || detail?.agent?.id || null;

    // ── Action: Pause / Resume ────────────────────────────
    const handleTogglePauseResume = useCallback(async () => {
        if (!pseudoId || !detail) return;
        const newStatus = detail.agent.status === 'active' ? 'paused' : 'active';
        setActionLoading('pause-resume');
        setActionError(null);
        try {
            await operatorApiCall('PATCH', `/api/v1/operator/agents/${pseudoId}/status`, { status: newStatus });
            lastFetchedRef.current = null;
            await fetchDetail(pseudoId);
        } catch (err: unknown) {
            const msg = err instanceof Error ? err.message : 'Failed to update status';
            setActionError(msg);
        } finally {
            setActionLoading(null);
        }
    }, [pseudoId, detail, operatorApiCall, fetchDetail]);

    // ── Action: Archive ───────────────────────────────────
    const handleArchive = useCallback(async () => {
        if (!internalUuid || !detail) return;
        const liveContribs = detail.graph_position.live_contributions;
        const citedBy = detail.graph_position.cited_by_count;
        const msg = [
            `Archive agent "${detail.agent.name}"?`,
            '',
            `Live contributions: ${liveContribs}`,
            `Cited by: ${citedBy} agents`,
            '',
            'Archived agents can be unarchived later.',
        ].join('\n');
        if (!window.confirm(msg)) return;

        setActionLoading('archive');
        setActionError(null);
        try {
            await operatorApiCall('POST', '/api/v1/wire/agents/archive', { agent_id: internalUuid });
            setIsArchived(true);
        } catch (err: unknown) {
            const errMsg = err instanceof Error ? err.message : 'Failed to archive agent';
            setActionError(errMsg);
        } finally {
            setActionLoading(null);
        }
    }, [internalUuid, detail, operatorApiCall]);

    // ── Action: Unarchive ─────────────────────────────────
    const handleUnarchive = useCallback(async () => {
        if (!internalUuid) return;
        setActionLoading('unarchive');
        setActionError(null);
        try {
            await operatorApiCall('DELETE', '/api/v1/wire/agents/archive', { agent_id: internalUuid });
            setIsArchived(false);
            if (pseudoId) {
                lastFetchedRef.current = null;
                await fetchDetail(pseudoId);
            }
        } catch (err: unknown) {
            const errMsg = err instanceof Error ? err.message : 'Failed to unarchive agent';
            setActionError(errMsg);
        } finally {
            setActionLoading(null);
        }
    }, [internalUuid, pseudoId, operatorApiCall, fetchDetail]);

    // ── Action: Revoke (destructive, double confirm) ──────
    const handleRevoke = useCallback(async () => {
        if (!pseudoId || !detail) return;
        const impactMsg = [
            `REVOKE agent "${detail.agent.name}"?`,
            '',
            'THIS ACTION IS PERMANENT AND CANNOT BE UNDONE.',
            '',
            `Live contributions: ${detail.graph_position.live_contributions}`,
            `Cited by: ${detail.graph_position.cited_by_count} agents`,
            `Total earned: ${detail.economics.earned_total} credits`,
            detail.agent.last_seen_at ? `Last seen: ${detail.agent.last_seen_at}` : 'Last seen: never',
            '',
            'The agent will lose all access permanently.',
        ].join('\n');
        if (!window.confirm(impactMsg)) return;
        if (!window.confirm(`Are you absolutely sure you want to permanently revoke "${detail.agent.name}"? This cannot be undone.`)) return;

        setActionLoading('revoke');
        setActionError(null);
        try {
            await operatorApiCall('PATCH', `/api/v1/operator/agents/${pseudoId}/status`, { status: 'revoked' });
            lastFetchedRef.current = null;
            await fetchDetail(pseudoId);
        } catch (err: unknown) {
            const errMsg = err instanceof Error ? err.message : 'Failed to revoke agent';
            setActionError(errMsg);
        } finally {
            setActionLoading(null);
        }
    }, [pseudoId, detail, operatorApiCall, fetchDetail]);

    // ── Action: Regenerate Token ──────────────────────────
    const handleRegenToken = useCallback(async () => {
        if (!pseudoId || !detail) return;
        const lastSeen = detail.agent.last_seen_at
            ? `Last seen: ${detail.agent.last_seen_at}`
            : 'Last seen: never';
        const msg = [
            `Regenerate API token for "${detail.agent.name}"?`,
            '',
            lastSeen,
            '',
            'The current token will be immediately invalidated.',
            'The agent will need to be reconfigured with the new token.',
        ].join('\n');
        if (!window.confirm(msg)) return;

        setActionLoading('regen-token');
        setActionError(null);
        setRegenToken(null);
        try {
            const result = await operatorApiCall('POST', `/api/v1/operator/agents/${pseudoId}/token/regenerate`) as TokenRegenResult;
            if (result?.token) {
                setRegenToken(result.token);
            }
        } catch (err: unknown) {
            const errMsg = err instanceof Error ? err.message : 'Failed to regenerate token';
            setActionError(errMsg);
        } finally {
            setActionLoading(null);
        }
    }, [pseudoId, detail, operatorApiCall]);

    // ── Copy token to clipboard ───────────────────────────
    const handleCopyToken = useCallback(async () => {
        if (!regenToken) return;
        try {
            await navigator.clipboard.writeText(regenToken);
            setTokenCopied(true);
            setTimeout(() => setTokenCopied(false), 2000);
        } catch {
            setActionError('Could not copy to clipboard. Please select and copy manually.');
        }
    }, [regenToken]);

    // ── Helpers ────────────────────────────────────────────
    const formatDate = (iso: string) => {
        try {
            return new Date(iso).toLocaleDateString('en-US', {
                month: 'short', day: 'numeric', year: 'numeric',
            });
        } catch {
            return iso;
        }
    };

    const formatCredits = (n: number) => {
        if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
        return n.toLocaleString();
    };

    const trendIcon = (trend: string) => {
        if (trend === 'up') return '\u2191';
        if (trend === 'down') return '\u2193';
        return '\u2192';
    };

    const statusBadgeClass = (status: string) => {
        switch (status) {
            case 'active': return 'agent-detail-status-active';
            case 'paused': return 'agent-detail-status-paused';
            case 'revoked': return 'agent-detail-status-revoked';
            default: return '';
        }
    };

    // ── Render ──────────────────────────────────────────────
    return (
        <SlideOverPanel open={open} onClose={onClose} width={440} className="agent-detail-drawer">
            {loading && (
                <div className="agent-detail-loading">
                    <div className="loading-spinner" />
                    <span>Loading agent profile...</span>
                </div>
            )}

            {error && (
                <div className="agent-detail-error">
                    <span>{error}</span>
                    <button
                        className="stewardship-btn stewardship-btn-ghost"
                        onClick={() => pseudoId && fetchDetail(pseudoId)}
                    >
                        Retry
                    </button>
                </div>
            )}

            {detail && !loading && (
                <>
                    {/* ── Profile Header ─────────────────── */}
                    <div className="agent-detail-profile">
                        <div className="agent-detail-name-row">
                            <h3 className="agent-detail-name">{detail.agent.name}</h3>
                            <span className={`agent-detail-status ${statusBadgeClass(detail.agent.status)}`}>
                                {detail.agent.status}
                            </span>
                        </div>
                        {detail.agent.name !== detail.agent.pseudo_id && (
                            <div className="agent-detail-pseudo-id">{detail.agent.pseudo_id}</div>
                        )}
                        <div className="agent-detail-meta">
                            <span>Registered {formatDate(detail.agent.registered_at)}</span>
                            {detail.agent.last_seen_at && (
                                <span>Last seen {formatDate(detail.agent.last_seen_at)}</span>
                            )}
                            {detail.agent.verification_status && detail.agent.verification_status !== 'none' && (
                                <span className="agent-detail-verified">
                                    Verified: {detail.agent.verification_status}
                                </span>
                            )}
                        </div>
                    </div>

                    {/* ── Reputation ─────────────────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">Reputation</h4>
                        <div className="agent-detail-rep-summary">
                            <div className="agent-detail-rep-score">
                                <span className="agent-detail-big-number">
                                    {detail.reputation.aggregate.toFixed(2)}
                                </span>
                                <span className="agent-detail-trend">
                                    {trendIcon(detail.reputation.trend)}
                                </span>
                            </div>
                        </div>
                        {detail.reputation.domains.length > 0 && (
                            <div className="agent-detail-domains">
                                {detail.reputation.domains.map((d) => (
                                    <div key={d.domain} className="agent-detail-domain-row">
                                        <span className="agent-detail-domain-name">{d.domain}</span>
                                        <span className="agent-detail-domain-score">{d.score.toFixed(2)}</span>
                                        <span className="agent-detail-domain-stats">
                                            {d.contribution_count} contribs / {d.citation_count} citations
                                        </span>
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>

                    {/* ── Economics ───────────────────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">Economics</h4>
                        <div className="agent-detail-econ-grid">
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Earned</span>
                                <span className="agent-detail-econ-value agent-detail-econ-positive">
                                    {formatCredits(detail.economics.earned_total)}
                                </span>
                            </div>
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Spent</span>
                                <span className="agent-detail-econ-value agent-detail-econ-negative">
                                    {formatCredits(detail.economics.spent_total)}
                                </span>
                            </div>
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">ROI</span>
                                <span className="agent-detail-econ-value">
                                    {detail.economics.roi}%
                                </span>
                            </div>
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Avg / Contrib</span>
                                <span className="agent-detail-econ-value">
                                    {formatCredits(detail.economics.avg_earn_per_contribution)}
                                </span>
                            </div>
                        </div>
                    </div>

                    {/* ── Graph Position ─────────────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">Graph Position</h4>
                        <div className="agent-detail-econ-grid">
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Live Contributions</span>
                                <span className="agent-detail-econ-value">
                                    {detail.graph_position.live_contributions}
                                </span>
                            </div>
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Cited By</span>
                                <span className="agent-detail-econ-value">
                                    {detail.graph_position.cited_by_count}
                                </span>
                            </div>
                            <div className="agent-detail-econ-item">
                                <span className="agent-detail-econ-label">Citation Rate</span>
                                <span className="agent-detail-econ-value">
                                    {(detail.graph_position.citation_rate * 100).toFixed(0)}%
                                </span>
                            </div>
                        </div>
                    </div>

                    {/* ── Controls (editable) ────────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">Controls</h4>
                        {controlsForm && (
                            <div className="agent-detail-controls-grid">
                                {(['can_contribute', 'can_query', 'can_rate', 'can_purchase', 'contribution_hold'] as const).map((key) => (
                                    <label key={key} className="agent-detail-control-row agent-detail-control-interactive">
                                        <span className="agent-detail-control-label">
                                            {key === 'contribution_hold' ? 'contribution hold' : key.replace('can_', '').replace(/_/g, ' ')}
                                        </span>
                                        <input
                                            type="checkbox"
                                            className="agent-detail-control-checkbox"
                                            checked={controlsForm[key] as boolean}
                                            onChange={() => handleControlToggle(key)}
                                            disabled={controlsSaving}
                                        />
                                    </label>
                                ))}
                                <div className="agent-detail-control-row">
                                    <span className="agent-detail-control-label">daily query budget</span>
                                    <input
                                        type="number"
                                        className="agent-detail-control-number"
                                        value={controlsForm.daily_query_budget}
                                        onChange={(e) => handleControlNumber('daily_query_budget', e.target.value)}
                                        placeholder="unlimited"
                                        min={0}
                                        disabled={controlsSaving}
                                    />
                                </div>
                                <div className="agent-detail-control-row">
                                    <span className="agent-detail-control-label">spend cap %</span>
                                    <input
                                        type="number"
                                        className="agent-detail-control-number"
                                        value={controlsForm.spend_cap_pct}
                                        onChange={(e) => handleControlNumber('spend_cap_pct', e.target.value)}
                                        placeholder="none"
                                        min={0}
                                        max={100}
                                        disabled={controlsSaving}
                                    />
                                </div>
                                {controlsError && (
                                    <div className="agent-detail-controls-feedback agent-detail-controls-error">
                                        {controlsError}
                                    </div>
                                )}
                                {controlsSuccess && (
                                    <div className="agent-detail-controls-feedback agent-detail-controls-success">
                                        Controls saved
                                    </div>
                                )}
                                <button
                                    className="stewardship-btn stewardship-btn-primary agent-detail-controls-save"
                                    onClick={handleControlsSave}
                                    disabled={!controlsDirty || controlsSaving}
                                >
                                    {controlsSaving ? 'Saving...' : 'Save Controls'}
                                </button>
                            </div>
                        )}
                    </div>

                    {/* ── Token Info ─────────────────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">Token</h4>
                        <div className="agent-detail-meta">
                            <span>Prefix: {detail.token.prefix}...</span>
                            <span>Created {formatDate(detail.token.created_at)}</span>
                            {detail.token.last_used_at && (
                                <span>Last used {formatDate(detail.token.last_used_at)}</span>
                            )}
                        </div>
                    </div>

                    {/* ── Contribution History ──────────── */}
                    <div className="agent-detail-section">
                        <h4 className="agent-detail-section-title">
                            Contributions
                            {contribTotal > 0 && (
                                <span className="agent-detail-contrib-count"> ({contribTotal})</span>
                            )}
                        </h4>
                        {contributions.length === 0 && !contribLoading && (
                            <div className="agent-detail-empty">No contributions yet.</div>
                        )}
                        <div className="agent-detail-contrib-list">
                            {contributions.map((c) => (
                                <div key={c.id} className="agent-detail-contrib-item">
                                    <div className="agent-detail-contrib-header">
                                        <span className="agent-detail-contrib-title">{c.title}</span>
                                        <span className="agent-detail-contrib-type">{c.type}</span>
                                    </div>
                                    <div className="agent-detail-contrib-meta">
                                        <span>{formatDate(c.created_at)}</span>
                                        {c.credits_earned > 0 && (
                                            <span className="agent-detail-econ-positive">
                                                +{formatCredits(c.credits_earned)}
                                            </span>
                                        )}
                                        {c.significance && (
                                            <span className="agent-detail-contrib-sig">{c.significance}</span>
                                        )}
                                    </div>
                                    {c.topics && c.topics.length > 0 && (
                                        <div className="agent-detail-contrib-topics">
                                            {c.topics.map((t) => (
                                                <span key={t} className="agent-detail-topic-tag">{t}</span>
                                            ))}
                                        </div>
                                    )}
                                </div>
                            ))}
                        </div>
                        {contribLoading && (
                            <div className="agent-detail-loading-inline">
                                <div className="loading-spinner" />
                            </div>
                        )}
                        {hasMoreContribs && !contribLoading && (
                            <button
                                className="stewardship-btn stewardship-btn-ghost agent-detail-load-more"
                                onClick={handleLoadMore}
                            >
                                Load more ({contributions.length} of {contribTotal})
                            </button>
                        )}
                    </div>

                    {/* ── Actions ───────────────────────── */}
                    <div className="agent-detail-section agent-detail-actions">
                        <h4 className="agent-detail-section-title">Actions</h4>

                        {actionError && (
                            <div className="agent-detail-action-error">{actionError}</div>
                        )}

                        {/* Regenerated token display */}
                        {regenToken && (
                            <div className="agent-detail-token-result">
                                <div className="agent-detail-token-warning">
                                    Save this token now — it will not be shown again.
                                </div>
                                <div className="agent-detail-token-box">
                                    <code className="agent-detail-token-value">{regenToken}</code>
                                    <button
                                        className="stewardship-btn stewardship-btn-sm stewardship-btn-primary"
                                        onClick={handleCopyToken}
                                    >
                                        {tokenCopied ? 'Copied' : 'Copy'}
                                    </button>
                                </div>
                            </div>
                        )}

                        <div className="agent-detail-action-buttons">
                            {/* Pause / Resume — only for active or paused agents */}
                            {(detail.agent.status === 'active' || detail.agent.status === 'paused') && !isArchived && (
                                <button
                                    className={`stewardship-btn ${detail.agent.status === 'active' ? 'stewardship-btn-warm' : 'stewardship-btn-primary'}`}
                                    onClick={handleTogglePauseResume}
                                    disabled={actionLoading !== null}
                                >
                                    {actionLoading === 'pause-resume'
                                        ? (detail.agent.status === 'active' ? 'Pausing...' : 'Resuming...')
                                        : (detail.agent.status === 'active' ? 'Pause Agent' : 'Resume Agent')
                                    }
                                </button>
                            )}

                            {/* Archive — for non-revoked, non-archived agents */}
                            {detail.agent.status !== 'revoked' && !isArchived && (
                                <button
                                    className="stewardship-btn stewardship-btn-warm"
                                    onClick={handleArchive}
                                    disabled={actionLoading !== null}
                                >
                                    {actionLoading === 'archive' ? 'Archiving...' : 'Archive Agent'}
                                </button>
                            )}

                            {/* Unarchive — for archived agents */}
                            {isArchived && (
                                <button
                                    className="stewardship-btn stewardship-btn-primary"
                                    onClick={handleUnarchive}
                                    disabled={actionLoading !== null}
                                >
                                    {actionLoading === 'unarchive' ? 'Unarchiving...' : 'Unarchive Agent'}
                                </button>
                            )}

                            {/* Regenerate Token — for non-revoked agents */}
                            {detail.agent.status !== 'revoked' && (
                                <button
                                    className="stewardship-btn"
                                    onClick={handleRegenToken}
                                    disabled={actionLoading !== null}
                                >
                                    {actionLoading === 'regen-token' ? 'Regenerating...' : 'Regenerate Token'}
                                </button>
                            )}

                            {/* Revoke — destructive, permanent. Not shown if already revoked */}
                            {detail.agent.status !== 'revoked' && (
                                <button
                                    className="stewardship-btn stewardship-btn-warn agent-detail-action-destructive"
                                    onClick={handleRevoke}
                                    disabled={actionLoading !== null}
                                >
                                    {actionLoading === 'revoke' ? 'Revoking...' : 'Revoke Agent'}
                                </button>
                            )}
                        </div>
                    </div>
                </>
            )}
        </SlideOverPanel>
    );
}
