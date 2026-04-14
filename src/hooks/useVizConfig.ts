import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** Matches the seed default in viz_config.rs */
export interface PyramidVizConfig {
    schema_type: 'pyramid_viz_config';
    rendering: {
        tier: 'auto' | 'minimal' | 'standard' | 'rich';
        max_dots_per_layer: number;
        always_collapse: boolean;
        force_all_nodes: boolean;
    };
    overlays: {
        structure: boolean;
        web_edges: boolean;
        staleness: boolean;
        provenance: boolean;
        weight_intensity: boolean;
    };
    chronicle: {
        show_mechanical_ops: boolean;
        auto_expand_decisions: boolean;
    };
    ticker: {
        enabled: boolean;
        position: 'bottom' | 'top';
    };
    window: {
        auto_pop_on_build: boolean;
    };
    density: {
        repulsion: number | 'auto';
        attraction: number | 'auto';
        damping: number | 'auto';
        settle_threshold: number | 'auto';
        label_min_radius: number | 'auto';
        max_iterations: number | 'auto';
        center_gravity: number | 'auto';
    };
}

const DEFAULT_VIZ_CONFIG: PyramidVizConfig = {
    schema_type: 'pyramid_viz_config',
    rendering: {
        tier: 'auto',
        max_dots_per_layer: 10,
        always_collapse: false,
        force_all_nodes: false,
    },
    overlays: {
        structure: true,
        web_edges: true,
        staleness: true,
        provenance: true,
        weight_intensity: true,
    },
    chronicle: {
        show_mechanical_ops: false,
        auto_expand_decisions: true,
    },
    ticker: {
        enabled: true,
        position: 'bottom',
    },
    window: {
        auto_pop_on_build: true,
    },
    density: {
        repulsion: 'auto',
        attraction: 'auto',
        damping: 'auto',
        settle_threshold: 'auto',
        label_min_radius: 'auto',
        max_iterations: 'auto',
        center_gravity: 'auto',
    },
};

interface UseVizConfigResult {
    config: PyramidVizConfig;
    loading: boolean;
    updateConfig: (partial: Partial<PyramidVizConfig>) => Promise<void>;
}

/**
 * Hook to load and live-reload the pyramid_viz_config contribution.
 * Supports per-pyramid override (slug-scoped) with global fallback.
 * Subscribes to ConfigSynced events for live reload when config changes.
 */
export function useVizConfig(slug?: string): UseVizConfigResult {
    const [config, setConfig] = useState<PyramidVizConfig>(DEFAULT_VIZ_CONFIG);
    const [loading, setLoading] = useState(true);

    // Load config on mount and when slug changes
    useEffect(() => {
        setLoading(true);
        invoke<PyramidVizConfig>('pyramid_get_viz_config', { slug: slug ?? null })
            .then((cfg) => setConfig({ ...DEFAULT_VIZ_CONFIG, ...cfg }))
            .catch(() => setConfig(DEFAULT_VIZ_CONFIG))
            .finally(() => setLoading(false));
    }, [slug]);

    // Subscribe to ConfigSynced events for live reload
    useEffect(() => {
        const unlisten = listen<{ slug: string; kind: { type: string; schema_type?: string } }>(
            'cross-build-event',
            (event) => {
                const kind = event.payload?.kind;
                if (kind?.type === 'config_synced' && kind?.schema_type === 'pyramid_viz_config') {
                    // Reload config — either our slug changed or global changed
                    invoke<PyramidVizConfig>('pyramid_get_viz_config', { slug: slug ?? null })
                        .then((cfg) => setConfig({ ...DEFAULT_VIZ_CONFIG, ...cfg }))
                        .catch(() => {});
                }
            },
        );
        return () => { unlisten.then((fn) => fn()); };
    }, [slug]);

    const updateConfig = useCallback(
        async (partial: Partial<PyramidVizConfig>) => {
            const merged = { ...config, ...partial, schema_type: 'pyramid_viz_config' as const };
            await invoke('pyramid_set_viz_config', { slug: slug ?? null, config: merged });
            // ConfigSynced event will trigger reload via the listener above
        },
        [config, slug],
    );

    return { config, loading, updateConfig };
}
