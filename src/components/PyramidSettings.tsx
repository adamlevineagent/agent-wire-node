import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../contexts/AppContext';

interface PyramidConfigInfo {
    api_key_set: boolean;
    auth_token_set: boolean;
    primary_model: string;
    fallback_model_1: string;
    fallback_model_2: string;
    auto_execute: boolean;
}

export function PyramidSettings() {
    const { state, dispatch } = useAppContext();
    const [apiKey, setApiKey] = useState('');
    const [authToken, setAuthToken] = useState('');
    const [primaryModel, setPrimaryModel] = useState('');
    const [autoExecute, setAutoExecute] = useState(false);
    const [configInfo, setConfigInfo] = useState<PyramidConfigInfo | null>(null);
    const [saving, setSaving] = useState(false);
    const [saved, setSaved] = useState(false);
    const [testing, setTesting] = useState(false);
    const [testResult, setTestResult] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);

    const fetchConfig = useCallback(async () => {
        try {
            const info = await invoke<PyramidConfigInfo>('pyramid_get_config');
            setConfigInfo(info);
            setAutoExecute(info.auto_execute);
            dispatch({ type: 'SET_AUTO_EXECUTE', enabled: info.auto_execute });
        } catch (err) {
            console.error('Failed to fetch pyramid config:', err);
        }
    }, [dispatch]);

    useEffect(() => {
        fetchConfig();
    }, [fetchConfig]);

    const handleSave = useCallback(async () => {
        setSaving(true);
        setError(null);
        try {
            await invoke('pyramid_set_config', {
                ...(apiKey ? { apiKey } : {}),
                ...(authToken ? { authToken } : {}),
                ...(primaryModel ? { primaryModel } : {}),
                autoExecute,
            });
            setSaved(true);
            setTimeout(() => setSaved(false), 2000);
            await fetchConfig();
            setPrimaryModel(''); // Clear the override field after save
        } catch (err) {
            setError(String(err));
        } finally {
            setSaving(false);
        }
    }, [apiKey, authToken, primaryModel, fetchConfig]);

    const handleTestApiKey = useCallback(async () => {
        if (!apiKey) {
            setTestResult('Enter an API key first');
            return;
        }
        setTesting(true);
        setTestResult(null);
        try {
            // Save first so the key is available
            await invoke('pyramid_set_config', {
                apiKey,
                ...(authToken ? { authToken } : {}),
            });
            // Test via IPC — key stays server-side
            const result = await invoke<string>('pyramid_test_api_key');
            setTestResult(result);
            await fetchConfig();
        } catch (err) {
            setTestResult(`Test failed: ${err}`);
        } finally {
            setTesting(false);
        }
    }, [apiKey, authToken, fetchConfig]);

    return (
        <div className="pyramid-settings">
            <div className="settings-section">
                <div className="settings-section-header">Pyramid Engine</div>
                <p className="settings-section-desc">
                    Configure the Knowledge Pyramid engine. An OpenRouter API key is required
                    to build pyramids from your workspace content.
                </p>

                {configInfo && (
                    <div className="pyramid-config-status">
                        <div className={`config-status-item ${configInfo.api_key_set ? 'set' : 'unset'}`}>
                            <span className="config-status-indicator">
                                {configInfo.api_key_set ? '[OK]' : '[!!]'}
                            </span>
                            <span>
                                OpenRouter API Key: {configInfo.api_key_set ? 'Configured' : 'Not set'}
                            </span>
                        </div>
                        <div className={`config-status-item ${configInfo.auth_token_set ? 'set' : 'unset'}`}>
                            <span className="config-status-indicator">
                                {configInfo.auth_token_set ? '[OK]' : '[--]'}
                            </span>
                            <span>
                                Auth Token: {configInfo.auth_token_set ? 'Configured' : 'Not set (optional)'}
                            </span>
                        </div>
                        <div className="config-status-item set">
                            <span className="config-status-indicator">[&gt;&gt;]</span>
                            <span>Primary model: {configInfo.primary_model}</span>
                        </div>
                    </div>
                )}
            </div>

            <div className="settings-section">
                <div className="settings-section-header">Primary Model</div>
                <p className="settings-section-desc">
                    OpenRouter model slug for the planner and pyramid builder.
                    Browse models at{' '}
                    <a href="https://openrouter.ai/models" target="_blank" rel="noreferrer"
                       style={{ color: 'var(--accent-primary, #6366f1)' }}>
                        openrouter.ai/models
                    </a>.
                </p>
                <div className="settings-field-row">
                    <input
                        type="text"
                        className="settings-input"
                        value={primaryModel}
                        onChange={(e) => setPrimaryModel(e.target.value)}
                        placeholder={configInfo?.primary_model ?? 'inception/mercury-2'}
                    />
                    <button
                        className="btn btn-secondary"
                        onClick={() => {
                            setPrimaryModel('inception/mercury-2');
                        }}
                        title="Reset to default model"
                    >
                        Reset
                    </button>
                </div>
                {primaryModel && primaryModel !== configInfo?.primary_model && (
                    <div style={{ fontSize: '12px', color: 'var(--accent-warning, #f59e0b)', marginTop: '4px' }}>
                        Will change from {configInfo?.primary_model} → {primaryModel} on save
                    </div>
                )}
            </div>

            <div className="settings-section">
                <div className="settings-section-header">Auto-Execute</div>
                <p className="settings-section-desc">
                    When enabled, safe plans (navigation, read-only queries) execute immediately
                    without showing a preview. Plans with costs or side effects always show a preview
                    for your approval regardless of this setting.
                </p>
                <label style={{ display: 'flex', alignItems: 'center', gap: '8px', cursor: 'pointer' }}>
                    <input
                        type="checkbox"
                        checked={autoExecute}
                        onChange={(e) => setAutoExecute(e.target.checked)}
                    />
                    <span style={{ fontSize: '14px', color: 'var(--text-primary, #e0e0e0)' }}>
                        Auto-execute safe plans
                    </span>
                </label>
            </div>

            <div className="settings-section">
                <div className="settings-section-header">OpenRouter API Key</div>
                <p className="settings-section-desc">
                    Used for LLM calls during pyramid building. Get a key at openrouter.ai.
                </p>
                <div className="settings-field-row">
                    <input
                        type="password"
                        className="settings-input"
                        value={apiKey}
                        onChange={(e) => setApiKey(e.target.value)}
                        placeholder={configInfo?.api_key_set ? '(key already set - enter new to replace)' : 'sk-or-...'}
                    />
                    <button
                        className="btn btn-secondary"
                        onClick={handleTestApiKey}
                        disabled={testing}
                    >
                        {testing ? 'Testing...' : 'Test Key'}
                    </button>
                </div>
                {testResult && (
                    <div className={`test-result ${testResult.includes('valid') ? 'success' : 'error'}`}>
                        {testResult}
                    </div>
                )}
            </div>

            <div className="settings-section">
                <div className="settings-section-header">Auth Token</div>
                <p className="settings-section-desc">
                    Token for Vibesmithy connection (optional). Used to authenticate pyramid
                    API requests from the Vibesmithy client.
                </p>
                <input
                    type="password"
                    className="settings-input"
                    value={authToken}
                    onChange={(e) => setAuthToken(e.target.value)}
                    placeholder={configInfo?.auth_token_set ? '(token already set - enter new to replace)' : 'Enter auth token'}
                />
            </div>

            {error && (
                <div className="pyramid-error">{error}</div>
            )}

            <button
                className={`save-btn ${saved ? 'save-success' : ''}`}
                onClick={handleSave}
                disabled={saving}
            >
                {saved ? 'Saved' : saving ? 'Saving...' : 'Save Pyramid Settings'}
            </button>
        </div>
    );
}
