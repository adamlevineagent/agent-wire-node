import { useState, useEffect, useRef } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

interface DomainScore {
    domain: string;
    score: number;
}

interface ReputationData {
    global_score: number;
    domains?: DomainScore[];
}

interface MeResponse {
    pseudonym: string;
    [key: string]: unknown;
}

// --- Component ---

/**
 * ReputationDisplay fetches and renders Wire reputation data.
 * Shows global_score and per-domain breakdown.
 * Falls back to em dash when no data is available.
 */
export function ReputationDisplay() {
    const { wireApiCall } = useAppContext();
    const [reputation, setReputation] = useState<ReputationData | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState(false);
    const mountedRef = useRef(true);

    useEffect(() => {
        mountedRef.current = true;
        return () => { mountedRef.current = false; };
    }, []);

    useEffect(() => {
        let cancelled = false;

        async function fetchReputation() {
            try {
                // Get the pseudo_id from /api/v1/me (node_id != pseudo_id)
                const me = await wireApiCall('GET', '/api/v1/me') as MeResponse | null;
                const pseudoId = me?.pseudonym;

                if (!pseudoId) {
                    if (!cancelled && mountedRef.current) {
                        setLoading(false);
                        setError(true);
                    }
                    return;
                }

                const data = await wireApiCall(
                    'GET',
                    `/api/v1/wire/reputation/${encodeURIComponent(pseudoId)}`
                ) as ReputationData | null;

                if (!cancelled && mountedRef.current) {
                    if (data && typeof data.global_score === 'number') {
                        setReputation(data);
                        setError(false);
                    } else {
                        setError(true);
                    }
                    setLoading(false);
                }
            } catch {
                if (!cancelled && mountedRef.current) {
                    setError(true);
                    setLoading(false);
                }
            }
        }

        fetchReputation();

        return () => { cancelled = true; };
    }, [wireApiCall]);

    // Loading state -- show dash while loading
    if (loading) {
        return <ReputationFallback />;
    }

    // Error or no data -- show dash
    if (error || !reputation) {
        return <ReputationFallback />;
    }

    // Has data -- render score + domains
    return (
        <div className="reputation-display">
            <div className="identity-info-card">
                <span className="identity-label">Reputation</span>
                <span className="identity-value glow">
                    {reputation.global_score.toFixed(1)}
                </span>
            </div>

            {reputation.domains && reputation.domains.length > 0 && (
                <div className="reputation-domains">
                    <span className="reputation-domains-label">Domain Scores</span>
                    <div className="reputation-domain-list">
                        {reputation.domains.map((d) => (
                            <div key={d.domain} className="reputation-domain-item">
                                <span className="reputation-domain-name">{d.domain}</span>
                                <span className="reputation-domain-score">
                                    {d.score.toFixed(1)}
                                </span>
                            </div>
                        ))}
                    </div>
                </div>
            )}
        </div>
    );
}

/** Fallback: renders a single em dash in the existing card style */
function ReputationFallback() {
    return (
        <div className="identity-info-card">
            <span className="identity-label">Reputation</span>
            <span className="identity-value">{'\u2014'}</span>
        </div>
    );
}
