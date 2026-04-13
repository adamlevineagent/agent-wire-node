/**
 * Dedicated window for the Pyramid Surface.
 * Rendered when the Tauri window URL contains ?window=pyramid-surface.
 *
 * If a slug is provided, shows the full pyramid view.
 * If no slug, shows the Grid View (mission control).
 */

import { useState, useCallback, useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PyramidSurface } from './PyramidSurface';
import { GridView } from './GridView';
import { useVizConfig } from '../../hooks/useVizConfig';

interface PyramidSurfaceWindowProps {
    slug?: string;
}

export function PyramidSurfaceWindow({ slug: initialSlug }: PyramidSurfaceWindowProps) {
    const [currentSlug, setCurrentSlug] = useState<string | undefined>(initialSlug);
    const { config } = useVizConfig(currentSlug);

    // Show the window once React is ready
    useEffect(() => {
        getCurrentWindow().show();
    }, []);

    // Navigate to a specific pyramid
    const handleSelectPyramid = useCallback((slug: string) => {
        setCurrentSlug(slug);
    }, []);

    // Navigate back to grid
    const handleBackToGrid = useCallback(() => {
        setCurrentSlug(undefined);
    }, []);

    // Handle node click — for now, just log (inspector integration comes later)
    const handleNodeClick = useCallback((nodeId: string) => {
        console.log('Node clicked in pyramid window:', nodeId);
        // TODO: Open NodeInspectorPanel in this window
    }, []);

    // Grid View (no slug selected)
    if (!currentSlug) {
        return (
            <div className="ps-window">
                <div className="ps-window-header">
                    <h2 className="ps-window-title">Pyramid Surface</h2>
                </div>
                <GridView
                    onSelectPyramid={handleSelectPyramid}
                    maxDotsPerLayer={config.rendering.max_dots_per_layer}
                />
            </div>
        );
    }

    // Full pyramid view
    return (
        <div className="ps-window">
            <div className="ps-window-header">
                <button className="ps-window-back" onClick={handleBackToGrid}>
                    &larr; All Pyramids
                </button>
                <h2 className="ps-window-title">{currentSlug}</h2>
            </div>
            <PyramidSurface
                slug={currentSlug}
                mode="full"
                onNodeClick={handleNodeClick}
            />
        </div>
    );
}
