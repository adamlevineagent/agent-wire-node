import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { AgentDetailDrawer } from './AgentDetailDrawer';

interface RosterAgent {
    id: string;
    pseudonym: string;
    name?: string;
    status?: string;
    contribution_count?: number;
    reputation?: number;
    reputation_roi?: string;
    created_at?: string;
}

interface PulseData {
    fleet_online?: string[];
    [key: string]: unknown;
}

interface CreateAgentResponse {
    api_token: string;
    agent_id: string;
    pseudo_id?: string;
}

export function FleetOverview() {
    const { wireApiCall, operatorApiCall, state } = useAppContext();
    const [roster, setRoster] = useState<RosterAgent[]>([]);
    const [onlineIds, setOnlineIds] = useState<Set<string>>(new Set());
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // Agent detail drawer state
    const [drawerPseudoId, setDrawerPseudoId] = useState<string | null>(null);
    const [drawerOpen, setDrawerOpen] = useState(false);

    // Cache internal UUIDs resolved from detail responses (needed by Phase 1b for archive ops)
    const uuidCacheRef = useRef<Map<string, string>>(new Map());

    // ── Create Agent state ─────────────────────────────────
    const [showCreateForm, setShowCreateForm] = useState(false);
    const [createName, setCreateName] = useState('');
    const [createLoading, setCreateLoading] = useState(false);
    const [createError, setCreateError] = useState<string | null>(null);
    const [createdToken, setCreatedToken] = useState<string | null>(null);
    const [tokenCopied, setTokenCopied] = useState(false);

    // ── Filter state ───────────────────────────────────────
    const [filterText, setFilterText] = useState('');
    const [filterStatus, setFilterStatus] = useState<'all' | 'active' | 'paused' | 'archived'>('all');
    const [filterOnlineOnly, setFilterOnlineOnly] = useState(false);

    const handleAgentClick = (pseudoId: string) => {
        setDrawerPseudoId(pseudoId);
        setDrawerOpen(true);
    };

    const handleDrawerClose = () => {
        setDrawerOpen(false);
    };

    const handleUuidResolved = useCallback((pseudoId: string, uuid: string) => {
        uuidCacheRef.current.set(pseudoId, uuid);
    }, []);

    // ── Create Agent handler ──────────────────────────────
    const handleCreateAgent = async () => {
        const trimmed = createName.trim();
        if (!trimmed) return;
        setCreateLoading(true);
        setCreateError(null);

        try {
            const data = await wireApiCall('POST', '/api/v1/register', {
                name: trimmed,
                operator_email: state.email,
            }) as CreateAgentResponse;
            setCreatedToken(data.api_token);
            setTokenCopied(false);
            setCreateName('');
            setShowCreateForm(false);
            // Refresh roster to include the new agent
            fetchData();
        } catch (err: unknown) {
            const message = err instanceof Error ? err.message : 'Failed to create agent';
            if (message.includes('409') || message.toLowerCase().includes('taken') || message.toLowerCase().includes('exists')) {
                setCreateError('An agent with that name already exists. Choose a different name.');
            } else {
                setCreateError(message);
            }
        } finally {
            setCreateLoading(false);
        }
    };

    const handleCopyToken = () => {
        if (!createdToken) return;
        navigator.clipboard.writeText(createdToken).then(() => {
            setTokenCopied(true);
        }).catch(() => {
            // Fallback: select all in a temp textarea
        });
    };

    // ── Filtered roster ───────────────────────────────────
    const filteredRoster = useMemo(() => {
        let agents = roster;
        if (filterText) {
            const lower = filterText.toLowerCase();
            agents = agents.filter(a =>
                (a.name && a.name.toLowerCase().includes(lower)) ||
                a.pseudonym.toLowerCase().includes(lower)
            );
        }
        if (filterStatus !== 'all') {
            agents = agents.filter(a => a.status === filterStatus);
        }
        if (filterOnlineOnly) {
            agents = agents.filter(a => onlineIds.has(a.id));
        }
        return agents;
    }, [roster, filterText, filterStatus, filterOnlineOnly, onlineIds]);

    const fetchData = useCallback(async () => {
        setLoading(true);
        setError(null);

        const fetchRoster = wireApiCall('GET', '/api/v1/wire/roster')
            .then((data: any) => {
                // Response wrapped in wireEnvelope — roster data at data.data.existing_agents
                const rawAgents = data?.data?.existing_agents || data?.existing_agents || data?.agents || (Array.isArray(data) ? data : []);
                // Map server shape (name, pseudo_id) to component shape (id, pseudonym)
                const agents = (rawAgents as any[]).map((a: any) => ({
                    id: a.pseudo_id || a.id || a.name,
                    pseudonym: a.pseudo_id || a.pseudonym || a.name,
                    name: a.name,
                    status: a.status,
                    contribution_count: a.contribution_count,
                    reputation: a.reputation,
                    reputation_roi: a.reputation_roi,
                    created_at: a.created_at,
                }));
                setRoster(agents);
            })
            .catch((err: any) => {
                setError(err?.message || 'Failed to load roster');
            });

        const fetchPulse = wireApiCall('GET', '/api/v1/wire/pulse')
            .then((data: any) => {
                const pulse = data as PulseData;
                if (pulse?.fleet_online) {
                    setOnlineIds(new Set(pulse.fleet_online));
                }
            })
            .catch(() => {
                // Pulse failure is non-critical — agents still show, just without online status
            });

        Promise.all([fetchRoster, fetchPulse]).finally(() => setLoading(false));
    }, [wireApiCall]);

    useEffect(() => {
        fetchData();
    }, [fetchData]);

    if (loading) {
        return (
            <div className="fleet-overview">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading fleet roster...</span>
                </div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="fleet-overview">
                <div className="fleet-overview-header">
                    <h3>Agent Fleet</h3>
                </div>
                <div className="corpora-error">
                    <span>{error}</span>
                    <button
                        className="stewardship-btn stewardship-btn-ghost"
                        onClick={fetchData}
                    >
                        Retry
                    </button>
                </div>
            </div>
        );
    }

    return (
        <div className="fleet-overview">
            <div className="fleet-overview-header">
                <div className="fleet-overview-header-left">
                    <h3>Agent Fleet</h3>
                    <span className="fleet-overview-count">
                        {roster.length} agent{roster.length !== 1 ? 's' : ''}
                        {onlineIds.size > 0 && ` \u00B7 ${onlineIds.size} online`}
                    </span>
                </div>
                <button
                    className="stewardship-btn stewardship-btn-primary fleet-create-btn"
                    onClick={() => { setShowCreateForm(true); setCreateError(null); }}
                >
                    + Create Agent
                </button>
            </div>

            {/* ── Create Agent Form ───────────────── */}
            {showCreateForm && (
                <div className="fleet-create-form">
                    <div className="fleet-create-form-inner">
                        <label className="fleet-create-label">Agent Name</label>
                        <div className="fleet-create-input-row">
                            <input
                                type="text"
                                className="fleet-create-input"
                                value={createName}
                                onChange={(e) => setCreateName(e.target.value)}
                                placeholder="e.g. research-alpha"
                                autoFocus
                                disabled={createLoading}
                                onKeyDown={(e) => { if (e.key === 'Enter') handleCreateAgent(); }}
                            />
                            <button
                                className="stewardship-btn stewardship-btn-primary"
                                onClick={handleCreateAgent}
                                disabled={!createName.trim() || createLoading}
                            >
                                {createLoading ? 'Creating...' : 'Create'}
                            </button>
                            <button
                                className="stewardship-btn stewardship-btn-ghost"
                                onClick={() => { setShowCreateForm(false); setCreateError(null); setCreateName(''); }}
                                disabled={createLoading}
                            >
                                Cancel
                            </button>
                        </div>
                        {createError && (
                            <div className="fleet-create-error">{createError}</div>
                        )}
                    </div>
                </div>
            )}

            {/* ── Token Modal ─────────────────────── */}
            {createdToken && (
                <div className="fleet-token-modal-overlay" onClick={() => setCreatedToken(null)}>
                    <div className="fleet-token-modal" onClick={(e) => e.stopPropagation()}>
                        <h4 className="fleet-token-modal-title">Agent Created</h4>
                        <p className="fleet-token-modal-warning">
                            Save this API token now. It will not be shown again.
                        </p>
                        <div className="fleet-token-display">
                            <code className="fleet-token-value">{createdToken}</code>
                            <button
                                className="stewardship-btn stewardship-btn-ghost fleet-token-copy"
                                onClick={handleCopyToken}
                            >
                                {tokenCopied ? 'Copied' : 'Copy'}
                            </button>
                        </div>
                        <button
                            className="stewardship-btn stewardship-btn-primary fleet-token-dismiss"
                            onClick={() => setCreatedToken(null)}
                        >
                            I saved the token
                        </button>
                    </div>
                </div>
            )}

            {/* ── Filter Bar ──────────────────────── */}
            {roster.length > 0 && (
                <div className="fleet-filter-bar">
                    <input
                        type="text"
                        className="fleet-filter-search"
                        placeholder="Search by name..."
                        value={filterText}
                        onChange={(e) => setFilterText(e.target.value)}
                    />
                    <select
                        className="fleet-filter-select"
                        value={filterStatus}
                        onChange={(e) => setFilterStatus(e.target.value as typeof filterStatus)}
                    >
                        <option value="all">All Status</option>
                        <option value="active">Active</option>
                        <option value="paused">Paused</option>
                        <option value="archived">Archived</option>
                    </select>
                    <label className="fleet-filter-online-toggle">
                        <input
                            type="checkbox"
                            checked={filterOnlineOnly}
                            onChange={(e) => setFilterOnlineOnly(e.target.checked)}
                        />
                        <span>Online Only</span>
                    </label>
                </div>
            )}

            {roster.length === 0 ? (
                <div className="fleet-empty">
                    <p>No agents in your fleet yet. Agents appear here once registered with the Wire.</p>
                </div>
            ) : filteredRoster.length === 0 ? (
                <div className="fleet-empty">
                    <p>No agents match your filters.</p>
                </div>
            ) : (
                <div className="fleet-agent-grid">
                    {filteredRoster.map((agent) => {
                        const isOnline = onlineIds.has(agent.id);
                        return (
                            <div
                                key={agent.id}
                                className="fleet-agent-card fleet-agent-card-clickable"
                                onClick={() => handleAgentClick(agent.id)}
                                role="button"
                                tabIndex={0}
                                onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') handleAgentClick(agent.id); }}
                            >
                                <div className="fleet-agent-card-header">
                                    <span className="fleet-agent-pseudonym">
                                        {agent.name || agent.pseudonym}
                                    </span>
                                    <span className={`fleet-agent-status ${isOnline ? 'fleet-agent-online' : 'fleet-agent-offline'}`}>
                                        {isOnline ? 'Online' : 'Offline'}
                                    </span>
                                </div>
                                {agent.name && agent.pseudonym && agent.name !== agent.pseudonym && (
                                    <div className="fleet-agent-pseudo-id" style={{ fontSize: '11px', color: 'var(--text-secondary)', marginBottom: '8px' }}>
                                        {agent.pseudonym}
                                    </div>
                                )}
                                <div className="fleet-agent-card-stats">
                                    <div className="fleet-agent-stat">
                                        <span className="fleet-agent-stat-label">Contributions</span>
                                        <span className="fleet-agent-stat-value">
                                            {agent.contribution_count != null ? agent.contribution_count.toLocaleString() : '--'}
                                        </span>
                                    </div>
                                    <div className="fleet-agent-stat">
                                        <span className="fleet-agent-stat-label">Reputation</span>
                                        <span className="fleet-agent-stat-value">
                                            {agent.reputation_roi || (agent.reputation != null ? agent.reputation.toFixed(1) : '--')}
                                        </span>
                                    </div>
                                </div>
                            </div>
                        );
                    })}
                </div>
            )}

            <AgentDetailDrawer
                pseudoId={drawerPseudoId}
                open={drawerOpen}
                onClose={handleDrawerClose}
                operatorApiCall={operatorApiCall}
                onUuidResolved={handleUuidResolved}
                cachedUuid={drawerPseudoId ? uuidCacheRef.current.get(drawerPseudoId) ?? null : null}
            />
        </div>
    );
}
