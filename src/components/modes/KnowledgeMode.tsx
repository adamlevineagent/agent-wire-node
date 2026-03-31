import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../../contexts/AppContext';
import { CorporaList } from '../stewardship/CorporaList';
import { CorpusDetail } from '../stewardship/CorpusDetail';
import { DocumentDetail } from '../stewardship/DocumentDetail';
import { SyncStatus } from '../SyncStatus';

type KnowledgeTab = 'corpora' | 'sync';

export function KnowledgeMode() {
    const { state, currentView } = useAppContext();
    const [activeTab, setActiveTab] = useState<KnowledgeTab>('corpora');
    const [syncing, setSyncing] = useState(false);
    const view = currentView('knowledge');

    // Stack-based navigation for deep views (corpus detail, document detail)
    if (view.view === 'corpus-detail' && view.props.slug) {
        return (
            <div className="mode-container">
                <CorpusDetail slug={view.props.slug as string} />
            </div>
        );
    }

    if (view.view === 'document-detail' && view.props.documentId) {
        return (
            <div className="mode-container">
                <DocumentDetail documentId={view.props.documentId as string} />
            </div>
        );
    }

    const handleSync = async () => {
        setSyncing(true);
        try {
            await invoke("sync_content");
        } catch (err) {
            console.error("Sync failed:", err);
        } finally {
            setSyncing(false);
        }
    };

    // Root view: sub-tab navigation
    return (
        <div className="mode-container">
            {/* Sub-tab navigation */}
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'corpora' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('corpora')}
                >
                    Corpora
                </button>
                <button
                    className={`node-tab ${activeTab === 'sync' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('sync')}
                >
                    Local Sync
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'corpora' && (
                    <div className="fleet-layout">
                        <section className="fleet-section">
                            <CorporaList />
                        </section>
                    </div>
                )}
                {activeTab === 'sync' && (
                    <SyncStatus
                        syncState={state.syncState}
                        syncing={syncing}
                        onSync={handleSync}
                    />
                )}
            </div>
        </div>
    );
}
