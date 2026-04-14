import { useState } from 'react';
import { QueueLiveView } from '../QueueLiveView';
import { MarketDashboard } from '../MarketDashboard';

type MarketTab = 'queue' | 'market';

export function MarketMode() {
    const [activeTab, setActiveTab] = useState<MarketTab>('queue');

    return (
        <div className="mode-container market-mode">
            {/* Sub-tab navigation */}
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'queue' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('queue')}
                >
                    Queue
                </button>
                <button
                    className={`node-tab ${activeTab === 'market' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('market')}
                >
                    Market
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'queue' && <QueueLiveView />}
                {activeTab === 'market' && <MarketDashboard />}
            </div>
        </div>
    );
}
