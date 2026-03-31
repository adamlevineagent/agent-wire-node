import { useState } from 'react';
import { DashboardOverview } from './DashboardOverview';
import { MarketView } from '../MarketView';
import { InfrastructurePanel } from './InfrastructurePanel';

type NetworkTab = 'dashboard' | 'market' | 'infrastructure';

export function DashboardMode() {
    const [activeTab, setActiveTab] = useState<NetworkTab>('dashboard');

    return (
        <div className="mode-container dashboard-enhanced">
            {/* Sub-tab navigation */}
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'dashboard' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('dashboard')}
                >
                    Dashboard
                </button>
                <button
                    className={`node-tab ${activeTab === 'market' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('market')}
                >
                    Market
                </button>
                <button
                    className={`node-tab ${activeTab === 'infrastructure' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('infrastructure')}
                >
                    Infrastructure
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'dashboard' && (
                    <DashboardOverview onNavigateInfrastructure={() => setActiveTab('infrastructure')} />
                )}
                {activeTab === 'market' && <MarketView />}
                {activeTab === 'infrastructure' && <InfrastructurePanel />}
            </div>
        </div>
    );
}
