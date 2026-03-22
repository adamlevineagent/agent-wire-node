import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface PyramidConfigInfo {
    api_key_set: boolean;
    auth_token_set: boolean;
    primary_model: string;
    fallback_model_1: string;
    fallback_model_2: string;
}

export function PyramidSettings() {
    const [apiKey, setApiKey] = useState('');
    const [authToken, setAuthToken] = useState('');
    const [vibesmithyUrl, setVibesmithyUrl] = useState('http://localhost:3333');
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
        } catch (err) {
            console.error('Failed to fetch pyramid config:', err);
        }
    }, []);

    useEffect(() => {
        fetchConfig();
    }, [fetchConfig]);

    const handleSave = useCallback(async () => {
        setSaving(true);
        setError(null);
        try {
            await invoke('pyramid_set_config', {
                apiKey: apiKey || '',
                authToken: authToken || '',
            });
            setSaved(true);
            setTimeout(() => setSaved(false), 2000);
            await fetchConfig();
        } catch (err) {
            setError(String(err));
        } finally {
            setSaving(false);
        }
    }, [apiKey, authToken, fetchConfig]);

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
                authToken: authToken || '',
            });
            // Try a minimal API call by listing models
            const resp = await fetch('https://openrouter.ai/api/v1/models', {
                headers: { Authorization: `Bearer ${apiKey}` },
            });
            if (resp.ok) {
                setTestResult('API key is valid');
                await fetchConfig();
            } else {
                setTestResult(`API key test failed: ${resp.status} ${resp.statusText}`);
            }
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

            <div className="settings-section">
                <div className="settings-section-header">Vibesmithy URL</div>
                <p className="settings-section-desc">
                    The URL where your Vibesmithy client is running.
                </p>
                <input
                    type="text"
                    className="settings-input"
                    value={vibesmithyUrl}
                    onChange={(e) => setVibesmithyUrl(e.target.value)}
                    placeholder="http://localhost:3333"
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
