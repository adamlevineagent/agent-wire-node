import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

interface MeshThread {
    id: string;
    name?: string;
    status?: string;
    [key: string]: unknown;
}

interface MeshIntent {
    id?: string;
    description: string;
    scope?: string;
    target_id?: string;
    thread_id?: string;
    created_at?: string;
    [key: string]: unknown;
}

interface MeshStatus {
    active_threads: number;
    threads: MeshThread[];
    intents: MeshIntent[];
    board: Record<string, unknown>;
}

export function MeshPanel() {
    const { wireApiCall } = useAppContext();
    const [status, setStatus] = useState<MeshStatus | null>(null);
    const [board, setBoard] = useState<Record<string, unknown>>({});
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // Write blackboard form state
    const [writeKey, setWriteKey] = useState('');
    const [writeValue, setWriteValue] = useState('');
    const [writeThreadId, setWriteThreadId] = useState('');
    const [writeLoading, setWriteLoading] = useState(false);

    // Declare intent form state
    const [intentDescription, setIntentDescription] = useState('');
    const [intentScope, setIntentScope] = useState('');
    const [intentTargetId, setIntentTargetId] = useState('');
    const [intentThreadId, setIntentThreadId] = useState('');
    const [intentLoading, setIntentLoading] = useState(false);

    const fetchData = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const [statusData, boardData] = await Promise.all([
                wireApiCall('GET', '/api/v1/mesh/status') as Promise<MeshStatus>,
                wireApiCall('GET', '/api/v1/mesh/board') as Promise<{ board: Record<string, unknown> }>,
            ]);
            setStatus(statusData);
            setBoard(boardData?.board || {});
        } catch (err: any) {
            setError(err?.message || 'Failed to load mesh status');
        } finally {
            setLoading(false);
        }
    }, [wireApiCall]);

    useEffect(() => {
        fetchData();
    }, [fetchData]);

    const formatAge = (createdAt: string | undefined): string => {
        if (!createdAt) return 'unknown age';
        const diffMs = Date.now() - new Date(createdAt).getTime();
        if (diffMs < 0) return 'just now';
        const seconds = Math.floor(diffMs / 1000);
        if (seconds < 60) return `${seconds}s ago`;
        const minutes = Math.floor(seconds / 60);
        if (minutes < 60) return `${minutes}m ago`;
        const hours = Math.floor(minutes / 60);
        if (hours < 24) return `${hours}h ago`;
        const days = Math.floor(hours / 24);
        return `${days}d ago`;
    };

    const handleReleaseIntent = async (intent: MeshIntent) => {
        const intentId = intent.id;
        if (!intentId) {
            alert('Cannot release intent: missing intent ID');
            return;
        }
        const msg = `Release intent?\n\nID: ${intentId}\nDescription: ${intent.description}\nScope: ${intent.scope || '(none)'}\nTarget: ${intent.target_id || '(none)'}`;
        if (!confirm(msg)) return;

        try {
            const headers: Record<string, string> = {};
            if (intent.thread_id) {
                headers['X-Wire-Thread'] = intent.thread_id;
            }
            await wireApiCall(
                'DELETE',
                `/api/v1/mesh/intent?intent_id=${encodeURIComponent(intentId)}`,
                null,
                Object.keys(headers).length > 0 ? headers : undefined
            );
            await fetchData();
        } catch (err: any) {
            alert(`Release intent failed: ${err?.message || 'Unknown error'}`);
        }
    };

    const handleDeleteBoardKey = async (key: string) => {
        const msg = `Delete blackboard key?\n\nKey: ${key}\n\nThis removes the key from the shared scratchpad.`;
        if (!confirm(msg)) return;

        try {
            await wireApiCall(
                'DELETE',
                `/api/v1/mesh/board?key=${encodeURIComponent(key)}`
            );
            await fetchData();
        } catch (err: any) {
            alert(`Delete key failed: ${err?.message || 'Unknown error'}`);
        }
    };

    const handleWriteBoard = async () => {
        if (!writeKey.trim()) return;
        const msg = `Write to blackboard?\n\nKey: ${writeKey}\nValue: ${writeValue}\nThread: ${writeThreadId || '(none)'}`;
        if (!confirm(msg)) return;

        setWriteLoading(true);
        try {
            const headers: Record<string, string> = {};
            if (writeThreadId.trim()) {
                headers['X-Wire-Thread'] = writeThreadId.trim();
            }
            await wireApiCall('POST', '/api/v1/mesh/board', {
                action: 'write',
                key: writeKey.trim(),
                value: writeValue,
            }, headers);
            setWriteKey('');
            setWriteValue('');
            setWriteThreadId('');
            await fetchData();
        } catch (err: any) {
            alert(`Write failed: ${err?.message || 'Unknown error'}`);
        } finally {
            setWriteLoading(false);
        }
    };

    const handleDeclareIntent = async () => {
        if (!intentDescription.trim()) return;
        const msg = `Declare intent?\n\nDescription: ${intentDescription}\nScope: ${intentScope || '(none)'}\nTarget: ${intentTargetId || '(none)'}\nThread: ${intentThreadId || '(none)'}`;
        if (!confirm(msg)) return;

        setIntentLoading(true);
        try {
            const headers: Record<string, string> = {};
            if (intentThreadId.trim()) {
                headers['X-Wire-Thread'] = intentThreadId.trim();
            }
            const body: Record<string, string> = {
                action: 'declare',
                description: intentDescription.trim(),
            };
            if (intentScope.trim()) body.scope = intentScope.trim();
            if (intentTargetId.trim()) body.target_id = intentTargetId.trim();

            await wireApiCall('POST', '/api/v1/mesh/intent', body, headers);
            setIntentDescription('');
            setIntentScope('');
            setIntentTargetId('');
            setIntentThreadId('');
            await fetchData();
        } catch (err: any) {
            alert(`Intent declaration failed: ${err?.message || 'Unknown error'}`);
        } finally {
            setIntentLoading(false);
        }
    };

    if (loading) {
        return (
            <div className="fleet-mesh-panel">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading mesh status...</span>
                </div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="fleet-mesh-panel">
                <div className="fleet-mesh-header">
                    <h3>Mesh Coordination</h3>
                </div>
                <div className="corpora-error">
                    <span>{error}</span>
                    <button
                        className="stewardship-btn stewardship-btn-ghost"
                        onClick={fetchData}
                    >
                        Retry
                    </button>
                </div>
            </div>
        );
    }

    const boardEntries = Object.entries(board);

    return (
        <div className="fleet-mesh-panel">
            <div className="fleet-mesh-header">
                <h3>Mesh Coordination</h3>
                <span className="fleet-mesh-thread-count">
                    {status?.active_threads ?? 0} active thread{(status?.active_threads ?? 0) !== 1 ? 's' : ''}
                </span>
            </div>

            {/* Threads */}
            <section className="fleet-mesh-section">
                <h4 className="fleet-mesh-section-title">Threads</h4>
                {(!status?.threads || status.threads.length === 0) ? (
                    <p className="fleet-mesh-empty">No active threads.</p>
                ) : (
                    <div className="fleet-mesh-thread-list">
                        {status.threads.map((thread, i) => (
                            <div key={thread.id || i} className="fleet-mesh-thread-item">
                                <span className="fleet-mesh-thread-id">{thread.id}</span>
                                {thread.name && <span className="fleet-mesh-thread-name">{thread.name}</span>}
                                {thread.status && <span className="fleet-mesh-thread-status">{thread.status}</span>}
                            </div>
                        ))}
                    </div>
                )}
            </section>

            {/* Intents */}
            <section className="fleet-mesh-section">
                <h4 className="fleet-mesh-section-title">Intents</h4>
                {(!status?.intents || status.intents.length === 0) ? (
                    <p className="fleet-mesh-empty">No declared intents.</p>
                ) : (
                    <div className="fleet-mesh-intent-list">
                        {status.intents.map((intent, i) => (
                            <div key={intent.id || i} className="fleet-mesh-intent-item">
                                <div className="fleet-mesh-intent-header">
                                    <span className="fleet-mesh-intent-desc">{intent.description}</span>
                                    <button
                                        className="stewardship-btn stewardship-btn-ghost stewardship-btn-sm"
                                        onClick={() => handleReleaseIntent(intent)}
                                        title="Release this intent"
                                    >
                                        Release
                                    </button>
                                </div>
                                <div className="fleet-mesh-intent-meta">
                                    {intent.scope && (
                                        <span className="fleet-mesh-intent-scope">Scope: {intent.scope}</span>
                                    )}
                                    {intent.target_id && (
                                        <span className="fleet-mesh-intent-target">Target: {intent.target_id}</span>
                                    )}
                                    <span className="fleet-mesh-intent-age">{formatAge(intent.created_at)}</span>
                                    {intent.thread_id && (
                                        <span className="fleet-mesh-intent-thread">Thread: {intent.thread_id}</span>
                                    )}
                                </div>
                            </div>
                        ))}
                    </div>
                )}

                {/* Declare intent form */}
                <div className="fleet-mesh-form">
                    <h5 className="fleet-mesh-form-title">Declare Intent</h5>
                    <input
                        className="fleet-mesh-input"
                        type="text"
                        placeholder="Description (required)"
                        value={intentDescription}
                        onChange={(e) => setIntentDescription(e.target.value)}
                    />
                    <div className="fleet-mesh-form-row">
                        <input
                            className="fleet-mesh-input"
                            type="text"
                            placeholder="Scope"
                            value={intentScope}
                            onChange={(e) => setIntentScope(e.target.value)}
                        />
                        <input
                            className="fleet-mesh-input"
                            type="text"
                            placeholder="Target ID"
                            value={intentTargetId}
                            onChange={(e) => setIntentTargetId(e.target.value)}
                        />
                    </div>
                    <input
                        className="fleet-mesh-input"
                        type="text"
                        placeholder="Thread ID (for X-Wire-Thread header)"
                        value={intentThreadId}
                        onChange={(e) => setIntentThreadId(e.target.value)}
                    />
                    <button
                        className="stewardship-btn stewardship-btn-primary"
                        onClick={handleDeclareIntent}
                        disabled={intentLoading || !intentDescription.trim()}
                    >
                        {intentLoading ? 'Declaring...' : 'Declare Intent'}
                    </button>
                </div>
            </section>

            {/* Blackboard */}
            <section className="fleet-mesh-section">
                <h4 className="fleet-mesh-section-title">Blackboard</h4>
                {boardEntries.length === 0 ? (
                    <p className="fleet-mesh-empty">Blackboard is empty.</p>
                ) : (
                    <div className="fleet-mesh-board">
                        {boardEntries.map(([key, value]) => (
                            <div key={key} className="fleet-mesh-board-entry">
                                <span className="fleet-mesh-board-key">{key}</span>
                                <span className="fleet-mesh-board-value">
                                    {typeof value === 'object' ? JSON.stringify(value) : String(value)}
                                </span>
                                <button
                                    className="stewardship-btn stewardship-btn-ghost stewardship-btn-sm"
                                    onClick={() => handleDeleteBoardKey(key)}
                                    title="Delete this blackboard key"
                                >
                                    Delete
                                </button>
                            </div>
                        ))}
                    </div>
                )}

                {/* Write blackboard form */}
                <div className="fleet-mesh-form">
                    <h5 className="fleet-mesh-form-title">Write to Blackboard</h5>
                    <div className="fleet-mesh-form-row">
                        <input
                            className="fleet-mesh-input"
                            type="text"
                            placeholder="Key (required)"
                            value={writeKey}
                            onChange={(e) => setWriteKey(e.target.value)}
                        />
                        <input
                            className="fleet-mesh-input"
                            type="text"
                            placeholder="Value"
                            value={writeValue}
                            onChange={(e) => setWriteValue(e.target.value)}
                        />
                    </div>
                    <input
                        className="fleet-mesh-input"
                        type="text"
                        placeholder="Thread ID (for X-Wire-Thread header)"
                        value={writeThreadId}
                        onChange={(e) => setWriteThreadId(e.target.value)}
                    />
                    <button
                        className="stewardship-btn stewardship-btn-primary"
                        onClick={handleWriteBoard}
                        disabled={writeLoading || !writeKey.trim()}
                    >
                        {writeLoading ? 'Writing...' : 'Write'}
                    </button>
                </div>
            </section>
        </div>
    );
}
