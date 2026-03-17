import { useEffect, useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { useAppContext, TunnelStatusData } from '../contexts/AppContext';
import { Sidebar } from './Sidebar';
import { ModeRouter } from './ModeRouter';
import type { CreditStats, SyncState } from './Dashboard';

interface AppShellProps {
    onLogout: () => void;
}

export function AppShell({ onLogout }: AppShellProps) {
    const { state, dispatch } = useAppContext();
    const [appVersion, setAppVersion] = useState('');
    const [updateAvailable, setUpdateAvailable] = useState<{ version: string; body?: string } | null>(null);
    const [installing, setInstalling] = useState(false);
    const [syncing, setSyncing] = useState(false);
    const [retryingTunnel, setRetryingTunnel] = useState(false);

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

    // Poll for notification count every 30 seconds
    useEffect(() => {
        const fetchNotifications = async () => {
            try {
                const messages = await invoke<Array<{ read_at: string | null }>>('get_messages');
                const unread = messages.filter(m => !m.read_at).length;
                dispatch({ type: 'SET_NOTIFICATION_COUNT', count: unread });
            } catch {
                // silent fail
            }
        };
        fetchNotifications();
        const interval = setInterval(fetchNotifications, 30000);
        return () => clearInterval(interval);
    }, [dispatch]);

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

                {/* Mode content */}
                <div className="mode-content-area">
                    <ModeRouter />
                </div>
            </div>
        </div>
    );
}
