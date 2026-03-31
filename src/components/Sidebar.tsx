import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext, Mode } from '../contexts/AppContext';
import { LOCAL_TOOLS } from '../config/wire-actions';

// --- Glow priority: Operations > Fleet > Knowledge > Understanding > Tools ---
type GlowPriority = 'operations' | 'fleet' | 'knowledge' | 'pyramids' | 'tools';
const GLOW_ORDER: GlowPriority[] = ['operations', 'fleet', 'knowledge', 'pyramids', 'tools'];

type VisualState = 'glow-full' | 'glow-subtle' | 'bright' | 'subtle' | 'dim';

interface SidebarItem {
    key: Mode;
    label: string;
    section: 'YOUR WORLD' | 'IN MOTION' | 'THE WIRE' | 'YOU';
    headline: () => string;
    context: () => string;
    shouldGlow: () => boolean;
}

function formatTimeAgo(isoString: string | null): string {
    if (!isoString) return 'never';
    const diff = Date.now() - new Date(isoString).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
}

export function Sidebar() {
    const { state, setMode } = useAppContext();
    const [appVersion, setAppVersion] = useState<string>('');

    useEffect(() => {
        invoke<string>('get_app_version').then(setAppVersion).catch(() => {});
    }, []);

    const localToolCount = LOCAL_TOOLS.length;
    const publishedToolCount = LOCAL_TOOLS.filter(t => t.published).length;

    const items: SidebarItem[] = useMemo(() => [
        {
            key: 'pyramids' as Mode,
            label: 'Understanding',
            section: 'YOUR WORLD' as const,
            headline: () => `${state.pyramidCount} pyramid${state.pyramidCount !== 1 ? 's' : ''}`,
            context: () => state.latestApexQuestion ?? 'No pyramids yet',
            shouldGlow: () => state.syncState?.is_syncing === true, // building proxy
        },
        {
            key: 'knowledge' as Mode,
            label: 'Knowledge',
            section: 'YOUR WORLD' as const,
            headline: () => `${state.docCount} doc${state.docCount !== 1 ? 's' : ''}`,
            context: () => `${state.corpusCount} corpor${state.corpusCount !== 1 ? 'a' : 'us'} · synced ${formatTimeAgo(state.lastSyncTime)}`,
            shouldGlow: () => state.syncState?.is_syncing === true,
        },
        {
            key: 'tools' as Mode,
            label: 'Tools',
            section: 'YOUR WORLD' as const,
            headline: () => `${localToolCount} tool${localToolCount !== 1 ? 's' : ''}`,
            context: () => publishedToolCount > 0 ? `${publishedToolCount} published` : 'none published',
            shouldGlow: () => false, // Sprint 0: no glow trigger
        },
        {
            key: 'fleet' as Mode,
            label: 'Fleet',
            section: 'IN MOTION' as const,
            headline: () => `${state.fleetOnlineCount} online`,
            context: () => `${state.taskCount} task${state.taskCount !== 1 ? 's' : ''}`,
            shouldGlow: () => state.taskCount > 0,
        },
        {
            key: 'operations' as Mode,
            label: 'Operations',
            section: 'IN MOTION' as const,
            headline: () => {
                const total = state.notificationCount + state.messageCount;
                return total > 0 ? `${total} new` : 'clear';
            },
            context: () => {
                const parts: string[] = [];
                if (state.notificationCount > 0) parts.push(`${state.notificationCount} notification${state.notificationCount !== 1 ? 's' : ''}`);
                if (state.messageCount > 0) parts.push(`${state.messageCount} message${state.messageCount !== 1 ? 's' : ''}`);
                return parts.length > 0 ? parts.join(' · ') : 'no activity';
            },
            shouldGlow: () => state.notificationCount > 0 || state.messageCount > 0,
        },
        {
            key: 'search' as Mode,
            label: 'Search',
            section: 'THE WIRE' as const,
            headline: () => '',
            context: () => '',
            shouldGlow: () => false,
        },
        {
            key: 'compose' as Mode,
            label: 'Compose',
            section: 'THE WIRE' as const,
            headline: () => state.draftCount > 0 ? `${state.draftCount} draft${state.draftCount !== 1 ? 's' : ''}` : '',
            context: () => '',
            shouldGlow: () => false,
        },
    ], [state, localToolCount, publishedToolCount]);

    // Compute glow assignments: max 2 glow, rest get bright dot
    const glowMap = useMemo(() => {
        const map: Record<string, VisualState> = {};
        const glowing: string[] = [];

        for (const priority of GLOW_ORDER) {
            const item = items.find(i => i.key === priority);
            if (item && item.shouldGlow()) {
                if (glowing.length === 0) {
                    map[priority] = 'glow-full';
                    glowing.push(priority);
                } else if (glowing.length === 1) {
                    map[priority] = 'glow-subtle';
                    glowing.push(priority);
                } else {
                    map[priority] = 'bright';
                }
            }
        }
        return map;
    }, [items]);

    function getVisualState(item: SidebarItem): VisualState {
        if (glowMap[item.key]) return glowMap[item.key];
        // Search is always dim
        if (item.key === 'search') return 'dim';
        // Compose is subtle if drafts, dim otherwise
        if (item.key === 'compose') return state.draftCount > 0 ? 'subtle' : 'dim';
        // Default: subtle if has content
        return 'subtle';
    }

    const creditDisplay = Math.floor(
        state.creditBalance > 0
            ? state.creditBalance
            : (state.credits?.credits_earned ?? 0)
    );

    const isOnline = state.tunnelStatus?.status === 'Connected';
    const isConnecting = typeof state.tunnelStatus?.status === 'string' &&
        ['Connecting', 'Provisioning', 'Downloading'].includes(state.tunnelStatus.status);

    // Group items by section for rendering headers
    const sections: { label: string; items: SidebarItem[] }[] = [
        { label: 'YOUR WORLD', items: items.filter(i => i.section === 'YOUR WORLD') },
        { label: 'IN MOTION', items: items.filter(i => i.section === 'IN MOTION') },
        { label: 'THE WIRE', items: items.filter(i => i.section === 'THE WIRE') },
    ];

    return (
        <nav className="sidebar">
            {/* Scrollable main section */}
            <div className="sidebar-scrollable">
                {sections.map(section => (
                    <div key={section.label}>
                        <div className="sidebar-section-header">{section.label}</div>
                        {section.items.map(item => {
                            const isActive = state.activeMode === item.key;
                            const vs = getVisualState(item);
                            const headline = item.headline();
                            const context = item.context();

                            return (
                                <button
                                    key={item.key}
                                    className={[
                                        'sidebar-item',
                                        isActive ? 'sidebar-item-active' : '',
                                        `sidebar-vs-${vs}`,
                                    ].join(' ')}
                                    onClick={() => setMode(item.key)}
                                    title={item.label}
                                >
                                    <div className="sidebar-item-line1">
                                        <span className="sidebar-item-label">{item.label}</span>
                                        {headline && (
                                            <span className="sidebar-item-metric">{headline}</span>
                                        )}
                                    </div>
                                    {context && (
                                        <div className="sidebar-item-line2">{context}</div>
                                    )}
                                </button>
                            );
                        })}
                    </div>
                ))}
            </div>

            {/* Pinned bottom section */}
            <div className="sidebar-pinned">
                <div className="sidebar-section-header">YOU</div>

                {/* Network */}
                <button
                    className={`sidebar-compact-item ${state.activeMode === 'dashboard' ? 'sidebar-item-active' : ''}`}
                    onClick={() => setMode('dashboard')}
                    title="Network"
                >
                    <div className={`sidebar-status-dot ${isOnline ? 'online' : isConnecting ? 'connecting' : 'offline'}`} />
                    <span className="sidebar-compact-label">Network</span>
                    <span className="sidebar-compact-metric">{creditDisplay} credits</span>
                </button>

                {/* Identity */}
                <button
                    className={`sidebar-compact-item ${state.activeMode === 'identity' ? 'sidebar-item-active' : ''}`}
                    onClick={() => setMode('identity')}
                    title="Identity"
                >
                    <span className="sidebar-compact-label">@{state.email?.split('@')[0] ?? 'node'}</span>
                </button>

                {/* Settings */}
                <button
                    className={`sidebar-compact-item ${state.activeMode === 'settings' ? 'sidebar-item-active' : ''}`}
                    onClick={() => setMode('settings')}
                    title="Settings"
                >
                    <span className="sidebar-compact-icon">&#x2699;&#xFE0F;</span>
                    <span className="sidebar-compact-label">Settings</span>
                </button>
            </div>

            {appVersion && (
                <div className="version-badge">v{appVersion}</div>
            )}
        </nav>
    );
}
