import { useEffect, useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { useAppContext, TunnelStatusData } from '../contexts/AppContext';
import { Sidebar } from './Sidebar';
import { IntentBar } from './IntentBar';
import { ModeRouter } from './ModeRouter';
import type { CreditStats, SyncState } from './Dashboard';
import type { SlugInfo } from './pyramid-types';

interface AppShellProps {
    onLogout: () => void;
}

export function AppShell({ onLogout }: AppShellProps) {
    const { state, dispatch, operatorApiCall, wireApiCall } = useAppContext();
    const [appVersion, setAppVersion] = useState('');
    const [updateAvailable, setUpdateAvailable] = useState<{ version: string; body?: string } | null>(null);
    const [installing, setInstalling] = useState(false);
    const [syncing, setSyncing] = useState(false);
    const [retryingTunnel, setRetryingTunnel] = useState(false);
    const [urlCopied, setUrlCopied] = useState(false);

    const tunnelUrl = state.tunnelStatus?.tunnel_url ?? null;
    const tunnelConnected = state.tunnelStatus?.status === 'Connected';

    const handleOpenTunnelUrl = useCallback(async () => {
        if (!tunnelUrl) return;
        const fullUrl = tunnelUrl.replace(/\/$/, '') + '/p/';
        try {
            await invoke('open_url_in_browser', { url: fullUrl });
        } catch (e) {
            console.error('Failed to open URL:', e);
        }
    }, [tunnelUrl]);

    const handleCopyTunnelUrl = useCallback(async () => {
        if (!tunnelUrl) return;
        const fullUrl = tunnelUrl.replace(/\/$/, '') + '/p/';
        try {
            await navigator.clipboard.writeText(fullUrl);
            setUrlCopied(true);
            setTimeout(() => setUrlCopied(false), 2000);
        } catch (e) {
            console.error('Failed to copy URL:', e);
        }
    }, [tunnelUrl]);

    // Fetch app version on mount
    useEffect(() => {
        getVersion().then((v) => setAppVersion(v)).catch(() => {});
    }, []);

    // Check for updates on mount and every 30 minutes
    useEffect(() => {
        const checkUpdate = async () => {
            try {
                const info = await invoke<{ available: boolean; version?: string; body?: string }>('check_for_update');
                if (info.available && info.version) {
                    setUpdateAvailable({ version: info.version, body: info.body });
                }
            } catch (e) {
                console.debug('Update check:', e);
            }
        };
        checkUpdate();
        const interval = setInterval(checkUpdate, 30 * 60 * 1000);
        return () => clearInterval(interval);
    }, []);

    // Poll for credit stats + sync status every 2 seconds
    useEffect(() => {
        const fetchStats = async () => {
            try {
                const [creditStats, syncState] = await Promise.all([
                    invoke<CreditStats>('get_credits'),
                    invoke<SyncState>('get_sync_status'),
                ]);
                dispatch({ type: 'SET_CREDITS', credits: creditStats });
                dispatch({ type: 'SET_SYNC_STATE', syncState });
            } catch (err) {
                console.error('Failed to fetch stats:', err);
            }
        };
        fetchStats();
        const interval = setInterval(fetchStats, 2000);
        return () => clearInterval(interval);
    }, [dispatch]);

    // Poll for tunnel status every 3 seconds
    useEffect(() => {
        const fetchTunnel = async () => {
            try {
                const ts = await invoke<TunnelStatusData>('get_tunnel_status');
                dispatch({ type: 'SET_TUNNEL_STATUS', tunnelStatus: ts });
            } catch (err) {
                console.error('Failed to fetch tunnel status:', err);
            }
        };
        fetchTunnel();
        const interval = setInterval(fetchTunnel, 3000);
        return () => clearInterval(interval);
    }, [dispatch]);

    // Poll pyramid count every 30 seconds
    useEffect(() => {
        const fetchPyramids = async () => {
            try {
                const slugs = await invoke<SlugInfo[]>('pyramid_list_slugs');
                dispatch({ type: 'SET_PYRAMID_COUNT', count: slugs.length });
            } catch {
                // silent fail
            }
        };
        fetchPyramids();
        const interval = setInterval(fetchPyramids, 30000);
        return () => clearInterval(interval);
    }, [dispatch]);

    // Poll draft count every 30 seconds
    useEffect(() => {
        const fetchDrafts = async () => {
            try {
                const drafts = await invoke<unknown[]>('get_compose_drafts');
                dispatch({ type: 'SET_DRAFT_COUNT', count: Array.isArray(drafts) ? drafts.length : 0 });
            } catch {
                // silent fail
            }
        };
        fetchDrafts();
        const interval = setInterval(fetchDrafts, 30000);
        return () => clearInterval(interval);
    }, [dispatch]);

    // Poll fleet pulse every 60 seconds (lifted from DashboardMode)
    useEffect(() => {
        const fetchPulse = async () => {
            try {
                const data: any = await wireApiCall('GET', '/api/v1/wire/pulse');
                dispatch({
                    type: 'SET_FLEET_PULSE',
                    fleetOnlineCount: data?.online_agents ?? 0,
                    taskCount: data?.active_tasks ?? 0,
                });
            } catch {
                // silent fail
            }
        };
        fetchPulse();
        const interval = setInterval(fetchPulse, 60000);
        return () => clearInterval(interval);
    }, [dispatch, wireApiCall]);

    // Poll for Wire message count every 30 seconds (DMs/circle messages — separate from notifications)
    useEffect(() => {
        const fetchMessages = async () => {
            try {
                const messages = await invoke<Array<{ read_at: string | null }>>('get_messages');
                const unread = messages.filter(m => !m.read_at).length;
                dispatch({ type: 'SET_MESSAGE_COUNT', count: unread });
            } catch {
                // silent fail
            }
        };
        fetchMessages();
        const interval = setInterval(fetchMessages, 30000);
        return () => clearInterval(interval);
    }, [dispatch]);

    // Poll for notification count every 60 seconds (uses operator auth — dual-auth endpoint)
    useEffect(() => {
        if (!state.operatorSessionToken) return;
        const fetchNotificationCount = async () => {
            try {
                const data: any = await operatorApiCall('GET', '/api/v1/wire/notifications?read=false&limit=1');
                const count = typeof data?.unread_count === 'number'
                    ? data.unread_count
                    : (Array.isArray(data?.notifications) ? data.notifications.length : 0);
                dispatch({ type: 'SET_NOTIFICATION_COUNT', count });
            } catch {
                // silent fail — notification badge may be stale
            }
        };
        fetchNotificationCount();
        const interval = setInterval(fetchNotificationCount, 60000);
        return () => clearInterval(interval);
    }, [dispatch, state.operatorSessionToken, operatorApiCall]);

    // Try to acquire operator session on mount
    useEffect(() => {
        invoke('get_operator_session')
            .then((result: unknown) => {
                const r = result as Record<string, unknown>;
                dispatch({
                    type: 'SET_OPERATOR_SESSION',
                    operatorId: (r.operator_id as string) ?? null,
                    operatorSessionToken: (r.session_token as string) ?? null,
                });
            })
            .catch(() => {
                // Operator session not available — app works in agent-only mode
            });
    }, [dispatch]);

    const handleSync = useCallback(async () => {
        setSyncing(true);
        try {
            await invoke('sync_content');
            const sync = await invoke<SyncState>('get_sync_status');
            dispatch({ type: 'SET_SYNC_STATE', syncState: sync });
        } catch (err) {
            console.error('Sync failed:', err);
        } finally {
            setSyncing(false);
        }
    }, [dispatch]);

    const handleRetryTunnel = useCallback(async () => {
        setRetryingTunnel(true);
        try {
            await invoke('retry_tunnel');
        } catch (err) {
            console.error('Tunnel retry failed:', err);
        } finally {
            setRetryingTunnel(false);
        }
    }, []);

    const handleInstallUpdate = useCallback(async () => {
        setInstalling(true);
        try {
            await invoke('install_update');
        } catch (e) {
            console.error('Update install failed:', e);
            setInstalling(false);
        }
    }, []);

    return (
        <div className="app-shell">
            <Sidebar />

            <div className="app-main">
                {/* Header bar */}
                <header className="app-shell-header">
                    <div className="header-brand">
                        <div className="wire-logo-header">W</div>
                        <div>
                            <h1>Wire Node <span className="app-version">v{appVersion}</span></h1>
                        </div>
                    </div>
                    <div className="header-actions">
                        <button
                            className="sync-btn"
                            onClick={handleSync}
                            disabled={syncing}
                        >
                            {syncing ? 'Syncing...' : 'Sync'}
                        </button>
                        {tunnelUrl && (
                            <>
                                <button
                                    className="sync-btn"
                                    onClick={handleOpenTunnelUrl}
                                    title="Open your public web surface"
                                    style={{ fontFamily: 'monospace', maxWidth: 320, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                                >
                                    {tunnelUrl.replace(/^https?:\/\//, '')}/p/
                                </button>
                                <button
                                    className="sync-btn"
                                    onClick={handleCopyTunnelUrl}
                                    title="Copy public URL to clipboard"
                                    style={{ minWidth: 32 }}
                                >
                                    {urlCopied ? 'Copied!' : '\u{1F4CB}'}
                                </button>
                            </>
                        )}
                        {tunnelConnected && !tunnelUrl && (
                            <span
                                className="sync-btn"
                                style={{ color: '#eab308', cursor: 'default' }}
                                title="Tunnel reports Connected but no URL is set"
                            >
                                tunnel up, no URL
                            </span>
                        )}
                        {state.tunnelStatus?.status !== 'Connected' && (
                            <button
                                className="sync-btn"
                                onClick={handleRetryTunnel}
                                disabled={retryingTunnel}
                            >
                                {retryingTunnel ? 'Connecting...' : 'Retry Tunnel'}
                            </button>
                        )}
                        {state.email && (
                            <span className="user-email">{state.email}</span>
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
                            {installing ? 'Installing...' : 'Install & Restart'}
                        </button>
                    </div>
                )}

                {/* Intent bar */}
                <IntentBar />

                {/* Mode content */}
                <div className="mode-content-area">
                    <ModeRouter />
                </div>
            </div>
        </div>
    );
}
