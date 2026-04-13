import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useVizConfig } from '../../hooks/useVizConfig';
import { useUnifiedLayout } from './useUnifiedLayout';
import { CanvasRenderer } from './CanvasRenderer';
import { DomRenderer } from './DomRenderer';
import { MiniaturePyramid } from './MiniaturePyramid';
import type { PyramidRenderer } from './PyramidRenderer';
import type {
    SurfaceMode,
    LayoutMode,
    OverlayState,
    SurfaceNode,
    StaleLogEntry,
} from './types';

interface PyramidSurfaceProps {
    slug: string;
    mode?: SurfaceMode;
    staleLog?: StaleLogEntry[];
    onNodeClick?: (nodeId: string) => void;
    onNavigateToSlug?: (slug: string, nodeId: string) => void;
}

interface TreeResponse {
    id: string;
    depth: number;
    headline: string;
    distilled: string;
    self_prompt?: string | null;
    thread_id?: string | null;
    source_path?: string | null;
    children: TreeResponse[];
}

export function PyramidSurface({
    slug,
    mode = 'full',
    staleLog = [],
    onNodeClick,
    onNavigateToSlug,
}: PyramidSurfaceProps) {
    const containerRef = useRef<HTMLDivElement>(null);
    const rendererRef = useRef<PyramidRenderer | null>(null);
    const rafRef = useRef(0);
    const isIdleRef = useRef(false);
    const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    const { config } = useVizConfig(slug);
    const [treeData, setTreeData] = useState<TreeResponse[] | null>(null);
    const [containerSize, setContainerSize] = useState({ width: 0, height: 0 });
    const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
    const [layoutMode, setLayoutMode] = useState<LayoutMode>('pyramid');
    const [overlays, setOverlays] = useState<OverlayState>({
        structure: true,
        web: config.overlays.web_edges,
        staleness: config.overlays.staleness,
        provenance: config.overlays.provenance,
        build: false,
        weightIntensity: config.overlays.weight_intensity,
    });

    // ── Load tree data ──────────────────────────────────────────────
    useEffect(() => {
        invoke<TreeResponse[]>('pyramid_tree', { slug })
            .then(setTreeData)
            .catch(() => setTreeData(null));
    }, [slug]);

    // ── Observe container size ──────────────────────────────────────
    useEffect(() => {
        const el = containerRef.current;
        if (!el) return;
        const ro = new ResizeObserver((entries) => {
            const entry = entries[0];
            if (entry) {
                setContainerSize({
                    width: entry.contentRect.width,
                    height: entry.contentRect.height,
                });
            }
        });
        ro.observe(el);
        return () => ro.disconnect();
    }, []);

    // ── Compute layout ──────────────────────────────────────────────
    const { nodes, edges } = useUnifiedLayout(
        treeData,
        containerSize.width,
        containerSize.height,
        staleLog,
    );

    // ── Miniature mode (for ticker/nested) ──────────────────────────
    const miniatureLayers = useMemo(() => {
        if (!nodes.length) return [];
        const byDepth = new Map<number, number>();
        for (const n of nodes) {
            byDepth.set(n.depth, (byDepth.get(n.depth) ?? 0) + 1);
        }
        return Array.from(byDepth.entries())
            .map(([depth, count]) => ({ depth, count }))
            .sort((a, b) => a.depth - b.depth);
    }, [nodes]);

    // ── Renderer lifecycle ──────────────────────────────────────────
    useEffect(() => {
        const el = containerRef.current;
        if (!el || mode === 'ticker') return;

        // Create renderer based on tier
        const tier = config.rendering.tier === 'auto' ? 'standard' : config.rendering.tier;
        const renderer: PyramidRenderer =
            tier === 'minimal' ? new DomRenderer() : new CanvasRenderer();

        renderer.attach(el);
        rendererRef.current = renderer;

        return () => {
            renderer.destroy();
            rendererRef.current = null;
            cancelAnimationFrame(rafRef.current);
        };
    }, [mode, config.rendering.tier]);

    // ── Resize renderer when container changes ──────────────────────
    useEffect(() => {
        rendererRef.current?.resize(containerSize.width, containerSize.height);
    }, [containerSize]);

    // ── Render loop ─────────────────────────────────────────────────
    useEffect(() => {
        if (mode === 'ticker' || !rendererRef.current) return;

        const draw = () => {
            rendererRef.current?.render(nodes, edges, overlays, hoveredNodeId);
        };

        const loop = () => {
            draw();
            if (!isIdleRef.current) {
                rafRef.current = requestAnimationFrame(loop);
            }
        };

        // Reset idle state on data change
        isIdleRef.current = false;
        if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
        idleTimerRef.current = setTimeout(() => {
            isIdleRef.current = true;
            draw(); // Final frame
        }, 5000);

        rafRef.current = requestAnimationFrame(loop);

        return () => {
            cancelAnimationFrame(rafRef.current);
            if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
        };
    }, [nodes, edges, overlays, hoveredNodeId, mode]);

    // ── Mouse interaction ───────────────────────────────────────────
    const lastMoveRef = useRef(0);
    const handleMouseMove = useCallback(
        (e: React.MouseEvent) => {
            const now = Date.now();
            if (now - lastMoveRef.current < 16) return; // throttle to ~60fps
            lastMoveRef.current = now;

            const renderer = rendererRef.current;
            if (!renderer) return;

            const hit = renderer.hitTest(e.clientX, e.clientY, nodes);
            setHoveredNodeId(hit?.nodeId ?? null);

            // Wake from idle
            if (isIdleRef.current) {
                isIdleRef.current = false;
                if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
                idleTimerRef.current = setTimeout(() => {
                    isIdleRef.current = true;
                }, 5000);
                rafRef.current = requestAnimationFrame(function loop() {
                    rendererRef.current?.render(nodes, edges, overlays, hoveredNodeId);
                    if (!isIdleRef.current) rafRef.current = requestAnimationFrame(loop);
                });
            }
        },
        [nodes, edges, overlays, hoveredNodeId],
    );

    const handleClick = useCallback(
        (e: React.MouseEvent) => {
            const renderer = rendererRef.current;
            if (!renderer) return;
            const hit = renderer.hitTest(e.clientX, e.clientY, nodes);
            if (hit) {
                // Check for cross-slug navigation
                if (hit.nodeId.includes('/') && onNavigateToSlug) {
                    const parts = hit.nodeId.split('/');
                    onNavigateToSlug(parts[0], parts.slice(1).join('/'));
                } else {
                    onNodeClick?.(hit.nodeId);
                }
            }
        },
        [nodes, onNodeClick, onNavigateToSlug],
    );

    const handleMouseLeave = useCallback(() => {
        setHoveredNodeId(null);
    }, []);

    // ── Overlay toggle handler ──────────────────────────────────────
    const toggleOverlay = useCallback((key: keyof OverlayState) => {
        setOverlays((prev) => ({ ...prev, [key]: !prev[key] }));
    }, []);

    // ── Ticker mode: render miniature only ──────────────────────────
    if (mode === 'ticker') {
        return (
            <div className="ps-ticker" ref={containerRef}>
                <MiniaturePyramid
                    layers={miniatureLayers}
                    maxDotsPerLayer={config.rendering.max_dots_per_layer}
                    width={120}
                    height={40}
                    forceAllNodes={config.rendering.force_all_nodes}
                />
            </div>
        );
    }

    // ── Nested mode: compact with "Open" button ─────────────────────
    if (mode === 'nested') {
        return (
            <div className="ps-nested" ref={containerRef}>
                <MiniaturePyramid
                    layers={miniatureLayers}
                    maxDotsPerLayer={config.rendering.max_dots_per_layer}
                    width={200}
                    height={80}
                    forceAllNodes={config.rendering.force_all_nodes}
                />
                <button className="ps-open-btn" title="Open Pyramid">
                    Open
                </button>
            </div>
        );
    }

    // ── Full mode ───────────────────────────────────────────────────
    return (
        <div className="ps-full">
            {/* Toolbar */}
            <div className="ps-toolbar">
                <div className="ps-layout-toggle">
                    <button
                        className={`ps-toggle-btn ${layoutMode === 'pyramid' ? 'active' : ''}`}
                        onClick={() => setLayoutMode('pyramid')}
                    >
                        Pyramid
                    </button>
                    <button
                        className={`ps-toggle-btn ${layoutMode === 'density' ? 'active' : ''}`}
                        onClick={() => setLayoutMode('density')}
                    >
                        Density
                    </button>
                </div>
                <div className="ps-overlay-toggles">
                    {(['structure', 'web', 'staleness', 'provenance', 'weightIntensity'] as const).map((key) => (
                        <button
                            key={key}
                            className={`ps-overlay-btn ${overlays[key] ? 'active' : ''}`}
                            onClick={() => toggleOverlay(key)}
                            title={key}
                        >
                            {key === 'weightIntensity' ? 'Weight' : key.charAt(0).toUpperCase() + key.slice(1)}
                        </button>
                    ))}
                </div>
            </div>

            {/* Canvas container — tooltip lives here so node coords align */}
            <div
                className="ps-canvas-container"
                ref={containerRef}
                onMouseMove={handleMouseMove}
                onClick={handleClick}
                onMouseLeave={handleMouseLeave}
            >
                {/* Tooltip */}
                {hoveredNodeId && (() => {
                    const node = nodes.find((n) => n.id === hoveredNodeId);
                    if (!node) return null;
                    return (
                        <div
                            className="ps-tooltip"
                            style={{
                                left: node.x + 12,
                                top: node.y - 8,
                            }}
                        >
                            <div className="ps-tooltip-headline">{node.headline}</div>
                            <div className="ps-tooltip-meta">
                                L{node.depth} · {node.id}
                            </div>
                            {node.distilled && (
                                <div className="ps-tooltip-distilled">
                                    {node.distilled.slice(0, 120)}
                                    {node.distilled.length > 120 ? '...' : ''}
                                </div>
                            )}
                        </div>
                    );
                })()}
            </div>
        </div>
    );
}
