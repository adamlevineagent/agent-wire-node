import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

// ── Fleet roster types (matching Rust FleetRoster / FleetPeer) ──────

interface FleetPeer {
    node_id: string;
    name: string;
    tunnel_url: string;
    models_loaded: string[];
    serving_rules: string[];
    queue_depths: Record<string, number>;
    total_queue_depth: number;
    last_seen: string; // ISO 8601 from chrono
}

interface FleetRoster {
    peers: Record<string, FleetPeer>;
    fleet_jwt: string | null;
    self_operator_id: string | null;
}

// ── Helpers ─────────────────────────────────────────────────────────

/** True when last_seen is within 120s of now. */
function isPeerFresh(peer: FleetPeer): boolean {
    try {
        const lastSeen = new Date(peer.last_seen).getTime();
        return Date.now() - lastSeen < 120_000;
    } catch {
        return false;
    }
}

function formatModelName(modelId: string): string {
    const parts = modelId.split('/');
    return parts[parts.length - 1] ?? modelId;
}

function truncateUrl(url: string, maxLen = 40): string {
    if (url.length <= maxLen) return url;
    return url.slice(0, maxLen - 3) + '...';
}

// ── Component ───────────────────────────────────────────────────────

export function MarketDashboard() {
    const [roster, setRoster] = useState<FleetRoster | null>(null);

    // Poll fleet roster every 5s
    useEffect(() => {
        let active = true;
        const poll = async () => {
            try {
                const data = await invoke<FleetRoster>('get_fleet_roster');
                if (active) setRoster(data);
            } catch {
                // Roster not available yet — leave null
            }
        };
        poll();
        const interval = setInterval(poll, 5000);
        return () => {
            active = false;
            clearInterval(interval);
        };
    }, []);

    const peers = roster?.peers ? Object.values(roster.peers) : [];
    const peerCount = peers.length;
    const hasFleet = peerCount > 0;

    return (
        <div className="market-dashboard">
            <div className="market-dashboard-header">
                <h2>Compute Market</h2>
                <div className="market-enable-toggle">
                    {hasFleet ? (
                        <span className="fleet-badge-active">
                            Fleet Active ({peerCount} peer{peerCount !== 1 ? 's' : ''})
                        </span>
                    ) : (
                        <span className="market-status-badge">Local Only</span>
                    )}
                </div>
            </div>

            <div className="market-dashboard-content">
                {/* Fleet peers section */}
                {hasFleet && (
                    <div className="fleet-peers-section">
                        <h3 className="fleet-peers-title">Fleet Peers</h3>
                        <div className="fleet-peers-grid">
                            {peers.map(peer => (
                                <FleetPeerCard key={peer.node_id} peer={peer} />
                            ))}
                        </div>
                    </div>
                )}

                {/* Phase 1 info (always visible) */}
                {!hasFleet && (
                    <div className="market-info-card">
                        <h3>Phase 1: Local Queue</h3>
                        <p>
                            Your node's compute queue is managing local GPU resources.
                            Market features (selling compute, buying from the network)
                            will be enabled in a future update.
                        </p>
                    </div>
                )}

                <div className="market-info-card">
                    <h3>What's Coming</h3>
                    <ul className="market-roadmap-list">
                        <li>Order book for compute capacity</li>
                        <li>Per-model pricing and queue priority</li>
                        <li>Fleet routing across nodes</li>
                        <li>Earnings from serving compute</li>
                    </ul>
                </div>
            </div>
        </div>
    );
}

// ── Fleet peer card ────────────────────────────────────────────────

function FleetPeerCard({ peer }: { peer: FleetPeer }) {
    const fresh = isPeerFresh(peer);
    const queueEntries = Object.entries(peer.queue_depths);

    return (
        <div className={`fleet-peer-card ${fresh ? 'fleet-peer-card-fresh' : ''}`}>
            <div className="fleet-peer-card-header">
                <div className="fleet-peer-name" title={peer.node_id}>
                    {peer.name || peer.node_id}
                </div>
                <span
                    className={`fleet-peer-status-dot ${fresh ? 'fleet-peer-status-dot-online' : 'fleet-peer-status-dot-stale'}`}
                    title={fresh ? 'Online' : 'Stale'}
                />
            </div>

            {/* Serving rules */}
            {peer.serving_rules && peer.serving_rules.length > 0 && (
                <div className="fleet-peer-models">
                    <div className="fleet-peer-models-label">Serving Rules</div>
                    <div className="fleet-peer-models-list">
                        {peer.serving_rules.map(rule => (
                            <span key={rule} className="fleet-peer-model-tag" title={rule}>
                                {rule}
                            </span>
                        ))}
                    </div>
                </div>
            )}

            {/* Models loaded */}
            {peer.models_loaded.length > 0 && (
                <div className="fleet-peer-models">
                    <div className="fleet-peer-models-label">Models</div>
                    <div className="fleet-peer-models-list">
                        {peer.models_loaded.map(model => (
                            <span key={model} className="fleet-peer-model-tag" title={model}>
                                {formatModelName(model)}
                            </span>
                        ))}
                    </div>
                </div>
            )}

            {/* Queue load */}
            <div className="fleet-peer-queues">
                <div className="fleet-peer-queues-label">
                    Queue Load: {peer.total_queue_depth ?? 0}
                </div>
                {queueEntries.length > 0 && queueEntries.map(([model, depth]) => (
                    <div key={model} className="fleet-peer-queue-row">
                        <span className="fleet-peer-queue-model">{formatModelName(model)}</span>
                        <span className="fleet-peer-queue-depth">{depth}</span>
                    </div>
                ))}
            </div>

            {/* Tunnel URL (debug) */}
            <div className="fleet-peer-tunnel" title={peer.tunnel_url}>
                {truncateUrl(peer.tunnel_url)}
            </div>
        </div>
    );
}
