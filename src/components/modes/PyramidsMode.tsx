import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PyramidDashboard } from '../PyramidDashboard';
import { PyramidFirstRun } from '../PyramidFirstRun';
import { CrossPyramidTimeline } from '../CrossPyramidTimeline';
import { SlugInfo, PyramidConfigInfo } from '../pyramid-types';

type PyramidsTab = 'dashboard' | 'builds';

export function PyramidsMode() {
    const [showFirstRun, setShowFirstRun] = useState(false);
    const [checking, setChecking] = useState(true);
    const [tab, setTab] = useState<PyramidsTab>('dashboard');

    useEffect(() => {
        (async () => {
            try {
                const [slugs, config] = await Promise.all([
                    invoke<SlugInfo[]>('pyramid_list_slugs'),
                    invoke<PyramidConfigInfo>('pyramid_get_config'),
                ]);
                // Show first-run if no slugs AND no API key configured
                if (slugs.length === 0 && !config.api_key_set) {
                    setShowFirstRun(true);
                }
            } catch {
                // If commands fail, just show the dashboard
            } finally {
                setChecking(false);
            }
        })();
    }, []);

    if (checking) {
        return (
            <div className="mode-container">
                <div className="pyramid-loading">Loading...</div>
            </div>
        );
    }

    if (showFirstRun) {
        return <PyramidFirstRun onComplete={() => setShowFirstRun(false)} />;
    }

    return (
        <div className="mode-container">
            <div className="pyramids-mode-tabs">
                <button
                    className={`pyramids-mode-tab ${tab === 'dashboard' ? 'pyramids-mode-tab-active' : ''}`}
                    onClick={() => setTab('dashboard')}
                >
                    Dashboard
                </button>
                <button
                    className={`pyramids-mode-tab ${tab === 'builds' ? 'pyramids-mode-tab-active' : ''}`}
                    onClick={() => setTab('builds')}
                >
                    Builds
                </button>
            </div>
            {tab === 'dashboard' && <PyramidDashboard />}
            {tab === 'builds' && <CrossPyramidTimeline />}
        </div>
    );
}
