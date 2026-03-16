import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface MarketState {
    hosted_documents: Record<string, HostedDocument>;
    total_hosted_bytes: number;
    last_evaluation_at: string | null;
    is_evaluating: boolean;
}

interface HostedDocument {
    document_id: string;
    corpus_id: string;
    body_hash: string;
    size_bytes: number;
    pulls_served: number;
    credits_earned: number;
    hosted_since: string;
}

function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatCredits(n: number): string {
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return n.toFixed(2);
}

function formatRelativeTime(timestamp: string): string {
    const now = Date.now();
    const then = new Date(timestamp).getTime();
    const diff = now - then;
    if (diff < 60_000) return "just now";
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
}

export function MarketView() {
    const [market, setMarket] = useState<MarketState | null>(null);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        const fetch = async () => {
            try {
                const ms = await invoke<MarketState>("get_market_surface");
                setMarket(ms);
            } catch (err) {
                console.error("Failed to fetch market surface:", err);
            } finally {
                setLoading(false);
            }
        };

        fetch();
        const interval = setInterval(fetch, 10_000);
        return () => clearInterval(interval);
    }, []);

    if (loading) {
        return (
            <div className="market-loading">
                <div className="loading-pulse">Loading market data...</div>
            </div>
        );
    }

    const docs = market ? Object.values(market.hosted_documents) : [];
    const totalHostedBytes = market?.total_hosted_bytes || 0;
    const totalCredits = docs.reduce((sum, d) => sum + d.credits_earned, 0);
    const totalPulls = docs.reduce((sum, d) => sum + d.pulls_served, 0);

    // Sort by credits earned (highest first)
    const sortedDocs = [...docs].sort((a, b) => b.credits_earned - a.credits_earned);

    return (
        <div className="market-view">
            {/* Market Summary */}
            <div className="market-summary">
                <div className="market-stat">
                    <div className="market-stat-value">{docs.length}</div>
                    <div className="market-stat-label">documents hosted</div>
                </div>
                <div className="market-stat">
                    <div className="market-stat-value">{formatBytes(totalHostedBytes)}</div>
                    <div className="market-stat-label">storage used</div>
                </div>
                <div className="market-stat">
                    <div className="market-stat-value">{totalPulls.toLocaleString()}</div>
                    <div className="market-stat-label">total pulls</div>
                </div>
                <div className="market-stat">
                    <div className="market-stat-value">{formatCredits(totalCredits)}</div>
                    <div className="market-stat-label">credits earned</div>
                </div>
            </div>

            {market?.last_evaluation_at && (
                <div className="market-evaluation-info">
                    {market.is_evaluating
                        ? "Evaluating market opportunities..."
                        : `Last evaluated: ${formatRelativeTime(market.last_evaluation_at)}`}
                </div>
            )}

            {/* Document Table */}
            {sortedDocs.length === 0 ? (
                <div className="market-empty">
                    <div className="market-empty-icon">W</div>
                    <p className="market-empty-title">No documents hosted yet</p>
                    <p className="market-empty-desc">
                        Enable mesh hosting in Settings to automatically discover and host
                        high-demand documents from the Wire network. Documents with more pulls
                        earn more credits.
                    </p>
                </div>
            ) : (
                <div className="market-table">
                    <div className="market-table-header">
                        <span className="market-col-doc">Document</span>
                        <span className="market-col-num">Pulls</span>
                        <span className="market-col-num">Credits</span>
                        <span className="market-col-size">Size</span>
                        <span className="market-col-since">Hosted</span>
                    </div>
                    <div className="market-table-body">
                        {sortedDocs.map((doc) => {
                            // Competitive position: higher pulls = stronger position
                            const position = doc.pulls_served > 100
                                ? "strong"
                                : doc.pulls_served > 10
                                    ? "moderate"
                                    : "building";

                            return (
                                <div key={doc.document_id} className="market-row">
                                    <span className="market-col-doc">
                                        <span className={`position-indicator ${position}`} title={`Position: ${position}`} />
                                        <span className="market-doc-id" title={doc.document_id}>
                                            {doc.document_id.length > 16
                                                ? doc.document_id.slice(0, 8) + "..." + doc.document_id.slice(-4)
                                                : doc.document_id}
                                        </span>
                                        <span className="market-corpus-id">{doc.corpus_id}</span>
                                    </span>
                                    <span className="market-col-num">{doc.pulls_served.toLocaleString()}</span>
                                    <span className="market-col-num">{formatCredits(doc.credits_earned)}</span>
                                    <span className="market-col-size">{formatBytes(doc.size_bytes)}</span>
                                    <span className="market-col-since">{formatRelativeTime(doc.hosted_since)}</span>
                                </div>
                            );
                        })}
                    </div>
                </div>
            )}
        </div>
    );
}
