import { useState } from 'react';
import { useAppContext } from '../../contexts/AppContext';
import { FleetOverview } from '../fleet/FleetOverview';
import { MeshPanel } from '../fleet/MeshPanel';
import { TaskBoard } from '../fleet/TaskBoard';
import { CorpusDetail } from '../stewardship/CorpusDetail';
import { DocumentDetail } from '../stewardship/DocumentDetail';

type FleetTab = 'fleet' | 'mesh' | 'tasks';

export function FleetMode() {
    const { currentView } = useAppContext();
    const [activeTab, setActiveTab] = useState<FleetTab>('fleet');
    const view = currentView('fleet');

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

    // Root view: sub-tab navigation
    return (
        <div className="mode-container">
            {/* Sub-tab navigation */}
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'fleet' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('fleet')}
                >
                    Fleet Overview
                </button>
                <button
                    className={`node-tab ${activeTab === 'mesh' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('mesh')}
                >
                    Coordination
                </button>
                <button
                    className={`node-tab ${activeTab === 'tasks' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('tasks')}
                >
                    Tasks
                </button>
            </nav>

            {/* Tab content */}
            <div className="node-tab-content">
                {activeTab === 'fleet' && (
                    <div className="fleet-layout">
                        <FleetOverview />
                    </div>
                )}
                {activeTab === 'mesh' && (
                    <div className="fleet-layout">
                        <MeshPanel />
                    </div>
                )}
                {activeTab === 'tasks' && (
                    <div className="fleet-layout">
                        <TaskBoard />
                    </div>
                )}
            </div>
        </div>
    );
}
