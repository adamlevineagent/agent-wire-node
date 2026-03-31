import { useState, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AddWorkspace } from './AddWorkspace';
import { BuildProgress } from './BuildProgress';

interface PyramidConfigInfo {
    api_key_set: boolean;
    auth_token_set: boolean;
    primary_model: string;
    fallback_model_1: string;
    fallback_model_2: string;
}

interface PyramidFirstRunProps {
    onComplete: () => void;
}

type WizardStep = 'api-key' | 'workspace' | 'done';

export function PyramidFirstRun({ onComplete }: PyramidFirstRunProps) {
    const [step, setStep] = useState<WizardStep>('api-key');
    const [apiKey, setApiKey] = useState('');
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);

    const handleSaveApiKey = useCallback(async () => {
        if (!apiKey.trim()) {
            setError('Please enter an API key');
            return;
        }
        setSaving(true);
        setError(null);
        try {
            await invoke('pyramid_set_config', {
                apiKey: apiKey.trim(),
                authToken: '',
            });
            setStep('workspace');
        } catch (err) {
            setError(String(err));
        } finally {
            setSaving(false);
        }
    }, [apiKey]);

    const handleWorkspaceComplete = useCallback(() => {
        setStep('done');
    }, []);

    const handleSkipWorkspace = useCallback(() => {
        onComplete();
    }, [onComplete]);

    return (
        <div className="first-run-overlay">
            <div className="first-run-panel">
                {step === 'api-key' && (
                    <div className="first-run-step">
                        <div className="first-run-logo">W</div>
                        <h1>Build Your First Knowledge Pyramid</h1>
                        <p className="first-run-subtitle">
                            A knowledge pyramid distills a codebase or document folder
                            into layered, queryable intelligence. To get started, you need
                            an OpenRouter API key for the LLM that powers the build.
                        </p>

                        {error && (
                            <div className="first-run-error">{error}</div>
                        )}

                        <div className="first-run-field">
                            <input
                                type="password"
                                className="settings-input"
                                value={apiKey}
                                onChange={(e) => setApiKey(e.target.value)}
                                placeholder="sk-or-..."
                                onKeyDown={(e) => e.key === 'Enter' && handleSaveApiKey()}
                            />
                            <span className="first-run-hint">
                                Get a key at openrouter.ai -- any model with 100k+ context works.
                            </span>
                        </div>

                        <div className="first-run-actions">
                            <button
                                className="btn btn-primary btn-lg"
                                onClick={handleSaveApiKey}
                                disabled={saving}
                            >
                                {saving ? 'Saving...' : 'Save Key & Pick a Folder'}
                            </button>
                            <button
                                className="btn btn-ghost"
                                onClick={onComplete}
                            >
                                Skip for now
                            </button>
                        </div>
                    </div>
                )}

                {step === 'workspace' && (
                    <div className="first-run-step">
                        <h2>Link a Folder</h2>
                        <p className="first-run-subtitle">
                            Pick a project directory or document folder. Wire Node will
                            create a corpus from its contents and build a knowledge pyramid
                            automatically.
                        </p>
                        <AddWorkspace
                            onComplete={handleWorkspaceComplete}
                            onCancel={handleSkipWorkspace}
                        />
                    </div>
                )}

                {step === 'done' && (
                    <div className="first-run-step">
                        <div className="first-run-logo complete">&#x2713;</div>
                        <h1>Your First Build is Starting</h1>
                        <p className="first-run-subtitle">
                            Your workspace is linked and a pyramid build has been queued.
                            Head to the dashboard to watch progress and explore results
                            as layers complete.
                        </p>
                        <div className="first-run-actions">
                            <button
                                className="btn btn-primary btn-lg"
                                onClick={onComplete}
                            >
                                Go to Dashboard
                            </button>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
}
