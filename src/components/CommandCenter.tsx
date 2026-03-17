import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { ImpactStats } from "./ImpactStats";
import { ActivityFeed } from "./ActivityFeed";
import { SyncStatus } from "./SyncStatus";
import { TunnelStatus } from "./TunnelStatus";
import { MarketView } from "./MarketView";
import { Messages } from "./Messages";
import { Settings } from "./Settings";
import { LogViewer } from "./LogViewer";
import type { CreditStats, SyncState } from "./Dashboard";

interface CommandCenterProps {
    authState: {
        email: string | null;
        node_id: string | null;
    };
    onLogout: () => void;
}

interface TunnelStatusData {
    tunnel_id: string | null;
    tunnel_url: string | null;
    status: string | { Error: string };
}

type TabKey = "dashboard" | "sync" | "market" | "messages" | "settings" | "logs";

function getTunnelLabel(status: TunnelStatusData["status"]): { text: string; className: string } {
    if (typeof status === "object" && "Error" in status) {
        return { text: "Error", className: "status-badge error" };
    }
    switch (status) {
        case "Connected": return { text: "Connected", className: "status-badge online" };
        case "Connecting": return { text: "Connecting", className: "status-badge connecting" };
        case "Provisioning": return { text: "Provisioning", className: "status-badge connecting" };
        case "Downloading": return { text: "Downloading", className: "status-badge connecting" };
        default: return { text: "Offline", className: "status-badge offline" };
    }
}

function getServerStatus(
    credits: CreditStats | null,
    syncState: SyncState | null,
    tunnelStatus: TunnelStatusData | null,
): { text: string; className: string } {
    if (tunnelStatus?.status === "Connected") {
        return { text: "Online", className: "status-badge online" };
    }
    if (syncState && Object.keys(syncState.linked_folders).length > 0) {
        return { text: "Online", className: "status-badge online" };
    }
    if (credits) {
        return { text: "Ready", className: "status-badge connecting" };
    }
    return { text: "Offline", className: "status-badge offline" };
}

export function CommandCenter({ authState, onLogout }: CommandCenterProps) {
    const [credits, setCredits] = useState<CreditStats | null>(null);
    const [syncState, setSyncState] = useState<SyncState | null>(null);
    const [tunnelStatus, setTunnelStatus] = useState<TunnelStatusData | null>(null);
    const [syncing, setSyncing] = useState(false);
    const [retryingTunnel, setRetryingTunnel] = useState(false);
    const [activeTab, setActiveTab] = useState<TabKey>("dashboard");
    const [appVersion, setAppVersion] = useState<string>("");
    const [updateAvailable, setUpdateAvailable] = useState<{ version: string; body?: string } | null>(null);
    const [installing, setInstalling] = useState(false);

    // Fetch app version on mount
    useEffect(() => {
        getVersion().then((v) => setAppVersion(v)).catch(() => {});
    }, []);

    // Check for updates on mount and every 30 minutes
    useEffect(() => {
        const checkUpdate = async () => {
            try {
                const info = await invoke<{ available: boolean; version?: string; body?: string }>("check_for_update");
                if (info.available && info.version) {
                    setUpdateAvailable({ version: info.version, body: info.body });
                }
            } catch (e) {
                console.debug("Update check:", e);
            }
        };
        checkUpdate();
        const interval = setInterval(checkUpdate, 30 * 60 * 1000);
        return () => clearInterval(interval);
    }, []);

    const handleInstallUpdate = useCallback(async () => {
        setInstalling(true);
        try {
            await invoke("install_update");
        } catch (e) {
            console.error("Update install failed:", e);
            setInstalling(false);
        }
    }, []);

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

    // Poll for tunnel status every 3 seconds
    useEffect(() => {
        const fetchTunnel = async () => {
            try {
                const ts = await invoke<TunnelStatusData>("get_tunnel_status");
                setTunnelStatus(ts);
            } catch (err) {
                console.error("Failed to fetch tunnel status:", err);
            }
        };

        fetchTunnel();
        const interval = setInterval(fetchTunnel, 3000);
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

    const handleRetryTunnel = useCallback(async () => {
        setRetryingTunnel(true);
        try {
            await invoke("retry_tunnel");
        } catch (err) {
            console.error("Tunnel retry failed:", err);
        } finally {
            setRetryingTunnel(false);
        }
    }, []);

    const folderCount = syncState ? Object.keys(syncState.linked_folders).length : 0;
    const docCount = syncState?.cached_documents?.length || 0;

    return (
        <div className="command-center">
            {/* Header */}
            <header className="cc-header">
                <div className="header-brand">
                    <div className="wire-logo-header">W</div>
                    <div>
                        <h1>Wire Node <span className="app-version">v{appVersion}</span></h1>
                        <span className={getServerStatus(credits, syncState, tunnelStatus).className}>
                            {getServerStatus(credits, syncState, tunnelStatus).text}
                        </span>
                        {tunnelStatus && typeof tunnelStatus.status === "string" &&
                            ["Connected", "Connecting", "Provisioning", "Downloading"].includes(tunnelStatus.status) && (
                                <span className={getTunnelLabel(tunnelStatus.status).className} style={{ fontSize: "0.7em", opacity: 0.8 }}>
                                    Tunnel {getTunnelLabel(tunnelStatus.status).text}
                                </span>
                            )}
                        {tunnelStatus?.tunnel_url && (
                            <span className="tunnel-url">{tunnelStatus.tunnel_url.replace("https://", "")}</span>
                        )}
                    </div>
                </div>
                <div className="header-actions">
                    <button
                        className="sync-btn"
                        onClick={handleSync}
                        disabled={syncing}
                    >
                        {syncing ? "Syncing..." : "Sync"}
                    </button>
                    {tunnelStatus?.status !== "Connected" && (
                        <button
                            className="sync-btn"
                            onClick={handleRetryTunnel}
                            disabled={retryingTunnel}
                        >
                            {retryingTunnel ? "Connecting..." : "Retry Tunnel"}
                        </button>
                    )}
                    {authState.email && (
                        <span className="user-email">{authState.email}</span>
                    )}
                    <button className="logout-btn" onClick={onLogout} title="Logout">[x]</button>
                </div>
            </header>

            {/* Update Banner */}
            {updateAvailable && (
                <div className="update-banner">
                    <span>v{updateAvailable.version} is available!</span>
                    {updateAvailable.body && <span className="update-notes">{updateAvailable.body}</span>}
                    <button
                        className="update-install-btn"
                        onClick={handleInstallUpdate}
                        disabled={installing}
                    >
                        {installing ? "Installing..." : "Install & Restart"}
                    </button>
                </div>
            )}

            {/* Tab Navigation */}
            <nav className="cc-tabs">
                {([
                    { key: "dashboard" as const, label: "Dashboard" },
                    { key: "sync" as const, label: `Sync (${folderCount})` },
                    { key: "market" as const, label: "Market" },
                    { key: "messages" as const, label: "Messages" },
                    { key: "settings" as const, label: "Settings" },
                    { key: "logs" as const, label: "Logs" },
                ]).map((tab) => (
                    <button
                        key={tab.key}
                        className={`cc-tab ${activeTab === tab.key ? "cc-tab-active" : ""}`}
                        onClick={() => setActiveTab(tab.key)}
                    >
                        {tab.label}
                    </button>
                ))}
            </nav>

            {/* Tab Content */}
            {activeTab === "dashboard" && (
                <>
                    {/* Main Grid */}
                    <div className="cc-grid">
                        {/* Left Panel -- Tunnel Status */}
                        <aside className="cc-sidebar">
                            <TunnelStatus credits={credits} tunnelStatus={tunnelStatus} />
                        </aside>

                        {/* Center Panel -- Impact Stats */}
                        <main className="cc-main">
                            <ImpactStats credits={credits} />
                        </main>

                        {/* Right Panel -- Activity Feed */}
                        <aside className="cc-activity">
                            <div className="panel-header">
                                <h3>Wire Activity Feed</h3>
                                <span className="live-dot" />
                            </div>
                            <ActivityFeed credits={credits} />
                        </aside>
                    </div>

                    {/* Bottom Bar -- Network Summary */}
                    <footer className="cc-footer">
                        <div className="network-pulse">
                            <div className="pulse-waveform">
                                <div className="pulse-line" />
                                <div className="pulse-line delay" />
                            </div>
                            <div className="network-stats">
                                <div className="net-stat">
                                    <span className="net-value">{folderCount}</span>
                                    <span className="net-label">{folderCount === 1 ? "folder linked" : "folders linked"}</span>
                                </div>
                                <div className="net-divider" />
                                <div className="net-stat">
                                    <span className="net-value">{docCount}</span>
                                    <span className="net-label">documents cached</span>
                                </div>
                                <div className="net-divider" />
                                <div className="net-stat">
                                    <span className="net-value">{credits?.total_bytes_formatted || "0 B"}</span>
                                    <span className="net-label">total served</span>
                                </div>
                                <div className="net-divider" />
                                <div className="net-stat">
                                    <span className="net-value glow">{Math.floor((credits?.server_credit_balance || 0) > 0 ? credits!.server_credit_balance : (credits?.credits_earned || 0))}</span>
                                    <span className="net-label">credits earned</span>
                                </div>
                            </div>
                        </div>
                    </footer>
                </>
            )}

            {activeTab === "sync" && (
                <div className="cc-tab-content">
                    <SyncStatus
                        syncState={syncState}
                        syncing={syncing}
                        onSync={handleSync}
                    />
                </div>
            )}

            {activeTab === "market" && (
                <div className="cc-tab-content">
                    <MarketView />
                </div>
            )}

            {activeTab === "messages" && (
                <div className="cc-tab-content">
                    <Messages />
                </div>
            )}

            {activeTab === "settings" && (
                <div className="cc-tab-content">
                    <Settings />
                </div>
            )}

            {activeTab === "logs" && (
                <div className="cc-tab-content">
                    <LogViewer />
                </div>
            )}
        </div>
    );
}
