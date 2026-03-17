import { useAppContext, Mode } from '../contexts/AppContext';

interface ModeItem {
    key: Mode;
    icon: string;
    label: string;
    badge?: () => number | null;
}

export function Sidebar() {
    const { state, setMode } = useAppContext();

    // TODO: Pending requests endpoint requires agent auth (requireWireScope), not operator auth.
    // Re-enable once an operator-compatible pending requests endpoint exists.
    // For now, badge always shows 0.
    const curationCount = 0;

    const modeItems: ModeItem[] = [
        { key: 'dashboard', icon: '\u{1F3E0}', label: 'Dashboard' },
        { key: 'search', icon: '\u{1F50D}', label: 'Search' },
        { key: 'warroom', icon: '\u{1F4E1}', label: 'Warroom' },
        { key: 'compose', icon: '\u{270D}\uFE0F', label: 'Compose' },
        {
            key: 'agents',
            icon: '\u{1F916}',
            label: 'Agents',
            badge: () => curationCount > 0 ? curationCount : null,
        },
        { key: 'node', icon: '\u{1F5A5}\uFE0F', label: 'Node' },
        {
            key: 'activity',
            icon: '\u{1F4CB}',
            label: 'Activity',
            badge: () => state.notificationCount > 0 ? state.notificationCount : null,
        },
        { key: 'identity', icon: '\u{1F464}', label: 'Identity' },
        { key: 'settings', icon: '\u{2699}\uFE0F', label: 'Settings' },
    ];

    const creditDisplay = Math.floor(
        state.creditBalance > 0
            ? state.creditBalance
            : (state.credits?.credits_earned ?? 0)
    );

    return (
        <nav className="sidebar">
            {/* Identity badge */}
            <div className="sidebar-identity">
                <div className="sidebar-avatar">W</div>
                <div className="sidebar-identity-info">
                    <span className="sidebar-handle">{state.email?.split('@')[0] ?? 'Node'}</span>
                    <span className="sidebar-credits">{creditDisplay} credits</span>
                </div>
            </div>

            {/* Mode items */}
            <div className="sidebar-modes">
                {modeItems.map((item) => {
                    const isActive = state.activeMode === item.key;
                    const badgeValue = item.badge?.() ?? null;
                    return (
                        <button
                            key={item.key}
                            className={`sidebar-mode-item ${isActive ? 'sidebar-mode-active' : ''}`}
                            onClick={() => setMode(item.key)}
                            title={item.label}
                        >
                            <span className="sidebar-mode-icon">{item.icon}</span>
                            <span className="sidebar-mode-label">{item.label}</span>
                            {badgeValue !== null && (
                                <span className="sidebar-mode-badge">{badgeValue}</span>
                            )}
                        </button>
                    );
                })}
            </div>

            {/* Node status at bottom */}
            <div className="sidebar-node-status">
                <div className={`sidebar-status-dot ${
                    state.tunnelStatus?.status === 'Connected' ? 'online' :
                    typeof state.tunnelStatus?.status === 'string' &&
                    ['Connecting', 'Provisioning', 'Downloading'].includes(state.tunnelStatus.status) ? 'connecting' :
                    'offline'
                }`} />
                <span className="sidebar-status-text">
                    {state.tunnelStatus?.status === 'Connected' ? 'Online' :
                     typeof state.tunnelStatus?.status === 'string' &&
                     ['Connecting', 'Provisioning'].includes(state.tunnelStatus.status) ? 'Connecting' :
                     'Offline'}
                </span>
            </div>
        </nav>
    );
}
