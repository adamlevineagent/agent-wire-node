import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PyramidDashboard } from '../PyramidDashboard';
import { PyramidFirstRun } from '../PyramidFirstRun';
import { CrossPyramidTimeline } from '../CrossPyramidTimeline';
import { DadbearOversightPage } from '../DadbearOversightPage';
import { GridView } from '../pyramid-surface/GridView';
import { usePyramidWindow } from '../../hooks/usePyramidWindow';
import { SlugInfo, PyramidConfigInfo } from '../pyramid-types';

type PyramidsTab = 'dashboard' | 'grid' | 'builds' | 'oversight';

/** Default max dots per layer — matches useVizConfig default */
const GRID_MAX_DOTS_PER_LAYER = 10;

export function PyramidsMode() {
    const [showFirstRun, setShowFirstRun] = useState(false);
    const [checking, setChecking] = useState(true);
    const [tab, setTab] = useState<PyramidsTab>('dashboard');

    const { openWindow } = usePyramidWindow();

    // When a card is clicked in the grid, open the pyramid surface window
    const handleGridSelectPyramid = useCallback((slug: string) => {
        openWindow(slug);
    }, [openWindow]);

    useEffect(() => {
        (async () => {
            try {
                const [slugs, config] = await Promise.all([
                    invoke<SlugInfo[]>('pyramid_list_slugs'),
                    invoke<PyramidConfigInfo>('pyramid_get_config'),
                ]);
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
            {/*
             * Builds tab stays mounted across tab switches — the hook
             * behind CrossPyramidTimeline owns a live `cross-build-event`
             * subscription and a 30s poll, both of which would be torn
             * down and restarted on every mount. The component paints
             * nothing when hidden, and the hook's work continues in the
             * background so when the user comes back the list is
             * already current rather than empty-then-idle.
             */}
            <div style={{ display: tab === 'builds' ? 'contents' : 'none' }}>
                <CrossPyramidTimeline />
            </div>
            {tab === 'oversight' && <DadbearOversightPage />}
        </div>
    );
}
