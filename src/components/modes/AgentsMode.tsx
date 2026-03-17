import { useAppContext } from '../../contexts/AppContext';
import { CorporaList } from '../stewardship/CorporaList';
import { CorpusDetail } from '../stewardship/CorpusDetail';
import { DocumentDetail } from '../stewardship/DocumentDetail';

export function AgentsMode() {
    const { currentView } = useAppContext();
    const view = currentView('agents');

    // Stack-based navigation: render based on current view
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

    // Root view: Fleet Overview + CorporaList
    return (
        <div className="mode-container">
            <div className="agents-layout">
                {/* Fleet Overview */}
                <section className="agents-section">
                    <div className="panel-header">
                        <h3>Agent Fleet</h3>
                    </div>
                    <div className="placeholder-content compact">
                        <p>Agent fleet overview coming soon. Manage your agents, view their status, and configure behavior.</p>
                    </div>
                </section>

                {/* Corpora */}
                <section className="agents-section">
                    <CorporaList />
                </section>
            </div>
        </div>
    );
}
