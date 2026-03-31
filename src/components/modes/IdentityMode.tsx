import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../../contexts/AppContext';
import { ReputationDisplay } from '../identity/ReputationDisplay';

// --- Types ---

interface HandleInfo {
    id: string;
    handle: string;
    display_handle: string;
    payment_type: 'full' | 'layaway';
    status: 'active' | 'suspended' | 'released';
    created_at: string;
}

interface Transaction {
    id: string;
    amount: number;
    reason: string;
    reference_id?: string;
    balance_after?: number;
    created_at: string;
    // Legacy fields for backward compat
    type?: string;
    contribution_title?: string;
}

interface EarningsResponse {
    current_balance: number;
    recent_transactions: Transaction[];
    total_transactions?: number;
    has_more?: boolean;
}

interface HandleCheckResult {
    available: boolean;
    handle: string;
    cost?: number;
    reason?: string;
}

interface HandleCacheData {
    handles: HandleInfo[];
    cached_at: string | null;
}

// --- Component ---

export function IdentityMode() {
    const { state, operatorApiCall, wireApiCall } = useAppContext();

    // Handle state
    const [myHandles, setMyHandles] = useState<HandleInfo[]>([]);
    const [loadingHandles, setLoadingHandles] = useState(true);
    const [handleError, setHandleError] = useState<string | null>(null);
    const [syncing, setSyncing] = useState(false);
    const [cachedAt, setCachedAt] = useState<string | null>(null);
    const [showingCached, setShowingCached] = useState(false);

    // Handle check state
    const [checkInput, setCheckInput] = useState('');
    const [checking, setChecking] = useState(false);
    const [checkResult, setCheckResult] = useState<HandleCheckResult | null>(null);
    const [checkError, setCheckError] = useState<string | null>(null);

    // Handle registration state
    const [registering, setRegistering] = useState(false);
    const [registerError, setRegisterError] = useState<string | null>(null);
    const [registerSuccess, setRegisterSuccess] = useState<string | null>(null);

    // Transaction history state
    const [transactions, setTransactions] = useState<Transaction[]>([]);
    const [loadingTransactions, setLoadingTransactions] = useState(true);
    const [transactionError, setTransactionError] = useState<string | null>(null);
    const [transactionOffset, setTransactionOffset] = useState(0);
    const [transactionTotal, setTransactionTotal] = useState(0);
    const [hasMoreTransactions, setHasMoreTransactions] = useState(false);
    const TRANSACTION_LIMIT = 20;

    const mountedRef = useRef(true);
    useEffect(() => {
        mountedRef.current = true;
        return () => { mountedRef.current = false; };
    }, []);

    // Write handle data to local cache
    const cacheHandles = useCallback(async (handles: HandleInfo[]) => {
        try {
            await invoke('cache_wire_handles', { handles });
        } catch (err) {
            console.error('Failed to cache handles:', err);
        }
    }, []);

    // Load cached handles from disk
    const loadCachedHandles = useCallback(async (): Promise<HandleCacheData | null> => {
        try {
            const data = await invoke<HandleCacheData>('get_cached_wire_handles');
            return data;
        } catch {
            return null;
        }
    }, []);

    // Fetch handles from Wire API (background refresh)
    const refreshFromWire = useCallback(async (): Promise<HandleInfo[] | null> => {
        if (!state.operatorSessionToken) return null;
        try {
            const data: any = await operatorApiCall('GET', '/api/v1/wire/handles');
            const handles = data?.handles || data || [];
            return Array.isArray(handles) ? handles : [];
        } catch {
            return null;
        }
    }, [operatorApiCall, state.operatorSessionToken]);

    // Main fetch flow: cache-first, then background refresh
    const fetchHandles = useCallback(async () => {
        // Step 1: Load cached data immediately
        const cached = await loadCachedHandles();
        if (cached && cached.handles && cached.handles.length > 0) {
            if (mountedRef.current) {
                setMyHandles(cached.handles);
                setCachedAt(cached.cached_at);
                setShowingCached(true);
                setLoadingHandles(false);
            }
        }

        // Step 2: Background refresh from Wire API
        if (mountedRef.current) {
            setSyncing(true);
        }

        const fresh = await refreshFromWire();

        if (!mountedRef.current) return;

        if (fresh !== null) {
            // Wire API succeeded — update display and cache
            setMyHandles(fresh);
            setHandleError(null);
            setShowingCached(false);
            setCachedAt(null);
            setSyncing(false);
            setLoadingHandles(false);
            cacheHandles(fresh);
        } else {
            // Wire API failed — keep cached data if we have it
            setSyncing(false);
            setLoadingHandles(false);
            if (!cached || !cached.handles || cached.handles.length === 0) {
                setHandleError('Failed to load handles');
            }
            // If we have cached data, showingCached remains true
        }
    }, [loadCachedHandles, refreshFromWire, cacheHandles]);

    useEffect(() => {
        fetchHandles();
    }, [fetchHandles]);

    // Fetch transaction history from Wire API
    const fetchTransactions = useCallback(async (offset = 0, append = false) => {
        setLoadingTransactions(true);
        setTransactionError(null);
        try {
            const data = await wireApiCall('GET', `/api/v1/wire/my/earnings?limit=${TRANSACTION_LIMIT}&offset=${offset}`) as EarningsResponse;
            const txns = data?.recent_transactions || [];
            if (append) {
                setTransactions(prev => [...prev, ...txns]);
            } else {
                setTransactions(txns);
            }
            setTransactionOffset(offset);
            setTransactionTotal(data?.total_transactions ?? 0);
            setHasMoreTransactions(data?.has_more ?? txns.length >= TRANSACTION_LIMIT);
        } catch (err: any) {
            setTransactionError(err?.message || 'Failed to load transactions');
        } finally {
            setLoadingTransactions(false);
        }
    }, [wireApiCall]);

    useEffect(() => {
        fetchTransactions();
    }, [fetchTransactions]);

    // Check handle availability
    const handleCheck = useCallback(async () => {
        const handle = checkInput.trim().replace(/^@+/, '');
        if (!handle) return;
        setChecking(true);
        setCheckResult(null);
        setCheckError(null);

        try {
            const data: any = await operatorApiCall('GET', `/api/v1/wire/handles/check?handle=${encodeURIComponent(handle)}`);
            setCheckResult(data as HandleCheckResult);
        } catch (err: any) {
            setCheckError(err?.message || 'Failed to check availability');
        } finally {
            setChecking(false);
        }
    }, [checkInput, operatorApiCall]);

    // Register handle
    const handleRegister = useCallback(async (paymentType: 'full' | 'layaway') => {
        // checkResult.handle is already "@foo" from the server; checkInput may or may not have @
        const raw = checkResult?.handle || checkInput.trim();
        const normalized = raw.replace(/^@+/, '');
        if (!normalized) return;

        setRegistering(true);
        setRegisterError(null);
        setRegisterSuccess(null);

        try {
            await operatorApiCall('POST', '/api/v1/wire/handles', {
                handle: normalized,
                payment_type: paymentType,
            });
            setRegisterSuccess(`@${normalized} registered successfully!`);
            setCheckResult(null);
            setCheckInput('');
            fetchHandles();
        } catch (err: any) {
            setRegisterError(err?.message || 'Failed to register handle');
        } finally {
            setRegistering(false);
        }
    }, [checkResult, checkInput, operatorApiCall, fetchHandles]);

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            handleCheck();
        }
    };

    const activeHandle = myHandles.find(h => h.status === 'active');
    const currentHandle = activeHandle || myHandles[0];

    // Format cached_at timestamp for display
    const formatCachedAt = (iso: string | null): string => {
        if (!iso) return 'unknown';
        try {
            return new Date(iso).toLocaleString();
        } catch {
            return 'unknown';
        }
    };

    return (
        <div className="mode-container identity-mode">
            {/* Identity Summary */}
            <div className="identity-summary">
                <h2>Identity</h2>

                <div className="identity-info-grid">
                    <div className="identity-info-card">
                        <span className="identity-label">Email</span>
                        <span className="identity-value">{state.email || '\u2014'}</span>
                    </div>
                    <div className="identity-info-card">
                        <span className="identity-label">Operator ID</span>
                        <span className="identity-value mono">
                            {state.operatorId
                                ? `${state.operatorId.slice(0, 8)}...${state.operatorId.slice(-4)}`
                                : '\u2014'}
                        </span>
                    </div>
                    <ReputationDisplay />
                </div>
            </div>

            {/* Handle Section */}
            <div className="identity-handle-section">
                {loadingHandles ? (
                    <div className="handle-loading">
                        <div className="loading-spinner" />
                        <span>Loading handles...</span>
                    </div>
                ) : handleError ? (
                    <div className="handle-error">
                        <span>{handleError}</span>
                        <button className="handle-retry-btn" onClick={fetchHandles}>Retry</button>
                    </div>
                ) : currentHandle ? (
                    /* Has a handle */
                    <div className="handle-card">
                        <div className="handle-card-header">
                            <h3>
                                Your Handle
                                {syncing && (
                                    <span className="handle-sync-badge">syncing...</span>
                                )}
                            </h3>
                        </div>

                        <div className="handle-card-body">
                            <div className="handle-display">
                                <span className="handle-name">@{currentHandle.handle.replace(/^@+/, '')}</span>
                                <span className={`handle-status ${currentHandle.status === 'active' ? 'handle-status-active' : ''}`}>
                                    {currentHandle.status.charAt(0).toUpperCase() + currentHandle.status.slice(1)}
                                </span>
                            </div>
                            {currentHandle.payment_type === 'layaway' && (
                                <div className="handle-meta">
                                    Payment: Layaway
                                </div>
                            )}
                            {currentHandle.created_at && (
                                <div className="handle-meta">
                                    Registered: {new Date(currentHandle.created_at).toLocaleDateString()}
                                </div>
                            )}
                            {showingCached && (
                                <div className="handle-cache-indicator">
                                    cached — last synced {formatCachedAt(cachedAt)}
                                </div>
                            )}
                        </div>
                    </div>
                ) : (
                    /* No handle -- claim form */
                    <div className="handle-card">
                        <div className="handle-card-header">
                            <h3>Claim Your Handle</h3>
                            <p className="handle-card-desc">
                                Reserve a unique identity on the Wire network.
                            </p>
                        </div>

                        <div className="handle-card-body">
                            {/* Success message */}
                            {registerSuccess && (
                                <div className="compose-result compose-result-success">
                                    <span>{registerSuccess}</span>
                                </div>
                            )}

                            {/* Check input */}
                            <div className="handle-check">
                                <div className="handle-check-input">
                                    <span className="handle-at">@</span>
                                    <input
                                        type="text"
                                        value={checkInput}
                                        onChange={(e) => {
                                            setCheckInput(e.target.value);
                                            setCheckResult(null);
                                            setCheckError(null);
                                            setRegisterError(null);
                                        }}
                                        onKeyDown={handleKeyDown}
                                        placeholder="yourhandle"
                                        className="handle-input"
                                    />
                                </div>
                                <button
                                    className="handle-check-btn"
                                    onClick={handleCheck}
                                    disabled={checking || !checkInput.trim()}
                                >
                                    {checking ? 'Checking...' : 'Check Availability'}
                                </button>
                            </div>

                            {/* Check result */}
                            {checkError && (
                                <div className="handle-check-result handle-check-error">
                                    {checkError}
                                </div>
                            )}

                            {checkResult && (
                                <div className={`handle-check-result ${checkResult.available ? 'handle-check-available' : 'handle-check-taken'}`}>
                                    {checkResult.available ? (
                                        <>
                                            <div className="handle-available-msg">
                                                @{checkResult.handle} is available!
                                            </div>
                                            {checkResult.cost && (
                                                <div className="handle-cost">
                                                    Cost: {checkResult.cost.toLocaleString()} credits
                                                </div>
                                            )}

                                            {registerError && (
                                                <div className="handle-check-result handle-check-error" style={{ marginTop: '8px' }}>
                                                    {registerError}
                                                </div>
                                            )}

                                            <div className="handle-register-actions">
                                                <button
                                                    className="handle-pay-btn"
                                                    onClick={() => handleRegister('full')}
                                                    disabled={registering}
                                                >
                                                    {registering ? 'Registering...' : 'Pay Now'}
                                                </button>
                                                <button
                                                    className="handle-layaway-btn"
                                                    onClick={() => handleRegister('layaway')}
                                                    disabled={registering}
                                                >
                                                    Start Layaway
                                                </button>
                                            </div>
                                        </>
                                    ) : (
                                        <div className="handle-taken-msg">
                                            @{checkResult.handle} is not available.
                                            {checkResult.reason && <span> {checkResult.reason}</span>}
                                        </div>
                                    )}
                                </div>
                            )}
                        </div>
                    </div>
                )}
            </div>

            {/* Transaction History */}
            <div className="credit-history">
                <h3>Transaction History</h3>
                {loadingTransactions && transactions.length === 0 ? (
                    <div className="handle-loading">
                        <div className="loading-spinner" />
                        <span>Loading transactions...</span>
                    </div>
                ) : transactionError && transactions.length === 0 ? (
                    <div className="handle-error">
                        <span>{transactionError}</span>
                        <button className="handle-retry-btn" onClick={() => fetchTransactions()}>Retry</button>
                    </div>
                ) : transactions.length === 0 ? (
                    <div className="credit-history-list">
                        <div className="credit-history-empty">
                            No transactions yet.
                        </div>
                    </div>
                ) : (
                    <>
                        <div className="transaction-list">
                            {transactions.map((tx) => {
                                const reason = tx.reason || tx.type || 'unknown';
                                const reasonLabel = reason
                                    .replace(/_/g, ' ')
                                    .replace(/\b\w/g, (c: string) => c.toUpperCase());
                                return (
                                    <div key={tx.id} className="transaction-row">
                                        <div className="transaction-amount-col">
                                            <span className={`transaction-amount ${tx.amount >= 0 ? 'transaction-positive' : 'transaction-negative'}`}>
                                                {tx.amount >= 0 ? '+' : ''}{tx.amount.toLocaleString()}
                                            </span>
                                        </div>
                                        <div className="transaction-detail-col">
                                            <span className="transaction-type">{reasonLabel}</span>
                                            {tx.reference_id && (
                                                <span className="transaction-reference" title={tx.reference_id}>
                                                    ref: {tx.reference_id.length > 20 ? tx.reference_id.slice(0, 20) + '...' : tx.reference_id}
                                                </span>
                                            )}
                                            {tx.balance_after != null && (
                                                <span className="transaction-balance">bal: {tx.balance_after.toLocaleString()}</span>
                                            )}
                                        </div>
                                        <div className="transaction-time-col">
                                            <span className="transaction-time">
                                                {new Date(tx.created_at).toLocaleDateString()}
                                            </span>
                                        </div>
                                    </div>
                                );
                            })}
                        </div>
                        {hasMoreTransactions && (
                            <button
                                className="handle-retry-btn"
                                onClick={() => fetchTransactions(transactionOffset + TRANSACTION_LIMIT, true)}
                                disabled={loadingTransactions}
                            >
                                {loadingTransactions ? 'Loading...' : 'Load More'}
                            </button>
                        )}
                    </>
                )}
            </div>
        </div>
    );
}
