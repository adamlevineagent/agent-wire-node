/**
 * Detects the best available rendering tier for the current environment.
 *
 * Detection order: WebGPU → WebGL2 → Canvas 2D → DOM
 * User override via pyramid_viz_config contribution takes precedence.
 */

import { useMemo } from 'react';

export type RenderTier = 'rich' | 'standard' | 'minimal';

interface RenderTierInfo {
    /** The detected or configured tier */
    tier: RenderTier;
    /** Whether WebGPU is available (future use) */
    hasWebGPU: boolean;
    /** Whether WebGL2 is available */
    hasWebGL2: boolean;
    /** Human-readable description */
    description: string;
}

export function useRenderTier(configuredTier: string): RenderTierInfo {
    return useMemo(() => {
        const hasWebGPU = typeof navigator !== 'undefined' && 'gpu' in navigator;

        let hasWebGL2 = false;
        try {
            const testCanvas = document.createElement('canvas');
            hasWebGL2 = !!testCanvas.getContext('webgl2');
        } catch {
            hasWebGL2 = false;
        }

        // If user configured a specific tier, respect it
        if (configuredTier === 'rich') {
            return {
                tier: hasWebGL2 ? 'rich' : 'standard',
                hasWebGPU,
                hasWebGL2,
                description: hasWebGL2 ? 'Rich (WebGL2)' : 'Standard (WebGL2 unavailable, using Canvas 2D)',
            };
        }
        if (configuredTier === 'standard') {
            return { tier: 'standard', hasWebGPU, hasWebGL2, description: 'Standard (Canvas 2D)' };
        }
        if (configuredTier === 'minimal') {
            return { tier: 'minimal', hasWebGPU, hasWebGL2, description: 'Minimal (DOM)' };
        }

        // Auto-detect
        if (hasWebGL2) {
            return { tier: 'rich', hasWebGPU, hasWebGL2, description: 'Auto: Rich (WebGL2 detected)' };
        }
        return { tier: 'standard', hasWebGPU, hasWebGL2, description: 'Auto: Standard (Canvas 2D)' };
    }, [configuredTier]);
}
