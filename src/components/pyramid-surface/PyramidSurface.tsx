import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useVizConfig } from '../../hooks/useVizConfig';
import { usePyramidWindow } from '../../hooks/usePyramidWindow';
import { usePyramidData } from './usePyramidData';
import { useVisualEncoding } from './useVisualEncoding';
import { useChronicleStream } from './useChronicleStream';
import { useVizMapping } from './useVizMapping';
import { CanvasRenderer } from './CanvasRenderer';
import { DomRenderer } from './DomRenderer';
import { GpuRenderer } from './GpuRenderer';
import { MiniaturePyramid } from './MiniaturePyramid';
import { Chronicle } from './Chronicle';
import { EventTicker } from './EventTicker';
import { Minimap } from './Minimap';
import type { PyramidRenderer } from './PyramidRenderer';
import type {
    SurfaceMode,
    LayoutMode,
    OverlayState,
    StaleLogEntry,
} from './types';

interface PyramidSurfaceProps {
    slug: string;
    mode?: SurfaceMode;
    staleLog?: StaleLogEntry[];
    onNodeClick?: (nodeId: string) => void;
    onNavigateToSlug?: (slug: string, nodeId: string) => void;
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
    const { openWindow } = usePyramidWindow();
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

    // ── Chronicle event stream (Phase 4) ─────────────────────────────
    const { entries: chronicleEntries, generation: chronicleGen, clear: _clearChronicle } = useChronicleStream(slug);
    const [chronicleOpen, setChronicleOpen] = useState(false);

    // ── Unified data: static tree + build progress + event bus ───────
    const {
        nodes,
        edges,
        isBuilding,
        currentStep,
        buildProgress: buildProg,
        buildVizState,
        loading,
    } = usePyramidData(slug, containerSize.width, containerSize.height, staleLog);

    // ── Viz-from-YAML: map current step to viz primitive (AD-1) ─────
    const { getVizPrimitive } = useVizMapping(slug, isBuilding);
    const activeVizPrimitive = isBuilding && currentStep
        ? getVizPrimitive(currentStep)
        : null;

    // Apply viz primitive + build viz state to renderer
    useEffect(() => {
        if (rendererRef.current) {
            rendererRef.current.setActiveVizPrimitive(activeVizPrimitive);
            rendererRef.current.setBuildVizState(buildVizState);
        }
    }, [activeVizPrimitive, buildVizState]);

    // ── Visual encoding (three-axis: brightness, saturation, border) ──
    const { encodings: visualEncodings, linkIntensities } = useVisualEncoding(
        slug,
        overlays.weightIntensity && !isBuilding,
    );

    // Apply encodings + link intensities to renderer when they change
    useEffect(() => {
        if (rendererRef.current) {
            if (visualEncodings.size > 0) rendererRef.current.setNodeEncodings(visualEncodings);
            if (linkIntensities.size > 0) rendererRef.current.setLinkIntensities(linkIntensities);
        }
    }, [visualEncodings, linkIntensities]);

    // Auto-enable build overlay and open chronicle when building
    useEffect(() => {
        if (isBuilding) {
            setOverlays((prev) => ({ ...prev, build: true }));
            setChronicleOpen(true);
        }
    }, [isBuilding]);

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

        // Create renderer based on tier with auto-detection
        let detectedTier = config.rendering.tier;
        if (detectedTier === 'auto') {
            // Auto-detect: try WebGL2, fall back to Canvas 2D
            const testCanvas = document.createElement('canvas');
            detectedTier = testCanvas.getContext('webgl2') ? 'rich' : 'standard';
        }

        let renderer: PyramidRenderer;
        if (detectedTier === 'minimal') {
            renderer = new DomRenderer();
            renderer.attach(el);
        } else if (detectedTier === 'rich') {
            try {
                const gpu = new GpuRenderer();
                gpu.attach(el); // attach() throws if WebGL2 unavailable
                renderer = gpu;
            } catch {
                // WebGL2 not available — fall back to Canvas 2D
                renderer = new CanvasRenderer();
                renderer.attach(el);
            }
        } else {
            renderer = new CanvasRenderer();
            renderer.attach(el);
        }

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

    // ── Toggle chronicle panel ────────────────────────────────────────
    // (Must be above conditional returns to satisfy React hook rules)
    const toggleChronicle = useCallback(() => {
        setChronicleOpen((prev) => !prev);
    }, []);

    // ── Handle ticker entry click → open chronicle ─────────────────
    const handleTickerClick = useCallback(() => {
        setChronicleOpen(true);
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
                <button className="ps-open-btn" title="Open Pyramid" onClick={() => openWindow(slug)}>
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
                    <button
                        className={`ps-overlay-btn ${chronicleOpen ? 'active' : ''}`}
                        onClick={toggleChronicle}
                        title="Chronicle"
                    >
                        Chronicle
                    </button>
                </div>
            </div>

            {/* Build status bar */}
            {isBuilding && (
                <div className="ps-build-bar">
                    <span className="ps-build-step">{currentStep ?? 'Building...'}</span>
                    {buildProg && (
                        <span className="ps-build-progress">
                            {buildProg.done}/{buildProg.total}
                        </span>
                    )}
                </div>
            )}

            {/* Loading indicator */}
            {loading && nodes.length === 0 && (
                <div className="ps-loading">Loading pyramid...</div>
            )}

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

                {/* Minimap overlay — top-right corner of canvas */}
                {miniatureLayers.length > 0 && (
                    <div className="ps-minimap-overlay">
                        <Minimap
                            layers={miniatureLayers}
                            maxDotsPerLayer={config.rendering.max_dots_per_layer}
                        />
                    </div>
                )}
            </div>

            {/* Chronicle panel — collapsible below the canvas */}
            {chronicleOpen && (
                <Chronicle
                    slug={slug}
                    entries={chronicleEntries}
                    generation={chronicleGen}
                    onArtifactClick={onNodeClick}
                    showMechanicalOps={config.chronicle.show_mechanical_ops}
                    autoExpandDecisions={config.chronicle.auto_expand_decisions}
                />
            )}

            {/* Event Ticker — bottom bar, always visible when entries exist */}
            {config.ticker.enabled && (
                <EventTicker
                    entries={chronicleEntries}
                    generation={chronicleGen}
                    onEntryClick={handleTickerClick}
                />
            )}
        </div>
    );
}
