import { useState } from 'react';
import { QueueLiveView } from '../QueueLiveView';
import { MarketDashboard } from '../MarketDashboard';
import { ComputeChronicle } from '../ComputeChronicle';
import { ComputeMarketDashboard } from '../market/ComputeMarketDashboard';

type MarketTab = 'queue' | 'chronicle' | 'hosting' | 'compute';

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
                    className={`node-tab ${activeTab === 'chronicle' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('chronicle')}
                >
                    Chronicle
                </button>
                <button
                    className={`node-tab ${activeTab === 'compute' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('compute')}
                >
                    Compute
                </button>
                <button
                    className={`node-tab ${activeTab === 'hosting' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('hosting')}
                >
                    Hosting
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'queue' && <QueueLiveView />}
                {activeTab === 'chronicle' && <ComputeChronicle />}
                {activeTab === 'compute' && <ComputeMarketDashboard />}
                {activeTab === 'hosting' && <MarketDashboard />}
            </div>
        </div>
    );
}
