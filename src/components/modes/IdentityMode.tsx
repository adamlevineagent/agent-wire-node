import { useState, useEffect, useCallback } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// --- Types ---

interface HandleInfo {
    handle: string;
    status: 'active' | 'suspended' | 'released';
    layaway_active: boolean;
    registered_at?: string;
    layaway_progress?: number;
    layaway_paid?: number;
    layaway_total?: number;
}

interface HandleCheckResult {
    available: boolean;
    handle: string;
    cost?: number;
    reason?: string;
}

// --- Component ---

export function IdentityMode() {
    const { state, operatorApiCall } = useAppContext();

    // Handle state
    const [myHandles, setMyHandles] = useState<HandleInfo[]>([]);
    const [loadingHandles, setLoadingHandles] = useState(true);
    const [handleError, setHandleError] = useState<string | null>(null);

    // Handle check state
    const [checkInput, setCheckInput] = useState('');
    const [checking, setChecking] = useState(false);
    const [checkResult, setCheckResult] = useState<HandleCheckResult | null>(null);
    const [checkError, setCheckError] = useState<string | null>(null);

    // Handle registration state
    const [registering, setRegistering] = useState(false);
    const [registerError, setRegisterError] = useState<string | null>(null);
    const [registerSuccess, setRegisterSuccess] = useState<string | null>(null);

    const creditBalance = state.creditBalance > 0
        ? state.creditBalance
        : (state.credits?.credits_earned ?? 0);

    // Fetch handles
    const fetchHandles = useCallback(async () => {
        if (!state.operatorSessionToken) {
            setLoadingHandles(false);
            return;
        }
        try {
            const data: any = await operatorApiCall('GET', '/api/v1/wire/handles');
            const handles = data?.handles || data || [];
            setMyHandles(Array.isArray(handles) ? handles : []);
            setHandleError(null);
        } catch (err: any) {
            setHandleError(err?.message || 'Failed to load handles');
        } finally {
            setLoadingHandles(false);
        }
    }, [operatorApiCall, state.operatorSessionToken]);

    useEffect(() => {
        fetchHandles();
    }, [fetchHandles]);

    // Check handle availability
    const handleCheck = useCallback(async () => {
        const handle = checkInput.trim().replace(/^@/, '');
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
        const normalized = raw.replace(/^@/, '');
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

    const activeHandle = myHandles.find(h => h.status === 'active' && !h.layaway_active);
    const layawayHandle = myHandles.find(h => h.layaway_active);
    const currentHandle = activeHandle || layawayHandle;

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
                    <div className="identity-info-card">
                        <span className="identity-label">Credit Balance</span>
                        <span className="identity-value glow">{Math.floor(creditBalance).toLocaleString()}</span>
                    </div>
                    <div className="identity-info-card">
                        <span className="identity-label">Reputation</span>
                        <span className="identity-value">\u2014</span>
                    </div>
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
                            <h3>Your Handle</h3>
                        </div>

                        {currentHandle.status === 'active' && !currentHandle.layaway_active ? (
                            <div className="handle-card-body">
                                <div className="handle-display">
                                    <span className="handle-name">@{currentHandle.handle.replace(/^@/, '')}</span>
                                    <span className="handle-status handle-status-active">Active</span>
                                </div>
                                {currentHandle.registered_at && (
                                    <div className="handle-meta">
                                        Registered: {new Date(currentHandle.registered_at).toLocaleDateString()}
                                    </div>
                                )}
                            </div>
                        ) : currentHandle.layaway_active ? (
                            <div className="handle-card-body">
                                <div className="handle-display">
                                    <span className="handle-name">@{currentHandle.handle.replace(/^@/, '')}</span>
                                    <span className="handle-status handle-status-layaway">Layaway</span>
                                </div>
                                <div className="layaway-progress">
                                    <div className="layaway-bar">
                                        <div
                                            className="layaway-fill"
                                            style={{ width: `${currentHandle.layaway_progress || 0}%` }}
                                        />
                                    </div>
                                    <div className="layaway-info">
                                        <span className="layaway-pct">
                                            {Math.round(currentHandle.layaway_progress || 0)}%
                                        </span>
                                        <span className="layaway-amounts">
                                            {(currentHandle.layaway_paid || 0).toLocaleString()} / {(currentHandle.layaway_total || 0).toLocaleString()} credits
                                        </span>
                                    </div>
                                </div>
                                <div className="handle-meta">
                                    Progress updates appear in your Activity feed.
                                </div>
                            </div>
                        ) : (
                            <div className="handle-card-body">
                                <div className="handle-display">
                                    <span className="handle-name">@{currentHandle.handle.replace(/^@/, '')}</span>
                                    <span className="handle-status">{currentHandle.status}</span>
                                </div>
                            </div>
                        )}
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

            {/* Credit History */}
            <div className="credit-history">
                <h3>Credit History</h3>
                <div className="credit-history-summary">
                    <div className="credit-history-balance">
                        <span className="credit-history-label">Current Balance</span>
                        <span className="credit-history-value glow">
                            {Math.floor(creditBalance).toLocaleString()}
                        </span>
                    </div>
                    <div className="credit-history-earned">
                        <span className="credit-history-label">Total Earned</span>
                        <span className="credit-history-value">
                            {Math.floor(state.credits?.credits_earned ?? 0).toLocaleString()}
                        </span>
                    </div>
                </div>
                <div className="credit-history-list">
                    <div className="credit-history-empty">
                        Detailed transaction history coming soon.
                    </div>
                </div>
            </div>
        </div>
    );
}
