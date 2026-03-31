import { useState, useEffect } from 'react';
import { LOCAL_TOOLS } from '../../config/wire-actions';
import { useAppContext } from '../../contexts/AppContext';

type ToolsTab = 'my-tools' | 'discover' | 'create';

const TYPE_BADGE_COLORS: Record<string, string> = {
    action: '#3b82f6',
    chain: '#8b5cf6',
    skill: '#10b981',
    template: '#f59e0b',
};

interface WireTool {
    id: string;
    title: string;
    type: string;
    description: string;
    published: boolean;
    createdAt?: string;
}

export function ToolsMode() {
    const [activeTab, setActiveTab] = useState<ToolsTab>('my-tools');

    return (
        <div className="mode-container">
            <nav className="node-tabs">
                <button
                    className={`node-tab ${activeTab === 'my-tools' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('my-tools')}
                >
                    My Tools
                </button>
                <button
                    className={`node-tab ${activeTab === 'discover' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('discover')}
                >
                    Discover
                </button>
                <button
                    className={`node-tab ${activeTab === 'create' ? 'node-tab-active' : ''}`}
                    onClick={() => setActiveTab('create')}
                >
                    Create
                </button>
            </nav>

            <div className="node-tab-content">
                {activeTab === 'my-tools' && <MyToolsPanel />}
                {activeTab === 'discover' && <DiscoverPanel />}
                {activeTab === 'create' && <CreatePanel />}
            </div>
        </div>
    );
}

function MyToolsPanel() {
    const { wireApiCall } = useAppContext();
    const [wireTools, setWireTools] = useState<WireTool[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        setError(null);

        // Fetch published action contributions from the Wire
        wireApiCall('GET', '/api/v1/wire/my/contributions')
            .then((data: unknown) => {
                if (cancelled) return;
                // Handle various response shapes
                const contributions = Array.isArray(data)
                    ? data
                    : (data as Record<string, unknown>)?.contributions
                    ?? (data as Record<string, unknown>)?.data
                    ?? [];
                const actions = (contributions as Array<Record<string, unknown>>)
                    .filter(c => c.type === 'action')
                    .map(c => ({
                        id: String(c.id ?? c.uuid ?? ''),
                        title: String(c.title ?? 'Untitled'),
                        type: String(c.type ?? 'action'),
                        description: String(c.body ?? '').slice(0, 200),
                        published: true,
                        createdAt: c.created_at ? String(c.created_at) : undefined,
                    }));
                setWireTools(actions);
            })
            .catch((err: unknown) => {
                if (cancelled) return;
                console.warn('Failed to fetch Wire tools:', err);
                setError('Could not load published tools from the Wire.');
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });

        return () => { cancelled = true; };
    }, [wireApiCall]);

    // Merge local tools (planner) with Wire-published tools
    const allTools: Array<WireTool & { usageCount?: number }> = [
        ...LOCAL_TOOLS.map(t => ({
            id: t.id,
            title: t.title,
            type: t.type,
            description: t.description,
            published: t.published,
            usageCount: t.usageCount,
            createdAt: undefined as string | undefined,
        })),
        ...wireTools,
    ];

    return (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
            {loading && (
                <p style={{ color: 'var(--text-secondary)', fontSize: '13px' }}>
                    Loading tools...
                </p>
            )}
            {error && (
                <p style={{ color: 'var(--accent-warning, #f59e0b)', fontSize: '13px' }}>
                    {error}
                </p>
            )}
            {!loading && allTools.map((tool) => (
                <div
                    key={tool.id}
                    style={{
                        background: 'var(--bg-secondary, #1a1a2e)',
                        border: '1px solid var(--border-primary, #2a2a4a)',
                        borderRadius: '8px',
                        padding: '16px',
                    }}
                >
                    <div style={{ display: 'flex', alignItems: 'center', gap: '10px', marginBottom: '8px' }}>
                        <span style={{ fontSize: '15px', fontWeight: 600, color: 'var(--text-primary, #e0e0e0)' }}>
                            {tool.title}
                        </span>
                        <span
                            style={{
                                fontSize: '11px',
                                fontWeight: 600,
                                textTransform: 'uppercase',
                                letterSpacing: '0.05em',
                                padding: '2px 8px',
                                borderRadius: '4px',
                                background: TYPE_BADGE_COLORS[tool.type] ?? '#6b7280',
                                color: '#fff',
                            }}
                        >
                            {tool.type}
                        </span>
                        {tool.published && (
                            <span
                                style={{
                                    fontSize: '11px',
                                    padding: '2px 6px',
                                    borderRadius: '4px',
                                    background: 'rgba(16, 185, 129, 0.15)',
                                    color: '#10b981',
                                }}
                            >
                                Published
                            </span>
                        )}
                    </div>
                    <p style={{ margin: 0, fontSize: '13px', color: 'var(--text-secondary, #a0a0b0)', lineHeight: 1.5 }}>
                        {tool.description}
                    </p>
                    {tool.createdAt && (
                        <p style={{ margin: '4px 0 0', fontSize: '11px', color: 'var(--text-tertiary, #6b7280)' }}>
                            Published {new Date(tool.createdAt).toLocaleDateString()}
                        </p>
                    )}
                </div>
            ))}
            {!loading && allTools.length === 0 && (
                <p style={{ color: 'var(--text-secondary)' }}>
                    No tools yet. Execute a plan with "Publish to Wire" enabled to create one.
                </p>
            )}
        </div>
    );
}

function DiscoverPanel() {
    return (
        <div style={{ padding: '24px 0' }}>
            <p style={{ color: 'var(--text-secondary, #a0a0b0)', fontSize: '14px' }}>
                Search the Wire for published tools and chains. Coming in Sprint 3.
            </p>
        </div>
    );
}

function CreatePanel() {
    return (
        <div style={{ padding: '24px 0' }}>
            <p style={{ color: 'var(--text-secondary, #a0a0b0)', fontSize: '14px' }}>
                Describe what you need, intelligence builds it. Coming in Sprint 3.
            </p>
        </div>
    );
}
