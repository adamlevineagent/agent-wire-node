import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ActivityFeed } from "./ActivityFeed";
import { ImpactStats } from "./ImpactStats";
import { SyncStatus } from "./SyncStatus";

interface DashboardProps {
    authState: {
        email: string | null;
        node_id: string | null;
    };
}

export interface CreditStats {
    documents_served: number;
    pulls_served_total: number;
    credits_earned: number;
    total_bytes_served: number;
    total_bytes_formatted: string;
    today_documents_served: number;
    today_bytes_served: number;
    session_documents_served: number;
    session_bytes_served: number;
    session_uptime: string;
    total_uptime_seconds: number;
    first_started_at: string | null;
    achievements: Achievement[];
}

export interface Achievement {
    id: string;
    emoji: string;
    current_level: number;
    current_name: string;
    next_name: string | null;
    next_threshold: number | null;
    current_value: number;
    progress_pct: number;
}

export interface SyncState {
    linked_folders: Record<string, string>; // folder_path -> corpus_slug
    cached_documents: CachedDocument[];
    total_size_bytes: number;
    last_sync_at: string | null;
    is_syncing: boolean;
}

export interface CachedDocument {
    document_id: string;
    corpus_slug: string;
    source_path: string;
    body_hash: string;
    file_size_bytes: number;
    cached_at: string;
}

export interface ServeEvent {
    document_id: string;
    bytes: number;
    timestamp: string;
    message: string;
    token_id: string;
    event_type: string;
}

export function Dashboard({ authState }: DashboardProps) {
    const [credits, setCredits] = useState<CreditStats | null>(null);
    const [syncState, setSyncState] = useState<SyncState | null>(null);
    const [syncing, setSyncing] = useState(false);
    const [activeTab, setActiveTab] = useState<"impact" | "feed" | "sync">("impact");

    // Poll for credit stats + sync status every 2 seconds
    useEffect(() => {
        const fetchStats = async () => {
            try {
                const [creditStats, sync] = await Promise.all([
                    invoke<CreditStats>("get_credits"),
                    invoke<SyncState>("get_sync_status"),
                ]);
                setCredits(creditStats);
                setSyncState(sync);
            } catch (err) {
                console.error("Failed to fetch stats:", err);
            }
        };

        fetchStats();
        const interval = setInterval(fetchStats, 2000);
        return () => clearInterval(interval);
    }, []);

    const handleSync = useCallback(async () => {
        setSyncing(true);
        try {
            await invoke("sync_content");
            const sync = await invoke<SyncState>("get_sync_status");
            setSyncState(sync);
        } catch (err) {
            console.error("Sync failed:", err);
        } finally {
            setSyncing(false);
        }
    }, []);

    return (
        <div className="dashboard">
            {/* Header */}
            <header className="dashboard-header">
                <div className="header-brand">
                    <span className="wire-icon">W</span>
                    <div>
                        <h1>Wire Node</h1>
                        <span className="status-badge online">Online</span>
                    </div>
                </div>
                <div className="header-user">
                    {authState.email && (
                        <span className="user-email">{authState.email}</span>
                    )}
                </div>
            </header>

            {/* Tab Bar */}
            <div className="tab-bar">
                <button
                    className={`tab ${activeTab === "impact" ? "active" : ""}`}
                    onClick={() => setActiveTab("impact")}
                >
                    Impact
                </button>
                <button
                    className={`tab ${activeTab === "feed" ? "active" : ""}`}
                    onClick={() => setActiveTab("feed")}
                >
                    Activity
                </button>
                <button
                    className={`tab ${activeTab === "sync" ? "active" : ""}`}
                    onClick={() => setActiveTab("sync")}
                >
                    Sync
                </button>
            </div>

            {/* Tab Content */}
            <div className="tab-content">
                {activeTab === "impact" ? (
                    <ImpactStats credits={credits} />
                ) : activeTab === "feed" ? (
                    <ActivityFeed credits={credits} />
                ) : (
                    <SyncStatus
                        syncState={syncState}
                        syncing={syncing}
                        onSync={handleSync}
                    />
                )}
            </div>
        </div>
    );
}
