/**
 * Dedicated window for the Pyramid Surface.
 * Rendered when the Tauri window URL contains ?window=pyramid-surface.
 *
 * If a slug is provided, shows the full pyramid view.
 * If no slug, shows the Grid View (mission control).
 */

import { useState, useCallback, useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { PyramidSurface } from './PyramidSurface';
import { GridView } from './GridView';
import { NodeInspectorModal } from '../theatre/NodeInspectorModal';
import { useVizConfig } from '../../hooks/useVizConfig';
import type { LiveNodeInfo } from '../theatre/types';

interface PyramidSurfaceWindowProps {
    slug?: string;
}

export function PyramidSurfaceWindow({ slug: initialSlug }: PyramidSurfaceWindowProps) {
    const [currentSlug, setCurrentSlug] = useState<string | undefined>(initialSlug);
    const [inspectedNodeId, setInspectedNodeId] = useState<string | null>(null);
    const [allNodes, setAllNodes] = useState<LiveNodeInfo[]>([]);
    const { config } = useVizConfig(currentSlug);

    // Load node data for the inspector panel whenever slug changes or a node is opened.
    // The live-node read model now includes question nodes when available, so this keeps
    // q-* navigation in sync without making the surface modal depend on renderer internals.
    useEffect(() => {
        if (!currentSlug) {
            setAllNodes([]);
            return;
        }
        invoke<LiveNodeInfo[]>('pyramid_build_live_nodes', { slug: currentSlug })
            .then(setAllNodes)
            .catch(() => setAllNodes([]));
    }, [currentSlug, inspectedNodeId]);

    // Show the window once React is ready
    useEffect(() => {
        getCurrentWindow().show();
    }, []);

    // Navigate to a specific pyramid
    const handleSelectPyramid = useCallback((slug: string) => {
        setCurrentSlug(slug);
        setInspectedNodeId(null);
    }, []);

    // Navigate back to grid
    const handleBackToGrid = useCallback(() => {
        setCurrentSlug(undefined);
        setInspectedNodeId(null);
    }, []);

    // Handle node click — open the inspector panel
    const handleNodeClick = useCallback((nodeId: string) => {
        setInspectedNodeId(nodeId);
    }, []);

    // Handle inspector navigation (arrow keys between nodes)
    const handleInspectorNavigate = useCallback((nodeId: string) => {
        setInspectedNodeId(nodeId);
    }, []);

    // Close inspector
    const handleCloseInspector = useCallback(() => {
        setInspectedNodeId(null);
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
            <div className="ps-window-body">
                <PyramidSurface
                    slug={currentSlug}
                    mode="full"
                    onNodeClick={handleNodeClick}
                />
                {inspectedNodeId && currentSlug && (
                    <NodeInspectorModal
                        slug={currentSlug}
                        nodeId={inspectedNodeId}
                        allNodes={allNodes}
                        onClose={handleCloseInspector}
                        onNavigate={handleInspectorNavigate}
                    />
                )}
            </div>
        </div>
    );
}
