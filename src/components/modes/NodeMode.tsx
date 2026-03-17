import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../../contexts/AppContext';
import { SyncStatus } from '../SyncStatus';
import { MarketView } from '../MarketView';
import { LogViewer } from '../LogViewer';

type NodeTab = 'sync' | 'market' | 'logs';

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
                {activeTab === 'logs' && <LogViewer />}
            </div>
        </div>
    );
}
