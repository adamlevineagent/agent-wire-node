import { useAppContext } from '../contexts/AppContext';
import { DashboardMode } from './modes/DashboardMode';
import { SearchMode } from './modes/SearchMode';
import { WarroomMode } from './modes/WarroomMode';
import { ComposeMode } from './modes/ComposeMode';
import { AgentsMode } from './modes/AgentsMode';
import { NodeMode } from './modes/NodeMode';
import { ActivityMode } from './modes/ActivityMode';
import { IdentityMode } from './modes/IdentityMode';
import { SettingsMode } from './modes/SettingsMode';

export function ModeRouter() {
    const { state } = useAppContext();

    switch (state.activeMode) {
        case 'dashboard':
            return <DashboardMode />;
        case 'search':
            return <SearchMode />;
        case 'warroom':
            return <WarroomMode />;
        case 'compose':
            return <ComposeMode />;
        case 'agents':
            return <AgentsMode />;
        case 'node':
            return <NodeMode />;
        case 'activity':
            return <ActivityMode />;
        case 'identity':
            return <IdentityMode />;
        case 'settings':
            return <SettingsMode />;
        default:
            return <DashboardMode />;
    }
}
