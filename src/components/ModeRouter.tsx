import { useAppContext } from '../contexts/AppContext';
import { DashboardMode } from './modes/DashboardMode';
import { PyramidsMode } from './modes/PyramidsMode';
import { SearchMode } from './modes/SearchMode';
import { ComposeMode } from './modes/ComposeMode';
import { FleetMode } from './modes/FleetMode';
import { KnowledgeMode } from './modes/KnowledgeMode';
import { ToolsMode } from './modes/ToolsMode';
import { OperationsMode } from './modes/OperationsMode';
import { IdentityMode } from './modes/IdentityMode';
import { SettingsMode } from './modes/SettingsMode';

export function ModeRouter() {
    const { state } = useAppContext();

    switch (state.activeMode) {
        case 'pyramids':
            return <PyramidsMode />;
        case 'knowledge':
            return <KnowledgeMode />;
        case 'tools':
            return <ToolsMode />;
        case 'dashboard':
            return <DashboardMode />;
        case 'search':
            return <SearchMode />;
        case 'compose':
            return <ComposeMode />;
        case 'fleet':
            return <FleetMode />;
        case 'operations':
            return <OperationsMode />;
        case 'identity':
            return <IdentityMode />;
        case 'settings':
            return <SettingsMode />;
        default:
            return <PyramidsMode />;
    }
}
