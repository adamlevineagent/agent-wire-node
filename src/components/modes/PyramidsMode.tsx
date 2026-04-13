import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PyramidDashboard } from '../PyramidDashboard';
import { PyramidFirstRun } from '../PyramidFirstRun';
import { CrossPyramidTimeline } from '../CrossPyramidTimeline';
import { DadbearOversightPage } from '../DadbearOversightPage';
import { GridView } from '../pyramid-surface/GridView';
import { SlugInfo, PyramidConfigInfo } from '../pyramid-types';

type PyramidsTab = 'dashboard' | 'grid' | 'builds' | 'oversight';

/** Default max dots per layer — matches useVizConfig default */
const GRID_MAX_DOTS_PER_LAYER = 10;

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

    // When a card is clicked in the grid, switch to dashboard tab
    // (PyramidDashboard manages its own selected slug internally)
    const handleGridSelectPyramid = useCallback((_slug: string) => {
        setTab('dashboard');
    }, []);

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
                    className={`pyramids-mode-tab ${tab === 'grid' ? 'pyramids-mode-tab-active' : ''}`}
                    onClick={() => setTab('grid')}
                >
                    Grid
                </button>
                <button
                    className={`pyramids-mode-tab ${tab === 'builds' ? 'pyramids-mode-tab-active' : ''}`}
                    onClick={() => setTab('builds')}
                >
                    Builds
                </button>
                <button
                    className={`pyramids-mode-tab ${tab === 'oversight' ? 'pyramids-mode-tab-active' : ''}`}
                    onClick={() => setTab('oversight')}
                >
                    Oversight
                </button>
            </div>
            {tab === 'dashboard' && <PyramidDashboard />}
            {tab === 'grid' && (
                <GridView
                    onSelectPyramid={handleGridSelectPyramid}
                    maxDotsPerLayer={GRID_MAX_DOTS_PER_LAYER}
                />
            )}
            {tab === 'builds' && <CrossPyramidTimeline />}
            {tab === 'oversight' && <DadbearOversightPage />}
        </div>
    );
}
