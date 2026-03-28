import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../../contexts/AppContext';
import { SyncStatus } from '../SyncStatus';
import { MarketView } from '../MarketView';
import { LogViewer } from '../LogViewer';
import { PyramidPublicationStatus } from '../PyramidPublicationStatus';
import { RemoteConnectionStatus } from '../RemoteConnectionStatus';

type NodeTab = 'sync' | 'market' | 'pyramids' | 'remote' | 'logs';

export function NodeMode() {
    const { state } = useAppContext();
    const [activeTab, setActiveTab] = useState<NodeTab>('sync');
    const [syncing, setSyncing] = useState(false);

    const handleSync = useCallback(async () => {
        setSyncing(true);
        try {
            await invoke("sync_content");
        } catch (err) {
            console.error("Sync failed:", err);
        } finally {
            setSyncing(false);
        }
    }, []);

    const folderCount = state.syncState ? Object.keys(state.syncState.linked_folders).length : 0;

    // Extract tunnel info from state if available
    const tunnelUrl = state.tunnelStatus?.tunnel_url ?? null;
    const tunnelConnected = state.tunnelStatus?.status === "Connected";

    return (
        <div className="mode-container">
            {/* Sub-tab navigation */}
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'sync' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('sync')}
                >
                    Sync ({folderCount})
                </button>
                <button
                    className={`node-tab ${activeTab === 'market' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('market')}
                >
                    Market
                </button>
                <button
                    className={`node-tab ${activeTab === 'pyramids' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('pyramids')}
                >
                    Pyramids
                </button>
                <button
                    className={`node-tab ${activeTab === 'remote' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('remote')}
                >
                    Remote
                </button>
                <button
                    className={`node-tab ${activeTab === 'logs' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('logs')}
                >
                    Logs
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'sync' && (
                    <SyncStatus
                        syncState={state.syncState}
                        syncing={syncing}
                        onSync={handleSync}
                    />
                )}
                {activeTab === 'market' && <MarketView />}
                {activeTab === 'pyramids' && <PyramidPublicationStatus />}
                {activeTab === 'remote' && (
                    <RemoteConnectionStatus
                        tunnelUrl={tunnelUrl}
                        tunnelConnected={tunnelConnected}
                    />
                )}
                {activeTab === 'logs' && <LogViewer />}
            </div>
        </div>
    );
}
